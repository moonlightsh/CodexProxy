//! macOS 登录钥匙串后端（设计 §3.1）：读、写、删全部经 Apple 签名的 `/usr/bin/security`，
//! 不链接 Security.framework，避免 ad-hoc 签名的本工具成为条目 ACL 的所有者导致 Codex 读取时弹框。
//!
//! 转义、命令行拼装、退出码映射与带超时的子进程执行是纯逻辑，全平台编译与测试；
//! 真正调用 `security` 的读写删只在 macOS 上编译。
//!
//! 错误信息只含操作名与退出码，绝不嵌入 `security` 的 stdout / stderr（stdout 可能就是 Key）。

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::consts;

/// `security` 找不到条目时的退出码（`errSecItemNotFound`）。
const EXIT_NOT_FOUND: i32 = 44;
/// 等待子进程退出时的轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 钥匙串操作，仅用于拼装错误文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeychainOp {
    Read,
    Write,
    Delete,
}

impl KeychainOp {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "读取",
            Self::Write => "写入",
            Self::Delete => "删除",
        }
    }
}

/// 子进程正常结束时的结果：退出码（被信号终止时为 `None`）与完整 stdout。
#[derive(Debug)]
pub(crate) struct RunOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

/// 子进程未能正常结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunError {
    /// 无法启动程序。
    Spawn,
    /// 超时，已结束子进程。
    Timeout,
}

/// 写入前的 Token 校验：非空且只含可打印 ASCII（0x21–0x7E）。这是 `security -i` 逐行
/// 解析命令的注入防线（换行会变成第二条命令），必须在启动子进程前调用。错误信息不含 Token。
pub(crate) fn validate_token(token: &str) -> anyhow::Result<()> {
    if token.is_empty() {
        anyhow::bail!("凭据不能为空");
    }
    if !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        anyhow::bail!("凭据只能包含可打印 ASCII 字符");
    }
    Ok(())
}

/// `security -i` 双引号参数的转义：`\` → `\\`，`"` → `\"`，其余原样。
pub(crate) fn escape_for_security(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '"') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 经 stdin 送给 `security -i` 的覆盖写入命令（一行，以 `\n` 结尾）。Token 不进入 argv。
pub(crate) fn add_command_line(service: &str, account: &str, token: &str) -> String {
    format!(
        "add-generic-password -U -a \"{}\" -s \"{}\" -w \"{}\"\n",
        escape_for_security(account),
        escape_for_security(service),
        escape_for_security(token)
    )
}

fn op_error(op: KeychainOp, code: Option<i32>) -> anyhow::Error {
    match code {
        Some(code) => anyhow::anyhow!("钥匙串{}失败（security 退出码 {code}）", op.name()),
        None => anyhow::anyhow!("钥匙串{}失败（security 被信号终止）", op.name()),
    }
}

/// `find-generic-password -w` 的结果映射：0 → stdout 去掉末尾一个换行（为空视为不存在）；
/// 44 → 不存在；其他 → 错误。
pub(crate) fn map_read_exit(code: Option<i32>, stdout: &[u8]) -> anyhow::Result<Option<String>> {
    match code {
        Some(0) => {
            let value = stdout.strip_suffix(b"\n").unwrap_or(stdout);
            if value.is_empty() {
                return Ok(None);
            }
            match String::from_utf8(value.to_vec()) {
                Ok(token) => Ok(Some(token)),
                Err(_) => anyhow::bail!("钥匙串读取失败（内容不是有效 UTF-8）"),
            }
        }
        Some(EXIT_NOT_FOUND) => Ok(None),
        other => Err(op_error(KeychainOp::Read, other)),
    }
}

/// `security -i add-generic-password` 的结果映射：只有 0 成功。
pub(crate) fn map_write_exit(code: Option<i32>) -> anyhow::Result<()> {
    match code {
        Some(0) => Ok(()),
        other => Err(op_error(KeychainOp::Write, other)),
    }
}

/// `delete-generic-password` 的结果映射：0 或 44（本就不存在）都算成功。
pub(crate) fn map_delete_exit(code: Option<i32>) -> anyhow::Result<()> {
    match code {
        Some(0) | Some(EXIT_NOT_FOUND) => Ok(()),
        other => Err(op_error(KeychainOp::Delete, other)),
    }
}

/// 子进程未能正常结束时的错误文案。
pub(crate) fn run_error(op: KeychainOp, error: RunError) -> anyhow::Error {
    match error {
        RunError::Timeout => anyhow::anyhow!(
            "钥匙串{}超时（{} 秒）",
            op.name(),
            consts::SECURITY_TIMEOUT_SECS
        ),
        RunError::Spawn => anyhow::anyhow!(
            "钥匙串{}失败（无法启动 {}）",
            op.name(),
            consts::SECURITY_TOOL
        ),
    }
}

/// 启动 `program args…`，可选写入 stdin（写完立即关闭），收集 stdout，丢弃 stderr；
/// 超过 `timeout` 仍未退出则结束子进程并返回 [`RunError::Timeout`]。
///
/// stdout 由独立线程读到 EOF，避免子进程因管道写满而阻塞；主线程每 20 ms `try_wait` 一次。
pub(crate) fn run_with_timeout(
    program: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<RunOutput, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RunError::Spawn)?;

    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });

    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // 在独立线程里写，子进程不读 stdin 而挂起时也不会卡住下面的超时判定。
        // 子进程提前退出时写入会失败（EPIPE），结果以退出码为准，这里忽略。
        // 写完 pipe 随线程结束 drop，关闭 stdin，让 `security -i` 读到 EOF 后退出。
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            // 超时，或无法查询状态：结束子进程，按超时处理。读取线程在管道关闭后自行结束，不等待。
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout);
            }
        }
    };
    let stdout = reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok(RunOutput {
        code: status.code(),
        stdout,
    })
}

fn run_security(op: KeychainOp, args: &[&str], stdin: Option<&[u8]>) -> anyhow::Result<RunOutput> {
    run_with_timeout(
        Path::new(consts::SECURITY_TOOL),
        args,
        stdin,
        Duration::from_secs(consts::SECURITY_TIMEOUT_SECS),
    )
    .map_err(|error| run_error(op, error))
}

/// 读取 service 对应条目的密码；不存在返回 `Ok(None)`。
#[cfg(target_os = "macos")]
pub(super) fn read(service: &str) -> anyhow::Result<Option<String>> {
    let output = run_security(
        KeychainOp::Read,
        &[
            "find-generic-password",
            "-s",
            service,
            "-a",
            consts::KEYCHAIN_ACCOUNT,
            "-w",
        ],
        None,
    )?;
    map_read_exit(output.code, &output.stdout)
}

/// 写入（覆盖）service 对应条目。Token 经 stdin 送给 `security -i`，不出现在 argv。
#[cfg(target_os = "macos")]
pub(super) fn write(service: &str, token: &str) -> anyhow::Result<()> {
    validate_token(token)?;
    let line = add_command_line(service, consts::KEYCHAIN_ACCOUNT, token);
    let output = run_security(KeychainOp::Write, &["-i"], Some(line.as_bytes()))?;
    map_write_exit(output.code)
}

/// 删除 service 对应条目；不存在视为成功。
#[cfg(target_os = "macos")]
pub(super) fn delete(service: &str) -> anyhow::Result<()> {
    let output = run_security(
        KeychainOp::Delete,
        &[
            "delete-generic-password",
            "-s",
            service,
            "-a",
            consts::KEYCHAIN_ACCOUNT,
        ],
        None,
    )?;
    map_delete_exit(output.code)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    #[test]
    fn validate_token_accepts_printable_ascii_only() {
        let all: String = (0x21u8..=0x7e).map(char::from).collect();
        assert!(validate_token("hk-ABC_123").is_ok() && validate_token(&all).is_ok());
        assert_eq!(validate_token("").unwrap_err().to_string(), "凭据不能为空");
        for bad in [
            " ",
            "a b",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\0b",
            "a\u{7f}",
            "sk-中文",
            "é",
        ] {
            let err = validate_token(bad).unwrap_err().to_string();
            assert_eq!(err, "凭据只能包含可打印 ASCII 字符", "{bad:?}");
            // 固定文案本身含空格；只对去掉空白后至少 2 字节的样本检查“不含 token”，否则恒为假阳性
            if bad.trim().len() >= 2 {
                assert!(!err.contains(bad), "{err}");
            }
        }
    }

    #[test]
    fn escape_doubles_backslash_and_escapes_quote() {
        assert_eq!(escape_for_security(r#"a\b"c'd$e"#), r#"a\\b\"c'd$e"#);
        assert_eq!(escape_for_security("plain"), "plain");
    }

    #[test]
    fn add_command_line_quotes_and_escapes_every_value() {
        assert_eq!(
            add_command_line("svc/x", "acct", r#"k"\1"#),
            "add-generic-password -U -a \"acct\" -s \"svc/x\" -w \"k\\\"\\\\1\"\n"
        );
    }

    #[test]
    fn read_exit_mapping() {
        assert_eq!(
            map_read_exit(Some(0), b"hk-1\n").unwrap().as_deref(),
            Some("hk-1")
        );
        assert_eq!(
            map_read_exit(Some(0), b"hk-1").unwrap().as_deref(),
            Some("hk-1")
        );
        assert_eq!(map_read_exit(Some(0), b"\n").unwrap(), None);
        assert_eq!(map_read_exit(Some(44), b"").unwrap(), None);
        assert!(map_read_exit(Some(0), &[0xff, b'\n']).is_err());
        assert!(map_read_exit(Some(51), b"").is_err() && map_read_exit(None, b"").is_err());
    }

    #[test]
    fn write_and_delete_exit_mapping() {
        assert!(map_write_exit(Some(0)).is_ok());
        for code in [Some(44), Some(45), Some(2), None] {
            assert!(map_write_exit(code).is_err());
        }
        assert!(map_delete_exit(Some(0)).is_ok() && map_delete_exit(Some(44)).is_ok());
        for code in [Some(45), Some(1), None] {
            assert!(map_delete_exit(code).is_err());
        }
    }

    #[test]
    fn op_errors_name_operation_only() {
        let e = map_read_exit(Some(51), b"hk-secret\n")
            .unwrap_err()
            .to_string();
        assert_eq!(e, "钥匙串读取失败（security 退出码 51）");
        assert_eq!(
            map_write_exit(None).unwrap_err().to_string(),
            "钥匙串写入失败（security 被信号终止）"
        );
        assert_eq!(
            run_error(KeychainOp::Delete, RunError::Timeout).to_string(),
            "钥匙串删除超时（10 秒）"
        );
        assert_eq!(
            run_error(KeychainOp::Read, RunError::Spawn).to_string(),
            "钥匙串读取失败（无法启动 /usr/bin/security）"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_feeds_stdin_and_collects_stdout() {
        let out = run_with_timeout(
            Path::new("/bin/sh"),
            &["-c", "cat; exit 3"],
            Some(b"abc"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!((out.code, out.stdout), (Some(3), b"abc".to_vec()));
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_hanging_child() {
        let start = std::time::Instant::now();
        let r = run_with_timeout(
            Path::new("/bin/sleep"),
            &["30"],
            None,
            Duration::from_millis(300),
        );
        assert!(matches!(r, Err(RunError::Timeout)) && start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn run_with_timeout_reports_spawn_failure() {
        let r = run_with_timeout(
            Path::new("/nonexistent/codex-helper-no-such-tool"),
            &[],
            None,
            Duration::from_secs(1),
        );
        assert!(matches!(r, Err(RunError::Spawn)));
    }
}
