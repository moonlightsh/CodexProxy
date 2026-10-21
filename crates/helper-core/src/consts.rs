//! 固定参数（设计 §1.1）。全部写死，不提供界面修改。

/// 模型网关地址（明文 HTTP，`wire_api = "responses"`）。
pub const GATEWAY_BASE_URL: &str = "http://10.20.30.61:8080";
/// 模型网关主机，用于可达性检测与“恒直连”判定。
pub const GATEWAY_HOST: &str = "10.20.30.61";
/// 模型网关端口。
pub const GATEWAY_PORT: u16 = 8080;

/// 上游 SOCKS5 主机（无认证）。
pub const SOCKS5_HOST: &str = "10.20.30.61";
/// 上游 SOCKS5 端口。
pub const SOCKS5_PORT: u16 = 7891;
/// 安全上游端口（独立 mihomo-auth 实例：TLS + mTLS + SOCKS5 用户名密码，设计 §4.1）。
pub const SOCKS5_AUTH_PORT: u16 = 7892;

/// 本地分流代理默认端口。
pub const DEFAULT_PROXY_PORT: u16 = 17891;

/// Secure 激活时旧代理句柄的排空上限（设计 §9.1：旧连接最多 5 分钟排空后关闭）。
/// 超时后强制关闭，避免旧隧道永久绕过进程门禁。
pub const LEGACY_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// 仅供应急覆盖本地代理端口的环境变量；界面不暴露。
pub const PROXY_PORT_ENV: &str = "CODEX_HELPER_PROXY_PORT";

/// 固定凭据 target：Windows Credential Manager 的 target，macOS 钥匙串条目的 service。
pub const CREDENTIAL_TARGET: &str = "codex-helper/managed-gateway";
/// 写入凭据时使用的 UserName 字段。
pub const CREDENTIAL_USER_NAME: &str = "CodexHelper";
/// macOS 钥匙串条目的 account（`security -a`）；service 即 [`CREDENTIAL_TARGET`]。
pub const KEYCHAIN_ACCOUNT: &str = "codex-helper";
/// macOS 上读写钥匙串使用的系统工具（绝对路径，不查 PATH；设计 §3.1）。
pub const SECURITY_TOOL: &str = "/usr/bin/security";
/// 每次调用 `security` 子进程的超时（秒）。
pub const SECURITY_TIMEOUT_SECS: u64 = 10;

/// SOCKS5 密码的钥匙串 / Credential Manager target 前缀（设计 §5.2）：后接 device-id。
pub const PROXY_PASSWORD_TARGET_PREFIX: &str = "codex-helper/proxy-password/";
/// 客户端软件私钥的 target 前缀（设计 §5.2）：后接 device-id。
pub const PROXY_KEY_TARGET_PREFIX: &str = "codex-helper/proxy-key/";

/// 管理签名（ed25519）验证公钥，64 字节小写 hex。
///
/// ⚠️ 当前是**测试 CA 公钥**（种子 0x00..0x1f，与 `process_policy` 测试夹具同一把），
/// 仅供开发与测试验证链路；**发布前必须替换为管理员保管的管理公钥**，替换动作列入发布清单。
/// 公钥本身是公开信任锚而非秘密，可以随二进制分发（设计 §5.1）。
pub const POLICY_VERIFY_KEY_HEX: &str =
    "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";

/// `config.toml` 中受管 provider 的 id。
pub const PROVIDER_ID: &str = "managed_gateway";
/// 受管 provider 的显示名。
pub const PROVIDER_NAME: &str = "Managed Gateway";
/// 受管 provider 的 wire_api。
pub const PROVIDER_WIRE_API: &str = "responses";

/// 受管根键 `approval_policy` 的固定值（自定义审批策略）。
pub const APPROVAL_POLICY: &str = "on-request";
/// 受管根键 `approvals_reviewer` 的固定值（自动审批 reviewer）。
pub const APPROVALS_REVIEWER: &str = "auto_review";
/// 受管根键 `sandbox_mode` 的固定值（允许写工作区）。
pub const SANDBOX_MODE: &str = "workspace-write";

/// 凭据读取程序的文件名（与主程序同目录发布）。
#[cfg(windows)]
pub const CREDENTIAL_EXE_NAME: &str = "codex-helper-credential.exe";
/// 凭据读取程序的文件名（开发模式）。
#[cfg(not(windows))]
pub const CREDENTIAL_EXE_NAME: &str = "codex-helper-credential";

/// `config.toml` 的固定备份文件名（每次校正前覆盖）。
pub const CONFIG_BACKUP_FILE_NAME: &str = "config.toml.codex-helper-bak";

/// 工具数据目录名：`%LOCALAPPDATA%\CodexHelper`。
pub const DATA_DIR_NAME: &str = "CodexHelper";
/// 仅供测试与开发调试覆盖数据目录的环境变量。
pub const DATA_DIR_ENV: &str = "CODEX_HELPER_DATA_DIR";

/// 当前平台标识（core 内部使用；桌面层与前端一律读 `Status::platform`）。
pub const PLATFORM: &str = if cfg!(windows) {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else {
    "other"
};

/// 退出前需要检测的 Codex 引擎进程名（Windows 不区分大小写）。
#[cfg(windows)]
pub const CODEX_PROCESS_NAME: &str = "codex.exe";

/// 退出前需要检测的 Codex 引擎进程名。macOS 为实测值；Linux 是开发模式，不检测，
/// 这个值只为能编译。
#[cfg(not(windows))]
pub const CODEX_PROCESS_NAME: &str = "codex";

/// 网关与 SOCKS5 可达性的定时重测间隔（秒）。
pub const HEALTH_PROBE_INTERVAL_SECS: u64 = 30;

/// 开发环境检测（设计 §16）：Python 最低版本（major, minor）。
pub const PYTHON_MIN_VERSION: (u32, u32) = (3, 13);
/// pip 镜像主机（内网文档 setup-for-company-network）。
pub const PIP_MIRROR_HOST: &str = "mirrors.aliyun.com";
/// pip 镜像的 simple 索引路径（比较时忽略末尾 `/`）。
pub const PIP_MIRROR_PATH: &str = "/pypi/simple";
/// pip 镜像的推荐 index-url（展示与修复提示用）。
pub const PIP_MIRROR_INDEX_URL: &str = "http://mirrors.aliyun.com/pypi/simple/";
/// npm 镜像主机。
pub const NPM_MIRROR_HOST: &str = "registry.npmmirror.com";
/// npm 镜像的推荐 registry（展示与修复提示用）。
pub const NPM_MIRROR_REGISTRY: &str = "https://registry.npmmirror.com/";
/// 检测时单个子进程的超时（秒）。
pub const DEVENV_PROBE_TIMEOUT_SECS: u64 = 15;

/// macOS 开发环境检测：用登录 shell 取用户终端里的 PATH 时的超时（秒）。
pub const LOGIN_SHELL_TIMEOUT_SECS: u64 = 5;
/// `SHELL` 缺失或不可用时的登录 shell（macOS 默认 shell）。
pub const LOGIN_SHELL_FALLBACK: &str = "/bin/zsh";
/// macOS 上总是参与查找的常见安装目录（Homebrew：Apple Silicon / Intel）。
pub const MACOS_EXTRA_PATH_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// 组件更新检查（组件更新检查设计 §2）：ChatGPT Desktop 发布源清单（`windows-release.json`）。
pub const CHATGPT_RELEASE_FEED_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/windows-release.json";
/// 清单中 `downloadUrl` 的白名单前缀（设计 C9：https + 主机 + 路径前缀）。
pub const CHATGPT_DOWNLOAD_URL_PREFIX: &str = "https://persistent.oaistatic.com/codex-app-prod/";
/// Codex CLI 最新 release 的 GitHub API 地址。
pub const CODEX_RELEASES_API_URL: &str =
    "https://api.github.com/repos/openai/codex/releases/latest";
/// GitHub API 限流（403/429）时的回退地址：读取 302 `Location` 末段取 tag。
pub const CODEX_RELEASES_LATEST_URL: &str = "https://github.com/openai/codex/releases/latest";
/// Codex CLI release 页面前缀（后接 tag）。
pub const CODEX_RELEASE_TAG_URL_PREFIX: &str = "https://github.com/openai/codex/releases/tag/";
/// 组件更新检查单个 HTTP 请求的超时（秒）。
pub const COMPONENT_CHECK_TIMEOUT_SECS: u64 = 15;
/// 组件更新检查响应体上限（字节）：超出即判清单无效，防超大或无限流响应。
pub const COMPONENT_CHECK_MAX_BODY_BYTES: usize = 256 * 1024;
