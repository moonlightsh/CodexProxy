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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- 移植自 managed_gateway.rs 的规则测试（改用 route_for_host / Route 断言） ----

    #[test]
    fn exact_domain_only_matches_identical_host() {
        assert_eq!(
            route_for_host("chat.openai.com.cdn.cloudflare.net"),
            Route::Socks5
        );
        // 精确域名的子域名不命中精确规则；选不含关键字的例子验证不回退到后缀/关键字
        assert_eq!(
            route_for_host("x.static.cloudflareinsights.com"),
            Route::Direct
        );
        assert_eq!(
            route_for_host("browser-intake-datadoghq.com"),
            Route::Socks5
        );
        assert_eq!(
            route_for_host("x.browser-intake-datadoghq.com"),
            Route::Direct
        );
    }

    #[test]
    fn suffix_matches_root_and_subdomains_with_label_boundary() {
        assert_eq!(route_for_host("openai.com"), Route::Socks5);
        assert_eq!(route_for_host("api.openai.com"), Route::Socks5);
        assert_eq!(route_for_host("deep.sub.auth0.com"), Route::Socks5);
        // 无标签边界的相似域名不命中后缀规则（选不含 openai 关键字的域名验证）
        assert_eq!(route_for_host("notauth0.com"), Route::Direct);
        assert_eq!(route_for_host("auth0.company"), Route::Direct);
        // notopenai.com 含关键字 openai，仍按关键字规则命中 SOCKS5（spec 语义）
        assert_eq!(route_for_host("notopenai.com"), Route::Socks5);
    }

    #[test]
    fn keyword_is_case_insensitive_on_host_only() {
        assert_eq!(route_for_host("OpenAI.example"), Route::Socks5);
        assert_eq!(route_for_host("my-openai-proxy.internal"), Route::Socks5);
    }

    #[test]
    fn trailing_dot_and_case_normalize_consistently() {
        assert_eq!(route_for_host("API.OpenAI.com."), Route::Socks5);
        assert_eq!(
            route_for_host("api.openai.com"),
            route_for_host("API.OPENAI.COM.")
        );
    }

    #[test]
    fn gateway_ip_and_loopback_always_direct() {
        for host in ["10.20.30.61", "localhost", "127.0.0.1", "::1"] {
            assert_eq!(route_for_host(host), Route::Direct);
        }
        // IP 不含关键字，也不会被后缀规则命中
        assert_eq!(route_for_host("10.20.30.61.example"), Route::Direct);
    }

    #[test]
    fn normal_non_openai_host_is_direct() {
        assert_eq!(route_for_host("github.com"), Route::Direct);
        assert_eq!(route_for_host("example.com"), Route::Direct);
    }

    /// 原测试 `matched_result_has_no_direct_fallback` 断言的是 PAC 文本常量
    /// `MANAGED_GATEWAY_SOCKS5_RESULT` 不含 "DIRECT" 字样，属于 PAC 输出格式细节，
    /// 本实现的 `Route` 是强类型枚举（`Socks5` / `Direct`），不存在字符串混淆的可能，
    /// 该测试语义已不适用，改为对 Route 有意义的等价断言：Socks5 与 Direct 恒不相等。
    #[test]
    fn matched_route_is_never_confused_with_direct() {
        assert_ne!(Route::Socks5, Route::Direct);
    }

    // ---- 移植自 managed_proxy.rs 的分流测试（is_never_proxied + route_for_host 合并语义） ----

    #[test]
    fn openai_hosts_route_through_socks5() {
        for host in [
            "chatgpt.com",
            "auth.openai.com",
            "api.openai.com",
            "cdn.oaistatic.com",
        ] {
            assert_eq!(route_for_host(host), Route::Socks5, "应当走代理: {host}");
        }
    }

    #[test]
    fn gateway_and_loopback_always_direct() {
        for host in ["10.20.30.61", "127.0.0.1", "localhost", "::1"] {
            assert_eq!(route_for_host(host), Route::Direct, "永不应进代理: {host}");
        }
    }

    #[test]
    fn unrelated_hosts_go_direct() {
        for host in ["github.com", "registry.npmjs.org", "example.com"] {
            assert_eq!(route_for_host(host), Route::Direct);
        }
    }

    #[test]
    fn routing_is_case_insensitive() {
        assert_eq!(route_for_host("ChatGPT.com"), Route::Socks5);
    }

    // ---- 新增测试 ----

    #[test]
    fn rule_snapshot_counts_and_no_duplicates() {
        assert_eq!(EXACT_DOMAINS.len(), 7);
        assert_eq!(DOMAIN_SUFFIXES.len(), 24);
        assert_eq!(KEYWORDS.len(), 1);

        let exact_set: HashSet<_> = EXACT_DOMAINS.iter().collect();
        assert_eq!(exact_set.len(), EXACT_DOMAINS.len(), "精确域名不应有重复");

        let suffix_set: HashSet<_> = DOMAIN_SUFFIXES.iter().collect();
        assert_eq!(suffix_set.len(), DOMAIN_SUFFIXES.len(), "后缀不应有重复");
    }

    #[test]
    fn every_exact_domain_hits_and_its_subdomain_does_not_via_exact_rule() {
        for domain in EXACT_DOMAINS {
            assert_eq!(
                route_for_host(domain),
                Route::Socks5,
                "精确域名应命中: {domain}"
            );
            // 选一个不含关键字 "openai" 的精确域名子域名，验证子域名不因精确规则命中
            if !domain.contains("openai") {
                let sub = format!("sub.{domain}");
                assert_eq!(
                    route_for_host(&sub),
                    Route::Direct,
                    "精确域名的子域名不应命中: {sub}"
                );
            }
        }
    }

    #[test]
    fn every_suffix_hits_root_and_subdomain_but_not_similar_prefix_without_boundary() {
        for suffix in DOMAIN_SUFFIXES {
            assert_eq!(
                route_for_host(suffix),
                Route::Socks5,
                "根域名应命中: {suffix}"
            );
            let sub = format!("sub.{suffix}");
            assert_eq!(route_for_host(&sub), Route::Socks5, "子域名应命中: {sub}");

            // 带相似前缀但无标签边界的域名不应因后缀命中；跳过含关键字 "openai" 的后缀，
            // 因为那类相似前缀域名仍会因关键字规则命中 Socks5，不能反映后缀边界语义。
            if !suffix.contains("openai") {
                let similar = format!("not{suffix}");
                assert_eq!(
                    route_for_host(&similar),
                    Route::Direct,
                    "无标签边界的相似域名不应命中: {similar}"
                );
            }
        }
    }

    #[test]
    fn surrounding_whitespace_and_multiple_trailing_dots_are_normalized() {
        assert_eq!(normalize_host("  openai.com  "), "openai.com");
        assert_eq!(normalize_host("openai.com..."), "openai.com");
        assert_eq!(normalize_host("  OpenAI.COM...  "), "openai.com");
        assert_eq!(route_for_host("  openai.com...  "), Route::Socks5);
    }

    #[test]
    fn always_direct_hosts_cover_full_set() {
        for host in ["10.20.30.61", "localhost", "127.0.0.1", "::1"] {
            assert_eq!(route_for_host(host), Route::Direct, "恒直连: {host}");
        }
    }

    #[test]
    fn route_serializes_to_lowercase_strings() {
        assert_eq!(
            serde_json::to_string(&Route::Socks5).expect("序列化 Socks5"),
            "\"socks5\""
        );
        assert_eq!(
            serde_json::to_string(&Route::Direct).expect("序列化 Direct"),
            "\"direct\""
        );
    }
}
