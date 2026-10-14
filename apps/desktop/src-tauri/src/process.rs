//! Codex 引擎进程检测（设计 §7“工具退出而 Codex 仍在运行”）。
//!
//! - Windows：用 ToolHelp32 进程快照枚举进程名（`codex.exe`，不区分大小写）。
//! - macOS：执行 `/usr/bin/pgrep -x codex`，按退出码判断。
//! - 其他平台：开发模式，不检测，一律视为未运行。

use helper_core::consts;

/// macOS 进程查询工具的绝对路径（系统自带）。
#[cfg(target_os = "macos")]
const PGREP_TOOL: &str = "/usr/bin/pgrep";

/// 进程名是否为目标进程。Windows 文件名不区分大小写，按 ASCII 忽略大小写比较。
// 非 Windows 不枚举进程，只有单元测试使用。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn matches_process_name(candidate: &str, target: &str) -> bool {
    candidate.eq_ignore_ascii_case(target)
}

/// 由 `pgrep` 退出码判断进程是否在运行：`1` 表示没有匹配进程；`0`、其他退出码
/// 或被信号终止（`None`）都按“正在运行”处理，宁可多弹一次确认。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn running_from_pgrep_exit(code: Option<i32>) -> bool {
    code != Some(1)
}

/// 用 `pgrep -x <name>` 判断是否存在进程名恰好为 `name` 的进程。
///
/// 无法启动 `pgrep` 时记日志并按“正在运行”处理。不设超时：调用方已在
/// `spawn_blocking` 里执行，pgrep 只读进程表。
#[cfg(target_os = "macos")]
fn process_running(name: &str) -> bool {
    use std::process::{Command, Stdio};

    match Command::new(PGREP_TOOL)
        .arg("-x")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => running_from_pgrep_exit(status.code()),
        Err(error) => {
            helper_core::log::event(
                "desktop.process_snapshot",
                serde_json::json!({ "ok": false, "error": error.to_string() }),
            );
            true
        }
    }
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

/// Codex 引擎进程（`codex`）是否正在运行。无法判断时按“正在运行”处理。
#[cfg(target_os = "macos")]
pub fn is_codex_running() -> bool {
    process_running(consts::CODEX_PROCESS_NAME)
}

/// 其他平台为开发模式：不检测 Codex 进程，一律返回 `false`，退出时不弹确认。
#[cfg(not(any(windows, target_os = "macos")))]
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
        assert!(matches_process_name("codex.exe", "codex.exe"));
        assert!(matches_process_name("Codex.EXE", "codex.exe"));
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

    #[test]
    fn codex_process_name_per_platform() {
        assert_eq!(
            consts::CODEX_PROCESS_NAME,
            if cfg!(windows) { "codex.exe" } else { "codex" }
        );
    }

    #[test]
    fn pgrep_exit_code_mapping() {
        assert!(running_from_pgrep_exit(Some(0)));
        assert!(!running_from_pgrep_exit(Some(1)));
        for code in [Some(2), Some(3), Some(-1), None] {
            assert!(running_from_pgrep_exit(code), "{code:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pgrep_reports_absent_process_as_not_running() {
        // 名字不超过 15 字节，避免进程名被截断。
        assert!(!process_running(&format!(
            "chx-none-{}",
            std::process::id()
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pgrep_finds_running_child() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let found = process_running("sleep");
        let _ = child.kill();
        let _ = child.wait();
        assert!(found);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn non_windows_never_reports_codex_running() {
        assert!(!is_codex_running());
    }
}
