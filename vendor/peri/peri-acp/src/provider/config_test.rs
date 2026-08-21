use super::*;

fn provider(id: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: "openai".to_string(),
        api_key: "key".to_string(),
        ..Default::default()
    }
}

#[test]
fn app_config_contains_only_explicit_provider_configuration() {
    let config = AppConfig::default();
    assert!(config.providers.is_empty());
    let serialized = serde_json::to_value(&config).unwrap();
    assert!(serialized.get("active_alias").is_none());
    assert!(serialized.get("profiles").is_none());
    assert!(serialized.get("skills_dir").is_none());
    assert!(serialized.get("skillsDir").is_none());
}

#[test]
fn app_config_merges_provider_list_and_optional_values() {
    let mut global = AppConfig {
        providers: vec![provider("global")],
        language: Some("zh-CN".to_string()),
        ..Default::default()
    };
    let workspace = AppConfig {
        providers: vec![provider("workspace")],
        compact: Some(peri_acp_types::compact::CompactConfig::default()),
        ..Default::default()
    };

    global.merge_overrides(workspace);
    assert_eq!(global.providers[0].id, "workspace");
    assert_eq!(global.language.as_deref(), Some("zh-CN"));
    assert!(global.compact.is_some());
}

#[test]
fn provider_models_preserve_model_metadata_without_fixed_tiers() {
    let models: ProviderModels = serde_json::from_value(serde_json::json!({
        "gpt-5.6-luna": {"contextWindow": 200000},
        "model-a": {"contextWindow": 200000}
    }))
    .unwrap();
    assert!(models.models.contains_key("gpt-5.6-luna"));
    assert!(models.models.contains_key("model-a"));
}
