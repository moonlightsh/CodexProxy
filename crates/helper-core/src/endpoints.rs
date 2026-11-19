//! 网关 URL 与上游 SOCKS5 的解析、规范化与展示。
//!
//! 设计：`docs/superpowers/specs/2026-11-16-configurable-endpoints-design.md` §1.2–§1.3。
//! 本类型是 `crate::endpoints::Endpoints`，与组件检测用的 `component_updates::Endpoints` 不是同一个。

use crate::rules;
use crate::state::HelperState;

/// 去空白后仍是空串。
pub const MSG_EMPTY: &str = "不能为空";
/// 含空白、控制字符或非可打印 ASCII。
pub const MSG_GRAPHIC: &str = "只能包含可打印 ASCII 字符";
/// 网关协议不是 http / https。
pub const MSG_SCHEME: &str = "协议只允许 http 或 https";
/// 网关 URL 没有主机。
pub const MSG_HOST: &str = "必须包含主机";
/// 网关 URL 带了用户名或密码。
pub const MSG_USERINFO: &str = "不能包含用户名或密码";
/// 网关 URL 带了查询参数或片段。
pub const MSG_QUERY: &str = "不能包含查询参数或片段";
/// 网关 URL 无法解析，或没有可用端口。
pub const MSG_URL: &str = "不是合法的 URL";
/// SOCKS5 不是 `host:port`。
pub const MSG_SOCKS_FORM: &str = "格式应为 host:port";
/// SOCKS5 端口不是 1–65535。
pub const MSG_SOCKS_PORT: &str = "端口必须在 1–65535 之间";
/// SOCKS5 写了协议。
pub const MSG_SOCKS_SCHEME: &str = "不要带协议（例如不要写 socks5://）";

/// 出错的是哪一侧输入，供界面就地提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointField {
    /// 模型网关 URL。
    Gateway,
    /// 上游 SOCKS5 地址。
    Socks5,
}

impl EndpointField {
    /// 日志与错误载荷里的稳定字段名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Socks5 => "socks5",
        }
    }
}

/// 端点解析失败。`Display` 只输出中文原因，不含字段名。
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EndpointError {
    /// 哪一侧输入不合法。
    pub field: EndpointField,
    /// 中文原因，与本模块的 `MSG_*` 常量相同。
    pub message: String,
}

/// 一次生效的网关与上游 SOCKS5。字段都已规范化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// 规范化后的网关 URL，无末尾 `/`，无 query 与 fragment。
    pub gateway_base_url: String,
    /// 小写主机，IPv6 不带方括号。
    pub gateway_host: String,
    /// 显式端口，或按 scheme 取 80 / 443。
    pub gateway_port: u16,
    /// 小写主机，IPv6 不带方括号。
    pub socks5_host: String,
    /// 1–65535。
    pub socks5_port: u16,
}

impl Endpoints {
    /// 来自 consts 的默认值。
    pub fn default_values() -> Self {
        Self::parse(
            crate::consts::GATEWAY_BASE_URL,
            &format!(
                "{}:{}",
                crate::consts::SOCKS5_HOST,
                crate::consts::SOCKS5_PORT
            ),
        )
        .expect("内置默认网关与 SOCKS5 地址必须能被解析")
    }

    /// 以 `defaults` 为底。state 里为 `Some` 的项单独解析；失败则该项回落默认值并记日志。
    ///
    /// 日志只记字段名和原因，不记用户输入的原文。
    pub fn resolve(state: &HelperState, defaults: &Endpoints) -> Self {
        let mut resolved = defaults.clone();
        if let Some(gateway) = state.gateway_base_url.as_deref() {
            match Self::parse(gateway, &defaults.socks5_addr()) {
                Ok(parsed) => {
                    resolved.gateway_base_url = parsed.gateway_base_url;
                    resolved.gateway_host = parsed.gateway_host;
                    resolved.gateway_port = parsed.gateway_port;
                }
                Err(error) => log_fallback(&error),
            }
        }
        if let Some(socks) = state.socks5_addr.as_deref() {
            match Self::parse(&defaults.gateway_base_url, socks) {
                Ok(parsed) => {
                    resolved.socks5_host = parsed.socks5_host;
                    resolved.socks5_port = parsed.socks5_port;
                }
                Err(error) => log_fallback(&error),
            }
        }
        resolved
    }

    /// 解析并校验用户输入。先网关，再 SOCKS5，返回第一个错误。
    pub fn parse(gateway_url: &str, socks5_addr: &str) -> Result<Self, EndpointError> {
        let (gateway_base_url, gateway_host, gateway_port) = parse_gateway(gateway_url)?;
        let (socks5_host, socks5_port) = parse_socks(socks5_addr)?;
        Ok(Self {
            gateway_base_url,
            gateway_host,
            gateway_port,
            socks5_host,
            socks5_port,
        })
    }

    /// 展示用 `host:port`。IPv6 主机加方括号。
    pub fn gateway_addr(&self) -> String {
        display_addr(&self.gateway_host, self.gateway_port)
    }

    /// 展示用 `host:port`。IPv6 主机加方括号。
    pub fn socks5_addr(&self) -> String {
        display_addr(&self.socks5_host, self.socks5_port)
    }
}

fn log_fallback(error: &EndpointError) {
    crate::log::event(
        "endpoints.fallback",
        serde_json::json!({
            "field": error.field.as_str(),
            "message": error.message,
        }),
    );
}

fn failure(field: EndpointField, message: &str) -> EndpointError {
    EndpointError {
        field,
        message: message.to_string(),
    }
}

/// 去掉两端空白。空串与非可打印 ASCII 在 URL / 地址解析之前拒绝。
fn prepare(field: EndpointField, raw: &str) -> Result<&str, EndpointError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(failure(field, MSG_EMPTY));
    }
    if !trimmed.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(failure(field, MSG_GRAPHIC));
    }
    Ok(trimmed)
}

fn parse_gateway(raw: &str) -> Result<(String, String, u16), EndpointError> {
    let text = prepare(EndpointField::Gateway, raw)?;
    let url = reqwest::Url::parse(text).map_err(|_| failure(EndpointField::Gateway, MSG_URL))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(failure(EndpointField::Gateway, MSG_SCHEME));
    }
    let Some(host_str) = url.host_str() else {
        return Err(failure(EndpointField::Gateway, MSG_HOST));
    };
    // 当前 `url` 的 `host_str()` 对 IPv6 带方括号；存盘与展示规则要求不带。
    let host_str = host_str
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host_str);
    if !url.username().is_empty() || url.password().is_some() {
        return Err(failure(EndpointField::Gateway, MSG_USERINFO));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(failure(EndpointField::Gateway, MSG_QUERY));
    }
    let host = rules::normalize_host(host_str);
    if host.is_empty() {
        return Err(failure(EndpointField::Gateway, MSG_HOST));
    }
    let Some(port) = url.port_or_known_default() else {
        return Err(failure(EndpointField::Gateway, MSG_URL));
    };
    Ok((canonical_gateway_url(&url, &host, port), host, port))
}

fn canonical_gateway_url(url: &reqwest::Url, host: &str, port: u16) -> String {
    let mut path = url.path().to_string();
    while path.ends_with('/') {
        path.pop();
    }
    let host_part = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port_part = if url.port().is_some() {
        format!(":{port}")
    } else {
        String::new()
    };
    format!("{}://{host_part}{port_part}{path}", url.scheme())
}

fn parse_socks(raw: &str) -> Result<(String, u16), EndpointError> {
    let text = prepare(EndpointField::Socks5, raw)?;
    if text.contains("://") {
        return Err(failure(EndpointField::Socks5, MSG_SOCKS_SCHEME));
    }
    let (host_raw, port_text) = split_socks(text)?;
    let port: u16 = port_text
        .parse()
        .map_err(|_| failure(EndpointField::Socks5, MSG_SOCKS_PORT))?;
    if port == 0 {
        return Err(failure(EndpointField::Socks5, MSG_SOCKS_PORT));
    }
    let host = rules::normalize_host(host_raw);
    if host.is_empty() {
        return Err(failure(EndpointField::Socks5, MSG_SOCKS_FORM));
    }
    Ok((host, port))
}

fn split_socks(text: &str) -> Result<(&str, &str), EndpointError> {
    if let Some(inside) = text.strip_prefix('[') {
        let Some((host, after)) = inside.split_once(']') else {
            return Err(failure(EndpointField::Socks5, MSG_SOCKS_FORM));
        };
        if host.is_empty() {
            return Err(failure(EndpointField::Socks5, MSG_SOCKS_FORM));
        }
        let Some(port_text) = after.strip_prefix(':') else {
            return Err(failure(EndpointField::Socks5, MSG_SOCKS_FORM));
        };
        if port_text.is_empty() {
            return Err(failure(EndpointField::Socks5, MSG_SOCKS_PORT));
        }
        return Ok((host, port_text));
    }

    let Some((host, port_text)) = text.rsplit_once(':') else {
        return Err(failure(EndpointField::Socks5, MSG_SOCKS_FORM));
    };
    if host.is_empty() || host.contains(':') || port_text.is_empty() {
        return Err(failure(
            EndpointField::Socks5,
            if port_text.is_empty() {
                MSG_SOCKS_PORT
            } else {
                MSG_SOCKS_FORM
            },
        ));
    }
    Ok((host, port_text))
}

fn display_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::HelperState;

    #[test]
    fn parse_accepts_documented_forms() {
        let cases = [
            (
                "http://10.20.30.61:8080",
                "10.20.30.61:7891",
                "http://10.20.30.61:8080",
                "10.20.30.61",
                8080u16,
                "10.20.30.61",
                7891u16,
                "10.20.30.61:8080",
                "10.20.30.61:7891",
            ),
            (
                "  HTTP://Example.COM:8080/  ",
                " Example.COM:7891 ",
                "http://example.com:8080",
                "example.com",
                8080,
                "example.com",
                7891,
                "example.com:8080",
                "example.com:7891",
            ),
            (
                "https://example.com",
                "[::1]:7891",
                "https://example.com",
                "example.com",
                443,
                "::1",
                7891,
                "example.com:443",
                "[::1]:7891",
            ),
            (
                "http://127.0.0.1/proxy/",
                "localhost:1",
                "http://127.0.0.1/proxy",
                "127.0.0.1",
                80,
                "localhost",
                1,
                "127.0.0.1:80",
                "localhost:1",
            ),
            (
                "http://[::1]:8080",
                "[2001:db8::1]:65535",
                "http://[::1]:8080",
                "::1",
                8080,
                "2001:db8::1",
                65535,
                "[::1]:8080",
                "[2001:db8::1]:65535",
            ),
            (
                "http://example.com:80/proxy///",
                "127.0.0.1:7891",
                "http://example.com/proxy",
                "example.com",
                80,
                "127.0.0.1",
                7891,
                "example.com:80",
                "127.0.0.1:7891",
            ),
            // 设计 §1.3：先去掉首尾空白（含换行）。计划拒绝表把末尾 `\n` 标成
            // MSG_GRAPHIC，与“先 trim”冲突，这里按设计接受。
            (
                "http://10.20.30.61:8080\n",
                "127.0.0.1:1\n",
                "http://10.20.30.61:8080",
                "10.20.30.61",
                8080,
                "127.0.0.1",
                1,
                "10.20.30.61:8080",
                "127.0.0.1:1",
            ),
        ];

        for (gateway, socks, base, ghost, gport, shost, sport, gaddr, saddr) in cases {
            let got = Endpoints::parse(gateway, socks).unwrap_or_else(|error| {
                panic!("parse({gateway:?}, {socks:?}) 应成功，实际：{error}");
            });
            assert_eq!(got.gateway_base_url, base, "gateway={gateway:?}");
            assert_eq!(got.gateway_host, ghost, "gateway={gateway:?}");
            assert_eq!(got.gateway_port, gport, "gateway={gateway:?}");
            assert_eq!(got.socks5_host, shost, "socks={socks:?}");
            assert_eq!(got.socks5_port, sport, "socks={socks:?}");
            assert_eq!(got.gateway_addr(), gaddr, "gateway={gateway:?}");
            assert_eq!(got.socks5_addr(), saddr, "socks={socks:?}");
        }
    }

    #[test]
    fn parse_rejects() {
        let gateway_ok = "http://127.0.0.1:9";
        let socks_ok = "127.0.0.1:1";
        let cases = [
            ("", socks_ok, EndpointField::Gateway, MSG_EMPTY),
            ("   ", socks_ok, EndpointField::Gateway, MSG_EMPTY),
            (
                "http://10.20.30.61:80\n80",
                socks_ok,
                EndpointField::Gateway,
                MSG_GRAPHIC,
            ),
            (
                "http://用户:8080",
                socks_ok,
                EndpointField::Gateway,
                MSG_GRAPHIC,
            ),
            (
                "ftp://10.20.30.61",
                socks_ok,
                EndpointField::Gateway,
                MSG_SCHEME,
            ),
            ("http://", socks_ok, EndpointField::Gateway, MSG_URL),
            (
                "http://user:pass@10.20.30.61:8080",
                socks_ok,
                EndpointField::Gateway,
                MSG_USERINFO,
            ),
            (
                "http://10.20.30.61:8080?q=1",
                socks_ok,
                EndpointField::Gateway,
                MSG_QUERY,
            ),
            (
                "http://10.20.30.61:8080#frag",
                socks_ok,
                EndpointField::Gateway,
                MSG_QUERY,
            ),
            (gateway_ok, "", EndpointField::Socks5, MSG_EMPTY),
            (
                gateway_ok,
                "socks5://127.0.0.1:1",
                EndpointField::Socks5,
                MSG_SOCKS_SCHEME,
            ),
            (
                gateway_ok,
                "127.0.0.1",
                EndpointField::Socks5,
                MSG_SOCKS_FORM,
            ),
            (gateway_ok, "[::1]", EndpointField::Socks5, MSG_SOCKS_FORM),
            (
                gateway_ok,
                "::1:7891",
                EndpointField::Socks5,
                MSG_SOCKS_FORM,
            ),
            (
                gateway_ok,
                "127.0.0.1:0",
                EndpointField::Socks5,
                MSG_SOCKS_PORT,
            ),
            (
                gateway_ok,
                "127.0.0.1:65536",
                EndpointField::Socks5,
                MSG_SOCKS_PORT,
            ),
            (
                gateway_ok,
                "127.0.0.1:abc",
                EndpointField::Socks5,
                MSG_SOCKS_PORT,
            ),
            (
                gateway_ok,
                "127.0.0.1:\n1",
                EndpointField::Socks5,
                MSG_GRAPHIC,
            ),
        ];

        for (gateway, socks, field, message) in cases {
            let error = Endpoints::parse(gateway, socks)
                .expect_err(&format!("parse({gateway:?}, {socks:?}) 应失败"));
            assert_eq!(error.field, field, "gateway={gateway:?} socks={socks:?}");
            assert_eq!(
                error.message, message,
                "gateway={gateway:?} socks={socks:?}"
            );
        }
    }

    #[test]
    fn default_values_match_consts() {
        let got = Endpoints::default_values();
        assert_eq!(got.gateway_base_url, "http://10.20.30.61:8080");
        assert_eq!(got.gateway_host, "10.20.30.61");
        assert_eq!(got.gateway_port, 8080);
        assert_eq!(got.socks5_host, "10.20.30.61");
        assert_eq!(got.socks5_port, 7891);
    }

    #[test]
    fn resolve_uses_defaults_when_state_fields_absent() {
        let defaults = Endpoints::default_values();
        let got = Endpoints::resolve(&HelperState::default(), &defaults);
        assert_eq!(got, defaults);
    }

    #[test]
    fn resolve_overrides_only_the_present_field() {
        let defaults = Endpoints::default_values();

        let state = HelperState {
            gateway_base_url: Some("https://gw.example/proxy/".into()),
            ..Default::default()
        };
        let got = Endpoints::resolve(&state, &defaults);
        assert_eq!(got.gateway_base_url, "https://gw.example/proxy");
        assert_eq!(got.gateway_port, 443);
        assert_eq!(got.socks5_host, defaults.socks5_host);
        assert_eq!(got.socks5_port, defaults.socks5_port);
        assert_eq!(got.socks5_addr(), "10.20.30.61:7891");

        let state = HelperState {
            socks5_addr: Some("[::1]:9".into()),
            ..Default::default()
        };
        let got = Endpoints::resolve(&state, &defaults);
        assert_eq!(got.gateway_base_url, defaults.gateway_base_url);
        assert_eq!(got.gateway_host, defaults.gateway_host);
        assert_eq!(got.gateway_port, defaults.gateway_port);
        assert_eq!(got.socks5_host, "::1");
        assert_eq!(got.socks5_port, 9);
    }

    #[test]
    fn resolve_falls_back_and_logs_when_one_field_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        crate::log::init(dir.path());
        let defaults = Endpoints::default_values();

        let state = HelperState {
            gateway_base_url: Some("ftp://bad".into()),
            socks5_addr: Some("127.0.0.1:9".into()),
            ..Default::default()
        };
        let got = Endpoints::resolve(&state, &defaults);
        assert_eq!(got.gateway_base_url, defaults.gateway_base_url);
        assert_eq!(got.gateway_host, defaults.gateway_host);
        assert_eq!(got.gateway_port, defaults.gateway_port);
        assert_eq!(got.socks5_host, "127.0.0.1");
        assert_eq!(got.socks5_port, 9);
        let text = std::fs::read_to_string(crate::log::log_file_path().unwrap()).unwrap();
        assert!(text.contains("endpoints.fallback"), "{text}");
        assert!(text.contains("gateway"), "{text}");

        let state = HelperState {
            gateway_base_url: Some("https://gw.example/proxy/".into()),
            socks5_addr: Some("nope".into()),
            ..Default::default()
        };
        let got = Endpoints::resolve(&state, &defaults);
        assert_eq!(got.gateway_base_url, "https://gw.example/proxy");
        assert_eq!(got.gateway_host, "gw.example");
        assert_eq!(got.gateway_port, 443);
        assert_eq!(got.socks5_host, defaults.socks5_host);
        assert_eq!(got.socks5_port, defaults.socks5_port);
        let text = std::fs::read_to_string(crate::log::log_file_path().unwrap()).unwrap();
        assert!(text.contains("socks5"), "{text}");
    }
}
