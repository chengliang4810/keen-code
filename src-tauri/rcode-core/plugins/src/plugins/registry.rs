use super::*;

const MAX_REGISTRY_BYTES: usize = 8 * 1024 * 1024;
const MAX_MARKETPLACES: usize = 32;
const MAX_CATALOG_PLUGINS: usize = 4096;

pub const OFFICIAL_MARKETPLACE_SOURCE: &str = "github:anthropics/claude-plugins-official";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketplaceEntry {
    pub name: String,
    pub source: String,
    pub plugins: Vec<MarketplaceCatalogPlugin>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketplaceCatalogPlugin {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub category: Option<String>,
    pub keywords: Vec<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketplaceRegistry {
    #[serde(default)]
    pub initialized: bool,
    pub sources: Vec<MarketplaceEntry>,
}

impl MarketplaceEntry {
    pub fn from_materialization(market: &MarketplaceMaterialization) -> Result<Self> {
        let PluginInstallSource::Marketplace { source } = market.source() else {
            return Err(PluginError::Invalid("市场更新来源无效".into()));
        };
        let manifest = market.manifest();
        let entry = Self {
            name: manifest.name.clone(),
            source: source.clone(),
            plugins: manifest
                .plugins
                .iter()
                .map(|plugin| MarketplaceCatalogPlugin {
                    name: plugin.name.clone(),
                    description: plugin.description.clone(),
                    version: plugin.version.clone(),
                    category: plugin.category.clone(),
                    keywords: plugin.keywords.clone(),
                })
                .collect(),
        };
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<()> {
        validate_marketplace_name(&self.name)?;
        if !Path::new(&self.source).is_absolute() {
            validate_marketplace_source(&self.source)?;
        }
        if self.source.is_empty()
            || self.source.len() > 8192
            || self.source.chars().any(char::is_control)
            || self.plugins.len() > MAX_CATALOG_PLUGINS
        {
            return Err(PluginError::Invalid("市场记录超过上限或来源无效".into()));
        }
        let mut names = BTreeSet::new();
        for plugin in &self.plugins {
            PluginId::from_components(&plugin.name, Some(&self.name))?;
            if !names.insert(marketplace_name_key(&plugin.name)) {
                return Err(PluginError::Invalid("市场包含重复插件名称".into()));
            }
        }
        Ok(())
    }
}

impl MarketplaceRegistry {
    pub fn load(data_root: &Path) -> Result<Self> {
        let storage = PluginStorage::under(data_root);
        storage.validate_layout()?;
        let path = data_root.join("plugins/marketplaces.json");
        validate_controlled_path(&data_root.join("plugins"), &path, "市场登记文件")?;
        let file = match fs::File::open(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            result => result?,
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_REGISTRY_BYTES as u64 {
            return Err(PluginError::Invalid(
                "市场登记文件不是普通文件或超出大小上限".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_REGISTRY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(PluginError::Invalid("市场登记文件超出大小上限".into()));
        }
        let registry: Self = serde_json::from_slice(&bytes)?;
        registry.validate()?;
        Ok(registry)
    }

    fn validate(&self) -> Result<()> {
        if self.sources.len() > MAX_MARKETPLACES {
            return Err(PluginError::Invalid("市场数量超过上限 32".into()));
        }
        let mut names = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !names.insert(marketplace_name_key(&source.name)) {
                return Err(PluginError::Invalid("市场名称重复".into()));
            }
        }
        Ok(())
    }

    pub fn save(&self, data_root: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(PluginError::Invalid("市场登记文件超出大小上限".into()));
        }
        PluginStorage::under(data_root).ensure_directories()?;
        let path = data_root.join("plugins/marketplaces.json");
        validate_controlled_path(&data_root.join("plugins"), &path, "市场登记文件")?;
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(path)
            .map_err(|error| PluginError::Io(error.error))?;
        Ok(())
    }

    pub fn insert(&mut self, entry: MarketplaceEntry, replace: bool) -> Result<()> {
        entry.validate()?;
        if let Some(previous) = self
            .sources
            .iter_mut()
            .find(|source| source.name.eq_ignore_ascii_case(&entry.name))
        {
            if !replace {
                return Err(PluginError::Invalid("市场名称已存在".into()));
            }
            *previous = entry;
        } else {
            if self.sources.len() >= MAX_MARKETPLACES {
                return Err(PluginError::Invalid("市场数量超过上限 32".into()));
            }
            self.sources.push(entry);
        }
        self.sources
            .sort_by_key(|source| marketplace_name_key(&source.name));
        self.initialized = true;
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        let index = self
            .sources
            .iter()
            .position(|source| source.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| PluginError::Invalid("找不到市场来源".into()))?;
        self.sources.remove(index);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local_market(dir: &Path, name: &str) -> MarketplaceEntry {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(dir.join(MARKETPLACE_MANIFEST), json!({"name":name,"plugins":[{"name":"review","source":"./review","description":"Review","version":"1.0"}]}).to_string()).unwrap();
        MarketplaceEntry::from_materialization(
            &materialize_marketplace_source(dir.to_str().unwrap(), &dir.join("cache")).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn registry_roundtrip_duplicate_identity_and_removal_preserve_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("market");
        let entry = local_market(&source, "Example");
        let mut registry = MarketplaceRegistry::default();
        registry.insert(entry.clone(), false).unwrap();
        let mut duplicate = entry;
        duplicate.name = "example".into();
        assert!(registry.insert(duplicate, false).is_err());
        registry.save(temp.path()).unwrap();
        let mut loaded = MarketplaceRegistry::load(temp.path()).unwrap();
        assert_eq!(loaded.sources[0].plugins[0].name, "review");
        loaded.remove("EXAMPLE").unwrap();
        loaded.save(temp.path()).unwrap();
        assert!(
            MarketplaceRegistry::load(temp.path())
                .unwrap()
                .sources
                .is_empty()
        );
        assert!(source.join(MARKETPLACE_MANIFEST).is_file());
    }

    #[test]
    fn sources_reject_credentials_and_metadata_endpoints_before_io() {
        for source in [
            "https://user:secret@example.com/market.json",
            "https://example.com/market.json?token=secret",
            "http://169.254.169.254/latest",
            "git:https://user:secret@example.com/repo.git",
            r#"{"source":"url","url":"https://example.com/market.json","headers":{"Authorization":"secret"}}"#,
        ] {
            assert!(validate_marketplace_source(source).is_err(), "{source}");
        }
        assert!(
            validate_marketplace_source(OFFICIAL_MARKETPLACE_SOURCE)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_registry_write_keeps_last_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let mut registry = MarketplaceRegistry::default();
        registry
            .insert(local_market(&temp.path().join("market"), "example"), false)
            .unwrap();
        registry.save(temp.path()).unwrap();
        let duplicate = registry.sources[0].plugins[0].clone();
        registry.sources[0].plugins.push(duplicate);
        assert!(registry.save(temp.path()).is_err());
        assert_eq!(
            MarketplaceRegistry::load(temp.path()).unwrap().sources[0]
                .plugins
                .len(),
            1
        );
    }

    #[test]
    fn marketplace_install_update_and_runtime_preserve_config_and_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("market");
        local_market(&source, "example");
        let plugin_root = source.join("review");
        fs::create_dir_all(plugin_root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(plugin_root.join("commands")).unwrap();
        fs::create_dir_all(plugin_root.join("skills/review")).unwrap();
        fs::write(plugin_root.join(PLUGIN_MANIFEST), json!({"name":"review","version":"1","userConfig":{"token":{"type":"string","sensitive":true,"required":true},"limit":{"type":"number"}}}).to_string()).unwrap();
        fs::write(plugin_root.join("commands/review.md"), "Review $ARGUMENTS").unwrap();
        fs::write(
            plugin_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nReview",
        )
        .unwrap();
        fs::write(plugin_root.join(".mcp.json"), r#"{"mcpServers":{"review":{"command":"server","env":{"TOKEN":"${user_config.token}"}}}}"#).unwrap();
        let cache = temp.path().join("market-cache");
        let data = temp.path().join("data");
        let manager = PluginManager::new(&data);
        let id = PluginId::parse("review@example").unwrap();
        let mut secrets = InMemorySecretStore::default();
        let market = materialize_marketplace_source(source.to_str().unwrap(), &cache).unwrap();
        let plan =
            resolve_marketplace_plugin_install_plan(&market, Some("review"), &cache).unwrap();
        manager
            .install_from_directories(plan, UserConfigUpdate::default(), &mut secrets)
            .unwrap();
        assert!(!manager.load_state().unwrap().plugins[0].enabled);
        manager
            .update_user_config(
                &id,
                UserConfigUpdate {
                    values: BTreeMap::from([
                        ("token".into(), json!("test-token")),
                        ("limit".into(), json!(3)),
                    ]),
                    replace: false,
                },
                &mut secrets,
            )
            .unwrap();
        manager.set_enabled(&id, true).unwrap();
        let old_path = manager.load_state().unwrap().plugins[0]
            .install_path
            .clone();
        fs::write(
            plugin_root.join("commands/review.md"),
            "Updated review $ARGUMENTS",
        )
        .unwrap();
        let market = refresh_marketplace_source(source.to_str().unwrap(), &cache).unwrap();
        manager
            .install_from_directories(
                resolve_marketplace_plugin_install_plan(&market, Some("review"), &cache).unwrap(),
                UserConfigUpdate::default(),
                &mut secrets,
            )
            .unwrap();
        let state = manager.load_state().unwrap();
        assert!(state.plugins[0].enabled);
        assert_ne!(state.plugins[0].install_path, old_path);
        assert_eq!(state.plugins[0].public_user_config["limit"], 3);
        assert!(
            !fs::read_to_string(data.join("plugins/state.json"))
                .unwrap()
                .contains("test-token")
        );
        let snapshot = manager
            .runtime_snapshot(temp.path(), &BTreeMap::new(), &secrets)
            .unwrap();
        assert_eq!(snapshot.plugins[0].commands.len(), 1);
        assert_eq!(snapshot.plugins[0].skills.len(), 1);
        assert_eq!(snapshot.plugins[0].mcp_servers.len(), 1);
    }

    #[test]
    fn access_policy_runs_before_loading_plugin_manifest() {
        let temp = tempfile::tempdir().unwrap();
        local_market(temp.path(), "example");
        fs::create_dir_all(temp.path().join("review/.claude-plugin")).unwrap();
        fs::write(
            temp.path().join("review").join(PLUGIN_MANIFEST),
            "invalid JSON",
        )
        .unwrap();
        let market = materialize_marketplace_source(
            temp.path().to_str().unwrap(),
            &temp.path().join("cache"),
        )
        .unwrap();
        let error = resolve_marketplace_plugin_install_plan_checked(
            &market,
            Some("review"),
            &temp.path().join("cache"),
            &|_| Err(PluginError::Invalid("policy-denied".into())),
        )
        .unwrap_err();
        assert!(error.to_string().contains("policy-denied"));
    }
}
