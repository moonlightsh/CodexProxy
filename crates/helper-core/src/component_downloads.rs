//! 受控的组件代理下载：URL 由后端生成，仅代理运行时可用；不安装或解压。
//! GitHub 每跳重定向显式验证，临时文件在失败/取消时由 RAII 清理。

use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::component_updates::{self, NetworkPath};
use crate::consts;

const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadComponent {
    ChatgptDesktop,
    CodexCli,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadRequest {
    pub id: String,
    pub component: DownloadComponent,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResult {
    pub path: String,
    pub file_name: String,
    pub received_bytes: u64,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct DownloadError {
    pub code: &'static str,
    pub message: String,
}

fn error(code: &'static str, message: impl Into<String>) -> DownloadError {
    DownloadError {
        code,
        message: message.into(),
    }
}

fn network_error(err: reqwest::Error) -> DownloadError {
    // reqwest 的原始错误可能含带签名参数的 URL，不送往日志或前端。
    error(
        "downloadNetwork",
        if err.is_timeout() {
            "下载超时，请检查代理后重试"
        } else {
            "下载连接失败或响应不完整，请检查代理后重试"
        },
    )
}

struct ActiveDownload {
    id: String,
    cancel: watch::Sender<bool>,
}

#[derive(Default)]
pub struct DownloadManager {
    active: Mutex<Option<ActiveDownload>>,
}

struct DownloadGuard<'a> {
    manager: &'a DownloadManager,
    id: String,
    receiver: watch::Receiver<bool>,
}

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        let mut active = self
            .manager
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if active.as_ref().is_some_and(|task| task.id == self.id) {
            *active = None;
        }
    }
}

impl DownloadManager {
    fn begin(&self, id: &str) -> Result<DownloadGuard<'_>, DownloadError> {
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(error("downloadRequest", "下载任务标识无效"));
        }
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() {
            return Err(error("downloadBusy", "已有下载正在进行，请稍后重试"));
        }
        let (cancel, receiver) = watch::channel(false);
        *active = Some(ActiveDownload {
            id: id.into(),
            cancel,
        });
        Ok(DownloadGuard {
            manager: self,
            id: id.into(),
            receiver,
        })
    }

    pub fn cancel(&self, id: &str) -> bool {
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        active
            .as_ref()
            .is_some_and(|task| task.id == id && task.cancel.send(true).is_ok())
    }

    /// 生产入口只接收组件和版本，不接受 URL。目录由桌面端系统 API 解析。
    pub async fn download(
        &self,
        request: DownloadRequest,
        network: NetworkPath,
        directory: &Path,
        progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<DownloadResult, DownloadError> {
        let client = download_client(&network)?;
        let guard = self.begin(&request.id)?;
        progress(DownloadProgress {
            received_bytes: 0,
            total_bytes: None,
        });
        with_cancel(
            async {
                let target = resolve_target(&client, &request).await?;
                save_download(&client, target, directory, &progress).await
            },
            guard.receiver.clone(),
        )
        .await
    }
}

async fn with_cancel(
    operation: impl Future<Output = Result<DownloadResult, DownloadError>>,
    mut receiver: watch::Receiver<bool>,
) -> Result<DownloadResult, DownloadError> {
    let cancelled = async {
        loop {
            if *receiver.borrow_and_update() {
                break;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        biased;
        () = cancelled => Err(error("downloadCancelled", "下载已取消")),
        result = tokio::time::timeout(Duration::from_secs(7200), operation) => {
            result.unwrap_or_else(|_| Err(error("downloadTimeout", "下载超过 2 小时，请重试")))
        }
    }
}

fn download_client(network: &NetworkPath) -> Result<reqwest::Client, DownloadError> {
    let NetworkPath::ViaProxy { proxy_url } = network else {
        return Err(error(
            "downloadProxy",
            "请先启用本地代理，再下载组件（不会回退直连）",
        ));
    };
    reqwest::Client::builder()
        .use_native_tls()
        .no_proxy()
        .proxy(
            reqwest::Proxy::all(proxy_url)
                .map_err(|_| error("downloadProxy", "本地代理地址无效"))?,
        )
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("codex-helper/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| error("downloadProxy", "无法创建代理下载客户端"))
}

fn cli_filename(os: &str, arch: &str) -> Result<String, DownloadError> {
    if !matches!(arch, "x86_64" | "aarch64") {
        return Err(error("downloadPlatform", "当前处理器架构暂不支持组件下载"));
    }
    match os {
        "windows" => Ok(format!("codex-{arch}-pc-windows-msvc.exe.zip")),
        "macos" => Ok(format!("codex-{arch}-apple-darwin.tar.gz")),
        _ => Err(error("downloadPlatform", "当前平台暂不支持组件下载")),
    }
}

struct DownloadTarget {
    component: DownloadComponent,
    filename: String,
    url: String,
}

fn cli_target(version: &str, os: &str, arch: &str) -> Result<DownloadTarget, DownloadError> {
    let tag = format!("rust-v{version}");
    if !component_updates::is_release_tag(&tag) {
        return Err(error("downloadRequest", "目标版本无效，请重新检查组件更新"));
    }
    let filename = cli_filename(os, arch)?;
    let url = format!("https://github.com/openai/codex/releases/download/{tag}/{filename}");
    Ok(DownloadTarget {
        component: DownloadComponent::CodexCli,
        filename,
        url,
    })
}

fn allowed_download_url(component: DownloadComponent, input: &str) -> bool {
    if !input.bytes().all(|b| b.is_ascii_graphic()) || input.contains('\\') {
        return false;
    }
    if component == DownloadComponent::ChatgptDesktop {
        return component_updates::is_allowed_download_url(input);
    }
    let Ok(url) = reqwest::Url::parse(input) else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || url.path().to_ascii_lowercase().contains("%2e")
    {
        return false;
    }
    match url.host_str() {
        Some("github.com") => {
            let Some(rest) = url.path().strip_prefix("/openai/codex/releases/download/") else {
                return false;
            };
            let Some((tag, filename)) = rest.split_once('/') else {
                return false;
            };
            component_updates::is_release_tag(tag)
                && url.query().is_none()
                && ["windows", "macos"].into_iter().any(|os| {
                    ["x86_64", "aarch64"]
                        .into_iter()
                        .any(|arch| cli_filename(os, arch).is_ok_and(|name| name == filename))
                })
        }
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => {
            url.path().starts_with("/github-production-release-asset")
        }
        _ => false,
    }
}

fn desktop_target(
    version: &str,
    release: component_updates::DesktopRelease,
) -> Result<DownloadTarget, DownloadError> {
    if release.version != version {
        return Err(error(
            "downloadSource",
            "远端版本已变化，请重新检查组件更新后下载",
        ));
    }
    let url = release
        .download_url
        .ok_or_else(|| error("downloadSource", "清单未提供当前平台的下载文件"))?;
    Ok(DownloadTarget {
        component: DownloadComponent::ChatgptDesktop,
        filename: format!("ChatGPT-{}-x64.msix", release.version),
        url,
    })
}

async fn resolve_target(
    client: &reqwest::Client,
    request: &DownloadRequest,
) -> Result<DownloadTarget, DownloadError> {
    match request.component {
        DownloadComponent::CodexCli => cli_target(
            &request.version,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ),
        DownloadComponent::ChatgptDesktop => {
            if !cfg!(windows) {
                return Err(error("downloadPlatform", "当前平台暂不支持桌面端组件下载"));
            }
            let mut response = client
                .get(consts::CHATGPT_RELEASE_FEED_URL)
                .send()
                .await
                .map_err(network_error)?;
            if !response.status().is_success() {
                return Err(error(
                    "downloadSource",
                    format!("组件清单返回 HTTP {}", response.status().as_u16()),
                ));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(network_error)? {
                if body.len() + chunk.len() > consts::COMPONENT_CHECK_MAX_BODY_BYTES {
                    return Err(error("downloadSource", "组件清单过大"));
                }
                body.extend_from_slice(&chunk);
            }
            let release = component_updates::parse_desktop_feed(&body)
                .map_err(|_| error("downloadSource", "组件清单无效，请重新检查更新"))?;
            desktop_target(&request.version, release)
        }
    }
}

async fn file_response(
    client: &reqwest::Client,
    target: &DownloadTarget,
) -> Result<reqwest::Response, DownloadError> {
    let mut url =
        reqwest::Url::parse(&target.url).map_err(|_| error("downloadSource", "下载地址无效"))?;
    for step in 0..=MAX_REDIRECTS {
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if response.status().is_redirection() {
            if step == MAX_REDIRECTS {
                return Err(error("downloadSource", "下载重定向次数过多"));
            }
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| url.join(s).ok())
                .ok_or_else(|| error("downloadSource", "下载重定向地址无效"))?;
            if !allowed_download_url(target.component, next.as_str()) {
                return Err(error("downloadSource", "下载重定向不在官方地址白名单内"));
            }
            url = next;
            continue;
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(error(
                "downloadNetwork",
                format!(
                    "下载服务器返回 HTTP {}，请检查代理后重试",
                    response.status().as_u16()
                ),
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
        {
            return Err(error(
                "downloadSource",
                "服务器未返回组件文件，请检查代理后重试",
            ));
        }
        return Ok(response);
    }
    unreachable!()
}

async fn save_download(
    client: &reqwest::Client,
    target: DownloadTarget,
    directory: &Path,
    progress: &(impl Fn(DownloadProgress) + Send + Sync),
) -> Result<DownloadResult, DownloadError> {
    let mut response = file_response(client, &target).await?;
    let total = response.content_length();
    if total.is_some_and(|bytes| bytes > MAX_DOWNLOAD_BYTES) {
        return Err(error("downloadSize", "下载文件超过 2 GiB 上限"));
    }
    std::fs::create_dir_all(directory).map_err(|_| {
        error(
            "downloadFile",
            "无法创建系统下载目录下的 CodexHelper 文件夹",
        )
    })?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".codex-helper-")
        .suffix(".part")
        .tempfile_in(directory)
        .map_err(|_| {
            error(
                "downloadFile",
                "无法在下载目录创建临时文件，请检查权限和磁盘空间",
            )
        })?;
    let mut received = 0u64;
    let mut last_progress = Instant::now();
    progress(DownloadProgress {
        received_bytes: received,
        total_bytes: total,
    });
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        received += chunk.len() as u64;
        if received > MAX_DOWNLOAD_BYTES || total.is_some_and(|total| received > total) {
            return Err(error("downloadSize", "下载文件大小超出预期"));
        }
        // 每次只同步写一个网络分块，文件始终由 NamedTempFile 持有；取消时 Windows 也能立即清理。
        temporary
            .write_all(&chunk)
            .map_err(|_| error("downloadFile", "下载写入失败，请检查权限和磁盘空间"))?;
        if last_progress.elapsed() >= Duration::from_millis(200) || received == chunk.len() as u64 {
            progress(DownloadProgress {
                received_bytes: received,
                total_bytes: total,
            });
            last_progress = Instant::now();
        }
    }
    if received == 0 || total.is_some_and(|total| total != received) {
        return Err(error("downloadSize", "下载文件为空或不完整，请重试"));
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| error("downloadFile", "无法完成下载文件写入，请检查磁盘空间"))?;
    let original = Path::new(&target.filename);
    let stem = original.file_stem().unwrap_or_default().to_string_lossy();
    let extension = original
        .extension()
        .map(|v| format!(".{}", v.to_string_lossy()))
        .unwrap_or_default();
    for suffix in 0..1000 {
        let file_name = if suffix == 0 {
            target.filename.clone()
        } else {
            format!("{stem} ({suffix}){extension}")
        };
        let destination = directory.join(&file_name);
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {
                progress(DownloadProgress {
                    received_bytes: received,
                    total_bytes: total,
                });
                return Ok(DownloadResult {
                    path: destination.to_string_lossy().into_owned(),
                    file_name,
                    received_bytes: received,
                });
            }
            Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temporary = err.file
            }
            Err(_) => {
                return Err(error(
                    "downloadFile",
                    "无法保存下载文件，请检查目录权限和磁盘空间",
                ));
            }
        }
    }
    Err(error(
        "downloadFile",
        "同名下载文件过多，请清理下载目录后重试",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Route, route_for_host};
    use std::sync::Mutex;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    #[test]
    fn desktop_target_rejects_changed_version_and_missing_file() {
        let release = component_updates::DesktopRelease { version: "26.1013.4894.0".into(), download_url: Some("https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix".into()) };
        assert!(desktop_target("26.1012.1.0", release.clone()).is_err());
        assert_eq!(
            desktop_target("26.1013.4894.0", release).unwrap().filename,
            "ChatGPT-26.1013.4894.0-x64.msix"
        );
        assert!(
            desktop_target(
                "26.1013.4894.0",
                component_updates::DesktopRelease {
                    version: "26.1013.4894.0".into(),
                    download_url: None
                }
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn https_download_uses_proxy_connect_tunnel() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
                assert!(headers.len() < 4096);
            }
            assert!(headers.starts_with(b"CONNECT unreachable.invalid:443 HTTP/1.1\r\n"));
            assert!(
                !String::from_utf8_lossy(&headers)
                    .to_ascii_lowercase()
                    .contains("authorization")
            );
            socket
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut tls = [0u8; 3];
            socket.read_exact(&mut tls).await.unwrap();
            assert_eq!(&tls[..2], &[0x16, 0x03]);
            assert!((1..=3).contains(&tls[2]));
        });
        let http = download_client(&NetworkPath::ViaProxy { proxy_url }).unwrap();
        assert!(
            http.get("https://unreachable.invalid/archive")
                .send()
                .await
                .is_err()
        );
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn failed_proxy_never_retries_directly() {
        let target = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("direct forbidden"))
            .mount(&target)
            .await;
        let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", unused.local_addr().unwrap());
        drop(unused);
        let http = download_client(&NetworkPath::ViaProxy { proxy_url }).unwrap();
        assert!(http.get(target.uri()).send().await.is_err());
        assert!(target.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_length_response_can_complete_without_inventing_a_total() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\narchive")
                .await
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let events = Mutex::new(Vec::new());
        let result = save_download(&client(), target(url), directory.path(), &|p| {
            events.lock().unwrap().push(p)
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(result.path).unwrap(), b"archive");
        assert_eq!(events.lock().unwrap().last().unwrap().total_bytes, None);
        peer.await.unwrap();
    }

    #[test]
    fn cli_packages_match_platform_and_architecture() {
        assert_eq!(
            cli_filename("windows", "x86_64").unwrap(),
            "codex-x86_64-pc-windows-msvc.exe.zip"
        );
        assert_eq!(
            cli_filename("windows", "aarch64").unwrap(),
            "codex-aarch64-pc-windows-msvc.exe.zip"
        );
        assert_eq!(
            cli_filename("macos", "aarch64").unwrap(),
            "codex-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            cli_filename("macos", "x86_64").unwrap(),
            "codex-x86_64-apple-darwin.tar.gz"
        );
        assert!(cli_filename("linux", "x86_64").is_err());
        assert!(cli_filename("windows", "x86").is_err());
    }

    #[test]
    fn cli_target_is_a_file_bound_to_requested_stable_version() {
        let target = cli_target("0.166.0", "windows", "x86_64").unwrap();
        assert_eq!(
            target.url,
            "https://github.com/openai/codex/releases/download/rust-v0.166.0/codex-x86_64-pc-windows-msvc.exe.zip"
        );
        assert!(cli_target("0.166.0/../../evil", "windows", "x86_64").is_err());
        assert!(cli_target("0.166.0-alpha.1", "windows", "x86_64").is_err());
    }

    #[test]
    fn redirects_allow_official_signed_assets_and_reject_other_destinations() {
        for url in [
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/abc?sig=secret&se=expiry",
            "https://objects.githubusercontent.com/github-production-release-asset-2e65be/1/abc?sig=secret",
        ] {
            assert!(
                allowed_download_url(DownloadComponent::CodexCli, url),
                "{url}"
            );
        }
        for url in [
            "http://release-assets.githubusercontent.com/file",
            "https://release-assets.githubusercontent.com.evil.test/file",
            "https://user:pass@release-assets.githubusercontent.com/file",
            "https://release-assets.githubusercontent.com:8443/file",
            "https://github.com/other/project/releases/download/v1/file.zip",
            "https://127.0.0.1/file",
            "https://release-assets.githubusercontent.com/file#fragment",
            "https://release-assets.githubusercontent.com/fi\nle",
        ] {
            assert!(
                !allowed_download_url(DownloadComponent::CodexCli, url),
                "{url}"
            );
        }
        assert!(!allowed_download_url(
            DownloadComponent::ChatgptDesktop,
            "https://github.com/openai/codex/releases/download/rust-v0.1.0/file.zip"
        ));
    }

    #[test]
    fn github_asset_hosts_use_upstream_proxy() {
        assert_eq!(
            route_for_host("release-assets.githubusercontent.com"),
            Route::Socks5
        );
        assert_eq!(
            route_for_host("objects.githubusercontent.com"),
            Route::Socks5
        );
        assert_eq!(route_for_host("evil.githubusercontent.com"), Route::Direct);
    }

    fn request(id: &str) -> DownloadRequest {
        DownloadRequest {
            id: id.into(),
            component: DownloadComponent::CodexCli,
            version: "0.166.0".into(),
        }
    }

    fn target(url: String) -> DownloadTarget {
        DownloadTarget {
            component: DownloadComponent::CodexCli,
            filename: "codex-test.zip".into(),
            url,
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn stopped_proxy_creates_no_directory_or_request() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("not-created");
        let result = DownloadManager::default()
            .download(
                request("one"),
                NetworkPath::Direct { reason: None },
                &directory,
                |_| {},
            )
            .await;
        assert_eq!(result.unwrap_err().code, "downloadProxy");
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn proxy_client_reaches_proxy_instead_of_target_host() {
        let proxy = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"archive".to_vec()))
            .mount(&proxy)
            .await;
        let client = download_client(&NetworkPath::ViaProxy {
            proxy_url: proxy.uri(),
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let result = save_download(
            &client,
            target("http://unreachable.invalid/archive".into()),
            directory.path(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(result.path).unwrap(), b"archive");
        assert_eq!(proxy.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn saves_complete_file_reports_progress_and_preserves_existing_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"new archive".to_vec()))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("codex-test.zip"), b"existing").unwrap();
        let events = Mutex::new(Vec::new());
        let result = save_download(&client(), target(server.uri()), directory.path(), &|p| {
            events.lock().unwrap().push(p)
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&result.path).unwrap(), b"new archive");
        assert_eq!(
            std::fs::read(directory.path().join("codex-test.zip")).unwrap(),
            b"existing"
        );
        assert_eq!(result.file_name, "codex-test (1).zip");
        assert_eq!(result.received_bytes, 11);
        assert_eq!(events.lock().unwrap().last().unwrap().received_bytes, 11);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn rejects_html_instead_of_saving_a_login_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("<html>blocked</html>", "text/html"),
            )
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        assert!(
            save_download(&client(), target(server.uri()), directory.path(), &|_| {})
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn rejects_redirect_to_untrusted_host_before_requesting_it() {
        let destination = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", destination.uri()))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let err = save_download(&client(), target(server.uri()), directory.path(), &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code, "downloadSource");
        assert!(destination.received_requests().await.unwrap().is_empty());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_without_creating_a_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "2147483649"))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        assert!(
            save_download(&client(), target(server.uri()), directory.path(), &|_| {})
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn cancellation_removes_partial_file_and_releases_single_download_slot() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\npartial")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let manager = DownloadManager::default();
        let guard = manager.begin("first").unwrap();
        assert!(manager.begin("second").is_err());
        assert!(!manager.cancel("wrong"));
        let directory = tempfile::tempdir().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Mutex::new(Some(started_tx));
        let callback = |p: DownloadProgress| {
            if p.received_bytes > 0
                && let Some(tx) = started_tx.lock().unwrap().take()
            {
                let _ = tx.send(());
            }
        };
        let http = client();
        let download = with_cancel(
            save_download(&http, target(url), directory.path(), &callback),
            guard.receiver.clone(),
        );
        let cancel = async {
            tokio::time::timeout(Duration::from_secs(3), started_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(manager.cancel("first"));
        };
        let (result, ()) = tokio::join!(download, cancel);
        assert_eq!(result.unwrap_err().code, "downloadCancelled");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        drop(guard);
        assert!(manager.begin("second").is_ok());
        peer.abort();
    }

    #[tokio::test]
    async fn truncated_response_never_publishes_partial_file() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
                .await
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        assert!(
            save_download(&client(), target(url), directory.path(), &|_| {})
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        peer.await.unwrap();
    }
}
