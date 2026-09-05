use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::{
    atomic_write_settings, external_plugin_cache_dir, get_marketplace_manifest, match_project_path,
    plugin_storage_component, remove_from_enabled_plugins, InstallerError, PluginUpdateInfo,
};
use crate::plugin::{
    config::{load_installed_plugins, save_installed_plugins},
    types::{InstalledPlugins, PluginId},
};

pub async fn uninstall_plugin(
    plugin_id: &str,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let plugin_id = PluginId::parse(plugin_id)?;
    let name = plugin_id.plugin.clone();
    let marketplace = plugin_id.require_marketplace()?.to_owned();

    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path))?;

    let entry = installed
        .plugins
        .iter()
        .find(|p| {
            PluginId::parse(&p.id).is_ok_and(|candidate| candidate == plugin_id)
                && match_project_path(&p.project_path, project_dir)
        })
        .ok_or(InstallerError::PluginNotFound { name, marketplace })?;

    let install_path = entry.install_path.clone();
    let scope = entry.scope;
    let is_external = entry.origin.is_external();

    let is_last_scope = !installed.plugins.iter().any(|p| {
        PluginId::parse(&p.id).is_ok_and(|candidate| candidate == plugin_id)
            && (p.scope != scope
                || (p.scope == scope && !match_project_path(&p.project_path, project_dir)))
    });

    installed.plugins.retain(|p| {
        !(PluginId::parse(&p.id).is_ok_and(|candidate| candidate == plugin_id)
            && p.scope == scope
            && match_project_path(&p.project_path, project_dir))
    });
    save_installed_plugins(&installed, Some(&plugins_path))?;

    remove_from_enabled_plugins(&plugin_id, &scope, claude_dir, project_dir)?;

    if is_last_scope {
        // 仅 Peri 安装的插件才清理自己创建的 external/data/cache 内容；
        // Claude 外部记录不应被本模块接管。
        if !is_external {
            let external_dir = external_plugin_cache_dir(claude_dir, &plugin_id);
            if external_dir.exists() {
                tokio::fs::remove_dir_all(&external_dir).await.ok();
            }

            let data_dir = claude_dir
                .join("plugins")
                .join("data")
                .join(plugin_storage_component(&plugin_id));
            if data_dir.exists() {
                tokio::fs::remove_dir_all(&data_dir).await.ok();
            }

            remove_plugin_options(&plugin_id, claude_dir)?;

            let _ = mark_orphaned(&install_path).await;
        }
    }

    Ok(())
}

/// 标记插件版本为孤儿（延迟删除）
async fn mark_orphaned(install_path: &Path) -> Result<(), InstallerError> {
    if !install_path.exists() {
        return Ok(());
    }

    tokio::task::spawn_blocking({
        let path = install_path.to_path_buf();
        move || {
            let orphaned_file = path.join(".orphaned_at");
            let _ = std::fs::write(&orphaned_file, chrono::Utc::now().to_rfc3339());
            Ok::<(), InstallerError>(())
        }
    })
    .await
    .map_err(|e| InstallerError::SettingsError(format!("spawn_blocking 失败: {e}")))?
}

/// 从 settings.json 删除插件配置选项
fn remove_plugin_options(plugin_id: &PluginId, claude_dir: &Path) -> Result<(), InstallerError> {
    let settings_path = claude_dir.join("settings.json");
    if !settings_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    if let Some(obj) = value.as_object_mut() {
        if let Some(configs) = obj.get_mut("pluginConfigs").and_then(|v| v.as_object_mut()) {
            let keys_to_remove: Vec<String> = configs
                .keys()
                .filter(|key| PluginId::parse(key).is_ok_and(|candidate| candidate == *plugin_id))
                .cloned()
                .collect();
            for key in keys_to_remove {
                configs.remove(&key);
            }
        }

        atomic_write_settings(&settings_path, &value)?;
    }

    Ok(())
}

pub async fn check_updates(
    installed: &InstalledPlugins,
    marketplace_cache_dir: &Path,
) -> Vec<PluginUpdateInfo> {
    let mut manifest_cache: HashMap<String, crate::plugin::types::MarketplaceManifest> =
        HashMap::new();
    let mut result = Vec::new();

    for plugin in &installed.plugins {
        let Ok(plugin_id) = PluginId::parse(&plugin.id) else {
            continue;
        };
        let Ok(marketplace) = plugin_id.require_marketplace() else {
            continue;
        };

        if !manifest_cache.contains_key(marketplace) {
            if let Ok(manifest) = get_marketplace_manifest(marketplace, marketplace_cache_dir) {
                manifest_cache.insert(marketplace.to_owned(), manifest);
            } else {
                continue;
            }
        }

        let manifest = &manifest_cache[marketplace];
        if let Some(latest) = manifest.plugins.iter().find(|candidate| {
            PluginId::from_components(candidate.name.as_str(), Some(marketplace))
                .is_ok_and(|candidate_id| candidate_id == plugin_id)
        }) {
            let latest_version = latest
                .sha
                .as_ref()
                .map(|s| s.chars().take(7).collect::<String>())
                .unwrap_or_else(|| latest.version.clone());

            if latest_version != plugin.version {
                result.push(PluginUpdateInfo {
                    plugin_id: plugin.id.clone(),
                    current_version: plugin.version.clone(),
                    latest_version,
                });
            }
        }
    }

    result
}

/// 清理 `cache/<完整 PluginId 安全键>/<version>` 下超过 7 天未使用的孤儿版本。
pub async fn cleanup_orphaned_plugins(claude_dir: &Path) -> Result<usize, InstallerError> {
    const CLEANUP_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000; // 7 天

    let cache_dir = claude_dir.join("plugins").join("cache");
    if !cache_dir.exists() {
        return Ok(0);
    }

    let installed = load_installed_plugins(Some(
        &claude_dir.join("plugins").join("installed_plugins.json"),
    ))?;
    let installed_paths: std::collections::HashSet<PathBuf> = installed
        .plugins
        .iter()
        .map(|p| p.install_path.clone())
        .collect();

    let now = chrono::Utc::now().timestamp_millis();
    let mut deleted_count = 0;

    let mut entries = tokio::fs::read_dir(&cache_dir)
        .await
        .map_err(|e| InstallerError::SettingsError(format!("读取 cache 目录失败: {e}")))?;

    let mut tasks = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| InstallerError::SettingsError(format!("读取目录条目失败: {e}")))?
    {
        if !entry.file_type().await?.is_dir() {
            continue;
        }

        let plugin_cache_path = entry.path();
        let installed_paths_clone = installed_paths.clone();

        let task = tokio::task::spawn_blocking(move || {
            let mut count = 0;

            if let Ok(version_entries) = std::fs::read_dir(&plugin_cache_path) {
                for version_entry in version_entries.flatten() {
                    if !version_entry.file_type()?.is_dir() {
                        continue;
                    }

                    let version_path = version_entry.path();

                    if installed_paths_clone.contains(&version_path) {
                        let _ = std::fs::remove_file(version_path.join(".orphaned_at"));
                        continue;
                    }

                    let orphaned_file = version_path.join(".orphaned_at");
                    if let Ok(metadata) = std::fs::metadata(&orphaned_file) {
                        if let Ok(modified) = metadata.modified() {
                            let age_ms = now
                                - modified
                                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as i64;

                            if age_ms > CLEANUP_AGE_MS
                                && std::fs::remove_dir_all(&version_path).is_ok()
                            {
                                count += 1;
                            }
                        }
                    }
                }

                if plugin_cache_path.read_dir()?.count() == 0 {
                    let _ = std::fs::remove_dir(&plugin_cache_path);
                }
            }

            Ok::<usize, InstallerError>(count)
        });

        tasks.push(task);
    }

    for task in tasks {
        if let Ok(Ok(count)) = task.await {
            deleted_count += count;
        }
    }

    Ok(deleted_count)
}
