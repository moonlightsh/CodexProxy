//! 前端可调用的命令（接口契约，与 `apps/desktop/src/api.ts` 一一对应）。
//!
//! 所有命令返回 `Result<_, ErrorPayload>`，错误为 `{ code, message }`。
//!
//! 事件（Rust → 前端）：
//! - `status-changed`：payload 为 `Status`，任何状态变化后推送；
//! - `connection-recorded`：payload 为 `ConnectionRecord`，每条代理连接结束后推送；
//! - `key-required`：payload 为空，启动对账发现已启用但凭据缺失时推送（前端聚焦 Key 录入）。

// 阶段 3 任务 3.1 接入事件推送后移除本行。
#![allow(dead_code)]

use std::sync::Arc;

use helper_core::manager::Manager;
use helper_core::types::{ConnectionRecord, EnableRequest, ErrorPayload, SaveKeyRequest, Status};

/// 事件名：状态变化。
pub const EVENT_STATUS_CHANGED: &str = "status-changed";
/// 事件名：新的连接记录。
pub const EVENT_CONNECTION_RECORDED: &str = "connection-recorded";
/// 事件名：需要录入 Key。
pub const EVENT_KEY_REQUIRED: &str = "key-required";

/// 托管给 Tauri 的共享状态。
pub struct AppState {
    pub manager: Arc<Manager>,
}

/// 当前状态快照（不做网络探测）。
#[tauri::command]
pub async fn get_status(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 立即重测网关与 SOCKS5 可达性并返回最新状态。
#[tauri::command]
pub async fn refresh_status(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 启用受管模式（首次录入 Key 或打开开关）。
#[tauri::command]
pub async fn enable(
    state: tauri::State<'_, AppState>,
    request: EnableRequest,
) -> Result<Status, ErrorPayload> {
    let _ = (&state.manager, request);
    todo!("阶段 3 任务 3.1")
}

/// 停用受管模式。
#[tauri::command]
pub async fn disable(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 重新录入 Key（只校验并保存，不改变启用状态）。
#[tauri::command]
pub async fn save_key(
    state: tauri::State<'_, AppState>,
    request: SaveKeyRequest,
) -> Result<Status, ErrorPayload> {
    let _ = (&state.manager, request);
    todo!("阶段 3 任务 3.1")
}

/// 清除 Key。
#[tauri::command]
pub async fn clear_key(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 停用状态下一键清理受管残留。
#[tauri::command]
pub async fn cleanup_residue(state: tauri::State<'_, AppState>) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 一键移除 `.env` 中的 Codex++ 旧块。
#[tauri::command]
pub async fn remove_legacy_env_block(
    state: tauri::State<'_, AppState>,
) -> Result<Status, ErrorPayload> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}

/// 开关开机自启（写当前用户 Run 注册表项，带 `--minimized`），并记录到状态文件。
#[tauri::command]
pub async fn set_autostart(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<Status, ErrorPayload> {
    let _ = (app, &state.manager, enabled);
    todo!("阶段 3 任务 3.1")
}

/// 最近连接（最新在前，最多 50 条）。
#[tauri::command]
pub fn recent_connections(state: tauri::State<'_, AppState>) -> Vec<ConnectionRecord> {
    let _ = &state.manager;
    todo!("阶段 3 任务 3.1")
}
