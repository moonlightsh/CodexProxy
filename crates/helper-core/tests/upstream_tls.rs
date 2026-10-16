//! TLS/mTLS 上游通道集成测试（设计 §7、§10）。
//!
//! 用 rcgen 生成测试 CA / 服务端证书（SAN `localhost`）/ 客户端证书，
//! 起 rustls 假服务端验证：正确配置握手成功、信任锚不含签发 CA 时失败、
//! SAN 不匹配失败、无客户端证书被要求 mTLS 的服务端拒绝。
//! 只连本机回环，不访问外网；全部证书仅测试夹具。

use std::sync::Arc;

use helper_core::credential::Secret;
use helper_core::upstream::TlsUpstreamConfig;
use helper_core::upstream::wrap_tls;
use rcgen::CertificateParams;
use rcgen::DnType;
use rcgen::ExtendedKeyUsagePurpose;
use rcgen::IsCa;
use rcgen::Issuer;
use rcgen::KeyPair;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::ServerName;
use rustls_pki_types::pem::PemObject as _;
use tokio::io::AsyncReadExt as _;
use tokio::io::AsyncWriteExt as _;

/// 测试夹具：一套 CA + 服务端证书（SAN localhost）+ 客户端证书（clientAuth EKU），全部 PEM。
struct TestCa {
    ca_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

fn test_ca() -> TestCa {
    // CA
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "codex-helper-test-ca");
    let ca_key = KeyPair::generate().expect("ca key");
    let _ca = ca_params.self_signed(&ca_key).expect("ca cert");

    // 服务端证书：SAN localhost
    let server_params = CertificateParams::new(vec!["localhost".to_string()]).expect("srv params");
    let server_key = KeyPair::generate().expect("srv key");
    let server_cert = server_params
        .signed_by(&server_key, &Issuer::from_params(&ca_params, &ca_key))
        .expect("srv cert");

    // 客户端证书：clientAuth EKU
    let mut client_params = CertificateParams::new(Vec::<String>::new()).expect("cli params");
    client_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    let client_key = KeyPair::generate().expect("cli key");
    let client_cert = client_params
        .signed_by(&client_key, &Issuer::from_params(&ca_params, &ca_key))
        .expect("cli cert");

    TestCa {
        ca_pem: _ca.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

impl TestCa {
    fn client_config(&self, server_name: &str) -> TlsUpstreamConfig {
        TlsUpstreamConfig {
            server_name: server_name.to_string(),
            ca_pem: self.ca_pem.as_bytes().to_vec(),
            client_cert_pem: self.client_cert_pem.as_bytes().to_vec(),
            client_key_pem: Secret::new(self.client_key_pem.clone()),
        }
    }
}

/// 起 mTLS rustls 服务端（要求并验证客户端证书），读到 1 字节后回显 1 字节。
/// 返回监听地址。
async fn spawn_mtls_echo_server(fixture: &TestCa) -> std::io::Result<std::net::SocketAddr> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let parse = |pem: &str| -> std::io::Result<_> {
        CertificateDer::from_pem_slice(pem.as_bytes())
            .map_err(|e| std::io::Error::other(e.to_string()))
    };
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(parse(&fixture.ca_pem)?)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions: &[&rustls::SupportedProtocolVersion] =
        &[&rustls::version::TLS13, &rustls::version::TLS12];
    let verifier =
        rustls::server::WebPkiClientVerifier::builder_with_provider(roots.into(), provider.clone())
            .build()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![parse(&fixture.server_cert_pem)?],
            PrivateKeyDer::from_pem_slice(fixture.server_key_pem.as_bytes())
                .map_err(|e| std::io::Error::other(e.to_string()))?,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buf = [0u8; 1];
                if tls.read_exact(&mut buf).await.is_ok() {
                    let _ = tls.write_all(&buf).await;
                }
            });
        }
    });
    Ok(addr)
}

#[tokio::test]
async fn correct_config_handshake_succeeds_and_echoes() {
    let fixture = test_ca();
    let addr = spawn_mtls_echo_server(&fixture).await.expect("server");
    let cfg = fixture.client_config("localhost");
    let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
    let mut tls = wrap_tls(tcp, &cfg).await.expect("handshake");
    tls.write_all(b"x").await.expect("write");
    let mut buf = [0u8; 1];
    tls.read_exact(&mut buf).await.expect("echo");
    assert_eq!(&buf, b"x");
}

#[tokio::test]
async fn untrusted_ca_fails_handshake() {
    let fixture = test_ca();
    let other = test_ca(); // 另一套 CA：服务端用它签发
    let addr = spawn_mtls_echo_server(&other).await.expect("server");
    // 客户端信任 fixture 的 CA，服务端证书由 other 签发 → 不可信
    let cfg = fixture.client_config("localhost");
    let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
    let error = wrap_tls(tcp, &cfg).await.expect_err("must fail");
    assert!(error.to_string().contains("TLS 握手失败"), "{error}");
}

#[tokio::test]
async fn wrong_server_name_fails_handshake() {
    let fixture = test_ca();
    let addr = spawn_mtls_echo_server(&fixture).await.expect("server");
    // 证书 SAN 是 localhost，用 "evil" 校验 → 必须失败
    let cfg = fixture.client_config("evil");
    let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
    let error = wrap_tls(tcp, &cfg).await.expect_err("must fail");
    assert!(error.to_string().contains("TLS 握手失败"), "{error}");
}

/// 无客户端证书的连接必须被要求 mTLS 的服务端拒绝（fail-closed 的服务端侧证据）。
/// TLS1.3 下客户端握手在收到服务端 Finished 时即完成，服务端对空证书的拒绝
/// 以 alert 在首次读/写时到达：断言应用数据永远无法往返。
#[tokio::test]
async fn missing_client_cert_is_rejected_by_mtls_server() {
    let fixture = test_ca();
    let addr = spawn_mtls_echo_server(&fixture).await.expect("server");
    // 不带客户端证书、只信任该 CA 的普通客户端
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(fixture.ca_pem.as_bytes()).expect("ca"))
        .expect("root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions: &[&rustls::SupportedProtocolVersion] =
        &[&rustls::version::TLS13, &rustls::version::TLS12];
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
    let name = ServerName::try_from("localhost").expect("name");
    let mut tls = connector.connect(name, tcp).await.expect("connect");
    // 服务端拒绝空证书：写入或读取必然失败，不可能收到回显
    tls.write_all(b"x").await.expect("write");
    let mut buf = [0u8; 1];
    let result = tls.read_exact(&mut buf).await;
    assert!(result.is_err(), "无客户端证书必须被 mTLS 服务端拒绝");
}
