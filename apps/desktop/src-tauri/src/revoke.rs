//! 托盘“撤销配置并退出”（设计 §6，仅 macOS 菜单提供）。
//!
//! 流程：确认 → `Manager::revoke_managed_config`（停代理、还原 config.toml、移除 .env 受管块）
//! → 关闭开机自启（系统 LaunchAgent + 状态文件）→ 停止代理并退出。任一步失败都不退出，
//! 弹出带错误码的提示，用户可再次选择（撤销是幂等的）。
//!
//! 确认框不用 tauri-plugin-dialog：锁定版本会把“取消 / 失败 / 未知响应”映射成第二个按钮，
//! 无法与点击区分，也不能把默认按钮设为“取消”。这里改用 `/usr/bin/osascript` 的
//! `display dialog`：“取消”同时是默认按钮和 cancel button，回车、Esc、⌘.、点“取消”都以
//! 退出码 1 结束；只有 stdout 恰好是确认标记时才执行撤销。文案经 `argv` 传入，不拼进脚本。

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::process::{Command, Stdio};

use helper_core::log;
use helper_core::types::ErrorPayload;
use serde_json::json;
use tauri::{AppHandle, Manager as _};
use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};

use crate::commands::{self, AppState};
use crate::events;
use crate::lifecycle::{self, Lifecycle};

/// 确认框标题（也是托盘菜单项文案）。
pub const REVOKE_TITLE: &str = "撤销配置并退出";
/// 确认框正文（设计 §6，逐字）。
pub const REVOKE_MESSAGE: &str = "将还原 Codex 的 config.toml、移除 .env 受管块、关闭开机自启，然后退出。API Key 保留在钥匙串。删除 CodexHelper 前请先做这一步；完成后需要重启 Codex。";
/// 确认按钮。
pub const REVOKE_CONFIRM_LABEL: &str = "撤销并退出";
/// 取消按钮（默认按钮，也是 cancel button）。
pub const REVOKE_CANCEL_LABEL: &str = "取消";

/// 系统自带的 AppleScript 解释器。
const OSASCRIPT_TOOL: &str = "/usr/bin/osascript";

/// 确认框脚本，每行一个 `-e`。`argv` 依次为正文、标题、取消标签、确认标签。
/// 600 秒无人操作视为取消，避免对话框被遮住时退出互斥一直被占着。
const REVOKE_DIALOG_SCRIPT: [&str; 7] = [
    "on run argv",
    "activate",
    "set r to display dialog (item 1 of argv) with title (item 2 of argv) buttons {item 3 of argv, item 4 of argv} default button (item 3 of argv) cancel button (item 3 of argv) with icon caution giving up after 600",
    "if gave up of r then return \"revoke-cancelled\"",
    "if button returned of r is (item 4 of argv) then return \"revoke-confirmed\"",
    "return \"revoke-cancelled\"",
    "end run",
];

/// 脚本在用户点击确认按钮时输出的唯一标记。
const REVOKE_CONFIRMED_TOKEN: &str = "revoke-confirmed";

/// osascript 参数：每行脚本前加 `-e`，之后依次追加正文、标题、取消标签、确认标签。
pub fn revoke_dialog_args() -> Vec<String> {
    let mut args: Vec<String> = REVOKE_DIALOG_SCRIPT
        .iter()
        .flat_map(|line| ["-e".to_string(), (*line).to_string()])
        .collect();
    args.extend(
        [
            REVOKE_MESSAGE,
            REVOKE_TITLE,
            REVOKE_CANCEL_LABEL,
            REVOKE_CONFIRM_LABEL,
        ]
        .map(str::to_string),
    );
    args
}

/// 只有退出码为 0 且 stdout 恰好是确认标记（可带一个末尾换行）时才算确认；其余一律视为取消。
pub fn revoke_confirmed(code: Option<i32>, stdout: &[u8]) -> bool {
    if code != Some(0) {
        return false;
    }
    let out = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    out == REVOKE_CONFIRMED_TOKEN.as_bytes()
}

/// 撤销失败时的提示正文：说明没有退出、需要重试，并附错误码。
pub fn revoke_failure_message(error: &ErrorPayload) -> String {
    format!(
        "撤销未完成，CodexHelper 没有退出。请处理后再次选择「撤销配置并退出」，或到主窗口处理。\n\n错误码：{}\n{}",
        error.code, error.message
    )
}

/// 撤销流程的结果。
#[derive(Debug, PartialEq)]
pub enum RevokeOutcome {
    /// 用户没有确认（含超时、对话框无法显示）
    Cancelled,
    /// 某一步失败；`step` 为 `"revoke"` 或 `"autostart"`
    Failed {
        step: &'static str,
        error: ErrorPayload,
    },
    /// 全部完成，可以退出
    Exit,
}

/// 依次执行确认、撤销、关闭开机自启。任一步不成立即停止，后续步骤不调用。不碰 `AppHandle`，便于测试。
pub async fn run_revoke_steps(
    confirm: impl AsyncFnOnce() -> bool,
    revoke: impl AsyncFnOnce() -> Result<(), ErrorPayload>,
    disable_autostart: impl AsyncFnOnce() -> Result<(), ErrorPayload>,
) -> RevokeOutcome {
    if !confirm().await {
        return RevokeOutcome::Cancelled;
    }
    if let Err(error) = revoke().await {
        return RevokeOutcome::Failed {
            step: "revoke",
            error,
        };
    }
    if let Err(error) = disable_autostart().await {
        return RevokeOutcome::Failed {
            step: "autostart",
            error,
        };
    }
    RevokeOutcome::Exit
}

/// 弹出 osascript 确认框（阻塞操作放在 `spawn_blocking`）。无法启动或任务失败都视为取消。
async fn ask_revoke_confirmation() -> bool {
    let output = tauri::async_runtime::spawn_blocking(|| {
        Command::new(OSASCRIPT_TOOL)
            .args(revoke_dialog_args())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
    })
    .await;
    match output {
        Ok(Ok(output)) => revoke_confirmed(output.status.code(), &output.stdout),
        _ => false,
    }
}

/// 托盘“撤销配置并退出”：与“退出”共用互斥；确认后撤销、关闭开机自启并退出。
pub fn request_revoke(app: &AppHandle) {
    let Some(lifecycle) = app.try_state::<Lifecycle>() else {
        return;
    };
    if !lifecycle.try_begin_exit() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let manager = app.state::<AppState>().manager.clone();
        let outcome = run_revoke_steps(
            ask_revoke_confirmation,
            async || {
                let status = manager
                    .revoke_managed_config()
                    .await
                    .map_err(ErrorPayload::from)?;
                events::publish_status(&app, &status);
                Ok(())
            },
            async || {
                commands::apply_autostart_checked(false, true, |on| {
                    commands::apply_system_autostart(&app, on)
                })?;
                manager
                    .set_autostart_flag(false)
                    .await
                    .map(|_| ())
                    .map_err(ErrorPayload::from)
            },
        )
        .await;
        match outcome {
            RevokeOutcome::Cancelled => {
                log::event("desktop.revoke", json!({ "confirmed": false }));
                end_attempt(&app);
            }
            RevokeOutcome::Failed { step, error } => {
                let status = manager.status().await;
                events::publish_status(&app, &status);
                events::publish_operation_failed(&app, &error);
                log::event(
                    "desktop.revoke",
                    json!({ "confirmed": true, "ok": false, "code": error.code, "step": step }),
                );
                app.dialog()
                    .message(revoke_failure_message(&error))
                    .title(REVOKE_TITLE)
                    .kind(MessageDialogKind::Error)
                    .buttons(MessageDialogButtons::OkCustom("知道了".to_string()))
                    .show(|_| {});
                end_attempt(&app);
            }
            RevokeOutcome::Exit => {
                log::event("desktop.revoke", json!({ "confirmed": true, "ok": true }));
                lifecycle::exit_after_shutdown(&app).await;
            }
        }
    });
}

/// 撤销未退出而结束时释放退出互斥。
fn end_attempt(app: &AppHandle) {
    if let Some(lifecycle) = app.try_state::<Lifecycle>() {
        lifecycle.end_exit_attempt();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn revoke_confirmed_only_for_confirm_token() {
        assert!(revoke_confirmed(Some(0), b"revoke-confirmed\n"));
        assert!(revoke_confirmed(Some(0), b"revoke-confirmed"));
        let cases: &[(Option<i32>, &[u8])] = &[
            // 点“取消”、回车、Esc、⌘.：都命中 cancel button，osascript 报 -128
            (Some(1), b""),
            // 600 秒超时
            (Some(0), b"revoke-cancelled\n"),
            (Some(0), b""),
            (Some(0), b"revoke-confirmed\n\n"),
            (Some(0), b" revoke-confirmed\n"),
            (Some(0), b"Revoke-Confirmed\n"),
            (Some(0), "button returned:撤销并退出\n".as_bytes()),
            (Some(1), b"revoke-confirmed\n"),
            (Some(2), b"revoke-confirmed\n"),
            (None, b"revoke-confirmed\n"),
        ];
        for &(code, out) in cases {
            assert!(
                !revoke_confirmed(code, out),
                "{code:?} {:?}",
                String::from_utf8_lossy(out)
            );
        }
    }

    #[test]
    fn revoke_dialog_args_bind_return_and_esc_to_cancel() {
        let args = revoke_dialog_args();
        let (script, tail) = args.split_at(args.len() - 4);
        assert_eq!(tail, [REVOKE_MESSAGE, REVOKE_TITLE, "取消", "撤销并退出"]);
        assert!(script.chunks(2).all(|p| p.len() == 2 && p[0] == "-e"));
        let dialog = script
            .iter()
            .find(|l| l.contains("display dialog"))
            .expect("缺 display dialog");
        for needle in [
            "buttons {item 3 of argv, item 4 of argv}",
            "default button (item 3 of argv)",
            "cancel button (item 3 of argv)",
            "giving up after",
        ] {
            assert!(dialog.contains(needle), "{needle}");
        }
        // 文案只经 argv 传入
        assert!(
            !script
                .iter()
                .any(|l| l.contains("撤销") || l.contains("取消"))
        );
        let confirm_line = script
            .iter()
            .find(|l| l.contains("revoke-confirmed"))
            .unwrap();
        assert!(confirm_line.contains("button returned of r is (item 4 of argv)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn revoke_dialog_script_compiles() {
        // 只编译不运行，不显示任何界面
        let out =
            std::env::temp_dir().join(format!("codex-helper-revoke-{}.scpt", std::process::id()));
        let mut args = vec!["-o".to_string(), out.display().to_string()];
        args.extend(
            REVOKE_DIALOG_SCRIPT
                .iter()
                .flat_map(|l| ["-e".to_string(), l.to_string()]),
        );
        let status = Command::new("/usr/bin/osacompile")
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(status.success());
    }

    fn payload(code: &str) -> ErrorPayload {
        ErrorPayload {
            code: code.to_string(),
            message: format!("{code} 失败"),
        }
    }

    #[test]
    fn revoke_steps_cancelled_runs_nothing_else() {
        let calls = RefCell::new(Vec::new());
        let outcome = tauri::async_runtime::block_on(run_revoke_steps(
            async || {
                calls.borrow_mut().push("confirm");
                false
            },
            async || {
                calls.borrow_mut().push("revoke");
                Ok(())
            },
            async || {
                calls.borrow_mut().push("autostart");
                Ok(())
            },
        ));
        assert_eq!(outcome, RevokeOutcome::Cancelled);
        assert_eq!(*calls.borrow(), ["confirm"]);
    }

    #[test]
    fn revoke_steps_stop_at_revoke_failure() {
        let calls = RefCell::new(Vec::new());
        let outcome = tauri::async_runtime::block_on(run_revoke_steps(
            async || {
                calls.borrow_mut().push("confirm");
                true
            },
            async || {
                calls.borrow_mut().push("revoke");
                Err(payload("io"))
            },
            async || {
                calls.borrow_mut().push("autostart");
                Ok(())
            },
        ));
        assert_eq!(
            outcome,
            RevokeOutcome::Failed {
                step: "revoke",
                error: payload("io")
            }
        );
        assert_eq!(*calls.borrow(), ["confirm", "revoke"]);
    }

    #[test]
    fn revoke_steps_report_autostart_failure() {
        let calls = RefCell::new(Vec::new());
        let outcome = tauri::async_runtime::block_on(run_revoke_steps(
            async || {
                calls.borrow_mut().push("confirm");
                true
            },
            async || {
                calls.borrow_mut().push("revoke");
                Ok(())
            },
            async || {
                calls.borrow_mut().push("autostart");
                Err(payload("internal"))
            },
        ));
        assert_eq!(
            outcome,
            RevokeOutcome::Failed {
                step: "autostart",
                error: payload("internal")
            }
        );
        assert_eq!(*calls.borrow(), ["confirm", "revoke", "autostart"]);
    }

    #[test]
    fn revoke_steps_exit_after_all_succeed() {
        let calls = RefCell::new(Vec::new());
        let outcome = tauri::async_runtime::block_on(run_revoke_steps(
            async || {
                calls.borrow_mut().push("confirm");
                true
            },
            async || {
                calls.borrow_mut().push("revoke");
                Ok(())
            },
            async || {
                calls.borrow_mut().push("autostart");
                Ok(())
            },
        ));
        assert_eq!(outcome, RevokeOutcome::Exit);
        assert_eq!(*calls.borrow(), ["confirm", "revoke", "autostart"]);
    }

    #[test]
    fn revoke_dialog_copy_is_fixed() {
        assert_eq!(REVOKE_TITLE, "撤销配置并退出");
        assert_eq!(
            (REVOKE_CANCEL_LABEL, REVOKE_CONFIRM_LABEL),
            ("取消", "撤销并退出")
        );
        assert_eq!(
            REVOKE_MESSAGE,
            "将还原 Codex 的 config.toml、移除 .env 受管块、关闭开机自启，然后退出。API Key 保留在钥匙串。删除 CodexHelper 前请先做这一步；完成后需要重启 Codex。"
        );
    }

    #[test]
    fn revoke_failure_message_shows_code_and_retry_hint() {
        let e = ErrorPayload {
            code: "io".into(),
            message: "写入 .env 失败".into(),
        };
        assert_eq!(
            revoke_failure_message(&e),
            "撤销未完成，CodexHelper 没有退出。请处理后再次选择「撤销配置并退出」，或到主窗口处理。\n\n错误码：io\n写入 .env 失败"
        );
    }
}
