//! `agents-state.json` 的严格读取与 Runtime 设置投影。
//!
//! 该文件只保存设置页的禁用状态和模型选择覆盖。Agent Markdown 仍由
//! `agent_catalog` 负责解析；两者通过稳定 ID 和候选指纹关联，避免产生第二套
//! Agent 定义事实源。

use super::PluginId;
use super::agent_catalog::AgentCatalogEntry;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const AGENT_SETTINGS_FILE: &str = "agents-state.json";
const MAX_AGENT_SETTINGS_BYTES: u64 = 512 * 1024;
const MAX_AGENT_SETTINGS_ENTRIES: usize = 512;
const MAX_AGENT_ID_BYTES: usize = 256;
const ALLOWED_REASONING_LEVELS: [&str; 7] =
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 已验证的 Runtime 模型覆盖；`none` 是有意保留的显式推理等级。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentModelSelectionOverride {
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
}

/// 当前设置文件的唯一内存表示。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AgentSettingsState {
    disabled_agent_ids: BTreeSet<String>,
    built_in_model_overrides: BTreeMap<String, AgentModelSelectionOverride>,
    plugin_model_overrides: BTreeMap<String, AgentModelSelectionOverride>,
}

impl AgentSettingsState {
    /// 判断任意稳定 ID 候选是否被设置页禁用。
    pub(super) fn is_disabled<'a>(&self, candidate_ids: impl IntoIterator<Item = &'a str>) -> bool {
        candidate_ids.into_iter().any(|candidate| {
            self.disabled_agent_ids
                .iter()
                .any(|stored| stored.eq_ignore_ascii_case(candidate))
        })
    }

    /// 按稳定 ID 查找内置 Agent 的模型覆盖。
    pub(super) fn built_in_override<'a>(
        &self,
        candidate_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<AgentModelSelectionOverride> {
        find_override(&self.built_in_model_overrides, candidate_ids)
    }

    /// 按稳定 ID 查找插件 Agent 的模型覆盖。
    pub(super) fn plugin_override<'a>(
        &self,
        candidate_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<AgentModelSelectionOverride> {
        find_override(&self.plugin_model_overrides, candidate_ids)
    }

    /// 返回不含提示词、路径和凭据的规范摘要输入，供候选缓存失效使用。
    pub(super) fn fingerprint_material(&self) -> Value {
        self.persisted_value()
    }

    /// 以唯一 camelCase 结构输出设置状态；不包含 Agent 正文或凭据。
    fn persisted_value(&self) -> Value {
        json!({
            "disabledAgentIds": self.disabled_agent_ids.iter().cloned().collect::<Vec<_>>(),
            "builtInModelSelectionOverrides": model_override_values(&self.built_in_model_overrides),
            "pluginAgentModelSelectionOverrides": model_override_values(&self.plugin_model_overrides),
        })
    }

    #[cfg(test)]
    pub(super) fn from_value(value: Value) -> Result<Self, String> {
        parse_state_value(value)
    }
}

/// 校验设置命令即将落盘的完整对象，并返回规范化 JSON，避免服务层先写后校验。
pub(crate) fn normalize_agent_settings_value(value: &Value) -> Result<Value, String> {
    let state = parse_state_value(value.clone())?;
    Ok(state.persisted_value())
}

/// 读取当前数据根下的唯一设置文件；文件不存在代表全新空设置。
pub(super) fn read_agent_settings(data_root: &Path) -> Result<AgentSettingsState, String> {
    let value = read_agent_settings_value(data_root)?;
    parse_state_value(value)
}

/// 返回设置文件路径，供前端设置命令和 Runtime 共用同一边界。
pub(super) fn agent_settings_path(data_root: &Path) -> PathBuf {
    data_root.join(AGENT_SETTINGS_FILE)
}

/// 读取并规范化完整设置对象，供 Tauri 设置命令和 Runtime 共用。
pub(crate) fn read_agent_settings_value(data_root: &Path) -> Result<Value, String> {
    let path = agent_settings_path(data_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(format!("读取 Agent 设置失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Agent 设置文件必须是普通文件".to_owned());
    }
    if metadata.len() > MAX_AGENT_SETTINGS_BYTES {
        return Err("Agent 设置文件超过大小限制".to_owned());
    }
    let content = fs::read(&path).map_err(|error| format!("读取 Agent 设置失败：{error}"))?;
    let value: Value =
        serde_json::from_slice(content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&content))
            .map_err(|_| "Agent 设置文件 JSON 无效".to_owned())?;
    normalize_agent_settings_value(&value)
}

/// 原子保存已经校验过的设置对象，确保读写端不引入第二种结构。
pub(crate) fn write_agent_settings_value(data_root: &Path, value: &Value) -> Result<(), String> {
    let normalized = normalize_agent_settings_value(value)?;
    let bytes = serde_json::to_vec_pretty(&normalized)
        .map_err(|error| format!("Agent 设置编码失败：{error}"))?;
    super::atomic_write_private(&agent_settings_path(data_root), &bytes)
}

/// 严格校验并规范化一个前端 `modelSelection` 对象。
pub(crate) fn normalize_model_selection_value(
    value: &Value,
) -> Result<AgentModelSelectionOverride, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "modelSelection 必须是对象".to_owned())?;
    reject_unknown_keys(object, ["providerId", "modelId", "options"])?;
    let provider_id = required_identifier(object, "providerId")?;
    let model_id = required_identifier(object, "modelId")?;
    let reasoning_effort = match object.get("options") {
        None => None,
        Some(Value::Object(options)) => {
            reject_unknown_keys(options, ["reasoningLevel"])?;
            match options.get("reasoningLevel") {
                None => None,
                Some(Value::String(value)) => Some(normalize_reasoning_level(value)?),
                Some(Value::Null) => {
                    return Err("modelSelection.options.reasoningLevel 不能为 null".to_owned());
                }
                Some(_) => {
                    return Err("modelSelection.options.reasoningLevel 必须是字符串".to_owned());
                }
            }
        }
        Some(Value::Null) => return Err("modelSelection.options 不能为 null".to_owned()),
        Some(_) => return Err("modelSelection.options 必须是对象".to_owned()),
    };
    Ok(AgentModelSelectionOverride {
        model: format!("{provider_id}::{model_id}"),
        reasoning_effort,
    })
}

/// 将规范化覆盖转换回 Source 使用的结构化对象。
pub(crate) fn model_selection_value(selection: &AgentModelSelectionOverride) -> Value {
    let mut value = Map::new();
    let (provider_id, model_id) = selection
        .model
        .split_once("::")
        .expect("模型覆盖在构造时已校验 provider::model");
    value.insert(
        "providerId".to_owned(),
        Value::String(provider_id.to_owned()),
    );
    value.insert("modelId".to_owned(), Value::String(model_id.to_owned()));
    if let Some(reasoning_effort) = &selection.reasoning_effort {
        value.insert(
            "options".to_owned(),
            json!({ "reasoningLevel": reasoning_effort }),
        );
    }
    Value::Object(value)
}

/// 构造 Source 约定的稳定插件 Agent ID：`plugin:<plugin>@<marketplace>:<agent>`。
pub(crate) fn plugin_agent_id(plugin: &PluginId, agent_name: &str) -> Result<String, String> {
    let marketplace = plugin
        .marketplace
        .as_deref()
        .ok_or_else(|| "运行时插件缺少 marketplace 身份".to_owned())?;
    let agent_name = normalize_component(agent_name, "插件 Agent 名称")?;
    Ok(format!(
        "plugin:{}@{}:{}",
        plugin.plugin, marketplace, agent_name
    ))
}

/// 将当前 Rust 内部命名空间名称映射为唯一 Source ID；仅用于同一进程内匹配，
/// 不作为持久化别名或历史格式迁移。
pub(super) fn plugin_agent_id_from_runtime_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("plugin:")?;
    let mut parts = rest.splitn(3, ':');
    let marketplace = parts.next()?;
    let plugin = parts.next()?;
    let agent = parts.next()?;
    if marketplace.is_empty() || plugin.is_empty() || agent.is_empty() {
        return None;
    }
    Some(format!("plugin:{plugin}@{marketplace}:{agent}"))
}

/// 为目录条目生成设置状态可引用的稳定 ID 集合。
pub(super) fn candidate_ids_for_entry(entry: &AgentCatalogEntry) -> Vec<String> {
    let name = entry.name.to_ascii_lowercase();
    match entry.source.as_str() {
        "builtin" => vec![entry.name.clone(), format!("built-in:{name}")],
        "global" => vec![entry.name.clone(), format!("user:user:{name}")],
        "project" => vec![entry.name.clone(), format!("user:workspace:{name}")],
        "plugin" => {
            let mut ids = vec![entry.name.clone()];
            if let Some(id) = plugin_agent_id_from_runtime_name(&entry.name) {
                ids.push(id);
            }
            ids
        }
        _ => vec![entry.name.clone()],
    }
}

fn parse_state_value(value: Value) -> Result<AgentSettingsState, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Agent 设置根必须是对象".to_owned())?;
    reject_unknown_keys(
        object,
        [
            "disabledAgentIds",
            "builtInModelSelectionOverrides",
            "pluginAgentModelSelectionOverrides",
        ],
    )?;
    let disabled_agent_ids = parse_disabled_ids(object.get("disabledAgentIds"))?;
    let built_in_model_overrides =
        parse_overrides(object.get("builtInModelSelectionOverrides"), false)?;
    let plugin_model_overrides =
        parse_overrides(object.get("pluginAgentModelSelectionOverrides"), true)?;
    Ok(AgentSettingsState {
        disabled_agent_ids,
        built_in_model_overrides,
        plugin_model_overrides,
    })
}

fn parse_disabled_ids(value: Option<&Value>) -> Result<BTreeSet<String>, String> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| "disabledAgentIds 必须是数组".to_owned())?;
    if values.len() > MAX_AGENT_SETTINGS_ENTRIES {
        return Err("disabledAgentIds 条目过多".to_owned());
    }
    let mut result = BTreeSet::new();
    for value in values {
        let id = value
            .as_str()
            .ok_or_else(|| "disabledAgentIds 只能包含字符串".to_owned())?;
        let id = normalize_component(id, "disabledAgentIds")?;
        if result
            .iter()
            .any(|stored: &String| stored.eq_ignore_ascii_case(&id))
        {
            return Err("disabledAgentIds 不能包含重复 ID".to_owned());
        }
        result.insert(id);
    }
    Ok(result)
}

fn parse_overrides(
    value: Option<&Value>,
    plugin_ids: bool,
) -> Result<BTreeMap<String, AgentModelSelectionOverride>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "模型选择覆盖必须是对象".to_owned())?;
    if object.len() > MAX_AGENT_SETTINGS_ENTRIES {
        return Err("模型选择覆盖条目过多".to_owned());
    }
    let mut result = BTreeMap::new();
    for (key, value) in object {
        let key = normalize_component(key, "模型选择覆盖键")?;
        if plugin_ids && !key.starts_with("plugin:") {
            return Err("插件模型覆盖键必须使用 plugin: 命名空间".to_owned());
        }
        if plugin_ids && !is_stable_plugin_agent_id(&key) {
            return Err("插件模型覆盖键必须使用规范插件 Agent ID".to_owned());
        }
        if !plugin_ids && key.starts_with("plugin:") {
            return Err("内置模型覆盖键不能使用 plugin: 命名空间".to_owned());
        }
        let normalized = normalize_model_selection_value(value)?;
        if result
            .keys()
            .any(|stored: &String| stored.eq_ignore_ascii_case(&key))
        {
            return Err("模型选择覆盖键不能只以大小写区分".to_owned());
        }
        result.insert(key, normalized);
    }
    Ok(result)
}

fn find_override<'a>(
    overrides: &BTreeMap<String, AgentModelSelectionOverride>,
    candidate_ids: impl IntoIterator<Item = &'a str>,
) -> Option<AgentModelSelectionOverride> {
    candidate_ids.into_iter().find_map(|candidate| {
        overrides
            .iter()
            .find(|(stored, _)| stored.eq_ignore_ascii_case(candidate))
            .map(|(_, selection)| selection.clone())
    })
}

fn model_override_values(
    overrides: &BTreeMap<String, AgentModelSelectionOverride>,
) -> Map<String, Value> {
    overrides
        .iter()
        .map(|(key, value)| (key.clone(), model_selection_value(value)))
        .collect()
}

fn required_identifier(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value = object
        .get(key)
        .ok_or_else(|| format!("modelSelection 缺少 {key}"))?;
    let value = value
        .as_str()
        .ok_or_else(|| format!("modelSelection.{key} 必须是字符串"))?;
    normalize_component(value, key)
}

fn normalize_component(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_AGENT_ID_BYTES
        || value.chars().any(char::is_control)
        || value.contains("::")
    {
        return Err(format!("{label} 为空、过长或包含控制字符"));
    }
    Ok(value.to_owned())
}

fn is_stable_plugin_agent_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("plugin:") else {
        return false;
    };
    let Some((plugin_marketplace, agent)) = rest.rsplit_once(':') else {
        return false;
    };
    let Some((plugin, marketplace)) = plugin_marketplace.split_once('@') else {
        return false;
    };
    !plugin.is_empty()
        && !marketplace.is_empty()
        && !agent.is_empty()
        && !plugin.contains(':')
        && !plugin.contains('@')
        && !marketplace.contains(':')
        && !marketplace.contains('@')
        && !agent.contains(':')
        && !agent.contains('@')
}

fn normalize_reasoning_level(value: &str) -> Result<String, String> {
    let value = value.trim();
    if ALLOWED_REASONING_LEVELS.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err("modelSelection.options.reasoningLevel 不是受支持的等级".to_owned())
    }
}

fn reject_unknown_keys<const N: usize>(
    object: &Map<String, Value>,
    allowed: [&str; N],
) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Agent 设置包含未知字段".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_none_and_empty_options_are_preserved() {
        let selection = normalize_model_selection_value(&json!({
            "providerId": "provider",
            "modelId": "model",
            "options": {"reasoningLevel": "none"},
        }))
        .expect("none 应是合法显式选择");
        assert_eq!(selection.reasoning_effort.as_deref(), Some("none"));
        assert_eq!(
            model_selection_value(&selection),
            json!({
                "providerId": "provider",
                "modelId": "model",
                "options": {"reasoningLevel": "none"},
            })
        );

        let inherited = normalize_model_selection_value(&json!({
            "providerId": "provider",
            "modelId": "model",
            "options": {},
        }))
        .expect("空 options 应表示未设置推理等级");
        assert_eq!(inherited.reasoning_effort, None);
        assert_eq!(model_selection_value(&inherited)["options"], Value::Null);
    }

    #[test]
    fn state_rejects_unknown_fields_and_plugin_id_mixups() {
        assert!(AgentSettingsState::from_value(json!({"unknown": true})).is_err());
        assert!(
            AgentSettingsState::from_value(json!({
                "pluginAgentModelSelectionOverrides": {
                    "user:user:agent": {"providerId":"p", "modelId":"m"}
                }
            }))
            .is_err()
        );
        assert!(
            AgentSettingsState::from_value(json!({
                "builtInModelSelectionOverrides": {
                    "plugin:agent@market:one": {"providerId":"p", "modelId":"m"}
                }
            }))
            .is_err()
        );
    }

    #[test]
    fn plugin_id_uses_single_stable_namespace() {
        let plugin =
            PluginId::from_components("review-tools", Some("official")).expect("插件身份应合法");
        assert_eq!(
            plugin_agent_id(&plugin, "reviewer").expect("插件 Agent ID 应合法"),
            "plugin:review-tools@official:reviewer"
        );
        assert_eq!(
            plugin_agent_id_from_runtime_name("plugin:official:review-tools:reviewer"),
            Some("plugin:review-tools@official:reviewer".to_owned())
        );
    }

    #[test]
    fn disabled_and_override_state_changes_fingerprint() {
        let empty = AgentSettingsState::from_value(json!({})).expect("空设置应合法");
        let configured = AgentSettingsState::from_value(json!({
            "disabledAgentIds": ["built-in:reviewer"],
            "builtInModelSelectionOverrides": {
                "reviewer": {
                    "providerId": "provider",
                    "modelId": "model",
                    "options": {"reasoningLevel": "none"}
                }
            }
        }))
        .expect("禁用与模型覆盖应合法");

        assert!(configured.is_disabled(["built-in:reviewer"]));
        assert_eq!(
            configured
                .built_in_override(["reviewer"])
                .expect("应找到内置 Agent 覆盖")
                .reasoning_effort
                .as_deref(),
            Some("none")
        );
        assert_ne!(
            empty.fingerprint_material(),
            configured.fingerprint_material()
        );
    }
}
