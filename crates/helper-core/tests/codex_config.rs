//! config.toml 校正与还原的夹具驱动测试（设计 §11：逐字节还原且保留启用期间的无关修改）。

use std::path::PathBuf;

use helper_core::codex_config::{self, PreviousConfig};
use helper_core::consts;

const COMMAND: &str =
    "C:\\Users\\张三\\AppData\\Local\\Programs\\CodexHelper\\codex-helper-credential.exe";

struct Sandbox {
    _dir: tempfile::TempDir,
    config: PathBuf,
    backup: PathBuf,
}

impl Sandbox {
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
        let sandbox = Self::new();
        std::fs::write(&sandbox.config, text).unwrap();
        sandbox
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap()
    }

    fn write(&self, text: &str) {
        std::fs::write(&self.config, text).unwrap();
    }

    /// 模拟一次启用：记录原值后校正。
    fn enable(&self) -> PreviousConfig {
        let previous = codex_config::inspect(&self.config, COMMAND)
            .unwrap()
            .previous();
        let outcome = codex_config::apply_managed(&self.config, &self.backup, COMMAND).unwrap();
        assert!(outcome.changed);
        assert!(
            codex_config::inspect(&self.config, COMMAND)
                .unwrap()
                .fully_managed
        );
        previous
    }
}

fn parse(text: &str) -> toml_edit::DocumentMut {
    text.trim_start_matches('\u{feff}').parse().unwrap()
}

/// 语义比较：两段文本解析后按 TOML 值相等（忽略格式与键序）。
fn assert_same_values(left: &str, right: &str) {
    let left: toml_edit::Value = parse(left).as_table().clone().into_inline_table().into();
    let right: toml_edit::Value = parse(right).as_table().clone().into_inline_table().into();
    assert_eq!(normalize(&left), normalize(&right));
}

fn normalize(value: &toml_edit::Value) -> String {
    match value {
        toml_edit::Value::InlineTable(table) => {
            let mut entries: Vec<_> = table
                .iter()
                .map(|(key, value)| format!("{key}={}", normalize(value)))
                .collect();
            entries.sort();
            format!("{{{}}}", entries.join(","))
        }
        toml_edit::Value::Array(array) => {
            let items: Vec<_> = array.iter().map(normalize).collect();
            format!("[{}]", items.join(","))
        }
        toml_edit::Value::String(value) => format!("{:?}", value.value()),
        toml_edit::Value::Integer(value) => value.value().to_string(),
        toml_edit::Value::Float(value) => value.value().to_string(),
        toml_edit::Value::Boolean(value) => value.value().to_string(),
        toml_edit::Value::Datetime(value) => value.value().to_string(),
    }
}

fn assert_round_trip(name: &str, original: &str) {
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();
    assert!(
        codex_config::restore(&sandbox.config, &previous).unwrap(),
        "{name}"
    );
    assert_eq!(sandbox.read(), original, "夹具 {name} 未能逐字节还原");
}

const FIXTURES: &[(&str, &str)] = &[
    ("空文件", ""),
    ("只有注释", "# Codex 配置\n# 第二行注释\n"),
    ("只有注释无结尾换行", "# Codex 配置"),
    (
        "已有 model_provider 带行尾注释",
        "model = \"gpt-5.2-codex\"\nmodel_provider = \"custom\"   # 公司代理\napproval_policy = \"untrusted\"\napprovals_reviewer = \"human_review\"\nsandbox_mode = \"read-only\"\n\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://example.invalid/v1\"\n",
    ),
    (
        "无 model_provider",
        "model = \"gpt-5.2-codex\"\nmodel_reasoning_effort = \"high\"\n",
    ),
    (
        "已有其他 provider",
        "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"Custom\"\nenv_key = \"CUSTOM_KEY\"\n\n[model_providers.other]\nname = \"Other\"\nwire_api = \"chat\"\n\n[projects.\"C:\\\\work\\\\repo\"]\ntrust_level = \"trusted\"\n",
    ),
    (
        "含 projects 与 plugins",
        "# 顶部注释\nmodel = \"o3\"\n\n[projects.\"/Users/me/work\"]\ntrust_level = \"trusted\"\n\n# 插件配置\n[plugins]\nenabled = [\"a\", \"b\"]\n\n[plugins.a]\npath = 'C:\\plugins\\a'\n",
    ),
    (
        "无结尾换行",
        "model = \"o3\"\n[projects.\"/w\"]\ntrust_level = \"trusted\"",
    ),
    (
        "CRLF 换行",
        "model = \"o3\"\r\nmodel_provider = \"custom\" # 注释\r\n\r\n[model_providers.custom]\r\nname = \"Custom\"\r\n\r\n[projects.\"C:\\\\w\"]\r\ntrust_level = \"trusted\"\r\n",
    ),
    (
        "CRLF 且无结尾换行",
        "model = \"o3\"\r\n\r\n[projects.\"C:\\\\w\"]\r\ntrust_level = \"trusted\"",
    ),
    (
        "显式空 model_providers 表头",
        "model = \"o3\"\n\n[model_providers]\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n",
    ),
    (
        "UTF-8 BOM",
        "\u{feff}model = \"o3\"\nmodel_provider = \"custom\"\n",
    ),
    (
        "model_catalog_json 为最后一个根键",
        "model = \"o3\"\nmodel_catalog_json = \"C:\\\\tmp\\\\catalog.json\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n",
    ),
];

#[test]
fn fixtures_round_trip_byte_for_byte() {
    for (name, original) in FIXTURES {
        assert_round_trip(name, original);
    }
}

#[test]
fn consecutive_apply_is_idempotent_for_every_fixture() {
    for (name, original) in FIXTURES {
        let sandbox = Sandbox::with(original);
        sandbox.enable();
        let enabled = sandbox.read();
        std::fs::remove_file(&sandbox.backup).unwrap();
        let outcome =
            codex_config::apply_managed(&sandbox.config, &sandbox.backup, COMMAND).unwrap();
        assert!(!outcome.changed, "{name}");
        assert_eq!(sandbox.read(), enabled, "{name}");
        assert!(!sandbox.backup.exists(), "{name}");
    }
}

/// `model_catalog_json` 启用时被移除、停用时追加回根键末尾：不是最后一个根键时位置无法保证，
/// 这里只要求语义相等（键值相同）；它原本就是最后一个根键的情形见 FIXTURES，可逐字节还原。
#[test]
fn catalog_pointer_in_the_middle_restores_semantically() {
    let original = concat!(
        "# 顶部注释\n",
        "model_catalog_json = 'C:\\tmp\\catalog.json'\n",
        "model = \"o3\"\n",
        "model_provider = \"custom\"\n",
        "\n[model_providers.custom]\n",
        "name = \"Custom\"\n",
    );
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();
    assert_eq!(
        previous.model_catalog_json.as_deref(),
        Some("C:\\tmp\\catalog.json")
    );
    assert!(!sandbox.read().contains("model_catalog_json"));
    // 被移除键上方的注释留在原处
    assert!(sandbox.read().starts_with("# 顶部注释\nmodel = \"o3\"\n"));

    assert!(codex_config::restore(&sandbox.config, &previous).unwrap());
    let restored = sandbox.read();
    assert_same_values(&restored, original);
    assert!(restored.starts_with("# 顶部注释\n"));
    assert!(restored.ends_with("[model_providers.custom]\nname = \"Custom\"\n"));
}

#[test]
fn restore_keeps_user_changes_made_while_enabled() {
    let original = concat!(
        "model = \"o3\"\n",
        "model_provider = \"custom\" # 原 provider\n",
        "\n[model_providers.custom]\n",
        "name = \"Custom\"\n",
        "\n[projects.\"/w\"]\n",
        "trust_level = \"trusted\"\n",
    );
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();

    // 启用期间用户的无关修改：改根键、在 custom 节末尾加注释、改 projects、新增 provider 与新节；
    // 同时在受管节里加了一个键（随受管节一起删除）。
    let edited = sandbox
        .read()
        .replace("model = \"o3\"", "model = \"gpt-5\"")
        .replace(
            "name = \"Custom\"\n",
            "name = \"Custom\"\n# 备注：custom 走公司网关\n",
        )
        .replace("trust_level = \"trusted\"", "trust_level = \"untrusted\"")
        .replace(
            "wire_api = \"responses\"\n",
            "wire_api = \"responses\"\nstream_max_retries = 3\n",
        )
        + "\n[model_providers.added]\nname = \"Added\"\n\n[plugins]\nenabled = true\n";
    sandbox.write(&edited);

    assert!(codex_config::restore(&sandbox.config, &previous).unwrap());
    let expected = original
        .replace("model = \"o3\"", "model = \"gpt-5\"")
        .replace(
            "name = \"Custom\"\n",
            "name = \"Custom\"\n# 备注：custom 走公司网关\n",
        )
        .replace("trust_level = \"trusted\"", "trust_level = \"untrusted\"")
        + "\n[model_providers.added]\nname = \"Added\"\n\n[plugins]\nenabled = true\n";
    assert_eq!(sandbox.read(), expected);
    let inspection = codex_config::inspect(&sandbox.config, COMMAND).unwrap();
    assert!(!inspection.has_managed_residue());
    assert_eq!(inspection.model_provider.as_deref(), Some("custom"));
}

#[test]
fn reapply_after_drift_keeps_user_changes_and_restore_is_still_exact() {
    let original = "model = \"o3\"\nmodel_provider = \"custom\"   # 原 provider\n";
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();

    // 用户（或其他工具）改回了 model_provider 并改了 base_url，同时新增无关键
    let drifted = sandbox
        .read()
        .replace("\"managed_gateway\"   #", "\"custom\"   #")
        .replace("http://10.20.30.61:8080", "http://evil:1")
        + "\n[tui]\nnotifications = true\n";
    sandbox.write(&drifted);
    let outcome = codex_config::apply_managed(&sandbox.config, &sandbox.backup, COMMAND).unwrap();
    assert!(outcome.changed);
    assert_eq!(std::fs::read_to_string(&sandbox.backup).unwrap(), drifted);
    let text = sandbox.read();
    assert!(text.contains("model_provider = \"managed_gateway\"   # 原 provider\n"));
    assert!(text.contains("[tui]\nnotifications = true\n"));
    assert!(
        codex_config::inspect(&sandbox.config, COMMAND)
            .unwrap()
            .fully_managed
    );

    codex_config::restore(&sandbox.config, &previous).unwrap();
    assert_eq!(
        sandbox.read(),
        format!("{original}\n[tui]\nnotifications = true\n")
    );
}

#[test]
fn comment_after_last_table_before_managed_header_moves_to_file_end() {
    let original = "model = \"o3\"\n\n[model_providers.custom]\nname = \"Custom\"\n";
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();
    let edited = sandbox
        .read()
        .replace("name = \"Custom\"\n", "name = \"Custom\"\n# 尾注释\n");
    sandbox.write(&edited);
    codex_config::restore(&sandbox.config, &previous).unwrap();
    assert_eq!(sandbox.read(), format!("{original}# 尾注释\n"));
}

#[test]
fn residue_cleanup_matches_restore_without_previous_values() {
    for (name, original) in FIXTURES {
        let sandbox = Sandbox::with(original);
        sandbox.enable();
        assert!(
            codex_config::remove_residue(&sandbox.config).unwrap(),
            "{name}"
        );
        let inspection = codex_config::inspect(&sandbox.config, COMMAND).unwrap();
        assert!(!inspection.has_managed_residue(), "{name}");
        // 残留清理不碰 model_catalog_json、也无从得知原 model_provider：其余内容应与原文一致
        let doc = parse(&sandbox.read());
        assert!(doc.get("model_provider").is_none(), "{name}");
    }
}

/// 启用在记录原值之后中断：停用状态下的残留清理按记录的原值还原，逐字节回到原文。
#[test]
fn interrupted_enable_is_restored_from_recorded_previous() {
    for (name, original) in FIXTURES {
        let sandbox = Sandbox::with(original);
        let previous = sandbox.enable();
        codex_config::remove_residue_with(&sandbox.config, &previous).unwrap();
        assert_eq!(sandbox.read(), *original, "夹具 {name} 未能逐字节还原");
        assert!(
            !codex_config::has_residue(&sandbox.config).unwrap(),
            "{name}"
        );
    }
}

/// 干净配置（无受管痕迹）里用户原值恰好等于受管固定值（如本来就是 `on-request`）：
/// 如实记录为原值，停用后逐字节写回——因为受管固定值并非全部等于 Codex 默认值
/// （`sandbox_mode` 默认 `read-only`），不能静默删掉用户显式写的行。
#[test]
fn clean_config_values_equal_to_managed_constants_are_recorded_and_restored() {
    let original = concat!(
        "model = \"o3\"\n",
        "model_provider = \"custom\"\n",
        "approval_policy = \"on-request\"\n",
        "approvals_reviewer = \"auto_review\"\n",
        "sandbox_mode = \"workspace-write\"\n",
    );
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();
    // 启用前无受管痕迹：同值键是用户真正的配置，如实记录
    assert_eq!(previous.approval_policy.as_deref(), Some("on-request"));
    assert_eq!(previous.approvals_reviewer.as_deref(), Some("auto_review"));
    assert_eq!(previous.sandbox_mode.as_deref(), Some("workspace-write"));

    // 停用后逐字节回到原文（不丢用户显式写的行）
    assert!(codex_config::restore(&sandbox.config, &previous).unwrap());
    assert_eq!(sandbox.read(), original);
    assert!(!codex_config::has_residue(&sandbox.config).unwrap());
}

/// 已带受管痕迹（启用中断 / 残留）时，值等于受管固定值的键才视为残留、不当作原值：
/// 避免把本工具自己写入的值当成用户原值写回而造成永久残留。
#[test]
fn managed_valued_keys_are_absorbed_only_when_residue_present() {
    // model_provider 指向受管 provider（残留）+ 三个键值等于受管固定值
    let residue = concat!(
        "model_provider = \"managed_gateway\"\n",
        "approval_policy = \"on-request\"\n",
        "approvals_reviewer = \"auto_review\"\n",
        "sandbox_mode = \"workspace-write\"\n",
    );
    let sandbox = Sandbox::with(residue);
    let previous = codex_config::inspect(&sandbox.config, COMMAND)
        .unwrap()
        .previous();
    assert_eq!(previous.model_provider, None);
    assert_eq!(previous.approval_policy, None);
    assert_eq!(previous.approvals_reviewer, None);
    assert_eq!(previous.sandbox_mode, None);
}

/// 启用时受管根键被校正为固定值、停用时还原为用户原值（含值不同于受管值与不存在的情形）。
#[test]
fn approval_and_sandbox_keys_are_set_and_restored() {
    let original = concat!(
        "model = \"o3\"\n",
        "approval_policy = \"never\"\n",
        "sandbox_mode = \"danger-full-access\"\n",
    );
    let sandbox = Sandbox::with(original);
    let previous = sandbox.enable();
    assert_eq!(previous.approval_policy.as_deref(), Some("never"));
    assert_eq!(previous.sandbox_mode.as_deref(), Some("danger-full-access"));
    assert_eq!(previous.approvals_reviewer, None);

    let doc = parse(&sandbox.read());
    assert_eq!(
        doc["approval_policy"].as_str(),
        Some(consts::APPROVAL_POLICY)
    );
    assert_eq!(
        doc["approvals_reviewer"].as_str(),
        Some(consts::APPROVALS_REVIEWER)
    );
    assert_eq!(doc["sandbox_mode"].as_str(), Some(consts::SANDBOX_MODE));

    assert!(codex_config::restore(&sandbox.config, &previous).unwrap());
    assert_eq!(sandbox.read(), original);
}

/// 无记录的残留清理：值等于受管固定值的审批 / 沙箱键被删除，其他值保留。
#[test]
fn residue_cleanup_drops_managed_valued_approval_keys_keeps_user_values() {
    let original = concat!("model = \"o3\"\n", "approval_policy = \"never\"\n",);
    let sandbox = Sandbox::with(original);
    sandbox.enable();
    // 模拟启用中断后状态文件丢失：无记录，但值等于受管值的键应被清理
    let text = sandbox.read().replace("approval_policy = \"never\"\n", "");
    sandbox.write(&text);
    assert!(codex_config::remove_residue(&sandbox.config).unwrap());
    let doc = parse(&sandbox.read());
    assert!(doc.get("model_provider").is_none());
    assert!(doc.get("approval_policy").is_none());
    assert!(doc.get("approvals_reviewer").is_none());
    assert!(doc.get("sandbox_mode").is_none());

    // 用户自己的值（不同于受管值）：即使 model_provider 指向受管 provider 也不删
    let user = "approval_policy = \"never\"\napprovals_reviewer = \"mine\"\n";
    let sandbox = Sandbox::with(&format!("{user}model_provider = \"managed_gateway\"\n"));
    assert!(codex_config::remove_residue(&sandbox.config).unwrap());
    let doc = parse(&sandbox.read());
    assert_eq!(doc["approval_policy"].as_str(), Some("never"));
    assert_eq!(doc["approvals_reviewer"].as_str(), Some("mine"));
    assert!(doc.get("sandbox_mode").is_none());
}

/// 结构不符时 inspect 与 apply_managed 一致地报错，且不写入任何内容。
#[test]
fn inspect_reports_structure_errors_without_writing() {
    let text = "model = \"o3\"\n\n[model_providers.managed_gateway]\nauth = [\"x\"]\n";
    let sandbox = Sandbox::with(text);
    match codex_config::inspect(&sandbox.config, COMMAND) {
        Err(codex_config::ConfigError::NotATable { key }) => {
            assert_eq!(key, "model_providers.managed_gateway.auth")
        }
        other => panic!("应返回 NotATable，实际 {other:?}"),
    }
    assert_eq!(sandbox.read(), text);
    assert!(!sandbox.backup.exists());
    // 残留判定与清理对结构宽松
    assert!(codex_config::has_residue(&sandbox.config).unwrap());
    assert!(codex_config::remove_residue(&sandbox.config).unwrap());
    assert_eq!(sandbox.read(), "model = \"o3\"\n");
}
