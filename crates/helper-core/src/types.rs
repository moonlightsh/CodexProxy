//! 跨模块共享的数据类型与错误（接口契约）。
//!
//! 这些类型会序列化给前端（`apps/desktop/src/types.ts` 与之一一对应），字段统一 camelCase。
//! 修改任何字段都必须同步前端类型。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::rules::Route;

/// 网关 Key 校验结果（`GET /v1/models`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyCheck {
    /// 2xx
    Ok,
    /// 401 / 403
    Unauthorized,
    /// 5xx
    ServerError,
    /// 超时、网络错误或其他无法判定的状态码
    TimeoutOrNetwork,
}

/// TCP 可达性（仅用于状态灯，不阻断启用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reachability {
    /// 尚未检测
    #[default]
    Unknown,
    Reachable,
    Unreachable,
}

/// 本地代理运行状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyState {
    #[default]
    Stopped,
    /// 由本进程监听
    Running,
    /// 端口上是本工具的另一个实例（经自识端点确认），已复用
    External,
}

/// 本地代理状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub state: ProxyState,
    pub port: u16,
}

/// 一条代理连接的记录（设计 §6）。连接结束后产生，写入内存环形缓冲并按采样写日志。
///
/// 只包含主机、端口、决策、结果与耗时；绝不包含请求头、URL 路径 / 查询参数或 body。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRecord {
    /// 连接开始时间，Unix 毫秒
    pub time: u64,
    pub host: String,
    pub port: u16,
    pub decision: Route,
    /// 上游（SOCKS5 隧道或直连）是否建立成功
    pub ok: bool,
    /// 建立上游连接耗时（毫秒）；失败时为失败前耗时
    pub ms: u64,
}

/// 界面展示用的完整状态快照。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Windows 为 true；其他平台为开发模式（界面顶部横幅）
    pub platform_supported: bool,
    /// 受管模式开关（状态文件 `enabled`）
    pub enabled: bool,
    /// 凭据是否已保存（不回显 Key）
    pub key_configured: bool,
    /// 已启用但凭据缺失（fail-closed，需要录入 Key）
    pub needs_key: bool,
    pub proxy: ProxyStatus,
    /// 模型网关 TCP 可达性
    pub gateway: Reachability,
    /// 上游 SOCKS5 TCP 可达性
    pub socks5: Reachability,
    /// 展示用：网关地址，如 `10.20.30.61:8080`
    pub gateway_addr: String,
    /// 展示用：SOCKS5 地址，如 `10.20.30.61:7891`
    pub socks5_addr: String,
    /// `config.toml` 已处于受管状态（model_provider 与受管节都正确）
    pub config_managed: bool,
    /// `.env` 受管块存在且内容与当前端口一致
    pub env_managed: bool,
    /// 停用状态下仍残留受管内容（可一键清理）
    pub residue: bool,
    /// `.env` 中存在 Codex++ 旧块（警告并提供一键移除）
    pub legacy_env_block: bool,
    /// 存在根键 `model_catalog_json` 时的值（启用前需确认）
    pub model_catalog_json: Option<String>,
    /// `config.toml` 无法解析或结构不符时的原因
    pub config_error: Option<String>,
    /// 开机自启（状态文件 `autostart`）
    pub autostart: bool,
    /// Codex home 路径（展示用）
    pub codex_home: String,
}

/// 启用请求（设计 §5.1）。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EnableRequest {
    /// 新录入的 Key；`None` 表示使用已保存的凭据（打开开关）
    pub key: Option<String>,
    /// 网关 5xx / 超时后用户选择“仍然保存”
    pub force_save: bool,
    /// 用户已确认移除 `model_catalog_json` 指针
    pub confirm_remove_catalog: bool,
}

impl fmt::Debug for EnableRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnableRequest")
            .field("key", &self.key.as_ref().map(|_| "***"))
            .field("force_save", &self.force_save)
            .field("confirm_remove_catalog", &self.confirm_remove_catalog)
            .finish()
    }
}

/// 仅保存 / 替换 Key（“重新录入”），不改变启用状态。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveKeyRequest {
    pub key: String,
    /// 网关 5xx / 超时后用户选择“仍然保存”
    pub force_save: bool,
}

impl fmt::Debug for SaveKeyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaveKeyRequest")
            .field("key", &"***")
            .field("force_save", &self.force_save)
            .finish()
    }
}

/// 启动对账的结论（设计 §5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReconcileOutcome {
    /// 未启用且无残留
    Idle,
    /// 未启用但检测到受管残留，需提示一键清理
    ResidueFound,
    /// 已启用，受管配置已幂等校正、代理已就绪
    Applied,
    /// 已启用但凭据缺失：不启动代理，弹窗要求录入 Key
    NeedsKey,
    /// 已启用但校正失败（原因见 `error`）
    Failed,
}

/// 启动对账报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    pub outcome: ReconcileOutcome,
    pub error: Option<ErrorPayload>,
    pub status: Status,
}

/// 卸载清理报告（设计 §5.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub env_block_removed: bool,
    pub config_restored: bool,
    pub state_reset: bool,
    pub key_purged: bool,
}

/// 编排层错误。消息为中文，可直接展示；绝不携带 Key。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagerError {
    #[error("API Key 不能为空")]
    EmptyKey,
    #[error("网关拒绝了该 API Key（401/403），未保存")]
    KeyRejected,
    #[error("网关暂时不可用（{reason}），可以选择仍然保存")]
    GatewayUnavailable { reason: String },
    #[error("尚未录入 API Key")]
    CredentialMissing,
    #[error("config.toml 中存在 model_catalog_json 指针（{path}），启用前需要确认移除")]
    CatalogConfirmationRequired { path: String },
    #[error("本地代理端口 {port} 不可用（{reason}），且不是本工具的实例")]
    PortUnavailable { port: u16, reason: String },
    #[error("config.toml 无法处理：{message}。请先修复该文件")]
    ConfigInvalid { message: String },
    #[error("凭据读写失败：{message}")]
    Credential { message: String },
    #[error("文件读写失败：{message}")]
    Io { message: String },
    #[error("{message}")]
    Internal { message: String },
}

impl ManagerError {
    /// 稳定的错误码，前端据此决定交互（如“仍然保存”“确认移除 catalog”）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyKey => "emptyKey",
            Self::KeyRejected => "keyRejected",
            Self::GatewayUnavailable { .. } => "gatewayUnavailable",
            Self::CredentialMissing => "credentialMissing",
            Self::CatalogConfirmationRequired { .. } => "catalogConfirmationRequired",
            Self::PortUnavailable { .. } => "portUnavailable",
            Self::ConfigInvalid { .. } => "configInvalid",
            Self::Credential { .. } => "credential",
            Self::Io { .. } => "io",
            Self::Internal { .. } => "internal",
        }
    }

    /// 转为给前端的负载。
    pub fn payload(&self) -> ErrorPayload {
        ErrorPayload {
            code: self.code().to_string(),
            message: self.to_string(),
        }
    }
}

/// 传给前端的错误：`{ code, message }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

impl From<ManagerError> for ErrorPayload {
    fn from(error: ManagerError) -> Self {
        error.payload()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_debug_never_prints_key() {
        let enable = EnableRequest {
            key: Some("sk-secret".into()),
            ..Default::default()
        };
        let save = SaveKeyRequest {
            key: "sk-secret".into(),
            force_save: false,
        };
        assert!(!format!("{enable:?}").contains("sk-secret"));
        assert!(!format!("{save:?}").contains("sk-secret"));
    }

    #[test]
    fn dto_fields_serialize_as_camel_case() {
        let json = serde_json::to_value(Status::default()).unwrap();
        assert!(json.get("platformSupported").is_some());
        assert!(json.get("modelCatalogJson").is_some());
        let request: EnableRequest =
            serde_json::from_str(r#"{"key":"k","forceSave":true,"confirmRemoveCatalog":true}"#)
                .unwrap();
        assert!(request.force_save && request.confirm_remove_catalog);
    }

    #[test]
    fn error_payload_carries_code_and_message() {
        let payload = ManagerError::PortUnavailable {
            port: 17891,
            reason: "address in use".into(),
        }
        .payload();
        assert_eq!(payload.code, "portUnavailable");
        assert!(payload.message.contains("17891"));
    }
}
