//! 本地分流代理 `127.0.0.1:17891`（设计 §6）。
//!
//! 移植自 CodexPlusPlus `managed_proxy.rs`：
//!
//! - 命中 OpenAI 规则 → 经上游 SOCKS5 建隧道；失败返回 502，**绝不回退直连**，不重试；
//! - 未命中 → 由本机直连（连接超时 10 秒，失败 502）；
//! - 同时支持 `CONNECT host:port` 与绝对 URI 明文请求（改写为 origin-form 转发）；
//! - 自识端点 `GET /__codex_helper_proxy_id` 用于端口复用判定；
//! - 每条连接结束后产生 [`ConnectionRecord`]，交给回调（界面环形缓冲）并按采样写日志。
//!
//! 分流只调用 [`crate::rules::route_for_host`]（唯一判定点）。为防止连接复用绕过判定，
//! 每个客户端连接只服务一个请求：明文请求强制 `Connection: close`，上游响应结束即关闭客户端连接。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::consts;
use crate::rules::{self, Route};
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

/// 自识探测的整体超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// 自识探测最多读取的应答字节数。
const PROBE_MAX_RESPONSE_BYTES: u64 = 4096;
/// accept 出错后的退避时间（如句柄耗尽），之后继续监听。
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// 读取请求头时的单次读取块大小。
const READ_CHUNK_BYTES: usize = 8 * 1024;
/// 发出错误应答后排空客户端输入的最长时间与字节上限。
const LINGER_TIMEOUT: Duration = Duration::from_secs(1);
const LINGER_MAX_BYTES: usize = 1024 * 1024;
/// 成功连接的日志采样：前 N 条全记，之后每 M 条记 1 条；失败全部记录。
const LOG_SAMPLE_HEAD: u64 = 20;
const LOG_SAMPLE_EVERY: u64 = 20;

const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const CONNECTION_ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
/// 明文转发前移除的逐跳头；`Connection` 随后统一改写为 `close`。
const HOP_BY_HOP_HEADERS: &[&str] = &["connection", "proxy-connection", "keep-alive"];

/// 代理请求的两种形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyRequestKind {
    /// `CONNECT host:443 HTTP/1.1`，建隧道后不解密。
    Connect,
    /// `GET http://host:8080/path HTTP/1.1`，绝对 URI 明文转发。
    Plain,
}

impl ProxyRequestKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Plain => "plain",
        }
    }
}

/// 解析出的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTarget {
    pub host: String,
    pub port: u16,
    pub kind: ProxyRequestKind,
}

/// 解析代理请求首行；无法识别返回 `None`（调用方回 400）。
///
/// 只看首行：不解析请求头，也不碰 body。
pub fn parse_proxy_request_line(line: &str) -> Option<ProxyTarget> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        return Some(ProxyTarget {
            host,
            port,
            kind: ProxyRequestKind::Connect,
        });
    }
    // 明文代理只接受绝对 URI；origin-form 说明对方把我们当普通服务器，不属代理语义。
    let rest = strip_http_scheme(target)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    let (host, port) = split_host_port(authority, 80)?;
    Some(ProxyTarget {
        host,
        port,
        kind: ProxyRequestKind::Plain,
    })
}

/// 去掉 `http://`（不区分大小写）。引擎对 HTTPS 目标一律用 CONNECT，这里不接受 https 绝对 URI。
fn strip_http_scheme(target: &str) -> Option<&str> {
    const SCHEME: &str = "http://";
    let head = target.get(..SCHEME.len())?;
    head.eq_ignore_ascii_case(SCHEME)
        .then(|| &target[SCHEME.len()..])
}

/// 拆主机与端口，兼容 IPv6 字面量 `[::1]:443`。
fn split_host_port(input: &str, default_port: u16) -> Option<(String, u16)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(rest) = input.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        let port = match tail.strip_prefix(':') {
            Some(port_text) => port_text.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_ascii_lowercase(), port));
    }
    match input.rsplit_once(':') {
        // 带端口：主机不得为空，也不得含冗余冒号（裸写 IPv6 必须带方括号）。
        Some((host, port_text)) => {
            if host.is_empty() || host.contains(':') {
                return None;
            }
            let port = port_text.parse().ok()?;
            Some((host.to_ascii_lowercase(), port))
        }
        None => Some((input.to_ascii_lowercase(), default_port)),
    }
}

/// 绝对 URI 取 path + query 作为 origin-form；authority 后直接是查询串时补 `/`，片段不发给服务器。
fn origin_form_target(uri: &str) -> Option<String> {
    let rest = strip_http_scheme(uri)?;
    let start = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let tail = rest[start..].split('#').next().unwrap_or_default();
    Some(if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    })
}

/// 把绝对 URI 首行改写成 origin-form，方法与协议版本不变。
fn origin_form_request_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let uri = parts.next()?;
    let version = parts.next().unwrap_or("HTTP/1.1");
    Some(format!("{method} {} {version}", origin_form_target(uri)?))
}

/// 改写明文代理请求头（`header` 含结尾空行）：首行改为 origin-form，移除 `Connection`、
/// `Proxy-Connection`、`Keep-Alive`（连同其折行），末尾追加 `Connection: close`。
///
/// 按字节处理，其余头原样保留（包括非 UTF-8 的头值）。
fn rewrite_plain_request_head(header: &[u8]) -> Option<Vec<u8>> {
    let block = header.strip_suffix(b"\r\n\r\n")?;
    let mut lines = split_crlf(block);
    let request_line = origin_form_request_line(std::str::from_utf8(lines.next()?).ok()?)?;

    let mut rewritten = Vec::with_capacity(header.len() + 32);
    rewritten.extend_from_slice(request_line.as_bytes());
    rewritten.extend_from_slice(b"\r\n");
    let mut dropping = false;
    for line in lines {
        // obs-fold 续行跟随上一个头的去留
        let is_continuation = matches!(line.first(), Some(b' ' | b'\t'));
        if !is_continuation {
            let name = line
                .split(|byte| *byte == b':')
                .next()
                .unwrap_or_default()
                .trim_ascii();
            dropping = HOP_BY_HOP_HEADERS
                .iter()
                .any(|hop| name.eq_ignore_ascii_case(hop.as_bytes()));
        }
        if !dropping {
            rewritten.extend_from_slice(line);
            rewritten.extend_from_slice(b"\r\n");
        }
    }
    rewritten.extend_from_slice(b"Connection: close\r\n\r\n");
    Some(rewritten)
}

/// 按 `\r\n` 切分字节串。
fn split_crlf(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(bytes);
    std::iter::from_fn(move || {
        let current = rest?;
        match find_subsequence(current, b"\r\n") {
            Some(index) => {
                rest = Some(&current[index + 2..]);
                Some(&current[..index])
            }
            None => {
                rest = None;
                Some(current)
            }
        }
    })
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// SOCKS5 无认证问候。
pub fn build_socks5_greeting() -> [u8; 3] {
    // VER=5, NMETHODS=1, METHOD=0x00
    [0x05, 0x01, 0x00]
}

/// SOCKS5 CONNECT 请求，固定域名寻址（ATYP=0x03），不在本地解析 DNS。
pub fn build_socks5_connect_request(host: &str, port: u16) -> anyhow::Result<Vec<u8>> {
    let host_bytes = host.as_bytes();
    if host_bytes.is_empty() {
        bail!("SOCKS5 目标主机为空");
    }
    let host_len = u8::try_from(host_bytes.len())
        .map_err(|_| anyhow!("SOCKS5 目标主机过长：{} 字节", host_bytes.len()))?;
    let mut request = Vec::with_capacity(7 + host_bytes.len());
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_len]);
    request.extend_from_slice(host_bytes);
    request.extend_from_slice(&port.to_be_bytes());
    Ok(request)
}

/// 校验方法协商应答：只接受无认证。
pub fn parse_socks5_method_reply(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() < 2 {
        bail!("SOCKS5 方法应答长度不足");
    }
    if bytes[0] != 0x05 {
        bail!("SOCKS5 版本不支持：0x{:02x}", bytes[0]);
    }
    if bytes[1] != 0x00 {
        bail!("SOCKS5 上游要求认证方法 0x{:02x}", bytes[1]);
    }
    Ok(())
}

/// 校验 CONNECT 应答：REP≠0x00 一律视为上游失败。
pub fn parse_socks5_connect_reply(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() < 2 {
        bail!("SOCKS5 CONNECT 应答长度不足");
    }
    if bytes[0] != 0x05 {
        bail!("SOCKS5 版本不支持：0x{:02x}", bytes[0]);
    }
    match bytes[1] {
        0x00 => Ok(()),
        code => bail!("SOCKS5 上游拒绝：{}", socks5_reply_message(code)),
    }
}

fn socks5_reply_message(code: u8) -> &'static str {
    match code {
        0x01 => "上游一般性故障",
        0x02 => "规则不允许",
        0x03 => "网络不可达",
        0x04 => "主机不可达",
        0x05 => "连接被拒绝",
        0x06 => "TTL 过期",
        0x07 => "不支持的命令",
        0x08 => "不支持的地址类型",
        _ => "未知错误",
    }
}

/// CONNECT 应答总长度（含绑定地址）；字节不足以判定时返回 `None`。
pub fn socks5_connect_reply_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 5 {
        return None;
    }
    let addr_len = match bytes[3] {
        0x01 => 4,
        0x03 => 1 + usize::from(bytes[4]),
        0x04 => 16,
        _ => return None,
    };
    Some(4 + addr_len + 2)
}

/// 经指定上游 SOCKS5 建立到目标的隧道。失败返回错误，**调用方不得回退直连**。
pub async fn connect_via_socks5_at(
    upstream_host: &str,
    upstream_port: u16,
    host: &str,
    port: u16,
) -> anyhow::Result<tokio::net::TcpStream> {
    // 先构造请求：主机名为空或过长时不必连接上游
    let request = build_socks5_connect_request(host, port)?;
    let mut stream = tokio::time::timeout(
        SOCKS5_TIMEOUT,
        TcpStream::connect((upstream_host, upstream_port)),
    )
    .await
    .map_err(|_| anyhow!("SOCKS5 上游连接超时"))?
    .context("SOCKS5 上游连接失败")?;
    tokio::time::timeout(SOCKS5_TIMEOUT, socks5_handshake(&mut stream, &request))
        .await
        .map_err(|_| anyhow!("SOCKS5 握手超时"))??;
    Ok(stream)
}

async fn socks5_handshake(stream: &mut TcpStream, connect_request: &[u8]) -> anyhow::Result<()> {
    stream.write_all(&build_socks5_greeting()).await?;
    let mut method_reply = [0u8; 2];
    stream.read_exact(&mut method_reply).await?;
    parse_socks5_method_reply(&method_reply)?;

    stream.write_all(connect_request).await?;
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).await?;
    parse_socks5_connect_reply(&head)?;
    let total =
        socks5_connect_reply_len(&head).ok_or_else(|| anyhow!("SOCKS5 应答地址类型未知"))?;
    let mut rest = vec![0u8; total.saturating_sub(head.len())];
    stream.read_exact(&mut rest).await?;
    Ok(())
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
        let mut records = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        while records.len() >= RECENT_CAPACITY {
            records.pop_front();
        }
        records.push_back(record);
    }

    /// 快照，最新的在前。
    pub fn snapshot(&self) -> Vec<ConnectionRecord> {
        let records = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        records.iter().rev().cloned().collect()
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
    /// 通知监听循环退出；drop 发送端同样触发退出
    stop: Option<oneshot::Sender<()>>,
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
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            // 监听循环退出前会释放监听器并等待全部连接任务被中止
            let _ = task.await;
        }
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        // 尽力而为：丢弃发送端即通知监听循环退出，由其在后台释放端口并中止在途连接
        self.stop.take();
    }
}

/// 解析生效端口：`CODEX_HELPER_PROXY_PORT` 可覆盖，非法值（含 0）回落默认 17891。
pub fn proxy_port() -> u16 {
    proxy_port_from(std::env::var(crate::consts::PROXY_PORT_ENV).ok().as_deref())
}

/// [`proxy_port`] 的纯函数版本。
pub fn proxy_port_from(value: Option<&str>) -> u16 {
    value
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(consts::DEFAULT_PROXY_PORT)
}

/// 探测端口上是否是本工具的代理实例（请求自识端点，2 秒超时）。
pub async fn probe_existing(port: u16) -> bool {
    let probe = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let request = format!(
            "GET {PROXY_ID_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        (&mut stream)
            .take(PROBE_MAX_RESPONSE_BYTES)
            .read_to_end(&mut response)
            .await
            .ok()?;
        Some(is_identify_response(&response))
    };
    tokio::time::timeout(PROBE_TIMEOUT, probe)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// 应答是否为自识端点的 200 且 body 恰为固定标识。
fn is_identify_response(response: &[u8]) -> bool {
    let Some(end) = find_subsequence(response, b"\r\n\r\n") else {
        return false;
    };
    let status_line = split_crlf(&response[..end]).next().unwrap_or_default();
    let mut parts = status_line.split(|byte| *byte == b' ');
    let version_ok = parts
        .next()
        .is_some_and(|version| version.starts_with(b"HTTP/1."));
    let status_ok = parts.next() == Some(b"200".as_slice());
    version_ok && status_ok && &response[end + 4..] == PROXY_ID_BODY.as_bytes()
}

/// 在 `127.0.0.1:port` 上启动代理。端口被占用返回 [`ProxyStartError::PortUnavailable`]，不换端口。
pub async fn spawn(port: u16, config: ProxyConfig) -> Result<ProxyHandle, ProxyStartError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| bind_error(port, error))?;
    Ok(spawn_with_listener(listener, config))
}

/// 端口占用与拒绝访问（Windows 保留端口 / WSAEACCES）归为端口不可用，其余原样上报。
fn bind_error(port: u16, error: std::io::Error) -> ProxyStartError {
    match error.kind() {
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied => {
            ProxyStartError::PortUnavailable {
                port,
                reason: error.to_string(),
            }
        }
        _ => ProxyStartError::Io(error),
    }
}

/// 用现成监听器启动（测试用；`port` 取监听器实际端口）。必须在 tokio 运行时内调用。
pub fn spawn_with_listener(listener: tokio::net::TcpListener, config: ProxyConfig) -> ProxyHandle {
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let shared = Arc::new(Shared {
        config,
        listen_port: port,
        successes: AtomicU64::new(0),
    });
    let (stop_tx, stop_rx) = oneshot::channel();
    crate::log::event("proxy.listening", json!({ "port": port }));
    let task = tokio::spawn(serve(listener, shared, stop_rx));
    ProxyHandle {
        port,
        stop: Some(stop_tx),
        task: Some(task),
    }
}

/// 所有连接任务共享的运行参数与计数。
struct Shared {
    config: ProxyConfig,
    /// 本代理实际监听的端口，用于防自环
    listen_port: u16,
    /// 成功连接计数，用于日志采样
    successes: AtomicU64,
}

impl Shared {
    /// 交出一条连接记录：按采样写日志，再交给回调。
    fn emit(&self, kind: ProxyRequestKind, record: ConnectionRecord) {
        let should_log =
            !record.ok || sample_success(self.successes.fetch_add(1, Ordering::Relaxed));
        if should_log {
            // 只记主机、端口、形态、决策、结果与耗时；绝不记请求头、URL 路径 / 查询与 body
            crate::log::event(
                "proxy.connection",
                json!({
                    "host": record.host,
                    "port": record.port,
                    "kind": kind.as_str(),
                    "decision": record.decision,
                    "ok": record.ok,
                    "ms": record.ms,
                }),
            );
        }
        if let Some(sink) = &self.config.on_record {
            sink(record);
        }
    }
}

/// 第 `seq` 条（从 0 计）成功连接是否写日志。
fn sample_success(seq: u64) -> bool {
    seq < LOG_SAMPLE_HEAD || seq.is_multiple_of(LOG_SAMPLE_EVERY)
}

/// 待交出的连接记录：显式 [`PendingRecord::finish`] 或 drop（含任务被中止）时交出，恰好一次。
struct PendingRecord {
    shared: Arc<Shared>,
    kind: ProxyRequestKind,
    record: Option<ConnectionRecord>,
}

impl PendingRecord {
    fn finish(&mut self) {
        if let Some(record) = self.record.take() {
            self.shared.emit(self.kind, record);
        }
    }
}

impl Drop for PendingRecord {
    fn drop(&mut self) {
        self.finish();
    }
}

/// 监听循环：收到停止信号后释放监听器，并中止、等待全部在途连接任务。
async fn serve(listener: TcpListener, shared: Arc<Shared>, mut stop: oneshot::Receiver<()>) {
    let port = shared.listen_port;
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(handle_connection(stream, Arc::clone(&shared)));
                }
                Err(error) => {
                    // 不退出监听：短暂退避后继续
                    crate::log::event(
                        "proxy.accept_failed",
                        json!({ "port": port, "message": error.to_string() }),
                    );
                    tokio::select! {
                        biased;
                        _ = &mut stop => break,
                        () = tokio::time::sleep(ACCEPT_BACKOFF) => {}
                    }
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    connections.shutdown().await;
    crate::log::event("proxy.stopped", json!({ "port": port }));
}

/// 读取请求头的结果。
enum RequestHead {
    /// 完整请求头（含结尾空行）与其后已读入的多余字节（明文请求体的开头等，必须原样转发）
    Complete { header: Vec<u8>, extra: Vec<u8> },
    /// 超过 [`MAX_HEADER_BYTES`] 仍未读到空行
    TooLarge,
    /// 客户端在请求头完整前关闭
    Closed,
}

/// 按块读到 `\r\n\r\n` 为止；请求头（含结尾空行）不得超过 [`MAX_HEADER_BYTES`]。
async fn read_request_head<R: AsyncRead + Unpin>(stream: &mut R) -> std::io::Result<RequestHead> {
    let mut buffer = Vec::with_capacity(READ_CHUNK_BYTES);
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(RequestHead::Closed);
        }
        // 空行可能跨块，回退 3 字节再找
        let search_from = buffer.len().saturating_sub(3);
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_subsequence(&buffer[search_from..], b"\r\n\r\n") {
            let end = search_from + index + 4;
            if end > MAX_HEADER_BYTES {
                return Ok(RequestHead::TooLarge);
            }
            let extra = buffer.split_off(end);
            return Ok(RequestHead::Complete {
                header: buffer,
                extra,
            });
        }
        // 已读满上限仍无空行：之后出现的空行结尾必然超限
        if buffer.len() >= MAX_HEADER_BYTES {
            return Ok(RequestHead::TooLarge);
        }
    }
}

/// 请求首行（去首尾空白）；非 UTF-8 返回 `None`。
fn request_line(header: &[u8]) -> Option<&str> {
    let line = split_crlf(header).next()?;
    std::str::from_utf8(line).ok().map(str::trim)
}

fn is_self_identify_request(first_line: &str) -> bool {
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    method.eq_ignore_ascii_case("GET") && path == PROXY_ID_PATH
}

fn identify_response() -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PROXY_ID_BODY}",
        PROXY_ID_BODY.len()
    )
}

/// 目标是否就是本代理自身（回环地址 + 本代理监听端口），转发会无限自环。
fn is_self_target(target: &ProxyTarget, listen_port: u16) -> bool {
    target.port == listen_port && is_loopback_host(&target.host)
}

fn is_loopback_host(host: &str) -> bool {
    let host = rules::normalize_host(host);
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
        Err(_) => false,
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// 处理一个客户端连接：只服务一个请求（CONNECT 隧道或一次明文转发）。
async fn handle_connection(mut client: TcpStream, shared: Arc<Shared>) {
    let time = unix_millis();
    let _ = client.set_nodelay(true);

    let (header, extra) = match read_request_head(&mut client).await {
        Ok(RequestHead::Complete { header, extra }) => (header, extra),
        Ok(RequestHead::TooLarge) => return reject(client, "header_too_large").await,
        Ok(RequestHead::Closed) | Err(_) => return,
    };
    let Some(first_line) = request_line(&header) else {
        return reject(client, "bad_request_line").await;
    };

    // 自识端点：origin-form 请求，不进代理路径，不产生记录。
    if is_self_identify_request(first_line) {
        return respond_and_close(client, identify_response().as_bytes()).await;
    }

    let Some(target) = parse_proxy_request_line(first_line) else {
        return reject(client, "bad_request_line").await;
    };
    if is_self_target(&target, shared.listen_port) {
        return reject(client, "self_loop").await;
    }
    let plain_head = match target.kind {
        ProxyRequestKind::Connect => None,
        ProxyRequestKind::Plain => match rewrite_plain_request_head(&header) {
            Some(head) => Some(head),
            None => return reject(client, "bad_request_line").await,
        },
    };

    let decision = rules::route_for_host(&target.host);
    let connect_started = Instant::now();
    let upstream = connect_upstream(&shared.config, &target, decision).await;
    let mut pending = PendingRecord {
        shared: Arc::clone(&shared),
        kind: target.kind,
        record: Some(ConnectionRecord {
            time,
            host: target.host.clone(),
            port: target.port,
            decision,
            ok: upstream.is_ok(),
            ms: elapsed_millis(connect_started),
        }),
    };

    let Ok(upstream) = upstream else {
        // 命中规则却连不上上游时只能 502：回退直连会让 OpenAI 流量绕过 SOCKS5。
        pending.finish();
        return respond_and_close(client, BAD_GATEWAY).await;
    };
    let _ = upstream.set_nodelay(true);

    match plain_head {
        None => {
            tunnel(&mut client, upstream, &extra).await;
            pending.finish();
        }
        Some(head) => {
            forward_plain(&mut client, upstream, &head, &extra).await;
            pending.finish();
            // 上游响应结束即关闭客户端连接：一个连接只服务一个请求
            linger_close(client).await;
        }
    }
}

/// 按分流决策建立上游连接。SOCKS5 路径失败绝不改走直连。
async fn connect_upstream(
    config: &ProxyConfig,
    target: &ProxyTarget,
    decision: Route,
) -> anyhow::Result<TcpStream> {
    match decision {
        Route::Socks5 => {
            connect_via_socks5_at(
                &config.socks5_host,
                config.socks5_port,
                &target.host,
                target.port,
            )
            .await
        }
        Route::Direct => connect_direct(config, target).await,
    }
}

async fn connect_direct(config: &ProxyConfig, target: &ProxyTarget) -> anyhow::Result<TcpStream> {
    let override_addr = config
        .direct_overrides
        .as_ref()
        .and_then(|overrides| overrides.get(&rules::normalize_host(&target.host)))
        .copied();
    let connect = async {
        match override_addr {
            Some(addr) => TcpStream::connect(addr).await,
            None => TcpStream::connect((target.host.as_str(), target.port)).await,
        }
    };
    tokio::time::timeout(DIRECT_CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| anyhow!("直连目标超时"))?
        .context("直连目标失败")
}

/// CONNECT：回 200 后双向透传，不解密；请求头之后已读入的字节先交给上游。
async fn tunnel(client: &mut TcpStream, mut upstream: TcpStream, extra: &[u8]) {
    if client.write_all(CONNECTION_ESTABLISHED).await.is_err() {
        return;
    }
    if !extra.is_empty() && upstream.write_all(extra).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
}

/// 明文转发：发送改写后的请求头与已读入的请求体开头，随后透传，直到上游响应结束（上游关闭）。
async fn forward_plain(client: &mut TcpStream, mut upstream: TcpStream, head: &[u8], extra: &[u8]) {
    if upstream.write_all(head).await.is_err() {
        return;
    }
    if !extra.is_empty() && upstream.write_all(extra).await.is_err() {
        return;
    }
    let (mut client_read, mut client_write) = client.split();
    let (mut upstream_read, mut upstream_write) = upstream.split();
    let request = async {
        let _ = tokio::io::copy(&mut client_read, &mut upstream_write).await;
        let _ = upstream_write.shutdown().await;
    };
    let response = async {
        let _ = tokio::io::copy(&mut upstream_read, &mut client_write).await;
        let _ = client_write.shutdown().await;
    };
    tokio::pin!(request, response);
    tokio::select! {
        () = &mut response => {}
        // 客户端发完（半关闭）后继续等上游响应结束
        () = &mut request => response.await,
    }
}

/// 回 400 并关闭；只记原因，不记请求内容。
async fn reject(client: TcpStream, reason: &'static str) {
    crate::log::event("proxy.bad_request", json!({ "reason": reason }));
    respond_and_close(client, BAD_REQUEST).await;
}

/// 写出应答后关闭连接。
async fn respond_and_close(mut client: TcpStream, response: &[u8]) {
    if client.write_all(response).await.is_err() {
        return;
    }
    linger_close(client).await;
}

/// 关闭写方向后短暂排空客户端输入再关闭：带着未读数据关闭会发 RST，客户端可能丢失已发出的应答。
async fn linger_close(mut client: TcpStream) {
    let _ = client.shutdown().await;
    let drain = async {
        let mut sink = vec![0u8; READ_CHUNK_BYTES];
        let mut total = 0;
        while total < LINGER_MAX_BYTES {
            match client.read(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(read) => total += read,
            }
        }
    };
    let _ = tokio::time::timeout(LINGER_TIMEOUT, drain).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connect_with_explicit_port() {
        let target = parse_proxy_request_line("CONNECT chatgpt.com:443 HTTP/1.1").expect("parsed");
        assert_eq!(target.host, "chatgpt.com");
        assert_eq!(target.port, 443);
        assert_eq!(target.kind, ProxyRequestKind::Connect);
    }

    #[test]
    fn connect_without_port_defaults_to_443() {
        let target = parse_proxy_request_line("CONNECT auth.openai.com HTTP/1.1").expect("parsed");
        assert_eq!(target.port, 443);
    }

    #[test]
    fn parses_absolute_uri_with_port_and_query() {
        let line = "GET http://10.20.30.61:8080/models?client_version=0.154.0 HTTP/1.1";
        let target = parse_proxy_request_line(line).expect("parsed");
        assert_eq!(target.host, "10.20.30.61");
        assert_eq!(target.port, 8080);
        assert_eq!(target.kind, ProxyRequestKind::Plain);
    }

    #[test]
    fn absolute_uri_without_port_defaults_to_80() {
        let target =
            parse_proxy_request_line("POST http://example.com/api HTTP/1.1").expect("parsed");
        assert_eq!(target.port, 80);
    }

    #[test]
    fn parses_ipv6_literal() {
        let target = parse_proxy_request_line("CONNECT [::1]:8443 HTTP/1.1").expect("parsed");
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, 8443);
    }

    #[test]
    fn method_is_case_insensitive() {
        let target = parse_proxy_request_line("connect chatgpt.com:443 HTTP/1.1").expect("parsed");
        assert_eq!(target.kind, ProxyRequestKind::Connect);
    }

    #[test]
    fn rejects_origin_form_and_garbage() {
        assert!(parse_proxy_request_line("GET /models HTTP/1.1").is_none());
        assert!(parse_proxy_request_line("GET").is_none());
        assert!(parse_proxy_request_line("").is_none());
        assert!(parse_proxy_request_line("CONNECT :443 HTTP/1.1").is_none());
    }

    #[test]
    fn builds_no_auth_greeting() {
        assert_eq!(build_socks5_greeting(), [0x05, 0x01, 0x00]);
    }

    #[test]
    fn builds_domain_connect_request() {
        let request = build_socks5_connect_request("chatgpt.com", 443).expect("built");
        assert_eq!(&request[..5], &[0x05, 0x01, 0x00, 0x03, 11]);
        assert_eq!(&request[5..16], b"chatgpt.com");
        assert_eq!(&request[16..], &[0x01, 0xbb]); // 443 大端
    }

    #[test]
    fn rejects_empty_and_overlong_host() {
        assert!(build_socks5_connect_request("", 443).is_err());
        let long_host = "a".repeat(256);
        assert!(build_socks5_connect_request(&long_host, 443).is_err());
        let max_host = "a".repeat(255);
        assert!(build_socks5_connect_request(&max_host, 443).is_ok());
    }

    #[test]
    fn accepts_no_auth_method_reply_only() {
        assert!(parse_socks5_method_reply(&[0x05, 0x00]).is_ok());
        assert!(parse_socks5_method_reply(&[0x05, 0x02]).is_err());
        assert!(parse_socks5_method_reply(&[0x04, 0x00]).is_err());
        assert!(parse_socks5_method_reply(&[0x05]).is_err());
    }

    #[test]
    fn maps_connect_reply_codes() {
        assert!(parse_socks5_connect_reply(&[0x05, 0x00, 0x00, 0x01]).is_ok());
        let refused = parse_socks5_connect_reply(&[0x05, 0x05, 0x00, 0x01]).expect_err("refused");
        assert!(refused.to_string().contains("连接被拒绝"));
        let unreachable =
            parse_socks5_connect_reply(&[0x05, 0x03, 0x00, 0x01]).expect_err("unreachable");
        assert!(unreachable.to_string().contains("网络不可达"));
    }

    #[test]
    fn computes_connect_reply_length_per_address_type() {
        assert_eq!(
            socks5_connect_reply_len(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
            Some(10)
        );
        assert_eq!(
            socks5_connect_reply_len(&[0x05, 0x00, 0x00, 0x03, 3, b'a', b'b', b'c', 0, 0]),
            Some(10)
        );
        assert_eq!(socks5_connect_reply_len(&[0x05, 0x00]), None);
    }

    // ---- 以下为本项目新增 ----

    #[test]
    fn computes_ipv6_reply_length_and_rejects_unknown_address_type() {
        let mut ipv6 = vec![0x05, 0x00, 0x00, 0x04];
        ipv6.extend_from_slice(&[0; 18]);
        assert_eq!(socks5_connect_reply_len(&ipv6), Some(22));
        assert_eq!(socks5_connect_reply_len(&[0x05, 0x00, 0x00, 0x09, 0]), None);
    }

    #[test]
    fn rejects_https_absolute_uri_and_accepts_uppercase_scheme() {
        assert!(parse_proxy_request_line("GET https://chatgpt.com/ HTTP/1.1").is_none());
        let target =
            parse_proxy_request_line("GET HTTP://Example.COM:8080/a HTTP/1.1").expect("parsed");
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 8080);
    }

    #[test]
    fn origin_form_keeps_path_and_query() {
        let cases = [
            (
                "http://h:8080/models?client_version=1",
                "/models?client_version=1",
            ),
            ("http://h", "/"),
            ("http://h?x=1", "/?x=1"),
            ("http://h/a#frag", "/a"),
            ("HTTP://h/upper", "/upper"),
        ];
        for (uri, expected) in cases {
            assert_eq!(origin_form_target(uri).as_deref(), Some(expected), "{uri}");
        }
        assert_eq!(origin_form_target("/already-origin"), None);
    }

    #[test]
    fn rewrite_forces_connection_close_and_strips_proxy_headers() {
        let header = b"POST http://h:8080/v1/responses?x=1 HTTP/1.1\r\n\
Host: h:8080\r\n\
proxy-connection: keep-alive\r\n\
Keep-Alive: timeout=5,\r\n max=100\r\n\
CONNECTION: keep-alive\r\n\
Content-Length: 2\r\n\
X-Raw: \xff\r\n\r\n";
        let rewritten = rewrite_plain_request_head(header).expect("rewritten");
        assert_eq!(
            rewritten,
            b"POST /v1/responses?x=1 HTTP/1.1\r\n\
Host: h:8080\r\n\
Content-Length: 2\r\n\
X-Raw: \xff\r\n\
Connection: close\r\n\r\n"
                .to_vec()
        );
    }

    #[test]
    fn rewrite_adds_connection_close_when_absent() {
        let rewritten =
            rewrite_plain_request_head(b"GET http://h/ HTTP/1.0\r\n\r\n").expect("rewritten");
        assert_eq!(
            rewritten,
            b"GET / HTTP/1.0\r\nConnection: close\r\n\r\n".to_vec()
        );
        assert!(rewrite_plain_request_head(b"GET http://h/ HTTP/1.1\r\n").is_none());
    }

    #[tokio::test]
    async fn reads_head_and_keeps_extra_bytes() {
        let mut input: &[u8] = b"POST http://h/ HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody";
        let RequestHead::Complete { header, extra } =
            read_request_head(&mut input).await.expect("read")
        else {
            panic!("应读到完整请求头");
        };
        assert!(header.ends_with(b"Content-Length: 4\r\n\r\n"));
        assert_eq!(extra, b"body");
    }

    #[tokio::test]
    async fn header_limit_is_inclusive_of_terminator() {
        let prefix = b"GET http://h/ HTTP/1.1\r\nX-Pad: ";
        let pad = MAX_HEADER_BYTES - prefix.len() - 4;
        let mut exact = prefix.to_vec();
        exact.extend(std::iter::repeat_n(b'a', pad));
        exact.extend_from_slice(b"\r\n\r\n");
        assert_eq!(exact.len(), MAX_HEADER_BYTES);
        let mut input = exact.as_slice();
        assert!(matches!(
            read_request_head(&mut input).await.expect("read"),
            RequestHead::Complete { .. }
        ));

        let mut over = prefix.to_vec();
        over.extend(std::iter::repeat_n(b'a', pad + 1));
        over.extend_from_slice(b"\r\n\r\n");
        let mut input = over.as_slice();
        assert!(matches!(
            read_request_head(&mut input).await.expect("read"),
            RequestHead::TooLarge
        ));

        let endless = vec![b'a'; MAX_HEADER_BYTES * 2];
        let mut input = endless.as_slice();
        assert!(matches!(
            read_request_head(&mut input).await.expect("read"),
            RequestHead::TooLarge
        ));

        let mut input: &[u8] = b"GET http://h/ HTTP/1.1\r\n";
        assert!(matches!(
            read_request_head(&mut input).await.expect("read"),
            RequestHead::Closed
        ));
    }

    #[test]
    fn request_line_requires_utf8() {
        assert_eq!(
            request_line(b"CONNECT a:1 HTTP/1.1\r\n\r\n"),
            Some("CONNECT a:1 HTTP/1.1")
        );
        assert_eq!(request_line(b"GET http://\xff/ HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn self_identify_matches_exact_path_only() {
        assert!(is_self_identify_request(
            "GET /__codex_helper_proxy_id HTTP/1.1"
        ));
        assert!(!is_self_identify_request(
            "GET /__codex_helper_proxy_id?x HTTP/1.1"
        ));
        assert!(!is_self_identify_request(
            "POST /__codex_helper_proxy_id HTTP/1.1"
        ));
        assert!(is_identify_response(identify_response().as_bytes()));
    }

    #[test]
    fn identify_response_requires_200_and_exact_body() {
        assert!(!is_identify_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"
        ));
        assert!(!is_identify_response(
            b"HTTP/1.1 404 Not Found\r\n\r\ncodex-helper-managed-proxy"
        ));
        assert!(!is_identify_response(b"codex-helper-managed-proxy"));
        assert!(is_identify_response(
            b"HTTP/1.0 200 OK\r\n\r\ncodex-helper-managed-proxy"
        ));
    }

    #[test]
    fn detects_self_loop_targets() {
        let target = |host: &str, port: u16| ProxyTarget {
            host: host.to_string(),
            port,
            kind: ProxyRequestKind::Plain,
        };
        for host in [
            "127.0.0.1",
            "127.0.0.2",
            "localhost",
            "LocalHost.",
            "::1",
            "0.0.0.0",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_self_target(&target(host, 17891), 17891), "{host}");
        }
        assert!(!is_self_target(&target("127.0.0.1", 8080), 17891));
        assert!(!is_self_target(&target("10.20.30.61", 17891), 17891));
        assert!(!is_self_target(&target("example.com", 17891), 17891));
    }

    #[test]
    fn port_override_falls_back_to_default_when_invalid() {
        assert_eq!(proxy_port_from(None), consts::DEFAULT_PROXY_PORT);
        assert_eq!(proxy_port_from(Some(" 18000 ")), 18000);
        for invalid in ["", "  ", "0", "-1", "65536", "abc", "17891x"] {
            assert_eq!(
                proxy_port_from(Some(invalid)),
                consts::DEFAULT_PROXY_PORT,
                "{invalid:?}"
            );
        }
    }

    fn record(port: u16) -> ConnectionRecord {
        ConnectionRecord {
            time: u64::from(port),
            host: "example.com".into(),
            port,
            decision: Route::Direct,
            ok: true,
            ms: 1,
        }
    }

    #[test]
    fn recent_connections_keep_newest_first_within_capacity() {
        let recent = RecentConnections::new();
        assert!(recent.snapshot().is_empty());
        let shared = recent.clone();
        for port in 1..=(RECENT_CAPACITY as u16 + 5) {
            shared.push(record(port));
        }
        let snapshot = recent.snapshot();
        assert_eq!(snapshot.len(), RECENT_CAPACITY);
        assert_eq!(snapshot[0].port, RECENT_CAPACITY as u16 + 5);
        assert_eq!(snapshot[RECENT_CAPACITY - 1].port, 6);
    }

    #[test]
    fn success_logging_is_sampled() {
        let logged: Vec<u64> = (0..100).filter(|seq| sample_success(*seq)).collect();
        let mut expected: Vec<u64> = (0..20).collect();
        expected.extend([20, 40, 60, 80]);
        assert_eq!(logged, expected);
    }

    #[test]
    fn port_conflicts_map_to_port_unavailable() {
        for kind in [
            std::io::ErrorKind::AddrInUse,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let error = bind_error(17891, std::io::Error::from(kind));
            assert!(
                matches!(error, ProxyStartError::PortUnavailable { port: 17891, .. }),
                "{kind:?}"
            );
        }
        let other = bind_error(17891, std::io::Error::from(std::io::ErrorKind::Other));
        assert!(matches!(other, ProxyStartError::Io(_)));
    }
}
