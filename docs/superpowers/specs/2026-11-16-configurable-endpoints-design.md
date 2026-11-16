# 网关与上游 SOCKS5 地址可配置、本地代理可关闭设计

- 日期：2026-11-16
- 状态：已实现，`scripts/gate.sh` 全绿；macOS 手工验证未完成（未启动 `tauri dev`，避免改写仓库外的 `~/.codex/config.toml` 与 `~/.codex/.env`）
- 范围：
  - 把写死的模型网关地址（`http://10.20.30.61:8080`）与上游 SOCKS5 地址（`10.20.30.61:7891`）改为用户可在桌面界面配置的两个独立参数；
  - 新增“启用本地代理”开关：关闭时不启动本地代理、不写 `~/.codex/.env` 受管块（见 §7）。
- 用户已确认：
  - 网关与 SOCKS5 分开配置；
  - 在桌面应用界面修改，持久化到 `state.json`；
  - 受管模式启用时保存后立即生效；
  - 网关变更后用已存 Key 校验新网关，失败只警告、不阻止保存；
  - 采用“`Endpoints` 值对象 + 显式参数传递”的实现方案；
  - 代理开关关闭时整体关闭代理（不启动代理、不写 `.env`、不检测 SOCKS5），`config.toml` 接管不受影响。

## 背景

`crates/helper-core/src/consts.rs` 把网关与 SOCKS5 写死（原设计 §1.1“全部写死，不提供界面修改”）。用到这些常量的地方：

- `codex_config::ensure_managed`：写入 `config.toml` 的 `model_providers.managed_gateway.base_url`；
- `codex_env::block_lines`：`.env` 受管块的 `NO_PROXY=<网关主机>,127.0.0.1,localhost`；
- `rules::is_always_direct`：网关主机与 SOCKS5 主机恒直连；
- `proxy::ProxyConfig`：Legacy 模式的无认证 SOCKS5 上游；
- `gateway::verify_key`：Key 校验 `GET {网关}/v1/models`；
- `ManagerOptions`：可达性检测目标与 `Status.gateway_addr` / `socks5_addr` 展示。

Secure 模式的上游取自注册包 `server_host`，不在本设计范围内，行为不变。

现状核查（2026-11-16，只读）：mTLS 核心（Task 1–9）已合入主干，但桌面端没有暴露 Secure 激活入口，所有实际用户都在 Legacy 模式。服务端 `aiproxy:/opt/mihomo` 的 `mihomo-auth`（7892，`require-and-verify`）已部署运行，10-20 之后没有客户端流量；旧入口 7891 仍对外监听。

## 非目标

- 不提供环境变量或命令行覆盖。
- 不修改 Secure 模式上游（仍取注册包里的值）。
- 不改分流规则集，不改本地代理端口。
- 不为 Codex 自动重启：网关变更或代理开关变更后只提示用户重启 Codex。
- 代理关闭时不提供组件直连下载（沿用现有“请先启用本地代理”拒绝）。

## 1. 数据模型与校验

### 1.1 存储（`state.rs`）

`HelperState` 新增两个字段（结构体已有 `#[serde(default)]`，旧状态文件无缝兼容）：

```rust
/// 用户配置的模型网关 URL；None = 使用默认值
pub gateway_base_url: Option<String>,
/// 用户配置的上游 SOCKS5 地址（host:port）；None = 使用默认值
pub socks5_addr: Option<String>,
```

保存时若规范化后的值等于默认值，存 `None`。这样以后默认值改了，没自定义过的用户会自动跟随。

### 1.2 值对象（新建 `endpoints.rs`）

```rust
pub struct Endpoints {
    pub gateway_base_url: String, // 规范化后，无末尾 '/'
    pub gateway_host: String,     // 规范化（小写），IPv6 不带方括号
    pub gateway_port: u16,        // 显式端口，或按 scheme 取 80 / 443
    pub socks5_host: String,      // 规范化（小写），IPv6 不带方括号
    pub socks5_port: u16,
}

impl Endpoints {
    /// 来自 consts 的默认值。
    pub fn default_values() -> Self;
    /// 以 `defaults` 为底，state 中为 Some 的项覆盖；某项解析失败时回落默认值并记日志。
    pub fn resolve(state: &HelperState, defaults: &Endpoints) -> Self;
    /// 解析并校验用户输入。
    pub fn parse(gateway_url: &str, socks5_addr: &str) -> Result<Self, EndpointError>;
    /// 展示用 `host:port`（IPv6 带方括号）。
    pub fn gateway_addr(&self) -> String;
    pub fn socks5_addr(&self) -> String;
}
```

`EndpointError` 带字段标识（`Gateway` / `Socks5`）与中文原因，供界面就地提示。

### 1.3 校验规则

网关 URL：

- scheme 只允许 `http` 或 `https`；
- 必须有主机；不能带用户名密码（`user:pass@`）、query、fragment；
- 允许带路径（如 `/proxy`），去掉末尾 `/`；
- 端口可省略，省略时 `http` 取 80、`https` 取 443。

SOCKS5 地址：

- 格式为 `host:port`，不带 scheme；
- 端口在 1–65535 之间；
- IPv6 写成 `[::1]:7891`。

两者共同：

- 去首尾空白后不能为空；不能含空白或控制字符，也不能含非可打印 ASCII（与组件白名单检查同一风格）；
- 允许填回环地址（便于开发调试）。

### 1.4 常量

`consts.rs` 里的 `GATEWAY_BASE_URL` / `GATEWAY_HOST` / `GATEWAY_PORT` / `SOCKS5_HOST` / `SOCKS5_PORT` 保留，注释改成“默认值”。文件顶部“全部写死，不提供界面修改”那句改为说明这两组地址可以由用户配置。

## 2. 地址如何传到各模块

原则：主机和地址只有一个来源，就是 `Manager` 当前持有的 `Endpoints`。下游函数从参数拿值，不再读 consts。

### 2.1 `ManagerOptions`

把 `socks5_host`、`socks5_port`、`gateway_base_url`、`gateway_host`、`gateway_port` 五个字段合并为：

```rust
/// 默认地址（生产为 consts；测试注入假网关与假 SOCKS5）
pub default_endpoints: Endpoints,
```

实际生效的值为 `Endpoints::resolve(&state, &options.default_endpoints)`。

### 2.2 `Manager`

`Runtime` 短锁内新增 `endpoints: Endpoints`。`Manager::new` 与 `reconcile_on_startup` 读 state 后初始化，之后各步骤都从这里取值。

### 2.3 各使用点

| 模块 | 现状 | 改为 |
| --- | --- | --- |
| `codex_config::ensure_managed` 及受管判定 | 读 `consts::GATEWAY_BASE_URL` | 新增参数 `gateway_base_url: &str`；`base_url` 和当前值不一致即视为未受管，对账时校正 |
| `codex_env::block_lines` 及 `env_managed` 判定 | `NO_PROXY` 写死 `GATEWAY_HOST` | 新增参数 `gateway_host: &str`（IPv6 不带方括号）；内容不一致即视为未受管 |
| `rules::route_for_host` | `is_always_direct` 读 consts | 签名改为 `route_for_host(host, always_direct: &[String])`；回环地址仍在函数内写死 |
| `proxy::ProxyConfig` | `socks5_host/port` 默认取 consts | 移入共享的 `Arc<RwLock<Routing>>`，见 §3.1 |
| 可达性检测 | `options.gateway_host/port`、`options.socks5_host/port` | 读 `runtime.endpoints` |
| `gateway::verify_key` | 固定 URL 的包装 | 删除，统一调用 `verify_key_at(endpoints.gateway_base_url, …)` |
| `Status` | `gateway_addr`、`socks5_addr` | 保留；新增 `gateway_base_url`（完整 URL，用于预填）与 `endpoints_customized: bool` |

Secure 模式：上游仍取注册包；`always_direct` 同样生效，网关主机始终直连。

## 3. 保存流程与错误处理

### 3.1 代理热更新

本地代理每个客户端连接只处理一个请求（`proxy.rs` 模块注释：明文请求强制 `Connection: close`，CONNECT 隧道结束即关闭）。分流判定 `rules::route_for_host` 与上游连接 `connect_upstream` 都在连接开始时读配置，没有缓存，也没有连接池。所以可以热更新，不用重启：

```rust
pub struct Routing {
    pub socks5_host: String,
    pub socks5_port: u16,
    /// 恒直连主机：网关主机 + SOCKS5 主机（已规范化）
    pub always_direct_hosts: Vec<String>,
}
```

- `ProxyConfig` 去掉 `socks5_host/port`，新增 `routing: Arc<RwLock<Routing>>`；
- `handle_connection` 开头取一次 `Routing` 快照（clone 后立即释放锁，不跨 `await` 持锁），分流判定和上游连接都用这份快照；
- `Manager` 保存地址时就地更新这份 `Routing`，Legacy 与 Secure 模式都适用，不触发 Secure 的排空和门禁重建；
- 外部复用实例（`External`）是另一个进程，改不到，返回 `ExternalProxy` 警告。

边界：

1. 已建立的 CONNECT 隧道（如流式响应）继续走旧 SOCKS5，直到自己关闭；新连接走新地址。界面不额外提示。
2. 网关地址对正在运行的 Codex 不会立即生效：Codex 直连网关（`NO_PROXY` 包含网关主机），只在启动时读取 `config.toml` 与 `.env`，必须重启 Codex。

### 3.2 新接口

```rust
pub struct SetEndpointsRequest {
    pub gateway_base_url: Option<String>, // None = 恢复默认
    pub socks5_addr: Option<String>,      // None = 恢复默认
}

pub enum EndpointWarning {
    /// 新网关拒绝了已存 Key（401 / 403）
    KeyRejected,
    /// 无法连接新网关完成 Key 校验（网络不通 / 超时 / 其他错误）
    KeyCheckUnavailable,
    /// 本地代理由外部实例提供，新 SOCKS5 地址不会对它生效
    ExternalProxy,
    /// 受管已启用，且网关地址或 `.env` 受管块发生变化，需要重启 Codex
    RestartCodex,
}

pub struct SetEndpointsOutcome {
    pub status: Status,
    pub warnings: Vec<EndpointWarning>,
}

impl Manager {
    pub async fn set_endpoints(&self, req: SetEndpointsRequest)
        -> Result<SetEndpointsOutcome, ManagerError>;
}
```

Tauri 新增命令 `set_endpoints`，序列化方式与现有命令一致。`ManagerError` 新增 `InvalidEndpoint { field, message }`。

### 3.3 步骤

持有 ops 锁时：

1. `Endpoints::parse` 解析输入（`None` 取默认值）。失败返回 `InvalidEndpoint`，什么都不改。
2. 和当前值完全相同就直接返回当前状态，不带警告。
3. 原子写入 `state.json`，更新 `runtime.endpoints`；本地代理运行中时同时更新它的共享 `Routing`（代理关闭时只存值，下次启动代理时使用）。
4. 如果处于启用状态，复用现有的幂等校正路径，对账 `config.toml`（`base_url`）与 `.env`（`NO_PROXY`），含备份。
5. 刷新可达性。

释放 ops 锁后（避免 Key 校验超时阻塞其他操作）：

6. 只有网关 URL 变了、并且已存 Key 时，才用新地址调用 `verify_key_at`：401/403 得到 `KeyRejected`，其他失败得到 `KeyCheckUnavailable`。保存结果不受影响。
7. 网关 URL 变了且处于启用状态时，追加 `RestartCodex`；代理开关开启、本地代理为 External 且 SOCKS5 变了时，追加 `ExternalProxy`。

### 3.4 部分失败

`state.json` 是唯一的期望值来源。第 4 步失败（例如 `config.toml` 结构不对）时，state 保留新值，接口返回错误，界面照常显示 `config_error`。用户修好文件或下次启动对账时会自动校正。不做回滚，避免界面显示的地址和用户刚填的不一致。

### 3.5 其他入口

`enable`、`save_key` 里的 Key 校验，统一改用 `runtime.endpoints.gateway_base_url`。

### 3.6 日志

记录新旧地址（地址不是机密信息），不记录 Key。

## 4. 界面

新模块 `apps/desktop/src/endpointpanel.ts`，结构参照 `keypanel.ts`，放在 Key 面板下面：

```
┌ 网关设置 ─────────────────────────────────────┐
│ 模型网关    [http://10.20.30.61:8080      ]   │
│ 上游 SOCKS5 [10.20.30.61:7891             ]   │
│                     [恢复默认]  [保存]        │
│ (已自定义) / 就地错误 / 警告信息               │
└──────────────────────────────────────────────┘
```

- 输入框预填 `status.gateway_base_url` 和 `status.socks5_addr`。用户正在编辑（输入框有焦点或内容已改动）时，状态刷新不覆盖输入内容。
- 「保存」调用 `set_endpoints`，期间按钮置灰，`busy` 集合新增 `"endpoints"` 键。
- 「恢复默认」调用 `set_endpoints(None, None)`，只在 `endpoints_customized` 为真时可点。
- `InvalidEndpoint` 显示在对应输入框下面。
- 警告文案：
  - `KeyRejected`：「当前 Key 在新网关校验失败（被拒绝），请重新录入 Key」，附「重新录入」按钮，点击调用 `keyPanel.openForm()`；
  - `KeyCheckUnavailable`：「无法连接新网关校验 Key，请检查地址或网络」；
  - `ExternalProxy`：「本地代理由外部实例提供，新 SOCKS5 地址需重启该实例后生效」；
  - `RestartCodex`：「请重启 Codex 生效」（网关变更与代理开关变更共用）。
- 状态灯区域直接读新的 `gateway_addr` / `socks5_addr`，不用改。
- `types.ts`、`api.ts` 补充类型和调用；`dev-mock.ts` 增加 `set_endpoints` 的 mock。

## 5. 测试

Rust 单元测试：

- `endpoints.rs`：合法和非法输入的表格测试，覆盖 scheme、userinfo、query/fragment、IPv6、端口边界、默认端口、末尾斜杠、控制字符，以及 `resolve` 的默认值回落；
- `rules.rs`：现有测试改用新签名。新增：自定义主机恒直连，同时旧默认 IP 不再恒直连；
- `codex_env.rs`、`codex_config.rs`：用自定义值生成内容；值不一致时判定为未受管，对账后被校正；
- `state.rs`：旧 `state.json` 缺少新字段时能正常反序列化，取 `None`；
- `proxy.rs`：更新 `Routing` 后，新连接使用新 SOCKS5 地址。

Rust 集成测试（`tests/manager.rs`，用假网关和假 SOCKS5）：

- 启用状态下 `set_endpoints`：`config.toml` 的 `base_url`、`.env` 的 `NO_PROXY` 都更新，`Status` 返回新值，带 `RestartCodex` 警告；
- SOCKS5 热更新：起两个假 SOCKS5，切换后新连接打到第二个，代理句柄与监听端口不变；
- Key 校验：新网关返回 401 得到 `KeyRejected`，地址不可达得到 `KeyCheckUnavailable`，两种情况保存都成功；
- 非法输入：返回 `InvalidEndpoint`，state 和文件都不变；
- 恢复默认：state 字段变回 `None`，`endpoints_customized` 为 false；
- 重启 Manager 后从 state 恢复自定义值；
- 停用状态下保存：只写 state，不碰 `config.toml` / `.env`。

前端（`node:test`）：

- 警告到提示文案的映射；
- 编辑中不覆盖输入的判定。

手工验证：macOS 开发环境启动应用，修改两个地址后检查 `~/.codex/config.toml`、`~/.codex/.env` 与状态灯；恢复默认后再检查一次。

## 6. 文档同步

- `2026-09-23-codex-helper-design.md` §1.1：网关与 SOCKS5 两行注明“默认值，可在界面修改，见本设计”；补一行说明本地代理可由用户关闭。
- `2026-09-23-codex-helper-design.md` §1.2：把“不支持修改网关、SOCKS5、端口或规则”改为只限制本地代理端口与分流规则，并指向本设计。
- 实施完成后：本设计“状态”行如实更新；实施计划回填勾选框并追加执行记录。

## 7. 本地代理开关

### 7.1 存储

`HelperState` 新增：

```rust
/// 用户关闭了本地代理（缺省 false = 开启，旧状态文件行为不变）
pub proxy_disabled: bool,
```

字段用否定式，这样 serde 的默认值 `false` 就等于“开启”，不需要自定义 default 函数。

### 7.2 与受管开关的关系

两个开关互相独立：受管开关决定是否接管 `config.toml` 和 `.env`；代理开关只决定接管时要不要启动代理。

| 受管 | 代理 | `config.toml` | `.env` 受管块 | 本地代理 |
| --- | --- | --- | --- | --- |
| 开 | 开 | 接管 | 写入 | 运行 |
| 开 | 关 | 接管 | 不写；已有就删除 | 不启动；已启动就停止 |
| 关 | 任意 | 还原 | 删除 | 停止 |

受管模式关闭时，代理开关只作为偏好保存在 state 里。

### 7.3 核心改动

- `enable`、`reconcile_locked`、`reconcile_on_startup`：期望的 `.env` 状态和代理状态都按 `proxy_disabled` 计算。代理关闭时，`env_managed` 的判定改为“受管块不存在”。
- 残留判定（`residue`）：受管开、代理关时，`.env` 受管块不算残留，由对账删除。
- 新接口：

  ```rust
  pub async fn set_proxy_enabled(&self, enabled: bool)
      -> Result<SetEndpointsOutcome, ManagerError>; // 复用 outcome：status + warnings
  ```

  Tauri 命令 `set_proxy_enabled`。在 ops 锁内依次：值没变直接返回；写 state；受管开时启动或停止代理（复用现有 `ensure_proxy` / 停止路径），写入或删除 `.env` 受管块；刷新可达性。受管开且 `.env` 有变化时追加 `RestartCodex`。关闭时如果代理是 External，停不了它，追加 `ExternalProxy`，`.env` 照样删除。
- Secure 互斥：Secure 激活状态下拒绝关闭代理，返回新增的 `ManagerError::SecureRequiresProxy`；代理关闭时 `activate_secure` 同样返回这个错误。
- 可达性：代理关闭时不检测 SOCKS5。`Reachability` 新增 `Disabled` 变体（前端 `types.ts` 同步）。
- `Status` 新增 `proxy_enabled: bool`。
- 组件更新检查：`network_path_for` 在代理关闭时返回 `Direct { reason: Some("本地代理未开启，已直连") }`，和现有的“本地代理未运行，已直连”区分开。`network_path_for` 只看 `Status`，通过新增的 `proxy_enabled` 判断。
- 组件下载：不改，沿用现有 `downloadProxy` 错误“请先启用本地代理，再下载组件（不会回退直连）”。

### 7.4 界面

- “网关设置”区加一个“启用本地代理”开关，复用 `switchcontrol.ts`，调用 `set_proxy_enabled`，`busy` 键为 `"proxy"`。
- 代理关闭时，SOCKS5 输入框置灰，但仍可修改、保存，值留到下次开启时使用；状态灯显示“未启用”。
- 警告文案复用 §4 的 `RestartCodex`；关闭代理时的 `ExternalProxy` 改用“本地代理由外部实例提供，需要手动停止该实例”（前端按触发操作选文案）。
- `SecureRequiresProxy` 显示“Secure 模式需要本地代理，请先停用 Secure”。

### 7.5 测试

- 开 → 关（受管开）：`.env` 受管块被删、代理句柄释放、端口空出来，`config.toml` 不变，带 `RestartCodex`；
- 关 → 开（受管开）：受管块写回，代理开始监听；
- 受管关时切换开关：只写 state，不碰文件，不启动代理；
- 代理关闭状态下启动对账：残留的受管块被删，`residue` 为 false，代理不启动；
- 代理关闭状态下 `enable`：接管 `config.toml`，不写 `.env`，不启动代理；
- Secure 互斥的两个方向；
- 旧 `state.json` 反序列化后 `proxy_disabled == false`；
- `network_path_for`：代理关闭时返回带新原因的 `Direct`；
- 前端：代理关闭时 SOCKS5 输入框置灰、状态灯显示“未启用”的判定函数。
