//! CodexHelper 桌面应用入口（设计 §8）。
//!
//! 薄命令层：业务全部委托 `helper_core::manager::Manager`。托盘、单实例、开机自启、
//! 关闭隐藏到托盘、退出确认与 30 秒可达性重测在阶段 3 任务 3.1 实现。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

fn main() {
    tauri::Builder::default()
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
        .run(tauri::generate_context!())
        .expect("CodexHelper 启动失败");
}
