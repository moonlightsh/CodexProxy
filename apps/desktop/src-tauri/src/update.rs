//! 内部升级渠道（设计 §15）：把 HTTP 服务器上的版本 manifest 作为升级检测地址，
//! 复用 `tauri-plugin-updater` 完成检测 / 下载 / Ed25519 签名校验 / NSIS 安装。
//!
//! 检测地址优先级：环境变量 `CODEX_HELPER_UPDATE_URL`（运行时覆盖）> `tauri.conf.json`
//! 里的 `plugins.updater.endpoints`（构建期默认）。签名公钥固定写在 `tauri.conf.json`，
//! 私钥仅在构建签名时通过 `TAURI_SIGNING_PRIVATE_KEY` 使用，绝不入库。
//!
//! 运行时覆盖 endpoints 的 API 只在 Rust 侧可用，因此升级逻辑封装为命令，前端只调用
//! `check_update` / `install_update`，不直接使用 JS 插件 API。

use helper_core::log;
use helper_core::types::ErrorPayload;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Url};
use tauri_plugin_updater::{Updater, UpdaterExt as _};

/// 运行时覆盖检测地址的环境变量（应急/内网自定义用；界面不暴露，与端口覆盖同一思路）。
pub const UPDATE_URL_ENV: &str = "CODEX_HELPER_UPDATE_URL";

/// 升级检测结果（返回给前端，serde camelCase 与 `types.ts` 的 `UpdateCheck` 对应）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// 是否有可用更新。
    pub available: bool,
    /// 当前运行版本。
    pub current_version: String,
    /// 可用的新版本号（`available` 为 true 时存在）。
    pub version: Option<String>,
    /// 更新说明（manifest 的 `notes` 字段，可能为空）。
    pub notes: Option<String>,
    /// 发布日期（RFC3339 文本，可能为空）。
    pub date: Option<String>,
}

/// 读取运行时覆盖的检测地址；未设置或空白时返回 `None`（改用构建期配置的 endpoints）。
fn override_endpoint() -> Option<String> {
    std::env::var(UPDATE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 构建 updater：存在环境变量覆盖时替换 endpoints，否则沿用 `tauri.conf.json` 的配置。
fn build_updater(app: &AppHandle) -> Result<Updater, ErrorPayload> {
    let mut builder = app.updater_builder();
    if let Some(url) = override_endpoint() {
        let endpoint = Url::parse(&url).map_err(|error| ErrorPayload {
            code: "update".to_string(),
            message: format!("检测地址无效（{UPDATE_URL_ENV}={url}）：{error}"),
        })?;
        builder = builder
            .endpoints(vec![endpoint])
            .map_err(|error| map_error("检测地址无效", error))?;
    }
    builder
        .build()
        .map_err(|error| map_error("初始化升级检测失败", error))
}

/// 把 updater 错误映射为前端错误载荷（统一 `code = "update"`）。
fn map_error(context: &str, error: tauri_plugin_updater::Error) -> ErrorPayload {
    ErrorPayload {
        code: "update".to_string(),
        message: format!("{context}：{error}"),
    }
}

/// 检测是否有可用更新（只检测、不下载）。前端“检查更新”按钮与启动自动检测都调用它。
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateCheck, ErrorPayload> {
    let current_version = app.package_info().version.to_string();
    let updater = build_updater(&app)?;
    match updater.check().await {
        Ok(Some(update)) => {
            log::event(
                "desktop.update.check",
                json!({ "available": true, "version": update.version }),
            );
            Ok(UpdateCheck {
                available: true,
                current_version,
                version: Some(update.version.clone()),
                notes: update.body.clone(),
                date: update.date.map(|date| date.to_string()),
            })
        }
        Ok(None) => {
            log::event("desktop.update.check", json!({ "available": false }));
            Ok(UpdateCheck {
                available: false,
                current_version,
                version: None,
                notes: None,
                date: None,
            })
        }
        Err(error) => {
            log::event("desktop.update.check", json!({ "ok": false }));
            Err(map_error("检查更新失败", error))
        }
    }
}

/// 下载并安装可用更新，成功后重启应用；无可用更新时返回错误由前端提示刷新。
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), ErrorPayload> {
    let updater = build_updater(&app)?;
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            return Err(ErrorPayload {
                code: "update".to_string(),
                message: "没有可安装的更新（可能已是最新版本）。".to_string(),
            });
        }
        Err(error) => return Err(map_error("检查更新失败", error)),
    };
    log::event(
        "desktop.update.install",
        json!({ "version": update.version }),
    );
    update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
        .map_err(|error| map_error("下载或安装更新失败", error))?;
    log::event("desktop.update.install", json!({ "ok": true }));
    // 安装器已在后台运行：退出并重启当前进程，让 NSIS 接管替换（`restart` 发散，不返回）。
    app.restart()
}
