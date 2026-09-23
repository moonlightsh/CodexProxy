//! 启用 / 停用 / 启动对账 / 清理的编排（设计 §5、§7）。
//!
//! 桌面应用的命令层全部委托给 [`Manager`]；卸载清理由 credential.exe 调用 [`cleanup`]。
//! 所有改变状态的操作串行执行（内部互斥），任一步失败撤销已完成的步骤。
//!
//! 约定：
//!
//! - 锁分两层：
//!   - 操作锁（异步互斥）串行化所有改变状态的步骤（凭据、config.toml、.env、状态文件的写入与
//!     代理启停），并持有本进程的代理句柄。Key 校验（网络，最长 15 秒）在获取操作锁之前完成，
//!     锁内只做本地步骤与代理自识探测。
//!   - 运行时短锁（`std::sync::Mutex`）保存代理运行状态与可达性缓存，只在读写字段时持有、绝不跨
//!     `await`。代理运行状态只在持有操作锁时写入。
//!
//!   `status()` 与 `probe_reachability()` 不等待操作锁：只读文件与短锁快照，因此在启用进行中可能
//!   反映中间状态；各操作的返回值是该操作完成时的状态，桌面层以此推送 `status-changed`。
//!   最近连接缓冲同样在锁外共享：代理任务里的连接记录回调绝不获取任何编排锁（避免死锁）。
//! - 文件写入全部经 `codex_config` / `codex_env` / `state` / `fsutil`（内部为原子写）。
//! - 错误、日志与 Debug 输出绝不携带 Key；日志只记错误码、步骤名、布尔与端口。
//! - 状态文件损坏：改名为 `state.json.corrupt`（覆盖）后按默认状态继续；`status` 只按默认值展示、
//!   不改名；卸载清理视为缺失、不触碰。
//! - 停用中途失败：尽力完成其余步骤并返回第一个错误；状态文件的 `enabled` 只在配置还原成功后才置为
//!   `false`。配置还原失败时状态保持“已启用”，下次启动对账会按已启用重新接管（界面同时看到错误）；
//!   配置已还原而 `.env` 块移除失败时状态置为停用，`.env` 残留由 `residue` 提示一键清理。
//! - 启用中断：若进程在“记录原值”之后、“置为已启用”之前退出，状态文件为停用但留有 `previous_*`，
//!   config.toml 可能已被接管。此时停用 / 清理在 `model_provider` 仍指向受管 provider 时按记录的原值
//!   还原（而不是删除该键），再次启用也保留原记录，用户原来的 provider 不会丢失。

use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};

use serde_json::json;
use tokio::sync::Mutex;

use crate::codex_config::{self, ApplyOutcome, ConfigError, ConfigInspection, PreviousConfig};
use crate::codex_env;
use crate::consts;
use crate::credential::{CredentialStore, Secret};
use crate::fsutil;
use crate::gateway;
use crate::log;
use crate::paths::{self, HelperPaths};
use crate::proxy::{
    self, ProxyConfig, ProxyHandle, ProxyStartError, RecentConnections, RecordSink,
};
use crate::state::{self, HelperState};
use crate::types::{
    CleanupReport, ConnectionRecord, EnableRequest, KeyCheck, ManagerError, ProxyState,
    ProxyStatus, Reachability, ReconcileOutcome, ReconcileReport, SaveKeyRequest, Status,
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
        Ok(Self {
            paths: HelperPaths::detect()?,
            credentials,
            credential_command: paths::credential_command_path()?,
            proxy_port: proxy::proxy_port(),
            socks5_host: consts::SOCKS5_HOST.to_string(),
            socks5_port: consts::SOCKS5_PORT,
            gateway_base_url: consts::GATEWAY_BASE_URL.to_string(),
            gateway_host: consts::GATEWAY_HOST.to_string(),
            gateway_port: consts::GATEWAY_PORT,
            on_record,
        })
    }
}

/// 编排器。内部持有代理句柄、最近连接缓冲与最近一次可达性结果。
pub struct Manager {
    options: ManagerOptions,
    /// 操作锁：串行化所有改变状态的步骤，持有本进程代理句柄
    ops: Mutex<Ops>,
    /// 运行时短锁：代理运行状态与可达性缓存（绝不跨 `await` 持有）
    runtime: std::sync::Mutex<Runtime>,
    /// 最近连接：代理任务在锁外直接写入
    recent: RecentConnections,
}

/// 操作锁保护的状态。不变式：`proxy.is_some()` 时运行时的 `proxy_state == Running`。
#[derive(Default)]
struct Ops {
    /// 本进程启动的代理
    proxy: Option<ProxyHandle>,
}

/// 运行时短锁保护的状态。`proxy_state` 只在持有操作锁时写入；可达性缓存随时可写。
#[derive(Debug, Clone, Copy, Default)]
struct Runtime {
    proxy_state: ProxyState,
    gateway: Reachability,
    socks5: Reachability,
}

impl Manager {
    pub fn new(options: ManagerOptions) -> Self {
        Self {
            options,
            ops: Mutex::new(Ops::default()),
            runtime: std::sync::Mutex::new(Runtime::default()),
            recent: RecentConnections::new(),
        }
    }

    /// 当前状态快照（读文件 + 运行时短锁快照，不做网络探测）。
    ///
    /// 不等待操作锁：启用进行中（Key 校验、代理自检）也立即返回，可能反映操作的中间状态。
    pub async fn status(&self) -> Status {
        self.build_status()
    }

    /// 启用（设计 §5.1）：校验 Key → 写凭据 → 记录原值 → 校正 config.toml → 启动代理并自检
    /// → 写 .env → 状态检测。任一步失败撤销已完成步骤。
    ///
    /// 失败时凭据、config.toml（含备份文件）、.env 与状态文件恢复为调用前的内容，只停止本次新启动的
    /// 代理。第 7 步的网络可达性检测不在这里做（不阻断启用）：桌面层启用后调用
    /// [`Manager::probe_reachability`]。
    ///
    /// 预检与 Key 校验在操作锁之外进行（不阻塞状态查询、退出与其他操作）；取得操作锁后重新预检，
    /// 以锁内看到的状态为准执行写入步骤。
    pub async fn enable(&self, request: EnableRequest) -> Result<Status, ManagerError> {
        match self.enable_inner(request).await {
            Ok(status) => {
                log::event(
                    "manager.enable_ok",
                    json!({ "port": self.options.proxy_port, "proxy": status.proxy.state }),
                );
                Ok(status)
            }
            Err(failure) => {
                log::event(
                    "manager.enable_failed",
                    json!({ "code": failure.error.code(), "step": failure.step }),
                );
                Err(failure.error)
            }
        }
    }

    /// 仅校验并保存 / 替换 Key（“重新录入”），不改变启用状态。Key 校验在操作锁之外进行。
    pub async fn save_key(&self, request: SaveKeyRequest) -> Result<Status, ManagerError> {
        let result = async {
            let key = self.check_new_key(&request.key, request.force_save).await?;
            let _ops = self.ops.lock().await;
            self.options
                .credentials
                .write(&key)
                .map_err(credential_error)?;
            Ok(self.build_status())
        }
        .await;
        log::event(
            "manager.save_key",
            json!({ "ok": result.is_ok(), "code": result.as_ref().err().map(ManagerError::code) }),
        );
        result
    }

    /// 停用（设计 §5.2）：移除 .env 块 → 还原 config.toml → 停止代理 → 更新状态文件。
    pub async fn disable(&self) -> Result<Status, ManagerError> {
        let mut ops = self.ops.lock().await;
        let result = self.disable_locked(&mut ops).await;
        log::event(
            "manager.disable",
            json!({ "ok": result.is_ok(), "code": result.as_ref().err().map(ManagerError::code) }),
        );
        result?;
        Ok(self.build_status())
    }

    /// 删除凭据（“清除 Key”）。已启用时 Codex 请求将 fail-closed。
    pub async fn clear_key(&self) -> Result<Status, ManagerError> {
        let _ops = self.ops.lock().await;
        let result = self.options.credentials.delete().map_err(credential_error);
        log::event(
            "manager.clear_key",
            json!({ "ok": result.is_ok(), "code": result.as_ref().err().map(ManagerError::code) }),
        );
        result?;
        Ok(self.build_status())
    }

    /// 工具启动对账（设计 §5.3）。
    pub async fn reconcile_on_startup(&self) -> ReconcileReport {
        let mut ops = self.ops.lock().await;
        let (outcome, error) = match self.reconcile_locked(&mut ops).await {
            Ok(outcome) => (outcome, None),
            Err(error) => (ReconcileOutcome::Failed, Some(error)),
        };
        log::event(
            "manager.reconcile",
            json!({ "outcome": outcome, "code": error.as_ref().map(ManagerError::code) }),
        );
        ReconcileReport {
            outcome,
            error: error.map(|error| error.payload()),
            status: self.build_status(),
        }
    }

    /// 停用状态下的一键清理残留（等价于 §5.4 不删除凭据的清理）。
    pub async fn cleanup_residue(&self) -> Result<Status, ManagerError> {
        let mut ops = self.ops.lock().await;
        let result = async {
            let mut snapshot = read_state(&self.options.paths.state_file())?;
            quarantine_corrupt_state(&self.options.paths, &mut snapshot);
            let (report, error) = cleanup_files(&self.options.paths, &snapshot);
            self.stop_proxy(&mut ops).await;
            log_cleanup(&report, error.as_ref());
            error.map_or(Ok(()), Err)
        }
        .await;
        result?;
        Ok(self.build_status())
    }

    /// 一键移除 `.env` 中的 Codex++ 旧块。
    pub async fn remove_legacy_env_block(&self) -> Result<Status, ManagerError> {
        let _ops = self.ops.lock().await;
        let env_path = self.options.paths.env_file();
        let result = codex_env::remove_legacy_block_from_file(&env_path)
            .map_err(|error| io_error(&env_path, error));
        log::event(
            "manager.remove_legacy_env_block",
            json!({ "ok": result.is_ok(), "removed": result.as_ref().ok() }),
        );
        result?;
        Ok(self.build_status())
    }

    /// 记录开机自启开关到状态文件（注册表项由桌面应用负责）。
    pub async fn set_autostart_flag(&self, enabled: bool) -> Result<Status, ManagerError> {
        let _ops = self.ops.lock().await;
        let paths = &self.options.paths;
        let mut snapshot = read_state(&paths.state_file())?;
        quarantine_corrupt_state(paths, &mut snapshot);
        if !snapshot.exists || snapshot.state.autostart != enabled {
            let mut next = snapshot.state.clone();
            next.autostart = enabled;
            save_state(paths, &next)?;
        }
        log::event("manager.set_autostart", json!({ "autostart": enabled }));
        Ok(self.build_status())
    }

    /// 检测网关与 SOCKS5 的 TCP 可达性并缓存结果，返回最新状态。
    pub async fn probe_reachability(&self) -> Status {
        // 网络探测不持任何锁、不等待操作锁；只在写缓存时持运行时短锁。
        let options = &self.options;
        let (gateway, socks5) = tokio::join!(
            gateway::tcp_reachable(
                &options.gateway_host,
                options.gateway_port,
                gateway::TCP_PROBE_TIMEOUT
            ),
            gateway::tcp_reachable(
                &options.socks5_host,
                options.socks5_port,
                gateway::TCP_PROBE_TIMEOUT
            ),
        );
        {
            let mut runtime = self.runtime();
            runtime.gateway = reachability(gateway);
            runtime.socks5 = reachability(socks5);
        }
        self.build_status()
    }

    /// 最近连接（最新在前，最多 50 条）。
    pub fn recent_connections(&self) -> Vec<ConnectionRecord> {
        self.recent.snapshot()
    }

    /// 退出前停止本进程的代理（不改配置：fail-closed 为有意行为）。
    ///
    /// 与写入步骤互斥（避免与正在进行的启用交错），但不等待 Key 校验：校验在操作锁之外进行。
    pub async fn shutdown(&self) {
        let mut ops = self.ops.lock().await;
        self.stop_proxy(&mut ops).await;
        log::event(
            "manager.shutdown",
            json!({ "port": self.options.proxy_port }),
        );
    }

    // ---- 启用 ----

    async fn enable_inner(&self, request: EnableRequest) -> Result<Status, StepError> {
        let EnableRequest {
            key,
            force_save,
            confirm_remove_catalog,
        } = request;
        let preflight = StepError::at("preflight");
        let verify = StepError::at("verify_key");

        // 0. 预检（锁外、只读）：配置无法处理或需要确认 catalog 时不做 Key 校验
        self.enable_preflight(confirm_remove_catalog)
            .map_err(preflight)?;

        // 1. 新 Key 先校验（锁外：网络校验最长 15 秒，期间不阻塞状态查询、退出与其他操作）
        let new_key = match key {
            Some(raw) => Some(self.check_new_key(&raw, force_save).await.map_err(verify)?),
            None => None,
        };

        let mut ops = self.ops.lock().await;
        // 等锁期间文件可能已被其他操作改变：锁内重新预检，以此为准
        let (mut snapshot, inspection) = self
            .enable_preflight(confirm_remove_catalog)
            .map_err(preflight)?;
        // 未录入新 Key：凭据必须已存在
        if new_key.is_none() {
            match self.options.credentials.read() {
                Ok(Some(_)) => {}
                Ok(None) => return Err(verify(ManagerError::CredentialMissing)),
                Err(error) => return Err(verify(credential_error(error))),
            }
        }

        let mut undo = EnableUndo::default();
        let result = self
            .enable_steps(
                &mut ops,
                &mut snapshot,
                &inspection,
                new_key.as_deref(),
                &mut undo,
            )
            .await;
        if let Err(failure) = result {
            self.rollback_enable(&mut ops, undo).await;
            return Err(failure);
        }
        Ok(self.build_status())
    }

    /// 启用预检（只读、不产生任何写入）：读状态文件、检查 config.toml、catalog 指针须已确认。
    fn enable_preflight(
        &self,
        confirm_remove_catalog: bool,
    ) -> Result<(StateSnapshot, ConfigInspection), ManagerError> {
        let snapshot = read_state(&self.options.paths.state_file())?;
        let inspection = self.inspect_config()?;
        if let Some(path) = &inspection.model_catalog_json
            && !confirm_remove_catalog
        {
            return Err(ManagerError::CatalogConfirmationRequired { path: path.clone() });
        }
        Ok((snapshot, inspection))
    }

    /// 第 2–7 步。每完成一步即登记撤销信息，失败由调用方按相反顺序撤销。
    async fn enable_steps(
        &self,
        ops: &mut Ops,
        snapshot: &mut StateSnapshot,
        inspection: &ConfigInspection,
        new_key: Option<&str>,
        undo: &mut EnableUndo,
    ) -> Result<(), StepError> {
        let paths = &self.options.paths;

        // 2. 写凭据（仅有新 Key 时）：先读出旧凭据用于回滚
        if let Some(key) = new_key {
            let step = StepError::at("write_credential");
            let credentials = &self.options.credentials;
            let old = credentials.read().map_err(credential_error).map_err(step)?;
            undo.credential = Some(old);
            credentials
                .write(key)
                .map_err(credential_error)
                .map_err(step)?;
        }

        // 3. 记录原值：仅在“停用 → 启用”的转换时；已启用时绝不覆盖 previous_*
        let mut next = snapshot.state.clone();
        let transition = !snapshot.state.enabled;
        if transition {
            let step = StepError::at("record_previous");
            undo.state = Some(snapshot.raw.clone());
            quarantine_corrupt_state(paths, snapshot);
            // 上次启用在记录原值之后中断：当前 model_provider 是受管残留，已记录的原值才是用户的
            // 真实原值，保留不覆盖
            let recorded = snapshot.state.previous();
            let interrupted = inspection.model_provider.as_deref() == Some(consts::PROVIDER_ID)
                && recorded != PreviousConfig::default();
            if interrupted {
                log::event("manager.keep_recorded_previous", json!({}));
            } else {
                next.set_previous(inspection.previous());
            }
            save_state(paths, &next).map_err(step)?;
        }

        // 4. 校正 config.toml（apply_managed 内部先备份再原子写入）
        let step = StepError::at("apply_config");
        let config_path = paths.config_toml();
        let backup_path = paths.config_backup();
        undo.backup =
            Some(read_raw(&backup_path).map_err(|error| step(io_error(&backup_path, error)))?);
        let outcome = codex_config::apply_managed(
            &config_path,
            &backup_path,
            &self.options.credential_command,
        )
        .map_err(|error| step(config_error(error, &config_path)))?;
        undo.config = Some(outcome);

        // 5. 启动代理并自检（先启代理再写 .env，避免 .env 指向无人监听的端口）
        let before = self.proxy_state();
        let started = self
            .ensure_proxy(ops)
            .await
            .map_err(StepError::at("start_proxy"))?;
        undo.proxy = Some(ProxyUndo { before, started });

        // 6. 写 .env 受管块
        let step = StepError::at("write_env");
        let env_path = paths.env_file();
        let env_before = read_raw(&env_path).map_err(|error| step(io_error(&env_path, error)))?;
        let changed = codex_env::write_block_to_file(&env_path, self.options.proxy_port)
            .map_err(|error| step(io_error(&env_path, error)))?;
        if changed {
            undo.env = Some(env_before);
        }

        // 7. 置为已启用
        if transition {
            next.enabled = true;
            save_state(paths, &next).map_err(StepError::at("save_state"))?;
        }
        Ok(())
    }

    /// 按相反顺序撤销已完成的步骤。撤销失败只记日志，调用方仍返回原始错误。
    async fn rollback_enable(&self, ops: &mut Ops, undo: EnableUndo) {
        let paths = &self.options.paths;
        if let Some(raw) = undo.env {
            log_rollback(
                "write_env",
                restore_raw(&paths.env_file(), raw.as_deref()).is_ok(),
            );
        }
        if let Some(ProxyUndo { before, started }) = undo.proxy {
            if started {
                // 只停止本步新启动的代理；复用的本进程代理与外部实例不动
                if let Some(handle) = ops.proxy.take() {
                    handle.shutdown().await;
                }
                self.set_proxy_state(ProxyState::Stopped);
                log_rollback("start_proxy", true);
            } else {
                self.set_proxy_state(before);
            }
        }
        if let Some(outcome) = undo.config {
            let ok = codex_config::rollback(&paths.config_toml(), &outcome).is_ok();
            log_rollback("apply_config", ok);
        }
        if let Some(raw) = undo.backup {
            let ok = restore_raw(&paths.config_backup(), raw.as_deref()).is_ok();
            log_rollback("config_backup", ok);
        }
        if let Some(raw) = undo.state {
            let ok = restore_raw(&paths.state_file(), raw.as_deref()).is_ok();
            log_rollback("record_previous", ok);
        }
        if let Some(old) = undo.credential {
            let credentials = &self.options.credentials;
            let ok = match old {
                Some(secret) => credentials.write(secret.expose()).is_ok(),
                None => credentials.delete().is_ok(),
            };
            log_rollback("write_credential", ok);
        }
    }

    /// trim → 空值拒绝 → `GET /v1/models` 校验。5xx / 超时仅在 `force_save` 时放行。
    async fn check_new_key(&self, raw: &str, force_save: bool) -> Result<String, ManagerError> {
        let key = raw.trim();
        if key.is_empty() {
            return Err(ManagerError::EmptyKey);
        }
        let check = gateway::verify_key_at(
            &self.options.gateway_base_url,
            key,
            gateway::KEY_CHECK_TIMEOUT,
        )
        .await;
        log::event(
            "manager.key_check",
            json!({ "result": check, "force": force_save }),
        );
        match check {
            KeyCheck::Ok => {}
            KeyCheck::Unauthorized => return Err(ManagerError::KeyRejected),
            KeyCheck::ServerError if !force_save => {
                return Err(ManagerError::GatewayUnavailable {
                    reason: "网关返回服务器错误 5xx".to_string(),
                });
            }
            KeyCheck::TimeoutOrNetwork if !force_save => {
                return Err(ManagerError::GatewayUnavailable {
                    reason: "连接网关超时或网络错误".to_string(),
                });
            }
            KeyCheck::ServerError | KeyCheck::TimeoutOrNetwork => {}
        }
        Ok(key.to_string())
    }

    // ---- 停用 ----

    async fn disable_locked(&self, ops: &mut Ops) -> Result<(), ManagerError> {
        let paths = &self.options.paths;
        let mut snapshot = read_state(&paths.state_file())?;
        // 预检：配置无法解析或结构不符时不做任何修改，用户先修复配置
        self.inspect_config()?;
        quarantine_corrupt_state(paths, &mut snapshot);

        let mut first_error: Option<ManagerError> = None;

        // 1. 移除 .env 受管块
        let env_path = paths.env_file();
        if let Err(error) = codex_env::remove_block_from_file(&env_path) {
            first_error.get_or_insert(io_error(&env_path, error));
        }

        // 2. 还原 config.toml：已启用按记录的原值定点还原；否则清理受管残留（含启用中断的情形）
        let config_path = paths.config_toml();
        let config_restored = match restore_config(&config_path, &snapshot.state) {
            Ok(_) => true,
            Err(error) => {
                first_error.get_or_insert(config_error(error, &config_path));
                false
            }
        };

        // 3. 停止本进程代理
        self.stop_proxy(ops).await;

        // 4. 配置已还原才置为停用；否则保持已启用，下次启动对账按已启用重新接管
        if config_restored && snapshot.exists {
            let mut next = snapshot.state.clone();
            next.enabled = false;
            next.clear_previous();
            if next != snapshot.state
                && let Err(error) = save_state(paths, &next)
            {
                first_error.get_or_insert(error);
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    // ---- 对账 ----

    async fn reconcile_locked(&self, ops: &mut Ops) -> Result<ReconcileOutcome, ManagerError> {
        let paths = &self.options.paths;
        let mut snapshot = read_state(&paths.state_file())?;
        quarantine_corrupt_state(paths, &mut snapshot);

        if !snapshot.state.enabled {
            // 对结构宽松的残留判定（与一键清理一致）：结构不符的受管节同样提示清理
            let config_residue = codex_config::has_residue(&paths.config_toml()).unwrap_or(false);
            let env_block = codex_env::inspect_file(&paths.env_file(), self.options.proxy_port)
                .is_ok_and(|inspection| inspection.has_block);
            return Ok(if config_residue || env_block {
                ReconcileOutcome::ResidueFound
            } else {
                ReconcileOutcome::Idle
            });
        }

        // 已启用但凭据缺失：不启动代理、不改任何文件（fail-closed）
        match self.options.credentials.read() {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(ReconcileOutcome::NeedsKey),
            Err(error) => return Err(credential_error(error)),
        }

        // 幂等重做第 4–6 步，不记录原值
        let config_path = paths.config_toml();
        codex_config::apply_managed(
            &config_path,
            &paths.config_backup(),
            &self.options.credential_command,
        )
        .map_err(|error| config_error(error, &config_path))?;
        // 端口被他人占用时在这里失败，已存在的 .env 不被改动
        self.ensure_proxy(ops).await?;
        let env_path = paths.env_file();
        codex_env::write_block_to_file(&env_path, self.options.proxy_port)
            .map_err(|error| io_error(&env_path, error))?;
        Ok(ReconcileOutcome::Applied)
    }

    // ---- 代理 ----

    /// 确保本地代理可用：本进程已持有 → 复用；端口上是本工具另一实例 → 视为 External 复用；
    /// 否则启动。返回本次是否新启动了代理（回滚时只停止它）。
    async fn ensure_proxy(&self, ops: &mut Ops) -> Result<bool, ManagerError> {
        if ops.proxy.is_some() {
            self.set_proxy_state(ProxyState::Running);
            return Ok(false);
        }
        let port = self.options.proxy_port;
        if proxy::probe_existing(port).await {
            self.set_proxy_state(ProxyState::External);
            log::event("manager.proxy_external", json!({ "port": port }));
            return Ok(false);
        }
        match proxy::spawn(port, self.proxy_config()).await {
            Ok(handle) => {
                ops.proxy = Some(handle);
                self.set_proxy_state(ProxyState::Running);
                Ok(true)
            }
            Err(error) => {
                self.set_proxy_state(ProxyState::Stopped);
                log::event("manager.proxy_start_failed", json!({ "port": port }));
                Err(match error {
                    ProxyStartError::PortUnavailable { port, reason } => {
                        ManagerError::PortUnavailable { port, reason }
                    }
                    ProxyStartError::Io(error) => ManagerError::Internal {
                        message: format!("启动本地代理失败：{error}"),
                    },
                })
            }
        }
    }

    /// 停止本进程代理。External 实例不属于本进程、无法停止，状态直接置为 Stopped 并记日志。
    async fn stop_proxy(&self, ops: &mut Ops) {
        let port = self.options.proxy_port;
        if let Some(handle) = ops.proxy.take() {
            handle.shutdown().await;
            log::event("manager.proxy_stopped", json!({ "port": port }));
        } else if self.proxy_state() == ProxyState::External {
            log::event("manager.proxy_external_left", json!({ "port": port }));
        }
        self.set_proxy_state(ProxyState::Stopped);
    }

    /// 代理参数：连接记录先进环形缓冲，再交给外部回调。回调运行在代理任务里，绝不获取任何编排锁。
    fn proxy_config(&self) -> ProxyConfig {
        let recent = self.recent.clone();
        let external = self.options.on_record.clone();
        let sink: RecordSink = Arc::new(move |record: ConnectionRecord| {
            recent.push(record.clone());
            if let Some(external) = &external {
                external(record);
            }
        });
        ProxyConfig {
            socks5_host: self.options.socks5_host.clone(),
            socks5_port: self.options.socks5_port,
            on_record: Some(sink),
            direct_overrides: None,
        }
    }

    // ---- 状态 ----

    /// 运行时短锁。只在读写字段时持有，绝不跨 `await`。
    fn runtime(&self) -> MutexGuard<'_, Runtime> {
        self.runtime.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn proxy_state(&self) -> ProxyState {
        self.runtime().proxy_state
    }

    /// 只应在持有操作锁时调用。
    fn set_proxy_state(&self, state: ProxyState) {
        self.runtime().proxy_state = state;
    }

    /// 检查 config.toml：无法解析、根键类型不符或受管结构不是 table 时返回 `ConfigInvalid`。
    fn inspect_config(&self) -> Result<ConfigInspection, ManagerError> {
        let config_path = self.options.paths.config_toml();
        codex_config::inspect(&config_path, &self.options.credential_command)
            .map_err(|error| config_error(error, &config_path))
    }

    /// 状态快照：读文件 + 运行时短锁快照，不获取操作锁。
    fn build_status(&self) -> Status {
        let options = &self.options;
        let paths = &options.paths;
        let state = match read_state(&paths.state_file()) {
            Ok(snapshot) => snapshot.state,
            Err(_) => HelperState::default(),
        };
        let key_configured = options.credentials.exists();

        let config_path = paths.config_toml();
        let (inspection, config_error_message) =
            match codex_config::inspect(&config_path, &options.credential_command) {
                Ok(inspection) => (inspection, None),
                Err(error) => (
                    ConfigInspection::default(),
                    Some(config_error(error, &config_path).to_string()),
                ),
            };
        // inspect 因结构不符报错时，残留改用对结构宽松的判定（与一键清理一致）
        let config_residue = if config_error_message.is_none() {
            inspection.has_managed_residue()
        } else {
            codex_config::has_residue(&config_path).unwrap_or(false)
        };
        let env =
            codex_env::inspect_file(&paths.env_file(), options.proxy_port).unwrap_or_default();
        let runtime = *self.runtime();

        Status {
            platform_supported: cfg!(windows),
            enabled: state.enabled,
            key_configured,
            needs_key: state.enabled && !key_configured,
            proxy: ProxyStatus {
                state: runtime.proxy_state,
                port: options.proxy_port,
            },
            gateway: runtime.gateway,
            socks5: runtime.socks5,
            gateway_addr: format!("{}:{}", options.gateway_host, options.gateway_port),
            socks5_addr: format!("{}:{}", options.socks5_host, options.socks5_port),
            config_managed: inspection.fully_managed,
            env_managed: env.block_up_to_date,
            residue: !state.enabled && (config_residue || env.has_block),
            legacy_env_block: env.has_legacy_block,
            model_catalog_json: inspection.model_catalog_json,
            config_error: config_error_message,
            autostart: state.autostart,
            codex_home: paths.codex_home.display().to_string(),
        }
    }
}

/// 卸载清理（设计 §5.4），供 `codex-helper-credential.exe cleanup [--purge-key]` 调用。
///
/// 执行 §5.2 的第 1、2、4 步；状态文件缺失时按残留规则清理；`purge_key` 时同时删除凭据。
/// 文件或块不存在都视为成功。
///
/// 尽力完成全部步骤：任一步失败仍继续其余步骤，最后返回遇到的第一个错误；全部成功时返回如实的
/// 报告（只有真的发生改动的项才为 `true`）。状态文件损坏视为缺失（不改名、不覆盖）。
pub fn cleanup(
    paths: &HelperPaths,
    credentials: &dyn CredentialStore,
    purge_key: bool,
) -> anyhow::Result<CleanupReport> {
    let (snapshot, read_error) = match read_state(&paths.state_file()) {
        Ok(snapshot) if snapshot.corrupt => {
            log::event("manager.state_corrupt", json!({ "renamed": false }));
            (StateSnapshot::missing(), None)
        }
        Ok(snapshot) => (snapshot, None),
        // 无法读取：不知道是否已启用，按残留规则清理（不会删除用户自己的 model_provider）
        Err(error) => (StateSnapshot::missing(), Some(error)),
    };
    let (mut report, files_error) = cleanup_files(paths, &snapshot);
    let mut first_error = read_error.or(files_error);

    if purge_key {
        let existed = credentials
            .read()
            .map(|secret| secret.is_some())
            .unwrap_or(true);
        match credentials.delete() {
            Ok(()) => report.key_purged = existed,
            Err(error) => {
                first_error.get_or_insert(credential_error(error));
            }
        }
    }

    log_cleanup(&report, first_error.as_ref());
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(report),
    }
}

// ---- 内部类型与辅助函数 ----

/// 失败的步骤名（写日志）与错误。
struct StepError {
    step: &'static str,
    error: ManagerError,
}

impl StepError {
    /// 生成把错误标注为指定步骤的映射函数。
    fn at(step: &'static str) -> impl Fn(ManagerError) -> StepError + Copy {
        move |error| StepError { step, error }
    }
}

/// 启用各步的撤销信息。`Some` 表示该步已产生（或可能产生）改动，需要撤销。
#[derive(Default)]
struct EnableUndo {
    /// 第 2 步：调用前的凭据（`None` 表示原先没有凭据，回滚时删除）
    credential: Option<Option<Secret>>,
    /// 第 3 / 7 步：调用前状态文件的原始字节（`None` 表示原先不存在）
    state: Option<Option<Vec<u8>>>,
    /// 第 4 步：调用前备份文件的原始字节
    backup: Option<Option<Vec<u8>>>,
    /// 第 4 步：`apply_managed` 的结果
    config: Option<ApplyOutcome>,
    /// 第 5 步
    proxy: Option<ProxyUndo>,
    /// 第 6 步：调用前 `.env` 的原始字节（仅在实际写入时登记）
    env: Option<Option<Vec<u8>>>,
}

struct ProxyUndo {
    /// 调用前的代理状态
    before: ProxyState,
    /// 本步是否新启动了代理
    started: bool,
}

/// 读取到的状态文件。
struct StateSnapshot {
    /// 调用前的原始字节（回滚时逐字节写回）；文件不存在为 `None`
    raw: Option<Vec<u8>>,
    /// 解析结果；文件不存在或损坏时为默认值
    state: HelperState,
    /// 文件存在且解析成功
    exists: bool,
    /// 文件存在但内容损坏（尚未改名）
    corrupt: bool,
}

impl StateSnapshot {
    fn missing() -> Self {
        Self {
            raw: None,
            state: HelperState::default(),
            exists: false,
            corrupt: false,
        }
    }
}

/// 读取状态文件：不存在 → 默认值；内容损坏 → 默认值并标记 `corrupt`；其他读取失败 → `Io`。
fn read_state(path: &Path) -> Result<StateSnapshot, ManagerError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StateSnapshot::missing());
        }
        Err(error) => return Err(io_error(path, error)),
    };
    // 文件刚读取成功，此处的失败即内容损坏
    match state::load(path) {
        Ok(Some(state)) => Ok(StateSnapshot {
            raw: Some(raw),
            state,
            exists: true,
            corrupt: false,
        }),
        Ok(None) => Ok(StateSnapshot::missing()),
        Err(_) => Ok(StateSnapshot {
            raw: Some(raw),
            state: HelperState::default(),
            exists: false,
            corrupt: true,
        }),
    }
}

/// 把损坏的状态文件改名为 `state.json.corrupt`（覆盖）后按默认状态继续。
///
/// 改名失败只记日志：随后的保存会原子覆盖损坏文件。`raw` 保持不变，启用回滚时仍写回原内容。
fn quarantine_corrupt_state(paths: &HelperPaths, snapshot: &mut StateSnapshot) {
    if !snapshot.corrupt {
        return;
    }
    let state_path = paths.state_file();
    let renamed = std::fs::rename(&state_path, state_path.with_extension("json.corrupt")).is_ok();
    log::event("manager.state_corrupt", json!({ "renamed": renamed }));
    snapshot.corrupt = false;
}

fn save_state(paths: &HelperPaths, state: &HelperState) -> Result<(), ManagerError> {
    state::save(&paths.state_file(), state).map_err(|error| ManagerError::Io {
        message: format!("{error:#}"),
    })
}

/// 读取原始字节；不存在返回 `None`。
fn read_raw(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 把文件恢复为给定的原始字节（`None` 则删除）；已一致时不写。
fn restore_raw(path: &Path, raw: Option<&[u8]>) -> std::io::Result<()> {
    if read_raw(path)?.as_deref() == raw {
        return Ok(());
    }
    match raw {
        Some(bytes) => fsutil::atomic_write(path, bytes),
        None => fsutil::remove_file_if_exists(path).map(|_| ()),
    }
}

/// 按状态文件还原 config.toml（结构宽松，不经 `inspect` 判定）：
///
/// - 已启用 → 按记录的原值定点还原；
/// - 停用（或状态文件缺失 / 损坏，按默认状态）→ 只清理受管残留，用户自己的 model_provider 不动；
///   但 model_provider 仍指向受管 provider 且状态文件留有原值（启用中断）时按原值还原。
fn restore_config(config_path: &Path, state: &HelperState) -> Result<bool, ConfigError> {
    if state.enabled {
        codex_config::restore(config_path, &state.previous())
    } else {
        codex_config::remove_residue_with(config_path, &state.previous())
    }
}

/// §5.4 清理的共同实现（不含凭据）：移除 `.env` 块 → 还原配置 → 重置状态文件。
///
/// 尽力完成全部步骤，返回如实的报告与遇到的第一个错误。状态文件只在存在时更新（保留 autostart），
/// 且只在配置已还原成功后才置为停用。
fn cleanup_files(
    paths: &HelperPaths,
    snapshot: &StateSnapshot,
) -> (CleanupReport, Option<ManagerError>) {
    let mut report = CleanupReport::default();
    let mut first_error: Option<ManagerError> = None;

    let env_path = paths.env_file();
    match codex_env::remove_block_from_file(&env_path) {
        Ok(changed) => report.env_block_removed = changed,
        Err(error) => {
            first_error.get_or_insert(io_error(&env_path, error));
        }
    }

    // 已启用 → 按原值还原；停用或状态文件缺失 → 只删受管残留（用户自己的 model_provider 不动，
    // 启用中断时按记录的原值还原）。状态文件缺失或损坏时 `snapshot.state` 为默认值。
    let config_path = paths.config_toml();
    let state = snapshot.exists.then_some(&snapshot.state);
    let config_ok = match restore_config(&config_path, &snapshot.state) {
        Ok(changed) => {
            report.config_restored = changed;
            true
        }
        Err(error) => {
            first_error.get_or_insert(config_error(error, &config_path));
            false
        }
    };

    if let Some(state) = state
        && config_ok
    {
        let mut next = state.clone();
        next.enabled = false;
        next.clear_previous();
        if next != *state {
            match save_state(paths, &next) {
                Ok(()) => report.state_reset = true,
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
    }

    (report, first_error)
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> ManagerError {
    ManagerError::Io {
        message: format!("{}：{error}", path.display()),
    }
}

fn config_error(error: ConfigError, config_path: &Path) -> ManagerError {
    match error {
        ConfigError::Parse { message } => ManagerError::ConfigInvalid { message },
        ConfigError::NotATable { key } => ManagerError::ConfigInvalid {
            message: format!("{key} 不是 table"),
        },
        ConfigError::Io(error) => io_error(config_path, error),
    }
}

/// 凭据存储的错误不携带 Token（见 `credential` 模块），可直接展示。
fn credential_error(error: anyhow::Error) -> ManagerError {
    ManagerError::Credential {
        message: format!("{error:#}"),
    }
}

fn reachability(reachable: bool) -> Reachability {
    if reachable {
        Reachability::Reachable
    } else {
        Reachability::Unreachable
    }
}

fn log_rollback(step: &str, ok: bool) {
    log::event("manager.rollback", json!({ "step": step, "ok": ok }));
}

fn log_cleanup(report: &CleanupReport, error: Option<&ManagerError>) {
    log::event(
        "manager.cleanup",
        json!({
            "env_block_removed": report.env_block_removed,
            "config_restored": report.config_restored,
            "state_reset": report.state_reset,
            "key_purged": report.key_purged,
            "code": error.map(ManagerError::code),
        }),
    );
}
