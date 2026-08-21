//! `LlmProvider::into_model()` / `context_window()` 的强类型映射测试。
//!
//! 冻结 Task 7 的协议边界：factory 产出 `Box<dyn peri_model::Model>`，
//! 而非旧 LLM facade trait。环境读取仍只发生在 ACP
//! （`from_env` / `from_config`），`peri-model` 不解析任何环境变量。

use super::*;

fn openai_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: model.to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    }
}

fn anthropic_provider(model: &str) -> LlmProvider {
    LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: model.to_string(),
        base_url: None,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    }
}

#[test]
fn into_model_openai_produces_openai_compatible_protocol() {
    let model = openai_provider("gpt-4o").into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::OpenAiCompatible
    ));
    assert_eq!(prepared.model_id(), "gpt-4o");
    // PreparedModelRequest 是有意的安全观测投影：endpoint path 被脱敏为 /[REDACTED]，
    // host 保留。协议补全路径（/v1/chat/completions）只发生在私有请求构造期。
    assert_eq!(prepared.endpoint().host_str(), Some("api.example.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
}

#[test]
fn into_model_anthropic_produces_anthropic_protocol() {
    let model = anthropic_provider("claude-sonnet-4-6").into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::Anthropic
    ));
    assert_eq!(prepared.model_id(), "claude-sonnet-4-6");
    // 同 OpenAI：host 保留，path 在观测投影中脱敏。
    assert_eq!(prepared.endpoint().host_str(), Some("api.anthropic.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
}

#[test]
fn into_model_thinking_config_applies_max_tokens() {
    // max_tokens 语义：into_model 从 provider 的 max_tokens 读取（默认 32000）。
    let model = openai_provider("gpt-4o")
        .with_model_name("gpt-4o".to_string())
        .into_model();
    let body = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(32000));

    let provider_with_think = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: "gpt-4o".to_string(),
        effort: Some("medium".to_string()),
        max_tokens: 16384,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    };
    let body = provider_with_think
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(16384));
    // effort 配置透传：reasoning_effort + thinking.enabled
    assert_eq!(body["reasoning_effort"], serde_json::json!("medium"));
    assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }));
}

#[test]
fn with_max_tokens_overrides_output_limit_without_changing_model() {
    let model = openai_provider("gpt-4o").with_max_tokens(4096).into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");

    assert_eq!(prepared.model_id(), "gpt-4o");
    assert_eq!(
        prepared.body().as_value()["max_tokens"],
        serde_json::json!(4096)
    );
}

#[test]
fn into_model_anthropic_extended_thinking_applied() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    };
    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 10_000 })
    );
    assert_eq!(
        body["output_config"],
        serde_json::json!({ "effort": "high" })
    );
    assert_eq!(body["max_tokens"], serde_json::json!(64000));
}

#[test]
fn output_limit_disables_invalid_anthropic_thinking_budget() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64_000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    }
    .with_max_tokens(1_024);

    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();

    assert_eq!(body["max_tokens"], serde_json::json!(1_024));
    assert!(body.get("thinking").is_none());
    assert!(body.get("output_config").is_none());
}

#[test]
fn output_limit_clamps_anthropic_thinking_below_total_limit() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64_000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    }
    .with_max_tokens(4_096);

    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();

    assert_eq!(body["max_tokens"], serde_json::json!(4_096));
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 4_095 })
    );
}

#[test]
fn into_model_invalid_base_url_falls_back_without_panic() {
    // fail-soft：非法 base_url 不 panic，回落到默认 endpoint；
    // 真正无效的 endpoint 由协议层在 prepare/stream 时 fail closed。
    let provider = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "not a url".to_string(),
        model: "gpt-4o".to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    };
    let model = provider.into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    // 非法 base_url 回落到默认 endpoint（api.openai.com），host 保留，path 脱敏。
    assert_eq!(prepared.endpoint().host_str(), Some("api.openai.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
}

#[test]
fn context_window_is_200k_for_both_providers() {
    assert_eq!(openai_provider("gpt-4o").context_window(), 200_000);
    assert_eq!(
        anthropic_provider("claude-sonnet-4-6").context_window(),
        200_000
    );
}

#[test]
fn from_config_reads_explicit_model_metadata() {
    let cfg = PeriConfig {
        config: AppConfig {
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "openai".into(),
                api_key: "k".into(),
                models: ProviderModels {
                    models: [("gpt-x".to_string(), serde_json::Value::Null)]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let p = LlmProvider::from_config(&cfg).unwrap();
    assert_eq!(p.model_name(), "gpt-x");
    assert!(!p.context_1m());
    assert_eq!(p.effort_key(), ":effort=high");
    let body = p
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(32000));
    assert_eq!(body["reasoning_effort"], serde_json::json!("high"));
}

/// Agent 模型覆盖只解析 KeenCode 限定模型；省略模型由宿主工厂沿用会话。
#[test]
fn resolve_agent_model_accepts_only_provider_qualified_values() {
    let cfg = PeriConfig {
        config: AppConfig {
            providers: vec![ProviderConfig {
                id: "provider-b".into(),
                provider_type: "anthropic".into(),
                api_key: "key-b".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let inherited = openai_provider("parent-model");

    let AgentModelResolution::Resolved(qualified) =
        LlmProvider::resolve_agent_model(&cfg, &inherited, "provider-b::direct-model")
    else {
        panic!("有效限定模型应解析成功");
    };
    assert_eq!(qualified.display_name(), "Anthropic");
    assert_eq!(qualified.model_name(), "direct-model");

    for invalid in ["", "   ", "model-a"] {
        assert!(matches!(
            LlmProvider::resolve_agent_model(&cfg, &inherited, invalid),
            AgentModelResolution::Error(_)
        ));
    }
}

/// 显式模型选择的语法或 Provider 配置错误必须 fail closed。
#[test]
fn resolve_agent_model_rejects_invalid_selection_and_provider_config() {
    let inherited = openai_provider("parent-model");
    let mut cfg = PeriConfig {
        config: AppConfig {
            providers: vec![ProviderConfig {
                id: "provider-a".into(),
                provider_type: "openai".into(),
                api_key: "key-a".into(),
                base_url: "https://api.example.com/v1".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };

    for invalid in ["\n", "::model", "provider-a::", "provider-a\n::model"] {
        assert!(matches!(
            LlmProvider::resolve_agent_model(&cfg, &inherited, invalid),
            AgentModelResolution::Error(_)
        ));
    }
    assert!(matches!(
        LlmProvider::resolve_agent_model(&cfg, &inherited, "missing::model"),
        AgentModelResolution::Error(_)
    ));

    cfg.config.providers[0].api_key.clear();
    assert!(matches!(
        LlmProvider::resolve_agent_model(&cfg, &inherited, "provider-a::model"),
        AgentModelResolution::Error(_)
    ));
    cfg.config.providers[0].api_key = "key-a".into();
    cfg.config.providers[0].provider_type = "unsupported".into();
    assert!(matches!(
        LlmProvider::resolve_agent_model(&cfg, &inherited, "provider-a::model"),
        AgentModelResolution::Error(_)
    ));
    cfg.config.providers[0].provider_type = "openai".into();
    cfg.config.providers[0].base_url = "not a url".into();
    assert!(matches!(
        LlmProvider::resolve_agent_model(&cfg, &inherited, "provider-a::model"),
        AgentModelResolution::Error(_)
    ));
}

#[test]
fn from_config_does_not_infer_a_model_without_explicit_metadata() {
    let cfg = PeriConfig {
        config: AppConfig {
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                api_key: "k".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(LlmProvider::from_config(&cfg).is_none());
}
