//! macOS 实现：libproc 枚举全部进程的 TCP socket，按四元组定位连接方。
//!
//! 流程：`proc_listpids(PROC_ALL_PIDS)`（`pids_by_type(All)`）→ 每个进程 `PROC_PIDLISTFDS` → socket fd 取
//! `PROC_PIDFDSOCKETINFO` → 只认方向相反的对端 socket（见 [`super::is_peer_socket`]）→
//! 唯一 PID → `proc_pidinfo(PROC_PIDTBSDINFO)` 取 uid/启动时间、`proc_pidpath` 取映像路径 →
//! 复核该 PID 仍持有连接且启动时间不变（防查询期间进程退出、PID 复用）。
//! 无权查看的进程（其他用户/root）被跳过；若连接恰由其持有则“未找到”，fail-closed。

use std::ffi::c_int;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, anyhow, bail};
use libproc::libproc::bsd_info::BSDInfo;
use libproc::libproc::file_info::{ListFDs, ProcFDType, pidfdinfo};
use libproc::libproc::net_info::{InSIAddr, SocketFDInfo, SocketInfoKind};
use libproc::libproc::proc_pid::{listpidinfo, pidinfo, pidpath};
use libproc::processes::{ProcFilter, pids_by_type};

use super::{Endpoint, PeerInfo, PeerResolver, is_peer_socket, unique_owner, validate_query};

/// `<netinet/in_pcb.h>`：`INP_IPV4` / `INP_IPV6`（`insi_vflag` 位）。
const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;

pub(super) struct SystemResolver;

impl PeerResolver for SystemResolver {
    fn resolve(&self, local: (IpAddr, u16), remote: (IpAddr, u16)) -> anyhow::Result<PeerInfo> {
        let (local, remote) = validate_query(local, remote)?;
        let pids = pids_by_type(ProcFilter::All).context("列出进程失败")?;
        let owners = pids
            .into_iter()
            .filter_map(|p| i32::try_from(p).ok())
            .filter(|&pid| pid > 0 && owns_peer_socket(pid, local, remote));
        let pid = unique_owner(owners)?;

        let before = bsd_info(pid)?;
        let image_path = pidpath(pid).map_err(|e| anyhow!("读取进程 {pid} 映像路径失败：{e}"))?;
        // 复核：同一进程实例仍持有该连接（期间退出或 PID 被复用都拒绝）
        if !owns_peer_socket(pid, local, remote) {
            bail!("进程 {pid} 在解析期间不再持有该连接");
        }
        let after = bsd_info(pid)?;
        let start_time_ms = start_ms(&before);
        if start_time_ms != start_ms(&after) || before.pbi_uid != after.pbi_uid {
            bail!("进程 {pid} 在解析期间发生变化（疑似 PID 复用）");
        }
        if start_time_ms == 0 || image_path.is_empty() {
            bail!("进程 {pid} 身份信息不完整");
        }
        Ok(PeerInfo {
            pid,
            user_scope: after.pbi_uid.to_string(),
            start_time_ms,
            image_path,
        })
    }
}

/// 当前进程有效 uid 的十进制字符串；读取失败返回空串。
pub(super) fn current_user_scope() -> String {
    let pid = std::process::id() as i32;
    bsd_info(pid)
        .map(|i| i.pbi_uid.to_string())
        .unwrap_or_default()
}

fn bsd_info(pid: i32) -> anyhow::Result<BSDInfo> {
    pidinfo::<BSDInfo>(pid, 0).map_err(|e| anyhow!("读取进程 {pid} 信息失败：{e}"))
}

/// 启动时间（Unix 毫秒）。
fn start_ms(info: &BSDInfo) -> u64 {
    info.pbi_start_tvsec
        .saturating_mul(1000)
        .saturating_add(info.pbi_start_tvusec / 1000)
}

/// 进程 `pid` 是否持有该连接的对端 socket；任何查询失败（无权限、已退出）视为不持有。
fn owns_peer_socket(pid: i32, local: Endpoint, remote: Endpoint) -> bool {
    let Ok(info) = pidinfo::<BSDInfo>(pid, 0) else {
        return false;
    };
    let Ok(fds) = listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize) else {
        return false;
    };
    fds.iter()
        .filter(|fd| matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::Socket))
        .filter_map(|fd| pidfdinfo::<SocketFDInfo>(pid, fd.proc_fd).ok())
        .filter(|s| matches!(SocketInfoKind::from(s.psi.soi_kind), SocketInfoKind::Tcp))
        .any(|s| {
            // SAFETY：soi_kind == Tcp 时内核填充的是 pri_tcp 分支
            let ini = unsafe { s.psi.soi_proto.pri_tcp }.tcpsi_ini;
            let sock_local = endpoint(ini.insi_vflag, &ini.insi_laddr, ini.insi_lport);
            let sock_foreign = endpoint(ini.insi_vflag, &ini.insi_faddr, ini.insi_fport);
            match (sock_local, sock_foreign) {
                (Some(l), Some(f)) => is_peer_socket(l, f, local, remote),
                _ => false,
            }
        })
}

/// 解码内核 socket 地址：端口为网络字节序（低 16 位）；IPv4 存放在 `ina_46` 尾部。
fn endpoint(vflag: u8, addr: &InSIAddr, port: c_int) -> Option<Endpoint> {
    let port = u16::from_be(port as u16);
    if vflag & INI_IPV4 != 0 {
        // SAFETY：INP_IPV4 时内核按 in_addr_4in6 布局填充
        let s_addr = unsafe { addr.ina_46.i46a_addr4.s_addr };
        Some((IpAddr::V4(Ipv4Addr::from(s_addr.to_ne_bytes())), port))
    } else if vflag & INI_IPV6 != 0 {
        // SAFETY：INP_IPV6 时内核按 in6_addr 布局填充
        let bytes = unsafe { addr.ina_6.s6_addr };
        Some((IpAddr::V6(Ipv6Addr::from(bytes)), port))
    } else {
        None
    }
}
