//! `~/.codex/.env` 受管块（设计 §4.3）。
//!
//! codex 引擎启动时通过 `arg0::load_dotenv()` 读取 `CODEX_HOME/.env`；受管块让引擎把出站流量交给
//! 本地分流代理。只管理两行标记之间的内容，保留用户其他行；**绝不写入任何凭据**。

use std::path::Path;

use crate::consts;
use crate::fsutil;

/// 受管块起始标记。
pub const BEGIN_MARKER: &str = "# >>> codex-helper managed gateway (自动生成，请勿手改) >>>";
/// 受管块结束标记。
pub const END_MARKER: &str = "# <<< codex-helper managed gateway <<<";
/// Codex++ 旧块起始标记（只检测与按需移除，不自动删除）。
pub const LEGACY_BEGIN_MARKER: &str =
    "# >>> codex-plus-plus managed gateway (自动生成，请勿手改) >>>";
/// Codex++ 旧块结束标记。
pub const LEGACY_END_MARKER: &str = "# <<< codex-plus-plus managed gateway <<<";

/// 受管块内允许出现的键（顺序固定，绝不包含凭据）。
const MANAGED_KEYS: [&str; 3] = ["HTTP_PROXY=", "HTTPS_PROXY=", "NO_PROXY="];

/// `.env` 文件的只读检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvInspection {
    pub exists: bool,
    /// 存在本工具的受管块
    pub has_block: bool,
    /// 受管块存在、内容与当前端口渲染结果一致、且位于文件末尾
    pub block_up_to_date: bool,
    /// 存在 Codex++ 旧块
    pub has_legacy_block: bool,
}

/// 剥离文本开头的 UTF-8 BOM，返回 `(是否存在 BOM, 剩余文本)`。
fn strip_bom(text: &str) -> (bool, &str) {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest),
        None => (false, text),
    }
}

/// 探测文本的主导换行风格：CRLF 严格多数则用 CRLF，否则（含无换行的新文件）用 LF。
fn detect_eol(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lone_lf = text.matches('\n').count() - crlf;
    if crlf > lone_lf { "\r\n" } else { "\n" }
}

/// 按目标换行风格拼接行；非空结果以单个换行结尾，空结果原样返回 `""`。
fn join_lines(lines: &[String], eol: &str) -> String {
    let joined = lines.join(eol);
    if joined.is_empty() {
        joined
    } else {
        joined + eol
    }
}

/// 判断一行是否是受管键行（`HTTP_PROXY=` / `HTTPS_PROXY=` / `NO_PROXY=`）。
fn is_managed_key_line(line: &str) -> bool {
    MANAGED_KEYS.iter().any(|key| line.starts_with(key))
}

/// 受管块的 5 行内容（不含行终止符）：起始标记 + 3 个键 + 结束标记。
fn block_lines(proxy_port: u16) -> [String; 5] {
    let proxy = format!("http://127.0.0.1:{proxy_port}");
    [
        BEGIN_MARKER.to_string(),
        format!("HTTP_PROXY={proxy}"),
        format!("HTTPS_PROXY={proxy}"),
        format!("NO_PROXY={},127.0.0.1,localhost", consts::GATEWAY_HOST),
        END_MARKER.to_string(),
    ]
}

/// 在 `text` 中查找并剥离 `begin`/`end` 标记之间的块，其余标记（如另一套标记）与用户内容原样保留。
/// 返回保留下来的行（已去掉结尾空行）。
///
/// 对不完整块的保守处理：
/// - 只有起始标记、直到文件末尾或下一个起始标记都没有匹配的结束标记：只删起始标记行，以及紧随其后
///   连续的受管键行（`HTTP_PROXY=` / `HTTPS_PROXY=` / `NO_PROXY=`），其后的用户内容原样保留。
/// - 只有结束标记、没有配对的起始标记：只删该行。
fn scan_and_strip(text: &str, begin: &str, end: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == begin {
            // 在遇到下一个起始标记之前寻找配对的结束标记。
            let mut matched_end = None;
            let mut j = i + 1;
            while j < lines.len() {
                let t = lines[j].trim();
                if t == end {
                    matched_end = Some(j);
                    break;
                }
                if t == begin {
                    break; // 前一个起始标记未闭合，视为不完整块
                }
                j += 1;
            }
            match matched_end {
                Some(end_idx) => {
                    i = end_idx + 1;
                }
                None => {
                    // 不完整块：只吞掉起始标记行及紧随其后的受管键行。
                    let mut k = i + 1;
                    while k < lines.len() && is_managed_key_line(lines[k]) {
                        k += 1;
                    }
                    i = k;
                }
            }
        } else if trimmed == end {
            // 孤立的结束标记：只删该行。
            i += 1;
        } else {
            kept.push(lines[i].to_string());
            i += 1;
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    kept
}

/// 渲染受管块文本（含结尾换行）。
pub fn render_block(proxy_port: u16) -> String {
    block_lines(proxy_port).join("\n") + "\n"
}

/// 写入或更新受管块（幂等），块固定在文件末尾。
///
/// 先剥离已存在的本工具受管块（含重复出现的多个块、不完整块），再把全新渲染的块追加到末尾；
/// 因此端口变化、重复块合并、覆盖用户自定义的同名变量都是这一步的自然结果。
pub fn upsert_block(existing: &str, proxy_port: u16) -> String {
    let (has_bom, body) = strip_bom(existing);
    let eol = detect_eol(body);
    let mut kept = scan_and_strip(body, BEGIN_MARKER, END_MARKER);
    if !kept.is_empty() {
        kept.push(String::new()); // 用户内容与受管块之间留一空行
    }
    kept.extend(block_lines(proxy_port));
    let mut out = join_lines(&kept, eol);
    if has_bom && !out.is_empty() {
        out.insert(0, '\u{feff}');
    }
    out
}

/// 移除受管块，保留用户其他行；只剩空白时返回空串。
pub fn remove_block(existing: &str) -> String {
    strip_and_render(existing, BEGIN_MARKER, END_MARKER)
}

/// 移除 Codex++ 旧块，保留其他内容（包括本工具的受管块）。
pub fn remove_legacy_block(existing: &str) -> String {
    strip_and_render(existing, LEGACY_BEGIN_MARKER, LEGACY_END_MARKER)
}

/// `remove_block` / `remove_legacy_block` 的共同实现：剥离指定标记块，保留 BOM 与换行风格。
fn strip_and_render(existing: &str, begin: &str, end: &str) -> String {
    let (has_bom, body) = strip_bom(existing);
    let eol = detect_eol(body);
    let kept = scan_and_strip(body, begin, end);
    let mut out = join_lines(&kept, eol);
    if has_bom && !out.is_empty() {
        out.insert(0, '\u{feff}');
    }
    out
}

/// 检查文本内容。
pub fn inspect_text(existing: &str, proxy_port: u16) -> EnvInspection {
    let (_, body) = strip_bom(existing);
    // 不完整块（只有起始或结束标记）也算残留：停用状态下的残留检测与旧块警告都不能漏掉它。
    let has_block = has_marker(body, BEGIN_MARKER, END_MARKER);
    let has_legacy_block = has_marker(body, LEGACY_BEGIN_MARKER, LEGACY_END_MARKER);
    // “块内容等于 render_block(port) 且位于末尾”等价于 upsert 结果与原文一致（幂等）。
    let block_up_to_date = has_block && upsert_block(existing, proxy_port) == existing;
    EnvInspection {
        exists: true,
        has_block,
        block_up_to_date,
        has_legacy_block,
    }
}

/// 检查文件；不存在返回 `exists = false` 的默认值。
pub fn inspect_file(env_path: &Path, proxy_port: u16) -> std::io::Result<EnvInspection> {
    match fsutil::read_optional(env_path)? {
        Some(text) => Ok(inspect_text(&text, proxy_port)),
        None => Ok(EnvInspection::default()),
    }
}

/// 原子写入受管块。返回是否实际写入（内容无变化时不写）。
pub fn write_block_to_file(env_path: &Path, proxy_port: u16) -> std::io::Result<bool> {
    let existing = fsutil::read_optional(env_path)?.unwrap_or_default();
    let updated = upsert_block(&existing, proxy_port);
    if updated == existing {
        return Ok(false);
    }
    fsutil::atomic_write(env_path, updated.as_bytes())?;
    Ok(true)
}

/// 从文件移除受管块；移除后只剩空白则删除文件。文件或块不存在视为成功。返回是否有改动。
pub fn remove_block_from_file(env_path: &Path) -> std::io::Result<bool> {
    remove_via(env_path, BEGIN_MARKER, END_MARKER)
}

/// 从文件移除 Codex++ 旧块；移除后只剩空白则删除文件。文件或块不存在视为成功。返回是否有改动。
pub fn remove_legacy_block_from_file(env_path: &Path) -> std::io::Result<bool> {
    remove_via(env_path, LEGACY_BEGIN_MARKER, LEGACY_END_MARKER)
}

/// 文本中是否出现指定标记（完整块或不完整块都算）。
fn has_marker(body: &str, begin: &str, end: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == begin || trimmed == end
    })
}

/// `remove_block_from_file` / `remove_legacy_block_from_file` 的共同实现。
///
/// 文件中没有对应标记时不做任何改写（不规范化换行、不删除只含空白的文件），返回 `Ok(false)`，
/// 保证“是否有改动”如实反映受管块是否被移除。
fn remove_via(env_path: &Path, begin: &str, end: &str) -> std::io::Result<bool> {
    let Some(existing) = fsutil::read_optional(env_path)? else {
        return Ok(false);
    };
    if !has_marker(strip_bom(&existing).1, begin, end) {
        return Ok(false);
    }
    let updated = strip_and_render(&existing, begin, end);
    if updated == existing {
        return Ok(false);
    }
    if updated.trim().is_empty() {
        fsutil::remove_file_if_exists(env_path)?;
    } else {
        fsutil::atomic_write(env_path, updated.as_bytes())?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- render_block ----

    #[test]
    fn renders_block_with_proxy_and_no_proxy() {
        let block = render_block(17891);
        assert!(block.contains("HTTP_PROXY=http://127.0.0.1:17891"));
        assert!(block.contains("HTTPS_PROXY=http://127.0.0.1:17891"));
        assert!(block.contains("NO_PROXY=10.20.30.61,127.0.0.1,localhost"));
        assert!(block.starts_with(BEGIN_MARKER));
        assert!(block.trim_end().ends_with(END_MARKER));
    }

    #[test]
    fn renders_block_matches_design_doc_literally() {
        // 设计 §4.3 的示例文本，逐字符核对。
        let expected = "# >>> codex-helper managed gateway (自动生成，请勿手改) >>>\n\
             HTTP_PROXY=http://127.0.0.1:17891\n\
             HTTPS_PROXY=http://127.0.0.1:17891\n\
             NO_PROXY=10.20.30.61,127.0.0.1,localhost\n\
             # <<< codex-helper managed gateway <<<\n";
        assert_eq!(render_block(17891), expected);
    }

    #[test]
    fn block_contains_only_three_managed_keys_and_no_credentials() {
        let block = render_block(17891);
        let inner: Vec<&str> = block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter(|line| *line != BEGIN_MARKER && *line != END_MARKER)
            .collect();
        assert_eq!(inner.len(), 3);
        for line in &inner {
            assert!(is_managed_key_line(line));
        }
        let lowered = block.to_ascii_lowercase();
        assert!(!lowered.contains("key"));
        assert!(!lowered.contains("token"));
        assert!(!lowered.contains("bearer"));
        assert!(!lowered.contains("secret"));
    }

    // ---- upsert_block（移植自 managed_env.rs） ----

    #[test]
    fn writes_block_into_empty_file() {
        let out = upsert_block("", 17891);
        assert_eq!(out, render_block(17891));
    }

    #[test]
    fn keeps_user_lines_and_appends_block_at_end() {
        let existing = "UNRELATED=value\nAWS_ACCESS_KEY_ID=keep-me\n";
        let out = upsert_block(existing, 17891);
        assert!(out.starts_with("UNRELATED=value\nAWS_ACCESS_KEY_ID=keep-me\n"));
        // 块必须在末尾，dotenv 后定义胜出
        let block_start = out.find(BEGIN_MARKER).expect("block present");
        let user_end = out.find("AWS_ACCESS_KEY_ID").expect("user line present");
        assert!(block_start > user_end);
    }

    #[test]
    fn upsert_is_idempotent() {
        let once = upsert_block("KEEP=1\n", 17891);
        let twice = upsert_block(&once, 17891);
        assert_eq!(once, twice);
        assert_eq!(twice.matches(BEGIN_MARKER).count(), 1);
    }

    #[test]
    fn upsert_rewrites_changed_port() {
        let old = upsert_block("KEEP=1\n", 17891);
        let new = upsert_block(&old, 18000);
        assert!(new.contains("HTTPS_PROXY=http://127.0.0.1:18000"));
        assert!(!new.contains("17891"));
        assert_eq!(new.matches(BEGIN_MARKER).count(), 1);
    }

    #[test]
    fn upsert_merges_duplicate_blocks_into_single_trailing_block() {
        // 人为构造出两个完整的受管块（例如上次崩溃留下的残留）。
        let existing = format!(
            "KEEP=1\n{a}\nKEEP=2\n{b}\n",
            a = render_block(1111).trim_end(),
            b = render_block(2222).trim_end()
        );
        let out = upsert_block(&existing, 17891);
        assert_eq!(out.matches(BEGIN_MARKER).count(), 1);
        assert!(out.contains("KEEP=1"));
        assert!(out.contains("KEEP=2"));
        assert!(!out.contains("1111"));
        assert!(!out.contains("2222"));
        assert!(out.contains("HTTPS_PROXY=http://127.0.0.1:17891"));
        assert!(out.trim_end().ends_with(END_MARKER));
    }

    // ---- remove_block（移植自 managed_env.rs） ----

    #[test]
    fn remove_restores_user_content_and_is_idempotent() {
        let existing = "KEEP=1\n";
        let with_block = upsert_block(existing, 17891);
        let removed = remove_block(&with_block);
        assert_eq!(removed, "KEEP=1\n");
        assert_eq!(remove_block(&removed), removed);
    }

    #[test]
    fn remove_yields_empty_when_only_block_present() {
        let with_block = upsert_block("", 17891);
        assert_eq!(remove_block(&with_block), "");
    }

    #[test]
    fn remove_handles_crlf_markers() {
        let existing = format!(
            "KEEP=1\r\n{BEGIN_MARKER}\r\nHTTPS_PROXY=http://127.0.0.1:1\r\n{END_MARKER}\r\n"
        );
        let removed = remove_block(&existing);
        assert!(!removed.contains("HTTPS_PROXY"));
        assert!(removed.contains("KEEP=1"));
    }

    #[test]
    fn managed_block_wins_over_user_defined_proxy() {
        let existing = "HTTPS_PROXY=http://user-proxy:9999\n";
        let out = upsert_block(existing, 17891);
        let user_pos = out.find("http://user-proxy:9999").expect("user line kept");
        let managed_pos = out
            .find("HTTPS_PROXY=http://127.0.0.1:17891")
            .expect("managed line present");
        // dotenvy 逐行 set_var，后面的赋值生效
        assert!(managed_pos > user_pos);
    }

    // 原 `env_file_path_is_dot_env_under_home` 测试针对 `managed_env_file_path`，
    // 该函数已被 `paths::HelperPaths::env_file` 取代，对应断言见 paths.rs 的
    // `derived_paths_live_under_their_roots` 测试，这里不再重复。

    // ---- 不完整块的保守处理（本任务新增） ----

    #[test]
    fn remove_incomplete_block_missing_end_marker_keeps_trailing_user_content() {
        let existing = format!(
            "USER_LINE=1\n{BEGIN_MARKER}\nHTTP_PROXY=http://127.0.0.1:17891\nSOME_OTHER_USER_LINE=keep\n"
        );
        let out = remove_block(&existing);
        assert_eq!(out, "USER_LINE=1\nSOME_OTHER_USER_LINE=keep\n");
    }

    #[test]
    fn remove_incomplete_block_missing_end_marker_stops_at_first_non_key_line() {
        // 起始标记之后不是紧邻的受管键行时，什么都不额外删除。
        let existing = format!("{BEGIN_MARKER}\nUSER_LINE=1\n");
        let out = remove_block(&existing);
        assert_eq!(out, "USER_LINE=1\n");
    }

    #[test]
    fn remove_incomplete_block_missing_begin_marker_only_removes_end_line() {
        let existing = format!("USER_LINE=1\n{END_MARKER}\nOTHER=2\n");
        let out = remove_block(&existing);
        assert_eq!(out, "USER_LINE=1\nOTHER=2\n");
    }

    // ---- 换行风格 ----

    #[test]
    fn upsert_preserves_crlf_line_ending_when_existing_is_mostly_crlf() {
        let existing = "KEEP=1\r\nKEEP=2\r\n";
        let out = upsert_block(existing, 17891);
        assert!(out.contains("KEEP=1\r\n"));
        assert!(out.contains(&format!("{BEGIN_MARKER}\r\n")));
        assert!(!out.contains("\n\n")); // 不应混入裸 LF 造成的双空行
        assert_eq!(out.matches("\r\n").count(), out.matches('\n').count());
    }

    #[test]
    fn upsert_uses_lf_for_new_or_lf_file() {
        let out = upsert_block("", 17891);
        assert!(!out.contains('\r'));
        let out2 = upsert_block("KEEP=1\n", 17891);
        assert!(!out2.contains('\r'));
    }

    #[test]
    fn remove_preserves_crlf_style_of_original_file() {
        let existing = format!("KEEP=1\r\n{BEGIN_MARKER}\r\nHTTP_PROXY=x\r\n{END_MARKER}\r\n");
        let out = remove_block(&existing);
        assert_eq!(out, "KEEP=1\r\n");
    }

    // ---- UTF-8 BOM ----

    #[test]
    fn upsert_and_remove_preserve_leading_bom() {
        let existing = "\u{feff}KEEP=1\n";
        let with_block = upsert_block(existing, 17891);
        assert!(with_block.starts_with('\u{feff}'));
        assert!(with_block.contains(BEGIN_MARKER));

        let removed = remove_block(&with_block);
        assert_eq!(removed, "\u{feff}KEEP=1\n");
    }

    #[test]
    fn bom_does_not_interfere_with_marker_at_start_of_file() {
        // BOM 之后紧跟起始标记本身（无其他用户行）。
        let existing = format!("\u{feff}{}", upsert_block("", 17891));
        let inspection = inspect_text(&existing, 17891);
        assert!(inspection.has_block);
        assert!(inspection.block_up_to_date);
    }

    #[test]
    fn remove_of_bom_only_block_returns_empty_without_bom() {
        let existing = format!("\u{feff}{}", upsert_block("", 17891));
        assert_eq!(remove_block(&existing), "");
    }

    // ---- Codex++ 旧块 ----

    #[test]
    fn inspect_detects_legacy_block_without_treating_it_as_own_block() {
        let legacy = format!(
            "{LEGACY_BEGIN_MARKER}\nHTTPS_PROXY=http://127.0.0.1:9999\n{LEGACY_END_MARKER}\n"
        );
        let inspection = inspect_text(&legacy, 17891);
        assert!(inspection.has_legacy_block);
        assert!(!inspection.has_block);
        assert!(!inspection.block_up_to_date);
    }

    #[test]
    fn remove_block_never_touches_legacy_block() {
        let legacy = format!(
            "{LEGACY_BEGIN_MARKER}\nHTTPS_PROXY=http://127.0.0.1:9999\n{LEGACY_END_MARKER}\n"
        );
        let with_own = upsert_block(&legacy, 17891);
        let out = remove_block(&with_own);
        // 本工具的受管块被移除，旧块原样保留。
        assert!(!out.contains(BEGIN_MARKER));
        assert!(out.contains(LEGACY_BEGIN_MARKER));
        assert!(out.contains("HTTPS_PROXY=http://127.0.0.1:9999"));
    }

    #[test]
    fn remove_legacy_block_removes_only_legacy_keeps_own_block_and_user_content() {
        let legacy = format!(
            "USER=1\n{LEGACY_BEGIN_MARKER}\nHTTPS_PROXY=http://127.0.0.1:9999\n{LEGACY_END_MARKER}\n"
        );
        let with_own = upsert_block(&legacy, 17891);
        let out = remove_legacy_block(&with_own);
        assert!(!out.contains(LEGACY_BEGIN_MARKER));
        assert!(out.contains("USER=1"));
        assert!(out.contains(BEGIN_MARKER));
        assert!(out.contains("HTTPS_PROXY=http://127.0.0.1:17891"));
    }

    // ---- inspect_text ----

    #[test]
    fn inspect_text_reports_up_to_date_only_when_block_matches_and_trailing() {
        let up_to_date = upsert_block("KEEP=1\n", 17891);
        let report = inspect_text(&up_to_date, 17891);
        assert!(report.exists);
        assert!(report.has_block);
        assert!(report.block_up_to_date);
        assert!(!report.has_legacy_block);

        // 端口不一致：块存在，但内容与当前端口渲染结果不符。
        let stale = inspect_text(&up_to_date, 18000);
        assert!(stale.has_block);
        assert!(!stale.block_up_to_date);
    }

    #[test]
    fn inspect_text_no_block_reports_false() {
        let report = inspect_text("KEEP=1\n", 17891);
        assert!(!report.has_block);
        assert!(!report.block_up_to_date);
        assert!(!report.has_legacy_block);
    }

    // ---- 文件级函数（tempfile，不碰真实用户目录） ----

    #[test]
    fn inspect_file_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let report = inspect_file(&path, 17891).unwrap();
        assert_eq!(report, EnvInspection::default());
    }

    #[test]
    fn write_block_to_file_creates_parent_dir_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".env");
        assert!(write_block_to_file(&path, 17891).unwrap());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, render_block(17891));

        // 内容无变化时不写（第二次返回 false）。
        assert!(!write_block_to_file(&path, 17891).unwrap());

        // 端口变化时才真正重写。
        assert!(write_block_to_file(&path, 18000).unwrap());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("18000"));
    }

    #[test]
    fn write_block_to_file_keeps_existing_user_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "KEEP=1\n").unwrap();
        assert!(write_block_to_file(&path, 17891).unwrap());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("KEEP=1\n"));
        assert!(content.contains(BEGIN_MARKER));
    }

    #[test]
    fn remove_block_from_file_deletes_file_when_only_whitespace_remains() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        write_block_to_file(&path, 17891).unwrap();
        assert!(path.exists());

        assert!(remove_block_from_file(&path).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn remove_block_from_file_keeps_file_with_remaining_user_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "KEEP=1\n").unwrap();
        write_block_to_file(&path, 17891).unwrap();

        assert!(remove_block_from_file(&path).unwrap());
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "KEEP=1\n");
    }

    #[test]
    fn remove_block_from_file_missing_file_returns_ok_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        assert!(!remove_block_from_file(&path).unwrap());
    }

    #[test]
    fn remove_legacy_block_from_file_removes_only_legacy_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let legacy = format!(
            "USER=1\n{LEGACY_BEGIN_MARKER}\nHTTPS_PROXY=http://127.0.0.1:9999\n{LEGACY_END_MARKER}\n"
        );
        std::fs::write(&path, legacy).unwrap();
        write_block_to_file(&path, 17891).unwrap();

        assert!(remove_legacy_block_from_file(&path).unwrap());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains(LEGACY_BEGIN_MARKER));
        assert!(content.contains("USER=1"));
        assert!(content.contains(BEGIN_MARKER));
    }

    #[test]
    fn remove_legacy_block_from_file_missing_file_returns_ok_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        assert!(!remove_legacy_block_from_file(&path).unwrap());
    }

    // ---- 阶段 1 合并后的补充 ----

    #[test]
    fn remove_from_file_without_markers_leaves_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        for original in ["  \n\n", "KEEP=1\r\nOTHER=2\n\n\n"] {
            std::fs::write(&path, original).unwrap();
            assert!(!remove_block_from_file(&path).unwrap());
            assert!(!remove_legacy_block_from_file(&path).unwrap());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn inspect_detects_incomplete_blocks_as_residue() {
        let current = format!("KEEP=1\n{BEGIN_MARKER}\nHTTPS_PROXY=http://127.0.0.1:17891\n");
        let inspection = inspect_text(&current, 17891);
        assert!(inspection.has_block);
        assert!(!inspection.block_up_to_date);

        let legacy = format!("{LEGACY_BEGIN_MARKER}\nHTTP_PROXY=http://127.0.0.1:1\n");
        assert!(inspect_text(&legacy, 17891).has_legacy_block);
        let orphan_end = format!("KEEP=1\n{LEGACY_END_MARKER}\n");
        assert!(inspect_text(&orphan_end, 17891).has_legacy_block);
    }
}
