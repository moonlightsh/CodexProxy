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
/// 2xx → Ok；401 / 403 → Unauthorized；5xx → ServerError；超时 / 网络错误 / 其他 → TimeoutOrNetwork。
pub async fn verify_key_at(base_url: &str, key: &str, timeout: Duration) -> KeyCheck {
    let _ = (base_url, key, timeout);
    todo!("阶段 1 任务 1.6")
}

/// TCP 可达性：在超时内能否建立连接。
pub async fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let _ = (host, port, timeout);
    todo!("阶段 1 任务 1.6")
}
