//! 对端进程身份解析（设计 §6.3、§6.4）。
//!
//! 给定本地代理已接受连接的四元组（`local` = 代理侧本地端点，`remote` = 对端端点），
//! 找出**唯一**持有对端 socket（本地端点 == `remote`、远端端点 == `local`）的进程，
//! 返回其 PID、用户作用域、启动时间与映像路径。只做连接所有者查询与平台证据收集，
//! 不含业务白名单（白名单判定见进程策略模块）。
//!
//! fail-closed：查不到、多候选、权限不足、进程中途退出或 PID 被复用，一律返回 `Err`，
//! 调用方必须拒绝该连接，不得放行或回退。

use std::net::IpAddr;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as platform;

/// 连接所有者进程的身份证据。
///
/// - `user_scope`：macOS 为有效 uid 的十进制字符串；Windows 为进程 token 的用户 SID 字符串；
/// - `start_time_ms`：进程启动时间（Unix 毫秒），与 `pid` 组合标识进程实例，防 PID 复用；
/// - `image_path`：内核报告的进程映像路径（运行中的映像，不是磁盘上同名新文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub pid: i32,
    pub user_scope: String,
    pub start_time_ms: u64,
    pub image_path: String,
}

/// 连接所有者解析器。测试中可替换为假实现。
pub trait PeerResolver: Send + Sync {
    /// `local`：代理侧（accept 得到的 socket）本地端点；`remote`：对端端点。
    fn resolve(&self, local: (IpAddr, u16), remote: (IpAddr, u16)) -> anyhow::Result<PeerInfo>;
}

/// 当前进程的用户作用域（与 [`PeerInfo::user_scope`] 同格式）。
///
/// 读取失败时返回空字符串：解析成功的 `PeerInfo` 的作用域永不为空，空串不会与之相等，
/// 因而按用户作用域比对时自然拒绝。
pub fn current_user_scope() -> String {
    platform::current_user_scope()
}

/// 当前平台的系统解析器：macOS / Windows 为真实实现，其他平台恒返回 `Err`。
pub fn system_resolver() -> Box<dyn PeerResolver> {
    Box::new(platform::SystemResolver)
}

/// 不支持的平台：一律拒绝。
#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{PeerInfo, PeerResolver};
    use std::net::IpAddr;

    pub(super) struct SystemResolver;

    impl PeerResolver for SystemResolver {
        fn resolve(&self, _l: (IpAddr, u16), _r: (IpAddr, u16)) -> anyhow::Result<PeerInfo> {
            anyhow::bail!("当前平台不支持对端进程身份解析")
        }
    }

    pub(super) fn current_user_scope() -> String {
        String::new()
    }
}

/// 端点：(地址, 端口)。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // 仅 macOS/Windows 实现使用
pub(crate) type Endpoint = (IpAddr, u16);

/// 归一化端点：IPv4 映射的 IPv6 地址（`::ffff:a.b.c.d`）折叠为 IPv4，
/// 使双栈监听收到的对端地址能与客户端 IPv4 socket 比对。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // 仅 macOS/Windows 实现使用
pub(crate) fn normalize(ep: Endpoint) -> Endpoint {
    (ep.0.to_canonical(), ep.1)
}

/// 校验并归一化查询四元组：两端都必须是回环地址且端口非 0（只认本机连接方）。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // 仅 macOS/Windows 实现使用
pub(crate) fn validate_query(
    local: Endpoint,
    remote: Endpoint,
) -> anyhow::Result<(Endpoint, Endpoint)> {
    let (local, remote) = (normalize(local), normalize(remote));
    for (name, ep) in [("本地", local), ("对端", remote)] {
        if ep.1 == 0 || !ep.0.is_loopback() {
            anyhow::bail!("{name}端点 {}:{} 不是有效的回环端点，拒绝解析", ep.0, ep.1);
        }
    }
    Ok((local, remote))
}

/// socket（本地端点 `sock_local`、远端端点 `sock_foreign`）是否为该连接的**对端** socket。
///
/// 方向必须严格相反：对端 socket 的本地端点 == 查询的 `remote`、远端端点 == 查询的 `local`。
/// 代理自身 accept 得到的 socket 方向相同，因此不会被误认为连接方。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // 仅 macOS/Windows 实现使用
pub(crate) fn is_peer_socket(
    sock_local: Endpoint,
    sock_foreign: Endpoint,
    query_local: Endpoint,
    query_remote: Endpoint,
) -> bool {
    normalize(sock_local) == normalize(query_remote)
        && normalize(sock_foreign) == normalize(query_local)
}

/// 从候选 PID 中挑出唯一所有者：同一 PID 多个 fd（dup）算一个；
/// 零个或多个不同 PID 一律拒绝。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // 仅 macOS/Windows 实现使用
pub(crate) fn unique_owner(pids: impl IntoIterator<Item = i32>) -> anyhow::Result<i32> {
    let mut owner = None;
    for pid in pids {
        match owner {
            None => owner = Some(pid),
            Some(p) if p == pid => {}
            Some(p) => anyhow::bail!("连接存在多个候选所有者进程（至少 {p} 与 {pid}），拒绝"),
        }
    }
    owner.ok_or_else(|| anyhow::anyhow!("未找到连接所有者进程（不存在、已退出或无权查看）"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(port: u16) -> Endpoint {
        (IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn normalize_folds_ipv4_mapped_ipv6() {
        let mapped = (IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), 80);
        assert_eq!(normalize(mapped), v4(80));
        let v6 = (IpAddr::V6(Ipv6Addr::LOCALHOST), 80);
        assert_eq!(normalize(v6), v6);
        assert_eq!(normalize(v4(1)), v4(1));
    }

    #[test]
    fn validate_query_accepts_loopback_and_normalizes() {
        let mapped = (IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), 5000);
        let (l, r) = validate_query(v4(17891), mapped).unwrap();
        assert_eq!((l, r), (v4(17891), v4(5000)));
        let v6 = (IpAddr::V6(Ipv6Addr::LOCALHOST), 1);
        assert!(validate_query(v6, (IpAddr::V6(Ipv6Addr::LOCALHOST), 2)).is_ok());
    }

    #[test]
    fn validate_query_rejects_zero_port_unspecified_and_non_loopback() {
        assert!(validate_query(v4(0), v4(5000)).is_err());
        assert!(validate_query(v4(17891), v4(0)).is_err());
        let any = (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5000);
        assert!(validate_query(v4(17891), any).is_err());
        let lan = (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 5000);
        assert!(validate_query(v4(17891), lan).is_err());
        assert!(validate_query(lan, v4(5000)).is_err());
    }

    #[test]
    fn peer_socket_requires_opposite_direction() {
        let (server, client) = (v4(17891), v4(50000));
        // 客户端 socket：本地=client，远端=server → 是对端
        assert!(is_peer_socket(client, server, server, client));
        // 代理自身 accept 的 socket：本地=server，远端=client → 不是对端
        assert!(!is_peer_socket(server, client, server, client));
        // 监听 socket（远端未指定）→ 不是
        let any = (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        assert!(!is_peer_socket(server, any, server, client));
        // 端口不同 → 不是
        assert!(!is_peer_socket(v4(50001), server, server, client));
        // IPv4 映射地址经归一化后可匹配
        let mapped_server = (IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), 17891);
        assert!(is_peer_socket(client, mapped_server, server, client));
    }

    #[test]
    fn unique_owner_rules() {
        assert!(unique_owner([]).is_err());
        assert_eq!(unique_owner([42]).unwrap(), 42);
        assert_eq!(unique_owner([42, 42]).unwrap(), 42);
        assert!(unique_owner([42, 43]).is_err());
    }
}
