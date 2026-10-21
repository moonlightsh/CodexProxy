//! 组件更新检查（ChatGPT Desktop / Codex CLI）。
//!
//! 设计文档：`docs/superpowers/specs/2026-10-20-component-update-check-design.md`
//!
//! 只检测、不下载不安装：展示本地与远端最新版本，有更新时给出官方下载入口。
//!
//! 约束与不变量：
//! - **门禁的本进程放行对进程内任何经本地代理端口（默认 17891）的客户端生效，门禁本身不校验
//!   目标**（设计 §7 方案 d）。因此约定：进程内只有组件检测把本地代理当代理使用，
//!   **新增进程内代理客户端须评审**（评估是否落入门禁放行面与上游白名单约束）。
//! - 检测 URL 全部来自 `consts` 的编译期常量，不接受外部输入的目标地址。
//! - 检测全程无凭据：不带 Authorization / Cookie 等任何凭据头，只带固定 User-Agent。
//! - Secure 模式下代理未就绪时零出站，绝不回退直连（设计 C4）。

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;

    /// reqwest 必须启用 TLS 后端：对 https 地址真正建连并发出 TLS ClientHello。
    #[tokio::test]
    async fn https_scheme_sends_tls_client_hello() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 3];
            socket.read_exact(&mut head).await.unwrap();
            head
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let request = tokio::spawn(async move {
            let _ = client
                .get(format!("https://127.0.0.1:{port}/"))
                .send()
                .await;
        });
        let head = tokio::time::timeout(Duration::from_secs(3), accept)
            .await
            .expect("3 秒内没有收到 TLS 握手")
            .unwrap();
        request.abort();
        assert_eq!(head[0], 0x16, "首字节应为 TLS 握手记录类型");
        assert_eq!(head[1], 0x03, "第二字节应为 TLS 主版本号");
    }
}
