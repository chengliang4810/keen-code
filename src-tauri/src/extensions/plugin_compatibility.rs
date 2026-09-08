//! 插件模型别名的本地配置和有效模型解析。
use super::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PluginModelAliases {
    pub sonnet: Option<String>,
    pub opus: Option<String>,
    pub haiku: Option<String>,
}

impl PluginModelAliases {
    fn normalize(&mut self) -> Result<(), String> {
        for value in [&mut self.sonnet, &mut self.opus, &mut self.haiku] {
            *value = value
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .map(normalize_model_reference)
                .transpose()?;
        }
        Ok(())
    }

    fn retain_available(&mut self, available: &BTreeSet<String>) {
        for value in [&mut self.sonnet, &mut self.opus, &mut self.haiku] {
            if value.as_ref().is_some_and(|v| !available.contains(v)) {
                *value = None;
            }
        }
    }

    pub(super) fn mappings(&self) -> BTreeMap<String, String> {
        [
            ("sonnet", &self.sonnet),
            ("opus", &self.opus),
            ("haiku", &self.haiku),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.clone().map(|v| (key.to_owned(), v)))
        .collect()
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|e| e.to_string())?
        .join("plugin-model-aliases.json"))
}

fn read_config(path: &Path) -> Result<PluginModelAliases, String> {
    if !current_regular_file_exists(path, "插件模型适配")? {
        return Ok(PluginModelAliases::default());
    }
    let content = read_text_limited(path)?;
    let mut config: PluginModelAliases =
        serde_json::from_str(&content).map_err(|e| format!("插件模型适配配置无效：{e}"))?;
    config.normalize()?;
    Ok(config)
}

fn available_models(app: &AppHandle) -> Result<BTreeSet<String>, String> {
    Ok(crate::providers::list(app)
        .map_err(|e| e.to_string())?
        .providers
        .into_iter()
        .flat_map(|provider| {
            provider
                .models
                .into_iter()
                .map(move |model| format!("{}::{model}", provider.id))
        })
        .collect())
}

/// 每次建立扩展快照重新核对供应商目录；已删除的供应商或模型视为继承。
#[tauri::command]
pub fn plugin_model_aliases_get(app: AppHandle) -> Result<PluginModelAliases, String> {
    let mut config = read_config(&config_path(&app)?)?;
    config.retain_available(&available_models(&app)?);
    Ok(config)
}

#[tauri::command]
pub async fn plugin_model_aliases_set(
    mut config: PluginModelAliases,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<PluginModelAliases, String> {
    {
        let _guard = state.lock_io()?;
        config.normalize()?;
        config.retain_available(&available_models(&app)?);
        let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
        atomic_write_private(&config_path(&app)?, &bytes)?;
    }
    refresh_known_runtime_projects(&app, runtime.inner()).await?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_persist_and_deleted_models_inherit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aliases.json");
        assert_eq!(read_config(&path).unwrap(), PluginModelAliases::default());
        let mut config = PluginModelAliases {
            sonnet: Some("p::fast".into()),
            opus: Some("removed::large".into()),
            haiku: Some("p::deleted".into()),
        };
        atomic_write_private(&path, &serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(read_config(&path).unwrap(), config);
        config.retain_available(&BTreeSet::from(["p::fast".into()]));
        assert_eq!(
            config.mappings(),
            BTreeMap::from([("sonnet".into(), "p::fast".into())])
        );
        config.retain_available(&BTreeSet::new());
        assert_eq!(config, PluginModelAliases::default());
        assert!(serde_json::from_str::<PluginModelAliases>(r#"{"unknown":null}"#).is_err());
    }
}
