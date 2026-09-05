use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::atomic_file::{atomic_replace_private, AtomicFileError};
#[cfg(test)]
use crate::plugin::config::{load_installed_plugins, save_installed_plugins};
use crate::plugin::types::{InstallScope, PluginId};
#[cfg(test)]
use crate::plugin::types::{InstalledPlugin, InstalledPlugins};
use crate::plugin::{marketplace::read_manifest_from_path, PluginConfigError};

mod install;
mod uninstall;

pub use install::{install_plugin, update_plugin};
pub use uninstall::{check_updates, cleanup_orphaned_plugins, uninstall_plugin};

// ─── Plugin Discovery ──────────────────────────────────────────────────

/// 在所有已知 marketplace 中查找插件，返回找到的 marketplace 名称。
///
/// 当 `plugin install <name>` 未指定 @marketplace 时使用此函数进行自动发现。
pub fn find_plugin_in_marketplaces(
    plugin_name: &str,
    marketplace_cache_dir: &Path,
) -> Result<String, InstallerError> {
    let known = crate::plugin::config::load_known_marketplaces(None)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))?;

    for mkt in &known {
        let mkt_name = crate::plugin::marketplace::MarketplaceManager::extract_name(&mkt.source);
        match get_marketplace_manifest(&mkt_name, marketplace_cache_dir) {
            Ok(manifest) => {
                if manifest.plugins.iter().any(|plugin| {
                    crate::plugin::marketplace::plugin_names_equal(&plugin.name, plugin_name)
                }) {
                    return Ok(mkt_name);
                }
            }
            Err(_) => continue, // 该 marketplace 不可用，跳过
        }
    }
    Err(InstallerError::PluginNotFound {
        name: plugin_name.into(),
        marketplace: "所有已配置的 marketplace".into(),
    })
}

// ─── Error & Types ────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum InstallerError {
    /// 插件 ID 不符合共享契约。
    #[error("{0}")]
    InvalidPluginId(#[from] peri_acp_types::plugin::PluginIdError),
    #[error("插件未找到: {name} (marketplace: {marketplace})")]
    PluginNotFound { name: String, marketplace: String },
    #[error("复制失败: {src} -> {dst}")]
    CopyFailed {
        src: PathBuf,
        dst: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("配置错误: {0}")]
    ConfigError(#[from] PluginConfigError),
    #[error("Settings 错误: {0}")]
    SettingsError(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct PluginUpdateInfo {
    pub plugin_id: String,
    pub current_version: String,
    pub latest_version: String,
}

// ─── Utility Functions ────────────────────────────────────────────────

/// 递归复制插件目录，并拒绝复制过程中的任何符号链接或特殊文件。
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let source_metadata = std::fs::symlink_metadata(src)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("插件来源必须是普通目录：{}", src.display()),
        ));
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        let metadata = std::fs::symlink_metadata(&src_path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("插件来源不能包含符号链接：{}", src_path.display()),
            ));
        }
        if file_name == ".git" {
            continue;
        }
        if metadata.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("插件来源不能包含特殊文件：{}", src_path.display()),
            ));
        }
    }
    Ok(())
}

/// 从 marketplace 条目生成合成 plugin.json（用于无原生 manifest 的 LSP/MCP 插件）
pub(crate) fn generate_synthetic_manifest(
    target_dir: &Path,
    marketplace_plugin: &crate::plugin::types::MarketplacePlugin,
) -> Result<(), std::io::Error> {
    let mut manifest = serde_json::Map::new();
    manifest.insert("name".into(), serde_json::json!(marketplace_plugin.name));
    if !marketplace_plugin.version.is_empty() {
        manifest.insert(
            "version".into(),
            serde_json::json!(marketplace_plugin.version),
        );
    }
    if !marketplace_plugin.description.is_empty() {
        manifest.insert(
            "description".into(),
            serde_json::json!(marketplace_plugin.description),
        );
    }
    if let Some(ref author) = marketplace_plugin.author {
        if let Ok(val) = serde_json::to_value(author) {
            manifest.insert("author".into(), val);
        }
    }

    if let Some(lsp_servers) = marketplace_plugin.extra.get("lspServers") {
        if let Some(map) = lsp_servers.as_object() {
            let entries: Vec<serde_json::Value> = map
                .iter()
                .map(|(server_name, config)| {
                    let mut entry = config.clone();
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert("name".into(), serde_json::json!(server_name));
                    }
                    entry
                })
                .collect();
            if !entries.is_empty() {
                manifest.insert("lspServers".into(), serde_json::json!(entries));
            }
        }
    }

    if let Some(mcp_servers) = marketplace_plugin.extra.get("mcpServers") {
        manifest.insert("mcpServers".into(), mcp_servers.clone());
    }

    let claude_plugin_dir = target_dir.join(".claude-plugin");
    std::fs::create_dir_all(&claude_plugin_dir)?;
    let manifest_path = claude_plugin_dir.join("plugin.json");
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, json)?;

    Ok(())
}

pub(crate) fn get_marketplace_manifest(
    marketplace: &str,
    marketplace_cache_dir: &Path,
) -> Result<crate::plugin::types::MarketplaceManifest, InstallerError> {
    // 统一使用缓存 helper，拒绝路径逃逸并编码包含路径段的 marketplace 名称。
    let path = crate::plugin::marketplace::marketplace_cache_dir_for_namespace(
        marketplace_cache_dir,
        marketplace,
    )
    .map_err(InstallerError::SettingsError)?;
    let manifest_path =
        crate::plugin::marketplace::find_marketplace_json(&path).ok_or_else(|| {
            InstallerError::PluginNotFound {
                name: String::new(),
                marketplace: marketplace.into(),
            }
        })?;
    read_manifest_from_path(&manifest_path)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))
}

/// 按共享 PluginId 契约匹配 enabledPlugins 中的键，忽略 ASCII 大小写差异。
fn enabled_plugin_key_matches(key: &str, plugin_id: &PluginId) -> bool {
    PluginId::parse(key).is_ok_and(|candidate| candidate == *plugin_id)
}

pub(crate) fn atomic_write_settings(
    path: &Path,
    value: &serde_json::Value,
) -> Result<(), InstallerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))?;
    atomic_replace_private(path, json.as_bytes()).map_err(|error| match error {
        AtomicFileError::Replace(error) => {
            InstallerError::SettingsError(format!("rename 失败: {error}"))
        }
        error => InstallerError::Io(error.into_io_error()),
    })?;
    Ok(())
}

/// 将 PluginId 的稳定存储名限制为一个跨平台安全的路径组件。
pub(crate) fn plugin_storage_component(plugin_id: &PluginId) -> String {
    crate::plugin::marketplace::bounded_storage_component(
        &plugin_id.storage_component(),
        "plugin-id",
    )
}

/// 返回 URL 来源插件的共享缓存目录；插件名与 marketplace 均参与隔离。
pub(crate) fn external_plugin_cache_dir(claude_dir: &Path, plugin_id: &PluginId) -> PathBuf {
    claude_dir
        .join("plugins")
        .join("external")
        .join(plugin_storage_component(plugin_id))
}

/// 解析 marketplace manifest 中的安全相对来源路径；`.` 与 `./` 表示市场根目录。
pub(crate) fn resolve_marketplace_source_path(raw: &str) -> Result<PathBuf, InstallerError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\\') {
        return Err(InstallerError::SettingsError(
            "marketplace 插件 source 必须是非空安全相对路径".into(),
        ));
    }
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' && raw.as_bytes()[0].is_ascii_alphabetic() {
        return Err(InstallerError::SettingsError(
            "marketplace 插件 source 不能包含 Windows 路径前缀".into(),
        ));
    }

    // Claude Code marketplace 清单普遍使用 `./plugins/foo`；去掉唯一允许的
    // 前导当前目录段后再逐段校验，仍拒绝中间或重复的 `.` 路径段。
    if matches!(raw, "." | "./") {
        return Ok(PathBuf::new());
    }
    let raw = raw.strip_prefix("./").unwrap_or(raw);
    let mut normalized = PathBuf::new();
    let mut has_normal = false;
    for component in Path::new(raw).components() {
        match component {
            std::path::Component::Normal(segment) => {
                normalized.push(segment);
                has_normal = true;
            }
            std::path::Component::CurDir => {
                return Err(InstallerError::SettingsError(
                    "marketplace 插件 source 不能包含 '.' 路径段".into(),
                ));
            }
            std::path::Component::ParentDir => {
                return Err(InstallerError::SettingsError(
                    "marketplace 插件 source 不能包含 '..' 路径段".into(),
                ));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(InstallerError::SettingsError(
                    "marketplace 插件 source 必须是相对路径".into(),
                ));
            }
        }
    }
    if !has_normal {
        return Err(InstallerError::SettingsError(
            "marketplace 插件 source 必须至少包含一个普通路径段".into(),
        ));
    }
    Ok(normalized)
}

/// 将市场内的相对 source 解析为 canonical 路径，并拒绝路径上的任何符号链接。
pub(crate) fn resolve_marketplace_source_dir(
    marketplace_root: &Path,
    relative: &Path,
) -> Result<PathBuf, InstallerError> {
    let root_metadata = std::fs::symlink_metadata(marketplace_root).map_err(|error| {
        InstallerError::SettingsError(format!(
            "读取 marketplace 根目录失败 '{}': {error}",
            marketplace_root.display()
        ))
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(InstallerError::SettingsError(format!(
            "marketplace 根目录必须是普通目录：{}",
            marketplace_root.display()
        )));
    }

    let canonical_root = marketplace_root.canonicalize().map_err(|error| {
        InstallerError::SettingsError(format!(
            "无法规范化 marketplace 根目录 '{}': {error}",
            marketplace_root.display()
        ))
    })?;
    let mut current = marketplace_root.to_path_buf();

    for component in relative.components() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => current.push(name),
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir => {
                return Err(InstallerError::SettingsError(
                    "marketplace 插件 source 必须是安全相对路径".into(),
                ));
            }
        }

        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            InstallerError::SettingsError(format!(
                "读取 marketplace 插件路径失败 '{}': {error}",
                current.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(InstallerError::SettingsError(format!(
                "marketplace 插件 source 不能包含符号链接：{}",
                current.display()
            )));
        }
    }

    let canonical = current.canonicalize().map_err(|error| {
        InstallerError::SettingsError(format!(
            "无法规范化 marketplace 插件路径 '{}': {error}",
            current.display()
        ))
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(InstallerError::SettingsError(
            "marketplace 插件 source 必须位于市场根目录内".into(),
        ));
    }
    Ok(canonical)
}

pub fn update_enabled_plugins(
    plugin_id: &PluginId,
    scope: InstallScope,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let settings_path = match scope {
        InstallScope::User => claude_dir.join("settings.json"),
        InstallScope::Project => {
            if let Some(pd) = project_dir {
                pd.join(".claude").join("settings.json")
            } else {
                claude_dir.join("settings.json")
            }
        }
        InstallScope::Local => claude_dir.join("settings.json"),
    };

    let mut value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    let obj = value.as_object_mut().unwrap();
    let enabled = obj
        .entry("enabledPlugins")
        .or_insert(serde_json::Value::Object(serde_json::Map::new()));

    let enabled_map = if let Some(arr) = enabled.as_array() {
        let map: serde_json::Map<String, serde_json::Value> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| (s.to_string(), serde_json::Value::Bool(true)))
            .collect();
        *enabled = serde_json::Value::Object(map.clone());
        map
    } else {
        enabled.as_object().cloned().unwrap_or_default()
    };

    let plugin_id_text = plugin_id.to_string();
    let already_enabled = enabled_map
        .keys()
        .any(|key| enabled_plugin_key_matches(key, plugin_id));
    if !already_enabled {
        if let Some(obj) = enabled.as_object_mut() {
            obj.insert(plugin_id_text, serde_json::Value::Bool(true));
        }
    }

    atomic_write_settings(&settings_path, &value)
}

pub fn remove_from_enabled_plugins(
    plugin_id: &PluginId,
    scope: &InstallScope,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let settings_path = match scope {
        InstallScope::User => claude_dir.join("settings.json"),
        InstallScope::Project => {
            if let Some(pd) = project_dir {
                pd.join(".claude").join("settings.json")
            } else {
                claude_dir.join("settings.json")
            }
        }
        InstallScope::Local => claude_dir.join("settings.json"),
    };

    if !settings_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| InstallerError::SettingsError(e.to_string()))?;
    if let Some(obj) = value.as_object_mut() {
        if let Some(enabled) = obj.get_mut("enabledPlugins") {
            if let Some(arr) = enabled.as_array_mut() {
                arr.retain(|value| {
                    value
                        .as_str()
                        .is_none_or(|key| !enabled_plugin_key_matches(key, plugin_id))
                });
            } else if let Some(map) = enabled.as_object_mut() {
                let keys_to_remove: Vec<String> = map
                    .keys()
                    .filter(|key| enabled_plugin_key_matches(key, plugin_id))
                    .cloned()
                    .collect();
                for key in keys_to_remove {
                    map.remove(&key);
                }
            }
        }
    }

    atomic_write_settings(&settings_path, &value)
}

/// 匹配 project_path：两者都为 None，或者路径字符串匹配
pub(crate) fn match_project_path(stored: &Option<String>, given: Option<&Path>) -> bool {
    match (stored, given) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(_), None) => false,
        (Some(s), Some(p)) => {
            let given_str = p.to_str().unwrap_or("");
            s == given_str || s.ends_with(given_str) || given_str.ends_with(s)
        }
    }
}

#[cfg(test)]
#[path = "installer_test.rs"]
mod tests;
