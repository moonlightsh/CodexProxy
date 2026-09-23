//! 工具自身状态文件（设计 §4.1）：`%LOCALAPPDATA%\CodexHelper\state.json`。不保存 Key。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codex_config::PreviousConfig;

/// 状态文件内容。字段名与设计文档中的 JSON 一致（snake_case），缺失字段取默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HelperState {
    pub enabled: bool,
    /// 启用前 `model_provider` 的值；不存在为 `null`
    pub previous_model_provider: Option<String>,
    /// 启用时被移除的外部 catalog 指针；无则为 `null`
    pub previous_model_catalog_json: Option<String>,
    pub autostart: bool,
}

impl HelperState {
    /// 取出记录的原值。
    pub fn previous(&self) -> PreviousConfig {
        PreviousConfig {
            model_provider: self.previous_model_provider.clone(),
            model_catalog_json: self.previous_model_catalog_json.clone(),
        }
    }

    /// 记录原值。只应在“停用 → 启用”的转换时调用，已启用状态下不得覆盖。
    pub fn set_previous(&mut self, previous: PreviousConfig) {
        self.previous_model_provider = previous.model_provider;
        self.previous_model_catalog_json = previous.model_catalog_json;
    }

    /// 清空原值（停用完成后）。
    pub fn clear_previous(&mut self) {
        self.previous_model_provider = None;
        self.previous_model_catalog_json = None;
    }
}

/// 读取状态文件；不存在返回 `Ok(None)`；内容损坏返回错误（由调用方决定如何处理）。
pub fn load(path: &Path) -> anyhow::Result<Option<HelperState>> {
    let _ = path;
    todo!("阶段 1 任务 1.5")
}

/// 原子写入状态文件（格式化 JSON）。
pub fn save(path: &Path, state: &HelperState) -> anyhow::Result<()> {
    let _ = (path, state);
    todo!("阶段 1 任务 1.5")
}
