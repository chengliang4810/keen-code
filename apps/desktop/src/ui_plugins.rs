//! 原输入框插件引用的宿主验证；资源身份随 ACP 消息进入 Journal，用户正文不改写。
use crate::extensions::{ExtensionsState, PluginDto};
use keencode_model::InputReference;
use std::{path::Path, sync::OnceLock};
use tauri::{AppHandle, Manager, State};

/// 原输入框允许裸名和带引号的 URI；完整 token 匹配避免 @proof-other 冒充 @proof。
fn prompt_mentions(text: &str, keys: &[&str]) -> bool {
    static TOKENS: OnceLock<regex::Regex> = OnceLock::new();
    let tokens = TOKENS.get_or_init(|| {
        regex::Regex::new(r#"(?:^|\s)@(?:"((?:\\.|[^"\\])*)"|([^\s@]+))"#)
            .expect("固定引用 token 正则有效")
    });
    tokens.captures_iter(text).any(|capture| {
        let whole = capture.get(0).expect("正则包含完整匹配");
        if text[whole.end()..]
            .chars()
            .next()
            .is_some_and(|next| !next.is_whitespace())
        {
            return false;
        }
        let token = capture
            .get(1)
            .or_else(|| capture.get(2))
            .expect("引用 token 有内容")
            .as_str();
        keys.iter().any(|key| token.eq_ignore_ascii_case(key))
    })
}

/// 名称、URI、当前安装及正文引用必须同时一致；普通文件/会话引用不能当作插件接受。
fn selected_references(
    text: &str,
    references: &[InputReference],
    catalog: &[PluginDto],
) -> Result<Vec<InputReference>, String> {
    if text.len() > 4 * 1024 * 1024 {
        return Err("插件消息正文超过限制".into());
    }
    InputReference::validate_all(references).map_err(|error| error.to_string())?;
    for reference in references {
        let identity = reference
            .path
            .strip_prefix("plugin://")
            .ok_or("当前只支持插件资源引用")?;
        let id = crate::plugins::PluginId::parse(identity).map_err(|error| error.to_string())?;
        if id.marketplace.is_none() || id.to_string() != identity || id.plugin != reference.name {
            return Err("插件引用缺少准确的市场身份".into());
        }
        let plugin = catalog
            .iter()
            .find(|plugin| plugin.name == identity)
            .ok_or("所选插件已失效，请重新选择")?;
        if !plugin.enabled {
            return Err("所选插件已禁用，请重新选择".into());
        }
        if !prompt_mentions(text, &[&reference.name, identity, &reference.path]) {
            return Err("所选插件缺少原消息中的明确引用".into());
        }
    }
    Ok(references.to_vec())
}

/// Session admission 和实际启动均重新验证，页面缓存不是启用状态的事实源。
pub(crate) fn validate_for_session(
    app: &AppHandle,
    cwd: &Path,
    text: &str,
    references: &[InputReference],
) -> Result<(), String> {
    if references.is_empty() {
        return Ok(());
    }
    let catalog = crate::extensions::plugins_list(
        Some(cwd.to_string_lossy().into_owned()),
        app.clone(),
        app.state::<ExtensionsState>(),
    )?;
    selected_references(text, references, &catalog.plugins).map(|_| ())
}

/// 编辑重发在回退之前完成预检；真正发送仍由 ACP 再次检查，预检不授予后续访问权限。
#[tauri::command(async)]
pub fn ui_plugin_references_validate(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    cwd: String,
    text: String,
    references: Vec<InputReference>,
) -> Result<Vec<InputReference>, String> {
    let catalog = crate::extensions::plugins_list(Some(cwd), app, state)?;
    selected_references(&text, &references, &catalog.plugins)
}

/// 尚未接通的 detached 边界明确拒绝扩展，不能只丢弃字段后继续执行。
pub(crate) fn reject_unsupported_references(
    meta: Option<&keencode_acp::schema::Meta>,
) -> Result<(), String> {
    if meta.is_some_and(|meta| meta.contains_key("keencode/messageReferences")) {
        return Err("此请求尚不支持用户资源引用".into());
    }
    Ok(())
}

/// 解码有界的权威消息身份；结构非法的扩展不能退化为无引用发送。
pub(crate) fn decode_references(
    meta: Option<&keencode_acp::schema::Meta>,
) -> Result<Vec<InputReference>, String> {
    let Some(value) = meta.and_then(|meta| meta.get("keencode/messageReferences")) else {
        return Ok(Vec::new());
    };
    let references: Vec<InputReference> =
        serde_json::from_value(value.clone()).map_err(|_| "消息引用元数据无效")?;
    InputReference::validate_all(&references).map_err(|error| error.to_string())?;
    Ok(references)
}

#[cfg(test)]
mod tests;
