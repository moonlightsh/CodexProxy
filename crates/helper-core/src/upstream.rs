//! 上游 TLS/mTLS 通道（设计 §7）。
//!
//! `TlsUpstreamConfig` 携带注册包下发的信任锚（CA PEM）、客户端证书与私钥，
//! `build_connector` 产出仅信任该 CA、提交客户端证书、最低 TLS1.2 / 优先 TLS1.3 的连接器。
//! 任何 TLS 失败（证书不可信、SAN 不匹配、私钥不匹配）都是终止性错误：调用方不得回退明文。
//!
//! [`connect_secure`] 组合完整序列：TCP → TLS(mTLS) → SOCKS5 用户名密码 → CONNECT → 装箱。

use std::fmt;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use tokio::net::TcpStream;

use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

use crate::credential::Secret;
use crate::proxy::{
    BoxedTransport, SOCKS5_TIMEOUT, build_socks5_auth_request, build_socks5_connect_request,
    socks5_auth_handshake,
};

/// 允许的 TLS 协议版本：优先 1.3，最低 1.2（设计 §4.2）。
const TLS_VERSIONS: &[&rustls::SupportedProtocolVersion] =
    &[&rustls::version::TLS13, &rustls::version::TLS12];

/// TLS/mTLS 上游配置。
///
/// - `server_name`：管理员指定的服务端真实身份（TLS SNI 与证书校验名），不是连接 IP；
/// - `ca_pem`：注册包下发的服务端 CA（PEM），是唯一的信任锚；
/// - `client_cert_pem` / `client_key_pem`：设备客户端证书与软件私钥（私钥全程 [`Secret`] 包装）。
pub struct TlsUpstreamConfig {
    pub server_name: String,
    pub ca_pem: Vec<u8>,
    pub client_cert_pem: Vec<u8>,
    pub client_key_pem: Secret,
}

impl TlsUpstreamConfig {
    /// 构建连接器：roots 仅含 `ca_pem`；`with_client_auth_cert` 提交客户端证书；
    /// TLS1.3 优先、最低 1.2；绝不使用 `dangerous` 跳过证书校验配置。
    pub fn build_connector(&self) -> anyhow::Result<tokio_rustls::TlsConnector> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut roots = rustls::RootCertStore::empty();
        let ca_certs = load_certs(&self.ca_pem, "服务端 CA")?;
        if ca_certs.is_empty() {
            anyhow::bail!("服务端 CA 为空：拒绝建立无信任锚的 TLS 连接");
        }
        for cert in ca_certs {
            roots
                .add(cert)
                .map_err(|_| anyhow::anyhow!("服务端 CA 证书无法加入信任锚"))?;
        }

        let client_certs = load_certs(&self.client_cert_pem, "客户端证书")?;
        if client_certs.is_empty() {
            anyhow::bail!("客户端证书为空");
        }
        let client_key = load_key(&self.client_key_pem)?;

        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(TLS_VERSIONS)
            .map_err(|_| anyhow::anyhow!("TLS 协议版本配置不受支持"))?
            .with_root_certificates(roots)
            .with_client_auth_cert(client_certs, client_key)
            .map_err(|_| anyhow::anyhow!("客户端证书或私钥无法用于 mTLS"))?;
        // 设计 §4.2：首版禁用客户端会话恢复（TLS1.2 session id/ticket 与 TLS1.3 票据均不存储），
        // 不让会话恢复成为证书吊销的旁路；每次连接都做完整握手并重新校验证书。
        // 0-RTT（enable_early_data）rustls 默认关闭，且依赖会话恢复，此处一并失效。
        config.resumption = rustls::client::Resumption::disabled();

        Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
    }

    /// 解析出的 TLS ServerName（校验名），失败即配置错误。
    fn server_name(&self) -> anyhow::Result<ServerName<'static>> {
        ServerName::try_from(self.server_name.clone())
            .map_err(|_| anyhow::anyhow!("TLS ServerName 无效：{}", self.server_name))
    }
}

/// 解析 PEM 证书串；错误信息不含 PEM 内容。
fn load_certs(pem: &[u8], what: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject as _;
    let mut certs = Vec::new();
    for (index, result) in CertificateDer::pem_slice_iter(pem).enumerate() {
        match result {
            Ok(cert) => certs.push(cert),
            Err(_) => anyhow::bail!("{what} 第 {index} 段 PEM 解析失败"),
        }
    }
    Ok(certs)
}

/// 解析 PEM 私钥；错误信息不含私钥内容。
fn load_key(key: &Secret) -> anyhow::Result<PrivateKeyDer<'static>> {
    use rustls::pki_types::pem::PemObject as _;
    PrivateKeyDer::from_pem_slice(key.expose().as_bytes())
        .map_err(|_| anyhow::anyhow!("客户端私钥 PEM 解析失败"))
}

/// 在已建立的 TCP 连接上完成 TLS 握手：校验服务端证书与 ServerName，并提交客户端证书。
pub async fn wrap_tls(
    conn: tokio::net::TcpStream,
    cfg: &TlsUpstreamConfig,
) -> anyhow::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let connector = cfg.build_connector()?;
    let name = cfg.server_name()?;
    let stream = connector
        .connect(name, conn)
        .await
        .map_err(|_| anyhow::anyhow!("TLS 握手失败（证书校验或 mTLS 未通过）"))?;
    Ok(stream)
}

/// 安全上游：地址 + TLS/mTLS 配置 + SOCKS5 用户名密码（设计 §7）。
///
/// `host`/`port` 是 TCP 连接地址；服务端身份校验只看 `tls.server_name`。
/// `Debug` 只输出地址、校验名与用户名，密码与 PEM（含私钥）一律不输出。
pub struct SecureUpstream {
    pub host: String,
    pub port: u16,
    pub tls: TlsUpstreamConfig,
    pub username: String,
    pub password: Secret,
}

impl fmt::Debug for SecureUpstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecureUpstream")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("server_name", &self.tls.server_name)
            .field("username", &self.username)
            .field("password", &self.password)
            .finish_non_exhaustive()
    }
}

/// 经安全上游建立到目标的隧道：TCP → TLS(mTLS) → SOCKS5 用户名密码 → CONNECT → 装箱。
///
/// fail-closed：任何阶段失败都直接返回错误，不重试、不回退明文/旧入口/直连；
/// TLS 未通过时一个 SOCKS5 字节都不发。
///
/// 超时（分两段，与旧无认证路径一致，全程≤ 2×[`SOCKS5_TIMEOUT`]）：
/// 1. TCP 建连独占一个 `SOCKS5_TIMEOUT`；
/// 2. TLS 握手 + 认证问候/方法应答 + RFC1929 子协商 + CONNECT 应答**共用**一个 `SOCKS5_TIMEOUT`。
///
/// 连接器每次现建、不缓存，且已禁用会话恢复：每条连接都是完整握手。
pub async fn connect_secure(
    up: &SecureUpstream,
    host: &str,
    port: u16,
) -> anyhow::Result<BoxedTransport> {
    // 先校验目标与凭据格式：配置错误时不发起任何出站连接。
    let connect_request = build_socks5_connect_request(host, port)?;
    drop(build_socks5_auth_request(&up.username, &up.password)?);

    let tcp = tokio::time::timeout(
        SOCKS5_TIMEOUT,
        TcpStream::connect((up.host.as_str(), up.port)),
    )
    .await
    .map_err(|_| anyhow!("安全上游连接超时"))?
    .context("安全上游连接失败")?;
    // TCP 特有选项在包 TLS 前设置（设计 §7）
    let _ = tcp.set_nodelay(true);

    let handshake = async {
        let mut tls = wrap_tls(tcp, &up.tls).await?;
        socks5_auth_handshake(&mut tls, &up.username, &up.password, &connect_request).await?;
        anyhow::Ok(tls)
    };
    let tls = tokio::time::timeout(SOCKS5_TIMEOUT, handshake)
        .await
        .map_err(|_| anyhow!("安全上游握手超时（TLS/认证/CONNECT）"))??;
    Ok(Box::new(tls))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_ca() {
        let cfg = TlsUpstreamConfig {
            server_name: "localhost".into(),
            ca_pem: b"not a pem".to_vec(),
            client_cert_pem: b"nope".to_vec(),
            client_key_pem: Secret::new("nope"),
        };
        assert!(cfg.build_connector().is_err());
        assert!(cfg.server_name().is_ok());
    }

    #[test]
    fn rejects_invalid_server_name() {
        let cfg = TlsUpstreamConfig {
            server_name: "".into(),
            ca_pem: Vec::new(),
            client_cert_pem: Vec::new(),
            client_key_pem: Secret::new(""),
        };
        assert!(cfg.server_name().is_err());
    }
}
