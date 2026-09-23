//! 凭据存储（设计 §4.4）。
//!
//! - Windows：Credential Manager，`CRED_TYPE_GENERIC`，当前用户作用域。
//! - 其他平台：进程内内存实现（开发模式），同进程内多个实例共享。
//!
//! 只做读写删除，不日志化凭据内容；错误对象绝不携带 Token。

use std::fmt;
use std::sync::Mutex;

use crate::consts;

/// 凭据明文的包装：`Debug` 不输出内容，也不实现 `Display`，防止误写进日志或错误。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出明文。只应在写 stdout（credential.exe get）或发起鉴权请求时调用。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// 固定 target 的凭据存储。编排层通过该 trait 访问凭据，测试时注入内存实现。
pub trait CredentialStore: Send + Sync {
    /// 读取凭据；不存在返回 `Ok(None)`。
    fn read(&self) -> anyhow::Result<Option<Secret>>;
    /// 写入（覆盖）凭据。
    fn write(&self, token: &str) -> anyhow::Result<()>;
    /// 删除凭据；不存在视为成功。
    fn delete(&self) -> anyhow::Result<()>;
    /// 凭据是否存在（读取失败视为不存在）。
    fn exists(&self) -> bool {
        matches!(self.read(), Ok(Some(_)))
    }
}

/// 测试用内存实现，每个实例相互独立。
#[derive(Default)]
pub struct MemoryCredentialStore {
    inner: Mutex<Option<String>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置一个凭据。
    pub fn with_token(token: &str) -> Self {
        Self {
            inner: Mutex::new(Some(token.to_string())),
        }
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn read(&self) -> anyhow::Result<Option<Secret>> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        Ok(guard.clone().map(Secret::new))
    }

    fn write(&self, token: &str) -> anyhow::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        *guard = Some(token.to_string());
        Ok(())
    }

    fn delete(&self) -> anyhow::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        *guard = None;
        Ok(())
    }
}

/// 系统凭据存储：Windows 为 Credential Manager，其他平台为进程内内存实现。
#[derive(Debug, Clone)]
pub struct SystemCredentialStore {
    target: String,
}

impl SystemCredentialStore {
    /// 指定 target（测试可用临时 target）。
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    /// 固定的受管网关 target：`codex-helper/managed-gateway`。
    pub fn managed() -> Self {
        Self::new(consts::CREDENTIAL_TARGET)
    }

    pub fn target(&self) -> &str {
        &self.target
    }
}

impl CredentialStore for SystemCredentialStore {
    fn read(&self) -> anyhow::Result<Option<Secret>> {
        todo!("阶段 1 任务 1.4：移植 Windows Credential Manager 读取；非 Windows 为内存实现")
    }

    fn write(&self, token: &str) -> anyhow::Result<()> {
        let _ = token;
        todo!("阶段 1 任务 1.4")
    }

    fn delete(&self) -> anyhow::Result<()> {
        todo!("阶段 1 任务 1.4")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("sk-very-secret");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(secret.expose(), "sk-very-secret");
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryCredentialStore::new();
        assert!(!store.exists());
        store.write("sk-1").unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), "sk-1");
        store.delete().unwrap();
        store.delete().unwrap();
        assert!(store.read().unwrap().is_none());
    }
}
