//! 路径解析：Codex home、工具数据目录、凭据读取程序路径。
//!
//! 所有模块都通过 [`HelperPaths`] 拿路径，测试时注入临时目录，不碰真实用户目录。

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::consts;

/// 本工具读写的全部路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperPaths {
    /// Codex 配置目录（`CODEX_HOME` 或 `~/.codex`）。
    pub codex_home: PathBuf,
    /// 工具数据目录（`%LOCALAPPDATA%\CodexHelper`）。
    pub data_dir: PathBuf,
}

impl HelperPaths {
    /// 显式指定两个根目录（测试与注入用）。
    pub fn new(codex_home: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            data_dir: data_dir.into(),
        }
    }

    /// 按当前环境解析真实路径。
    pub fn detect() -> anyhow::Result<Self> {
        Ok(Self::new(default_codex_home()?, default_data_dir()?))
    }

    /// `~/.codex/config.toml`
    pub fn config_toml(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }

    /// `~/.codex/config.toml.codex-helper-bak`（固定名，覆盖）。
    pub fn config_backup(&self) -> PathBuf {
        self.codex_home.join(consts::CONFIG_BACKUP_FILE_NAME)
    }

    /// `~/.codex/.env`
    pub fn env_file(&self) -> PathBuf {
        self.codex_home.join(".env")
    }

    /// `%LOCALAPPDATA%\CodexHelper\state.json`
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// `%LOCALAPPDATA%\CodexHelper\logs`
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
}

/// Codex home：非空的 `CODEX_HOME` 优先，否则 `~/.codex`（与 Codex 自身解析一致）。
pub fn default_codex_home() -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let base = directories::BaseDirs::new().context("无法定位用户主目录")?;
    Ok(base.home_dir().join(".codex"))
}

/// 工具数据目录：`CODEX_HELPER_DATA_DIR`（仅测试 / 开发）优先，否则本地应用数据目录下的 `CodexHelper`。
///
/// Windows 上即 `%LOCALAPPDATA%\CodexHelper`。
pub fn default_data_dir() -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os(consts::DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let base = directories::BaseDirs::new().context("无法定位本地应用数据目录")?;
    Ok(base.data_local_dir().join(consts::DATA_DIR_NAME))
}

/// 当前安装目录下凭据读取程序的规范化绝对路径（写入 `auth.command`）。
///
/// 以当前可执行文件所在目录为安装目录；Windows 上去掉 `\\?\` 前缀，保证 Codex 能直接执行。
pub fn credential_command_path() -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("无法定位当前可执行文件")?;
    let dir = exe.parent().context("当前可执行文件没有上级目录")?;
    Ok(credential_command_in(dir))
}

/// 指定目录下凭据读取程序的规范化绝对路径。
pub fn credential_command_in(dir: &Path) -> String {
    let path = dir.join(consts::CREDENTIAL_EXE_NAME);
    let normalized = dunce::canonicalize(&path).unwrap_or_else(|_| {
        let dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        dir.join(consts::CREDENTIAL_EXE_NAME)
    });
    dunce::simplified(&normalized).to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_live_under_their_roots() {
        let paths = HelperPaths::new("/codex", "/data");
        assert_eq!(paths.config_toml(), Path::new("/codex/config.toml"));
        assert_eq!(
            paths.config_backup(),
            Path::new("/codex/config.toml.codex-helper-bak")
        );
        assert_eq!(paths.env_file(), Path::new("/codex/.env"));
        assert_eq!(paths.state_file(), Path::new("/data/state.json"));
        assert_eq!(paths.log_dir(), Path::new("/data/logs"));
    }

    #[test]
    fn credential_command_is_absolute_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let command = credential_command_in(dir.path());
        assert!(Path::new(&command).is_absolute());
        assert!(command.ends_with(consts::CREDENTIAL_EXE_NAME));
        assert!(!command.starts_with(r"\\?\"));
    }
}
