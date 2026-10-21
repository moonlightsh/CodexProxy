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
        script(dir.path(), "codex", "sleep 5");
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

// @@MAIN@@
