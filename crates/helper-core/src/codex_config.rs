//! `~/.codex/config.toml` 受管 provider 的校正与还原（设计 §4.2、§5、§7）。
//!
//! 只增删改受管键，其余内容（注释、其他 provider、projects、plugins 等）用 `toml_edit` 原样保留。
//! 与原实现不同：配置无法解析、`model_providers` 或受管节不是 table 时一律中止、不写入。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 启用前需要记录、停用时需要还原的原值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousConfig {
    /// 启用前根键 `model_provider` 的值；不存在为 `None`
    pub model_provider: Option<String>,
    /// 启用时被移除的根键 `model_catalog_json`；不存在为 `None`
    pub model_catalog_json: Option<String>,
}

/// `config.toml` 的只读检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigInspection {
    /// 文件是否存在
    pub exists: bool,
    /// 根键 `model_provider`（字符串值）
    pub model_provider: Option<String>,
    /// 根键 `model_catalog_json`（字符串值）
    pub model_catalog_json: Option<String>,
    /// 是否存在 `[model_providers.managed_gateway]` 节
    pub has_managed_provider: bool,
    /// 受管内容是否完全正确（model_provider 指向受管 provider，且受管节的全部受管键与期望一致，
    /// 包括给定的 credential command）；用于状态灯与“是否需要重新校正”
    pub fully_managed: bool,
}

impl ConfigInspection {
    /// 是否残留任何受管内容（model_provider 指向受管 provider，或存在受管节）。
    pub fn has_managed_residue(&self) -> bool {
        self.has_managed_provider
            || self.model_provider.as_deref() == Some(crate::consts::PROVIDER_ID)
    }

    /// 由当前配置得出“启用前原值”；若 model_provider 已指向受管 provider（残留），视为 `None`。
    pub fn previous(&self) -> PreviousConfig {
        PreviousConfig {
            model_provider: self
                .model_provider
                .clone()
                .filter(|value| value != crate::consts::PROVIDER_ID),
            model_catalog_json: self.model_catalog_json.clone(),
        }
    }
}

/// 校正 / 还原失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 配置无法解析：中止，不写入
    #[error("config.toml 解析失败：{message}")]
    Parse { message: String },
    /// `model_providers`、受管节或其 `auth` 不是 table：中止，不写入
    #[error("{key} 不是 table")]
    NotATable { key: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 一次校正的结果，供失败时精确回滚。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// 是否实际写入了文件（内容无变化时不写、不备份）
    pub changed: bool,
    /// 写入前的原文；`None` 表示写入前文件不存在
    pub original: Option<String>,
}

/// 只读检查。文件不存在返回 `exists = false` 的默认值；无法解析返回 `ConfigError::Parse`。
///
/// `credential_command` 用于判定 `fully_managed`（auth.command 是否指向当前安装目录）。
pub fn inspect(
    config_path: &Path,
    credential_command: &str,
) -> Result<ConfigInspection, ConfigError> {
    let _ = (config_path, credential_command);
    todo!("阶段 1 任务 1.3")
}

/// 校正受管内容（幂等）：
///
/// - `model_provider = "managed_gateway"`；
/// - `[model_providers.managed_gateway]`：name / base_url / wire_api；移除 env_key、
///   experimental_bearer_token、requires_openai_auth；`[...auth]` command = `credential_command`、
///   args = ["get", "codex-helper/managed-gateway"]；
/// - 移除根键 `model_catalog_json`（调用方已确认并记录原值）。
///
/// 内容有变化且文件已存在时，先把原文件复制到 `backup_path`（固定名，覆盖），再原子写入。
/// 内容无变化时不写入、不备份。
pub fn apply_managed(
    config_path: &Path,
    backup_path: &Path,
    credential_command: &str,
) -> Result<ApplyOutcome, ConfigError> {
    let _ = (config_path, backup_path, credential_command);
    todo!("阶段 1 任务 1.3")
}

/// 撤销一次 [`apply_managed`]：`changed` 时写回原文（原先不存在则删除文件）。
pub fn rollback(config_path: &Path, outcome: &ApplyOutcome) -> Result<(), ConfigError> {
    let _ = (config_path, outcome);
    todo!("阶段 1 任务 1.3")
}

/// 停用还原（设计 §5.2 第 2 步）：定点修改，不以备份整文件覆盖。
///
/// - `model_provider`、`model_catalog_json` 还原为 `previous` 中的值（`None` 则删除该键）；
/// - 删除 `[model_providers.managed_gateway]` 整节（若 `model_providers` 因此为空且原本是隐式表则一并移除）。
///
/// 文件不存在视为成功。返回是否实际写入。
pub fn restore(config_path: &Path, previous: &PreviousConfig) -> Result<bool, ConfigError> {
    let _ = (config_path, previous);
    todo!("阶段 1 任务 1.3")
}

/// 无状态文件时的残留清理（设计 §5.4）：`model_provider == "managed_gateway"` 时删除该键，
/// 并删除受管 provider 节。文件不存在视为成功。返回是否实际写入。
pub fn remove_residue(config_path: &Path) -> Result<bool, ConfigError> {
    let _ = config_path;
    todo!("阶段 1 任务 1.3")
}
