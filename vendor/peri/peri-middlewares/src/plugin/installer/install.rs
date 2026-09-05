use std::path::Path;

use super::{
    copy_dir_recursive, generate_synthetic_manifest, get_marketplace_manifest, match_project_path,
    resolve_marketplace_source_dir, resolve_marketplace_source_path, update_enabled_plugins,
    InstallerError,
};
use crate::plugin::{
    config::{load_installed_plugins, load_plugin_manifest, save_installed_plugins},
    types::{InstallScope, InstalledPlugin, PluginId, PluginOrigin},
};

/// 安装 marketplace 中声明的插件，并保留直接执行 Git 的参数语义。
pub async fn install_plugin(
    name: &str,
    marketplace: &str,
    scope: InstallScope,
    marketplace_cache_dir: &Path,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<InstalledPlugin, InstallerError> {
    let plugin_id = PluginId::from_components(name, Some(marketplace))?;
    let name = plugin_id.plugin.clone();
    let marketplace = plugin_id.require_marketplace()?.to_owned();
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path))?;

    let manifest = get_marketplace_manifest(&marketplace, marketplace_cache_dir)?;

    let marketplace_plugin = manifest
        .plugins
        .into_iter()
        .find(|p| {
            PluginId::from_components(&p.name, Some(&marketplace))
                .is_ok_and(|candidate| candidate == plugin_id)
        })
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.clone(),
            marketplace: marketplace.clone(),
        })?;

    let marketplace_root = crate::plugin::marketplace::marketplace_cache_dir_for_namespace(
        marketplace_cache_dir,
        &marketplace,
    )
    .map_err(InstallerError::SettingsError)?;
    let source_dir = {
        if let Some(obj) = marketplace_plugin.source.as_object() {
            if obj.get("source").and_then(|v| v.as_str()) == Some("url") {
                let url = obj.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    InstallerError::SettingsError("URL 源缺少 url 字段".to_string())
                })?;

                let external_cache = super::external_plugin_cache_dir(claude_dir, &plugin_id);

                if !crate::plugin::marketplace::git_checkout_is_valid(&external_cache) {
                    crate::plugin::marketplace::clone_git_checkout(
                        url,
                        &external_cache,
                        crate::plugin::marketplace::git_checkout_is_valid,
                    )
                    .await
                    .map_err(InstallerError::SettingsError)?;
                }

                // 旧版本或外部中断可能留下空目录；它不能被当成成功的
                // checkout，否则后续会把空目录误安装为插件。
                if !external_cache.is_dir() || !external_cache.join(".git").exists() {
                    return Err(InstallerError::SettingsError(
                        "外部插件缓存不完整，缺少 Git checkout".into(),
                    ));
                }

                external_cache
            } else {
                return Err(InstallerError::SettingsError(
                    "不支持的 source 对象格式".to_string(),
                ));
            }
        } else {
            let raw = marketplace_plugin.source.as_str().ok_or_else(|| {
                InstallerError::SettingsError(
                    "marketplace 插件 source 必须是字符串路径或 URL 对象".into(),
                )
            })?;
            let normalized = resolve_marketplace_source_path(raw)?;
            let candidate = marketplace_root.join(&normalized);
            if !candidate.exists() {
                return Err(InstallerError::PluginNotFound {
                    name: name.clone(),
                    marketplace: marketplace.clone(),
                });
            }
            resolve_marketplace_source_dir(&marketplace_root, &normalized)?
        }
    };
    if !source_dir.exists() {
        return Err(InstallerError::PluginNotFound {
            name: name.clone(),
            marketplace: marketplace.clone(),
        });
    }
    let manifest_path = source_dir.join(".claude-plugin").join("plugin.json");
    let has_native_manifest = match std::fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(InstallerError::SettingsError(
                "marketplace 插件清单不能是符号链接".into(),
            ));
        }
        Ok(metadata) if metadata.is_file() => {
            load_plugin_manifest(&source_dir)?;
            true
        }
        Ok(_) => {
            return Err(InstallerError::SettingsError(
                "marketplace 插件清单必须是普通文件".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(InstallerError::SettingsError(format!(
                "读取 marketplace 插件清单失败：{error}"
            )))
        }
    };

    let version = marketplace_plugin
        .sha
        .as_ref()
        .map(|s| s.chars().take(7).collect())
        .unwrap_or_else(|| {
            let v = marketplace_plugin.version.clone();
            if v.is_empty() {
                chrono::Utc::now().format("%Y%m%d%H%M%S").to_string()
            } else {
                v
            }
        });
    let version_storage =
        crate::plugin::marketplace::bounded_storage_component(&version, "plugin-version");

    let target_dir = claude_dir
        .join("plugins")
        .join("cache")
        // 统一使用完整 PluginId 的大小写无关安全存储键，避免 marketplace/name
        // 分段在 Windows 上发生大小写或替换碰撞。
        .join(super::plugin_storage_component(&plugin_id))
        .join(&version_storage);

    tokio::task::spawn_blocking({
        let source_dir = source_dir.clone();
        let target_dir = target_dir.clone();
        move || -> Result<(), InstallerError> {
            if target_dir.exists() {
                let _ = std::fs::remove_dir_all(&target_dir);
            }
            std::fs::create_dir_all(&target_dir)?;
            copy_dir_recursive(&source_dir, &target_dir).map_err(|e| {
                InstallerError::CopyFailed {
                    src: source_dir.clone(),
                    dst: target_dir.clone(),
                    source: e,
                }
            })?;

            if !has_native_manifest {
                generate_synthetic_manifest(&target_dir, &marketplace_plugin)?;
            }

            Ok(())
        }
    })
    .await
    .map_err(|e| InstallerError::SettingsError(format!("spawn_blocking 失败: {e}")))??;

    let plugin_id_text = plugin_id.to_string();
    let project_path = project_dir.and_then(|p| p.to_str()).map(|s| s.to_string());
    let installed_plugin = InstalledPlugin {
        id: plugin_id_text.clone(),
        name,
        version,
        marketplace,
        install_path: target_dir,
        scope,
        project_path,
        origin: PluginOrigin::PeriInstalled,
    };

    installed.plugins.retain(|p| {
        !(PluginId::parse(&p.id).is_ok_and(|candidate| candidate == plugin_id)
            && p.scope == scope
            && match_project_path(&p.project_path, project_dir))
    });
    installed.plugins.push(installed_plugin.clone());
    save_installed_plugins(&installed, Some(&plugins_path))?;

    update_enabled_plugins(&plugin_id, scope, claude_dir, project_dir)?;

    Ok(installed_plugin)
}

pub async fn update_plugin(
    plugin_id: &str,
    marketplace_cache_dir: &Path,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<InstalledPlugin, InstallerError> {
    let parsed_id = PluginId::parse(plugin_id)?;
    let name = parsed_id.plugin.clone();
    let marketplace = parsed_id.require_marketplace()?.to_owned();
    let plugin_id = parsed_id.to_string();

    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let installed = load_installed_plugins(Some(&plugins_path))?;
    let current = installed
        .plugins
        .iter()
        .find(|p| PluginId::parse(&p.id).is_ok_and(|candidate| candidate == parsed_id))
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.clone(),
            marketplace: marketplace.clone(),
        })?;

    let manifest = get_marketplace_manifest(&marketplace, marketplace_cache_dir)?;
    let latest = manifest
        .plugins
        .iter()
        .find(|p| {
            PluginId::from_components(&p.name, Some(&marketplace))
                .is_ok_and(|candidate| candidate == parsed_id)
        })
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.clone(),
            marketplace: marketplace.clone(),
        })?;

    let latest_version = latest
        .sha
        .as_ref()
        .map(|s| s.chars().take(7).collect::<String>())
        .unwrap_or_else(|| latest.version.clone());

    if latest_version == current.version {
        return Ok(current.clone());
    }

    super::uninstall::uninstall_plugin(&plugin_id, claude_dir, project_dir).await?;
    install_plugin(
        &name,
        &marketplace,
        current.scope,
        marketplace_cache_dir,
        claude_dir,
        project_dir,
    )
    .await
}
