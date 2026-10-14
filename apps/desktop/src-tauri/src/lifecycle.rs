//! 窗口关闭与进程退出（设计 §7、§8）。
//!
//! - 关闭按钮只隐藏到托盘；托盘“退出”才真正结束进程。
//! - 退出前若受管模式已启用且 Codex 引擎在运行，弹窗确认：退出后 Codex 请求将失败（fail-closed）。
//! - 任何退出路径都尽力停止本进程的代理（不改配置）。
//! - 托盘“退出”与“撤销配置并退出”（见 [`crate::revoke`]）共用一把互斥，任一流程进行中时，
//!   另一个点击直接忽略。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use helper_core::log;
use serde_json::json;
use tauri::{AppHandle, Manager as _, RunEvent, Window, WindowEvent};
use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};

use crate::commands::AppState;
use crate::events;
use crate::process;

/// 退出时等待代理停止的上限（编排锁可能被最长 15 秒的 Key 校验占用，不无限等待）。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 退出流程的共享状态。
#[derive(Default)]
pub struct Lifecycle {
    /// 托盘“退出”或“撤销配置并退出”流程进行中（含确认对话框），防止重复弹窗
    quitting: AtomicBool,
    /// 已执行过代理停止
    shut_down: AtomicBool,
}

impl Lifecycle {
    /// 开始一次退出类流程；已有流程进行中时返回 `false`，调用方应直接忽略本次点击。
    pub fn try_begin_exit(&self) -> bool {
        !self.quitting.swap(true, Ordering::SeqCst)
    }

    /// 退出类流程未退出而结束（取消或失败），释放互斥。
    pub fn end_exit_attempt(&self) {
        self.quitting.store(false, Ordering::SeqCst);
    }
}

/// 是否需要退出确认：只有受管模式已启用且 Codex 在运行时，退出才会让 Codex 请求失败。
pub fn should_confirm_quit(enabled: bool, codex_running: bool) -> bool {
    enabled && codex_running
}

/// 主窗口关闭按钮：阻止关闭并隐藏到托盘。
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == events::MAIN_WINDOW
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// 事件循环回调：任何退出路径都尽力停止代理。
pub fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            // 事件循环回调运行在主线程、不在异步运行时内，可以阻塞等待。
            tauri::async_runtime::block_on(shutdown_once(app));
        }
        // macOS 开发模式：窗口隐藏后点击程序坞图标重新显示。
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => events::show_main_window(app),
        _ => {}
    }
}

/// 托盘“退出”：必要时确认，然后停止代理并结束进程。
pub fn request_quit(app: &AppHandle) {
    let Some(lifecycle) = app.try_state::<Lifecycle>() else {
        return;
    };
    if !lifecycle.try_begin_exit() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if confirm_quit_if_needed(&app).await {
            log::event("desktop.quit", json!({ "confirmed": true }));
            exit_after_shutdown(&app).await;
        } else {
            log::event("desktop.quit", json!({ "confirmed": false }));
            if let Some(lifecycle) = app.try_state::<Lifecycle>() {
                lifecycle.end_exit_attempt();
            }
        }
    });
}

/// 停止代理后结束进程。不弹 Codex 运行确认（调用方已确认过）。
pub async fn exit_after_shutdown(app: &AppHandle) {
    shutdown_once(app).await;
    app.exit(0);
}

/// 返回是否继续退出。
async fn confirm_quit_if_needed(app: &AppHandle) -> bool {
    let enabled = app.state::<AppState>().manager.status().await.enabled;
    if !enabled {
        return true;
    }
    let codex_running = tauri::async_runtime::spawn_blocking(process::is_codex_running)
        .await
        .unwrap_or(true);
    if !should_confirm_quit(enabled, codex_running) {
        return true;
    }
    ask_quit_confirmation(app).await
}

/// 弹出退出确认对话框，返回用户是否确认退出。
async fn ask_quit_confirmation(app: &AppHandle) -> bool {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(
            "检测到 Codex 正在运行。退出后 Codex 的请求将失败，直到重新打开本工具。\n\n确定要退出 CodexHelper 吗？",
        )
        .title("退出 CodexHelper")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "退出".to_string(),
            "取消".to_string(),
        ))
        .show(move |confirmed| {
            let _ = sender.send(confirmed);
        });
    receiver.await.unwrap_or(false)
}

/// 停止本进程代理（只执行一次，带超时）。
async fn shutdown_once(app: &AppHandle) {
    if let Some(lifecycle) = app.try_state::<Lifecycle>()
        && lifecycle.shut_down.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let manager = state.manager.clone();
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, manager.shutdown())
        .await
        .is_err()
    {
        log::event("desktop.shutdown", json!({ "timeout": true }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_confirmation_only_when_enabled_and_codex_running() {
        assert!(should_confirm_quit(true, true));
        assert!(!should_confirm_quit(true, false));
        assert!(!should_confirm_quit(false, true));
        assert!(!should_confirm_quit(false, false));
    }

    #[test]
    fn exit_attempt_is_exclusive_until_ended() {
        let l = Lifecycle::default();
        assert!(l.try_begin_exit());
        assert!(!l.try_begin_exit());
        l.end_exit_attempt();
        assert!(l.try_begin_exit());
    }
}
