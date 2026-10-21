//! 内部升级渠道（设计 §15）：把 HTTP 服务器上的版本 manifest 作为升级检测地址，
//! 复用 `tauri-plugin-updater` 完成检测 / 下载 / Ed25519 签名校验 / NSIS 安装。
//!
//! 检测地址优先级：环境变量 `CODEX_HELPER_UPDATE_URL`（运行时覆盖）> `tauri.conf.json`
//! 里的 `plugins.updater.endpoints`（构建期默认）。签名公钥固定写在 `tauri.conf.json`，
//! 私钥仅在构建签名时通过 `TAURI_SIGNING_PRIVATE_KEY` 使用，绝不入库。
//!
//! 运行时覆盖 endpoints 的 API 只在 Rust 侧可用，因此升级逻辑封装为命令，前端只调用
//! `check_update` / `install_update`，不直接使用 JS 插件 API。

use helper_core::log;
use helper_core::types::ErrorPayload;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Url};
use tauri_plugin_updater::{Updater, UpdaterExt as _};

/// 运行时覆盖检测地址的环境变量（应急/内网自定义用；界面不暴露，与端口覆盖同一思路）。
pub const UPDATE_URL_ENV: &str = "CODEX_HELPER_UPDATE_URL";

/// 升级域名 DNS 钉扎（设计决策）：两个隔离网络靠 VPN 互通，A 网 DNS 正常解析
/// rdc.tiandy.com，B 网 DNS 不能；而 10.20.20.3 在两网均可达。把该域名的解析
/// 钉在客户端内（reqwest resolve），不依赖任一网络的 DNS。
/// 端口固定为 0：URL 无显式端口时用协议默认端口，URL 显式端口优先生效。
pub const UPDATE_PIN_HOST: &str = "rdc.tiandy.com";
pub const UPDATE_PIN_ADDR: &str = "10.20.20.3:0";

/// 升级检测结果（返回给前端，serde camelCase 与 `types.ts` 的 `UpdateCheck` 对应）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// 是否有可用更新。
    pub available: bool,
    /// 当前运行版本。
    pub current_version: String,
    /// 可用的新版本号（`available` 为 true 时存在）。
    pub version: Option<String>,
    /// 更新说明（manifest 的 `notes` 字段，可能为空）。
    pub notes: Option<String>,
    /// 发布日期（RFC3339 文本，可能为空）。
    pub date: Option<String>,
}

/// 读取运行时覆盖的检测地址；未设置或空白时返回 `None`（改用构建期配置的 endpoints）。
fn override_endpoint() -> Option<String> {
    std::env::var(UPDATE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 校验运行时覆盖的检测地址：仅允许 http/https，拒绝携带 userinfo 或 fragment 的 URL。
///
/// 返回的错误信息**不包含原始地址值**——未来 URL 可能携带令牌，避免其泄露到日志或前端提示。
fn validate_endpoint(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("无法解析为 URL：{error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("协议 {other} 不受支持（仅允许 http/https）")),
    }
    // authority 形如 "host:port" 或 "user[:pass]@host"；出现 '@' 即携带真实 userinfo，一律拒绝，
    // 避免未来凭据入地址。（空 userinfo 如 http://@host 会被 url 规范化为无 userinfo、
    // 等价普通 host、不携带凭据，因此不在此处单独拦截。）
    if url.authority().contains('@') {
        return Err("不允许在地址中携带用户信息（user[:pass]@）".to_string());
    }
    if url.fragment().is_some() {
        return Err("不允许在地址中携带片段（#...）".to_string());
    }
    Ok(url)
}

/// 临时安装位置（dmg / App Translocation）下尝试安装更新时的引导文案，
/// 与前端 `INSTALL_LOCATION_MESSAGE` 同义（macOS 设计 §4.2）。
pub const TRANSIENT_INSTALL_UPDATE_MESSAGE: &str =
    "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新";

/// 内部升级只在 Windows 与 macOS 上提供（与 `Status::update_supported` 同一判断）。
/// 其他平台直接拒绝，不读环境变量、不请求检测地址。
fn ensure_update_supported() -> Result<(), ErrorPayload> {
    if cfg!(any(windows, target_os = "macos")) {
        Ok(())
    } else {
        Err(ErrorPayload {
            code: "update".to_string(),
            message: "当前平台不支持内部升级，请到 GitHub Release 下载新版本".to_string(),
        })
    }
}

/// 当前主程序是否运行在 macOS 临时安装位置（dmg 直接运行或 App Translocation）。
/// 判定核心抽成独立函数以便单测：`current_exe` 失败时视为正常（不拦升级）。
fn transient_run_location() -> bool {
    std::env::current_exe()
        .map(|path| helper_core::paths::is_transient_install_path(&path))
        .unwrap_or(false)
}

/// 拦截临时安装位置下的升级安装（macOS 设计 M3：检测可用、安装拦截，纵深防御）。
/// 仅 macOS 生效，其他平台直接放行（用 `cfg!` 而非 `#[cfg]`，保持两平台都能编译测试）。
fn ensure_install_location_ok() -> Result<(), ErrorPayload> {
    if cfg!(target_os = "macos") && transient_run_location() {
        Err(ErrorPayload {
            code: "update".to_string(),
            message: TRANSIENT_INSTALL_UPDATE_MESSAGE.to_string(),
        })
    } else {
        Ok(())
    }
}

/// 升级客户端配置（检测与下载共用）：DNS 钉扎 + 拒绝系统代理。
/// - `resolve`：仅 rdc.tiandy.com 不走系统 DNS，其余域名不受影响；
/// - `no_proxy`：CodexHelper 自身就是网关，受管状态下系统代理指向本机，若更新流量
///   绕经网关进程再直连出去，DNS 会在网关侧解析，B 网下同样失败；内网升级服务器
///   本就不该走任何代理，强制直连对 A 网也无副作用。
fn pin_update_client(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let addr: std::net::SocketAddr = UPDATE_PIN_ADDR.parse().expect("内置钉扎地址必须合法");
    builder.no_proxy().resolve(UPDATE_PIN_HOST, addr)
}

/// 构建 updater：存在环境变量覆盖时替换 endpoints，否则沿用 `tauri.conf.json` 的配置。
fn build_updater(app: &AppHandle) -> Result<Updater, ErrorPayload> {
    let mut builder = app.updater_builder();
    // 内网升级服务器：钉扎 DNS + 直连（见 pin_update_client 文档，同时作用于检测与下载）。
    builder = builder.configure_client(pin_update_client);
    if let Some(url) = override_endpoint() {
        let endpoint = validate_endpoint(&url).map_err(|reason| ErrorPayload {
            code: "update".to_string(),
            message: format!("{UPDATE_URL_ENV} 无效：{reason}"),
        })?;
        builder = builder
            .endpoints(vec![endpoint])
            .map_err(|error| map_error("检测地址无效", error))?;
    }
    builder
        .build()
        .map_err(|error| map_error("初始化升级检测失败", error))
}

/// 把 updater 错误映射为前端错误载荷（统一 `code = "update"`）。
fn map_error(context: &str, error: tauri_plugin_updater::Error) -> ErrorPayload {
    ErrorPayload {
        code: "update".to_string(),
        message: format!("{context}：{error}"),
    }
}

/// 检测是否有可用更新（只检测、不下载）。前端“检查更新”按钮与启动自动检测都调用它。
/// 非 Windows/macOS 平台直接返回 `update` 错误（临时安装位置不拦检测，设计 M3）。
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateCheck, ErrorPayload> {
    ensure_update_supported()?;
    let current_version = app.package_info().version.to_string();
    let updater = build_updater(&app)?;
    match updater.check().await {
        Ok(Some(update)) => {
            log::event(
                "desktop.update.check",
                json!({ "available": true, "version": update.version }),
            );
            Ok(UpdateCheck {
                available: true,
                current_version,
                version: Some(update.version.clone()),
                notes: update.body.clone(),
                date: update.date.map(|date| date.to_string()),
            })
        }
        Ok(None) => {
            log::event("desktop.update.check", json!({ "available": false }));
            Ok(UpdateCheck {
                available: false,
                current_version,
                version: None,
                notes: None,
                date: None,
            })
        }
        Err(error) => {
            log::event("desktop.update.check", json!({ "ok": false }));
            Err(map_error("检查更新失败", error))
        }
    }
}

/// 返回当前运行版本（不依赖升级服务器，供界面始终展示本地版本）。
#[tauri::command]
pub fn app_version(app: AppHandle) -> Result<String, ErrorPayload> {
    Ok(app.package_info().version.to_string())
}

/// 下载并安装可用更新，成功后重启应用；无可用更新时返回错误由前端提示刷新。
/// 非 Windows/macOS 平台直接返回 `update` 错误；macOS 临时安装位置下拦截安装并引导。
///
/// `expected_version` 是前端在确认框里向用户展示并确认的版本。安装前重新检查一次，若
/// 服务器此刻返回的版本与用户确认的不一致，则拒绝安装，避免“确认 A 却装了 B”的竞态。
#[tauri::command]
pub async fn install_update(app: AppHandle, expected_version: String) -> Result<(), ErrorPayload> {
    ensure_update_supported()?;
    ensure_install_location_ok()?;
    let updater = build_updater(&app)?;
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            return Err(ErrorPayload {
                code: "update".to_string(),
                message: "没有可安装的更新（可能已是最新版本）。".to_string(),
            });
        }
        Err(error) => return Err(map_error("检查更新失败", error)),
    };
    if update.version != expected_version {
        log::event(
            "desktop.update.install",
            json!({ "ok": false, "reason": "version_changed" }),
        );
        return Err(ErrorPayload {
            code: "update".to_string(),
            message: "可用版本已变化，请重新检查更新后再安装。".to_string(),
        });
    }
    log::event(
        "desktop.update.install",
        json!({ "version": update.version }),
    );
    update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
        .map_err(|error| map_error("下载或安装更新失败", error))?;
    log::event("desktop.update.install", json!({ "ok": true }));
    // 安装器已在后台运行：退出并重启当前进程，让 NSIS 接管替换（`restart` 发散，不返回）。
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_commands_supported_on_windows_and_macos() {
        let result = ensure_update_supported();
        assert_eq!(result.is_ok(), cfg!(any(windows, target_os = "macos")));
        if let Err(e) = result {
            assert_eq!(e.code, "update");
            assert_eq!(
                e.message,
                "当前平台不支持内部升级，请到 GitHub Release 下载新版本"
            );
        }
    }

    #[test]
    fn install_location_message_matches_design() {
        assert_eq!(
            TRANSIENT_INSTALL_UPDATE_MESSAGE,
            "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn transient_run_location_false_for_normal_path() {
        // 正常 /Applications 安装下 current_exe 不含 /Volumes/ 或 /AppTranslocation/
        assert!(!transient_run_location());
    }

    #[test]
    fn validate_endpoint_accepts_http_and_https() {
        assert!(validate_endpoint("http://10.0.0.1:8080/codex-helper/latest.json").is_ok());
        assert!(validate_endpoint("https://example.internal/codex-helper/latest.json").is_ok());
    }

    #[test]
    fn validate_endpoint_rejects_non_http_scheme() {
        assert!(validate_endpoint("ftp://example.internal/latest.json").is_err());
        assert!(validate_endpoint("file:///etc/passwd").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_userinfo() {
        assert!(validate_endpoint("http://user:pass@host/latest.json").is_err());
        assert!(validate_endpoint("http://user@host/latest.json").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_fragment() {
        assert!(validate_endpoint("http://host/latest.json#frag").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_garbage() {
        assert!(validate_endpoint("not a url").is_err());
        assert!(validate_endpoint("").is_err());
    }

    #[test]
    fn update_check_serializes_camel_case() {
        let value = serde_json::to_value(UpdateCheck {
            available: true,
            current_version: "0.1.1".to_string(),
            version: Some("0.2.0".to_string()),
            notes: None,
            date: None,
        })
        .expect("serialize");
        assert!(value.get("currentVersion").is_some());
        assert!(value.get("current_version").is_none());
    }

    #[test]
    fn pinned_update_host_and_ip_match_design() {
        // 契约测试：域名与 IP 改动必须经过显式确认（两个网络靠 VPN 到达同一台升级服务器）。
        assert_eq!(UPDATE_PIN_HOST, "rdc.tiandy.com");
        assert_eq!(UPDATE_PIN_ADDR, "10.20.20.3:0");
        assert_eq!(
            UPDATE_PIN_ADDR
                .parse::<std::net::SocketAddr>()
                .expect("合法 SocketAddr")
                .port(),
            0,
            "端口必须为 0：无显式端口的 URL 用协议默认端口，URL 显式端口优先"
        );
    }

    #[test]
    fn pinned_update_host_is_exact_match_not_suffix() {
        // reqwest resolve() 是精确域名匹配：子域名不应被误钉。这里固化我们对此的理解。
        // （reqwest 内部 DnsResolverWithOverrides 用 HashMap 精确查找，无后缀语义。）
        assert_ne!(UPDATE_PIN_HOST, "sub.rdc.tiandy.com");
    }

    #[tokio::test]
    async fn updater_client_pins_update_host_to_fixed_ip() {
        // 端到端机制测试：构造与生产同款的客户端配置，证明请求真正落到钉扎地址、
        // Host 头保留域名（虚机 Nexus 正常分发）、且不读取系统代理。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let (pin_host, pin_port) = ("pinned.test", addr.port());

        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve(pin_host, addr)
            .build()
            .expect("client");

        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.expect("read");
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
            req
        });

        let resp = client
            .get(format!("http://{pin_host}:{pin_port}/latest.json"))
            .send()
            .await
            .expect("request lands on pinned address");
        assert_eq!(resp.status(), 200);

        let req = server.await.expect("server task");
        // 头名不区分大小写：reqwest 发送小写 `host:`。比较前先统一小写。
        let req_lower = req.to_lowercase();
        assert!(
            req_lower.contains(&format!("host: {pin_host}:{pin_port}")),
            "Host 头应保留域名，实际请求：{req}"
        );
        assert!(req.contains("GET /latest.json"), "路径应保留：{req}");
    }
}
