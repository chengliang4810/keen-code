//! ACP protocol state builders.
//!
//! Converts internal agent state into ACP protocol types
//! (modes, models, config options) for `session/new` and `session/set_*` responses.

// [TRAP] build_config_options 必须按优先级顺序返回（mode → model → thinking_effort）
// Session Config Options 覆盖旧的 Session Modes API，顺序错乱会导致 UI 显示异常。

pub use agent_client_protocol_schema::v1::{
    SessionConfigId, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionConfigValueId, SessionMode, SessionModeId, SessionModeState,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};

use crate::provider::{LlmProvider, PeriConfig};

/// Parse a mode ID string into a `PermissionMode`.
pub fn parse_permission_mode(mode_id: &str) -> PermissionMode {
    match mode_id {
        "accept_edit" => PermissionMode::AcceptEdit,
        "auto" => PermissionMode::AutoMode,
        "bypass" => PermissionMode::Bypass,
        _ => PermissionMode::Default,
    }
}

/// Build ACP `SessionModeState` from the current permission mode.
pub fn build_mode_state(pm: &SharedPermissionMode) -> SessionModeState {
    let current = pm.load();
    let current_id = match current {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let all_modes = vec![
        SessionMode::new(SessionModeId::new("default"), "Default")
            .description("All sensitive tools require approval"),
        SessionMode::new(SessionModeId::new("accept_edit"), "Accept Edit")
            .description("Allow filesystem edits"),
        SessionMode::new(SessionModeId::new("auto"), "Auto Mode")
            .description("LLM decides approval"),
        SessionMode::new(SessionModeId::new("bypass"), "Bypass").description("Allow everything"),
    ];
    SessionModeState::new(SessionModeId::new(current_id), all_modes)
}

/// Build ACP `SessionConfigOption` list from config.
///
/// Per ACP spec, config options supersede the older Session Modes API.
/// Returns mode, model, and thinking_effort in priority order (higher priority first).
pub fn build_config_options(
    _peri_config: &PeriConfig,
    provider: &LlmProvider,
    current_mode: PermissionMode,
) -> Vec<SessionConfigOption> {
    let mut options = Vec::with_capacity(3);

    // ── Mode (category: mode) ──
    let current_mode_id = match current_mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let mode_options = vec![
        SessionConfigSelectOption::new(SessionConfigValueId::new("default"), "Default"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("accept_edit"), "Accept Edit"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("auto"), "Auto Mode"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("bypass"), "Bypass"),
    ];
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("mode"),
            "Session Mode",
            SessionConfigValueId::new(current_mode_id),
            SessionConfigSelectOptions::Ungrouped(mode_options),
        )
        .category(SessionConfigOptionCategory::Mode),
    );

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
