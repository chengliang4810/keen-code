//! ACP protocol state builders.
//!
//! Converts internal agent state into ACP config options for `session/new` and
//! `session/set_*` responses.

pub use agent_client_protocol_schema::v1::{
    SessionConfigId, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionConfigValueId,
};

use crate::provider::LlmProvider;

/// Build ACP `SessionConfigOption` list from config.
///
/// Returns model and thinking-effort options in stable priority order.
pub fn build_config_options(provider: &LlmProvider) -> Vec<SessionConfigOption> {
    let mut options = Vec::with_capacity(2);

    // ── Model (category: model) ──
    // 每个会话独立持有供应商；当前值直接反映该会话实际模型，供客户端恢复模型显示。
    let current_model = provider.model_name().to_string();
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            SessionConfigValueId::new(current_model.clone()),
            SessionConfigSelectOptions::Ungrouped(vec![SessionConfigSelectOption::new(
                SessionConfigValueId::new(current_model.clone()),
                current_model.clone(),
            )]),
        )
        .category(SessionConfigOptionCategory::Model),
    );

    // ── Thinking effort (category: thought_level) ──
    let effort = provider.effort().unwrap_or("medium");
    let thinking_options = vec![
        SessionConfigSelectOption::new(SessionConfigValueId::new("low"), "Low".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("medium"), "Medium".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("high"), "High".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("xhigh"), "XHigh".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("max"), "Max".to_string()),
    ];
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("thinking_effort"),
            "Thinking Effort",
            SessionConfigValueId::new(effort),
            SessionConfigSelectOptions::Ungrouped(thinking_options),
        )
        .category(SessionConfigOptionCategory::ThoughtLevel),
    );

    options
}
