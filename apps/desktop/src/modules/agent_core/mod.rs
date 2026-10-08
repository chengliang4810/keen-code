mod client_tools;
pub(crate) mod commands;
pub(crate) mod control;
pub(crate) mod extensions;
mod history;
pub(crate) mod instructions;
pub(crate) mod marketplace;
pub(crate) mod memory;
pub(crate) mod resources;
mod tools;

use rcode_runtime::network::validate_network_endpoint;
pub use rcode_runtime::AgentRuntime;
use rcode_runtime::{EventBridge, ModelConfig, PermissionMode, RunLease};

use rcode_agent::{AgentId, PlanGuard, SessionId, TurnId};
#[cfg(test)]
use rcode_model::ReasoningCapability;
use rcode_model::{Message, MessageRole, ProviderProtocol};
use rcode_provider::ApiKey;
#[cfg(test)]
use rcode_provider::ProviderConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
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

#[tauri::command]
pub async fn agent_core_start(
    app: AppHandle,
    state: State<'_, AgentRuntime>,
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
    let run_state = state.inner().clone();
    let events = EventBridge::new(move |event| {
        on_event
            .send(event)
            .map_err(|_| "Agent 界面连接已关闭".into())
    });
    tauri::async_runtime::spawn(async move {
        let result = execute_turn(&app, request, cwd, run_state, events.clone(), &lease).await;
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
    state: AgentRuntime,
    events: Arc<EventBridge>,
    lease: &RunLease,
) -> Result<(), String> {
    let key = match &request.secret_account {
        Some(account) => super::secrets::read_secret(app, &app.state(), "rcode-ai", account)?,
        None => None,
    };
    let key = key
        .filter(|key| !key.is_empty())
        .map(ApiKey::new)
        .transpose()
        .map_err(|error| error.to_string())?;
    let model = request.model_config();
    let provider = Arc::new(model.provider(key).await?);
    let cancellation = &lease.cancellation;
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
    let result = state
        .run_turn(
            lease,
            provider,
            registry,
            turn,
            events.clone(),
            request.permission_mode,
        )
        .await?;
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

impl NativeAgentRequest {
    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            provider_id: self.provider_id.clone(),
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            context_limit: self.context_limit,
            max_output_tokens: self.max_output_tokens,
            image_input: self.image_input.unwrap_or(true),
            reasoning_body: self.reasoning_body.clone(),
            allow_private_network: self.allow_private_network,
        }
    }
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
        request.model_config().apply_capabilities(&mut config);
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
        request.model_config().apply_capabilities(&mut config);
        assert!(config.default_capabilities.image_input);
        assert_eq!(config.default_capabilities.max_output_tokens, None);
        request.max_output_tokens = Some(4096);
        request.image_input = Some(false);
        assert!(validate_request(&request).is_ok());
        request.model_config().apply_capabilities(&mut config);
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
