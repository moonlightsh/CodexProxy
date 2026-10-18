//! Windows 实现：`GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)` 定位连接所有者 PID。
//!
//! 流程：查询 IPv4 与 IPv6 两张 TCP 表（地址统一归一化，覆盖双栈与 IPv4 映射）→
//! 只认方向相反的对端行（见 [`super::is_peer_socket`]）→ 唯一 PID →
//! `OpenProcess` 持有进程句柄（句柄存续期间 PID 不会被复用）→ 取创建时间、映像路径、
//! token 用户 SID → 复核该 PID 仍是唯一所有者。任何一步失败都返回 `Err`（fail-closed）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, anyhow, bail};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE, HLOCAL, LocalFree, NO_ERROR,
};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

use super::{Endpoint, PeerInfo, PeerResolver, is_peer_socket, unique_owner, validate_query};

/// `ws2def.h`：地址族常量（避免为两个整数引入 WinSock feature）。
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;
/// TIME_WAIT 等已无所属进程的行，owning PID 记为 0（System Idle）。这类行不算候选；
/// 若只剩这类行，结果是“未找到”而不是“打开进程 0 失败”。
const IDLE_PID: u32 = 0;
/// FILETIME（1601-01-01 起 100ns）与 Unix 纪元之差。
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

pub(super) struct SystemResolver;

impl PeerResolver for SystemResolver {
    fn resolve(&self, local: (IpAddr, u16), remote: (IpAddr, u16)) -> anyhow::Result<PeerInfo> {
        let (local, remote) = validate_query(local, remote)?;
        let pid = unique_owner(owner_pids(local, remote)?)?;
        let process = Handle::open_process(pid as u32)?;
        let start_time_ms = creation_time_ms(&process)?;
        let image_path = image_path(&process)?;
        let user_scope = user_sid(&process)?;
        // 复核：持有句柄后该 PID 仍是唯一所有者（查询与打开之间进程可能已退出并被复用）
        if unique_owner(owner_pids(local, remote)?)? != pid {
            bail!("进程 {pid} 在解析期间不再持有该连接");
        }
        Ok(PeerInfo {
            pid,
            user_scope,
            start_time_ms,
            image_path,
        })
    }
}

/// 当前进程 token 的用户 SID 字符串；读取失败返回空串。
pub(super) fn current_user_scope() -> String {
    // SAFETY：伪句柄无需关闭
    let current = Handle(unsafe { GetCurrentProcess() }, false);
    user_sid(&current).unwrap_or_default()
}

/// 进程句柄；`.1` 为是否需要关闭（伪句柄不关闭）。
struct Handle(HANDLE, bool);

impl Handle {
    fn open_process(pid: u32) -> anyhow::Result<Self> {
        // SAFETY：FFI 调用，返回的句柄由 Drop 关闭
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .with_context(|| format!("打开进程 {pid} 失败"))?;
        Ok(Self(h, true))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.1 {
            // SAFETY：句柄由 OpenProcess/OpenProcessToken 取得且只关闭一次
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// 两张 TCP 表中持有对端 socket 的全部 PID（含重复）。
fn owner_pids(local: Endpoint, remote: Endpoint) -> anyhow::Result<Vec<i32>> {
    let mut pids = Vec::new();
    let v4 = tcp_table(AF_INET)?;
    // SAFETY：表由 GetExtendedTcpTable 以 MIB_TCPTABLE_OWNER_PID 布局填充
    for row in unsafe { rows::<MIB_TCPROW_OWNER_PID>(&v4)? } {
        let l = (
            IpAddr::V4(Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
            port(row.dwLocalPort),
        );
        let r = (
            IpAddr::V4(Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes())),
            port(row.dwRemotePort),
        );
        if row.dwOwningPid != IDLE_PID && is_peer_socket(l, r, local, remote) {
            pids.push(pid_i32(row.dwOwningPid)?);
        }
    }
    let v6 = tcp_table(AF_INET6)?;
    // SAFETY：同上，MIB_TCP6TABLE_OWNER_PID 布局
    for row in unsafe { rows::<MIB_TCP6ROW_OWNER_PID>(&v6)? } {
        let l = (
            IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
            port(row.dwLocalPort),
        );
        let r = (
            IpAddr::V6(Ipv6Addr::from(row.ucRemoteAddr)),
            port(row.dwRemotePort),
        );
        if row.dwOwningPid != IDLE_PID && is_peer_socket(l, r, local, remote) {
            pids.push(pid_i32(row.dwOwningPid)?);
        }
    }
    Ok(pids)
}

fn pid_i32(pid: u32) -> anyhow::Result<i32> {
    i32::try_from(pid).map_err(|_| anyhow!("连接所有者 PID {pid} 超出范围"))
}

/// 表中端口为网络字节序，存放在低 16 位。
fn port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// 读取一张 TCP 表（`TCP_TABLE_OWNER_PID_ALL`）；以 `u32` 缓冲保证行结构 4 字节对齐。
fn tcp_table(af: u32) -> anyhow::Result<Vec<u32>> {
    let mut need: u32 = 4096;
    // 表在两次调用之间可能增长，有限次重试
    for _ in 0..8 {
        let mut buf = vec![0u32; (need as usize).div_ceil(4)];
        let mut len = u32::try_from(buf.len() * 4).context("TCP 表过大")?;
        // SAFETY：缓冲区长度与 len 一致，由系统按表布局填充
        let ret = unsafe {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut len,
                false,
                af,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if ret == NO_ERROR.0 {
            return Ok(buf);
        }
        if ret != ERROR_INSUFFICIENT_BUFFER.0 {
            bail!("GetExtendedTcpTable(af={af}) 失败：{ret}");
        }
        need = len.saturating_add(1024);
    }
    bail!("GetExtendedTcpTable(af={af}) 多次重试仍缓冲不足")
}

/// 把表缓冲解释为 `dwNumEntries` + 行数组（行从偏移 4 字节起）。
///
/// # Safety
/// `buf` 必须是 `GetExtendedTcpTable` 以 `T` 行布局成功填充的缓冲区。
unsafe fn rows<T: Copy>(buf: &[u32]) -> anyhow::Result<&[T]> {
    let n = *buf.first().ok_or_else(|| anyhow!("TCP 表为空缓冲"))? as usize;
    let avail = (buf.len() - 1) * 4;
    if n.checked_mul(size_of::<T>())
        .is_none_or(|need| need > avail)
    {
        bail!("TCP 表行数 {n} 超出缓冲区");
    }
    // SAFETY：上面已校验边界；T 的对齐为 4，buf[1..] 起点满足对齐
    Ok(unsafe { std::slice::from_raw_parts(buf.as_ptr().add(1).cast::<T>(), n) })
}

/// 进程创建时间（Unix 毫秒）。
fn creation_time_ms(process: &Handle) -> anyhow::Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY：四个输出参数均为有效指针
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
        .context("读取进程创建时间失败")?;
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let ms = ticks
        .checked_sub(FILETIME_UNIX_EPOCH)
        .map(|t| t / 10_000)
        .filter(|&ms| ms > 0)
        .ok_or_else(|| anyhow!("进程创建时间无效"))?;
    Ok(ms)
}

/// 进程映像的 Win32 完整路径字符串（内核记录的路径；磁盘上该路径的文件可能已被替换，
/// 见 [`super::PeerInfo::image_path`]）。
fn image_path(process: &Handle) -> anyhow::Result<String> {
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY：缓冲区长度与 len 一致
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .context("读取进程映像路径失败")?;
    let path = String::from_utf16(&buf[..len as usize]).context("进程映像路径不是有效 UTF-16")?;
    if path.is_empty() {
        bail!("进程映像路径为空");
    }
    Ok(path)
}

/// 进程 token 的用户 SID 字符串（如 `S-1-5-21-…`）。
fn user_sid(process: &Handle) -> anyhow::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY：输出句柄由下方 Handle 关闭
    unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }
        .context("打开进程 token 失败")?;
    let token = Handle(token, true);

    let mut len = 0u32;
    // 第一次调用只取所需长度，必然以缓冲不足失败
    // SAFETY：空缓冲 + 有效的长度输出指针
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
    if len == 0 {
        bail!("读取 token 用户信息长度失败");
    }
    // u64 缓冲保证 TOKEN_USER 指针对齐
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY：缓冲区至少 len 字节
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .context("读取 token 用户信息失败")?;
    // SAFETY：系统已按 TOKEN_USER 布局填充，Sid 指向同一缓冲区内
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    let mut text = PWSTR::null();
    // SAFETY：sid 有效；输出字符串由 LocalFree 释放
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.context("SID 转字符串失败")?;
    // SAFETY：text 是系统分配的以 NUL 结尾的宽字符串
    let result = unsafe { text.to_string() }.context("SID 字符串不是有效 UTF-16");
    // SAFETY：释放 ConvertSidToStringSidW 分配的内存，只释放一次
    let _ = unsafe { LocalFree(HLOCAL(text.0.cast())) };
    let sid = result?;
    if sid.is_empty() {
        bail!("SID 字符串为空");
    }
    Ok(sid)
}
