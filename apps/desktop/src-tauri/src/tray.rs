//! 托盘（设计 §8）：显示窗口 / 受管模式开关 / 退出；左键单击显示窗口。

use std::sync::atomic::{AtomicBool, Ordering};

use helper_core::log;
use helper_core::types::{EnableRequest, ErrorPayload};
use serde_json::json;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager as _, Wry};

use crate::commands::AppState;
use crate::events;
use crate::lifecycle;

const TRAY_ID: &str = "main";
const TOOLTIP: &str = "CodexHelper";
const MENU_SHOW: &str = "show";
const MENU_MANAGED: &str = "managed";
const MENU_QUIT: &str = "quit";

/// 托盘的共享状态。
pub struct TrayState {
    /// “受管模式”勾选项，随 `status-changed` 同步
    managed: CheckMenuItem<Wry>,
    /// 托盘切换正在进行（防止连续点击排队执行相反的操作）
    toggling: AtomicBool,
}

/// 托盘切换受管模式时要执行的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToggleAction {
    Enable,
    Disable,
}

impl ToggleAction {
    /// 已启用 → 停用；未启用 → 用已保存的凭据启用。
    fn for_enabled(enabled: bool) -> Self {
        if enabled { Self::Disable } else { Self::Enable }
    }
}

/// 创建托盘图标与菜单。勾选初始为未启用，启动对账完成后由状态推送同步。
pub fn build(app: &App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示窗口", true, None::<&str>)?;
    let managed = CheckMenuItem::with_id(app, MENU_MANAGED, "受管模式", true, false, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &managed, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_icon_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;

    app.manage(TrayState {
        managed,
        toggling: AtomicBool::new(false),
    });
    Ok(())
}

/// 把“受管模式”勾选设为实际状态。托盘尚未创建时忽略。
pub fn sync_managed_check(app: &AppHandle, enabled: bool) {
    if let Some(tray) = app.try_state::<TrayState>() {
        let _ = tray.managed.set_checked(enabled);
    }
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => events::show_main_window(app),
        MENU_MANAGED => toggle_managed(app),
        MENU_QUIT => lifecycle::request_quit(app),
        _ => {}
    }
}

fn on_tray_icon_event(tray: &TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        events::show_main_window(tray.app_handle());
    }
}

/// 托盘切换受管模式。菜单点击时勾选已被系统翻转，无论成败最终都恢复为实际状态。
fn toggle_managed(app: &AppHandle) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let app = app.clone();
    if tray.toggling.swap(true, Ordering::SeqCst) {
        // 上一次切换仍在进行：不排队执行，只把勾选恢复为实际状态。
        tauri::async_runtime::spawn(async move {
            let status = app.state::<AppState>().manager.status().await;
            sync_managed_check(&app, status.enabled);
        });
        return;
    }
    tauri::async_runtime::spawn(async move {
        run_toggle(&app).await;
        if let Some(tray) = app.try_state::<TrayState>() {
            tray.toggling.store(false, Ordering::SeqCst);
        }
    });
}

async fn run_toggle(app: &AppHandle) {
    let manager = app.state::<AppState>().manager.clone();
    let action = ToggleAction::for_enabled(manager.status().await.enabled);
    let result = match action {
        ToggleAction::Enable => manager.enable(EnableRequest::default()).await,
        ToggleAction::Disable => manager.disable().await,
    };
    let enable = action == ToggleAction::Enable;
    match result {
        Ok(status) => {
            log::event(
                "desktop.tray_toggle",
                json!({ "enable": enable, "ok": true }),
            );
            events::publish_status(app, &status);
            if enable {
                events::probe_and_publish(app, &manager).await;
            }
        }
        Err(error) => {
            let payload = ErrorPayload::from(error);
            log::event(
                "desktop.tray_toggle",
                json!({ "enable": enable, "ok": false, "code": payload.code }),
            );
            events::show_main_window(app);
            events::publish_operation_failed(app, &payload);
            let status = manager.status().await;
            events::publish_status(app, &status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_disables_when_enabled_and_enables_otherwise() {
        assert_eq!(ToggleAction::for_enabled(true), ToggleAction::Disable);
        assert_eq!(ToggleAction::for_enabled(false), ToggleAction::Enable);
    }
}
