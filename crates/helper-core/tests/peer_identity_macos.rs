//! 对端进程身份解析的 macOS 实机测试（设计 §6.4）。
//!
//! 只连本机回环：自连验证 pid/uid/映像路径与当前进程一致；用子进程 `/usr/bin/nc`
//! 作连接方，验证解析到的是子进程而不是代理自身 accept 的 socket（方向不能弄反）；
//! 不存在的四元组必须报错。
//!
//! 本文件的测试串行执行：并发测试中 fork/spawn 子进程时，子进程会短暂继承本进程的
//! socket fd，使同一连接出现多个候选所有者而被（正确地）拒绝，导致自连用例偶发失败。
#![cfg(target_os = "macos")]

use std::io::Read;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use helper_core::peer_identity::{current_user_scope, system_resolver};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn ep(a: SocketAddr) -> (IpAddr, u16) {
    (a.ip(), a.port())
}

fn canon(p: &str) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {p}: {e}"))
}

fn uid_from_id_cmd() -> String {
    let out = Command::new("/usr/bin/id").arg("-u").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn assert_is_self(listener: TcpListener) {
    let addr = listener.local_addr().unwrap();
    let _client = TcpStream::connect(addr).unwrap();
    let (server_side, peer) = listener.accept().unwrap();
    let local = server_side.local_addr().unwrap();

    let info = system_resolver().resolve(ep(local), ep(peer)).unwrap();
    assert_eq!(info.pid, std::process::id() as i32);
    assert_eq!(info.user_scope, current_user_scope());
    let exe = std::env::current_exe().unwrap();
    assert_eq!(canon(&info.image_path), std::fs::canonicalize(exe).unwrap());
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
fn resolves_self_connection_ipv4() {
    let _g = serial();
    assert_is_self(TcpListener::bind("127.0.0.1:0").unwrap());
}

#[test]
fn resolves_self_connection_ipv6() {
    let _g = serial();
    let Ok(l) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
        eprintln!("本机无 IPv6 回环，跳过");
        return;
    };
    assert_is_self(l);
}

#[test]
fn current_user_scope_is_decimal_uid() {
    let _g = serial();
    let scope = current_user_scope();
    assert!(!scope.is_empty());
    assert_eq!(scope, uid_from_id_cmd());
}

#[test]
fn resolves_child_process_not_own_accepted_socket() {
    let _g = serial();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    // stdin 保持管道打开，nc 会一直保持连接直到被结束
    let mut child = Command::new("/usr/bin/nc")
        .args(["127.0.0.1", &port])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (mut server_side, peer) = listener.accept().unwrap();
    let local = server_side.local_addr().unwrap();

    let result = system_resolver().resolve(ep(local), ep(peer));
    let _ = child.kill();
    let _ = child.wait();
    let info = result.unwrap();
    assert_eq!(info.pid, child.id() as i32);
    assert_ne!(info.pid, std::process::id() as i32);
    assert_eq!(canon(&info.image_path), canon("/usr/bin/nc"));
    assert_eq!(info.user_scope, current_user_scope());

    // 子进程退出后连接已关闭，再查必须失败
    server_side
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = [0u8; 1];
    let _ = server_side.read(&mut buf);
    assert!(system_resolver().resolve(ep(local), ep(peer)).is_err());
}

#[test]
fn unknown_tuple_is_error() {
    let _g = serial();
    // 先占一个端口再释放，保证该四元组不存在任何连接
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let local = ep(addr);
    let remote = (addr.ip(), 1);
    assert!(system_resolver().resolve(local, remote).is_err());
}

#[test]
fn listener_socket_never_matches() {
    let _g = serial();
    // 只有监听 socket、没有连接时，用监听端点作查询不得命中监听 socket
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let any = "127.0.0.1:65000".parse::<SocketAddr>().unwrap();
    assert!(system_resolver().resolve(ep(any), ep(addr)).is_err());
}
