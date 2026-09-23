//! 前端可调用的命令（接口契约，与 `apps/desktop/src/api.ts` 一一对应）。
//!
//! 所有命令返回 `Result<_, ErrorPayload>`，错误为 `{ code, message }`。
//!
//! 事件（Rust → 前端）：
//! - `status-changed`：payload 为 `Status`，任何状态变化后推送；
//! - `connection-recorded`：payload 为 `ConnectionRecord`，每条代理连接结束后推送；
//! - `key-required`：payload 为空，启动对账发现已启用但凭据缺失时推送（前端聚焦 Key 录入）；
//! - `reconcile-finished`：payload 为 `ReconcileReport`，启动对账完成后推送一次；
//! - `operation-failed`：payload 为 `ErrorPayload`，非前端发起的操作（如托盘开关）失败时推送。

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use helper_core::log;
use helper_core::manager::Manager;
use helper_core::types::{
    ConnectionRecord, EnableRequest, ErrorPayload, ManagerError, ReconcileReport, SaveKeyRequest,
    Status,
};
use serde_json::json;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt as _;

use crate::events;

/// 事件名：状态变化。
pub const EVENT_STATUS_CHANGED: &str = "status-changed";
/// 事件名：新的连接记录。
pub const EVENT_CONNECTION_RECORDED: &str = "connection-recorded";
/// 事件名：需要录入 Key。
pub const EVENT_KEY_REQUIRED: &str = "key-required";
/// 事件名：启动对账完成。
pub const EVENT_RECONCILE_FINISHED: &str = "reconcile-finished";
/// 事件名：非前端发起的操作失败。
pub const EVENT_OPERATION_FAILED: &str = "operation-failed";

/// 应用句柄的共享单元：`setup` 中写入一次，之后无锁读取。
///
/// 连接记录回调与命令都通过它推送事件（命令签名是契约，不能额外注入 `AppHandle`）。
pub type AppHandleCell = Arc<OnceLock<AppHandle>>;

/// 托管给 Tauri 的共享状态。
pub struct AppState {
    pub manager: Arc<Manager>,
    /// 应用句柄（`setup` 之后可用）
    pub handle: AppHandleCell,
    /// 最近一次启动对账的报告；对账完成前为 `None`
    pub reconcile_report: Mutex<Option<ReconcileReport>>,
}

impl AppState {
    pub fn new(manager: Arc<Manager>, handle: AppHandleCell) -> Self {
        Self {
            manager,
            handle,
            reconcile_report: Mutex::new(None),
        }
    }

    /// 保存启动对账报告（先保存再推送 `reconcile-finished`，保证前端不会两头落空）。
    pub fn store_reconcile_report(&self, report: ReconcileReport) {
        *self
            .reconcile_report
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(report);
    }

    fn reconcile_report(&self) -> Option<ReconcileReport> {
        self.reconcile_report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 推送最新状态（事件 + 托盘勾选同步）。句柄尚未就绪时忽略。
    fn publish(&self, status: &Status) {
        if let Some(app) = self.handle.get() {
            events::publish_status(app, status);
        }
    }

    /// 改变状态的操作收尾：成功推送返回的状态；失败时操作可能已部分生效（如停用尽力完成其余步骤），
    /// 同样推送实际状态，保证界面与托盘一致，再把错误交给前端。
    async fn finish(&self, result: Result<Status, ManagerError>) -> Result<Status, ErrorPayload> {
        match result {
            Ok(status) => {
                self.publish(&status);
                Ok(status)
            }
            Err(error) => {
                let status = self.manager.status().await;
                self.publish(&status);
                Err(error.into())
            }
        }
    }
}

/// 当前状态快照（不做网络探测）。
#[tauri::command]
pub async fn get_status(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    Ok(state.manager.status().await)
}

/// 立即重测网关与 SOCKS5 可达性并返回最新状态。
#[tauri::command]
pub async fn refresh_status(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let status = state.manager.probe_reachability().await;
    state.publish(&status);
    Ok(status)
}

/// 启用受管模式（首次录入 Key 或打开开关）。
#[tauri::command]
pub async fn enable(
    state: tauri::State<'_, AppState>,
    request: EnableRequest,
) -> Result<Status, ErrorPayload> {
    let result = state.manager.enable(request).await;
    let enabled = result.is_ok();
    let status = state.finish(result).await?;
    // 设计 §5.1 第 7 步：可达性检测不阻断启用，启用成功后异步探测再推送。
    if enabled && let Some(app) = state.handle.get() {
        events::spawn_probe(app.clone(), state.manager.clone());
    }
    Ok(status)
}

/// 停用受管模式。
#[tauri::command]
pub async fn disable(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let result = state.manager.disable().await;
    state.finish(result).await
}

/// 重新录入 Key（只校验并保存，不改变启用状态）。
#[tauri::command]
pub async fn save_key(
    state: tauri::State<'_, AppState>,
    request: SaveKeyRequest,
) -> Result<Status, ErrorPayload> {
    let result = state.manager.save_key(request).await;
    state.finish(result).await
}

/// 清除 Key。
#[tauri::command]
pub async fn clear_key(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let result = state.manager.clear_key().await;
    state.finish(result).await
}

/// 停用状态下一键清理受管残留。
#[tauri::command]
pub async fn cleanup_residue(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let result = state.manager.cleanup_residue().await;
    state.finish(result).await
}

/// 一键移除 `.env` 中的 Codex++ 旧块。
#[tauri::command]
pub async fn remove_legacy_env_block(
    state: tauri::State<'_, AppState>,
) -> Result<Status, ErrorPayload> {
    let result = state.manager.remove_legacy_env_block().await;
    state.finish(result).await
}

/// 开关开机自启（写当前用户 Run 注册表项，带 `--minimized`），并记录到状态文件。
#[tauri::command]
pub async fn set_autostart(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<Status, ErrorPayload> {
    if let Err(message) = apply_system_autostart(&app, enabled) {
        log::event(
            "desktop.set_autostart",
            json!({ "enabled": enabled, "ok": false }),
        );
        return Err(autostart_error(&message));
    }
    let result = state.manager.set_autostart_flag(enabled).await;
    state.finish(result).await
}

/// 最近连接（最新在前，最多 50 条）。
#[tauri::command]
pub fn recent_connections(state: tauri::State<'_, AppState>) -> Vec<ConnectionRecord> {
    state.manager.recent_connections()
}

/// 最近一次启动对账的报告；对账尚未完成时为 `None`（前端加载晚于 `reconcile-finished` 事件时用它补齐）。
#[tauri::command]
pub fn get_reconcile_report(state: tauri::State<'_, AppState>) -> Option<ReconcileReport> {
    state.reconcile_report()
}

/// 让系统自启项与期望一致。已一致时不动（Windows 删除不存在的注册表值会报错）。
fn apply_system_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().ok() == Some(enabled) {
        return Ok(());
    }
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|error| error.to_string())
}

/// 自启插件失败统一映射为 `internal`。
fn autostart_error(reason: &str) -> ErrorPayload {
    ErrorPayload {
        code: "internal".to_string(),
        message: format!("开机自启设置失败：{reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_error_uses_internal_code() {
        let payload = autostart_error("拒绝访问");
        assert_eq!(payload.code, "internal");
        assert_eq!(payload.message, "开机自启设置失败：拒绝访问");
    }
}
