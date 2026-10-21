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

use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, MutexGuard};

use helper_core::peer_identity::system_resolver;

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
