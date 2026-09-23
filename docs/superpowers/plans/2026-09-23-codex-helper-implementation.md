# CodexHelper 实施计划

状态：已评审（2026-09-23 对话确认），执行中
设计：`docs/superpowers/specs/2026-09-23-codex-helper-design.md`
移植源：CodexPlusPlus `claude/windows-managed-gateway-pac` 分支 `228bc0d`（只读参考，不修改）

## 1. 分支与合并

- 实施分支：`claude/codex-helper-impl`（基于 `claude/codex-helper-design`）。
- 并行任务各自在独立 git worktree 中工作，分支 `claude/wt-<任务>`；同一任务的审查、修复在该 worktree 内串行进行。
- 每阶段结束由主会话合并全部任务分支、跑门禁、汇报，之后删除临时分支与 worktree。
- 提交信息中文，格式 `feat(core): ...` / `test(...)` / `ci: ...`，结尾 `Co-Authored-By: Claude Code`。不 push。

## 2. 契约文件（阶段 0 定稿，任务不得修改）

以下文件构成跨任务接口，任务 agent 不得修改；确需调整在输出中申报，由主会话合并时统一处理：

- 根 `Cargo.toml`、`crates/helper-core/Cargo.toml`、`apps/credential/Cargo.toml`（依赖已预先声明）
- `crates/helper-core/src/lib.rs`、`consts.rs`、`types.rs`、`paths.rs`、`fsutil.rs`、`rules.rs` 的公共 API
- 各模块桩文件中的**公共函数签名与类型**（函数体、私有辅助、测试可自由编写）
- `apps/desktop/src/types.ts`、`apps/desktop/src/api.ts`、`apps/desktop/src-tauri/src/commands.rs` 的命令名与参数

契约要点：

| 模块 | 公共接口 |
| --- | --- |
| `consts` | 设计 §1.1 全部固定参数 |
| `paths` | `HelperPaths { codex_home, data_dir }` 及派生路径；`credential_command_path()` |
| `fsutil` | `atomic_write`、`read_optional`、`remove_file_if_exists` |
| `types` | `Status`、`EnableRequest`、`SaveKeyRequest`、`ConnectionRecord`、`KeyCheck`、`Reachability`、`ManagerError`（`code()` / `payload()`）、`ErrorPayload`、`ReconcileReport`、`CleanupReport` |
| `rules` | `Route`、`normalize_host`、`route_for_host`、三组规则常量 |
| `codex_config` | `inspect`、`apply_managed`（返回 `ApplyOutcome` 供回滚）、`rollback`、`restore`、`remove_residue`、`PreviousConfig`、`ConfigError` |
| `codex_env` | 标记常量（含 Codex++ 旧标记）、`render_block` / `upsert_block` / `remove_block` / `remove_legacy_block` / `inspect_text`、文件级 `inspect_file` / `write_block_to_file` / `remove_block_from_file` / `remove_legacy_block_from_file` |
| `credential` | `Secret`（Debug 脱敏）、`CredentialStore` trait、`MemoryCredentialStore`、`SystemCredentialStore` |
| `state` | `HelperState`、`load`、`save` |
| `log` | `init`、`event`、`redact_value`、`redact_text` |
| `gateway` | `verify_key` / `verify_key_at`、`tcp_reachable` |
| `proxy` | 请求行与 SOCKS5 报文纯函数、`connect_via_socks5_at`、`ProxyConfig`、`RecordSink`、`RecentConnections`、`ProxyHandle::shutdown`、`spawn` / `spawn_with_listener`、`probe_existing`、`proxy_port` |
| `manager` | `ManagerOptions`（可注入路径、凭据、端口、上游、网关）、`Manager` 的 enable / save_key / disable / clear_key / reconcile_on_startup / cleanup_residue / remove_legacy_env_block / set_autostart_flag / probe_reachability / recent_connections / shutdown / status；自由函数 `cleanup` |

Tauri 命令：`get_status`、`refresh_status`、`enable`、`disable`、`save_key`、`clear_key`、`cleanup_residue`、
`remove_legacy_env_block`、`set_autostart`、`recent_connections`；事件：`status-changed`、`connection-recorded`、`key-required`。

## 3. 阶段与任务

模型：未注明者为 Opus 5.5。审查一律 Opus 5.5 · xhigh，只读，对照设计章节、移植源与原测试；
有阻断问题才修复，最多两轮，仍未解决上报主会话。

### 阶段 0：脚手架与契约（主会话）

workspace、依赖预声明、契约文件、全部模块桩、`apps/credential` 与 `apps/desktop` 骨架（Vite + 原生 TS）、
占位图标、门禁脚本 `scripts/gate.sh` 与 `scripts/check-windows.sh`、本计划文档。

调整说明：`rules.rs` 在阶段 0 直接完成移植（代理的集成测试在运行时依赖 `route_for_host`，
不能以桩的形式并行），任务 1.1 改为规则测试移植与快照核对。

### 阶段 1：核心模块并行实现

| # | 任务 | 实现者 | 要点 |
| --- | --- | --- | --- |
| 1.1 | `rules.rs` 测试与快照核对 | Sonnet 5 · medium | 移植原规则与分流测试；核对 7 精确 + 24 后缀 + 1 关键字 |
| 1.2 | `codex_env.rs` | Sonnet 5 · high | 新标记；Codex++ 旧块检测与移除；空白删文件；原子写 |
| 1.3 | `codex_config.rs` | Opus 5.5 · high | 记录原值 / 还原；拒写损坏配置与非 table；逐字节保留 |
| 1.4 | `credential.rs` | Sonnet 5 · high | Windows Credential Manager + 非 Windows 进程内内存实现 |
| 1.5 | `state.rs` + `log.rs` | Sonnet 5 · medium | 状态文件原子写；脱敏 JSONL 日志与压缩 |
| 1.6 | `gateway.rs` | Sonnet 5 · high | wiremock：200 / 401 / 403 / 5xx / 超时；TCP 可达性 |
| 1.7 | `proxy.rs` + `tests/proxy.rs` | Opus 5.5 · xhigh | 自识端点、直连 10 秒超时、连接记录、32 KiB、502 不回退直连 |
| 1.8 | CI、发布、NSIS | Sonnet 5 · high | `ci.yml`、`release.yml`、externalBin、`NSIS_HOOK_PREUNINSTALL` |

externalBin 放在独立的 `tauri.bundle.conf.json`，只在发布时通过 `tauri build --config` 合并，
避免 `cargo build/clippy/test --workspace` 因缺少 sidecar 二进制而失败。

### 阶段 2：编排层与凭据程序（2.2 在 2.1 完成后开始）

| # | 任务 | 实现者 | 要点 |
| --- | --- | --- | --- |
| 2.1 | `manager.rs` | Opus 5.5 · xhigh | 启用 / 停用 / 对账 / 清理与逐步回滚；临时 `CODEX_HOME` 编排测试（设计 §11） |
| 2.2 | `apps/credential` | Sonnet 5 · high | `get` 固定 target 与 stdout / stderr 约定；`cleanup [--purge-key]` |

### 阶段 3：桌面应用

| # | 任务 | 实现者 | 要点 |
| --- | --- | --- | --- |
| 3.1 | `src-tauri` | Opus 5.5 · high | 命令层、托盘、单实例、自启 `--minimized`、关闭隐藏、退出前检测 `codex.exe`、30 秒重测、事件推送 |
| 3.2 | 前端 `src/` | Sonnet 5 · high | 设计 §8 单页中文界面、首次仅 Key 录入、catalog 确认、“仍然保存”、旧块警告、明文即时清空 |

### 阶段 4：全量对抗审查

7 个维度（Opus 5.5 · xhigh）：设计符合性、移植保真度、安全与隐私、失败与回滚、代理协议、Windows 平台、测试完备性。
每个发现 3 票对抗验证（Opus 5.5 · high，默认驳回，≥2 票成立），按文件分组修复，循环至连续两轮无新发现（最多 3 轮），
最后完整性评审。

### 阶段 5：收尾（主会话）

最终门禁、提交、输出设计 §13 Windows 实机验收清单。

## 4. 门禁

每阶段合并后执行 `scripts/gate.sh`：

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-windows.sh        # macOS 上对 x86_64-pc-windows-msvc 做 cargo check
npm --prefix apps/desktop run check
```

阶段 3 起追加桌面程序构建。

## 5. 本机无法验证、需 CI 或 Windows 实机

- Credential Manager 真实读写（CI `windows-latest` 测试）；
- NSIS 打包、安装目录、卸载 hook、开机自启注册表；
- 设计 §13 全部实机验收项。
