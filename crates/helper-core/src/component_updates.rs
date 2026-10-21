//! 组件更新检查（ChatGPT Desktop / Codex CLI）。
//!
//! 设计文档：`docs/superpowers/specs/2026-10-20-component-update-check-design.md`
//!
//! 只检测、不下载不安装：展示本地与远端最新版本，有更新时给出官方下载入口。
//!
//! 约束与不变量：
//! - **门禁的本进程放行对进程内任何经本地代理端口（默认 17891）的客户端生效，门禁本身不校验
//!   目标**（设计 §7 方案 d）。因此约定：进程内只有组件检测把本地代理当代理使用，
//!   **新增进程内代理客户端须评审**（评估是否落入门禁放行面与上游白名单约束）。
//! - 检测 URL 全部来自 `consts` 的编译期常量，不接受外部输入的目标地址。
//! - 检测全程无凭据：不带 Authorization / Cookie 等任何凭据头，只带固定 User-Agent。
//! - Secure 模式下代理未就绪时零出站，绝不回退直连（设计 C4）。

use std::cmp::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::consts;
use crate::state::ProxyMode;
use crate::types::{ProxyState, Status};

// ---------------------------------------------------------------------------
// 数据结构（与 apps/desktop/src/types.ts 一一对应）
// ---------------------------------------------------------------------------

/// 单组件的本地探测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LocalState {
    Installed {
        version: String,
    },
    /// 仅用于 CLI：PATH 无 codex 命中
    NotInstalled,
    /// 仅用于 ChatGPT：Get-AppxPackage 空输出（无法区分“未安装”与“当前用户未注册”）
    NotRegisteredForUser,
    ProbeFailed {
        detail: String,
    },
    /// 非 Windows 的 ChatGPT 行：不探测
    UnsupportedPlatform,
}

/// 远端最新版本结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RemoteState {
    Ok {
        version: String,
        source: RemoteSource,
        /// 远端失败的简要原因，仅本地缓存兜底时有值
        #[serde(skip_serializing_if = "Option::is_none")]
        fallback_from: Option<String>,
    },
    Unreachable {
        detail: String,
    },
    /// 清单/响应格式无效、或下载地址未命中白名单
    Invalid {
        detail: String,
    },
    /// Secure 模式下代理未就绪：本次未发出任何远端请求
    Blocked {
        detail: String,
    },
    UnsupportedPlatform,
}

/// 远端来源标记（展示用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RemoteSource {
    /// windows-release.json 清单
    DesktopReleaseFeed,
    /// GitHub releases latest API
    GithubApi,
    /// releases/latest 302 回退
    GithubRedirect,
    /// $CODEX_HOME/version.json 本地缓存（非公开格式，仅兜底）
    CodexLocalCache { checked_at: String },
}

/// 两组件的总体比较结果（由 local + remote 派生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Comparison {
    UpdateAvailable,
    UpToDate,
    /// 本地比远端新（如离线包装了新版、清单滞后）：不提示更新
    LocalNewer,
    /// 任一侧缺版本，或本地为 0.0.0 开发版不比较
    Unknown,
}

/// 单组件检测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdate {
    pub local: LocalState,
    pub remote: RemoteState,
    pub comparison: Comparison,
    /// 有更新时的下载入口：ChatGPT 为带版本号 downloadUrl；CLI 为 release 页面
    pub download_url: Option<String>,
    /// 本次检测是否经本地代理出网
    pub via_proxy: bool,
    /// 直连原因文案（仅用于序列化输出）
    pub direct_reason: Option<&'static str>,
}

/// 两组件总返回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdates {
    pub chatgpt_desktop: ComponentUpdate,
    pub codex_cli: ComponentUpdate,
}

/// 组件种类（决定版本比较规则；ChatGPT 点分数字、CLI semver）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    ChatgptDesktop,
    CodexCli,
}

/// ChatGPT 清单解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRelease {
    /// 四段 `packageVersion`
    pub version: String,
    /// x64 条目的下载地址（已过白名单）；清单无 x64 条目时为 None
    pub download_url: Option<String>,
}

/// Codex CLI 本地更新检查缓存（`version.json`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCache {
    pub latest_version: String,
    /// 去掉小数秒后的检查时间
    pub checked_at: String,
}

// ---------------------------------------------------------------------------
// 纯函数：版本比较、解析、白名单
// ---------------------------------------------------------------------------

/// 解析点分数字版本：1~4 段纯 ASCII 数字；格式不符返回 None。
fn parse_dotted(version: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    parts.iter().map(|part| parse_digits(part)).collect()
}

/// 非空、纯 ASCII 数字的段转 u64（溢出也视为格式不符）。
fn parse_digits(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// ChatGPT 版本比较（应用内 `un()` 同构）：点分数字逐段数值比较、缺段按 0。
/// 只用于四段 `packageVersion` 对 `Get-AppxPackage` 四段 Version；`releaseVersion` 绝不参与。
pub fn compare_dotted(local: &str, remote: &str) -> Option<Ordering> {
    let mut local = parse_dotted(local)?;
    let mut remote = parse_dotted(remote)?;
    local.resize(4, 0);
    remote.resize(4, 0);
    Some(local.cmp(&remote))
}

/// semver 解析结果：核心三段 + 预发布标识（`+build` 已忽略）。
struct Semver<'a> {
    core: [u64; 3],
    pre: Vec<&'a str>,
}

fn parse_semver(version: &str) -> Option<Semver<'_>> {
    let version = match version.split_once('+') {
        Some((head, build)) if is_identifiers(build) => head,
        Some(_) => return None,
        None => version,
    };
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) if is_identifiers(pre) => (core, pre.split('.').collect()),
        Some(_) => return None,
        None => (version, Vec::new()),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let [major, minor, patch] = parts.as_slice() else {
        return None;
    };
    Some(Semver {
        core: [
            parse_digits(major)?,
            parse_digits(minor)?,
            parse_digits(patch)?,
        ],
        pre,
    })
}

/// 点分标识串：每段非空且只含 `[0-9A-Za-z-]`。
fn is_identifiers(text: &str) -> bool {
    text.split('.')
        .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn compare_pre(local: &[&str], remote: &[&str]) -> Ordering {
    match (local.is_empty(), remote.is_empty()) {
        (true, true) => return Ordering::Equal,
        // 有预发布后缀 < 无后缀
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    for (a, b) in local.iter().zip(remote) {
        let order = match (parse_digits(a), parse_digits(b)) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => a.cmp(b),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    // 前缀相同时短者小
    local.len().cmp(&remote.len())
}

/// Codex CLI 版本比较：`M.m.p[-pre][+build]`，忽略 `+build`；格式不符返回 None。
pub fn compare_semver(local: &str, remote: &str) -> Option<Ordering> {
    let local = parse_semver(local)?;
    let remote = parse_semver(remote)?;
    Some(
        local
            .core
            .cmp(&remote.core)
            .then_with(|| compare_pre(&local.pre, &remote.pre)),
    )
}

/// 本地开发版：核心段为 `0.0.0`（含 `0.0.0-xxx`），不参与比较（设计 C6）。
pub fn is_dev_build(version: &str) -> bool {
    parse_semver(version).is_some_and(|v| v.core == [0, 0, 0])
}

/// 由本地与远端状态派生总体比较结果：仅 `Installed × Ok` 时比较，其余一律 `Unknown`。
pub fn derive_comparison(
    kind: ComponentKind,
    local: &LocalState,
    remote: &RemoteState,
) -> Comparison {
    let (
        LocalState::Installed { version: local },
        RemoteState::Ok {
            version: remote, ..
        },
    ) = (local, remote)
    else {
        return Comparison::Unknown;
    };
    let order = match kind {
        ComponentKind::ChatgptDesktop => compare_dotted(local, remote),
        ComponentKind::CodexCli if is_dev_build(local) => None,
        ComponentKind::CodexCli => compare_semver(local, remote),
    };
    match order {
        Some(Ordering::Less) => Comparison::UpdateAvailable,
        Some(Ordering::Equal) => Comparison::UpToDate,
        Some(Ordering::Greater) => Comparison::LocalNewer,
        None => Comparison::Unknown,
    }
}

/// `windows-release.json` 只声明所需字段；serde 默认忽略未知字段（含 `releaseVersion`）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedRaw {
    schema_version: u32,
    package_version: String,
    packages: Vec<FeedPackage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedPackage {
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    download_url: Option<String>,
}

/// JSON 解析错误按类别给出文案（不带响应体内容）。
fn json_error_text(error: &serde_json::Error) -> String {
    if error.is_data() {
        "缺少字段或字段类型不符".to_string()
    } else {
        "不是有效的 JSON".to_string()
    }
}

/// 解析 ChatGPT 发布源清单（设计 §2.1 / C2 / C9）。
///
/// 只采信 `schemaVersion == 1`；`packageVersion` 须为合法点分数字；按 x64 取下载地址（C12），
/// 出现的地址必须命中白名单，否则整份清单不采信。
pub fn parse_desktop_feed(body: &[u8]) -> Result<DesktopRelease, String> {
    let raw: FeedRaw = serde_json::from_slice(body).map_err(|e| json_error_text(&e))?;
    if raw.schema_version != 1 {
        return Err(format!("不支持的 schemaVersion {}", raw.schema_version));
    }
    if parse_dotted(&raw.package_version).is_none() {
        return Err("packageVersion 格式不符".to_string());
    }
    let download_url = raw
        .packages
        .into_iter()
        .find(|p| p.architecture.as_deref() == Some("x64"))
        .and_then(|p| p.download_url);
    if let Some(url) = &download_url
        && !is_allowed_download_url(url)
    {
        return Err("下载地址不在官方白名单内".to_string());
    }
    Ok(DesktopRelease {
        version: raw.package_version,
        download_url,
    })
}

/// 下载地址白名单（C9）：https、无用户信息、主机精确匹配、无显式端口、无 query/fragment，
/// 路径以白名单前缀开头且更长。原串含 `..`、`%2e`、`\` 一律拒绝（不依赖解析器的规范化）。
pub fn is_allowed_download_url(url: &str) -> bool {
    if url.contains("..") || url.to_ascii_lowercase().contains("%2e") || url.contains('\\') {
        return false;
    }
    let (Ok(parsed), Ok(prefix)) = (
        reqwest::Url::parse(url),
        reqwest::Url::parse(consts::CHATGPT_DOWNLOAD_URL_PREFIX),
    ) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.host_str().is_some()
        && parsed.host_str() == prefix.host_str()
        && parsed.port().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.path().starts_with(prefix.path())
        && parsed.path().len() > prefix.path().len()
}

/// 与 aitools-download `prepare.py` 的 `rust-v[0-9]+\.[0-9]+\.[0-9]+`（fullmatch）等价的手写判定。
pub fn is_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix("rust-v") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// GitHub release 响应只取 `tag_name`，其余字段忽略。
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

/// 解析 GitHub releases latest API 响应，返回通过 tag 判定的 `tag_name`。
pub fn parse_github_release(body: &[u8]) -> Result<String, String> {
    let release: GithubRelease = serde_json::from_slice(body).map_err(|e| json_error_text(&e))?;
    if is_release_tag(&release.tag_name) {
        Ok(release.tag_name)
    } else {
        Err("tag 格式不符".to_string())
    }
}

/// 从 releases/latest 302 的 `Location` 取末段 tag（去掉 query/fragment）。
pub fn tag_from_location(location: &str) -> Option<String> {
    let end = location.find(['?', '#']).unwrap_or(location.len());
    let tag = location[..end].rsplit('/').next()?;
    is_release_tag(tag).then(|| tag.to_string())
}

/// Codex CLI release 页面地址。
pub fn release_page_url(tag: &str) -> String {
    format!("{}{tag}", consts::CODEX_RELEASE_TAG_URL_PREFIX)
}

/// 解析 `Get-AppxPackage … | ForEach-Object { $_.Version }` 的输出（每行一个版本）。
///
/// 去 BOM、按行 trim、丢空行；全空判“未安装或当前用户未注册”；多行取最大版本
/// （同一用户下多版本/架构并存）；有输出但没有合法版本判探测失败。
pub fn parse_appx_output(stdout: &str) -> LocalState {
    let lines: Vec<&str> = stdout
        .lines()
        .map(|line| line.trim_matches(|c: char| c == '\u{feff}' || c.is_whitespace()))
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return LocalState::NotRegisteredForUser;
    }
    let best = lines
        .into_iter()
        .filter(|line| parse_dotted(line).is_some())
        .max_by(|a, b| compare_dotted(a, b).unwrap_or(Ordering::Equal));
    match best {
        Some(version) => LocalState::Installed {
            version: version.to_string(),
        },
        None => LocalState::ProbeFailed {
            detail: "Get-AppxPackage 输出无法识别".to_string(),
        },
    }
}

/// 从 `codex --version` 输出取版本：`codex-cli ` 之后第一个词，须为合法 semver。
pub fn parse_cli_version(output: &str) -> Option<String> {
    const MARKER: &str = "codex-cli ";
    let start = output.find(MARKER)? + MARKER.len();
    let version = output[start..].split_whitespace().next()?;
    parse_semver(version).map(|_| version.to_string())
}

/// `version.json` 只取所需字段（非公开格式）。
#[derive(Deserialize)]
struct VersionCacheRaw {
    latest_version: String,
    last_checked_at: String,
}

/// 解析 Codex CLI 自身更新检查缓存；任何失败返回 None（兜底的兜底没有意义）。
pub fn parse_version_cache(body: &[u8]) -> Option<VersionCache> {
    let raw: VersionCacheRaw = serde_json::from_slice(body).ok()?;
    parse_semver(&raw.latest_version)?;
    Some(VersionCache {
        latest_version: raw.latest_version,
        checked_at: strip_fraction_seconds(&raw.last_checked_at),
    })
}

/// `2026-10-16T05:53:49.254443Z` → `2026-10-16T05:53:49Z`；无小数秒原样返回。
fn strip_fraction_seconds(time: &str) -> String {
    let Some(dot) = time.find('.') else {
        return time.to_string();
    };
    let rest = &time[dot + 1..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    format!("{}{}", &time[..dot], &rest[digits..])
}

// ---------------------------------------------------------------------------
// 网络路径决策（设计 C4 / §4.2）
// ---------------------------------------------------------------------------

/// 本次检测的网络路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPath {
    /// 经本地代理；`proxy_url` 固定为 `http://127.0.0.1:<port>`（不用 localhost，避免解析到 ::1）
    ViaProxy { proxy_url: String },
    /// 直连；`reason` 为展示给用户的直连原因
    Direct { reason: Option<&'static str> },
    /// Secure 未就绪：远端探测整体跳过（零出站）
    Blocked { detail: &'static str },
}

/// Secure 未就绪文案：一般情形。
const BLOCKED_NOT_RUNNING: &str = "Secure 代理未就绪（代理未运行）";
/// Secure 未就绪文案：端口被外部进程占用（External 或 blocked_reason == port_occupied）。
const BLOCKED_PORT_OCCUPIED: &str = "Secure 代理未就绪（端口被外部进程占用）";
/// Legacy + External 的直连原因。
const DIRECT_EXTERNAL: &str = "代理为外部实例，模式未知，已直连";
/// Legacy + Stopped 的直连原因。
const DIRECT_STOPPED: &str = "本地代理未运行，已直连";

/// 按 Manager 状态快照决定网络路径（C4）。
///
/// 只看 `status.mode`（状态文件模式）与 `status.proxy.state`，Secure 下再看 `secure.blocked_reason`
/// 选文案；**不读 `secure.active`**——Secure 被阻断时 state 为 Stopped，按 active 判定会错误直连。
/// Secure 模式下非 Running 一律零出站，绝不直连。
pub fn network_path_for(status: &Status) -> NetworkPath {
    let via_proxy = || NetworkPath::ViaProxy {
        proxy_url: format!("http://127.0.0.1:{}", status.proxy.port),
    };
    match (status.mode, status.proxy.state) {
        (_, ProxyState::Running) => via_proxy(),
        (ProxyMode::Secure, ProxyState::External) => NetworkPath::Blocked {
            detail: BLOCKED_PORT_OCCUPIED,
        },
        (ProxyMode::Secure, ProxyState::Stopped) => NetworkPath::Blocked {
            detail: if status.secure.blocked_reason.as_deref() == Some("port_occupied") {
                BLOCKED_PORT_OCCUPIED
            } else {
                BLOCKED_NOT_RUNNING
            },
        },
        (ProxyMode::Legacy, ProxyState::External) => NetworkPath::Direct {
            reason: Some(DIRECT_EXTERNAL),
        },
        (ProxyMode::Legacy, ProxyState::Stopped) => NetworkPath::Direct {
            reason: Some(DIRECT_STOPPED),
        },
    }
}

/// 按网络路径构建 HTTP 客户端；`Blocked` 或构建失败返回 None。
///
/// 共同：native-tls、整体超时、不跟随重定向（302 回退显式读 `Location`）、固定 User-Agent、
/// 不带任何凭据。经代理时显式 `.proxy()`（不读环境变量代理）；直连时 `.no_proxy()`。
pub fn build_client(path: &NetworkPath, timeout: Duration) -> Option<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .use_native_tls()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("codex-helper/", env!("CARGO_PKG_VERSION")));
    let builder = match path {
        NetworkPath::Blocked { .. } => return None,
        NetworkPath::ViaProxy { proxy_url } => {
            builder.proxy(reqwest::Proxy::all(proxy_url.as_str()).ok()?)
        }
        NetworkPath::Direct { .. } => builder.no_proxy(),
    };
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::AsyncReadExt;

    use super::*;

    const X64_URL: &str =
        "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix";

    /// §2.1 实测清单 + releaseVersion + 未知字段。
    fn feed_fixture() -> String {
        json!({
            "schemaVersion": 1,
            "releaseDate": "2026-10-17T05:55:44Z",
            "packageVersion": "26.1013.4894.0",
            "releaseVersion": "26.1013.40934",
            "futureField": { "a": 1 },
            "packages": [
                { "architecture": "x64", "downloadUrl": X64_URL },
                { "architecture": "arm64", "downloadUrl":
                  "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-arm64.msix" }
            ]
        })
        .to_string()
    }

    fn installed(version: &str) -> LocalState {
        LocalState::Installed {
            version: version.into(),
        }
    }

    fn remote_ok(version: &str, source: RemoteSource) -> RemoteState {
        RemoteState::Ok {
            version: version.into(),
            source,
            fallback_from: None,
        }
    }

    #[test]
    fn serializes_design_example_exactly() {
        let value = ComponentUpdates {
            chatgpt_desktop: ComponentUpdate {
                local: installed("26.1013.4894.0"),
                remote: remote_ok("26.1013.4894.0", RemoteSource::DesktopReleaseFeed),
                comparison: Comparison::UpToDate,
                download_url: None,
                via_proxy: true,
                direct_reason: None,
            },
            codex_cli: ComponentUpdate {
                local: installed("0.162.1"),
                remote: RemoteState::Ok {
                    version: "0.163.0".into(),
                    source: RemoteSource::CodexLocalCache {
                        checked_at: "2026-10-16T05:53:49Z".into(),
                    },
                    fallback_from: Some("github api 403 限流".into()),
                },
                comparison: Comparison::UpdateAvailable,
                download_url: Some(
                    "https://github.com/openai/codex/releases/tag/rust-v0.163.0".into(),
                ),
                via_proxy: false,
                direct_reason: Some("代理为外部实例，模式未知，已直连"),
            },
        };
        let actual = serde_json::to_value(&value).unwrap();
        let expected = json!({
          "chatgptDesktop": {
            "local": { "kind": "installed", "version": "26.1013.4894.0" },
            "remote": { "kind": "ok", "version": "26.1013.4894.0",
                        "source": { "kind": "desktopReleaseFeed" } },
            "comparison": "upToDate", "downloadUrl": null, "viaProxy": true, "directReason": null
          },
          "codexCli": {
            "local": { "kind": "installed", "version": "0.162.1" },
            "remote": { "kind": "ok", "version": "0.163.0",
                        "source": { "kind": "codexLocalCache", "checkedAt": "2026-10-16T05:53:49Z" },
                        "fallbackFrom": "github api 403 限流" },
            "comparison": "updateAvailable",
            "downloadUrl": "https://github.com/openai/codex/releases/tag/rust-v0.163.0",
            "viaProxy": false, "directReason": "代理为外部实例，模式未知，已直连"
          }
        });
        assert_eq!(actual, expected);
        assert!(
            actual["chatgptDesktop"]["remote"]
                .get("fallbackFrom")
                .is_none()
        );
    }

    #[test]
    fn unit_variants_serialize_as_kind_objects() {
        assert_eq!(
            to(&LocalState::NotInstalled),
            json!({"kind": "notInstalled"})
        );
        assert_eq!(
            to(&LocalState::NotRegisteredForUser),
            json!({"kind": "notRegisteredForUser"})
        );
        assert_eq!(
            to(&LocalState::UnsupportedPlatform),
            json!({"kind": "unsupportedPlatform"})
        );
        assert_eq!(
            to(&RemoteState::UnsupportedPlatform),
            json!({"kind": "unsupportedPlatform"})
        );
        assert_eq!(
            to(&LocalState::ProbeFailed { detail: "x".into() }),
            json!({"kind": "probeFailed", "detail": "x"})
        );
        assert_eq!(
            to(&RemoteState::Blocked { detail: "y".into() }),
            json!({"kind": "blocked", "detail": "y"})
        );
        assert_eq!(
            to(&RemoteSource::GithubRedirect),
            json!({"kind": "githubRedirect"})
        );
        assert_eq!(to(&Comparison::LocalNewer), json!("localNewer"));
    }

    /// 测试辅助：任意可序列化值转 JSON。
    fn to<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    #[test]
    fn compare_dotted_pads_missing_segments_with_zero() {
        assert_eq!(
            compare_dotted("26.1013.4894", "26.1013.4894.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare_dotted("26.1013.4894.0", "26.1013.4895.0"),
            Some(Ordering::Less)
        );
        // 记录：若误用 releaseVersion，装机版本恒小于它（永远提示有更新）
        assert_eq!(
            compare_dotted("26.1013.4894.0", "26.1013.40934"),
            Some(Ordering::Less)
        );
        assert_eq!(compare_dotted("a.b", "1"), None);
        assert_eq!(compare_dotted("", "1"), None);
        assert_eq!(compare_dotted("1.2.3.4.5", "1"), None);
    }

    #[test]
    fn release_version_never_participates() {
        let release = parse_desktop_feed(feed_fixture().as_bytes()).unwrap();
        assert_eq!(release.version, "26.1013.4894.0");
        let remote = remote_ok(&release.version, RemoteSource::DesktopReleaseFeed);
        assert_eq!(
            derive_comparison(
                ComponentKind::ChatgptDesktop,
                &installed("26.1013.4894.0"),
                &remote
            ),
            Comparison::UpToDate
        );
    }

    #[test]
    fn compare_semver_handles_prerelease_and_build() {
        let cmp = compare_semver;
        assert_eq!(cmp("0.164.0-alpha.2", "0.164.0"), Some(Ordering::Less));
        assert_eq!(
            cmp("0.164.0-alpha.2", "0.164.0-alpha.10"),
            Some(Ordering::Less)
        );
        assert_eq!(cmp("0.164.0-alpha", "0.164.0-beta"), Some(Ordering::Less));
        assert_eq!(cmp("0.163.0+abc", "0.163.0"), Some(Ordering::Equal));
        assert_eq!(cmp("0.162.1", "0.163.0"), Some(Ordering::Less));
        assert_eq!(cmp("0.164.0", "0.164.0-rc.1"), Some(Ordering::Greater));
        assert_eq!(cmp("1.0", "1.0.0"), None);
        assert_eq!(cmp("v1.0.0", "1.0.0"), None);
        assert_eq!(cmp("1.0.0-", "1.0.0"), None);
    }

    #[test]
    fn dev_build_compares_unknown() {
        let remote = remote_ok("0.163.0", RemoteSource::GithubApi);
        assert_eq!(
            derive_comparison(ComponentKind::CodexCli, &installed("0.0.0"), &remote),
            Comparison::Unknown
        );
        assert_eq!(
            derive_comparison(ComponentKind::CodexCli, &installed("0.0.0-dev"), &remote),
            Comparison::Unknown
        );
        assert!(is_dev_build("0.0.0"));
        assert!(!is_dev_build("0.0.1"));
        // ChatGPT 不适用开发版规则：照常按点分数字比较
        let desktop = remote_ok("0.0.1.0", RemoteSource::DesktopReleaseFeed);
        assert_eq!(
            derive_comparison(ComponentKind::ChatgptDesktop, &installed("0.0.0"), &desktop),
            Comparison::UpdateAvailable
        );
    }

    #[test]
    fn derive_comparison_matrix() {
        let cli = ComponentKind::CodexCli;
        let ok = |v: &str| remote_ok(v, RemoteSource::GithubApi);
        assert_eq!(
            derive_comparison(cli, &installed("0.162.1"), &ok("0.163.0")),
            Comparison::UpdateAvailable
        );
        assert_eq!(
            derive_comparison(cli, &installed("0.163.0"), &ok("0.163.0")),
            Comparison::UpToDate
        );
        assert_eq!(
            derive_comparison(cli, &installed("0.164.0"), &ok("0.163.0")),
            Comparison::LocalNewer
        );
        for local in [
            LocalState::NotInstalled,
            LocalState::ProbeFailed { detail: "x".into() },
            LocalState::NotRegisteredForUser,
        ] {
            assert_eq!(
                derive_comparison(cli, &local, &ok("0.163.0")),
                Comparison::Unknown
            );
        }
        for remote in [
            RemoteState::Unreachable { detail: "x".into() },
            RemoteState::Invalid { detail: "x".into() },
            RemoteState::Blocked { detail: "x".into() },
        ] {
            assert_eq!(
                derive_comparison(cli, &installed("0.162.1"), &remote),
                Comparison::Unknown
            );
        }
        let cached = RemoteState::Ok {
            version: "0.163.0".into(),
            source: RemoteSource::CodexLocalCache {
                checked_at: "2026-10-16T05:53:49Z".into(),
            },
            fallback_from: Some("github api 403 限流".into()),
        };
        assert_eq!(
            derive_comparison(cli, &installed("0.162.1"), &cached),
            Comparison::UpdateAvailable
        );
    }

    #[test]
    fn desktop_feed_parses_real_shape_and_tolerates_unknown_fields() {
        let release = parse_desktop_feed(feed_fixture().as_bytes()).unwrap();
        assert_eq!(
            release,
            DesktopRelease {
                version: "26.1013.4894.0".into(),
                download_url: Some(X64_URL.into()),
            }
        );
    }

    #[test]
    fn desktop_feed_rejects_missing_fields_and_bad_schema() {
        let pkgs = json!([{ "architecture": "x64", "downloadUrl": X64_URL }]);
        let cases = [
            json!({ "schemaVersion": 1, "packages": pkgs }).to_string(),
            json!({ "schemaVersion": 1, "packageVersion": "26.1013.4894.0" }).to_string(),
            json!({ "schemaVersion": 2, "packageVersion": "26.1013.4894.0", "packages": pkgs })
                .to_string(),
            json!({ "schemaVersion": 1, "packageVersion": "abc", "packages": pkgs }).to_string(),
            "<html>login</html>".to_string(),
        ];
        for body in cases {
            assert!(parse_desktop_feed(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn desktop_feed_without_x64_entry_has_no_url() {
        let body = json!({
            "schemaVersion": 1, "packageVersion": "26.1013.4894.0",
            "packages": [{ "architecture": "arm64", "downloadUrl":
              "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-arm64.msix" }]
        })
        .to_string();
        let release = parse_desktop_feed(body.as_bytes()).unwrap();
        assert_eq!(release.version, "26.1013.4894.0");
        assert_eq!(release.download_url, None);
    }

    #[test]
    fn allowed_download_url_rejects_bypass_forms() {
        assert!(is_allowed_download_url(X64_URL));
        // 主机大小写：Url 会小写化主机，不构成绕过，接受
        assert!(is_allowed_download_url(
            "https://PERSISTENT.oaistatic.com/codex-app-prod/x"
        ));
        for url in [
            "http://persistent.oaistatic.com/codex-app-prod/x.msix",
            "https://persistent.oaistatic.com@evil.com/codex-app-prod/x.msix",
            "https://user:pw@persistent.oaistatic.com/codex-app-prod/x",
            "https://persistent.oaistatic.com.evil.com/codex-app-prod/x",
            "https://persistent.oaistatic.com:8443/codex-app-prod/x",
            "https://persistent.oaistatic.com/codex-app-prod/../other/x",
            "https://persistent.oaistatic.com/codex-app-prod/%2e%2e/x",
            "https://persistent.oaistatic.com/codex-app-prod/%2E%2E/x",
            "https://persistent.oaistatic.com/codex-app-prod/..\\x",
            "https://persistent.oaistatic.com/codex-app-prodx/x",
            "https://persistent.oaistatic.com/codex-app-prod/",
            "https://persistent.oaistatic.com/codex-app-prod/x?y=1",
            "https://persistent.oaistatic.com/codex-app-prod/x#y",
            "not a url",
        ] {
            assert!(!is_allowed_download_url(url), "{url}");
        }
    }

    #[test]
    fn desktop_feed_with_offlist_url_is_err() {
        let body = json!({
            "schemaVersion": 1, "packageVersion": "26.1013.4894.0",
            "packages": [{ "architecture": "x64", "downloadUrl": "https://evil.example/ChatGPT-x64.msix" }]
        })
        .to_string();
        assert_eq!(
            parse_desktop_feed(body.as_bytes()),
            Err("下载地址不在官方白名单内".to_string())
        );
    }

    #[test]
    fn release_tag_matches_prepare_py_regex() {
        for tag in ["rust-v0.162.1", "rust-v10.0.123"] {
            assert!(is_release_tag(tag), "{tag}");
        }
        for tag in [
            "rust-v0.164.0-alpha.2",
            "v0.162.1",
            "rust-v0.162",
            "rust-v0.162.1.0",
            "rust-v0.1a.1",
            "rust-v.1.1",
            " rust-v0.1.1",
            "rust-v0.1.1 ",
            "rust-v",
        ] {
            assert!(!is_release_tag(tag), "{tag}");
        }
    }

    #[test]
    fn github_release_takes_only_tag_name() {
        let body =
            r#"{"tag_name":"rust-v0.163.0","name":"0.163.0","prerelease":false,"assets":[]}"#;
        assert_eq!(
            parse_github_release(body.as_bytes()),
            Ok("rust-v0.163.0".to_string())
        );
        assert!(parse_github_release(br#"{"tag_name":"rust-v0.164.0-alpha.2"}"#).is_err());
        assert!(parse_github_release(b"{}").is_err());
    }

    #[test]
    fn tag_from_location_takes_last_segment() {
        let want = Some("rust-v0.163.0".to_string());
        let base = "https://github.com/openai/codex/releases";
        assert_eq!(
            tag_from_location(&format!("{base}/tag/rust-v0.163.0")),
            want
        );
        assert_eq!(
            tag_from_location("/openai/codex/releases/tag/rust-v0.163.0"),
            want
        );
        assert_eq!(
            tag_from_location(&format!("{base}/tag/rust-v0.163.0?x=1")),
            want
        );
        assert_eq!(
            tag_from_location(&format!("{base}/tag/rust-v0.163.0#a")),
            want
        );
        assert_eq!(tag_from_location(&format!("{base}/latest")), None);
        assert_eq!(
            tag_from_location(&format!("{base}/tag/rust-v0.164.0-alpha.2")),
            None
        );
        assert_eq!(tag_from_location(""), None);
    }

    #[test]
    fn release_page_url_appends_tag() {
        assert_eq!(
            release_page_url("rust-v0.163.0"),
            "https://github.com/openai/codex/releases/tag/rust-v0.163.0"
        );
    }

    #[test]
    fn appx_output_takes_max_version() {
        assert_eq!(
            parse_appx_output("26.1013.4894.0\r\n26.1020.100.0\r\n"),
            installed("26.1020.100.0")
        );
        assert_eq!(
            parse_appx_output("26.1020.100.0\r\n26.1013.4894.0\r\n"),
            installed("26.1020.100.0")
        );
        assert_eq!(
            parse_appx_output("\u{feff}26.1013.4894.0\r\n\r\n"),
            installed("26.1013.4894.0")
        );
    }

    #[test]
    fn appx_output_blank_is_not_registered() {
        for out in ["", "\r\n  \r\n", "\u{feff}"] {
            assert_eq!(parse_appx_output(out), LocalState::NotRegisteredForUser);
        }
    }

    #[test]
    fn appx_output_garbage_is_probe_failed() {
        assert_eq!(
            parse_appx_output("Get-AppxPackage : 无法识别…"),
            LocalState::ProbeFailed {
                detail: "Get-AppxPackage 输出无法识别".into()
            }
        );
    }

    #[test]
    fn cli_version_parses_codex_cli_line() {
        assert_eq!(
            parse_cli_version("codex-cli 0.162.1\n"),
            Some("0.162.1".into())
        );
        assert_eq!(
            parse_cli_version("codex-cli 0.164.0-alpha.2"),
            Some("0.164.0-alpha.2".into())
        );
        assert_eq!(parse_cli_version("codex-cli 0.0.0"), Some("0.0.0".into()));
        assert_eq!(
            parse_cli_version("\nWARNING x\ncodex-cli 0.163.0\n"),
            Some("0.163.0".into())
        );
        for out in ["0.162.1", "codex-cli", "codex-cli x.y", "codex-cli "] {
            assert_eq!(parse_cli_version(out), None, "{out}");
        }
    }

    #[test]
    fn version_cache_parses_real_file_and_ignores_garbage() {
        let real = br#"{"latest_version":"0.163.0","last_checked_at":"2026-10-16T05:53:49.254443Z","dismissed_version":null}"#;
        assert_eq!(
            parse_version_cache(real),
            Some(VersionCache {
                latest_version: "0.163.0".into(),
                checked_at: "2026-10-16T05:53:49Z".into(),
            })
        );
        let whole = br#"{"latest_version":"0.163.0","last_checked_at":"2026-10-16T05:53:49Z"}"#;
        assert_eq!(
            parse_version_cache(whole).map(|c| c.checked_at),
            Some("2026-10-16T05:53:49Z".into())
        );
        for body in [
            br#"{"last_checked_at":"2026-10-16T05:53:49Z"}"#.as_slice(),
            br#"{"latest_version":"abc","last_checked_at":"2026-10-16T05:53:49Z"}"#,
            b"not json",
        ] {
            assert_eq!(parse_version_cache(body), None);
        }
    }

    /// reqwest 必须启用 TLS 后端：对 https 地址真正建连并发出 TLS ClientHello。
    #[tokio::test]
    async fn https_scheme_sends_tls_client_hello() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 3];
            socket.read_exact(&mut head).await.unwrap();
            head
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let request = tokio::spawn(async move {
            let _ = client
                .get(format!("https://127.0.0.1:{port}/"))
                .send()
                .await;
        });
        let head = tokio::time::timeout(Duration::from_secs(3), accept)
            .await
            .expect("3 秒内没有收到 TLS 握手")
            .unwrap();
        request.abort();
        assert_eq!(head[0], 0x16, "首字节应为 TLS 握手记录类型");
        assert_eq!(head[1], 0x03, "第二字节应为 TLS 主版本号");
    }

    fn status(mode: ProxyMode, state: ProxyState, port: u16) -> Status {
        Status {
            mode,
            proxy: crate::types::ProxyStatus { state, port },
            ..Default::default()
        }
    }

    const NOT_RUNNING: &str = "Secure 代理未就绪（代理未运行）";
    const PORT_OCCUPIED: &str = "Secure 代理未就绪（端口被外部进程占用）";

    #[test]
    fn secure_running_goes_via_proxy() {
        assert_eq!(
            network_path_for(&status(ProxyMode::Secure, ProxyState::Running, 17891)),
            NetworkPath::ViaProxy {
                proxy_url: "http://127.0.0.1:17891".into()
            }
        );
        assert_eq!(
            network_path_for(&status(ProxyMode::Secure, ProxyState::Running, 23456)),
            NetworkPath::ViaProxy {
                proxy_url: "http://127.0.0.1:23456".into()
            }
        );
    }

    #[test]
    fn secure_not_running_is_blocked() {
        let blocked = NetworkPath::Blocked {
            detail: NOT_RUNNING,
        };
        let mut st = status(ProxyMode::Secure, ProxyState::Stopped, 17891);
        assert_eq!(network_path_for(&st), blocked);
        // 证明不看 secure.active
        st.secure.active = true;
        assert_eq!(network_path_for(&st), blocked);
    }

    #[test]
    fn secure_external_or_port_occupied_is_blocked() {
        let occupied = NetworkPath::Blocked {
            detail: PORT_OCCUPIED,
        };
        assert_eq!(
            network_path_for(&status(ProxyMode::Secure, ProxyState::External, 17891)),
            occupied
        );
        let mut st = status(ProxyMode::Secure, ProxyState::Stopped, 17891);
        st.secure.blocked_reason = Some("port_occupied".into());
        assert_eq!(network_path_for(&st), occupied);
        st.secure.blocked_reason = Some("bad_signature".into());
        assert_eq!(
            network_path_for(&st),
            NetworkPath::Blocked {
                detail: NOT_RUNNING
            }
        );
    }

    #[test]
    fn legacy_paths() {
        assert_eq!(
            network_path_for(&status(ProxyMode::Legacy, ProxyState::Running, 17891)),
            NetworkPath::ViaProxy {
                proxy_url: "http://127.0.0.1:17891".into()
            }
        );
        assert_eq!(
            network_path_for(&status(ProxyMode::Legacy, ProxyState::External, 17891)),
            NetworkPath::Direct {
                reason: Some("代理为外部实例，模式未知，已直连")
            }
        );
        assert_eq!(
            network_path_for(&status(ProxyMode::Legacy, ProxyState::Stopped, 17891)),
            NetworkPath::Direct {
                reason: Some("本地代理未运行，已直连")
            }
        );
    }

    #[test]
    fn legacy_with_stale_secure_active_still_follows_mode() {
        let mut st = status(ProxyMode::Legacy, ProxyState::Stopped, 17891);
        st.secure.active = true;
        assert_eq!(
            network_path_for(&st),
            NetworkPath::Direct {
                reason: Some("本地代理未运行，已直连")
            }
        );
    }

    #[test]
    fn blocked_builds_no_client() {
        let timeout = Duration::from_secs(15);
        assert!(
            build_client(
                &NetworkPath::Blocked {
                    detail: NOT_RUNNING
                },
                timeout
            )
            .is_none()
        );
        let via = NetworkPath::ViaProxy {
            proxy_url: "http://127.0.0.1:17891".into(),
        };
        assert!(build_client(&via, timeout).is_some());
        assert!(build_client(&NetworkPath::Direct { reason: None }, timeout).is_some());
    }
}
