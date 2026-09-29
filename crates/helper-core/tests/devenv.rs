//! 开发环境检测的集成测试（设计 §16）：临时目录里放假的 python3 / node / npm 脚本，
//! 验证 PATH 查找顺序与检测编排。只在类 Unix 平台运行（假命令是 sh 脚本）；
//! Windows 的注册表 PATH 与 `.cmd` 调用见 tests/devenv_windows.rs（CI windows-latest）与实机验收。
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use helper_core::devenv::{self, MirrorState, RuntimeState, SearchEnv};

const TIMEOUT: Duration = Duration::from_secs(10);

/// 写一个带可执行位的 sh 脚本。
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}

/// 假 python3：`--version` 输出指定版本，`-P -m pip config list` 输出指定配置。
fn fake_python(dir: &Path, version: &str, pip_config: &str) {
    let body = format!(
        "case \"$1\" in\n\
         --version) echo 'Python {version}' ;;\n\
         -P) [ \"$2 $3 $4 $5\" = '-m pip config list' ] || exit 3; printf '%s\\n' \"{pip_config}\" ;;\n\
         *) exit 2 ;;\n\
         esac"
    );
    script(dir, "python3", &body);
}

fn fake_node(dir: &Path, registry: &str) {
    script(dir, "node", "[ \"$1\" = --version ] && echo v24.16.0");
    script(
        dir,
        "npm",
        &format!("[ \"$*\" = 'config get registry' ] || exit 3; echo '{registry}'"),
    );
}

#[test]
fn find_in_path_is_dir_major_then_ext_order() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    script(second.path(), "tool.exe", "true");
    script(first.path(), "tool.cmd", "true");
    script(second.path(), "tool.com", "true");
    let path = path_of(&[first.path(), second.path()]);
    let exts = vec![".com".to_string(), ".exe".to_string(), ".cmd".to_string()];
    // 第一个目录只有 .cmd，也先于第二个目录的 .com / .exe 命中。
    let found = devenv::find_in_path("tool", &path, &exts).unwrap();
    assert_eq!(found, first.path().join("tool.cmd"));
    // 同一目录内按扩展名顺序。
    let path = path_of(&[second.path()]);
    let found = devenv::find_in_path("tool", &path, &exts).unwrap();
    assert_eq!(found, second.path().join("tool.com"));
}

#[test]
fn find_in_path_skips_relative_dirs_directories_and_non_executables() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("python3")).unwrap();
    let plain = dir.path().join("node");
    std::fs::write(&plain, "not executable").unwrap();
    let mut entries = vec![PathBuf::from("relative/bin")];
    entries.push(dir.path().to_path_buf());
    let path = std::env::join_paths(entries).unwrap();
    assert_eq!(devenv::find_in_path("python3", &path, &[]), None);
    assert_eq!(devenv::find_in_path("node", &path, &[]), None);
    assert_eq!(devenv::find_in_path("missing", &OsString::new(), &[]), None);
}

#[tokio::test]
async fn detects_runtimes_and_configured_mirrors() {
    let bin = tempfile::tempdir().unwrap();
    fake_python(
        bin.path(),
        "3.13.14",
        "global.index-url='http://mirrors.aliyun.com/pypi/simple/'\ninstall.trusted-host='mirrors.aliyun.com'",
    );
    fake_node(bin.path(), "https://registry.npmmirror.com/");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(report.python.state, RuntimeState::Ok);
    assert_eq!(report.python.version.as_deref(), Some("3.13.14"));
    assert_eq!(report.python.min_version.as_deref(), Some("3.13"));
    let python_path = bin.path().join("python3").display().to_string();
    assert_eq!(report.python.path.as_deref(), Some(python_path.as_str()));
    assert_eq!(report.pip_mirror.state, MirrorState::Configured);
    assert_eq!(report.node.state, RuntimeState::Ok);
    assert_eq!(report.node.version.as_deref(), Some("24.16.0"));
    assert_eq!(report.npm_mirror.state, MirrorState::Configured);
}

#[tokio::test]
async fn too_old_python_skips_pip_and_default_sources_are_reported() {
    let bin = tempfile::tempdir().unwrap();
    fake_python(
        bin.path(),
        "3.12.9",
        "global.index-url='http://mirrors.aliyun.com/pypi/simple/'",
    );
    fake_node(bin.path(), "https://registry.npmjs.org/");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(report.python.state, RuntimeState::TooOld);
    assert_eq!(report.python.version.as_deref(), Some("3.12.9"));
    assert_eq!(report.pip_mirror.state, MirrorState::Skipped);
    assert_eq!(report.npm_mirror.state, MirrorState::NotConfigured);
    assert_eq!(
        report.npm_mirror.current.as_deref(),
        Some("https://registry.npmjs.org/")
    );
}

#[tokio::test]
async fn first_path_hit_wins_even_if_a_later_one_is_newer() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    fake_python(old.path(), "3.11.2", "");
    fake_python(new.path(), "3.13.14", "");
    let env = SearchEnv::new(path_of(&[old.path(), new.path()]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    // 命令行里的 python 解析到的是 PATH 上第一个，后面更新的版本不算数。
    assert_eq!(report.python.state, RuntimeState::TooOld);
    assert_eq!(report.python.version.as_deref(), Some("3.11.2"));
}

#[tokio::test]
async fn missing_broken_and_failing_tools_are_reported() {
    let empty = tempfile::tempdir().unwrap();
    let env = SearchEnv::new(path_of(&[empty.path()]), Vec::new());
    let report = devenv::detect_in(&env, TIMEOUT).await;
    assert_eq!(report.python.state, RuntimeState::Missing);
    assert_eq!(report.pip_mirror.state, MirrorState::Skipped);
    assert_eq!(report.node.state, RuntimeState::Missing);
    assert_eq!(report.npm_mirror.state, MirrorState::Skipped);

    let bin = tempfile::tempdir().unwrap();
    // python3 能报版本，但没有 pip；node 无法输出版本号。
    script(
        bin.path(),
        "python3",
        "[ \"$1\" = --version ] && { echo 'Python 3.13.1'; exit 0; }\n\
         echo '/usr/bin/python3: No module named pip' >&2; exit 1",
    );
    script(bin.path(), "node", "echo boom >&2; exit 7");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());
    let report = devenv::detect_in(&env, TIMEOUT).await;
    assert_eq!(report.python.state, RuntimeState::Ok);
    assert_eq!(report.pip_mirror.state, MirrorState::Failed);
    assert_eq!(
        report.pip_mirror.detail.as_deref(),
        Some("当前 Python 没有安装 pip")
    );
    assert_eq!(report.node.state, RuntimeState::Broken);
    assert!(report.node.detail.unwrap().contains("退出码 7"));
    assert_eq!(report.npm_mirror.state, MirrorState::Skipped);
}

#[tokio::test]
async fn node_without_npm_reports_failed_mirror() {
    let bin = tempfile::tempdir().unwrap();
    script(bin.path(), "node", "echo v20.11.1");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(report.node.state, RuntimeState::Ok, "{:?}", report.node);
    assert_eq!(report.node.version.as_deref(), Some("20.11.1"));
    assert_eq!(report.npm_mirror.state, MirrorState::Failed);
    assert_eq!(
        report.npm_mirror.detail.as_deref(),
        Some("PATH 上找不到 npm")
    );
}

#[tokio::test]
async fn hanging_command_is_killed_after_timeout() {
    let bin = tempfile::tempdir().unwrap();
    // PATH 里只有临时目录，外部命令要写绝对路径。
    script(bin.path(), "python3", "exec /bin/sleep 30");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());

    let report = devenv::detect_in(&env, Duration::from_secs(2)).await;

    // 语义断言（不做墙钟断言，见提交 383d492）：走的是超时分支。
    assert_eq!(report.python.state, RuntimeState::Broken);
    assert!(report.python.detail.unwrap().contains("超过 2 秒未完成"));
    assert_eq!(report.pip_mirror.state, MirrorState::Skipped);
}

#[tokio::test]
async fn store_alias_without_version_is_store_stub() {
    let root = tempfile::tempdir().unwrap();
    // is_store_alias 先把 `/` 换成 `\`，所以 Unix 上的 `<tmp>/Microsoft/WindowsApps/` 也会命中。
    let apps = root.path().join("Microsoft").join("WindowsApps");
    std::fs::create_dir_all(&apps).unwrap();
    script(
        &apps,
        "python3",
        "echo 'Python was not found; run without arguments to install from the Microsoft Store' >&2; exit 9009",
    );
    let env = SearchEnv::new(path_of(&[&apps]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(
        report.python.state,
        RuntimeState::StoreStub,
        "{:?}",
        report.python
    );
    assert_eq!(report.python.version, None);
    assert_eq!(report.pip_mirror.state, MirrorState::Skipped);
}

#[tokio::test]
async fn failing_pip_and_npm_config_commands_report_failed_mirrors() {
    let bin = tempfile::tempdir().unwrap();
    // pip 存在但 `config list` 失败（stderr 不含 “No module named pip”）；npm `config get` 失败。
    script(
        bin.path(),
        "python3",
        "[ \"$1\" = --version ] && { echo 'Python 3.13.14'; exit 0; }\n\
         echo 'ERROR: bad config file' >&2; exit 1",
    );
    script(bin.path(), "node", "echo v24.16.0");
    script(bin.path(), "npm", "echo 'npm error' >&2; exit 5");
    let env = SearchEnv::new(path_of(&[bin.path()]), Vec::new());

    let report = devenv::detect_in(&env, TIMEOUT).await;

    assert_eq!(report.python.state, RuntimeState::Ok);
    assert_eq!(report.pip_mirror.state, MirrorState::Failed);
    assert_eq!(
        report.pip_mirror.detail.as_deref(),
        Some("pip config list 执行失败（退出码 1）")
    );
    assert_eq!(report.node.state, RuntimeState::Ok);
    assert_eq!(report.npm_mirror.state, MirrorState::Failed);
    assert_eq!(
        report.npm_mirror.detail.as_deref(),
        Some("npm config get registry 执行失败（退出码 5）")
    );
}
