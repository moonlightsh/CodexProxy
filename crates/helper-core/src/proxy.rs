//! 本地分流代理 `127.0.0.1:17891`（设计 §6）。
//!
//! 移植自 CodexPlusPlus `managed_proxy.rs`：
//!
//! - 命中 OpenAI 规则 → 经上游 SOCKS5 建隧道；失败返回 502，**绝不回退直连**，不重试；
//! - 未命中 → 由本机直连（连接超时 10 秒，失败 502）；
//! - 同时支持 `CONNECT host:port` 与绝对 URI 明文请求（改写为 origin-form 转发）；
//! - 自识端点 `GET /__codex_helper_proxy_id` 用于端口复用判定；
//! - 每条连接结束后产生 [`ConnectionRecord`]，交给回调（界面环形缓冲）并按采样写日志。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::types::ConnectionRecord;

/// 自识端点路径。
pub const PROXY_ID_PATH: &str = "/__codex_helper_proxy_id";
/// 自识端点返回的固定标识。
pub const PROXY_ID_BODY: &str = "codex-helper-managed-proxy";
/// 请求头上限；超出或首行无法解析返回 400。
pub const MAX_HEADER_BYTES: usize = 32 * 1024;
/// SOCKS5 上游连接与握手各自的超时。
pub const SOCKS5_TIMEOUT: Duration = Duration::from_secs(3);
/// 直连目标的连接超时。
pub const DIRECT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 界面保留的最近连接条数。
pub const RECENT_CAPACITY: usize = 50;

/// 代理请求的两种形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyRequestKind {
    /// `CONNECT host:443 HTTP/1.1`，建隧道后不解密。
    Connect,
    /// `GET http://host:8080/path HTTP/1.1`，绝对 URI 明文转发。
    Plain,
}

/// 解析出的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTarget {
    pub host: String,
    pub port: u16,
    pub kind: ProxyRequestKind,
}

/// 解析代理请求首行；无法识别返回 `None`（调用方回 400）。
pub fn parse_proxy_request_line(line: &str) -> Option<ProxyTarget> {
    let _ = line;
    todo!("阶段 1 任务 1.7")
}

/// SOCKS5 无认证问候。
pub fn build_socks5_greeting() -> [u8; 3] {
    todo!("阶段 1 任务 1.7")
}

/// SOCKS5 CONNECT 请求，固定域名寻址（ATYP=0x03），不在本地解析 DNS。
pub fn build_socks5_connect_request(host: &str, port: u16) -> anyhow::Result<Vec<u8>> {
    let _ = (host, port);
    todo!("阶段 1 任务 1.7")
}

/// 校验方法协商应答：只接受无认证。
pub fn parse_socks5_method_reply(bytes: &[u8]) -> anyhow::Result<()> {
    let _ = bytes;
    todo!("阶段 1 任务 1.7")
}

/// 校验 CONNECT 应答：REP≠0x00 一律视为上游失败。
pub fn parse_socks5_connect_reply(bytes: &[u8]) -> anyhow::Result<()> {
    let _ = bytes;
    todo!("阶段 1 任务 1.7")
}

/// CONNECT 应答总长度（含绑定地址）；字节不足以判定时返回 `None`。
pub fn socks5_connect_reply_len(bytes: &[u8]) -> Option<usize> {
    let _ = bytes;
    todo!("阶段 1 任务 1.7")
}

/// 经指定上游 SOCKS5 建立到目标的隧道。失败返回错误，**调用方不得回退直连**。
pub async fn connect_via_socks5_at(
    upstream_host: &str,
    upstream_port: u16,
    host: &str,
    port: u16,
) -> anyhow::Result<tokio::net::TcpStream> {
    let _ = (upstream_host, upstream_port, host, port);
    todo!("阶段 1 任务 1.7")
}

/// 连接记录回调。
pub type RecordSink = Arc<dyn Fn(ConnectionRecord) + Send + Sync>;

/// 代理运行参数。上游可注入（测试），默认为固定的受管 SOCKS5。
#[derive(Clone)]
pub struct ProxyConfig {
    pub socks5_host: String,
    pub socks5_port: u16,
    /// 每条连接结束后调用
    pub on_record: Option<RecordSink>,
    /// 仅供测试：直连时把主机名（规范化后）映射到固定地址，用于验证“命中规则但上游失败时
    /// 目标未收到直连”。生产必须为 `None`；只影响直连路径，不影响分流判定与 SOCKS5 路径。
    pub direct_overrides: Option<Arc<HashMap<String, SocketAddr>>>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            socks5_host: crate::consts::SOCKS5_HOST.to_string(),
            socks5_port: crate::consts::SOCKS5_PORT,
            on_record: None,
            direct_overrides: None,
        }
    }
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("socks5_host", &self.socks5_host)
            .field("socks5_port", &self.socks5_port)
            .field("on_record", &self.on_record.as_ref().map(|_| "<fn>"))
            .field("direct_overrides", &self.direct_overrides)
            .finish()
    }
}

/// 最近连接的内存环形缓冲（容量 [`RECENT_CAPACITY`]），可克隆共享。
#[derive(Clone, Default)]
pub struct RecentConnections {
    inner: Arc<Mutex<VecDeque<ConnectionRecord>>>,
}

impl RecentConnections {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条；超出容量时丢弃最旧的。
    pub fn push(&self, record: ConnectionRecord) {
        let _ = (&self.inner, record);
        todo!("阶段 1 任务 1.7")
    }

    /// 快照，最新的在前。
    pub fn snapshot(&self) -> Vec<ConnectionRecord> {
        todo!("阶段 1 任务 1.7")
    }
}

/// 启动失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ProxyStartError {
    /// 端口被占用或不可绑定（含 Windows 保留端口导致的拒绝访问）
    #[error("端口 {port} 不可用：{reason}")]
    PortUnavailable { port: u16, reason: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 已启动的代理。持有即保活；[`ProxyHandle::shutdown`] 或 drop 时停止监听并中断在途连接。
pub struct ProxyHandle {
    port: u16,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for ProxyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyHandle")
            .field("port", &self.port)
            .finish()
    }
}

impl ProxyHandle {
    /// 实际监听的端口。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 停止监听并中断所有在途连接；返回时端口已释放，可立即重新绑定。
    pub async fn shutdown(self) {
        let _ = &self.task;
        todo!("阶段 1 任务 1.7")
    }
}

/// 解析生效端口：`CODEX_HELPER_PROXY_PORT` 可覆盖，非法值（含 0）回落默认 17891。
pub fn proxy_port() -> u16 {
    proxy_port_from(std::env::var(crate::consts::PROXY_PORT_ENV).ok().as_deref())
}

/// [`proxy_port`] 的纯函数版本。
pub fn proxy_port_from(value: Option<&str>) -> u16 {
    let _ = value;
    todo!("阶段 1 任务 1.7")
}

/// 探测端口上是否是本工具的代理实例（请求自识端点，2 秒超时）。
pub async fn probe_existing(port: u16) -> bool {
    let _ = port;
    todo!("阶段 1 任务 1.7")
}

/// 在 `127.0.0.1:port` 上启动代理。端口被占用返回 [`ProxyStartError::PortUnavailable`]，不换端口。
pub async fn spawn(port: u16, config: ProxyConfig) -> Result<ProxyHandle, ProxyStartError> {
    let _ = (port, config);
    todo!("阶段 1 任务 1.7")
}

/// 用现成监听器启动（测试用；`port` 取监听器实际端口）。
pub fn spawn_with_listener(listener: tokio::net::TcpListener, config: ProxyConfig) -> ProxyHandle {
    let _ = (listener, config);
    todo!("阶段 1 任务 1.7")
}
