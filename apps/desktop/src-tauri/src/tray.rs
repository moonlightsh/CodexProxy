//! 托盘（设计 §8）：左键单击显示窗口。菜单：
//!
//! - Windows：显示窗口 / 受管模式开关 / 分隔线 / 退出；
//! - macOS：显示窗口 / 受管模式开关 / 分隔线 / 撤销配置并退出（设计 §6，见 [`crate::revoke`]）/ 退出。

use std::sync::atomic::{AtomicBool, Ordering};

use helper_core::log;
use helper_core::types::{EnableRequest, ErrorPayload};
use serde_json::json;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager as _, Wry};

use crate::commands::AppState;
use crate::events;
use crate::lifecycle;
use crate::revoke;

const TRAY_ID: &str = "main";
const TOOLTIP: &str = "CodexHelper";
const MENU_SHOW: &str = "show";
const MENU_MANAGED: &str = "managed";
const MENU_REVOKE: &str = "revoke";
const MENU_QUIT: &str = "quit";

/// 托盘菜单项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEntry {
    Show,
    Managed,
    Separator,
    Revoke,
    Quit,
}

/// 菜单布局：“撤销配置并退出”位于分隔线之后、“退出”之前。
fn menu_layout(with_revoke: bool) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry::Show, MenuEntry::Managed, MenuEntry::Separator];
    if with_revoke {
        entries.push(MenuEntry::Revoke);
    }
    entries.push(MenuEntry::Quit);
    entries
}

/// 菜单项文案；分隔线没有文案。
fn menu_label(entry: MenuEntry) -> &'static str {
    match entry {
        MenuEntry::Show => "显示窗口",
        MenuEntry::Managed => "受管模式",
        MenuEntry::Separator => "",
        MenuEntry::Revoke => revoke::REVOKE_TITLE,
        MenuEntry::Quit => "退出",
    }
}

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
    let managed = CheckMenuItem::with_id(
        app,
        MENU_MANAGED,
        menu_label(MenuEntry::Managed),
        true,
        false,
        None::<&str>,
    )?;
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
    for entry in menu_layout(cfg!(target_os = "macos")) {
        let label = menu_label(entry);
        let item: Box<dyn IsMenuItem<Wry>> = match entry {
            MenuEntry::Show => Box::new(MenuItem::with_id(
                app,
                MENU_SHOW,
                label,
                true,
                None::<&str>,
            )?),
            MenuEntry::Managed => Box::new(managed.clone()),
            MenuEntry::Separator => Box::new(PredefinedMenuItem::separator(app)?),
            MenuEntry::Revoke => Box::new(MenuItem::with_id(
                app,
                MENU_REVOKE,
                label,
                true,
                None::<&str>,
            )?),
            MenuEntry::Quit => Box::new(MenuItem::with_id(
                app,
                MENU_QUIT,
                label,
                true,
                None::<&str>,
            )?),
        };
        items.push(item);
    }
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|item| item.as_ref()).collect();
    let menu = Menu::with_items(app, &refs)?;

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
        MENU_REVOKE => revoke::request_revoke(app),
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

    use MenuEntry::*;

    #[test]
    fn tray_menu_without_revoke_is_unchanged() {
        assert_eq!(menu_layout(false), [Show, Managed, Separator, Quit]);
    }

    #[test]
    fn tray_menu_with_revoke_puts_it_after_separator_before_quit() {
        assert_eq!(menu_layout(true), [Show, Managed, Separator, Revoke, Quit]);
    }

    #[test]
    fn tray_menu_labels() {
        let labels: Vec<_> = [Show, Managed, Revoke, Quit].map(menu_label).into();
        assert_eq!(labels, ["显示窗口", "受管模式", "撤销配置并退出", "退出"]);
    }
}
