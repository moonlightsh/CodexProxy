//! 管理签名进程策略（设计 §6.2、§6.3、§8.2）。
//!
//! 职责边界：
//! - 本模块只做纯逻辑：策略包解析与 ed25519 签名验证、结构校验、对端进程是否获准的判定
//!   （`decide`）、按进程实例键控的决策缓存、映像文件 SHA-256 摘要。
//! - 连接所有者的查证（四元组→进程）在 `peer_identity`；串成准入门禁在 `connection_gate`；
//!   策略生命周期（注册、持久化、版本状态机）在 Manager。
//!
//! fail-closed：签名不匹配、JSON 损坏、结构缺失、摘要格式非法、策略过期、未知角色——
//! 一律 `Err`，调用方必须拒绝激活/拒绝连接，不得回退到无策略运行。
//!
//! 已知残余风险（Task 5 交接，设计 §6.4）：`decide` 比对的 `image_sha256_hex` 由调用方对
//! `PeerInfo.image_path` 处的**磁盘文件**计算（`hash_image_file`）。`image_path` 是内核记录的
//! 路径字符串，运行中的可执行文件被 unlink/rename 后在原路径放入新文件，这里校验的是新文件
//! 而不是运行中的映像。不能把该哈希当作运行中映像的证明；按 PID 取内核证据的强校验
//! （macOS SecCode 动态校验 / Windows 已打开句柄取证）留给后续版本。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::peer_identity::{PeerInfo, current_user_scope};

/// 本版唯一允许的角色：真实发起代理连接的 Codex 引擎（设计 §6.1）。
/// 策略携带其他角色名时按 fail-closed 拒绝，强制走客户端升级而不是静默忽略。
pub const ROLE_CODEX_ENGINE: &str = "codex-engine";

/// 决策缓存条目上限。键控缓存已随进程实例/映像/策略版本自然失效，上限只防长期运行的
/// 无界增长；超限时整体清空，后续决策重新计算，不影响 fail-closed 语义。
const CACHE_MAX_ENTRIES: usize = 1024;

/// 决策缓存键：（pid、进程启动时间毫秒、映像 SHA-256、策略版本）。
pub type PolicyCacheKey = (i32, u64, String, u64);

/// 已批准的代码映像：SHA-256 摘要 + 目标平台。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodeImage {
    pub sha256_hex: String,
    pub os: String,
    pub arch: String,
}

/// 允许的角色及其附加约束。
///
/// `path_prefixes` 是**安装位置约束**，不单独授信（设计 §6.2）：命中已批准映像摘要后，
/// 进程映像路径还必须落在其中一个前缀内；为空表示该策略不限制安装位置。
/// 前缀按字面字符串比较（大小写敏感，偏向拒绝而不是放宽）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AllowedRole {
    pub role: String,
    pub images: Vec<CodeImage>,
    pub path_prefixes: Vec<String>,
}

/// 管理签名策略。`not_after_unix` 为 Unix 秒；到期校验在 [`parse_and_verify`]，
/// 策略加载后的生命周期（续期、重载）由 Manager 负责。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProcessPolicy {
    pub policy_id: String,
    pub version: u64,
    pub not_after_unix: u64,
    pub roles: Vec<AllowedRole>,
}

/// 连接准入判定。`reason` 为机器可读短码：`"user"`（用户作用域不符）、
/// `"image"`（映像未获策略批准）。日志只输出短码，不携带对端细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 允许，附命中的角色名。
    Allow { role: String },
    /// 拒绝。
    Deny { reason: &'static str },
}

/// 策略包 JSON 的信封结构（设计 §6.2）：`policy` 是签名覆盖的规范字节来源。
/// 签名本身由调用方从注册包（`EnrollmentBundle.policy_signature`，Task 8）的独立字段取得，
/// 不从信封内读取——避免同一字节里两处签名来源不一致的歧义。
#[derive(Debug, Deserialize)]
struct PolicyEnvelope {
    policy: Value,
}

/// 解析并验证管理签名的策略包，返回可直接用于判定的 [`ProcessPolicy`]。
///
/// 规范化规则（签名双方必须一致）：
/// 1. `bundle_json` 解析为 `serde_json::Value` 后，签名覆盖 `serde_json::to_vec(&value)`
///    的字节。`Value` 内部是 BTreeMap 序（键排序），因此对同一逻辑对象，字段顺序不同的
///    JSON 文本会得到相同规范字节；`signature` 参数必须对这同一规范字节计算。
/// 2. 签名验证成功后，才把 `policy` 对象反序列化为 `ProcessPolicy` 并做结构校验。
///
/// fail-closed 拒绝：JSON 损坏、缺少信封字段、签名不匹配、结构缺失、摘要/平台字段非法、
/// 角色不在白名单、策略已过期（`not_after_unix` 小于当前 Unix 秒）。版本回退检测需要跨
/// 调用的持久状态，不在此函数内，由调用方（Manager）比对已安装版本。
pub fn parse_and_verify(
    bundle_json: &[u8],
    signature: &[u8],
    verify_key: &VerifyingKey,
) -> anyhow::Result<ProcessPolicy> {
    let envelope: PolicyEnvelope =
        serde_json::from_slice(bundle_json).context("策略包 JSON 损坏或缺少 policy 字段")?;
    let canonical = canonical_bytes(&envelope.policy)?;
    let sig = Signature::from_slice(signature)
        .map_err(|_| anyhow!("策略签名长度非法（期望 ed25519 64 字节）"))?;
    verify_key
        .verify(&canonical, &sig)
        .map_err(|_| anyhow!("策略签名验证失败：拒绝使用该策略包"))?;
    let policy: ProcessPolicy =
        serde_json::from_value(envelope.policy).context("策略结构不符合 ProcessPolicy 模式")?;
    validate(&policy)?;
    if policy.not_after_unix <= now_unix() {
        bail!(
            "策略已过期（not_after_unix={}）：拒绝",
            policy.not_after_unix
        );
    }
    Ok(policy)
}

/// [`Value`] 的规范字节：`serde_json::to_vec`。`Value` 的 map 是 BTreeMap（键排序）。
/// 若编译图中 `preserve_order` 特性被统一开启（tauri/schemars 传递依赖可能如此），
/// 字段顺序会变为插入序，签名双方的规范字节就不再稳定——这里显式做一次递归键排序
/// 规范化，不依赖特性组合。
fn canonical_bytes(value: &Value) -> anyhow::Result<Vec<u8>> {
    let sorted = sort_object_keys(value);
    serde_json::to_vec(&sorted).context("策略对象无法序列化为规范字节")
}

/// 递归地把所有 JSON 对象的键按字典序重排（数组顺序保持原样，数字/字符串原样保留）。
fn sort_object_keys(value: &Value) -> Value {
    match value {
        // 不依赖 serde_json 特性组合：显式按字典序排序后再重建对象。
        // 若只靠 BTreeMap 特性下的 collect，一旦 preserve_order 被开启，
        // 这里会退化为插入序 no-op，导致与签发端的规范字节不一致。
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_object_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_object_keys).collect()),
        other => other.clone(),
    }
}

/// 结构校验：policy_id 非空、roles 非空、每个角色的角色名已知、images 非空且每个摘要
/// 是 64 位小写 hex、os/arch 非空。
fn validate(policy: &ProcessPolicy) -> anyhow::Result<()> {
    if policy.policy_id.is_empty() {
        bail!("策略 policy_id 为空");
    }
    if policy.roles.is_empty() {
        bail!("策略 roles 为空：没有允许任何程序");
    }
    for allowed in &policy.roles {
        if allowed.role != ROLE_CODEX_ENGINE {
            bail!(
                "未知角色 {}：本客户端只认识 {ROLE_CODEX_ENGINE}",
                allowed.role
            );
        }
        if allowed.images.is_empty() {
            bail!("角色 {} 的 images 为空", allowed.role);
        }
        for image in &allowed.images {
            validate_sha256_hex(&image.sha256_hex)
                .with_context(|| format!("角色 {} 的映像摘要非法", allowed.role))?;
            if image.os.is_empty() || image.arch.is_empty() {
                bail!("角色 {} 的映像 os/arch 为空", allowed.role);
            }
        }
    }
    Ok(())
}

/// 摘要必须是 64 位小写 hex（规范形态，拒绝大写与空白）。
pub(crate) fn validate_sha256_hex(hex: &str) -> anyhow::Result<()> {
    let valid = hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if valid {
        Ok(())
    } else {
        bail!("SHA-256 摘要必须是 64 位小写 hex：{hex}");
    }
}

/// 当前 Unix 秒。独立成函数便于文档说明；测试不做时间模拟，过期用例直接构造边界值。
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 判定对端进程是否获准使用代理。
///
/// 顺序（设计 §6.3）：
/// 1. 用户作用域：与 `current_user_scope()` 不等 → `Deny { reason: "user" }`。
///    （本地代理只服务当前用户；`image` 未命中也绝不应把这一步做成可放过的软失败。）
/// 2. 映像批准：`image_sha256_hex` 必须命中某角色的 `images` 中“摘要 + os + arch”全部
///    匹配的条目；命中后进程映像路径还必须落在该角色的某个 `path_prefixes` 前缀内
///    （无前缀配置则不限制）。任一不满足 → `Deny { reason: "image" }`。
///
/// `image_sha256_hex` 的语义见模块头“已知残余风险”：它是对 `image_path` 处磁盘文件的
/// 摘要，不能证明运行中的映像未被替换。
///
/// 本函数纯同步、无 IO；哈希计算由调用方完成并传入。
pub fn decide(policy: &ProcessPolicy, peer: &PeerInfo, image_sha256_hex: &str) -> Decision {
    if peer.user_scope != current_user_scope() {
        return Decision::Deny { reason: "user" };
    }
    let (os, arch) = (current_os(), current_arch());
    for allowed in &policy.roles {
        let image_hit = allowed.images.iter().any(|image| {
            image.sha256_hex == image_sha256_hex && image.os == os && image.arch == arch
        });
        if !image_hit {
            continue;
        }
        let path_ok = allowed.path_prefixes.is_empty()
            || allowed
                .path_prefixes
                .iter()
                .any(|prefix| peer.image_path.starts_with(prefix.as_str()));
        if path_ok {
            return Decision::Allow {
                role: allowed.role.clone(),
            };
        }
    }
    Decision::Deny { reason: "image" }
}

/// 当前平台名（与策略 `CodeImage.os` 同一写法）：`macos` / `windows` / 其他。
pub fn current_os() -> String {
    std::env::consts::OS.to_string()
}

/// 当前架构名（与策略 `CodeImage.arch` 同一写法）：`aarch64` / `x86_64`。
pub fn current_arch() -> String {
    std::env::consts::ARCH.to_string()
}

/// 映像验证决策缓存。键 =（pid、进程启动时间毫秒、映像 SHA-256、策略版本）：
/// 进程退出、PID 复用（start_time 变化）、映像替换（哈希变化）、策略更新（版本变化）
/// 都得到新键，旧条目自然失效（设计 §6.3：不能仅以 PID/路径缓存）。
///
/// 仅缓存 `decide` 的判定结果；`resolve`（连接所有者查证）每条连接都重新做，不缓存。
pub struct PolicyCache {
    map: Mutex<HashMap<PolicyCacheKey, Decision>>,
}

impl PolicyCache {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// 命中返回缓存的判定；键任一分量不同（含同 pid 不同 start_time）即未命中。
    pub fn get(&self, key: &PolicyCacheKey) -> Option<Decision> {
        self.map
            .lock()
            .expect("PolicyCache 锁中毒")
            .get(key)
            .cloned()
    }

    /// 写入缓存；超过 [`CACHE_MAX_ENTRIES`] 时整体清空再写入（防无界增长，语义不变）。
    pub fn put(&self, key: PolicyCacheKey, decision: Decision) {
        let mut map = self.map.lock().expect("PolicyCache 锁中毒");
        if map.len() >= CACHE_MAX_ENTRIES {
            map.clear();
        }
        map.insert(key, decision);
    }
}

impl Default for PolicyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 计算文件 SHA-256，返回 64 位小写 hex。
///
/// 见模块头“已知残余风险”：这是对**磁盘路径**文件的摘要，`PeerInfo.image_path` 是内核
/// 记录的路径字符串，不能把本结果当作运行中映像未被打换的证明。
pub fn hash_image_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("无法打开映像文件 {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("读取映像文件失败")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// 字节转小写 hex。
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // ---------- 测试夹具 ----------

    /// 生成签名密钥对与“对规范字节签名”的辅助，模拟管理签发端。`tag` 混入种子使不同调用得到不同密钥。
    fn signing_key_with(tag: u8) -> SigningKey {
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(tag);
        }
        SigningKey::from_bytes(&seed)
    }

    fn signing_setup() -> SigningKey {
        signing_key_with(0)
    }

    /// 构造合法策略 `Value`（与签发端字段顺序故意不同，验证规范化）。
    fn policy_value(
        version: u64,
        not_after: u64,
        sha: &str,
        os: &str,
        arch: &str,
        prefixes: &[&str],
    ) -> Value {
        serde_json::json!({
            "roles": [{
                "role": ROLE_CODEX_ENGINE,
                "images": [{ "sha256_hex": sha, "os": os, "arch": arch }],
                "path_prefixes": prefixes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            }],
            "not_after_unix": not_after,
            "policy_id": "test-policy",
            "version": version,
        })
    }

    /// 签发：返回（bundle 字节, 签名字节）。bundle 的 policy 字段顺序故意打乱。
    fn issue(value: &Value, key: &SigningKey) -> (Vec<u8>, Vec<u8>) {
        let canonical = canonical_bytes(value).unwrap();
        let sig = key.sign(&canonical);
        let bundle =
            serde_json::json!({ "signature": hex_lower(&sig.to_bytes()), "policy": value });
        (
            serde_json::to_vec(&bundle).unwrap(),
            sig.to_bytes().to_vec(),
        )
    }

    fn policy_from(value: &Value, key: &SigningKey) -> ProcessPolicy {
        let (bundle, sig) = issue(value, key);
        parse_and_verify(&bundle, &sig, &key.verifying_key()).unwrap()
    }

    /// 与当前用户同作用域的 PeerInfo。
    fn peer_now(image_path: &str) -> PeerInfo {
        PeerInfo {
            pid: 4321,
            user_scope: current_user_scope(),
            start_time_ms: 1_700_000_000_000,
            image_path: image_path.to_string(),
        }
    }

    const GOOD_SHA: &str = "af11111111111111111111111111111111111111111111111111111111111111";
    const OTHER_SHA: &str = "22222222222222222222222222222222222222222222222222222222222222ee";
    const FAR_FUTURE: u64 = 4_102_444_800; // 2100-01-01

    // ---------- parse_and_verify ----------

    #[test]
    fn valid_signature_passes_even_with_reordered_fields() {
        let key = signing_setup();
        // 签发端字段顺序 = json! 宏顺序（roles 在前）；与规范字节不同序也能验证通过。
        let value = policy_value(3, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let policy = policy_from(&value, &key);
        assert_eq!(policy.version, 3);
        assert_eq!(policy.policy_id, "test-policy");
        assert_eq!(policy.roles.len(), 1);
        assert_eq!(policy.roles[0].role, ROLE_CODEX_ENGINE);
    }

    #[test]
    fn tampered_policy_rejected() {
        let key = signing_setup();
        let value = policy_value(3, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let (bundle, sig) = issue(&value, &key);
        // 篡改：把摘要换成另一个值，签名保持原样 → 必须拒绝。
        let mut parsed: Value = serde_json::from_slice(&bundle).unwrap();
        parsed["policy"]["roles"][0]["images"][0]["sha256_hex"] = Value::from(OTHER_SHA);
        let tampered = serde_json::to_vec(&parsed).unwrap();
        let err = parse_and_verify(&tampered, &sig, &key.verifying_key()).unwrap_err();
        assert!(
            err.to_string().contains("签名验证失败"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn wrong_key_rejected() {
        let key = signing_setup();
        let other = signing_key_with(1); // 不同种子 → 不同公钥
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let (bundle, sig) = issue(&value, &key);
        let err = parse_and_verify(&bundle, &sig, &other.verifying_key()).unwrap_err();
        assert!(
            err.to_string().contains("签名验证失败"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn expired_policy_rejected() {
        let key = signing_setup();
        let now = now_unix();
        // not_after == now 与 not_after < now 都过期。
        for not_after in [now, now.saturating_sub(1)] {
            let value = policy_value(1, not_after, GOOD_SHA, "macos", "aarch64", &[]);
            let (bundle, sig) = issue(&value, &key);
            let err = parse_and_verify(&bundle, &sig, &key.verifying_key()).unwrap_err();
            assert!(err.to_string().contains("已过期"), "unexpected: {err}");
        }
    }

    #[test]
    fn malformed_structures_rejected() {
        let key = signing_setup();
        let vk = key.verifying_key();

        // 签名长度非法。
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let (bundle, _) = issue(&value, &key);
        assert!(parse_and_verify(&bundle, &[1u8; 10], &vk).is_err());

        // JSON 损坏。
        assert!(parse_and_verify(b"{not json", &[0u8; 64], &vk).is_err());
        // 缺 policy 信封字段。
        let no_sig: Vec<u8> = br#"{}"#.to_vec();
        assert!(parse_and_verify(&no_sig, &[0u8; 64], &vk).is_err());

        // 合法签名但结构缺失：roles 空、未知角色、摘要非法、os 空、policy_id 空。
        struct Case(&'static str, Value);
        let cases = [
            Case(
                "roles 空",
                serde_json::json!({"policy_id":"p","version":1,"not_after_unix":FAR_FUTURE,"roles":[]}),
            ),
            Case(
                "未知角色",
                serde_json::json!({"policy_id":"p","version":1,"not_after_unix":FAR_FUTURE,
                "roles":[{"role":"browser","images":[{"sha256_hex":GOOD_SHA,"os":"macos","arch":"aarch64"}],"path_prefixes":[]}]}),
            ),
            Case(
                "摘要大写",
                serde_json::json!({"policy_id":"p","version":1,"not_after_unix":FAR_FUTURE,
                "roles":[{"role":ROLE_CODEX_ENGINE,"images":[{"sha256_hex":GOOD_SHA.to_uppercase(),"os":"macos","arch":"aarch64"}],"path_prefixes":[]}]}),
            ),
            Case(
                "摘要短",
                serde_json::json!({"policy_id":"p","version":1,"not_after_unix":FAR_FUTURE,
                "roles":[{"role":ROLE_CODEX_ENGINE,"images":[{"sha256_hex":"abcd","os":"macos","arch":"aarch64"}],"path_prefixes":[]}]}),
            ),
            Case(
                "os 空",
                serde_json::json!({"policy_id":"p","version":1,"not_after_unix":FAR_FUTURE,
                "roles":[{"role":ROLE_CODEX_ENGINE,"images":[{"sha256_hex":GOOD_SHA,"os":"","arch":"aarch64"}],"path_prefixes":[]}]}),
            ),
            Case(
                "policy_id 空",
                serde_json::json!({"policy_id":"","version":1,"not_after_unix":FAR_FUTURE,
                "roles":[{"role":ROLE_CODEX_ENGINE,"images":[{"sha256_hex":GOOD_SHA,"os":"macos","arch":"aarch64"}],"path_prefixes":[]}]}),
            ),
        ];
        for Case(name, policy_val) in cases {
            let canonical = canonical_bytes(&policy_val).unwrap();
            let sig = key.sign(&canonical);
            let bundle =
                serde_json::json!({"signature": hex_lower(&sig.to_bytes()), "policy": policy_val});
            let bundle = serde_json::to_vec(&bundle).unwrap();
            let res = parse_and_verify(&bundle, &sig.to_bytes(), &vk);
            assert!(
                res.is_err(),
                "{name} 应被拒绝，实际：{:?}",
                res.map(|p| p.policy_id)
            );
        }
    }

    // ---------- decide ----------

    #[test]
    fn decide_user_scope_mismatch_denies() {
        let key = signing_setup();
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let policy = policy_from(&value, &key);
        let mut peer = peer_now("/Applications/CodexCLI.app/Contents/MacOS/codex");
        peer.user_scope = if peer.user_scope == "0" {
            "1".into()
        } else {
            "0".into()
        };
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Deny { reason: "user" }
        );
    }

    #[test]
    fn decide_unknown_hash_denies() {
        let key = signing_setup();
        // 平台/架构与当前一致、摘要不在名单。
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, &current_os(), &current_arch(), &[]);
        let policy = policy_from(&value, &key);
        let peer = peer_now("/anywhere/codex");
        assert_eq!(
            decide(&policy, &peer, OTHER_SHA),
            Decision::Deny { reason: "image" }
        );
    }

    #[test]
    fn decide_platform_mismatch_denies() {
        let key = signing_setup();
        // 摘要正确但 os 写成与当前平台不同的值 → 不命中（避免硬编码 "windows"：
        // 在 Windows runner 上 current_os() 本就是 windows，硬编码会让用例恒假失败）。
        let wrong_os = if current_os() == "windows" {
            "macos"
        } else {
            "windows"
        };
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, wrong_os, &current_arch(), &[]);
        let policy = policy_from(&value, &key);
        let peer = peer_now("/anywhere/codex");
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Deny { reason: "image" }
        );
    }

    #[test]
    fn decide_path_prefix_mismatch_denies() {
        let key = signing_setup();
        let value = policy_value(
            1,
            FAR_FUTURE,
            GOOD_SHA,
            &current_os(),
            &current_arch(),
            &["/Applications/ChatGPT.app/"],
        );
        let policy = policy_from(&value, &key);
        let peer = peer_now("/tmp/codex");
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Deny { reason: "image" }
        );
        // 前缀必须是完整路径的前缀：/Applications/ChatGPT.app.evil 不放行。
        let peer = peer_now("/Applications/ChatGPT.app.evil/codex");
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Deny { reason: "image" }
        );
    }

    #[test]
    fn decide_approved_image_allows() {
        let key = signing_setup();
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, &current_os(), &current_arch(), &[]);
        let policy = policy_from(&value, &key);
        let peer = peer_now("/Applications/ChatGPT.app/Contents/MacOS/codex");
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            }
        );
        // 有前缀且命中也放行。
        let value = policy_value(
            1,
            FAR_FUTURE,
            GOOD_SHA,
            &current_os(),
            &current_arch(),
            &["/Applications/"],
        );
        let policy = policy_from(&value, &key);
        assert_eq!(
            decide(&policy, &peer, GOOD_SHA),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            }
        );
    }

    // ---------- PolicyCache ----------

    #[test]
    fn cache_same_key_roundtrip() {
        let cache = PolicyCache::new();
        let key = (42, 1_700_000_000_000, GOOD_SHA.to_string(), 7);
        assert_eq!(cache.get(&key), None);
        cache.put(
            key.clone(),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string(),
            },
        );
        assert_eq!(
            cache.get(&key),
            Some(Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            })
        );
    }

    #[test]
    fn cache_same_pid_different_start_time_misses() {
        let cache = PolicyCache::new();
        let k1 = (42, 1000, GOOD_SHA.to_string(), 7);
        cache.put(
            k1.clone(),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string(),
            },
        );
        // 同 pid 不同 start_time（PID 复用）→ 未命中。
        let k2 = (42, 2000, GOOD_SHA.to_string(), 7);
        assert_eq!(cache.get(&k2), None);
        // 同实例不同映像哈希（映像被替换）→ 未命中。
        let k3 = (42, 1000, OTHER_SHA.to_string(), 7);
        assert_eq!(cache.get(&k3), None);
        // 同实例同哈希不同策略版本 → 未命中。
        let k4 = (42, 1000, GOOD_SHA.to_string(), 8);
        assert_eq!(cache.get(&k4), None);
    }

    #[test]
    fn cache_bounded_by_clearing() {
        let cache = PolicyCache::new();
        for i in 0..CACHE_MAX_ENTRIES {
            cache.put(
                (i as i32, 0, GOOD_SHA.to_string(), 1),
                Decision::Deny { reason: "image" },
            );
        }
        // 达到上限后再 put → 整体清空，此前的键全部失效。
        cache.put(
            (9999, 0, GOOD_SHA.to_string(), 1),
            Decision::Deny { reason: "image" },
        );
        assert_eq!(cache.get(&(0, 0, GOOD_SHA.to_string(), 1)), None);
        assert_eq!(
            cache.get(&(9999, 0, GOOD_SHA.to_string(), 1)),
            Some(Decision::Deny { reason: "image" })
        );
    }

    // ---------- hash_image_file ----------

    #[test]
    fn hash_image_file_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.bin");
        std::fs::write(&path, b"hello policy\n").unwrap();
        // 与 shasum -a 256 核对过的期望值。
        let expect = "6f512aa0c087187c28697e833491e9ff6d1cfb66fcfe9b40d76e38f154bc4bb5".to_string();
        assert_eq!(hash_image_file(&path).unwrap(), expect);
    }

    #[test]
    fn hash_image_file_empty_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            hash_image_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(hash_image_file(&dir.path().join("missing.bin")).is_err());
    }

    // ---------- 残余风险与规范化的文档性断言 ----------

    #[test]
    fn canonical_bytes_sorts_keys_recursively_and_preserves_arrays() {
        let v: Value = serde_json::from_str(r#"{"z":1,"a":{"y":[3,2,1],"b":0}}"#).unwrap();
        let canon = String::from_utf8(canonical_bytes(&v).unwrap()).unwrap();
        assert_eq!(
            canon,
            r#"{"a":{"b":0,"y":[3,2,1]}},"z":1}"#.replace("},\"z\"", ",\"z\"")
        );
    }

    #[test]
    fn debug_output_contains_no_secrets() {
        // 策略是公开文档，Debug 输出包含策略字段是允许的；这里断言 Decision/ProcessPolicy
        // 的 Debug 不携带秘密类型（模块内根本没有秘密字段）。
        let key = signing_setup();
        let value = policy_value(1, FAR_FUTURE, GOOD_SHA, "macos", "aarch64", &[]);
        let policy = policy_from(&value, &key);
        let s = format!("{policy:?}");
        assert!(s.contains("test-policy"));
        let _ = format!("{:?}", Decision::Deny { reason: "user" });
    }

    #[test]
    fn hex_lower_known_vector() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn signature_of_reordered_json_still_verifies() {
        // 同一逻辑策略的两种字段顺序（对象键顺序不同）→ 相同规范字节 → 同一签名可验。
        let key = signing_setup();
        let v1: Value = serde_json::from_str(&format!(
            r#"{{"policy_id":"p","version":1,"not_after_unix":{FAR_FUTURE},"roles":[{{"role":"{ROLE_CODEX_ENGINE}","images":[{{"sha256_hex":"{GOOD_SHA}","os":"macos","arch":"aarch64"}}],"path_prefixes":[]}}]}}"#
        )).unwrap();
        let v2: Value = serde_json::from_str(&format!(
            r#"{{"roles":[{{"path_prefixes":[],"images":[{{"arch":"aarch64","os":"macos","sha256_hex":"{GOOD_SHA}"}}],"role":"{ROLE_CODEX_ENGINE}"}}],"not_after_unix":{FAR_FUTURE},"version":1,"policy_id":"p"}}"#
        )).unwrap();
        assert_eq!(canonical_bytes(&v1).unwrap(), canonical_bytes(&v2).unwrap());
        let canonical = canonical_bytes(&v1).unwrap();
        let sig = key.sign(&canonical);
        let bundle2 = serde_json::json!({"policy": v2, "signature": hex_lower(&sig.to_bytes())});
        parse_and_verify(
            &serde_json::to_vec(&bundle2).unwrap(),
            &sig.to_bytes(),
            &key.verifying_key(),
        )
        .expect("重排后的同一策略必须验证通过");
    }
}
