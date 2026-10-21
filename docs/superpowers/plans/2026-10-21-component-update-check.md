# 组件更新检查（ChatGPT Desktop / Codex CLI）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 手动一键检查 ChatGPT Desktop（Windows）与 Codex CLI（Windows/macOS）的本地版本与远端最新版本，按 Manager 状态选择经代理 / 直连 / 零出站，有更新时给出官方下载入口。

**Architecture:** helper-core 新模块 `component_updates.rs` 承载数据结构、解析与比较纯函数、网络路径决策纯函数和 `check_all` 编排（本地探测与远端请求可注入）；Tauri 命令 `check_component_updates` 只取 `Manager::status()` 快照 + 调用；`connection_gate.rs` 对本进程 PID 放行使 Secure 模式下检测可经代理；前端新增 `componentpanel.ts` + 纯函数 `componentformat.ts`。三条通道并行（core / gate / frontend），最后一个集成任务合并。

**Tech Stack:** Rust 2024（reqwest 0.12 + native-tls、serde 1.0.229、tokio、wiremock 0.6）、Tauri 2、TypeScript（`node --test --experimental-strip-types`）。

**Spec:** `docs/superpowers/specs/2026-10-20-component-update-check-design.md`（本计划的依据，实现者必须同时完整阅读）

## Global Constraints

- 代码注释、界面文案、提交信息一律中文；提交信息风格 `feat(scope): …`，正文后空一行写尾注 `Co-Authored-By: Pi`；中文提交信息先写入 `/tmp/cuc-<任务号>.txt` 再 `git commit -F` 传入。
- 禁止 push、建 MR、`git reset --hard` / `clean -f` / `branch -D` / force 类操作；每条通道只在自己的 worktree 里改自己名下的文件。
- 除 workspace `reqwest` 追加 `features = ["native-tls"]`（**不加 `json`**）外不新增任何 Rust crate / npm 包；不新增 semver/regex crate（C5、C6）。
- 响应体解析一律读字节 + `serde_json::from_slice`；所有 client：`.timeout(15s)`、`.redirect(Policy::none())`；直连 client 必须 `.no_proxy()`；固定 `User-Agent: codex-helper/<CARGO_PKG_VERSION>`，不带任何凭据头。
- ChatGPT 比较只用 `packageVersion`（四段）对 `Get-AppxPackage` 四段 Version；`releaseVersion` 绝不参与比较；**不请求** `windows-store-update.json`。
- 白名单前缀：`https://persistent.oaistatic.com/codex-app-prod/`；CLI tag 正则语义 `^rust-v\d+\.\d+\.\d+$`（手写判定）。
- `Get-AppxPackage` 子进程超时 = `consts::DEVENV_PROBE_TIMEOUT_SECS`（15s，C13）；沿用 devenv 子进程防护（CREATE_NO_WINDOW、Job Object、kill_on_drop）。
- Secure 模式（`status.mode == Secure`）下非 Running 绝不发远端请求，绝不直连（C4）。
- 门禁：本进程 PID 放行只返回 `Decision::Allow { role: "codex-helper-self" }`，解析失败照旧 fail-closed（§7）。
- 门禁检查：`scripts/gate.sh`（fmt / clippy `-D warnings` / `cargo test --workspace` / check-windows / tsc / 前端 test / 前端 build）必须全绿。
- 前端测试只能是纯函数（无 DOM shim），断言只用 `node-test.d.ts` 已声明的 `ok/equal/deepEqual/match`。
- TDD：先写失败测试并实际跑出红，再写最小实现跑绿；写完即绿的测试必须在任务报告中列出并说明它防住了什么。

## Review Focus

1. **白名单绕过形态**：`downloadUrl` 为 `https://persistent.oaistatic.com@evil.com/codex-app-prod/x.msix`、`…oaistatic.com.evil.com…`、`:8443` 端口、`/codex-app-prod/../other/`、`%2e%2e`、`/codex-app-prodx/`、`http://` 时一律判 `invalid`，绝不展示假版本（Task C2 `allowed_download_url_rejects_bypass_forms`）。
2. **Secure 下代理处于 External / SecureBlocked（port_occupied）**：同样零出站、远端为 `blocked`，文案区分“代理未运行 / 端口被外部进程占用”（Task C3 `secure_external_or_port_occupied_is_blocked` + Task C5 `secure_blocked_sends_zero_requests`）。
3. **PowerShell 输出带 BOM / CRLF / 空行 / 非版本行**：BOM 与空白剥离后仍能取到版本；全空白判 `notRegisteredForUser`；只有非版本行判 `probeFailed`（Task C2 `appx_output_*`）。
4. **超大或畸形响应体**（HTML 登录页、无限流）：超过 256 KiB 截断判 `invalid`，非 JSON 判 `invalid`，不挂起、不 OOM（Task C5 `oversized_body_is_invalid`、`desktop_feed_html_is_invalid`）。
5. **检测本身拉起子进程时 socket 句柄被继承导致门禁瞬时多候选**：`check_all` 先跑完全部本地探测再发远端请求（Task C5 `local_probes_finish_before_any_remote_request`）；门禁侧以真实 socket 验证同进程放行与他进程仍 403（Task G2）。

---

## 跨通道接口契约（三条通道唯一共同依据）

### Rust（`crates/helper-core/src/component_updates.rs`，core 通道产出）

所有枚举：`#[derive(Debug, Clone, PartialEq, Eq, Serialize)]` + `#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]`（`Comparison` 除外，见下）。

```rust
pub enum LocalState {
    Installed { version: String },
    NotInstalled,              // 仅 CLI：PATH 无 codex
    NotRegisteredForUser,      // 仅 ChatGPT：Get-AppxPackage 空输出
    ProbeFailed { detail: String },
    UnsupportedPlatform,       // 实施补充（见“实施偏差”D2）：非 Windows 的 ChatGPT 行
}
pub enum RemoteState {
    Ok { version: String, source: RemoteSource,
         #[serde(skip_serializing_if = "Option::is_none")] fallback_from: Option<String> },
    Unreachable { detail: String },
    Invalid { detail: String },
    Blocked { detail: String },
    UnsupportedPlatform,
}
pub enum RemoteSource { DesktopReleaseFeed, GithubApi, GithubRedirect, CodexLocalCache { checked_at: String } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Comparison { UpdateAvailable, UpToDate, LocalNewer, Unknown }

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdate {
    pub local: LocalState, pub remote: RemoteState, pub comparison: Comparison,
    pub download_url: Option<String>, pub via_proxy: bool, pub direct_reason: Option<&'static str>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdates { pub chatgpt_desktop: ComponentUpdate, pub codex_cli: ComponentUpdate }
```

### JSON 形状（设计 §4.1 示例原样，双方测试都用它做夹具）

```json
{
  "chatgptDesktop": {
    "local": { "kind": "installed", "version": "26.1013.4894.0" },
    "remote": { "kind": "ok", "version": "26.1013.4894.0", "source": { "kind": "desktopReleaseFeed" } },
    "comparison": "upToDate", "downloadUrl": null, "viaProxy": true, "directReason": null
  },
  "codexCli": {
    "local": { "kind": "installed", "version": "0.162.1" },
    "remote": { "kind": "ok", "version": "0.163.0",
                "source": { "kind": "codexLocalCache", "checkedAt": "2026-10-16T05:53:49Z" },
                "fallbackFrom": "github api 403 限流" },
    "comparison": "updateAvailable",
    "downloadUrl": "https://github.com/openai/codex/releases/tag/rust-v0.163.0",
    "viaProxy": false, "directReason": "代理为外部实例，模式未知，已直连"
  }
}
```

单元变体形状：`{"kind":"notInstalled"}`、`{"kind":"notRegisteredForUser"}`、`{"kind":"unsupportedPlatform"}`、`{"kind":"githubApi"}`；`Comparison` 为纯字符串。

### TypeScript（`apps/desktop/src/types.ts`，frontend 通道产出）

```ts
export type LocalState =
  | { kind: "installed"; version: string }
  | { kind: "notInstalled" }
  | { kind: "notRegisteredForUser" }
  | { kind: "probeFailed"; detail: string }
  | { kind: "unsupportedPlatform" };
export type RemoteSource =
  | { kind: "desktopReleaseFeed" } | { kind: "githubApi" } | { kind: "githubRedirect" }
  | { kind: "codexLocalCache"; checkedAt: string };
export type RemoteState =
  | { kind: "ok"; version: string; source: RemoteSource; fallbackFrom?: string }
  | { kind: "unreachable"; detail: string }
  | { kind: "invalid"; detail: string }
  | { kind: "blocked"; detail: string }
  | { kind: "unsupportedPlatform" };
export type Comparison = "updateAvailable" | "upToDate" | "localNewer" | "unknown";
export interface ComponentUpdate {
  local: LocalState; remote: RemoteState; comparison: Comparison;
  downloadUrl: string | null; viaProxy: boolean; directReason: string | null;
}
export interface ComponentUpdates { chatgptDesktop: ComponentUpdate; codexCli: ComponentUpdate }
```

### 命令

- Tauri 命令名：`check_component_updates`，无参数；签名 `pub async fn check_component_updates(state: tauri::State<'_, AppState>) -> Result<ComponentUpdates, ErrorPayload>`，**恒返回 `Ok`**（实施偏差 D1）。
- `api.ts`：`checkComponentUpdates: () => invoke<ComponentUpdates>("check_component_updates")`。

### Rust 侧固定文案（前端只展示 `detail` / `directReason`，不自己拼这些）

| 场景 | 字段 | 文案 |
| --- | --- | --- |
| Secure + 非 Running（一般） | `remote.detail` | `Secure 代理未就绪（代理未运行）` |
| Secure + External 或 `secure.blockedReason == "port_occupied"` | `remote.detail` | `Secure 代理未就绪（端口被外部进程占用）` |
| Legacy + External | `directReason` | `代理为外部实例，模式未知，已直连` |
| Legacy + Stopped | `directReason` | `本地代理未运行，已直连` |
| 非 Windows ChatGPT | `local` / `remote` | 均 `unsupportedPlatform` |

## 通道与文件归属

| 通道 | worktree / 分支 | 只许改的文件 |
| --- | --- | --- |
| core | `.worktrees/component-update-check` / `pi/component-update-check` | `Cargo.toml`（workspace）、`Cargo.lock`、`crates/helper-core/src/{component_updates.rs,lib.rs,devenv.rs,consts.rs}`、`crates/helper-core/tests/component_updates.rs`、`apps/desktop/src-tauri/src/{commands.rs,main.rs}` |
| gate | `.worktrees/cuc-gate` / `pi/cuc-gate` | `crates/helper-core/src/connection_gate.rs`、`crates/helper-core/src/proxy.rs`（仅门禁 Allow 分支与日志辅助函数）、`crates/helper-core/tests/gate_self_allow.rs`（新增） |
| frontend | `.worktrees/cuc-frontend` / `pi/cuc-frontend` | `apps/desktop/src/{types.ts,api.ts,dev-mock.ts,componentpanel.ts,componentformat.ts,componentformat.test.ts,main.ts,styles.css}`（`index.html` 不需要改：挂载点 `#app` 已够用） |
| 集成 | core worktree | 仅合并提交；发现的接缝问题回到所属通道文件修 |

三条通道都基于 `666cdba`，互不依赖；frontend 只依赖上面的契约。gate 与 frontend 的 worktree 里没有本计划文件，执行者按绝对路径
`/Users/sxk/Documents/code/ai-space/CodexHelper/.worktrees/component-update-check/docs/superpowers/plans/2026-10-21-component-update-check.md` 阅读，不要把计划提交合入这两个分支。

## 已核实的代码事实（写接口用）

- `Manager::status(&self) -> Status`（async，短锁快照，无网络）；`Status.mode: state::ProxyMode { Legacy, Secure }`（serde 原名 `"Legacy"`/`"Secure"`）、`Status.proxy: ProxyStatus { state: ProxyState { Stopped, Running, External }, port: u16 }`，其中 `port` 即 `ManagerOptions.proxy_port`（`build_status` 直接赋值）；`Status.secure.blocked_reason: Option<String>`（取值含 `port_occupied`、`port_unavailable`、`rebuild_failed`）；`Status.codex_home: String`（`paths.codex_home.display()`）。`Manager` 不暴露 `options`/`paths` 访问器。
- `connection_gate.rs`：`ConnectionGate { resolver, policy, cache }`、`new(resolver, policy)`、`resolve_with_retry(local, remote) -> anyhow::Result<PeerInfo>`（2 次、间隔 150ms）、`admit(&PeerInfo) -> Decision`（先 `hash_image_file` 再查缓存/`decide`）；`Decision { Allow { role: String }, Deny { reason: &'static str } }` 在 `process_policy.rs`；`PeerInfo.pid: i32`。
- `proxy.rs:901-934`：`spawn_blocking(move || gate.resolve_with_retry(local, remote).map(|peer| gate.admit(&peer)))`，套 `GATE_TIMEOUT`；`Some(Decision::Allow { .. }) => {}`。**现有代码没有 `proxy.gate_allowed` 事件**，只有 `proxy.gate_denied`。
- 解析器：`is_peer_socket` 只认方向相反的 socket（代理自己 accept 的那端被排除），`unique_owner` 对同一 PID 去重——同进程两端**不会**判为多候选；`tests/peer_identity_macos.rs::resolves_self_connection_ipv4` 已实测自连解析出本进程 PID。残余风险只有“建连瞬间同进程 spawn 子进程继承 fd”（模块头已记录），见 Review Focus 5。
- `devenv.rs`：`SearchEnv { path, exts }`、`SearchEnv::system()`（pub）、`fn find(&self, name)`（私有）、`fn pick_login_shell(Option<&OsStr>) -> PathBuf`（私有）、`pub async fn with_login_shell_path(SearchEnv, &Path, Duration) -> SearchEnv`（`cfg(unix)`）、`async fn run(&Path, &[&str], &SearchEnv, Duration) -> Result<CommandOutput, String>`（私有；含 CREATE_NO_WINDOW、Job Object、kill_on_drop、超时），`struct CommandOutput { code, stdout, stderr }`（私有）、`fn exit_text(Option<i32>)`。
- `consts::DEVENV_PROBE_TIMEOUT_SECS = 15`、`LOGIN_SHELL_TIMEOUT_SECS = 5`。
- `gateway.rs` client 惯例：`reqwest::Client::builder().no_proxy().redirect(Policy::none()).timeout(t).build()`。
- `commands.rs`：`AppState { manager: Arc<Manager>, handle, reconcile_report }`；`detect_dev_env()` 无 State 参数故可直接返回值。带 `State<'_>` 的 async 命令**必须**返回 `Result`（Tauri 限制）。`main.rs:66-82` 的 `generate_handler!` 列表。
- 前端：`dom.ts::el()`/`clear()`；`format.ts::StatusLevel = "ok" | "warn" | "bad"`；`main.ts` 用 `OpKind` + `busy: Set` 管理进行中状态，`appRoot.append(title, tabs.root, messageRegion.root, gatewayPage, devEnvPage, updatePanel.root, platformHint)`；`devenvpanel.ts` 用 `rendered` 比较避免 30s 重渲染清掉选区；`dev-mock.ts` 在 `mockIPC` 的 `switch (cmd)` 里按 URL 参数返回场景。
- Cargo.lock 已有 `native-tls 0.2.18`、`hyper-tls 0.6.0`、`tokio-native-tls 0.3.1`（来自 reqwest 0.13.5），`reqwest 0.12.28` 当前无 TLS。
- CI 矩阵为 windows-latest + macos-latest（无 Linux，native-tls 不会拉 openssl-sys 构建问题）。

## 设计 §9 留给实施计划的事项（拍板）

- `componentpanel.ts` DOM：根 `div.component-region`（沿用 `.update-region` 的上边框与间距）→ 标题行 `div.component-header`（`h2.section-title` “组件版本” + 按钮 `btn btn-sm` “检查组件更新”）→ `p.component-network`（网络路径说明）→ 两个 `div.component-row`（`span.component-label`、`span.component-versions`“本地 X · 最新 Y”、`span.component-badge.component-badge-<level>`、可选 `p.component-note`、可选 `div.component-download` 内 `p.component-download-hint` + `pre.component-url`（`user-select: all`，不做链接、不自动打开））→ `p.footer-hint` 固定说明“ChatGPT Desktop 自带的 codex.exe 随应用更新，不在 Codex CLI 行统计。”。
- wiremock 夹具：直接用设计 §2.1 实测的 `windows-release.json`（含 `releaseDate`、x64/arm64 两条 `packages`）并额外加 `"releaseVersion":"26.1013.40934"` 与未知字段 `"futureField":{"a":1}`；GitHub 夹具 `{"tag_name":"rust-v0.163.0","name":"0.163.0","prerelease":false,"assets":[]}`；`version.json` 夹具用 §4.3 实测内容。
- GitHub API 解析：`#[derive(Deserialize)] struct GithubRelease { tag_name: String }`（serde 默认忽略未知字段），只取 `tag_name`，再过同一个 tag 判定（实施偏差 D7）。
- 302 回退复用同一个 client 实例（同为 `redirect(Policy::none())`）。

---

## 通道 core（worktree `.worktrees/component-update-check`）

### Task C1: reqwest 启用 native-tls + devenv 可见性提升 + 常量

**Files:**
- Modify: `Cargo.toml`（workspace `reqwest = { version = "0.12", default-features = false, features = ["native-tls"] }`）、`Cargo.lock`（由 cargo 自动更新）
- Modify: `crates/helper-core/src/devenv.rs`（可见性）
- Modify: `crates/helper-core/src/consts.rs`（追加常量）
- Create: `crates/helper-core/src/component_updates.rs`（仅模块头注释 + 本任务的测试），`crates/helper-core/src/lib.rs` 注册 `pub mod component_updates;`

**Interfaces:**
- Produces（devenv.rs，只改可见性不改行为）：`impl SearchEnv { pub(crate) fn find(&self, name: &str) -> Option<PathBuf> }`、`pub(crate) fn pick_login_shell(shell: Option<&OsStr>) -> PathBuf`、`pub(crate) async fn run(program: &Path, args: &[&str], env: &SearchEnv, timeout: Duration) -> Result<CommandOutput, String>`、`pub(crate) struct CommandOutput { pub(crate) code: Option<i32>, pub(crate) stdout: String, pub(crate) stderr: String }`（`success()` 同提为 `pub(crate)`）、`pub(crate) fn exit_text(code: Option<i32>) -> String`。`pick_login_shell` 上的 `cfg_attr(not(target_os = "macos"), allow(dead_code))` 保留。
- Produces（consts.rs）：
  - `CHATGPT_RELEASE_FEED_URL: &str = "https://persistent.oaistatic.com/codex-app-prod/windows-release.json"`
  - `CHATGPT_DOWNLOAD_URL_PREFIX: &str = "https://persistent.oaistatic.com/codex-app-prod/"`
  - `CODEX_RELEASES_API_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest"`
  - `CODEX_RELEASES_LATEST_URL: &str = "https://github.com/openai/codex/releases/latest"`
  - `CODEX_RELEASE_TAG_URL_PREFIX: &str = "https://github.com/openai/codex/releases/tag/"`
  - `COMPONENT_CHECK_TIMEOUT_SECS: u64 = 15`、`COMPONENT_CHECK_MAX_BODY_BYTES: usize = 256 * 1024`
- `component_updates.rs` 模块头注释（交付物，原文要点必须出现）：设计文档路径；“门禁的本进程放行对进程内**任何**经本地代理端口（默认 17891）的客户端生效，门禁本身不校验目标；约定进程内只有组件检测把本地代理当代理使用，新增进程内代理客户端须评审”；检测 URL 全部来自 `consts` 编译期常量；检测全程无凭据。

- [x] **Step 1: 写失败测试**（`component_updates.rs` 内 `#[cfg(test)] mod tests`）

`https_scheme_sends_tls_client_hello`（`#[tokio::test]`）：本地 `tokio::net::TcpListener::bind("127.0.0.1:0")`，spawn 任务 accept 后读首 3 字节；用 `reqwest::Client::builder().no_proxy().timeout(2s).build()` GET `https://127.0.0.1:<port>/`（结果忽略）；accept 与读取整体套 `tokio::time::timeout(3s)`，超时即断言失败；断言读到的首字节 `== 0x16`、第二字节 `== 0x03`（TLS 握手记录）。

- [x] **Step 2: 跑红**

Run: `cargo test -p helper-core --lib component_updates::tests::https_scheme_sends_tls_client_hello`
Expected: FAIL（未启用 TLS 时 reqwest 不建连，accept 侧读不到字节或超时）

- [x] **Step 3: 改 workspace reqwest feature、devenv 可见性、consts 常量、模块头注释**

- [x] **Step 4: 跑绿 + 依赖复核**

Run: `cargo test -p helper-core --lib component_updates && cargo tree -p helper-core -i native-tls`
Expected: PASS；`cargo tree` 输出中 `native-tls` 被 `reqwest v0.12.28` 引用。`git diff Cargo.lock` 只出现 reqwest 0.12.28 依赖列表新增 `hyper-tls`/`native-tls`/`tokio-native-tls` 等既有包；若出现新的 `[[package]]` 条目，停下报告（可能违反“不新增依赖”）。

- [x] **Step 5: 回归 devenv + clippy**

Run: `cargo test -p helper-core --test devenv && cargo clippy -p helper-core --all-targets -- -D warnings`
Expected: PASS（这几项在 devenv 内已有调用点，提升可见性不会引入 dead_code；不得新增任何 `allow`）

- [x] **Step 6: Commit**（`feat(core): reqwest 启用 native-tls，devenv 探测能力开放给组件检测`）

### Task C2: 数据结构、序列化与纯函数（解析 / 比较 / 白名单）

**Files:**
- Modify: `crates/helper-core/src/component_updates.rs`

**Interfaces:**
- Consumes: 契约中的全部类型（本任务落地）、`consts::CHATGPT_DOWNLOAD_URL_PREFIX`、`reqwest::Url`
- Produces（全部 `pub`）：
  - `fn compare_dotted(local: &str, remote: &str) -> Option<std::cmp::Ordering>`：1~4 段纯 ASCII 数字（u64），缺段按 0；任一侧格式不符 → `None`
  - `fn compare_semver(local: &str, remote: &str) -> Option<Ordering>`：`M.m.p[-pre][+build]`；忽略 `+build`；核心相等时“有预发布 < 无预发布”，双方都有时按点分标识逐个比较（纯数字按数值，否则 ASCII 字典序，前缀短者小）；格式不符 → `None`
  - `fn is_dev_build(version: &str) -> bool`：核心段为 `0.0.0`（含 `0.0.0-xxx`）
  - `enum ComponentKind { ChatgptDesktop, CodexCli }`（不序列化）
  - `fn derive_comparison(kind: ComponentKind, local: &LocalState, remote: &RemoteState) -> Comparison`：仅 `Installed` × `Ok` 时比较；CLI 本地 `is_dev_build` → `Unknown`；`remote > local` → `UpdateAvailable`，相等 → `UpToDate`，`local > remote` → `LocalNewer`，比较返回 `None` → `Unknown`；其余组合 → `Unknown`
  - `struct DesktopRelease { pub version: String, pub download_url: Option<String> }` 与 `fn parse_desktop_feed(body: &[u8]) -> Result<DesktopRelease, String>`：只声明 `schemaVersion`(u32，必须 == 1)/`packageVersion`/`packages[].architecture`/`packages[].downloadUrl`，容忍未知字段；`packageVersion` 须过 `compare_dotted` 自比较（格式合法）；取 `architecture == "x64"` 的条目，无此条目 → `download_url: None`；有则必须过 `is_allowed_download_url`，否则 `Err("下载地址不在官方白名单内")`
  - `fn is_allowed_download_url(url: &str) -> bool`：原串不得含 `..`、`%2e`/`%2E`、`\`；`Url::parse` 后 scheme == `https`、无 username/password、`host_str() == Some("persistent.oaistatic.com")`、`port().is_none()`、无 query 与 fragment、`path()` 以 `/codex-app-prod/` 开头且长度更长
  - `fn is_release_tag(tag: &str) -> bool`：等价 `^rust-v\d+\.\d+\.\d+$`（手写：前缀 `rust-v` + 恰好三段非空 ASCII 数字）
  - `fn parse_github_release(body: &[u8]) -> Result<String, String>`：`#[derive(Deserialize)] struct GithubRelease { tag_name: String }`，返回 tag（须 `is_release_tag`，否则 `Err("tag 格式不符")`）
  - `fn tag_from_location(location: &str) -> Option<String>`：去掉 `?`/`#` 之后部分，取最后一个 `/` 后的段，过 `is_release_tag`
  - `fn release_page_url(tag: &str) -> String`：`CODEX_RELEASE_TAG_URL_PREFIX + tag`
  - `fn parse_appx_output(stdout: &str) -> LocalState`：去 BOM（`\u{feff}`）、按行 trim、丢空行；无行 → `NotRegisteredForUser`；合法四段（`compare_dotted` 可比）中取最大 → `Installed`；有行但无合法版本 → `ProbeFailed { detail: "Get-AppxPackage 输出无法识别" }`
  - `fn parse_cli_version(output: &str) -> Option<String>`：找到 `codex-cli ` 之后第一个空白分隔词，须 `compare_semver(v, v).is_some()`
  - `struct VersionCache { pub latest_version: String, pub checked_at: String }` 与 `fn parse_version_cache(body: &[u8]) -> Option<VersionCache>`：读 `latest_version` / `last_checked_at`；`latest_version` 须 semver 合法；`checked_at` 去掉小数秒（`2026-10-16T05:53:49.254443Z` → `2026-10-16T05:53:49Z`）；任何失败 → `None`

- [x] **Step 1: 写失败测试**（同文件 tests 模块，下列每个一个 `#[test]`）

- `serializes_design_example_exactly`：按契约构造 §4.1 示例两组件值，`serde_json::to_value` 后 `assert_eq!` 契约 JSON（`serde_json::json!` 字面量），并断言 `chatgptDesktop.remote` 无 `fallbackFrom` 键
- `unit_variants_serialize_as_kind_objects`：`NotInstalled`→`{"kind":"notInstalled"}`、`NotRegisteredForUser`→`{"kind":"notRegisteredForUser"}`、`LocalState::UnsupportedPlatform` 与 `RemoteState::UnsupportedPlatform`→`{"kind":"unsupportedPlatform"}`、`ProbeFailed{detail:"x"}`→`{"kind":"probeFailed","detail":"x"}`、`Blocked{detail:"y"}`→`{"kind":"blocked","detail":"y"}`、`RemoteSource::GithubRedirect`→`{"kind":"githubRedirect"}`、`Comparison::LocalNewer`→`"localNewer"`
- `compare_dotted_pads_missing_segments_with_zero`：`("26.1013.4894", "26.1013.4894.0") == Equal`；`("26.1013.4894.0","26.1013.4895.0") == Less`；`("26.1013.4894.0","26.1013.40934")` 仅作为“若误用 releaseVersion 会得 Less”的记录断言；`("a.b","1")`、`("","1")`、`("1.2.3.4.5","1")` → `None`
- `release_version_never_participates`：`parse_desktop_feed` 对含 `"releaseVersion":"26.1013.40934"` 与 `"packageVersion":"26.1013.4894.0"` 的夹具返回 `version == "26.1013.4894.0"`；再以本地 `26.1013.4894.0` 派生 → `UpToDate`
- `compare_semver_handles_prerelease_and_build`：`0.164.0-alpha.2 < 0.164.0`；`0.164.0-alpha.2 < 0.164.0-alpha.10`；`0.164.0-alpha < 0.164.0-beta`；`0.163.0+abc == 0.163.0`；`0.162.1 < 0.163.0`；`1.0` / `v1.0.0` / `1.0.0-` → `None`
- `dev_build_compares_unknown`：CLI 本地 `Installed{"0.0.0"}` × 远端 `Ok{"0.163.0"}` → `Unknown`；本地 `0.0.0-dev` 同样 `Unknown`；ChatGPT 不适用该规则
- `derive_comparison_matrix`：`(0.162.1, 0.163.0)→UpdateAvailable`、`(0.163.0, 0.163.0)→UpToDate`、`(0.164.0, 0.163.0)→LocalNewer`；`NotInstalled`/`ProbeFailed`/`NotRegisteredForUser` × `Ok` → `Unknown`；`Installed` × `Unreachable`/`Invalid`/`Blocked` → `Unknown`；远端为 `CodexLocalCache` 来源的 `Ok` 照常比较
- `desktop_feed_parses_real_shape_and_tolerates_unknown_fields`：§2.1 实测夹具 + `releaseVersion` + `"futureField":{"a":1}` → `version == "26.1013.4894.0"`、`download_url == Some(".../releases/26.1013.4894.0/ChatGPT-x64.msix")`
- `desktop_feed_rejects_missing_fields_and_bad_schema`：缺 `packageVersion` / 缺 `packages` / `schemaVersion: 2` / `packageVersion: "abc"` / 非 JSON（`<html>`）→ `Err`
- `desktop_feed_without_x64_entry_has_no_url`：只含 arm64 条目 → `Ok` 且 `download_url == None`
- `allowed_download_url_rejects_bypass_forms`（Review Focus 1）：接受 `https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix`；拒绝 `http://…`、`https://persistent.oaistatic.com@evil.com/codex-app-prod/x.msix`、`https://user:pw@persistent.oaistatic.com/codex-app-prod/x`、`https://persistent.oaistatic.com.evil.com/codex-app-prod/x`、`https://persistent.oaistatic.com:8443/codex-app-prod/x`、`https://persistent.oaistatic.com/codex-app-prod/../other/x`、`https://persistent.oaistatic.com/codex-app-prod/%2e%2e/x`、`https://persistent.oaistatic.com/codex-app-prodx/x`、`https://persistent.oaistatic.com/codex-app-prod/`、`https://persistent.oaistatic.com/codex-app-prod/x?y=1`、`https://PERSISTENT.oaistatic.com/codex-app-prod/x`（Url 会小写化主机——此条断言**接受**，记录主机大小写不构成绕过）
- `desktop_feed_with_offlist_url_is_err`：x64 `downloadUrl` 为 `https://evil.example/ChatGPT-x64.msix` → `Err("下载地址不在官方白名单内")`
- `release_tag_matches_prepare_py_regex`：接受 `rust-v0.162.1`、`rust-v10.0.123`；拒绝 `rust-v0.164.0-alpha.2`、`v0.162.1`、`rust-v0.162`、`rust-v0.162.1.0`、`rust-v0.1a.1`、`rust-v.1.1`、` rust-v0.1.1`
- `github_release_takes_only_tag_name`：`{"tag_name":"rust-v0.163.0","name":"0.163.0","prerelease":false,"assets":[]}` → `Ok("rust-v0.163.0")`；`{"tag_name":"rust-v0.164.0-alpha.2"}` → `Err`；`{}` → `Err`
- `tag_from_location_takes_last_segment`：`https://github.com/openai/codex/releases/tag/rust-v0.163.0` → `Some`；相对路径 `/openai/codex/releases/tag/rust-v0.163.0` → `Some`；尾随 `?x=1` / `#a` 去掉后 `Some`；`.../releases/latest` → `None`；`.../tag/rust-v0.164.0-alpha.2` → `None`；空串 → `None`
- `appx_output_takes_max_version`（Review Focus 3）：`"26.1013.4894.0\r\n26.1020.100.0\r\n"` → `Installed{"26.1020.100.0"}`；`"\u{feff}26.1013.4894.0\r\n\r\n"` → `Installed{"26.1013.4894.0"}`
- `appx_output_blank_is_not_registered`：`""`、`"\r\n  \r\n"` → `NotRegisteredForUser`
- `appx_output_garbage_is_probe_failed`：`"Get-AppxPackage : 无法识别…"` → `ProbeFailed`
- `cli_version_parses_codex_cli_line`：`"codex-cli 0.162.1\n"` → `Some("0.162.1")`；`"codex-cli 0.164.0-alpha.2"` → `Some("0.164.0-alpha.2")`；`"codex-cli 0.0.0"` → `Some("0.0.0")`；`"0.162.1"`、`"codex-cli"`、`"codex-cli x.y"` → `None`
- `version_cache_parses_real_file_and_ignores_garbage`：§4.3 实测内容 → `latest_version == "0.163.0"`、`checked_at == "2026-10-16T05:53:49Z"`；无小数秒的 `"2026-10-16T05:53:49Z"` 原样；缺 `latest_version` / `latest_version: "abc"` / 非 JSON → `None`

- [x] **Step 2: 跑红**

Run: `cargo test -p helper-core --lib component_updates`
Expected: 编译失败（类型/函数未定义）——按“先声明类型与 `todo!()` 签名再跑”的方式让其编译通过后 FAIL（panic `not yet implemented`）

- [x] **Step 3: 实现上述类型与函数**（不引入 regex/semver crate）

- [x] **Step 4: 跑绿**

Run: `cargo test -p helper-core --lib component_updates && cargo clippy -p helper-core --all-targets -- -D warnings`
Expected: PASS

- [x] **Step 5: Commit**（`feat(core): 组件更新检查的数据结构、解析与版本比较纯函数`）

### Task C3: 网络路径决策纯函数 + client 构建

**Files:**
- Modify: `crates/helper-core/src/component_updates.rs`

**Interfaces:**
- Consumes: `crate::types::{Status, ProxyState}`、`crate::state::ProxyMode`
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum NetworkPath {
      /// 经本地代理；`proxy_url` 固定为 `http://127.0.0.1:<port>`（不用 localhost，避免解析到 ::1）
      ViaProxy { proxy_url: String },
      Direct { reason: Option<&'static str> },
      /// Secure 未就绪：远端探测整体跳过
      Blocked { detail: &'static str },
  }
  pub fn network_path_for(status: &Status) -> NetworkPath;
  pub fn build_client(path: &NetworkPath, timeout: Duration) -> Option<reqwest::Client>; // Blocked → None
  ```
  决策表（C4，判定只看 `status.mode` 与 `status.proxy.state`，Secure 下再看 `status.secure.blocked_reason` 选文案；**不读 `secure.active`**）：

  | mode | state | 结果 |
  | --- | --- | --- |
  | Secure | Running | `ViaProxy` |
  | Secure | External，或 `blocked_reason == Some("port_occupied")` | `Blocked { "Secure 代理未就绪（端口被外部进程占用）" }` |
  | Secure | 其余（Stopped） | `Blocked { "Secure 代理未就绪（代理未运行）" }` |
  | Legacy | Running | `ViaProxy` |
  | Legacy | External | `Direct { Some("代理为外部实例，模式未知，已直连") }` |
  | Legacy | Stopped | `Direct { Some("本地代理未运行，已直连") }` |

  `build_client`：共同 `.use_native_tls().timeout(timeout).redirect(Policy::none()).user_agent(concat!("codex-helper/", env!("CARGO_PKG_VERSION")))`；`ViaProxy` 加 `.proxy(Proxy::all(proxy_url)?)`（不调用 `no_proxy()`——`.proxy()` 显式设置后不读环境变量）；`Direct` 加 `.no_proxy()`。构建失败返回 `None`（调用方按 `Unreachable{"无法创建 HTTP 客户端"}` 处理）。

- [x] **Step 1: 写失败测试**

- `secure_running_goes_via_proxy`：`Status { mode: Secure, proxy: { state: Running, port: 17891 }, ..Default }` → `ViaProxy { "http://127.0.0.1:17891" }`；端口取 `status.proxy.port`（再用 port 23456 断言跟随）
- `secure_not_running_is_blocked`：Secure + Stopped → `Blocked{"Secure 代理未就绪（代理未运行）"}`；`secure.active = true` 但 state Stopped 仍 Blocked（证明不看 active）
- `secure_external_or_port_occupied_is_blocked`（Review Focus 2）：Secure + External → 端口占用文案；Secure + Stopped + `blocked_reason = Some("port_occupied")` → 端口占用文案；`blocked_reason = Some("bad_signature")` → 代理未运行文案
- `legacy_paths`：Running → ViaProxy；External → Direct 外部实例文案；Stopped → Direct 未运行文案
- `legacy_with_stale_secure_active_still_follows_mode`：Legacy + `secure.active = true` + Stopped → Direct（mode 优先）
- `blocked_builds_no_client`：`build_client(&Blocked{..}, 15s).is_none()`；`ViaProxy`/`Direct` → `Some`

- [x] **Step 2: 跑红** — Run: `cargo test -p helper-core --lib component_updates::tests` Expected: FAIL（未定义 / todo!）

- [x] **Step 3: 实现**

- [x] **Step 4: 跑绿** — Run: 同上 + `cargo clippy -p helper-core --all-targets -- -D warnings` Expected: PASS

- [x] **Step 5: Commit**（`feat(core): 组件检测按 Manager 状态决定经代理/直连/零出站`）

### Task C4: 本地版本探测（Codex CLI / ChatGPT Desktop）

**Files:**
- Modify: `crates/helper-core/src/component_updates.rs`
- Create: `crates/helper-core/tests/component_updates.rs`（本任务先放 unix 假命令用例）

**Interfaces:**
- Consumes: `devenv::{SearchEnv, run, pick_login_shell, with_login_shell_path, exit_text}`（Task C1）、`parse_cli_version`/`parse_appx_output`（Task C2）
- Produces:
  ```rust
  pub trait LocalProbe: Send + Sync {
      fn chatgpt_desktop(&self) -> impl Future<Output = LocalState> + Send;
      fn codex_cli(&self) -> impl Future<Output = LocalState> + Send;
  }
  pub struct SystemProbe { pub timeout: Duration }          // 生产实现
  pub async fn probe_codex_cli(env: &SearchEnv, timeout: Duration) -> LocalState;
  pub async fn probe_chatgpt_desktop(timeout: Duration) -> LocalState; // 非 Windows 恒 UnsupportedPlatform
  ```
  - `probe_codex_cli`：`env.find("codex")` 为 `None` → `NotInstalled`；`run(path, &["--version"], env, timeout)`，对 `stdout + "\n" + stderr` 调 `parse_cli_version`；`Err(e)` → `ProbeFailed { detail: format!("codex --version {e}") }`；无版本 → `ProbeFailed { detail: format!("codex --version 没有输出版本号（{}）", exit_text(code)) }`。
  - `probe_chatgpt_desktop`（`cfg(windows)`）：程序用绝对路径 `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`（`SystemRoot` 缺失时用 `C:\Windows`；不经 PATH 查找，防 PATH 劫持），参数 `["-NoProfile", "-NonInteractive", "-Command", "Get-AppxPackage -Name OpenAI.Codex | ForEach-Object { $_.Version }"]`，环境 `SearchEnv::system()`，`run` 的 `Err` → `ProbeFailed`，退出码非 0 → `ProbeFailed { detail: "Get-AppxPackage 执行失败（退出码 N）" }`，否则 `parse_appx_output(stdout)`。
  - `SystemProbe::codex_cli`：`SearchEnv::system()`；macOS 上经 `with_login_shell_path(env, &pick_login_shell(std::env::var_os("SHELL").as_deref()), Duration::from_secs(consts::LOGIN_SHELL_TIMEOUT_SECS))` 合并登录 shell PATH，再 `probe_codex_cli`。`SystemProbe { timeout: Duration::from_secs(consts::DEVENV_PROBE_TIMEOUT_SECS) }`（C13）。

- [x] **Step 1: 写失败测试**（`tests/component_updates.rs`，`#[cfg(unix)]` 的用例写在 `mod unix_probe`，假命令写法照抄 `tests/devenv.rs::script`）

- `cli_probe_reads_first_codex_on_path`：两个临时目录各放一个 `codex`，第一个输出 `codex-cli 0.162.1`、第二个输出 `codex-cli 9.9.9`；`probe_codex_cli(&SearchEnv::new(path, vec![]), 10s)` → `Installed{"0.162.1"}`
- `cli_probe_reads_version_from_stderr`：脚本 `echo 'codex-cli 0.163.0' >&2; exit 1` → `Installed{"0.163.0"}`
- `cli_probe_missing_is_not_installed`：空目录 → `NotInstalled`
- `cli_probe_garbage_is_probe_failed`：输出 `hello` → `ProbeFailed`，detail 含 `没有输出版本号`
- `cli_probe_timeout_is_probe_failed`：脚本 `sleep 5`，timeout 200ms → `ProbeFailed`，detail 含 `超过`
- `cli_probe_finds_cmd_shim_via_exts`：只放可执行的 `codex.cmd`（sh 脚本）、`exts = vec![".cmd".into()]` → `Installed`（模拟 Windows npm 全局安装的 `codex.cmd` 经 PATHEXT 查找）
- `chatgpt_probe_is_unsupported_off_windows`（`#[cfg(not(windows))]`，`#[tokio::test]`）：`probe_chatgpt_desktop(1s).await == LocalState::UnsupportedPlatform`

Windows 真实 `Get-AppxPackage` 不做自动化（CI 机器未装 ChatGPT），列入 Task I1 真机验收。

- [x] **Step 2: 跑红** — Run: `cargo test -p helper-core --test component_updates` Expected: 编译失败 → 补 `todo!()` 签名后 FAIL

- [x] **Step 3: 实现**

- [x] **Step 4: 跑绿 + Windows 编译检查** — Run: `cargo test -p helper-core --test component_updates && scripts/check-windows.sh` Expected: PASS（check-windows 覆盖 `cfg(windows)` 分支编译）

- [x] **Step 5: Commit**（`feat(core): 组件本地版本探测（Get-AppxPackage / codex --version）`）

### Task C5: `check_all` 编排与远端请求（wiremock）

**Files:**
- Modify: `crates/helper-core/src/component_updates.rs`
- Modify: `crates/helper-core/tests/component_updates.rs`

**Interfaces:**
- Consumes: Task C2 纯函数、Task C3 `NetworkPath`/`build_client`、Task C4 `LocalProbe`/`SystemProbe`
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Endpoints { pub desktop_feed: String, pub github_api: String, pub github_latest: String }
  impl Endpoints { pub fn production() -> Self }   // 取 consts 三个常量；测试改指 wiremock
  pub struct CheckContext<P: LocalProbe> {
      pub network: NetworkPath,
      pub timeout: Duration,          // 单请求超时，生产 COMPONENT_CHECK_TIMEOUT_SECS
      pub endpoints: Endpoints,
      pub probe: P,
      pub codex_home: PathBuf,        // 读 <codex_home>/version.json
      pub desktop_supported: bool,    // 生产 cfg!(windows)（C8）
  }
  impl CheckContext<SystemProbe> { pub fn production(network: NetworkPath, codex_home: PathBuf) -> Self }
  pub async fn check_all<P: LocalProbe>(ctx: &CheckContext<P>) -> ComponentUpdates;
  ```
- 编排顺序（Review Focus 5）：① `tokio::join!` 两个本地探测（`desktop_supported == false` 时 ChatGPT 本地直接 `UnsupportedPlatform`，不调 probe）；② 再 `tokio::join!` 两个远端探测。`Blocked` 时跳过 ②，两行 `remote = Blocked { detail }`、`via_proxy = false`、`direct_reason = None`；`desktop_supported == false` 时 ChatGPT 行 `remote = UnsupportedPlatform`、`via_proxy = false`、`direct_reason = None`、不发请求。其余行 `via_proxy = matches!(network, ViaProxy)`，`direct_reason = Direct.reason`。
- 响应体读取：`response.chunk()` 循环累计，超过 `COMPONENT_CHECK_MAX_BODY_BYTES` 立即停止并判 `Invalid { "响应体超过 256 KiB" }`。
- 错误文案只按类别（不含 URL、不含响应体）：reqwest 错误 `is_timeout()` → `超时`，`is_connect()` → `连接失败`，其余 → `网络错误`。
- ChatGPT：GET `desktop_feed` → 非 2xx：`Unreachable{"清单返回 HTTP <code>"}`；网络错误：`Unreachable{<类别>}`；`parse_desktop_feed` 失败：`Invalid{"清单格式无效：<原因>"}`（白名单失败原因原样 `下载地址不在官方白名单内`）；成功：`Ok{version, DesktopReleaseFeed, None}`。`download_url` 仅在 `UpdateAvailable` 时取清单 x64 地址。
- CLI 回退链（§2.2/§4.3）：
  1. GET `github_api`（`Accept: application/vnd.github+json`）：2xx 且 `parse_github_release` 成功 → `Ok{GithubApi}`；
  2. 仅当 API 返回 403 或 429：同一 client GET `github_latest`，3xx 且 `Location` 经 `tag_from_location` 得到 tag → `Ok{GithubRedirect}`；
  3. 上述未得到版本（任何原因）→ 读 `<codex_home>/version.json`，`parse_version_cache` 成功 → `Ok{version: latest_version, CodexLocalCache{checked_at}, fallback_from: Some(<失败链>)}`；
  4. 仍无 → API 以 2xx 返回但响应/tag 无效时 `Invalid{<失败链>}`，否则 `Unreachable{<失败链>}`。
  失败链文案：API 段 `github api 403 限流` / `github api 429 限流` / `github api 返回 HTTP <code>` / `github api <类别>` / `github api 响应无效：<原因>`；302 段以 `；302 回退失败：` 接 `缺少 Location` / `tag 格式不符` / `HTTP <code>` / `<类别>`。`download_url` 仅在 `UpdateAvailable` 时为 `release_page_url(format!("rust-v{version}"))`。
- 日志：结束时一次 `log::event("component_updates.checked", json!({ "network": "viaProxy"|"direct"|"blocked", "chatgpt_local": <kind>, "chatgpt_remote": <kind>, "chatgpt_version": <远端版本或 null>, "cli_local": <kind>, "cli_remote": <kind>, "cli_version": <远端版本或 null> }))`；不记 URL、响应体、detail。

- [x] **Step 1: 写失败测试**（`tests/component_updates.rs`；`FakeProbe { desktop: LocalState, cli: LocalState, calls: Arc<AtomicUsize> }`；`ctx()` 辅助：`Direct{None}`、timeout 2s、endpoints 指向 `MockServer::uri()` 下 `/codex-app-prod/windows-release.json`、`/repos/openai/codex/releases/latest`、`/openai/codex/releases/latest`，`codex_home` 为临时目录，`desktop_supported = true`）

ChatGPT：
- `desktop_feed_ok_up_to_date`：本地 `26.1013.4894.0` + 实测夹具 → `remote == Ok{"26.1013.4894.0", DesktopReleaseFeed, None}`、`UpToDate`、`download_url == None`
- `desktop_feed_update_available_has_versioned_url`：本地 `26.1012.1.0` → `UpdateAvailable`，`download_url` 以 `https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/` 开头
- `desktop_feed_404_is_unreachable`：→ `Unreachable{"清单返回 HTTP 404"}`、`Unknown`
- `desktop_feed_timeout_is_unreachable`：`ResponseTemplate::set_delay(2s)`，ctx timeout 300ms → `Unreachable{"超时"}`
- `desktop_feed_offlist_url_is_invalid`：x64 `downloadUrl` 指向 `https://evil.example/…` → `Invalid`，detail 含 `白名单`，`download_url == None`，`comparison == Unknown`
- `desktop_feed_html_is_invalid`（Review Focus 4）：200 + `<html>login</html>` → `Invalid`，detail 以 `清单格式无效` 开头
- `oversized_body_is_invalid`（Review Focus 4）：200 + 300 KiB 的 `[` 重复字节 → `Invalid{"响应体超过 256 KiB"}`
- `desktop_unsupported_skips_feed_request`：`desktop_supported = false` → ChatGPT 行 `local`/`remote` 均 `UnsupportedPlatform`、`via_proxy == false`；mock 上 feed 路径收到 0 个请求；FakeProbe 的 desktop 探测调用 0 次

CLI：
- `cli_api_ok`：API 200 夹具 → `Ok{"0.163.0", GithubApi, None}`；本地 `0.162.1` → `UpdateAvailable`、`download_url == "https://github.com/openai/codex/releases/tag/rust-v0.163.0"`
- `cli_api_403_falls_back_to_redirect`：API 403；latest 302 `Location: https://github.com/openai/codex/releases/tag/rust-v0.163.0` → `Ok{GithubRedirect}`、无 `fallback_from`
- `cli_api_429_falls_back_to_redirect`：同上 429
- `cli_redirect_missing_location_falls_back_to_cache`：API 403、latest 302 无 Location、写入实测 `version.json` → `Ok{"0.163.0", CodexLocalCache{"2026-10-16T05:53:49Z"}, Some(s)}`，`s` 以 `github api 403 限流` 开头且含 `缺少 Location`；序列化后 `remote` 含 `fallbackFrom`
- `cli_redirect_prerelease_tag_rejected_falls_back_to_cache`：Location 末段 `rust-v0.164.0-alpha.2` → 走缓存，`s` 含 `tag 格式不符`
- `cli_api_500_skips_redirect_and_uses_cache`：API 500 → 缓存；latest 路径收到 0 个请求；`s == "github api 返回 HTTP 500"`
- `cli_all_fail_without_cache_is_unreachable`：API 403 + latest 404 + 无缓存 → `Unreachable{"github api 403 限流；302 回退失败：HTTP 404"}`、`Unknown`、`download_url == None`
- `cli_api_invalid_tag_without_cache_is_invalid`：API 200 `{"tag_name":"rust-v0.164.0-alpha.2"}` + 无缓存 → `Invalid`，detail 以 `github api 响应无效` 开头
- `cli_cache_garbage_is_ignored`：API 500 + `version.json` 写 `not json` → `Unreachable`

网络路径与隔离：
- `secure_blocked_sends_zero_requests`（C4/§6 零出站）：`network = Blocked{"Secure 代理未就绪（代理未运行）"}`，三条路径都挂 200 mock → 两行 `remote == Blocked{同文案}`、`via_proxy == false`、`direct_reason == None`、`comparison == Unknown`；`server.received_requests().await.unwrap().is_empty()`；FakeProbe 两个本地探测各调用 1 次且 `local` 原样返回
- `via_proxy_routes_through_proxy`：`network = ViaProxy{ proxy_url: server.uri() }`，endpoints 改为不可解析主机 `http://feed.invalid/codex-app-prod/windows-release.json` 等；mock 以 `path(...)` 匹配（若 wiremock 对 absolute-form 请求的 path 匹配不生效，改 `any()` 并断言收到请求的 `url.host_str() == Some("feed.invalid")`）→ 远端 `Ok`、`via_proxy == true`
- `direct_reports_reason`：`Direct{Some("代理为外部实例，模式未知，已直连")}` → 两行 `direct_reason` 等于该文案、`via_proxy == false`
- `local_probes_finish_before_any_remote_request`（Review Focus 5）：FakeProbe 持有 `Arc<MockServer>`，在两个探测内 `sleep(100ms)` 后记录 `server.received_requests()` 长度 → 两次都为 0；检查结束后请求数 > 0
- `one_component_failure_does_not_affect_other`：feed 404 + API 200 → ChatGPT `Unreachable`、CLI `Ok`
- `requests_carry_no_credentials`：检查结束后遍历收到的请求：无 `authorization`、`cookie`、`proxy-authorization` 头；`user-agent` 以 `codex-helper/` 开头

- [x] **Step 2: 跑红** — Run: `cargo test -p helper-core --test component_updates` Expected: FAIL（`check_all` 为 `todo!()`）

- [x] **Step 3: 实现 `check_all` 与远端请求私有函数**（`fetch_desktop(client, ctx) -> (RemoteState, Option<String>)`、`fetch_cli(client, ctx) -> RemoteState`、`read_capped(response, cap) -> Result<Vec<u8>, String>` 等私有名由实现者定）

- [x] **Step 4: 跑绿** — Run: `cargo test -p helper-core && cargo clippy -p helper-core --all-targets -- -D warnings` Expected: PASS

- [x] **Step 5: Commit**（`feat(core): 组件更新检查编排、GitHub 302 回退与本地缓存兜底`）

### Task C6: Tauri 命令 `check_component_updates`

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands.rs`（命令 + 测试；文件头“命令契约”注释无需改）
- Modify: `apps/desktop/src-tauri/src/main.rs`（`generate_handler!` 在 `commands::detect_dev_env,` 之后加 `commands::check_component_updates,`）

**Interfaces:**
- Consumes: `component_updates::{network_path_for, CheckContext, SystemProbe, check_all, ComponentUpdates}`
- Produces:
  ```rust
  /// 组件更新检查（组件更新检查设计 §4.4）：只检测、不下载不安装；网络路径由 Manager 状态快照决定。
  #[tauri::command]
  pub async fn check_component_updates(state: tauri::State<'_, AppState>) -> Result<ComponentUpdates, ErrorPayload>;
  fn component_check_context(status: &Status) -> CheckContext<SystemProbe>;
  ```
  `component_check_context` = `CheckContext::production(network_path_for(status), PathBuf::from(&status.codex_home))`；命令体 = `status().await` → `component_check_context` → `Ok(check_all(&ctx).await)`。**恒返回 `Ok`**（失败已编码进各组件状态，实施偏差 D1）。

- [x] **Step 1: 写失败测试**（`commands.rs` 现有 `mod tests`）

`component_context_follows_status_snapshot`：`Status { mode: Secure, proxy: ProxyStatus { state: Stopped, port: 17891 }, codex_home: "/tmp/cuc-home".into(), ..Default::default() }` → `ctx.network` 为 `Blocked`、`ctx.codex_home == PathBuf::from("/tmp/cuc-home")`、`ctx.endpoints == Endpoints::production()`、`ctx.desktop_supported == cfg!(windows)`、`ctx.timeout == Duration::from_secs(15)`；再以 Legacy + Running + port 23456 断言 `ViaProxy{"http://127.0.0.1:23456"}`。

- [x] **Step 2: 跑红** — Run: `cargo test -p codex-helper component_context` Expected: FAIL（未定义）

- [x] **Step 3: 实现命令与注册**

- [x] **Step 4: 跑绿 + 门禁** — Run: `cargo test -p codex-helper && cargo clippy --workspace --all-targets -- -D warnings && scripts/check-windows.sh` Expected: PASS

- [x] **Step 5: Commit**（`feat(desktop): 新增 check_component_updates 命令`）

---

## 通道 gate（worktree `.worktrees/cuc-gate`）

执行顺序：G1 → G2。

### Task G1: 真实 socket 验证——同进程两端解析出本进程 PID（§7 方案 d 的前提）

**Files:**
- Create: `crates/helper-core/tests/gate_self_allow.rs`（`#![cfg(any(target_os = "macos", windows))]`；Windows 上每个用例加 `#[ignore = "需在真实 Windows 上运行"]`，与 `peer_identity_windows.rs` 一致；文件内 `static SERIAL: Mutex<()>` 串行化，理由同 `peer_identity_macos.rs` 头注释）

**Interfaces:**
- Consumes: `peer_identity::system_resolver`（现有）
- Produces: 本文件的夹具函数（Task G2 追加用例复用）：`serial()`、`spawn_fake_target() -> (u16, Arc<AtomicUsize>)`（照抄 `tests/proxy.rs` 同名函数：回 `200 OK`、计数）

已核实的结论（写进本文件头注释）：`is_peer_socket` 只认与代理 accept 端方向相反的 socket，`unique_owner` 对同 PID 去重，所以“本进程客户端连本进程监听端口”不会被判为多候选；`peer_identity_macos.rs::resolves_self_connection_ipv4` 已用同步 std socket 证明。本任务补的是**组件检测的真实形态**：tokio 监听 + tokio/reqwest 客户端、同进程、重复多次。

- [x] **Step 1: 写测试**

- `tokio_self_connection_resolves_to_own_pid`（`#[tokio::test(flavor = "multi_thread")]`）：`tokio::net::TcpListener::bind("127.0.0.1:0")`，同进程 `TcpStream::connect` 10 次；每次 accept 后在 `spawn_blocking` 里 `system_resolver().resolve(local, peer)` → `Ok` 且 `pid == std::process::id() as i32`
- `self_connection_with_concurrent_child_spawn`（`#[ignore = "诊断用：观察 fd 继承造成的瞬时多候选"]`）：一个任务循环 `tokio::process::Command::new("/usr/bin/true")`（Windows：`cmd.exe /c exit 0`）`.status()` 50 次，同时另一任务做 50 次上一个用例的自连解析；统计 `Err` 次数并 `eprintln!`，断言为 0

- [x] **Step 2: 跑测试** — Run: `cargo test -p helper-core --test gate_self_allow && cargo test -p helper-core --test gate_self_allow -- --ignored self_connection_with_concurrent_child_spawn`
  Expected: 第一个 PASS——**写完即绿**（现有解析器已满足；它防住的是“解析器改动后把同进程两端判为多候选，使 §7 放行失效”的回归，须在报告列出）；诊断用例的 `Err` 次数写进报告。

  若第一个用例失败且报“多个候选所有者”：说明 `is_peer_socket` 方向判定在 tokio/双栈下失效——停下报告，修复思路是在 `peer_identity/mod.rs` 用该四元组补单测并修正比较，属于本通道之外的文件，不擅自改。
  若只有诊断用例出现 `Err`：根因是 spawn 瞬间子进程短暂继承 socket，`unique_owner` 看到两个 PID（fail-closed 的已知代价）。处理：不改解析器语义；组件检测侧由 Task C5 保证“本地探测（spawn）全部结束后才发远端请求”消除自身重叠；真机若仍偶发，记为遗留项（候选方案：在 1s 门禁预算内把复查次数从 2 提到 3），本计划不改。

- [x] **Step 3: Commit**（`test(gate): 真实 socket 验证同进程连接解析出本进程 PID`）

### Task G2: 门禁对本进程 PID 放行 + self 放行日志

**Files:**
- Modify: `crates/helper-core/src/connection_gate.rs`（常量、字段、`admit` 前置判定、模块头补“本进程放行”说明与 §7 不变量、单测）
- Modify: `crates/helper-core/src/proxy.rs`（仅门禁 `match checked` 的 Allow 分支 + 一个私有日志辅助函数及其单测）
- Modify: `crates/helper-core/tests/gate_self_allow.rs`（追加端到端用例）

**Interfaces:**
- Produces:
  - `pub const ROLE_CODEX_HELPER_SELF: &str = "codex-helper-self";`（connection_gate.rs）
  - `ConnectionGate` 新增私有字段 `self_pid: i32`，`new()` 内取 `std::process::id() as i32`；`new` 签名不变（manager.rs 与现有测试零改动）
  - `admit(&self, peer)` 第一步：`peer.pid == self.self_pid` → `Decision::Allow { role: ROLE_CODEX_HELPER_SELF.to_string() }`，不算映像摘要、不读写缓存；其余路径逐字不变。放在 `admit` 内即满足设计“resolve 成功之后、映像哈希之前”。
  - proxy.rs：`fn gate_allowed_event(role: &str) -> Option<serde_json::Value>`：`role == ROLE_CODEX_HELPER_SELF` → `Some(json!({ "self": true }))`，否则 `None`；Allow 分支改为 `Some(Decision::Allow { role }) => { if let Some(detail) = gate_allowed_event(&role) { crate::log::event("proxy.gate_allowed", detail); } }`。现有代码没有 `proxy.gate_allowed` 事件，其他角色放行**不新增**日志（实施偏差 D4）。
- 不改：`resolve_with_retry`（解析失败、超时、多候选照旧 fail-closed）、`GATE_TIMEOUT`、proxy 其余逻辑。

- [x] **Step 1: 写失败测试**

connection_gate.rs（复用现有 `peer_with` / `policy_allowing` / `temp_image` / `FakeResolver`）：
- `admit_allows_self_pid_without_policy_or_image`：`pid = std::process::id() as i32`、`image_path = "/nonexistent/self-image"`、策略只批准无关摘要 → `Allow { role: ROLE_CODEX_HELPER_SELF }`
- `admit_other_pid_still_uses_policy`：pid 4321 + 未批准映像 → `Deny { reason: "image" }`；pid 4321 + 批准映像 → `Allow { role: ROLE_CODEX_ENGINE }`（后半**写完即绿**，防前置判定误伤他进程）
- `resolve_failure_still_fails_closed`：`FakeResolver` 恒 `Err` → `resolve_with_retry` 为 `Err` 且调用 2 次（**写完即绿**，防把 self 判定挪进解析失败分支）

proxy.rs `mod tests`：
- `gate_allowed_event_only_for_self`：`gate_allowed_event(ROLE_CODEX_HELPER_SELF) == Some(json!({"self": true}))`（无 `pid` 键）、`gate_allowed_event(ROLE_CODEX_ENGINE).is_none()`

tests/gate_self_allow.rs（门禁代理：`ConnectionGate::new(system_resolver(), policy)`，策略只批准一个随机临时文件的摘要——**不含**测试二进制映像，放行只可能来自 self 判定；`spawn_with_listener` + `ProxyConfig { gate: Some(gate), direct_overrides: {"unmatched.example" → 假目标}, ..Default }`）：
- `same_process_reqwest_through_gated_proxy_is_allowed`：`reqwest::Client::builder().proxy(Proxy::all("http://127.0.0.1:<port>")).timeout(5s)` GET `http://unmatched.example/ping` → 200、假目标计数 1（组件检测的真实用法）
- `same_process_repeated_requests_all_allowed`：同进程 `TcpStream` 连发 10 次 `GET http://unmatched.example/ping HTTP/1.1` → 每次以 `HTTP/1.1 200` 开头
- `other_process_through_gated_proxy_still_403`：子进程 `curl`（macOS `/usr/bin/curl`；Windows `%SystemRoot%\System32\curl.exe`）`-s -o <null> -w "%{http_code}" -x http://127.0.0.1:<port> http://unmatched.example/ping` → stdout == `403`、假目标计数 0（§6 回归；**写完即绿**，防放行面扩大到他进程）

- [x] **Step 2: 跑红** — Run: `cargo test -p helper-core --lib connection_gate gate_allowed_event && cargo test -p helper-core --test gate_self_allow`
  Expected: `admit_allows_self_pid_without_policy_or_image`、`gate_allowed_event_only_for_self` 编译失败/FAIL；两个同进程端到端用例 FAIL（403）；其余如上标注写完即绿

- [x] **Step 3: 实现**

- [x] **Step 4: 跑绿 + 全量回归** — Run: `cargo test -p helper-core && cargo clippy -p helper-core --all-targets -- -D warnings && scripts/check-windows.sh` Expected: PASS；`tests/proxy.rs::gate::*`、`tests/manager.rs` 的 Secure 用例全部仍绿（它们的假 peer pid 都是 4321、且已核实没有用例依赖“系统解析器 + 本进程自连被拒”）

- [x] **Step 5: Commit**（`feat(gate): Secure 门禁对 CodexHelper 本进程放行`）

---

## 通道 frontend（worktree `.worktrees/cuc-frontend`）

只依赖“跨通道接口契约”一节；不读 Rust 代码也能完成。执行顺序 F1 → F2 → F3。该 worktree 若无 `apps/desktop/node_modules`，先 `npm ci --prefix apps/desktop`（按 lock 安装既有依赖，不算新增依赖）。

### Task F1: 类型、api 封装与行视图纯函数

**Files:**
- Modify: `apps/desktop/src/types.ts`（在开发环境类型之后加“组件更新检查（与 crates/helper-core/src/component_updates.rs 一一对应）”分节，内容即契约 TS 定义）
- Modify: `apps/desktop/src/api.ts`（`checkComponentUpdates` + 注释“组件更新检查（只检测、不下载）”）
- Create: `apps/desktop/src/componentformat.ts`、`apps/desktop/src/componentformat.test.ts`

**Interfaces:**
- Produces（componentformat.ts，不依赖 DOM）：
  ```ts
  export type ComponentId = "chatgptDesktop" | "codexCli";
  export const COMPONENT_LABELS: Record<ComponentId, string>; // "ChatGPT Desktop" / "Codex CLI（独立安装）"
  export interface ComponentRowView {
    label: string;
    localText: string;
    remoteText: string;
    badge: { text: string; level: StatusLevel };
    /** 缓存兜底说明等补充信息；无则 null */
    note: string | null;
    /** 仅 comparison === "updateAvailable" 且 downloadUrl 非空时有值 */
    download: { hint: string; url: string } | null;
  }
  export function componentRowViewState(id: ComponentId, row: ComponentUpdate): ComponentRowView;
  export function componentNetworkText(result: ComponentUpdates): string;
  export function componentCheckButton(checking: boolean): { text: string; disabled: boolean };
  ```
- 文案（精确值）：
  - `localText`：installed → 版本号原样；notInstalled → `未安装`；notRegisteredForUser → `未安装或当前用户未注册（预配安装后可能需重新登录）`；probeFailed → `探测失败：${detail}`；unsupportedPlatform → `暂不支持（二期）`
  - `remoteText`：ok → 版本号原样；unreachable → `远端不可达：${detail}`；invalid → `清单无效：${detail}`；blocked → `detail` 原样；unsupportedPlatform → `暂不支持（二期）`
  - `badge`（按顺序取第一条命中）：comparison `updateAvailable` → `有新版本 v${remote.version}`/`warn`；`upToDate` → `已是最新`/`ok`；`localNewer` → `本地较新`/`ok`；`unknown` 时：remote 或 local 为 unsupportedPlatform → `暂不支持（二期）`/`warn`；codexCli 且 local 为 installed 且版本核心段为 `0.0.0` → `开发版，不比较`/`warn`；remote blocked → `Secure 代理未就绪`/`bad`；remote unreachable → `远端不可达`/`bad`；remote invalid → `清单无效`/`bad`；local notInstalled → `未安装`/`warn`；local notRegisteredForUser → `未注册`/`warn`；local probeFailed → `探测失败`/`bad`；其余 → `未知`/`warn`
  - `note`：remote ok 且 source 为 codexLocalCache → `远端不可达（${fallbackFrom ?? "原因未知"}），以下为 Codex 本地缓存，检查于 ${checkedAt}`（`checkedAt` 原样显示，不做时区换算）；其余 null
  - `download.hint`：chatgptDesktop → `请通过离线包管线更新（离线安装还需 ChatGPT-License.xml）。官方安装包地址：`；codexCli → `请通过离线包管线更新 Codex CLI。发布页：`
  - `componentNetworkText`：任一行 remote 为 blocked → `未发出远端请求（Secure 代理未就绪）`；任一行 viaProxy → `经本地代理检测`；任一行 directReason 非 null → `直连检测：${directReason}`；否则 `直连检测`（判定顺序如列）
  - `componentCheckButton`：checking → `{ text: "检查中…", disabled: true }`；否则 `{ text: "检查组件更新", disabled: false }`

- [x] **Step 1: 写失败测试**（componentformat.test.ts，夹具 `EXAMPLE: ComponentUpdates` = 契约 JSON 原样（tsc 同时校验类型与契约一致），`row(patch)` 辅助）

- `契约示例：ChatGPT 已是最新、CLI 缓存兜底有更新`：ChatGPT `badge` `{已是最新, ok}`、`download === null`；CLI `badge.text === "有新版本 v0.163.0"`、`level === "warn"`、`note === "远端不可达（github api 403 限流），以下为 Codex 本地缓存，检查于 2026-10-16T05:53:49Z"`、`download.url === "https://github.com/openai/codex/releases/tag/rust-v0.163.0"`；`componentNetworkText(EXAMPLE) === "经本地代理检测"`
- `Secure 未就绪：远端为 detail、徽标 bad、网络说明零出站`：两行 remote `{kind:"blocked", detail:"Secure 代理未就绪（代理未运行）"}`、comparison unknown、viaProxy false → `remoteText === "Secure 代理未就绪（代理未运行）"`、`badge {Secure 代理未就绪, bad}`、`componentNetworkText === "未发出远端请求（Secure 代理未就绪）"`
- `开发版 0.0.0 不比较`：codexCli local `0.0.0` + remote ok `0.163.0` + unknown → `badge.text === "开发版，不比较"`、`download === null`
- `本地状态文案`：notInstalled / notRegisteredForUser / probeFailed(detail "超过 15 秒未完成") / unsupportedPlatform 的 `localText` 与 `badge` 逐一断言
- `远端失败文案`：unreachable(`超时`) → `远端不可达：超时`/`远端不可达`；invalid(`下载地址不在官方白名单内`) → `清单无效：下载地址不在官方白名单内`/`清单无效`
- `本地较新不提示下载`：localNewer + downloadUrl 非空 → `badge {本地较新, ok}`、`download === null`
- `ChatGPT 有更新引导离线包管线`：updateAvailable + downloadUrl → `download.hint` 含 `ChatGPT-License.xml`、`download.url` 原样
- `直连说明带原因`：两行 viaProxy false、directReason `代理为外部实例，模式未知，已直连` → `直连检测：代理为外部实例，模式未知，已直连`
- `检查按钮状态`：`componentCheckButton(true)` / `(false)` deepEqual 上述值

- [x] **Step 2: 跑红** — Run: `npm --prefix apps/desktop test` Expected: FAIL（模块不存在）

- [x] **Step 3: 实现 types / api / componentformat**

- [x] **Step 4: 跑绿** — Run: `npm --prefix apps/desktop test && npm --prefix apps/desktop run check` Expected: PASS

- [x] **Step 5: Commit**（`feat(desktop): 组件更新检查的类型、命令封装与行视图纯函数`）

### Task F2: 组件版本区块 UI 与 main.ts 接线

**Files:**
- Create: `apps/desktop/src/componentpanel.ts`
- Modify: `apps/desktop/src/main.ts`、`apps/desktop/src/styles.css`

**Interfaces:**
- Consumes: Task F1 全部导出、`dom.ts::{el, clear}`、`api.checkComponentUpdates`
- Produces:
  ```ts
  export interface ComponentPanelHandlers { onCheck: () => void }
  export interface ComponentPanelState { checking: boolean; result: ComponentUpdates | null; checkedAt: number | null }
  export function createComponentPanel(handlers: ComponentPanelHandlers): { root: HTMLElement; update: (state: ComponentPanelState) => void };
  ```
  DOM 结构与 class 按“设计 §9 拍板”一节；未检测时结果区显示 `p.component-empty`“点击“检查组件更新”查看 ChatGPT Desktop 与 Codex CLI 的最新版本。”；`checkedAt` 非 null 时标题行右侧 `span.component-checked` 显示 `上次检查 ${formatClock(checkedAt)}`（复用 `devenvformat.ts::formatClock`）；与 devenvpanel 相同的 `rendered` 引用比较，结果未变时不重建结果区（保护用户正在复制的 URL）；`results` 设 `aria-busy`；动态内容全部走 `el()` 的 textContent，不用 innerHTML。
- main.ts：`OpKind` 加 `"checkComponentUpdates"`；模块状态 `componentUpdates: ComponentUpdates | null`、`componentCheckedAt: number | null`；`runCheckComponentUpdates()`：已在进行中则返回 → `busy.add` → `render()` → `await api.checkComponentUpdates()` 成功写结果与 `Date.now()`，失败 `messageRegion.show(toErrorPayload(error).message, "error")` 并保留上次结果 → `finally` 删 busy 并 `render()`；`appRoot.append(...)` 中把 `componentPanel.root` 放在 `updatePanel.root` 之后；`render()` 调 `componentPanel.update(...)`。区块**始终显示**（不随 `showUpdatePanel`；平台差异由 Rust 返回的 unsupportedPlatform 表达）。**不在启动或切页时自动检测**（C10）。
- styles.css：在“更新区”之后加“组件版本区”分节：`.component-region` 同 `.update-region`；`.component-badge-ok|warn|bad` 用现有 `--ok-bg/--ok-fg`、`--warn-*`、`--bad-*` 变量；`.component-url` 等宽、`user-select: all`、`word-break: break-all`；暗色无需额外变量（变量已在 `prefers-color-scheme` 中重定义）。

- [x] **Step 1: 类型检查先红** — 先在 main.ts 写好 `import { createComponentPanel } from "./componentpanel"` 与调用 → Run: `npm --prefix apps/desktop run check` Expected: FAIL（模块不存在）。本任务是 DOM 胶水，无纯函数新增，不另写 node 测试（视图逻辑已在 F1 覆盖），在报告中说明。

- [x] **Step 2: 实现 componentpanel.ts、main.ts 接线、样式**

- [x] **Step 3: 跑绿** — Run: `npm --prefix apps/desktop run check && npm --prefix apps/desktop test && npm --prefix apps/desktop run build` Expected: PASS

- [x] **Step 4: Commit**（`feat(desktop): 新增组件版本区块`）

### Task F3: dev-mock 组件检测场景

**Files:**
- Modify: `apps/desktop/src/dev-mock.ts`

**Interfaces:**
- 新 URL 参数 `?components=`（可与 `?scenario=` 组合），取值：`latest`（默认：两组件已是最新、经代理）、`cliUpdate`（CLI `GithubApi` 来源 `0.163.0`、本地 `0.162.1`、updateAvailable、release 页 URL）、`cacheFallback`（即契约示例：CLI 缓存兜底 + 直连原因）、`blocked`（Secure 未就绪：两行 blocked、本地照常）。`status.platform === "macos"` 时 ChatGPT 行改为 local/remote 均 `unsupportedPlatform`、comparison `unknown`。
- `case "check_component_updates"`：`setTimeout` 800ms 后 resolve（预览“检查中…”），返回值类型标注为 `ComponentUpdates`（tsc 校验与契约一致）；文件头注释的场景列表补上 `?components=`。

- [x] **Step 1: 先红** — 在 `case` 中先写 `return componentUpdates(componentsKind, status);` 调用 → Run: `npm --prefix apps/desktop run check` Expected: FAIL（函数未定义）

- [x] **Step 2: 实现 `function componentUpdates(kind: string, status: Status): ComponentUpdates`**

- [x] **Step 3: 跑绿 + 产物检查** — Run: `npm --prefix apps/desktop run check && npm --prefix apps/desktop run build && ! grep -r "__CODEX_HELPER_DEV_MOCK__" apps/desktop/dist` Expected: PASS（生产产物不含 mock）。手动：`npm --prefix apps/desktop run dev`，浏览器打开 `?components=cacheFallback`、`?components=blocked`、`?scenario=macos` 目视确认四种状态与“检查中…”。

- [x] **Step 4: Commit**（`feat(desktop): dev-mock 增加组件检测预览场景`）

---

## 集成

### Task I1: 合并三通道、门禁与契约核对

**Files:**
- 无新文件；只在 core worktree 产生合并提交。接缝问题回到所属通道的文件修复并在该通道分支上提交后重新合并。

- [x] **Step 1: 合并** — 在 core worktree：`git merge --no-ff pi/cuc-gate -F /tmp/cuc-merge-gate.txt`、`git merge --no-ff pi/cuc-frontend -F /tmp/cuc-merge-frontend.txt`（提交信息 `merge: 合并组件更新检查 gate 通道` / `… frontend 通道`，尾注 `Co-Authored-By: Pi`）。三通道文件不相交，预期无冲突；若冲突，停下报告，不用 `-X ours/theirs` 粗暴解决。

- [x] **Step 2: 门禁** — Run: `scripts/gate.sh` Expected: 末行 `== gate passed`

- [x] **Step 3: 契约形状核对**
  - Rust：`cargo test -p helper-core --lib component_updates::tests::serializes_design_example_exactly` PASS
  - TS：`componentformat.test.ts` 的 `EXAMPLE` 与 Rust 测试中的 `json!` 字面量逐字段比对（同为契约 JSON 原样）；`dev-mock.ts` 的 `componentUpdates()` 返回值已被 tsc 以 `ComponentUpdates` 校验
  - 命令名：`grep -n "check_component_updates" apps/desktop/src-tauri/src/{commands.rs,main.rs} apps/desktop/src/{api.ts,dev-mock.ts}` 四处都命中
  - 变体名覆盖：Rust 每个 `kind`（installed / notInstalled / notRegisteredForUser / probeFailed / unsupportedPlatform / ok / unreachable / invalid / blocked / desktopReleaseFeed / githubApi / githubRedirect / codexLocalCache）在 `types.ts` 中都有对应字面量；`componentformat.ts` 的 switch 在 `noImplicitReturns` 下穷尽

- [x] **Step 4: 依赖复核** — Run: `cargo tree -p helper-core -i native-tls && git diff 666cdba -- Cargo.toml apps/desktop/package.json apps/desktop/package-lock.json` Expected: native-tls 被 reqwest 0.12.28 引用；Cargo.toml 仅 reqwest feature 一处变化；package*.json 无变化

- [ ] **Step 5: 手动验收清单（真机，结果写入报告；无法执行的标“未验证”）**——逐条对应设计 §6：Windows 代理运行两组件结果与线上清单 / GitHub latest 一致、`Get-AppxPackage` 版本与“设置→应用”一致；Legacy 停代理直连或失败原因正确；Secure 停代理时远端“Secure 代理未就绪”且日志无 `component_updates` 之外的出站、“最近连接”无新记录；Secure 运行时检测成功、“最近连接”可见 `persistent.oaistatic.com` / `api.github.com`、日志有 `proxy.gate_allowed {"self":true}`，同时 `curl -x http://127.0.0.1:17891 https://api.github.com` 仍 403；未安装组件的机器显示未安装；macOS CLI 行可用（Finder 启动）、ChatGPT 行“暂不支持（二期）”；`peer_identity_windows` 与 `gate_self_allow` 在 Windows 真机 `-- --ignored` 跑通。

- [ ] **Step 6: 更新设计文档状态**（`docs/superpowers/specs/2026-10-20-component-update-check-design.md` 状态行改“已实现（2026-10-xx）”并在 §10 追加“实施偏差”指向本计划；提交 `docs(spec): 组件更新检查设计标记为已实现`）

---

## 实施偏差与设计缺口（执行者遵循此处，不再回头改设计假设）

| 编号 | 内容 | 处理 |
| --- | --- | --- |
| D1 | 设计 §4.4 写 `check_component_updates(...) -> ComponentUpdates`；Tauri 要求带 `State<'_>` 引用参数的 async 命令返回 `Result` | 签名 `Result<ComponentUpdates, ErrorPayload>`，恒 `Ok`；前端 `invoke<ComponentUpdates>` 不受影响 |
| D2 | `LocalState` 无“不支持平台”变体，macOS 的 ChatGPT 行本地列无合法值 | 新增 `LocalState::UnsupportedPlatform`（`{"kind":"unsupportedPlatform"}`），不探测 |
| D3 | 设计说代理端口取 `ManagerOptions`、缓存目录取 `HelperPaths`；`Manager` 不暴露二者 | 端口取 `status.proxy.port`（`build_status` 直接赋自 `options.proxy_port`，同源）；codex home 取 `PathBuf::from(status.codex_home)`；不改 manager.rs |
| D4 | 设计称“自身放行记 `proxy.gate_allowed {"self":true}`、其他路径日志不变”，但现有代码无 `gate_allowed` 事件 | 只为 self 放行新增该事件；其他放行仍不记 |
| D5 | `CheckContext` 设计只列网络路径/超时/探测函数/HelperPaths | 追加 `endpoints`（测试指向 wiremock，生产取 consts）与 `desktop_supported`（生产 `cfg!(windows)`，测试可在 macOS 跑 ChatGPT 流程）；探测注入用 `LocalProbe` trait（RPITIT，不引入 async-trait） |
| D6 | 设计只对 403/429 定义 302 回退，未说 API 其他失败 | 其他失败（网络/5xx/响应无效）跳过 302，直接本地缓存兜底 |
| D7 | 设计只对 302 回退的 tag 定义正则 | API 的 `tag_name` 也过同一判定（预发布/异常 tag 判 invalid 或走缓存），与 prepare.py “最新版”口径一致 |
| D8 | 清单缺 x64 条目未定义 | 版本照常比较、`download_url = None`（与 C12 的 arm64 无条目处理同构）；白名单只约束出现的地址 |
| D9 | `schemaVersion` 未规定取值 | 非 1 判 `invalid`（未知 schema 不采信） |
| D10 | 响应体大小无上限 | 256 KiB 上限，超出判 `invalid` |
| D11 | Legacy + Stopped 的 `direct_reason` 未给文案 | `本地代理未运行，已直连` |
| D12 | PowerShell 程序路径未规定 | `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe` 绝对路径，不经 PATH |
| D13 | `checked_at` 展示格式未规定 | 去小数秒后原样显示（不做时区换算） |
| D14 | 编排顺序未规定 | 先完成全部本地探测再发远端请求（降低同进程 spawn 与建连重叠导致门禁瞬时多候选的概率） |
| D15 | 组件区块显示条件未规定 | 始终显示；dev-mock 在设计三场景之外多一个 `blocked` 预览 |

