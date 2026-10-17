//! 注册包解析与代理秘密存储（设计 §5.1、§5.2）。
//!
//! 职责边界：
//! - `parse_enrollment` 只做注册包（管理员签发的 JSON）的字段与形态校验：device_id 非空、
//!   端口 > 0、CA / 客户端证书 PEM 块存在、有效期与策略字段形态合法。**不做**管理签名
//!   验证（`process_policy::parse_and_verify`）、TLS 可用性检查（`upstream`）——调用方
//!   （Manager 的激活流程，Task 9）按序组合。
//! - `save_secrets` / `load_secrets` / `delete_secrets` 把 SOCKS5 密码与客户端软件私钥
//!   写入系统凭据存储（Windows Credential Manager / macOS 登录钥匙串 / 其他平台进程内
//!   开发表），target 为 `codex-helper/proxy-password/<device-id>` 与
//!   `codex-helper/proxy-key/<device-id>`，绝不复用现有 `codex-helper/managed-gateway` 条目。
//!
//! 秘密纪律：密码与私钥全程 [`Secret`] 包装，错误信息与 `Debug` 输出绝不携带内容。
//!
//! 私钥紧凑编码（设计 §5.2"体积可控"）：P-256 私钥 PEM（PKCS#8 约 227 字节 / SEC1 约 227
//! 字节）会撞上 macOS `security -i` 的 2048 字节单行命令长度上限（含 target、account 与
//! 转义开销后余量不足）。因此**存储层**使用紧凑格式：
//! `v1:<kind>:<base64(DER)>`，`kind` 为 `pkcs8` / `sec1` / `pkcs1`；**读取层**还原为标准
//! PEM（`-----BEGIN PRIVATE KEY-----` 等段落）再交给 TLS 库（`upstream::load_key` 的
//! 既有路径，不需要任何解析改动）。存储内容仍是可打印 ASCII、单行、无空白，通过
//! `security -i` 的注入防线校验。
//!
//! fail-closed：解析或存储任何失败都向上传播错误，调用方必须拒绝激活，不回退、不截断、
//! 不部分写入成功就当作整体成功（密码写成功而私钥写失败时，`save_secrets` 会尽力回滚
//! 已写入的密码条目再返回错误）。

use anyhow::{Context, anyhow, bail};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, pem::PemObject};

use crate::consts;
use crate::credential::{CredentialStore, Secret, SystemCredentialStore};

/// 从 [`consts::POLICY_VERIFY_KEY_HEX`] 构造管理签名验证公钥。
/// 常量必须是 64 字节小写 hex 且能构造 ed25519 验证钥；否则返回错误（发布前防误填，
/// 有单测钉住；运行时这里再检查一次并 fail-closed）。
pub fn policy_verify_key() -> anyhow::Result<VerifyingKey> {
    let text = consts::POLICY_VERIFY_KEY_HEX;
    let mut bytes = [0u8; 32];
    let raw = text.as_bytes();
    if raw.len() != 64 {
        bail!("管理签名公钥常量长度非法");
    }
    for i in 0..32 {
        let hi = (raw[i * 2] as char)
            .to_digit(16)
            .context("管理签名公钥含非 hex 字符")? as u8;
        let lo = (raw[i * 2 + 1] as char)
            .to_digit(16)
            .context("管理签名公钥含非 hex 字符")? as u8;
        bytes[i] = hi << 4 | lo;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| anyhow!("管理签名公钥不是有效的 ed25519 验证钥"))
}

/// 注册包 JSON 的原始形态（字段名与签发端一致）。
#[derive(Debug, Deserialize)]
struct EnrollmentRaw {
    device_id: String,
    server_host: String,
    server_port: u16,
    server_name: String,
    ca_pem: String,
    client_cert_pem: String,
    policy_bundle_json: String,
    policy_signature: String,
    not_before_unix: u64,
    not_after_unix: u64,
}

/// 管理员签发的注册包（设计 §5.1 第 4 步）：非秘密部分。
///
/// 密码与客户端私钥**不在**注册包里，经独立受控渠道交付后由
/// [`save_secrets`] 写入系统存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentBundle {
    pub device_id: String,
    pub server_host: String,
    pub server_port: u16,
    pub server_name: String,
    pub ca_pem: String,
    pub client_cert_pem: String,
    pub policy_bundle_json: String,
    pub policy_signature: String,
    pub not_before_unix: u64,
    pub not_after_unix: u64,
}

/// SOCKS5 用户名密码 + 客户端软件私钥（设计 §5.2）。
///
/// `password` / `client_key_pem` 全程 [`Secret`]：`Debug` 不输出内容，错误不携带内容。
/// `client_key_pem` 命名沿用接口约定，实际承载标准 PEM 文本（紧凑编码只存在于存储层，
/// 见模块文档）。
pub struct ProxySecrets {
    pub username: String,
    pub password: Secret,
    pub client_key_pem: Secret,
}

impl std::fmt::Debug for ProxySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只输出用户名（非秘密）；密码与私钥一律脱敏。
        f.debug_struct("ProxySecrets")
            .field("username", &self.username)
            .field("password", &"Secret(***)")
            .field("client_key_pem", &"Secret(***)")
            .finish()
    }
}

/// 校验字符串恰好包含一个 PEM `CERTIFICATE` 块（`-----BEGIN CERTIFICATE-----` 起止）。
/// 只查形态，不解析证书内容（链路校验在 upstream / 激活流程）。
fn require_cert_pem(pem: &str, what: &str) -> anyhow::Result<()> {
    if pem.contains("-----BEGIN CERTIFICATE-----") && pem.contains("-----END CERTIFICATE-----") {
        return Ok(());
    }
    bail!("注册包的{what}缺少 CERTIFICATE PEM 块");
}

/// 管理签名的形态校验：64 字节小写或大写 hex（128 个 hex 字符）。
/// 允许大写输入，验证方 `Signature::from_slice` 前由调用方统一小写；
/// 非 hex 或长度不对直接拒绝——fail-closed，不猜测修复。
fn require_signature_hex(sig: &str) -> anyhow::Result<()> {
    if sig.len() == 128 && sig.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(());
    }
    bail!("注册包的 policy_signature 必须是 128 位 hex（64 字节 ed25519 签名）");
}

/// 解析注册包 JSON。字段校验（全部 fail-closed）：
/// - `device_id` 非空、且为可打印 ASCII（它参与钥匙串 target 拼接，必须能安全拼入）；
/// - `server_port` > 0（serde 已保证 u16，0 视为非法）；
/// - `ca_pem` / `client_cert_pem` 各含 CERTIFICATE PEM 块；
/// - `policy_signature` 为 128 位 hex；
/// - `not_before_unix <= not_after_unix`。
///
/// 不校验：管理签名（Task 9 组合 `process_policy::parse_and_verify`）、证书链与 EKU
/// （`upstream` 连接时校验）、有效期是否覆盖当前时刻（激活时机由 Manager 决定）。
pub fn parse_enrollment(json: &[u8]) -> anyhow::Result<EnrollmentBundle> {
    let raw: EnrollmentRaw = serde_json::from_slice(json).context("注册包 JSON 损坏或字段缺失")?;

    if raw.device_id.is_empty() {
        bail!("注册包的 device_id 不能为空");
    }
    if !raw.device_id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        bail!("注册包的 device_id 只能包含可打印 ASCII 字符（不含空格）");
    }
    if raw.server_port == 0 {
        bail!("注册包的 server_port 必须大于 0");
    }
    if raw.server_host.trim().is_empty() {
        bail!("注册包的 server_host 不能为空");
    }
    if raw.server_name.trim().is_empty() {
        bail!("注册包的 server_name 不能为空");
    }
    require_cert_pem(&raw.ca_pem, "ca_pem（服务端信任锚）")?;
    require_cert_pem(&raw.client_cert_pem, "client_cert_pem（客户端证书）")?;
    if raw.policy_bundle_json.trim().is_empty() {
        bail!("注册包的 policy_bundle_json 不能为空");
    }
    require_signature_hex(&raw.policy_signature)?;
    if raw.not_before_unix > raw.not_after_unix {
        bail!("注册包的有效期非法：not_before_unix > not_after_unix");
    }

    Ok(EnrollmentBundle {
        device_id: raw.device_id,
        server_host: raw.server_host,
        server_port: raw.server_port,
        server_name: raw.server_name,
        ca_pem: raw.ca_pem,
        client_cert_pem: raw.client_cert_pem,
        policy_bundle_json: raw.policy_bundle_json,
        policy_signature: raw.policy_signature,
        not_before_unix: raw.not_before_unix,
        not_after_unix: raw.not_after_unix,
    })
}

/// 紧凑私钥编码的版本前缀。
const COMPACT_KEY_PREFIX: &str = "v1:";

/// 私钥 DER 的 base64 编码表（标准字母表，含填充 `=`；填充符在可打印 ASCII 范围内）。
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64[(n >> 18) as usize & 0x3f] as char);
        out.push(BASE64[(n >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            BASE64[(n >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64[n as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// PEM 私钥 → 紧凑存储格式 `v1:<kind>:<base64(DER)>`。
/// `kind` 记录 PEM 段落标签，读取时原样还原，不做跨格式转换。
fn pem_to_compact(pem: &Secret) -> anyhow::Result<String> {
    let key = PrivateKeyDer::from_pem_slice(pem.expose().as_bytes())
        .map_err(|_| anyhow!("客户端私钥 PEM 解析失败（不是有效的 PKCS#8/SEC1/PKCS#1 私钥）"))?;
    let (kind, der) = match &key {
        PrivateKeyDer::Pkcs8(k) => ("pkcs8", k.secret_pkcs8_der()),
        PrivateKeyDer::Sec1(k) => ("sec1", k.secret_sec1_der()),
        PrivateKeyDer::Pkcs1(k) => ("pkcs1", k.secret_pkcs1_der()),
        // PrivateKeyDer 标记为 non_exhaustive：未来新增格式时保守拒绝，不猜测编码。
        _ => bail!("不支持的私钥格式"),
    };
    Ok(format!("{COMPACT_KEY_PREFIX}{kind}:{}", base64_encode(der)))
}

/// base64 解码（标准字母表，要求规范填充；失败即拒绝，不猜测修复）。
fn base64_decode(text: &str) -> anyhow::Result<Vec<u8>> {
    let mut map = [255u8; 256];
    for (i, &b) in BASE64.iter().enumerate() {
        map[b as usize] = i as u8;
    }
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        bail!("紧凑私钥的 base64 长度非法");
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && i != bytes.len() / 4 - 1) {
            bail!("紧凑私钥的 base64 填充非法");
        }
        let mut vals = [0u8; 4];
        for (j, &b) in chunk.iter().enumerate() {
            let is_pad = b == b'=';
            let v = map[b as usize];
            // 非填充字符必须在字母表内；填充字符只能出现在末尾对应位置。
            if (!is_pad && v == 255) || (is_pad && j < 4 - pad) {
                bail!("紧凑私钥的 base64 字符非法");
            }
            vals[j] = if is_pad { 0 } else { v };
        }
        let n = (vals[0] as u32) << 18
            | (vals[1] as u32) << 12
            | (vals[2] as u32) << 6
            | vals[3] as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// 紧凑存储格式 → PEM 文本（`Secret` 包装）。输入不含秘密标签之外的内容时才可能成功。
fn compact_to_pem(compact: &str) -> anyhow::Result<Secret> {
    let rest = compact
        .strip_prefix(COMPACT_KEY_PREFIX)
        .ok_or_else(|| anyhow!("存储的私钥不是紧凑格式（缺 v1: 前缀）"))?;
    let (kind, b64) = rest
        .split_once(':')
        .ok_or_else(|| anyhow!("存储的私钥缺少 kind 字段"))?;
    let (label, der) = match kind {
        "pkcs8" => ("PRIVATE KEY", base64_decode(b64)?),
        "sec1" => ("EC PRIVATE KEY", base64_decode(b64)?),
        "pkcs1" => ("RSA PRIVATE KEY", base64_decode(b64)?),
        _ => bail!("存储的私钥 kind 未知：{kind}"),
    };
    // 浅校验：三种格式的 DER 都必须是 SEQUENCE（首字节 0x30）且不至于小到明显非密钥。
    // 深层语义校验（能否真的用于 TLS）在构建连接器时进行（upstream，激活时 fail-closed）；
    // 存储层把明显垃圾挡在读取阶段，避免坏数据无声流入。
    if der.first() != Some(&0x30) || der.len() < 16 {
        bail!("紧凑私钥解码后不是有效的私钥 DER");
    }
    // 还原为 rustls PEM 解析器接受的形态：标签行 + 64 字符折行的 base64 + 结束行。
    let encoded = base64_encode(&der);
    let mut pem = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 是 ASCII"));
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {label}-----\n"));
    Ok(Secret::new(pem))
}

/// 校验 device_id 可安全拼入 target（非空、可打印 ASCII、不含空格）。
/// 长度上，device-id 是管理员生成的短标识（如 `device-a1b2c3`），拼上前缀远低于
/// macOS 钥匙串 2048 字节命令行上限与 Windows target 上限；真实限制在写入层兜底。
fn validate_device_id(device_id: &str) -> anyhow::Result<()> {
    if device_id.is_empty() {
        bail!("device_id 不能为空");
    }
    if !device_id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        bail!("device_id 只能包含可打印 ASCII 字符（不含空格）");
    }
    Ok(())
}

/// SOCKS5 凭据校验（RFC1929 / 设计 §4.3）：用户名与密码各 1–255 字节。
/// 超限报错不截断；错误信息不含凭据内容。
fn validate_username(username: &str) -> anyhow::Result<()> {
    if username.is_empty() || username.len() > 255 {
        bail!("SOCKS5 用户名长度必须在 1–255 字节之间");
    }
    Ok(())
}

fn validate_password(password: &Secret) -> anyhow::Result<()> {
    if password.expose().is_empty() || password.expose().len() > 255 {
        bail!("SOCKS5 密码长度必须在 1–255 字节之间");
    }
    Ok(())
}

/// device_id → （密码 target，私钥 target）。
fn targets_for(device_id: &str) -> (String, String) {
    (
        format!("{}{}", consts::PROXY_PASSWORD_TARGET_PREFIX, device_id),
        format!("{}{}", consts::PROXY_KEY_TARGET_PREFIX, device_id),
    )
}

/// 把 SOCKS5 密码与客户端软件私钥写入系统凭据存储（设计 §5.2）。
///
/// - 密码原样存储（随机 32 字节 hex = 64 字符，可打印 ASCII）；
/// - 私钥转紧凑格式 `v1:<kind>:<base64(DER)>`，避开 `security -i` 单行 2048 字节上限；
/// - 沿用系统存储的字符/长度校验（可打印 ASCII、单条上限），超限报错**不截断**；
/// - 先写密码后写私钥：私钥写入失败时尽力回滚已写入的密码条目，保持"两条都在或都不在"。
///
/// 绝不触碰 `codex-helper/managed-gateway`（那是网关 API Key 的 target，读写协议不同）。
pub fn save_secrets(device_id: &str, secrets: &ProxySecrets) -> anyhow::Result<()> {
    validate_device_id(device_id)?;
    validate_username(&secrets.username)?;
    validate_password(&secrets.password)?;
    // 用户名与 device-id 的一致性（设计 §5.1：用户名固定为 device-<id>）：不一致即拒绝，
    // 避免"存了密码、连接时却用派生用户名"的错位配对。
    let expected_username = format!("device-{device_id}");
    if secrets.username != expected_username {
        bail!("用户名必须为 {expected_username}（与 device-id 一致）");
    }

    let compact_key = pem_to_compact(&secrets.client_key_pem)?;
    let (password_target, key_target) = targets_for(device_id);

    let password_store = SystemCredentialStore::new(password_target);
    let key_store = SystemCredentialStore::new(key_target);

    password_store
        .write(secrets.password.expose())
        .context("写入 SOCKS5 密码到系统凭据存储失败")?;
    if let Err(err) = key_store.write(&compact_key) {
        // 回滚密码条目，避免出现"有密码无私钥"的半状态；回滚失败在错误链里注明。
        if let Err(rollback) = password_store.delete() {
            return Err(err).with_context(|| {
                format!("回滚密码条目也失败了：{rollback}（device_id={device_id}）")
            });
        }
        return Err(err).with_context(|| format!("写入客户端私钥失败（device_id={device_id}）"));
    }
    Ok(())
}

/// 读取某设备的 SOCKS5 密码与客户端私钥。
///
/// 任何一条缺失、格式非法、或用户名与 device_id 不一致 → 错误（fail-closed，
/// 不返回半份秘密、不用空值顶替）。用户名一致性：target 由 device_id 派生，读回的
/// 凭据必须属于同一设备，混用等于 A 设备的密码配 B 设备的证书。
pub fn load_secrets(device_id: &str) -> anyhow::Result<ProxySecrets> {
    validate_device_id(device_id)?;
    let (password_target, key_target) = targets_for(device_id);

    let password = SystemCredentialStore::new(&password_target)
        .read()
        .context("读取 SOCKS5 密码失败")?
        .ok_or_else(|| {
            anyhow!("未找到设备 {device_id} 的 SOCKS5 密码（target={password_target}）")
        })?;
    validate_password(&password)
        .with_context(|| format!("存储的 SOCKS5 密码长度非法（device_id={device_id}）"))?;

    let compact = SystemCredentialStore::new(&key_target)
        .read()
        .context("读取客户端私钥失败")?
        .ok_or_else(|| anyhow!("未找到设备 {device_id} 的客户端私钥（target={key_target}）"))?;
    let client_key_pem = compact_to_pem(compact.expose())
        .with_context(|| format!("存储的客户端私钥无法解码（device_id={device_id}）"))?;

    Ok(ProxySecrets {
        username: format!("device-{device_id}"),
        password,
        client_key_pem,
    })
}

/// 删除某设备的两条秘密条目；不存在视为成功（幂等，供注销/重置流程用）。
/// 返回类型为 `()`（简报接口）：删除是尽力而为的清理动作，失败不阻断流程，
/// 残留由幂等重删清理；需要严格确认删除结果的调用方可用 `load_secrets` 验证。
pub fn delete_secrets(device_id: &str) {
    // device_id 非法时拼出的 target 不会命中任何生产条目（前缀不同），删除无害。
    let _ = validate_device_id(device_id);
    let (password_target, key_target) = targets_for(device_id);
    let _ = SystemCredentialStore::new(password_target).delete();
    let _ = SystemCredentialStore::new(key_target).delete();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一对 P-256 私钥（PKCS#8 PEM）与自签证书（rcgen），仅测试夹具。
    fn test_key_and_cert() -> (String, String) {
        use rcgen::{CertificateParams, KeyPair};
        let key = KeyPair::generate().expect("生成测试密钥");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("证书参数");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "codex-helper-test-device");
        let cert = params.self_signed(&key).expect("自签测试证书");
        (key.serialize_pem(), cert.pem())
    }

    const SIG_HEX: &str = "c5a530958105b1c73d83184d5a5f4c2f7307f00fbe4521fb23b8ff19deda7db93a738799c99590d0267048880316317f40a9308fa4b90b06a5ef501f21c67b07";

    /// 合法注册包 JSON（ca_pem 用占位块——字段校验只查块存在；client_cert_pem 用真证书）。
    fn valid_enrollment_json() -> Vec<u8> {
        let (_, cert_pem) = test_key_and_cert();
        serde_json::json!({
            "device_id": "a1b2c3",
            "server_host": "10.20.30.61",
            "server_port": 7892,
            "server_name": "proxy.internal.example",
            "ca_pem": "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            "client_cert_pem": cert_pem,
            "policy_bundle_json": "{\"policy\":{}}",
            "policy_signature": SIG_HEX,
            "not_before_unix": 1,
            "not_after_unix": 4102444800u64,
        })
        .to_string()
        .into_bytes()
    }

    // ---------- parse_enrollment ----------

    #[test]
    fn parse_valid_enrollment() {
        let bundle = parse_enrollment(&valid_enrollment_json()).unwrap();
        assert_eq!(bundle.device_id, "a1b2c3");
        assert_eq!(bundle.server_host, "10.20.30.61");
        assert_eq!(bundle.server_port, 7892);
        assert_eq!(bundle.server_name, "proxy.internal.example");
        assert!(bundle.ca_pem.contains("BEGIN CERTIFICATE"));
        assert!(bundle.client_cert_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(bundle.policy_signature, SIG_HEX);
        assert_eq!(bundle.not_before_unix, 1);
        assert_eq!(bundle.not_after_unix, 4102444800);
    }

    #[test]
    fn parse_rejects_missing_fields() {
        let raw = valid_enrollment_json();
        for field in [
            "device_id",
            "server_host",
            "server_port",
            "server_name",
            "ca_pem",
            "client_cert_pem",
            "policy_bundle_json",
            "policy_signature",
            "not_before_unix",
            "not_after_unix",
        ] {
            let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            let mut obj = value.as_object().unwrap().clone();
            obj.remove(field);
            let bytes = serde_json::to_vec(&obj).unwrap();
            let err = parse_enrollment(&bytes).unwrap_err().to_string();
            assert!(
                err.contains("字段缺失") || err.contains("损坏"),
                "{field}: {err}"
            );
        }
    }

    #[test]
    fn parse_rejects_bad_values() {
        let raw = valid_enrollment_json();
        let mutate = |k: &str, v: serde_json::Value| -> Vec<u8> {
            let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            value[k] = v;
            serde_json::to_vec(&value).unwrap()
        };
        // device_id 空 / 含空格 / 非 ASCII
        assert!(parse_enrollment(&mutate("device_id", "".into())).is_err());
        assert!(parse_enrollment(&mutate("device_id", "a b".into())).is_err());
        assert!(parse_enrollment(&mutate("device_id", "设备-1".into())).is_err());
        // 端口 0
        assert!(parse_enrollment(&mutate("server_port", 0.into())).is_err());
        // 坏 PEM：缺块、只有 BEGIN
        assert!(parse_enrollment(&mutate("ca_pem", "not a pem".into())).is_err());
        assert!(parse_enrollment(&mutate("ca_pem", "-----BEGIN CERTIFICATE-----".into())).is_err());
        assert!(parse_enrollment(&mutate("client_cert_pem", "".into())).is_err());
        // host / name / policy 空
        assert!(parse_enrollment(&mutate("server_host", "  ".into())).is_err());
        assert!(parse_enrollment(&mutate("server_name", "".into())).is_err());
        assert!(parse_enrollment(&mutate("policy_bundle_json", " ".into())).is_err());
        // 签名形态：短、非 hex
        assert!(parse_enrollment(&mutate("policy_signature", "abcd".into())).is_err());
        assert!(parse_enrollment(&mutate("policy_signature", "zz".repeat(64).into())).is_err());
        // 有效期倒挂（not_after 波归 0，小于 not_before=1）
        assert!(parse_enrollment(&mutate("not_after_unix", 0u64.into())).is_err());
        // 坏 JSON
        assert!(parse_enrollment(b"{ nope").is_err());
    }

    #[test]
    fn parse_ignores_unknown_fields_but_rejects_wrong_types() {
        // 未知字段忽略（前向兼容：注册包 schema 演进不破坏旧客户端）；
        // 已知字段类型错误必须失败。
        let raw = valid_enrollment_json();
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["extra_field"] = "whatever".into();
        assert!(parse_enrollment(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["server_port"] = "not-a-number".into();
        assert!(parse_enrollment(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    // ---------- 紧凑私钥编解码 ----------

    #[test]
    fn compact_roundtrip_matches_original_pem() {
        let (key_pem, _) = test_key_and_cert();
        let compact = pem_to_compact(&Secret::new(key_pem.clone())).unwrap();
        assert!(compact.starts_with("v1:pkcs8:"));
        assert!(!compact.contains('\n') && !compact.contains(' '));
        // 还原后的 PEM 能被 rustls 解析，且 DER 完全一致
        let restored = compact_to_pem(&compact).unwrap();
        let orig = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
        let back = PrivateKeyDer::from_pem_slice(restored.expose().as_bytes()).unwrap();
        assert_eq!(orig.secret_der(), back.secret_der());
    }

    #[test]
    fn compact_rejects_garbage() {
        for bad in [
            "",
            "v1:",
            "v1:pkcs8",
            "v1:pkcs8:",
            "v2:pkcs8:AAAA",
            "v1:unknown:AAAA",
            "v1:pkcs8:A",
            "v1:pkcs8:AAA=",
            "v1:pkcs8:!!!!",
        ] {
            assert!(compact_to_pem(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn pem_to_compact_rejects_non_key_pem() {
        // 证书 PEM 不是私钥
        let (_, cert_pem) = test_key_and_cert();
        assert!(pem_to_compact(&Secret::new(cert_pem)).is_err());
        assert!(pem_to_compact(&Secret::new("")).is_err());
    }

    #[test]
    fn debug_and_errors_never_leak_secrets() {
        let secrets = ProxySecrets {
            username: "device-x".into(),
            password: Secret::new("super-secret-password"),
            client_key_pem: Secret::new("super-secret-key"),
        };
        let dbg = format!("{secrets:?}");
        assert!(dbg.contains("Secret(***)") && !dbg.contains("super-secret"));
        // 校验错误不含内容
        let err = validate_password(&Secret::new("x".repeat(300)))
            .unwrap_err()
            .to_string();
        assert!(!err.contains('x') || !err.contains("xxx"), "{err}");
    }

    #[test]
    fn base64_codec_known_vectors() {
        // RFC 4648 测试向量
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert!(base64_decode("Zm9vYmFy=").is_err()); // 长度非 4 倍数
    }

    #[test]
    fn policy_verify_key_hex_is_valid_point() {
        // 常量必须是 64 字节 hex 且能构造 ed25519 验证钥（防止发布前误填）。
        assert!(policy_verify_key().is_ok());
        // 大写 hex 不被接受（fail-closed，不做猜测修复）——间接验证解析严格性。
        assert_eq!(consts::POLICY_VERIFY_KEY_HEX.len(), 64);
        assert!(
            consts::POLICY_VERIFY_KEY_HEX
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    // ---------- 秘密存储（dev-mode 内存表；macOS 真钥匙串见下方单独模块） ----------

    /// 随机 device_id（可打印 ASCII）：dev-mode 的内存表无副作用；macOS 真钥匙串测试
    /// 在独立模块里用带随机后缀的 target。
    fn random_device_id(tag: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("dev-{tag}-{pid}-{nanos}-{seq}")
    }

    fn test_secrets(device_id: &str, key_pem: &str) -> ProxySecrets {
        ProxySecrets {
            username: format!("device-{device_id}"),
            password: Secret::new(format!("pass-{device_id}")),
            client_key_pem: Secret::new(key_pem.to_string()),
        }
    }

    #[test]
    fn secrets_roundtrip_dev_store() {
        let (key_pem, _) = test_key_and_cert();
        let device = random_device_id("roundtrip");
        let (password_target, key_target) = targets_for(&device);
        assert_eq!(
            password_target,
            format!("{}{}", consts::PROXY_PASSWORD_TARGET_PREFIX, device)
        );
        assert_eq!(
            key_target,
            format!("{}{}", consts::PROXY_KEY_TARGET_PREFIX, device)
        );
        // 绝不可能是生产 target
        assert_ne!(password_target, consts::CREDENTIAL_TARGET);
        assert_ne!(key_target, consts::CREDENTIAL_TARGET);

        save_secrets(&device, &test_secrets(&device, &key_pem)).unwrap();
        let loaded = load_secrets(&device).unwrap();
        assert_eq!(loaded.username, format!("device-{device}"));
        assert_eq!(loaded.password.expose(), format!("pass-{device}"));
        // 还原的私钥与原始 DER 一致
        let orig = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
        let back =
            PrivateKeyDer::from_pem_slice(loaded.client_key_pem.expose().as_bytes()).unwrap();
        assert_eq!(orig.secret_der(), back.secret_der());

        delete_secrets(&device);
        assert!(load_secrets(&device).is_err(), "删除后必须读不到");
        delete_secrets(&device); // 幂等
    }

    #[test]
    fn save_rejects_oversized_password() {
        let (key_pem, _) = test_key_and_cert();
        let device = random_device_id("oversize");
        let mut secrets = test_secrets(&device, &key_pem);
        secrets.password = Secret::new("p".repeat(256));
        let err = save_secrets(&device, &secrets).unwrap_err().to_string();
        assert!(err.contains("1–255"), "{err}");
        assert!(!err.contains("pppp"), "错误不得携带密码内容");
        // 拒绝发生在写入前：两条都不存在
        assert!(load_secrets(&device).is_err());
    }

    #[test]
    fn save_rejects_invalid_inputs() {
        let (key_pem, _) = test_key_and_cert();
        // device_id 非法
        for bad_device in ["", "a b", "设 备"] {
            let secrets = test_secrets("x", &key_pem);
            assert!(
                save_secrets(bad_device, &secrets).is_err(),
                "{bad_device:?}"
            );
        }
        // 用户名与 device-id 不一致
        let device = random_device_id("mismatch");
        let mut secrets = test_secrets(&device, &key_pem);
        secrets.username = "device-other".to_string();
        assert!(save_secrets(&device, &secrets).is_err());
        // 用户名/密码为空
        let mut secrets = test_secrets(&device, &key_pem);
        secrets.username = String::new();
        assert!(save_secrets(&device, &secrets).is_err());
        let mut secrets = test_secrets(&device, &key_pem);
        secrets.password = Secret::new("");
        assert!(save_secrets(&device, &secrets).is_err());
        // 私钥不是有效 PEM
        let mut secrets = test_secrets(&device, &key_pem);
        secrets.client_key_pem = Secret::new("not a key");
        assert!(save_secrets(&device, &secrets).is_err());
        assert!(
            load_secrets(&device).is_err(),
            "私钥解析失败时密码条目也不应残留"
        );
    }
}

/// macOS 真钥匙串小用例（设计 §10、秘密纪律）：target 由随机后缀 device_id 派生，必然带
/// 随机后缀，绝不触碰生产条目 `codex-helper/managed-gateway` 或任何固定 target；
/// 结束（含 panic）删除两条条目。仅 macOS 编译；其他平台此模块不存在（dev-mode 已覆盖）。
#[cfg(all(test, target_os = "macos"))]
mod keychain_tests {
    use super::*;

    /// 随机后缀 device_id：target = 前缀 + 随机串，测试间与生产条目零冲突。
    fn random_device_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("macos-{pid}-{nanos}-{seq}")
    }

    /// 结束（含 panic）删除两条条目，避免残留在开发机钥匙串。
    struct DeviceGuard(String);

    impl Drop for DeviceGuard {
        fn drop(&mut self) {
            delete_secrets(&self.0);
        }
    }

    fn test_key_pem() -> String {
        use rcgen::KeyPair;
        KeyPair::generate().expect("生成测试密钥").serialize_pem()
    }

    #[test]
    fn keychain_roundtrip_with_random_target() {
        let device = random_device_id();
        let _guard = DeviceGuard(device.clone());
        let (password_target, key_target) = targets_for(&device);
        // 铁律：绝不等于生产 target，且确实带随机后缀
        assert_ne!(password_target, consts::CREDENTIAL_TARGET);
        assert_ne!(key_target, consts::CREDENTIAL_TARGET);
        assert!(password_target.starts_with(consts::PROXY_PASSWORD_TARGET_PREFIX));
        assert!(key_target.starts_with(consts::PROXY_KEY_TARGET_PREFIX));

        let key_pem = test_key_pem();
        save_secrets(
            &device,
            &ProxySecrets {
                username: format!("device-{device}"),
                password: Secret::new("hk-proxy-pass-1"),
                client_key_pem: Secret::new(key_pem.clone()),
            },
        )
        .unwrap();

        let loaded = load_secrets(&device).unwrap();
        assert_eq!(loaded.username, format!("device-{device}"));
        assert_eq!(loaded.password.expose(), "hk-proxy-pass-1");
        let orig = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
        let back =
            PrivateKeyDer::from_pem_slice(loaded.client_key_pem.expose().as_bytes()).unwrap();
        assert_eq!(orig.secret_der(), back.secret_der());

        delete_secrets(&device);
        assert!(load_secrets(&device).is_err());
    }

    #[test]
    fn keychain_overwrite_and_reload() {
        let device = random_device_id();
        let _guard = DeviceGuard(device.clone());
        let key_pem = test_key_pem();
        save_secrets(
            &device,
            &ProxySecrets {
                username: format!("device-{device}"),
                password: Secret::new("first-pass"),
                client_key_pem: Secret::new(key_pem.clone()),
            },
        )
        .unwrap();
        save_secrets(
            &device,
            &ProxySecrets {
                username: format!("device-{device}"),
                password: Secret::new("second-pass"),
                client_key_pem: Secret::new(key_pem),
            },
        )
        .unwrap();
        let loaded = load_secrets(&device).unwrap();
        assert_eq!(loaded.password.expose(), "second-pass");
    }
}
