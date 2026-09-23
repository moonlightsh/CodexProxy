//! 启动流程（设计 §5.3）：自启状态对账 → 启动对账 → 首次可达性检测 → 每 30 秒重测。

use std::time::Duration;

use helper_core::consts;
use helper_core::log;
use helper_core::manager::Manager;
use helper_core::types::ReconcileOutcome;
use serde_json::json;
use tauri::{AppHandle, Manager as _};
use tauri_plugin_autostart::ManagerExt as _;
use tokio::time::{Instant, MissedTickBehavior};

use crate::commands::{AppState, EVENT_KEY_REQUIRED, EVENT_RECONCILE_FINISHED};
use crate::events;

/// 开机自启时附带的参数：只驻留托盘，不显示窗口。
pub const MINIMIZED_ARG: &str = "--minimized";

/// 命令行是否含 `--minimized`。
pub fn has_minimized_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == MINIMIZED_ARG)
}

/// 启动对账结论需要的界面提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attention {
    /// 显示主窗口
    pub show_window: bool,
    /// 推送 `key-required`（前端聚焦 Key 录入）
    pub key_required: bool,
}

/// 凭据缺失需要录入 Key；校正失败或发现残留需要用户处理；其余静默（`--minimized` 时保持托盘）。
pub fn attention_for(outcome: ReconcileOutcome) -> Attention {
    match outcome {
        ReconcileOutcome::NeedsKey => Attention {
            show_window: true,
            key_required: true,
        },
        ReconcileOutcome::Failed | ReconcileOutcome::ResidueFound => Attention {
            show_window: true,
            key_required: false,
        },
        ReconcileOutcome::Idle | ReconcileOutcome::Applied => Attention {
            show_window: false,
            key_required: false,
        },
    }
}

/// 在后台执行启动流程。
pub fn spawn(app: AppHandle) {
    let manager = app.state::<AppState>().manager.clone();
    tauri::async_runtime::spawn(async move {
        sync_autostart_flag(&app, &manager).await;
        reconcile(&app, &manager).await;
        events::probe_and_publish(&app, &manager).await;
        health_probe_loop(&app, &manager).await;
    });
}

/// 自启状态以系统实际状态为准（用户可能在系统设置里关掉了启动项），不一致时写回状态文件。
async fn sync_autostart_flag(app: &AppHandle, manager: &Manager) {
    let actual = match app.autolaunch().is_enabled() {
        Ok(actual) => actual,
        Err(_) => {
            log::event("desktop.autostart_sync", json!({ "ok": false }));
            return;
        }
    };
    if manager.status().await.autostart == actual {
        return;
    }
    let result = manager.set_autostart_flag(actual).await;
    log::event(
        "desktop.autostart_sync",
        json!({
            "autostart": actual,
            "ok": result.is_ok(),
            "code": result.as_ref().err().map(|error| error.code()),
        }),
    );
}

/// 启动对账：先保存报告再推送，保证前端无论加载早晚都能拿到结论。
async fn reconcile(app: &AppHandle, manager: &Manager) {
    let report = manager.reconcile_on_startup().await;
    let attention = attention_for(report.outcome);
    app.state::<AppState>()
        .store_reconcile_report(report.clone());
    events::emit(app, EVENT_RECONCILE_FINISHED, &report);
    events::publish_status(app, &report.status);
    if attention.show_window {
        events::show_main_window(app);
    }
    if attention.key_required {
        events::emit(app, EVENT_KEY_REQUIRED, ());
    }
}

/// 每 `HEALTH_PROBE_INTERVAL_SECS` 秒重测网关与 SOCKS5 可达性（设计 §7）。
async fn health_probe_loop(app: &AppHandle, manager: &Manager) {
    let period = Duration::from_secs(consts::HEALTH_PROBE_INTERVAL_SECS);
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        events::probe_and_publish(app, manager).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimized_flag_detected_anywhere_in_args() {
        assert!(has_minimized_flag(["codex-helper.exe", "--minimized"]));
        assert!(has_minimized_flag(vec![
            "C:\\Program Files\\CodexHelper\\codex-helper.exe".to_string(),
            "--foo".to_string(),
            "--minimized".to_string(),
        ]));
    }

    #[test]
    fn minimized_flag_requires_exact_match() {
        assert!(!has_minimized_flag(["codex-helper.exe"]));
        assert!(!has_minimized_flag(Vec::<String>::new()));
        assert!(!has_minimized_flag(["codex-helper.exe", "--minimize"]));
        assert!(!has_minimized_flag([
            "codex-helper.exe",
            "--minimized=true"
        ]));
        assert!(!has_minimized_flag(["codex-helper.exe", "-minimized"]));
    }

    #[test]
    fn attention_matches_reconcile_outcome() {
        assert_eq!(
            attention_for(ReconcileOutcome::NeedsKey),
            Attention {
                show_window: true,
                key_required: true
            }
        );
        for outcome in [ReconcileOutcome::Failed, ReconcileOutcome::ResidueFound] {
            assert_eq!(
                attention_for(outcome),
                Attention {
                    show_window: true,
                    key_required: false
                }
            );
        }
        for outcome in [ReconcileOutcome::Idle, ReconcileOutcome::Applied] {
            assert_eq!(
                attention_for(outcome),
                Attention {
                    show_window: false,
                    key_required: false
                }
            );
        }
    }
}
