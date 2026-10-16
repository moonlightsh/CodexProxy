//! 本地分流代理的集成测试。
//!
//! 用假 SOCKS5 上游与假目标服务器验证三条硬性行为：命中规则走上游、未命中直连、
//! 命中但上游不可用时返回 502 且**不尝试直连**；以及连接记录、400、端口冲突、shutdown、
//! 自识探测与明文转发细节。全部只连本机回环地址，假上游不真正解析域名，不访问外网。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use helper_core::proxy::ProxyConfig;
use helper_core::proxy::ProxyHandle;
use helper_core::proxy::ProxyStartError;
use helper_core::proxy::RecordSink;
use helper_core::proxy::spawn_with_listener;
use helper_core::rules::Route;
use helper_core::types::ConnectionRecord;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

/// 极简 SOCKS5 服务器：无认证握手，CONNECT 一律成功，之后把收到的数据回显。
/// 返回 (host:port, 收到的 CONNECT 目标记录)。
async fn spawn_fake_socks5() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind fake socks5");
    let addr = listener.local_addr().expect("addr");
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let seen = Arc::clone(&seen_clone);
            tokio::spawn(async move {
                let mut greeting = [0u8; 3];
                if stream.read_exact(&mut greeting).await.is_err() {
                    return;
                }
                if stream.write_all(&[0x05, 0x00]).await.is_err() {
                    return;
                }
                let mut head = [0u8; 5];
                if stream.read_exact(&mut head).await.is_err() {
                    return;
                }
                let host_len = usize::from(head[4]);
                let mut host = vec![0u8; host_len];
                if stream.read_exact(&mut host).await.is_err() {
                    return;
                }
                let mut port = [0u8; 2];
                if stream.read_exact(&mut port).await.is_err() {
                    return;
                }
                let target = format!(
                    "{}:{}",
                    String::from_utf8_lossy(&host),
                    u16::from_be_bytes(port)
                );
                seen.lock().expect("lock").push(target);
                let reply = [0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 80];
                if stream.write_all(&reply).await.is_err() {
                    return;
                }
                let mut buffer = vec![0u8; 4096];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if stream.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr.to_string(), seen)
}

/// 假目标服务器：记录是否有人连入，并回一个固定响应。
async fn spawn_fake_target() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind fake target");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits_clone.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let received = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = format!("seen:{}", received.lines().next().unwrap_or_default());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (port, hits)
}

async fn spawn_proxy(socks5: &str) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy");
    let (host, port) = socks5.rsplit_once(':').expect("socks5 addr");
    let handle = spawn_with_listener(
        listener,
        ProxyConfig {
            socks5_host: host.to_string(),
            socks5_port: port.parse().expect("socks5 port"),
            ..ProxyConfig::default()
        },
    );
    let proxy_port = handle.port();
    // 句柄活到进程结束：测试内不需要回收。
    std::mem::forget(handle);
    proxy_port
}

#[tokio::test]
async fn openai_host_is_tunneled_through_socks5_upstream() {
    let (socks5, seen) = spawn_fake_socks5().await;
    let proxy_port = spawn_proxy(&socks5).await;

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client
        .write_all(b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n")
        .await
        .expect("send connect");

    let mut response = vec![0u8; 64];
    let read = client.read(&mut response).await.expect("read response");
    let text = String::from_utf8_lossy(&response[..read]).to_string();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "expected tunnel established, got: {text}"
    );

    // 隧道建立后的字节应该透传到上游（假上游会回显）。
    client.write_all(b"ping").await.expect("write tunnel");
    let mut echo = vec![0u8; 4];
    client.read_exact(&mut echo).await.expect("read echo");
    assert_eq!(&echo, b"ping");

    let targets = seen.lock().expect("lock").clone();
    assert_eq!(targets, vec!["chatgpt.com:443".to_string()]);
}

#[tokio::test]
async fn unmatched_host_goes_direct_and_preserves_query() {
    let (socks5, seen) = spawn_fake_socks5().await;
    let (target_port, hits) = spawn_fake_target().await;
    let proxy_port = spawn_proxy(&socks5).await;

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://127.0.0.1:{target_port}/models?client_version=0.154.0 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
    );
    client
        .write_all(request.as_bytes())
        .await
        .expect("send request");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read response");
    assert!(response.contains("200 OK"), "got: {response}");
    // 目标看到的应该是 origin-form 且保留 query
    assert!(
        response.contains("GET /models?client_version=0.154.0"),
        "origin-form rewrite lost query: {response}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(seen.lock().expect("lock").is_empty(), "must not use socks5");
}

/// 拒绝一切 CONNECT 的假 SOCKS5，用来验证不回退直连。
async fn spawn_refusing_socks5() -> (String, Arc<AtomicBool>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind refusing socks5");
    let addr = listener.local_addr().expect("addr");
    let contacted = Arc::new(AtomicBool::new(false));
    let contacted_clone = Arc::clone(&contacted);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            contacted_clone.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut greeting = [0u8; 3];
                let _ = stream.read_exact(&mut greeting).await;
                let _ = stream.write_all(&[0x05, 0x00]).await;
                let mut head = [0u8; 5];
                let _ = stream.read_exact(&mut head).await;
                let host_len = usize::from(head[4]);
                let mut rest = vec![0u8; host_len + 2];
                let _ = stream.read_exact(&mut rest).await;
                // REP=0x05 连接被拒绝
                let _ = stream
                    .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await;
            });
        }
    });
    (addr.to_string(), contacted)
}

#[tokio::test]
async fn matched_host_returns_502_instead_of_falling_back_to_direct() {
    let (socks5, contacted) = spawn_refusing_socks5().await;
    let proxy_port = spawn_proxy(&socks5).await;

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client
        .write_all(b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n")
        .await
        .expect("send connect");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "must fail closed, got: {response}"
    );
    assert!(
        !response.contains("200 Connection Established"),
        "must never fall back to direct: {response}"
    );
    assert!(contacted.load(Ordering::SeqCst), "socks5 should be tried");
}

#[tokio::test]
async fn unreachable_socks5_upstream_also_fails_closed() {
    // 绑完就释放，得到一个几乎肯定无人监听的端口。
    let dead_port = {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let proxy_port = spawn_proxy(&format!("127.0.0.1:{dead_port}")).await;

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client
        .write_all(b"CONNECT api.openai.com:443 HTTP/1.1\r\n\r\n")
        .await
        .expect("send connect");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "must fail closed, got: {response}"
    );
}

#[tokio::test]
async fn self_identify_endpoint_reports_managed_proxy() {
    let (socks5, _) = spawn_fake_socks5().await;
    let proxy_port = spawn_proxy(&socks5).await;

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client
        .write_all(b"GET /__codex_helper_proxy_id HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .expect("send request");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read response");
    assert!(response.contains("200 OK"), "got: {response}");
    assert!(
        response.contains("codex-helper-managed-proxy"),
        "got: {response}"
    );
}

// ---- 以下为本项目新增 ----

type Records = Arc<Mutex<Vec<ConnectionRecord>>>;

fn record_sink() -> (RecordSink, Records) {
    let records: Records = Arc::new(Mutex::new(Vec::new()));
    let sink_records = Arc::clone(&records);
    let sink: RecordSink = Arc::new(move |record| sink_records.lock().expect("lock").push(record));
    (sink, records)
}

fn proxy_config(socks5: &str) -> ProxyConfig {
    let (host, port) = socks5.rsplit_once(':').expect("socks5 addr");
    ProxyConfig {
        socks5_host: host.to_string(),
        socks5_port: port.parse().expect("socks5 port"),
        ..ProxyConfig::default()
    }
}

/// 启动一个带记录回调的代理，返回句柄（随测试结束 drop）。
async fn spawn_recording_proxy(config: ProxyConfig) -> (ProxyHandle, Records) {
    let (sink, records) = record_sink();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy");
    let handle = spawn_with_listener(
        listener,
        ProxyConfig {
            on_record: Some(sink),
            ..config
        },
    );
    (handle, records)
}

/// 等到记录条数达到 `count`（记录在连接结束后才交出），最多 5 秒。
async fn wait_for_records(records: &Records, count: usize) -> Vec<ConnectionRecord> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = records.lock().expect("lock").clone();
        if snapshot.len() >= count {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "记录数未达到 {count}：{snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 新增测试的读写超时：行为回归时让测试明确失败，而不是挂起。
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// 读到连接关闭（带超时）。
async fn read_until_closed(client: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut response = Vec::new();
    tokio::time::timeout(IO_TIMEOUT, client.read_to_end(&mut response))
        .await
        .expect("代理应在应答后关闭连接")
        .expect("read response");
    response
}

/// 发送一段原始请求并读到连接关闭。
async fn send_and_read(proxy_port: u16, request: &[u8]) -> String {
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client.write_all(request).await.expect("send request");
    String::from_utf8_lossy(&read_until_closed(&mut client).await).to_string()
}

/// 经代理向 `chatgpt.com:443` 建隧道（假 SOCKS5 回显）。CONNECT 与首段隧道数据一次写出，
/// 验证请求头之后已读入的字节也会交给上游。
async fn open_pipelined_tunnel(proxy_port: u16) -> tokio::net::TcpStream {
    const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    client
        .write_all(b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\nping")
        .await
        .expect("send connect");
    let mut reply = vec![0u8; ESTABLISHED.len() + 4];
    tokio::time::timeout(IO_TIMEOUT, client.read_exact(&mut reply))
        .await
        .expect("隧道应建立并回显")
        .expect("read tunnel");
    assert_eq!(&reply[..ESTABLISHED.len()], ESTABLISHED);
    assert_eq!(&reply[ESTABLISHED.len()..], b"ping");
    client
}

/// 绑完就释放，得到一个几乎肯定无人监听的端口。
async fn dead_port() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    listener.local_addr().expect("addr").port()
}

fn unix_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("millis")
}

/// 完整读取请求（头 + Content-Length 请求体）后记录原始字节，回显请求体并关闭。
async fn spawn_recording_target() -> (u16, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind recording target");
    let port = listener.local_addr().expect("addr").port();
    let requests: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_clone = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = Arc::clone(&requests_clone);
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = vec![0u8; 8192];
                let header_end = loop {
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buffer[..read]);
                    if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                while raw.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buffer[..read]);
                }
                let body = raw[header_end..].to_vec();
                requests.lock().expect("lock").push(raw);
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(&body);
                let _ = stream.write_all(&response).await;
            });
        }
    });
    (port, requests)
}

/// 接受连接、读取但从不应答的监听（模拟握手卡死的 SOCKS5 或无关服务）。
async fn spawn_silent_listener() -> (String, Arc<AtomicBool>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind silent listener");
    let addr = listener.local_addr().expect("addr");
    let contacted = Arc::new(AtomicBool::new(false));
    let contacted_clone = Arc::clone(&contacted);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            contacted_clone.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 1024];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                }
            });
        }
    });
    (addr.to_string(), contacted)
}

#[tokio::test]
async fn matched_host_refused_by_socks5_never_reaches_direct_target() {
    let (socks5, contacted) = spawn_refusing_socks5().await;
    let (target_port, hits) = spawn_fake_target().await;
    let target_addr = SocketAddr::from(([127, 0, 0, 1], target_port));
    // 命中规则的主机与未命中的主机都映射到同一个假目标：若发生回退直连，假目标必然收到连接。
    let overrides = HashMap::from([
        ("chatgpt.com".to_string(), target_addr),
        ("unmatched.example".to_string(), target_addr),
    ]);
    let (proxy, records) = spawn_recording_proxy(ProxyConfig {
        direct_overrides: Some(Arc::new(overrides)),
        ..proxy_config(&socks5)
    })
    .await;

    let response = send_and_read(
        proxy.port(),
        b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
    let response = send_and_read(
        proxy.port(),
        b"GET http://ChatGPT.com./backend-api/ HTTP/1.1\r\nHost: chatgpt.com\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
    assert!(contacted.load(Ordering::SeqCst), "应先尝试 SOCKS5");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "命中规则的请求绝不能回退直连"
    );

    // 未命中规则的主机走同一映射到达假目标，证明映射本身生效。
    let response = send_and_read(
        proxy.port(),
        b"GET http://unmatched.example/ping HTTP/1.1\r\nHost: unmatched.example\r\n\r\n",
    )
    .await;
    assert!(response.contains("200 OK"), "got: {response}");
    assert!(
        response.contains("seen:GET /ping HTTP/1.1"),
        "got: {response}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let records = wait_for_records(&records, 3).await;
    let summary: Vec<(&str, Route, bool)> = records
        .iter()
        .map(|record| (record.host.as_str(), record.decision, record.ok))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("chatgpt.com", Route::Socks5, false),
            ("chatgpt.com.", Route::Socks5, false),
            ("unmatched.example", Route::Direct, true),
        ]
    );
}

#[tokio::test]
async fn overlong_matched_host_fails_closed_without_contacting_upstream() {
    let (socks5, contacted) = spawn_refusing_socks5().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    let host = format!("{}.openai.com", "a".repeat(250));
    let request = format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n");
    let response = send_and_read(proxy.port(), request.as_bytes()).await;
    assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
    assert!(!contacted.load(Ordering::SeqCst), "主机名过长不应连接上游");

    let records = wait_for_records(&records, 1).await;
    assert_eq!(records[0].decision, Route::Socks5);
    assert!(!records[0].ok);
}

#[tokio::test]
async fn silent_socks5_upstream_times_out_with_502() {
    let (socks5, contacted) = spawn_silent_listener().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    let started = Instant::now();
    let response =
        send_and_read(proxy.port(), b"CONNECT api.openai.com:443 HTTP/1.1\r\n\r\n").await;
    let elapsed = started.elapsed();
    assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
    assert!(contacted.load(Ordering::SeqCst));
    assert!(
        elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_secs(8),
        "握手应在约 3 秒后超时：{elapsed:?}"
    );

    let records = wait_for_records(&records, 1).await;
    assert!(!records[0].ok);
    assert!(records[0].ms >= 2900, "ms: {}", records[0].ms);
}

#[tokio::test]
async fn unmatched_host_to_released_port_returns_502_and_records_failure() {
    let (socks5, seen) = spawn_fake_socks5().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;
    let port = dead_port().await;

    let request = format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    let response = send_and_read(proxy.port(), request.as_bytes()).await;
    assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
    assert!(
        seen.lock().expect("lock").is_empty(),
        "直连失败不得改走 SOCKS5"
    );

    let records = wait_for_records(&records, 1).await;
    assert_eq!(records[0].host, "127.0.0.1");
    assert_eq!(records[0].port, port);
    assert_eq!(records[0].decision, Route::Direct);
    assert!(!records[0].ok);
}

#[tokio::test]
async fn records_cover_tunnel_and_direct_but_not_identify_or_bad_requests() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (target_port, _) = spawn_fake_target().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    // 自识端点与 400 不产生记录（它们在建立上游之前就结束）。
    let response = send_and_read(
        proxy.port(),
        b"GET /__codex_helper_proxy_id HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    )
    .await;
    assert!(
        response.contains("codex-helper-managed-proxy"),
        "got: {response}"
    );
    let response = send_and_read(proxy.port(), b"GET /models HTTP/1.1\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 400"), "got: {response}");
    assert!(records.lock().expect("lock").is_empty());

    // 隧道：连接结束时产生记录。
    let before = unix_millis();
    let client = open_pipelined_tunnel(proxy.port()).await;
    drop(client);
    let tunnel = wait_for_records(&records, 1).await.remove(0);
    let after = unix_millis();
    assert_eq!(tunnel.host, "chatgpt.com");
    assert_eq!(tunnel.port, 443);
    assert_eq!(tunnel.decision, Route::Socks5);
    assert!(tunnel.ok);
    assert!(
        (before..=after).contains(&tunnel.time),
        "time: {}",
        tunnel.time
    );
    assert!(tunnel.ms < 3000);

    // 明文直连：上游响应结束后产生记录。
    let request =
        format!("GET http://127.0.0.1:{target_port}/x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    let response = send_and_read(proxy.port(), request.as_bytes()).await;
    assert!(response.contains("200 OK"), "got: {response}");
    let direct = wait_for_records(&records, 2).await.remove(1);
    assert_eq!(direct.host, "127.0.0.1");
    assert_eq!(direct.port, target_port);
    assert_eq!(direct.decision, Route::Direct);
    assert!(direct.ok);
    assert!(direct.ms < 10_000);

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(records.lock().expect("lock").len(), 2, "不应有多余记录");
}

#[tokio::test]
async fn oversized_header_returns_400() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (target_port, hits) = spawn_fake_target().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    // 空行出现在 32 KiB 之后
    let mut request =
        format!("GET http://127.0.0.1:{target_port}/ HTTP/1.1\r\nX-Pad: ").into_bytes();
    request.extend(std::iter::repeat_n(b'a', 33 * 1024));
    request.extend_from_slice(b"\r\n\r\n");
    let response = send_and_read(proxy.port(), &request).await;
    assert!(response.starts_with("HTTP/1.1 400"), "got: {response}");

    // 超过 32 KiB 仍无空行：不必等到空行出现就应回 400
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .expect("connect proxy");
    let mut partial =
        format!("GET http://127.0.0.1:{target_port}/ HTTP/1.1\r\nX-Pad: ").into_bytes();
    partial.extend(std::iter::repeat_n(b'a', 40 * 1024));
    client
        .write_all(&partial)
        .await
        .expect("send partial header");
    let response = read_until_closed(&mut client).await;
    assert!(response.starts_with(b"HTTP/1.1 400"));

    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn unparseable_request_line_returns_400() {
    let (socks5, seen) = spawn_fake_socks5().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    let bad_requests: [&[u8]; 5] = [
        b"GET /models HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        b"HELLO\r\n\r\n",
        b"CONNECT :443 HTTP/1.1\r\n\r\n",
        b"GET https://chatgpt.com/ HTTP/1.1\r\n\r\n",
        b"GET http://\xff.example/ HTTP/1.1\r\n\r\n",
    ];
    for request in bad_requests {
        let response = send_and_read(proxy.port(), request).await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "request {:?} got: {response}",
            String::from_utf8_lossy(request)
        );
    }
    assert!(seen.lock().expect("lock").is_empty());
    assert!(records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn requests_targeting_the_proxy_itself_return_400() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;
    let port = proxy.port();

    for request in [
        format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n"),
        format!("CONNECT [::1]:{port} HTTP/1.1\r\n\r\n"),
    ] {
        let response = send_and_read(port, request.as_bytes()).await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "{request:?} got: {response}"
        );
    }
    assert!(records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn plain_request_forces_connection_close_and_strips_proxy_headers() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (target_port, requests) = spawn_recording_target().await;
    let (proxy, _records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    let request = format!(
        "GET http://127.0.0.1:{target_port}/v1/models?limit=5 HTTP/1.1\r\n\
         Host: 127.0.0.1:{target_port}\r\n\
         Proxy-Connection: keep-alive\r\n\
         Connection: keep-alive\r\n\
         Keep-Alive: timeout=5\r\n\
         Accept: */*\r\n\r\n"
    );
    let response = send_and_read(proxy.port(), request.as_bytes()).await;
    assert!(response.contains("200 OK"), "got: {response}");

    let received = requests.lock().expect("lock").clone();
    assert_eq!(received.len(), 1);
    let text = String::from_utf8_lossy(&received[0]).to_string();
    assert!(
        text.starts_with("GET /v1/models?limit=5 HTTP/1.1\r\n"),
        "got: {text}"
    );
    assert!(text.contains("\r\nConnection: close\r\n"), "got: {text}");
    assert!(text.contains("\r\nAccept: */*\r\n"), "got: {text}");
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("proxy-connection"), "got: {text}");
    assert!(!lower.contains("keep-alive"), "got: {text}");
    assert_eq!(lower.matches("connection:").count(), 1, "got: {text}");
}

#[tokio::test]
async fn post_body_reaches_target_intact() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (target_port, requests) = spawn_recording_target().await;
    let (proxy, _records) = spawn_recording_proxy(proxy_config(&socks5)).await;

    // 请求体与请求头同一次写出：头部之后已被代理读入的字节必须原样转发。
    let body = br#"{"model":"gpt","input":"hello"}"#;
    let mut request = format!(
        "POST http://127.0.0.1:{target_port}/v1/responses HTTP/1.1\r\n\
         Host: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    let response = send_and_read(proxy.port(), &request).await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(
        response.ends_with(r#"{"model":"gpt","input":"hello"}"#),
        "got: {response}"
    );

    // 大请求体分两次写出：头部 + 一部分请求体先到，其余稍后到。
    let big_body: Vec<u8> = (0..200_000u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .expect("connect proxy");
    let head = format!(
        "POST http://127.0.0.1:{target_port}/upload HTTP/1.1\r\n\
         Host: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        big_body.len()
    );
    let mut first = head.into_bytes();
    first.extend_from_slice(&big_body[..1000]);
    client.write_all(&first).await.expect("send head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    client
        .write_all(&big_body[1000..])
        .await
        .expect("send rest");
    let response = read_until_closed(&mut client).await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(&big_body), "回显的请求体不完整");

    let received = requests.lock().expect("lock").clone();
    assert_eq!(received.len(), 2);
    assert!(received[0].ends_with(body));
    assert!(received[1].ends_with(&big_body));
}

#[tokio::test]
async fn spawn_on_occupied_port_returns_port_unavailable() {
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let port = occupied.local_addr().expect("addr").port();

    let result = helper_core::proxy::spawn(port, ProxyConfig::default()).await;
    match result {
        Err(ProxyStartError::PortUnavailable { port: reported, .. }) => {
            assert_eq!(reported, port);
        }
        other => panic!("应返回 PortUnavailable，实际：{other:?}"),
    }
}

#[tokio::test]
async fn shutdown_releases_port_and_closes_inflight_tunnels() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (sink, records) = record_sink();
    let proxy = helper_core::proxy::spawn(
        0,
        ProxyConfig {
            on_record: Some(sink),
            ..proxy_config(&socks5)
        },
    )
    .await
    .expect("spawn proxy");
    let port = proxy.port();
    assert_ne!(port, 0);

    let mut client = open_pipelined_tunnel(port).await;

    proxy.shutdown().await;

    // 在途隧道被中止：客户端读到 EOF。
    let mut buffer = [0u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buffer))
        .await
        .expect("隧道应被关闭")
        .expect("read after shutdown");
    assert_eq!(read, 0);
    // 被中止的隧道同样交出记录（shutdown 返回前）。
    let snapshot = records.lock().expect("lock").clone();
    assert_eq!(snapshot.len(), 1, "{snapshot:?}");
    assert!(snapshot[0].ok);

    // 端口已释放：可立即重新绑定，也可在同一端口重启代理。
    let rebound = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("端口应已释放");
    drop(rebound);
    let restarted = helper_core::proxy::spawn(port, proxy_config(&socks5))
        .await
        .expect("同端口重启");
    assert!(helper_core::proxy::probe_existing(port).await);
    restarted.shutdown().await;
}

#[tokio::test]
async fn probe_existing_recognizes_only_this_proxy() {
    let (socks5, _) = spawn_fake_socks5().await;
    let (proxy, records) = spawn_recording_proxy(proxy_config(&socks5)).await;
    assert!(helper_core::proxy::probe_existing(proxy.port()).await);
    assert!(records.lock().expect("lock").is_empty());

    // 返回其他内容的 HTTP 服务
    let (other_port, _) = spawn_fake_target().await;
    assert!(!helper_core::proxy::probe_existing(other_port).await);

    // 已关闭的端口
    assert!(!helper_core::proxy::probe_existing(dead_port().await).await);

    // 接受连接却从不应答：2 秒超时后判定为否
    let (silent, _) = spawn_silent_listener().await;
    let silent_port: u16 = silent
        .rsplit_once(':')
        .expect("addr")
        .1
        .parse()
        .expect("port");
    let started = Instant::now();
    assert!(!helper_core::proxy::probe_existing(silent_port).await);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1900) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
}
