//! 凭据存储（设计 §4.4）。
//!
//! - Windows：Credential Manager，`CRED_TYPE_GENERIC`，当前用户作用域。
//! - 其他平台：进程内内存实现（“开发模式”）：所有 `SystemCredentialStore` 实例按 target
//!   共享同一张全局表，但这张表只存在于当前进程的内存中——换一个进程（例如独立编译
//!   运行的 `credential` 读取程序）在非 Windows 上执行时看到的是它自己进程里的空表，
//!   读不到本进程写入的任何内容。仅用于本机开发与非 Windows 测试，不做跨进程 / 跨
//!   重启持久化，也不落盘。
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

/// 系统凭据存储：Windows 为 Credential Manager，其他平台为进程内内存实现（开发模式，
/// 见模块文档）。
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

#[cfg(windows)]
impl CredentialStore for SystemCredentialStore {
    fn read(&self) -> anyhow::Result<Option<Secret>> {
        Ok(windows_backend::read(&self.target)?.map(Secret::new))
    }

    fn write(&self, token: &str) -> anyhow::Result<()> {
        windows_backend::write(&self.target, token)
    }

    fn delete(&self) -> anyhow::Result<()> {
        windows_backend::delete(&self.target)
    }
}

#[cfg(not(windows))]
impl CredentialStore for SystemCredentialStore {
    fn read(&self) -> anyhow::Result<Option<Secret>> {
        Ok(dev_store::read(&self.target)?.map(Secret::new))
    }

    fn write(&self, token: &str) -> anyhow::Result<()> {
        dev_store::write(&self.target, token)
    }

    fn delete(&self) -> anyhow::Result<()> {
        dev_store::delete(&self.target)
    }
}

/// Windows Credential Manager 的具体读写实现（`CRED_TYPE_GENERIC`）。
#[cfg(windows)]
mod windows_backend {
    use windows::Win32::Security::Credentials::{
        CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_ENTERPRISE, CRED_TYPE_GENERIC, CREDENTIALW,
        CredDeleteW, CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{HRESULT, PCWSTR, PWSTR};

    use crate::consts;

    /// ERROR_NOT_FOUND：凭据不存在。
    const ERROR_NOT_FOUND: u32 = 1168;

    /// windows crate 把 Win32 BOOL 失败包装成 `Result`，错误码以 HRESULT 形式携带。
    fn is_not_found(err: &windows::core::Error) -> bool {
        err.code() == HRESULT::from_win32(ERROR_NOT_FOUND)
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 读取指定 target 的凭据；不存在返回 `Ok(None)`。
    pub(super) fn read(target: &str) -> anyhow::Result<Option<String>> {
        let mut credential_ptr: *mut CREDENTIALW = std::ptr::null_mut();
        let target_wide = to_wide(target);
        // SAFETY: target_wide 是以 NUL 结尾的宽字符缓冲区，在本次调用期间一直存活；
        // credential_ptr 是有效的输出指针。调用成功时 Credential Manager 会为它分配
        // 一块内存，调用方（下面）负责用 CredFree 释放。
        let result = unsafe {
            CredReadW(
                PCWSTR(target_wide.as_ptr()),
                CRED_TYPE_GENERIC,
                0,
                &mut credential_ptr,
            )
        };
        if let Err(err) = result {
            if is_not_found(&err) {
                return Ok(None);
            }
            anyhow::bail!("读取凭据失败（code={:#x}）", err.code().0);
        }
        // SAFETY: CredReadW 刚刚成功返回，credential_ptr 指向一块由 Credential Manager
        // 分配、已初始化的 CREDENTIALW，在下面 CredFree 之前一直有效。
        let credential = unsafe { &*credential_ptr };
        // 空 blob 视为凭据不存在（例如曾被写入过一条空记录）：CredentialBlobSize 为 0 时
        // CredentialBlob 很可能是空指针，`slice::from_raw_parts` 即使长度为 0 也要求指针
        // 非空且对齐，所以必须在构造切片之前就判空，不能等切片造出来再判断 is_empty。
        let decoded = if credential.CredentialBlobSize == 0 || credential.CredentialBlob.is_null() {
            None
        } else {
            // SAFETY: 上面已确认 CredentialBlobSize != 0 且 CredentialBlob 非空；
            // 二者由 Credential Manager 保证互相一致（指针指向至少 CredentialBlobSize
            // 字节、已初始化的内存），该内存在下面 CredFree 之前一直有效，这里只读不写。
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    credential.CredentialBlob as *const u8,
                    credential.CredentialBlobSize as usize,
                )
            };
            Some(String::from_utf8(bytes.to_vec()))
        };
        // SAFETY: credential_ptr 之后不再使用；无论上面 UTF-8 解码是否成功都必须释放，
        // 否则会泄漏 Credential Manager 分配的内存。
        unsafe { CredFree(credential_ptr as *const core::ffi::c_void) };
        match decoded {
            None => Ok(None),
            Some(Ok(token)) => Ok(Some(token)),
            Some(Err(_)) => anyhow::bail!("凭据内容不是有效 UTF-8"),
        }
    }

    /// 写入（覆盖）指定 target 的凭据。
    pub(super) fn write(target: &str, token: &str) -> anyhow::Result<()> {
        let max = CRED_MAX_CREDENTIAL_BLOB_SIZE as usize;
        if token.len() > max {
            anyhow::bail!("凭据过长：超过 Credential Manager 单条上限（{max} 字节）");
        }
        let target_wide = to_wide(target);
        let user_name_wide = to_wide(consts::CREDENTIAL_USER_NAME);
        let credential = CREDENTIALW {
            Flags: Default::default(),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target_wide.as_ptr() as *mut u16),
            Comment: PWSTR::null(),
            LastWritten: Default::default(),
            CredentialBlobSize: token.len() as u32,
            CredentialBlob: token.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_ENTERPRISE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR(user_name_wide.as_ptr() as *mut u16),
        };
        // SAFETY: credential 中引用的三个缓冲区（target_wide、token、user_name_wide）
        // 都是本函数栈上的局部变量，存活到调用结束；CredWriteW 只在调用期间读取它们
        // （只需要 `*const CREDENTIALW`），不保留指针，也不需要写回 credential 本身。
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|err| anyhow::anyhow!("写入凭据失败（code={:#x}）", err.code().0))
    }

    /// 删除指定 target 的凭据；不存在视为成功。
    pub(super) fn delete(target: &str) -> anyhow::Result<()> {
        let target_wide = to_wide(target);
        // SAFETY: target_wide 在调用期间存活；CredDeleteW 只读取该缓冲区。
        let result = unsafe { CredDeleteW(PCWSTR(target_wide.as_ptr()), CRED_TYPE_GENERIC, 0) };
        if let Err(err) = result {
            if is_not_found(&err) {
                return Ok(());
            }
            anyhow::bail!("删除凭据失败（code={:#x}）", err.code().0);
        }
        Ok(())
    }
}

/// 非 Windows 平台的“开发模式”实现：进程内全局表，按 target 共享（见模块文档）。
#[cfg(not(windows))]
mod dev_store {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn table() -> &'static Mutex<HashMap<String, String>> {
        static TABLE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) fn read(target: &str) -> anyhow::Result<Option<String>> {
        let guard = table()
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        Ok(guard.get(target).cloned())
    }

    pub(super) fn write(target: &str, token: &str) -> anyhow::Result<()> {
        let mut guard = table()
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        guard.insert(target.to_string(), token.to_string());
        Ok(())
    }

    pub(super) fn delete(target: &str) -> anyhow::Result<()> {
        let mut guard = table()
            .lock()
            .map_err(|_| anyhow::anyhow!("凭据锁已损坏"))?;
        guard.remove(target);
        Ok(())
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

/// 非 Windows 下 `SystemCredentialStore`（开发模式）的行为测试。
#[cfg(all(test, not(windows)))]
mod dev_mode_tests {
    use super::*;

    #[test]
    fn write_read_overwrite_delete_and_idempotent_delete() {
        let store = SystemCredentialStore::new("codex-helper/test-round-trip");
        assert!(store.read().unwrap().is_none());
        assert!(!store.exists());

        store.write("sk-first").unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), "sk-first");
        assert!(store.exists());

        // 覆盖
        store.write("sk-second").unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), "sk-second");

        // 删除；重复删除幂等
        store.delete().unwrap();
        store.delete().unwrap();
        assert!(store.read().unwrap().is_none());
        assert!(!store.exists());
    }

    #[test]
    fn different_targets_are_isolated() {
        let a = SystemCredentialStore::new("codex-helper/test-isolated-a");
        let b = SystemCredentialStore::new("codex-helper/test-isolated-b");

        a.write("sk-a").unwrap();
        assert!(b.read().unwrap().is_none());

        b.write("sk-b").unwrap();
        assert_eq!(a.read().unwrap().unwrap().expose(), "sk-a");
        assert_eq!(b.read().unwrap().unwrap().expose(), "sk-b");

        a.delete().unwrap();
        b.delete().unwrap();
    }

    #[test]
    fn instances_with_same_target_share_storage() {
        let target = "codex-helper/test-shared-instance";
        let first = SystemCredentialStore::new(target);
        let second = SystemCredentialStore::new(target);

        first.write("sk-shared").unwrap();
        assert_eq!(second.read().unwrap().unwrap().expose(), "sk-shared");

        second.delete().unwrap();
        assert!(first.read().unwrap().is_none());
    }
}

/// Windows 下 `SystemCredentialStore`（Credential Manager）的行为测试。
///
/// 本机（macOS）不会编译这一段，只由 `scripts/check-windows.sh` 做交叉编译检查；
/// 真正的运行验证在 CI 的 `windows-latest` 上进行（设计 §11、§13）。
#[cfg(all(test, windows))]
mod windows_mode_tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows::Win32::Security::Credentials::CRED_MAX_CREDENTIAL_BLOB_SIZE;

    use super::*;

    /// 生成形如 `codex-helper/test-<pid>-<纳秒>-<计数>` 的临时 target，避免与真实凭据
    /// 及并行测试互相冲突。
    fn unique_test_target() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("codex-helper/test-{pid}-{nanos}-{seq}")
    }

    /// 测试结束（含 panic 时）务必删除临时凭据，避免残留在开发机的 Credential Manager 中。
    struct TempTargetGuard(SystemCredentialStore);

    impl Drop for TempTargetGuard {
        fn drop(&mut self) {
            let _ = self.0.delete();
        }
    }

    #[test]
    fn write_read_overwrite_delete_round_trip_on_temp_target() {
        let store = SystemCredentialStore::new(unique_test_target());
        let _guard = TempTargetGuard(store.clone());

        // 初始不存在
        assert_eq!(store.read().unwrap(), None);
        // 写入并读取
        store.write("sk-first").unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), "sk-first");
        // 覆盖
        store.write("sk-second").unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), "sk-second");
        // 清理；重复删除幂等
        store.delete().unwrap();
        store.delete().unwrap();
        assert_eq!(store.read().unwrap(), None);
    }

    #[test]
    fn write_rejects_oversized_token() {
        let store = SystemCredentialStore::new(unique_test_target());
        let _guard = TempTargetGuard(store.clone());

        let oversized = "a".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize + 1);
        let err = store.write(&oversized).unwrap_err();
        let message = err.to_string();
        // 错误消息只应说明超限（含上限字节数），不携带 Token 内容。
        assert!(!message.contains(&oversized));
        assert!(message.contains("过长"));
        assert!(message.contains(&CRED_MAX_CREDENTIAL_BLOB_SIZE.to_string()));
        // 校验发生在真正调用 Win32 API 之前，凭据不应被写入。
        assert_eq!(store.read().unwrap(), None);
    }

    #[test]
    fn write_accepts_token_at_exact_size_limit() {
        let store = SystemCredentialStore::new(unique_test_target());
        let _guard = TempTargetGuard(store.clone());

        let boundary = "a".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);
        store.write(&boundary).unwrap();
        assert_eq!(store.read().unwrap().unwrap().expose(), boundary);
    }

    #[test]
    fn empty_blob_is_treated_as_absent() {
        let store = SystemCredentialStore::new(unique_test_target());
        let _guard = TempTargetGuard(store.clone());

        // 空字符串长度不超限，能通过写入前的长度校验；写入后 CredentialBlobSize 为 0，
        // 读取路径必须能正确处理这种“空 blob”而不是解引用空指针。
        store.write("").unwrap();
        assert_eq!(store.read().unwrap(), None);
        assert!(!store.exists());
    }
}
