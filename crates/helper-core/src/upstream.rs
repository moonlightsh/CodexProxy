//! 上游 TLS/mTLS 通道（设计 §7）。
//!
//! `TlsUpstreamConfig` 携带注册包下发的信任锚（CA PEM）、客户端证书与私钥，
//! `build_connector` 产出仅信任该 CA、提交客户端证书、最低 TLS1.2 / 优先 TLS1.3 的连接器。
//! 任何 TLS 失败（证书不可信、SAN 不匹配、私钥不匹配）都是终止性错误：调用方不得回退明文。

use std::sync::Arc;

use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

use crate::credential::Secret;

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
        let ca_count = load_certs(&self.ca_pem, "服务端 CA")?.len();
        if ca_count == 0 {
            anyhow::bail!("服务端 CA 为空：拒绝建立无信任锚的 TLS 连接");
        }
        for cert in load_certs(&self.ca_pem, "服务端 CA")? {
            roots
                .add(cert)
                .map_err(|_| anyhow::anyhow!("服务端 CA 证书无法加入信任锚"))?;
        }

        let client_certs = load_certs(&self.client_cert_pem, "客户端证书")?;
        if client_certs.is_empty() {
            anyhow::bail!("客户端证书为空");
        }
        let client_key = load_key(&self.client_key_pem)?;

        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(TLS_VERSIONS)
            .map_err(|_| anyhow::anyhow!("TLS 协议版本配置不受支持"))?
            .with_root_certificates(roots)
            .with_client_auth_cert(client_certs, client_key)
            .map_err(|_| anyhow::anyhow!("客户端证书或私钥无法用于 mTLS"))?;

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
