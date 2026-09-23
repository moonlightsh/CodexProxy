//! 脱敏诊断日志（设计 §10）：JSON Lines，写到 `%LOCALAPPDATA%\CodexHelper\logs\`。
//!
//! - 未调用 [`init`] 前所有写入静默丢弃；写日志永不 panic、永不向调用方返回错误。
//! - 写入前对 detail 做脱敏：键名疑似凭据（authorization / token / key / secret / password 等）
//!   的值、以及形如 `Bearer xxx`、`sk-xxx` 的字符串一律替换为 `***`。
//! - 单文件超过上限时压缩保留尾部（移植自 Codex++ diagnostic_log）。

use std::path::PathBuf;

/// 设置日志目录（通常为 `HelperPaths::log_dir()`）。可重复调用，以最后一次为准。
pub fn init(log_dir: impl Into<PathBuf>) {
    let _ = log_dir.into();
    // 阶段 1 任务 1.5 实现；桩实现为空操作，保证其他模块可调用。
}

/// 当前日志文件路径；未初始化返回 `None`。
pub fn log_file_path() -> Option<PathBuf> {
    None
}

/// 追加一条事件。`detail` 会先经 [`redact_value`] 脱敏。
pub fn event(name: &str, detail: serde_json::Value) {
    let _ = (name, detail);
    // 阶段 1 任务 1.5 实现；桩实现为空操作。
}

/// 就地脱敏 JSON 值。
pub fn redact_value(value: &mut serde_json::Value) {
    let _ = value;
    todo!("阶段 1 任务 1.5")
}

/// 脱敏任意文本（错误消息等）。
pub fn redact_text(text: &str) -> String {
    let _ = text;
    todo!("阶段 1 任务 1.5")
}
