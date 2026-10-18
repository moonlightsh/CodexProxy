//! 编排层集成测试（设计 §11 编排行）。
//!
//! 每个测试使用独立的临时 CODEX_HOME 与数据目录、内存凭据、wiremock 假网关与空闲端口；
//! 不访问外网、不修改本进程环境变量、不写真实用户目录。诊断日志统一写到
//! `CARGO_TARGET_TMPDIR` 下的共享目录，并与各测试的临时目录一起扫描，确认任何落盘文件都不含 Key。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use helper_core::codex_config;
use helper_core::codex_env;
use helper_core::consts;
use helper_core::credential::{CredentialStore, MemoryCredentialStore};
use helper_core::manager::{self, Manager, ManagerOptions};
use helper_core::paths::HelperPaths;
use helper_core::proxy::{self, ProxyConfig, RecordSink};
use helper_core::rules::Route;
use helper_core::state::{self, HelperState};
use helper_core::types::{
    CleanupReport, EnableRequest, ManagerError, ProxyState, ProxyStatus, Reachability,
    ReconcileOutcome, SaveKeyRequest,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 测试 Key：不以 `sk-` 开头，日志的通用脱敏规则不会替它兜底，扫描结果只取决于编排层自身。
const KEY: &str = "hk-TEST-7f3a9c2e5b1d4a60";
const OLD_KEY: &str = "hk-OLD-0c4e8a2f6b9d1e37";
const COMMAND: &str = "C:\\Program Files\\CodexHelper\\codex-helper-credential.exe";
/// macOS App Translocation 下的凭据程序路径（临时安装位置）。
const TRANSIENT_COMMAND: &str = "/private/var/folders/zz/abc/T/AppTranslocation/8E1A-44F2/d/CodexHelper.app/Contents/MacOS/codex-helper-credential";
/// 直接在 dmg 里运行时的凭据程序路径（临时安装位置）。
const VOLUME_COMMAND: &str =
    "/Volumes/CodexHelper/CodexHelper.app/Contents/MacOS/codex-helper-credential";

/// 真实感夹具：注释、行尾注释、其他 provider、projects、plugins。
const USER_CONFIG: &str = r#"# 用户的 Codex 配置
model = "gpt-5.2-codex"
model_provider = "custom"   # 公司内部代理
approval_policy = "untrusted"
approvals_reviewer = "human_review"
sandbox_mode = "read-only"

[model_providers.custom]
name = "Custom"
base_url = "https://llm.example.invalid/v1"
env_key = "CUSTOM_API_KEY"

# 项目信任列表
[projects."/Users/me/work/app"]
trust_level = "trusted"

[plugins.review]
enabled = true
"#;

/// 带外部 catalog 指针的夹具（指针为最后一个根键，停用后可逐字节还原）。
const CATALOG_CONFIG: &str = r#"# 用户的 Codex 配置
model = "gpt-5.2-codex"
model_catalog_json = "C:\\catalog\\models.json"

[projects."/Users/me/work/app"]
trust_level = "trusted"
"#;
const CATALOG_PATH: &str = "C:\\catalog\\models.json";

const USER_ENV: &str = "# 用户自己的变量\nFOO=bar\n";

// ---- 夹具 ----

/// 全部测试共享的诊断日志目录（日志是进程级单例，不能指向会被删除的临时目录）。
fn log_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("manager-test-logs");
        let _ = std::fs::remove_dir_all(&dir);
        helper_core::log::init(&dir);
        dir
    })
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn port_is_free(port: u16) -> bool {
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok()
}

fn with_key(key: &str) -> EnableRequest {
    EnableRequest {
        key: Some(key.to_string()),
        ..Default::default()
    }
}

fn read(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn collect_files(root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.insert(path, bytes);
        }
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

struct Sandbox {
    codex: tempfile::TempDir,
    data: tempfile::TempDir,
    credentials: Arc<MemoryCredentialStore>,
    gateway: MockServer,
    gateway_url: String,
    port: u16,
    socks5_port: u16,
    records: Arc<AtomicUsize>,
}

impl Sandbox {
    async fn new() -> Self {
        Self::with_gateway(200).await
    }

    /// 假网关对 `GET /v1/models` 固定返回 `status`。
    async fn with_gateway(status: u16) -> Self {
        Self::with_response(ResponseTemplate::new(status)).await
    }

    /// 假网关延迟 `delay` 后返回 200（模拟慢速 Key 校验）。
    async fn with_slow_gateway(delay: Duration) -> Self {
        Self::with_response(ResponseTemplate::new(200).set_delay(delay)).await
    }

    async fn with_response(response: ResponseTemplate) -> Self {
        log_dir();
        let gateway = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(response)
            .mount(&gateway)
            .await;
        let gateway_url = gateway.uri();
        Self {
            codex: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
            credentials: Arc::new(MemoryCredentialStore::new()),
            gateway,
            gateway_url,
            port: free_port(),
            // 无人监听的端口：代理的 SOCKS5 上游、可达性检测都应判为不可达
            socks5_port: free_port(),
            records: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Key 校验走一个无人监听的地址（连接被拒 → 超时或网络错误）。
    async fn with_dead_gateway() -> Self {
        let mut sandbox = Self::new().await;
        sandbox.gateway_url = format!("http://127.0.0.1:{}", free_port());
        sandbox
    }

    fn paths(&self) -> HelperPaths {
        HelperPaths::new(self.codex.path(), self.data.path())
    }

    fn options_with(&self, command: &str) -> ManagerOptions {
        let records = Arc::clone(&self.records);
        let sink: RecordSink = Arc::new(move |_| {
            records.fetch_add(1, Ordering::SeqCst);
        });
        ManagerOptions {
            paths: self.paths(),
            credentials: self.credentials.clone(),
            credential_command: command.to_string(),
            proxy_port: self.port,
            socks5_host: "127.0.0.1".to_string(),
            socks5_port: self.socks5_port,
            gateway_base_url: self.gateway_url.clone(),
            gateway_host: "127.0.0.1".to_string(),
            gateway_port: self.gateway.address().port(),
            on_record: Some(sink),
            secure_resolver: None,
            secure_drain_timeout: None,
        }
    }

    fn manager(&self) -> Manager {
        Manager::new(self.options_with(COMMAND))
    }

    fn config(&self) -> PathBuf {
        self.paths().config_toml()
    }

    fn backup(&self) -> PathBuf {
        self.paths().config_backup()
    }

    fn env(&self) -> PathBuf {
        self.paths().env_file()
    }

    fn state_file(&self) -> PathBuf {
        self.paths().state_file()
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.config(), text).unwrap();
    }

    fn write_env(&self, text: &str) {
        std::fs::write(self.env(), text).unwrap();
    }

    fn write_state(&self, state: &HelperState) {
        state::save(&self.state_file(), state).unwrap();
    }

    fn read_config(&self) -> String {
        std::fs::read_to_string(self.config()).unwrap()
    }

    fn state(&self) -> Option<HelperState> {
        state::load(&self.state_file()).unwrap()
    }

    fn credential(&self) -> Option<String> {
        self.credentials
            .read()
            .unwrap()
            .map(|secret| secret.expose().to_string())
    }

    /// 两个根目录下全部文件的内容（用于“无任何写入 / 逐字节一致”断言）。
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        collect_files(self.codex.path(), &mut files);
        collect_files(self.data.path(), &mut files);
        files
    }

    async fn gateway_hits(&self) -> usize {
        self.gateway
            .received_requests()
            .await
            .map_or(0, |requests| requests.len())
    }

    /// 设计 §10：临时 CODEX_HOME、数据目录与诊断日志中的任何文件都不含 Key。
    fn assert_no_key_on_disk(&self) {
        let mut files = self.snapshot();
        collect_files(log_dir(), &mut files);
        for (path, bytes) in files {
            for key in [KEY, OLD_KEY] {
                assert!(!contains(&bytes, key), "{} 中出现了 Key", path.display());
            }
        }
    }
}

/// 端口上的“其他程序”：接受连接后回一个与自识端点无关的响应。
async fn spawn_foreign_listener() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });
    (port, task)
}

/// 假目标服务器：回一个固定的 200 响应。
async fn spawn_fake_target() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    port
}

fn assert_code(result: Result<impl std::fmt::Debug, ManagerError>, code: &str) -> ManagerError {
    let error = result.expect_err("操作应失败");
    assert_eq!(error.code(), code, "{error}");
    error
}

// ---- 1. 启用 → 停用 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_with_new_key_then_disable_restores_everything() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();

    let status = manager
        .enable(with_key(&format!("  {KEY}\n")))
        .await
        .expect("启用应成功");
    assert!(status.enabled && status.key_configured && !status.needs_key);
    assert!(status.config_managed && status.env_managed && !status.residue);
    assert_eq!(status.config_error, None);
    assert_eq!(
        status.proxy,
        ProxyStatus {
            state: ProxyState::Running,
            port: sandbox.port
        }
    );
    assert_eq!(sandbox.gateway_hits().await, 1);

    // 凭据：trim 后写入
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));

    // config.toml 符合 §4.2，其余内容保留
    let text = sandbox.read_config();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["model_provider"].as_str(), Some(consts::PROVIDER_ID));
    let provider = &doc["model_providers"]["managed_gateway"];
    assert_eq!(provider["name"].as_str(), Some(consts::PROVIDER_NAME));
    assert_eq!(
        provider["base_url"].as_str(),
        Some(consts::GATEWAY_BASE_URL)
    );
    assert_eq!(provider["wire_api"].as_str(), Some("responses"));
    assert_eq!(provider["auth"]["command"].as_str(), Some(COMMAND));
    let args: Vec<_> = provider["auth"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert_eq!(args, ["get", consts::CREDENTIAL_TARGET]);
    assert!(text.contains("# 用户的 Codex 配置"));
    assert!(text.contains("[model_providers.custom]"));
    assert!(text.contains("[projects.\"/Users/me/work/app\"]"));
    assert!(text.contains("[plugins.review]"));
    // 备份为启用前原文
    assert_eq!(
        read(&sandbox.backup()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );

    // .env 符合 §4.3：块在末尾，用户行保留（含 ALL_PROXY）
    let env = std::fs::read_to_string(sandbox.env()).unwrap();
    assert_eq!(
        env,
        format!("{USER_ENV}\n{}", codex_env::render_block(sandbox.port))
    );
    assert!(env.contains(&format!("HTTPS_PROXY=http://127.0.0.1:{}", sandbox.port)));
    assert!(env.contains(&format!("ALL_PROXY=http://127.0.0.1:{}", sandbox.port)));

    // config.toml 受管根键已校正为固定值
    let doc: toml_edit::DocumentMut = sandbox.read_config().parse().unwrap();
    assert_eq!(doc["approval_policy"].as_str(), Some("on-request"));
    assert_eq!(doc["approvals_reviewer"].as_str(), Some("auto_review"));
    assert_eq!(doc["sandbox_mode"].as_str(), Some("workspace-write"));

    // 状态文件：三个受管根键的原值已记录
    let state = sandbox.state().unwrap();
    assert!(state.enabled);
    assert_eq!(state.previous_model_provider.as_deref(), Some("custom"));
    assert_eq!(state.previous_model_catalog_json, None);
    assert_eq!(state.previous_approval_policy.as_deref(), Some("untrusted"));
    assert_eq!(
        state.previous_approvals_reviewer.as_deref(),
        Some("human_review")
    );
    assert_eq!(state.previous_sandbox_mode.as_deref(), Some("read-only"));

    // 代理已在端口上监听且自识
    assert!(proxy::probe_existing(sandbox.port).await);

    let status = manager.disable().await.expect("停用应成功");
    assert!(!status.enabled && !status.config_managed && !status.env_managed);
    assert!(!status.residue && status.key_configured);
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    let state = sandbox.state().unwrap();
    assert_eq!(state, HelperState::default());
    assert!(port_is_free(sandbox.port).await, "停用后端口应已释放");
    // 凭据默认保留
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_created_by_enable_is_deleted_by_disable() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();

    manager.enable(with_key(KEY)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(sandbox.env()).unwrap(),
        codex_env::render_block(sandbox.port)
    );
    manager.disable().await.unwrap();
    assert!(!sandbox.env().exists(), "原先不存在的 .env 应被删除");
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    sandbox.assert_no_key_on_disk();
}

// ---- 2. 启用期间的无关修改 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_made_while_enabled_survive_disable() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();

    let edited = sandbox
        .read_config()
        .replace("model = \"gpt-5.2-codex\"", "model = \"o3\"")
        .replace("trust_level = \"trusted\"", "trust_level = \"untrusted\"")
        + "\n[model_providers.extra]\nname = \"Extra\"\nbase_url = \"https://extra.invalid\"\n";
    sandbox.write_config(&edited);

    manager.disable().await.unwrap();
    let text = sandbox.read_config();
    let expected = USER_CONFIG
        .replace("model = \"gpt-5.2-codex\"", "model = \"o3\"")
        .replace("trust_level = \"trusted\"", "trust_level = \"untrusted\"")
        + "\n[model_providers.extra]\nname = \"Extra\"\nbase_url = \"https://extra.invalid\"\n";
    assert_eq!(text, expected);
    assert!(!text.contains("managed_gateway"));
    sandbox.assert_no_key_on_disk();
}

// ---- 3. Key 校验 ----

#[tokio::test]
async fn rejected_key_keeps_existing_credential_and_files() {
    for status in [401, 403] {
        let sandbox = Sandbox::with_gateway(status).await;
        sandbox.write_config(USER_CONFIG);
        sandbox.write_env(USER_ENV);
        sandbox.credentials.write(OLD_KEY).unwrap();
        let manager = sandbox.manager();
        let before = sandbox.snapshot();

        assert_code(manager.enable(with_key(KEY)).await, "keyRejected");
        // “仍然保存”也不能放行 401 / 403
        let forced = EnableRequest {
            force_save: true,
            ..with_key(KEY)
        };
        assert_code(manager.enable(forced).await, "keyRejected");

        assert_eq!(sandbox.snapshot(), before);
        assert_eq!(sandbox.credential().as_deref(), Some(OLD_KEY));
        assert_eq!(manager.status().await.proxy.state, ProxyState::Stopped);
        assert!(port_is_free(sandbox.port).await);
        sandbox.assert_no_key_on_disk();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_server_error_requires_force_save() {
    let sandbox = Sandbox::with_gateway(503).await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    let error = assert_code(manager.enable(with_key(KEY)).await, "gatewayUnavailable");
    assert!(error.to_string().contains("5xx"), "{error}");
    assert_eq!(sandbox.snapshot(), before, "未确认时不得有任何写入");
    assert_eq!(sandbox.credential(), None);

    let status = manager
        .enable(EnableRequest {
            force_save: true,
            ..with_key(KEY)
        })
        .await
        .expect("仍然保存后应启用成功");
    assert!(status.enabled && status.config_managed && status.env_managed);
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_gateway_requires_force_save() {
    let sandbox = Sandbox::with_dead_gateway().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    let error = assert_code(manager.enable(with_key(KEY)).await, "gatewayUnavailable");
    assert!(error.to_string().contains("超时或网络错误"), "{error}");
    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.credential(), None);

    let status = manager
        .enable(EnableRequest {
            force_save: true,
            ..with_key(KEY)
        })
        .await
        .unwrap();
    assert!(status.enabled);
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn empty_or_missing_key_is_rejected_without_writes() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    assert_code(manager.enable(with_key("   \n")).await, "emptyKey");
    assert_code(
        manager.enable(EnableRequest::default()).await,
        "credentialMissing",
    );
    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.gateway_hits().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_with_saved_credential_skips_key_check() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.credentials.write(OLD_KEY).unwrap();
    let manager = sandbox.manager();

    let status = manager.enable(EnableRequest::default()).await.unwrap();
    assert!(status.enabled && status.config_managed && status.env_managed);
    assert_eq!(sandbox.gateway_hits().await, 0);
    assert_eq!(sandbox.credential().as_deref(), Some(OLD_KEY));
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn save_key_validates_and_leaves_files_alone() {
    let rejected = Sandbox::with_gateway(401).await;
    rejected.credentials.write(OLD_KEY).unwrap();
    let manager = rejected.manager();
    assert_code(
        manager
            .save_key(SaveKeyRequest {
                key: KEY.into(),
                force_save: true,
            })
            .await,
        "keyRejected",
    );
    assert_eq!(rejected.credential().as_deref(), Some(OLD_KEY));

    let failing = Sandbox::with_gateway(500).await;
    let manager = failing.manager();
    let request = SaveKeyRequest {
        key: KEY.into(),
        force_save: false,
    };
    assert_code(
        manager.save_key(request.clone()).await,
        "gatewayUnavailable",
    );
    assert_eq!(failing.credential(), None);
    let status = manager
        .save_key(SaveKeyRequest {
            force_save: true,
            ..request
        })
        .await
        .unwrap();
    assert!(status.key_configured);

    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();
    assert_code(
        manager
            .save_key(SaveKeyRequest {
                key: " ".into(),
                force_save: false,
            })
            .await,
        "emptyKey",
    );
    let status = manager
        .save_key(SaveKeyRequest {
            key: format!("\t{KEY} "),
            force_save: false,
        })
        .await
        .unwrap();
    assert!(status.key_configured && !status.enabled);
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));
    assert_eq!(sandbox.snapshot(), before, "save_key 不改任何文件");
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_key_chars_are_rejected_before_gateway_and_writes() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();
    for key in ["hk 1", "hk-中文", "hk-a\nb", "hk-\u{7f}", "\u{e9}"] {
        let error = assert_code(manager.enable(with_key(key)).await, "invalidKeyChars");
        assert_eq!(error.to_string(), "Key 只能包含英文字母、数字和符号");
        assert!(!error.to_string().contains(key), "{error}");
        let error = assert_code(
            manager
                .save_key(SaveKeyRequest {
                    key: key.into(),
                    force_save: true,
                })
                .await,
            "invalidKeyChars",
        );
        assert_eq!(error.to_string(), "Key 只能包含英文字母、数字和符号");
        assert!(!error.to_string().contains(key), "{error}");
    }
    // 空值判定在字符校验之前
    assert_code(manager.enable(with_key("   \n")).await, "emptyKey");
    assert_code(
        manager
            .save_key(SaveKeyRequest {
                key: "   \n".into(),
                force_save: true,
            })
            .await,
        "emptyKey",
    );
    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.gateway_hits().await, 0, "字符校验应在访问网关之前");
    assert_eq!(sandbox.credential(), None);
    assert!(port_is_free(sandbox.port).await);
}

#[tokio::test]
async fn printable_symbols_in_key_are_accepted() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager();
    let key = r#"hk-A\B"C'D$E;F#G!`{}|~"#;
    let status = manager
        .save_key(SaveKeyRequest {
            key: format!("\n{key}\n"),
            force_save: false,
        })
        .await
        .unwrap();
    assert!(status.key_configured);
    assert_eq!(sandbox.credential().as_deref(), Some(key));
}

// ---- 4. model_catalog_json ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_pointer_requires_confirmation_and_is_restored() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(CATALOG_CONFIG);
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    let status = manager.status().await;
    assert_eq!(status.model_catalog_json.as_deref(), Some(CATALOG_PATH));

    match manager.enable(with_key(KEY)).await {
        Err(ManagerError::CatalogConfirmationRequired { path }) => assert_eq!(path, CATALOG_PATH),
        other => panic!("应要求确认 catalog，实际 {other:?}"),
    }
    assert_eq!(sandbox.snapshot(), before, "未确认时不得有任何写入");
    assert_eq!(sandbox.gateway_hits().await, 0, "预检先于 Key 校验");
    assert_eq!(sandbox.credential(), None);

    let status = manager
        .enable(EnableRequest {
            confirm_remove_catalog: true,
            ..with_key(KEY)
        })
        .await
        .unwrap();
    assert!(status.enabled && status.config_managed);
    assert_eq!(status.model_catalog_json, None);
    assert!(!sandbox.read_config().contains("model_catalog_json"));
    let state = sandbox.state().unwrap();
    assert_eq!(
        state.previous_model_catalog_json.as_deref(),
        Some(CATALOG_PATH)
    );
    assert_eq!(state.previous_model_provider, None);

    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(CATALOG_CONFIG.as_bytes().to_vec())
    );
    sandbox.assert_no_key_on_disk();
}

// ---- 5. 端口被外部程序占用 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_listener_on_port_rolls_back_everything() {
    for old_key in [Some(OLD_KEY), None] {
        let (port, foreign) = spawn_foreign_listener().await;
        let mut sandbox = Sandbox::new().await;
        sandbox.port = port;
        sandbox.write_config(USER_CONFIG);
        sandbox.write_env(USER_ENV);
        // 非规范格式的状态文件：回滚后必须逐字节一致（而不是重新格式化）
        std::fs::write(sandbox.state_file(), "{\"autostart\":true}").unwrap();
        if let Some(old_key) = old_key {
            sandbox.credentials.write(old_key).unwrap();
        }
        let manager = sandbox.manager();
        let before = sandbox.snapshot();

        match manager.enable(with_key(KEY)).await {
            Err(ManagerError::PortUnavailable { port: reported, .. }) => {
                assert_eq!(reported, port)
            }
            other => panic!("应报端口不可用，实际 {other:?}"),
        }
        // config.toml、.env、状态文件与调用前逐字节相同，也不残留备份
        assert_eq!(sandbox.snapshot(), before);
        assert!(!sandbox.backup().exists());
        // 凭据回滚为旧值（原先无凭据则被删除）
        assert_eq!(sandbox.credential().as_deref(), old_key);
        assert_eq!(manager.status().await.proxy.state, ProxyState::Stopped);
        sandbox.assert_no_key_on_disk();
        foreign.abort();
    }
}

// ---- 6. 端口上是本工具的另一个实例 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_helper_instance_is_reused_as_external() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let other = proxy::spawn(sandbox.port, ProxyConfig::default())
        .await
        .unwrap();
    let manager = sandbox.manager();

    let status = manager.enable(with_key(KEY)).await.expect("应复用已有实例");
    assert_eq!(status.proxy.state, ProxyState::External);
    assert!(status.env_managed && status.config_managed);

    // 外部实例不属于本进程：停用时无法停止，状态置为 Stopped
    let status = manager.disable().await.unwrap();
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert!(proxy::probe_existing(sandbox.port).await);
    other.shutdown().await;
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    sandbox.assert_no_key_on_disk();
}

// ---- 7. .env 写入失败 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_write_failure_rolls_back_config_and_stops_new_proxy() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    std::fs::create_dir(sandbox.env()).unwrap();
    sandbox.credentials.write(OLD_KEY).unwrap();
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    assert_code(manager.enable(with_key(KEY)).await, "io");
    assert_eq!(sandbox.snapshot(), before, "config.toml 与状态文件应回滚");
    assert!(!sandbox.backup().exists());
    assert!(sandbox.env().is_dir());
    assert!(sandbox.state().is_none(), "状态文件原先不存在，应被删除");
    assert_eq!(sandbox.credential().as_deref(), Some(OLD_KEY));
    let status = manager.status().await;
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert!(!status.enabled);
    assert!(port_is_free(sandbox.port).await, "新启动的代理应已停止");
    sandbox.assert_no_key_on_disk();
}

// ---- 8. 损坏的 config.toml ----

#[tokio::test]
async fn invalid_config_is_rejected_before_any_write() {
    let cases = [
        "model = \"o3\"\n[model_providers.managed_gateway\nname = 1\n",
        "model_provider = 1\n",
        "model_providers = \"x\"\n",
        "[model_providers]\nmanaged_gateway = { name = \"x\" }\n",
        "[model_providers.managed_gateway]\nauth = \"x\"\n",
    ];
    for text in cases {
        let sandbox = Sandbox::new().await;
        sandbox.write_config(text);
        sandbox.write_env(USER_ENV);
        let manager = sandbox.manager();
        let before = sandbox.snapshot();

        let status = manager.status().await;
        assert!(status.config_error.is_some(), "{text}");
        assert!(!status.config_managed);

        let error = assert_code(manager.enable(with_key(KEY)).await, "configInvalid");
        assert!(error.to_string().contains("config.toml"), "{error}");
        assert_eq!(sandbox.snapshot(), before, "{text}");
        assert!(!sandbox.backup().exists());
        assert_eq!(sandbox.credential(), None);
        assert_eq!(sandbox.gateway_hits().await, 0);
        assert!(port_is_free(sandbox.port).await);

        // 停用同样先预检：不做任何修改
        assert_code(manager.disable().await, "configInvalid");
        assert_eq!(sandbox.snapshot(), before, "{text}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_with_broken_config_changes_nothing() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();

    sandbox.write_config("model = [\n");
    let before = sandbox.snapshot();
    assert_code(manager.disable().await, "configInvalid");
    assert_eq!(sandbox.snapshot(), before);
    let status = manager.status().await;
    assert!(status.enabled && status.env_managed);
    assert_eq!(status.proxy.state, ProxyState::Running, "预检失败不停代理");
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

// ---- 9. 已启用时再次启用 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_enable_with_new_key_keeps_previous_values() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    manager.enable(with_key(OLD_KEY)).await.unwrap();
    let state_before = sandbox.state().unwrap();

    let status = manager.enable(with_key(KEY)).await.unwrap();
    assert!(status.enabled);
    assert_eq!(sandbox.state().unwrap(), state_before);
    assert_eq!(
        sandbox.state().unwrap().previous_model_provider.as_deref(),
        Some("custom")
    );
    assert_eq!(sandbox.credential().as_deref(), Some(KEY));
    assert_eq!(status.proxy.state, ProxyState::Running);

    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    sandbox.assert_no_key_on_disk();
}

// ---- 10. 启动对账 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_repairs_drift_and_starts_proxy() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let first = sandbox.manager();
    first.enable(with_key(KEY)).await.unwrap();
    first.shutdown().await;
    let state_before = sandbox.state().unwrap();

    // 被改动的 base_url、被删除的 .env
    let drifted = sandbox
        .read_config()
        .replace(consts::GATEWAY_BASE_URL, "http://evil.invalid:1");
    sandbox.write_config(&drifted);
    std::fs::remove_file(sandbox.env()).unwrap();

    let manager = sandbox.manager();
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Applied, "{report:?}");
    assert_eq!(report.error, None);
    assert!(report.status.config_managed && report.status.env_managed);
    assert_eq!(report.status.proxy.state, ProxyState::Running);
    assert!(sandbox.read_config().contains(consts::GATEWAY_BASE_URL));
    assert!(!sandbox.read_config().contains("evil.invalid"));
    assert!(proxy::probe_existing(sandbox.port).await);
    // 不记录原值
    assert_eq!(sandbox.state().unwrap(), state_before);

    // 停用后仍能逐字节还原（.env 被删过，用户行已随之丢失，只剩无块）
    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert!(!sandbox.env().exists());
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_rewrites_credential_command_after_reinstall() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let first = sandbox.manager();
    first.enable(with_key(KEY)).await.unwrap();
    first.shutdown().await;

    let moved = "D:\\Apps\\CodexHelper\\codex-helper-credential.exe";
    let manager = Manager::new(sandbox.options_with(moved));
    assert!(
        !manager.status().await.config_managed,
        "command 不同即不完全受管"
    );
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Applied);
    assert!(report.status.config_managed);
    let doc: toml_edit::DocumentMut = sandbox.read_config().parse().unwrap();
    assert_eq!(
        doc["model_providers"]["managed_gateway"]["auth"]["command"].as_str(),
        Some(moved)
    );
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_without_key_needs_key_and_touches_nothing() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let first = sandbox.manager();
    first.enable(with_key(KEY)).await.unwrap();
    first.shutdown().await;
    sandbox.credentials.delete().unwrap();
    // 制造漂移：对账在缺 Key 时也不能去修它
    let drifted = sandbox
        .read_config()
        .replace(consts::GATEWAY_BASE_URL, "http://evil.invalid:1");
    sandbox.write_config(&drifted);
    let before = sandbox.snapshot();

    let manager = sandbox.manager();
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::NeedsKey);
    assert!(report.status.needs_key && report.status.enabled);
    assert_eq!(report.status.proxy.state, ProxyState::Stopped);
    assert_eq!(sandbox.snapshot(), before);
    assert!(port_is_free(sandbox.port).await, "缺 Key 时不启动代理");
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn reconcile_reports_residue_when_disabled() {
    // 配置残留（无状态文件）
    let sandbox = Sandbox::new().await;
    sandbox.write_config(
        "model_provider = \"managed_gateway\"\n\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
    );
    let manager = sandbox.manager();
    let before = sandbox.snapshot();
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::ResidueFound);
    assert!(report.status.residue && !report.status.enabled);
    assert_eq!(sandbox.snapshot(), before, "对账只提示，不自动清理");

    // .env 中只有不完整块（停用状态）
    let sandbox = Sandbox::new().await;
    sandbox.write_state(&HelperState::default());
    sandbox.write_env(&format!(
        "FOO=bar\n{}\nHTTP_PROXY=http://127.0.0.1:1\n",
        codex_env::BEGIN_MARKER
    ));
    let report = sandbox.manager().reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::ResidueFound);
    assert!(report.status.residue);
    assert!(port_is_free(sandbox.port).await);
}

#[tokio::test]
async fn reconcile_is_idle_when_clean() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let before = sandbox.snapshot();
    let report = sandbox.manager().reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Idle);
    assert_eq!(report.error, None);
    assert!(!report.status.residue && !report.status.enabled);
    assert_eq!(sandbox.snapshot(), before);

    // 什么都不存在同样是 Idle
    let empty = Sandbox::new().await;
    assert_eq!(
        empty.manager().reconcile_on_startup().await.outcome,
        ReconcileOutcome::Idle
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_fails_on_foreign_port_without_touching_env() {
    let (port, foreign) = spawn_foreign_listener().await;
    let mut sandbox = Sandbox::new().await;
    sandbox.port = port;
    sandbox.write_config(USER_CONFIG);
    sandbox.credentials.write(KEY).unwrap();
    sandbox.write_state(&HelperState {
        enabled: true,
        previous_model_provider: Some("custom".into()),
        ..Default::default()
    });
    // 已存在的 .env（块端口与当前不同，正常对账会改写它）
    let env_text = format!("{USER_ENV}\n{}", codex_env::render_block(1));
    sandbox.write_env(&env_text);

    let manager = sandbox.manager();
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Failed);
    assert_eq!(report.error.as_ref().unwrap().code, "portUnavailable");
    assert_eq!(read(&sandbox.env()), Some(env_text.into_bytes()));
    assert_eq!(report.status.proxy.state, ProxyState::Stopped);
    assert!(report.status.enabled);
    foreign.abort();
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn reconcile_reports_invalid_config_as_failed() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config("model = [\n");
    sandbox.credentials.write(KEY).unwrap();
    sandbox.write_state(&HelperState {
        enabled: true,
        ..Default::default()
    });
    let before = sandbox.snapshot();
    let report = sandbox.manager().reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Failed);
    assert_eq!(report.error.unwrap().code, "configInvalid");
    assert!(report.status.config_error.is_some());
    assert_eq!(sandbox.snapshot(), before);
    assert!(port_is_free(sandbox.port).await);
}

// ---- 11. 卸载清理 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_restores_enabled_configuration() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    manager.set_autostart_flag(true).await.unwrap();
    manager.shutdown().await;

    let report = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
    assert_eq!(
        report,
        CleanupReport {
            env_block_removed: true,
            config_restored: true,
            state_reset: true,
            key_purged: false,
        }
    );
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert_eq!(
        sandbox.state().unwrap(),
        HelperState {
            autostart: true,
            ..Default::default()
        },
        "保留 autostart"
    );
    assert_eq!(sandbox.credential().as_deref(), Some(KEY), "默认保留凭据");
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn cleanup_without_state_file_removes_managed_residue() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(concat!(
        "model = \"o3\"\n",
        "model_provider = \"managed_gateway\"\n",
        "\n[model_providers.custom]\nname = \"Custom\"\n",
        "\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
        "\n[model_providers.managed_gateway.auth]\ncommand = \"x\"\n",
    ));
    sandbox.write_env(&format!("{USER_ENV}\n{}", codex_env::render_block(17891)));

    let report = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
    assert!(report.env_block_removed && report.config_restored);
    assert!(!report.state_reset && !report.key_purged);
    assert_eq!(
        sandbox.read_config(),
        "model = \"o3\"\n\n[model_providers.custom]\nname = \"Custom\"\n"
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert!(!sandbox.state_file().exists(), "状态文件缺失时不创建");
}

#[tokio::test]
async fn cleanup_when_disabled_keeps_user_model_provider() {
    let sandbox = Sandbox::new().await;
    sandbox.write_state(&HelperState::default());
    // 用户自己的 provider，外加一段受管残留节
    let text =
        format!("{USER_CONFIG}\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n");
    sandbox.write_config(&text);
    let report = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
    assert!(report.config_restored);
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );

    // 没有任何受管内容时：配置逐字节不动
    let clean = Sandbox::new().await;
    clean.write_state(&HelperState::default());
    clean.write_config(USER_CONFIG);
    let before = clean.snapshot();
    let report = manager::cleanup(&clean.paths(), clean.credentials.as_ref(), false).unwrap();
    assert_eq!(report, CleanupReport::default());
    assert_eq!(clean.snapshot(), before);
    let doc: toml_edit::DocumentMut = clean.read_config().parse().unwrap();
    assert_eq!(doc["model_provider"].as_str(), Some("custom"));
}

#[tokio::test]
async fn cleanup_purges_key_and_is_idempotent() {
    let sandbox = Sandbox::new().await;
    sandbox.credentials.write(KEY).unwrap();
    sandbox.write_state(&HelperState {
        enabled: true,
        previous_model_provider: Some("custom".into()),
        previous_model_catalog_json: None,
        previous_approval_policy: Some("untrusted".into()),
        previous_approvals_reviewer: None,
        previous_sandbox_mode: Some("read-only".into()),
        autostart: false,
        ..Default::default()
    });
    sandbox.write_config(
        "model_provider = \"managed_gateway\"\n\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
    );
    sandbox.write_env(&codex_env::render_block(17891));

    let first = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), true).unwrap();
    assert_eq!(
        first,
        CleanupReport {
            env_block_removed: true,
            config_restored: true,
            state_reset: true,
            key_purged: true,
        }
    );
    assert_eq!(sandbox.credential(), None);
    assert!(!sandbox.env().exists(), ".env 只剩空白时删除");
    assert_eq!(
        sandbox.read_config(),
        "model_provider = \"custom\"\napproval_policy = \"untrusted\"\nsandbox_mode = \"read-only\"\n"
    );
    assert_eq!(sandbox.state().unwrap(), HelperState::default());

    let before = sandbox.snapshot();
    let second = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), true).unwrap();
    assert_eq!(second, CleanupReport::default(), "第二次执行应全部为 false");
    assert_eq!(sandbox.snapshot(), before);
    sandbox.assert_no_key_on_disk();
}

#[test]
fn cleanup_succeeds_when_nothing_exists() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HelperPaths::new(dir.path().join("no-codex"), dir.path().join("no-data"));
    let credentials = MemoryCredentialStore::new();
    let report = manager::cleanup(&paths, &credentials, true).unwrap();
    assert_eq!(report, CleanupReport::default());
    assert!(!paths.codex_home.exists() && !paths.data_dir.exists());
}

#[tokio::test]
async fn cleanup_treats_corrupt_state_as_missing() {
    let sandbox = Sandbox::new().await;
    std::fs::write(sandbox.state_file(), "{损坏").unwrap();
    sandbox.write_config(&format!(
        "{USER_CONFIG}\n[model_providers.managed_gateway]\nname = \"x\"\n"
    ));
    let report = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
    assert!(report.config_restored && !report.state_reset);
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(
        read(&sandbox.state_file()),
        Some("{损坏".as_bytes().to_vec())
    );
}

#[tokio::test]
async fn cleanup_reports_error_but_finishes_other_steps() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config("model = [\n");
    sandbox.write_env(&format!("{USER_ENV}\n{}", codex_env::render_block(17891)));
    sandbox.write_state(&HelperState {
        enabled: true,
        ..Default::default()
    });
    let error = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false)
        .expect_err("配置无法解析时应报错");
    assert!(error.to_string().contains("config.toml"), "{error}");
    // .env 块仍被移除；配置未还原，状态保持已启用
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert!(sandbox.state().unwrap().enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_residue_uses_same_logic_and_stops_proxy() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();

    let status = manager.cleanup_residue().await.unwrap();
    assert!(!status.enabled && !status.residue && status.key_configured);
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert!(port_is_free(sandbox.port).await);
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert_eq!(sandbox.credential().as_deref(), Some(KEY), "不删凭据");

    // 停用 + 残留：一键清理后 residue 消失
    sandbox.write_env(&format!(
        "{USER_ENV}{}",
        codex_env::render_block(sandbox.port)
    ));
    assert!(manager.status().await.residue);
    let status = manager.cleanup_residue().await.unwrap();
    assert!(!status.residue);
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    sandbox.assert_no_key_on_disk();
}

/// 受管节结构不符（inspect 报 NotATable）时：status 同时给出 config_error 与 residue，
/// 一键清理与卸载清理对结构宽松，仍能完成 .env、config.toml 与状态文件的清理。
#[tokio::test]
async fn cleanup_tolerates_structure_errors_in_residue() {
    let broken = format!("{USER_CONFIG}\n[model_providers.managed_gateway]\nauth = \"x\"\n");
    for uninstall in [false, true] {
        let sandbox = Sandbox::new().await;
        sandbox.write_config(&broken);
        sandbox.write_env(&format!(
            "{USER_ENV}{}",
            codex_env::render_block(sandbox.port)
        ));
        sandbox.write_state(&HelperState {
            enabled: false,
            previous_model_provider: Some("other".into()),
            ..Default::default()
        });
        let manager = sandbox.manager();

        let status = manager.status().await;
        let message = status.config_error.expect("应报告结构不符");
        assert!(
            message.contains("model_providers.managed_gateway.auth"),
            "{message}"
        );
        assert!(status.residue && !status.config_managed);
        let report = manager.reconcile_on_startup().await;
        assert_eq!(report.outcome, ReconcileOutcome::ResidueFound);

        if uninstall {
            let report =
                manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
            assert_eq!(
                report,
                CleanupReport {
                    env_block_removed: true,
                    config_restored: true,
                    state_reset: true,
                    key_purged: false,
                }
            );
        } else {
            manager.cleanup_residue().await.expect("一键清理应成功");
        }
        let status = manager.status().await;
        assert!(
            !status.residue && status.config_error.is_none(),
            "{uninstall}"
        );
        assert_eq!(
            read(&sandbox.config()),
            Some(USER_CONFIG.as_bytes().to_vec())
        );
        assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
        assert_eq!(sandbox.state().unwrap(), HelperState::default());
    }
}

// ---- 12. 清除 Key、旧块、自启 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_key_marks_needs_key_without_touching_files() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    let before = sandbox.snapshot();

    let status = manager.clear_key().await.unwrap();
    assert!(status.enabled && !status.key_configured && status.needs_key);
    assert_eq!(status.proxy.state, ProxyState::Running, "不停代理");
    assert_eq!(sandbox.snapshot(), before, "不改文件");
    assert_eq!(sandbox.credential(), None);

    manager.disable().await.unwrap();
    let status = manager.clear_key().await.unwrap();
    assert!(!status.needs_key, "停用时不需要 Key");
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn remove_legacy_env_block_only_touches_legacy_block() {
    let sandbox = Sandbox::new().await;
    let legacy = format!(
        "{}\nHTTP_PROXY=http://127.0.0.1:9999\n{}\n",
        codex_env::LEGACY_BEGIN_MARKER,
        codex_env::LEGACY_END_MARKER
    );
    sandbox.write_env(&format!("{USER_ENV}{legacy}"));
    let manager = sandbox.manager();
    assert!(manager.status().await.legacy_env_block);

    let status = manager.remove_legacy_env_block().await.unwrap();
    assert!(!status.legacy_env_block);
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    // 再次移除：无改动也成功
    manager.remove_legacy_env_block().await.unwrap();
}

#[tokio::test]
async fn set_autostart_flag_persists_and_creates_missing_state() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager();
    assert!(!manager.status().await.autostart);

    let status = manager.set_autostart_flag(true).await.unwrap();
    assert!(status.autostart && !status.enabled);
    assert_eq!(
        sandbox.state().unwrap(),
        HelperState {
            autostart: true,
            ..Default::default()
        }
    );
    // 新实例读到同样的值
    assert!(sandbox.manager().status().await.autostart);
    let status = manager.set_autostart_flag(false).await.unwrap();
    assert!(!status.autostart);
    assert!(!sandbox.state().unwrap().autostart);
}

// ---- 13. 连接记录 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_connections_are_recorded_and_forwarded() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    assert!(manager.recent_connections().is_empty());

    let target = spawn_fake_target().await;
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", sandbox.port))
        .await
        .unwrap();
    let request =
        format!("GET http://127.0.0.1:{target}/ping HTTP/1.1\r\nHost: 127.0.0.1:{target}\r\n\r\n");
    client.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let records = loop {
        let records = manager.recent_connections();
        if !records.is_empty() || tokio::time::Instant::now() > deadline {
            break records;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.host, "127.0.0.1");
    assert_eq!(record.port, target);
    assert_eq!(record.decision, Route::Direct);
    assert!(record.ok);
    // 回调先写环形缓冲、再调用外部回调：看到缓冲非空时外部回调可能尚未执行，带超时等待
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while sandbox.records.load(Ordering::SeqCst) < 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        sandbox.records.load(Ordering::SeqCst),
        1,
        "外部回调应被调用"
    );

    manager.disable().await.unwrap();
    // 停用后记录仍保留（界面展示）
    assert_eq!(manager.recent_connections().len(), 1);
    sandbox.assert_no_key_on_disk();
}

// ---- 15. 并发 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_enable_and_disable_are_serialized() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = Arc::new(sandbox.manager());

    for round in 0..4 {
        let mut tasks = Vec::new();
        for index in 0..6 {
            let worker = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                if (index + round) % 2 == 0 {
                    worker.enable(with_key(KEY)).await.map(|_| ())
                } else {
                    worker.disable().await.map(|_| ())
                }
            }));
            // 夹杂只读调用
            let reader = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                reader.status().await;
                Ok::<(), ManagerError>(())
            }));
        }
        for task in tasks {
            task.await.unwrap().expect("串行执行时每个操作都应成功");
        }

        let status = manager.status().await;
        let state = sandbox.state().unwrap_or_default();
        assert_eq!(status.enabled, state.enabled);
        if state.enabled {
            assert!(status.config_managed && status.env_managed);
            assert_eq!(state.previous_model_provider.as_deref(), Some("custom"));
            assert_eq!(status.proxy.state, ProxyState::Running);
            assert!(proxy::probe_existing(sandbox.port).await);
        } else {
            assert_eq!(
                read(&sandbox.config()),
                Some(USER_CONFIG.as_bytes().to_vec())
            );
            assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
            assert_eq!(state, HelperState::default());
            assert_eq!(status.proxy.state, ProxyState::Stopped);
            assert!(port_is_free(sandbox.port).await);
        }
        // 原子写不留临时文件
        for path in sandbox.snapshot().keys() {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(!name.starts_with(".codex-helper-"), "残留临时文件 {name}");
        }
    }

    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    sandbox.assert_no_key_on_disk();
}

// ---- 停用中途失败、状态文件损坏、状态与可达性 ----

/// 停用时配置还原失败：状态文件保持 `enabled = true`（previous 不清空），代理已停；
/// 修复后下次启动对账按“已启用”重新接管，再停用即可完整还原。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_keeps_enabled_flag_when_config_restore_fails() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    let state_before = sandbox.state().unwrap();

    let codex = sandbox.codex.path();
    let set_mode = |mode| {
        std::fs::set_permissions(codex, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    set_mode(0o555);
    // 以 root 运行时目录权限不生效，无法构造写入失败，跳过
    if std::fs::write(codex.join(".probe"), b"").is_ok() {
        let _ = std::fs::remove_file(codex.join(".probe"));
        set_mode(0o755);
        manager.shutdown().await;
        return;
    }
    let error = manager.disable().await.expect_err("目录不可写时停用应失败");
    set_mode(0o755);
    assert_eq!(error.code(), "io", "{error}");

    assert_eq!(
        sandbox.state().unwrap(),
        state_before,
        "配置未还原时不得置为停用"
    );
    let status = manager.status().await;
    assert!(status.enabled && status.config_managed && status.env_managed);
    assert!(!status.residue);
    assert_eq!(status.proxy.state, ProxyState::Stopped);

    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Applied);
    assert_eq!(report.status.proxy.state, ProxyState::Running);

    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert_eq!(sandbox.state().unwrap(), HelperState::default());
    sandbox.assert_no_key_on_disk();
}

/// 状态文件写入失败（数据目录不可写）：启用在“记录原值”一步失败，新 Key 回滚为旧值，
/// config.toml 与 .env 不被触碰，代理未启动。
#[cfg(unix)]
#[tokio::test]
async fn state_write_failure_rolls_back_credential() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.credentials.write(OLD_KEY).unwrap();
    let manager = sandbox.manager();
    let before = sandbox.snapshot();

    let data = sandbox.data.path();
    let set_mode = |mode| {
        std::fs::set_permissions(data, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    set_mode(0o555);
    if std::fs::write(data.join(".probe"), b"").is_ok() {
        let _ = std::fs::remove_file(data.join(".probe"));
        set_mode(0o755);
        return;
    }
    let result = manager.enable(with_key(KEY)).await;
    set_mode(0o755);
    assert_code(result, "io");

    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.credential().as_deref(), Some(OLD_KEY));
    assert!(port_is_free(sandbox.port).await);
    sandbox.assert_no_key_on_disk();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_state_file_is_quarantined_and_defaults_apply() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    std::fs::write(sandbox.state_file(), "{不是 JSON").unwrap();
    let corrupt = sandbox.paths().data_dir.join("state.json.corrupt");
    let manager = sandbox.manager();

    // status 不崩溃、不改名，按默认值展示
    let status = manager.status().await;
    assert!(!status.enabled && !status.autostart);
    assert!(!corrupt.exists());

    // 启用预检失败时同样不改名
    assert_code(
        manager.enable(EnableRequest::default()).await,
        "credentialMissing",
    );
    assert!(!corrupt.exists());

    let status = manager.enable(with_key(KEY)).await.unwrap();
    assert!(status.enabled);
    assert_eq!(read(&corrupt), Some("{不是 JSON".as_bytes().to_vec()));
    let state = sandbox.state().unwrap();
    assert!(state.enabled);
    assert_eq!(state.previous_model_provider.as_deref(), Some("custom"));

    manager.disable().await.unwrap();
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );

    // 设置自启时遇到损坏文件：改名（覆盖旧的 .corrupt）后按默认值写入
    std::fs::write(sandbox.state_file(), "[]").unwrap();
    manager.set_autostart_flag(true).await.unwrap();
    assert_eq!(read(&corrupt), Some(b"[]".to_vec()));
    assert_eq!(
        sandbox.state().unwrap(),
        HelperState {
            autostart: true,
            ..Default::default()
        }
    );
    sandbox.assert_no_key_on_disk();
}

#[tokio::test]
async fn status_reports_static_fields_and_file_inspection() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(CATALOG_CONFIG);
    let manager = sandbox.manager();
    let status = manager.status().await;
    let expected = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    };
    assert_eq!(status.platform, expected);
    assert_eq!(status.platform, consts::PLATFORM);
    assert_eq!(
        status.platform_supported,
        cfg!(any(windows, target_os = "macos"))
    );
    assert_eq!(
        status.update_supported,
        cfg!(any(windows, target_os = "macos"))
    );
    assert!(
        status.install_location_ok,
        "测试用 COMMAND 不是临时安装位置"
    );
    assert!(!status.enabled && !status.key_configured && !status.needs_key);
    assert_eq!(
        status.proxy,
        ProxyStatus {
            state: ProxyState::Stopped,
            port: sandbox.port
        }
    );
    assert_eq!(status.gateway, Reachability::Unknown);
    assert_eq!(status.socks5, Reachability::Unknown);
    assert_eq!(
        status.gateway_addr,
        format!("127.0.0.1:{}", sandbox.gateway.address().port())
    );
    assert_eq!(
        status.socks5_addr,
        format!("127.0.0.1:{}", sandbox.socks5_port)
    );
    assert_eq!(status.model_catalog_json.as_deref(), Some(CATALOG_PATH));
    assert_eq!(status.config_error, None);
    assert_eq!(
        status.codex_home,
        sandbox.codex.path().display().to_string()
    );
    assert!(!status.config_managed && !status.env_managed && !status.residue);

    // 结构不符：config_error 给出原因
    sandbox.write_config("model_providers = 1\n");
    let status = manager.status().await;
    let message = status.config_error.expect("应报告结构不符");
    assert!(message.contains("model_providers"), "{message}");
}

#[tokio::test]
async fn probe_reachability_caches_results() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager();
    let status = manager.probe_reachability().await;
    // 网关指向 wiremock（可连），SOCKS5 指向无人监听的端口
    assert_eq!(status.gateway, Reachability::Reachable);
    assert_eq!(status.socks5, Reachability::Unreachable);
    // 缓存：普通 status 不再探测但保留结果
    let status = manager.status().await;
    assert_eq!(status.gateway, Reachability::Reachable);
    assert_eq!(status.socks5, Reachability::Unreachable);
}

#[test]
fn manager_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Manager>();

    let dir = tempfile::tempdir().unwrap();
    let options = ManagerOptions {
        paths: HelperPaths::new(dir.path(), dir.path()),
        credentials: Arc::new(MemoryCredentialStore::new()),
        credential_command: COMMAND.into(),
        proxy_port: 1,
        socks5_host: "127.0.0.1".into(),
        socks5_port: 1,
        gateway_base_url: "http://127.0.0.1:1".into(),
        gateway_host: "127.0.0.1".into(),
        gateway_port: 1,
        on_record: None,
        secure_resolver: None,
        secure_drain_timeout: None,
    };
    let manager = Manager::new(options);
    // 只构造、不轮询：桌面命令层要求这些 future 可跨线程
    assert_send(&manager.status());
    assert_send(&manager.enable(EnableRequest::default()));
    assert_send(&manager.save_key(SaveKeyRequest::default()));
    assert_send(&manager.disable());
    assert_send(&manager.clear_key());
    assert_send(&manager.reconcile_on_startup());
    assert_send(&manager.cleanup_residue());
    assert_send(&manager.remove_legacy_env_block());
    assert_send(&manager.set_autostart_flag(true));
    assert_send(&manager.probe_reachability());
    assert_send(&manager.shutdown());
    assert_send(&manager.revoke_managed_config());
}

#[test]
fn production_options_use_fixed_parameters() {
    let options = ManagerOptions::production(Arc::new(MemoryCredentialStore::new()), None)
        .expect("生产配置应可解析");
    assert_eq!(options.gateway_base_url, consts::GATEWAY_BASE_URL);
    assert_eq!(options.gateway_host, consts::GATEWAY_HOST);
    assert_eq!(options.gateway_port, consts::GATEWAY_PORT);
    assert_eq!(options.socks5_host, consts::SOCKS5_HOST);
    assert_eq!(options.socks5_port, consts::SOCKS5_PORT);
    assert_eq!(options.proxy_port, proxy::proxy_port());
    assert!(
        options
            .credential_command
            .ends_with(consts::CREDENTIAL_EXE_NAME)
    );
    assert!(Path::new(&options.credential_command).is_absolute());
}

// ---- 16. 启用中断（崩溃窗口） ----

/// 模拟启用在“记录原值”之后、“置为已启用”之前崩溃：config.toml 已被接管、.env 已写入受管块，
/// 状态文件仍为停用但留有原值。
fn simulate_interrupted_enable(sandbox: &Sandbox) {
    sandbox.write_config(USER_CONFIG);
    codex_config::apply_managed(&sandbox.config(), &sandbox.backup(), COMMAND).unwrap();
    sandbox.write_env(&format!(
        "{USER_ENV}\n{}",
        codex_env::render_block(sandbox.port)
    ));
    sandbox.write_state(&HelperState {
        enabled: false,
        previous_model_provider: Some("custom".into()),
        previous_approval_policy: Some("untrusted".into()),
        previous_approvals_reviewer: Some("human_review".into()),
        previous_sandbox_mode: Some("read-only".into()),
        ..Default::default()
    });
}

fn assert_fully_restored(sandbox: &Sandbox) {
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec()),
        "应还原为用户原来的 provider，而不是删除 model_provider"
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert_eq!(sandbox.state().unwrap(), HelperState::default());
}

#[tokio::test]
async fn interrupted_enable_is_restored_by_disable() {
    let sandbox = Sandbox::new().await;
    simulate_interrupted_enable(&sandbox);
    let manager = sandbox.manager();

    let status = manager.status().await;
    assert!(!status.enabled && status.residue);
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::ResidueFound);

    let status = manager.disable().await.expect("停用应成功");
    assert!(!status.enabled && !status.residue);
    assert_fully_restored(&sandbox);
}

#[tokio::test]
async fn interrupted_enable_is_restored_by_cleanup_residue() {
    let sandbox = Sandbox::new().await;
    simulate_interrupted_enable(&sandbox);
    let status = sandbox.manager().cleanup_residue().await.unwrap();
    assert!(!status.residue);
    assert_fully_restored(&sandbox);
}

#[tokio::test]
async fn interrupted_enable_is_restored_by_uninstall_cleanup() {
    let sandbox = Sandbox::new().await;
    simulate_interrupted_enable(&sandbox);
    let report = manager::cleanup(&sandbox.paths(), sandbox.credentials.as_ref(), false).unwrap();
    assert_eq!(
        report,
        CleanupReport {
            env_block_removed: true,
            config_restored: true,
            state_reset: true,
            key_purged: false,
        }
    );
    assert_fully_restored(&sandbox);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_enable_after_interruption_keeps_recorded_previous() {
    let sandbox = Sandbox::new().await;
    simulate_interrupted_enable(&sandbox);
    let manager = sandbox.manager();

    let status = manager.enable(with_key(KEY)).await.expect("启用应成功");
    assert!(status.enabled && status.config_managed && status.env_managed);
    let state = sandbox.state().unwrap();
    assert!(state.enabled);
    assert_eq!(
        state.previous_model_provider.as_deref(),
        Some("custom"),
        "不得用受管残留（视为 None）覆盖已记录的原值"
    );

    manager.disable().await.unwrap();
    assert_fully_restored(&sandbox);
    sandbox.assert_no_key_on_disk();
}

// ---- 17. 停用状态下的停用 ----

/// 停用状态下再次停用：用户自己的 model_provider 保留（即使状态文件留有其他原值），受管残留节移除。
#[tokio::test]
async fn disable_when_disabled_keeps_user_provider_and_removes_residue() {
    for recorded in [None, Some("other")] {
        let sandbox = Sandbox::new().await;
        sandbox.write_state(&HelperState {
            enabled: false,
            previous_model_provider: recorded.map(str::to_string),
            ..Default::default()
        });
        sandbox.write_config(&format!(
            "{USER_CONFIG}\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n"
        ));
        let manager = sandbox.manager();
        assert!(manager.status().await.residue);

        let status = manager.disable().await.expect("停用应成功");
        assert!(!status.enabled && !status.residue);
        assert_eq!(
            read(&sandbox.config()),
            Some(USER_CONFIG.as_bytes().to_vec()),
            "{recorded:?}"
        );
        assert_eq!(sandbox.state().unwrap(), HelperState::default());
    }
}

/// 给文件加 macOS 的用户不可变标志（`chflags uchg`），使其无法被原子替换；drop 时恢复。
#[cfg(target_os = "macos")]
struct Immutable(PathBuf);

#[cfg(target_os = "macos")]
impl Immutable {
    fn set(path: &Path) -> Self {
        let status = std::process::Command::new("chflags")
            .arg("uchg")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "chflags uchg 失败");
        Self(path.to_path_buf())
    }
}

#[cfg(target_os = "macos")]
impl Drop for Immutable {
    fn drop(&mut self) {
        // 必须恢复，否则临时目录无法删除
        let _ = std::process::Command::new("chflags")
            .arg("nouchg")
            .arg(&self.0)
            .status();
    }
}

/// 停用时配置还原成功而 `.env` 块移除失败：返回 Io 错误，状态置为停用，`.env` 残留由 residue 提示，
/// 启动对账报告 ResidueFound；恢复可写后一键清理即可完成。
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_env_failure_after_config_restore_leaves_residue() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    let env_before = read(&sandbox.env());

    let immutable = Immutable::set(&sandbox.env());
    let error = manager
        .disable()
        .await
        .expect_err(".env 不可替换时停用应失败");
    assert_eq!(error.code(), "io", "{error}");
    assert!(error.to_string().contains(".env"), "{error}");

    assert_eq!(read(&sandbox.env()), env_before, ".env 未被改动");
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec()),
        "配置仍应还原"
    );
    assert_eq!(
        sandbox.state().unwrap(),
        HelperState::default(),
        "配置已还原：状态置为停用"
    );
    let status = manager.status().await;
    assert!(!status.enabled && status.residue && !status.config_managed);
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert!(port_is_free(sandbox.port).await);

    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::ResidueFound);
    assert!(port_is_free(sandbox.port).await, "停用状态下对账不启动代理");

    drop(immutable);
    let status = manager.cleanup_residue().await.unwrap();
    assert!(!status.residue);
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    sandbox.assert_no_key_on_disk();
}

// ---- 18. 锁粒度 ----

/// Key 校验在操作锁之外：假网关延迟 6 秒时，启用进行中 status / probe_reachability / shutdown
/// 都立即返回（以「启用尚未完成」表达不等待语义，避免 CI 高负载下的墙钟抖动），启用随后照常完成。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_does_not_wait_for_slow_key_check() {
    let sandbox = Sandbox::with_slow_gateway(Duration::from_secs(6)).await;
    sandbox.write_config(USER_CONFIG);
    let manager = Arc::new(sandbox.manager());

    let worker = Arc::clone(&manager);
    let enabling = tokio::spawn(async move { worker.enable(with_key(KEY)).await });
    // 等到 Key 校验请求已发出（启用正在等待网关响应）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while sandbox.gateway_hits().await == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(sandbox.gateway_hits().await, 1);

    let status = manager.status().await;
    assert!(!status.enabled);
    assert!(!enabling.is_finished(), "status 应在 Key 校验期间返回");

    let status = manager.probe_reachability().await;
    assert_eq!(status.gateway, Reachability::Reachable);
    assert!(
        !enabling.is_finished(),
        "probe_reachability 应在 Key 校验期间返回"
    );

    manager.shutdown().await;
    assert!(!enabling.is_finished(), "shutdown 应在 Key 校验期间返回");

    let status = enabling.await.unwrap().expect("启用应成功");
    assert!(status.enabled && status.config_managed && status.env_managed);
    assert_eq!(status.proxy.state, ProxyState::Running);
    // 可达性缓存不被启用覆盖
    assert_eq!(status.gateway, Reachability::Reachable);
    manager.shutdown().await;
    sandbox.assert_no_key_on_disk();
}

/// 锁外校验期间文件被改动：锁内重新预检，以最新内容为准（catalog 指针需确认时不写入任何内容）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enable_rechecks_preflight_after_key_check() {
    let sandbox = Sandbox::with_slow_gateway(Duration::from_secs(3)).await;
    sandbox.write_config(USER_CONFIG);
    let manager = Arc::new(sandbox.manager());

    let worker = Arc::clone(&manager);
    let enabling = tokio::spawn(async move { worker.enable(with_key(KEY)).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while sandbox.gateway_hits().await == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 校验期间用户加入了 catalog 指针
    sandbox.write_config(CATALOG_CONFIG);
    let before = sandbox.snapshot();

    match enabling.await.unwrap() {
        Err(ManagerError::CatalogConfirmationRequired { path }) => assert_eq!(path, CATALOG_PATH),
        other => panic!("应要求确认 catalog，实际 {other:?}"),
    }
    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.credential(), None);
    assert!(port_is_free(sandbox.port).await);
}

// ---- 19. 安装位置 ----

#[cfg(target_os = "macos")]
const INSTALL_LOCATION_MESSAGE: &str = "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开";

#[cfg(target_os = "macos")]
#[tokio::test]
async fn status_flags_transient_install_location() {
    let sandbox = Sandbox::new().await;
    for command in [TRANSIENT_COMMAND, VOLUME_COMMAND] {
        let manager = Manager::new(sandbox.options_with(command));
        assert!(!manager.status().await.install_location_ok, "{command}");
    }
    assert!(sandbox.manager().status().await.install_location_ok);
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_from_transient_location_is_rejected_without_writes() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let before = sandbox.snapshot();

    let manager = Manager::new(sandbox.options_with(TRANSIENT_COMMAND));
    let error = assert_code(manager.enable(with_key(KEY)).await, "installLocation");
    assert_eq!(error.to_string(), INSTALL_LOCATION_MESSAGE);

    sandbox.credentials.write(OLD_KEY).unwrap();
    assert_code(
        manager.enable(EnableRequest::default()).await,
        "installLocation",
    );
    let volume = Manager::new(sandbox.options_with(VOLUME_COMMAND));
    assert_code(volume.enable(with_key(KEY)).await, "installLocation");

    assert_eq!(sandbox.snapshot(), before);
    assert_eq!(sandbox.gateway_hits().await, 0);
    assert_eq!(sandbox.credential().as_deref(), Some(OLD_KEY));
    assert!(port_is_free(sandbox.port).await);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn autostart_on_from_transient_location_is_rejected() {
    let sandbox = Sandbox::new().await;
    let manager = Manager::new(sandbox.options_with(TRANSIENT_COMMAND));
    let error = assert_code(manager.set_autostart_flag(true).await, "installLocation");
    assert_eq!(error.to_string(), INSTALL_LOCATION_MESSAGE);
    assert!(!sandbox.state_file().exists());
    let status = manager.set_autostart_flag(false).await.unwrap();
    assert!(!status.autostart);
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_from_transient_location_keeps_previous_command() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let first = sandbox.manager();
    first.enable(with_key(KEY)).await.unwrap();
    first.shutdown().await;
    let state_before = sandbox.state();

    let manager = Manager::new(sandbox.options_with(TRANSIENT_COMMAND));
    let status = manager.status().await;
    assert!(!status.install_location_ok);
    assert!(status.config_managed, "按旧的 auth.command 判定受管");

    // 制造漂移：base_url 被改、.env 被删
    let drifted = sandbox
        .read_config()
        .replace(consts::GATEWAY_BASE_URL, "http://evil.invalid:1");
    sandbox.write_config(&drifted);
    std::fs::remove_file(sandbox.env()).unwrap();

    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Applied);
    assert_eq!(report.error, None);
    assert!(report.status.config_managed && report.status.env_managed);
    assert!(!report.status.install_location_ok);
    assert_eq!(report.status.proxy.state, ProxyState::Running);
    let text = sandbox.read_config();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc["model_providers"]["managed_gateway"]["auth"]["command"].as_str(),
        Some(COMMAND)
    );
    assert!(!text.contains(TRANSIENT_COMMAND) && !text.contains("evil.invalid"));
    assert_eq!(sandbox.state(), state_before);

    manager.disable().await.unwrap();
    assert_eq!(sandbox.read_config(), USER_CONFIG);
    sandbox.assert_no_key_on_disk();
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_from_transient_location_without_managed_command_fails_untouched() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    sandbox.write_state(&HelperState {
        enabled: true,
        ..Default::default()
    });
    sandbox.credentials.write(KEY).unwrap();
    let before = sandbox.snapshot();

    let manager = Manager::new(sandbox.options_with(TRANSIENT_COMMAND));
    let report = manager.reconcile_on_startup().await;
    assert_eq!(report.outcome, ReconcileOutcome::Failed);
    assert_eq!(report.error.unwrap().code, "installLocation");
    assert_eq!(report.status.proxy.state, ProxyState::Stopped);
    assert_eq!(sandbox.snapshot(), before);
    assert!(port_is_free(sandbox.port).await);
}

#[cfg(not(target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_location_is_ignored_off_macos() {
    let sandbox = Sandbox::new().await;
    for command in [TRANSIENT_COMMAND, VOLUME_COMMAND] {
        let manager = Manager::new(sandbox.options_with(command));
        assert!(manager.status().await.install_location_ok, "{command}");
    }
    let manager = Manager::new(sandbox.options_with(TRANSIENT_COMMAND));
    manager.set_autostart_flag(true).await.unwrap();
    manager.enable(with_key(KEY)).await.unwrap();
    let doc: toml_edit::DocumentMut = sandbox.read_config().parse().unwrap();
    assert_eq!(
        doc["model_providers"]["managed_gateway"]["auth"]["command"].as_str(),
        Some(TRANSIENT_COMMAND)
    );
    manager.shutdown().await;
}

// ---- 20. 撤销受管配置 ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_restores_config_keeps_key_and_stops_proxy() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    manager.set_autostart_flag(true).await.unwrap();

    let status = manager.revoke_managed_config().await.unwrap();
    assert!(!status.enabled && !status.residue && status.key_configured && status.autostart);
    assert_eq!(status.proxy.state, ProxyState::Stopped);
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec())
    );
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert_eq!(sandbox.credential().as_deref(), Some(KEY), "不删 Key");
    assert_eq!(
        sandbox.state(),
        Some(HelperState {
            autostart: true,
            ..Default::default()
        }),
        "不改 autostart"
    );
    assert!(port_is_free(sandbox.port).await);

    // 幂等
    let before = sandbox.snapshot();
    manager.revoke_managed_config().await.unwrap();
    assert_eq!(sandbox.snapshot(), before);
    sandbox.assert_no_key_on_disk();

    // 全新环境：什么都不创建
    let fresh = Sandbox::new().await;
    let before = fresh.snapshot();
    fresh.manager().revoke_managed_config().await.unwrap();
    assert_eq!(fresh.snapshot(), before);
}

/// 撤销中途 `.env` 块移除失败：返回原错误，配置照常还原；`.env` 块仍在，代理被重新拉起，
/// Codex 不会指向无人监听的端口。
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_failure_restarts_proxy_when_env_block_remains() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    let env_before = read(&sandbox.env());

    let immutable = Immutable::set(&sandbox.env());
    let error = manager
        .revoke_managed_config()
        .await
        .expect_err(".env 不可替换时撤销应失败");
    assert_eq!(error.code(), "io", "{error}");
    assert!(error.to_string().contains(".env"), "{error}");
    assert_eq!(read(&sandbox.env()), env_before, ".env 未被改动");
    assert_eq!(
        read(&sandbox.config()),
        Some(USER_CONFIG.as_bytes().to_vec()),
        "cleanup 继续完成配置还原"
    );
    assert!(
        proxy::probe_existing(sandbox.port).await,
        "代理应被重新拉起"
    );
    let status = manager.status().await;
    assert_eq!(status.proxy.state, ProxyState::Running);
    // 现有 cleanup 语义：配置还原成功即置为停用，`.env` 块仍在 → 残留
    assert!(!sandbox.state().unwrap().enabled);
    assert!(status.residue);

    drop(immutable);
    let status = manager.revoke_managed_config().await.unwrap();
    assert_eq!(read(&sandbox.env()), Some(USER_ENV.as_bytes().to_vec()));
    assert!(!status.residue);
    assert!(port_is_free(sandbox.port).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_failure_without_env_block_leaves_proxy_stopped() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();
    sandbox.write_config("model = [\n");

    let error = manager
        .revoke_managed_config()
        .await
        .expect_err("配置无法解析时撤销应失败");
    assert_eq!(error.code(), "configInvalid", "{error}");
    assert_eq!(
        read(&sandbox.env()),
        Some(USER_ENV.as_bytes().to_vec()),
        "块已移除"
    );
    assert!(sandbox.state().unwrap().enabled, "配置未还原：状态保持启用");
    assert!(port_is_free(sandbox.port).await);
    assert_eq!(manager.status().await.proxy.state, ProxyState::Stopped);
}

/// 撤销失败且 `.env` 无法读取（这里被换成目录）：无法确认受管块已移除，按“块仍在”处理，
/// 代理被重新拉起（macOS 计划 C6 行为第 4 条）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_failure_restarts_proxy_when_env_unreadable() {
    let sandbox = Sandbox::new().await;
    sandbox.write_config(USER_CONFIG);
    sandbox.write_env(USER_ENV);
    let manager = sandbox.manager();
    manager.enable(with_key(KEY)).await.unwrap();

    std::fs::remove_file(sandbox.env()).unwrap();
    std::fs::create_dir(sandbox.env()).unwrap();
    manager
        .revoke_managed_config()
        .await
        .expect_err(".env 无法读取时撤销应失败");
    assert!(
        proxy::probe_existing(sandbox.port).await,
        "代理应被重新拉起"
    );
    assert_eq!(manager.status().await.proxy.state, ProxyState::Running);

    manager.shutdown().await;
    std::fs::remove_dir(sandbox.env()).unwrap();
}

// ---- Secure 模式（设计 §6.3、§8.1、§9.1，Task 9）----

mod secure {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use helper_core::peer_identity::{PeerInfo, PeerResolver};
    use helper_core::process_policy::{self as policy_mod, ROLE_CODEX_ENGINE};
    use helper_core::proxy_identity::{self, ProxySecrets};
    use helper_core::state::ProxyMode;
    use helper_core::types::SecureStatus;
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair};
    use std::net::IpAddr;

    /// 发送原始请求并读到连接关闭（本模块内联版，与 tests/proxy.rs 同语义）。
    async fn send_and_read(port: u16, request: &[u8]) -> String {
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect proxy");
        client.write_all(request).await.expect("send request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut response))
            .await
            .expect("代理应在应答后关闭连接")
            .expect("read response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// 2100-01-01：测试运行的任何时间都早于它。
    const FAR_FUTURE: u64 = 4_102_444_800;

    /// 与 `consts::POLICY_VERIFY_KEY_HEX`（种子 0x00..0x1f）配对的管理签名私钥，
    /// 仅测试夹具（Task 8 已声明该常量当前是测试 CA 公钥）。
    fn policy_signing_key() -> SigningKey {
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = i as u8;
        }
        SigningKey::from_bytes(&seed)
    }

    /// 随机 device_id（可打印 ASCII）：macOS 真 Keychain target 因此必带随机后缀，
    /// 绝不触碰生产条目；其他平台落在进程内 dev 表。
    fn random_device_id(tag: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("sec-{tag}-{pid}-{nanos}-{seq}")
    }

    /// 结束（含 panic）删除两条代理秘密条目。
    struct SecretsGuard(String);

    impl Drop for SecretsGuard {
        fn drop(&mut self) {
            proxy_identity::delete_secrets(&self.0);
        }
    }

    /// 测试证书链：CA + 客户端证书（clientAuth EKU）+ 客户端私钥。
    struct TestCerts {
        ca_pem: String,
        client_cert_pem: String,
        client_key_pem: String,
    }

    fn test_certs() -> TestCerts {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "codex-helper-manager-test-ca");
        let ca_key = KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();

        let mut client_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        client_params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ClientAuth);
        client_params
            .distinguished_name
            .push(DnType::CommonName, "codex-helper-manager-test-device");
        let client_key = KeyPair::generate().unwrap();
        let client_cert = client_params.self_signed(&client_key).unwrap();

        TestCerts {
            ca_pem: ca.pem(),
            client_cert_pem: client_cert.pem(),
            client_key_pem: client_key.serialize_pem(),
        }
    }

    /// 签发策略包（对规范字节签名）并返回 (bundle_json, signature_hex)。
    /// 策略批准的映像摘要是 `sha`（os/arch 取当前值）。
    fn issue_policy(version: u64, sha: &str) -> (String, String) {
        let policy_value = serde_json::json!({
            "policy_id": "manager-test-policy",
            "version": version,
            "not_after_unix": FAR_FUTURE,
            "roles": [{
                "role": ROLE_CODEX_ENGINE,
                "images": [{
                    "sha256_hex": sha,
                    "os": policy_mod::current_os(),
                    "arch": policy_mod::current_arch(),
                }],
                "path_prefixes": [],
            }],
        });
        // 规范字节 = 递归键排序后的 serde_json 紧凑序列化（与 process_policy 约定一致）
        let canonical = canonical_bytes(&policy_value);
        let sig = policy_signing_key().sign(&canonical);
        let bundle = serde_json::json!({ "policy": policy_value });
        (bundle.to_string(), hex_lower(&sig.to_bytes()))
    }

    fn canonical_bytes(value: &serde_json::Value) -> Vec<u8> {
        fn sort(value: &serde_json::Value) -> serde_json::Value {
            match value {
                serde_json::Value::Object(map) => {
                    let mut entries: Vec<(String, serde_json::Value)> =
                        map.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    entries.into_iter().collect::<serde_json::Value>()
                }
                serde_json::Value::Array(items) => {
                    serde_json::Value::Array(items.iter().map(sort).collect())
                }
                other => other.clone(),
            }
        }
        serde_json::to_vec(&sort(value)).unwrap()
    }

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 写临时映像文件，返回其 SHA-256 摘要。
    fn temp_image_sha(content: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-engine.bin");
        std::fs::write(&path, content).unwrap();
        policy_mod::hash_image_file(&path).unwrap()
    }

    /// Secure 激活夹具：证书链 + 策略 + 秘密 + 注册包文件。
    struct SecureFixture {
        device_id: String,
        _secrets: SecretsGuard,
        bundle_path: PathBuf,
    }

    /// 生成完整注册包并写入 `sandbox`；返回夹具。
    async fn setup_secure_enrollment(sandbox: &Sandbox, policy_version: u64) -> SecureFixture {
        let certs = test_certs();
        let device_id = random_device_id("fx");
        let secrets_guard = SecretsGuard(device_id.clone());
        proxy_identity::save_secrets(
            &device_id,
            &ProxySecrets {
                username: format!("device-{device_id}"),
                password: helper_core::credential::Secret::new("manager-test-pass"),
                client_key_pem: helper_core::credential::Secret::new(certs.client_key_pem.clone()),
            },
        )
        .expect("写入代理秘密");

        let sha = temp_image_sha(b"manager-test-engine-image");
        let (policy_json, signature_hex) = issue_policy(policy_version, &sha);
        let bundle = serde_json::json!({
            "device_id": device_id,
            "server_host": "127.0.0.1",
            "server_port": 1, // 无需真实上游：激活不连上游（连接时 fail-closed 由 upstream 覆盖）
            "server_name": "localhost",
            "ca_pem": certs.ca_pem,
            "client_cert_pem": certs.client_cert_pem,
            "policy_bundle_json": policy_json,
            "policy_signature": signature_hex,
            "not_before_unix": 1,
            "not_after_unix": FAR_FUTURE,
        });
        let bundle_path = sandbox.paths().data_dir.join("enrollment-draft.json");
        std::fs::write(&bundle_path, bundle.to_string()).unwrap();

        SecureFixture {
            device_id,
            _secrets: secrets_guard,
            bundle_path,
        }
    }

    /// 恒定返回固定 PeerInfo 的假解析器。
    struct FakeResolver {
        peer: PeerInfo,
        fail: bool,
    }

    impl PeerResolver for FakeResolver {
        fn resolve(
            &self,
            _local: (IpAddr, u16),
            _remote: (IpAddr, u16),
        ) -> anyhow::Result<PeerInfo> {
            if self.fail {
                anyhow::bail!("fake resolver failure");
            }
            Ok(self.peer.clone())
        }
    }

    fn fake_peer() -> PeerInfo {
        PeerInfo {
            pid: 4321,
            user_scope: helper_core::peer_identity::current_user_scope(),
            start_time_ms: 1_700_000_000_000,
            image_path: "/nonexistent/codex-engine".into(),
        }
    }

    /// 带 fake resolver 的 Sandbox（每次激活取一个新实例）。
    async fn secure_sandbox() -> (Sandbox, Manager) {
        let sandbox = Sandbox::new().await;
        // 启用 Legacy（记录 previous_、写 .env、启动旧句柄——供排空/切换断言）
        sandbox.write_config(USER_CONFIG);
        sandbox.write_env(USER_ENV);
        let peer = fake_peer();
        let mut options = sandbox.options_with(COMMAND);
        options.secure_resolver = Some(Arc::new(move || {
            Box::new(FakeResolver {
                peer: peer.clone(),
                fail: false,
            })
        }));
        options.secure_drain_timeout = Some(Duration::from_millis(200));
        let manager = Manager::new(options);
        (sandbox, manager)
    }

    /// 预先占住 `port` 的假探测应答器：模拟另一个 CodexHelper 实例（自识端点应答）。
    async fn spawn_fake_helper_instance(port: u16) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind fake helper");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 27\r\nConnection: close\r\n\r\ncodex-helper-managed-proxy",
                        )
                        .await;
                });
            }
        })
    }

    /// 状态文件写入 Secure 模式（重启对账用）。
    fn write_secure_state(sandbox: &Sandbox, fixture: &SecureFixture, enabled: bool) {
        sandbox.write_state(&HelperState {
            enabled,
            mode: ProxyMode::Secure,
            secure_device_id: Some(fixture.device_id.clone()),
            secure_policy_id: Some("manager-test-policy".into()),
            secure_policy_version: Some(1),
            ..Default::default()
        });
    }

    // ---- 测试 ----

    /// 激活后排空旧 Legacy 句柄、以本进程 Secure 句柄监听、状态反映 SecureActive。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_uses_own_handle_and_drains_legacy() {
        let (sandbox, manager) = secure_sandbox().await;
        manager.enable(with_key(KEY)).await.unwrap();
        assert_eq!(manager.status().await.proxy.state, ProxyState::Running);
        assert_eq!(manager.status().await.mode, ProxyMode::Legacy);

        let fixture = setup_secure_enrollment(&sandbox, 1).await;
        let status = manager
            .activate_secure(&fixture.bundle_path)
            .await
            .expect("激活应成功");

        // SecureActive：本进程持有 Secure 句柄，模式与状态文件都已切换
        assert_eq!(status.mode, ProxyMode::Secure);
        assert_eq!(
            status.secure,
            SecureStatus {
                active: true,
                device_id: Some(fixture.device_id.clone()),
                blocked_reason: None,
            }
        );
        assert_eq!(status.proxy.state, ProxyState::Running);
        let state = sandbox.state().unwrap();
        assert_eq!(state.mode, ProxyMode::Secure);
        assert_eq!(
            state.secure_device_id.as_deref(),
            Some(fixture.device_id.as_str())
        );
        assert_eq!(state.secure_policy_version, Some(1));
        assert_eq!(state.secure_blocked_reason, None);
        // 注册包被归档（对账重建用）
        assert!(sandbox.paths().enrollment_bundle().exists());

        // 端口上仍是本工具在监听（新 Secure 句柄）
        assert!(proxy::probe_existing(sandbox.port).await);

        // 停用回滚：回 Legacy、已启用时重启 Legacy 代理；策略版本记录保留
        let status = manager.deactivate_secure().await.unwrap();
        assert_eq!(status.mode, ProxyMode::Legacy);
        assert_eq!(status.proxy.state, ProxyState::Running);
        assert!(!status.secure.active);
        let state = sandbox.state().unwrap();
        assert_eq!(state.mode, ProxyMode::Legacy);
        assert_eq!(
            state.secure_policy_version,
            Some(1),
            "版本记录不得因停用丢失"
        );
        manager.shutdown().await;
        sandbox.assert_no_key_on_disk();
    }

    /// 端口被外部进程（假探测应答器）占用：激活失败，状态非 External，模式不变。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn secure_mode_never_reuses_external() {
        let (sandbox, manager) = secure_sandbox().await;
        let fixture = setup_secure_enrollment(&sandbox, 1).await;
        // 伪装成另一 CodexHelper 实例的假探测应答器（自识端点 200 + 固定 body）
        let fake = spawn_fake_helper_instance(sandbox.port).await;

        let error = manager
            .activate_secure(&fixture.bundle_path)
            .await
            .expect_err("Secure 不得复用外部实例");
        assert_eq!(error.code(), "securePortOccupied", "{error}");
        assert!(error.to_string().contains("独占"), "{error}");
        let status = manager.status().await;
        assert_ne!(status.proxy.state, ProxyState::External);
        assert_eq!(status.mode, ProxyMode::Legacy, "激活失败不得切换模式");
        assert!(!status.secure.active);
        // 状态文件记录 blocked_reason，模式未变
        let state = sandbox.state().unwrap();
        assert_eq!(state.mode, ProxyMode::Legacy);
        assert_eq!(
            state.secure_blocked_reason.as_deref(),
            Some("port_occupied")
        );

        // 对账同样不得进入 External：写入 Secure 状态后重启对账必须失败
        manager.shutdown().await;
        write_secure_state(&sandbox, &fixture, true);
        sandbox.credentials.write(KEY).unwrap();
        let manager2 = sandbox.manager();
        manager2.sync_mode_from_state();
        let report = manager2.reconcile_on_startup().await;
        assert_eq!(report.outcome, ReconcileOutcome::Failed);
        assert_eq!(report.error.as_ref().unwrap().code, "securePortOccupied");
        assert_ne!(report.status.proxy.state, ProxyState::External);

        fake.abort();
        sandbox.assert_no_key_on_disk();
    }

    /// 端口被无关程序占用（bind 失败）：激活失败且不进入 External。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn port_occupied_blocks_activation() {
        let (sandbox, manager) = secure_sandbox().await;
        let fixture = setup_secure_enrollment(&sandbox, 1).await;
        // 静默占用 sandbox.port（不自识应答）：probe 不命中，spawn 时 bind 失败
        let foreign = tokio::net::TcpListener::bind(("127.0.0.1", sandbox.port))
            .await
            .expect("夹具应能占用 sandbox 端口");
        let foreign_task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = foreign.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                });
            }
        });

        let error = manager
            .activate_secure(&fixture.bundle_path)
            .await
            .expect_err("端口被占时激活必须失败");
        assert_eq!(error.code(), "securePortOccupied", "{error}");
        let status = manager.status().await;
        assert_ne!(status.proxy.state, ProxyState::External);
        assert_eq!(status.proxy.state, ProxyState::Stopped);
        assert_eq!(status.mode, ProxyMode::Legacy);
        let state = sandbox.state().unwrap();
        assert_eq!(
            state.secure_blocked_reason.as_deref(),
            Some("port_unavailable")
        );
        foreign_task.abort();
        sandbox.assert_no_key_on_disk();
    }

    /// 管理签名验证失败：拒绝激活，不改状态文件的模式。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_signature_rejects_activation() {
        let (sandbox, manager) = secure_sandbox().await;
        let fixture = setup_secure_enrollment(&sandbox, 1).await;
        // 篡改注册包：换一个 device_id（策略字节不变、签名失效）
        let raw = std::fs::read_to_string(&fixture.bundle_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["device_id"] = "tampered-device".into();
        let tampered = sandbox.paths().data_dir.join("tampered.json");
        std::fs::write(&tampered, value.to_string()).unwrap();

        let error = manager
            .activate_secure(&tampered)
            .await
            .expect_err("签名验证失败必须拒绝激活");
        assert_eq!(error.code(), "secureActivation", "{error}");
        let status = manager.status().await;
        assert_eq!(status.mode, ProxyMode::Legacy);
        assert!(!status.secure.active);
        // 状态文件不应记录 Secure
        let state = sandbox.state();
        assert!(state.is_none() || state.unwrap().mode == ProxyMode::Legacy);
        sandbox.assert_no_key_on_disk();
    }

    /// 策略版本倒退（Task 6 交接）：新注册包版本低于已见版本 → 拒绝；停用后也不放行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policy_version_rollback_rejected_even_after_deactivation() {
        let (sandbox, manager) = secure_sandbox().await;
        let v2 = setup_secure_enrollment(&sandbox, 2).await;
        manager.activate_secure(&v2.bundle_path).await.unwrap();

        // 版本 1 < 已见 2：激活新注册包被拒
        let v1 = setup_secure_enrollment(&sandbox, 1).await;
        let error = manager
            .activate_secure(&v1.bundle_path)
            .await
            .expect_err("版本回退必须拒绝");
        assert_eq!(error.code(), "policyVersionRollback", "{error}");
        assert_eq!(sandbox.state().unwrap().secure_policy_version, Some(2));

        // 停用回 Legacy 后重试：版本记录保留，仍然拒绝
        manager.deactivate_secure().await.unwrap();
        let error = manager
            .activate_secure(&v1.bundle_path)
            .await
            .expect_err("停用后版本回退同样必须拒绝");
        assert_eq!(error.code(), "policyVersionRollback", "{error}");
        manager.shutdown().await;
        sandbox.assert_no_key_on_disk();
    }

    /// 激活后 Secure 代理实际生效：命中 OpenAI 域名的请求先过门禁（fake resolver 放行，
    /// 映像摘要命中策略）再走安全上游；上游不可达时 502，绝不回退 Legacy SOCKS5。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn secure_proxy_gates_and_uses_secure_upstream() {
        let (sandbox, _first) = secure_sandbox().await;

        let fixture = setup_secure_enrollment(&sandbox, 1).await;
        // 让 fake peer 的映像路径指向真实临时文件（摘要已入策略）
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("codex-engine.bin");
        std::fs::write(&image, b"manager-test-engine-image").unwrap();
        let mut options = sandbox.options_with(COMMAND);
        let image_path = image.to_string_lossy().into_owned();
        options.secure_resolver = Some(Arc::new(move || {
            Box::new(FakeResolver {
                peer: PeerInfo {
                    pid: 4321,
                    user_scope: helper_core::peer_identity::current_user_scope(),
                    start_time_ms: 1_700_000_000_000,
                    image_path: image_path.clone(),
                },
                fail: false,
            })
        }));
        options.secure_drain_timeout = Some(Duration::from_millis(200));
        let manager = Manager::new(options);
        manager.enable(with_key(KEY)).await.unwrap();

        let status = manager.activate_secure(&fixture.bundle_path).await.unwrap();
        assert!(status.secure.active);

        // 准入放行 + 上游（127.0.0.1:1，无人监听）失败 → 502；端口 1 不会命中 Legacy 上游
        // （sandbox.socks5_port 也是空闲端口，若回退 Legacy 会连它——同样是连接拒绝，
        // 但 502 已证明 fail-closed）。关键断言：不回 403（准入通过）、不挂起。
        let response = send_and_read(
            status.proxy.port,
            b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com\r\n\r\n",
        )
        .await;
        assert!(response.contains("502 Bad Gateway"), "got: {response}");

        // 未知映像（摘要不命中）：403，零出站
        let mut tampered_bundle = std::fs::read_to_string(&fixture.bundle_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&tampered_bundle).unwrap();
        // 重发一个批准其它摘要的注册包（重签）
        let other_sha = temp_image_sha(b"other-image");
        let (policy_json, signature_hex) = issue_policy(3, &other_sha);
        value["policy_bundle_json"] = serde_json::Value::String(policy_json);
        value["policy_signature"] = serde_json::Value::String(signature_hex);
        tampered_bundle = value.to_string();
        let path = sandbox.paths().data_dir.join("v3.json");
        std::fs::write(&path, tampered_bundle).unwrap();
        manager.activate_secure(&path).await.unwrap();

        let response = send_and_read(
            status.proxy.port,
            b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com\r\n\r\n",
        )
        .await;
        assert!(response.contains("403 Forbidden"), "got: {response}");

        manager.shutdown().await;
        sandbox.assert_no_key_on_disk();
    }

    /// Legacy 全链路回归：默认模式仍允许 External 复用（既有行为不变）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_reuse_still_works() {
        let sandbox = Sandbox::new().await;
        sandbox.write_config(USER_CONFIG);
        let other = proxy::spawn(sandbox.port, ProxyConfig::default())
            .await
            .unwrap();
        let manager = sandbox.manager();

        let status = manager.enable(with_key(KEY)).await.unwrap();
        assert_eq!(
            status.proxy.state,
            ProxyState::External,
            "Legacy 仍复用外部实例"
        );
        assert_eq!(status.mode, ProxyMode::Legacy);
        other.shutdown().await;
        sandbox.assert_no_key_on_disk();
    }
}
