//! 对端进程身份解析的 Windows 真机测试（设计 §6.4），镜像 tests/peer_identity_macos.rs。
//!
//! 需在真实 Windows 上手动运行：`cargo test -p helper-core --test peer_identity_windows -- --ignored`。
//! 只连本机回环：自连解析到当前进程；子进程（PowerShell）作连接方时解析到子进程；
//! 不存在的四元组必须报错。与 macOS 版相同，测试串行执行，避免并发 spawn 子进程时
//! 句柄继承造成同一连接出现多个候选所有者。
#![cfg(windows)]

use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use helper_core::peer_identity::{current_user_scope, system_resolver};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn ep(a: SocketAddr) -> (IpAddr, u16) {
    (a.ip(), a.port())
}

fn canon(p: &str) -> std::path::PathBuf {
    dunce::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {p}: {e}"))
}

fn assert_is_self(listener: TcpListener) {
    let addr = listener.local_addr().unwrap();
    let _client = TcpStream::connect(addr).unwrap();
    let (server_side, peer) = listener.accept().unwrap();
    let local = server_side.local_addr().unwrap();

    let info = system_resolver().resolve(ep(local), ep(peer)).unwrap();
    assert_eq!(info.pid, std::process::id() as i32);
    assert_eq!(info.user_scope, current_user_scope());
    assert!(info.user_scope.starts_with("S-1-"), "{}", info.user_scope);
    let exe = dunce::canonicalize(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(canon(&info.image_path), exe);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(
        info.start_time_ms > 0 && info.start_time_ms <= now_ms,
        "{}",
        info.start_time_ms
    );
}

#[test]
#[ignore = "需在真实 Windows 上运行"]
fn resolves_self_connection_ipv4() {
    let _g = serial();
    assert_is_self(TcpListener::bind("127.0.0.1:0").unwrap());
}

#[test]
#[ignore = "需在真实 Windows 上运行"]
fn resolves_self_connection_ipv6() {
    let _g = serial();
    let Ok(l) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
        eprintln!("本机无 IPv6 回环，跳过");
        return;
    };
    assert_is_self(l);
}

#[test]
#[ignore = "需在真实 Windows 上运行"]
fn current_user_scope_is_sid() {
    let _g = serial();
    assert!(current_user_scope().starts_with("S-1-"));
}

#[test]
#[ignore = "需在真实 Windows 上运行"]
fn resolves_child_process_not_own_accepted_socket() {
    let _g = serial();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "$c = New-Object Net.Sockets.TcpClient('127.0.0.1', {port}); Start-Sleep -Seconds 30"
    );
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (server_side, peer) = listener.accept().unwrap();
    let local = server_side.local_addr().unwrap();

    let result = system_resolver().resolve(ep(local), ep(peer));
    let _ = child.kill();
    let _ = child.wait();
    let info = result.unwrap();
    assert_eq!(info.pid, child.id() as i32);
    assert_ne!(info.pid, std::process::id() as i32);
    assert!(
        info.image_path
            .to_ascii_lowercase()
            .ends_with("powershell.exe"),
        "{}",
        info.image_path
    );
    assert_eq!(info.user_scope, current_user_scope());
}

#[test]
#[ignore = "需在真实 Windows 上运行"]
fn unknown_tuple_is_error() {
    let _g = serial();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    assert!(system_resolver().resolve(ep(addr), (addr.ip(), 1)).is_err());
}
