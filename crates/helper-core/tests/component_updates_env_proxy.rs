//! 直连客户端不受环境变量代理影响（组件更新检查设计 §4.2：直连 client 用 `.no_proxy()`）。
//!
//! 单独成一个测试二进制、只含一个测试：Rust 2024 中修改进程环境变量需要 `unsafe`，
//! 只有在没有其他线程并发读写环境时才安全——因此在启动任何运行时/线程之前设置。

use std::io::ErrorKind;
use std::net::TcpListener;
use std::time::Duration;

use helper_core::component_updates::{NetworkPath, build_client};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

#[test]
fn direct_client_ignores_env_proxy() {
    // 陷阱代理：只监听不应答。若客户端读了环境变量代理，就会连到这里并卡到超时。
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let trap_url = format!("http://{}", trap.local_addr().unwrap());
    // SAFETY：此时进程内尚未启动运行时或其他线程（本二进制只有这一个测试），
    // 不存在并发读写环境变量。
    unsafe {
        for name in PROXY_VARS {
            std::env::set_var(name, &trap_url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let client = build_client(
            &NetworkPath::Direct { reason: None },
            Duration::from_secs(2),
        )
        .expect("直连 client 构建成功");
        let response = client
            .get(format!("{}/probe", server.uri()))
            .send()
            .await
            .expect("直连请求应直达目标，不经环境变量代理");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    });

    // 陷阱代理不得收到任何连接。
    match trap.accept() {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        Ok((_, peer)) => panic!("直连 client 读了环境变量代理：陷阱收到来自 {peer} 的连接"),
        Err(error) => panic!("检查陷阱监听失败：{error}"),
    }
}
