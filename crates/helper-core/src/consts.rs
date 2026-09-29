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

/// 本地分流代理默认端口。
pub const DEFAULT_PROXY_PORT: u16 = 17891;
/// 仅供应急覆盖本地代理端口的环境变量；界面不暴露。
pub const PROXY_PORT_ENV: &str = "CODEX_HELPER_PROXY_PORT";

/// Windows Credential Manager 中的固定凭据 target。
pub const CREDENTIAL_TARGET: &str = "codex-helper/managed-gateway";
/// 写入凭据时使用的 UserName 字段。
pub const CREDENTIAL_USER_NAME: &str = "CodexHelper";

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

/// 退出前需要检测的 Codex 引擎进程名（Windows 不区分大小写）。
pub const CODEX_PROCESS_NAME: &str = "codex.exe";

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
