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

/// 校验运行时覆盖的检测地址：仅允许 http/https，拒绝携带 userinfo 或 fragment 的 URL。
///
/// 返回的错误信息**不包含原始地址值**——未来 URL 可能携带令牌，避免其泄露到日志或前端提示。
fn validate_endpoint(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("无法解析为 URL：{error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("协议 {other} 不受支持（仅允许 http/https）")),
    }
    // authority 形如 "host:port" 或 "user[:pass]@host"；出现 '@' 即携带真实 userinfo，一律拒绝，
    // 避免未来凭据入地址。（空 userinfo 如 http://@host 会被 url 规范化为无 userinfo、
    // 等价普通 host、不携带凭据，因此不在此处单独拦截。）
    if url.authority().contains('@') {
        return Err("不允许在地址中携带用户信息（user[:pass]@）".to_string());
    }
    if url.fragment().is_some() {
        return Err("不允许在地址中携带片段（#...）".to_string());
    }
    Ok(url)
}

/// 临时安装位置（dmg / App Translocation）下尝试安装更新时的引导文案，
/// 与前端 `INSTALL_LOCATION_MESSAGE` 同义（macOS 设计 §4.2）。
pub const TRANSIENT_INSTALL_UPDATE_MESSAGE: &str =
    "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新";

/// 内部升级只在 Windows 与 macOS 上提供（与 `Status::update_supported` 同一判断）。
/// 其他平台直接拒绝，不读环境变量、不请求检测地址。
fn ensure_update_supported() -> Result<(), ErrorPayload> {
    if cfg!(any(windows, target_os = "macos")) {
        Ok(())
    } else {
        Err(ErrorPayload {
            code: "update".to_string(),
            message: "当前平台不支持内部升级，请到 GitHub Release 下载新版本".to_string(),
        })
    }
}

/// 当前主程序是否运行在 macOS 临时安装位置（dmg 直接运行或 App Translocation）。
/// 判定核心抽成独立函数以便单测：`current_exe` 失败时视为正常（不拦升级）。
fn transient_run_location() -> bool {
    std::env::current_exe()
        .map(|path| helper_core::paths::is_transient_install_path(&path))
        .unwrap_or(false)
}

/// 拦截临时安装位置下的升级安装（macOS 设计 M3：检测可用、安装拦截，纵深防御）。
/// 仅 macOS 生效，其他平台直接放行（用 `cfg!` 而非 `#[cfg]`，保持两平台都能编译测试）。
fn ensure_install_location_ok() -> Result<(), ErrorPayload> {
    if cfg!(target_os = "macos") && transient_run_location() {
        Err(ErrorPayload {
            code: "update".to_string(),
            message: TRANSIENT_INSTALL_UPDATE_MESSAGE.to_string(),
        })
    } else {
        Ok(())
    }
}

/// 构建 updater：存在环境变量覆盖时替换 endpoints，否则沿用 `tauri.conf.json` 的配置。
fn build_updater(app: &AppHandle) -> Result<Updater, ErrorPayload> {
    let mut builder = app.updater_builder();
    if let Some(url) = override_endpoint() {
        let endpoint = validate_endpoint(&url).map_err(|reason| ErrorPayload {
            code: "update".to_string(),
            message: format!("{UPDATE_URL_ENV} 无效：{reason}"),
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
/// 非 Windows/macOS 平台直接返回 `update` 错误（临时安装位置不拦检测，设计 M3）。
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateCheck, ErrorPayload> {
    ensure_update_supported()?;
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

/// 返回当前运行版本（不依赖升级服务器，供界面始终展示本地版本）。
#[tauri::command]
pub fn app_version(app: AppHandle) -> Result<String, ErrorPayload> {
    Ok(app.package_info().version.to_string())
}

/// 下载并安装可用更新，成功后重启应用；无可用更新时返回错误由前端提示刷新。
/// 非 Windows/macOS 平台直接返回 `update` 错误；macOS 临时安装位置下拦截安装并引导。
///
/// `expected_version` 是前端在确认框里向用户展示并确认的版本。安装前重新检查一次，若
/// 服务器此刻返回的版本与用户确认的不一致，则拒绝安装，避免“确认 A 却装了 B”的竞态。
#[tauri::command]
pub async fn install_update(app: AppHandle, expected_version: String) -> Result<(), ErrorPayload> {
    ensure_update_supported()?;
    ensure_install_location_ok()?;
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
    if update.version != expected_version {
        log::event(
            "desktop.update.install",
            json!({ "ok": false, "reason": "version_changed" }),
        );
        return Err(ErrorPayload {
            code: "update".to_string(),
            message: "可用版本已变化，请重新检查更新后再安装。".to_string(),
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_commands_supported_on_windows_and_macos() {
        let result = ensure_update_supported();
        assert_eq!(result.is_ok(), cfg!(any(windows, target_os = "macos")));
        if let Err(e) = result {
            assert_eq!(e.code, "update");
            assert_eq!(
                e.message,
                "当前平台不支持内部升级，请到 GitHub Release 下载新版本"
            );
        }
    }

    #[test]
    fn install_location_message_matches_design() {
        assert_eq!(
            TRANSIENT_INSTALL_UPDATE_MESSAGE,
            "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn transient_run_location_false_for_normal_path() {
        // 正常 /Applications 安装下 current_exe 不含 /Volumes/ 或 /AppTranslocation/
        assert!(!transient_run_location());
    }

    #[test]
    fn validate_endpoint_accepts_http_and_https() {
        assert!(validate_endpoint("http://10.0.0.1:8080/codex-helper/latest.json").is_ok());
        assert!(validate_endpoint("https://example.internal/codex-helper/latest.json").is_ok());
    }

    #[test]
    fn validate_endpoint_rejects_non_http_scheme() {
        assert!(validate_endpoint("ftp://example.internal/latest.json").is_err());
        assert!(validate_endpoint("file:///etc/passwd").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_userinfo() {
        assert!(validate_endpoint("http://user:pass@host/latest.json").is_err());
        assert!(validate_endpoint("http://user@host/latest.json").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_fragment() {
        assert!(validate_endpoint("http://host/latest.json#frag").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_garbage() {
        assert!(validate_endpoint("not a url").is_err());
        assert!(validate_endpoint("").is_err());
    }

    #[test]
    fn update_check_serializes_camel_case() {
        let value = serde_json::to_value(UpdateCheck {
            available: true,
            current_version: "0.1.1".to_string(),
            version: Some("0.2.0".to_string()),
            notes: None,
            date: None,
        })
        .expect("serialize");
        assert!(value.get("currentVersion").is_some());
        assert!(value.get("current_version").is_none());
    }
}
