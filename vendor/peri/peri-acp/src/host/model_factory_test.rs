//! 子 Agent 模型选择的宿主工厂策略测试。
//!
//! 解析层（`resolve_agent_model`）保留错误语义；本文件冻结宿主侧产品
//! 策略：任何解析失败（供应商/模型已被删除、非法输入）都告警并回退
//! 会话 Provider，子 Agent 派发不中断。

use crate::host::model_factory::resolve_subagent_provider;
use crate::provider::{config::AppConfig, LlmProvider, PeriConfig, ProviderConfig};

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

fn config_with_provider(id: &str) -> PeriConfig {
    PeriConfig {
        config: AppConfig {
            providers: vec![ProviderConfig {
                id: id.to_string(),
                provider_type: "anthropic".to_string(),
                api_key: "key".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn valid_qualified_model_resolves_configured_provider() {
    let inherited = openai_provider("parent-model");
    let resolved = resolve_subagent_provider(
        &inherited,
        &config_with_provider("provider-a"),
        "provider-a::direct-model",
    );
    assert_eq!(resolved.display_name(), "Anthropic");
    assert_eq!(resolved.model_name(), "direct-model");
}

#[test]
fn empty_selection_keeps_session_provider() {
    let inherited = openai_provider("parent-model");
    let resolved = resolve_subagent_provider(&inherited, &config_with_provider("provider-a"), "");
    assert_eq!(resolved.model_name(), "parent-model");
}

#[test]
fn deleted_provider_or_model_falls_back_to_session_provider() {
    let inherited = openai_provider("parent-model");
    // 供应商整体被删除。
    let resolved = resolve_subagent_provider(
        &inherited,
        &config_with_provider("provider-a"),
        "ghost::model",
    );
    assert_eq!(resolved.display_name(), inherited.display_name());
    assert_eq!(resolved.model_name(), "parent-model");
}

#[test]
fn invalid_selection_falls_back_to_session_provider() {
    let inherited = openai_provider("parent-model");
    let resolved = resolve_subagent_provider(
        &inherited,
        &config_with_provider("provider-a"),
        "bad\u{7}input",
    );
    assert_eq!(resolved.model_name(), "parent-model");
}
