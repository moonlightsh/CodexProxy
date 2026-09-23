//! Codex 引擎进程检测（设计 §7“工具退出而 Codex 仍在运行”）。
//!
//! Windows 用 ToolHelp32 进程快照枚举进程名；其他平台为开发模式，不检测。

use helper_core::consts;

/// 进程名是否为目标进程。Windows 文件名不区分大小写，按 ASCII 忽略大小写比较。
// 非 Windows 开发模式不枚举进程，只有单元测试使用。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn matches_process_name(candidate: &str, target: &str) -> bool {
    candidate.eq_ignore_ascii_case(target)
}

/// Codex 引擎进程（`codex.exe`）是否正在运行。
///
/// 快照失败时无法判断，按“正在运行”处理：宁可多弹一次确认，也不让用户在不知情时切断 Codex。
#[cfg(windows)]
pub fn is_codex_running() -> bool {
    match windows_impl::any_process_named(consts::CODEX_PROCESS_NAME) {
        Ok(found) => found,
        Err(error) => {
            helper_core::log::event(
                "desktop.process_snapshot",
                serde_json::json!({ "ok": false, "error": error.to_string() }),
            );
            true
        }
    }
}

/// 非 Windows 开发模式：Codex 桌面版的引擎进程名只在 Windows 上确定，这里一律返回 `false`，
/// 退出时不弹确认。
#[cfg(not(windows))]
pub fn is_codex_running() -> bool {
    let _ = consts::CODEX_PROCESS_NAME;
    false
}

#[cfg(windows)]
mod windows_impl {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    use super::matches_process_name;

    /// 枚举进程快照，判断是否存在指定进程名。
    pub fn any_process_named(target: &str) -> windows::core::Result<bool> {
        // SAFETY：快照句柄在本函数内创建并关闭；PROCESSENTRY32W 按要求先设置 dwSize。
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = false;
            let mut next = Process32FirstW(snapshot, &mut entry);
            while next.is_ok() {
                if matches_process_name(&exe_name(&entry.szExeFile), target) {
                    found = true;
                    break;
                }
                next = Process32NextW(snapshot, &mut entry);
            }
            let _ = CloseHandle(snapshot);
            Ok(found)
        }
    }

    /// 以 NUL 结尾的 UTF-16 文件名。
    fn exe_name(raw: &[u16]) -> String {
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        String::from_utf16_lossy(&raw[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_name_match_ignores_ascii_case() {
        assert!(matches_process_name(
            "codex.exe",
            consts::CODEX_PROCESS_NAME
        ));
        assert!(matches_process_name(
            "Codex.EXE",
            consts::CODEX_PROCESS_NAME
        ));
        assert!(matches_process_name("CODEX.EXE", "codex.exe"));
    }

    #[test]
    fn process_name_match_requires_exact_name() {
        assert!(!matches_process_name("codex", "codex.exe"));
        assert!(!matches_process_name("codex.exe.bak", "codex.exe"));
        assert!(!matches_process_name("mycodex.exe", "codex.exe"));
        assert!(!matches_process_name("codex-helper.exe", "codex.exe"));
        assert!(!matches_process_name("", "codex.exe"));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_never_reports_codex_running() {
        assert!(!is_codex_running());
    }
}
