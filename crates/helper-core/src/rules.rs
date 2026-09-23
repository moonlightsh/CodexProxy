//! OpenAI 域名规则快照与主机分流判定（设计 §1.1、§6）。
//!
//! 移植自 CodexPlusPlus `managed_gateway.rs` 的规则与匹配函数，去掉了 PAC 生成相关部分。
//! 这里是分流的唯一判定点：本地代理只调用 [`route_for_host`]。

use serde::{Deserialize, Serialize};

use crate::consts;

/// 精确域名（来自 BlackMatrix7 OpenAI Clash 规则 2025-06-06 域名类快照）。
pub const EXACT_DOMAINS: &[&str] = &[
    "browser-intake-datadoghq.com",
    "chat.openai.com.cdn.cloudflare.net",
    "openai-api.arkoselabs.com",
    "openaicom-api-bdcpf8c6d2e9atf6.z01.azurefd.net",
    "openaicomproductionae4b.blob.core.windows.net",
    "production-openaicom-storage.azureedge.net",
    "static.cloudflareinsights.com",
];

/// 域名后缀（匹配根域名及其所有子域名，按 DNS 标签边界）。
pub const DOMAIN_SUFFIXES: &[&str] = &[
    "ai.com",
    "algolia.net",
    "api.statsig.com",
    "auth0.com",
    "chatgpt.com",
    "chatgpt.livekit.cloud",
    "client-api.arkoselabs.com",
    "events.statsigapi.net",
    "featuregates.org",
    "host.livekit.cloud",
    "identrust.com",
    "intercom.io",
    "intercomcdn.com",
    "launchdarkly.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "observeit.net",
    "openai.com",
    "openaiapi-site.azureedge.net",
    "openaicom.imgix.net",
    "segment.io",
    "sentry.io",
    "stripe.com",
    "turn.livekit.cloud",
];

/// 域名关键字（只匹配规范化后的主机名，不匹配 URL / 路径 / 查询参数）。
pub const KEYWORDS: &[&str] = &["openai"];

/// 分流结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// 经上游 SOCKS5。失败时必须 502，不得降级直连。
    Socks5,
    /// 由本机直接连出。
    Direct,
}

/// 主机名规范化：去首尾空白、转小写、移除末尾的点。
pub fn normalize_host(host: &str) -> String {
    let mut host = host.trim().to_lowercase();
    while host.ends_with('.') {
        host.pop();
    }
    host
}

/// 网关与回环地址恒为直连，在任何规则之前判定。
fn is_always_direct(host: &str) -> bool {
    host == consts::GATEWAY_HOST
        || host == consts::SOCKS5_HOST
        || host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
}

/// 判断后缀是否按 DNS 标签边界匹配（`openai.com` 匹配 `api.openai.com`，不匹配 `notopenai.com`）。
fn suffix_matches_label_boundary(host: &str, suffix: &str) -> bool {
    host == suffix
        || (host.len() > suffix.len()
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
}

/// 主机分流判定：恒直连 → 精确域名 → 后缀 → 关键字，命中任一规则走 SOCKS5，否则直连。
pub fn route_for_host(host: &str) -> Route {
    let host = normalize_host(host);
    if is_always_direct(&host) {
        return Route::Direct;
    }
    if EXACT_DOMAINS.contains(&host.as_str()) {
        return Route::Socks5;
    }
    if DOMAIN_SUFFIXES
        .iter()
        .any(|suffix| suffix_matches_label_boundary(&host, suffix))
    {
        return Route::Socks5;
    }
    if KEYWORDS.iter().any(|keyword| host.contains(keyword)) {
        return Route::Socks5;
    }
    Route::Direct
}
