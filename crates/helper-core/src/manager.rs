//! 启用 / 停用 / 启动对账 / 清理的编排（设计 §5、§7）。
//!
//! 桌面应用的命令层全部委托给 [`Manager`]；卸载清理由 credential.exe 调用 [`cleanup`]。
//! 所有操作串行执行（内部互斥），任一步失败撤销已完成的步骤。

use std::sync::Arc;

use crate::credential::CredentialStore;
use crate::paths::HelperPaths;
use crate::proxy::RecordSink;
use crate::types::{
    CleanupReport, ConnectionRecord, EnableRequest, ManagerError, ReconcileReport, SaveKeyRequest,
    Status,
};

/// 编排层的全部可注入依赖。生产环境用 [`ManagerOptions::production`]，测试注入临时目录、
/// 内存凭据、假网关与假 SOCKS5。
#[derive(Clone)]
pub struct ManagerOptions {
    pub paths: HelperPaths,
    pub credentials: Arc<dyn CredentialStore>,
    /// 写入 `auth.command` 的凭据程序绝对路径
    pub credential_command: String,
    /// 本地代理端口（生产为 `proxy::proxy_port()`）
    pub proxy_port: u16,
    pub socks5_host: String,
    pub socks5_port: u16,
    /// Key 校验使用的网关地址（生产为 `consts::GATEWAY_BASE_URL`）
    pub gateway_base_url: String,
    /// 网关可达性检测的主机与端口
    pub gateway_host: String,
    pub gateway_port: u16,
    /// 每条代理连接结束后的外部回调（桌面应用用它推送事件）；编排层自身维护环形缓冲
    pub on_record: Option<RecordSink>,
}

impl ManagerOptions {
    /// 生产配置：真实路径、固定网关与 SOCKS5、`CODEX_HELPER_PROXY_PORT` 可覆盖的端口。
    pub fn production(
        credentials: Arc<dyn CredentialStore>,
        on_record: Option<RecordSink>,
    ) -> anyhow::Result<Self> {
        let _ = (credentials, on_record);
        todo!("阶段 2 任务 2.1")
    }
}

/// 编排器。内部持有代理句柄、最近连接缓冲与最近一次可达性结果。
pub struct Manager {
    options: ManagerOptions,
}

impl Manager {
    pub fn new(options: ManagerOptions) -> Self {
        Self { options }
    }

    /// 当前状态快照（读文件 + 内存状态，不做网络探测）。
    pub async fn status(&self) -> Status {
        let _ = &self.options;
        todo!("阶段 2 任务 2.1")
    }

    /// 启用（设计 §5.1）：校验 Key → 写凭据 → 记录原值 → 校正 config.toml → 启动代理并自检
    /// → 写 .env → 状态检测。任一步失败撤销已完成步骤。
    pub async fn enable(&self, request: EnableRequest) -> Result<Status, ManagerError> {
        let _ = request;
        todo!("阶段 2 任务 2.1")
    }

    /// 仅校验并保存 / 替换 Key（“重新录入”），不改变启用状态。
    pub async fn save_key(&self, request: SaveKeyRequest) -> Result<Status, ManagerError> {
        let _ = request;
        todo!("阶段 2 任务 2.1")
    }

    /// 停用（设计 §5.2）：移除 .env 块 → 还原 config.toml → 停止代理 → 更新状态文件。
    pub async fn disable(&self) -> Result<Status, ManagerError> {
        todo!("阶段 2 任务 2.1")
    }

    /// 删除凭据（“清除 Key”）。已启用时 Codex 请求将 fail-closed。
    pub async fn clear_key(&self) -> Result<Status, ManagerError> {
        todo!("阶段 2 任务 2.1")
    }

    /// 工具启动对账（设计 §5.3）。
    pub async fn reconcile_on_startup(&self) -> ReconcileReport {
        todo!("阶段 2 任务 2.1")
    }

    /// 停用状态下的一键清理残留（等价于 §5.4 不删除凭据的清理）。
    pub async fn cleanup_residue(&self) -> Result<Status, ManagerError> {
        todo!("阶段 2 任务 2.1")
    }

    /// 一键移除 `.env` 中的 Codex++ 旧块。
    pub async fn remove_legacy_env_block(&self) -> Result<Status, ManagerError> {
        todo!("阶段 2 任务 2.1")
    }

    /// 记录开机自启开关到状态文件（注册表项由桌面应用负责）。
    pub async fn set_autostart_flag(&self, enabled: bool) -> Result<Status, ManagerError> {
        let _ = enabled;
        todo!("阶段 2 任务 2.1")
    }

    /// 检测网关与 SOCKS5 的 TCP 可达性并缓存结果，返回最新状态。
    pub async fn probe_reachability(&self) -> Status {
        todo!("阶段 2 任务 2.1")
    }

    /// 最近连接（最新在前，最多 50 条）。
    pub fn recent_connections(&self) -> Vec<ConnectionRecord> {
        todo!("阶段 2 任务 2.1")
    }

    /// 退出前停止本进程的代理（不改配置：fail-closed 为有意行为）。
    pub async fn shutdown(&self) {
        todo!("阶段 2 任务 2.1")
    }
}

/// 卸载清理（设计 §5.4），供 `codex-helper-credential.exe cleanup [--purge-key]` 调用。
///
/// 执行 §5.2 的第 1、2、4 步；状态文件缺失时按残留规则清理；`purge_key` 时同时删除凭据。
/// 文件或块不存在都视为成功。
pub fn cleanup(
    paths: &HelperPaths,
    credentials: &dyn CredentialStore,
    purge_key: bool,
) -> anyhow::Result<CleanupReport> {
    let _ = (paths, credentials, purge_key);
    todo!("阶段 2 任务 2.1")
}
