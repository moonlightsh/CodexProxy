//! `codex-helper-credential` 二进制级测试：通过 `Command` 启动真实编译产物，验证
//! usage / 非法参数的退出码与 stderr，以及 `cleanup` 在真实文件系统上的效果。
//!
//! 刻意不在这里测试两类会真实读写凭据存储的路径：
//! - `get` 的合法路径（合法 target 会真的读取凭据存储；Windows 上即真实 Credential
//!   Manager）——只验证非法 target 在到达凭据存储之前就被拒绝，这一点不涉及平台差异，
//!   在任何系统上跑都是安全的；
//! - `cleanup --purge-key`（带删除语义的合法路径）——`SystemCredentialStore::managed()`
//!   固定使用 `consts::CREDENTIAL_TARGET`，没有任何环境变量能重定向它（`CODEX_HOME` /
//!   `CODEX_HELPER_DATA_DIR` 只重定向文件路径），因此在 Windows 上执行它会真的删除当前
//!   用户 Credential Manager 里 `codex-helper/managed-gateway` 这个受管条目；这一点与
//!   `get` 禁止二进制级测试的理由完全对称，而且删除比读取破坏性更大。`--purge-key` 的
//!   删除语义改由 `src/main.rs` 中基于 `MemoryCredentialStore` 的单元测试覆盖
//!   （`cleanup_with_purge_key_deletes_credential` 等）。
//!
//! 每个测试使用独立的临时目录并只给子进程设置环境变量，不触碰本测试进程自身的环境，
//! 也不写真实用户目录，可安全并行运行。

use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codex-helper-credential"))
}

fn run(command: &mut Command) -> Output {
    command.output().expect("启动 codex-helper-credential 失败")
}

fn usage_line() -> String {
    format!(
        "usage: codex-helper-credential get {} | codex-helper-credential cleanup [--purge-key]\n",
        helper_core::consts::CREDENTIAL_TARGET
    )
}

#[test]
fn no_arguments_prints_usage_and_exits_2() {
    let output = run(&mut bin());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), usage_line());
}

#[test]
fn unknown_command_prints_usage_and_exits_2() {
    let output = run(bin().arg("status"));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), usage_line());
}

#[test]
fn cleanup_with_unknown_flag_is_rejected() {
    let output = run(bin().args(["cleanup", "--unknown"]));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), usage_line());
}

#[test]
fn get_with_illegal_target_is_rejected_before_touching_credential_store() {
    // 合法 target 不在这里测试（见文件顶部说明）；非法 target 在参数匹配阶段即被拒绝，
    // 不会走到任何凭据存储实现，这一点与运行平台无关，可以放心执行。
    let output = run(bin().args(["get", "some-other-target"]));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), usage_line());
}

#[test]
fn get_with_extra_argument_is_rejected() {
    let output = run(bin().args(["get", helper_core::consts::CREDENTIAL_TARGET, "extra"]));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), usage_line());
}

#[test]
fn cleanup_reconciles_real_files_in_subprocess() {
    let codex_home = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    // 没有状态文件：按“残留清理”规则处理，只移除受管 provider 节与受管 .env 块，
    // 用户自己的 provider（custom）保留。这个 config.toml 片段与它的还原结果直接取自
    // `codex_config` / `manager` 阶段 1 已验证的固定用例，避免自行猜测 toml_edit 的
    // 格式化细节（空行、表头位置）导致断言出错。
    std::fs::write(
        codex_home.path().join("config.toml"),
        concat!(
            "model = \"o3\"\n",
            "model_provider = \"managed_gateway\"\n",
            "\n[model_providers.custom]\nname = \"Custom\"\n",
            "\n[model_providers.managed_gateway]\nname = \"Managed Gateway\"\n",
            "\n[model_providers.managed_gateway.auth]\ncommand = \"x\"\n",
        ),
    )
    .unwrap();
    std::fs::write(
        codex_home.path().join(".env"),
        helper_core::codex_env::render_block(17891),
    )
    .unwrap();

    let output = run(bin()
        .arg("cleanup")
        .env("CODEX_HOME", codex_home.path())
        .env("CODEX_HELPER_DATA_DIR", data_dir.path()));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "cleanup done: env_block_removed=true config_restored=true state_reset=false key=kept\n"
    );

    let config = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
    assert_eq!(
        config,
        "model = \"o3\"\n\n[model_providers.custom]\nname = \"Custom\"\n"
    );
    assert!(
        !codex_home.path().join(".env").exists(),
        ".env 只剩受管块时应在移除后被删除"
    );
    assert!(
        !data_dir.path().join("state.json").exists(),
        "本来就没有状态文件，清理不应凭空创建"
    );
    assert!(
        !data_dir.path().join("logs").exists(),
        "codex-helper-credential 不应写日志文件（本程序被 Codex 高频调用，见 main.rs 模块文档）"
    );
}

// `cleanup --purge-key` 的合法（删除）路径刻意不在二进制级测试中执行：`SystemCredentialStore::
// managed()` 固定使用 `consts::CREDENTIAL_TARGET`，没有环境变量能重定向凭据 target；在 Windows
// 上这会真的删除当前用户 Credential Manager 里的受管条目。非 Windows 平台的 `SystemCredentialStore`
// 是仅存在于子进程内存中的开发态实现（见 `credential` 模块文档：“这张表只存在于当前进程的内存
// 中……读不到本进程写入的任何内容”），换一个进程（这里的子进程）看到的是空表，因此在非 Windows
// 上执行是安全的；但为了让本测试套件在任何平台上的行为都一致、不依赖这一平台差异细节，仍然
// 用 `#[cfg(not(windows))]` 显式限定，避免将来有人在 Windows CI 上启用了真实凭据后误删。
#[cfg(not(windows))]
#[test]
fn cleanup_with_purge_key_succeeds_when_nothing_exists() {
    let codex_home = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    let output = run(bin()
        .args(["cleanup", "--purge-key"])
        .env("CODEX_HOME", codex_home.path())
        .env("CODEX_HELPER_DATA_DIR", data_dir.path()));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "cleanup done: env_block_removed=false config_restored=false state_reset=false key=absent\n"
    );
    assert!(!codex_home.path().join("config.toml").exists());
    assert!(!codex_home.path().join(".env").exists());
}
