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
