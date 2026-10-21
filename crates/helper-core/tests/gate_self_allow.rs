//! 门禁本进程放行的真实 socket 测试（组件更新检查设计 §7 方案 d）。
//!
//! 已核实的结论：解析器 `is_peer_socket` 只认与代理 accept 端方向相反的 socket，
//! `unique_owner` 对同一 PID 去重，所以“本进程客户端连本进程监听端口”不会被判为多候选；
//! `peer_identity_macos.rs::resolves_self_connection_ipv4` 已用同步 std socket 证明。
//! 本文件补的是组件检测的真实形态：tokio 监听 + tokio/reqwest 客户端、同进程、重复多次，
//! 以及带门禁的代理端到端放行本进程、仍拒绝他进程。
//!
//! 串行执行：并发测试中 spawn 子进程时，子进程会短暂继承本进程的 socket 句柄，使同一
//! 连接出现多个候选所有者而被（正确地）拒绝，导致自连用例偶发失败（同 peer_identity_macos.rs）。
//! 为避免持锁跨 await，每个用例先取串行锁、再自建 tokio 运行时。
//!
//! Windows 上需在真实机器手动运行：`cargo test -p helper-core --test gate_self_allow -- --ignored`。
#![cfg(any(target_os = "macos", windows))]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use helper_core::connection_gate::ConnectionGate;
use helper_core::peer_identity::system_resolver;
use helper_core::process_policy::{
    AllowedRole, CodeImage, ProcessPolicy, ROLE_CODEX_ENGINE, current_arch, current_os,
    hash_image_file,
};
use helper_core::proxy::{ProxyConfig, ProxyHandle, spawn_with_listener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 2100-01-01：测试运行的任何时间都早于它。
const FAR_FUTURE: u64 = 4_102_444_800;
/// 端到端用例的单次 IO 超时。
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// 假目标主机名：不命中分流规则，走直连并由 `direct_overrides` 映射到本机假目标。
const TARGET_HOST: &str = "unmatched.example";

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

fn ep(a: SocketAddr) -> (IpAddr, u16) {
    (a.ip(), a.port())
}

/// 同进程 tokio 客户端连接 `listener`，accept 后按代理视角的四元组解析连接所有者。
async fn resolve_self_connection(
    listener: &tokio::net::TcpListener,
) -> anyhow::Result<helper_core::peer_identity::PeerInfo> {
    let addr = listener.local_addr()?;
    let client = tokio::net::TcpStream::connect(addr).await?;
    let (server_side, peer) = listener.accept().await?;
    let local = server_side.local_addr()?;
    // 解析器是阻塞调用，与 proxy.rs 一样放进 spawn_blocking。
    let resolved =
        tokio::task::spawn_blocking(move || system_resolver().resolve(ep(local), ep(peer))).await?;
    drop(client);
    drop(server_side);
    resolved
}

#[test]
#[cfg_attr(windows, ignore = "需在真实 Windows 上运行")]
fn tokio_self_connection_resolves_to_own_pid() {
    let _g = serial();
    runtime().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        for round in 0..10 {
            let info = resolve_self_connection(&listener)
                .await
                .unwrap_or_else(|e| panic!("第 {round} 次自连解析失败：{e}"));
            assert_eq!(info.pid, std::process::id() as i32, "第 {round} 次");
        }
    });
}

#[test]
#[ignore = "诊断用：观察 fd 继承造成的瞬时多候选"]
fn self_connection_with_concurrent_child_spawn() {
    let _g = serial();
    runtime().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let spawner = tokio::spawn(async {
            for _ in 0..50 {
                let mut command = if cfg!(windows) {
                    let mut c = tokio::process::Command::new("cmd.exe");
                    c.args(["/c", "exit", "0"]);
                    c
                } else {
                    tokio::process::Command::new("/usr/bin/true")
                };
                let _ = command.status().await;
            }
        });
        let mut errors = 0usize;
        for _ in 0..50 {
            match resolve_self_connection(&listener).await {
                Ok(info) => assert_eq!(info.pid, std::process::id() as i32),
                Err(error) => {
                    errors += 1;
                    eprintln!("自连解析失败：{error}");
                }
            }
        }
        spawner.await.expect("spawner task");
        eprintln!("并发 spawn 下自连解析失败次数：{errors}/50");
        assert_eq!(errors, 0, "并发 spawn 下出现瞬时多候选");
    });
}

// ---------- 端到端：带真实门禁的代理 ----------

/// 假目标服务器：计数连入次数，回固定 `200 OK`（照抄 tests/proxy.rs 同名函数）。
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

/// 带真实门禁的代理：解析器为 `system_resolver`，策略只批准一个随机临时文件的摘要——
/// **不含**测试二进制映像，所以任何放行都只可能来自本进程 PID 判定。
/// 返回（代理句柄、假目标命中计数）；句柄须在用例内保持存活。
async fn spawn_gated_proxy() -> (ProxyHandle, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let anchor = dir.path().join("policy-anchor.bin");
    let random = format!(
        "anchor-{}-{:?}",
        std::process::id(),
        std::time::Instant::now()
    );
    std::fs::write(&anchor, random.as_bytes()).expect("write anchor");
    let sha = hash_image_file(&anchor).expect("hash anchor");
    let policy = Arc::new(ProcessPolicy {
        policy_id: "gate-self-allow-policy".into(),
        version: 1,
        not_after_unix: FAR_FUTURE,
        roles: vec![AllowedRole {
            role: ROLE_CODEX_ENGINE.into(),
            images: vec![CodeImage {
                sha256_hex: sha,
                os: current_os(),
                arch: current_arch(),
            }],
            path_prefixes: vec![],
        }],
    });
    let gate = Arc::new(ConnectionGate::new(system_resolver(), policy));
    let (target_port, hits) = spawn_fake_target().await;
    let overrides = HashMap::from([(
        TARGET_HOST.to_string(),
        SocketAddr::from(([127, 0, 0, 1], target_port)),
    )]);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy");
    let handle = spawn_with_listener(
        listener,
        ProxyConfig {
            gate: Some(gate),
            direct_overrides: Some(Arc::new(overrides)),
            ..ProxyConfig::default()
        },
    );
    (handle, hits)
}

/// 同进程原始 TCP 客户端经代理发一次明文 GET，读到连接关闭为止。
async fn raw_get_through_proxy(proxy_port: u16) -> String {
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect proxy");
    let request = format!("GET http://{TARGET_HOST}/ping HTTP/1.1\r\nHost: {TARGET_HOST}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("send request");
    let mut response = Vec::new();
    tokio::time::timeout(IO_TIMEOUT, client.read_to_end(&mut response))
        .await
        .expect("代理应在超时内关闭连接")
        .expect("read response");
    String::from_utf8_lossy(&response).into_owned()
}

#[test]
#[cfg_attr(windows, ignore = "需在真实 Windows 上运行")]
fn same_process_reqwest_through_gated_proxy_is_allowed() {
    let _g = serial();
    runtime().block_on(async {
        let (proxy, hits) = spawn_gated_proxy().await;
        // 组件检测的真实用法：reqwest 以 HTTP 代理身份接入本地代理。
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{}", proxy.port())).unwrap())
            .timeout(IO_TIMEOUT)
            .build()
            .expect("build client");
        let response = client
            .get(format!("http://{TARGET_HOST}/ping"))
            .send()
            .await
            .expect("send through proxy");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    });
}

#[test]
#[cfg_attr(windows, ignore = "需在真实 Windows 上运行")]
fn same_process_repeated_requests_all_allowed() {
    let _g = serial();
    runtime().block_on(async {
        let (proxy, hits) = spawn_gated_proxy().await;
        for round in 0..10 {
            let response = raw_get_through_proxy(proxy.port()).await;
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "第 {round} 次：{response}"
            );
        }
        assert_eq!(hits.load(Ordering::SeqCst), 10);
    });
}

/// 他进程 HTTP 客户端：macOS 系统 curl；Windows 自带 curl.exe。
fn curl_program() -> std::path::PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::path::Path::new(&root)
            .join("System32")
            .join("curl.exe")
    } else {
        "/usr/bin/curl".into()
    }
}

#[test]
#[cfg_attr(windows, ignore = "需在真实 Windows 上运行")]
fn other_process_through_gated_proxy_still_403() {
    let _g = serial();
    runtime().block_on(async {
        let (proxy, hits) = spawn_gated_proxy().await;
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let output = tokio::process::Command::new(curl_program())
            .args(["-s", "-o", null, "-w", "%{http_code}", "-x"])
            .arg(format!("http://127.0.0.1:{}", proxy.port()))
            .arg(format!("http://{TARGET_HOST}/ping"))
            // 防环境里的 NO_PROXY 让 curl 绕过 -x 直接连目标。
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .await
            .expect("run curl");
        let code = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_eq!(
            code,
            "403",
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "他进程不得触达目标");
    });
}
