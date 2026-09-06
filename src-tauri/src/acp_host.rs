//! 进程内 ACP Host：把标准 JSON-RPC 请求路由到唯一 Agent Runtime。
//!
//! 该模块只负责协议边界、Session 控制面和响应编码。模型协议、工具执行、
//! Journal 归约与桌面实时投递仍由 [`AgentRuntime`] 和其下游组件负责。

use crate::agent_runtime::{AgentRuntime, AgentRuntimeError, RootTurnOptions};
use crate::session_commands::{
    PLAN_MODE_CONTRACT_EN, ULTRA_MODE_CONTRACT_EN, authorized_metadata, close_session_for_mutation,
    restore_session_after_mutation, retry_session_mutation, session_mode_state,
};
use keencode_acp::schema;
use keencode_acp::{
    AcpBoundaryError, AcpIncomingFrame, AcpNotification, AcpRequest, AcpRequestDecoder,
    AcpResponseEncoder, AcpResponsePayload,
};
use keencode_agent::{CollaborationIdGenerator, UuidCollaborationIdGenerator};
use keencode_resources::{
    ROOT_AGENT_ID, ReasoningEffortSnapshot, SessionEvent, SessionForkRequest as RuntimeForkRequest,
    SessionId, TurnStatus, TurnStopReason,
};
use keencode_runtime::{
    RuntimeError, RuntimeEventPayload, RuntimeEventReceiveError, RuntimeEventSubscription,
    RuntimeSession, RuntimeSnapshot,
};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Manager};

mod extensions;
mod file_changes;
mod mcp_oauth;

/// 在创建应用级 Registry 时绑定有回执的 ACP OAuth 通知接收器。
pub(crate) fn mcp_oauth_event_sink(
    app: &AppHandle,
) -> Arc<dyn crate::mcp_oauth::McpOAuthEventSink> {
    Arc::new(mcp_oauth::AcpOAuthEventSink::new(app))
}

#[cfg(test)]
mod tests;

/// 标准 ACP Host 只承诺实现协议版本 1。
const SUPPORTED_PROTOCOL_VERSION: schema::ProtocolVersion = schema::ProtocolVersion::V1;
/// `session/list` 单页的固定上限；客户端可用返回的 cursor 继续读取。
const SESSION_LIST_PAGE_SIZE: usize = 100;
/// ACP `_meta` 中可选的稳定创建操作标识。
const META_OPERATION_ID: &str = "keencode/operationId";
/// ACP `_meta` 中可选的精确 Turn 标识。
const META_TURN_ID: &str = "keencode/turnId";
/// ACP `_meta` 中可选的本轮 Ultra 开关。
const META_ULTRA_MODE: &str = "keencode/ultraMode";
/// ACP `_meta` 中可选的 Fork 标题。
const META_TITLE: &str = "keencode/title";
/// ACP 响应 `_meta` 中的最小 Session 快照键。
const META_SNAPSHOT: &str = "keencode/snapshot";
/// ACP 响应 `_meta` 中完整 `session/load` 历史恢复的最终游标事实。
const META_REPLAY: &str = "keencode/replay";
/// ACP 初始化响应 `_meta` 中的默认 Session cwd。
const META_DEFAULT_CWD: &str = "keencode/defaultCwd";
/// 标准 Session 配置项：Provider 与模型的可逆选择。
const CONFIG_MODEL_ID: &str = "model";
/// 未选择实际 Provider/模型时的显式空选择，不代表任何可调用模型。
const UNCONFIGURED_MODEL_ID: &str = "unconfigured";
/// 标准 Session 配置项：Provider 中立推理强度。
const CONFIG_REASONING_EFFORT_ID: &str = "reasoning_effort";
/// Runtime 当前支持的推理强度值，`ultra` 由独立 Ultra 元数据表达。
const REASONING_EFFORT_VALUES: &[(&str, &str)] = &[
    ("none", "None"),
    ("minimal", "Minimal"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Maximum"),
];
/// 只有根 Agent 的用户 Turn 才能作为标准 Prompt 的终态。
const ROOT_SOURCE_AGENT_ID: &str = ROOT_AGENT_ID;

/// 全局唯一的当前进程 ACP Host。
static ACP_HOST: OnceLock<Arc<AcpHost>> = OnceLock::new();

/// ACP 握手状态；协议版本只在成功 initialize 后固定。
#[derive(Default)]
struct HandshakeState {
    /// 已经完成握手的协议版本。
    protocol_version: Option<schema::ProtocolVersion>,
    /// 首次握手协商出的完整 Client 能力；重复握手必须完全一致。
    client_capabilities: Option<schema::ClientCapabilities>,
}

/// 协议方法执行失败时使用的固定安全错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostFailure {
    /// JSON-RPC 信封或方法名不符合规范。
    InvalidRequest,
    /// 请求参数不符合当前实现能力或值域。
    InvalidParams,
    /// 当前 Host 没有该方法。
    MethodNotFound,
    /// 需要先完成 ACP 握手。
    AuthRequired,
    /// 目标 Session 不存在、损坏或不属于当前授权根目录。
    ResourceNotFound,
    /// Session 绑定的连接配置已改变，当前请求未启动模型回合。
    ProviderConfigurationChanged,
    /// 当前 Session 或默认选择没有可用的 Provider/模型。
    ProviderNotConfigured,
    /// 当前 Provider 配置无法加载到注册表。
    ProviderReloadFailed,
    /// Runtime 或响应编码发生内部错误。
    Internal,
}

impl HostFailure {
    /// 转换为官方 ACP JSON-RPC 错误对象，不包含请求正文、路径细节或 Provider 输出。
    fn rpc_error(self) -> schema::Error {
        match self {
            Self::InvalidRequest => schema::Error::invalid_request(),
            Self::InvalidParams => schema::Error::invalid_params(),
            Self::MethodNotFound => schema::Error::method_not_found(),
            Self::AuthRequired => schema::Error::auth_required(),
            Self::ResourceNotFound => schema::Error::resource_not_found(None),
            Self::ProviderConfigurationChanged => schema::Error::internal_error()
                .data(serde_json::json!({"keencode/errorCode": "provider_configuration_changed"})),
            Self::ProviderNotConfigured => schema::Error::internal_error()
                .data(serde_json::json!({"keencode/errorCode": "provider_not_configured"})),
            Self::ProviderReloadFailed => schema::Error::internal_error()
                .data(serde_json::json!({"keencode/errorCode": "provider_reload_failed"})),
            Self::Internal => schema::Error::internal_error(),
        }
    }
}

/// 当前桌面进程中唯一的 ACP Host 实例。
struct AcpHost {
    /// 用于读取应用授权目录、本地设置和扩展状态的 Tauri 句柄。
    app: AppHandle,
    /// 唯一的 Session/Turn Runtime。
    runtime: Arc<AgentRuntime>,
    /// 严格解码 JSON-RPC 请求。
    decoder: AcpRequestDecoder,
    /// 封闭类型化响应编码器。
    encoder: AcpResponseEncoder,
    /// 握手后固定的协议版本。
    handshake: Mutex<HandshakeState>,
    /// 序列化 Session 创建、删除、Fork 与模式控制面的并发修改。
    control_gate: tokio::sync::Mutex<()>,
}

/// 安装当前应用唯一 ACP Host；必须在 Agent Runtime 已进入 Tauri State 后调用。
pub(crate) fn install(app: &AppHandle, runtime: Arc<AgentRuntime>) -> Result<(), String> {
    let host = Arc::new(AcpHost {
        app: app.clone(),
        runtime,
        decoder: AcpRequestDecoder::new(),
        encoder: AcpResponseEncoder::new(),
        handshake: Mutex::new(HandshakeState::default()),
        control_gate: tokio::sync::Mutex::new(()),
    });
    ACP_HOST
        .set(host)
        .map_err(|_| "ACP Host 已经初始化".to_owned())
}

/// Tauri 唯一 ACP 请求入口；标准通知成功或失败都不产生返回值。
#[tauri::command]
pub async fn acp_dispatch(message: serde_json::Value) -> Result<Option<serde_json::Value>, String> {
    let host = ACP_HOST
        .get()
        .ok_or_else(|| "ACP Host 尚未初始化".to_owned())?;
    host.dispatch(message).await
}

impl AcpHost {
    /// 严格解码并分发一个 JSON-RPC 值，同时尽可能原样保留合法请求 ID。
    async fn dispatch(&self, message: Value) -> Result<Option<Value>, String> {
        if looks_like_client_response(&message) {
            let response_json = serde_json::to_string(&message)
                .map_err(|_| "ACP Client Response 无法序列化".to_owned())?;
            crate::client_request::route_client_response(self.runtime.as_ref(), &response_json)?;
            return Ok(None);
        }
        let request_id = request_id_from_value(&message);
        let raw = serde_json::to_vec(&message).map_err(|_| "ACP 请求无法序列化".to_owned())?;
        let incoming = match self.decoder.decode_raw(&raw) {
            Ok(incoming) => incoming,
            Err(error) => {
                return self
                    .error_value(
                        request_id.unwrap_or(schema::RequestId::Null),
                        boundary_failure(error),
                    )
                    .map(Some);
            }
        };

        match incoming {
            AcpIncomingFrame::Request(frame) => {
                let (id, request) = frame.into_parts();
                if matches!(id, schema::RequestId::Null) {
                    return self.error_value(id, HostFailure::InvalidRequest).map(Some);
                }
                if !matches!(&request, AcpRequest::Initialize(_)) && !self.is_initialized() {
                    return self.error_value(id, HostFailure::AuthRequired).map(Some);
                }
                match self.dispatch_request(id.clone(), request).await {
                    Ok(value) => Ok(Some(value)),
                    Err(failure) => self.error_value(id, failure).map(Some),
                }
            }
            AcpIncomingFrame::Notification(notification) => {
                // 握手前的通知不改变 Host 状态；按 JSON-RPC 约定静默丢弃。
                if self.is_initialized() {
                    self.dispatch_notification(notification).await;
                }
                Ok(None)
            }
        }
    }

    /// 分发一个已严格解码且带请求 ID 的标准 ACP 请求。
    async fn dispatch_request(
        &self,
        id: schema::RequestId,
        request: AcpRequest,
    ) -> Result<Value, HostFailure> {
        match request {
            AcpRequest::Initialize(request) => {
                let response = self.handle_initialize(request)?;
                self.result_value(id, &response)
            }
            AcpRequest::Authenticate(_) => Err(HostFailure::InvalidParams),
            AcpRequest::NewSession(request) => {
                let response = self.handle_new_session(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::LoadSession(request) => {
                let response = self.handle_load_session(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::Prompt(request) => {
                let response = self.handle_prompt(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::DeleteSession(request) => {
                let response = self.handle_delete_session(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::SetSessionConfigOption(request) => {
                let response = self.handle_set_config_option(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::SetSessionMode(request) => {
                let response = self.handle_set_mode(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::ListSessions(request) => {
                let response = self.handle_list_sessions(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::ForkSession(request) => {
                let response = self.handle_fork_session(request).await?;
                self.result_value(id, &response)
            }
            AcpRequest::ReadFileChange(request) => {
                let response = file_changes::read(self, request).await?;
                self.result_value(id, &response)
            }
            // KeenCode 扩展由独立模块处理，标准 Host 只负责把已解码请求交给它。
            request => extensions::dispatch(self, id, request).await,
        }
    }

    /// 分发不产生响应的标准通知。
    async fn dispatch_notification(&self, notification: AcpNotification) {
        match notification {
            AcpNotification::Cancel(notification) => self.handle_cancel(notification).await,
            AcpNotification::SessionConfigUpdate(_) => {
                // 配置通知只作为 ACP 输入边界保留；配置刷新由现有 Tauri 控制面完成。
            }
        }
    }

    /// 返回当前是否已完成初始化握手。
    fn is_initialized(&self) -> bool {
        self.handshake
            .lock()
            .ok()
            .and_then(|state| state.protocol_version.clone())
            .is_some()
    }

    /// 完成一次只支持协议版本 1 的初始化握手。
    fn handle_initialize(
        &self,
        request: schema::InitializeRequest,
    ) -> Result<keencode_acp::InitializeResponseDto, HostFailure> {
        if request.protocol_version != SUPPORTED_PROTOCOL_VERSION
            && request.protocol_version != schema::ProtocolVersion::LATEST
        {
            return Err(HostFailure::InvalidParams);
        }
        let default_cwd = crate::workspace::app_data_session_root(&self.app)
            .map_err(|_| HostFailure::Internal)?
            .to_string_lossy()
            .into_owned();
        let mut state = self.handshake.lock().map_err(|_| HostFailure::Internal)?;
        if state.protocol_version.is_some() {
            if state.client_capabilities.as_ref() != Some(&request.client_capabilities) {
                return Err(HostFailure::InvalidParams);
            }
            self.runtime
                .elicitation_coordinator()
                .negotiate_client_capabilities(&request.client_capabilities)
                .map_err(|_| HostFailure::InvalidParams)?;
            return Ok(initialize_response(default_cwd));
        }
        self.runtime
            .elicitation_coordinator()
            .negotiate_client_capabilities(&request.client_capabilities)
            .map_err(|_| HostFailure::InvalidParams)?;
        let response = initialize_response(default_cwd);
        state.protocol_version = Some(SUPPORTED_PROTOCOL_VERSION);
        state.client_capabilities = Some(request.client_capabilities);
        Ok(response)
    }

    /// 创建新 Session，并在返回前建立唯一实时投递世代。
    async fn handle_new_session(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        reject_mcp_servers(&request.mcp_servers)?;
        let project_root = self.authorized_cwd(&request.cwd)?;
        let operation_id = operation_id(request.meta.as_ref())?;
        self.ensure_extensions(&project_root).await?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, None, &operation_id)
            .map_err(map_runtime_failure)?;
        let session_id = session.session_id().as_str().to_owned();
        self.runtime
            .focus_session(&session_id)
            .map_err(map_runtime_failure)?;
        self.runtime
            .ensure_session_delivery(&session_id)
            .map_err(map_runtime_failure)?;
        let snapshot = session.snapshot().map_err(|_| HostFailure::Internal)?;
        let config_options = self.config_options(&snapshot)?;
        Ok(
            schema::NewSessionResponse::new(schema::SessionId::new(session_id))
                .modes(session_mode_state(snapshot.state.plan.enabled))
                .config_options(config_options)
                .meta(Some(snapshot_meta(&self.app, &snapshot, None))),
        )
    }

    /// 加载既有 Session、校验 cwd 绑定，并在响应前完整投递历史。
    async fn handle_load_session(
        &self,
        request: schema::LoadSessionRequest,
    ) -> Result<schema::LoadSessionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        reject_mcp_servers(&request.mcp_servers)?;
        let session_id = request.session_id.0.as_ref().to_owned();
        let requested_root = self.authorized_cwd(&request.cwd)?;
        let (_, stored_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        if requested_root != stored_root {
            return Err(HostFailure::ResourceNotFound);
        }
        self.ensure_extensions(&stored_root).await?;
        let session = self
            .runtime
            .open_or_create_session(&stored_root, Some(&session_id), "acp-load")
            .map_err(map_runtime_failure)?;
        self.runtime
            .ensure_session_delivery(&session_id)
            .map_err(map_runtime_failure)?;
        // 标准 `session/load` 的响应必须建立在完整历史已经进入同一投递泵的
        // 事实之上；把最后一页控制结果写进 `_meta`，避免私有客户端再做第二次
        // 全量 replay。实时 catch-up 仍由独立的 `keencode/session/replay` 提供。
        let replay = self.replay_full_session(&session_id).await?;
        let snapshot = session.snapshot().map_err(|_| HostFailure::Internal)?;
        let config_options = self.config_options(&snapshot)?;
        let mut meta = snapshot_meta(&self.app, &snapshot, None);
        meta.insert(
            META_REPLAY.to_owned(),
            serde_json::to_value(&replay).map_err(|_| HostFailure::Internal)?,
        );
        Ok(schema::LoadSessionResponse::new()
            .modes(session_mode_state(snapshot.state.plan.enabled))
            .config_options(config_options)
            .meta(Some(meta)))
    }

    /// 分页投递既有 Session 的完整权威历史，并返回 `hasMore=false` 的末页事实。
    async fn replay_full_session(
        &self,
        session_id: &str,
    ) -> Result<keencode_acp::ReplaySessionResponse, HostFailure> {
        let mut after = None;
        loop {
            let page = self
                .runtime
                .replay_session(session_id, after, 1_000)
                .await
                .map_err(map_runtime_failure)?;
            if page.session_id != session_id {
                return Err(HostFailure::Internal);
            }
            if page.start_after != after.unwrap_or(0) {
                return Err(HostFailure::Internal);
            }
            if !page.has_more {
                return Ok(page);
            }
            if page.next_after <= after.unwrap_or(0) {
                return Err(HostFailure::Internal);
            }
            after = Some(page.next_after);
        }
    }

    /// 合并文本 Prompt、注入本轮开发者上下文，并等待根 Turn 权威终态。
    async fn handle_prompt(
        &self,
        request: schema::PromptRequest,
    ) -> Result<schema::PromptResponse, HostFailure> {
        let session_id = request.session_id.0.as_ref().to_owned();
        let text = prompt_text(request.prompt)?;
        let turn_id = prompt_turn_id(request.meta.as_ref())?;
        let ultra_mode = meta_bool(request.meta.as_ref(), META_ULTRA_MODE)?;
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-prompt")
            .map_err(map_runtime_failure)?;
        self.runtime
            .ensure_session_delivery(&session_id)
            .map_err(map_runtime_failure)?;
        self.ensure_extensions(&project_root).await?;
        let snapshot = session.snapshot().map_err(|_| HostFailure::Internal)?;
        let developer_context = self.developer_context(snapshot.state.plan.enabled, ultra_mode)?;
        // 必须先订阅，再调用 start_root_turn，避免 TurnCompleted 在响应等待前被错过。
        let mut subscription = session.subscribe().map_err(|_| HostFailure::Internal)?;
        self.runtime
            .start_root_turn(
                &session_id,
                &turn_id,
                &text,
                RootTurnOptions {
                    developer_context,
                    plan_enabled: snapshot.state.plan.enabled,
                },
            )
            .await
            .map_err(map_runtime_failure)?;
        let terminal = self
            .wait_for_turn_terminal(&session, &turn_id, &mut subscription)
            .await?;
        let final_snapshot = self
            .runtime
            .session_snapshot(&session_id)
            .map_err(map_runtime_failure)?;
        let stop_reason = prompt_stop_reason(&terminal)?;
        Ok(
            schema::PromptResponse::new(stop_reason).meta(Some(snapshot_meta(
                &self.app,
                &final_snapshot,
                Some(&turn_id),
            ))),
        )
    }

    /// 构造标准 Session 配置目录；只公开无凭据的 Provider、模型和推理强度。
    fn config_options(
        &self,
        snapshot: &RuntimeSnapshot,
    ) -> Result<Vec<schema::SessionConfigOption>, HostFailure> {
        let catalog = crate::acp_provider_catalog(&self.app).map_err(|_| HostFailure::Internal)?;
        let model_values = catalog
            .providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    // `providerId::modelId` 是前端现有合同；含分隔符的标识不发布，
                    // 避免客户端无法无歧义地还原 Provider 与模型。
                    .filter(|model| !provider.id.contains("::") && !model.contains("::"))
                    .map(|model| {
                        let value = format!("{}::{}", provider.id, model);
                        let name = format!("{} / {model}", provider.name);
                        schema::SessionConfigSelectOption::new(value, name)
                    })
            })
            .collect::<Vec<_>>();
        let mut options = Vec::with_capacity(2);
        let selected = match snapshot.state.provider.as_ref() {
            Some(provider) => Some((provider.provider_id.as_str(), provider.model.as_str())),
            None => match (
                catalog.active_provider_id.as_deref(),
                catalog.active_model_id.as_deref(),
            ) {
                (Some(provider), Some(model)) => Some((provider, model)),
                (None, None) => None,
                _ => return Err(HostFailure::Internal),
            },
        };
        let current_model = selected
            .map(|(provider, model)| {
                if provider.is_empty()
                    || model.is_empty()
                    || provider.contains("::")
                    || model.contains("::")
                {
                    return Err(HostFailure::Internal);
                }
                Ok(format!("{provider}::{model}"))
            })
            .transpose()?;
        options.push(model_config_option(current_model, model_values));

        let current_effort = snapshot
            .state
            .provider
            .as_ref()
            .and_then(|provider| provider.reasoning_effort)
            .map(reasoning_effort_name)
            .unwrap_or("none");
        let effort_values = REASONING_EFFORT_VALUES
            .iter()
            .map(|(value, name)| schema::SessionConfigSelectOption::new(*value, *name))
            .collect::<Vec<_>>();
        options.push(
            schema::SessionConfigOption::select(
                CONFIG_REASONING_EFFORT_ID,
                "Reasoning effort",
                current_effort,
                effort_values,
            )
            .description(Some(
                "Provider-neutral reasoning effort for this Session".to_owned(),
            ))
            .category(Some(schema::SessionConfigOptionCategory::ThoughtLevel)),
        );
        Ok(options)
    }

    /// 只接受当前 Provider 目录中已经公布的无歧义模型值。
    fn model_selection(&self, value: &str) -> Result<Option<(String, String)>, HostFailure> {
        if value.is_empty()
            || value.trim() != value
            || value.len() > 1_024
            || value.chars().any(char::is_control)
        {
            return Err(HostFailure::InvalidParams);
        }
        let Some((provider_id, model)) = value.split_once("::") else {
            return Ok(None);
        };
        if provider_id.is_empty()
            || model.is_empty()
            || model.contains("::")
            || provider_id.contains("::")
        {
            return Ok(None);
        }
        let catalog = crate::acp_provider_catalog(&self.app).map_err(|_| HostFailure::Internal)?;
        let known = catalog.providers.iter().any(|provider| {
            provider.id == provider_id && provider.models.iter().any(|known| known == model)
        });
        Ok(known.then(|| (provider_id.to_owned(), model.to_owned())))
    }

    /// 删除一个没有活动工作的持久 Session。
    async fn handle_delete_session(
        &self,
        request: keencode_acp::DeleteSessionRequest,
    ) -> Result<keencode_acp::DeleteSessionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        let session_id = request.session_id.0.as_ref().to_owned();
        let metadata = self
            .runtime
            .stored_sessions()
            .map_err(|_| HostFailure::Internal)?
            .into_iter()
            .find(|metadata| metadata.session_id.as_str() == session_id);
        // 删除响应可能在客户端丢失；已经不存在的合法目标须允许幂等重试。
        SessionId::new(session_id.clone()).map_err(|_| HostFailure::InvalidParams)?;
        let Some(metadata) = metadata else {
            return Ok(keencode_acp::DeleteSessionResponse::new());
        };
        if metadata.corrupt {
            return Err(HostFailure::ResourceNotFound);
        }
        let _ = crate::session_commands::authorize_stored_root(&self.app, &metadata.project_root)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        match self.runtime.runtime_manager().get(session_id.clone()) {
            Ok(session) => {
                if session
                    .has_active_work()
                    .map_err(|_| HostFailure::Internal)?
                {
                    return Err(HostFailure::InvalidParams);
                }
                drop(session);
                self.runtime
                    .close_session(&session_id)
                    .await
                    .map_err(map_runtime_failure)?;
            }
            Err(RuntimeError::SessionNotRegistered) => {}
            Err(_) => return Err(HostFailure::Internal),
        }
        retry_session_mutation(|| self.runtime.runtime_manager().delete(session_id.clone()))
            .await
            .map_err(|_| HostFailure::Internal)?;
        if self
            .runtime
            .focused_session_id()
            .map_err(|_| HostFailure::Internal)?
            .as_deref()
            == Some(session_id.as_str())
        {
            self.runtime.clear_focus();
        }
        Ok(keencode_acp::DeleteSessionResponse::new())
    }

    /// 通过标准配置项显式更新模型绑定或推理强度，拒绝活动会话和未知配置。
    async fn handle_set_config_option(
        &self,
        request: schema::SetSessionConfigOptionRequest,
    ) -> Result<schema::SetSessionConfigOptionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        let session_id = request.session_id.0.as_ref().to_owned();
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-config")
            .map_err(map_runtime_failure)?;
        if session
            .has_active_work()
            .map_err(|_| HostFailure::Internal)?
        {
            return Err(HostFailure::InvalidParams);
        }
        let operation_id = operation_id(request.meta.as_ref())?;
        let config_id = request.config_id.0.as_ref();
        let value = request.value.0.as_ref();
        match config_id {
            CONFIG_MODEL_ID => {
                if value == UNCONFIGURED_MODEL_ID {
                    // `unconfigured` 只是只读配置列表中表达“尚未选择”的占位值，
                    // 不是 Runtime 可执行的 Provider 目标；不为它伪造控制事件或收据。
                    return Err(HostFailure::InvalidParams);
                }
                let (provider_id, model) = self
                    .model_selection(value)?
                    .ok_or(HostFailure::InvalidParams)?;
                self.runtime
                    .set_session_model(&session_id, &operation_id, &provider_id, &model)
                    .map_err(map_runtime_failure)?;
            }
            CONFIG_REASONING_EFFORT_ID => {
                if !REASONING_EFFORT_VALUES
                    .iter()
                    .any(|(identifier, _)| *identifier == value)
                {
                    return Err(HostFailure::InvalidParams);
                }
                self.runtime
                    .set_session_effort(&session_id, &operation_id, value)
                    .map_err(map_runtime_failure)?;
            }
            _ => return Err(HostFailure::InvalidParams),
        }
        let updated = self
            .runtime
            .session_snapshot(&session_id)
            .map_err(map_runtime_failure)?;
        Ok(
            schema::SetSessionConfigOptionResponse::new(self.config_options(&updated)?)
                .meta(Some(snapshot_meta(&self.app, &updated, None))),
        )
    }

    /// 按持久 Plan 状态实现标准 `session/set_mode`。
    async fn handle_set_mode(
        &self,
        request: schema::SetSessionModeRequest,
    ) -> Result<schema::SetSessionModeResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        let session_id = request.session_id.0.as_ref().to_owned();
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-mode")
            .map_err(map_runtime_failure)?;
        let snapshot = session.snapshot().map_err(|_| HostFailure::Internal)?;
        let modes = session_mode_state(snapshot.state.plan.enabled);
        keencode_acp::validate_set_session_mode_request(&request, &modes)
            .map_err(|_| HostFailure::InvalidParams)?;
        let mode_id = request.mode_id.0.as_ref();
        let operation_id = operation_id(request.meta.as_ref())?;
        let requested_plan_enabled = mode_id == "plan";

        // 先从权威 Journal 对账显式 operationId。响应丢失后，即使当前模式已经被
        // 后续操作改变，也必须返回原操作的成功事实，而不能再次提交或静默复用
        // 当前快照伪装成原响应。
        if let Some(record) = session
            .committed_control_event(&operation_id)
            .map_err(|_| HostFailure::Internal)?
        {
            let same_request = matches!(
                &record.event,
                SessionEvent::PlanChanged { plan } if plan.enabled == requested_plan_enabled
            );
            if !same_request {
                return Err(HostFailure::InvalidParams);
            }
            let current = session.snapshot().map_err(|_| HostFailure::Internal)?;
            return Ok(schema::SetSessionModeResponse::new()
                .meta(Some(snapshot_meta(&self.app, &current, None))));
        }

        if session
            .has_active_work()
            .map_err(|_| HostFailure::Internal)?
        {
            return Err(HostFailure::InvalidParams);
        }
        let mut plan = snapshot.state.plan.clone();
        plan.enabled = requested_plan_enabled;
        session
            .set_plan(&operation_id, plan)
            .map_err(|_| HostFailure::Internal)?;
        let updated = session.snapshot().map_err(|_| HostFailure::Internal)?;
        Ok(schema::SetSessionModeResponse::new()
            .meta(Some(snapshot_meta(&self.app, &updated, None))))
    }

    /// 列出当前授权范围内的健康持久 Session，并支持固定大小的偏移游标。
    async fn handle_list_sessions(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        let cwd_filter = request
            .cwd
            .as_deref()
            .map(|path| self.authorized_cwd(path))
            .transpose()?;
        let start = parse_cursor(request.cursor.as_deref())?;
        let mut sessions = Vec::new();
        for metadata in self
            .runtime
            .stored_sessions()
            .map_err(|_| HostFailure::Internal)?
        {
            if metadata.corrupt {
                continue;
            }
            let Ok(root) =
                crate::session_commands::authorize_stored_root(&self.app, &metadata.project_root)
            else {
                continue;
            };
            if cwd_filter.as_ref().is_some_and(|filter| filter != &root) {
                continue;
            }
            let updated_at = crate::session_commands::rfc3339_from_ms(metadata.updated_at_unix_ms)
                .map_err(|_| HostFailure::Internal)?;
            sessions.push(
                schema::SessionInfo::new(
                    schema::SessionId::new(metadata.session_id.as_str().to_owned()),
                    root,
                )
                .title(Some(metadata.title))
                .updated_at(Some(updated_at)),
            );
        }
        if start > sessions.len() {
            return Err(HostFailure::InvalidParams);
        }
        let end = start
            .saturating_add(SESSION_LIST_PAGE_SIZE)
            .min(sessions.len());
        let page = sessions[start..end].to_vec();
        let next_cursor = (end < sessions.len()).then(|| end.to_string());
        Ok(schema::ListSessionsResponse::new(page).next_cursor(next_cursor))
    }

    /// Fork 一个空闲 Session，并恢复源 Session 的运行时所有权。
    async fn handle_fork_session(
        &self,
        request: schema::ForkSessionRequest,
    ) -> Result<schema::ForkSessionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        reject_mcp_servers(&request.mcp_servers)?;
        let source_id = request.session_id.0.as_ref().to_owned();
        let requested_root = self.authorized_cwd(&request.cwd)?;
        let (_, source_root) = authorized_metadata(&self.runtime, &self.app, &source_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        if requested_root != source_root {
            return Err(HostFailure::ResourceNotFound);
        }
        let operation_id = operation_id(request.meta.as_ref())?;
        let title = meta_text(request.meta.as_ref(), META_TITLE, 512)?;
        let source = self
            .runtime
            .open_or_create_session(&source_root, Some(&source_id), "acp-fork")
            .map_err(map_runtime_failure)?;
        if source
            .has_active_work()
            .map_err(|_| HostFailure::Internal)?
        {
            return Err(HostFailure::InvalidParams);
        }
        drop(source);
        let context = close_session_for_mutation(&self.runtime, &self.app, &source_id)
            .await
            .map_err(|_| HostFailure::Internal)?;
        let runtime_request = RuntimeForkRequest {
            source_session_id: SessionId::new(source_id.clone())
                .map_err(|_| HostFailure::InvalidParams)?,
            operation_id,
            title,
        };
        let fork_result = retry_session_mutation(|| {
            self.runtime
                .runtime_manager()
                .fork_closed_session(runtime_request.clone())
        })
        .await;
        let restored = restore_session_after_mutation(&self.runtime, &source_id, &context);
        if restored.is_err() {
            return Err(HostFailure::Internal);
        }
        let fork_result = fork_result.map_err(|_| HostFailure::Internal)?;
        let target_id = fork_result.session_id.as_str().to_owned();
        let target = self
            .runtime
            .open_or_create_session(&context.project_root, Some(&target_id), "acp-fork-open")
            .map_err(map_runtime_failure)?;
        self.runtime
            .ensure_session_delivery(&target_id)
            .map_err(map_runtime_failure)?;
        let snapshot = target.snapshot().map_err(|_| HostFailure::Internal)?;
        let config_options = self.config_options(&snapshot)?;
        Ok(
            schema::ForkSessionResponse::new(schema::SessionId::new(target_id))
                .modes(session_mode_state(snapshot.state.plan.enabled))
                .config_options(config_options)
                .meta(Some(snapshot_meta(&self.app, &snapshot, None))),
        )
    }

    /// 按标准 `session/cancel` 语义只向精确根 Turn 发出取消令牌。
    async fn handle_cancel(&self, notification: schema::CancelNotification) {
        let session_id = notification.session_id.0.as_ref().to_owned();
        let explicit_turn = match meta_string(notification.meta.as_ref(), META_TURN_ID) {
            Ok(turn) => turn,
            Err(_) => return,
        };
        let turn_id = match explicit_turn {
            Some(turn) => Some(turn),
            None => self
                .runtime
                .session_snapshot(&session_id)
                .ok()
                .and_then(|snapshot| active_root_turn(&snapshot)),
        };
        if let Some(turn_id) = turn_id {
            let _ = self.runtime.cancel_turn(&session_id, &turn_id);
        }
    }

    /// 等待指定根 Turn 的权威终态；慢订阅者 Lag 后回到 Snapshot 检查。
    async fn wait_for_turn_terminal(
        &self,
        session: &RuntimeSession,
        turn_id: &str,
        subscription: &mut RuntimeEventSubscription,
    ) -> Result<TerminalTurn, HostFailure> {
        if let Some(terminal) = self.snapshot_terminal(session, turn_id)? {
            return Ok(terminal);
        }
        loop {
            match subscription.recv().await {
                Ok(delivery) => {
                    let should_check = match delivery.payload {
                        RuntimeEventPayload::Authoritative(record) => {
                            authoritative_turn_terminal(&record.event, turn_id)
                        }
                        RuntimeEventPayload::Control(_) => true,
                        RuntimeEventPayload::Transient(_) => false,
                    };
                    if should_check
                        && let Some(terminal) = self.snapshot_terminal(session, turn_id)?
                    {
                        return Ok(terminal);
                    }
                }
                Err(RuntimeEventReceiveError::Lagged(_)) => {
                    if let Some(terminal) = self.snapshot_terminal(session, turn_id)? {
                        return Ok(terminal);
                    }
                }
                Err(RuntimeEventReceiveError::Closed) => {
                    return self
                        .snapshot_terminal(session, turn_id)?
                        .ok_or(HostFailure::Internal);
                }
            }
        }
    }

    /// 从当前 Session Snapshot 读取指定 Turn 的终态。
    fn snapshot_terminal(
        &self,
        session: &RuntimeSession,
        turn_id: &str,
    ) -> Result<Option<TerminalTurn>, HostFailure> {
        let snapshot = session.snapshot().map_err(|_| HostFailure::Internal)?;
        Ok(snapshot.state.turns.values().find_map(|turn| {
            if turn.turn_id.as_str() != turn_id
                || turn.source_agent_id.as_str() != ROOT_SOURCE_AGENT_ID
                || turn.parent_turn_id.is_some()
                || turn.status == TurnStatus::Running
            {
                return None;
            }
            Some(TerminalTurn {
                status: turn.status.clone(),
                stop_reason: turn.stop_reason,
            })
        }))
    }

    /// 在调用方的可靠授权范围内规范化 cwd。
    fn authorized_cwd(&self, path: &Path) -> Result<PathBuf, HostFailure> {
        if !path.is_absolute() {
            return Err(HostFailure::InvalidParams);
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| HostFailure::InvalidParams)?;
        if !canonical.is_dir() {
            return Err(HostFailure::InvalidParams);
        }
        let text = canonical.to_str().ok_or(HostFailure::InvalidParams)?;
        crate::session_commands::authorize_stored_root(&self.app, text)
            .map_err(|_| HostFailure::InvalidParams)
    }

    /// 按项目根刷新本地 Skills、MCP、插件和 LSP 候选。
    async fn ensure_extensions(&self, project_root: &Path) -> Result<(), HostFailure> {
        crate::extensions::ensure_runtime_extension_candidate(
            &self.app,
            project_root,
            &self.runtime,
            false,
        )
        .await
        .map(|_| ())
        .map_err(|_| HostFailure::Internal)
    }

    /// 读取本地记忆、持久 Plan 和本轮 Ultra 的动态开发者上下文。
    fn developer_context(
        &self,
        plan_enabled: bool,
        ultra_mode: bool,
    ) -> Result<Option<String>, HostFailure> {
        let mut contexts = Vec::new();
        if let Some(memories) = self.app.try_state::<Arc<crate::memories::MemoryService>>() {
            let settings =
                crate::app_settings::get(&self.app).map_err(|_| HostFailure::Internal)?;
            if let Some(memory) = memories
                .prompt_context(settings.local_memories, settings.interface_language)
                .map_err(|_| HostFailure::Internal)?
            {
                contexts.push(memory);
            }
        }
        if plan_enabled {
            contexts.push(PLAN_MODE_CONTRACT_EN.to_owned());
        }
        if ultra_mode {
            contexts.push(ULTRA_MODE_CONTRACT_EN.to_owned());
        }
        Ok((!contexts.is_empty()).then(|| contexts.join("\n\n")))
    }

    /// 使用封闭 ACP ResponsePayload 编码结果，再恢复为 Tauri JSON Value。
    fn result_value<T>(&self, id: schema::RequestId, result: &T) -> Result<Value, HostFailure>
    where
        T: AcpResponsePayload,
    {
        let raw = self
            .encoder
            .encode_result(id, result)
            .map_err(|_| HostFailure::Internal)?;
        serde_json::from_slice(&raw).map_err(|_| HostFailure::Internal)
    }

    /// 使用官方 ACP 错误编码器生成一个完整响应。
    fn error_value(&self, id: schema::RequestId, failure: HostFailure) -> Result<Value, String> {
        let raw = self
            .encoder
            .encode_error(id, &failure.rpc_error())
            .map_err(|_| "ACP 错误响应无法序列化".to_owned())?;
        serde_json::from_slice(&raw).map_err(|_| "ACP 错误响应无法恢复".to_owned())
    }
}

/// 一个根 Turn 的终态快照。
struct TerminalTurn {
    /// Runtime 归约后的粗粒度状态。
    status: TurnStatus,
    /// 非正常终态的精确资源层原因。
    stop_reason: Option<TurnStopReason>,
}

/// 如实发布实际选择；未配置或已不在目录中的模型不能被列表第一项偷偷替换。
fn model_config_option(
    current: Option<String>,
    mut values: Vec<schema::SessionConfigSelectOption>,
) -> schema::SessionConfigOption {
    let current = current.unwrap_or_else(|| UNCONFIGURED_MODEL_ID.to_owned());
    if !values
        .iter()
        .any(|option| option.value.0.as_ref() == current)
    {
        let name = if current == UNCONFIGURED_MODEL_ID {
            "未配置模型".to_owned()
        } else {
            format!("{current}（当前选择暂不可用）")
        };
        values.insert(
            0,
            schema::SessionConfigSelectOption::new(current.clone(), name),
        );
    }
    schema::SessionConfigOption::select(CONFIG_MODEL_ID, "Model", current, values)
        .description(Some("Provider and model used by this Session".to_owned()))
        .category(Some(schema::SessionConfigOptionCategory::Model))
}

/// 将权威模型终态保真映射为 ACP 停止原因；执行故障仍使用 JSON-RPC 错误。
fn prompt_stop_reason(terminal: &TerminalTurn) -> Result<schema::StopReason, HostFailure> {
    match (&terminal.status, terminal.stop_reason) {
        (TurnStatus::Completed, None) => Ok(schema::StopReason::EndTurn),
        (TurnStatus::Cancelled, Some(TurnStopReason::Cancelled)) => {
            Ok(schema::StopReason::Cancelled)
        }
        (TurnStatus::Failed, Some(TurnStopReason::LimitReached)) => {
            Ok(schema::StopReason::MaxTurnRequests)
        }
        (TurnStatus::Failed, Some(TurnStopReason::ModelOutputLimit)) => {
            Ok(schema::StopReason::MaxTokens)
        }
        (TurnStatus::Failed, Some(TurnStopReason::ModelRefusal)) => Ok(schema::StopReason::Refusal),
        _ => Err(HostFailure::Internal),
    }
}

/// 从原始 JSON-RPC 值中尽量恢复合法请求 ID；非法 ID 必须返回 null 或不响应。
fn request_id_from_value(message: &Value) -> Option<schema::RequestId> {
    message
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(|id| serde_json::from_value(id.clone()).ok())
}

/// 判断一个没有 `method` 的输入是否应进入 Client Response 路由。
///
/// 只要出现 `result` 或 `error` 就不再尝试按 Agent 请求解码；完整响应
/// 信封和对应 DTO 仍由现有 Client Response Handler 继续严格校验。
fn looks_like_client_response(message: &Value) -> bool {
    let Some(object) = message.as_object() else {
        return false;
    };
    !object.contains_key("method")
        && (object.contains_key("result") || object.contains_key("error"))
}

/// 生成首次和重复 ACP 握手完全一致的能力响应，并提供可直接新建 Session 的默认 cwd。
fn initialize_response(default_cwd: String) -> keencode_acp::InitializeResponseDto {
    let session_capabilities = keencode_acp::InitializeSessionCapabilitiesDto::new()
        .list(Some(schema::SessionListCapabilities::new()))
        .fork(Some(schema::SessionForkCapabilities::new()));
    let capabilities = keencode_acp::InitializeAgentCapabilitiesDto::new()
        .load_session(true)
        .prompt_capabilities(schema::PromptCapabilities::default())
        .mcp_capabilities(schema::McpCapabilities::default())
        .session_capabilities(session_capabilities);
    let mut meta = Map::new();
    meta.insert(META_DEFAULT_CWD.to_owned(), Value::String(default_cwd));
    keencode_acp::InitializeResponseDto::new(SUPPORTED_PROTOCOL_VERSION)
        .agent_capabilities(capabilities)
        .agent_info(Some(schema::Implementation::new("KeenCode", "0.0.1")))
        .meta(Some(meta))
}

/// 把持久化推理强度转换成标准 Session 配置值。
fn reasoning_effort_name(effort: ReasoningEffortSnapshot) -> &'static str {
    match effort {
        ReasoningEffortSnapshot::Minimal => "minimal",
        ReasoningEffortSnapshot::Low => "low",
        ReasoningEffortSnapshot::Medium => "medium",
        ReasoningEffortSnapshot::High => "high",
        ReasoningEffortSnapshot::ExtraHigh => "xhigh",
        ReasoningEffortSnapshot::Maximum => "max",
    }
}

/// 将 ACP 解码边界错误映射为安全 JSON-RPC 分类。
fn boundary_failure(error: AcpBoundaryError) -> HostFailure {
    match error {
        AcpBoundaryError::UnknownMethod => HostFailure::MethodNotFound,
        AcpBoundaryError::InvalidMethod => HostFailure::InvalidRequest,
        _ => HostFailure::InvalidParams,
    }
}

/// Runtime 错误的公开 ACP 映射；不使用内部错误正文。
fn map_runtime_failure(error: AgentRuntimeError) -> HostFailure {
    match error {
        AgentRuntimeError::InvalidSession => HostFailure::InvalidParams,
        AgentRuntimeError::SessionUnavailable | AgentRuntimeError::SessionProjectMismatch => {
            HostFailure::ResourceNotFound
        }
        AgentRuntimeError::ProviderNotConfigured => HostFailure::ProviderNotConfigured,
        AgentRuntimeError::ProviderConfigurationChanged => {
            HostFailure::ProviderConfigurationChanged
        }
        AgentRuntimeError::ProviderReloadFailed => HostFailure::ProviderReloadFailed,
        _ => HostFailure::Internal,
    }
}

/// 标准 New/Load/Fork 不接受 ACP 请求携带的 MCP Server 配置。
fn reject_mcp_servers(servers: &[schema::McpServer]) -> Result<(), HostFailure> {
    if servers.is_empty() {
        Ok(())
    } else {
        Err(HostFailure::InvalidParams)
    }
}

/// 把 ACP Prompt 中的文本块按顺序合并；未声明能力的内容必须显式拒绝。
fn prompt_text(blocks: Vec<schema::ContentBlock>) -> Result<String, HostFailure> {
    let mut texts = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            schema::ContentBlock::Text(text) => texts.push(text.text),
            _ => return Err(HostFailure::InvalidParams),
        }
    }
    let text = texts.join("\n");
    if text.trim().is_empty() {
        return Err(HostFailure::InvalidParams);
    }
    Ok(text)
}

/// 从保留元数据中读取一个有界字符串。
fn meta_string(meta: Option<&schema::Meta>, key: &str) -> Result<Option<String>, HostFailure> {
    meta_text(meta, key, 128)
}

/// 从保留元数据中读取指定上限的非空字符串。
fn meta_text(
    meta: Option<&schema::Meta>,
    key: &str,
    maximum_bytes: usize,
) -> Result<Option<String>, HostFailure> {
    let Some(value) = meta.and_then(|meta| meta.get(key)) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(HostFailure::InvalidParams);
    };
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(HostFailure::InvalidParams);
    }
    Ok(Some(value.to_owned()))
}

/// 从保留元数据中读取一个严格布尔值。
fn meta_bool(meta: Option<&schema::Meta>, key: &str) -> Result<bool, HostFailure> {
    let Some(value) = meta.and_then(|meta| meta.get(key)) else {
        return Ok(false);
    };
    value.as_bool().ok_or(HostFailure::InvalidParams)
}

/// 解析本 Host 自己生成的十进制偏移游标。
fn parse_cursor(cursor: Option<&str>) -> Result<usize, HostFailure> {
    cursor
        .map(|cursor| {
            if cursor.is_empty() || cursor.trim() != cursor {
                return Err(HostFailure::InvalidParams);
            }
            cursor
                .parse::<usize>()
                .map_err(|_| HostFailure::InvalidParams)
        })
        .transpose()
        .map(|cursor| cursor.unwrap_or(0))
}

/// 生成标准响应可携带的最小、无正文 Session 快照。
fn snapshot_meta(
    app: &AppHandle,
    snapshot: &RuntimeSnapshot,
    turn_id: Option<&str>,
) -> schema::Meta {
    let mut meta = Map::new();
    let active_turn_id = active_root_turn(snapshot);
    let state = if snapshot.closed {
        "disconnected"
    } else if active_turn_id.is_some() {
        "streaming"
    } else {
        "ready"
    };
    let last_error = snapshot
        .recovery_required
        .then(|| "Session 需要恢复后才能继续".to_owned());
    let diagnostics_path = app
        .try_state::<Arc<crate::diagnostics::Diagnostics>>()
        .map(|diagnostics| diagnostics.path().to_string_lossy().into_owned());
    let snapshot_value = serde_json::json!({
        "sessionId": snapshot.state.session_id.as_str(),
        "state": state,
        "activeTurnId": active_turn_id,
        "backend": "acp",
        "projectPath": &snapshot.state.project_root,
        "title": &snapshot.state.title,
        "lastError": last_error,
        "diagnosticsPath": diagnostics_path,
    });
    meta.insert(META_SNAPSHOT.to_owned(), snapshot_value);
    if let Some(turn_id) = turn_id {
        meta.insert(META_TURN_ID.to_owned(), Value::String(turn_id.to_owned()));
    }
    meta
}

/// 读取显式业务身份；未提供时为本次合法请求分配新的随机操作标识。
fn operation_id(meta: Option<&schema::Meta>) -> Result<String, HostFailure> {
    match meta_string(meta, META_OPERATION_ID)? {
        Some(value) => Ok(value),
        None => Ok(format!(
            "operation-{}",
            UuidCollaborationIdGenerator.next_message_id()
        )),
    }
}

/// 为未携带私有 Turn 元数据的 Prompt 分配一次性随机 Turn 标识。
///
/// JSON-RPC ID 只负责响应关联；同一客户端重连后可能复用该 ID，不能将它
/// 持久化为 Session 内的业务身份。
fn default_turn_id() -> String {
    format!("turn-{}", UuidCollaborationIdGenerator.next_message_id())
}

/// 读取显式 Turn 身份，未提供时为本次 Prompt 生成新的 Turn 标识。
fn prompt_turn_id(meta: Option<&schema::Meta>) -> Result<String, HostFailure> {
    meta_string(meta, META_TURN_ID).map(|turn_id| turn_id.unwrap_or_else(default_turn_id))
}

/// 返回当前运行中的根 Turn，供无 TurnId 的取消通知选择唯一目标。
fn active_root_turn(snapshot: &RuntimeSnapshot) -> Option<String> {
    snapshot
        .state
        .turns
        .values()
        .filter(|turn| {
            turn.source_agent_id.as_str() == ROOT_SOURCE_AGENT_ID
                && turn.parent_turn_id.is_none()
                && turn.status == TurnStatus::Running
        })
        .max_by_key(|turn| turn.started_at_unix_ms)
        .map(|turn| turn.turn_id.as_str().to_owned())
}

/// 判断一条权威事件是否包含目标根 Turn 终态。
fn authoritative_turn_terminal(event: &SessionEvent, turn_id: &str) -> bool {
    match event {
        SessionEvent::TurnCompleted { turn_id: completed } => completed.as_str() == turn_id,
        SessionEvent::TurnStopped {
            turn_id: stopped, ..
        } => stopped.as_str() == turn_id,
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .any(|event| authoritative_turn_terminal(event, turn_id)),
        _ => false,
    }
}
