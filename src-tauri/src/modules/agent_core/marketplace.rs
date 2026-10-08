use super::extensions::{data_root, extension_secrets, EXTENSION_WRITES};
use super::security::deny_secret_path;
use rcode_plugins::{
    inspect_plugin_components, load_plugin_manifest, materialize_marketplace_source,
    refresh_marketplace_source, resolve_marketplace_plugin_install_plan_checked,
    validate_marketplace_source, InstalledPlugin, MarketplaceEntry, MarketplaceRegistry,
    MaterializedPlugin, PluginId, PluginInstallSource, PluginManager, UserConfigDefinition,
    UserConfigUpdate, OFFICIAL_MARKETPLACE_SOURCE,
};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, sync::Mutex};
use tauri::{AppHandle, Manager};

static MARKETPLACE_OPERATIONS: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSource {
    name: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailablePlugin {
    name: String,
    marketplace: String,
    description: Option<String>,
    version: Option<String>,
    category: Option<String>,
    keywords: Vec<String>,
    installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceCatalog {
    initialized: bool,
    sources: Vec<MarketplaceSource>,
    plugins: Vec<AvailablePlugin>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetails {
    plugin: InstalledPlugin,
    description: Option<String>,
    fields: BTreeMap<String, UserConfigDefinition>,
    components: BTreeMap<String, usize>,
}

fn check_source(app: &AppHandle, source: &str, require_authorization: bool) -> Result<(), String> {
    deny_secret_path(Path::new(source))?;
    if let Some(path) =
        validate_marketplace_source(source).map_err(|_| "市场来源无效，不能包含凭据或敏感路径")?
    {
        deny_secret_path(&path)?;
        let manifest = if path.is_dir() {
            crate::modules::storage::reject_link(&path.join(".claude-plugin"))?;
            path.join(rcode_plugins::MARKETPLACE_MANIFEST)
        } else {
            path.clone()
        };
        crate::modules::storage::reject_link(&manifest)?;
        deny_secret_path(&std::fs::canonicalize(&manifest).map_err(|_| "市场来源缺少有效清单")?)?;
        if require_authorization
            && !app
                .state::<crate::modules::workspace::WorkspaceRegistry>()
                .is_authorized(&path)
        {
            return Err("请先选择并授权市场来源目录".into());
        }
    }
    Ok(())
}

pub(super) fn validate_plugin_tree(root: &Path) -> Result<(), String> {
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    deny_secret_path(&root)?;
    let mut pending = vec![root];
    let mut count = 0;
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() == ".git" {
                continue;
            }
            count += 1;
            if count > 4096 {
                return Err("插件文件数量超过上限".into());
            }
            let path = entry.path();
            deny_secret_path(&path)?;
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err("插件目录不允许重解析点".into());
                }
            }
            if meta.file_type().is_symlink() || !(meta.is_dir() || meta.is_file()) {
                return Err("插件目录不允许链接或特殊文件".into());
            }
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn cache_root() -> Result<std::path::PathBuf, String> {
    crate::modules::storage::directory("plugins/marketplaces")
}

fn checked_market(
    app: &AppHandle,
    source: &str,
    refresh: bool,
) -> Result<rcode_plugins::MarketplaceMaterialization, String> {
    check_source(app, source, false)?;
    let market = if refresh {
        refresh_marketplace_source(source, &cache_root()?)
    } else {
        materialize_marketplace_source(source, &cache_root()?)
    }
    .map_err(|_| "市场加载失败，请检查来源地址和网络连接后重试")?;
    Ok(market)
}

#[tauri::command]
pub async fn agent_marketplace_catalog(app: AppHandle) -> Result<MarketplaceCatalog, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::modules::storage::path("plugins/marketplaces.json")?;
        let root = data_root(&app)?;
        let registry = MarketplaceRegistry::load(&root).map_err(|e| e.to_string())?;
        let installed = PluginManager::new(&root)
            .load_state()
            .map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        let mut plugins = Vec::new();
        for source in registry.sources {
            for plugin in source.plugins {
                plugins.push(AvailablePlugin {
                    installed: installed.plugins.iter().any(|entry| {
                        entry.id.plugin.eq_ignore_ascii_case(&plugin.name)
                            && entry
                                .id
                                .marketplace
                                .as_deref()
                                .is_some_and(|name| name.eq_ignore_ascii_case(&source.name))
                    }),
                    name: plugin.name,
                    marketplace: source.name.clone(),
                    description: plugin.description,
                    version: plugin.version,
                    category: plugin.category,
                    keywords: plugin.keywords,
                });
            }
            sources.push(MarketplaceSource {
                name: source.name,
                source: source.source,
            });
        }
        plugins.sort_by_key(|plugin| {
            (
                plugin.name.to_ascii_lowercase(),
                plugin.marketplace.to_ascii_lowercase(),
            )
        });
        Ok(MarketplaceCatalog {
            initialized: registry.initialized,
            sources,
            plugins,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_marketplace_add(app: AppHandle, source: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = MARKETPLACE_OPERATIONS
            .lock()
            .map_err(|_| "市场操作锁不可用")?;
        check_source(&app, source.trim(), true)?;
        let market = checked_market(&app, source.trim(), false)?;
        let entry = MarketplaceEntry::from_materialization(&market).map_err(|e| e.to_string())?;
        let _write = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        let root = data_root(&app)?;
        crate::modules::storage::path("plugins/marketplaces.json")?;
        let mut registry = MarketplaceRegistry::load(&root).map_err(|e| e.to_string())?;
        registry.insert(entry, false).map_err(|e| e.to_string())?;
        registry.save(&root).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_marketplace_remove(app: AppHandle, name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = MARKETPLACE_OPERATIONS
            .lock()
            .map_err(|_| "市场操作锁不可用")?;
        let _write = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        let root = data_root(&app)?;
        crate::modules::storage::path("plugins/marketplaces.json")?;
        let mut registry = MarketplaceRegistry::load(&root).map_err(|e| e.to_string())?;
        registry.remove(&name).map_err(|e| e.to_string())?;
        registry.save(&root).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_marketplace_refresh(
    app: AppHandle,
    name: Option<String>,
    restore_default: bool,
    initialize_only: Option<bool>,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = MARKETPLACE_OPERATIONS
            .lock()
            .map_err(|_| "市场操作锁不可用")?;
        let root = data_root(&app)?;
        crate::modules::storage::path("plugins/marketplaces.json")?;
        let mut registry = MarketplaceRegistry::load(&root).map_err(|e| e.to_string())?;
        if initialize_only.unwrap_or(false) && registry.initialized {
            return Ok(Vec::new());
        }
        let mut targets: Vec<(Option<String>, String)> = registry
            .sources
            .iter()
            .filter(|source| {
                name.as_ref()
                    .is_none_or(|name| source.name.eq_ignore_ascii_case(name))
            })
            .map(|source| (Some(source.name.clone()), source.source.clone()))
            .collect();
        if name.is_some() && targets.is_empty() {
            return Err("找不到市场来源".into());
        }
        if restore_default
            && name.is_none()
            && !registry
                .sources
                .iter()
                .any(|source| source.name.eq_ignore_ascii_case("claude-plugins-official"))
        {
            targets.push((None, OFFICIAL_MARKETPLACE_SOURCE.into()));
        }
        let mut errors = Vec::new();
        for (previous, source) in targets {
            let result = (|| {
                let market = checked_market(&app, &source, true)?;
                let expected_name = previous.as_deref().or_else(|| {
                    (source == OFFICIAL_MARKETPLACE_SOURCE).then_some("claude-plugins-official")
                });
                if expected_name
                    .is_some_and(|name| !market.manifest().name.eq_ignore_ascii_case(name))
                {
                    return Err("市场名称与已登记名称不一致".into());
                }
                let entry =
                    MarketplaceEntry::from_materialization(&market).map_err(|e| e.to_string())?;
                registry.insert(entry, true).map_err(|e| e.to_string())
            })();
            if let Err(error) = result {
                errors.push(format!(
                    "{}: {error}",
                    previous.as_deref().unwrap_or("claude-plugins-official")
                ));
            }
        }
        registry.initialized = true;
        let _write = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        registry.save(&root).map_err(|e| e.to_string())?;
        Ok(errors)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_marketplace_install(app: AppHandle, plugin_id: String) -> Result<(), String> {
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = MARKETPLACE_OPERATIONS
            .lock()
            .map_err(|_| "市场操作锁不可用")?;
        let root = data_root(&app)?;
        crate::modules::storage::path("plugins/marketplaces.json")?;
        let registry = MarketplaceRegistry::load(&root).map_err(|e| e.to_string())?;
        let source = registry
            .sources
            .iter()
            .find(|source| {
                id.marketplace
                    .as_deref()
                    .is_some_and(|name| source.name.eq_ignore_ascii_case(name))
            })
            .ok_or("找不到插件市场来源")?;
        let market = checked_market(&app, &source.source, false)?;
        if !market.manifest().name.eq_ignore_ascii_case(&source.name) {
            return Err("市场名称与已登记名称不一致".into());
        }
        let plan = checked_plan(&market, &id.plugin)?;
        install_plan(&app, plan, &[])
    })
    .await
    .map_err(|e| e.to_string())?
}

fn checked_plan(
    market: &rcode_plugins::MarketplaceMaterialization,
    name: &str,
) -> Result<Vec<MaterializedPlugin>, String> {
    resolve_marketplace_plugin_install_plan_checked(market, Some(name), &cache_root()?, &|root| {
        validate_plugin_tree(root).map_err(rcode_plugins::PluginError::Invalid)
    })
    .map_err(|e| e.to_string())
}

fn validate_update_targets(
    state: &rcode_plugins::PluginState,
    expected: &[(PluginId, std::path::PathBuf)],
) -> Result<(), String> {
    for (id, path) in expected {
        if !state.plugins.iter().any(|plugin| {
            plugin.id.to_string().eq_ignore_ascii_case(&id.to_string())
                && plugin.install_path == *path
        }) {
            return Err("插件安装状态已改变，请刷新后重试".into());
        }
    }
    Ok(())
}

fn install_plan(
    app: &AppHandle,
    plan: Vec<MaterializedPlugin>,
    expected: &[(PluginId, std::path::PathBuf)],
) -> Result<(), String> {
    let _write = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
    let manager = PluginManager::new(data_root(app)?);
    validate_update_targets(&manager.load_state().map_err(|e| e.to_string())?, expected)?;
    manager
        .install_from_directories(
            plan,
            UserConfigUpdate::default(),
            &mut extension_secrets(app),
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_plugins_update(app: AppHandle, plugin_id: Option<String>) -> Result<(), String> {
    let id = plugin_id
        .map(|id| PluginId::parse(&id).map_err(|e| e.to_string()))
        .transpose()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = MARKETPLACE_OPERATIONS
            .lock()
            .map_err(|_| "市场操作锁不可用")?;
        let manager = PluginManager::new(data_root(&app)?);
        let state = manager.load_state().map_err(|e| e.to_string())?;
        let targets: Vec<_> = state
            .plugins
            .iter()
            .filter(|plugin| {
                id.as_ref()
                    .is_none_or(|id| plugin.id.to_string().eq_ignore_ascii_case(&id.to_string()))
            })
            .filter(|plugin| id.is_some() || plugin.source.is_some())
            .collect();
        if targets.is_empty() {
            return Err("没有可更新的插件".into());
        }
        let expected: Vec<_> = targets
            .iter()
            .map(|plugin| (plugin.id.clone(), plugin.install_path.clone()))
            .collect();
        let mut plans = BTreeMap::new();
        let mut markets = BTreeMap::new();
        for plugin in targets {
            match manager
                .install_source(&plugin.id)
                .map_err(|e| e.to_string())?
            {
                PluginInstallSource::Local { path } => {
                    validate_plugin_tree(&path)?;
                    plans.insert(
                        plugin.id.to_string().to_ascii_lowercase(),
                        MaterializedPlugin {
                            id: plugin.id.clone(),
                            source_root: path.clone(),
                            source: Some(PluginInstallSource::Local { path }),
                        },
                    );
                }
                PluginInstallSource::Marketplace { source } => {
                    if !markets.contains_key(&source) {
                        markets.insert(source.clone(), checked_market(&app, &source, true)?);
                    }
                    let market = &markets[&source];
                    if !plugin
                        .id
                        .marketplace
                        .as_deref()
                        .is_some_and(|name| market.manifest().name.eq_ignore_ascii_case(name))
                    {
                        return Err("市场名称与插件安装记录不一致".into());
                    }
                    for item in checked_plan(market, &plugin.id.plugin)? {
                        plans.insert(item.id.to_string().to_ascii_lowercase(), item);
                    }
                }
            }
        }
        install_plan(&app, plans.into_values().collect(), &expected)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_pick_path(
    window: tauri::WebviewWindow,
    directory: bool,
    title: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    if title.len() > 256 || title.chars().any(char::is_control) {
        return Err("插件配置标题无效".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let picker = window.dialog().file().set_title(title).set_parent(&window);
        let selected = if directory {
            picker.blocking_pick_folder()
        } else {
            picker.blocking_pick_file()
        };
        selected
            .map(|selected| {
                let path = selected.into_path().map_err(|e| e.to_string())?;
                plugin_config_path(&path)
            })
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}

fn plugin_config_path(path: &Path) -> Result<String, String> {
    deny_secret_path(path)?;
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    deny_secret_path(&canonical)?;
    Ok(crate::modules::fs::to_canon(canonical))
}

#[tauri::command]
pub async fn agent_plugins_details(
    app: AppHandle,
    plugin_id: String,
) -> Result<PluginDetails, String> {
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = PluginManager::new(data_root(&app)?)
            .load_state()
            .map_err(|e| e.to_string())?;
        let plugin = state
            .plugins
            .into_iter()
            .find(|plugin| plugin.id.to_string().eq_ignore_ascii_case(&id.to_string()))
            .ok_or("插件未安装")?;
        let manifest = load_plugin_manifest(&plugin.install_path).map_err(|e| e.to_string())?;
        let inventory = inspect_plugin_components(&plugin.install_path, &manifest)
            .map_err(|e| e.to_string())?;
        let fields = manifest
            .user_config
            .into_iter()
            .map(|(name, mut field)| {
                if field.sensitive {
                    field.default = None;
                }
                field.extra.clear();
                (name, field)
            })
            .collect();
        let components = BTreeMap::from([
            ("Commands".into(), inventory.commands),
            ("Skills".into(), inventory.skills),
            ("MCP servers".into(), inventory.mcp),
        ]);
        Ok(PluginDetails {
            plugin,
            description: manifest.description,
            fields,
            components,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_install_rejects_sensitive_files_and_nested_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("commands")).unwrap();
        std::fs::write(temp.path().join("commands/review.md"), "Review").unwrap();
        assert!(validate_plugin_tree(temp.path()).is_ok());
        std::fs::write(temp.path().join("commands/.env.local"), "value").unwrap();
        assert!(validate_plugin_tree(temp.path()).is_err());
    }

    #[test]
    fn update_cannot_resurrect_a_concurrently_uninstalled_plugin() {
        let id = PluginId::parse("review@example").unwrap();
        let expected = vec![(id, std::path::PathBuf::from("previous-cache"))];
        assert!(
            validate_update_targets(&rcode_plugins::PluginState::default(), &expected).is_err()
        );
        assert!(validate_update_targets(&rcode_plugins::PluginState::default(), &[]).is_ok());
    }

    #[test]
    fn config_picker_rejects_sensitive_paths_before_returning_them() {
        let temp = tempfile::tempdir().unwrap();
        let public = temp.path().join("config.json");
        std::fs::write(&public, "{}").unwrap();
        assert!(plugin_config_path(&public).is_ok());
        let secret = temp.path().join(".env");
        std::fs::write(&secret, "value").unwrap();
        assert!(plugin_config_path(&secret).is_err());
    }
}
