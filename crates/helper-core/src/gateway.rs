//! 网关 Key 校验与 TCP 可达性检测（设计 §5.1 第 1、7 步）。
//!
//! 不记录 Authorization 头；错误与返回值不携带 Key。

use std::time::Duration;

use crate::types::KeyCheck;

/// Key 校验请求的整体超时。
pub const KEY_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// TCP 可达性检测的连接超时。
pub const TCP_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 用固定网关校验 Key：`GET {GATEWAY_BASE_URL}/v1/models`，Bearer 鉴权。
pub async fn verify_key(key: &str) -> KeyCheck {
    verify_key_at(crate::consts::GATEWAY_BASE_URL, key, KEY_CHECK_TIMEOUT).await
}

/// 指定网关地址与超时的校验入口（供测试注入 mock 网关）。
///
/// - 2xx → `Ok`；401 / 403 → `Unauthorized`；5xx → `ServerError`。
/// - 超时、网络错误、重定向响应或其他无法判定的状态码 → `TimeoutOrNetwork`。
/// - Key 含控制字符等无法作为请求头值的字节时 → `Unauthorized`：这类 Key 不可能被任何网关
///   接受，属于 Key 本身的问题而非网络问题，不应引导用户“仍然保存”。
///
/// 客户端固定关闭代理（网关在内网，即便环境变量或系统代理配置有误也不应影响本工具自身的
/// 请求）、关闭重定向跟随（避免把 Bearer 头带去重定向目标主机）。
pub async fn verify_key_at(base_url: &str, key: &str, timeout: Duration) -> KeyCheck {
    // 先单独校验 Key 能否作为请求头值，避免把 base_url 等其他构建错误也误判为 Key 无效。
    if reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")).is_err() {
        return KeyCheck::Unauthorized;
    }
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(_) => return KeyCheck::TimeoutOrNetwork,
    };
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let response = match client.get(&url).bearer_auth(key).send().await {
        Ok(response) => response,
        Err(_) => return KeyCheck::TimeoutOrNetwork,
    };
    match response.status().as_u16() {
        200..=299 => KeyCheck::Ok,
        401 | 403 => KeyCheck::Unauthorized,
        500..=599 => KeyCheck::ServerError,
        _ => KeyCheck::TimeoutOrNetwork,
    }
}

/// TCP 可达性：在超时内能否建立连接（设计 §5.1 第 7 步，仅用于状态灯）。
pub async fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    // 用 (host, port) 元组解析，IPv6 字面量（如 `::1`）无需方括号。
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn maps_2xx_to_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        assert_eq!(
            verify_key_at(&server.uri(), "sk-test", KEY_CHECK_TIMEOUT).await,
            KeyCheck::Ok
        );
    }

    #[tokio::test]
    async fn maps_401_and_403_to_unauthorized() {
        for status in [401, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            assert_eq!(
                verify_key_at(&server.uri(), "sk-bad", KEY_CHECK_TIMEOUT).await,
                KeyCheck::Unauthorized
            );
        }
    }

    #[tokio::test]
    async fn maps_5xx_to_server_error_and_dead_host_to_timeout_or_network() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        assert_eq!(
            verify_key_at(&server.uri(), "sk-x", KEY_CHECK_TIMEOUT).await,
            KeyCheck::ServerError
        );
        // 保留端口、无人监听：连接被立即拒绝，映射为 TimeoutOrNetwork，不误判为 Key 无效。
        assert_eq!(
            verify_key_at("http://127.0.0.1:9", "sk-x", KEY_CHECK_TIMEOUT).await,
            KeyCheck::TimeoutOrNetwork
        );
    }

    #[tokio::test]
    async fn slow_response_times_out_as_timeout_or_network() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;
        assert_eq!(
            verify_key_at(&server.uri(), "sk-slow", Duration::from_millis(200)).await,
            KeyCheck::TimeoutOrNetwork
        );
    }

    #[tokio::test]
    async fn request_carries_bearer_authorization_header() {
        let server = MockServer::start().await;
        // 未匹配到期望的 Authorization 头时 wiremock 不会返回该 mock 的响应，请求随之失败，
        // 从而验证 Bearer 头确实被发送。
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sk-carried"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            verify_key_at(&server.uri(), "sk-carried", KEY_CHECK_TIMEOUT).await,
            KeyCheck::Ok
        );
    }

    #[tokio::test]
    async fn redirect_is_not_followed_and_target_is_never_requested() {
        let server = MockServer::start().await;
        let redirect_target = format!("{}/evil", server.uri());
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", redirect_target.as_str()),
            )
            .mount(&server)
            .await;
        // 重定向目标指向同一 server 的另一路径；断言其从未被请求，证明确未跟随重定向。
        Mock::given(method("GET"))
            .and(path("/evil"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(
            verify_key_at(&server.uri(), "sk-redirect", KEY_CHECK_TIMEOUT).await,
            KeyCheck::TimeoutOrNetwork
        );
    }

    #[tokio::test]
    async fn trailing_slash_in_base_url_still_hits_v1_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let base_url = format!("{}/", server.uri());
        assert_eq!(
            verify_key_at(&base_url, "sk-slash", KEY_CHECK_TIMEOUT).await,
            KeyCheck::Ok
        );
    }

    #[tokio::test]
    async fn key_with_invalid_header_bytes_is_unauthorized_not_network_error() {
        let server = MockServer::start().await;
        // 换行符无法作为请求头值发送；请求应在构建阶段失败，且不 panic。
        assert_eq!(
            verify_key_at(&server.uri(), "sk-with\nnewline", KEY_CHECK_TIMEOUT).await,
            KeyCheck::Unauthorized
        );
    }

    #[tokio::test]
    async fn tcp_reachable_true_for_bound_listener_false_for_released_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(tcp_reachable("127.0.0.1", addr.port(), TCP_PROBE_TIMEOUT).await);
        drop(listener);
        assert!(!tcp_reachable("127.0.0.1", addr.port(), TCP_PROBE_TIMEOUT).await);
    }

    #[tokio::test]
    async fn tcp_reachable_accepts_bare_ipv6_literal() {
        // 部分环境没有 IPv6 回环，绑定失败时跳过。
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_reachable("::1", port, TCP_PROBE_TIMEOUT).await);
    }

    #[tokio::test]
    async fn invalid_base_url_is_network_error_not_key_rejection() {
        assert_eq!(
            verify_key_at("not a url", "sk-valid", KEY_CHECK_TIMEOUT).await,
            KeyCheck::TimeoutOrNetwork
        );
    }
}
