//! `codex-helper-credential`：供 Codex 命令鉴权调用的凭据读取器，兼作卸载清理入口（设计 §4.4、§5.4）。
//!
//! - `get codex-helper/managed-gateway`：成功时 stdout 仅输出 Token；失败时 stderr 输出不含凭据的
//!   简短原因，退出码非零。只允许这一个固定 target，其余参数一律拒绝，且绝不触碰凭据存储。
//! - `cleanup [--purge-key]`：卸载时撤销受管配置（设计 §5.4），可选同时删除凭据。
//!
//! 本程序被 Codex 高频调用（每次模型请求鉴权都会执行一次 `get`），因此不写日志文件，
//! 避免额外的 I/O 与泄漏面；核心逻辑集中在 [`run`]，只做参数分发与 stdout/stderr 输出，
//! 生产环境的存储与路径解析（[`SystemCredentialStore::managed`]、[`HelperPaths::detect`]）
//! 只在 [`main`] 中组装，方便单元测试注入假实现。
//!
//! `get` 与 `cleanup` 的所有 stderr / stdout 输出都刻意使用 ASCII 英文短句而非中文：
//! - `get` 可能被 Codex 在非 UTF-8 代码页的 Windows 控制台环境下调用；
//! - `cleanup` 由卸载器 NSIS 钩子通过 `nsExec::ExecToLog` 调用，其输出会被按 ANSI / OEM
//!   代码页解码后写入安装日志（见 `apps/desktop/src-tauri/windows/hooks.nsh`）。
//!
//! 中文在这两种场景下都可能显示为乱码，ASCII 则不受代码页影响，始终可读；`cleanup` 失败时
//! 进一步只展示 [`helper_core::types::ManagerError::code`] 这一稳定错误码，而不嵌入任何动态
//! 错误文本（可能是中文，也可能意外携带路径等细节）。

use std::ffi::OsString;
use std::io::Write;

use helper_core::consts;
use helper_core::credential::{CredentialStore, SystemCredentialStore};
use helper_core::manager;
use helper_core::paths::HelperPaths;
use helper_core::types::ManagerError;

fn main() {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let code = match collect_args(std::env::args_os()) {
        Ok(args) => {
            let store = SystemCredentialStore::managed();
            run(&args, &store, HelperPaths::detect, &mut stdout, &mut stderr)
        }
        // 非 UTF-8 参数（例如 Windows 上含未配对代理项的命令行）一律视为用法错误，
        // 而不是让 `String` 转换 panic（那样会以退出码 101 崩溃，且没有任何 stderr 输出）。
        Err(()) => {
            write_usage(&mut stderr);
            2
        }
    };
    std::process::exit(code);
}

/// 把 `args_os()`（跳过程序名）收集为 `Vec<String>`；含任何非 UTF-8 参数时返回 `Err(())`。
fn collect_args(args: impl Iterator<Item = OsString>) -> Result<Vec<String>, ()> {
    args.skip(1)
        .map(|arg| arg.into_string().map_err(|_| ()))
        .collect()
}

/// 参数分发与输出组装，不含任何真实 I/O 副作用之外的逻辑，供单元测试直接调用。
///
/// - `["get", CREDENTIAL_TARGET]`：读取凭据。
/// - `["cleanup"]` / `["cleanup", "--purge-key"]`：调用 [`manager::cleanup`]。
/// - 其余任何参数组合（包括 `get` 配合其他 target）一律视为用法错误，写 usage 到 stderr、
///   返回退出码 2；`get` 分支要求 `cmd == "get"` 与 `target == CREDENTIAL_TARGET` 同时成立
///   才会进入 [`run_get`]，因此非法 target 在匹配阶段就落入用法错误分支，[`run_get`]（进而
///   `store.read()`）根本不会被调用。
/// - `paths` 是 `FnOnce`：只有真正进入 `cleanup` 分支时才会被调用一次，`get` 分支完全不解析路径。
fn run(
    args: &[String],
    store: &dyn CredentialStore,
    paths: impl FnOnce() -> anyhow::Result<HelperPaths>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    match args {
        [cmd, target] if cmd == "get" && target == consts::CREDENTIAL_TARGET => {
            run_get(store, stdout, stderr)
        }
        [cmd] if cmd == "cleanup" => run_cleanup(paths, store, false, stdout, stderr),
        [cmd, flag] if cmd == "cleanup" && flag == "--purge-key" => {
            run_cleanup(paths, store, true, stdout, stderr)
        }
        _ => {
            write_usage(stderr);
            2
        }
    }
}

/// 用法说明（stderr）。ASCII 英文短句，理由见模块文档。
fn write_usage(stderr: &mut impl Write) {
    let _ = writeln!(
        stderr,
        "usage: codex-helper-credential get {} | codex-helper-credential cleanup [--purge-key]",
        consts::CREDENTIAL_TARGET
    );
}

/// `get`：成功时 stdout 只输出 Token 加一个换行（与移植源 `println!` 一致），stderr 无输出，
/// 退出码 0。凭据不存在或读取失败时 stderr 输出固定的英文短句、退出码 2。
///
/// 失败信息使用 ASCII 英文而非中文，理由见模块文档；另外这两条消息本身也绝不包含凭据内容或
/// 底层错误细节（`store.read()` 的错误对象即使意外携带敏感信息，也不会被打印出来）。
///
/// stdout 写入本身失败（例如管道已关闭）时不能当作成功处理：移植源的 `println!` 在这种情况
/// 下会 panic（非零退出），这里改为显式检测 `writeln!` / `flush()` 的结果，失败时同样落到
/// stderr 短句 + 非零退出码，避免“空输出 + 退出码 0”骗过调用方。
fn run_get(store: &dyn CredentialStore, stdout: &mut impl Write, stderr: &mut impl Write) -> i32 {
    match store.read() {
        Ok(Some(secret)) => {
            match writeln!(stdout, "{}", secret.expose()).and_then(|()| stdout.flush()) {
                Ok(()) => 0,
                Err(_) => {
                    let _ = writeln!(stderr, "credential write failed");
                    2
                }
            }
        }
        Ok(None) => {
            let _ = writeln!(stderr, "credential not found");
            2
        }
        Err(_) => {
            let _ = writeln!(stderr, "credential read failed");
            2
        }
    }
}

/// `cleanup [--purge-key]`：委托 [`manager::cleanup`]。成功时 stdout 打印一行不含 Key 的简短
/// ASCII 摘要，退出码 0；失败时 stderr 打印简短 ASCII 原因，退出码 1。
///
/// 路径解析（`paths()`）失败与 `manager::cleanup` 本身失败都归为“清理失败”：前者常见于生产
/// 环境定位不到用户主目录，输出固定短句、不嵌入具体错误文本（可能是中文，也可能意外携带
/// 路径细节）；后者的错误来自 `ManagerError`，取其稳定、ASCII、不含 Key 的 [`ManagerError::code`]
/// 展示（`Display` 虽然也不携带 Key，但是中文，不适合直接透出，理由见模块文档）。
fn run_cleanup(
    paths: impl FnOnce() -> anyhow::Result<HelperPaths>,
    store: &dyn CredentialStore,
    purge_key: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let paths = match paths() {
        Ok(paths) => paths,
        Err(_error) => {
            let _ = writeln!(stderr, "cleanup failed: paths_unavailable");
            return 1;
        }
    };
    match manager::cleanup(&paths, store, purge_key) {
        Ok(report) => {
            // `--purge-key` 语义：未请求时凭据本就保留（kept）；请求了但凭据本来就不存在时，
            // `report.key_purged` 为 false，此时应显示“absent”而非“kept”，否则会被误读成
            // “Key 仍然保留着”。
            let key_state = if !purge_key {
                "kept"
            } else if report.key_purged {
                "deleted"
            } else {
                "absent"
            };
            let _ = writeln!(
                stdout,
                "cleanup done: env_block_removed={} config_restored={} state_reset={} key={}",
                report.env_block_removed, report.config_restored, report.state_reset, key_state,
            );
            0
        }
        Err(error) => {
            let code = error
                .downcast_ref::<ManagerError>()
                .map(ManagerError::code)
                .unwrap_or("unknown");
            let _ = writeln!(stderr, "cleanup failed: {code}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use helper_core::codex_env;
    use helper_core::credential::{MemoryCredentialStore, Secret};
    use helper_core::state::{self, HelperState};

    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn usage_line() -> String {
        format!(
            "usage: codex-helper-credential get {} | codex-helper-credential cleanup [--purge-key]\n",
            consts::CREDENTIAL_TARGET
        )
    }

    /// `get` 分支不应解析路径；传给它的 `paths` 一旦被调用即视为契约被破坏。
    fn unreachable_paths() -> anyhow::Result<HelperPaths> {
        panic!("get 分支不应调用 paths()")
    }

    // ---- args_os 收集 ----

    #[test]
    fn collect_args_passes_through_valid_utf8() {
        let os_args = ["codex-helper-credential", "get", consts::CREDENTIAL_TARGET]
            .into_iter()
            .map(OsString::from);
        assert_eq!(
            collect_args(os_args).unwrap(),
            vec!["get".to_string(), consts::CREDENTIAL_TARGET.to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_args_rejects_non_utf8_without_panicking() {
        use std::os::unix::ffi::OsStringExt;

        let os_args = vec![
            OsString::from("codex-helper-credential"),
            OsString::from("get"),
            OsString::from_vec(vec![0xFF, 0xFE]),
        ];
        assert_eq!(collect_args(os_args.into_iter()), Err(()));
    }

    #[cfg(windows)]
    #[test]
    fn collect_args_rejects_unpaired_surrogate_without_panicking() {
        use std::os::windows::ffi::OsStringExt;

        // 0xD800 是未配对的高代理项，不构成合法 UTF-16 文本，`into_string()` 必然失败。
        let bad = OsString::from_wide(&[0xD800]);
        let os_args = vec![
            OsString::from("codex-helper-credential"),
            OsString::from("get"),
            bad,
        ];
        assert_eq!(collect_args(os_args.into_iter()), Err(()));
    }

    // ---- get ----

    #[test]
    fn get_success_prints_token_only_to_stdout() {
        let store = MemoryCredentialStore::with_token("sk-test-token");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["get", consts::CREDENTIAL_TARGET]),
            &store,
            unreachable_paths,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert_eq!(stdout, b"sk-test-token\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn get_missing_credential_reports_not_found_and_exits_2() {
        let store = MemoryCredentialStore::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["get", consts::CREDENTIAL_TARGET]),
            &store,
            unreachable_paths,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"credential not found\n");
    }

    #[test]
    fn get_read_failure_reports_fixed_message_and_exits_2() {
        struct FailingStore;
        impl CredentialStore for FailingStore {
            fn read(&self) -> anyhow::Result<Option<Secret>> {
                Err(anyhow::anyhow!("底层存储损坏：sk-should-not-leak"))
            }
            fn write(&self, _token: &str) -> anyhow::Result<()> {
                unreachable!("测试不写入")
            }
            fn delete(&self) -> anyhow::Result<()> {
                unreachable!("测试不删除")
            }
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["get", consts::CREDENTIAL_TARGET]),
            &FailingStore,
            unreachable_paths,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        // 固定短句，不透出底层错误信息（可能意外携带敏感内容）。
        assert_eq!(stderr, b"credential read failed\n");
    }

    #[test]
    fn get_stdout_write_failure_is_reported_as_failure_not_silent_success() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("broken pipe (simulated)"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("broken pipe (simulated)"))
            }
        }

        let store = MemoryCredentialStore::with_token("sk-should-not-leak-either");
        let mut stdout = FailingWriter;
        let mut stderr = Vec::new();
        let code = run(
            &args(&["get", consts::CREDENTIAL_TARGET]),
            &store,
            unreachable_paths,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2, "stdout 写入失败不应报告成功退出码 0");
        assert_eq!(stderr, b"credential write failed\n");
    }

    #[test]
    fn get_rejects_any_other_target_without_reading_store() {
        struct PanicOnReadStore {
            reads: AtomicUsize,
        }
        impl CredentialStore for PanicOnReadStore {
            fn read(&self) -> anyhow::Result<Option<Secret>> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                panic!("非法 target 绝不应触发凭据读取");
            }
            fn write(&self, _token: &str) -> anyhow::Result<()> {
                unreachable!("测试不写入")
            }
            fn delete(&self) -> anyhow::Result<()> {
                unreachable!("测试不删除")
            }
        }

        let store = PanicOnReadStore {
            reads: AtomicUsize::new(0),
        };
        let bad_invocations: Vec<Vec<String>> = vec![
            args(&["get", "some-other-target"]),
            args(&["get"]),
            args(&["get", consts::CREDENTIAL_TARGET, "extra"]),
            args(&["Get", consts::CREDENTIAL_TARGET]),
        ];
        for bad in bad_invocations {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let code = run(&bad, &store, unreachable_paths, &mut stdout, &mut stderr);
            assert_eq!(code, 2, "args={bad:?}");
            assert!(stdout.is_empty(), "args={bad:?}");
            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                usage_line(),
                "args={bad:?}"
            );
        }
        assert_eq!(
            store.reads.load(Ordering::SeqCst),
            0,
            "非法 target 不得读取凭据存储"
        );
    }

    // ---- 未知命令 ----

    #[test]
    fn unknown_argument_combinations_print_usage_and_exit_2() {
        let store = MemoryCredentialStore::new();
        let bad_invocations: Vec<Vec<String>> = vec![
            vec![],
            args(&["status"]),
            args(&["cleanup", "--unknown"]),
            args(&["cleanup", "--purge-key", "extra"]),
            args(&["Cleanup"]),
        ];
        for bad in bad_invocations {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let code = run(&bad, &store, unreachable_paths, &mut stdout, &mut stderr);
            assert_eq!(code, 2, "args={bad:?}");
            assert!(stdout.is_empty(), "args={bad:?}");
            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                usage_line(),
                "args={bad:?}"
            );
        }
    }

    // ---- cleanup ----

    #[test]
    fn cleanup_without_purge_key_restores_files_and_keeps_credential() {
        let codex_home = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let paths = HelperPaths::new(codex_home.path(), data_dir.path());

        // 已启用状态，原值为用户自己配置的 "custom" provider；autostart 应在清理后保留。
        state::save(
            &paths.state_file(),
            &HelperState {
                enabled: true,
                previous_model_provider: Some("custom".to_string()),
                previous_model_catalog_json: None,
                autostart: true,
            },
        )
        .unwrap();
        std::fs::write(
            paths.config_toml(),
            "model_provider = \"managed_gateway\"\n\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
        )
        .unwrap();
        std::fs::write(paths.env_file(), codex_env::render_block(17891)).unwrap();

        let store = MemoryCredentialStore::with_token("sk-keep-me");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["cleanup"]),
            &store,
            || Ok(paths.clone()),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        assert!(stderr.is_empty());
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "cleanup done: env_block_removed=true config_restored=true state_reset=true key=kept\n"
        );

        assert_eq!(
            std::fs::read_to_string(paths.config_toml()).unwrap(),
            "model_provider = \"custom\"\n"
        );
        assert!(!paths.env_file().exists(), ".env 只剩空白应被删除");
        assert_eq!(
            store.read().unwrap().unwrap().expose(),
            "sk-keep-me",
            "不带 --purge-key 时应保留凭据"
        );

        let saved_state = state::load(&paths.state_file()).unwrap().unwrap();
        assert!(!saved_state.enabled);
        assert!(saved_state.previous_model_provider.is_none());
        assert!(saved_state.autostart, "autostart 标志应保留");
    }

    #[test]
    fn cleanup_with_purge_key_deletes_credential() {
        let codex_home = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let paths = HelperPaths::new(codex_home.path(), data_dir.path());

        // 无状态文件：按残留规则清理。用户自己的键与变量必须原样保留。
        std::fs::write(
            paths.config_toml(),
            "model = \"o3\"\nmodel_provider = \"managed_gateway\"\n\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
        )
        .unwrap();
        std::fs::write(
            paths.env_file(),
            format!(
                "# 用户自己的变量\nFOO=bar\n\n{}",
                codex_env::render_block(17891)
            ),
        )
        .unwrap();

        let store = MemoryCredentialStore::with_token("sk-purge-me");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["cleanup", "--purge-key"]),
            &store,
            || Ok(paths.clone()),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        assert!(stderr.is_empty());
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "cleanup done: env_block_removed=true config_restored=true state_reset=false key=deleted\n"
        );
        assert!(store.read().unwrap().is_none(), "--purge-key 应删除凭据");

        // 实际还原结果：受管 model_provider 与受管节被删除，用户内容逐字节保留
        assert_eq!(
            std::fs::read_to_string(paths.config_toml()).unwrap(),
            "model = \"o3\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(paths.env_file()).unwrap(),
            "# 用户自己的变量\nFOO=bar\n"
        );
        assert!(!paths.state_file().exists(), "状态文件缺失时不创建");
    }

    #[test]
    fn cleanup_with_purge_key_reports_absent_when_credential_never_existed() {
        let codex_home = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let paths = HelperPaths::new(codex_home.path(), data_dir.path());

        let store = MemoryCredentialStore::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["cleanup", "--purge-key"]),
            &store,
            || Ok(paths.clone()),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        assert!(stderr.is_empty());
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "cleanup done: env_block_removed=false config_restored=false state_reset=false key=absent\n",
            "请求了 --purge-key 但凭据本来就不存在，不应被误读成 kept"
        );
    }

    #[test]
    fn cleanup_reports_failure_reason_without_leaking_key_when_paths_unavailable() {
        let store = MemoryCredentialStore::with_token("sk-should-not-appear");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["cleanup"]),
            &store,
            || Err(anyhow::anyhow!("找不到用户主目录：sk-should-not-appear")),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "cleanup failed: paths_unavailable\n"
        );
        // 凭据存储在路径解析失败时完全不应被触碰。
        assert_eq!(
            store.read().unwrap().unwrap().expose(),
            "sk-should-not-appear"
        );
    }

    #[test]
    fn cleanup_reports_manager_error_code_without_leaking_key_when_config_is_invalid() {
        let codex_home = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let paths = HelperPaths::new(codex_home.path(), data_dir.path());

        // 无状态文件 → 走残留清理路径（`codex_config::remove_residue`），它同样需要先解析
        // config.toml；这里故意写入语法错误的 TOML（未闭合的表头），触发
        // `ManagerError::ConfigInvalid`（对应 `code() == "configInvalid"`）。
        std::fs::write(
            paths.config_toml(),
            "model_provider = \"managed_gateway\"\n[unterminated",
        )
        .unwrap();

        let store = MemoryCredentialStore::with_token("sk-x");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            &args(&["cleanup", "--purge-key"]),
            &store,
            || Ok(paths.clone()),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "cleanup failed: configInvalid\n"
        );
        // 本测试要验证的契约就是上面这条固定 ASCII 短句：不含任何动态内容，自然也不含
        // Key。`manager::cleanup` 中 `--purge-key` 的凭据删除与文件清理是否成功无关（见
        // `manager.rs` 里 `first_error.get_or_insert` 的用法：只记录“第一个”错误，不会
        // 因为文件清理已失败而跳过后续的凭据删除，`MemoryCredentialStore::delete()` 也
        // 不会失败），因此这里凭据实际已被删除，但这不是本测试关注的点。
    }
}
