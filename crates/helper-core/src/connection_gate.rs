//! 本机进程身份门禁（设计 §6.3）。
//!
//! 职责：把「连接所有者查证」（`peer_identity`）与「进程策略判定」（`process_policy`）
//! 串成代理连接的准入门禁：
//!
//! 1. [`ConnectionGate::resolve_with_retry`]：按四元组解析连接所有者，失败时在门禁预算内
//!    做一次有界复查（见下）；
//! 2. [`ConnectionGate::admit`]：对对端映像文件计算 SHA-256，交给 [`decide`] 判定，
//!    结果按（pid、启动时间、映像摘要、策略版本）缓存。
//!
//! fail-closed：查证失败、复查后仍多候选、映像文件不可读、用户作用域不符、映像未获
//! 批准——一律拒绝，绝不放行、绝不回退，也绝不「取第一个候选」。
//!
//! 复查预算（Task 5 交接，控制器裁决）：Codex 引擎 fork/spawn 子进程会瞬时继承 socket，
//! 造成瞬时「多候选」误拒。允许在 1 秒门禁预算内做一次有界复查（间隔 100-200ms、
//! 总尝试 2 次）；仍失败则 fail-closed 拒绝并记日志。复查不区分失败种类：区分「瞬时
//! 多候选」与其他错误需要解析错误文案，脆弱且无收益；有界两次 + 150ms 间隔的最坏耗时
//! 仍在调用方的 1 秒门禁预算内。
//!
//! 缓存边界（设计 §6.3）：仅缓存 `decide` 的 Allow/Deny 结果；`resolve`（socket 所有者
//! 查证）每条连接都重新做；映像摘要每次重新计算（文件被替换即得新键，立即失效），
//! 不以 PID/路径缓存。

use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::peer_identity::{PeerInfo, PeerResolver};
use crate::process_policy::{
    Decision, PolicyCache, PolicyCacheKey, ProcessPolicy, decide, hash_image_file,
};

/// 解析尝试总数：初次 + 一次有界复查（Task 5 交接的控制器裁决）。
const RESOLVE_ATTEMPTS: usize = 2;
/// 复查间隔：100-200ms 预算内取 150ms，两次尝试合计仍远小于 1 秒门禁超时。
const RESOLVE_RETRY_DELAY: Duration = Duration::from_millis(150);

/// 本机进程身份门禁。由 [`crate::proxy::ProxyConfig::gate`] 持有；`None` 时代理保持
/// Legacy 行为（无门禁）。
pub struct ConnectionGate {
    resolver: Box<dyn PeerResolver>,
    policy: Arc<ProcessPolicy>,
    cache: Mutex<PolicyCache>,
}

impl ConnectionGate {
    /// 构造门禁。`resolver` 通常是 [`crate::peer_identity::system_resolver`]，
    /// 测试可注入假实现。
    pub fn new(resolver: Box<dyn PeerResolver>, policy: Arc<ProcessPolicy>) -> Self {
        Self {
            resolver,
            policy,
            cache: Mutex::new(PolicyCache::new()),
        }
    }

    /// 按四元组解析连接所有者，失败时做一次有界复查（见模块头「复查预算」）。
    ///
    /// 这是阻塞调用（含至多一次 150ms 复查间隔），调用方必须在 `spawn_blocking` 中执行，
    /// 并自行为整体套上门禁超时（设计 §6.4：单连接总门禁超时 1s，超时拒绝，不自动放行）。
    pub fn resolve_with_retry(
        &self,
        local: (IpAddr, u16),
        remote: (IpAddr, u16),
    ) -> anyhow::Result<PeerInfo> {
        let mut last_error = None;
        for attempt in 0..RESOLVE_ATTEMPTS {
            if attempt > 0 {
                // 瞬时多候选（fd 继承）通常在子进程退出后自愈；给一小段缓冲再复查。
                std::thread::sleep(RESOLVE_RETRY_DELAY);
            }
            match self.resolver.resolve(local, remote) {
                Ok(peer) => return Ok(peer),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("连接所有者解析失败")))
    }

    /// 对已解析出的对端身份做映像验证并给出判定。
    ///
    /// 顺序：计算映像摘要 → 组缓存键 → 命中缓存直接返回；未命中则 `decide` 后入缓存。
    /// 映像摘要计算失败（文件不存在/不可读）按 `"image"` 拒绝且不入缓存——没有摘要就
    /// 组不出缓存键，且不能替「曾经可读」的旧判定背书（fail-closed）。
    pub fn admit(&self, peer: &PeerInfo) -> Decision {
        let image_sha256_hex = match hash_image_file(Path::new(&peer.image_path)) {
            Ok(sha) => sha,
            Err(_) => return Decision::Deny { reason: "image" },
        };
        let key: PolicyCacheKey = (
            peer.pid,
            peer.start_time_ms,
            image_sha256_hex,
            self.policy.version,
        );
        let cache = self.cache.lock().expect("门禁缓存锁中毒");
        if let Some(cached) = cache.get(&key) {
            return cached;
        }
        let decision = decide(&self.policy, peer, &key.2);
        cache.put(key, decision.clone());
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_identity::current_user_scope;
    use crate::process_policy::{
        AllowedRole, CodeImage, ROLE_CODEX_ENGINE, current_arch, current_os,
    };
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 2100-01-01，任何测试运行时间都早于它。
    const FAR_FUTURE: u64 = 4_102_444_800;

    fn endpoint(port: u16) -> (IpAddr, u16) {
        (IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// 与当前用户同作用域的 PeerInfo。
    fn peer_with(image_path: &str) -> PeerInfo {
        PeerInfo {
            pid: 4321,
            user_scope: current_user_scope(),
            start_time_ms: 1_700_000_000_000,
            image_path: image_path.to_string(),
        }
    }

    /// 只批准摘要 `sha` 的单角色策略（平台/架构取当前值）。
    fn policy_allowing(sha: &str) -> Arc<ProcessPolicy> {
        Arc::new(ProcessPolicy {
            policy_id: "gate-test-policy".into(),
            version: 3,
            not_after_unix: FAR_FUTURE,
            roles: vec![AllowedRole {
                role: ROLE_CODEX_ENGINE.into(),
                images: vec![CodeImage {
                    sha256_hex: sha.into(),
                    os: current_os(),
                    arch: current_arch(),
                }],
                path_prefixes: vec![],
            }],
        })
    }

    /// 可编程假解析器：第 n 次调用（从 0 起）交给闭包；`counter` 记录总调用次数。
    struct FakeResolver<F> {
        script: F,
        counter: Arc<AtomicUsize>,
    }

    impl<F> FakeResolver<F> {
        fn new(script: F) -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    script,
                    counter: Arc::clone(&counter),
                },
                counter,
            )
        }
    }

    impl<F: Fn(usize) -> anyhow::Result<PeerInfo> + Send + Sync> PeerResolver for FakeResolver<F> {
        fn resolve(
            &self,
            _local: (IpAddr, u16),
            _remote: (IpAddr, u16),
        ) -> anyhow::Result<PeerInfo> {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            (self.script)(n)
        }
    }

    /// 写一个临时映像文件，返回（目录、映像路径、内容摘要）。
    fn temp_image(content: &[u8]) -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("codex.bin");
        std::fs::write(&path, content).expect("write image");
        let sha = hash_image_file(&path).expect("hash image");
        let path_str = path.to_string_lossy().into_owned();
        (dir, path_str, sha)
    }

    // ---------- resolve_with_retry ----------

    #[test]
    fn transient_multi_candidate_recovers_within_bounded_retry() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        // 第一次：fd 继承造成的瞬时多候选；复查时子进程已退出，解析成功。
        let (resolver, calls) = FakeResolver::new(move |n| {
            if n == 0 {
                anyhow::bail!("连接存在多个候选所有者进程（至少 111 与 222），拒绝");
            }
            Ok(peer.clone())
        });
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        let resolved = gate
            .resolve_with_retry(endpoint(17891), endpoint(50000))
            .expect("复查后应解析成功");
        assert_eq!(resolved.pid, 4321);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "恰好两次尝试");
    }

    #[test]
    fn persistent_error_fails_closed_after_exactly_two_attempts() {
        let (_dir, _image_path, sha) = temp_image(b"codex-engine-image");
        let (resolver, calls) = FakeResolver::new(|_n| anyhow::bail!("未找到连接所有者进程"));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        let error = gate
            .resolve_with_retry(endpoint(17891), endpoint(50000))
            .expect_err("持续失败必须拒绝");
        assert!(error.to_string().contains("未找到"), "{error}");
        // 有界复查：恰好两次，不得无限重试。
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn persistent_multi_candidate_fails_without_taking_first_candidate() {
        let (_dir, _image_path, sha) = temp_image(b"codex-engine-image");
        let (resolver, calls) = FakeResolver::new(|_n| {
            anyhow::bail!("连接存在多个候选所有者进程（至少 111 与 222），拒绝")
        });
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert!(
            gate.resolve_with_retry(endpoint(17891), endpoint(50000))
                .is_err(),
            "多候选复查后仍失败必须拒绝，不得取第一个候选"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn successful_first_attempt_does_not_retry() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        let peer_for_resolver = peer.clone();
        let (resolver, calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert!(
            gate.resolve_with_retry(endpoint(17891), endpoint(50000))
                .is_ok()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "成功后不再复查");
    }

    // ---------- admit ----------

    #[test]
    fn admit_allows_approved_image() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        let peer_for_resolver = peer.clone();
        let (resolver, _calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert_eq!(
            gate.admit(&peer),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            }
        );
    }

    #[test]
    fn admit_denies_unknown_image() {
        let (_dir, image_path, _sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        // 策略批准的是另一个摘要。
        let peer_for_resolver = peer.clone();
        let (resolver, _calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&"ff".repeat(32)));
        assert_eq!(gate.admit(&peer), Decision::Deny { reason: "image" });
    }

    #[test]
    fn admit_denies_missing_image_file() {
        let peer = peer_with("/definitely/not/here/codex");
        let (_dir, _image_path, sha) = temp_image(b"whatever");
        let peer_for_resolver = peer.clone();
        let (resolver, _calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert_eq!(gate.admit(&peer), Decision::Deny { reason: "image" });
    }

    #[test]
    fn admit_denies_user_scope_mismatch() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let mut peer = peer_with(&image_path);
        peer.user_scope = if peer.user_scope == "0" {
            "1".into()
        } else {
            "0".into()
        };
        let peer_for_resolver = peer.clone();
        let (resolver, _calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert_eq!(gate.admit(&peer), Decision::Deny { reason: "user" });
    }

    #[test]
    fn admit_repeated_calls_return_consistent_decision() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        let peer_for_resolver = peer.clone();
        let (resolver, _calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        for _ in 0..3 {
            assert_eq!(
                gate.admit(&peer),
                Decision::Allow {
                    role: ROLE_CODEX_ENGINE.to_string()
                }
            );
        }
    }

    #[test]
    fn admit_caches_by_process_instance_key() {
        let (_dir, image_path, sha) = temp_image(b"codex-engine-image");
        let peer = peer_with(&image_path);
        let peer_for_resolver = peer.clone();
        let (resolver, calls) = FakeResolver::new(move |_n| Ok(peer_for_resolver.clone()));
        let gate = ConnectionGate::new(Box::new(resolver), policy_allowing(&sha));
        assert_eq!(
            gate.admit(&peer),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            }
        );
        // 同一进程实例重复准入走缓存：再次调用仍返回同一判定。
        assert_eq!(
            gate.admit(&peer),
            Decision::Allow {
                role: ROLE_CODEX_ENGINE.to_string()
            }
        );
        let _ = calls;
    }
}
