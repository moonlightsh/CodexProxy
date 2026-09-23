//! 事件推送与主窗口辅助。
//!
//! 事件推送失败（窗口尚未加载、已销毁等）一律忽略：状态总能通过 `get_status` 重新拉取。

use std::sync::Arc;

use helper_core::manager::Manager;
use helper_core::types::{ErrorPayload, Status};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager as _};

use crate::commands::{EVENT_KEY_REQUIRED, EVENT_OPERATION_FAILED, EVENT_STATUS_CHANGED};
use crate::tray;

/// 主窗口标签（与 `tauri.conf.json` 一致）。
pub const MAIN_WINDOW: &str = "main";

/// 向前端推送事件，失败忽略。
pub fn emit<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: P) {
    let _ = app.emit(event, payload);
}

/// 推送 `status-changed`，并把托盘“受管模式”勾选同步为实际状态。
pub fn publish_status(app: &AppHandle, status: &Status) {
    tray::sync_managed_check(app, status.enabled);
    emit(app, EVENT_STATUS_CHANGED, status);
}

/// 推送非前端发起的操作失败；凭据缺失时额外推送 `key-required`。
pub fn publish_operation_failed(app: &AppHandle, error: &ErrorPayload) {
    emit(app, EVENT_OPERATION_FAILED, error);
    if error.code == "credentialMissing" {
        emit(app, EVENT_KEY_REQUIRED, ());
    }
}

/// 显示并聚焦主窗口（从托盘或最小化中恢复）。
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 异步检测网关与 SOCKS5 可达性并推送最新状态。
pub async fn probe_and_publish(app: &AppHandle, manager: &Manager) {
    let status = manager.probe_reachability().await;
    publish_status(app, &status);
}

/// 在后台执行 [`probe_and_publish`]（启用成功后使用，不阻塞调用方）。
pub fn spawn_probe(app: AppHandle, manager: Arc<Manager>) {
    tauri::async_runtime::spawn(async move {
        probe_and_publish(&app, &manager).await;
    });
}
