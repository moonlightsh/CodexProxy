# 网关与上游 SOCKS5 可配置、本地代理可关闭实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> 执行方式：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans，在已有分支 `claude/configurable-endpoints` 上逐项实施。每个步骤先写测试并实际跑出失败，再写实现并跑到通过，然后提交。不要跳过跑红。

**目标：** 把写死的模型网关 URL 与上游 SOCKS5 地址改成桌面界面可配置的两个参数，并增加“启用本地代理”开关；关闭时不启动本地代理、不写 `~/.codex/.env` 受管块，`config.toml` 接管保持不变。

**架构：** 新建 `endpoints::Endpoints` 值对象，由 `Manager` 在运行时短锁里持有唯一生效副本，经显式参数传给 `codex_config`、`codex_env`、`rules`。本地代理把上游地址和恒直连主机放进 `Arc<RwLock<Routing>>`，连接开始时拷贝快照，保存地址时就地更新，不重启监听、不排空、不重建 Secure 门禁。代理开关是 `HelperState.proxy_disabled` 上的独立偏好：受管开关决定是否接管配置，代理开关只决定接管时要不要写 `.env` 和启动代理。

**技术栈：** Rust 2024（helper-core：serde、toml_edit、reqwest 0.12 的 `Url`、tokio）、Tauri 2、TypeScript（`node --test --experimental-strip-types`，纯函数测试，无 DOM shim）。

**设计：** `docs/superpowers/specs/2026-11-16-configurable-endpoints-design.md`（本计划的唯一需求来源；与代码现状冲突时以该设计为准，下面“已核实的代码事实”只说明怎么接到现有签名上）。

## 全局约束

- 分支 `claude/configurable-endpoints` 已存在，直接在该分支上工作。不新建 worktree，不切换分支。
- 提交信息使用中文，风格 `feat(scope): …` 或 `docs: …`。正文后空一行，尾注原文为 `Co-Authored-By: Claude Code`。用 `git commit -F` 从文件读入，避免 shell 吃掉换行。
- 不 push，不建 PR，不改 git config，不使用 `--no-verify`、`reset --hard`、`clean -f`、force push。
- Secure 模式上游仍取注册包 `server_host` / `server_port`（`activate_secure_locked` 里 `SecureUpstream { host: bundle.server_host, port: bundle.server_port, … }`）。本计划不改这段赋值，不把用户配置的 SOCKS5 填进安全上游。
- 代理关闭时组件下载不回退直连。不改 `component_downloads::download_client` 的拒绝文案：非 `ViaProxy` 仍返回 `downloadProxy` / `请先启用本地代理，再下载组件（不会回退直连）`。
- 不提供环境变量或命令行覆盖网关与 SOCKS5。`CODEX_HELPER_PROXY_PORT` 继续只覆盖本地代理端口。
- 不改分流规则集（`EXACT_DOMAINS` / `DOMAIN_SUFFIXES` / `KEYWORDS`），不改 `DEFAULT_PROXY_PORT`（17891）。
- 不为 Codex 自动重启。网关变更或代理开关变更只通过 `RestartCodex` 警告提示用户。
- 不新增 Rust crate 或 npm 包。URL 解析用已经依赖的 `reqwest::Url`。
- 代码注释与界面文案使用中文。日志可以记地址，禁止记 Key。
- 前端测试只能是纯函数（无 DOM shim），断言只用 `node:assert/strict` 的 `ok` / `equal` / `deepEqual` / `match`。
- 门禁是 `scripts/gate.sh`（fmt、clippy `-D warnings`、`cargo test --workspace`、Windows 目标检查、`tsc`、前端 test、前端 build）。Task 8 必须全绿。
- 每个 Task 结束时工作区必须能编译。签名改动的同一个提交里改完所有调用点。

## 评审重点

1. **地址只有一个来源。** 今天 `options.gateway_base_url` 只给 Key 校验用，`ensure_managed` 仍把 `consts::GATEWAY_BASE_URL` 写进 `config.toml`（`tests/manager.rs` 的启用断言因此期望常量 URL）。Task 4 合并之后，启用、对账、`set_endpoints` 写入的 `base_url` 和 `NO_PROXY` 必须来自 `runtime.endpoints`。沙箱的 `default_endpoints` 是 wiremock 与 `127.0.0.1:<假 SOCKS5 端口>`，现有“`base_url == consts::GATEWAY_BASE_URL`”和“`.env` 等于 `render_block(port)`（主机为 `10.20.30.61`）”的断言要改成沙箱默认端点。不要为了保住旧断言把两套地址留着。
2. **恒直连不再读 consts。** `10.20.30.61` 不命中任何 SOCKS5 规则，去掉硬编码后它仍然是 `Direct`。能证明“旧默认 IP 不再被单独特判”的用例，是一个本来会走 SOCKS5 的主机（`api.openai.com`）：在 `always_direct` 里才是 `Direct`，不在里面仍是 `Socks5`。回环（`localhost` / `127.0.0.1` / `::1`）继续在 `route_for_host` 内部写死。
3. **热更新不重启代理。** `handle_connection` 在分流前拷贝一份 `Routing` 并立刻释放 `RwLock`，锁不跨 `await`。`set_endpoints` 只写这把锁，不调用 `stop_proxy` / `spawn`，不排空，不重建 Secure 门禁。已建立的 CONNECT 隧道继续用旧上游，测试不要求迁移它。
4. **`Runtime` 今天是 `Copy`。** `build_status` 写的是 `let runtime = *self.runtime();`。放入 `Endpoints` 后去掉 `Copy`，这里改成 `clone()`。短锁仍然只在拷贝字段时持有。
5. **第 4 步失败不回滚 state。** `state.json` 已写入新地址后，`config.toml` 校正失败就返回 `ConfigInvalid`，state 保留新值。界面随后看到的 `gateway_base_url` 是用户刚填的值，同时 `config_error` 有内容。
6. **Key 校验在 ops 锁外面。** 只有网关 URL 变了且凭据库里已有 Key 才调用 `verify_key_at`。401/403 → `KeyRejected`，其余失败 → `KeyCheckUnavailable`。两种情况保存都已经成功。日志事件里不得出现 Key 明文。
7. **`env_managed` 的新谓词有范围。** 仅当 `state.enabled && state.proxy_disabled` 时，`env_managed = !env.has_block`。受管关闭时仍用 `block_up_to_date`。若对所有 `proxy_disabled` 都改成“块不存在”，空闲且没有 `.env` 块时状态灯会从“未接管”变成“部分接管”。
8. **`network_path_for` 先看 Secure。** `proxy_enabled == false` 时 Legacy 返回 `Direct { reason: Some("本地代理未开启，已直连") }`，与“本地代理未运行，已直连”分开。Secure 且非 `Running` 仍返回 `Blocked`，不因为 `proxy_enabled` 为假而直连。组件下载遇到这条 `Direct` 仍走现有 `downloadProxy` 拒绝。
9. **SOCKS5 输入框只做视觉置灰。** 代理关闭时加样式类，不设置 `disabled` / `readonly`，用户仍能修改并保存，值留到下次开启。
10. **不要动 `component_updates::Endpoints`。** 那个类型是组件检测的三个 URL。新值对象是 `crate::endpoints::Endpoints`。禁止重命名旧类型，禁止用 `use crate::endpoints::*` 和 `use crate::component_updates::*` 混在同一个作用域。

## 已核实的代码事实（写接口时用这些签名，不要沿用想象）

- `consts.rs` 模块头是“全部写死，不提供界面修改”。`GATEWAY_BASE_URL = "http://10.20.30.61:8080"`，`GATEWAY_HOST = "10.20.30.61"`，`GATEWAY_PORT = 8080`，`SOCKS5_HOST = "10.20.30.61"`，`SOCKS5_PORT = 7891`。这些常量保留。
- `HelperState` 已有 `#[serde(default)]`。`state::load` 先要求顶层是 JSON 对象。旧文件缺字段会得到 `Default`。
- `ManagerOptions` 现有五个独立字段：`socks5_host`、`socks5_port`、`gateway_base_url`、`gateway_host`、`gateway_port`。`Default` 与 `production` 都填 consts。`tests/manager.rs` 的 `Sandbox::options_with` 把 `gateway_base_url` 设成 wiremock 的 `uri()`，主机 `127.0.0.1`，端口为 mock 端口；SOCKS5 主机 `127.0.0.1`，端口是一个无人监听的 `free_port()`。
- `Runtime` 在 `manager.rs` 约 148 行，`#[derive(Debug, Clone, Copy, Default)]`，字段是 `proxy_state`、`gateway`、`socks5`、`mode`、`secure_handle`。
- `gateway::verify_key(key)` 只是 `verify_key_at(consts::GATEWAY_BASE_URL, key, KEY_CHECK_TIMEOUT)` 的包装，仓库里没有其他调用点。`verify_key_at` 保留。`check_new_key` 已经调用 `verify_key_at(&self.options.gateway_base_url, …)`。
- `codex_config::inspect(path, credential_command)` 与 `apply_managed(path, backup, credential_command)` 没有网关参数。`ensure_managed` 把 `base_url` 写成 `consts::GATEWAY_BASE_URL`。`fully_managed` 的定义是“在副本上再跑一遍 `ensure_managed` 没有改动”。传入同一个 `gateway_base_url` 后，`base_url` 不一致自然变成未受管。
- `codex_env::block_lines(proxy_port)` 的 `NO_PROXY` 行是 `format!("NO_PROXY={},127.0.0.1,localhost", consts::GATEWAY_HOST)`。`render_block`、`upsert_block`、`inspect_text`、`inspect_file`、`write_block_to_file` 都只收端口。`block_up_to_date` 等于“`upsert_block` 的结果与原文相同”。调用点还包括 `manager.rs`、`tests/manager.rs`、`tests/codex_config.rs`、`apps/credential/src/main.rs`、`apps/credential/tests/cli.rs`。
- `rules::route_for_host(host)` 只收主机。`is_always_direct` 比较 `consts::GATEWAY_HOST`、`consts::SOCKS5_HOST` 以及 `localhost` / `127.0.0.1` / `::1`。生产调用点只有 `proxy.rs` 的 `handle_connection`。测试调用点在 `rules.rs` 与 `component_downloads.rs`。
- `ProxyConfig` 有 `socks5_host` / `socks5_port`。`connect_upstream` 在无 `secure_upstream` 时调用 `connect_via_socks5_at(&config.socks5_host, config.socks5_port, …)`。`spawn` / `spawn_with_listener` 把 `ProxyConfig` 放进 `Shared`。测试字面量在 `tests/proxy.rs`（两处）和 `tests/upstream_tls.rs`（一处）。
- `Manager::proxy_config` 从 `options.socks5_*` 填 `ProxyConfig`。`secure_proxy_config` 在这份配置上附加 `secure_upstream` 与 `gate`。
- `build_status` 的 `gateway_addr` / `socks5_addr` 来自 `options`，不是 state。`env_managed = env.block_up_to_date`。`residue = !state.enabled && (config_residue || env.has_block)`。
- `probe_reachability` 用 `options.gateway_host/port` 与 `options.socks5_host/port` 做 TCP 探测，超时 `gateway::TCP_PROBE_TIMEOUT`（3 秒）。不持有 ops 锁。
- `enable_steps` 的顺序是：写凭据 → 记原值 → `apply_managed` → `ensure_proxy` → `write_block_to_file` → 置 `enabled`。`reconcile_locked` 在已启用且有凭据时做 `apply_managed` → `ensure_proxy` → `write_block_to_file`。
- `Reachability` 是 `Unknown | Reachable | Unreachable`，`#[serde(rename_all = "camelCase")]`。前端 `types.ts` 的同名联合是 `"unknown" | "reachable" | "unreachable"`。
- `Status`（Rust）已有 `mode` 与 `secure`。前端 `types.ts` 的 `Status` 没有这两个字段，界面也不读它们。本计划不补这两个字段。
- `ErrorPayload` 只有 `code` 与 `message`。`ManagerError::code()` / `payload()` 在 `types.rs`。
- `network_path_for` 在 `component_updates.rs`。测试辅助 `status(mode, state, port)` 使用 `..Default::default()`。新增 `bool` 字段的 `Default` 是 `false`，所以这个辅助函数必须显式把 `proxy_enabled` 设为 `true`，否则现有“经代理 / 未运行已直连”用例会全部改道。
- `component_updates.rs` 已有 `pub struct Endpoints { desktop_feed, github_api, github_latest }`。新模块不要用同一个短名去替换它。
- Tauri：`commands.rs` 的 `AppState::finish` 在成功时推送返回的 `Status`，失败时再取一次 `status()` 推送后把 `ManagerError` 转成 `ErrorPayload`。`main.rs` 的 `generate_handler!` 列出全部命令。命令参数名与 `api.ts` 的 `invoke` 键一致（`request`、`enabled`）。
- 前端：`OpKind` 与 `busy: Set<OpKind>` 在 `main.ts`。布局是横幅 → 受管开关 → Key 面板 → 状态灯/连接/底部。`keypanel.ts` 的 `openForm()` / `focusInput()` 已存在。`switchcontrol.ts` 的 `createSwitchRegion` 绑死了“受管模式”和 `status.enabled`。`statuslights.ts` 直接读 `status.gatewayAddr` / `status.socks5Addr`。`dev-mock.ts` 的 `baseStatus()` 是唯一手写的完整 `Status` 字面量。`formatReachabilityLabel` 的 `switch` 必须覆盖新联合成员，否则 `tsc` 失败。
- 可打印 ASCII 的现有写法是 `bytes().all(|b| b.is_ascii_graphic())`（`component_updates.rs` 下载地址白名单）。空格和控制字符都不是 graphic。
- `log::event(name, detail)` 在未 `log::init` 时丢弃。`tests/manager.rs` 已通过 `log_dir()` 初始化日志。脱敏会把键名像 key/secret 的值换成 `***`。

## 文件结构（实施前先看这一张）

| 文件 | 动作 | 责任 |
| --- | --- | --- |
| `crates/helper-core/src/endpoints.rs` | 新建 | 解析、规范化、`resolve`、展示用 `host:port` |
| `crates/helper-core/src/lib.rs` | 修改 | `pub mod endpoints;` |
| `crates/helper-core/src/consts.rs` | 修改 | 模块头与五个地址常量的注释改为“默认值” |
| `crates/helper-core/src/state.rs` | 修改 | `gateway_base_url`、`socks5_addr`、`proxy_disabled` |
| `crates/helper-core/src/codex_config.rs` | 修改 | `inspect` / `apply_managed` / `ensure_managed` 增加 `gateway_base_url` |
| `crates/helper-core/src/codex_env.rs` | 修改 | 受管块函数增加 `gateway_host` |
| `crates/helper-core/src/rules.rs` | 修改 | `route_for_host(host, always_direct)` |
| `crates/helper-core/src/proxy.rs` | 修改 | `Routing`、快照、`ProxyConfig.routing` |
| `crates/helper-core/src/types.rs` | 修改 | `Status` 新字段、`Reachability::Disabled`、请求/结果/警告、两个 `ManagerError` |
| `crates/helper-core/src/manager.rs` | 修改 | `default_endpoints`、`runtime.endpoints`、`set_endpoints`、`set_proxy_enabled`、对账与启用按开关分支 |
| `crates/helper-core/src/gateway.rs` | 修改 | 删除无调用点的 `verify_key` |
| `crates/helper-core/src/component_updates.rs` | 修改 | `network_path_for` 认识 `proxy_enabled` |
| `crates/helper-core/tests/manager.rs` | 修改 | 构造 `default_endpoints`；新增保存、热更新、开关、Secure 互斥测试 |
| `crates/helper-core/tests/proxy.rs` | 修改 | `ProxyConfig` 字面量；新增 Routing 热更新测试 |
| `crates/helper-core/tests/upstream_tls.rs` | 修改 | `ProxyConfig` 字面量改为写 `routing` |
| `crates/helper-core/tests/codex_config.rs` | 修改 | `inspect` / `apply_managed` 增加 URL 参数 |
| `apps/credential/src/main.rs`、`apps/credential/tests/cli.rs` | 修改 | `render_block` 增加主机参数 |
| `apps/desktop/src-tauri/src/commands.rs`、`main.rs` | 修改 | `set_endpoints`、`set_proxy_enabled` |
| `apps/desktop/src/endpointformat.ts`、`endpointformat.test.ts`、`endpointpanel.ts` | 新建 | 纯函数与面板 |
| `apps/desktop/src/{types,api,main,dev-mock,statuslights,format,format.test,switchcontrol}.ts`、`styles.css` | 修改 | 类型、调用、布局、状态灯、开关、样式 |
| `docs/superpowers/specs/2026-09-23-codex-helper-design.md` | 修改 | 只改 §1.1 |

---

### Task 1: Endpoints 值对象与 state 字段

**文件：**

- 新建 `crates/helper-core/src/endpoints.rs`
- 修改 `crates/helper-core/src/lib.rs`（在 `codex_env` 与 `component_downloads` 之间加 `pub mod endpoints;`）
- 修改 `crates/helper-core/src/consts.rs` 的模块头和五个地址常量注释
- 修改 `crates/helper-core/src/state.rs` 的 `HelperState` 与 `save_then_load_round_trips` 字面量

**接口：**

- 产出 `crate::endpoints::EndpointField`：`Gateway`、`Socks5`，方法 `as_str() -> &'static str` 分别返回 `"gateway"`、`"socks5"`。
- 产出 `EndpointError { pub field: EndpointField, pub message: String }`，`thiserror` 显示为 `{message}`。
- 产出下面这些中文原因常量，测试与错误共用，不要在调用点再写一份：
  - `MSG_EMPTY = "不能为空"`
  - `MSG_GRAPHIC = "只能包含可打印 ASCII 字符"`
  - `MSG_SCHEME = "协议只允许 http 或 https"`
  - `MSG_HOST = "必须包含主机"`
  - `MSG_USERINFO = "不能包含用户名或密码"`
  - `MSG_QUERY = "不能包含查询参数或片段"`
  - `MSG_URL = "不是合法的 URL"`
  - `MSG_SOCKS_FORM = "格式应为 host:port"`
  - `MSG_SOCKS_PORT = "端口必须在 1–65535 之间"`
  - `MSG_SOCKS_SCHEME = "不要带协议（例如不要写 socks5://）"`
- 产出：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub gateway_base_url: String,
    pub gateway_host: String,
    pub gateway_port: u16,
    pub socks5_host: String,
    pub socks5_port: u16,
}

impl Endpoints {
    pub fn default_values() -> Self;
    pub fn resolve(state: &HelperState, defaults: &Endpoints) -> Self;
    pub fn parse(gateway_url: &str, socks5_addr: &str) -> Result<Self, EndpointError>;
    pub fn gateway_addr(&self) -> String;
    pub fn socks5_addr(&self) -> String;
}
```

- `HelperState` 在本任务只增加：

```rust
/// 用户配置的模型网关 URL；None = 使用调用方传入的默认值
pub gateway_base_url: Option<String>,
/// 用户配置的上游 SOCKS5 地址（host:port）；None = 使用调用方传入的默认值
pub socks5_addr: Option<String>,
```

`proxy_disabled` 留到 Task 5，避免这个任务把代理开关的行为做进去。

**规范化规则（测试按这个表断言，不要再放宽）：**

1. 先去掉两端空白。空串 → `MSG_EMPTY`。然后要求每个字节都是 `u8::is_ascii_graphic`，否则 `MSG_GRAPHIC`。这一步在 `Url::parse` 之前，所以全角字符、空格、控制字符都进不了 URL 解析器。
2. 网关：`reqwest::Url::parse` 失败 → `MSG_URL`。scheme 不是 `http`/`https` → `MSG_SCHEME`。`host_str()` 为 `None` → `MSG_HOST`。`username()` 非空或 `password().is_some()` → `MSG_USERINFO`。`query()` 或 `fragment()` 为 `Some`（含空串）→ `MSG_QUERY`。
3. 网关主机用 `url.host_str()`，再 `rules::normalize_host`（小写、去掉末尾点）。IPv6 不带方括号。端口用 `url.port_or_known_default()`，`http` 得 80，`https` 得 443；没有已知端口则 `MSG_URL`。
4. 重建 `gateway_base_url`：scheme 与主机小写；仅当 `url.port()` 是 `Some` 时写 `:端口`（因此 `http://host:80` 规范化后不再带 `:80`，`http://10.20.30.61:8080` 保留 `:8080`）；路径去掉全部末尾 `/`，结果为空则不写路径，否则保留前导 `/`。不带 query 与 fragment。
5. SOCKS5：含 `://` → `MSG_SOCKS_SCHEME`。以 `[` 开头则必须匹配 `[主机]:端口`，主机非空；否则按最后一个 `:` 分成主机与端口，主机非空且不含 `:`。分不开 → `MSG_SOCKS_FORM`。端口不是 `u16` 或不在 `1..=65535`（含 `0` 与 `65536`）→ `MSG_SOCKS_PORT`。主机经 `normalize_host`，存盘不带方括号。`::1:7891` 这种不带方括号的 IPv6 判为 `MSG_SOCKS_FORM`。
6. `gateway_addr` / `socks5_addr`：主机含 `:` 时写成 `[host]:port`，否则 `host:port`。
7. `default_values()` 用五个 consts 调 `parse`，`expect` 成功。结果必须是 `gateway_base_url == "http://10.20.30.61:8080"`、`socks5_addr() == "10.20.30.61:7891"`。
8. `resolve`：某字段为 `None` 时沿用 `defaults` 的对应字段。为 `Some` 时只解析那一项（另一项用 `defaults` 的展示串喂给 `parse`）。解析失败则该项回落 `defaults`，并 `log::event("endpoints.fallback", json!({ "field": "gateway" 或 "socks5", "message": error.message }))`。不把原始输入写进日志。

- [ ] **Step 1: 写失败测试**

在 `endpoints.rs` 底部 `#[cfg(test)] mod tests` 写下面这些测试。此时模块还没加入 `lib.rs`，下一步的编译失败就是红。

`parse_accepts_documented_forms`：下表每一行 `parse` 成功，并逐字段相等。最后一行再断言 `gateway_addr()` 与 `socks5_addr()`。

| gateway 输入 | socks5 输入 | gateway_base_url | gateway_host | gateway_port | socks5_host | socks5_port | gateway_addr | socks5_addr |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `http://10.20.30.61:8080` | `10.20.30.61:7891` | `http://10.20.30.61:8080` | `10.20.30.61` | 8080 | `10.20.30.61` | 7891 | `10.20.30.61:8080` | `10.20.30.61:7891` |
| `  HTTP://Example.COM:8080/  ` | ` Example.COM:7891 ` | `http://example.com:8080` | `example.com` | 8080 | `example.com` | 7891 | `example.com:8080` | `example.com:7891` |
| `https://example.com` | `[::1]:7891` | `https://example.com` | `example.com` | 443 | `::1` | 7891 | `example.com:443` | `[::1]:7891` |
| `http://127.0.0.1/proxy/` | `localhost:1` | `http://127.0.0.1/proxy` | `127.0.0.1` | 80 | `localhost` | 1 | `127.0.0.1:80` | `localhost:1` |
| `http://[::1]:8080` | `[2001:db8::1]:65535` | `http://[::1]:8080` | `::1` | 8080 | `2001:db8::1` | 65535 | `[::1]:8080` | `[2001:db8::1]:65535` |
| `http://example.com:80/proxy///` | `127.0.0.1:7891` | `http://example.com/proxy` | `example.com` | 80 | `127.0.0.1` | 7891 | `example.com:80` | `127.0.0.1:7891` |

`parse_rejects`：另一侧用合法值 `http://127.0.0.1:9` 或 `127.0.0.1:1`。断言 `err.field` 与 `err.message` 全字相等。

| 输入落在 | 值 | field | message |
| --- | --- | --- | --- |
| 网关 | `""` 与 `"   "` | Gateway | `MSG_EMPTY` |
| 网关 | `"http://10.20.30.61:8080\n"` | Gateway | `MSG_GRAPHIC` |
| 网关 | `"http://用户:8080"` | Gateway | `MSG_GRAPHIC` |
| 网关 | `"ftp://10.20.30.61"` | Gateway | `MSG_SCHEME` |
| 网关 | `"http://"` | Gateway | `MSG_URL` |
| 网关 | `"http://user:pass@10.20.30.61:8080"` | Gateway | `MSG_USERINFO` |
| 网关 | `"http://10.20.30.61:8080?q=1"` | Gateway | `MSG_QUERY` |
| 网关 | `"http://10.20.30.61:8080#frag"` | Gateway | `MSG_QUERY` |
| SOCKS5 | `""` | Socks5 | `MSG_EMPTY` |
| SOCKS5 | `"socks5://127.0.0.1:1"` | Socks5 | `MSG_SOCKS_SCHEME` |
| SOCKS5 | `"127.0.0.1"`、`"[::1]"`、`"::1:7891"` | Socks5 | `MSG_SOCKS_FORM` |
| SOCKS5 | `"127.0.0.1:0"`、`"127.0.0.1:65536"`、`"127.0.0.1:abc"` | Socks5 | `MSG_SOCKS_PORT` |
| SOCKS5 | `"127.0.0.1:1\n"` | Socks5 | `MSG_GRAPHIC` |

`default_values_match_consts`：`default_values()` 与上表第一行的五个字段相等。

`resolve_uses_defaults_when_state_fields_absent`：`HelperState::default()` 加 `default_values()` 得到与默认值相等的 `Endpoints`。

`resolve_overrides_only_the_present_field`：state 只设 `gateway_base_url = Some("https://gw.example/proxy/")`，SOCKS5 仍是默认 `10.20.30.61:7891`，网关 URL 为 `https://gw.example/proxy`，端口 443。再测只设 `socks5_addr = Some("[::1]:9")`，网关仍是默认 URL，SOCKS5 主机 `::1`、端口 9。

`resolve_falls_back_and_logs_when_one_field_is_invalid`：`log::init` 到 `tempfile::tempdir()`。state 的网关设为 `Some("ftp://bad")`，SOCKS5 设为 `Some("127.0.0.1:9")`。`resolve` 的网关字段等于 `defaults`，SOCKS5 是 `127.0.0.1:9`。读日志文件，断言含 `endpoints.fallback` 与 `"field":"gateway"`（JSON 紧凑或带空格都用 `contains("endpoints.fallback")` 与 `contains("gateway")`）。再把 SOCKS5 设成 `Some("nope")`、网关设成合法自定义 URL，断言网关保留自定义值、SOCKS5 回落默认，日志含 `socks5`。

`old_state_json_without_endpoint_fields_deserializes_to_none`（放在 `state.rs` 的 tests）：把 `{"enabled": true}` 写进临时文件，`load` 后 `gateway_base_url` 与 `socks5_addr` 都是 `None`，`enabled` 仍为 `true`。

`endpoint_fields_round_trip_as_snake_case`：保存一个两项都是 `Some` 的 state，读回相等；文件文本含 `"gateway_base_url"` 与 `"socks5_addr"`，不含驼峰名。

- [ ] **Step 2: 跑红**

Run: `cargo test -p helper-core --lib endpoints::`

Expected: FAIL，编译错误含 `file not found for module` 或 `cannot find module`（`lib.rs` 还没有 `mod endpoints`）。若先把空模块加进 `lib.rs` 再跑，失败信息应是 `cannot find function` / `cannot find struct` 一类，不能是 PASS。

- [ ] **Step 3: 实现**

按上面的规则实现 `endpoints.rs`。`parse` 先解析网关，再解析 SOCKS5，返回第一个错误。

`consts.rs` 模块头改为：“默认地址。用户可在桌面界面修改网关与上游 SOCKS5，见 `docs/superpowers/specs/2026-11-16-configurable-endpoints-design.md`；未自定义时使用这里的值。”五个常量的文档注释各加一句“默认值”。数值不变。

`state.rs` 增加两个 `Option<String>` 字段。`save_then_load_round_trips` 的结构体字面量补上 `gateway_base_url: None` 与 `socks5_addr: None`（该字面量没有 `..Default`，漏字段会编译失败）。其余已使用 `..Default::default()` 的测试不用改。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p helper-core --lib endpoints:: && cargo test -p helper-core --lib state::`

Expected: PASS。`endpoints::` 与 `state::` 里原有测试仍然通过。

Run: `cargo clippy -p helper-core --all-targets -- -D warnings`

Expected: PASS。不要为新类型加 `allow(dead_code)`；本任务的公开方法都被测试调用到。

- [ ] **Step 5: 提交**

```bash
git add crates/helper-core/src/endpoints.rs crates/helper-core/src/lib.rs crates/helper-core/src/consts.rs crates/helper-core/src/state.rs
git commit -F - <<'EOF'
feat(core): 增加端点值对象与 state 中的网关、SOCKS5 字段

旧状态文件没有这两个字段时反序列化为 None，自定义值留到后续任务再写入配置。

Co-Authored-By: Claude Code
EOF
```

---

### Task 2: 下游函数改为显式参数

**文件：**

- 修改 `crates/helper-core/src/codex_config.rs`
- 修改 `crates/helper-core/src/codex_env.rs`
- 修改 `crates/helper-core/src/rules.rs`
- 修改调用点：`crates/helper-core/src/manager.rs`、`crates/helper-core/src/proxy.rs`、`crates/helper-core/src/component_downloads.rs`、`crates/helper-core/tests/codex_config.rs`、`crates/helper-core/tests/manager.rs`、`apps/credential/src/main.rs`、`apps/credential/tests/cli.rs`

**接口：**

- 消耗 Task 1 的常量注释，本任务仍把 consts 的值当作“当前唯一传入值”。
- 产出：

```rust
pub fn inspect(config_path: &Path, credential_command: &str, gateway_base_url: &str) -> Result<ConfigInspection, ConfigError>;
pub fn apply_managed(config_path: &Path, backup_path: &Path, credential_command: &str, gateway_base_url: &str) -> Result<ApplyOutcome, ConfigError>;
fn ensure_managed(doc: &mut DocumentMut, credential_command: &str, gateway_base_url: &str) -> Result<bool, ConfigError>;

pub fn render_block(proxy_port: u16, gateway_host: &str) -> String;
pub fn upsert_block(existing: &str, proxy_port: u16, gateway_host: &str) -> String;
pub fn inspect_text(existing: &str, proxy_port: u16, gateway_host: &str) -> EnvInspection;
pub fn inspect_file(env_path: &Path, proxy_port: u16, gateway_host: &str) -> std::io::Result<EnvInspection>;
pub fn write_block_to_file(env_path: &Path, proxy_port: u16, gateway_host: &str) -> std::io::Result<bool>;

pub fn route_for_host(host: &str, always_direct: &[String]) -> Route;
```

- `block_lines(proxy_port, gateway_host)` 把 `NO_PROXY` 写成 `format!("NO_PROXY={gateway_host},127.0.0.1,localhost")`，不再读 `consts::GATEWAY_HOST`。
- `ensure_managed` 把 `base_url` 写成参数 `gateway_base_url`，不再读 `consts::GATEWAY_BASE_URL`。
- `is_always_direct(host, always_direct)`：`always_direct` 里任一元素经 `normalize_host` 后与已规范化的 `host` 相等，或 `host` 是 `localhost` / `127.0.0.1` / `::1`。删除对 `GATEWAY_HOST` / `SOCKS5_HOST` 的比较。
- 本任务的调用点全部传入常量，行为与改签名之前一致：
  - `manager.rs` 的 `inspect`、`apply_managed` 传入 `consts::GATEWAY_BASE_URL`。
  - `manager.rs` 的 `inspect_file`、`write_block_to_file` 传入 `consts::GATEWAY_HOST`。Key 校验仍用 `self.options.gateway_base_url`，本任务不改那一行。
  - `proxy.rs` 的 `route_for_host(&target.host, &always)`，其中 `always` 是 `[consts::GATEWAY_HOST.to_string(), consts::SOCKS5_HOST.to_string()]`。这是过渡，Task 3 换成 `Routing` 快照。
  - `codex_config.rs` 内部测试、`tests/codex_config.rs`、`Fixture::apply` 传入 `consts::GATEWAY_BASE_URL`。
  - `codex_env.rs` 内部测试、`tests/manager.rs`、`apps/credential` 的 `render_block(port)` 改为 `render_block(port, consts::GATEWAY_HOST)`。期望字符串里的 `NO_PROXY=10.20.30.61,...` 保持不变。
  - `rules.rs` 与 `component_downloads.rs` 里现有 `route_for_host(host)` 改为 `route_for_host(host, &[])`，回环用例因此仍然是 `Direct`。

- [ ] **Step 1: 写失败测试**

`codex_env.rs`：

`render_block_uses_supplied_gateway_host`：`render_block(17891, "gw.example")` 含 `NO_PROXY=gw.example,127.0.0.1,localhost`，且不含 `10.20.30.61`。四个代理变量仍指向 `http://127.0.0.1:17891`。

`mismatched_gateway_host_is_not_up_to_date_and_upsert_rewrites_it`：原文是 `render_block(17891, "10.20.30.61")`。`inspect_text(&text, 17891, "other.example")` 的 `has_block` 为真、`block_up_to_date` 为假。`upsert_block(&text, 17891, "other.example")` 含 `NO_PROXY=other.example,127.0.0.1,localhost`，不再含 `10.20.30.61`，用户行之外的标记仍然成对。

`codex_config.rs`：

`apply_rewrites_base_url_and_inspect_follows_the_same_url`：夹具内容为已受管、但 `base_url = "http://10.20.30.61:8080"`。`apply_managed(..., "http://127.0.0.1:9")` 返回 `changed == true`，读回的 `base_url` 是 `http://127.0.0.1:9`。`inspect(..., "http://10.20.30.61:8080")` 的 `fully_managed` 为假；`inspect(..., "http://127.0.0.1:9")` 的 `fully_managed` 为真。再对同一文件 `apply_managed(..., "http://127.0.0.1:9")`，第二次 `changed == false`。

`rules.rs`：把 `gateway_ip_and_loopback_always_direct` 换成：

`loopback_stays_direct_without_configured_hosts`：`route_for_host("localhost" | "127.0.0.1" | "::1", &[])` 都是 `Direct`。

`custom_host_is_direct_only_when_listed`：`route_for_host("api.openai.com", &["api.openai.com".into()]) == Direct`；`route_for_host("API.OpenAI.com.", &["api.openai.com".into()]) == Direct`（大小写与末尾点靠 `normalize_host`）；`route_for_host("api.openai.com", &[]) == Socks5`；`route_for_host("10.20.30.61", &[]) == Direct`（未命中规则，不是函数里的特判）。

- [ ] **Step 2: 跑红**

Run: `cargo test -p helper-core --lib -- codex_env::tests::render_block_uses_supplied_gateway_host codex_env::tests::mismatched_gateway_host_is_not_up_to_date_and_upsert_rewrites_it codex_config::tests::apply_rewrites_base_url_and_inspect_follows_the_same_url rules::tests::custom_host_is_direct_only_when_listed`

（`--lib` 只能出现一次；多个测试名过滤放在 `--` 之后。）

Expected: FAIL，编译错误指向这些新测试调用的参数个数（函数仍是旧签名）。不要在红之前改生产代码。

- [ ] **Step 3: 实现并改完全部调用点**

按接口改三个函数族。`codex_env.rs` 可以去掉 `use crate::consts`，如果文件里再没有别的 consts 引用。`rules.rs` 同样：生产路径不再 `use crate::consts`。

调用点替换是机械的，期望文本不要改：

- `render_block(X)` → `render_block(X, consts::GATEWAY_HOST)` 或 `helper_core::consts::GATEWAY_HOST`（credential crate）。
- `upsert_block(a, p)` → `upsert_block(a, p, consts::GATEWAY_HOST)`。
- `inspect_text(a, p)` / `inspect_file(path, p)` / `write_block_to_file(path, p)` 同样加第三个参数 `consts::GATEWAY_HOST`。`manager.rs` 里这三处用 `consts::GATEWAY_HOST`，不要改成 `options.gateway_host`。
- `inspect(path, cmd)` → `inspect(path, cmd, consts::GATEWAY_BASE_URL)`。
- `apply_managed(path, backup, cmd)` → `apply_managed(path, backup, cmd, consts::GATEWAY_BASE_URL)`。`manager.rs` 同样传常量，不要传 `options.gateway_base_url`。
- `route_for_host(h)` → `route_for_host(h, &[])`，只有 `proxy.rs` 那一处传两个常量主机。

`Fixture::apply` 内部补上 `consts::GATEWAY_BASE_URL`，这样 `codex_config.rs` 里几十个 `fixture.apply(COMMAND)` 不用逐个改参数。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p helper-core --lib codex_env:: && cargo test -p helper-core --lib codex_config:: && cargo test -p helper-core --lib rules:: && cargo test -p helper-core --lib component_downloads:: && cargo test -p helper-core --test codex_config && cargo test -p codex-helper-credential`

Expected: PASS。`renders_block_matches_design_doc_literally` 仍然与原来的 6 行文本相等。`component_downloads` 里 GitHub 资源主机仍是 `Socks5`。

Run: `cargo clippy -p helper-core --all-targets -- -D warnings && cargo clippy -p codex-helper-credential --all-targets -- -D warnings`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/helper-core/src/codex_config.rs crates/helper-core/src/codex_env.rs crates/helper-core/src/rules.rs crates/helper-core/src/manager.rs crates/helper-core/src/proxy.rs crates/helper-core/src/component_downloads.rs crates/helper-core/tests/codex_config.rs crates/helper-core/tests/manager.rs apps/credential/src/main.rs apps/credential/tests/cli.rs
git commit -F - <<'EOF'
feat(core): 配置、环境块与分流改为接收显式地址

调用点仍传入 consts 默认值，自定义地址的接线留到 Manager。

Co-Authored-By: Claude Code
EOF
```

---

### Task 3: 代理 Routing 热更新

**文件：**

- 修改 `crates/helper-core/src/proxy.rs`
- 修改 `crates/helper-core/tests/proxy.rs`
- 修改 `crates/helper-core/tests/upstream_tls.rs`
- 修改 `crates/helper-core/src/manager.rs` 的 `proxy_config`（只改填法，让编译通过；真正改成运行时端点是 Task 4）

**接口：**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub socks5_host: String,
    pub socks5_port: u16,
    /// 已规范化的恒直连主机：网关主机在前，SOCKS5 主机在后；两者相同时只保留一份
    pub always_direct_hosts: Vec<String>,
}

impl Routing {
    pub fn from_endpoints(endpoints: &crate::endpoints::Endpoints) -> Self;
}
```

`ProxyConfig` 删除 `socks5_host` 与 `socks5_port`，新增 `pub routing: Arc<std::sync::RwLock<Routing>>`。`Default` 用 `Routing::from_endpoints(&Endpoints::default_values())`。`Debug` 打印 `socks5_host`、`socks5_port` 与 `always_direct_hosts`，不打印锁本身。安全上游的注释改为“失败即 502，绝不回退 `routing` 里的无认证 SOCKS5”。

`handle_connection` 在读完请求头、通过门禁、解析出 target 之后、调用 `route_for_host` 之前：

```rust
let routing = shared
    .config
    .routing
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
```

随后立刻结束守卫。`route_for_host(&target.host, &routing.always_direct_hosts)` 与 `connect_upstream` 都只用这份本地 `routing`。`connect_upstream` 的签名改为接收 `&Routing`，无认证分支使用 `routing.socks5_host` / `routing.socks5_port`。不要把 `RwLockReadGuard` 持有到任何 `.await`。

`from_endpoints`：`socks5_host/port` 取端点字段；`always_direct_hosts` 先放 `gateway_host`，若 `socks5_host` 与它不等再追加。

`manager.rs` 的 `proxy_config` 在本任务改为：

```rust
let endpoints = crate::endpoints::Endpoints::parse(
    &self.options.gateway_base_url,
    &format!("{}:{}", self.options.socks5_host, self.options.socks5_port),
).expect("测试与生产注入的地址都必须能被解析");
ProxyConfig {
    routing: Arc::new(std::sync::RwLock::new(Routing::from_endpoints(&endpoints))),
    on_record: Some(sink),
    direct_overrides: None,
    secure_upstream: None,
    gate: None,
}
```

这仍读取 `ManagerOptions` 的旧字段。Task 4 会把这五个字段收成 `default_endpoints`，并让进程级 `Arc` 被热更新。本任务的 `Arc` 每次 `proxy_config()` 新建，热更新能力先在 `proxy` 集成测试里证明。

测试字面量替换（`tests/proxy.rs` 的 `spawn_proxy` 与 `proxy_config`，以及 `tests/upstream_tls.rs` 的 `spawn_proxy`）：

```rust
let mut routing = Routing::from_endpoints(&crate::endpoints::Endpoints::default_values());
routing.socks5_host = host.to_string();
routing.socks5_port = port;
ProxyConfig {
    routing: Arc::new(std::sync::RwLock::new(routing)),
    ..ProxyConfig::default()
}
```

`..Default` 会先做一份默认 `Arc`，再被字段覆盖，这是要的：测试必须拿住自己放进 `ProxyConfig` 的那份 `Arc` 才能热更新。`upstream_tls` 里诱饵地址同样写进 `routing`，`secure_upstream` 保持 `Some`。

- [ ] **Step 1: 写失败测试**

在 `tests/proxy.rs` 增加 `routing_update_sends_new_connections_to_the_new_socks5`（`#[tokio::test]`）：

1. `spawn_fake_socks5()` 两次，得到 `first` / `seen1` 与 `second` / `seen2`。
2. 用 `proxy_config(&first)` 通过 `spawn_with_listener` 启动，记下 `handle.port()`。
3. 向该端口发 `CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n`，读到 `200`。断言 `seen1` 含 `chatgpt.com:443`，`seen2` 为空。
4. 取出 `config.routing` 的 `Arc` 克隆（测试里在 `proxy_config` 返回前无法取出的话，把辅助函数改成同时返回 `Arc<RwLock<Routing>>`）。写入 `second` 的主机与端口。不要 `shutdown`。
5. 再发一次同样的 CONNECT。断言 `seen2` 含 `chatgpt.com:443`，`seen1` 的长度仍是 1。`handle.port()` 与步骤 2 相同。
6. `handle.shutdown().await`。

这一步先把测试写上，但 `ProxyConfig` 还没有 `routing` 字段，所以编译失败。

- [ ] **Step 2: 跑红**

Run: `cargo test -p helper-core --test proxy routing_update_sends_new_connections_to_the_new_socks5`

Expected: FAIL，编译错误含 `no field routing` 或 `cannot find struct Routing`。

- [ ] **Step 3: 实现**

按接口改 `proxy.rs`、两处集成测试的 `ProxyConfig` 字面量，以及 `manager.rs` 的 `proxy_config`。`handle_connection` 里删除原来的 `rules::route_for_host(&target.host)` 单参数调用。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p helper-core --test proxy && cargo test -p helper-core --test upstream_tls && cargo test -p helper-core --test manager`

Expected: PASS。原有“命中走 SOCKS5、未命中直连、上游失败 502”仍然通过。`manager` 集成测试仍通过，因为 `proxy_config` 继续使用沙箱注入的 `127.0.0.1` 与无人监听端口。

Run: `cargo clippy -p helper-core --all-targets -- -D warnings`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/helper-core/src/proxy.rs crates/helper-core/src/manager.rs crates/helper-core/tests/proxy.rs crates/helper-core/tests/upstream_tls.rs
git commit -F - <<'EOF'
feat(core): 本地代理的上游地址改为可热更新的 Routing

新连接在分流前拷贝快照，已建立的隧道不迁移。

Co-Authored-By: Claude Code
EOF
```

---

### Task 4: Manager::set_endpoints 与 Key 校验警告

**文件：**

- 修改 `crates/helper-core/src/types.rs`
- 修改 `crates/helper-core/src/manager.rs`
- 修改 `crates/helper-core/src/gateway.rs`（删除 `verify_key`）
- 修改 `crates/helper-core/tests/manager.rs`
- 本任务起，Task 2 里 manager 对 `inspect` / `apply_managed` / `inspect_file` / `write_block_to_file` 传 consts 的那几处，改为传 `runtime.endpoints` 的 `gateway_base_url` 或 `gateway_host`

**接口：**

`ManagerOptions` 删除 `socks5_host`、`socks5_port`、`gateway_base_url`、`gateway_host`、`gateway_port`，改为：

```rust
/// 默认地址。生产为 `Endpoints::default_values()`；测试注入假网关与假 SOCKS5。
pub default_endpoints: crate::endpoints::Endpoints,
```

`Default` 与 `production` 都设 `default_endpoints: Endpoints::default_values()`。

`Manager` 增加 `routing: Arc<RwLock<Routing>>`。`Runtime` 去掉 `Copy`，增加 `endpoints: Endpoints`。`new` 的做法：

1. `read_state(&options.paths.state_file())`。`Err`、`corrupt` 或文件缺失时用 `HelperState::default()`。`new` 不调用 `quarantine_corrupt_state`。
2. `endpoints = Endpoints::resolve(&state, &options.default_endpoints)`。
3. `routing = Arc::new(RwLock::new(Routing::from_endpoints(&endpoints)))`。
4. `runtime.endpoints = endpoints`。其余运行时字段仍是默认值。

`reconcile_on_startup` 在现有 `sync_mode_from_state()` 之后调用新的 `sync_endpoints_from_state()`：再读一次 state，`resolve`，写入 `runtime.endpoints` 与 `self.routing`。读失败时保持 `new` 已经设好的值。

`proxy_config` 不再 `parse` options，也不再新建 `Arc`。它只克隆 `self.routing` 放进 `ProxyConfig`。因此进程生命周期内只有一把锁，保存地址与下一次 `spawn` 看到的是同一份。

`build_status`：

```rust
let runtime = self.runtime().clone();
gateway_addr: runtime.endpoints.gateway_addr(),
socks5_addr: runtime.endpoints.socks5_addr(),
gateway_base_url: runtime.endpoints.gateway_base_url.clone(),
endpoints_customized: state.gateway_base_url.is_some() || state.socks5_addr.is_some(),
```

`endpoints_customized` 看 state 里有没有 `Some`，不看运行时值是否等于默认值。损坏字符串被 `resolve` 回落后，按钮仍然可点，用户能恢复默认。

`check_new_key`、`probe_reachability` 改为读 `runtime.endpoints`（`gateway_base_url`，以及 `gateway_host/port`、`socks5_host/port`）。`probe_reachability` 仍探测两边；Task 5 才让关闭的 SOCKS5 变成 `Disabled`。

`enable_steps` 与 `reconcile_locked` 的 `apply_managed` 传入当时的 `runtime.endpoints.gateway_base_url`；`write_block_to_file` / `inspect_file` 传入 `runtime.endpoints.gateway_host`。传入前先把 `endpoints` 克隆到本地变量，避免短锁跨 `await`。

`types.rs` 增加（serde `rename_all = "camelCase"`；警告是无字段枚举，序列化成字符串 `"keyRejected"` 等）：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SetEndpointsRequest {
    pub gateway_base_url: Option<String>,
    pub socks5_addr: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndpointWarning {
    KeyRejected,
    KeyCheckUnavailable,
    ExternalProxy,
    RestartCodex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEndpointsOutcome {
    pub status: Status,
    pub warnings: Vec<EndpointWarning>,
}
```

`Status` 增加 `pub gateway_base_url: String` 与 `pub endpoints_customized: bool`。`ErrorPayload` 增加 `#[serde(skip_serializing_if = "Option::is_none")] pub field: Option<String>`。`ManagerError` 增加：

```rust
#[error("{message}")]
InvalidEndpoint { field: &'static str, message: String },
```

`code()` 返回 `"invalidEndpoint"`。`payload()` 的 `field` 仅这个变体为 `Some(field.to_string())`，其余 `None`。`dto_fields_serialize_as_camel_case` 补上 `gatewayBaseUrl` 与 `endpointsCustomized` 键存在；构造 `ErrorPayload` 的现有测试字面量补 `field: None`，否则编译失败。

```rust
impl Manager {
    pub async fn set_endpoints(&self, req: SetEndpointsRequest) -> Result<SetEndpointsOutcome, ManagerError>;
}
```

锁内步骤（先 `self.ops.lock().await`）：

1. `None` 的一侧用 `options.default_endpoints` 的 `gateway_base_url` 或 `socks5_addr()`；`Some` 用用户字符串。`Endpoints::parse` 失败则 `ManagerError::InvalidEndpoint { field: error.field.as_str(), message: error.message }`，不写文件、不改运行时。
2. 五个字段与 `runtime.endpoints` 全相同：直接 `Ok(SetEndpointsOutcome { status: self.build_status(), warnings: vec![] })`。
3. 读出当前 state（`read_state` + 损坏则隔离，沿用启用路径的做法）。规范化后的网关 URL 等于 `default_endpoints.gateway_base_url` 则 state 字段写 `None`，否则 `Some(规范化 URL)`。SOCKS5 用 `socks5_addr()` 与 `default_endpoints.socks5_addr()` 比较，相等写 `None`，否则 `Some(展示串)`。`save_state`。更新 `runtime.endpoints`。然后写 `self.routing`（`Routing::from_endpoints`），不论代理是否在跑；代理没在跑时，下次 `proxy_config` 克隆的仍是这把锁。
4. 若 `state.enabled`：调用与 `reconcile_locked` 相同的 `apply_managed`（含备份）和 `write_block_to_file`。本任务代理开关还不存在，启用时仍然写 `.env`。这一步返回错误时 state 已经是新值，把错误原样返回，不回滚 state，不继续做步骤 5 和锁外步骤。
5. 在仍持有 ops 锁时调用现有可达性探测逻辑（与 `probe_reachability` 共用一个私有 async 函数，避免两份主机读取）。探测只在写 `runtime.gateway/socks5` 时拿短锁。

释放 ops 锁之后：

6. 记下进入步骤 3 之前的旧 `gateway_base_url` 与旧 SOCKS5 展示串。网关 URL 变了且 `credentials.read()` 为 `Ok(Some(secret))` 时，`verify_key_at(&new.gateway_base_url, secret.expose(), KEY_CHECK_TIMEOUT)`。`Unauthorized` → 警告 `KeyRejected`；`Ok` 不警告；`ServerError` 与 `TimeoutOrNetwork` → `KeyCheckUnavailable`。凭据读取失败同样算 `KeyCheckUnavailable`，不把存储错误变成 `Err`（保存已经完成）。没有 Key 或网关 URL 没变：不做请求。
7. 网关 URL 变了且 `status.enabled`：追加 `RestartCodex`。代理状态是 `External` 且 SOCKS5 展示串变了：追加 `ExternalProxy`。顺序固定为：Key 警告（若有）、`RestartCodex`（若有）、`ExternalProxy`（若有）。

日志：成功路径 `log::event("manager.set_endpoints", json!({ "from_gateway": old_url, "to_gateway": new_url, "from_socks5": old_socks, "to_socks5": new_socks }))`。失败路径记 `code`。两处 detail 都不放 Key。删除 `gateway::verify_key`。

`manager_futures_are_send` 增加 `assert_send(&manager.set_endpoints(SetEndpointsRequest::default()))`。

`production_options_use_fixed_parameters` 改为断言 `options.default_endpoints == Endpoints::default_values()`，删掉五个旧字段断言。

`Sandbox::options_with` 与 `manager_futures_are_send` 里的 `ManagerOptions` 字面量改为：

```rust
default_endpoints: Endpoints::parse(
    &self.gateway_url,
    &format!("127.0.0.1:{}", self.socks5_port),
).expect("沙箱地址可解析"),
```

`with_dead_gateway` 仍改 `gateway_url`，必须在 `options_with` 读 `gateway_url` 之前改完（现有顺序已经如此）。

启用成功后的文件断言（`tests/manager.rs` 里比较 `base_url` 与 `render_block` 的地方，包括 `simulate_interrupted_enable` 写进去的“已经受管”夹具）改为沙箱默认端点：

- `base_url` 期望值是 `sandbox.gateway_url`（wiremock 的 `uri()`，无末尾斜杠）。
- `.env` 期望值是 `codex_env::render_block(port, "127.0.0.1")`。`NO_PROXY` 因此是 `127.0.0.1,127.0.0.1,localhost`，这是主机参数的直接结果，不要再手写一份期望串。
- 凡是测试自己先写入“已是最新受管块”再启用/对账、并断言没有改写的夹具，主机也用 `"127.0.0.1"`，否则对账会把块当成过期。

`status` 快照测试里 `gateway_addr` / `socks5_addr` 的期望已经是 `127.0.0.1:端口`，与新的展示函数一致，保持断言，并补上 `gateway_base_url == sandbox.gateway_url`、`endpoints_customized == false`。

- [ ] **Step 1: 写失败测试**

在 `tests/manager.rs` 增加下面的测试。它们调用还不存在的 `set_endpoints`，所以下一步编译失败。

`set_endpoints_while_enabled_rewrites_config_and_env_and_warns_restart`：

1. `Sandbox::new`，写入 `USER_CONFIG`，`enable(with_key(KEY))` 成功。
2. 记录 `status.proxy.port` 与 `proxy.state == Running`。
3. `set_endpoints(SetEndpointsRequest { gateway_base_url: Some("http://127.0.0.1:9/proxy/".into()), socks5_addr: Some("127.0.0.1:1".into()) })` 返回 `Ok`。
4. 断言 `base_url == "http://127.0.0.1:9/proxy"`；`.env` 含 `NO_PROXY=127.0.0.1,127.0.0.1,localhost`；`outcome.status.gateway_base_url` 为该 URL；`gateway_addr == "127.0.0.1:9"`；`socks5_addr == "127.0.0.1:1"`；`endpoints_customized` 为真；`warnings == [KeyCheckUnavailable, RestartCodex]`（`127.0.0.1:9` 连不上，已存 Key 的校验失败，但保存成功；Key 警告在 `RestartCodex` 之前）。
5. state 的 `gateway_base_url` 是 `Some("http://127.0.0.1:9/proxy")`，`socks5_addr` 是 `Some("127.0.0.1:1")`。
6. `proxy.port` 不变，`proxy.state` 仍是 `Running`。
7. 读 `log::log_file_path()` 的内容，含 `http://127.0.0.1:9/proxy`，不含测试 Key 字符串。

`set_endpoints_hot_swaps_socks5_without_rebind`：

1. 在测试文件内复制一份 `tests/proxy.rs` 的 `spawn_fake_socks5`（约 30 行，无认证握手并记录 CONNECT 目标）。起两个。
2. 启用受管模式（代理已在 `sandbox.port` 监听，上游仍是沙箱那个无人端口）。
3. `set_endpoints`，网关保持 `Some(sandbox.gateway_url.clone())`，SOCKS5 设为第一个假上游的 `host:port`。
4. 向 `127.0.0.1:sandbox.port` 发 `CONNECT chatgpt.com:443`。第一个假上游的 seen 含 `chatgpt.com:443`。
5. 再 `set_endpoints` 到第二个假上游。`status.proxy.port` 仍是 `sandbox.port`，`state` 仍是 `Running`。再发一次 CONNECT。第二个 seen 含该目标，第一个 seen 的长度仍是 1。
6. 警告含 `ExternalProxy` 为假（这是本进程代理）。网关 URL 没变，所以没有 `RestartCodex`，也没有 Key 警告。

`set_endpoints_key_rejected_still_saves`：新的 wiremock 对 `GET /v1/models` 返回 401。启用后 `set_endpoints` 把网关指到它，SOCKS5 保持沙箱当前展示串。`warnings == [KeyRejected, RestartCodex]`，不含 `KeyCheckUnavailable`。`outcome.status.gateway_base_url` 是新 mock 的 `uri()`。state 已写入。

`set_endpoints_invalid_input_changes_nothing`：启用后给 `snapshot()`。`set_endpoints` 的网关为 `Some("ftp://bad".into())`，SOCKS5 为 `Some(sandbox 当前 socks5 展示串)`。错误 `code() == "invalidEndpoint"`，`payload().field == Some("gateway")`，`message == endpoints::MSG_SCHEME`。再测 SOCKS5 `Some("nope".into())` 时期望 `field == Some("socks5")` 且 message 为 `MSG_SOCKS_FORM`。两次之后 `snapshot()` 与调用前相等。

`set_endpoints_restore_default_clears_state_fields`：先写成自定义地址，再 `SetEndpointsRequest { gateway_base_url: None, socks5_addr: None }`。state 两个字段都是 `None`。`endpoints_customized` 为假。`status.gateway_base_url == sandbox.gateway_url`（沙箱默认值，不是 consts）。`config.toml` 的 `base_url` 回到 `sandbox.gateway_url`。

`set_endpoints_survives_manager_restart`：写入自定义网关与 SOCKS5，`shutdown`，用同一 `options_with` 再 `Manager::new`。`status()` 的 `gateway_base_url` 与 `socks5_addr` 是刚写入的规范化值，不需要先 `reconcile_on_startup`。

`set_endpoints_while_disabled_only_writes_state`：不启用。先写入一份用户 `config.toml` 与 `.env`。`set_endpoints` 成功，`warnings` 为空，`enabled` 为假。config 与 `.env` 字节不变。state 两个字段是 `Some`。

`set_endpoints_same_value_is_noop`：不启用、不写 state。`set_endpoints` 传入沙箱默认 URL 与默认 SOCKS5 展示串。返回的 `warnings` 为空，`endpoints_customized` 为假，state 文件仍然不存在。

`set_endpoints_keeps_new_state_when_config_apply_fails`：先启用成功，再把 `config.toml` 改成 `model_providers = 1\n`。`set_endpoints` 到一个合法新 URL 返回 `Err`，`code() == "configInvalid"`。随后 `status().gateway_base_url` 是新 URL，`config_error` 为 `Some`。state 文件里的 `gateway_base_url` 是 `Some(新 URL)`。`config.toml` 内容仍是 `model_providers = 1\n`。

- [ ] **Step 2: 跑红**

Run: `cargo test -p helper-core --test manager set_endpoints`

Expected: FAIL，编译错误含 `cannot find method set_endpoints` 或 `no field default_endpoints`（测试先改了构造，生产结构体还是旧字段时也算红）。不要在看到 PASS 之前写 `set_endpoints` 函数体。

- [ ] **Step 3: 实现**

按接口修改 `types.rs`、`manager.rs`，删除 `gateway.rs` 的 `verify_key`。把 Task 2 留在 manager 里的 consts 参数换成 `runtime.endpoints` 的对应字段。`enable` / `save_key` 的 Key 校验主机就是这个字段，满足设计 §3.5。

`inspect_config` 与 `build_status` 里的 `codex_config::inspect` 同样传入当前 `gateway_base_url`，否则 `config_managed` 会在自定义地址下永远为假。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p helper-core --test manager && cargo test -p helper-core --lib types:: && cargo test -p helper-core --lib gateway::`

Expected: PASS。`gateway::` 里原有 `verify_key_at` 测试仍在，没有对 `verify_key` 的引用。启用测试的 `base_url` 现在等于 wiremock URL，`env_managed` 仍为真。

Run: `cargo clippy -p helper-core --all-targets -- -D warnings`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/helper-core/src/types.rs crates/helper-core/src/manager.rs crates/helper-core/src/gateway.rs crates/helper-core/tests/manager.rs
git commit -F - <<'EOF'
feat(core): 保存端点时校正受管配置并在锁外校验 Key

非法输入不落盘；配置校正失败时保留已写入的 state，Key 失败只产生警告。

Co-Authored-By: Claude Code
EOF
```

---

### Task 5: 代理开关、对账、Secure 互斥与可达性

**文件：**

- 修改 `crates/helper-core/src/state.rs`（`proxy_disabled`）
- 修改 `crates/helper-core/src/types.rs`（`Reachability::Disabled`、`Status.proxy_enabled`、`ManagerError::SecureRequiresProxy`）
- 修改 `crates/helper-core/src/manager.rs`
- 修改 `crates/helper-core/src/component_updates.rs`
- 修改 `crates/helper-core/src/component_downloads.rs` 的测试（只加一条拒绝断言，不改 `download_client`）
- 修改 `crates/helper-core/tests/manager.rs`

**接口：**

```rust
/// 用户关闭了本地代理（缺省 false = 开启）
pub proxy_disabled: bool,
```

放在 `HelperState` 末尾。否定式字段，serde 默认 `false` 就是“开启”。

```rust
pub enum Reachability {
    Unknown,
    Reachable,
    Unreachable,
    /// 用户关闭了本地代理，不探测 SOCKS5
    Disabled,
}

// Status 新增
pub proxy_enabled: bool,

#[error("Secure 模式需要本地代理，请先停用 Secure")]
SecureRequiresProxy, // code() == "secureRequiresProxy"
```

`SetEndpointsOutcome` 被 `set_proxy_enabled` 复用，不新建结果类型。

```rust
impl Manager {
    pub async fn set_proxy_enabled(&self, enabled: bool) -> Result<SetEndpointsOutcome, ManagerError>;
}
```

`build_status`：`proxy_enabled: !state.proxy_disabled`。

`env_managed`：

```rust
env_managed: if state.enabled && state.proxy_disabled {
    !env.has_block
} else {
    env.block_up_to_date
},
```

`residue` 的公式不变：`!state.enabled && (config_residue || env.has_block)`。受管开启、代理关闭时 `enabled` 为真，残留灯本来就是灭的；对账负责删掉 `.env` 块，不把它当成需要“清理残留”按钮的状态。

期望矩阵（`enabled` 指受管开关，`proxy_disabled` 指代理开关）：

| enabled | proxy_disabled | config.toml | .env 受管块 | 本地代理 |
| --- | --- | --- | --- | --- |
| true | false | `apply_managed` | `write_block_to_file` | `ensure_proxy` |
| true | true | `apply_managed` | `remove_block_from_file` | 不调用 `ensure_proxy`；若本进程已有句柄则 `stop_proxy` |
| false | 任意 | 不改（停用路径仍走现有还原） | 停用路径仍删除 | 停用路径仍 `stop_proxy` |

受管关闭时 `set_proxy_enabled` 只写 `proxy_disabled`，不碰 `config.toml` / `.env`，不 `ensure_proxy`。`disable` 不改 `proxy_disabled`（偏好留下次启用用）。

`enable_steps`：在现有步骤 4（`apply_managed`）之后，若 `snapshot` 写完后的 state（或锁内重新读到的 state）`proxy_disabled` 为真：跳过 `ensure_proxy`，改为 `remove_block_from_file`；不要因为没启动代理而失败。为假：保持现在的先代理后 `.env`。进入 `enable_steps` 前，运行时端点已经在 `new` / `set_endpoints` 里设好，`apply_managed` 仍用 `runtime.endpoints.gateway_base_url`。

`reconcile_locked` 已启用分支同样按这个矩阵。代理关闭时不要因为端口上有 External 实例就去复用它。对账开始时 `sync_endpoints_from_state` 已在 Task 4 调用，本任务在它旁边把 `proxy_disabled` 也从 state 读进来（它只活在 state 里，`build_status` 每次读文件，内存不必再存一份）。

`set_proxy_enabled` 持有 ops 锁：

1. 读 state。`enabled == !state.proxy_disabled` 时直接返回当前 `status` 与空 `warnings`。
2. `!enabled` 且（`state.mode == Secure` 或 `runtime.mode == Secure`）：返回 `SecureRequiresProxy`，不写 state，不停止代理。
3. 写入 `proxy_disabled = !enabled`。
4. 若 `state.enabled`：`enabled` 为真则 `ensure_proxy` 再 `write_block_to_file`；为假则 `remove_block_from_file` 再 `stop_proxy`。`.env` 实际有改动（写或删返回 `true`）时记录 `env_changed`。
5. 刷新可达性（见下）。
6. 释放锁后组装警告：`state.enabled && env_changed` → `RestartCodex`。`!enabled` 且进入函数时 `proxy_state == External` → `ExternalProxy`（`.env` 仍然删除；`stop_proxy` 对 External 的现有行为是打日志并把内存状态置为 `Stopped`，沿用它）。

`activate_secure_locked` 在读注册包之前：若 `snapshot.state.proxy_disabled`，返回 `SecureRequiresProxy`。不读 bundle 文件，因此一个不存在的路径也必须是这个错误，而不是 `Io`。不改 `SecureUpstream` 的 `host` / `port` 赋值。

`probe_reachability` 与 `set_endpoints` / `set_proxy_enabled` 共用的探测函数：`proxy_disabled` 为真时不连接 SOCKS5，短锁里把 `runtime.socks5 = Reachability::Disabled`；网关仍按 TCP 结果写 `Reachable` / `Unreachable`。为假时两边都探测，与现在一致。

`network_path_for` 改成：

```rust
pub fn network_path_for(status: &Status) -> NetworkPath {
    if status.mode == ProxyMode::Secure {
        return match status.proxy.state {
            ProxyState::Running => NetworkPath::ViaProxy {
                proxy_url: format!("http://127.0.0.1:{}", status.proxy.port),
            },
            ProxyState::External => NetworkPath::Blocked { detail: BLOCKED_PORT_OCCUPIED },
            ProxyState::Stopped => NetworkPath::Blocked {
                detail: if status.secure.blocked_reason.as_deref() == Some("port_occupied") {
                    BLOCKED_PORT_OCCUPIED
                } else {
                    BLOCKED_NOT_RUNNING
                },
            },
        };
    }
    if !status.proxy_enabled {
        return NetworkPath::Direct { reason: Some("本地代理未开启，已直连") };
    }
    match status.proxy.state {
        ProxyState::Running => NetworkPath::ViaProxy {
            proxy_url: format!("http://127.0.0.1:{}", status.proxy.port),
        },
        ProxyState::External => NetworkPath::Direct { reason: Some(DIRECT_EXTERNAL) },
        ProxyState::Stopped => NetworkPath::Direct { reason: Some(DIRECT_STOPPED) },
    }
}
```

同文件的 `fn status(...)` 辅助函数加上 `proxy_enabled: true`。现有 Secure / Legacy 用例因此保持原期望。

`component_downloads.rs` 测试新增 `disabled_proxy_reason_is_still_rejected`：`download_client(&NetworkPath::Direct { reason: Some("本地代理未开启，已直连") })` 的错误 `code == "downloadProxy"`，`message` 含 `请先启用本地代理` 且含 `不会回退直连`。

`manager_futures_are_send` 增加 `set_proxy_enabled(false)`。

- [ ] **Step 1: 写失败测试**

`state.rs`：`old_state_json_defaults_proxy_disabled_to_false`，内容 `{"enabled": true}`，断言 `proxy_disabled == false`。`proxy_disabled_round_trips`：保存 `proxy_disabled: true`，读回为真，文本含 `"proxy_disabled": true`。`save_then_load_round_trips` 的全字段字面量补 `proxy_disabled: false`。

`component_updates.rs`：`proxy_switch_off_is_direct_with_its_own_reason`。`status(Legacy, Stopped, 17891)` 先断言原因是 `本地代理未运行，已直连`（辅助函数把 `proxy_enabled` 设成真）。再把 `proxy_enabled` 设为假，断言原因是 `本地代理未开启，已直连`，且与上一句不相等。`status(Secure, Stopped, 17891)` 在 `proxy_enabled = false` 时仍然是 `Blocked`，`detail` 为 `Secure 代理未就绪（代理未运行）`。

`types.rs`：`reachability_disabled_serializes_as_disabled`，`Reachability::Disabled` 的 JSON 是 `"disabled"`。`secure_requires_proxy_has_stable_code`：`code() == "secureRequiresProxy"`，`to_string()` 全字等于 `Secure 模式需要本地代理，请先停用 Secure`。

`tests/manager.rs`（都用现有 `Sandbox`；Secure 两条用现有 `secure_sandbox` 与 `setup_secure_enrollment`）：

`disable_proxy_while_managed_removes_env_and_frees_port`：启用成功后 `set_proxy_enabled(false)`。`.env` 不再含 `BEGIN_MARKER`。用户行若启用前有，仍在。`config.toml` 的 `model_provider` 仍是 `managed_gateway`，`base_url` 仍是沙箱网关 URL。`status.proxy.state == Stopped`，`proxy_enabled` 为假，`socks5 == Disabled`，`gateway` 不是 `Disabled`。`warnings == [RestartCodex]`。`probe_existing(sandbox.port)` 为假。`state.proxy_disabled` 为真，`enabled` 仍为真。

`enable_proxy_while_managed_writes_env_and_listens`：上一个状态继续，或单独再测：`set_proxy_enabled(true)` 后 `.env` 含 `render_block(sandbox.port, "127.0.0.1")` 的 `NO_PROXY` 行，`probe_existing` 为真，`proxy_enabled` 为真，警告含 `RestartCodex`。

`proxy_switch_while_unmanaged_only_writes_state`：不启用。`set_proxy_enabled(false)` 后 state 的 `proxy_disabled` 为真，`config.toml` 与 `.env` 若事先写入则字节不变，`probe_existing` 为假，`warnings` 为空。

`reconcile_with_proxy_disabled_drops_env_block_and_does_not_listen`：state 写 `enabled: true, proxy_disabled: true`，凭据里放上 Key，`config.toml` 写成已经受管但 `base_url` 是旧的 consts URL，`.env` 写成带 `BEGIN_MARKER` 的块。`reconcile_on_startup` 的 `outcome == Applied`。之后 `base_url` 是沙箱网关 URL，`.env` 没有受管块，`status.residue` 为假，`env_managed` 为真，`probe_existing` 为假。

`enable_with_proxy_disabled_skips_env_and_proxy`：state 先写 `proxy_disabled: true`（`enabled: false`）。`enable(with_key(KEY))` 成功。`config_managed` 为真，`.env` 不存在或没有 `BEGIN_MARKER`，`proxy.state == Stopped`，`proxy_enabled` 为假。

`secure_rejects_proxy_off_in_both_directions`：

1. `secure_sandbox` 上 `set_proxy_enabled(false)` 先写 `proxy_disabled`（此时还没激活，应成功）。然后 `activate_secure(Path::new("/no/such/bundle.json"))` 返回 `secureRequiresProxy`。`status.mode` 仍是 `Legacy`，`proxy_disabled` 仍为真。
2. 另一个 `secure_sandbox`：`proxy_disabled` 保持假，`setup_secure_enrollment` 后 `activate_secure` 成功，`secure.active` 为真。再 `set_proxy_enabled(false)` 返回 `secureRequiresProxy`。`proxy_disabled` 仍为假，`probe_existing` 仍为真，`mode` 仍是 `Secure`。

`network_path` 的单元测试已在上面。下载拒绝测试在 `component_downloads.rs`。

- [ ] **Step 2: 跑红**

Run: `cargo test -p helper-core --lib -- state::tests::old_state_json_defaults_proxy_disabled_to_false component_updates::tests::proxy_switch_off_is_direct_with_its_own_reason`

Run: `cargo test -p helper-core --test manager disable_proxy_while_managed_removes_env_and_frees_port`

（`--lib` 与 `--test` 的过滤器不能混在一条命令里，分两条跑。）

Expected: FAIL。state 测试是编译错误（没有字段 `proxy_disabled`）。manager 测试是 `cannot find method set_proxy_enabled`。`network_path_for` 测试在字段不存在时同样编译失败。

- [ ] **Step 3: 实现**

按接口改 state、types、manager、`network_path_for`。`download_client` 的函数体不动。`set_endpoints` 的步骤 4 改为调用与对账相同的“按 `proxy_disabled` 写或删 `.env`”私有函数，避免两套矩阵。代理关闭时 `set_endpoints` 不启动代理；若本次只改了网关且受管开启、`.env` 从“有块”变成“无块”，警告里的 `RestartCodex` 仍然追加（网关 URL 变了且 enabled）。

`Status` 的 `Default` 会让新的 `proxy_enabled` 为 `false`。`build_status` 必须每次按 state 赋值，不能依赖 `Default`。`component_updates` 的测试辅助函数显式写 `true`。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p helper-core --lib state:: && cargo test -p helper-core --lib component_updates::tests::proxy_switch_off_is_direct_with_its_own_reason && cargo test -p helper-core --lib component_updates::tests::legacy_paths && cargo test -p helper-core --lib component_downloads::tests::disabled_proxy_reason_is_still_rejected && cargo test -p helper-core --test manager`

Expected: PASS。原有 `legacy_paths`（未运行 / External / Running）期望不变。Secure 激活的既有测试仍通过。

Run: `cargo clippy -p helper-core --all-targets -- -D warnings`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/helper-core/src/state.rs crates/helper-core/src/types.rs crates/helper-core/src/manager.rs crates/helper-core/src/component_updates.rs crates/helper-core/src/component_downloads.rs crates/helper-core/tests/manager.rs
git commit -F - <<'EOF'
feat(core): 本地代理可关闭，并与 Secure 模式互斥

关闭时删除 .env 受管块且不探测 SOCKS5；组件下载遇到这条路径仍拒绝直连。

Co-Authored-By: Claude Code
EOF
```

---

### Task 6: Tauri 命令

**文件：**

- 修改 `apps/desktop/src-tauri/src/commands.rs`
- 修改 `apps/desktop/src-tauri/src/main.rs` 的 `generate_handler!`

**接口：**

```rust
#[tauri::command]
pub async fn set_endpoints(
    state: tauri::State<'_, AppState>,
    request: helper_core::types::SetEndpointsRequest,
) -> Result<helper_core::types::SetEndpointsOutcome, ErrorPayload>;

#[tauri::command]
pub async fn set_proxy_enabled(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<helper_core::types::SetEndpointsOutcome, ErrorPayload>;
```

成功：`state.publish(&outcome.status)`，返回 `Ok(outcome)`。失败：与 `finish` 相同，再取 `manager.status().await` 推送一次，然后 `Err(error.into())`。不要在命令里再调用 `probe_reachability`：Manager 已经在锁内刷新过可达性，返回的 `status` 就是最新快照。

`use` 列表加上 `SetEndpointsOutcome`、`SetEndpointsRequest`。

`generate_handler!` 在 `commands::set_autostart` 之后加入 `commands::set_endpoints` 与 `commands::set_proxy_enabled`。

- [ ] **Step 1: 写会失败的接线**

只改 `main.rs`：在 `generate_handler!` 里写上 `commands::set_endpoints` 和 `commands::set_proxy_enabled`。先不要在 `commands.rs` 添加函数。

- [ ] **Step 2: 跑红**

Run: `cargo check -p codex-helper`

Expected: FAIL，错误含 `cannot find set_endpoints in commands`（或等价的“找不到函数”）。

- [ ] **Step 3: 实现两个命令**

按接口写完整函数。命令层不解析地址、不拼警告文案。

- [ ] **Step 4: 跑绿**

Run: `cargo check -p codex-helper`

Expected: PASS，进程退出码 0。

- [ ] **Step 5: 提交**

```bash
git add apps/desktop/src-tauri/src/commands.rs apps/desktop/src-tauri/src/main.rs
git commit -F - <<'EOF'
feat(desktop): 暴露设置端点与代理开关命令

命令只转发 Manager 的结果，并在成功或失败后都推送状态。

Co-Authored-By: Claude Code
EOF
```

---

### Task 7: 前端网关设置面板与代理开关

**文件：**

- 新建 `apps/desktop/src/endpointformat.ts`
- 新建 `apps/desktop/src/endpointformat.test.ts`
- 新建 `apps/desktop/src/endpointpanel.ts`
- 修改 `apps/desktop/src/types.ts`、`api.ts`、`main.ts`、`dev-mock.ts`、`statuslights.ts`、`format.ts`、`format.test.ts`、`switchcontrol.ts`、`styles.css`

**接口（`types.ts`，与 Rust serde 一致）：**

```ts
export type Reachability = "unknown" | "reachable" | "unreachable" | "disabled";

export type EndpointWarning =
  | "keyRejected"
  | "keyCheckUnavailable"
  | "externalProxy"
  | "restartCodex";

export interface SetEndpointsRequest {
  gatewayBaseUrl: string | null;
  socks5Addr: string | null;
}

export interface SetEndpointsOutcome {
  status: Status;
  warnings: EndpointWarning[];
}
```

`Status` 增加 `gatewayBaseUrl: string`、`endpointsCustomized: boolean`、`proxyEnabled: boolean`。不增加 `mode` / `secure`。

`ErrorCode` 增加 `"invalidEndpoint"` 与 `"secureRequiresProxy"`。`ErrorPayload` 增加可选 `field?: "gateway" | "socks5"`。

`api.ts`：

```ts
setEndpoints: (request: SetEndpointsRequest) =>
  invoke<SetEndpointsOutcome>("set_endpoints", { request }),
setProxyEnabled: (enabled: boolean) =>
  invoke<SetEndpointsOutcome>("set_proxy_enabled", { enabled }),
```

`endpointformat.ts` 纯函数：

```ts
export function endpointWarningText(
  warning: EndpointWarning,
  source: "endpoints" | "proxy",
): string;

export function shouldKeepEndpointDraft(input: { focused: boolean; dirty: boolean }): boolean;

export function socks5Muted(proxyEnabled: boolean): boolean;

export function formatSocks5Light(
  proxyEnabled: boolean,
  reachability: Reachability,
): { text: string; level: StatusLevel };
```

文案全字匹配：

| warning | source | 文本 |
| --- | --- | --- |
| `keyRejected` | 任意 | `当前 Key 在新网关校验失败（被拒绝），请重新录入 Key` |
| `keyCheckUnavailable` | 任意 | `无法连接新网关校验 Key，请检查地址或网络` |
| `restartCodex` | 任意 | `请重启 Codex 生效` |
| `externalProxy` | `"endpoints"` | `本地代理由外部实例提供，新 SOCKS5 地址需重启该实例后生效` |
| `externalProxy` | `"proxy"` | `本地代理由外部实例提供，需要手动停止该实例` |

`shouldKeepEndpointDraft` 在 `focused || dirty` 时为真。`socks5Muted` 为 `!proxyEnabled`。`formatSocks5Light`：`!proxyEnabled` 或 `reachability === "disabled"` 时 `{ text: "未启用", level: "warn" }`，否则把现有 `formatReachabilityLabel` / `reachabilityLevel` 的结果原样返回。

`format.ts` 的两个 switch 增加 `"disabled"` 分支：文案 `未启用`，等级 `warn`。`format.test.ts` 现有“覆盖三态”改成覆盖四态，`"disabled"` 断言为 `未启用` / `warn`。

`switchcontrol.ts` 抽出 `createToggle(handlers: { onChange: (enabled: boolean) => void })`，返回 `{ root, update }`。`update` 接收 `{ enabled, busy, label, ariaLabel, onText, offText, busyText }`。DOM 与现在的受管开关相同：`div.switch-row` 里是 `span.switch-label`、`button.switch-control`（`role="switch"`、`aria-checked`、忙时 `aria-busy="true"`）、`span.switch-state-text`。`createSwitchRegion` 改为调用 `createToggle`，标签仍是“受管模式”，文案仍是“已启用 / 已停用 / 启用中… / 停用中…”，可切换条件仍是 `status.enabled || status.keyConfigured`。不要改受管开关的对外回调。

`endpointpanel.ts` 参照 `keypanel.ts` 用 `el` 建 DOM，不用 `innerHTML`：

```text
div.endpoint-panel
  h2.section-title            “网关设置”
  div.endpoint-row            span “模型网关” + input#endpoint-gateway
  p.endpoint-field-error      网关错误，默认 hidden
  div.endpoint-row            span “上游 SOCKS5” + input#endpoint-socks5
  p.endpoint-field-error      SOCKS5 错误，默认 hidden
  div（createToggle 的 root）  标签“启用本地代理”
  div.endpoint-actions        button “恢复默认” + button.btn.btn-primary “保存”
  div.endpoint-feedback       aria-live=polite
```

输入框 `autocomplete="off"`、`spellcheck="false"`。`aria-label` 分别为“模型网关”“上游 SOCKS5”。

行为：

- `update(status, busy)`：若任一输入框 `shouldKeepEndpointDraft` 为真，不改两个输入框的 `value`。否则分别填入 `status.gatewayBaseUrl` 与 `status.socks5Addr`。
- SOCKS5 输入框在 `socks5Muted(status.proxyEnabled)` 时加上 `endpoint-input-muted`，并且始终不设置 `disabled` 与 `readOnly`。
- “保存”在 `busy` 含 `"endpoints"` 时 `disabled`，文字改为“保存中…”。
- “恢复默认”在 `!status.endpointsCustomized` 或 `busy` 含 `"endpoints"` 时 `disabled`。
- 代理开关的 `busy` 来自 `busy.has("proxy")`，忙时文字“切换中…”，开/关文字“已启用”“已关闭”。
- `showFieldError(field, message)` 把对应 `p` 的文本设为 `message` 并取消 hidden，另一侧错误隐藏。
- `showWarnings(warnings, source)` 清空 feedback，逐条放 `p.endpoint-feedback-warn`，文本用 `endpointWarningText`。仅 `keyRejected` 后面追加按钮“重新录入”，点击 `handlers.onReenterKey`。
- `showMessage(message)` 在 feedback 放一条 `p.endpoint-feedback-error`。
- `markClean()` 把两个 dirty 标志清掉，供保存成功后的下一次 `update` 写入返回状态。
- `input` 事件把该框 dirty 设为真；`focus` / `blur` 维护 focused。保存点击时读取当前 `value`，不在面板里做格式校验。

`styles.css` 在 Key 面板样式后增加：`.endpoint-panel` 用与 `.key-form` 相同的边框、圆角、内边距和 `var(--panel-bg)`；`.endpoint-row` 与 `.endpoint-actions` 用 flex、`gap: 8px`、`align-items: center`；输入框 `flex: 1`；`.endpoint-field-error` 与 `.endpoint-feedback-error` 用 `var(--bad-fg)`；`.endpoint-feedback-warn` 用 `var(--warn-fg)`；`.endpoint-input-muted` 只降低不透明度（`opacity: 0.55`），不影响指针事件。

`main.ts`：

- `OpKind` 增加 `"endpoints"` 与 `"proxy"`。
- `content` 的子节点顺序改为：`bannerRegion.root`、`switchRegion.root`、`keyPanel.root`、`endpointPanel.root`、`normalModeRegion`。首次运行仍只隐藏受管开关和 `normalModeRegion`，网关设置保持可见。
- `renderGateway` 调用 `endpointPanel.update(status, busy)`。
- 保存：`busy` 已有 `"endpoints"` 则返回。先 `add`，`clearFeedback`，`renderGateway`。`api.setEndpoints({ gatewayBaseUrl, socks5Addr })`。成功则 `status = outcome.status`，`markClean()`，`showWarnings(outcome.warnings, "endpoints")`。失败则 `toErrorPayload`：`code === "invalidEndpoint"` 且 `field` 是 `"gateway"` 或 `"socks5"` 时 `showFieldError`；其他错误 `messageRegion.show(payload.message, "error")`。`finally` 里 `busy.delete` 再 `renderGateway`。`renderGateway` 不得清掉 feedback（`update` 不碰反馈节点）。
- 恢复默认：调用 `api.setEndpoints({ gatewayBaseUrl: null, socks5Addr: null })`，同样走 `"endpoints"` busy 与警告展示。
- 代理开关：`api.setProxyEnabled(enabled)`，busy 键 `"proxy"`。成功展示 `showWarnings(warnings, "proxy")`。失败且 `code === "secureRequiresProxy"` 时 `showMessage(payload.message)`（服务端文案已经是“Secure 模式需要本地代理，请先停用 Secure”）。其他错误进消息区。
- `onReenterKey`：`keyPanel.openForm(); keyPanel.focusInput();`。

`statuslights.ts` 的 SOCKS5 行改为 `formatSocks5Light(status.proxyEnabled, status.socks5)` 的 `text` 与 `level`。地址仍是 `status.socks5Addr`。网关行不变。

`dev-mock.ts` 的 `baseStatus()` 增加：

```ts
gatewayBaseUrl: "http://10.20.30.61:8080",
endpointsCustomized: false,
proxyEnabled: true,
```

`switch (cmd)` 增加：

- `set_endpoints`：参数 `args.request` 为 `SetEndpointsRequest`。`gatewayBaseUrl` 是空串或只含空白 → reject `{ code: "invalidEndpoint", message: "不能为空", field: "gateway" }`。`socks5Addr` 非空且既不匹配 `/^\S+:\d{1,5}$/` 也不匹配 `/^\[[^\]]+\]:\d{1,5}$/` → reject `{ code: "invalidEndpoint", message: "格式应为 host:port", field: "socks5" }`。`null` 表示该侧恢复默认常量。两边规范化后都等于默认常量时 `endpointsCustomized = false` 且 state 字段按默认显示；否则为真。`gatewayAddr` 用 `URL` 解析：主机含 `:` 时 `[host]:port`，省略端口时 `http` 用 80、`https` 用 443。若 `status.enabled` 且网关 URL 与调用前不同，`warnings` 含 `"restartCodex"`，否则空数组。返回 `{ status, warnings }` 并 `emit(EVENT_STATUS_CHANGED, status)`。
- `set_proxy_enabled`：`enabled === false` 且查询串 `scenario=secure` 时 reject `{ code: "secureRequiresProxy", message: "Secure 模式需要本地代理，请先停用 Secure" }`。否则 `proxyEnabled = enabled`，`socks5` 在关闭时为 `"disabled"`、开启时保持原值（若原值是 `"disabled"` 则改回 `"unknown"`），`proxy.state` 在开启时为 `"running"`、关闭时为 `"stopped"`。`status.enabled` 为真时 `warnings` 为 `["restartCodex"]`，否则 `[]`。同样 emit。

- [ ] **Step 1: 写失败测试**

`endpointformat.test.ts`：

`endpoint_warning_text_matches_design_copy`：上表五行 `equal`。

`draft_is_kept_when_focused_or_dirty`：`{focused:true, dirty:false}`、`{focused:false, dirty:true}` 为真；`{focused:false, dirty:false}` 为假。

`socks5_input_is_muted_only_when_proxy_is_off`：`socks5Muted(false)` 为真，`socks5Muted(true)` 为假。

`socks5_light_says_disabled_when_proxy_is_off`：`formatSocks5Light(false, "reachable")` 深等于 `{ text: "未启用", level: "warn" }`；`formatSocks5Light(true, "disabled")` 同样；`formatSocks5Light(true, "unreachable")` 深等于 `{ text: "不可达", level: "bad" }`。

`format.test.ts` 补 `"disabled"` 分支断言。此时 `Reachability` 还没有 `"disabled"`，或者函数还没导出，测试必须失败。

- [ ] **Step 2: 跑红**

Run: `npm --prefix apps/desktop test`

Expected: FAIL。`endpointformat.test.ts` 因找不到模块失败，或 `format.test.ts` 因 `"disabled"` 不能赋给 `Reachability` 失败。不能是全部 PASS。

- [ ] **Step 3: 实现**

按接口写纯函数、面板、类型、API、main 接线、mock、状态灯、样式，并把 `createSwitchRegion` 改到 `createToggle` 之上。生产代码不在面板里复制一份校验规则。

- [ ] **Step 4: 跑绿**

Run: `npm --prefix apps/desktop test && npm --prefix apps/desktop run check`

Expected: PASS。`check` 是 `tsc --noEmit`。测试数量比改动前多出本任务新增的 4 个 `test()`。

- [ ] **Step 5: 提交**

```bash
git add apps/desktop/src/endpointformat.ts apps/desktop/src/endpointformat.test.ts apps/desktop/src/endpointpanel.ts apps/desktop/src/types.ts apps/desktop/src/api.ts apps/desktop/src/main.ts apps/desktop/src/dev-mock.ts apps/desktop/src/statuslights.ts apps/desktop/src/format.ts apps/desktop/src/format.test.ts apps/desktop/src/switchcontrol.ts apps/desktop/src/styles.css
git commit -F - <<'EOF'
feat(desktop): 增加网关设置面板与本地代理开关

编辑中的输入不被状态刷新覆盖；代理关闭时 SOCKS5 只做视觉置灰。

Co-Authored-By: Claude Code
EOF
```

---

### Task 8: 文档同步与全量门禁

**文件：**

- 修改 `docs/superpowers/specs/2026-09-23-codex-helper-design.md` 的 §1.1 与 §1.2（用户已确认 §1.2 一起改）
- 修改 `docs/superpowers/specs/2026-11-16-configurable-endpoints-design.md` 的状态行
- 修改本计划文件：回填勾选框，末尾追加“执行记录”

§1.2 中“不支持修改网关、SOCKS5、端口或规则；不在运行时下载规则。”改为：“不支持修改本地代理端口或分流规则；不在运行时下载规则。网关与上游 SOCKS5 可在界面修改、本地代理可关闭，见 `2026-11-16-configurable-endpoints-design.md`。”§1.2 其他条目不动。

§1.1 标题由“固定参数（全部写死，不提供界面修改）”改为“固定参数（未自定义时的默认值）”。表格两行改为：

| 项 | 值 |
| --- | --- |
| 模型网关 | `http://10.20.30.61:8080`（`wire_api = "responses"`）。默认值，可在界面修改，见 `2026-11-16-configurable-endpoints-design.md` |
| 上游 SOCKS5 | `10.20.30.61:7891`（无认证）。默认值，可在界面修改，见 `2026-11-16-configurable-endpoints-design.md` |

表格后加一段：“本地代理可由用户关闭。关闭后不启动 `127.0.0.1:17891`、不写 `.env` 受管块；`config.toml` 是否接管仍由受管开关决定。详见 `2026-11-16-configurable-endpoints-design.md`。”本地代理端口、规则、凭据、provider、审批与沙箱这几行原文不动。

- [ ] **Step 1: 修改 §1.1 与 §1.2**

§1.1 只改标题、两行单元格和表格后的那一段；§1.2 只改上面指定的那一条。

- [ ] **Step 2: 全量门禁**

Run: `scripts/gate.sh`

Expected: 依次打印 `== cargo fmt --check`、`== cargo clippy`、`== cargo test`、`== windows check`、`== tsc`、`== frontend test`、`== frontend build`，最后一行是 `== gate passed`，退出码 0。fmt 失败就先 `cargo fmt --all` 再重跑，不要用 `--no-verify` 绕过。clippy 或测试失败时修实现并补回归测试，再重跑门禁；不要为了变绿加 `allow`。

手工项（门禁之外，在 macOS 开发环境有图形界面时做；做不到就在提交说明里写明没做，不要写成已做）：

1. `npm --prefix apps/desktop run tauri dev` 启动应用。
2. 启用受管模式后，把网关改成一个本机 HTTP 地址、SOCKS5 改成另一个 `host:port`，保存。检查 `~/.codex/config.toml` 的 `base_url`、`~/.codex/.env` 的 `NO_PROXY` 主机、状态灯上的两个地址。
3. 点“恢复默认”，两处文件回到 `http://10.20.30.61:8080` 与 `NO_PROXY=10.20.30.61,127.0.0.1,localhost`，状态灯地址回到 `10.20.30.61:8080` 与 `10.20.30.61:7891`。
4. 关闭“启用本地代理”。`.env` 受管块消失，`config.toml` 的 `model_provider` 仍是 `managed_gateway`，SOCKS5 灯为“未启用”，SOCKS5 输入框仍能打字。再打开开关，受管块回到文件末尾，本地代理灯为“运行中”。

- [ ] **Step 3: 更新设计状态与计划勾选框**

1. `2026-11-16-configurable-endpoints-design.md` 的“状态”行改为如实结果，例如：“已实现，`scripts/gate.sh` 全绿；macOS 手工验证已完成 / 未完成（写明原因）”。只写实际发生的事。
2. 本计划中已完成的步骤把 `- [ ]` 改为 `- [x]`（包括本 Step 与 Step 4）。没做的步骤保持 `- [ ]`，并在执行记录里说明。
3. 本计划末尾追加 `## 执行记录`：每个 Task 的提交哈希与标题；门禁结果；手工验证结果；实施中偏离计划的地方及原因。

- [ ] **Step 4: 提交**

```bash
git add docs/superpowers/specs/2026-09-23-codex-helper-design.md docs/superpowers/specs/2026-11-16-configurable-endpoints-design.md docs/superpowers/plans/2026-11-16-configurable-endpoints.md
git commit -F - <<'EOF'
docs: 网关与 SOCKS5 可配置、本地代理可关闭的文档同步与完成记录

旧设计 §1.1、§1.2 注明默认值可在界面修改；新设计标记实施状态；回填计划勾选框。

Co-Authored-By: Claude Code
EOF
```

若 Step 2 为了修门禁改了代码，那些文件放进同一个提交，提交信息改成说明修了什么，仍然带同一行尾注。不 push。

---

## 设计条目 → Task

| 设计条目 | Task |
| --- | --- |
| 用户确认：网关与 SOCKS5 分开配置，写入 `state.json`，界面修改 | Task 1 的两个字段；Task 4 的 `set_endpoints`；Task 7 的两个输入框 |
| 受管启用时保存后立即生效 | Task 4 步骤 4 在 `enabled` 时 `apply_managed` + 写 `.env`；Task 5 改为按代理开关写或删 `.env` |
| 网关变更后用已存 Key 校验，失败只警告 | Task 4 锁外步骤 6：`KeyRejected` / `KeyCheckUnavailable`，返回值仍是 `Ok` |
| `Endpoints` 值对象 + 显式参数 | Task 1 的类型；Task 2 的三个函数签名；Task 4 把参数换成 `runtime.endpoints` |
| 代理关闭：不启动代理、不写 `.env`、不探测 SOCKS5；`config.toml` 仍接管 | Task 5 的矩阵、`Reachability::Disabled`、`enable` / `reconcile` |
| 非目标：无环境变量/命令行覆盖，不改 Secure 上游，不改规则与本地端口，不自动重启 Codex，下载不回退直连 | 全局约束；Task 5 不改 `download_client`；Task 5 的 `activate_secure` 不改 `server_host` |
| §1.1 两个 `Option` 字段；等于默认值则存 `None`；旧文件兼容 | Task 1 的字段与反序列化测试；Task 4 步骤 3 的 `None` 写入 |
| §1.2 `Endpoints` 的五个字段与五个方法；`EndpointError` 带字段 | Task 1 |
| §1.3 校验：scheme、userinfo、query/fragment、路径与末尾斜杠、默认端口、SOCKS5 `host:port`、IPv6 方括号、空白与非可打印 ASCII、允许回环 | Task 1 的 `parse_accepts_documented_forms` 与 `parse_rejects` |
| §1.4 consts 保留，注释改为默认值，删掉“全部写死” | Task 1 的 `consts.rs` |
| §2.1 `ManagerOptions.default_endpoints` | Task 4 |
| §2.2 `Runtime.endpoints`；`new` 与 `reconcile_on_startup` 从 state 初始化 | Task 4 |
| §2.3 `ensure_managed` / `env` 判定改参数；不一致即未受管 | Task 2 的单测；Task 4 接线后由 manager 测试覆盖对账校正 |
| §2.3 `route_for_host(host, always_direct)`；回环仍写死 | Task 2 |
| §2.3 `ProxyConfig` 的 SOCKS5 移入 `Routing` | Task 3 |
| §2.3 可达性读 `runtime.endpoints` | Task 4 的 `probe_reachability`；Task 5 在关闭时跳过 SOCKS5 |
| §2.3 删除 `verify_key`，统一 `verify_key_at` | Task 4 |
| §2.3 `Status` 保留两个展示地址，新增 `gateway_base_url` 与 `endpoints_customized` | Task 4 |
| §2.3 Secure 上游不变；`always_direct` 对网关主机生效 | Task 3 的 `from_endpoints` 把网关主机放进列表；Task 5 明确不改 `SecureUpstream` 赋值 |
| §3.1 `Routing` 结构、连接开始时拷贝快照、不跨 await 持锁、保存时就地更新、External 警告、旧隧道不迁移、Codex 必须重启 | Task 3 的快照与测试；Task 4 的热更新集成测试与 `RestartCodex` / `ExternalProxy` |
| §3.2 `SetEndpointsRequest` / `EndpointWarning` / `SetEndpointsOutcome` / `ManagerError::InvalidEndpoint` | Task 4 的类型；Task 6 的命令 |
| §3.3 锁内 1–5 步与锁外 6–7 步 | Task 4；步骤 4 在 Task 5 改为尊重 `proxy_disabled` |
| §3.4 校正失败不回滚 state | Task 4 的 `set_endpoints_keeps_new_state_when_config_apply_fails` |
| §3.5 `enable` / `save_key` 用 `runtime.endpoints.gateway_base_url` | Task 4 |
| §3.6 日志记新旧地址，不记 Key | Task 4 的日志断言 |
| §4 面板位置、预填、编辑中不覆盖、busy `"endpoints"`、恢复默认、就地错误、四条警告、“重新录入”打开 Key 表单、状态灯读新地址、`types` / `api` / `dev-mock` | Task 7 |
| §5 Rust 单测：解析表、`resolve` 回落、rules 新签名、自定义主机恒直连、env/config 自定义值与校正、旧 state、Routing 热更新 | Task 1、Task 2、Task 3 |
| §5 集成：启用时改两个文件并带 `RestartCodex`、SOCKS5 热更新且端口不变、401 与不可达都只警告、非法输入不动文件、恢复默认、重启 Manager 读回 state、停用时只写 state | Task 4 |
| §5 前端：警告文案、编辑中不覆盖 | Task 7 |
| §5 手工：macOS 上改地址、看两个文件和状态灯、恢复默认 | Task 8 |
| §6 旧设计 §1.1、§1.2 同步，并补“本地代理可关闭” | Task 8 |
| 实施完成后更新设计状态、回填计划勾选框 | Task 8 Step 3 |
| §7.1 `proxy_disabled: bool`，缺省 false | Task 5 |
| §7.2 矩阵与“受管关闭时开关只是偏好” | Task 5 的 `proxy_switch_while_unmanaged_only_writes_state` 与 enable/reconcile 测试 |
| §7.3 `enable` / `reconcile` / 残留 / `set_proxy_enabled` / Secure 双向互斥 / `Reachability::Disabled` / `proxy_enabled` / `network_path_for` 新原因 / 下载逻辑不改 | Task 5 |
| §7.4 开关复用 `switchcontrol.ts`、busy `"proxy"`、SOCKS5 置灰仍可编辑、灯显示“未启用”、两条场景化警告、`SecureRequiresProxy` 文案 | Task 7；错误字符串在 Task 5 的 `ManagerError` 显示文本里 |
| §7.5 开关两个方向、受管关只写 state、对账删残留块、`enable` 不写 `.env`、Secure 双向、旧 state、`network_path_for`、前端置灰与“未启用” | Task 5 与 Task 7 |
