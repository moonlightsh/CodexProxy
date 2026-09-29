//! 开发环境检测（设计 §16）：Python 3.13+ / Node.js 是否可用，pip / npm 是否启用内网文档要求的镜像源。
//!
//! 只检测、不修改任何文件。纯逻辑（版本解析、PATH 查找、pip 配置解析、镜像判定）跨平台可测；
//! [`detect`] 负责组装子进程调用与（Windows）注册表 PATH 读取。

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{consts, log};

// ---------------------------------------------------------------------------
// 对外类型（与 apps/desktop/src/types.ts 一一对应，camelCase）
// ---------------------------------------------------------------------------

/// 运行时（Python / Node.js）的检测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeState {
    /// PATH 上找不到该命令
    Missing,
    /// PATH 上第一个命中的是 Microsoft Store 占位程序（应用执行别名），并未真正安装
    StoreStub,
    /// 找到了，但无法运行或未输出可识别的版本号（原因见 `detail`）
    Broken,
    /// 版本低于要求
    TooOld,
    Ok,
}

/// 运行时检测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReport {
    pub state: RuntimeState,
    /// 命令行实际会用到的可执行文件（PATH 上第一个命中）
    pub path: Option<String>,
    /// 解析出的版本号，如 `3.13.14`
    pub version: Option<String>,
    /// 最低版本要求（展示用）；无要求为 `None`
    pub min_version: Option<String>,
    /// `Broken` 时的原因
    pub detail: Option<String>,
}

/// 镜像源检测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorState {
    /// 对应运行时不满足要求，未检测
    Skipped,
    /// 已启用内网文档要求的镜像
    Configured,
    /// 指向镜像但为 http 且 `trusted-host` 不含该主机：pip 会忽略这个源
    Untrusted,
    /// 使用默认源或其他源
    NotConfigured,
    /// 检测失败（pip / npm 不可用、超时等），原因见 `detail`
    Failed,
}

/// 镜像源检测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorReport {
    pub state: MirrorState,
    /// 当前生效的地址（已去掉 userinfo）；pip 未设置 index-url（使用默认源）时为 `None`
    pub current: Option<String>,
    /// 期望的地址（修复提示用）
    pub expected: String,
    /// `Failed` 时的原因
    pub detail: Option<String>,
}

/// 开发环境检测报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevEnvReport {
    /// Windows 为 true；其他平台为开发模式（按 `python3` 检测，仅供调试）
    pub platform_supported: bool,
    pub python: RuntimeReport,
    pub pip_mirror: MirrorReport,
    pub node: RuntimeReport,
    pub npm_mirror: MirrorReport,
}

// ---------------------------------------------------------------------------
// 版本号
// ---------------------------------------------------------------------------

/// 解析出的版本号：只比较 major.minor，`text` 保留原文（如 `3.14.0rc1`）用于展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub text: String,
}

impl Version {
    /// 是否不低于 `(major, minor)`。
    pub fn at_least(&self, min: (u32, u32)) -> bool {
        (self.major, self.minor) >= min
    }
}

/// 解析 `3.13.14` / `24.16.0` / `3.14.0rc1` 这类版本号；至少需要 major 与 minor。
fn parse_version_token(token: &str) -> Option<Version> {
    let text = token.split_whitespace().next()?;
    let mut parts = text.split('.');
    let major = leading_number(parts.next()?)?;
    let minor = leading_number(parts.next()?)?;
    Some(Version {
        major,
        minor,
        text: text.to_string(),
    })
}

fn leading_number(part: &str) -> Option<u32> {
    let end = part
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(part.len());
    part[..end].parse().ok()
}

/// 解析 `python --version` 的输出（Python 3 写 stdout，Python 2 写 stderr，调用方合并两者传入）。
pub fn parse_python_version(output: &str) -> Option<Version> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Python ")
            .and_then(parse_version_token)
    })
}

/// 解析 `node --version` 的输出（`v24.16.0`）。
pub fn parse_node_version(output: &str) -> Option<Version> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix('v').and_then(parse_version_token))
}

// ---------------------------------------------------------------------------
// PATH 查找
// ---------------------------------------------------------------------------

/// Windows 命令行能直接执行的扩展名。PATHEXT 里的其他扩展名（`.PY`、`.JS` 等）需要关联程序，不参与查找。
const RUNNABLE_EXTS: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];

/// 按 PATHEXT 的顺序取可直接执行的扩展名（小写）；PATHEXT 缺失或不含任何可执行扩展名时用默认顺序。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn runnable_exts(pathext: Option<&OsStr>) -> Vec<String> {
    let mut exts: Vec<String> = Vec::new();
    if let Some(pathext) = pathext {
        for ext in pathext.to_string_lossy().split(';') {
            let ext = ext.trim().to_ascii_lowercase();
            if RUNNABLE_EXTS.contains(&ext.as_str()) && !exts.contains(&ext) {
                exts.push(ext);
            }
        }
    }
    if exts.is_empty() {
        exts = RUNNABLE_EXTS.iter().map(|ext| (*ext).to_string()).collect();
    }
    exts
}

/// 在 PATH 各目录中按顺序查找命令，返回第一个命中（与 cmd / PowerShell 一致：逐目录、目录内按扩展名顺序）。
///
/// `exts` 为空时直接匹配文件名（非 Windows）。不搜索当前目录，跳过相对路径条目。
pub fn find_in_path(name: &str, path: &OsStr, exts: &[String]) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        if !dir.is_absolute() {
            continue;
        }
        if exts.is_empty() {
            let candidate = dir.join(name);
            if is_candidate(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_candidate(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Windows：不跟随重解析点。`WindowsApps` 下的应用执行别名是 `IO_REPARSE_TAG_APPEXECLINK`，
/// 跟随读取会失败，但命令行确实会解析到它，必须算作候选。
#[cfg(windows)]
fn is_candidate(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir())
}

/// 非 Windows（开发模式）：普通文件且带可执行位。
#[cfg(not(windows))]
fn is_candidate(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// 路径是否位于 Microsoft Store 的应用执行别名目录（`...\Microsoft\WindowsApps\`）。
pub fn is_store_alias(path: &Path) -> bool {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        .contains("\\microsoft\\windowsapps\\")
}

/// 拼接系统 Path 与用户 Path（Windows 为新进程构造环境时的顺序：系统在前、用户在后）。
#[cfg_attr(not(windows), allow(dead_code))]
fn combine_paths(parts: &[Option<String>]) -> Option<OsString> {
    let joined: Vec<&str> = parts
        .iter()
        .flatten()
        .map(|part| part.trim_matches(';'))
        .filter(|part| !part.is_empty())
        .collect();
    (!joined.is_empty()).then(|| OsString::from(joined.join(";")))
}

// ---------------------------------------------------------------------------
// pip / npm 配置判定
// ---------------------------------------------------------------------------

/// 解析 `pip config list` 的输出：每行 `section.key='value'`（值为 Python `repr`），
/// 环境变量来源的 section 为 `:env:`。返回 `section.key` → 值。无法解析的行忽略。
///
/// key 原样保留大小写：pip 按 section 名精确匹配 `global` / `install`，`[Global]` 这类节不生效。
pub fn parse_pip_config_list(output: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in output.lines() {
        let Some((key, raw)) = line.trim().split_once('=') else {
            continue;
        };
        if let Some(value) = unrepr(raw.trim()) {
            values.insert(key.trim().to_string(), value);
        }
    }
    values
}

/// 还原 Python 字符串 `repr`（单引号或双引号包裹，处理 `\\`、`\'`、`\"`、`\n`、`\t`）。
fn unrepr(raw: &str) -> Option<String> {
    let quote = raw.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = raw.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Some(out)
}

/// `pip install` 实际生效的配置值：`:env:` > `install` > `global`（与 pip 的覆盖顺序一致）。
fn pip_effective<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    [":env:", "install", "global"]
        .iter()
        .find_map(|section| values.get(&format!("{section}.{key}")))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

/// 去掉 userinfo 后的地址（展示用，避免把私有源凭据显示到界面或日志）。
pub fn display_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        Err(_) => raw.to_string(),
    }
}

/// 把 `host[:port]` / `[ipv6][:port]` 拆成主机与端口原文。
fn split_host_port(netloc: &str) -> (&str, Option<&str>) {
    if let Some(rest) = netloc.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => (netloc, None),
        };
    }
    match netloc.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (netloc, None),
    }
}

/// URL 原文 authority 中显式写出的端口。不用 `Url::port()`：它会把默认端口（如 http 的 80）
/// 归一化为 None，而 pip 使用的 `urllib.parse` 保留显式写出的端口。
fn explicit_port(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let hostport = authority
        .rsplit_once('@')
        .map_or(authority, |(_, hostport)| hostport);
    split_host_port(hostport).1?.parse().ok()
}

/// 判定 pip 镜像（输入为 `pip config list` 解析结果）。
pub fn evaluate_pip_mirror(values: &BTreeMap<String, String>) -> MirrorReport {
    let expected = consts::PIP_MIRROR_INDEX_URL.to_string();
    let Some(index) = pip_effective(values, "index-url") else {
        return MirrorReport {
            state: MirrorState::NotConfigured,
            current: None,
            expected,
            detail: None,
        };
    };
    let current = Some(display_url(index));
    let parsed = Url::parse(index).ok();
    let is_mirror = parsed.as_ref().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(consts::PIP_MIRROR_HOST))
            && url
                .path()
                .trim_end_matches('/')
                .eq_ignore_ascii_case(consts::PIP_MIRROR_PATH)
    });
    if !is_mirror {
        return MirrorReport {
            state: MirrorState::NotConfigured,
            current,
            expected,
            detail: None,
        };
    }
    let is_https = parsed.as_ref().is_some_and(|url| url.scheme() == "https");
    let index_port = explicit_port(index);
    let trusted = pip_effective(values, "trusted-host").is_some_and(|hosts| {
        hosts.split_whitespace().any(|entry| {
            let (host, port) = split_host_port(entry);
            // 与 pip `is_secure_origin` 一致：条目不带端口时匹配任意端口；带端口时只有
            // index-url 显式写了同一端口才算信任（`http://host/` 的端口是 None，不等于 80）。
            host.eq_ignore_ascii_case(consts::PIP_MIRROR_HOST)
                && port.is_none_or(|port| port.parse::<u16>().ok() == index_port)
        })
    });
    MirrorReport {
        state: if is_https || trusted {
            MirrorState::Configured
        } else {
            MirrorState::Untrusted
        },
        current,
        expected,
        detail: None,
    }
}

/// 判定 npm 镜像（输入为 `npm config get registry` 的 stdout）。
pub fn evaluate_npm_registry(output: &str) -> MirrorReport {
    let expected = consts::NPM_MIRROR_REGISTRY.to_string();
    let Some(registry) = output.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return MirrorReport {
            state: MirrorState::Failed,
            current: None,
            expected,
            detail: Some("npm 没有输出 registry".to_string()),
        };
    };
    let is_mirror = Url::parse(registry).ok().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(consts::NPM_MIRROR_HOST))
    });
    MirrorReport {
        state: if is_mirror {
            MirrorState::Configured
        } else {
            MirrorState::NotConfigured
        },
        current: Some(display_url(registry)),
        expected,
        detail: None,
    }
}

fn skipped(expected: &str) -> MirrorReport {
    MirrorReport {
        state: MirrorState::Skipped,
        current: None,
        expected: expected.to_string(),
        detail: None,
    }
}

fn mirror_failed(expected: &str, detail: String) -> MirrorReport {
    MirrorReport {
        state: MirrorState::Failed,
        current: None,
        expected: expected.to_string(),
        detail: Some(detail),
    }
}

// ---------------------------------------------------------------------------
// 子进程与组装
// ---------------------------------------------------------------------------

/// Windows：子进程不弹控制台窗口（本工具是 GUI 程序）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 命令行里的 Python 命令名（内网文档要求 `python`）；开发模式下 macOS 只有 `python3`。
#[cfg(windows)]
const PYTHON_COMMAND: &str = "python";
#[cfg(not(windows))]
const PYTHON_COMMAND: &str = "python3";
const NODE_COMMAND: &str = "node";
const NPM_COMMAND: &str = "npm";

/// 查找命令用的环境：PATH 与可执行扩展名。子进程的 PATH 也设为这一份，保证与查找结果一致。
#[derive(Debug, Clone)]
pub struct SearchEnv {
    pub path: OsString,
    pub exts: Vec<String>,
}

impl SearchEnv {
    pub fn new(path: impl Into<OsString>, exts: Vec<String>) -> Self {
        Self {
            path: path.into(),
            exts,
        }
    }

    /// 当前系统的查找环境。
    ///
    /// Windows 从注册表重新读取“系统 Path + 用户 Path”：工具常驻托盘，进程启动时继承的 PATH
    /// 看不到之后安装的 Python / Node.js；读取失败时退回进程 PATH。其他平台用进程 PATH。
    pub fn system() -> Self {
        let process_path = std::env::var_os("PATH").unwrap_or_default();
        #[cfg(windows)]
        {
            let path = windows_env::fresh_path().unwrap_or(process_path);
            Self::new(path, runnable_exts(std::env::var_os("PATHEXT").as_deref()))
        }
        #[cfg(not(windows))]
        {
            Self::new(process_path, Vec::new())
        }
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        find_in_path(name, &self.path, &self.exts)
    }
}

struct CommandOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

fn exit_text(code: Option<i32>) -> String {
    code.map_or_else(|| "被终止".to_string(), |code| format!("退出码 {code}"))
}

/// 运行一个检测命令：参数全部是常量；stdin 为空、工作目录为系统临时目录、超时即结束进程。
async fn run(
    program: &Path,
    args: &[&str],
    env: &SearchEnv,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("PATH", &env.path)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let child = command
        .spawn()
        .map_err(|error| format!("无法启动：{error}"))?;
    // Windows 上 `.cmd` / `.bat` 经 cmd.exe 运行（如 npm.cmd → node.exe），kill_on_drop 只结束
    // 直接子进程。放进“关闭即结束”的 Job Object，函数返回（含超时）时整棵进程树一起结束；
    // 创建或关联失败时退回 kill_on_drop。
    #[cfg(windows)]
    let _job = child
        .raw_handle()
        .and_then(windows_job::KillOnCloseJob::assign);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => Err(format!("超过 {} 秒未完成", timeout.as_secs())),
        Ok(Err(error)) => Err(format!("执行失败：{error}")),
        Ok(Ok(output)) => Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    }
}

/// 检测一个运行时：取 PATH 上第一个命中（命令行实际会用到的那个）并运行 `--version`。
/// 返回报告，以及满足要求时的可执行文件路径（供后续镜像检测）。
async fn probe_runtime(
    env: &SearchEnv,
    name: &str,
    parse: fn(&str) -> Option<Version>,
    min: Option<(u32, u32)>,
    timeout: Duration,
) -> (RuntimeReport, Option<PathBuf>) {
    let min_version = min.map(|(major, minor)| format!("{major}.{minor}"));
    let Some(path) = env.find(name) else {
        let report = RuntimeReport {
            state: RuntimeState::Missing,
            path: None,
            version: None,
            min_version,
            detail: None,
        };
        return (report, None);
    };
    let shown = Some(path.display().to_string());
    let version = match run(&path, &["--version"], env, timeout).await {
        Ok(output) => parse(&format!("{}\n{}", output.stdout, output.stderr)).ok_or_else(|| {
            format!(
                "{name} --version 没有输出版本号（{}）",
                exit_text(output.code)
            )
        }),
        Err(error) => Err(format!("{name} --version {error}")),
    };
    match version {
        Err(detail) => {
            let state = if is_store_alias(&path) {
                RuntimeState::StoreStub
            } else {
                RuntimeState::Broken
            };
            let report = RuntimeReport {
                state,
                path: shown,
                version: None,
                min_version,
                detail: Some(detail),
            };
            (report, None)
        }
        Ok(version) => {
            let ok = min.is_none_or(|min| version.at_least(min));
            let report = RuntimeReport {
                state: if ok {
                    RuntimeState::Ok
                } else {
                    RuntimeState::TooOld
                },
                path: shown,
                version: Some(version.text),
                min_version,
                detail: None,
            };
            (report, ok.then_some(path))
        }
    }
}

async fn detect_python(env: &SearchEnv, timeout: Duration) -> (RuntimeReport, MirrorReport) {
    let expected = consts::PIP_MIRROR_INDEX_URL;
    let (report, python) = probe_runtime(
        env,
        PYTHON_COMMAND,
        parse_python_version,
        Some(consts::PYTHON_MIN_VERSION),
        timeout,
    )
    .await;
    let Some(python) = python else {
        return (report, skipped(expected));
    };
    // -P：不把工作目录加入 sys.path（3.11 起支持，这里已确认 ≥ 3.13），避免导入临时目录里的同名模块。
    let args = ["-P", "-m", "pip", "config", "list"];
    let mirror = match run(&python, &args, env, timeout).await {
        Err(error) => mirror_failed(expected, format!("pip config list {error}")),
        Ok(output) if output.success() => {
            evaluate_pip_mirror(&parse_pip_config_list(&output.stdout))
        }
        Ok(output) if output.stderr.contains("No module named pip") => {
            mirror_failed(expected, "当前 Python 没有安装 pip".to_string())
        }
        Ok(output) => mirror_failed(
            expected,
            format!("pip config list 执行失败（{}）", exit_text(output.code)),
        ),
    };
    (report, mirror)
}

async fn detect_node(env: &SearchEnv, timeout: Duration) -> (RuntimeReport, MirrorReport) {
    let expected = consts::NPM_MIRROR_REGISTRY;
    let (report, node) = probe_runtime(env, NODE_COMMAND, parse_node_version, None, timeout).await;
    if node.is_none() {
        return (report, skipped(expected));
    }
    let Some(npm) = env.find(NPM_COMMAND) else {
        return (
            report,
            mirror_failed(expected, "PATH 上找不到 npm".to_string()),
        );
    };
    let mirror = match run(&npm, &["config", "get", "registry"], env, timeout).await {
        Err(error) => mirror_failed(expected, format!("npm config get registry {error}")),
        Ok(output) if output.success() => evaluate_npm_registry(&output.stdout),
        Ok(output) => mirror_failed(
            expected,
            format!(
                "npm config get registry 执行失败（{}）",
                exit_text(output.code)
            ),
        ),
    };
    (report, mirror)
}

/// 在指定查找环境下检测（测试注入假命令用）。Python 与 Node.js 两条线并行。
pub async fn detect_in(env: &SearchEnv, timeout: Duration) -> DevEnvReport {
    let ((python, pip_mirror), (node, npm_mirror)) =
        tokio::join!(detect_python(env, timeout), detect_node(env, timeout));
    DevEnvReport {
        platform_supported: cfg!(windows),
        python,
        pip_mirror,
        node,
        npm_mirror,
    }
}

/// 检测当前系统。日志只记录各项状态与版本号，不记录路径与地址。
pub async fn detect() -> DevEnvReport {
    let env = SearchEnv::system();
    let timeout = Duration::from_secs(consts::DEVENV_PROBE_TIMEOUT_SECS);
    let report = detect_in(&env, timeout).await;
    log::event(
        "devenv.detect",
        json!({
            "python": report.python.state,
            "python_version": report.python.version,
            "pip_mirror": report.pip_mirror.state,
            "node": report.node.state,
            "node_version": report.node.version,
            "npm_mirror": report.npm_mirror.state,
        }),
    );
    report
}

// ---------------------------------------------------------------------------
// Windows：从注册表读取最新的系统 / 用户 Path
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod windows_env {
    use std::ffi::OsString;

    use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, REG_VALUE_TYPE, RRF_NOEXPAND,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::PCWSTR;

    use super::combine_paths;

    const SYSTEM_ENV_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const USER_ENV_KEY: &str = "Environment";

    /// 系统 Path 在前、用户 Path 在后；两者都读不到时返回 `None`。
    pub fn fresh_path() -> Option<OsString> {
        combine_paths(&[
            read_string(HKEY_LOCAL_MACHINE, SYSTEM_ENV_KEY, "Path"),
            read_string(HKEY_CURRENT_USER, USER_ENV_KEY, "Path"),
        ])
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn until_nul(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    /// 读取字符串值；`REG_EXPAND_SZ` 用当前进程环境展开（与 Explorer 为新进程构造 PATH 的方式一致）。
    fn read_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
        let subkey = wide(subkey);
        let value = wide(value);
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut buffer = vec![0u16; 2048];
        // 值可能在两次调用之间变长：ERROR_MORE_DATA 时按返回的字节数扩容重试。
        for _ in 0..4 {
            let mut kind = REG_VALUE_TYPE::default();
            let mut bytes = u32::try_from(buffer.len() * 2).ok()?;
            // SAFETY：pvdata 指向 buffer，pcbdata 初值为 buffer 的字节长度，RegGetValueW 不会越界写入。
            let status = unsafe {
                RegGetValueW(
                    root,
                    PCWSTR(subkey.as_ptr()),
                    PCWSTR(value.as_ptr()),
                    flags,
                    Some(&raw mut kind),
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&raw mut bytes),
                )
            };
            if status == ERROR_MORE_DATA {
                buffer.resize((bytes as usize).div_ceil(2) + 1, 0);
                continue;
            }
            if status != ERROR_SUCCESS {
                return None;
            }
            let len = (bytes as usize / 2).min(buffer.len());
            let text = until_nul(&buffer[..len]);
            return Some(if kind == REG_EXPAND_SZ {
                expand(&text).unwrap_or(text)
            } else {
                text
            });
        }
        None
    }

    fn expand(text: &str) -> Option<String> {
        let source = wide(text);
        // SAFETY：第一次传 None 只查询所需长度（含结尾 NUL）；第二次传入该长度的缓冲区。
        let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
        if needed == 0 {
            return None;
        }
        let mut out = vec![0u16; needed as usize];
        let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut out)) };
        if written == 0 || written as usize > out.len() {
            return None;
        }
        Some(until_nul(&out))
    }

    /// 直接测注册表读取（`SearchEnv::system()` 失败时会退回进程 PATH，测它证明不了读的是注册表）。
    /// 本机（macOS）只做交叉编译检查，运行验证在 CI 的 `windows-latest` 上（设计 §11、§16.4）。
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reads_system_path_value_from_registry() {
            let path = read_string(HKEY_LOCAL_MACHINE, SYSTEM_ENV_KEY, "Path")
                .expect("注册表系统环境键里应有 Path");
            assert!(!path.is_empty());
        }

        #[test]
        fn fresh_path_is_read_and_expanded() {
            let path = fresh_path()
                .expect("应能从注册表读到 Path")
                .to_string_lossy()
                .to_lowercase();
            assert!(path.contains("system32"), "{path}");
            // 系统 Path 通常以 REG_EXPAND_SZ 保存 %SystemRoot%\system32，展开后不应再出现变量引用。
            assert!(!path.contains("%systemroot%"), "{path}");
        }
    }
}

/// 子进程树的生命周期绑定（设计 §16.3）：句柄关闭时 Windows 结束 job 内的全部进程。
#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::RawHandle;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    pub struct KillOnCloseJob(HANDLE);

    // SAFETY：job 是内核对象句柄，可在任意线程使用与关闭；这里只独占持有并在 Drop 时关闭一次，
    // 跨 await 所在线程转移（tokio 多线程调度）不会造成数据竞争。
    unsafe impl Send for KillOnCloseJob {}

    impl KillOnCloseJob {
        /// 创建带 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的匿名 job 并把进程放进去；任何一步失败返回 `None`。
        /// spawn 与关联之间有极短窗口，这期间子进程若已拉起孙进程，孙进程不在 job 内（残余风险见 §16.4）。
        pub fn assign(process: RawHandle) -> Option<Self> {
            // SAFETY：无安全属性、无名称，创建匿名 job；返回的句柄立即交给 RAII 包装，Drop 时关闭。
            let job = Self(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
            // SAFETY：info 在调用期间有效，长度与 JobObjectExtendedLimitInformation 要求的结构一致。
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    len,
                )
            }
            .ok()?;
            // SAFETY：process 是 tokio Child 持有的进程句柄，调用期间 Child 仍存活，句柄有效。
            unsafe { AssignProcessToJobObject(job.0, HANDLE(process)) }.ok()?;
            Some(job)
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY：句柄由 CreateJobObjectW 返回且只在这里关闭一次；关闭后 job 内进程被结束。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn python_version_parses_stdout_stderr_and_prerelease() {
        let v = parse_python_version("Python 3.13.14\n").unwrap();
        assert_eq!((v.major, v.minor, v.text.as_str()), (3, 13, "3.13.14"));
        assert_eq!(
            parse_python_version("\nPython 2.7.18\r\n").unwrap().minor,
            7
        );
        assert_eq!(
            parse_python_version("Python 3.14.0rc1").unwrap().text,
            "3.14.0rc1"
        );
        // Microsoft Store 占位程序的提示文本不是版本号。
        assert!(parse_python_version("Python was not found; run without arguments").is_none());
        assert!(parse_python_version("").is_none());
        assert!(parse_python_version("Python 3").is_none());
    }

    #[test]
    fn node_version_parses_v_prefix_only() {
        let v = parse_node_version("v24.16.0\n").unwrap();
        assert_eq!((v.major, v.minor, v.text.as_str()), (24, 16, "24.16.0"));
        assert!(parse_node_version("24.16.0").is_none());
        assert!(parse_node_version("node: command not found").is_none());
    }

    #[test]
    fn version_minimum_compares_major_then_minor() {
        let min = consts::PYTHON_MIN_VERSION;
        assert!(parse_python_version("Python 3.13.0").unwrap().at_least(min));
        assert!(parse_python_version("Python 3.14.1").unwrap().at_least(min));
        assert!(parse_python_version("Python 4.0.0").unwrap().at_least(min));
        assert!(!parse_python_version("Python 3.12.9").unwrap().at_least(min));
        assert!(!parse_python_version("Python 2.99.0").unwrap().at_least(min));
    }

    #[test]
    fn runnable_exts_keep_pathext_order_and_drop_non_runnable() {
        let exts = runnable_exts(Some(OsStr::new(".COM;.EXE;.BAT;.CMD;.VBS;.JS;.PY;.exe")));
        assert_eq!(exts, [".com", ".exe", ".bat", ".cmd"]);
        assert_eq!(
            runnable_exts(Some(OsStr::new(".CMD; .EXE"))),
            [".cmd", ".exe"]
        );
        assert_eq!(runnable_exts(Some(OsStr::new(".PY"))), RUNNABLE_EXTS);
        assert_eq!(runnable_exts(None), RUNNABLE_EXTS);
    }

    #[test]
    fn combine_paths_puts_system_first_and_skips_empty() {
        let joined = combine_paths(&[Some("C:\\sys;".into()), Some(";C:\\user".into())]);
        assert_eq!(joined, Some(OsString::from("C:\\sys;C:\\user")));
        assert_eq!(
            combine_paths(&[None, Some("C:\\u".into())]),
            Some("C:\\u".into())
        );
        assert_eq!(combine_paths(&[Some(";".into()), None]), None);
    }

    #[test]
    fn store_alias_detection_is_case_insensitive() {
        assert!(is_store_alias(Path::new(
            r"C:\Users\a\AppData\Local\Microsoft\WindowsApps\python.exe"
        )));
        assert!(is_store_alias(Path::new(
            "C:/Users/a/AppData/Local/microsoft/windowsapps/python.exe"
        )));
        assert!(!is_store_alias(Path::new(
            r"C:\Users\a\AppData\Local\Programs\Python\Python313\python.exe"
        )));
    }

    #[test]
    fn pip_config_list_parses_repr_values() {
        let output = "global.index-url='http://mirrors.aliyun.com/pypi/simple/'\n\
                      install.trusted-host='mirrors.aliyun.com\\nexample.com'\n\
                      :env:.cert=\"C:\\\\certs\\\\it's.pem\"\n\
                      WARNING: something\n";
        let values = parse_pip_config_list(output);
        assert_eq!(
            values["global.index-url"],
            "http://mirrors.aliyun.com/pypi/simple/"
        );
        assert_eq!(
            values["install.trusted-host"],
            "mirrors.aliyun.com\nexample.com"
        );
        assert_eq!(values[":env:.cert"], "C:\\certs\\it's.pem");
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn pip_mirror_default_source_is_not_configured() {
        let report = evaluate_pip_mirror(&BTreeMap::new());
        assert_eq!(report.state, MirrorState::NotConfigured);
        assert_eq!(report.current, None);
        assert_eq!(report.expected, consts::PIP_MIRROR_INDEX_URL);
    }

    #[test]
    fn pip_mirror_http_requires_trusted_host() {
        let url = "http://mirrors.aliyun.com/pypi/simple/";
        let untrusted = evaluate_pip_mirror(&config(&[("global.index-url", url)]));
        assert_eq!(untrusted.state, MirrorState::Untrusted);
        assert_eq!(untrusted.current.as_deref(), Some(url));

        // 条目不带端口时匹配任意端口；带端口时 index-url 必须显式写出同一端口（pip 语义）。
        let port80 = "http://mirrors.aliyun.com:80/pypi/simple/";
        for (index, trusted, state) in [
            (url, "mirrors.aliyun.com", MirrorState::Configured),
            (url, "pypi.org\nMIRRORS.aliyun.com", MirrorState::Configured),
            (
                url,
                "pypi.org\nMIRRORS.aliyun.com:80",
                MirrorState::Untrusted,
            ),
            (port80, "mirrors.aliyun.com:80", MirrorState::Configured),
            (port80, "mirrors.aliyun.com", MirrorState::Configured),
            (port80, "mirrors.aliyun.com:8080", MirrorState::Untrusted),
        ] {
            let values = config(&[
                ("global.index-url", index),
                ("install.trusted-host", trusted),
            ]);
            assert_eq!(
                evaluate_pip_mirror(&values).state,
                state,
                "{index} / {trusted}"
            );
        }
        let https = config(&[("global.index-url", "https://mirrors.aliyun.com/pypi/simple")]);
        assert_eq!(evaluate_pip_mirror(&https).state, MirrorState::Configured);
    }

    #[test]
    fn pip_mirror_follows_pip_override_order() {
        let mirror = "https://mirrors.aliyun.com/pypi/simple/";
        let other = "https://pypi.tuna.tsinghua.edu.cn/simple";
        // install 节覆盖 global 节。
        let values = config(&[("global.index-url", mirror), ("install.index-url", other)]);
        let report = evaluate_pip_mirror(&values);
        assert_eq!(report.state, MirrorState::NotConfigured);
        assert_eq!(report.current.as_deref(), Some(other));
        // PIP_INDEX_URL 覆盖配置文件。
        let values = config(&[("install.index-url", other), (":env:.index-url", mirror)]);
        assert_eq!(evaluate_pip_mirror(&values).state, MirrorState::Configured);
        // trusted-host 同样取优先级最高的一处，不跨节合并。
        let values = config(&[
            ("global.index-url", "http://mirrors.aliyun.com/pypi/simple/"),
            ("global.trusted-host", "mirrors.aliyun.com"),
            ("install.trusted-host", "example.com"),
        ]);
        assert_eq!(evaluate_pip_mirror(&values).state, MirrorState::Untrusted);
        // 节名大小写敏感：[Global] 在 pip 里不生效。
        let values = config(&[("Global.index-url", mirror)]);
        assert_eq!(
            evaluate_pip_mirror(&values).state,
            MirrorState::NotConfigured
        );
    }

    #[test]
    fn pip_mirror_rejects_wrong_path_or_host_and_hides_userinfo() {
        for url in [
            "https://mirrors.aliyun.com/simple/",
            "https://mirrors.aliyun.com.evil.test/pypi/simple/",
            "not a url",
        ] {
            let report = evaluate_pip_mirror(&config(&[("global.index-url", url)]));
            assert_eq!(report.state, MirrorState::NotConfigured, "{url}");
        }
        let values = config(&[("global.index-url", "https://user:s3cret@nexus.test/simple/")]);
        let current = evaluate_pip_mirror(&values).current.unwrap();
        assert_eq!(current, "https://nexus.test/simple/");
    }

    #[test]
    fn npm_registry_matches_mirror_host() {
        let report = evaluate_npm_registry("https://registry.npmmirror.com/\n");
        assert_eq!(report.state, MirrorState::Configured);
        assert_eq!(
            report.current.as_deref(),
            Some("https://registry.npmmirror.com/")
        );
        let report = evaluate_npm_registry("https://registry.npmjs.org/\n");
        assert_eq!(report.state, MirrorState::NotConfigured);
        let report = evaluate_npm_registry("https://registry.npmmirror.com.evil.test/");
        assert_eq!(report.state, MirrorState::NotConfigured);
        let report = evaluate_npm_registry("https://u:p@npm.corp.test/");
        assert_eq!(report.current.as_deref(), Some("https://npm.corp.test/"));
        assert_eq!(evaluate_npm_registry(" \n").state, MirrorState::Failed);
    }

    #[test]
    fn report_serializes_camel_case() {
        let report = DevEnvReport {
            platform_supported: false,
            python: RuntimeReport {
                state: RuntimeState::StoreStub,
                path: None,
                version: None,
                min_version: Some("3.13".into()),
                detail: None,
            },
            pip_mirror: skipped("x"),
            node: RuntimeReport {
                state: RuntimeState::TooOld,
                path: None,
                version: None,
                min_version: None,
                detail: None,
            },
            npm_mirror: evaluate_npm_registry("https://registry.npmmirror.com/"),
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["platformSupported"], false);
        assert_eq!(json["python"]["state"], "storeStub");
        assert_eq!(json["python"]["minVersion"], "3.13");
        assert_eq!(json["pipMirror"]["state"], "skipped");
        assert_eq!(json["node"]["state"], "tooOld");
        assert_eq!(json["npmMirror"]["state"], "configured");
    }
}
