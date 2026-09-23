//! 脱敏诊断日志（设计 §6、§10）：JSON Lines，写到 `%LOCALAPPDATA%\CodexHelper\logs\`。
//!
//! - 未调用 [`init`] 前所有写入静默丢弃；写日志永不 panic、永不向调用方返回错误。
//! - 写入前对 detail 做脱敏：键名疑似凭据（authorization / token / key / secret / password 等）
//!   的值、以及形如 `Bearer xxx`、`sk-xxx` 的字符串一律替换为 `***`。
//! - 单文件超过上限时压缩保留尾部（移植自 Codex++ diagnostic_log 的压缩逻辑）。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// 日志文件名，位于 [`init`] 传入的目录下。
const LOG_FILE_NAME: &str = "codex-helper.log";

/// 日志文件上限：超过后触发压缩。
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// 压缩后保留的尾部大小（约 1 MiB）。
const COMPACTED_LOG_BYTES: u64 = 1024 * 1024;

/// 全局日志状态。持有该锁期间完成“读目录 + 压缩 + 追加”，天然串行化并发写入。
struct LogState {
    dir: Option<PathBuf>,
}

fn state() -> &'static Mutex<LogState> {
    static STATE: OnceLock<Mutex<LogState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(LogState { dir: None }))
}

/// 设置日志目录（通常为 `HelperPaths::log_dir()`）。可重复调用，以最后一次为准。
pub fn init(log_dir: impl Into<PathBuf>) {
    if let Ok(mut guard) = state().lock() {
        guard.dir = Some(log_dir.into());
    }
}

/// 当前日志文件路径；未初始化返回 `None`。
pub fn log_file_path() -> Option<PathBuf> {
    let guard = state().lock().ok()?;
    guard.dir.as_ref().map(|dir| dir.join(LOG_FILE_NAME))
}

/// 追加一条事件。`detail` 会先经 [`redact_value`] 脱敏。
///
/// 未初始化时静默丢弃；建目录、压缩、写入过程中的任何错误都被吞掉，绝不 panic、绝不向调用方
/// 返回错误。
pub fn event(name: &str, detail: serde_json::Value) {
    let mut detail = detail;
    redact_value(&mut detail);

    let Ok(guard) = state().lock() else {
        return;
    };
    let Some(dir) = guard.dir.as_ref() else {
        return;
    };
    // 持锁期间完成整次写入，串行化并发调用。
    let _ = append_event(dir, name, &detail, MAX_LOG_BYTES, COMPACTED_LOG_BYTES);
}

/// 实际的建目录 + 压缩 + 追加写入；阈值可注入以便测试。
fn append_event(
    dir: &Path,
    name: &str,
    detail: &Value,
    max_bytes: u64,
    compacted_bytes: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(LOG_FILE_NAME);
    compact_log_if_needed(&path, max_bytes, compacted_bytes)?;

    let record = json!({
        "timestamp_ms": now_ms(),
        "pid": std::process::id(),
        "event": name,
        "detail": detail,
    });
    let line = serde_json::to_string(&record)
        .unwrap_or_else(|error| json!({ "event": "log.serialization_failed", "detail": { "message": error.to_string() } }).to_string());

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // 拼成完整一行后一次性 write_all：避免 writeln! 内部分两次系统调用写入，
    // 在与本进程外的其他写者（如凭据子进程）共享同一文件时被交错。
    let mut buf = line;
    buf.push('\n');
    file.write_all(buf.as_bytes())?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 文件超过 `max_bytes` 时，保留尾部约 `compacted_bytes`，并丢弃不完整的首行。
fn compact_log_if_needed(path: &Path, max_bytes: u64, compacted_bytes: u64) -> std::io::Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if len <= max_bytes {
        return Ok(());
    }

    let keep = compacted_bytes.min(len);
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(len - keep))?;
    let mut tail = Vec::with_capacity(keep as usize);
    file.read_to_end(&mut tail)?;
    drop(file);
    if len > keep
        && let Some(pos) = tail.iter().position(|byte| *byte == b'\n')
    {
        tail.drain(..=pos);
    }

    crate::fsutil::atomic_write(path, &tail)
}

/// 键名（不区分大小写，`-` 与 `_` 等价）命中即判定为敏感字段，值一律替换为 `"***"`。
const SENSITIVE_KEYS: &[&str] = &[
    "authorization",
    "proxy_authorization",
    "token",
    "access_token",
    "refresh_token",
    "api_key",
    "apikey",
    "key",
    "secret",
    "password",
    "credential",
    "bearer",
];

/// 加固：常见 HTTP 头习惯用连字符（如 `x-api-key`），先把 `-` 规范化成 `_`
/// 再与 [`SENSITIVE_KEYS`] 及后缀规则比对，这样 `X-Api-Key`、`api-key` 等写法
/// 也能命中（连字符版本不在任务列出的必须集合中，此项为可选加固）。
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase().replace('-', "_");
    SENSITIVE_KEYS.contains(&lower.as_str())
        || lower.ends_with("_token")
        || lower.ends_with("_key")
        || lower.ends_with("_secret")
        || lower.ends_with("_password")
}

/// 就地脱敏 JSON 值：递归处理对象 / 数组；命中敏感键名的字段整体替换为 `"***"`；
/// 其余字符串值经 [`redact_text`] 处理。
pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *val = Value::String("***".to_string());
                } else {
                    redact_value(val);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item);
            }
        }
        Value::String(text) => {
            *text = redact_text(text);
        }
        _ => {}
    }
}

/// 脱敏任意文本：`Bearer <非空白>` → `Bearer ***`；`sk-` 后接 8 个及以上
/// `[A-Za-z0-9_-]` 的片段 → `sk-***`。不引入 regex，手写扫描，保留其余文本原样。
pub fn redact_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = match_bearer(&chars, i) {
            out.push_str("Bearer ***");
            i = end;
            continue;
        }
        if let Some(end) = match_sk_token(&chars, i) {
            out.push_str("sk-***");
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 在 `start` 处尝试匹配关键字 `Bearer`（ASCII 大小写不敏感），后接至少一个空白与一个
/// 非空白 token。匹配成功返回整个匹配（含 token）结束后的位置。
///
/// 不做词边界检查：任务要求“所有字符串值中的 Bearer <任意非空白>”一律脱敏，即便前面
/// 紧跟字母或中文（如“令牌Bearer abc”“myBearer abc”），多脱敏无害，漏脱敏才是风险。
fn match_bearer(chars: &[char], start: usize) -> Option<usize> {
    const KEYWORD_LEN: usize = 6;
    if start + KEYWORD_LEN > chars.len() {
        return None;
    }
    let candidate = &chars[start..start + KEYWORD_LEN];
    if !candidate
        .iter()
        .zip("bearer".chars())
        .all(|(actual, expected)| actual.to_ascii_lowercase() == expected)
    {
        return None;
    }

    let mut idx = start + KEYWORD_LEN;
    let ws_start = idx;
    while idx < chars.len() && chars[idx].is_whitespace() {
        idx += 1;
    }
    if idx == ws_start {
        // 关键字后没有空白，不是 "Bearer <token>" 形式（如 "Bearerxyz"）。
        return None;
    }

    let token_start = idx;
    while idx < chars.len() && !chars[idx].is_whitespace() {
        idx += 1;
    }
    if idx == token_start {
        // 空白后没有 token。
        return None;
    }
    Some(idx)
}

/// 在 `start` 处尝试匹配 `sk-` 后接 8 个及以上 `[A-Za-z0-9_-]` 的片段。
/// 匹配成功返回整个片段结束后的位置。
///
/// 不做词边界检查：与 [`match_bearer`] 采用同一取舍——任务要求是所有 "sk-" 后接
/// 8 个及以上 token 字符的片段都替换，没有边界例外；常见泄漏路径恰恰紧贴在非独立
/// 边界上，例如 `{:?}` 格式化多行文本时换行转义成字面量 `\n`（紧邻字符是字母
/// `n`）、URL 编码的 `%3D`（以字母 `D` 结尾）、请求头拼接的 `x-sk-...`。因此宁可
/// 多脱敏（"task-"、"risk-" 等普通词恰好以 "sk-" 结尾会被误伤），也不能漏脱敏。
fn match_sk_token(chars: &[char], start: usize) -> Option<usize> {
    const PREFIX: [char; 3] = ['s', 'k', '-'];
    if start + PREFIX.len() > chars.len() {
        return None;
    }
    if chars[start..start + PREFIX.len()] != PREFIX {
        return None;
    }

    let token_start = start + PREFIX.len();
    let mut idx = token_start;
    while idx < chars.len() && is_token_char(chars[idx]) {
        idx += 1;
    }
    if idx - token_start < 8 {
        return None;
    }
    Some(idx)
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// 全局日志状态是进程级单例。测试内用互斥锁串行化并发访问，并在 guard 释放时把
/// `dir` 复位为 `None`，避免污染同一测试二进制内其他模块（如 proxy、manager）
/// 涉及日志全局状态的测试。
#[cfg(test)]
pub(crate) struct TestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = state().lock() {
            guard.dir = None;
        }
    }
}

#[cfg(test)]
pub(crate) fn test_guard() -> TestGuard {
    static LOCK: Mutex<()> = Mutex::new(());
    let lock = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    TestGuard { _lock: lock }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninitialized_log_file_path_is_none_and_event_is_noop() {
        let _guard = test_guard();
        // 重置为未初始化状态。
        state().lock().unwrap().dir = None;
        assert_eq!(log_file_path(), None);
        // 不应 panic。
        event("noop.event", json!({"a": 1}));
    }

    #[test]
    fn init_sets_log_file_path_and_is_idempotent_to_last_call() {
        let _guard = test_guard();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        init(dir_a.path());
        assert_eq!(log_file_path(), Some(dir_a.path().join(LOG_FILE_NAME)));
        init(dir_b.path());
        assert_eq!(log_file_path(), Some(dir_b.path().join(LOG_FILE_NAME)));
    }

    #[test]
    fn event_appends_jsonl_line_with_redacted_detail() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());

        event(
            "proxy.connection",
            json!({
                "host": "example.com",
                "port": 443,
                "decision": "tunnel",
                "ok": true,
                "ms": 12,
                "kind": "connect",
                "token": "should-be-masked",
            }),
        );

        let path = dir.path().join(LOG_FILE_NAME);
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["event"], "proxy.connection");
        assert!(parsed["timestamp_ms"].as_u64().is_some());
        assert!(parsed["pid"].as_u64().is_some());
        assert_eq!(parsed["detail"]["host"], "example.com");
        assert_eq!(parsed["detail"]["port"], 443);
        assert_eq!(parsed["detail"]["decision"], "tunnel");
        assert_eq!(parsed["detail"]["ok"], true);
        assert_eq!(parsed["detail"]["ms"], 12);
        assert_eq!(parsed["detail"]["kind"], "connect");
        assert_eq!(parsed["detail"]["token"], "***");
    }

    #[test]
    fn event_creates_missing_log_directory() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("logs");
        init(&nested);

        event("boot", json!({"message": "启动"}));

        assert!(nested.join(LOG_FILE_NAME).exists());
    }

    #[test]
    fn event_serializes_writes_without_interleaving() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());

        let handles: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    for j in 0..20 {
                        event("burst", json!({"i": i, "j": j}));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let path = dir.path().join(LOG_FILE_NAME);
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 160);
        for line in lines {
            // 每一行必须是独立合法的 JSON，说明写入没有被交错破坏。
            serde_json::from_str::<Value>(line).unwrap();
        }
    }

    #[test]
    fn compact_log_keeps_tail_and_drops_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE_NAME);
        std::fs::write(&path, "line-1\nline-2\nline-3\nline-4\n").unwrap();

        compact_log_if_needed(&path, 12, 16).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "line-3\nline-4\n");
    }

    #[test]
    fn compact_log_noop_when_under_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE_NAME);
        std::fs::write(&path, "short\n").unwrap();

        compact_log_if_needed(&path, 1024, 512).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "short\n");
    }

    #[test]
    fn compact_log_missing_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.log");
        compact_log_if_needed(&path, 10, 5).unwrap();
    }

    #[test]
    fn event_triggers_compaction_via_injected_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE_NAME);
        std::fs::write(&path, "line-1\nline-2\nline-3\nline-4\n").unwrap();

        append_event(dir.path(), "next", &json!({"n": 5}), 12, 16).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("line-1"));
        assert!(content.contains("line-3"));
        assert!(content.contains("\"event\":\"next\""));
    }

    // ── redact_value ──────────────────────────────────────────────

    #[test]
    fn redact_value_masks_sensitive_keys_case_insensitively() {
        let mut value = json!({
            "Authorization": "Bearer abc",
            "PROXY-AUTHORIZATION": "Bearer abc",
            "token": "t",
            "Access_Token": "t",
            "refresh_token": "t",
            "api_key": "k",
            "ApiKey": "k",
            "key": "k",
            "secret": "s",
            "Password": "p",
            "credential": "c",
            "bearer": "b",
            "custom_token": "t",
            "custom_key": "k",
            "custom_secret": "s",
            "custom_password": "p",
        });
        redact_value(&mut value);
        for (_, v) in value.as_object().unwrap() {
            assert_eq!(v, &Value::String("***".to_string()));
        }
    }

    /// 审查发现（minor）：常见连字符头名（如 `x-api-key`）也应被脱敏，
    /// 通过把 '-' 规范化成 '_' 后再比对实现。
    #[test]
    fn redact_value_masks_hyphenated_header_style_keys() {
        let mut value = json!({
            "x-api-key": "leak",
            "X-Api-Key": "leak",
            "api-key": "leak",
            "proxy-authorization": "leak",
        });
        redact_value(&mut value);
        for (_, v) in value.as_object().unwrap() {
            assert_eq!(v, &Value::String("***".to_string()));
        }
    }

    #[test]
    fn redact_value_recurses_into_nested_objects_and_arrays() {
        let mut value = json!({
            "outer": {
                "inner": { "token": "leak" },
                "list": [ { "api_key": "leak" }, { "message": "sk-abcdefgh12345678" } ],
            }
        });
        redact_value(&mut value);
        assert_eq!(value["outer"]["inner"]["token"], "***");
        assert_eq!(value["outer"]["list"][0]["api_key"], "***");
        assert_eq!(value["outer"]["list"][1]["message"], "sk-***");
    }

    #[test]
    fn redact_value_preserves_plain_proxy_connection_fields() {
        let mut value = json!({
            "host": "api.openai.com",
            "port": 443,
            "decision": "tunnel",
            "ok": false,
            "ms": 87,
            "kind": "connect",
            "message": "connection refused",
            "path": "/v1/models",
        });
        let expected = value.clone();
        redact_value(&mut value);
        assert_eq!(value, expected);
    }

    #[test]
    fn redact_value_applies_string_rules_to_non_sensitive_string_fields() {
        let mut value = json!({
            "message": "auth header was Bearer abcdef1234 request failed",
        });
        redact_value(&mut value);
        assert_eq!(
            value["message"],
            "auth header was Bearer *** request failed"
        );
    }

    // ── redact_text ──────────────────────────────────────────────

    #[test]
    fn redact_text_masks_bearer_tokens() {
        assert_eq!(redact_text("Bearer abc123"), "Bearer ***");
        assert_eq!(
            redact_text("Authorization: Bearer abc.def-ghi"),
            "Authorization: Bearer ***"
        );
    }

    /// 审查发现（blocking）：旧实现在 "Bearer" 前有字母数字（含 CJK，因为
    /// `char::is_alphanumeric` 视中文为字母）时不脱敏，导致 "令牌Bearer abc123secret"
    /// 这类中文场景漏脱敏。任务要求是“所有字符串值中的 Bearer <任意非空白>”，不含词
    /// 边界例外，因此改为多脱敏也不留漏洞：紧贴关键字的前缀字符不影响匹配。
    #[test]
    fn redact_text_masks_bearer_even_when_preceded_by_word_or_cjk_chars() {
        assert_eq!(redact_text("unBearer something"), "unBearer ***");
        assert_eq!(redact_text("令牌Bearer abc123secret"), "令牌Bearer ***");
        assert_eq!(redact_text("MyBearer abc123secret"), "MyBearer ***");
    }

    #[test]
    fn redact_text_does_not_mask_bearer_without_trailing_token() {
        // 关键字后没有 "空白 + token" 形式的不算匹配。
        assert_eq!(redact_text("Bearerxyz"), "Bearerxyz");
    }

    /// 审查发现（minor）：HTTP 认证方案名不区分大小写（RFC 7235），"bearer"/"BEARER"
    /// 也应识别，且输出统一规范为 "Bearer ***"。
    #[test]
    fn redact_text_masks_bearer_case_insensitively() {
        assert_eq!(redact_text("bearer abc123secret"), "Bearer ***");
        assert_eq!(redact_text("BEARER abc123secret"), "Bearer ***");
        assert_eq!(redact_text("BeArEr abc123secret"), "Bearer ***");
    }

    #[test]
    fn redact_text_masks_sk_tokens_of_eight_or_more_chars() {
        assert_eq!(redact_text("key is sk-abcdefgh"), "key is sk-***");
        assert_eq!(
            redact_text("sk-ABCD1234_xyz-9 more text"),
            "sk-*** more text"
        );
    }

    /// 审查发现（blocking，第 2 轮）：旧实现保留了 "sk-" 前一字符若为字母数字或 '-'
    /// 则不算匹配的例外，这与 Bearer 统一采用的“宁可多脱敏，不可漏脱敏”原则不一致，
    /// 导致常见泄漏路径被漏掉：`{:?}` 格式化多行文本时换行转义成字面量 `\n`（紧邻字符
    /// 变成字母 `n`）、URL 编码的 `%3D`（以字母 `D` 结尾）、请求头拼接的 `x-sk-...`、
    /// 紧邻数字（如 `id:1sk-...`）。现改为与 Bearer 一致，不做前导字符例外，代价是
    /// "task-"、"risk-" 这类普通词恰好以 "sk-" 结尾也会被当作命中（可接受的误伤）。
    #[test]
    fn redact_text_masks_sk_token_without_leading_char_exception() {
        assert_eq!(redact_text("key_sk-abcdefgh12345"), "key_sk-***");

        // Debug 格式化多行文本：换行被转义为字面量 "\n"，紧邻 Key 前一个字符是字母 n。
        let secret = "line1\nsk-abcdefgh12345";
        let debug_text = format!("{secret:?}");
        let redacted = redact_text(&debug_text);
        assert!(!redacted.contains("abcdefgh12345"));
        assert!(redacted.contains("sk-***"));

        // URL 编码等号 "%3D" 以字母 D 结尾。
        assert_eq!(redact_text("api_key%3Dsk-abcdefgh1234"), "api_key%3Dsk-***");
        // 请求头风格拼接：紧邻连字符。
        assert_eq!(redact_text("x-sk-abcdefgh12345"), "x-sk-***");
        // 紧邻数字。
        assert_eq!(redact_text("id:1sk-abcdefgh1234"), "id:1sk-***");

        // 可接受的误伤：普通单词恰好以 "sk-" 结尾。
        assert_eq!(redact_text("task-abcdefgh12345"), "task-***");
        assert_eq!(redact_text("risk-abcdefgh12345"), "risk-***");
        // 不足 8 个 token 字符仍不算匹配，即便前缀是普通单词。
        assert_eq!(redact_text("task-1234"), "task-1234");
    }

    #[test]
    fn redact_text_keeps_short_sk_prefix_intact() {
        // 少于 8 个 token 字符不算匹配。
        assert_eq!(redact_text("sk-abcdef"), "sk-abcdef");
    }

    #[test]
    fn redact_text_keeps_plain_text_untouched() {
        assert_eq!(redact_text("HTTP 401 未授权"), "HTTP 401 未授权");
        assert_eq!(
            redact_text("host=api.openai.com port=443"),
            "host=api.openai.com port=443"
        );
    }

    #[test]
    fn redact_text_handles_multiple_matches_in_one_string() {
        assert_eq!(
            redact_text("Bearer aaaaaaaa then sk-bbbbbbbb and Bearer cccccccc"),
            "Bearer *** then sk-*** and Bearer ***"
        );
    }
}
