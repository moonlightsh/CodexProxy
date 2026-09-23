//! CodexHelper 桌面应用入口（设计 §8）。
//!
//! 只做组装：业务全部委托 `helper_core::manager::Manager`；命令见 [`commands`]，托盘见 [`tray`]，
//! 启动对账与定时重测见 [`startup`]，关闭隐藏与退出确认见 [`lifecycle`]。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod lifecycle;
mod process;
mod startup;
mod tray;

use std::sync::{Arc, OnceLock};

use helper_core::credential::SystemCredentialStore;
use helper_core::log;
use helper_core::manager::{Manager, ManagerOptions};
use helper_core::proxy::RecordSink;
use serde_json::json;
use tauri::Emitter as _;
use tauri_plugin_autostart::MacosLauncher;

use crate::commands::{AppHandleCell, AppState, EVENT_CONNECTION_RECORDED};

fn main() {
    let handle: AppHandleCell = Arc::new(OnceLock::new());
    let manager = match build_manager(&handle) {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            eprintln!("CodexHelper 初始化失败：{error:#}");
            std::process::exit(1);
        }
    };
    let setup_handle = handle.clone();

    let app = tauri::Builder::default()
        // 单实例必须最先注册：重复启动时唤起已有窗口（开机自启带 --minimized 的重复启动除外）。
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !startup::has_minimized_flag(&args) {
                events::show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![startup::MINIMIZED_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new(manager, handle))
        .manage(lifecycle::Lifecycle::default())
        .setup(move |app| {
            let _ = setup_handle.set(app.handle().clone());
            tray::build(app)?;
            if !startup::has_minimized_flag(std::env::args()) {
                events::show_main_window(app.handle());
            }
            startup::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(lifecycle::on_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::refresh_status,
            commands::enable,
            commands::disable,
            commands::save_key,
            commands::clear_key,
            commands::cleanup_residue,
            commands::remove_legacy_env_block,
            commands::set_autostart,
            commands::recent_connections,
            commands::get_reconcile_report,
        ])
        .build(tauri::generate_context!())
        .expect("CodexHelper 启动失败");
    app.run(lifecycle::on_run_event);
}

/// 初始化日志并创建编排器。连接记录回调通过应用句柄推送 `connection-recorded`。
fn build_manager(handle: &AppHandleCell) -> anyhow::Result<Manager> {
    let options = ManagerOptions::production(
        Arc::new(SystemCredentialStore::managed()),
        Some(connection_sink(handle.clone())),
    )?;
    log::init(options.paths.log_dir());
    log::event(
        "desktop.start",
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "minimized": startup::has_minimized_flag(std::env::args()),
            "proxy_port": options.proxy_port,
        }),
    );
    Ok(Manager::new(options))
}

/// 连接记录回调：运行在代理任务里，只做无锁读取句柄与非阻塞的事件投递，失败忽略。
fn connection_sink(handle: AppHandleCell) -> RecordSink {
    Arc::new(move |record| {
        if let Some(app) = handle.get() {
            let _ = app.emit(EVENT_CONNECTION_RECORDED, record);
        }
    })
}
