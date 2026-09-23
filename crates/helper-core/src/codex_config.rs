//! `~/.codex/config.toml` 受管 provider 的校正与还原（设计 §4.2、§5、§7）。
//!
//! 只增删改受管键，其余内容（注释、其他 provider、projects、plugins 等）用 `toml_edit` 原样保留。
//! 与原实现不同：配置无法解析、`model_providers` 或受管节不是 table 时一律中止、不写入。

use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{Array, Decor, DocumentMut, Item, RawString, Table, Value};

use crate::{consts, fsutil};

/// 启用前需要记录、停用时需要还原的原值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousConfig {
    /// 启用前根键 `model_provider` 的值；不存在为 `None`
    pub model_provider: Option<String>,
    /// 启用时被移除的根键 `model_catalog_json`；不存在为 `None`
    pub model_catalog_json: Option<String>,
}

/// `config.toml` 的只读检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigInspection {
    /// 文件是否存在
    pub exists: bool,
    /// 根键 `model_provider`（字符串值）
    pub model_provider: Option<String>,
    /// 根键 `model_catalog_json`（字符串值）
    pub model_catalog_json: Option<String>,
    /// 是否存在 `[model_providers.managed_gateway]` 节
    pub has_managed_provider: bool,
    /// 受管内容是否完全正确（model_provider 指向受管 provider，且受管节的全部受管键与期望一致，
    /// 包括给定的 credential command）；用于状态灯与“是否需要重新校正”
    pub fully_managed: bool,
}

impl ConfigInspection {
    /// 是否残留任何受管内容（model_provider 指向受管 provider，或存在受管节）。
    pub fn has_managed_residue(&self) -> bool {
        self.has_managed_provider
            || self.model_provider.as_deref() == Some(crate::consts::PROVIDER_ID)
    }

    /// 由当前配置得出“启用前原值”；若 model_provider 已指向受管 provider（残留），视为 `None`。
    pub fn previous(&self) -> PreviousConfig {
        PreviousConfig {
            model_provider: self
                .model_provider
                .clone()
                .filter(|value| value != crate::consts::PROVIDER_ID),
            model_catalog_json: self.model_catalog_json.clone(),
        }
    }
}

/// 校正 / 还原失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 配置无法解析：中止，不写入
    #[error("config.toml 解析失败：{message}")]
    Parse { message: String },
    /// `model_providers`、受管节或其 `auth` 不是 table：中止，不写入
    #[error("{key} 不是 table")]
    NotATable { key: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 一次校正的结果，供失败时精确回滚。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// 是否实际写入了文件（内容无变化时不写、不备份）
    pub changed: bool,
    /// 写入前的原文；`None` 表示写入前文件不存在
    pub original: Option<String>,
}

/// 只读检查。文件不存在返回 `exists = false` 的默认值。
///
/// 与 [`apply_managed`] 的判定一致：无法解析返回 `ConfigError::Parse`；`model_providers`、受管节或其
/// `auth` 存在但不是标准 table 时返回 `ConfigError::NotATable`（即“这份配置无法被校正”）。
/// `credential_command` 用于判定 `fully_managed`（auth.command 是否指向当前安装目录）。
pub fn inspect(
    config_path: &Path,
    credential_command: &str,
) -> Result<ConfigInspection, ConfigError> {
    let loaded = load(config_path)?;
    if loaded.original.is_none() {
        return Ok(ConfigInspection::default());
    }
    let doc = &loaded.doc;
    // 在副本上跑一遍校正：结构不符即报错，没有任何改动即“完全受管”，判定标准与 apply_managed 天然一致。
    let mut probe = doc.clone();
    let fully_managed = !ensure_managed(&mut probe, credential_command)?;
    Ok(ConfigInspection {
        exists: true,
        model_provider: root_string(doc, MODEL_PROVIDER_KEY),
        model_catalog_json: root_string(doc, MODEL_CATALOG_JSON_KEY),
        has_managed_provider: has_managed_provider(doc),
        fully_managed,
    })
}

/// 只读检查是否残留任何受管内容（同 [`ConfigInspection::has_managed_residue`]）。
///
/// 与 [`remove_residue`] 一样对结构宽松：`model_providers` 等不是标准 table 时不报错，只在文件无法解析
/// 时返回 `ConfigError::Parse`。用于 [`inspect`] 因结构不符报错时仍能判定“是否需要一键清理”。
/// 文件不存在返回 `Ok(false)`。
pub fn has_residue(config_path: &Path) -> Result<bool, ConfigError> {
    let LoadedConfig { original, doc } = load(config_path)?;
    if original.is_none() {
        return Ok(false);
    }
    Ok(has_managed_provider(&doc)
        || doc.get(MODEL_PROVIDER_KEY).and_then(Item::as_str) == Some(consts::PROVIDER_ID))
}

/// 校正受管内容（幂等）：
///
/// - `model_provider = "managed_gateway"`；
/// - `[model_providers.managed_gateway]`：name / base_url / wire_api；移除 env_key、
///   experimental_bearer_token、requires_openai_auth；`[...auth]` command = `credential_command`、
///   args = ["get", "codex-helper/managed-gateway"]；
/// - 移除根键 `model_catalog_json`（调用方已确认并记录原值）。
///
/// 内容有变化且文件已存在时，先把原文件复制到 `backup_path`（固定名，覆盖），再原子写入。
/// 内容无变化时不写入、不备份。
pub fn apply_managed(
    config_path: &Path,
    backup_path: &Path,
    credential_command: &str,
) -> Result<ApplyOutcome, ConfigError> {
    let LoadedConfig { original, mut doc } = load(config_path)?;
    let changed = ensure_managed(&mut doc, credential_command)?;
    if changed {
        if let Some(original) = &original {
            fsutil::atomic_write(backup_path, original.as_bytes())?;
        }
        let rendered = render(&doc, original.as_deref());
        fsutil::atomic_write(config_path, rendered.as_bytes())?;
    }
    Ok(ApplyOutcome { changed, original })
}

/// 撤销一次 [`apply_managed`]：`changed` 时写回原文（原先不存在则删除文件）。
pub fn rollback(config_path: &Path, outcome: &ApplyOutcome) -> Result<(), ConfigError> {
    if !outcome.changed {
        return Ok(());
    }
    match &outcome.original {
        Some(original) => fsutil::atomic_write(config_path, original.as_bytes())?,
        None => {
            fsutil::remove_file_if_exists(config_path)?;
        }
    }
    Ok(())
}

/// 停用还原（设计 §5.2 第 2 步）：定点修改，不以备份整文件覆盖。
///
/// - `model_provider`、`model_catalog_json` 还原为 `previous` 中的值（`None` 则删除该键）；
/// - 删除 `[model_providers.managed_gateway]` 整节（若 `model_providers` 因此为空且原本是隐式表则一并移除）。
///
/// 文件不存在视为成功。返回是否实际写入。
pub fn restore(config_path: &Path, previous: &PreviousConfig) -> Result<bool, ConfigError> {
    edit_existing(config_path, |doc| {
        restore_root_string(doc, MODEL_PROVIDER_KEY, previous.model_provider.as_deref());
        restore_root_string(
            doc,
            MODEL_CATALOG_JSON_KEY,
            previous.model_catalog_json.as_deref(),
        );
        remove_managed_provider(doc);
    })
}

/// 无状态文件时的残留清理（设计 §5.4）：`model_provider == "managed_gateway"` 时删除该键，
/// 并删除受管 provider 节。文件不存在视为成功。返回是否实际写入。
///
/// 等价于 `remove_residue_with(config_path, &PreviousConfig::default())`。
pub fn remove_residue(config_path: &Path) -> Result<bool, ConfigError> {
    remove_residue_with(config_path, &PreviousConfig::default())
}

/// 停用状态下的残留清理，兼顾“启用中断”：
///
/// - `model_provider == "managed_gateway"` 且 `recorded` 非空（启用在记录原值之后、置为已启用之前中断，
///   状态文件仍为停用但配置已被接管）→ 与 [`restore`] 相同，按记录的原值还原两个根键；
/// - `model_provider == "managed_gateway"` 且 `recorded` 为空 → 删除该键；
/// - 其他 `model_provider`（用户自己的）→ 不动，`model_catalog_json` 同样不动；
/// - 一律删除受管 provider 节。
///
/// 与 [`restore`] 一样对结构宽松（只在无法解析时报错）。文件不存在视为成功。返回是否实际写入。
pub fn remove_residue_with(
    config_path: &Path,
    recorded: &PreviousConfig,
) -> Result<bool, ConfigError> {
    let has_record = *recorded != PreviousConfig::default();
    edit_existing(config_path, |doc| {
        if doc.get(MODEL_PROVIDER_KEY).and_then(Item::as_str) == Some(consts::PROVIDER_ID) {
            if has_record {
                restore_root_string(doc, MODEL_PROVIDER_KEY, recorded.model_provider.as_deref());
                restore_root_string(
                    doc,
                    MODEL_CATALOG_JSON_KEY,
                    recorded.model_catalog_json.as_deref(),
                );
            } else {
                remove_root_key(doc, MODEL_PROVIDER_KEY);
            }
        }
        remove_managed_provider(doc);
    })
}

const MODEL_PROVIDER_KEY: &str = "model_provider";
const MODEL_CATALOG_JSON_KEY: &str = "model_catalog_json";
const MODEL_PROVIDERS_KEY: &str = "model_providers";
const AUTH_KEY: &str = "auth";
/// 与命令鉴权互斥、校正时从受管节移除的键。
const EXCLUSIVE_AUTH_KEYS: [&str; 3] = [
    "env_key",
    "experimental_bearer_token",
    "requires_openai_auth",
];
/// 凭据程序的子命令。
const CREDENTIAL_SUBCOMMAND: &str = "get";
const BOM: char = '\u{feff}';

/// 读取并解析后的配置。
struct LoadedConfig {
    /// 文件原文（含 BOM，逐字节）；文件不存在为 `None`
    original: Option<String>,
    doc: DocumentMut,
}

/// 读取并解析配置，同时校验根键 `model_provider` / `model_catalog_json` 的类型。
fn load(config_path: &Path) -> Result<LoadedConfig, ConfigError> {
    let bytes = match std::fs::read(config_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedConfig {
                original: None,
                doc: DocumentMut::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    let original = String::from_utf8(bytes).map_err(|_| ConfigError::Parse {
        message: "文件不是有效的 UTF-8 文本".to_string(),
    })?;
    let body = original.strip_prefix(BOM).unwrap_or(&original);
    let doc = body
        .parse::<DocumentMut>()
        .map_err(|error| ConfigError::Parse {
            message: describe_parse_error(body, &error),
        })?;
    for key in [MODEL_PROVIDER_KEY, MODEL_CATALOG_JSON_KEY] {
        if doc.get(key).is_some_and(|item| item.as_str().is_none()) {
            return Err(ConfigError::Parse {
                message: format!("{key} 不是字符串"),
            });
        }
    }
    Ok(LoadedConfig {
        original: Some(original),
        doc,
    })
}

/// 解析错误只报位置与原因。`TomlError` 的 Display 会引用出错行原文，可能含凭据，不能外传。
fn describe_parse_error(text: &str, error: &toml_edit::TomlError) -> String {
    let reason = error
        .message()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("；");
    let Some(span) = error.span() else {
        return reason;
    };
    let head = &text.as_bytes()[..span.start.min(text.len())];
    let line = head.iter().filter(|byte| **byte == b'\n').count() + 1;
    let line_start = head
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let column = String::from_utf8_lossy(&head[line_start..]).chars().count() + 1;
    format!("第 {line} 行第 {column} 列：{reason}")
}

/// 对已存在的配置做定点修改；文件不存在视为成功。只在内容实际变化时写入。
fn edit_existing(
    config_path: &Path,
    edit: impl FnOnce(&mut DocumentMut),
) -> Result<bool, ConfigError> {
    let LoadedConfig { original, mut doc } = load(config_path)?;
    let Some(original) = original else {
        return Ok(false);
    };
    let before = doc.to_string();
    edit(&mut doc);
    if doc.to_string() == before {
        return Ok(false);
    }
    fsutil::atomic_write(config_path, render(&doc, Some(&original)).as_bytes())?;
    Ok(true)
}

/// 在内存中校正受管内容，返回是否有改动。结构不符（非 table）时返回 `NotATable`。
fn ensure_managed(doc: &mut DocumentMut, credential_command: &str) -> Result<bool, ConfigError> {
    let before = doc.to_string();

    set_string(doc, MODEL_PROVIDER_KEY, consts::PROVIDER_ID);
    remove_root_key(doc, MODEL_CATALOG_JSON_KEY);

    // 新建的 model_providers 设为隐式表，不输出空的 [model_providers] 表头。
    let providers = child_table(doc, MODEL_PROVIDERS_KEY, MODEL_PROVIDERS_KEY, true)?;
    let provider_path = format!("{MODEL_PROVIDERS_KEY}.{}", consts::PROVIDER_ID);
    let provider = child_table(providers, consts::PROVIDER_ID, &provider_path, false)?;
    set_string(provider, "name", consts::PROVIDER_NAME);
    set_string(provider, "base_url", consts::GATEWAY_BASE_URL);
    set_string(provider, "wire_api", consts::PROVIDER_WIRE_API);
    // 命令鉴权与 env_key / experimental_bearer_token / requires_openai_auth 互斥
    for key in EXCLUSIVE_AUTH_KEYS {
        provider.remove(key);
    }
    let auth = child_table(
        provider,
        AUTH_KEY,
        &format!("{provider_path}.{AUTH_KEY}"),
        false,
    )?;
    set_string(auth, "command", credential_command);
    set_credential_args(auth);

    Ok(doc.to_string() != before)
}

/// 取子表；不存在则新建。存在但不是标准 table（inline table、数组、标量、表数组）时报错。
fn child_table<'a>(
    parent: &'a mut Table,
    key: &str,
    path: &str,
    implicit: bool,
) -> Result<&'a mut Table, ConfigError> {
    parent
        .entry(key)
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(implicit);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| ConfigError::NotATable {
            key: path.to_string(),
        })
}

/// 是否存在受管 provider 节（标准 table 或 inline table 中的同名项，与删除时的判定一致）。
fn has_managed_provider(doc: &DocumentMut) -> bool {
    doc.get(MODEL_PROVIDERS_KEY)
        .and_then(Item::as_table_like)
        .is_some_and(|providers| providers.contains_key(consts::PROVIDER_ID))
}

/// 读取根键的字符串值（类型已在 [`load`] 校验）。
fn root_string(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key).and_then(Item::as_str).map(str::to_string)
}

/// 把根键还原为给定值；`None` 删除该键。
fn restore_root_string(doc: &mut DocumentMut, key: &str, value: Option<&str>) {
    match value {
        Some(value) => set_string(doc, key, value),
        None => remove_root_key(doc, key),
    }
}

/// 设置字符串值；已相等则不动（保留原写法）。
fn set_string(table: &mut Table, key: &str, desired: &str) {
    if table.get(key).and_then(Item::as_str) != Some(desired) {
        set_value(table, key, basic_string(desired));
    }
}

/// 设置 `args = ["get", "<target>"]`；已相等则不动。
fn set_credential_args(auth: &mut Table) {
    let expected = [CREDENTIAL_SUBCOMMAND, consts::CREDENTIAL_TARGET];
    let matches = auth
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.len() == expected.len()
                && args
                    .iter()
                    .zip(expected)
                    .all(|(value, expected)| value.as_str() == Some(expected))
        });
    if !matches {
        let mut args = Array::new();
        for value in expected {
            args.push_formatted(basic_string(value));
        }
        set_value(auth, "args", Value::Array(args));
    }
}

/// 写入值。替换已有值时沿用原值的前后装饰（行尾注释、空白），键本身的装饰不受影响。
fn set_value(table: &mut Table, key: &str, mut value: Value) {
    if let Some(current) = table.get_mut(key).and_then(Item::as_value_mut) {
        *value.decor_mut() = current.decor().clone();
        *current = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

/// 生成 TOML 基本字符串（双引号）。toml_edit 默认对含反斜杠的串输出字面量串，
/// 这里固定用基本字符串并转义，Windows 路径写成 `"C:\\Users\\..."`，与设计 §4.2 一致。
fn basic_string(text: &str) -> Value {
    let mut encoded = String::with_capacity(text.len() + 2);
    encoded.push('"');
    for ch in text.chars() {
        match ch {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{8}' => encoded.push_str("\\b"),
            '\t' => encoded.push_str("\\t"),
            '\n' => encoded.push_str("\\n"),
            '\u{c}' => encoded.push_str("\\f"),
            '\r' => encoded.push_str("\\r"),
            ch if ch.is_control() => encoded.push_str(&format!("\\u{:04X}", u32::from(ch))),
            ch => encoded.push(ch),
        }
    }
    encoded.push('"');
    encoded
        .parse::<Value>()
        .unwrap_or_else(|_| Value::from(text))
}

/// 删除根键。键上方若有注释，把注释留在原位（挂到前一个根键的行尾装饰或根装饰上），避免误删用户注释。
fn remove_root_key(doc: &mut DocumentMut, key: &str) {
    let Some(prefix) = doc.key(key).map(|key| raw_prefix(key.leaf_decor())) else {
        return;
    };
    let previous_value = doc
        .iter()
        .take_while(|(name, _)| *name != key)
        .filter(|(_, item)| item.is_value())
        .last()
        .map(|(name, _)| name.to_string());
    doc.remove(key);

    let Some(comment) = comment_lines(&prefix) else {
        return;
    };
    let previous = previous_value.and_then(|name| doc.get_mut(&name)?.as_value_mut());
    match previous {
        Some(value) => {
            // 行尾装饰之后由编码器补换行，所以这里以换行开头、去掉末尾换行
            let decor = value.decor_mut();
            let suffix = decor.suffix().map(raw_str).unwrap_or_default();
            let body = comment.strip_suffix('\n').unwrap_or(comment);
            let body = body.strip_suffix('\r').unwrap_or(body);
            decor.set_suffix(format!("{suffix}\n{body}"));
        }
        None => {
            let decor = doc.decor_mut();
            let existing = decor.prefix().map(raw_str).unwrap_or_default();
            decor.set_prefix(format!("{existing}{comment}"));
        }
    }
}

/// 删除受管 provider 节。
///
/// 节头上方的注释在 toml_edit 中归属该节，实际上多是上一节的尾注释，移到下一个表头之前（或文件尾）保留。
/// `model_providers` 因此变空且为隐式表（本工具创建）时一并移除；用户写的显式空表头保留。
/// 删除是收尾动作，不做结构校验：`model_providers` 为 inline table 时同样删除其中的受管项，
/// 为其他类型时其中不可能有受管节，原样不动。
fn remove_managed_provider(doc: &mut DocumentMut) {
    let removed = match doc.get_mut(MODEL_PROVIDERS_KEY) {
        Some(Item::Table(providers)) => {
            let removed = providers.remove(consts::PROVIDER_ID);
            if (providers.is_implicit() || providers.is_dotted()) && providers.is_empty() {
                doc.remove(MODEL_PROVIDERS_KEY);
            }
            removed
        }
        Some(Item::Value(Value::InlineTable(providers))) => {
            providers.remove(consts::PROVIDER_ID);
            None
        }
        _ => None,
    };
    let Some(Item::Table(removed)) = removed else {
        return;
    };
    let Some((position, header)) = first_header(&removed) else {
        return;
    };
    let prefix = raw_prefix(header.decor());
    let Some(comment) = comment_lines(&prefix) else {
        return;
    };
    let mut next = None;
    next_header_position(doc, position, &mut next);
    let placed = next.is_some_and(|next| prepend_to_header(doc, next, comment));
    if !placed {
        let trailing = raw_str(doc.trailing()).to_string();
        doc.set_trailing(format!("{comment}{trailing}"));
    }
}

/// 表头是否会被输出（与 toml_edit 编码器的判定一致：无键值的隐式表不输出表头）。
fn has_visible_header(table: &Table) -> bool {
    !table.is_dotted() && !(table.is_implicit() && table.get_values().is_empty())
}

/// 子树中按文档位置最靠前的可见表头。
fn first_header(table: &Table) -> Option<(usize, &Table)> {
    let own = table
        .position()
        .filter(|_| has_visible_header(table))
        .map(|position| (position, table));
    let children = table.iter().flat_map(|(_, item)| child_tables(item));
    children
        .filter_map(first_header)
        .chain(own)
        .min_by_key(|(position, _)| *position)
}

/// 位置在 `after` 之后、最靠前的可见表头位置。
fn next_header_position(table: &Table, after: usize, best: &mut Option<usize>) {
    for (_, item) in table.iter() {
        for child in child_tables(item) {
            if let Some(position) = child.position().filter(|_| has_visible_header(child))
                && position > after
                && best.is_none_or(|best| position < best)
            {
                *best = Some(position);
            }
            next_header_position(child, after, best);
        }
    }
}

/// 把文本加到指定位置表头的前置装饰之前，返回是否找到该表头。
fn prepend_to_header(table: &mut Table, position: usize, text: &str) -> bool {
    for (_, item) in table.iter_mut() {
        let children: Vec<&mut Table> = match item {
            Item::Table(child) => vec![child],
            Item::ArrayOfTables(array) => array.iter_mut().collect(),
            _ => Vec::new(),
        };
        for child in children {
            if child.position() == Some(position) && has_visible_header(child) {
                let decor = child.decor_mut();
                let existing = decor.prefix().map(raw_str).unwrap_or("\n").to_string();
                decor.set_prefix(format!("{text}{existing}"));
                return true;
            }
            if prepend_to_header(child, position, text) {
                return true;
            }
        }
    }
    false
}

/// 标准表或表数组中的各个子表。
fn child_tables(item: &Item) -> Vec<&Table> {
    match item {
        Item::Table(table) => vec![table],
        Item::ArrayOfTables(array) => array.iter().collect(),
        _ => Vec::new(),
    }
}

fn raw_str(raw: &RawString) -> &str {
    raw.as_str().unwrap_or_default()
}

fn raw_prefix(decor: &Decor) -> String {
    decor.prefix().map(raw_str).unwrap_or_default().to_string()
}

/// 前置装饰中含注释的部分：从开头到最后一行注释（含其换行）。不含注释返回 `None`。
fn comment_lines(prefix: &str) -> Option<&str> {
    let hash = prefix.rfind('#')?;
    let end = prefix[hash..]
        .find('\n')
        .map_or(prefix.len(), |offset| hash + offset + 1);
    Some(&prefix[..end])
}

/// 按原文的格式习惯输出：保留 BOM；原文纯 CRLF 时把新行也写成 CRLF；原文无结尾换行时不补。
///
/// toml_edit 每个键值行后固定输出 `\n`，不保留 CRLF 与“无结尾换行”，这里补齐以实现逐字节还原。
fn render(doc: &DocumentMut, original: Option<&str>) -> String {
    let mut text = doc.to_string();
    let Some(original) = original else {
        return text;
    };
    let (bom, body) = match original.strip_prefix(BOM) {
        Some(body) => (true, body),
        None => (false, original),
    };
    if body.contains("\r\n") && body.matches('\n').count() == body.matches("\r\n").count() {
        text = to_crlf(&text);
    }
    if !body.is_empty()
        && !body.ends_with('\n')
        && let Some(stripped) = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
    {
        text.truncate(stripped.len());
    }
    if bom {
        text.insert(0, BOM);
    }
    text
}

/// 把单独的 `\n` 换成 `\r\n`，已是 `\r\n` 的保持不变。
fn to_crlf(text: &str) -> String {
    let mut output = String::with_capacity(text.len() + text.len() / 16);
    let mut previous = '\0';
    for ch in text.chars() {
        if ch == '\n' && previous != '\r' {
            output.push('\r');
        }
        output.push(ch);
        previous = ch;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: &str = "C:\\x\\codex-helper-credential.exe";

    struct Fixture {
        _dir: tempfile::TempDir,
        config: std::path::PathBuf,
        backup: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config.toml");
            let backup = dir.path().join(consts::CONFIG_BACKUP_FILE_NAME);
            Self {
                _dir: dir,
                config,
                backup,
            }
        }

        fn with(text: &str) -> Self {
            let fixture = Self::new();
            std::fs::write(&fixture.config, text).unwrap();
            fixture
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.config).unwrap()
        }

        fn apply(&self, command: &str) -> Result<ApplyOutcome, ConfigError> {
            apply_managed(&self.config, &self.backup, command)
        }
    }

    // ---- 移植自 Codex++ managed_gateway.rs ----

    #[test]
    fn apply_writes_provider_command_auth_and_preserves_unrelated_fields() {
        let fixture = Fixture::with(
            "model = \"gpt-5.2-codex\"\n# 用户注释\n[model_providers.custom]\nname = \"Custom\"\n",
        );
        fixture
            .apply("C:\\Program Files\\CodexHelper\\codex-helper-credential.exe")
            .unwrap();
        let text = fixture.read();
        assert!(text.contains("model_provider = \"managed_gateway\""));
        assert!(text.contains("[model_providers.managed_gateway]"));
        assert!(text.contains("base_url = \"http://10.20.30.61:8080\""));
        assert!(text.contains("wire_api = \"responses\""));
        assert!(text.contains("args = [\"get\", \"codex-helper/managed-gateway\"]"));
        assert!(text.contains("model = \"gpt-5.2-codex\""));
        assert!(text.contains("# 用户注释"));
        assert!(text.contains("[model_providers.custom]"));
        assert!(!text.contains("env_key"));
        assert!(!text.contains("experimental_bearer_token"));
        assert!(!text.contains("requires_openai_auth = true"));
    }

    #[test]
    fn apply_corrects_drift_on_every_launch() {
        let fixture = Fixture::new();
        fixture.apply(COMMAND).unwrap();
        let text = fixture
            .read()
            .replace("http://10.20.30.61:8080", "http://evil:1");
        std::fs::write(&fixture.config, text).unwrap();
        assert!(fixture.apply(COMMAND).unwrap().changed);
        let text = fixture.read();
        assert!(text.contains("http://10.20.30.61:8080"));
        assert!(text.contains("model_provider = \"managed_gateway\""));
    }

    #[test]
    fn apply_creates_config_when_missing_and_rejects_garbage() {
        let fixture = Fixture::new();
        let outcome = fixture.apply(COMMAND).unwrap();
        assert!(fixture.config.exists());
        assert!(outcome.changed);
        assert_eq!(outcome.original, None);
        // 文件原先不存在：不产生备份
        assert!(!fixture.backup.exists());
        // 损坏的 config.toml：拒绝写入，原文逐字节不变，也不备份（与原实现“重置为受管配置”不同）
        std::fs::write(&fixture.config, "<<<not toml>>>").unwrap();
        let error = fixture.apply(COMMAND).unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
        assert_eq!(std::fs::read(&fixture.config).unwrap(), b"<<<not toml>>>");
        assert!(!fixture.backup.exists());
    }

    #[test]
    fn detects_and_removes_external_catalog_pointer_only() {
        let fixture = Fixture::with(
            "model_catalog_json = \"C:\\\\tmp\\\\external-catalog.json\"\nmodel = \"gpt-5.2\"\n",
        );
        let catalog = fixture.config.with_file_name("external-catalog.json");
        std::fs::write(&catalog, "[]").unwrap();
        let original = fixture.read();
        assert_eq!(
            inspect(&fixture.config, COMMAND)
                .unwrap()
                .model_catalog_json
                .as_deref(),
            Some("C:\\tmp\\external-catalog.json")
        );
        fixture.apply(COMMAND).unwrap();
        assert_eq!(std::fs::read_to_string(&fixture.backup).unwrap(), original);
        let text = fixture.read();
        assert!(!text.contains("model_catalog_json"));
        assert!(text.contains("model = \"gpt-5.2\""));
        assert!(catalog.exists());
    }

    #[test]
    fn no_conflict_returns_none_and_removal_is_noop() {
        let fixture = Fixture::with("model = \"gpt-5.2\"\n");
        assert!(
            inspect(&fixture.config, COMMAND)
                .unwrap()
                .model_catalog_json
                .is_none()
        );
        assert!(!remove_residue(&fixture.config).unwrap());
        assert!(!restore(&fixture.config, &PreviousConfig::default()).unwrap());
        assert_eq!(fixture.read(), "model = \"gpt-5.2\"\n");
    }

    // ---- 拒写 ----

    #[test]
    fn non_table_structures_are_rejected_without_writing() {
        let cases = [
            ("model_providers = \"x\"\n", "model_providers"),
            ("model_providers = [1, 2]\n", "model_providers"),
            (
                "model_providers = { managed_gateway = { name = \"x\" } }\n",
                "model_providers",
            ),
            ("[[model_providers]]\nname = \"x\"\n", "model_providers"),
            (
                "[model_providers]\nmanaged_gateway = \"x\"\n",
                "model_providers.managed_gateway",
            ),
            (
                "[model_providers]\nmanaged_gateway = { name = \"x\" }\n",
                "model_providers.managed_gateway",
            ),
            (
                "[model_providers.managed_gateway]\nauth = \"x\"\n",
                "model_providers.managed_gateway.auth",
            ),
            (
                "[model_providers.managed_gateway]\nauth = { command = \"x\" }\n",
                "model_providers.managed_gateway.auth",
            ),
            (
                "[model_providers.managed_gateway]\nauth = [\"x\"]\n",
                "model_providers.managed_gateway.auth",
            ),
        ];
        for (text, expected_key) in cases {
            let fixture = Fixture::with(text);
            match fixture.apply(COMMAND) {
                Err(ConfigError::NotATable { key }) => assert_eq!(key, expected_key, "{text}"),
                other => panic!("{text} 应返回 NotATable，实际 {other:?}"),
            }
            assert_eq!(fixture.read(), text);
            assert!(!fixture.backup.exists());
            // inspect 与 apply_managed 判定一致：同样报 NotATable（只读，不写入）
            match inspect(&fixture.config, COMMAND) {
                Err(ConfigError::NotATable { key }) => assert_eq!(key, expected_key, "{text}"),
                other => panic!("{text} 的 inspect 应返回 NotATable，实际 {other:?}"),
            }
            assert_eq!(fixture.read(), text);
            assert!(!fixture.backup.exists());
        }
    }

    #[test]
    fn residue_check_and_cleanup_tolerate_non_table_structures() {
        let cases = [
            ("model_providers = \"x\"\n", false),
            (
                "model_provider = \"managed_gateway\"\nmodel_providers = [1]\n",
                true,
            ),
            (
                "[model_providers]\nmanaged_gateway = { name = \"x\" }\n",
                true,
            ),
            ("[model_providers.managed_gateway]\nauth = \"x\"\n", true),
        ];
        for (text, residue) in cases {
            let fixture = Fixture::with(text);
            assert!(inspect(&fixture.config, COMMAND).is_err(), "{text}");
            assert_eq!(has_residue(&fixture.config).unwrap(), residue, "{text}");
            // 清理路径对结构宽松：能删的受管内容照删，不因结构不符失败
            assert_eq!(remove_residue(&fixture.config).unwrap(), residue, "{text}");
            assert!(!has_residue(&fixture.config).unwrap(), "{text}");
        }
    }

    #[test]
    fn non_string_root_keys_are_rejected_by_every_operation() {
        for (text, key) in [
            ("model_provider = 1\n", "model_provider"),
            ("model_provider = { a = 1 }\n", "model_provider"),
            ("model_catalog_json = [\"a\"]\n", "model_catalog_json"),
            ("[model_catalog_json]\npath = \"a\"\n", "model_catalog_json"),
        ] {
            let fixture = Fixture::with(text);
            let errors = [
                inspect(&fixture.config, COMMAND).map(|_| ()).unwrap_err(),
                fixture.apply(COMMAND).map(|_| ()).unwrap_err(),
                restore(&fixture.config, &PreviousConfig::default())
                    .map(|_| ())
                    .unwrap_err(),
                remove_residue(&fixture.config).map(|_| ()).unwrap_err(),
            ];
            for error in errors {
                match error {
                    ConfigError::Parse { message } => {
                        assert!(message.contains(key), "{text}: {message}")
                    }
                    other => panic!("{text} 应返回 Parse，实际 {other:?}"),
                }
            }
            assert_eq!(fixture.read(), text);
            assert!(!fixture.backup.exists());
        }
    }

    #[test]
    fn unparsable_config_is_rejected_by_every_operation() {
        let text = "model = \"o3\"\n[model_providers.managed_gateway\nexperimental_bearer_token = \"sk-secret-123\"\n";
        let fixture = Fixture::with(text);
        let errors = [
            inspect(&fixture.config, COMMAND).map(|_| ()).unwrap_err(),
            fixture.apply(COMMAND).map(|_| ()).unwrap_err(),
            restore(&fixture.config, &PreviousConfig::default())
                .map(|_| ())
                .unwrap_err(),
            remove_residue(&fixture.config).map(|_| ()).unwrap_err(),
        ];
        for error in errors {
            let ConfigError::Parse { message } = &error else {
                panic!("应返回 Parse，实际 {error:?}");
            };
            // 只报位置与原因，不引用原文（原文可能含凭据）
            assert!(message.contains("第 2 行"), "{message}");
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains("sk-secret"));
            assert!(!shown.contains("model_providers.managed_gateway"));
        }
        assert_eq!(fixture.read(), text);
        assert!(!fixture.backup.exists());
    }

    #[test]
    fn invalid_utf8_is_a_parse_error() {
        let fixture = Fixture::new();
        std::fs::write(&fixture.config, [b'a', b'=', 0xff, b'\n']).unwrap();
        assert!(matches!(
            fixture.apply(COMMAND),
            Err(ConfigError::Parse { .. })
        ));
        assert_eq!(
            std::fs::read(&fixture.config).unwrap(),
            [b'a', b'=', 0xff, b'\n']
        );
    }

    // ---- 校正 ----

    #[test]
    fn apply_removes_exclusive_keys_and_keeps_other_user_keys() {
        let fixture = Fixture::with(concat!(
            "model_provider = \"managed_gateway\"\n",
            "\n[model_providers.managed_gateway]\n",
            "name = \"旧名字\"\n",
            "env_key = \"OPENAI_API_KEY\"\n",
            "experimental_bearer_token = \"sk-should-go\"\n",
            "requires_openai_auth = true\n",
            "stream_max_retries = 3\n",
            "\n[model_providers.managed_gateway.auth]\n",
            "command = \"old.exe\"\n",
            "args = [\"get\", \"other-target\"]\n",
            "timeout_ms = 5000\n",
        ));
        assert!(fixture.apply(COMMAND).unwrap().changed);
        let doc = fixture.read().parse::<DocumentMut>().unwrap();
        let provider = doc["model_providers"]["managed_gateway"]
            .as_table()
            .unwrap();
        assert_eq!(provider["name"].as_str(), Some(consts::PROVIDER_NAME));
        assert_eq!(
            provider["base_url"].as_str(),
            Some(consts::GATEWAY_BASE_URL)
        );
        assert_eq!(
            provider["wire_api"].as_str(),
            Some(consts::PROVIDER_WIRE_API)
        );
        for key in EXCLUSIVE_AUTH_KEYS {
            assert!(!provider.contains_key(key), "{key}");
        }
        assert_eq!(provider["stream_max_retries"].as_integer(), Some(3));
        let auth = provider["auth"].as_table().unwrap();
        assert_eq!(auth["command"].as_str(), Some(COMMAND));
        let args: Vec<_> = auth["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(args, ["get", consts::CREDENTIAL_TARGET]);
        assert_eq!(auth["timeout_ms"].as_integer(), Some(5000));
    }

    #[test]
    fn apply_is_idempotent_and_skips_backup_when_unchanged() {
        let fixture = Fixture::with("model = \"o3\"\n");
        let first = fixture.apply(COMMAND).unwrap();
        assert!(first.changed);
        assert_eq!(first.original.as_deref(), Some("model = \"o3\"\n"));
        assert_eq!(
            std::fs::read_to_string(&fixture.backup).unwrap(),
            "model = \"o3\"\n"
        );
        std::fs::remove_file(&fixture.backup).unwrap();
        let written = fixture.read();

        let second = fixture.apply(COMMAND).unwrap();
        assert!(!second.changed);
        assert_eq!(second.original.as_deref(), Some(written.as_str()));
        assert_eq!(fixture.read(), written);
        assert!(!fixture.backup.exists());
    }

    #[test]
    fn apply_overwrites_stale_backup_with_current_original() {
        let fixture = Fixture::with("model = \"o3\"\n");
        std::fs::write(&fixture.backup, "旧备份").unwrap();
        fixture.apply(COMMAND).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fixture.backup).unwrap(),
            "model = \"o3\"\n"
        );
    }

    #[test]
    fn apply_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("nested").join(".codex").join("config.toml");
        let backup = config.with_file_name(consts::CONFIG_BACKUP_FILE_NAME);
        let outcome = apply_managed(&config, &backup, COMMAND).unwrap();
        assert!(outcome.changed);
        assert!(config.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn windows_command_path_is_escaped_as_basic_string() {
        let command =
            "C:\\Users\\张三\\AppData\\Local\\Programs\\CodexHelper\\codex-helper-credential.exe";
        let fixture = Fixture::new();
        fixture.apply(command).unwrap();
        let text = fixture.read();
        assert!(text.contains(
            "command = \"C:\\\\Users\\\\张三\\\\AppData\\\\Local\\\\Programs\\\\CodexHelper\\\\codex-helper-credential.exe\""
        ));
        let doc = text.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model_providers"]["managed_gateway"]["auth"]["command"].as_str(),
            Some(command)
        );
    }

    #[test]
    fn basic_string_escapes_quotes_and_control_characters() {
        for text in [
            "a\"b",
            "tab\there",
            "line\nbreak",
            "cr\rlf",
            "\u{1}\u{7f}",
            "'单引号'",
        ] {
            let value = basic_string(text);
            let rendered = value.to_string();
            assert!(rendered.trim().starts_with('"'), "{rendered}");
            let parsed = rendered.parse::<Value>().unwrap();
            assert_eq!(parsed.as_str(), Some(text));
        }
    }

    #[test]
    fn replacing_values_keeps_trailing_comments() {
        let fixture = Fixture::with("model_provider = \"custom\"   # 公司代理\n");
        fixture.apply(COMMAND).unwrap();
        assert!(
            fixture
                .read()
                .starts_with("model_provider = \"managed_gateway\"   # 公司代理\n")
        );
    }

    #[test]
    fn utf8_bom_is_stripped_for_parsing_and_preserved_on_write() {
        let original = "\u{feff}model_provider = \"custom\"\n";
        let fixture = Fixture::with(original);
        let inspection = inspect(&fixture.config, COMMAND).unwrap();
        assert_eq!(inspection.model_provider.as_deref(), Some("custom"));
        let outcome = fixture.apply(COMMAND).unwrap();
        assert_eq!(outcome.original.as_deref(), Some(original));
        let text = fixture.read();
        assert!(text.starts_with("\u{feff}model_provider = \"managed_gateway\"\n"));
        assert_eq!(text.matches('\u{feff}').count(), 1);
        assert!(inspect(&fixture.config, COMMAND).unwrap().fully_managed);
        assert!(restore(&fixture.config, &inspection.previous()).unwrap());
        assert_eq!(fixture.read(), original);
    }

    // ---- 检查 ----

    #[test]
    fn inspect_missing_file_returns_default() {
        let fixture = Fixture::new();
        assert_eq!(
            inspect(&fixture.config, COMMAND).unwrap(),
            ConfigInspection::default()
        );
    }

    #[test]
    fn inspect_reports_values_and_full_management() {
        let fixture =
            Fixture::with("model_provider = \"custom\"\nmodel_catalog_json = \"C:\\\\c.json\"\n");
        let before = inspect(&fixture.config, COMMAND).unwrap();
        assert!(before.exists);
        assert_eq!(before.model_provider.as_deref(), Some("custom"));
        assert_eq!(before.model_catalog_json.as_deref(), Some("C:\\c.json"));
        assert!(!before.has_managed_provider);
        assert!(!before.fully_managed);
        assert!(!before.has_managed_residue());
        assert_eq!(
            before.previous(),
            PreviousConfig {
                model_provider: Some("custom".into()),
                model_catalog_json: Some("C:\\c.json".into()),
            }
        );

        fixture.apply(COMMAND).unwrap();
        let after = inspect(&fixture.config, COMMAND).unwrap();
        assert!(after.fully_managed);
        assert!(after.has_managed_provider);
        assert!(after.has_managed_residue());
        assert_eq!(after.model_provider.as_deref(), Some(consts::PROVIDER_ID));
        assert_eq!(after.model_catalog_json, None);
        // 残留的受管 model_provider 不能当作原值
        assert_eq!(after.previous(), PreviousConfig::default());
        // 安装目录变化：command 不同即不完全受管
        assert!(
            !inspect(&fixture.config, "D:\\other\\c.exe")
                .unwrap()
                .fully_managed
        );
    }

    #[test]
    fn inspect_is_not_fully_managed_when_any_managed_key_drifts() {
        let fixture = Fixture::new();
        fixture.apply(COMMAND).unwrap();
        let managed = fixture.read();
        let drifts = [
            managed.replace(
                "model_provider = \"managed_gateway\"",
                "model_provider = \"x\"",
            ),
            managed.replace("name = \"Managed Gateway\"", "name = \"x\""),
            managed.replace("wire_api = \"responses\"", "wire_api = \"chat\""),
            managed.replace("\"codex-helper/managed-gateway\"", "\"x\""),
            managed.replace(
                "wire_api = \"responses\"\n",
                "wire_api = \"responses\"\nenv_key = \"K\"\n",
            ),
            managed.replace(
                "wire_api = \"responses\"\n",
                "wire_api = \"responses\"\nrequires_openai_auth = false\n",
            ),
            format!("model_catalog_json = \"c.json\"\n{managed}"),
            managed.replace("args = [\"get\", \"codex-helper/managed-gateway\"]\n", ""),
        ];
        for drift in drifts {
            assert_ne!(drift, managed);
            std::fs::write(&fixture.config, &drift).unwrap();
            assert!(
                !inspect(&fixture.config, COMMAND).unwrap().fully_managed,
                "{drift}"
            );
        }
    }

    // ---- 回滚 ----

    #[test]
    fn rollback_restores_original_or_removes_created_file() {
        let fixture = Fixture::with("model = \"o3\"\r\n");
        let outcome = fixture.apply(COMMAND).unwrap();
        rollback(&fixture.config, &outcome).unwrap();
        assert_eq!(fixture.read(), "model = \"o3\"\r\n");

        let fresh = Fixture::new();
        let outcome = fresh.apply(COMMAND).unwrap();
        rollback(&fresh.config, &outcome).unwrap();
        assert!(!fresh.config.exists());
    }

    #[test]
    fn rollback_without_change_is_noop() {
        let fixture = Fixture::new();
        fixture.apply(COMMAND).unwrap();
        let outcome = fixture.apply(COMMAND).unwrap();
        assert!(!outcome.changed);
        std::fs::write(&fixture.config, "user = 1\n").unwrap();
        rollback(&fixture.config, &outcome).unwrap();
        assert_eq!(fixture.read(), "user = 1\n");
    }

    // ---- 还原与残留清理 ----

    #[test]
    fn restore_sets_or_removes_previous_values() {
        let fixture = Fixture::with("model = \"o3\"\n");
        fixture.apply(COMMAND).unwrap();
        let previous = PreviousConfig {
            model_provider: Some("custom".into()),
            model_catalog_json: Some("C:\\c.json".into()),
        };
        assert!(restore(&fixture.config, &previous).unwrap());
        let inspection = inspect(&fixture.config, COMMAND).unwrap();
        assert_eq!(inspection.model_provider.as_deref(), Some("custom"));
        assert_eq!(inspection.model_catalog_json.as_deref(), Some("C:\\c.json"));
        assert!(!inspection.has_managed_residue());

        assert!(restore(&fixture.config, &PreviousConfig::default()).unwrap());
        assert_eq!(fixture.read(), "model = \"o3\"\n");
        // 已还原：再次还原不写
        assert!(!restore(&fixture.config, &PreviousConfig::default()).unwrap());
    }

    #[test]
    fn restore_and_residue_tolerate_missing_file() {
        let fixture = Fixture::new();
        assert!(!restore(&fixture.config, &PreviousConfig::default()).unwrap());
        assert!(!remove_residue(&fixture.config).unwrap());
        assert!(!fixture.config.exists());
    }

    #[test]
    fn restore_keeps_explicit_empty_model_providers_but_drops_implicit_one() {
        let explicit = Fixture::with("[model_providers]\n");
        explicit.apply(COMMAND).unwrap();
        restore(&explicit.config, &PreviousConfig::default()).unwrap();
        assert_eq!(explicit.read(), "[model_providers]\n");

        let implicit = Fixture::with("model = \"o3\"\n");
        implicit.apply(COMMAND).unwrap();
        restore(&implicit.config, &PreviousConfig::default()).unwrap();
        let doc = implicit.read().parse::<DocumentMut>().unwrap();
        assert!(!doc.contains_key(MODEL_PROVIDERS_KEY));
    }

    #[test]
    fn remove_residue_only_touches_managed_content() {
        let fixture = Fixture::with("model_catalog_json = \"c.json\"\n");
        fixture.apply(COMMAND).unwrap();
        let text = fixture.read().replace(
            "model_provider = \"managed_gateway\"\n",
            "model_provider = \"managed_gateway\"\nmodel_catalog_json = \"user.json\"\n",
        );
        std::fs::write(&fixture.config, text).unwrap();
        assert!(remove_residue(&fixture.config).unwrap());
        assert_eq!(fixture.read(), "model_catalog_json = \"user.json\"\n");
        assert!(!remove_residue(&fixture.config).unwrap());

        // model_provider 指向其他 provider 时不删
        let other = Fixture::with(
            "model_provider = \"custom\"\n\n[model_providers.managed_gateway]\nname = \"x\"\n",
        );
        assert!(remove_residue(&other.config).unwrap());
        assert_eq!(other.read(), "model_provider = \"custom\"\n");
    }

    #[test]
    fn remove_residue_with_record_restores_interrupted_enable() {
        // 启用在记录原值后中断：配置已被接管，状态文件记录了原值
        let original =
            "model_provider = \"custom\"   # 公司代理\nmodel_catalog_json = \"c.json\"\n";
        let fixture = Fixture::with(original);
        fixture.apply(COMMAND).unwrap();
        let recorded = PreviousConfig {
            model_provider: Some("custom".into()),
            model_catalog_json: Some("c.json".into()),
        };
        assert!(remove_residue_with(&fixture.config, &recorded).unwrap());
        assert_eq!(fixture.read(), original);
        assert!(!has_residue(&fixture.config).unwrap());

        // model_provider 已是用户自己的值：原记录不生效，不改 model_provider 与 catalog
        let user = Fixture::with(
            "model_provider = \"mine\"\n\n[model_providers.managed_gateway]\nname = \"x\"\n",
        );
        assert!(remove_residue_with(&user.config, &recorded).unwrap());
        assert_eq!(user.read(), "model_provider = \"mine\"\n");

        // 记录为空：等同于 remove_residue
        let empty = Fixture::with("model_provider = \"managed_gateway\"\nmodel = \"o3\"\n");
        assert!(remove_residue_with(&empty.config, &PreviousConfig::default()).unwrap());
        assert_eq!(empty.read(), "model = \"o3\"\n");
    }

    #[test]
    fn has_residue_matches_inspection() {
        let missing = Fixture::new();
        assert!(!has_residue(&missing.config).unwrap());
        let clean = Fixture::with("model_provider = \"custom\"\n");
        assert!(!has_residue(&clean.config).unwrap());
        let managed = Fixture::new();
        managed.apply(COMMAND).unwrap();
        assert!(has_residue(&managed.config).unwrap());
        assert!(
            inspect(&managed.config, COMMAND)
                .unwrap()
                .has_managed_residue()
        );
        let broken = Fixture::with("model = [\n");
        assert!(matches!(
            has_residue(&broken.config),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn remove_residue_deletes_inline_managed_provider() {
        let fixture = Fixture::with(
            "model_provider = \"managed_gateway\"\nmodel_providers = { managed_gateway = { name = \"x\" }, custom = { name = \"c\" } }\n",
        );
        assert!(remove_residue(&fixture.config).unwrap());
        let doc = fixture.read().parse::<DocumentMut>().unwrap();
        assert!(!doc.contains_key(MODEL_PROVIDER_KEY));
        let providers = doc[MODEL_PROVIDERS_KEY].as_inline_table().unwrap();
        assert!(!providers.contains_key(consts::PROVIDER_ID));
        assert!(providers.contains_key("custom"));
    }

    #[test]
    fn removing_root_key_keeps_comment_above_it() {
        let fixture = Fixture::with(
            "model = \"o3\"\n# 自定义模型目录\nmodel_catalog_json = \"c.json\"\nsandbox = \"x\"\n",
        );
        fixture.apply(COMMAND).unwrap();
        assert!(
            fixture
                .read()
                .starts_with("model = \"o3\"\n# 自定义模型目录\nsandbox = \"x\"\n")
        );

        let first = Fixture::with("# 目录\nmodel_catalog_json = \"c.json\"\n\n[t]\na = 1\n");
        first.apply(COMMAND).unwrap();
        assert!(
            first
                .read()
                .starts_with("# 目录\nmodel_provider = \"managed_gateway\"\n")
        );
    }
}
