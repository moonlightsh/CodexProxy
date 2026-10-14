# CodexHelper macOS MVP 实施计划

> **给执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实施。步骤用复选框（`- [ ]`）跟踪。

**目标：** 在 macOS（Apple Silicon / Intel）上提供与 Windows 版相同的受管模式，产出 ad-hoc 签名的 universal dmg，随 v0.2.0 发布到 GitHub Release。

**架构：** helper-core 新增经 `/usr/bin/security` 的 Keychain 后端、Key 字符约束、安装位置判定与“撤销受管配置”编排；桌面层在 macOS 上用 `pgrep` 检测 Codex、隐藏内部升级、托盘增加“撤销配置并退出”；打包层新增 macOS 平台配置、`scripts/build-macos.sh` 与 release job。Windows 行为除 Key 字符约束外不变。

**技术栈：** Rust 2024 workspace、Tauri 2.11（tauri-plugin-dialog 2.7、tauri-plugin-autostart 2.5）、原生 TypeScript + Vite、`node --test`、GitHub Actions。

**设计：** `docs/superpowers/specs/2026-10-14-codex-helper-macos-design.md`（下称“设计”），基线 `docs/superpowers/specs/2026-09-23-codex-helper-design.md`（下称“原设计”）。执行者两份都要读。

## Global Constraints（全局约束）

- 仓库主目录 `/Users/sxk/Documents/code/ai-space/CodexHelper`，集成分支 `pi/macos-mvp`；worktree 统一放在 `/Users/sxk/Documents/code/ai-space/CodexHelper-wt/<通道名>`。本计划已提交到 `pi/macos-mvp`，worktree 里有同一份副本；执行期间如有修订，以主目录 `pi/macos-mvp` 上的版本为准。
- 绝对禁止：修改真实 `~/.codex`（含 `config.toml`、`.env`、`auth.json`）；读写或删除 Keychain 中 service=`codex-helper/managed-gateway` 的生产条目（含只读查询）；改动 `~/Library/LaunchAgents`；启动打包出的 GUI 应用（不 `open .app`、不跑 `tauri dev`）；`git push`；改 git config。
- 测试一律用临时 `CODEX_HOME` / `CODEX_HELPER_DATA_DIR`；真实 Keychain 测试只用 `codex-helper/test-<pid>-<纳秒>-<计数>` 形式的测试 service，结束（含 panic）时删除。
- 在 macOS 上运行 `cargo test` 绝不读写生产条目（设计 §3.3 硬性规则）；实施期间也不要手动运行 `codex-helper-credential get …` 或 `cleanup --purge-key`。
- 不新增第三方依赖；不改 `tauri.conf.json` 中除 `version` 以外的内容。
- 严格 TDD：先写失败测试并确认失败原因符合预期，再实现。
- 每个任务一个提交；提交信息中文，先写入临时文件再 `git commit -F <文件>`，末行 `Co-Authored-By: Pi`；只 `git add` 本任务列出的文件。
- 所有文档、注释、界面文案中文；代码风格与现有代码一致（错误文案中文、`log::event` 只记错误码与布尔）。
- 固定值（逐字取自设计）：account `codex-helper`；security 绝对路径 `/usr/bin/security`；子进程超时 10 秒；Key 允许字符 0x21–0x7E；`minimumSystemVersion` `12.0`；`signingIdentity` `"-"`；版本 `0.2.0`。
- 固定文案：
  - InvalidKeyChars：`Key 只能包含英文字母、数字和符号`
  - InstallLocation 与安装位置横幅：`请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开`
  - 撤销确认框正文：`将还原 Codex 的 config.toml、移除 .env 受管块、关闭开机自启，然后退出。API Key 保留在钥匙串。删除 CodexHelper 前请先做这一步；完成后需要重启 Codex。`；按钮 `撤销并退出` / `取消`
  - 主窗口底部（仅 macOS）：`卸载前请先在菜单栏选择「撤销配置并退出」`

### 门禁（每个任务提交前、在所在 worktree 根目录全部通过）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-windows.sh
TC="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"
PATH="$TC:$PATH" RUSTC="$TC/rustc" CARGO_TARGET_DIR=target/wincheck "$TC/cargo" clippy \
  --target x86_64-pc-windows-msvc -p helper-core -p codex-helper-credential -p codex-helper --all-targets -- -D warnings
npm --prefix apps/desktop ci        # 每个 worktree 首次
npm --prefix apps/desktop run check
npm --prefix apps/desktop test
npm --prefix apps/desktop run build
```

下文各任务的“门禁”步骤即指上面整组命令，期望全部退出码 0。

## 通道、分支与合并

| 通道 | 分支 | worktree | 起点 | 负责文件 |
| --- | --- | --- | --- | --- |
| core | `pi/macos-core` | `CodexHelper-wt/core` | `pi/macos-mvp` | `crates/helper-core/**`、`apps/credential/**` |
| pack | `pi/macos-pack` | `CodexHelper-wt/pack` | `pi/macos-mvp` | `apps/desktop/src-tauri/tauri.macos.conf.json`、`scripts/build-macos.sh`、`.github/workflows/release.yml`、版本号文件（`tauri.conf.json` 的 version、根 `Cargo.toml`、`apps/desktop/package.json`、`apps/desktop/package-lock.json`、`Cargo.lock`） |
| desktop | `pi/macos-desktop` | `CodexHelper-wt/desktop` | `pi/macos-core` 最终提交 | `apps/desktop/src-tauri/src/**`、`apps/desktop/src/**`、`apps/desktop/src-tauri/capabilities/**` |

- core 与 pack 并行；desktop 在 core 全部任务合入 `pi/macos-core` 后开始。
- 唯一的文件归属例外：`crates/helper-core/src/consts.rs` 中 `CODEX_PROCESS_NAME` 的平台拆分由 desktop 任务 D1 完成，core 不碰这一行。原因：`apps/desktop/src-tauri/src/process.rs` 的现有测试断言 `matches_process_name("codex.exe", consts::CODEX_PROCESS_NAME)`，core 若在 macOS 上把常量改成 `codex`，core 分支上的 `cargo test --workspace` 会失败，而 core 不能改 desktop 文件。desktop 分支从 core 最终提交拉出，这一行不会冲突。
- 全部完成后由主会话把 `pi/macos-desktop`（已含 core）与 `pi/macos-pack` 合入 `pi/macos-mvp` 并跑一次门禁；预计无冲突（core 与 desktop 都不改 `Cargo.lock`）。
- 设计 §9.2 实机验收不在本计划内，发布前由人工按设计执行。

## Review Focus

设计没明说、但最可能伤到同事的五类输入或故障（按可能性排序）。每条的测试已写进所属任务。

1. Key 里混入换行、回车、NUL 或引号（粘贴带尾随换行、调用方绕过编排层直接写凭据）：`security -i` 是逐行解释命令的，换行会变成第二条 security 命令。期望：后端在启动子进程前拒绝任何非 0x21–0x7E 字符，`\`、`"` 正确转义后原样读回。测试：C1 `validate_token_accepts_printable_ascii_only`、`keychain_rejects_unsafe_token_without_writing`、`keychain_special_characters_round_trip`。
2. 登录钥匙串被锁、条目 ACL 属于别的程序，`security` 弹出授权框或一直挂起：期望 10 秒后结束子进程并返回只含操作名的错误，Codex 侧表现为 `credential read failed`，不会无限卡住桌面应用。测试：C1 `run_with_timeout_kills_hanging_child`、`op_errors_name_operation_only`。
3. 从 dmg 或“下载”里直接打开（App Translocation），而受管模式之前已在 /Applications 启用过：期望启动对账不把 `auth.command` 改成临时路径，仍按旧值修复其余漂移，状态灯“配置已受管”按旧值判定；config 里已没有受管节时什么都不写。测试：C5 `reconcile_from_transient_location_keeps_previous_command`、`reconcile_from_transient_location_without_managed_command_fails_untouched`。
4. “撤销配置并退出”中途失败（`.env` 被锁定、目录不可写）：期望不退出、返回错误码；如果 `.env` 受管块还在，代理被重新拉起，Codex 不会指向无人监听的 17891。测试：C6 `revoke_failure_restarts_proxy_when_env_block_remains`。
5. 撤销确认框里按回车或 Esc、点“取消”、等待超时、对话框无法显示：期望一律视为取消，只有点击“撤销并退出”才执行。确认框不用 tauri-plugin-dialog（它在锁定版本里会把取消类结果映射成第二个按钮，见第三节开头），改用 `/usr/bin/osascript` 的 `display dialog`，“取消”同时是默认按钮和 cancel button。测试：D3 `revoke_confirmed_only_for_confirm_token`、`revoke_dialog_args_bind_return_and_esc_to_cancel`、`revoke_dialog_script_compiles`、`revoke_steps_*`。

---

## 文件结构

| 文件 | 通道 | 职责 |
| --- | --- | --- |
| `crates/helper-core/src/credential.rs`（改） | core | 按平台分派后端：Windows / macOS Keychain / 其他平台 dev_store |
| `crates/helper-core/src/credential/keychain.rs`（新） | core | `security` 转义、命令行拼装、退出码映射、带超时的子进程执行（全平台编译）；macOS 读写删实现 |
| `crates/helper-core/src/consts.rs`（改） | core / desktop | core 加 Keychain 三个常量与 `PLATFORM`；desktop 拆分 `CODEX_PROCESS_NAME` |
| `crates/helper-core/src/types.rs`（改） | core | `Status` 三个新字段；`ManagerError` 两个新变体 |
| `crates/helper-core/src/paths.rs`（改） | core | `is_transient_install_path` |
| `crates/helper-core/src/codex_config.rs`（改） | core | `managed_command`：读出受管节当前的 `auth.command` |
| `crates/helper-core/src/manager.rs`（改） | core | Key 字符校验、平台字段、安装位置门控、`revoke_managed_config` |
| `crates/helper-core/src/devenv.rs`（改） | core | `platform_supported` 包含 macOS |
| `apps/credential/src/main.rs`、`apps/credential/tests/cli.rs`（改） | core | 进程内真实 Keychain 的 `get` 约定测试；排除触及生产条目的子进程用例 |
| `apps/desktop/src-tauri/tauri.macos.conf.json`（新） | pack | macOS 平台打包配置 |
| `scripts/build-macos.sh`（新） | pack | universal 凭据程序 + `tauri build` → dmg |
| `.github/workflows/release.yml`（改） | pack | `macos-release` job |
| `apps/desktop/src-tauri/src/process.rs`（改） | desktop | macOS `pgrep` 检测 |
| `apps/desktop/src-tauri/src/update.rs`、`commands.rs`（改） | desktop | 非 Windows 拒绝升级命令；开机自启的安装位置预检 |
| `apps/desktop/src-tauri/src/revoke.rs`（新） | desktop | “撤销配置并退出”的确认框参数、结果判定与流程 |
| `apps/desktop/src-tauri/src/tray.rs`、`lifecycle.rs`、`main.rs`（改） | desktop | 菜单布局、退出流程拆分、注册模块 |
| `apps/desktop/src/types.ts`、`format.ts`、`errors.ts`、`banners.ts`、`main.ts`、`dev-mock.ts` 及对应 `*.test.ts`（改） | desktop | 前端字段、横幅、底部提示、隐藏升级区 |

---

# 一、core 通道（`pi/macos-core`）

准备：

```bash
cd /Users/sxk/Documents/code/ai-space/CodexHelper
git worktree add -b pi/macos-core ../CodexHelper-wt/core pi/macos-mvp
cd ../CodexHelper-wt/core && npm --prefix apps/desktop ci
```

### 任务 C1：Keychain 后端（经 `/usr/bin/security`）

**文件：**
- 新建：`crates/helper-core/src/credential/keychain.rs`
- 修改：`crates/helper-core/src/credential.rs`（模块文档、`mod keychain;`、平台分派、`dev_mode_tests` 的 cfg）
- 修改：`crates/helper-core/src/consts.rs`
- 修改：`apps/credential/tests/cli.rs`、`apps/credential/src/main.rs`（仅测试模块）

**接口：**
- 消费：现有 `CredentialStore` trait、`SystemCredentialStore::{new, managed, target}`。
- 产出（`consts`）：`pub const KEYCHAIN_ACCOUNT: &str = "codex-helper";`、`pub const SECURITY_TOOL: &str = "/usr/bin/security";`、`pub const SECURITY_TIMEOUT_SECS: u64 = 10;`。
- 产出（`credential::keychain`，`pub(crate)`，全平台编译；模块顶部 `#![cfg_attr(not(target_os = "macos"), allow(dead_code))]`。只有 macOS 函数用到的条目（`consts::KEYCHAIN_ACCOUNT`、`consts::SECURITY_TOOL`）在使用处写全路径 `crate::consts::…`，或把对应 `use` 加 `#[cfg(target_os = "macos")]`；`dead_code` 的 allow 管不到 `unused_imports`，Windows 交叉 clippy 会报错）：

```rust
pub(crate) enum KeychainOp { Read, Write, Delete }          // 中文名：读取 / 写入 / 删除
pub(crate) fn validate_token(token: &str) -> anyhow::Result<()>
pub(crate) fn escape_for_security(value: &str) -> String
pub(crate) fn add_command_line(service: &str, account: &str, token: &str) -> String
pub(crate) fn map_read_exit(code: Option<i32>, stdout: &[u8]) -> anyhow::Result<Option<String>>
pub(crate) fn map_write_exit(code: Option<i32>) -> anyhow::Result<()>
pub(crate) fn map_delete_exit(code: Option<i32>) -> anyhow::Result<()>
pub(crate) struct RunOutput { pub code: Option<i32>, pub stdout: Vec<u8> }
pub(crate) enum RunError { Spawn, Timeout }
pub(crate) fn run_error(op: KeychainOp, error: RunError) -> anyhow::Error
pub(crate) fn run_with_timeout(program: &Path, args: &[&str], stdin: Option<&[u8]>, timeout: Duration)
    -> Result<RunOutput, RunError>
#[cfg(target_os = "macos")] pub(super) fn read(service: &str) -> anyhow::Result<Option<String>>
#[cfg(target_os = "macos")] pub(super) fn write(service: &str, token: &str) -> anyhow::Result<()>
#[cfg(target_os = "macos")] pub(super) fn delete(service: &str) -> anyhow::Result<()>
```

行为约定（实现与测试共用）：

| 函数 | 约定 |
| --- | --- |
| `validate_token` | 空串 → `凭据不能为空`；任一字节不在 0x21–0x7E → `凭据只能包含可打印 ASCII 字符`。错误信息不含 token。在启动子进程前调用，是 `security -i` 逐行解析的注入防线 |
| `escape_for_security` | `\` → `\\`，`"` → `\"`，其余原样 |
| `add_command_line` | `format!("add-generic-password -U -a \"{a}\" -s \"{s}\" -w \"{t}\"\n")`，三个值都先经 `escape_for_security` |
| `map_read_exit` | `Some(0)`：stdout 去掉末尾一个 `\n`；为空 → `Ok(None)`；非 UTF-8 → 错误；`Some(44)` → `Ok(None)`；其他 → `op_error` |
| `map_write_exit` | 只有 `Some(0)` 成功 |
| `map_delete_exit` | `Some(0)` 或 `Some(44)` 成功 |
| 错误文案 | 退出码：`钥匙串{读取/写入/删除}失败（security 退出码 {n}）`；`None`：`钥匙串{op}失败（security 被信号终止）`；`RunError::Timeout`：`钥匙串{op}超时（10 秒）`；`RunError::Spawn`：`钥匙串{op}失败（无法启动 /usr/bin/security）`。绝不嵌入 stdout / stderr |
| `run_with_timeout` | `std::process::Command`；stderr → `Stdio::null()`；stdout 管道由独立线程读到 EOF；有 stdin 时写完立即关闭；主线程每 20 ms `try_wait` 一次直到超时，超时后 `kill` + `wait` 并返回 `Timeout` |
| macOS `read` | `security find-generic-password -s <svc> -a codex-helper -w` → `map_read_exit` |
| macOS `write` | 先 `validate_token`；`security -i`，stdin 为 `add_command_line(svc, KEYCHAIN_ACCOUNT, token)`，stdout 丢弃 → `map_write_exit`。token 不进入 argv |
| macOS `delete` | `security delete-generic-password -s <svc> -a codex-helper` → `map_delete_exit` |

`credential.rs` 分派：`#[cfg(windows)]` → `windows_backend`（不变）；`#[cfg(target_os = "macos")]` → `keychain`；`#[cfg(not(any(windows, target_os = "macos")))]` → `dev_store`（同时把 `dev_store` 模块本身与 `dev_mode_tests` 的 cfg 改成这个条件；否则 `dev_mode_tests` 里固定名字的 `codex-helper/test-round-trip` 等会写进真实钥匙串）。模块文档改为三个平台的说明。

- [ ] **步骤 1：先堵住触及生产条目的测试（不改实现）**

排查结论（实施时用 `grep -rn "SystemCredentialStore\|CARGO_BIN_EXE" crates apps --include=*.rs` 复核）：
- `apps/credential/tests/cli.rs::cleanup_with_purge_key_succeeds_when_nothing_exists`：子进程用 `SystemCredentialStore::managed()` 执行删除。macOS 后端落地后会删掉生产条目，也会读它来判断 `key=absent`。把 `#[cfg(not(windows))]` 改为 `#[cfg(not(any(windows, target_os = "macos")))]`，并把上方注释与文件顶部说明里“非 Windows 平台是内存实现”的表述改成“Windows 与 macOS 上都是真实系统存储”。
- `cli.rs` 其余用例：参数错误在到达凭据存储前就被拒绝；`cleanup_reconciles_real_files_in_subprocess` 不带 `--purge-key`，`manager::cleanup` 不调用凭据存储。保留。
- `crates/helper-core/src/credential.rs::dev_mode_tests`：见上方分派说明。
- `apps/credential/src/main.rs` 单元测试、`crates/helper-core/tests/*.rs`、`apps/desktop/src-tauri` 测试：只用 `MemoryCredentialStore` 或假实现。不改。

运行：`cargo test -p codex-helper-credential --test cli -- --list`
期望：在 macOS 上列表里没有 `cleanup_with_purge_key_succeeds_when_nothing_exists`。

- [ ] **步骤 2：写纯函数的失败测试**（`keychain.rs` 内 `#[cfg(test)] mod tests`，全平台）

```rust
#[test] fn validate_token_accepts_printable_ascii_only() {
    let all: String = (0x21u8..=0x7e).map(char::from).collect();
    assert!(validate_token("hk-ABC_123").is_ok() && validate_token(&all).is_ok());
    assert_eq!(validate_token("").unwrap_err().to_string(), "凭据不能为空");
    for bad in [" ", "a b", "a\nb", "a\rb", "a\tb", "a\0b", "a\u{7f}", "sk-中文", "é"] {
        let err = validate_token(bad).unwrap_err().to_string();
        assert_eq!(err, "凭据只能包含可打印 ASCII 字符", "{bad:?}");
        // 固定文案本身含空格；只对去掉空白后至少 2 字节的样本检查“不含 token”，否则恒为假阳性
        if bad.trim().len() >= 2 { assert!(!err.contains(bad), "{err}"); }
    }
}
#[test] fn escape_doubles_backslash_and_escapes_quote() {
    assert_eq!(escape_for_security(r#"a\b"c'd$e"#), r#"a\\b\"c'd$e"#);
    assert_eq!(escape_for_security("plain"), "plain");
}
#[test] fn add_command_line_quotes_and_escapes_every_value() {
    assert_eq!(
        add_command_line("svc/x", "acct", r#"k"\1"#),
        "add-generic-password -U -a \"acct\" -s \"svc/x\" -w \"k\\\"\\\\1\"\n"
    );
}
#[test] fn read_exit_mapping() {
    assert_eq!(map_read_exit(Some(0), b"hk-1\n").unwrap().as_deref(), Some("hk-1"));
    assert_eq!(map_read_exit(Some(0), b"hk-1").unwrap().as_deref(), Some("hk-1"));
    assert_eq!(map_read_exit(Some(0), b"\n").unwrap(), None);
    assert_eq!(map_read_exit(Some(44), b"").unwrap(), None);
    assert!(map_read_exit(Some(0), &[0xff, b'\n']).is_err());
    assert!(map_read_exit(Some(51), b"").is_err() && map_read_exit(None, b"").is_err());
}
#[test] fn write_and_delete_exit_mapping() {
    assert!(map_write_exit(Some(0)).is_ok());
    for code in [Some(44), Some(45), Some(2), None] { assert!(map_write_exit(code).is_err()); }
    assert!(map_delete_exit(Some(0)).is_ok() && map_delete_exit(Some(44)).is_ok());
    for code in [Some(45), Some(1), None] { assert!(map_delete_exit(code).is_err()); }
}
#[test] fn op_errors_name_operation_only() {
    let e = map_read_exit(Some(51), b"hk-secret\n").unwrap_err().to_string();
    assert_eq!(e, "钥匙串读取失败（security 退出码 51）");
    assert_eq!(map_write_exit(None).unwrap_err().to_string(), "钥匙串写入失败（security 被信号终止）");
    assert_eq!(run_error(KeychainOp::Delete, RunError::Timeout).to_string(), "钥匙串删除超时（10 秒）");
    assert_eq!(run_error(KeychainOp::Read, RunError::Spawn).to_string(),
               "钥匙串读取失败（无法启动 /usr/bin/security）");
}
#[cfg(unix)] #[test] fn run_with_timeout_feeds_stdin_and_collects_stdout() {
    let out = run_with_timeout(Path::new("/bin/sh"), &["-c", "cat; exit 3"], Some(b"abc"),
                               Duration::from_secs(5)).unwrap();
    assert_eq!((out.code, out.stdout), (Some(3), b"abc".to_vec()));
}
#[cfg(unix)] #[test] fn run_with_timeout_kills_hanging_child() {
    let start = Instant::now();
    let r = run_with_timeout(Path::new("/bin/sleep"), &["30"], None, Duration::from_millis(300));
    assert!(matches!(r, Err(RunError::Timeout)) && start.elapsed() < Duration::from_secs(5));
}
#[test] fn run_with_timeout_reports_spawn_failure() {
    let r = run_with_timeout(Path::new("/nonexistent/codex-helper-no-such-tool"), &[], None,
                             Duration::from_secs(1));
    assert!(matches!(r, Err(RunError::Spawn)));
}
```

`RunOutput` / `RunError` 需要 `Debug`（`unwrap` 用）。

- [ ] **步骤 3：写真实 Keychain 的失败测试**（`credential.rs` 新增 `#[cfg(all(test, target_os = "macos"))] mod keychain_tests`）

照搬 `windows_mode_tests` 的 `unique_test_target()`（`codex-helper/test-<pid>-<纳秒>-<计数>`，并加 `assert_ne!(target, consts::CREDENTIAL_TARGET)`）与 `TempTargetGuard`（Drop 时 `delete`）。测试：

- `keychain_write_read_overwrite_delete_round_trip`：初始 `read() == None`；写 `hk-first` 读回；写 `hk-second` 读回（`-U` 覆盖）；删两次都 `Ok`；最后 `None`、`!exists()`。
- `keychain_missing_entry_reads_none_and_delete_succeeds`：新 target 直接 `read() == None`，`delete()` 为 `Ok`。
- `keychain_special_characters_round_trip`：token `r#"hk-A\B"C'D$E;F#G!`{}|~"#` 写入后逐字读回。
- `keychain_rejects_unsafe_token_without_writing`：依次写 `"hk-a\nadd-generic-password -a x -s y -w z"`、`"hk 1"`、`"hk-中文"`、`""`，都 `Err`，每次之后 `read() == None`；只对去掉空白后至少 2 字节的 token 断言错误文本不含该 token（`""` 只断言 `Err` 与文案 `凭据不能为空`，`str::contains("")` 恒为真）。

- [ ] **步骤 3b：`codex-helper-credential get` 的 stdout / stderr 约定测试（进程内、真实钥匙串，设计 §3.3）**

在 `apps/credential/src/main.rs` 测试模块新增 `#[cfg(target_os = "macos")] mod keychain_get`，复制同样的 `unique_test_target()` 与 Drop 守卫（不导出 helper-core 的测试工具，也不给生产二进制加 service 覆盖入口）。store 用 `SystemCredentialStore::new(unique_test_target())` 注入 `run`，参数仍是 `["get", consts::CREDENTIAL_TARGET]`：

- `get_prints_real_keychain_token_only_to_stdout`：先 `store.write("hk-cli-1")`；`run(...)` 返回 0，`stdout == b"hk-cli-1\n"`，`stderr` 为空。
- `get_reports_not_found_for_missing_keychain_entry`：不写入；返回 2，stdout 为空，`stderr == b"credential not found\n"`。

这两个是契约测试，钉的是 `get` 的 stdout / stderr 约定而不是后端：实现前 macOS 上走的是 dev_store，它们会直接通过，没有红灯阶段，这是可以接受的例外。后端行为的红灯由步骤 2、3 提供。实现后必须重跑一次，确认在真实钥匙串下仍然通过。

CI 回退（设计 §3.3）：如果 CI `macos-latest` 因为没有可用的默认钥匙串导致这组测试失败，给这 4 个测试（以及步骤 3b 的 2 个）加 `#[ignore = "需要可用的登录钥匙串；本机运行：cargo test --workspace keychain_ -- --ignored"]`，在提交信息里注明，本机必须手动跑一次 `-- --ignored` 并通过。本计划不 push，所以实施期间无法判断是否需要回退；实施时不要预先加 `#[ignore]`。首次 push 后若 CI macOS 的 `keychain_*` / `get_*` 失败，由 core 通道按本条补一个提交（见第四节收尾）。

- [ ] **步骤 4：运行测试确认失败**

运行：`cargo test -p helper-core credential::`
期望：编译失败，报 `keychain` 模块与上述函数不存在。

运行：`cargo test -p codex-helper-credential keychain_get`
期望：PASS（步骤 3b 的契约测试，见上）。

- [ ] **步骤 5：实现**：`consts` 三个常量；`keychain.rs` 按上表实现；`credential.rs` 加 `mod keychain;` 与 macOS 分派。

- [ ] **步骤 6：运行测试确认通过**

运行：`cargo test -p helper-core credential::` 与 `cargo test -p codex-helper-credential`
期望：全部 PASS。测试残留由 `TempTargetGuard` 保证清理；不要用 `dump-keychain` 等命令列钥匙串核对（会连带列出生产条目）。

- [ ] **步骤 7：门禁**（见全局约束）

- [ ] **步骤 8：提交**

```bash
git add crates/helper-core/src/credential.rs crates/helper-core/src/credential/keychain.rs \
        crates/helper-core/src/consts.rs apps/credential/tests/cli.rs apps/credential/src/main.rs
git commit -F /tmp/codex-helper-c1-msg.txt   # 标题：feat(core): macOS 凭据改存钥匙串（经 /usr/bin/security）
```

### 任务 C2：平台标识、Status 新字段与新错误码

**文件：**
- 修改：`crates/helper-core/src/consts.rs`（新增 `PLATFORM`，不碰 `CODEX_PROCESS_NAME`）
- 修改：`crates/helper-core/src/types.rs`（`Status` 三个字段、`ManagerError` 两个变体及 `code()`、单元测试）
- 修改：`crates/helper-core/src/manager.rs`（`build_status` 填新字段、`platform_supported`）
- 修改：`crates/helper-core/src/devenv.rs`（`detect_in` 的 `platform_supported` 与字段文档）
- 测试：`crates/helper-core/tests/manager.rs`、`crates/helper-core/tests/devenv.rs`、`crates/helper-core/tests/devenv_windows.rs`

**接口：**
- 产出（`consts`）：`pub const PLATFORM: &str;` —— Windows `"windows"`，macOS `"macos"`，其他 `"other"`（用 `cfg!` 的 `if` 常量表达式或三个 `#[cfg]` 定义均可）。这是 core 内部新增，不属于接口契约；desktop 与前端一律读 `Status::platform`，不要依赖这个常量。
- 产出（`types::Status`，serde camelCase，放在 `platform_supported` 之后）：
  - `pub platform: String` —— `consts::PLATFORM`
  - `pub update_supported: bool` —— `cfg!(windows)`
  - `pub install_location_ok: bool` —— 本任务恒为 `true`，C5 改为真实判定
  - `platform_supported` 改为 `cfg!(any(windows, target_os = "macos"))`，字段文档改为“Windows 与 macOS 为 true；其他平台为开发模式”
- 产出（`types::ManagerError`，单元变体）：

| 变体 | `code()` | `Display`（逐字） |
| --- | --- | --- |
| `InvalidKeyChars` | `"invalidKeyChars"` | `Key 只能包含英文字母、数字和符号` |
| `InstallLocation` | `"installLocation"` | `请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开` |

- 产出（`devenv::DevEnvReport`）：`platform_supported` 同样改为 `cfg!(any(windows, target_os = "macos"))`，字段文档同步（macOS 按 `python3` 检测为正式支持）。
- 前端 `types.ts` 由 desktop 通道 D 任务同步，本任务不碰。

- [ ] **步骤 1：写失败测试**

`types.rs` 单元测试：
- `dto_fields_serialize_as_camel_case` 增加断言：`json` 含 `platform`、`updateSupported`、`installLocationOk` 三个键。
- 新增 `new_error_variants_have_stable_codes_and_fixed_copy`：两个变体的 `code()` 与 `to_string()` 分别等于上表；`payload()` 的 `code` / `message` 与之一致。

`tests/manager.rs::status_reports_static_fields_and_file_inspection`：把 `assert_eq!(status.platform_supported, cfg!(windows))` 替换为

```rust
let expected = if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "other" };
assert_eq!(status.platform, expected);
assert_eq!(status.platform, consts::PLATFORM);
assert_eq!(status.platform_supported, cfg!(any(windows, target_os = "macos")));
assert_eq!(status.update_supported, cfg!(windows));
assert!(status.install_location_ok, "测试用 COMMAND 不是临时安装位置");
```

`tests/devenv.rs::detects_runtimes_and_configured_mirrors`（`cfg(unix)`）末尾加 `assert_eq!(report.platform_supported, cfg!(target_os = "macos"));`；`tests/devenv_windows.rs::cmd_shims_in_dir_with_spaces_are_detected` 末尾加 `assert!(report.platform_supported);`（本机只经 Windows 交叉 clippy 编译，CI Windows 跑）。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test -p helper-core --lib types::` 与 `cargo test -p helper-core --test manager status_reports_static_fields_and_file_inspection`
期望：编译失败，报 `Status` 没有 `platform` 等字段、`ManagerError` 没有 `InvalidKeyChars` / `InstallLocation`。

- [ ] **步骤 3：实现**：按“接口”加常量、字段、变体与 `code()` 分支；`Manager::build_status` 填三个新字段并改 `platform_supported`；`devenv::detect_in` 改 `platform_supported`。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test -p helper-core`
期望：全部 PASS（`devenv` 集成测试在 macOS 上断言 `platform_supported == true`）。

- [ ] **步骤 5：门禁**（见全局约束）。`check-windows.sh` 与 Windows 交叉 clippy 必须通过：`devenv_windows.rs` 的新断言在这里编译。

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/consts.rs crates/helper-core/src/types.rs crates/helper-core/src/manager.rs \
        crates/helper-core/src/devenv.rs crates/helper-core/tests/manager.rs \
        crates/helper-core/tests/devenv.rs crates/helper-core/tests/devenv_windows.rs
git commit -F /tmp/codex-helper-c2-msg.txt   # 标题：feat(core): 状态新增平台、升级支持与安装位置字段，macOS 视为正式平台
```

### 任务 C3：Key 字符校验接入 `save_key` / `enable`（全平台）

**文件：**
- 修改：`crates/helper-core/src/credential.rs`（新增 `is_allowed_token` 与单元测试）
- 修改：`crates/helper-core/src/credential/keychain.rs`（`validate_token` 的字符判定改用 `is_allowed_token`，加一条一致性测试）
- 修改：`crates/helper-core/src/manager.rs`（`check_new_key`）
- 测试：`crates/helper-core/tests/manager.rs`

**接口：**
- 消费：C1 `keychain::validate_token`；C2 `ManagerError::InvalidKeyChars`。
- 产出（`credential`，全平台）：`pub fn is_allowed_token(token: &str) -> bool` —— 非空且每个字节都在 `0x21..=0x7E`。这是 Key 字符规则的唯一出处：`keychain::validate_token` 的“只能包含可打印 ASCII”分支与编排层都调用它（`keychain` 是私有子模块，编排层不能直接用 `validate_token`）。
- 行为（`Manager::check_new_key`）：`trim` → 空 → `EmptyKey` → `!is_allowed_token(key)` → `InvalidKeyChars` → 网关校验。字符校验在网络之前，被拒时不访问网关、不写任何东西。`save_key` 与 `enable(key: Some(..))` 都经这里；`enable(key: None)` 用已保存凭据，不重新校验。日志 `manager.key_check` 不变，被拒只经 `manager.save_key` / `manager.enable_failed` 记错误码。

- [ ] **步骤 1：写失败测试**

`credential.rs::tests`：
- `allowed_token_is_printable_ascii_only`：`(0x21u8..=0x7e).map(char::from).collect::<String>()` 与 `"hk-ABC_123"` 为 true；`""`、`" "`、`"a b"`、`"a\tb"`、`"a\nb"`、`"a\rb"`、`"a\0b"`、`"a\u{7f}"`、`"sk-中文"`、`"é"`、`"a\u{3000}b"` 为 false。

`keychain.rs::tests`：
- `validate_token_agrees_with_is_allowed_token`：对上面同一组样本逐个断言 `validate_token(s).is_ok() == is_allowed_token(s)`。

`tests/manager.rs`（放在 `// ---- 3.` 的 `save_key_validates_and_leaves_files_alone` 之后）：
- `invalid_key_chars_are_rejected_before_gateway_and_writes`：`write_config(USER_CONFIG)`、`write_env(USER_ENV)`，`before = snapshot()`。对 `["hk 1", "hk-中文", "hk-a\nb", "hk-\u{7f}", "\u{e9}"]` 中每个 key：`assert_code(manager.enable(with_key(key)).await, "invalidKeyChars")` 与 `assert_code(manager.save_key(SaveKeyRequest { key: key.into(), force_save: true }).await, "invalidKeyChars")`，错误 `to_string()` 等于 `Key 只能包含英文字母、数字和符号` 且不含该 key。最后 `snapshot() == before`、`gateway_hits() == 0`、`credential() == None`、`port_is_free(sandbox.port)`。另断言 `"   \n"` 仍是 `emptyKey`（空值判定在前）。
- `printable_symbols_in_key_are_accepted`：`save_key` 写入 ``format!("\n{}\n", r#"hk-A\B"C'D$E;F#G!`{}|~"#)`` 成功，`credential()` 等于去掉首尾换行后的原文（`trim` 在字符校验之前）。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test -p helper-core --lib credential::` 与 `cargo test -p helper-core --test manager invalid_key_chars`
期望：前者编译失败（`is_allowed_token` 不存在）；后者编译通过、运行 FAIL，`invalid_key_chars_are_rejected_before_gateway_and_writes` 报 `操作应失败`（假网关返回 200，`hk 1` 被启用）。

- [ ] **步骤 3：实现** `is_allowed_token`；`validate_token` 保留两条原有错误文案，只把字符判定换成调用它；`check_new_key` 按上面顺序插入一行判定。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test -p helper-core`
期望：全部 PASS，现有 `save_key_validates_and_leaves_files_alone`（`\t{KEY} ` 被 trim 后保存）不受影响。

- [ ] **步骤 5：门禁**（见全局约束）

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/credential.rs crates/helper-core/src/credential/keychain.rs \
        crates/helper-core/src/manager.rs crates/helper-core/tests/manager.rs
git commit -F /tmp/codex-helper-c3-msg.txt   # 标题：feat(core): Key 只允许可打印 ASCII（全平台）
```

### 任务 C4：安装位置判定与受管命令读取（纯函数）

**文件：**
- 修改：`crates/helper-core/src/paths.rs`（`is_transient_install_path` 与单元测试）
- 修改：`crates/helper-core/src/codex_config.rs`（`managed_command` 与单元测试）

**接口：**
- 产出（`paths`，全平台编译与测试）：`pub fn is_transient_install_path(path: &Path) -> bool` —— 对 `path.to_string_lossy()` 做字符串判定：以 `/Volumes/` 开头，或包含 `/AppTranslocation/`，即 `true`。区分大小写（`current_exe` 给出的是真实大小写）；不访问文件系统；Windows 路径恒为 `false`。
- 产出（`codex_config`）：`pub fn managed_command(config_path: &Path) -> Result<Option<String>, ConfigError>` —— 读出 `model_providers.managed_gateway.auth.command` 的当前值。

| 情形 | 结果 |
| --- | --- |
| 文件不存在 | `Ok(None)` |
| 无法解析 / 非 UTF-8 / 根键类型不符（同 `load`） | `Err(ConfigError::Parse { .. })` |
| `model_providers`、受管节或 `auth` 缺失或不是 table-like，`command` 缺失、不是字符串或为空串 | `Ok(None)` |
| 其他 | `Ok(Some(command))`，逐字返回（含 Windows 反斜杠路径） |

对结构宽松（与 `has_residue` 一致，用 `as_table_like`），只读不写；结构是否可校正仍由随后的 `apply_managed` 判定。

- [ ] **步骤 1：写失败测试**

`paths.rs::tests::transient_install_path_detection`：

```rust
for (path, expected) in [
    ("/Volumes/CodexHelper/CodexHelper.app/Contents/MacOS/codex-helper-credential", true),
    ("/private/var/folders/zz/abc/T/AppTranslocation/8E1A-44F2/d/CodexHelper.app/Contents/MacOS/codex-helper-credential", true),
    ("/Applications/CodexHelper.app/Contents/MacOS/codex-helper-credential", false),
    ("/Users/u/Applications/CodexHelper.app/Contents/MacOS/codex-helper-credential", false),
    ("/Users/u/Downloads/CodexHelper.app/Contents/MacOS/codex-helper-credential", false),
    ("/Volumes", false),
    ("/VolumesX/a", false),
    ("/Users/u/Volumes/a", false),
    ("/volumes/a", false),
    ("/Users/u/AppTranslocationX/a", false),
    (r"C:\Program Files\CodexHelper\codex-helper-credential.exe", false),
] {
    assert_eq!(is_transient_install_path(Path::new(path)), expected, "{path}");
}
```

`codex_config.rs::tests`：
- `managed_command_reads_current_auth_command`：`Fixture::new()` 时为 `Ok(None)`；`fixture.apply(COMMAND)` 后为 `Some(COMMAND)`；再 `apply` 一个含中文与空格的 Windows 路径（同 `windows_command_path_is_escaped_as_basic_string` 的值）后逐字读回该值。
- `managed_command_is_none_without_usable_command`：对下列 `Fixture::with(text)` 均为 `Ok(None)`：`"model = \"x\"\n"`、`"[model_providers.managed_gateway]\nname = \"x\"\n"`、`"[model_providers.managed_gateway.auth]\nargs = []\n"`、`"[model_providers.managed_gateway.auth]\ncommand = 1\n"`、`"[model_providers.managed_gateway.auth]\ncommand = \"\"\n"`、`"model_providers = 1\n"`；`"[model_providers.managed_gateway]\nauth = { command = \"x\" }\n"` 为 `Ok(Some("x"))`。
- `managed_command_rejects_unparsable_config`：`"model = [\n"` 与 `"model_provider = 1\n"` 为 `Err(ConfigError::Parse { .. })`；两者调用后文件内容不变。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test -p helper-core --lib -- paths:: codex_config::`
期望：编译失败，报 `is_transient_install_path`、`managed_command` 不存在。

- [ ] **步骤 3：实现**两个函数（`managed_command` 复用 `load`、`MODEL_PROVIDERS_KEY`、`AUTH_KEY`、`consts::PROVIDER_ID`）。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test -p helper-core --lib -- paths:: codex_config::`
期望：全部 PASS。

- [ ] **步骤 5：门禁**（见全局约束）

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/paths.rs crates/helper-core/src/codex_config.rs
git commit -F /tmp/codex-helper-c4-msg.txt   # 标题：feat(core): 新增临时安装位置判定与受管 auth.command 读取
```

### 任务 C5：安装位置门控（启用、开机自启、启动对账）

**文件：**
- 修改：`crates/helper-core/src/manager.rs`
- 测试：`crates/helper-core/tests/manager.rs`

**接口：**
- 消费：C2 `ManagerError::InstallLocation`、`Status::install_location_ok`；C4 `paths::is_transient_install_path`、`codex_config::managed_command`。
- Manager 如何得知自身凭据程序路径：沿用 `ManagerOptions::credential_command`（生产由 `paths::credential_command_path()` 从 `current_exe()` 推出，即 `.app/Contents/MacOS/codex-helper-credential`）。`ManagerOptions` 不加字段，桌面层与 `manager_futures_are_send` 的结构体字面量不用改。
- 测试如何注入临时安装路径：`Manager::new(sandbox.options_with(TRANSIENT_COMMAND))`（`Sandbox::options_with` 已存在）。`tests/manager.rs` 顶部新增两个常量（各平台都有用例引用，不会 dead code）：
  - `const TRANSIENT_COMMAND: &str = "/private/var/folders/zz/abc/T/AppTranslocation/8E1A-44F2/d/CodexHelper.app/Contents/MacOS/codex-helper-credential";`
  - `const VOLUME_COMMAND: &str = "/Volumes/CodexHelper/CodexHelper.app/Contents/MacOS/codex-helper-credential";`
- 新增私有方法（`manager.rs`）：
  - `fn transient_install(&self) -> bool` = `cfg!(target_os = "macos") && paths::is_transient_install_path(Path::new(&self.options.credential_command))`。非 macOS 恒为 `false`，因此 Windows / Linux 行为不变。
  - `fn effective_command(&self) -> String`：非临时位置 → `options.credential_command`；临时位置 → `codex_config::managed_command(config)` 的 `Ok(Some(旧值))`，其余情况回退 `options.credential_command`。只给 `build_status` 判定 `config_managed` 用。

行为表：

| 入口 | 临时安装位置（仅 macOS）时 |
| --- | --- |
| `build_status` | `install_location_ok = !transient_install()`；`config_managed` 用 `effective_command()` 调 `codex_config::inspect` 判定（按旧值）。其余字段不变 |
| `enable` | `enable_inner` 第一步（预检与 Key 校验之前）返回 `InstallLocation`，日志 step 为 `"install_location"`；不访问网关、不写任何文件或凭据、不启动代理 |
| `set_autostart_flag(true)` | 取锁后直接返回 `InstallLocation`，不读写状态文件。`false` 照常执行（撤销流程要用） |
| `reconcile_on_startup`（已启用、凭据存在） | 不用 `options.credential_command`：`managed_command(config)` 为 `Some(旧值)` → 用旧值 `apply_managed`，其余步骤（代理、`.env`）照旧，结论 `Applied`；为 `None` → 返回 `InstallLocation`（结论 `Failed`），在 `apply_managed` 之前，不改任何文件、不启动代理；`Err` → 经 `config_error` 返回（`configInvalid`） |
| `reconcile_on_startup`（停用 / 缺 Key） | 不变（`Idle` / `ResidueFound` / `NeedsKey`） |
| `disable`、`clear_key`、`save_key`、`cleanup_residue`、`remove_legacy_env_block`、`shutdown` | 不变 |

- [ ] **步骤 1：写失败测试**（`tests/manager.rs`，新开一节 `// ---- 19. 安装位置 ----`）

- `status_flags_transient_install_location`（`#[cfg(target_os = "macos")]`）：对 `TRANSIENT_COMMAND`、`VOLUME_COMMAND` 各建一个 Manager，`status().install_location_ok == false`；`sandbox.manager()`（`COMMAND`）为 `true`。
- `enable_from_transient_location_is_rejected_without_writes`（macOS，multi_thread）：`write_config(USER_CONFIG)`、`write_env(USER_ENV)`，`before = snapshot()`；`TRANSIENT_COMMAND` 下 `enable(with_key(KEY))` → `installLocation`，`to_string()` 等于 `请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开`；`credentials.write(OLD_KEY)` 后 `enable(EnableRequest::default())` 同样 `installLocation`；`VOLUME_COMMAND` 下再试一次。断言 `snapshot() == before`、`gateway_hits() == 0`、`credential() == Some(OLD_KEY)`、`port_is_free(sandbox.port)`。
- `autostart_on_from_transient_location_is_rejected`（macOS）：`set_autostart_flag(true)` → `installLocation`，`state_file()` 不存在；`set_autostart_flag(false)` → `Ok`，`!status.autostart`。
- `reconcile_from_transient_location_keeps_previous_command`（macOS，multi_thread）：`write_config(USER_CONFIG)`、`write_env(USER_ENV)`；`sandbox.manager()` 启用 `KEY` 后 `shutdown`，`state_before = state()`。`manager = Manager::new(options_with(TRANSIENT_COMMAND))`：`status()` 为 `!install_location_ok && config_managed`（按旧值）。制造漂移：`base_url` 换成 `http://evil.invalid:1`、删除 `.env`。`reconcile_on_startup()` → `outcome == Applied`、`error == None`、`report.status.config_managed && report.status.env_managed && !report.status.install_location_ok`、`proxy.state == Running`；`toml_edit` 解析后 `["model_providers"]["managed_gateway"]["auth"]["command"] == COMMAND`，配置不含 `TRANSIENT_COMMAND` 与 `evil.invalid`；`state() == state_before`。随后 `manager.disable()` 成功，`config.toml` 与 `USER_CONFIG` 逐字节一致；`assert_no_key_on_disk()`。
- `reconcile_from_transient_location_without_managed_command_fails_untouched`（macOS，multi_thread）：`write_config(USER_CONFIG)`、`write_env(USER_ENV)`、`write_state(&HelperState { enabled: true, ..Default::default() })`、`credentials.write(KEY)`，`before = snapshot()`。`TRANSIENT_COMMAND` 下 `reconcile_on_startup()` → `outcome == Failed`、`error.unwrap().code == "installLocation"`、`status.proxy.state == Stopped`；`snapshot() == before`；`port_is_free(sandbox.port)`。
- `transient_location_is_ignored_off_macos`（`#[cfg(not(target_os = "macos"))]`，multi_thread）：`TRANSIENT_COMMAND` 与 `VOLUME_COMMAND` 下 `status().install_location_ok` 均为 `true`；`TRANSIENT_COMMAND` 下 `set_autostart_flag(true)` 成功；`enable(with_key(KEY))` 成功且 `auth.command == TRANSIENT_COMMAND`；`shutdown()`。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test -p helper-core --test manager transient`
期望：macOS 上 5 个 `cfg(target_os = "macos")` 用例全部 FAIL（门控尚未生效：`install_location_ok` 仍为 `true`、启用与开机自启成功、对账把 `auth.command` 改写为 `TRANSIENT_COMMAND` 且结论为 `Applied`）。`transient_location_is_ignored_off_macos` 在 macOS 上不编译，在其他平台本来就通过（它钉的是“行为不变”）。

- [ ] **步骤 3：实现** `transient_install`、`effective_command`，按行为表改 `build_status`、`enable_inner`、`set_autostart_flag`、`reconcile_locked`。`reconcile_locked` 中“选命令”放在凭据检查之后、`apply_managed` 之前。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test -p helper-core --test manager`
期望：全部 PASS，含既有的 `reconcile_rewrites_credential_command_after_reinstall`（普通位置仍会改写 `auth.command`）。

- [ ] **步骤 5：门禁**（见全局约束）。Windows 交叉 clippy 需编译到 `transient_location_is_ignored_off_macos`。

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/manager.rs crates/helper-core/tests/manager.rs
git commit -F /tmp/codex-helper-c5-msg.txt   # 标题：feat(core): 临时安装位置下拒绝启用与开机自启，对账保留原 auth.command
```

### 任务 C6：`Manager::revoke_managed_config`（撤销受管配置）

**文件：**
- 修改：`crates/helper-core/src/manager.rs`
- 测试：`crates/helper-core/tests/manager.rs`

**接口：**
- 消费：现有 `cleanup(paths, credentials, purge_key)`、`stop_proxy`、`ensure_proxy`、`codex_env::inspect_file`。
- 产出：`pub async fn revoke_managed_config(&self) -> Result<Status, ManagerError>`，供 desktop 任务 D3 的托盘“撤销配置并退出”调用。开机自启的关闭（LaunchAgent + `set_autostart_flag(false)`）由桌面层随后完成，本方法不改 `autostart`。

行为：
1. 取操作锁（与启用 / 停用串行）。
2. `stop_proxy`。
3. `cleanup(&options.paths, options.credentials.as_ref(), false)`：不删 Key；`.env` 块、`config.toml`、状态文件的处理完全沿用现有实现（尽力完成全部步骤、返回第一个错误）。
4. 失败时把 `anyhow::Error` 还原为 `ManagerError`：`error.downcast::<ManagerError>()`，不是 `ManagerError` 时转 `Internal { message: format!("{error:#}") }`。若 `codex_env::inspect_file(&paths.env_file(), proxy_port)` 显示 `has_block`（含读失败时按“仍在”处理），则 `ensure_proxy(&mut ops)` 尽力重启代理，其错误只记日志；返回原错误。
5. 成功返回 `build_status()`。
6. 日志：`log::event("manager.revoke", json!({ "ok", "code", "proxy_restarted" }))`；`cleanup` 自身的 `manager.cleanup` 事件照常。

- [ ] **步骤 1：写失败测试**（`tests/manager.rs`，新开一节 `// ---- 20. 撤销受管配置 ----`）

- `revoke_restores_config_keeps_key_and_stops_proxy`（multi_thread）：`write_config(USER_CONFIG)`、`write_env(USER_ENV)`；启用 `KEY`；`set_autostart_flag(true)`。`revoke_managed_config()` → `Ok(status)`：`!status.enabled && !status.residue && status.key_configured && status.autostart`、`proxy.state == Stopped`；`config.toml == USER_CONFIG`、`.env == USER_ENV`（逐字节）；`credential() == Some(KEY)`；`state() == Some(HelperState { autostart: true, ..Default::default() })`；`port_is_free(sandbox.port)`。再调用一次仍 `Ok` 且 `snapshot()` 不变（幂等）。全新 Sandbox 上直接调用 → `Ok`，`snapshot()` 与调用前一致（不创建任何文件）。`assert_no_key_on_disk()`。
- `revoke_failure_restarts_proxy_when_env_block_remains`（`#[cfg(target_os = "macos")]`，multi_thread，复用 `Immutable`）：同上启用后 `env_before = read(&env())`，`Immutable::set(&env())`。`revoke_managed_config()` → `Err`，`code() == "io"` 且消息含 `.env`；`.env == env_before`；`config.toml == USER_CONFIG`（`cleanup` 继续完成配置还原）；`proxy::probe_existing(sandbox.port).await` 为真且 `status().proxy.state == Running`。同时断言现有 cleanup 语义下的中间状态：`state().unwrap().enabled == false`（配置还原成功即置为停用，manager.rs `cleanup_files`）、`status().residue` 为真（`.env` 块仍在）。这意味着重启的代理只在本次进程存活期间有效：下次启动对账走 `ResidueFound`，不会再拉起代理，所以 D3 的失败弹窗要提示用户重试撤销。`drop(immutable)` 后再撤销 → `Ok`，`.env == USER_ENV`，`!status.residue`，`port_is_free(sandbox.port)`。
- `revoke_failure_without_env_block_leaves_proxy_stopped`（multi_thread）：启用后把 `config.toml` 改写为 `"model = [\n"`。`revoke_managed_config()` → `code() == "configInvalid"`（证明 downcast 保留了原错误码，不是 `internal`）；`.env == USER_ENV`（块已移除）；`state().unwrap().enabled` 仍为真；`port_is_free(sandbox.port)`、`status().proxy.state == Stopped`。
- `manager_futures_are_send` 增加 `assert_send(&manager.revoke_managed_config());`。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test -p helper-core --test manager revoke`
期望：编译失败，报 `Manager` 没有 `revoke_managed_config` 方法。

- [ ] **步骤 3：实现** `revoke_managed_config`（按行为 1–6），方法文档写明“macOS 托盘‘撤销配置并退出’使用；不删 Key、不改 autostart”。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test -p helper-core --test manager`
期望：全部 PASS。

- [ ] **步骤 5：门禁**（见全局约束）

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/manager.rs crates/helper-core/tests/manager.rs
git commit -F /tmp/codex-helper-c6-msg.txt   # 标题：feat(core): 新增撤销受管配置（停代理 → 清理，失败时尽力恢复代理）
```

core 通道完成：`pi/macos-core` 上 C1–C6 六个提交，门禁全绿后通知主会话，desktop 通道从该提交拉出。

# 二、pack 通道（`pi/macos-pack`）

准备：

```bash
cd /Users/sxk/Documents/code/ai-space/CodexHelper
git worktree add -b pi/macos-pack ../CodexHelper-wt/pack pi/macos-mvp
cd ../CodexHelper-wt/pack && npm --prefix apps/desktop ci
```

本通道不改 Rust 与前端源码，“测试”是可执行的核验命令加期望输出：先在改动前运行，确认结果不符合期望（红），改动后再运行确认符合（绿）。三个任务都要跑完整门禁（与 core 并行，保证版本号与配置改动不破坏现有构建）。

本机工具链现状（2026-10-14 核实，实施时复核）：PATH 里的 `cargo` / `rustc` 是 Homebrew 版，只带 `aarch64-apple-darwin` 标准库；rustup 工具链 `~/.rustup/toolchains/stable-aarch64-apple-darwin` 只装了 `aarch64-apple-darwin` 与 `x86_64-pc-windows-msvc`。rustup 默认工具链是 Rosetta 下的 `stable-x86_64-apple-darwin`，不要用它构建。`@tauri-apps/cli` 2.11.5；`shellcheck` 可用，`actionlint` 未安装。

### 任务 P1：版本号统一升到 0.2.0

**文件：**
- 修改：`apps/desktop/src-tauri/tauri.conf.json`（只改 `version`）
- 修改：`Cargo.toml`（`[workspace.package] version`）
- 修改：`apps/desktop/package.json`、`apps/desktop/package-lock.json`（根 `version` 与 `packages[""].version`；目前是过期的 `0.1.0`，一并改正）
- 修改：`Cargo.lock`（`codex-helper`、`codex-helper-credential`、`helper-core` 三个条目）

**接口：**
- 产出：`tauri.conf.json` 的 `version = "0.2.0"`。它是 `release.yml` 两个 job 版本校验的唯一来源，也是 P2 dmg 文件名与 `Info.plist` 版本的来源。

- [ ] **步骤 1：写核验命令并确认失败**

```bash
(cd apps/desktop && node -p "const l=require('./package-lock.json'); [require('./src-tauri/tauri.conf.json').version, require('./package.json').version, l.version, l.packages[''].version].join(' ')")
grep -m1 '^version' Cargo.toml
awk '/^name = "(codex-helper|codex-helper-credential|helper-core)"$/{n=$3; getline; print n, $3}' Cargo.lock
```

期望（绿）：第一行 `0.2.0 0.2.0 0.2.0 0.2.0`；第二行 `version = "0.2.0"`；随后三行第二列均为 `"0.2.0"`。
现在运行得到 `0.1.2 0.1.2 0.1.0 0.1.0`、`version = "0.1.2"` 与三行 `"0.1.2"`（红，已于 2026-10-14 在 `pi/macos-mvp` 上实测）。

- [ ] **步骤 2：改版本号**

`tauri.conf.json` 与 `Cargo.toml` 手改一行；`(cd apps/desktop && npm version 0.2.0 --no-git-tag-version)`（同时改 package.json 与 lock，不装依赖、不打 tag）；`cargo update --workspace --offline`（只刷新工作区成员在 `Cargo.lock` 里的版本，不升级第三方依赖）。

- [ ] **步骤 3：重跑核验命令确认通过**，并核验改动范围：

```bash
git diff --stat pi/macos-mvp
git diff -U0 pi/macos-mvp -- apps/desktop/src-tauri/tauri.conf.json Cargo.toml apps/desktop/package.json apps/desktop/package-lock.json Cargo.lock | grep '^[-+] '
```

期望：只有上面 5 个文件；每个 `-` 行与对应 `+` 行只有版本号不同（`Cargo.lock` 共 3 对、`package-lock.json` 2 对、其余各 1 对），没有依赖条目变化。`tauri.conf.json` 除 `version` 外无变化（Windows 打包配置不变）。

- [ ] **步骤 4：门禁**（见全局约束）

- [ ] **步骤 5：提交**

```bash
git add apps/desktop/src-tauri/tauri.conf.json Cargo.toml apps/desktop/package.json \
        apps/desktop/package-lock.json Cargo.lock
git commit -F /tmp/codex-helper-p1-msg.txt   # 标题：chore: 版本号升至 0.2.0（Windows 与 macOS 同步发布）
```

### 任务 P2：macOS 打包配置与 `scripts/build-macos.sh`（本机跑通 universal dmg）

**文件：**
- 新建：`apps/desktop/src-tauri/tauri.macos.conf.json`
- 新建：`scripts/build-macos.sh`（可执行，git 文件模式 `100755`）

**接口：**
- 消费：P1 的 `version = "0.2.0"`；现有 `tauri.bundle.conf.json`（`externalBin: ["binaries/codex-helper-credential"]`，不改）。
- 产出（`tauri.macos.conf.json`，完整内容只有这些键）：

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "bundle": {
    "targets": ["app", "dmg"],
    "createUpdaterArtifacts": false,
    "macOS": { "signingIdentity": "-", "minimumSystemVersion": "12.0" }
  }
}
```

- 产出（`scripts/build-macos.sh`，无参数，本机与 P3 的 CI 共用）：

| 阶段 | 约定 |
| --- | --- |
| 开头 | `#!/usr/bin/env bash`、`set -euo pipefail`、`cd "$(dirname "$0")/.."`；头部中文注释写明用途、前置条件，以及步骤 1 实验确认的合并方式与 externalBin 后缀 |
| 平台 | `uname -s` 不是 `Darwin` → stderr 中文提示，退出 1 |
| 工具链 | 与 `check-windows.sh` 相同：`TC="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"` 存在时 `export PATH="$TC:$PATH" RUSTC="$TC/rustc"`（避开只有 aarch64 标准库的 Homebrew rustc；CI 上 dtolnay 装的就是这个工具链） |
| 预检 | `cargo`、`rustc`、`lipo` 任一不在 PATH → 中文提示，退出 1；`aarch64-apple-darwin`、`x86_64-apple-darwin` 任一不在 `"$(rustc --print sysroot)/lib/rustlib/"` 下 → 提示 `rustup target add --toolchain stable-aarch64-apple-darwin <target>`，退出 1；`apps/desktop/node_modules/.bin/tauri` 不存在 → 提示先 `npm --prefix apps/desktop ci`，退出 1。脚本自己不安装任何东西 |
| 凭据程序 | 两个 target 各 `cargo build --release -p codex-helper-credential --target <t>`；`lipo -create` 合成到 `apps/desktop/src-tauri/binaries/codex-helper-credential-<后缀>`（后缀按步骤 1 实验结果，设计预期 `universal-apple-darwin`）；逐个架构执行 `lipo <文件> -verify_arch arm64` 与 `-verify_arch x86_64` 校验（本机 lipo 一次只接受一个架构） |
| 打包 | 先删除 `target/universal-apple-darwin/release/bundle/{dmg,macos}` 旧产物（避免旧版本 dmg 被误认）；`cd apps/desktop && npx tauri build --target universal-apple-darwin --config src-tauri/tauri.bundle.conf.json` |
| 自检 | 定位 `bundle/dmg/*.dmg`，必须恰好 1 个，否则退出 1；对 `.app` 内 `codex-helper` 与 `codex-helper-credential` 逐个架构执行 `lipo <文件> -verify_arch arm64` / `x86_64`；`codesign --verify --deep --strict` 该 `.app` |
| 输出 | 最后打印 `产物：<dmg 路径>`；若设置了 `GITHUB_OUTPUT`，追加一行 `dmg=<dmg 路径>`（P3 用） |

不在脚本里设置或要求 `TAURI_SIGNING_PRIVATE_KEY`（`createUpdaterArtifacts=false`），不调用 `publish-update.sh`。`binaries/` 与 `target/` 已在 `.gitignore`，脚本跑完 `git status --short` 应为空。

- [ ] **步骤 1：准备工具链并做小实验，确认 Tauri 2.11 的实际行为（设计 §11）**

工具链（会给 rustup 的 aarch64 工具链下载 x86_64 标准库，不是项目依赖）：
`rustup target add --toolchain stable-aarch64-apple-darwin x86_64-apple-darwin`

实验在 worktree 里用临时文件做，结论写进脚本头部注释与提交信息，实验文件不提交：

1. 合并方式：先只放上面的 `tauri.macos.conf.json`，手工构建单架构 `.app`（较快）：
   `cargo build --release -p codex-helper-credential`，复制为 `binaries/codex-helper-credential-aarch64-apple-darwin`，在 `apps/desktop` 下以 `env -u TAURI_SIGNING_PRIVATE_KEY npx tauri build --target aarch64-apple-darwin --bundles app --config src-tauri/tauri.bundle.conf.json` 构建。
   期望：构建成功（证明 `createUpdaterArtifacts=false` 覆盖了主配置的 `true`，否则会因缺签名私钥失败）；`target/aarch64-apple-darwin/release/bundle/` 下没有 `nsis` 目录、没有 `*.tar.gz` / `*.sig`（`targets` 数组是整体替换而非拼接）；`Info.plist` 的 `LSMinimumSystemVersion` 为 `12.0`；`.app/Contents/MacOS` 里有 `codex-helper-credential`（`--config` 与平台文件同时生效）。若任一不符，以实测为准调整 `tauri.macos.conf.json`（例如数组被拼接时改用 `--bundles app,dmg`），并在计划执行记录里注明。
2. universal 下的 externalBin 后缀：删掉 `binaries/` 下其他文件，只放 lipo 合成的 `codex-helper-credential-universal-apple-darwin`，运行 universal 构建。
   期望：成功，`.app/Contents/MacOS/codex-helper-credential` 是 fat 文件。若 Tauri 报缺少 `…-aarch64-apple-darwin` / `…-x86_64-apple-darwin`，把脚本改为按报错要求的文件名放置（同一个 universal 文件复制成所需名字），在脚本头部注释记录。
3. dmg 产物路径与文件名：记录实际路径（预期 `target/universal-apple-darwin/release/bundle/dmg/CodexHelper_0.2.0_universal.dmg`）。若 dmg 步骤因 Finder 自动化授权弹框或挂起，改用 `CI=true` 运行并确认 Tauri 跳过窗口布局；这样的话脚本内 `export CI="${CI:-true}"`，并注释原因。

- [ ] **步骤 2：写核验命令并确认失败**

```bash
B=target/universal-apple-darwin/release/bundle; APP="$B/macos/CodexHelper.app"
node -e 'require("assert").deepStrictEqual(require("./apps/desktop/src-tauri/tauri.macos.conf.json"), {"$schema":"https://schema.tauri.app/config/2",bundle:{targets:["app","dmg"],createUpdaterArtifacts:false,macOS:{signingIdentity:"-",minimumSystemVersion:"12.0"}}}); console.log("conf-ok")'
shellcheck scripts/build-macos.sh && bash -n scripts/build-macos.sh && test -x scripts/build-macos.sh && echo script-ok
lipo -info "$APP/Contents/MacOS/codex-helper" "$APP/Contents/MacOS/codex-helper-credential"
ls "$APP/Contents/MacOS"
codesign -dv "$APP" 2>&1 | grep -E '^(Identifier|Signature|TeamIdentifier)='
codesign -dv "$APP/Contents/MacOS/codex-helper-credential" 2>&1 | grep '^Signature='
codesign --verify --deep --strict "$APP" && echo verify-ok
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' -c 'Print :CFBundleVersion' \
  -c 'Print :LSMinimumSystemVersion' -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist"
find "$B" \( -name '*.tar.gz' -o -name '*.sig' -o -name '*.exe' \) -print
MNT=$(mktemp -d) && hdiutil attach -nobrowse -readonly -mountpoint "$MNT" "$B"/dmg/*.dmg >/dev/null \
  && ls "$MNT" && lipo -info "$MNT/CodexHelper.app/Contents/MacOS/codex-helper-credential"; \
  hdiutil detach "$MNT" && rmdir "$MNT"
```

期望（绿）：
- `conf-ok`、`script-ok`
- 两条 `lipo -info` 都列出 `x86_64` 与 `arm64`
- `Contents/MacOS` 只有 `codex-helper` 与 `codex-helper-credential`（无三元组后缀）
- `Identifier=com.codexhelper.desktop`、`Signature=adhoc`、`TeamIdentifier=not set`；凭据程序 `Signature=adhoc`；`verify-ok`
- PlistBuddy 依次输出 `0.2.0`、`0.2.0`、`12.0`、`com.codexhelper.desktop`
- `find` 无输出（没有升级产物、没有 Windows 产物）
- 挂载目录里有 `CodexHelper.app` 与 `Applications`，其中凭据程序同样含两种架构；`hdiutil detach` 成功卸载。只挂载只读查看，不 `open` 任何 `.app`

现在运行：前两条报找不到文件，其余报路径不存在（红）。

- [ ] **步骤 3：按“接口”写 `tauri.macos.conf.json` 与 `scripts/build-macos.sh`**（`chmod +x`），结合步骤 1 的结论定稿。

- [ ] **步骤 4：本机跑通并核验**

运行：`scripts/build-macos.sh`
期望：退出码 0，最后一行 `产物：…/CodexHelper_0.2.0_universal.dmg`（以步骤 1 记录的实际文件名为准）。再跑步骤 2 的核验命令，全部符合期望（绿）。

再核验预检分支：`RUSTUP_HOME=/nonexistent PATH=/usr/bin:/bin scripts/build-macos.sh; echo "exit=$?"` 期望 stderr 有找不到 `cargo` 的中文提示、`exit=1`、没有开始编译。

- [ ] **步骤 5：核验 Windows 打包配置与仓库状态无变化**

```bash
git status --short
git diff pi/macos-mvp --stat
git diff pi/macos-mvp -- apps/desktop/src-tauri/tauri.conf.json | grep '^[-+] '
git diff --exit-code pi/macos-mvp -- apps/desktop/src-tauri/tauri.bundle.conf.json apps/desktop/src-tauri/windows .github scripts/check-windows.sh scripts/gate.sh scripts/publish-update.sh && echo windows-unchanged
```

期望：`git status --short` 只列出本任务两个新文件（`binaries/`、`target/` 不出现）；`--stat` 只有 P1 的 5 个文件加本任务 2 个新文件；`tauri.conf.json` 的差异只有 `-  "version": "0.1.2",` / `+  "version": "0.2.0",` 一对；最后输出 `windows-unchanged`。Tauri 只在 macOS 上合并 `tauri.macos.conf.json`，Windows 构建不读取它。

- [ ] **步骤 6：门禁**（见全局约束）

- [ ] **步骤 7：提交**

```bash
git add apps/desktop/src-tauri/tauri.macos.conf.json scripts/build-macos.sh
git ls-files -s scripts/build-macos.sh   # 期望模式 100755
git commit -F /tmp/codex-helper-p2-msg.txt   # 标题：build(macos): 新增 macOS 打包配置与 universal dmg 构建脚本
```

提交信息正文写明步骤 1 的三条实测结论（合并方式、externalBin 后缀、dmg 路径），P3 依赖这些结论。

### 任务 P3：`release.yml` 新增 `macos-release` job 与安装说明

**文件：**
- 修改：`.github/workflows/release.yml`（只在文件末尾追加 job；`on`、`permissions`、顶层 `env`、`windows-release` 一字不改）

**接口：**
- 消费：P1 的版本号；P2 的 `scripts/build-macos.sh`（无参数；在 `GITHUB_OUTPUT` 写 `dmg=<路径>`）。
- 产出：job `macos-release`，供 tag `v0.2.0` 触发。

job 约定：

| 项 | 值 |
| --- | --- |
| `runs-on` / `needs` | `macos-latest` / `windows-release`（串行，避免两个 job 同时创建同一 tag 的 Release） |
| `defaults.run.shell` | `bash` |
| 步骤顺序 | 检出代码 → 安装 Node.js（与 Windows 相同：22、npm 缓存）→ 校验 tag 版本与 tauri.conf.json 一致 → 安装 Rust 工具链 → 缓存 cargo 构建产物 → 安装前端依赖 → 构建 universal dmg → 上传 dmg 到 GitHub Release |
| 版本校验 | 与 `windows-release` 的同名步骤逐字相同（复制，不抽脚本、不用 YAML 锚点） |
| Rust 工具链 | `dtolnay/rust-toolchain@stable`，`with.targets: aarch64-apple-darwin,x86_64-apple-darwin` |
| 构建 | `id: build`，`run: bash scripts/build-macos.sh`；不设置 `TAURI_SIGNING_PRIVATE_KEY`、不调用 `publish-update.sh`（`latest.json` 仍只含 `windows-x86_64`）。顶层 `env` 的 `TARGET_TRIPLE` / `SIDECAR_NAME` 会被继承，P2 脚本不得读取这两个变量 |
| 上传 | `softprops/action-gh-release@v2`，`files: ${{ steps.build.outputs.dmg }}`，`fail_on_unmatched_files: true`，`body` 为下方安装说明。`windows-release` 不写 body，同一 Release 由本 job 补正文 |

Release 正文（设计 §7.3，逐字使用）：

```markdown
## macOS 安装说明（内部试用）

1. 打开 dmg，把 CodexHelper 拖到「应用程序」。
2. 首次打开被拦截时：系统设置 → 隐私与安全性 → “仍要打开”；或执行
   `xattr -dr com.apple.quarantine /Applications/CodexHelper.app`。
3. 卸载：先在菜单栏图标选“撤销配置并退出”，再把应用拖进废纸篓。如果已经先删了应用、Codex 打不开对话：重新安装后执行一次撤销。
4. 删除 Key（可选）：`security delete-generic-password -s codex-helper/managed-gateway -a codex-helper`。
```

- [ ] **步骤 1：写核验命令并确认失败**

前置工具：`ruby`（只用标准库 `yaml`；本机为 Homebrew ruby，2026-10-14 核实可用）。换机器执行时先 `command -v ruby` 确认；缺失就停下来报给用户，不要为核验给项目加 js-yaml 之类依赖。

```bash
F=.github/workflows/release.yml
if command -v actionlint >/dev/null; then actionlint "$F" && echo lint-ok; \
  else ruby -ryaml -e 'YAML.load_file(ARGV[0]); puts "yaml-ok"' "$F"; fi
ruby -ryaml -e '
  j = YAML.load_file(ARGV[0])["jobs"]; m = j.fetch("macos-release"); s = m["steps"]
  abort "runs-on/needs" unless m["runs-on"] == "macos-latest" && m["needs"] == "windows-release"
  ver = ->(job) { job["steps"].find { |x| x["name"] == "校验 tag 版本与 tauri.conf.json 一致" } }
  abort "版本校验不一致" unless ver.(m) && ver.(m) == ver.(j["windows-release"])
  abort "targets" unless s.find { |x| x["uses"].to_s.start_with?("dtolnay/rust-toolchain@") }&.dig("with", "targets") == "aarch64-apple-darwin,x86_64-apple-darwin"
  b = s.find { |x| x["id"] == "build" }; abort "build" unless b && b["run"].include?("scripts/build-macos.sh")
  u = s.find { |x| x["uses"].to_s.start_with?("softprops/action-gh-release@") }&.fetch("with")
  abort "upload" unless u && u["files"].include?("steps.build.outputs.dmg") && u["fail_on_unmatched_files"] == true
  %w[拖到「应用程序」 com.apple.quarantine 撤销配置并退出 delete-generic-password].each { |k| abort k unless u["body"].include?(k) }
  abort "不应出现升级发布" if m.to_s.include?("publish-update") || m.to_s.include?("TAURI_SIGNING_PRIVATE_KEY")
  puts "macos-job-ok"' "$F"
git diff -U0 pi/macos-mvp -- "$F" | grep -c '^-[^-]'
```

期望（绿）：`lint-ok` 或 `yaml-ok`；`macos-job-ok`；最后一行 `0`（只有新增行，`windows-release` 与其他已有内容一字未改）。
现在运行：第二条以 `key not found: "macos-release"`（`KeyError`）失败（红）。本机无 `actionlint` 时不要为此安装新工具，用 ruby 解析代替，并在提交信息里注明未跑 actionlint。

- [ ] **步骤 2：按 job 约定在 `release.yml` 末尾追加 `macos-release`**，步骤名与注释用中文，风格与 `windows-release` 一致（每步上方一两行说明“为什么”）。

- [ ] **步骤 3：重跑步骤 1 的核验命令确认通过**；另用 P2 留下的产物模拟 CI 输出约定：`export GITHUB_OUTPUT=$(mktemp) && CI=true bash scripts/build-macos.sh && cat "$GITHUB_OUTPUT"; rm -f "$GITHUB_OUTPUT"`，期望最后输出 `dmg=` 开头、指向存在的 `CodexHelper_0.2.0_universal.dmg`（增量构建，较快）。CI 实跑在打 tag 后由人工确认，本计划不 push、不打 tag。

- [ ] **步骤 4：门禁**（见全局约束）

- [ ] **步骤 5：提交**

```bash
git add .github/workflows/release.yml
git commit -F /tmp/codex-helper-p3-msg.txt   # 标题：ci(release): 新增 macOS universal dmg 发布 job 与安装说明
```

pack 通道完成：`pi/macos-pack` 上 P1–P3 三个提交，门禁全绿后通知主会话合并。

# 三、desktop 通道（`pi/macos-desktop`）

准备（core 通道 C1–C6 全部提交、门禁全绿之后）：

```bash
cd /Users/sxk/Documents/code/ai-space/CodexHelper
git log --oneline pi/macos-mvp..pi/macos-core   # 期望恰好 6 个提交（C1–C6）
git worktree add -b pi/macos-desktop ../CodexHelper-wt/desktop pi/macos-core
cd ../CodexHelper-wt/desktop && npm --prefix apps/desktop ci
```

本通道消费的 core 接口（`pi/macos-core` 上已有）：`Status::{platform, update_supported, install_location_ok}`；`ManagerError::{InvalidKeyChars, InstallLocation}`（经 `ErrorPayload::from` 得到 `code` / `message`）；`Manager::revoke_managed_config`；`Manager::set_autostart_flag`。本通道不改 `crates/helper-core/**`，唯一例外是 D1 改 `CODEX_PROCESS_NAME` 这一行。

撤销确认框为什么不用 tauri-plugin-dialog（2026-10-14 读 `Cargo.lock` 锁定的 tauri-plugin-dialog 2.7.3 与 rfd 0.16.0 源码得出）：
- `MessageDialogBuilder` 没有设置默认按钮或默认焦点的 API；macOS 上 `OkCancelCustom(a, b)` 的 `a` 是 default button（回车触发、高亮），`b` 是 alternate button。
- rfd（`backend/macos/utils/user_alert.rs`）在 `CFUserNotificationDisplayAlert` 调用失败、`kCFUserNotificationCancelResponse`、无法识别的响应时都返回 `Cancel`。
- 插件 `desktop.rs::show_message_dialog` 随后对所有平台无条件执行 `(Cancel, OkCancelCustom(_ok, cancel)) => Custom(cancel)`（注释说是为 Linux 加的，但没有 cfg 限制）。
- 结论：用这个插件时，第二个按钮的点击和“取消 / 失败 / 未知响应”在结果层无法区分。把“撤销并退出”放在第二位，失败会被当成确认；放在第一位，回车就会执行撤销，违反设计 §6“默认聚焦取消”。两种排法都不安全。
- 现有“退出”确认（`OkCancelCustom("退出", "取消")` + `show`）不受影响：取消类结果被映射成 `Custom("取消")`，仍判为不退出。本计划不改它。

因此 D3 的撤销确认框改用 `/usr/bin/osascript` 的 `display dialog`（系统自带，不新增依赖），设计 §6 的文案、按钮和“默认聚焦取消”都不变，只换实现机制：
- `buttons {"取消", "撤销并退出"} default button "取消" cancel button "取消"`：“取消”高亮、回车命中；回车 / Esc / ⌘. / 点“取消”都触发 cancel button，osascript 以错误 -128 退出（退出码 1）。
- 文案经 `on run argv` 作为参数传入，不拼进脚本文本，不存在 AppleScript 注入。
- 脚本只在“返回的按钮等于撤销按钮”时输出固定标记 `revoke-confirmed`；`giving up after 600` 超时（避免对话框被遮住时互斥一直被占着）、其他按钮都输出 `revoke-cancelled`。
- Rust 侧只有“退出码 0 且 stdout 恰好是 `revoke-confirmed\n`”才算确认。退出码非 0、被信号终止、无法启动、stdout 是其他内容，一律视为取消。这个判定是纯函数，有单元测试。
- 脚本语法由 `/usr/bin/osacompile` 编译检查（不显示任何界面），2026-10-14 已在本机试编通过。
- 未验证（不能启动 GUI）：对话框从后台托盘进程弹出时能不能拿到焦点、实际按键效果。列入第四节的实机验收。
- 这是相对原计划（插件对话框）的实现偏离，设计层面的行为没有变。执行 D3 之前需要用户确认这个方案（见第四节汇报项）。

### 任务 D1：Codex 进程名按平台拆分，macOS 用 `pgrep` 检测

**文件：**
- 修改：`crates/helper-core/src/consts.rs`（只改 `CODEX_PROCESS_NAME` 及其文档注释）
- 修改：`apps/desktop/src-tauri/src/process.rs`

**接口：**
- 产出（`consts`）：`#[cfg(windows)] pub const CODEX_PROCESS_NAME: &str = "codex.exe";`、`#[cfg(not(windows))] pub const CODEX_PROCESS_NAME: &str = "codex";`。macOS 引擎进程名为实测值；Linux 是开发模式，不检测，这个值只为能编译。
- 产出（`process`）：
  - `pub fn running_from_pgrep_exit(code: Option<i32>) -> bool`：全平台编译；非 macOS 加 `#[cfg_attr(not(target_os = "macos"), allow(dead_code))]`。`Some(1)` → `false`，其余（`Some(0)`、其他退出码、`None`）→ `true`。
  - `#[cfg(target_os = "macos")] fn process_running(name: &str) -> bool`：执行 `/usr/bin/pgrep -x <name>`（路径用本文件私有常量 `PGREP_TOOL`，不放进 consts），stdin / stdout / stderr 全部 `Stdio::null()`，调 `status()`。启动失败时记 `desktop.process_snapshot` `{ "ok": false, "error": … }`（事件名与 Windows 相同）并返回 `true`；否则返回 `running_from_pgrep_exit(status.code())`。不设超时：调用方已在 `spawn_blocking` 里执行，pgrep 只读进程表。
  - `is_codex_running()`：Windows 分支不变；`#[cfg(target_os = "macos")]` 调 `process_running(consts::CODEX_PROCESS_NAME)`；`#[cfg(not(any(windows, target_os = "macos")))]` 继续返回 `false`。
  - 模块文档改为分别说明三个平台。

- [ ] **步骤 1：写失败测试**（`process.rs` 的 `tests`）

```rust
#[test] fn codex_process_name_per_platform() {
    assert_eq!(consts::CODEX_PROCESS_NAME, if cfg!(windows) { "codex.exe" } else { "codex" });
}
#[test] fn pgrep_exit_code_mapping() {
    assert!(running_from_pgrep_exit(Some(0)));
    assert!(!running_from_pgrep_exit(Some(1)));
    for code in [Some(2), Some(3), Some(-1), None] { assert!(running_from_pgrep_exit(code)); }
}
```

- `process_name_match_ignores_ascii_case`：前两条断言里的 `consts::CODEX_PROCESS_NAME` 改成字面量 `"codex.exe"`。`matches_process_name` 只给 Windows 用，测试不再依赖平台常量。
- `#[cfg(target_os = "macos")] pgrep_reports_absent_process_as_not_running`：`process_running(&format!("chx-none-{}", std::process::id()))` 为 `false`。名字不超过 15 字节，避免进程名被截断。
- `#[cfg(target_os = "macos")] pgrep_finds_running_child`：`Command::new("/bin/sleep").arg("30").spawn()`，先把 `process_running("sleep")` 的结果存到变量，`kill()` + `wait()` 之后再断言为 `true`，保证断言失败时子进程也已被清理。
- `non_windows_never_reports_codex_running` 的 cfg 改为 `not(any(windows, target_os = "macos"))`。

- [ ] **步骤 2：运行测试，确认失败**

运行：`cargo test -p codex-helper process::`
期望：编译失败，报 `running_from_pgrep_exit`、`process_running` 不存在。

- [ ] **步骤 3：实现**：按“接口”拆分常量、加纯函数和 macOS 分支。

- [ ] **步骤 4：运行测试，确认通过**

运行：`cargo test -p codex-helper process::` 和 `cargo test -p helper-core`
期望：全部 PASS。

- [ ] **步骤 5：门禁**（见全局约束）。Windows 交叉 clippy 会编译 `windows_impl`，常量在 Windows 上仍是 `codex.exe`。

- [ ] **步骤 6：提交**

```bash
git add crates/helper-core/src/consts.rs apps/desktop/src-tauri/src/process.rs
git commit -F /tmp/codex-helper-d1-msg.txt   # 标题：feat(desktop): macOS 退出前用 pgrep 检测 Codex，进程名按平台区分
```

### 任务 D2：命令层门控（非 Windows 拒绝升级；开机自启先查安装位置）

**文件：**
- 修改：`apps/desktop/src-tauri/src/update.rs`
- 修改：`apps/desktop/src-tauri/src/commands.rs`

**接口：**
- 消费：`Status::{update_supported, install_location_ok}`、`ManagerError::InstallLocation`。
- 产出（`update`）：`fn ensure_update_supported() -> Result<(), ErrorPayload>`。`cfg!(windows)` 为真时返回 `Ok(())`；否则返回 `code = "update"`、`message = "当前平台不支持内部升级，请到 GitHub Release 下载新版本"`。判断条件必须和 `Status::update_supported` 一样用 `cfg!(windows)`。`check_update` 与 `install_update` 第一行先调它，在 `build_updater` 之前执行，所以不读环境变量、不请求检测地址。`app_version` 不变，`tauri_plugin_updater` 的注册也不变。
- 产出（`commands`）：`fn autostart_precheck(enabled: bool, install_location_ok: bool) -> Result<(), ErrorPayload>`。只有 `enabled && !install_location_ok` 时返回 `Err(ManagerError::InstallLocation.into())`。
- 产出（`commands`）：`pub(crate) fn apply_autostart_checked(enabled: bool, install_location_ok: bool, apply_system: impl FnOnce(bool) -> Result<(), String>) -> Result<(), ErrorPayload>`。先 `autostart_precheck`，失败时直接返回、**不调用** `apply_system`；通过后调用 `apply_system(enabled)`，其错误经 `autostart_error` 映射为 `internal`。`set_autostart` 和 D3 的撤销流程都经它调系统自启插件，“预检失败不写 LaunchAgent”就能用假闭包测，测试不碰真实 `~/Library/LaunchAgents`。

`set_autostart` 的执行顺序：

| 步骤 | 内容 |
| --- | --- |
| 1 | `install_location_ok` 只在 `enabled` 为真时查（`manager.status().await.install_location_ok`），否则按 `true`；调 `apply_autostart_checked(enabled, install_location_ok, \|on\| apply_system_autostart(&app, on))`。预检失败时记 `desktop.set_autostart` `{ "enabled": true, "ok": false, "code": "installLocation" }` 并返回，不调 `apply_system_autostart`。否则插件会把临时路径写进 LaunchAgent，随后 `set_autostart_flag(true)` 才会拒绝，LaunchAgent 就留了下来 |
| 2 | 插件失败时照旧记 `{ "enabled", "ok": false }` 并返回；成功后照旧 `set_autostart_flag(enabled)`。core 的门控（C5）作为第二道防线保留 |

`enabled = false` 不做预检，撤销流程要用（D3）。`startup::sync_autostart_flag` 不改：从临时位置启动时如果系统里已有 LaunchAgent，`set_autostart_flag(true)` 返回 `installLocation`，只记日志。

- [ ] **步骤 1：写失败测试**

`update.rs::tests`：

```rust
#[test] fn update_commands_only_supported_on_windows() {
    match ensure_update_supported() {
        Ok(()) => assert!(cfg!(windows)),
        Err(e) => {
            assert!(!cfg!(windows));
            assert_eq!(e.code, "update");
            assert_eq!(e.message, "当前平台不支持内部升级，请到 GitHub Release 下载新版本");
        }
    }
}
```

`commands.rs::tests`：

```rust
#[test] fn autostart_on_rejected_from_transient_location() {
    let e = autostart_precheck(true, false).unwrap_err();
    assert_eq!(e.code, "installLocation");
    assert_eq!(e.message, "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开");
    for (on, ok) in [(true, true), (false, false), (false, true)] {
        assert!(autostart_precheck(on, ok).is_ok());
    }
}
#[test] fn checked_autostart_skips_plugin_when_precheck_fails() {
    let mut calls = Vec::new();
    let e = apply_autostart_checked(true, false, |on| { calls.push(on); Ok(()) }).unwrap_err();
    assert_eq!(e.code, "installLocation");
    assert!(calls.is_empty(), "预检失败时不得调用自启插件");
}
#[test] fn checked_autostart_calls_plugin_and_maps_its_error() {
    for (on, ok) in [(true, true), (false, false), (false, true)] {
        let mut calls = Vec::new();
        apply_autostart_checked(on, ok, |v| { calls.push(v); Ok(()) }).unwrap();
        assert_eq!(calls, [on]);
    }
    let e = apply_autostart_checked(false, true, |_| Err("拒绝访问".into())).unwrap_err();
    assert_eq!((e.code.as_str(), e.message.as_str()), ("internal", "开机自启设置失败：拒绝访问"));
}
```

- [ ] **步骤 2：运行测试，确认失败**

运行：`cargo test -p codex-helper -- update:: commands::`
期望：编译失败，报 `ensure_update_supported`、`autostart_precheck`、`apply_autostart_checked` 不存在。

- [ ] **步骤 3：实现**：三个函数，以及 `check_update`、`install_update`、`set_autostart` 的接入。`set_autostart` 的文档注释改为“Windows 写 Run 注册表项，macOS 写 LaunchAgent”。

- [ ] **步骤 4：运行测试，确认通过**

运行：`cargo test -p codex-helper`
期望：全部 PASS。

- [ ] **步骤 5：门禁**（见全局约束）

- [ ] **步骤 6：提交**

```bash
git add apps/desktop/src-tauri/src/update.rs apps/desktop/src-tauri/src/commands.rs
git commit -F /tmp/codex-helper-d2-msg.txt   # 标题：feat(desktop): 非 Windows 拒绝升级命令，开机自启先检查安装位置
```

### 任务 D3：托盘“撤销配置并退出”（确认框、撤销流程、退出流程拆分、菜单布局）

确认框、流程和菜单项放在同一个任务。分开的话，`request_revoke` 在接入托盘之前没有调用方，macOS 上 `clippy -D warnings` 会报 dead code。确认框机制见本节之前“撤销确认框为什么不用 tauri-plugin-dialog”，**开工前先确认用户已同意 osascript 方案**。

**文件：**
- 新建：`apps/desktop/src-tauri/src/revoke.rs`
- 修改：`apps/desktop/src-tauri/src/lifecycle.rs`（拆出互斥与退出步骤，`request_quit` 行为不变）
- 修改：`apps/desktop/src-tauri/src/tray.rs`（菜单布局抽成纯函数，接入撤销）
- 修改：`apps/desktop/src-tauri/src/commands.rs`（`apply_system_autostart` 改为 `pub(crate)`；D2 的 `apply_autostart_checked` 已是 `pub(crate)`）
- 修改：`apps/desktop/src-tauri/src/main.rs`（`mod revoke;`，模块文档提到 [`revoke`]）
- `capabilities/default.json` 不改：确认框由 Rust 经 osascript 弹出，失败提示用 Rust 侧的插件对话框，前端没有新增 IPC

**接口：**
- 消费：C6 `Manager::revoke_managed_config()`；`Manager::set_autostart_flag(false)`；D2 的 `commands::{apply_system_autostart, apply_autostart_checked}`；`events::{publish_status, publish_operation_failed}`。
- 产出（`lifecycle`）：
  - `impl Lifecycle { pub fn try_begin_exit(&self) -> bool; pub fn end_exit_attempt(&self) }`：对现有 `quitting` 的 `swap(true)` / `store(false)`。“退出”和“撤销配置并退出”共用这一把互斥，任一流程进行中时，另一个点击直接忽略。
  - `pub async fn exit_after_shutdown(app: &AppHandle)`：先 `shutdown_once`，再 `app.exit(0)`。不弹 Codex 运行确认。
  - `request_quit` 改为用上面三个函数实现，`desktop.quit` 日志、确认逻辑与对话框（`OkCancelCustom("退出", "取消")` + `show`）都不变。
- 产出（`revoke`，全平台编译；模块顶部 `#![cfg_attr(not(target_os = "macos"), allow(dead_code))]`）：

```rust
pub const REVOKE_TITLE: &str = "撤销配置并退出";
pub const REVOKE_MESSAGE: &str;          // 全局约束“撤销确认框正文”，逐字
pub const REVOKE_CONFIRM_LABEL: &str = "撤销并退出";
pub const REVOKE_CANCEL_LABEL: &str = "取消";
const OSASCRIPT_TOOL: &str = "/usr/bin/osascript";
const REVOKE_DIALOG_SCRIPT: [&str; 7];   // 逐行见下方
const REVOKE_CONFIRMED_TOKEN: &str = "revoke-confirmed";
pub fn revoke_dialog_args() -> Vec<String>
pub fn revoke_confirmed(code: Option<i32>, stdout: &[u8]) -> bool
pub fn revoke_failure_message(error: &ErrorPayload) -> String
#[derive(Debug, PartialEq)]
pub enum RevokeOutcome { Cancelled, Failed { step: &'static str, error: ErrorPayload }, Exit }
pub async fn run_revoke_steps(
    confirm: impl AsyncFnOnce() -> bool,
    revoke: impl AsyncFnOnce() -> Result<(), ErrorPayload>,
    disable_autostart: impl AsyncFnOnce() -> Result<(), ErrorPayload>,
) -> RevokeOutcome
pub fn request_revoke(app: &AppHandle)
```

`REVOKE_DIALOG_SCRIPT`（逐行，每行一个 `-e`；`argv` 依次为正文、标题、取消标签、确认标签）：

```applescript
on run argv
activate
set r to display dialog (item 1 of argv) with title (item 2 of argv) buttons {item 3 of argv, item 4 of argv} default button (item 3 of argv) cancel button (item 3 of argv) with icon caution giving up after 600
if gave up of r then return "revoke-cancelled"
if button returned of r is (item 4 of argv) then return "revoke-confirmed"
return "revoke-cancelled"
end run
```

| 函数 | 约定 |
| --- | --- |
| `revoke_dialog_args` | 每行脚本前加一个 `"-e"`，之后依次追加 `REVOKE_MESSAGE`、`REVOKE_TITLE`、`REVOKE_CANCEL_LABEL`、`REVOKE_CONFIRM_LABEL`。文案只经参数传入，不拼进脚本 |
| `revoke_confirmed` | 只有 `code == Some(0)` 且 `stdout` 恰好是 `b"revoke-confirmed\n"`（容忍缺少末尾换行）时为 `true`；其余（退出码非 0：点“取消”、回车、Esc、⌘.；`None`：被信号终止；stdout 是 `revoke-cancelled`、空、其他文本、多余空白或换行）都为 `false` |
| `ask_revoke_confirmation`（私有，`async fn() -> bool`） | `spawn_blocking` 里执行 `Command::new(OSASCRIPT_TOOL).args(revoke_dialog_args())`，stdin / stderr 为 `Stdio::null()`，`output()`；启动失败或 `JoinError` → `false`；否则 `revoke_confirmed(status.code(), &stdout)` |
| `revoke_failure_message` | `format!("撤销未完成，CodexHelper 没有退出。请处理后再次选择「撤销配置并退出」，或到主窗口处理。\n\n错误码：{code}\n{message}")`。C6 说明了原因：`.env` 移除失败时状态已置为停用，重新拉起的代理只在本次进程内有效，必须重试撤销 |
| `run_revoke_steps` | 依次 `confirm` → `revoke` → `disable_autostart`。`confirm` 为假 → `Cancelled`，后两步不调用；`revoke` 失败 → `Failed { step: "revoke" }`，不调 `disable_autostart`；`disable_autostart` 失败 → `Failed { step: "autostart" }`；全部成功 → `Exit`。不碰 `AppHandle`，可用假闭包测 |
| `request_revoke` | 见下表 |

`request_revoke` 流程（`tauri::async_runtime::spawn`，日志事件 `desktop.revoke`，只记 `confirmed` / `ok` / `code` / `step`）：

| 步骤 | 内容 |
| --- | --- |
| 1 | `Lifecycle::try_begin_exit()` 返回 `false` 时直接忽略 |
| 2 | `run_revoke_steps` 的三个闭包：`confirm` = `ask_revoke_confirmation()`；`revoke` = `manager.revoke_managed_config().await`，成功时 `publish_status` 推送返回的状态，错误转 `ErrorPayload`；`disable_autostart` = `commands::apply_autostart_checked(false, true, \|on\| apply_system_autostart(app, on))`，成功后 `manager.set_autostart_flag(false)`（错误转 `ErrorPayload`） |
| 3 | `Cancelled`：记 `{ "confirmed": false }`，`end_exit_attempt` |
| 4 | `Failed { step, error }`：先 `publish_status(manager.status())`，再 `publish_operation_failed`；记 `{ "confirmed": true, "ok": false, "code", "step" }`；弹 `kind(Error)` 插件对话框，正文为 `revoke_failure_message`，按钮 `OkCustom("知道了")`（任何结果都只是关闭）；`end_exit_attempt`，不退出。重试是安全的，`revoke_managed_config` 幂等 |
| 5 | `Exit`：记 `{ "confirmed": true, "ok": true }`，`lifecycle::exit_after_shutdown(app)` |

- 产出（`tray`）：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEntry { Show, Managed, Separator, Revoke, Quit }
fn menu_layout(with_revoke: bool) -> Vec<MenuEntry>
fn menu_label(entry: MenuEntry) -> &'static str   // Separator → ""
```

`build` 按 `menu_layout(cfg!(target_os = "macos"))` 依次创建菜单项（`Managed` 是 `CheckMenuItem`，`Separator` 是 `PredefinedMenuItem::separator`）。新增 `const MENU_REVOKE: &str = "revoke";`，`on_menu_event` 增加 `MENU_REVOKE => revoke::request_revoke(app)`。模块文档改为两种平台的菜单说明。

- [ ] **步骤 1：写失败测试**

`revoke.rs::tests`（异步用例用 `tauri::async_runtime::block_on`，闭包用 Rust 2024 的 `async || { … }`，调用记录放 `RefCell<Vec<&str>>`）：

```rust
#[test] fn revoke_confirmed_only_for_confirm_token() {
    assert!(revoke_confirmed(Some(0), b"revoke-confirmed\n"));
    assert!(revoke_confirmed(Some(0), b"revoke-confirmed"));
    let cases: &[(Option<i32>, &[u8])] = &[
        (Some(1), b""),                           // 点“取消”、回车、Esc、⌘.：都命中 cancel button，osascript 报 -128
        (Some(0), b"revoke-cancelled\n"),         // 600 秒超时
        (Some(0), b""), (Some(0), b"revoke-confirmed\n\n"), (Some(0), b" revoke-confirmed\n"),
        (Some(0), b"Revoke-Confirmed\n"), (Some(0), "button returned:撤销并退出\n".as_bytes()),
        (Some(1), b"revoke-confirmed\n"), (Some(2), b"revoke-confirmed\n"), (None, b"revoke-confirmed\n"),
    ];
    for &(code, out) in cases {
        assert!(!revoke_confirmed(code, out), "{code:?} {:?}", String::from_utf8_lossy(out));
    }
}
#[test] fn revoke_dialog_args_bind_return_and_esc_to_cancel() {
    let args = revoke_dialog_args();
    let (script, tail) = args.split_at(args.len() - 4);
    assert_eq!(tail, [REVOKE_MESSAGE, REVOKE_TITLE, "取消", "撤销并退出"]);
    assert!(script.chunks(2).all(|p| p.len() == 2 && p[0] == "-e"));
    let dialog = script.iter().find(|l| l.contains("display dialog")).expect("缺 display dialog");
    for needle in ["buttons {item 3 of argv, item 4 of argv}", "default button (item 3 of argv)",
                   "cancel button (item 3 of argv)", "giving up after"] {
        assert!(dialog.contains(needle), "{needle}");
    }
    // 文案只经 argv 传入
    assert!(!script.iter().any(|l| l.contains("撤销") || l.contains("取消")));
    let confirm_line = script.iter().find(|l| l.contains("revoke-confirmed")).unwrap();
    assert!(confirm_line.contains("button returned of r is (item 4 of argv)"));
}
#[cfg(target_os = "macos")] #[test] fn revoke_dialog_script_compiles() {
    // 只编译不运行，不显示任何界面
    let out = std::env::temp_dir().join(format!("codex-helper-revoke-{}.scpt", std::process::id()));
    let mut args = vec!["-o".to_string(), out.display().to_string()];
    args.extend(REVOKE_DIALOG_SCRIPT.iter().flat_map(|l| ["-e".to_string(), l.to_string()]));
    let status = Command::new("/usr/bin/osacompile").args(&args)
        .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    let _ = std::fs::remove_file(&out);
    assert!(status.success());
}
#[test] fn revoke_steps_cancelled_runs_nothing_else() { /* confirm=false → Cancelled，记录只有 ["confirm"] */ }
#[test] fn revoke_steps_stop_at_revoke_failure() {
    /* revoke 返回 io 错误 → Failed { step: "revoke", error.code == "io" }，记录为 ["confirm", "revoke"] */
}
#[test] fn revoke_steps_report_autostart_failure() {
    /* disable_autostart 返回 internal → Failed { step: "autostart" }，记录为 ["confirm", "revoke", "autostart"] */
}
#[test] fn revoke_steps_exit_after_all_succeed() { /* → Exit，记录为 ["confirm", "revoke", "autostart"] */ }
#[test] fn revoke_dialog_copy_is_fixed() {
    assert_eq!(REVOKE_TITLE, "撤销配置并退出");
    assert_eq!((REVOKE_CANCEL_LABEL, REVOKE_CONFIRM_LABEL), ("取消", "撤销并退出"));
    assert_eq!(REVOKE_MESSAGE, "将还原 Codex 的 config.toml、移除 .env 受管块、关闭开机自启，然后退出。API Key 保留在钥匙串。删除 CodexHelper 前请先做这一步；完成后需要重启 Codex。");
}
#[test] fn revoke_failure_message_shows_code_and_retry_hint() {
    let e = ErrorPayload { code: "io".into(), message: "写入 .env 失败".into() };
    assert_eq!(revoke_failure_message(&e),
        "撤销未完成，CodexHelper 没有退出。请处理后再次选择「撤销配置并退出」，或到主窗口处理。\n\n错误码：io\n写入 .env 失败");
}
```

四个 `revoke_steps_*` 的注释即断言内容，实现时写成真实断言（结果用 `assert_eq!` 比较 `RevokeOutcome`，调用记录逐项比较）。

`lifecycle.rs::tests`：

```rust
#[test] fn exit_attempt_is_exclusive_until_ended() {
    let l = Lifecycle::default();
    assert!(l.try_begin_exit());
    assert!(!l.try_begin_exit());
    l.end_exit_attempt();
    assert!(l.try_begin_exit());
}
```

`tray.rs::tests`：

```rust
use MenuEntry::*;
#[test] fn tray_menu_without_revoke_is_unchanged() {
    assert_eq!(menu_layout(false), [Show, Managed, Separator, Quit]);
}
#[test] fn tray_menu_with_revoke_puts_it_after_separator_before_quit() {
    assert_eq!(menu_layout(true), [Show, Managed, Separator, Revoke, Quit]);
}
#[test] fn tray_menu_labels() {
    let labels: Vec<_> = [Show, Managed, Revoke, Quit].map(menu_label).into();
    assert_eq!(labels, ["显示窗口", "受管模式", "撤销配置并退出", "退出"]);
}
```

- [ ] **步骤 2：运行测试，确认失败**

运行：`cargo test -p codex-helper -- revoke:: lifecycle:: tray::`
期望：编译失败，报 `revoke` 模块、`try_begin_exit`、`menu_layout` 等不存在。实现后 `revoke_dialog_script_compiles` 是唯一调用外部工具的用例，只跑 `osacompile`，不弹界面；任何测试都不得运行 `osascript`。

- [ ] **步骤 3：实现**：按“接口”先拆 `lifecycle`，再写 `revoke.rs`，最后改 `tray.rs`，然后在 `main.rs` 注册模块。`request_revoke` 与 `request_quit` 结构对称，阻塞操作都放在异步任务里。不要为了测试去调 `apply_system_autostart`：它会改 `~/Library/LaunchAgents`。也不要为了“看一眼对话框”手动运行 `osascript` 脚本，实机效果留给第四节的人工验收。`request_quit` 只做机械替换（`swap`/`store` 换成 `try_begin_exit`/`end_exit_attempt`，确认后的两行换成 `exit_after_shutdown`），不改分支结构。

- [ ] **步骤 4：运行测试，确认通过**

运行：`cargo test -p codex-helper`
期望：全部 PASS，含原有的 `quit_confirmation_only_when_enabled_and_codex_running`、`toggle_disables_when_enabled_and_enables_otherwise`。

- [ ] **步骤 5：门禁**（见全局约束）。Windows 交叉 clippy 会编译 `revoke.rs`：在 Windows 上 `MENU_REVOKE` 分支和 `revoke` 函数都没有运行时调用方，但 `menu_layout(cfg!(...))` 与 `on_menu_event` 的匹配分支仍然引用它们，不需要额外的 `allow`。如果仍有 dead code 告警，只用模块顶部的 `cfg_attr` 处理。

- [ ] **步骤 6：提交**

```bash
git add apps/desktop/src-tauri/src/revoke.rs apps/desktop/src-tauri/src/lifecycle.rs \
        apps/desktop/src-tauri/src/tray.rs apps/desktop/src-tauri/src/commands.rs apps/desktop/src-tauri/src/main.rs
git commit -F /tmp/codex-helper-d3-msg.txt   # 标题：feat(desktop): macOS 托盘新增“撤销配置并退出”（osascript 确认框，默认按钮为取消）
```

### 任务 D4：前端同步（新字段、两个新错误码、安装位置横幅、底部提示、隐藏升级区）

**文件：**
- 修改：`apps/desktop/src/types.ts`、`errors.ts`、`format.ts`、`banners.ts`、`main.ts`、`dev-mock.ts`
- 测试：`apps/desktop/src/errors.test.ts`、`apps/desktop/src/format.test.ts`

**接口：**
- 消费：C2 的 `Status` JSON 字段；D2 的 `update` 错误。
- 产出（`types.ts`）：`export type Platform = "windows" | "macos" | "other";`。`Status` 在 `platformSupported` 之后加 `platform: Platform; updateSupported: boolean; installLocationOk: boolean;`，顺序与 Rust 一致。`ErrorCode` 加 `"invalidKeyChars" | "installLocation"`。
- 产出（`errors.ts`）：`export const INVALID_KEY_CHARS_MESSAGE = "Key 只能包含英文字母、数字和符号";`。`INLINE_CODES` 加 `invalidKeyChars`，在 Key 输入框旁就地提示；`installLocation` 走默认的 `"message"`。
- 产出（`format.ts`，纯函数，不从其他模块导入值，否则 `node --test` 解析不带扩展名的 import 会失败）：
  - `export const INSTALL_LOCATION_MESSAGE = "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开";`
  - `export const UNINSTALL_HINT = "卸载前请先在菜单栏选择「撤销配置并退出」";`
  - `export function installLocationBanner(status: Pick<Status, "installLocationOk">): string | null`：`false` 时返回 `INSTALL_LOCATION_MESSAGE`
  - `export function uninstallHint(status: Pick<Status, "platform">): string | null`：只有 `"macos"` 返回 `UNINSTALL_HINT`
  - `export function showUpdatePanel(status: Pick<Status, "updateSupported"> | null): boolean`：`status?.updateSupported === true`。状态未加载时也隐藏，macOS 上不会闪一下升级区

界面接入：

| 位置 | 改动 |
| --- | --- |
| `banners.ts` | `installLocationBanner(status)` 非空时，作为第一条横幅推入 `banner banner-error`（排在开发模式横幅之前）。首次运行时也显示 |
| `main.ts` 底部提示 | 新建 `const platformHint = el("p", { class: "footer-hint platform-hint" })`，在 `appRoot.append(...)` 里放在 `updatePanel.root` 之后。两个标签页、首次运行都可见。`render()` 中 `text = status ? uninstallHint(status) : null`；为 `null` 时 `hidden`。不新增 CSS |
| `main.ts` 升级区 | `render()` 中 `updatePanel.root.hidden = !showUpdatePanel(status)`。`runCheckUpdate` 开头在 `!showUpdatePanel(status)` 时直接返回。启动静默自检只在初始 `getStatus` 成功且 `showUpdatePanel(status)` 时执行，macOS 上完全不调 `check_update` |
| `main.ts` 错误处理 | 不改：`invalidKeyChars` 经 `classifyKeyError` 自动就地提示；`installLocation` 在消息区显示 Rust 给的 `message` |
| `dev-mock.ts` | `baseStatus` 加 `platform: "windows"`、`updateSupported: true`、`installLocationOk: true`。新场景：`?scenario=macos`（`running` 基础上 `platform: "macos"`、`updateSupported: false`）、`?scenario=transient`（`macos` 基础上 `installLocationOk: false`），并更新顶部场景列表注释。`enable` / `save_key`：非空校验之后，若 `trimmed` 不匹配 `/^[\x21-\x7e]+$/`，拒绝 `{ code: "invalidKeyChars", message: INVALID_KEY_CHARS_MESSAGE }`。`!installLocationOk` 时，`enable` 和 `set_autostart`（`enabled` 为真）拒绝 `{ code: "installLocation", message: INSTALL_LOCATION_MESSAGE }`。`!updateSupported` 时，`check_update` / `install_update` 拒绝 D2 的 `update` 错误（同一句文案） |

- [ ] **步骤 1：写失败测试**

`errors.test.ts`：`INLINE` 加 `"invalidKeyChars"`，`MESSAGE_ONLY` 加 `"installLocation"`（现有两个循环会覆盖它们）。新增：

```ts
test("invalidKeyChars 文案与 Rust 侧一致", () => {
  assert.equal(INVALID_KEY_CHARS_MESSAGE, "Key 只能包含英文字母、数字和符号");
});
```

`format.test.ts`：

```ts
test("安装位置不对时显示横幅，文案与 Rust 侧一致", () => {
  assert.equal(installLocationBanner({ installLocationOk: false }), "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开");
  assert.equal(installLocationBanner({ installLocationOk: true }), null);
  assert.equal(INSTALL_LOCATION_MESSAGE, "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开");
});
test("只有 macOS 显示卸载前提示", () => {
  assert.equal(uninstallHint({ platform: "macos" }), "卸载前请先在菜单栏选择「撤销配置并退出」");
  assert.equal(uninstallHint({ platform: "windows" }), null);
  assert.equal(uninstallHint({ platform: "other" }), null);
});
test("只有支持内部升级时显示升级区", () => {
  assert.equal(showUpdatePanel(null), false);
  assert.equal(showUpdatePanel({ updateSupported: false }), false);
  assert.equal(showUpdatePanel({ updateSupported: true }), true);
});
```

- [ ] **步骤 2：运行测试，确认失败**

运行：`npm --prefix apps/desktop test`
期望：FAIL。`errors.test.ts` 报 `INVALID_KEY_CHARS_MESSAGE` 未导出，`format.test.ts` 报 `installLocationBanner` 等未导出（strip-types 下表现为 `SyntaxError: ... does not provide an export named`）。

- [ ] **步骤 3：实现**：先改 `types.ts`，此时 `npm --prefix apps/desktop run check` 会报 `dev-mock.ts` 的 `baseStatus` 缺字段，这是预期的。再按“接口”和“界面接入”改其余文件。

- [ ] **步骤 4：运行测试，确认通过**

运行：`npm --prefix apps/desktop test && npm --prefix apps/desktop run check && npm --prefix apps/desktop run build && ! grep -rq __CODEX_HELPER_DEV_MOCK__ apps/desktop/dist && echo no-mock-in-dist`
期望：测试全部 PASS，类型检查和构建通过，最后输出 `no-mock-in-dist`。界面只做静态核对，不启动 `tauri dev`，也不打开 `.app`。

- [ ] **步骤 5：门禁**（见全局约束）

- [ ] **步骤 6：提交**

```bash
git add apps/desktop/src/types.ts apps/desktop/src/errors.ts apps/desktop/src/format.ts \
        apps/desktop/src/banners.ts apps/desktop/src/main.ts apps/desktop/src/dev-mock.ts \
        apps/desktop/src/errors.test.ts apps/desktop/src/format.test.ts
git commit -F /tmp/codex-helper-d4-msg.txt   # 标题：feat(desktop): 前端支持 macOS（安装位置横幅、卸载提示、隐藏升级区、新错误码）
```

desktop 通道完成：`pi/macos-desktop` 在 core 的 6 个提交之上有 D1–D4 四个提交，门禁全绿后通知主会话合并。

# 四、合并与收尾（主会话）

前提：三个通道都已完成，每个 worktree 的门禁全绿，`git status --short` 为空。

- [ ] **步骤 1：核对分支内容与预期冲突**

```bash
cd /Users/sxk/Documents/code/ai-space/CodexHelper
git log --oneline pi/macos-mvp..pi/macos-core      # 6 个：C1–C6
git log --oneline pi/macos-core..pi/macos-desktop  # 4 个：D1–D4
git log --oneline pi/macos-mvp..pi/macos-pack      # 3 个：P1–P3
git merge-base --is-ancestor pi/macos-core pi/macos-desktop && echo desktop-contains-core
comm -12 <(git diff --name-only pi/macos-mvp...pi/macos-desktop | sort) \
         <(git diff --name-only pi/macos-mvp...pi/macos-pack | sort)
```

期望：提交数分别为 6 / 4 / 3；输出 `desktop-contains-core`；`comm` 无输出，即两条线没有共同修改的文件。`comm` 有输出时先别合并，把重叠文件报给用户。

- [ ] **步骤 2：在主目录合并**（当前分支 `pi/macos-mvp`，本计划文件已在该分支提交）

```bash
git switch pi/macos-mvp
git merge --no-ff pi/macos-desktop -F /tmp/codex-helper-merge-desktop.txt   # 标题：合并 macOS core 与 desktop 通道
git merge --no-ff pi/macos-pack -F /tmp/codex-helper-merge-pack.txt         # 标题：合并 macOS pack 通道
```

两份提交信息都用中文，末行写 `Co-Authored-By: Pi`。出现冲突时运行 `git merge --abort`，不要猜测着解决，把冲突文件报给用户。

- [ ] **步骤 3：最终门禁**（在主目录执行全局约束里的整组命令；`apps/desktop/node_modules` 不存在时先 `npm --prefix apps/desktop ci`）

期望：全部退出码 0。core 与 pack 合并后，这里第一次同时验证 0.2.0 版本号与 desktop 改动。

- [ ] **步骤 4：集成打包核验**

运行 `scripts/build-macos.sh`，然后执行 P2 步骤 2 的核验命令。期望与 P2 相同：universal、ad-hoc 签名、版本 `0.2.0`、`LSMinimumSystemVersion` 为 `12.0`、没有升级产物。dmg 只读挂载查看，不打开任何 `.app`。最后 `git status --short` 为空。

- [ ] **步骤 5：收尾，不做的事**

- 不 `git push`，不打 tag，不删除 worktree 和分支（`git worktree remove` / `git branch -d` 由用户决定）。
- 设计 §9.2 实机验收由人工在发布前完成。本计划补充以下验收项：
  - 第 5 条（撤销确认框）：“取消”是高亮的默认按钮；按回车、按 Esc、按 ⌘.、点“取消”、放置 10 分钟不操作，都不执行撤销，CodexHelper 不退出，菜单可以再次使用；只有点“撤销并退出”才执行。对话框从托盘弹出时位于最前、能直接用键盘操作。
  - 第 5 条（撤销失败）：让 `.env` 不可写（`chflags uchg`，事后还原）后撤销，弹出带错误码与“再次选择”提示的错误框，应用不退出、代理仍在监听；还原后再撤销成功并退出。
  - 第 7 条：从 dmg 里直接运行时，勾选开机自启会被拒绝，`~/Library/LaunchAgents` 里不出现本工具的条目。
  - Windows 回归（D3 重构了 `request_quit`）：托盘“退出”在受管模式开启且 Codex 运行时弹确认，取消后不退出且可再次点击，确认后退出；受管模式关闭时直接退出。托盘菜单没有“撤销配置并退出”。
- 向用户汇报：13 个任务提交加 2 个合并提交、门禁结果、dmg 路径，以及没有验证的项（GUI 实机、osascript 对话框的焦点与按键行为、CI `macos-latest` 上真实 Keychain 测试）。
- 后续动作（不在本计划内执行）：首次 push 后，如果 CI `macos-latest` 上 `keychain_*` / `get_*` 测试因没有可用的默认钥匙串而失败，由 core 通道在 `pi/macos-core` 的后继分支按 C1 “CI 回退”条款补一个提交（加 `#[ignore = …]`，并在本机跑一次 `-- --ignored`）。
- 需用户确认的偏离：D3 撤销确认框从 tauri-plugin-dialog 改为 `/usr/bin/osascript display dialog`（原因见第三节开头）。设计 §6 的文案、按钮与“默认聚焦取消”不变。用户不同意时，D3 暂停，改用主窗口内的模态框（需新增命令与 capability），另行修订计划。

## 计划自检记录

按 writing-plans 的 Self-Review 五项逐条记录（2026-10-14）。

1. **规格覆盖**：§3.1 → C1；§3.2 → C3，界面就地提示在 D4；§3.3 → C1（含 cli.rs 的排查）；§4.1 → C2，升级命令拒绝在 D2，隐藏升级区在 D4；§4.2 → C4、C5，开机自启预检在 D2，横幅在 D4；§5 → D1；§6 → C6，确认框、关闭自启与退出在 D3；§7.1 → P2；§7.2 → P1、P3；§7.3 → P3（Release 正文），主窗口底部提示在 D4；§8 不改代码，D3 复用 `MacosLauncher::LaunchAgent`；§9.1 → 各任务测试与门禁；§9.2 → 第四节，人工验收；§10 接受风险，不写任务。没有缺口。
2. **步骤扫描**：每个代码步骤都写了文件、签名和固定值；行为由表格给出，没有誊抄函数体。没有自动化测试的胶水代码有四处：`request_revoke` 对 `RevokeOutcome` 的处理、`request_quit` 的机械替换、托盘 `build`、`main.ts` 的 DOM 接入。撤销的确认判定（`revoke_confirmed`）、步骤顺序（`run_revoke_steps`）、开机自启的“预检失败不调插件”（`apply_autostart_checked`）都已抽成可测函数；剩下的依赖 `AppHandle` 或 DOM，由流程表约束，并在第四节补了验收项（含 Windows 托盘退出回归）。没有“TBD”或“适当处理”之类的句子。
3. **类型一致性**：`revoke_managed_config`、`set_autostart_flag`、`InstallLocation` / `"installLocation"`、`InvalidKeyChars` / `"invalidKeyChars"`，在 C2、C5、C6 与 D2、D3、D4 中名字一致；`Status` 三个字段 Rust 侧为 `platform` / `update_supported` / `install_location_ok`，TS 侧为 `platform` / `updateSupported` / `installLocationOk`，与契约一致；`CODEX_PROCESS_NAME` 只在 D1 修改；`try_begin_exit`、`end_exit_attempt`、`exit_after_shutdown`、`menu_layout` 只在 D3 定义和使用；D4 的两句错误文案与 C2 表逐字一致，D4 的 `update` 文案与 D2 逐字一致。
4. **Review Focus**：第 5 条的 `revoke_confirmed_only_for_confirm_token`、`revoke_dialog_args_bind_return_and_esc_to_cancel`、`revoke_dialog_script_compiles`、`revoke_steps_*` 在 D3 步骤 1。评审指出原方案（插件 `OkCancelCustom` + `show_with_result`）会把取消类结果映射成第二个按钮，已改为 osascript，见第三节开头。第 1–4 条的测试在 C1、C5、C6，名字与 Review Focus 一致。desktop 侧另外补了两项：从临时位置打开开机自启会留下 LaunchAgent（D2 `autostart_on_rejected_from_transient_location`、`checked_autostart_skips_plugin_when_precheck_fails`）；状态未加载时 macOS 上会闪现升级区（D4 `showUpdatePanel(null)`）。没有找到其他未覆盖的高风险输入。
5. **篇幅**：设计 243 行，计划约 1310 行，大约 5.4 倍（评审后为 D3 换确认框机制、补可测函数与验收项，增加约 160 行）。多出来的主要是逐字断言、核验命令与期望输出，这些设计里没有，执行者也无法自行决定；代码块里没有实现体。desktop 一节约 450 行，对应 4 个任务、约 12 个源文件；其中约 100 行是确认框机制的依据与可测函数，执行者没有这些信息无法安全实现，没有再压缩。
