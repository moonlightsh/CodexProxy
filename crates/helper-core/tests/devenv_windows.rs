//! 开发环境检测的 Windows 集成测试（设计 §16）：系统搜索环境可用与 `.cmd` 调用。
//! 注册表读取本身由 devenv.rs 内 `windows_env::tests` 直接测试（这里的 `SearchEnv::system()`
//! 读取失败时会退回进程 PATH）。在 CI 的 windows-latest 上运行；类 Unix 平台见 tests/devenv.rs。
#![cfg(windows)]

use std::time::Duration;

use helper_core::devenv::{self, MirrorState, RuntimeState, SearchEnv};

const TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn system_search_env_is_usable() {
    let env = SearchEnv::system();
    let path = env.path.to_string_lossy().to_ascii_lowercase();
    assert!(!path.is_empty());
    assert!(path.contains("system32"), "{path}");
}

#[tokio::test]
async fn cmd_shims_in_dir_with_spaces_are_detected() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("node tools");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("node.cmd"), "@echo v24.16.0\r\n").unwrap();
    std::fs::write(
        bin.join("npm.cmd"),
        "@echo https://registry.npmmirror.com/\r\n",
    )
    .unwrap();
    let exts = [".COM", ".EXE", ".BAT", ".CMD"].map(String::from).to_vec();
    let env = SearchEnv::new(bin.clone().into_os_string(), exts);

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(report.node.state, RuntimeState::Ok, "{:?}", report.node);
    assert_eq!(report.node.version.as_deref(), Some("24.16.0"));
    assert_eq!(
        report.npm_mirror.state,
        MirrorState::Configured,
        "{:?}",
        report.npm_mirror
    );
}
