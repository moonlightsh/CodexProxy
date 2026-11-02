//! 组件文件下载 IPC；目录由系统解析，网络状态由 Manager 提供，不接收前端 URL 或路径。

use helper_core::component_downloads::{
    DownloadError, DownloadManager, DownloadProgress, DownloadRequest, DownloadResult,
};
use helper_core::component_updates;
use tauri::{AppHandle, Manager as _, ipc::Channel};

use crate::commands::AppState;

#[tauri::command]
pub async fn download_component(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    downloads: tauri::State<'_, DownloadManager>,
    request: DownloadRequest,
    on_progress: Channel<DownloadProgress>,
) -> Result<DownloadResult, DownloadError> {
    let directory = app
        .path()
        .download_dir()
        .map_err(|_| DownloadError {
            code: "downloadFile",
            message: "无法定位系统下载目录，请检查系统用户目录设置".into(),
        })?
        .join("CodexHelper");
    let status = state.manager.status().await;
    downloads
        .download(
            request,
            component_updates::network_path_for(&status),
            &directory,
            |progress| {
                let _ = on_progress.send(progress);
            },
        )
        .await
}

#[tauri::command]
pub fn cancel_component_download(downloads: tauri::State<'_, DownloadManager>, id: String) -> bool {
    downloads.cancel(&id)
}
