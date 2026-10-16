//! TLS/mTLS 上游通道集成测试（设计 §7、§10）。
//!
//! 用 rcgen 生成测试 CA / 服务端证书（SAN `localhost`）/ 客户端证书，
//! 起 rustls 假服务端验证：正确配置握手成功、信任锚不含签发 CA 时失败、
//! SAN 不匹配失败、无客户端证书被要求 mTLS 的服务端拒绝。
//! 只连本机回环，不访问外网；全部证书仅测试夹具。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use helper_core::credential::Secret;
use helper_core::proxy::ProxyConfig;
use helper_core::proxy::build_socks5_connect_request;
use helper_core::proxy::spawn_with_listener;
use helper_core::upstream::SecureUpstream;
use helper_core::upstream::TlsUpstreamConfig;
use helper_core::upstream::connect_secure;
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

/// mTLS rustls 服务端接受器：服务端证书由 `fixture` 签发，要求并验证由同一 CA 签发的客户端证书。
fn mtls_acceptor(fixture: &TestCa) -> std::io::Result<tokio_rustls::TlsAcceptor> {
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
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(server_config)))
}

/// 起 mTLS rustls 服务端（要求并验证客户端证书），读到 1 字节后回显 1 字节。
/// 返回监听地址。
async fn spawn_mtls_echo_server(fixture: &TestCa) -> std::io::Result<std::net::SocketAddr> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let acceptor = mtls_acceptor(fixture)?;
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

/// 设计 §4.2：首版禁用客户端会话恢复（不作为吊销旁路）。
/// 复用同一个 connector 连续两次握手，第二次必须仍是完整握手（Full），不得 Resumed；
/// 第一次握手后读回显，确保服务端下发的 TLS1.3 会话票据已被客户端接收。
#[tokio::test]
async fn session_resumption_is_disabled() {
    let fixture = test_ca();
    let addr = spawn_mtls_echo_server(&fixture).await.expect("server");
    let cfg = fixture.client_config("localhost");
    let connector = cfg.build_connector().expect("connector");
    for round in 0..2 {
        let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
        let name = ServerName::try_from("localhost").expect("name");
        let mut tls = connector.connect(name, tcp).await.expect("handshake");
        tls.write_all(b"x").await.expect("write");
        let mut buf = [0u8; 1];
        tls.read_exact(&mut buf).await.expect("echo");
        assert_eq!(
            tls.get_ref().1.handshake_kind(),
            Some(rustls::HandshakeKind::Full),
            "第 {round} 次握手必须是完整握手，会话恢复必须禁用"
        );
    }
}

// ---- Task 4：TCP → TLS(mTLS) → SOCKS5 用户名密码 → CONNECT 组合上游 ----

const TEST_USER: &str = "device-test";
const TEST_PASS: &str = "secret-pass-for-test";

/// 假安全上游（TLS 之内的 SOCKS5）的行为。
#[derive(Clone, Copy)]
enum Behavior {
    /// 正常：要求 05 01 02 问候，选 0x02，校验凭据，CONNECT 成功后回显。
    Normal,
    /// 降级攻击：方法应答选 0x00（无认证），之后只统计客户端还发来多少字节。
    OfferNoAuth,
    /// TLS 握手完成后一言不发，用于验证认证/CONNECT 阶段被超时覆盖。
    StallAfterTls,
}

/// 假安全上游的观测记录。
#[derive(Default)]
struct Observed {
    /// TCP 接入次数：断言失败后没有重试、没有明文回退。
    accepts: AtomicUsize,
    /// 服务端 TLS 握手成功次数。
    tls_ok: AtomicUsize,
    /// TLS 之内收到的全部应用层字节数。
    app_bytes: AtomicUsize,
    /// 收到 RFC1929 认证请求的次数。
    auth_requests: AtomicUsize,
    /// 收到的完整 CONNECT 请求原始字节。
    connects: Mutex<Vec<Vec<u8>>>,
    /// 降级应答（05 00）之后客户端发来的字节数；连接关闭后才写入。
    after_downgrade: Mutex<Option<usize>>,
}

impl Observed {
    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
    fn tls_ok(&self) -> usize {
        self.tls_ok.load(Ordering::SeqCst)
    }
    fn app_bytes(&self) -> usize {
        self.app_bytes.load(Ordering::SeqCst)
    }
    fn auth_requests(&self) -> usize {
        self.auth_requests.load(Ordering::SeqCst)
    }
    fn connects(&self) -> Vec<Vec<u8>> {
        self.connects.lock().expect("lock").clone()
    }
}

type ServerTls = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;

/// 从 TLS 流读取恰好 `n` 字节并计入应用层字节数；失败返回 `None`。
async fn read_counted(tls: &mut ServerTls, observed: &Observed, n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; n];
    tls.read_exact(&mut buf).await.ok()?;
    observed.app_bytes.fetch_add(n, Ordering::SeqCst);
    Some(buf)
}

/// 读到 EOF/错误为止，返回读到的字节数（计入应用层字节数）。
async fn drain_counted(tls: &mut ServerTls, observed: &Observed) -> usize {
    let mut total = 0usize;
    let mut buf = vec![0u8; 1024];
    while let Ok(read) = tls.read(&mut buf).await {
        if read == 0 {
            break;
        }
        total += read;
    }
    observed.app_bytes.fetch_add(total, Ordering::SeqCst);
    total
}

/// 起“TLS 之内是要求认证的 SOCKS5”的假安全上游。服务端证书由 `server_fixture` 签发，
/// 只接受同一 CA 签发的客户端证书。返回 (监听地址, 观测记录)。
async fn spawn_secure_socks5(
    server_fixture: &TestCa,
    behavior: Behavior,
) -> (std::net::SocketAddr, Arc<Observed>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind secure socks5");
    let addr = listener.local_addr().expect("addr");
    let acceptor = mtls_acceptor(server_fixture).expect("acceptor");
    let observed = Arc::new(Observed::default());
    let shared = Arc::clone(&observed);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let observed = Arc::clone(&shared);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                observed.tls_ok.fetch_add(1, Ordering::SeqCst);
                serve_socks5(&mut tls, &observed, behavior).await;
            });
        }
    });
    (addr, observed)
}

/// TLS 之内的 SOCKS5 服务端逻辑。
async fn serve_socks5(tls: &mut ServerTls, observed: &Observed, behavior: Behavior) {
    if let Behavior::StallAfterTls = behavior {
        // 不回任何应答，只把客户端发来的字节计数，直到对方关闭
        drain_counted(tls, observed).await;
        return;
    }
    let Some(greeting) = read_counted(tls, observed, 3).await else {
        return;
    };
    if let Behavior::OfferNoAuth = behavior {
        // 恶意/配置错误的上游：选择无认证；随后统计客户端还会发来多少字节
        if tls.write_all(&[0x05, 0x00]).await.is_err() || tls.flush().await.is_err() {
            return;
        }
        let after = drain_counted(tls, observed).await;
        *observed.after_downgrade.lock().expect("lock") = Some(after);
        return;
    }
    // 认证问候必须声明且仅声明 0x02
    if greeting != [0x05, 0x01, 0x02] || tls.write_all(&[0x05, 0x02]).await.is_err() {
        return;
    }
    let _ = tls.flush().await;
    // RFC1929：01 ULEN USER PLEN PASS
    let Some(head) = read_counted(tls, observed, 2).await else {
        return;
    };
    let Some(user) = read_counted(tls, observed, usize::from(head[1])).await else {
        return;
    };
    let Some(plen) = read_counted(tls, observed, 1).await else {
        return;
    };
    let Some(pass) = read_counted(tls, observed, usize::from(plen[0])).await else {
        return;
    };
    observed.auth_requests.fetch_add(1, Ordering::SeqCst);
    let ok = head[0] == 0x01 && user == TEST_USER.as_bytes() && pass == TEST_PASS.as_bytes();
    if !ok {
        // 错误凭据：回失败状态后断开
        let _ = tls.write_all(&[0x01, 0x01]).await;
        let _ = tls.flush().await;
        return;
    }
    if tls.write_all(&[0x01, 0x00]).await.is_err() || tls.flush().await.is_err() {
        return;
    }
    // CONNECT：05 CMD 00 03 LEN HOST PORT
    let Some(mut request) = read_counted(tls, observed, 5).await else {
        return;
    };
    let Some(rest) = read_counted(tls, observed, usize::from(request[4]) + 2).await else {
        return;
    };
    request.extend_from_slice(&rest);
    let is_connect = request[1] == 0x01;
    observed.connects.lock().expect("lock").push(request);
    // 只支持 CONNECT；其他命令回 0x07（不支持的命令）
    let reply: &[u8] = if is_connect {
        &[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 80]
    } else {
        &[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
    };
    if tls.write_all(reply).await.is_err() || tls.flush().await.is_err() || !is_connect {
        return;
    }
    // 隧道：回显
    let mut buffer = vec![0u8; 4096];
    while let Ok(read) = tls.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        if tls.write_all(&buffer[..read]).await.is_err() || tls.flush().await.is_err() {
            break;
        }
    }
}

/// 指向假安全上游的客户端配置：信任 `fixture` 的 CA 并提交其客户端证书。
fn secure_upstream(
    fixture: &TestCa,
    addr: std::net::SocketAddr,
    server_name: &str,
    password: &str,
) -> SecureUpstream {
    SecureUpstream {
        host: addr.ip().to_string(),
        port: addr.port(),
        tls: fixture.client_config(server_name),
        username: TEST_USER.to_string(),
        password: Secret::new(password),
    }
}

/// 等到 `check` 成立（服务端任务异步记录），最多 5 秒。
async fn eventually(check: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}

#[test]
fn secure_socks5_port_is_7892() {
    assert_eq!(helper_core::consts::SOCKS5_AUTH_PORT, 7892);
}

#[tokio::test]
async fn secure_roundtrip_succeeds() {
    let fixture = test_ca();
    let (addr, observed) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let up = secure_upstream(&fixture, addr, "localhost", TEST_PASS);
    let mut tunnel = connect_secure(&up, "chatgpt.com", 443)
        .await
        .expect("secure connect");
    tunnel.write_all(b"ping").await.expect("write");
    tunnel.flush().await.expect("flush");
    let mut echo = [0u8; 4];
    tunnel.read_exact(&mut echo).await.expect("echo");
    assert_eq!(&echo, b"ping");
    assert_eq!(
        observed.connects(),
        vec![build_socks5_connect_request("chatgpt.com", 443).expect("build")]
    );
    assert_eq!(observed.accepts(), 1);
    assert_eq!(observed.auth_requests(), 1);
}

#[tokio::test]
async fn wrong_password_fails_closed() {
    let fixture = test_ca();
    let (addr, observed) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let up = secure_upstream(&fixture, addr, "localhost", "wrong-pass-zzz");
    let error = match connect_secure(&up, "chatgpt.com", 443).await {
        Ok(_) => panic!("错误密码必须失败"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("认证失败"), "{error}");
    assert!(
        !error.contains("wrong-pass-zzz"),
        "错误信息不得含密码：{error}"
    );
    assert!(eventually(|| observed.auth_requests() == 1).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // 未发 CONNECT，且没有重试（只有一次 TCP 接入）
    assert!(observed.connects().is_empty());
    assert_eq!(observed.accepts(), 1);
}

/// 服务端在 TLS 内选 0x00（无认证）：客户端必须报降级错误，且此后零发送
/// （不发认证请求、不发 CONNECT），也不重连。
#[tokio::test]
async fn server_offering_no_auth_must_fail() {
    let fixture = test_ca();
    let (addr, observed) = spawn_secure_socks5(&fixture, Behavior::OfferNoAuth).await;
    let up = secure_upstream(&fixture, addr, "localhost", TEST_PASS);
    let error = match connect_secure(&up, "chatgpt.com", 443).await {
        Ok(_) => panic!("降级必须失败"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("拒绝认证降级"), "{error}");
    // 客户端失败后关闭连接，服务端统计降级应答之后收到的字节数
    assert!(
        eventually(|| observed.after_downgrade.lock().expect("lock").is_some()).await,
        "服务端应观察到连接关闭"
    );
    assert_eq!(
        *observed.after_downgrade.lock().expect("lock"),
        Some(0),
        "降级应答后客户端不得再发送任何字节"
    );
    assert_eq!(observed.auth_requests(), 0);
    assert!(observed.connects().is_empty());
    assert_eq!(observed.accepts(), 1);
}

/// 证书错误（服务端证书不可信 / ServerName 不匹配）：TLS 失败发生在任何 SOCKS5 字节之前，
/// 服务端收不到问候、认证请求与 CONNECT；也不改用明文重连。
#[tokio::test]
async fn bad_cert_fails_before_password_sent() {
    let fixture = test_ca();
    let other = test_ca();
    // 情形 1：服务端证书由另一 CA 签发（客户端只信任 fixture 的 CA）
    let (untrusted_addr, untrusted) = spawn_secure_socks5(&other, Behavior::Normal).await;
    // 情形 2：证书可信但 ServerName 不匹配（SAN 为 localhost，校验名为 evil）
    let (mismatch_addr, mismatch) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let cases = [
        (
            secure_upstream(&fixture, untrusted_addr, "localhost", TEST_PASS),
            untrusted,
        ),
        (
            secure_upstream(&fixture, mismatch_addr, "evil", TEST_PASS),
            mismatch,
        ),
    ];
    for (up, observed) in cases {
        let error = match connect_secure(&up, "chatgpt.com", 443).await {
            Ok(_) => panic!("证书错误必须失败"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("TLS 握手失败"), "{error}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(observed.accepts(), 1, "不得重试或回退明文");
        assert_eq!(observed.tls_ok(), 0);
        assert_eq!(observed.app_bytes(), 0, "认证请求不得发出");
        assert_eq!(observed.auth_requests(), 0);
        assert!(observed.connects().is_empty());
    }
}

/// 客户端只有 CONNECT 一条命令路径（设计 §4.2：不提供 BIND/UDP ASSOCIATE）：
/// 唯一的请求构造函数写死 CMD=0x01、ATYP=0x03，经安全上游实际发出的请求同样如此。
#[tokio::test]
async fn udp_associate_not_offered() {
    for (host, port) in [("chatgpt.com", 443), ("a", 1), ("10.0.0.1", 65535)] {
        let request = build_socks5_connect_request(host, port).expect("build");
        assert_eq!(request[..4], [0x05, 0x01, 0x00, 0x03], "{host}");
    }
    let fixture = test_ca();
    let (addr, observed) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let up = secure_upstream(&fixture, addr, "localhost", TEST_PASS);
    let _tunnel = connect_secure(&up, "api.openai.com", 443)
        .await
        .expect("secure connect");
    let connects = observed.connects();
    assert_eq!(connects.len(), 1);
    assert_eq!(
        connects[0][1], 0x01,
        "CMD 必须是 CONNECT，不得是 BIND(0x02)/UDP ASSOCIATE(0x03)"
    );
}

/// TCP 已连上但 TLS 握手不推进、或 TLS 后 SOCKS5 不应答：都必须在超时内失败（不无限挂起）。
#[tokio::test]
async fn stalled_upstream_times_out() {
    let fixture = test_ca();
    // TLS 握手阶段卡住：只接受 TCP，从不说话
    let silent = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind silent");
    let silent_addr = silent.local_addr().expect("addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    // 认证/CONNECT 阶段卡住：TLS 成功后不回方法应答
    let (stall_addr, _observed) = spawn_secure_socks5(&fixture, Behavior::StallAfterTls).await;

    let tls_stall = secure_upstream(&fixture, silent_addr, "localhost", TEST_PASS);
    let socks_stall = secure_upstream(&fixture, stall_addr, "localhost", TEST_PASS);
    let started = Instant::now();
    let (first, second) = tokio::join!(
        connect_secure(&tls_stall, "chatgpt.com", 443),
        connect_secure(&socks_stall, "chatgpt.com", 443),
    );
    let elapsed = started.elapsed();
    for result in [first, second] {
        let error = match result {
            Ok(_) => panic!("卡住的上游必须超时失败"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("超时"), "{error}");
    }
    assert!(
        elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_secs(6),
        "{elapsed:?}"
    );
}

/// 旧入口诱饵：只统计 TCP 接入次数，用来断言配置了安全上游后旧路径（7891 无认证）零连接。
async fn spawn_legacy_decoy() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind decoy");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((_stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    (addr, hits)
}

/// 起本地分流代理：旧上游指向诱饵，安全上游为 `secure`。
fn spawn_proxy(
    listener: tokio::net::TcpListener,
    decoy: std::net::SocketAddr,
    secure: SecureUpstream,
) -> helper_core::proxy::ProxyHandle {
    spawn_with_listener(
        listener,
        ProxyConfig {
            socks5_host: decoy.ip().to_string(),
            socks5_port: decoy.port(),
            secure_upstream: Some(Arc::new(secure)),
            ..ProxyConfig::default()
        },
    )
}

/// 向代理发 CONNECT，读回应答头（到空行为止）。
async fn proxy_connect(port: u16, target: &str) -> (tokio::net::TcpStream, String) {
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect proxy");
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    client.write_all(request.as_bytes()).await.expect("write");
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match client.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    (client, String::from_utf8_lossy(&head).into_owned())
}

/// `ProxyConfig.secure_upstream = Some` 时，命中规则的流量走安全上游，旧入口零连接。
#[tokio::test]
async fn proxy_socks5_route_uses_secure_upstream() {
    let fixture = test_ca();
    let (secure_addr, observed) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let (decoy, decoy_hits) = spawn_legacy_decoy().await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy");
    let proxy = spawn_proxy(
        listener,
        decoy,
        secure_upstream(&fixture, secure_addr, "localhost", TEST_PASS),
    );
    let (mut client, head) = proxy_connect(proxy.port(), "chatgpt.com:443").await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    client.write_all(b"ping").await.expect("write");
    let mut echo = [0u8; 4];
    client.read_exact(&mut echo).await.expect("echo");
    assert_eq!(&echo, b"ping");
    assert_eq!(
        observed.connects(),
        vec![build_socks5_connect_request("chatgpt.com", 443).expect("build")]
    );
    assert_eq!(decoy_hits.load(Ordering::SeqCst), 0, "不得走旧 7891 路径");
}

/// 安全上游失败（错误密码）：代理回 502，不回退旧入口、不重试。
#[tokio::test]
async fn proxy_secure_upstream_failure_returns_502_without_fallback() {
    let fixture = test_ca();
    let (secure_addr, observed) = spawn_secure_socks5(&fixture, Behavior::Normal).await;
    let (decoy, decoy_hits) = spawn_legacy_decoy().await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy");
    let proxy = spawn_proxy(
        listener,
        decoy,
        secure_upstream(&fixture, secure_addr, "localhost", "wrong-pass-zzz"),
    );
    let (_client, head) = proxy_connect(proxy.port(), "chatgpt.com:443").await;
    assert!(head.starts_with("HTTP/1.1 502"), "{head}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(decoy_hits.load(Ordering::SeqCst), 0, "不得回退旧 7891 路径");
    assert_eq!(observed.accepts(), 1, "不得重试");
    assert!(observed.connects().is_empty());
}

/// `ProxyConfig` / `SecureUpstream` 的 Debug 输出不含密码与私钥。
#[test]
fn secure_upstream_debug_is_redacted() {
    let fixture = test_ca();
    let addr: std::net::SocketAddr = "127.0.0.1:7892".parse().expect("addr");
    let config = ProxyConfig {
        secure_upstream: Some(Arc::new(secure_upstream(
            &fixture,
            addr,
            "localhost",
            TEST_PASS,
        ))),
        ..ProxyConfig::default()
    };
    let debug = format!("{config:?}");
    assert!(debug.contains("secure_upstream"), "{debug}");
    assert!(debug.contains("7892"), "{debug}");
    assert!(!debug.contains(TEST_PASS), "{debug}");
    assert!(!debug.contains("PRIVATE KEY"), "{debug}");
    let key_body = fixture.client_key_pem.lines().nth(1).expect("key line");
    assert!(!debug.contains(key_body), "{debug}");
    assert!(ProxyConfig::default().secure_upstream.is_none());
}
