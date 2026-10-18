# 代理 mTLS 与进程身份绑定实施计划（第一部分：协议与核心）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地设计的前四层：统一传输抽象、SOCKS5 RFC1929 认证、TLS/mTLS 上游、本机进程身份门禁与策略，及服务端 mihomo-auth 配置与签发脚本。

**Architecture:** helper-core 在现有本地代理（`127.0.0.1:17891`）内先做进程身份门禁再分流；OpenAI 流量经 TLS+mTLS+SOCKS5 密码到新端口 7892（独立 mihomo 实例）。Legacy 无认证路径保留但受模式开关控制。

**Tech Stack:** Rust（tokio、tokio-rustls 0.6、rustls 0.23、rustls-pemfile 2、ed25519-dalek 2、macOS libproc 0.14、windows crate 扩展 feature）；测试用 rcgen 0.13（dev）；服务端 bash + docker compose。

**Spec:** `docs/superpowers/specs/2026-10-16-proxy-mtls-process-binding-design.md`（下称"设计"）。执行者必须同时读 spec 与本计划；冲突以 spec 为准。

**范围裁定（写计划时的裁决）:**
- 本计划只覆盖后端协议/核心/服务端工件与状态机。前端界面呈现、托盘交互、真实设备注册运营（审批、分发渠道）、灰度部署执行为第二份计划，需另行确认后编写。
- Task 10 在 `mihomo-docker` 仓库执行；其余任务在 `CodexHelper`。实际部署、防火墙、旧端口收口由用户手动执行，不在本计划。

## Global Constraints

- 提交：分支 `pi/` 前缀；提交信息末尾 `Co-Authored-By: Pi`；**不 push**。代码注释与文档用中文。
- fail-closed 铁律：TLS/密码/身份任何失败不回退直连、不回退 7891、不重试明文；身份查不到/超时/多候选一律拒绝且不产生任何出站连接。
- 秘密纪律：密码/私钥不进日志、错误信息、Debug 输出；统一用 `Secret` 包装；macOS 测试绝不读写生产 Keychain 条目 `codex-helper/managed-gateway`，测试 target 一律带随机后缀。
- 协议字面值：认证问候 `[0x05,0x01,0x02]`；服务端方法应答必须 `0x05 0x02`，选 `0x00` 视为攻击/降级并失败；RFC1929 应答必须 `0x01 0x00`；用户名/密码各 1–255 字节。
- TLS：最低 1.2、优先 1.3；必须校验服务端证书与 ServerName；仅信任注册包下发的 CA；不实现会话恢复旁路。
- 新依赖仅限：tokio-rustls、rustls、rustls-pki-types、rustls-pemfile、ed25519-dalek（+ dev: rcgen）、libproc（仅 macOS）、windows（扩 feature）。不得引入其他网络/TLS 库。
- Windows 门禁 `scripts/check-windows.sh` 必须保持全绿；每任务收尾跑 `cargo test -p helper-core`（及涉及的 apps）。
- mihomo 固定版本 v1.19.30 字段：`client-auth-type: require-and-verify`、`client-auth-cert`、`certificate`/`private-key`、`users`；listener `type: socks`、`udp: false`。
- 服务端新增常量：`SOCKS5_AUTH_PORT = 7892`；秘密 target 前缀 `codex-helper/proxy-password/`、`codex-helper/proxy-key/`（后接 device-id）。

## Review Focus（跨任务风险与钉死测试）

1. 服务端在 TLS 内选择 SOCKS5 无认证方法 `0x00` → 客户端必须失败，绝不继续 CONNECT。→ Task 4 测试 `server_offering_no_auth_must_fail`。
2. 证书错误/SAN 不匹配 → 必须在发送任何 SOCKS5 字节前失败。→ Task 4 测试 `bad_cert_fails_before_password_sent`（假服务端断言未收到认证字节）。
3. 身份解析失败/超时/多候选 → 拒绝且零出站连接。→ Task 7 测试 `resolver_error_denies_without_upstream`。
4. 直连路由同样先过门禁（非 OpenAI 域名≠豁免）。→ Task 7 测试 `direct_route_also_gated`。
5. Secure 模式下 External 代理复用被禁用；端口被占 → 激活失败。→ Task 9 测试 `secure_mode_never_reuses_external`、`port_occupied_blocks_activation`。

---

### Task 1: 上游传输抽象（重构，无行为变化）

**Files:**
- Modify: `crates/helper-core/src/proxy.rs`（`connect_upstream`、`tunnel`、`forward_plain`、`PendingRecord` 相关）
- Test: 现有 `crates/helper-core/tests/proxy.rs` 全部保持不变且通过

**Interfaces（本任务产出，后续任务依赖）:**
- `pub type BoxedTransport = Box<dyn AsyncRead + AsyncWrite + Unpin + Send>;`（放在 `proxy.rs` 顶部公开）
- `async fn connect_upstream(...) -> anyhow::Result<BoxedTransport>`
- `async fn tunnel(client: &mut TcpStream, upstream: BoxedTransport, extra: &[u8])`
- `async fn forward_plain(client: &mut TcpStream, upstream: BoxedTransport, head: &[u8], extra: &[u8])`
- TCP 特有设置（`set_nodelay`）在装箱前完成。

- [ ] **Step 1:** 跑基线：`cargo test -p helper-core`，确认全绿，记录数量。
- [ ] **Step 2:** 重构上述四个函数为 `BoxedTransport` 泛化；`copy_bidirectional`/`split` 相应调整；不改变任何外部行为、日志事件名、错误文案。
- [ ] **Step 3:** `cargo test -p helper-core` 全绿（数量与基线一致）；`cargo clippy -p helper-core` 无新告警。
- [ ] **Step 4:** Commit `refactor(core): 上游连接抽象为 BoxedTransport`。

### Task 2: SOCKS5 RFC1929 用户名密码认证（纯函数 + 握手）

**Files:**
- Modify: `crates/helper-core/src/proxy.rs`
- Test: `crates/helper-core/tests/proxy.rs`（单测在 `proxy.rs` 内 `#[cfg(test)]`，沿用现有风格）

**Interfaces:**
- `pub fn build_socks5_auth_greeting() -> [u8; 3]`（返回 `[0x05,0x01,0x02]`）
- `pub fn parse_socks5_method_reply_auth(bytes: &[u8]) -> anyhow::Result<()>`（`0x05 0x02` 才 Ok；`0x00` 报"上游拒绝认证降级"错误）
- `pub fn build_socks5_auth_request(username: &str, password: &Secret) -> anyhow::Result<Vec<u8>>`（`01 ULEN USER PLEN PASSWD`；1–255 字节校验；错误信息不含凭据）
- `pub fn parse_socks5_auth_reply(bytes: &[u8]) -> anyhow::Result<()>`（`01 00` 才 Ok）
- 私有 `async fn socks5_auth_handshake(stream, username, password, connect_request)`：认证问候→方法应答→认证子协商→CONNECT（复用现有 CONNECT 解析）。

- [ ] **Step 1:** 失败测试（单测）：greeting 字节、方法应答 `0x00` 拒绝、auth 应答非 `01 00` 拒绝、超长用户名/密码报错、编码 roundtrip（固定向量 `("user","pass")` 的完整字节序列断言）。
- [ ] **Step 2:** 跑测确认按预期失败。
- [ ] **Step 3:** 实现五个函数与握手；错误信息只含阶段描述。
- [ ] **Step 4:** 集成测试：在 `tests/proxy.rs` 增加"要求认证的假 SOCKS5"（正确凭据成功 CONNECT、错误密码服务端断开且客户端报错、不回退直连）。
- [ ] **Step 5:** `cargo test -p helper-core` 全绿；Commit `feat(core): SOCKS5 RFC1929 用户名密码认证`。

### Task 3: TLS/mTLS 上游通道

**Files:**
- Create: `crates/helper-core/src/upstream.rs`
- Modify: `crates/helper-core/src/lib.rs`（`pub mod upstream;`）、根 `Cargo.toml`（新依赖 + dev `rcgen`）
- Test: `upstream.rs` 内单测 + `crates/helper-core/tests/upstream_tls.rs`

**Interfaces:**
- `pub struct TlsUpstreamConfig { pub server_name: String, pub ca_pem: Vec<u8>, pub client_cert_pem: Vec<u8>, pub client_key_pem: Secret }`
- `impl TlsUpstreamConfig { pub fn build_connector(&self) -> anyhow::Result<tokio_rustls::TlsConnector> }`（roots 仅含 ca_pem；`with_client_auth_cert`；TLS1.3 优先、最低 1.2；`dangerous` 配置禁止）
- `pub async fn wrap_tls(conn: tokio::net::TcpStream, cfg: &TlsUpstreamConfig) -> anyhow::Result<tokio_rustls::client::TlsStream<TokioTcpStream>>`

- [ ] **Step 1:** dev 依赖 rcgen；测试夹具：`test_ca()` 生成 CA + 服务端证书（SAN `localhost`）+ 客户端证书，产出 PEM。
- [ ] **Step 2:** 失败测试：正确配置握手成功（读完 1 字节应用数据回显）；信任锚不含签发 CA → 握手失败；SAN 不匹配（连接用 `server_name="evil"`）→ 失败；无客户端证书被要求 mTLS 的服务端拒绝。
- [ ] **Step 3:** 实现 `upstream.rs`；错误信息不含 PEM/密钥内容。
- [ ] **Step 4:** `cargo test -p helper-core` 全绿；`scripts/check-windows.sh` 绿；Commit `feat(core): TLS/mTLS 上游通道`。

### Task 4: 组合握手（TCP→TLS→认证→CONNECT）与 ProxyConfig 扩展

**Files:**
- Modify: `crates/helper-core/src/upstream.rs`、`proxy.rs`、`consts.rs`
- Test: `crates/helper-core/tests/upstream_tls.rs` 扩展

**Interfaces:**
- `pub struct SecureUpstream { pub host: String, pub port: u16, pub tls: TlsUpstreamConfig, pub username: String, pub password: Secret }`
- `pub async fn connect_secure(up: &SecureUpstream, host: &str, port: u16) -> anyhow::Result<crate::proxy::BoxedTransport>`：TCP（超时沿用 `SOCKS5_TIMEOUT`）→ `wrap_tls` → Task 2 认证握手 → CONNECT → 装箱。
- `ProxyConfig` 增 `pub secure_upstream: Option<Arc<SecureUpstream>>`（`Default` 为 `None`，`Debug` 脱敏）；`connect_upstream` 的 `Route::Socks5` 分支：`Some` 走 `connect_secure`，`None` 走旧路径。
- `consts.rs`：`pub const SOCKS5_AUTH_PORT: u16 = 7892;`

- [ ] **Step 1:** 失败测试（真 TLS 假 SOCKS5 服务端，rustls 服务端 + 认证逻辑）：
  - `secure_roundtrip_succeeds`：证书+密码正确，隧道回显；
  - `wrong_password_fails_closed`；
  - `server_offering_no_auth_must_fail`（服务端选 `0x00`，客户端必须报错且不发送 CONNECT）；
  - `bad_cert_fails_before_password_sent`（服务端记收到字节，断言认证请求未发出）；
  - `udp_associate_not_offered`（客户端不存在非 CONNECT 命令路径，代码断言）。
- [ ] **Step 2:** 确认失败；实现 `SecureUpstream::connect_secure` 与 ProxyConfig 接线。
- [ ] **Step 3:** 全量 `cargo test -p helper-core` 绿；现有 Legacy 路径测试不变绿。
- [ ] **Step 4:** Commit `feat(core): TLS+mTLS+SOCKS5 密码组合上游`。

### Task 5: 对端进程身份解析（平台实现）

**Files:**
- Create: `crates/helper-core/src/peer_identity/mod.rs`、`peer_identity/macos.rs`、`peer_identity/windows.rs`（其余平台逻辑并入 mod.rs 的拒绝实现）
- Modify: `lib.rs`、根 `Cargo.toml`（macOS `libproc`；windows feature 扩 `Win32_NetworkManagement_IpHelper`、`Win32_System_Threading`、`Win32_System_ProcessStatus`）
- Test: `peer_identity` 内单测 + `crates/helper-core/tests/peer_identity_macos.rs`（`cfg(target_os="macos")`）

**Interfaces:**
- `pub struct PeerInfo { pub pid: i32, pub user_scope: String, pub start_time_ms: u64, pub image_path: String }`（macOS `user_scope = uid` 十进制；Windows 为 SID 字符串）
- `pub fn current_user_scope() -> String`
- `pub trait PeerResolver: Send + Sync { fn resolve(&self, local: (IpAddr, u16), remote: (IpAddr, u16)) -> anyhow::Result<PeerInfo>; }`
- `pub fn system_resolver() -> Box<dyn PeerResolver>`（mac/windows 真实现；其他平台恒 `Err`）
- macOS 实现：`proc_listallpids` + `proc_pidfdinfo`(`PROC_PIDFDSOCKETINFO`) 匹配四元组，取 `proc_pidpath` 与 `proc_pidinfo` 启动时间/uid；必须排除自身监听 socket 的误匹配（比对本地/远端二元组方向）。
- Windows 实现：`GetExtendedTcpTable`(OWNER_PID_ALL) 匹配；SID 经 `OpenProcess`+token；此任务只需编译通过 + `#[ignore]` 的真机测试。

- [ ] **Step 1:** 失败测试（macOS 实机，本机可跑）：起本地 listener，自连，`resolve` 返回的 pid/uid/image_path 与 `std::process::id()`/当前 uid/当前 exe 一致；不存在的四元组 → `Err`；`current_user_scope` 与解析结果一致。
- [ ] **Step 2:** 实现平台模块；Windows 侧写 `#[ignore]` 镜像测试。
- [ ] **Step 3:** `cargo test -p helper-core` 绿 + `scripts/check-windows.sh` 绿（Windows 目标编译）。
- [ ] **Step 4:** Commit `feat(core): 对端进程身份解析（macOS/Windows）`。

### Task 6: 进程策略与签名验证（纯逻辑）

**Files:**
- Create: `crates/helper-core/src/process_policy.rs`
- Modify: `lib.rs`、`Cargo.toml`（ed25519-dalek）
- Test: `process_policy.rs` 内单测

**Interfaces:**
- `pub struct CodeImage { pub sha256_hex: String, pub os: String, pub arch: String }`
- `pub struct AllowedRole { pub role: String, pub images: Vec<CodeImage>, pub path_prefixes: Vec<String> }`（`role` 本版仅 `"codex-engine"`）
- `pub struct ProcessPolicy { pub policy_id: String, pub version: u64, pub not_after_unix: u64, pub roles: Vec<AllowedRole> }`
- `pub fn parse_and_verify(bundle_json: &[u8], signature: &[u8], verify_key: &ed25519_dalek::VerifyingKey) -> anyhow::Result<ProcessPolicy>`：签名覆盖 `serde_json::to_vec(&value)` 规范字节（BTreeMap 序）；过期/版本回退拒绝。
- `pub enum Decision { Allow { role: String }, Deny { reason: &'static str } }`
- `pub fn decide(policy: &ProcessPolicy, peer: &PeerInfo, image_sha256_hex: &str) -> Decision`：user_scope 不等 → Deny("user")；哈希+平台+架构未命中且路径前缀不匹配 → Deny("image")。
- `pub struct PolicyCache`：`fn get(&self, key: (i32,u64,String,u64)) -> Option<Decision>` / `fn put(...)`；键含 pid、start_time_ms、image_sha、policy.version。
- `pub fn hash_image_file(path: &Path) -> anyhow::Result<String>`（SHA-256 hex；用 ed25519-dalek 引入的 `sha2` 依赖）。

- [ ] **Step 1:** 失败单测：签名验证成功/篡改拒绝/过期拒绝；decide 各拒绝分支（用户、未知哈希、路径不匹配、命中放行）；缓存键失效（同 pid 不同 start_time → miss）。
- [ ] **Step 2:** 实现；`Debug` 不输出策略原文以外敏感内容（策略是公开文档，可输出）。
- [ ] **Step 3:** 全绿；Commit `feat(core): 管理签名进程策略`。

### Task 7: 连接门禁接入本地代理

**Files:**
- Create: `crates/helper-core/src/connection_gate.rs`
- Modify: `proxy.rs`（`handle_connection`、`ProxyConfig` 增 `pub gate: Option<Arc<ConnectionGate>>`）
- Test: `connection_gate.rs` 单测 + `tests/proxy.rs` 集成

**Interfaces:**
- `pub struct ConnectionGate { resolver: Box<dyn PeerResolver>, policy: std::sync::Arc<ProcessPolicy>, cache: std::sync::Mutex<PolicyCache> }`
- `impl ConnectionGate { pub fn new(resolver: Box<dyn PeerResolver>, policy: Arc<ProcessPolicy>) -> Self; pub fn admit(&self, peer: &PeerInfo) -> Decision }`（内部先查缓存，miss 时 `hash_image_file`+`decide`+入缓存）
- `handle_connection`：若 `gate` 存在——自识别端点（现有 `is_self_identify_request`）先放行返回固定标识；其余请求在解析 target 与任何上游/直连之前：`tokio::time::timeout(Duration::from_secs(1), spawn_blocking(resolve))`，失败/超时/`Err` → 403 关闭，零出站；`user_scope` 不符 → 403；Deny → 403；Allow → 正常路径。
- 403 响应体固定最小文案；新增日志事件 `proxy.gate_denied`（含 reason，不含 peer 敏感信息）。

- [ ] **Step 1:** 失败集成测试（fake resolver + 内联策略）：
  - `allowed_peer_passes`（直连路由用 direct_overrides 验证连通）；
  - `denied_peer_gets_403`；
  - `resolver_error_denies_without_upstream`（direct_overrides 目标记录器断言零连接）；
  - `direct_route_also_gated`（github.com 域名 + 拒绝身份 → 403）；
  - `self_identify_survives_gate`；
  - `gate_none_keeps_legacy_behavior`（现有测试即覆盖，勿破坏）。
- [ ] **Step 2:** 实现 gate 与接线；缓存仅缓存 Allow/Deny 决策，resolve 不缓存。
- [ ] **Step 3:** 全量测试绿（含 Legacy 回归）；Commit `feat(core): 本机进程身份门禁接入代理`。

### Task 8: 注册包与代理秘密存储

**Files:**
- Create: `crates/helper-core/src/proxy_identity.rs`
- Modify: `lib.rs`、`consts.rs`
- Test: `proxy_identity.rs` 单测（内存 store + macOS 随机 target 真钥匙串小用例）

**Interfaces:**
- `pub struct EnrollmentBundle { pub device_id: String, pub server_host: String, pub server_port: u16, pub server_name: String, pub ca_pem: String, pub client_cert_pem: String, pub policy_bundle_json: String, pub policy_signature: String, pub not_before_unix: u64, pub not_after_unix: u64 }`
- `pub fn parse_enrollment(json: &[u8]) -> anyhow::Result<EnrollmentBundle>`（字段校验：device_id 非空、端口>0、PEM 块存在）
- `pub struct ProxySecrets { pub username: String, pub password: Secret, pub client_key_pem: Secret }`
- `pub fn save_secrets(device_id: &str, secrets: &ProxySecrets) -> anyhow::Result<()>` / `pub fn load_secrets(device_id: &str) -> anyhow::Result<ProxySecrets>` / `pub fn delete_secrets(device_id: &str)`：经 `SystemCredentialStore::new(target)`，target = `consts::PROXY_PASSWORD_TARGET_PREFIX + device_id` 与 `PROXY_KEY_TARGET_PREFIX + device_id`；沿用字符/长度校验，超限报错不截断。
- `consts.rs`：两个前缀常量；`pub const POLICY_VERIFY_KEY_HEX: &str = "<ed25519 公钥 hex>"`（先放测试 CA 公钥，发布前替换为管理公钥——代码注释写明）。

- [ ] **Step 1:** 失败单测：注册包解析（合法/缺字段/坏 PEM）；秘密 roundtrip 用 `SystemCredentialStore::new("codex-helper-test-<rand>/...")`（macOS 真钥匙串、随机后缀、结束删除）；密码超 255 字节拒绝。
- [ ] **Step 2:** 实现；绝不触碰 `managed-gateway` target。
- [ ] **Step 3:** 全绿；Commit `feat(core): 注册包解析与代理秘密存储`。

### Task 9: Manager Secure 模式（独占、激活、排空）

**Files:**
- Modify: `crates/helper-core/src/manager.rs`、`state.rs`、`types.rs`（Status 增字段）
- Test: `crates/helper-core/tests/manager.rs` 扩展

**Interfaces:**
- `pub enum ProxyMode { Legacy, Secure }`（state.json 持久化，缺省 Legacy）
- `Manager::activate_secure(&self, bundle_path: &Path) -> Result<Status, ManagerError>`：parse_enrollment → parse_and_verify（用 `POLICY_VERIFY_KEY_HEX`）→ load_secrets → 构造 `SecureUpstream`+`ConnectionGate`（system_resolver）→ **先关停现有 proxy handle（排空）** → 以 Secure 配置 `spawn`；端口被外部进程占用（spawn 返回 PortUnavailable 或探测命中非本进程）→ 激活失败，不进入 External 复用。
- `ensure_proxy` / 启动对账：`mode == Secure` 时**删除** `probe_existing` 复用分支——端口被占一律报 `PortUnavailable`，提示退出旧实例。
- `Status` 增：`secure: { active: bool, device_id: Option<String>, blocked_reason: Option<String> }`（前端字段预留，本任务不改 TS 界面）。
- 回滚：`deactivate_secure()` → 停 handle → 模式回 Legacy → 按现有 Legacy 逻辑重启。

- [ ] **Step 1:** 失败测试（内存凭据 + fake TLS 上游 + fake resolver）：
  - `secure_mode_never_reuses_external`（预先占住 17891 的假探测应答器存在时，激活/对账失败且状态非 External）；
  - `port_occupied_blocks_activation`；
  - `activation_uses_own_handle_and_drains_legacy`（激活后旧 handle shutdown 被调用一次）;
  - `bad_signature_rejects_activation`；
  - Legacy 全链路回归不变。
- [ ] **Step 2:** 实现状态机与接线；日志事件 `manager.secure_activated` / `manager.secure_blocked`。
- [ ] **Step 3:** `cargo test -p helper-core` 与 `cargo test`（workspace）绿；`scripts/check-windows.sh` 绿。
- [ ] **Step 4:** Commit `feat(core): Manager Secure 模式与端口独占`。

### Task 10: 服务端 mihomo-auth 实例与签发脚本（mihomo-docker 仓库）

**Repo/cwd:** `/Users/sxk/Documents/code/ai-space/mihomo-docker`；分支 `pi/mihomo-auth`；**只 `git add` 本任务新建/修改的明确文件**，工作区已有的无关改动不得入库。

**Files:**
- Create: `config-auth/config.template.yaml`（listeners 生成模板）、`config-auth/ruleset/openai.yaml`（从 `config/ruleset/openai.yaml` 复制的快照）、`scripts/issue_device_cert.sh`、`scripts/gen_mihomo_auth_config.sh`、`scripts/verify_mihomo_auth.sh`、`docs/mihomo-auth.md`
- Modify: `docker-compose.yml`（追加 `mihomo-auth` 服务：独立容器名/配置目录 `./config-auth:/root/.config/mihomo`、端口 `0.0.0.0:7892:7892`、网络 `egress-net`、复用 entrypoint、`restart: unless-stopped`、同日志策略；**不改既有服务**）
- Test: `tests/test_mihomo_auth.sh`（沿用 `tests/` 现有 bash 测试风格）

**Interfaces/行为:**
- `issue_device_cert.sh <device-id> <out-dir>`：离线客户端 CA（若 `<out-dir>/client-ca.key` 不存在则生成并提示离线保存）→ 签发带 `clientAuth` EKU 设备证书 → `openssl rand -hex 32` 生成密码 → 产出 `device.crt/key/username/password` 文件与注册包 JSON 草稿（含设备证书、CA、端点 `10.20.30.61:7892`）；服务端证书同理可生成（SAN 含 IP `10.20.30.61`）。
- `gen_mihomo_auth_config.sh`：读模板+设备用户/密码/证书路径 → 渲染 `config-auth/config.yaml`；密码不回显；渲染文件权限 600。
- listener 模板固定：`type: socks`、`port: 7892`、`udp: false`、`client-auth-type: require-and-verify`、`client-auth-cert` 指向 CA bundle、`rule: openai-restrict`（sub-rules 与现有白名单语义一致，`MATCH,REJECT` 收尾）；所有账号密码非空（SOCKS4/空密码不可绕过）。
- `verify_mihomo_auth.sh`：容器内 TLS 探测负例清单（无证书/错证书/明文 SOCKS 直连必须失败），输出 PASS/FAIL。
- `.gitignore` 增 `config-auth/*`、`!config-auth/.gitkeep`、`config-auth/ruleset/` 白名单按仓库现有模式，证书/密码产物绝不入库。

- [ ] **Step 1:** `tests/test_mihomo_auth.sh` 失败测试：`docker compose config -q` 校验合法；grep 断言 compose 不含明文秘密；`gen` 幂等（两次渲染一致）；模板含 `require-and-verify` 与 `udp: false`；`tests/` 下其余脚本回归通过（按仓库 README 的测试入口）。
- [ ] **Step 2:** 实现脚本与 compose/模板/文档（文档含：部署步骤、本地 mTLS 探测命令示例——用 `curl --cert/--key --tlsv1.2` 说明不可行、改用 openssl s_client + 手工 SOCKS5 字节的示例、收口 7891 的手动步骤索引）。
- [ ] **Step 3:** 跑 `bash tests/test_mihomo_auth.sh` 全绿；`bash tests/test_compose.sh`（如存在）绿。
- [ ] **Step 4:** Commit `feat: mihomo-auth 独立实例与设备签发脚本`（该仓库内）。

---

## 执行说明（派发工作流用）

- 任务严格串行 1→10；每任务：实现者（thread 复用做修复轮）→ 任务评审（fresh）→ 修复轮 ≤5（scoped re-review）。
- 实现者/评审者报告写入 `.superpowers/sdd/2026-10-16-proxy-mtls-process-binding/task-N-{report,review}.md`，progress 记录追加 `progress.md`。
- 评审依据 = 本计划任务文本 + Global Constraints + spec 对应章节；Minor 留 ledger，不进修复轮。
- 全部任务后做整分支终审（最高档模型），输出遗留问题清单与裁决建议。
