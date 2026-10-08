mod client_tools;
pub(crate) mod commands;
pub(crate) mod control;
mod events;
pub(crate) mod extensions;
mod history;
pub(crate) mod instructions;
pub(crate) mod marketplace;
pub(crate) mod memory;
mod permissions;
pub(crate) mod resources;
pub(crate) mod security;
mod tools;
mod workspace_tools;

pub use control::AgentCoreState;

use control::ApprovalGate;
use events::EventBridge;
use permissions::PermissionMode;
use rcode_agent::{AgentId, AgentRunner, PlanGuard, RunLimits, SessionId, TurnId};
use rcode_model::{Message, MessageRole, ProviderProtocol, ReasoningCapability};
use rcode_provider::{ApiKey, ProviderClient, ProviderConfig};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{net::IpAddr, sync::Arc};
use tauri::{ipc::Channel, AppHandle, Manager, State};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAgentRequest {
    pub run_id: String,
    pub session_id: String,
    pub protocol: ProviderProtocol,
    pub provider_id: String,
    pub base_url: String,
    pub secret_account: Option<String>,
    pub model: String,
    pub context_limit: u64,
    /// 未提供时沿用 Runner 的窗口派生上限。
    pub max_output_tokens: Option<u32>,
    /// 未提供时保留既有内置模型的图片输入行为。
    pub image_input: Option<bool>,
    pub reasoning_body: Option<rcode_provider::ReasoningBody>,
    pub cwd: Option<String>,
    pub system: String,
    pub messages: Vec<Value>,
    pub plan_mode: bool,
    #[serde(default)]
    permission_mode: PermissionMode,
    #[serde(default)]
    pub client_tools: Vec<rcode_model::ToolDefinition>,
    pub subagent_tools: Option<Vec<String>>,
    // 本地服务与用户明确配置的端点允许局域网；内置云端地址始终禁用。
    pub allow_private_network: bool,
}

fn validate_request(request: &NativeAgentRequest) -> Result<(), String> {
    if request.run_id.len() > 128
        || request.model.trim().is_empty()
        || request.model.len() > 256
        || request.system.len() > 256 * 1024
        || request.messages.len() > 2048
        || !(8192..=4_000_000).contains(&request.context_limit)
        || request.max_output_tokens.is_some_and(|limit| {
            limit == 0 || u64::from(limit) > request.context_limit || limit > 1_000_000
        })
        || serde_json::to_vec(&request.messages)
            .map_err(|e| e.to_string())?
            .len()
            > 16 * 1024 * 1024
    {
        return Err("Agent 请求无效或超过大小上限".into());
    }
    SessionId::new(&request.session_id).map_err(|e| e.to_string())?;
    TurnId::new(&request.run_id).map_err(|e| e.to_string())?;
    validate_network_endpoint(&request.base_url)?;
    tools::validate_client_tools(&request.client_tools)?;
    if let Some(names) = &request.subagent_tools {
        tools::validate_subagent_tools(names, request.plan_mode, &request.client_tools)?;
        if request.cwd.is_none() {
            return Err("子智能体需要任务目录".into());
        }
    }
    if let Some(account) = &request.secret_account {
        let static_account = [
            "openai",
            "anthropic",
            "google",
            "xai",
            "cerebras",
            "groq",
            "deepseek",
            "mistral",
            "openrouter",
            "openai-compatible",
        ]
        .iter()
        .any(|id| account == &format!("{id}-api-key"));
        let custom_account = account
            .strip_prefix("compat-")
            .and_then(|s| s.strip_suffix("-api-key"))
            .is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            });
        if !static_account && !custom_account {
            return Err("模型密钥引用无效".into());
        }
    }
    Ok(())
}

pub(super) fn validate_network_endpoint(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "服务地址格式无效")?;
    if value.len() > 8192
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("服务地址只允许无凭据的 HTTP/HTTPS URL".into());
    }
    let host = url.host_str().ok_or("服务地址缺少主机")?;
    if matches!(
        host.to_ascii_lowercase().as_str(),
        "metadata" | "metadata.google.internal" | "metadata.azure.com"
    ) || host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| match ip {
            IpAddr::V4(ip) => ip.is_link_local() || ip.is_unspecified() || ip.is_multicast(),
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.segments()[0..2] == [0xfd00, 0xec2]
            }
        })
    {
        return Err("拒绝访问云元数据或无效服务地址".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_core_start(
    app: AppHandle,
    state: State<'_, AgentCoreState>,
    request: NativeAgentRequest,
    on_event: Channel<Value>,
) -> Result<(), String> {
    validate_request(&request)?;
    let cwd = super::workspace::authorize_spawn_cwd(
        &app.state(),
        request.cwd.as_deref(),
        &super::workspace::WorkspaceEnv::Local,
    )?;
    let lease = state.register(&request.run_id, &request.session_id)?;
    let run_state = state.0.clone();
    let events = EventBridge::new(on_event);
    tauri::async_runtime::spawn(async move {
        let result = execute_turn(
            &app,
            request,
            cwd,
            run_state,
            events.clone(),
            &lease.cancellation,
        )
        .await;
        match result {
            Ok(()) => {}
            Err(error) => {
                let _ = events.emit(json!({"type":"error", "message":rcode_model::redact_error_secrets_bounded(&error, 2048)}));
            }
        }
        drop(lease);
        // 先释放租约，界面收到 end 后立即提交下一轮也不会碰到旧运行。
        let _ = events.emit(json!({"type":"end"}));
    });
    Ok(())
}

async fn execute_turn(
    app: &AppHandle,
    request: NativeAgentRequest,
    cwd: Option<std::path::PathBuf>,
    state: Arc<std::sync::Mutex<control::RunRegistry>>,
    events: Arc<EventBridge>,
    cancellation: &rcode_agent::TurnCancellation,
) -> Result<(), String> {
    let key = match &request.secret_account {
        Some(account) => super::secrets::read_secret(app, &app.state(), "rcode-ai", account)?,
        None => None,
    };
    let mut config = match key.filter(|key| !key.is_empty()) {
        Some(key) => ProviderConfig::new(
            &request.provider_id,
            request.protocol,
            &request.base_url,
            ApiKey::new(key).map_err(|e| e.to_string())?,
        ),
        None if matches!(request.provider_id.as_str(), "openai" | "anthropic") => {
            return Err("尚未配置模型供应商密钥".into());
        }
        None => ProviderConfig::new_unauthenticated(
            &request.provider_id,
            request.protocol,
            &request.base_url,
        ),
    }
    .map_err(|e| e.to_string())?;
    apply_model_capabilities(&mut config, &request);
    let http =
        super::net::agent_http_client(&request.base_url, request.allow_private_network).await?;
    let provider = Arc::new(
        ProviderClient::new(config)
            .map_err(|e| e.to_string())?
            .with_http_client(http),
    );
    let mut messages = history::model_history(&request.messages)?;
    let (registry, clients) = tools::prepare_tools(
        app,
        cwd.as_deref(),
        events.clone(),
        cancellation,
        state.clone(),
        &request,
    )
    .await?;
    let runner = AgentRunner::new(
        provider,
        registry,
        RunLimits::new(24, 256).expect("static run limits"),
    )
    .with_event_sink(events.clone())
    .with_commit_sink(events.clone())
    .with_tool_approval_gate(Arc::new(ApprovalGate {
        run_id: request.run_id.clone(),
        state,
        events: events.clone(),
        permission_mode: request.permission_mode,
        plan_mode: request.plan_mode,
    }));
    messages.insert(0, Message::text(MessageRole::System, request.system));
    let mut turn = rcode_agent::TurnRequest::new(
        SessionId::new(request.session_id).expect("validated session ID"),
        TurnId::new(request.run_id).expect("validated turn ID"),
        AgentId::new("main").expect("static agent ID"),
        request.model,
        messages,
        if request.plan_mode {
            PlanGuard::read_only()
        } else {
            PlanGuard::inactive()
        },
    );
    turn.set_cancellation(cancellation.clone());
    let result = runner.run_turn(turn).await;
    for client in clients {
        let _ = client.close().await;
    }
    let cancelled = cancellation.is_cancelled();
    let hit_step_cap =
        result.state.terminal_reason() == Some(rcode_agent::TerminalReason::LimitReached);
    if let Some(error) = result.error {
        if !cancelled && !hit_step_cap {
            return Err(error.to_string());
        }
    }
    events.emit(json!({"type":"finish", "cancelled":cancelled, "hitStepCap":hit_step_cap}))?;
    Ok(())
}

fn apply_model_capabilities(config: &mut ProviderConfig, request: &NativeAgentRequest) {
    config.reasoning_body = request.reasoning_body.clone();
    config.default_capabilities.streaming = true;
    config.default_capabilities.tool_calling = true;
    config.default_capabilities.parallel_tool_calls = true;
    config.default_capabilities.reasoning = if request.reasoning_body.is_some() {
        ReasoningCapability::Configurable
    } else {
        ReasoningCapability::OutputOnly
    };
    config.default_capabilities.image_input = request.image_input.unwrap_or(true);
    config.default_capabilities.max_context_tokens = Some(request.context_limit);
    config.default_capabilities.max_output_tokens = request.max_output_tokens.map(u64::from);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_local_models_are_allowed_but_metadata_is_always_blocked() {
        assert!(validate_network_endpoint("http://127.0.0.1:1234/v1").is_ok());
        for url in [
            "http://169.254.169.254/latest",
            "http://metadata.google.internal/",
            "file:///tmp/x",
            "https://key:secret@example.com/",
        ] {
            assert!(validate_network_endpoint(url).is_err(), "{url}");
        }
    }

    fn model_request() -> NativeAgentRequest {
        serde_json::from_value(json!({"runId":"run", "sessionId":"session", "protocol":"chat_completions", "providerId":"openai-compatible", "baseUrl":"http://127.0.0.1:1234/v1", "model":"local", "contextLimit":65536, "system":"", "messages":[], "planMode":false, "allowPrivateNetwork":true})).unwrap()
    }

    #[test]
    fn reasoning_mapping_is_frozen_into_provider_and_rejects_payload_overrides() {
        let mut request = model_request();
        request.reasoning_body =
            Some(serde_json::from_value(json!({"reasoning":{"effort":"xhigh"}})).unwrap());
        let mut config =
            ProviderConfig::new_unauthenticated("local", request.protocol, &request.base_url)
                .unwrap();
        apply_model_capabilities(&mut config, &request);
        assert_eq!(config.reasoning_body, request.reasoning_body);
        assert_eq!(
            config.default_capabilities.reasoning,
            ReasoningCapability::Configurable
        );
        let mut ipc = json!({"runId":"run", "sessionId":"session", "protocol":"responses", "providerId":"local", "baseUrl":"http://127.0.0.1:1234/v1", "model":"local", "contextLimit":65536, "system":"", "messages":[], "planMode":false, "allowPrivateNetwork":true});
        ipc["reasoningBody"] = json!({"model":"other"});
        assert!(serde_json::from_value::<NativeAgentRequest>(ipc).is_err());
    }

    #[test]
    fn permission_mode_defaults_to_ask_and_rejects_unknown_ipc_values() {
        assert_eq!(model_request().permission_mode, PermissionMode::Ask);
        let request = json!({"runId":"run", "sessionId":"session", "protocol":"chat_completions", "providerId":"openai-compatible", "baseUrl":"http://127.0.0.1:1234/v1", "model":"local", "contextLimit":65536, "system":"", "messages":[], "planMode":false, "allowPrivateNetwork":true, "permissionMode":"invalid"});
        assert!(serde_json::from_value::<NativeAgentRequest>(request).is_err());
    }

    #[test]
    fn model_overrides_reach_provider_capabilities_without_changing_legacy_defaults() {
        let mut request = model_request();
        let mut config =
            ProviderConfig::new_unauthenticated("local", request.protocol, &request.base_url)
                .unwrap();
        apply_model_capabilities(&mut config, &request);
        assert!(config.default_capabilities.image_input);
        assert_eq!(config.default_capabilities.max_output_tokens, None);
        request.max_output_tokens = Some(4096);
        request.image_input = Some(false);
        assert!(validate_request(&request).is_ok());
        apply_model_capabilities(&mut config, &request);
        assert!(!config.default_capabilities.image_input);
        assert_eq!(config.default_capabilities.max_output_tokens, Some(4096));
        assert_eq!(config.default_capabilities.max_context_tokens, Some(65536));
    }

    #[test]
    fn invalid_model_output_limits_are_rejected_at_ipc_boundary() {
        for limit in [0, 65537, 1_000_001] {
            let mut request = model_request();
            request.max_output_tokens = Some(limit);
            assert!(validate_request(&request).is_err());
        }
    }
}
