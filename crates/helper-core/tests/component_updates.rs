//! 组件更新检查的集成测试（组件更新检查设计 §4.5）。
//!
//! - 本地探测：临时目录里放假的 `codex` 脚本（仅类 Unix），验证 PATH 查找与输出解析；
//! - 编排与远端请求：wiremock + 注入的假本地探测（不发真 spawn）。

#[cfg(unix)]
mod unix_probe {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use helper_core::component_updates::{LocalState, probe_codex_cli};
    use helper_core::devenv::SearchEnv;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// 写一个带可执行位的 sh 脚本（写法同 tests/devenv.rs）。
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    fn installed(version: &str) -> LocalState {
        LocalState::Installed {
            version: version.into(),
        }
    }

    #[tokio::test]
    async fn cli_probe_reads_first_codex_on_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        script(first.path(), "codex", "echo 'codex-cli 0.162.1'");
        script(second.path(), "codex", "echo 'codex-cli 9.9.9'");
        let env = SearchEnv::new(path_of(&[first.path(), second.path()]), vec![]);
        assert_eq!(probe_codex_cli(&env, TIMEOUT).await, installed("0.162.1"));
    }

    #[tokio::test]
    async fn cli_probe_reads_version_from_stderr() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "codex", "echo 'codex-cli 0.163.0' >&2; exit 1");
        let env = SearchEnv::new(path_of(&[dir.path()]), vec![]);
        assert_eq!(probe_codex_cli(&env, TIMEOUT).await, installed("0.163.0"));
    }

    #[tokio::test]
    async fn cli_probe_missing_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let env = SearchEnv::new(path_of(&[dir.path()]), vec![]);
        assert_eq!(
            probe_codex_cli(&env, TIMEOUT).await,
            LocalState::NotInstalled
        );
    }

    fn probe_failed_detail(state: LocalState) -> String {
        match state {
            LocalState::ProbeFailed { detail } => detail,
            other => panic!("应为 ProbeFailed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn cli_probe_garbage_is_probe_failed() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "codex", "echo hello");
        let env = SearchEnv::new(path_of(&[dir.path()]), vec![]);
        let detail = probe_failed_detail(probe_codex_cli(&env, TIMEOUT).await);
        assert!(detail.contains("没有输出版本号"), "{detail}");
    }

    #[tokio::test]
    async fn cli_probe_timeout_is_probe_failed() {
        let dir = tempfile::tempdir().unwrap();
        // 子进程 PATH 只有临时目录，须用绝对路径
        script(dir.path(), "codex", "exec /bin/sleep 5");
        let env = SearchEnv::new(path_of(&[dir.path()]), vec![]);
        let state = probe_codex_cli(&env, Duration::from_millis(200)).await;
        let detail = probe_failed_detail(state);
        assert!(detail.contains("超过"), "{detail}");
    }

    #[tokio::test]
    async fn cli_probe_finds_cmd_shim_via_exts() {
        // 模拟 Windows npm 全局安装的 codex.cmd 经 PATHEXT 查找
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "codex.cmd", "echo 'codex-cli 0.163.0'");
        let env = SearchEnv::new(path_of(&[dir.path()]), vec![".cmd".into()]);
        assert_eq!(probe_codex_cli(&env, TIMEOUT).await, installed("0.163.0"));
    }
}

#[cfg(not(windows))]
#[tokio::test]
async fn chatgpt_probe_is_unsupported_off_windows() {
    use helper_core::component_updates::{LocalState, probe_chatgpt_desktop};
    assert_eq!(
        probe_chatgpt_desktop(std::time::Duration::from_secs(1)).await,
        LocalState::UnsupportedPlatform
    );
}

// ---------------------------------------------------------------------------
// 编排与远端请求（wiremock + 假本地探测）
// ---------------------------------------------------------------------------

mod check_all {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use helper_core::component_updates::{
        CheckContext, Comparison, ComponentUpdate, ComponentUpdates, Endpoints, LocalProbe,
        LocalState, NetworkPath, RemoteSource, RemoteState, check_all,
    };
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FEED_PATH: &str = "/codex-app-prod/windows-release.json";
    const API_PATH: &str = "/repos/openai/codex/releases/latest";
    const LATEST_PATH: &str = "/openai/codex/releases/latest";
    const X64_URL: &str =
        "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix";
    const TAG_URL: &str = "https://github.com/openai/codex/releases/tag/rust-v0.163.0";
    const VERSION_JSON: &str = r#"{"latest_version":"0.163.0","last_checked_at":"2026-10-16T05:53:49.254443Z","dismissed_version":null}"#;

    /// 探测时观察的 mock 服务与记录到的已收请求数。
    type Observer = (Arc<MockServer>, Arc<Mutex<Vec<usize>>>);

    /// 假本地探测：原样返回预设状态并计数；可选地在探测中记录 mock 服务已收到的请求数。
    struct FakeProbe {
        desktop: LocalState,
        cli: LocalState,
        desktop_calls: Arc<AtomicUsize>,
        cli_calls: Arc<AtomicUsize>,
        observe: Option<Observer>,
    }

    impl FakeProbe {
        fn new(desktop: LocalState, cli: LocalState) -> Self {
            Self {
                desktop,
                cli,
                desktop_calls: Arc::default(),
                cli_calls: Arc::default(),
                observe: None,
            }
        }

        async fn observe(&self) {
            if let Some((server, seen)) = &self.observe {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let count = server.received_requests().await.unwrap().len();
                seen.lock().unwrap().push(count);
            }
        }
    }

    impl LocalProbe for FakeProbe {
        async fn chatgpt_desktop(&self) -> LocalState {
            self.desktop_calls.fetch_add(1, Ordering::SeqCst);
            self.observe().await;
            self.desktop.clone()
        }

        async fn codex_cli(&self) -> LocalState {
            self.cli_calls.fetch_add(1, Ordering::SeqCst);
            self.observe().await;
            self.cli.clone()
        }
    }

    fn installed(version: &str) -> LocalState {
        LocalState::Installed {
            version: version.into(),
        }
    }

    fn default_probe() -> FakeProbe {
        FakeProbe::new(installed("26.1013.4894.0"), installed("0.162.1"))
    }

    fn endpoints(base: &str) -> Endpoints {
        Endpoints {
            desktop_feed: format!("{base}{FEED_PATH}"),
            github_api: format!("{base}{API_PATH}"),
            github_latest: format!("{base}{LATEST_PATH}"),
        }
    }

    fn ctx(server: &MockServer, home: &Path, probe: FakeProbe) -> CheckContext<FakeProbe> {
        CheckContext {
            network: NetworkPath::Direct { reason: None },
            timeout: Duration::from_secs(2),
            endpoints: endpoints(&server.uri()),
            probe,
            codex_home: home.to_path_buf(),
            desktop_supported: true,
        }
    }

    fn feed_body(x64_url: &str) -> serde_json::Value {
        json!({
            "schemaVersion": 1,
            "releaseDate": "2026-10-17T05:55:44Z",
            "packageVersion": "26.1013.4894.0",
            "releaseVersion": "26.1013.40934",
            "futureField": { "a": 1 },
            "packages": [
                { "architecture": "x64", "downloadUrl": x64_url },
                { "architecture": "arm64", "downloadUrl":
                  "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-arm64.msix" }
            ]
        })
    }

    fn api_body() -> serde_json::Value {
        json!({"tag_name":"rust-v0.163.0","name":"0.163.0","prerelease":false,"assets":[]})
    }

    async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(server)
            .await;
    }

    /// 三条路径都挂正常响应。
    async fn mount_all_ok(server: &MockServer) {
        mount(
            server,
            FEED_PATH,
            ResponseTemplate::new(200).set_body_json(feed_body(X64_URL)),
        )
        .await;
        mount(
            server,
            API_PATH,
            ResponseTemplate::new(200).set_body_json(api_body()),
        )
        .await;
    }

    async fn hits(server: &MockServer, route: &str) -> usize {
        let requests = server.received_requests().await.unwrap();
        requests.iter().filter(|r| r.url.path() == route).count()
    }

    fn write_cache(home: &Path, body: &str) {
        std::fs::write(home.join("version.json"), body).unwrap();
    }

    fn detail_of(remote: &RemoteState) -> &str {
        match remote {
            RemoteState::Unreachable { detail }
            | RemoteState::Invalid { detail }
            | RemoteState::Blocked { detail } => detail,
            other => panic!("无 detail：{other:?}"),
        }
    }

    async fn run(server: &MockServer, home: &Path, probe: FakeProbe) -> ComponentUpdates {
        check_all(&ctx(server, home, probe)).await
    }

    fn desktop_ok() -> RemoteState {
        RemoteState::Ok {
            version: "26.1013.4894.0".into(),
            source: RemoteSource::DesktopReleaseFeed,
            fallback_from: None,
        }
    }

    #[tokio::test]
    async fn desktop_feed_ok_up_to_date() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let result = run(&server, home.path(), default_probe()).await;
        let row = result.chatgpt_desktop;
        assert_eq!(row.remote, desktop_ok());
        assert_eq!(row.comparison, Comparison::UpToDate);
        assert_eq!(row.download_url, None);
    }

    #[tokio::test]
    async fn desktop_feed_update_available_has_versioned_url() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let probe = FakeProbe::new(installed("26.1012.1.0"), installed("0.162.1"));
        let row = run(&server, home.path(), probe).await.chatgpt_desktop;
        assert_eq!(row.comparison, Comparison::UpdateAvailable);
        let url = row.download_url.unwrap();
        assert!(
            url.starts_with(
                "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/"
            ),
            "{url}"
        );
    }

    #[tokio::test]
    async fn desktop_feed_404_is_unreachable() {
        let server = MockServer::start().await;
        mount(&server, FEED_PATH, ResponseTemplate::new(404)).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe())
            .await
            .chatgpt_desktop;
        assert_eq!(
            row.remote,
            RemoteState::Unreachable {
                detail: "清单返回 HTTP 404".into()
            }
        );
        assert_eq!(row.comparison, Comparison::Unknown);
    }

    #[tokio::test]
    async fn desktop_feed_timeout_is_unreachable() {
        let server = MockServer::start().await;
        let slow = ResponseTemplate::new(200)
            .set_body_json(feed_body(X64_URL))
            .set_delay(Duration::from_secs(2));
        mount(&server, FEED_PATH, slow).await;
        let home = tempfile::tempdir().unwrap();
        let mut context = ctx(&server, home.path(), default_probe());
        context.timeout = Duration::from_millis(300);
        let row = check_all(&context).await.chatgpt_desktop;
        assert_eq!(
            row.remote,
            RemoteState::Unreachable {
                detail: "超时".into()
            }
        );
    }

    #[tokio::test]
    async fn desktop_feed_offlist_url_is_invalid() {
        let server = MockServer::start().await;
        let body = feed_body("https://evil.example/ChatGPT-x64.msix");
        mount(
            &server,
            FEED_PATH,
            ResponseTemplate::new(200).set_body_json(body),
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        let probe = FakeProbe::new(installed("26.1012.1.0"), installed("0.162.1"));
        let row = run(&server, home.path(), probe).await.chatgpt_desktop;
        assert!(
            matches!(row.remote, RemoteState::Invalid { .. }),
            "{:?}",
            row.remote
        );
        assert!(detail_of(&row.remote).contains("白名单"));
        assert_eq!(row.download_url, None);
        assert_eq!(row.comparison, Comparison::Unknown);
    }

    #[tokio::test]
    async fn desktop_feed_html_is_invalid() {
        let server = MockServer::start().await;
        let html = ResponseTemplate::new(200).set_body_string("<html>login</html>");
        mount(&server, FEED_PATH, html).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe())
            .await
            .chatgpt_desktop;
        assert!(
            matches!(row.remote, RemoteState::Invalid { .. }),
            "{:?}",
            row.remote
        );
        assert!(detail_of(&row.remote).starts_with("清单格式无效"));
    }

    #[tokio::test]
    async fn oversized_body_is_invalid() {
        let server = MockServer::start().await;
        let huge = ResponseTemplate::new(200).set_body_bytes(vec![b'['; 300 * 1024]);
        mount(&server, FEED_PATH, huge).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe())
            .await
            .chatgpt_desktop;
        assert_eq!(
            row.remote,
            RemoteState::Invalid {
                detail: "响应体超过 256 KiB".into()
            }
        );
    }

    #[tokio::test]
    async fn desktop_unsupported_skips_feed_request() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let probe = default_probe();
        let desktop_calls = probe.desktop_calls.clone();
        let mut context = ctx(&server, home.path(), probe);
        context.desktop_supported = false;
        let row = check_all(&context).await.chatgpt_desktop;
        assert_eq!(row.local, LocalState::UnsupportedPlatform);
        assert_eq!(row.remote, RemoteState::UnsupportedPlatform);
        assert!(!row.via_proxy);
        assert_eq!(hits(&server, FEED_PATH).await, 0);
        assert_eq!(desktop_calls.load(Ordering::SeqCst), 0);
    }

    fn cache_fallback(remote: &RemoteState) -> &str {
        match remote {
            RemoteState::Ok {
                version,
                source: RemoteSource::CodexLocalCache { checked_at },
                fallback_from: Some(reason),
            } => {
                assert_eq!(version, "0.163.0");
                assert_eq!(checked_at, "2026-10-16T05:53:49Z");
                reason
            }
            other => panic!("应为本地缓存兜底：{other:?}"),
        }
    }

    fn redirect_to(location: &str) -> ResponseTemplate {
        ResponseTemplate::new(302).insert_header("Location", location)
    }

    #[tokio::test]
    async fn cli_api_ok() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert_eq!(
            row.remote,
            RemoteState::Ok {
                version: "0.163.0".into(),
                source: RemoteSource::GithubApi,
                fallback_from: None,
            }
        );
        assert_eq!(row.comparison, Comparison::UpdateAvailable);
        assert_eq!(row.download_url.as_deref(), Some(TAG_URL));
    }

    async fn redirect_fallback_with(status: u16) {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(status)).await;
        mount(&server, LATEST_PATH, redirect_to(TAG_URL)).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert_eq!(
            row.remote,
            RemoteState::Ok {
                version: "0.163.0".into(),
                source: RemoteSource::GithubRedirect,
                fallback_from: None,
            }
        );
        assert_eq!(row.comparison, Comparison::UpdateAvailable);
    }

    #[tokio::test]
    async fn cli_api_403_falls_back_to_redirect() {
        redirect_fallback_with(403).await;
    }

    #[tokio::test]
    async fn cli_api_429_falls_back_to_redirect() {
        redirect_fallback_with(429).await;
    }

    #[tokio::test]
    async fn cli_redirect_missing_location_falls_back_to_cache() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(403)).await;
        mount(&server, LATEST_PATH, ResponseTemplate::new(302)).await;
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path(), VERSION_JSON);
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        let reason = cache_fallback(&row.remote);
        assert!(reason.starts_with("github api 403 限流"), "{reason}");
        assert!(reason.contains("缺少 Location"), "{reason}");
        assert_eq!(row.comparison, Comparison::UpdateAvailable);
        let value = serde_json::to_value(&row).unwrap();
        assert!(value["remote"].get("fallbackFrom").is_some());
    }

    #[tokio::test]
    async fn cli_redirect_prerelease_tag_rejected_falls_back_to_cache() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(403)).await;
        let pre = "https://github.com/openai/codex/releases/tag/rust-v0.164.0-alpha.2";
        mount(&server, LATEST_PATH, redirect_to(pre)).await;
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path(), VERSION_JSON);
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        let reason = cache_fallback(&row.remote);
        assert!(reason.contains("tag 格式不符"), "{reason}");
    }

    #[tokio::test]
    async fn cli_api_500_skips_redirect_and_uses_cache() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(500)).await;
        mount(&server, LATEST_PATH, redirect_to(TAG_URL)).await;
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path(), VERSION_JSON);
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert_eq!(cache_fallback(&row.remote), "github api 返回 HTTP 500");
        assert_eq!(hits(&server, LATEST_PATH).await, 0);
    }

    #[tokio::test]
    async fn cli_all_fail_without_cache_is_unreachable() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(403)).await;
        mount(&server, LATEST_PATH, ResponseTemplate::new(404)).await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert_eq!(
            row.remote,
            RemoteState::Unreachable {
                detail: "github api 403 限流；302 回退失败：HTTP 404".into()
            }
        );
        assert_eq!(row.comparison, Comparison::Unknown);
        assert_eq!(row.download_url, None);
    }

    #[tokio::test]
    async fn cli_api_invalid_tag_without_cache_is_invalid() {
        let server = MockServer::start().await;
        let body = json!({"tag_name": "rust-v0.164.0-alpha.2"});
        mount(
            &server,
            API_PATH,
            ResponseTemplate::new(200).set_body_json(body),
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert!(
            matches!(row.remote, RemoteState::Invalid { .. }),
            "{:?}",
            row.remote
        );
        assert!(detail_of(&row.remote).starts_with("github api 响应无效"));
    }

    #[tokio::test]
    async fn cli_cache_garbage_is_ignored() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(500)).await;
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path(), "not json");
        let row = run(&server, home.path(), default_probe()).await.codex_cli;
        assert!(
            matches!(row.remote, RemoteState::Unreachable { .. }),
            "{:?}",
            row.remote
        );
    }

    #[tokio::test]
    async fn secure_blocked_sends_zero_requests() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        mount(&server, LATEST_PATH, redirect_to(TAG_URL)).await;
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path(), VERSION_JSON);
        let probe = default_probe();
        let (desktop_calls, cli_calls) = (probe.desktop_calls.clone(), probe.cli_calls.clone());
        let mut context = ctx(&server, home.path(), probe);
        let detail = "Secure 代理未就绪（代理未运行）";
        context.network = NetworkPath::Blocked { detail };
        let result = check_all(&context).await;
        for (row, local) in [
            (&result.chatgpt_desktop, installed("26.1013.4894.0")),
            (&result.codex_cli, installed("0.162.1")),
        ] {
            assert_eq!(
                row.remote,
                RemoteState::Blocked {
                    detail: detail.into()
                }
            );
            assert!(!row.via_proxy);
            assert_eq!(row.direct_reason, None);
            assert_eq!(row.comparison, Comparison::Unknown);
            assert_eq!(row.local, local);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(desktop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(cli_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn via_proxy_routes_through_proxy() {
        // mock 服务充当 HTTP 代理：目标主机不可解析，只有经代理才能拿到响应
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let mut context = ctx(&server, home.path(), default_probe());
        context.network = NetworkPath::ViaProxy {
            proxy_url: server.uri(),
        };
        context.endpoints = endpoints("http://feed.invalid");
        let result = check_all(&context).await;
        assert_eq!(result.chatgpt_desktop.remote, desktop_ok());
        assert!(matches!(result.codex_cli.remote, RemoteState::Ok { .. }));
        assert!(result.chatgpt_desktop.via_proxy && result.codex_cli.via_proxy);
        let requests = server.received_requests().await.unwrap();
        assert!(!requests.is_empty());
        assert!(
            requests
                .iter()
                .all(|r| r.url.host_str() == Some("feed.invalid"))
        );
    }

    #[tokio::test]
    async fn direct_reports_reason() {
        let server = MockServer::start().await;
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let mut context = ctx(&server, home.path(), default_probe());
        let reason = "代理为外部实例，模式未知，已直连";
        context.network = NetworkPath::Direct {
            reason: Some(reason),
        };
        let result = check_all(&context).await;
        for row in [&result.chatgpt_desktop, &result.codex_cli] {
            assert_eq!(row.direct_reason, Some(reason));
            assert!(!row.via_proxy);
        }
    }

    #[tokio::test]
    async fn local_probes_finish_before_any_remote_request() {
        let server = Arc::new(MockServer::start().await);
        mount_all_ok(&server).await;
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut probe = default_probe();
        probe.observe = Some((server.clone(), seen.clone()));
        check_all(&ctx(&server, home.path(), probe)).await;
        assert_eq!(*seen.lock().unwrap(), vec![0, 0]);
        assert!(!server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_component_failure_does_not_affect_other() {
        let server = MockServer::start().await;
        mount(&server, FEED_PATH, ResponseTemplate::new(404)).await;
        mount(
            &server,
            API_PATH,
            ResponseTemplate::new(200).set_body_json(api_body()),
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        let result: ComponentUpdates = run(&server, home.path(), default_probe()).await;
        assert!(matches!(
            result.chatgpt_desktop.remote,
            RemoteState::Unreachable { .. }
        ));
        let cli: &ComponentUpdate = &result.codex_cli;
        assert!(matches!(cli.remote, RemoteState::Ok { .. }));
    }

    #[tokio::test]
    async fn requests_carry_no_credentials() {
        let server = MockServer::start().await;
        mount(&server, API_PATH, ResponseTemplate::new(403)).await;
        mount(&server, LATEST_PATH, redirect_to(TAG_URL)).await;
        mount(
            &server,
            FEED_PATH,
            ResponseTemplate::new(200).set_body_json(feed_body(X64_URL)),
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        run(&server, home.path(), default_probe()).await;
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests {
            for header in ["authorization", "cookie", "proxy-authorization"] {
                assert!(request.headers.get(header).is_none(), "{header}");
            }
            let agent = request.headers.get("user-agent").unwrap().to_str().unwrap();
            assert!(agent.starts_with("codex-helper/"), "{agent}");
        }
    }

    #[test]
    fn production_endpoints_are_consts() {
        let production = Endpoints::production();
        assert_eq!(
            production.desktop_feed,
            helper_core::consts::CHATGPT_RELEASE_FEED_URL
        );
        assert_eq!(
            production.github_api,
            helper_core::consts::CODEX_RELEASES_API_URL
        );
        assert_eq!(
            production.github_latest,
            helper_core::consts::CODEX_RELEASES_LATEST_URL
        );
    }
}
