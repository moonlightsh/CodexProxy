//! 工具自身状态文件（设计 §4.1）：`%LOCALAPPDATA%\CodexHelper\state.json`。不保存 Key。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codex_config::PreviousConfig;

/// 状态文件内容。字段名与设计文档中的 JSON 一致（snake_case），缺失字段取默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HelperState {
    pub enabled: bool,
    /// 启用前 `model_provider` 的值；不存在为 `null`
    pub previous_model_provider: Option<String>,
    /// 启用时被移除的外部 catalog 指针；无则为 `null`
    pub previous_model_catalog_json: Option<String>,
    pub autostart: bool,
}

impl HelperState {
    /// 取出记录的原值。
    pub fn previous(&self) -> PreviousConfig {
        PreviousConfig {
            model_provider: self.previous_model_provider.clone(),
            model_catalog_json: self.previous_model_catalog_json.clone(),
        }
    }

    /// 记录原值。只应在“停用 → 启用”的转换时调用，已启用状态下不得覆盖。
    pub fn set_previous(&mut self, previous: PreviousConfig) {
        self.previous_model_provider = previous.model_provider;
        self.previous_model_catalog_json = previous.model_catalog_json;
    }

    /// 清空原值（停用完成后）。
    pub fn clear_previous(&mut self) {
        self.previous_model_provider = None;
        self.previous_model_catalog_json = None;
    }
}

/// 读取状态文件；不存在返回 `Ok(None)`；内容损坏返回错误（由调用方决定如何处理）。
///
/// 缺失字段取默认值、未知字段忽略（`#[serde(default)]` 与 serde 默认宽容行为）。
/// 错误消息统一为中文、含路径、不含文件内容：非法 UTF-8 视为“损坏”，其余 IO 错误
/// （权限不足、路径是目录等）归为“读取失败”。
pub fn load(path: &Path) -> anyhow::Result<Option<HelperState>> {
    let text = match crate::fsutil::read_optional(path) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(anyhow::anyhow!(
                "状态文件损坏，无法解析：{}",
                path.display()
            ));
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "读取状态文件失败：{}：{}",
                path.display(),
                error
            ));
        }
    };
    // 先解析成通用 JSON 值：serde 派生的结构体默认也接受数组（按位置反序列化），
    // 若不先校验顶层必须是对象，形如 "[]" 的内容会被当成合法的全默认状态而非损坏。
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| anyhow::anyhow!("状态文件损坏，无法解析：{}", path.display()))?;
    if !value.is_object() {
        return Err(anyhow::anyhow!(
            "状态文件损坏，无法解析：{}",
            path.display()
        ));
    }
    let state = serde_json::from_value(value)
        .map_err(|_| anyhow::anyhow!("状态文件损坏，无法解析：{}", path.display()))?;
    Ok(Some(state))
}

/// 原子写入状态文件（格式化 JSON，结尾换行）。
pub fn save(path: &Path, state: &HelperState) -> anyhow::Result<()> {
    let mut text = serde_json::to_string_pretty(state)?;
    text.push('\n');
    crate::fsutil::atomic_write(path, text.as_bytes())
        .map_err(|error| anyhow::anyhow!("写入状态文件 {} 失败：{error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(load(&path).unwrap(), None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let state = HelperState {
            enabled: true,
            previous_model_provider: Some("custom".into()),
            previous_model_catalog_json: Some("/path/catalog.json".into()),
            autostart: true,
        };
        save(&path, &state).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn save_writes_formatted_json_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &HelperState::default()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains("\n  "), "应为格式化 JSON（含缩进）");
    }

    #[test]
    fn load_partial_json_fills_missing_fields_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"enabled": true}"#).unwrap();
        let state = load(&path).unwrap().unwrap();
        assert_eq!(
            state,
            HelperState {
                enabled: true,
                previous_model_provider: None,
                previous_model_catalog_json: None,
                autostart: false,
            }
        );
    }

    #[test]
    fn load_unknown_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"enabled": false, "unknown_future_field": 123, "autostart": true}"#,
        )
        .unwrap();
        let state = load(&path).unwrap().unwrap();
        assert!(!state.enabled);
        assert!(state.autostart);
    }

    #[test]
    fn load_corrupted_json_returns_chinese_error_with_path_but_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{不是合法 JSON").unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("状态文件损坏"));
        assert!(message.contains(&path.display().to_string()));
        assert!(!message.contains("不是合法 JSON"));
    }

    /// 审查发现（minor）：serde 派生结构体默认也接受 JSON 数组（按位置反序列化），
    /// 加上 `#[serde(default)]` 后，"[]" 会被当成合法的全默认状态而非损坏，需在
    /// 解析前先校验顶层必须是 JSON 对象。
    #[test]
    fn load_json_array_is_treated_as_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "[]").unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("状态文件损坏"));
        assert!(message.contains(&path.display().to_string()));
    }

    /// 审查发现（minor）：非 `InvalidData` 的 IO 错误（如路径实际是目录）应走
    /// “读取状态文件失败”分支，而非“状态文件损坏”分支。Windows 上打开目录返回
    /// `PermissionDenied`，同样落入该分支，因此无需 `cfg` 区分平台。
    #[test]
    fn load_directory_path_returns_read_failed_error_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state_dir");
        std::fs::create_dir(&path).unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("读取状态文件失败"));
        assert!(message.contains(&path.display().to_string()));
    }

    /// 非法 UTF-8 属于“内容损坏”，而非普通 IO 错误：消息同样要求中文、含路径、不含内容。
    #[test]
    fn load_invalid_utf8_returns_chinese_error_with_path_but_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, [0xFF, 0xFE, b'{', b'}']).unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("状态文件损坏"));
    }

    /// 设计 §4.1 中的 JSON 示例应能解析，且字段名为 snake_case。
    #[test]
    fn design_doc_example_parses_with_snake_case_fields() {
        let json = r#"{
  "enabled": true,
  "previous_model_provider": "custom",
  "previous_model_catalog_json": null,
  "autostart": false
}"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, json).unwrap();
        let state = load(&path).unwrap().unwrap();
        assert_eq!(
            state,
            HelperState {
                enabled: true,
                previous_model_provider: Some("custom".into()),
                previous_model_catalog_json: None,
                autostart: false,
            }
        );
    }

    #[test]
    fn set_previous_and_clear_previous_round_trip() {
        let mut state = HelperState::default();
        state.set_previous(PreviousConfig {
            model_provider: Some("custom".into()),
            model_catalog_json: Some("/catalog.json".into()),
        });
        assert_eq!(state.previous_model_provider.as_deref(), Some("custom"));
        assert_eq!(
            state.previous_model_catalog_json.as_deref(),
            Some("/catalog.json")
        );
        assert_eq!(
            state.previous(),
            PreviousConfig {
                model_provider: Some("custom".into()),
                model_catalog_json: Some("/catalog.json".into()),
            }
        );

        state.clear_previous();
        assert_eq!(state.previous_model_provider, None);
        assert_eq!(state.previous_model_catalog_json, None);
    }
}
