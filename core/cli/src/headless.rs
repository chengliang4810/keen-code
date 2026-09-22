//! 独立 headless Host 的组合根。
//!
//! ownership、discovery 和 admission 事实来自 `keencode-runtime`；本模块只把它们
//! 接到 CLI 的本地 NDJSON server。模型执行复用平台无关的 AgentRunner、Provider
//! Registry 和 Session Runtime；Desktop 的 Tauri 事件、密钥库和界面投影仍留在
//! desktop adapter。

use crate::server::{
    FixedIdlePolicy, HostActivity, HostDispatch, HostDispatchError, HostDispatchFuture,
    LocalHostServer, LocalHostServerConfig, LocalHostServerError, LocalHostServerHandle,
};
use keencode_acp::{
    AcpIncomingFrame, ConnectionId, HostDiscoveryRecord, HostLifecyclePhase, HostOwnerKind,
    HostTransportKind, KeenCodeEvent, KeenCodeEventEnvelope, KeenCodeEventEnvelopeParams,
    MAX_HOST_PROMPT_BYTES, OperationId, SessionUpdateDeliveryEnvelope,
};
use keencode_agent::{
    AgentId, AgentRunner, AgentStreamEvent, AgentStreamEventKind, ContextCompactionFailureKind,
    PlanGuard, RunLimits, SessionId as AgentSessionId, TerminalReason, ToolRegistry,
    TurnCancellation, TurnId as AgentTurnId, TurnRequest, TurnResult,
};
use keencode_model::{
    ContentBlock, ImageSource, Message, MessageRole, ModelProvider, ModelStreamEvent,
    ProviderCapabilities, ProviderProtocol, last_non_empty_text,
};
use keencode_provider::{
    ApiKey, ProviderConfig, ProviderModelPolicy, ProviderRegistration, ProviderRegistry,
    ResolvedProvider, WireResponseMode,
};
use keencode_resources::{
    MessageRole as ResourceMessageRole, ProviderProtocolSnapshot, ProviderSnapshot, ROOT_AGENT_ID,
    SessionEvent, SessionEventRecord, SessionMessage, SessionState, StoredSessionMetadata,
    ToolCompletionStatus as ResourceToolCompletionStatus, ToolLifecycle, ToolRequest,
    TurnStopReason,
};
use keencode_runtime::{
    AdmissionDisposition, CreateSessionRequest, ElicitationAnswerDisposition, ExecutionIdentity,
    HostRuntime, HostRuntimeAcquire, HostRuntimeError, OpenSessionResult, OperationState,
    OperationStatus, OperationTerminal, PromptAdmissionRequest, RuntimeError, RuntimeEventPayload,
    RuntimeEventReceiveError, RuntimeEventSubscription, RuntimeManager, RuntimeSession,
    RuntimeTurnRequest,
};
use keencode_tools::{ToolEnvironment, register_local_tools};
use keencode_web::{
    HostBusinessError, HostBusinessFuture, HostBusinessRouter, HostConnectionContext,
    HostWsAdapter, WebError, WebHost, WebHostConfig, WebServerOwner, WebToken,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

static NEXT_HEADLESS_ID: AtomicU64 = AtomicU64::new(1);

/// 独立 headless Host 的运行句柄。
pub struct HeadlessHost {
    runtime: Arc<HostRuntime>,
    backend: Arc<HeadlessInner>,
    server: Option<LocalHostServerHandle>,
}

impl fmt::Debug for HeadlessHost {
    /// 只展示不含路径和端点的 Host 身份。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeadlessHost")
            .field("host_id", &self.runtime.host_id())
            .field("phase", &self.runtime.phase().ok())
            .finish()
    }
}

impl HeadlessHost {
    /// 创建数据根、取得 headless owner、绑定 IPC，并在最后发布 discovery。
    pub async fn start(data_root: impl Into<PathBuf>) -> Result<Self, HeadlessHostError> {
        let data_root = data_root.into();
        fs::create_dir_all(&data_root).map_err(|error| HeadlessHostError::io(error.to_string()))?;
        let acquired = HostRuntime::acquire(&data_root, HostOwnerKind::Headless)
            .map_err(HeadlessHostError::runtime)?;
        let runtime = match acquired {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(runtime) => {
                let _ = runtime;
                return Err(HeadlessHostError::already_owned());
            }
        };
        let (transport, endpoint) =
            endpoint_for(runtime.data_root(), runtime.data_root_fingerprint());
        cleanup_stale_endpoint(&transport, &endpoint);
        let record = HostDiscoveryRecord::new_with_host_id(
            transport,
            endpoint.clone(),
            HostOwnerKind::Headless,
            std::process::id(),
            runtime.host_id().to_owned(),
            runtime.data_root_fingerprint().to_owned(),
            "starting",
        )
        .map_err(|error| HeadlessHostError::protocol(error.to_string()))?;
        let provider = headless_provider_from_environment().map_err(HeadlessHostError::provider)?;
        let inner = Arc::new(
            HeadlessInner::new(Arc::clone(&runtime), provider)
                .map_err(HeadlessHostError::runtime_operation)?,
        );
        let dispatch: Arc<dyn HostDispatch> = inner.clone();
        let activity: Arc<dyn HostActivity> = inner.clone();
        let server = LocalHostServer::bind(
            &record,
            LocalHostServerConfig {
                idle_policy: Some(Arc::new(FixedIdlePolicy::thirty_seconds())),
                ..LocalHostServerConfig::default()
            },
            dispatch,
            activity,
        )
        .await
        .map_err(HeadlessHostError::transport)?;
        if let Err(error) = runtime.mark_ready_and_publish(transport, endpoint) {
            drop(server);
            return Err(HeadlessHostError::runtime(error));
        }
        let server = server.spawn();
        Ok(Self {
            runtime,
            backend: inner,
            server: Some(server),
        })
    }

    /// 返回 Host 数据根指纹。
    pub fn data_root_fingerprint(&self) -> &str {
        self.runtime.data_root_fingerprint()
    }

    /// 返回 Host 实例 ID。
    pub fn host_id(&self) -> &str {
        self.runtime.host_id()
    }

    /// 返回 Host 当前生命周期 phase。
    pub fn phase(&self) -> Result<HostLifecyclePhase, HeadlessHostError> {
        self.runtime.phase().map_err(HeadlessHostError::runtime)
    }

    /// 等待 idle policy 或 server 错误，然后执行 owner shutdown 并清理 discovery。
    pub async fn wait(mut self) -> Result<(), HeadlessHostError> {
        let server = self
            .server
            .take()
            .ok_or_else(|| HeadlessHostError::shutdown("server handle 已被消费"))?;
        let server_result = server
            .wait_for_ctrl_c()
            .await
            .map_err(HeadlessHostError::transport);
        let web_result = self
            .backend
            .shutdown_web()
            .await
            .map_err(HeadlessHostError::web);
        let backend_result = self
            .backend
            .shutdown()
            .map_err(HeadlessHostError::runtime_operation);
        let shutdown_result = self
            .runtime
            .explicit_shutdown()
            .map(|_| ())
            .map_err(HeadlessHostError::runtime);
        server_result?;
        web_result?;
        backend_result?;
        shutdown_result
    }
}

/// headless Host 启动、transport 或 owner shutdown 错误。
#[derive(Debug)]
pub struct HeadlessHostError {
    message: String,
    code: crate::ExitCode,
}

impl HeadlessHostError {
    fn io(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: crate::ExitCode::HostUnavailable,
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: crate::ExitCode::HostUnavailable,
        }
    }

    fn transport(error: LocalHostServerError) -> Self {
        Self::io(error.to_string())
    }

    fn runtime(error: HostRuntimeError) -> Self {
        Self::io(error.to_string())
    }

    fn runtime_operation(error: RuntimeError) -> Self {
        Self::io(error.to_string())
    }

    fn web(error: WebError) -> Self {
        // Web transport 错误可能包含本地路径；headless CLI 只返回稳定摘要。
        let message = match error {
            WebError::Bind(_) => "Web Host 监听失败",
            WebError::Internal(_) => "Web Host 停止失败",
            _ => "Web Host 生命周期操作失败",
        };
        Self::io(message)
    }

    fn provider(_error: HeadlessProviderError) -> Self {
        // Provider 配置错误不能回显环境变量或上游错误正文；具体诊断只保留在
        // 调用方的受控开发日志中，CLI stderr 仅展示稳定摘要。
        Self::io("headless Provider 配置无效或未完成")
    }

    fn already_owned() -> Self {
        Self::io("当前数据根已经由其他 Host 持有")
    }

    fn shutdown(message: impl Into<String>) -> Self {
        Self::io(message)
    }

    /// 返回稳定 CLI 退出码。
    pub const fn exit_code(&self) -> crate::ExitCode {
        self.code
    }
}

impl fmt::Display for HeadlessHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HeadlessHostError {}

/// headless Provider 的安全配置错误；不携带环境变量正文或 API Key。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeadlessProviderError {
    /// 未同时提供基础地址和模型标识。
    MissingConfiguration,
    /// 协议、地址、模型或注册表校验失败。
    InvalidConfiguration,
}

/// 只从显式环境变量构造的 Provider 绑定。
///
/// API Key 只存在于进程内 ProviderClient 和这一份可清零的脱敏副本，绝不进入
/// Session Journal、Host discovery、operation status 或日志。没有配置 Provider
/// 时仍允许 Host 启动，以便列举既有 Session 和完成控制面诊断；执行 Prompt 时
/// 返回稳定的 Provider 未配置错误。
#[derive(Clone)]
struct HeadlessProvider {
    registry: ProviderRegistry,
    provider_id: String,
    model: String,
    secret: Option<Arc<Zeroizing<String>>>,
}

impl HeadlessProvider {
    fn resolve(&self) -> Result<(ResolvedProvider, ProviderSnapshot), HeadlessProviderError> {
        let provider = self
            .registry
            .resolve(&self.provider_id, &self.model)
            .map_err(|_| HeadlessProviderError::InvalidConfiguration)?;
        let snapshot = ProviderSnapshot {
            provider_id: provider.provider_id().to_owned(),
            model: provider.model().to_owned(),
            context_window: provider.capabilities(provider.model()).max_context_tokens,
            protocol: provider_protocol_snapshot(provider.protocol()),
            config_fingerprint: provider.config_identity().to_owned(),
            reasoning_effort: None,
        };
        Ok((provider, snapshot))
    }

    fn redact(&self, text: &str) -> String {
        let Some(secret) = &self.secret else {
            return text.to_owned();
        };
        if secret.is_empty() {
            return text.to_owned();
        }
        text.replace(secret.as_str(), "[REDACTED]")
    }
}

fn headless_provider_from_environment() -> Result<Option<HeadlessProvider>, HeadlessProviderError> {
    let base_url = first_environment(["KEENCODE_PROVIDER_BASE_URL", "OPENAI_BASE_URL"]);
    let model = first_environment(["KEENCODE_PROVIDER_MODEL", "OPENAI_MODEL"]);
    let api_key = first_environment(["KEENCODE_PROVIDER_API_KEY", "OPENAI_API_KEY"]);
    let protocol = std::env::var("KEENCODE_PROVIDER_PROTOCOL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "chat_completions".to_owned());

    if base_url.is_none() && model.is_none() && api_key.is_none() {
        return Ok(None);
    }
    let model = model.ok_or(HeadlessProviderError::MissingConfiguration)?;
    let base_url = base_url.unwrap_or_else(|| "https://api.openai.com/v1/".to_owned());
    let protocol = match protocol.trim().to_ascii_lowercase().as_str() {
        "chat_completions" | "chat" | "openai_chat" => ProviderProtocol::ChatCompletions,
        "responses" | "openai_responses" => ProviderProtocol::Responses,
        "messages" | "anthropic" | "anthropic_messages" => ProviderProtocol::Messages,
        _ => return Err(HeadlessProviderError::InvalidConfiguration),
    };
    let secret = api_key.map(|value| Arc::new(Zeroizing::new(value)));
    let credential_revision = secret.as_ref().map_or_else(
        || "headless-env-unauthenticated".to_owned(),
        |value| format!("headless-env-{:x}", Sha256::digest(value.as_bytes())),
    );
    let api_key = secret
        .as_ref()
        .map(|value| ApiKey::new(value.as_str()))
        .transpose()
        .map_err(|_| HeadlessProviderError::InvalidConfiguration)?;
    let mut config = match api_key {
        Some(api_key) => ProviderConfig::new("headless-env", protocol, base_url, api_key),
        None => ProviderConfig::new_unauthenticated("headless-env", protocol, base_url),
    }
    .map_err(|_| HeadlessProviderError::InvalidConfiguration)?;
    config.default_capabilities = ProviderCapabilities {
        streaming: true,
        tool_calling: true,
        ..ProviderCapabilities::default()
    };
    if std::env::var("KEENCODE_PROVIDER_RESPONSE_MODE")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("buffered"))
    {
        config.response_mode = WireResponseMode::Buffered;
    }
    let registration = ProviderRegistration::new(
        config,
        "Headless environment Provider",
        credential_revision,
        ProviderModelPolicy::Enumerated {
            models: vec![model.clone()],
        },
    )
    .map_err(|_| HeadlessProviderError::InvalidConfiguration)?;
    let registry = ProviderRegistry::new();
    registry
        .replace_all([registration])
        .map_err(|_| HeadlessProviderError::InvalidConfiguration)?;
    Ok(Some(HeadlessProvider {
        registry,
        provider_id: "headless-env".to_owned(),
        model,
        secret,
    }))
}

fn first_environment<const N: usize>(names: [&str; N]) -> Option<String> {
    names.into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

fn provider_protocol_snapshot(protocol: ProviderProtocol) -> ProviderProtocolSnapshot {
    match protocol {
        ProviderProtocol::Messages => ProviderProtocolSnapshot::AnthropicMessages,
        ProviderProtocol::ChatCompletions => ProviderProtocolSnapshot::OpenAiChatCompletions,
        ProviderProtocol::Responses => ProviderProtocolSnapshot::OpenAiResponses,
    }
}

#[derive(Clone)]
struct HeadlessInner {
    runtime: Arc<HostRuntime>,
    runtime_manager: Arc<RuntimeManager>,
    provider: Option<HeadlessProvider>,
    state: Arc<Mutex<HeadlessState>>,
    web_runtime: Arc<AsyncMutex<Option<HeadlessWebRuntime>>>,
}

struct HeadlessState {
    connections: BTreeSet<ConnectionId>,
    initialized: BTreeSet<ConnectionId>,
    /// 每条连接当前 load 的 Session；detached Turn 的实时事件据此转发给后续 attach。
    connection_sessions: BTreeMap<ConnectionId, String>,
    event_senders: BTreeMap<ConnectionId, broadcast::Sender<Value>>,
    operations: BTreeMap<OperationId, OperationControl>,
    operation_results: BTreeMap<OperationId, OperationResult>,
    /// 当前 Session delivery 的单调水位；历史重放和实时事件共用，避免 attach 后序号倒退。
    delivery_sequences: BTreeMap<String, u64>,
    web: WebState,
}

#[derive(Clone)]
struct OperationControl {
    connection_id: ConnectionId,
    session_id: String,
    turn_id: String,
    prompt: String,
    cancel: watch::Sender<bool>,
    cancellation: TurnCancellation,
    elicitation_id: Option<String>,
}

#[derive(Clone, Debug)]
struct OperationResult {
    stop_reason: String,
    text: Option<String>,
}

struct ExecutionOutcome {
    state: OperationState,
    code: &'static str,
    stop_reason: String,
    text: Option<String>,
}

impl ExecutionOutcome {
    fn failed(code: &'static str) -> Self {
        Self {
            state: OperationState::Failed,
            code,
            stop_reason: "refusal".to_owned(),
            text: None,
        }
    }

    fn from_turn_result(result: TurnResult, text: Option<String>) -> Self {
        let (state, code, stop_reason) = match result.state.terminal_reason() {
            Some(TerminalReason::Cancelled) => {
                (OperationState::Cancelled, "cancelled", "cancelled")
            }
            Some(TerminalReason::Completed) if result.error.is_none() => {
                (OperationState::Completed, "completed", "end_turn")
            }
            _ => (OperationState::Failed, "failed", "refusal"),
        };
        Self {
            state,
            code,
            stop_reason: stop_reason.to_owned(),
            text,
        }
    }
}

#[derive(Clone, Copy)]
struct WebState {
    running: bool,
    port: u16,
}

/// Headless Web 的完整 owner；只有保存该结构，HTTP/WS listener 才会持续存在。
struct HeadlessWebRuntime {
    host: Arc<WebHost>,
    owner: WebServerOwner,
    adapter: Arc<HostWsAdapter>,
}

/// 把 headless Host Core 接到 Web transport 的业务路由。
///
/// `HostWsAdapter` 只负责连接级队列。这里额外桥接 headless 的广播事件，避免
/// Web 客户端只能收到请求响应而收不到实时 Session delivery。
struct HeadlessWebBusinessRouter {
    inner: Arc<HeadlessInner>,
    adapter: Mutex<Option<Weak<HostWsAdapter>>>,
    bridges: Mutex<BTreeMap<ConnectionId, JoinHandle<()>>>,
}

impl HeadlessWebBusinessRouter {
    fn new(inner: Arc<HeadlessInner>) -> Self {
        Self {
            inner,
            adapter: Mutex::new(None),
            bridges: Mutex::new(BTreeMap::new()),
        }
    }

    fn attach_adapter(&self, adapter: &Arc<HostWsAdapter>) {
        if let Ok(mut current) = self.adapter.lock() {
            *current = Some(Arc::downgrade(adapter));
        }
    }

    fn start_bridge(&self, connection_id: &ConnectionId) -> Result<(), HostBusinessError> {
        let mut bridges = self.bridges.lock().map_err(|_| HostBusinessError::Router)?;
        if bridges.contains_key(connection_id) {
            return Ok(());
        }
        let adapter = self
            .adapter
            .lock()
            .map_err(|_| HostBusinessError::Router)?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(HostBusinessError::Router)?;
        let mut events = self
            .inner
            .subscribe(connection_id)
            .ok_or(HostBusinessError::Router)?;
        let adapter = Arc::downgrade(&adapter);
        let bridge_connection_id = connection_id.clone();
        let task = tokio::spawn(async move {
            let mut last_journal_sequence = 0;
            loop {
                match events.recv().await {
                    Ok(value) => {
                        let journal_sequence = value
                            .pointer("/params/envelope/journalSequence")
                            .and_then(Value::as_u64);
                        if let Some(sequence) = journal_sequence {
                            last_journal_sequence = sequence;
                        }
                        let payload = match serde_json::to_vec(&value) {
                            Ok(payload) => payload,
                            Err(_) => break,
                        };
                        let Some(adapter) = adapter.upgrade() else {
                            break;
                        };
                        if adapter
                            .publish(
                                &bridge_connection_id,
                                journal_sequence.unwrap_or(last_journal_sequence),
                                payload,
                            )
                            .is_err()
                        {
                            break;
                        }
                    }
                    // 广播落后时不能伪造缺失的 Journal 序号；客户端仍可通过
                    // session/load 重新读取权威历史，因此丢弃本次实时桥接并继续。
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        bridges.insert(connection_id.clone(), task);
        Ok(())
    }

    fn stop_bridge(&self, connection_id: &ConnectionId) {
        if let Ok(mut bridges) = self.bridges.lock()
            && let Some(task) = bridges.remove(connection_id)
        {
            task.abort();
        }
    }
}

impl HostBusinessRouter for HeadlessWebBusinessRouter {
    fn dispatch<'a>(
        &'a self,
        context: HostConnectionContext,
        frame: AcpIncomingFrame,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        let value = match frame.into_json_rpc_value() {
            Ok(value) => value,
            Err(_) => return Box::pin(async { Err(HostBusinessError::InvalidAcp) }),
        };
        if let Err(error) = self.start_bridge(&context.connection_id) {
            return Box::pin(async move { Err(error) });
        }
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let response = inner
                .dispatch_value(context.connection_id, value)
                .await
                .map_err(|_| HostBusinessError::Router)?;
            response
                .map(|value| serde_json::to_vec(&value).map_err(|_| HostBusinessError::Router))
                .transpose()
        })
    }

    fn dispatch_client_response<'a>(
        &'a self,
        context: HostConnectionContext,
        response: Value,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        if let Err(error) = self.start_bridge(&context.connection_id) {
            return Box::pin(async move { Err(error) });
        }
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .dispatch_value(context.connection_id, response)
                .await
                .map_err(|_| HostBusinessError::Router)
                .map(|_| None)
        })
    }

    fn disconnect(&self, context: HostConnectionContext) {
        self.stop_bridge(&context.connection_id);
        self.inner.disconnected(&context.connection_id);
    }
}

/// 查找随 CLI 发布的 Vite production bundle。
///
/// 安装包通常把资源放在可执行文件旁的 `web/`，开发/测试运行则使用仓库根的
/// `dist/`。环境变量优先，便于发行包和 E2E 显式指定资源根，而不会回退到源码目录。
fn resolve_web_static_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(root) = std::env::var("KEENCODE_WEB_STATIC_ROOT")
        && !root.trim().is_empty()
    {
        candidates.push(PathBuf::from(root));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        candidates.push(parent.join("web"));
        candidates.push(parent.join("dist"));
        candidates.push(parent.join(r"..\web"));
        candidates.push(parent.join(r"..\dist"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist"));
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join("dist"));
        candidates.push(current.join("web"));
    }
    candidates
        .into_iter()
        .find(|root| root.join("index.html").is_file())
}

impl HeadlessInner {
    fn new(
        runtime: Arc<HostRuntime>,
        provider: Option<HeadlessProvider>,
    ) -> Result<Self, RuntimeError> {
        let runtime_manager = RuntimeManager::new(keencode_runtime::RuntimeConfig::new(
            runtime.data_root().join("sessions"),
        ))?;
        Ok(Self {
            runtime,
            runtime_manager: Arc::new(runtime_manager),
            provider,
            state: Arc::new(Mutex::new(HeadlessState {
                connections: BTreeSet::new(),
                initialized: BTreeSet::new(),
                connection_sessions: BTreeMap::new(),
                event_senders: BTreeMap::new(),
                operations: BTreeMap::new(),
                operation_results: BTreeMap::new(),
                delivery_sequences: BTreeMap::new(),
                web: WebState {
                    running: false,
                    port: 32123,
                },
            })),
            web_runtime: Arc::new(AsyncMutex::new(None)),
        })
    }

    fn shutdown(&self) -> Result<(), RuntimeError> {
        self.runtime_manager.close_all()
    }

    fn ensure_connection(&self, connection_id: &ConnectionId) -> Result<(), HostDispatchError> {
        let mut state = self.state.lock().map_err(|_| HostDispatchError::Internal)?;
        if state.connections.insert(connection_id.clone()) {
            self.runtime
                .attach_client(connection_id.clone())
                .map_err(|_| HostDispatchError::Internal)?;
        }
        Ok(())
    }

    async fn dispatch_value(
        &self,
        connection_id: ConnectionId,
        message: Value,
    ) -> Result<Option<Value>, HostDispatchError> {
        self.ensure_connection(&connection_id)?;
        let id = message.get("id").cloned();
        if let Some(id) = &id
            && !valid_rpc_id(id)
        {
            return Ok(Some(rpc_error(Value::Null, -32600, "request id 无效")));
        }
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Ok(id.map(|id| rpc_error(id, -32600, "jsonrpc 必须是 2.0")));
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // Host 发出的 Elicitation 是 JSON-RPC Client Request；其响应没有
            // method，必须在这里消费，不能与普通 ACP response 混淆。
            self.handle_client_response(&connection_id, &message)?;
            return Ok(None);
        };
        if method == "initialize" {
            let response = self.initialize(&connection_id, id.clone())?;
            return Ok(id.map(|id| rpc_result(id, response)));
        }
        let initialized = self
            .state
            .lock()
            .map_err(|_| HostDispatchError::Internal)?
            .initialized
            .contains(&connection_id);
        if !initialized {
            return Ok(id.map(|id| rpc_error(id, -32000, "必须先完成 initialize")));
        }
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "session/new" => self.session_new(&params),
            "session/list" => self.session_list(&params),
            "session/load" => self.session_load(&connection_id, &params),
            "session/prompt" => self.session_prompt(connection_id, &params).await,
            "session/cancel" => self.session_cancel(&params),
            "keencode/operation/admit" => self.operation_admit(connection_id, &params).await,
            "keencode/operation/status" => self.operation_status(&params),
            "keencode/web/start" => self.web_start(&params).await,
            "keencode/web/stop" => self.web_stop().await,
            "keencode/web/status" => self.web_status().await,
            _ => Err(HandlerError::new(-32601, "Host 不支持该方法")),
        };
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(Some(match result {
            Ok(value) => rpc_result(id, value),
            Err(error) => rpc_error(id, error.code, error.message),
        }))
    }

    fn handle_client_response(
        &self,
        _connection_id: &ConnectionId,
        message: &Value,
    ) -> Result<(), HostDispatchError> {
        let Some(elicitation_id) = message.get("id").and_then(Value::as_str) else {
            // JSON-RPC response/notification 不是当前 Host 发出的 Elicitation，
            // 无需生成新的响应，避免把未知 response 变成额外业务副作用。
            return Ok(());
        };
        let pending = self
            .state
            .lock()
            .map_err(|_| HostDispatchError::Internal)?
            .operations
            .iter()
            .find(|(_, control)| control.elicitation_id.as_deref() == Some(elicitation_id))
            .map(|(operation_id, control)| (operation_id.clone(), control.clone()));
        let Some((operation_id, control)) = pending else {
            return Ok(());
        };

        let Some(answer) = message.get("result") else {
            if message.get("error").is_some() {
                self.finish_claimed(
                    &operation_id,
                    &control.session_id,
                    &control.turn_id,
                    &control.connection_id,
                    ExecutionOutcome::failed("user_input_rejected"),
                );
            }
            return Ok(());
        };
        let answer_json = serde_json::to_vec(answer).map_err(|_| HostDispatchError::Internal)?;
        let answer_digest = format!("{:x}", Sha256::digest(&answer_json));
        let answer_id = OperationId::new(format!("answer-{answer_digest}"))
            .map_err(|_| HostDispatchError::Internal)?;
        let receipt = self
            .runtime
            .prompt_queue()
            .answer_elicitation(&operation_id, elicitation_id, answer_id, answer_digest)
            .map_err(|_| HostDispatchError::Internal)?;
        if receipt.disposition != ElicitationAnswerDisposition::Accepted {
            return Ok(());
        }

        let answer_text = serde_json::to_string(answer).map_err(|_| HostDispatchError::Internal)?;
        let prompt = format!("{}\n\n用户输入：{}", control.prompt, answer_text);
        if prompt.len() > MAX_HOST_PROMPT_BYTES {
            self.finish_claimed(
                &operation_id,
                &control.session_id,
                &control.turn_id,
                &control.connection_id,
                ExecutionOutcome::failed("user_input_too_large"),
            );
            return Ok(());
        }
        if let Ok(mut state) = self.state.lock()
            && let Some(control_state) = state.operations.get_mut(&operation_id)
        {
            control_state.elicitation_id = None;
        }
        let cancelled = control.cancel.subscribe();
        self.spawn_execution(operation_id, control, prompt, cancelled);
        Ok(())
    }

    fn initialize(
        &self,
        connection_id: &ConnectionId,
        _id: Option<Value>,
    ) -> Result<Value, HostDispatchError> {
        let mut state = self.state.lock().map_err(|_| HostDispatchError::Internal)?;
        state.initialized.insert(connection_id.clone());
        let meta = self
            .runtime
            .initialize_meta()
            .map_err(|_| HostDispatchError::Internal)?;
        Ok(json!({
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {"name": "KeenCode Headless", "version": "0.1.0"},
            "_meta": meta,
        }))
    }

    fn session_new(&self, params: &Value) -> Result<Value, HandlerError> {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| HandlerError::new(-32602, "session/new 缺少 cwd"))?;
        let cwd = canonical_directory(cwd)?;
        let session_id = next_id("session");
        let title = "新对话".to_owned();
        self.runtime_manager
            .create(CreateSessionRequest {
                session_id: session_id.clone(),
                title: title.clone(),
                project_root: cwd.clone(),
            })
            .map_err(map_runtime_error)?;
        Ok(json!({
            "sessionId": session_id,
            "modes": {"currentModeId":"normal","availableModes":[{"id":"normal","name":"Normal"}]},
            "configOptions": [],
        }))
    }

    fn session_list(&self, params: &Value) -> Result<Value, HandlerError> {
        let requested_cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(canonical_directory)
            .transpose()?;
        let sessions = self
            .runtime_manager
            .list_stored_sessions_for_project(requested_cwd.as_deref())
            .map_err(map_runtime_error)?
            .into_iter()
            .map(session_metadata_value)
            .collect::<Vec<_>>();
        Ok(json!({"sessions":sessions}))
    }

    fn session_load(
        &self,
        connection_id: &ConnectionId,
        params: &Value,
    ) -> Result<Value, HandlerError> {
        let session_id = string_param(params, "sessionId")?;
        let requested_cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(canonical_directory)
            .transpose()?;
        let session = self.ensure_session(&session_id, requested_cwd.as_deref())?;
        self.state
            .lock()
            .map_err(|_| HandlerError::internal())?
            .connection_sessions
            .insert(connection_id.clone(), session_id.clone());
        let active_turn_id = session
            .active_turn_ids()
            .map_err(map_runtime_error)?
            .into_iter()
            .next()
            .map(|turn_id| turn_id.to_string());
        let replay = self
            .replay_session_history(connection_id, &session)
            .map_err(map_runtime_error)?;
        Ok(json!({
            "_meta":{
                "keencode/snapshot":{"activeTurnId":active_turn_id},
                "keencode/replay": replay,
            },
        }))
    }

    /// 将已写入 Journal 的历史事实投影到本次 attach 连接。
    ///
    /// Runtime 的广播只覆盖订阅建立之后的实时水位；detached Turn 或 Host 重启
    /// 后的首次 attach 必须从同一权威 Journal 恢复，不能依赖进程内事件缓存。
    fn replay_session_history(
        &self,
        connection_id: &ConnectionId,
        session: &RuntimeSession,
    ) -> Result<Value, RuntimeError> {
        let state = session.snapshot()?.state;
        let page = session.replay(None, keencode_resources::MAX_REPLAY_PAGE_RECORDS)?;
        let mut provider_by_turn = BTreeMap::new();
        let mut replayed_events = 0_u64;
        let mut next_delivery_sequence = self.next_delivery_sequence(session.session_id().as_str());
        for record in &page.records {
            let deliveries = headless_authoritative_deliveries(
                session,
                &state,
                record,
                self.provider.as_ref(),
                &mut provider_by_turn,
                &mut next_delivery_sequence,
            )?;
            replayed_events = replayed_events.saturating_add(deliveries.len() as u64);
            for delivery in deliveries {
                self.publish_session_event(connection_id, session.session_id().as_str(), delivery);
            }
        }
        self.set_delivery_sequence(session.session_id().as_str(), next_delivery_sequence);
        Ok(json!({
            "startAfter": 0,
            "nextAfter": page.next_after.unwrap_or(0),
            "throughJournalSequence": page.through_sequence,
            "replayedEvents": replayed_events,
            "hasMore": page.has_more,
        }))
    }

    fn ensure_session(
        &self,
        session_id: &str,
        requested_cwd: Option<&str>,
    ) -> Result<RuntimeSession, HandlerError> {
        let metadata = self
            .runtime_manager
            .stored_session_metadata(session_id)
            .map_err(map_runtime_error)?;
        if requested_cwd.is_some_and(|cwd| cwd != metadata.project_root.as_str()) {
            return Err(HandlerError::new(
                -32004,
                "Session 不存在或不在当前授权范围内",
            ));
        }
        let session = match self.runtime_manager.get(session_id) {
            Ok(session) => session,
            Err(RuntimeError::SessionNotRegistered) => match self
                .runtime_manager
                .open(session_id)
                .map_err(map_runtime_error)?
            {
                OpenSessionResult::Ready(session) => session,
                OpenSessionResult::Corrupt(_) => {
                    return Err(HandlerError::new(-32004, "Session 日志已损坏"));
                }
            },
            Err(error) => return Err(map_runtime_error(error)),
        };
        Ok(session)
    }

    async fn session_prompt(
        &self,
        connection_id: ConnectionId,
        params: &Value,
    ) -> Result<Value, HandlerError> {
        let session_id = string_param(params, "sessionId")?;
        let _ = self.ensure_session(&session_id, None)?;
        let prompt = prompt_text(params)?;
        let operation_id = operation_id(params).unwrap_or_else(|| next_id("prompt"));
        let status = self
            .admit_operation(
                connection_id,
                session_id.clone(),
                operation_id,
                prompt,
                false,
            )
            .await?;
        if status.state == OperationState::NeedsInput {
            return Err(HandlerError::new(-32006, "Prompt 需要用户输入"));
        }
        let status = self.wait_for_terminal(&status.operation_id).await?;
        if status.state == OperationState::NeedsInput {
            return Err(HandlerError::new(-32006, "Prompt 需要用户输入"));
        }
        let turn_id = status
            .execution
            .as_ref()
            .map(|execution| execution.turn_id.clone())
            .ok_or_else(HandlerError::internal)?;
        let operation_result = self
            .state
            .lock()
            .map_err(|_| HandlerError::internal())?
            .operation_results
            .get(&status.operation_id)
            .cloned();
        let stop_reason = operation_result.as_ref().map_or_else(
            || stop_reason_for_state(status.state).to_owned(),
            |value| value.stop_reason.clone(),
        );
        let mut response = json!({
            "stopReason": stop_reason,
            "_meta":{"keencode/turnId":turn_id},
        });
        if let Some(text) = operation_result.and_then(|value| value.text) {
            response["text"] = Value::String(text);
        }
        Ok(response)
    }

    async fn operation_admit(
        &self,
        connection_id: ConnectionId,
        params: &Value,
    ) -> Result<Value, HandlerError> {
        let session_id = string_param(params, "sessionId")?;
        let _ = self.ensure_session(&session_id, None)?;
        let prompt = string_param(params, "prompt")?;
        if prompt.is_empty() {
            return Err(HandlerError::new(-32602, "Prompt 不能为空"));
        }
        let operation_id =
            operation_id(params).ok_or_else(|| HandlerError::new(-32602, "缺少 operationId"))?;
        if params.get("detached").and_then(Value::as_bool) != Some(true) {
            return Err(HandlerError::new(
                -32602,
                "operation/admit 只接受 detached=true",
            ));
        }
        let status = self
            .admit_operation(connection_id, session_id, operation_id, prompt, true)
            .await?;
        Ok(json!({
            "operationId": status.operation_id,
            "sessionId": status.session_id,
            "execution": status.execution,
            "state": status.state,
            "status": status,
        }))
    }

    async fn admit_operation(
        &self,
        connection_id: ConnectionId,
        session_id: String,
        operation_id: String,
        prompt: String,
        detached: bool,
    ) -> Result<OperationStatus, HandlerError> {
        let operation_id = OperationId::new(operation_id)
            .map_err(|_| HandlerError::new(-32602, "operationId 无效"))?;
        let payload_digest = format!("{:x}", Sha256::digest(prompt.as_bytes()));
        let receipt = self
            .runtime
            .prompt_queue()
            .admit(PromptAdmissionRequest {
                connection_id,
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                prompt: prompt.clone(),
                payload_digest,
                detached,
            })
            .map_err(map_core_error)?;
        if receipt.disposition != AdmissionDisposition::Duplicate {
            self.start_next(&session_id)?;
        }
        let mut status = receipt.status;
        if status.execution.is_none() {
            status = self.wait_for_execution(&operation_id).await?;
        }
        Ok(status)
    }

    fn start_next(&self, session_id: &str) -> Result<(), HandlerError> {
        let Some(claimed) = self
            .runtime
            .prompt_queue()
            .claim_next(session_id)
            .map_err(map_core_error)?
        else {
            return Ok(());
        };
        let turn_id = next_id("turn");
        let task_id = next_id("task");
        let execution =
            ExecutionIdentity::new(claimed.session_id.clone(), turn_id.clone(), task_id)
                .map_err(map_core_error)?;
        self.runtime
            .prompt_queue()
            .bind_execution(&claimed.operation_id, execution)
            .map_err(map_core_error)?;
        let connection_id = claimed.connection_id.clone();
        let prompt = claimed.prompt.clone();
        let (cancel, cancelled) = watch::channel(false);
        let cancellation = TurnCancellation::new();
        let control = OperationControl {
            connection_id: connection_id.clone(),
            session_id: claimed.session_id.clone(),
            turn_id: turn_id.clone(),
            prompt: prompt.clone(),
            cancel,
            cancellation: cancellation.clone(),
            elicitation_id: None,
        };
        self.state
            .lock()
            .map_err(|_| HandlerError::internal())?
            .operations
            .insert(claimed.operation_id.clone(), control.clone());
        let operation_id = claimed.operation_id;
        self.spawn_execution(operation_id, control, prompt, cancelled);
        Ok(())
    }

    fn spawn_execution(
        &self,
        operation_id: OperationId,
        control: OperationControl,
        prompt: String,
        cancelled: watch::Receiver<bool>,
    ) {
        let inner = self.clone();
        tokio::spawn(async move {
            let result = inner
                .execute_claimed(
                    &control.connection_id,
                    &control.session_id,
                    &control.turn_id,
                    &prompt,
                    control.cancellation.clone(),
                    cancelled,
                )
                .await;
            inner.finish_claimed(
                &operation_id,
                &control.session_id,
                &control.turn_id,
                &control.connection_id,
                result,
            );
        });
    }

    async fn execute_claimed(
        &self,
        connection_id: &ConnectionId,
        session_id: &str,
        turn_id: &str,
        prompt: &str,
        cancellation: TurnCancellation,
        mut cancelled: watch::Receiver<bool>,
    ) -> ExecutionOutcome {
        let Some(provider) = self.provider.clone() else {
            return ExecutionOutcome::failed("provider_unavailable");
        };
        let session = match self.runtime_manager.get(session_id) {
            Ok(session) => session,
            Err(_) => return ExecutionOutcome::failed("session_unavailable"),
        };
        let project_root = match self.runtime_manager.stored_session_metadata(session_id) {
            Ok(metadata) => metadata.project_root,
            Err(_) => return ExecutionOutcome::failed("session_unavailable"),
        };
        let environment = match ToolEnvironment::new(Path::new(&project_root)) {
            Ok(environment) => Arc::new(environment),
            Err(_) => return ExecutionOutcome::failed("tool_environment_unavailable"),
        };
        let mut tools = ToolRegistry::new();
        if register_local_tools(&mut tools, environment).is_err() {
            return ExecutionOutcome::failed("tool_registry_unavailable");
        }
        let (resolved, provider_snapshot) = match provider.resolve() {
            Ok(provider) => provider,
            Err(_) => return ExecutionOutcome::failed("provider_unavailable"),
        };
        let mut messages = match session.model_transcript() {
            Ok(messages) => messages,
            Err(_) => return ExecutionOutcome::failed("session_unavailable"),
        };
        let input = Message::text(MessageRole::User, prompt);
        messages.push(input.clone());
        let agent_session_id = match AgentSessionId::new(session_id.to_owned()) {
            Ok(value) => value,
            Err(_) => return ExecutionOutcome::failed("session_unavailable"),
        };
        let agent_turn_id = match AgentTurnId::new(turn_id.to_owned()) {
            Ok(value) => value,
            Err(_) => return ExecutionOutcome::failed("runtime_failed"),
        };
        let source_agent_id = match AgentId::new(ROOT_AGENT_ID) {
            Ok(value) => value,
            Err(_) => return ExecutionOutcome::failed("runtime_failed"),
        };
        let mut request = TurnRequest::new(
            agent_session_id,
            agent_turn_id,
            source_agent_id,
            resolved.model().to_owned(),
            messages,
            PlanGuard::inactive(),
        );
        request.set_cancellation(cancellation.clone());
        let turn = RuntimeTurnRequest::root(request, vec![input], prompt.to_owned())
            .with_provider_snapshot(provider_snapshot.clone());
        let runner = AgentRunner::new(
            Arc::new(resolved) as Arc<dyn ModelProvider>,
            tools,
            RunLimits::default(),
        );
        let event_pump = match session.subscribe() {
            Ok(subscription) => {
                let (stop_sender, stop_receiver) = oneshot::channel();
                let inner = self.clone();
                let connection_id = connection_id.clone();
                let session_id = session_id.to_owned();
                let provider = provider.clone();
                let provider_snapshot = provider_snapshot.clone();
                let task = tokio::spawn(async move {
                    inner
                        .pump_runtime_events(
                            connection_id,
                            session_id,
                            provider,
                            provider_snapshot,
                            subscription,
                            stop_receiver,
                        )
                        .await;
                });
                Some((stop_sender, task))
            }
            Err(_) => None,
        };
        let bound = session.bind_agent_runner(runner);
        let run = bound.run_turn(turn);
        tokio::pin!(run);
        let result = tokio::select! {
            result = &mut run => result,
            changed = cancelled.changed() => {
                if changed.is_ok() && *cancelled.borrow() {
                    cancellation.cancel();
                }
                run.await
            }
        };
        if let Some((stop_sender, task)) = event_pump {
            // Runtime 在返回 TurnResult 前已完成 Runner 的事件 Sink；先发停止信号，
            // 事件泵以 biased select 优先消费已经进入广播队列的事件，再退出。
            let _ = stop_sender.send(());
            let _ = task.await;
        }
        match result {
            Ok(result) => {
                let text = result
                    .final_response
                    .as_ref()
                    .and_then(|response| last_non_empty_text(&response.content))
                    .map(|text| provider.redact(text));
                ExecutionOutcome::from_turn_result(result, text)
            }
            Err(_) => ExecutionOutcome::failed("runtime_failed"),
        }
    }

    /// 把 Runtime Session 的临时事件转成当前连接上的 ACP delivery。
    ///
    /// 订阅属于单个执行 Turn，因此只负责向发起该 Turn 的连接投递；Session 内
    /// 的权威事件仍由 Journal 恢复路径负责，不能在这里重复构造第二份事实源。
    async fn pump_runtime_events(
        &self,
        connection_id: ConnectionId,
        session_id: String,
        provider: HeadlessProvider,
        provider_snapshot: ProviderSnapshot,
        mut subscription: RuntimeEventSubscription,
        mut stop: oneshot::Receiver<()>,
    ) {
        let mut tool_arguments = BTreeMap::new();
        loop {
            let received = tokio::select! {
                // TurnResult 返回前所有 Provider 事件都已经进入广播队列；优先消费
                // 已就绪事件，避免停止信号抢先导致末尾文本丢失。
                biased;
                received = subscription.recv() => Some(received),
                _ = &mut stop => None,
            };
            let Some(received) = received else {
                break;
            };
            match received {
                Ok(delivery) => {
                    let delivery_sequence = self.next_delivery_sequence(&session_id);
                    match delivery.payload {
                        RuntimeEventPayload::Transient(event) => {
                            if let Some(value) = headless_transient_delivery(
                                &session_id,
                                delivery_sequence,
                                &provider_snapshot,
                                &provider,
                                &event,
                                &mut tool_arguments,
                            ) {
                                self.publish_session_event(&connection_id, &session_id, value);
                            }
                        }
                        // 当前 headless 只负责实时增量；权威历史通过 session/load 与
                        // 后续 Runtime replay 接口恢复，不能把 Journal 记录伪装成增量。
                        RuntimeEventPayload::Authoritative(_) => {}
                        RuntimeEventPayload::Control(_) => break,
                    }
                }
                Err(RuntimeEventReceiveError::Lagged(lag)) => {
                    // 不能伪造已经丢失的 delivery sequence；通知 Client 走 Snapshot/
                    // Journal 恢复，且不把 Provider 正文写进诊断消息。
                    self.publish_session_event(
                        &connection_id,
                        &session_id,
                        json!({
                            "jsonrpc":"2.0",
                            "method":"keencode/runtime/lagged",
                            "params":{
                                "sessionId":session_id,
                                "missedEvents":lag.missed_events,
                                "firstMissedDeliverySequence":lag.first_missed_delivery_sequence,
                                "lastMissedDeliverySequence":lag.last_missed_delivery_sequence,
                                "catchUp":"reload_snapshot_and_replay_journal"
                            }
                        }),
                    );
                }
                Err(RuntimeEventReceiveError::Closed) => break,
            }
        }
    }

    fn finish_claimed(
        &self,
        operation_id: &OperationId,
        session_id: &str,
        turn_id: &str,
        connection_id: &ConnectionId,
        outcome: ExecutionOutcome,
    ) {
        self.publish_terminal_event(connection_id, session_id, turn_id, &outcome);
        let terminal = OperationTerminal::new(outcome.code, None::<String>);
        if let Ok(terminal) = terminal {
            let _ = self
                .runtime
                .prompt_queue()
                .finish(operation_id, outcome.state, terminal);
        }
        if let Ok(mut state) = self.state.lock() {
            state.operations.remove(operation_id);
            state.operation_results.insert(
                operation_id.clone(),
                OperationResult {
                    stop_reason: outcome.stop_reason,
                    text: outcome.text,
                },
            );
            while state.operation_results.len() > 256 {
                let Some(oldest) = state.operation_results.keys().next().cloned() else {
                    break;
                };
                state.operation_results.remove(&oldest);
            }
        }
        let _ = self.start_next(session_id);
    }

    fn publish_terminal_event(
        &self,
        connection_id: &ConnectionId,
        session_id: &str,
        turn_id: &str,
        outcome: &ExecutionOutcome,
    ) {
        let event_type = match outcome.state {
            OperationState::Cancelled => "turn_cancelled",
            OperationState::Completed => "turn_completed",
            _ => "turn_failed",
        };
        let event = json!({
            "jsonrpc":"2.0",
            "method":"acp://delivery",
            "params":{"envelope":{
                "sessionId":session_id,
                "turnId":turn_id,
                "event":{"type":event_type,"stopReason":outcome.stop_reason}
            }}
        });
        self.publish_session_event(connection_id, session_id, event);
    }

    fn publish_session_event(
        &self,
        origin_connection_id: &ConnectionId,
        session_id: &str,
        event: Value,
    ) {
        let senders = self.state.lock().ok().map(|state| {
            state
                .event_senders
                .iter()
                .filter(|(connection_id, _)| {
                    *connection_id == origin_connection_id
                        || state
                            .connection_sessions
                            .get(*connection_id)
                            .is_some_and(|bound| bound == session_id)
                })
                .map(|(_, sender)| sender.clone())
                .collect::<Vec<_>>()
        });
        if let Some(senders) = senders {
            for sender in senders {
                let _ = sender.send(event.clone());
            }
        }
    }

    /// 返回并预留一个当前 Session 的 headless delivery 序号。
    fn next_delivery_sequence(&self, session_id: &str) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 1;
        };
        let sequence = state
            .delivery_sequences
            .entry(session_id.to_owned())
            .or_insert(0);
        *sequence = sequence.saturating_add(1).max(1);
        *sequence
    }

    /// 将 headless delivery 水位推进到实时 Runtime 的事件序号之后。
    fn set_delivery_sequence(&self, session_id: &str, sequence: u64) {
        if let Ok(mut state) = self.state.lock() {
            let current = state
                .delivery_sequences
                .entry(session_id.to_owned())
                .or_insert(0);
            *current = (*current).max(sequence);
        }
    }

    async fn wait_for_execution(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HandlerError> {
        loop {
            let status = self
                .runtime
                .prompt_queue()
                .status(operation_id)
                .map_err(map_core_error)?;
            if status.execution.is_some() || status.state.is_terminal() {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_for_terminal(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HandlerError> {
        loop {
            let status = self
                .runtime
                .prompt_queue()
                .status(operation_id)
                .map_err(map_core_error)?;
            if status.state.is_terminal() {
                return Ok(status);
            }
            if status.state == OperationState::NeedsInput {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn session_cancel(&self, params: &Value) -> Result<Value, HandlerError> {
        let session_id = string_param(params, "sessionId")?;
        let operation_id = operation_id(params)
            .map(OperationId::new)
            .transpose()
            .map_err(|_| HandlerError::new(-32602, "operationId 无效"))?;
        let turn_id = params
            .pointer("/_meta/keencode~1turnId")
            .and_then(Value::as_str);
        if let Some(operation_id) = &operation_id {
            let status = self
                .runtime
                .prompt_queue()
                .status(operation_id)
                .map_err(map_core_error)?;
            if status.session_id != session_id {
                return Ok(json!({"cancelled":false}));
            }
            if status.state == OperationState::Admitted {
                let terminal =
                    OperationTerminal::new("cancelled", None::<String>).map_err(map_core_error)?;
                self.runtime
                    .prompt_queue()
                    .cancel_pending(operation_id, terminal)
                    .map_err(map_core_error)?;
                let _ = self.start_next(&session_id);
                return Ok(json!({"cancelled":true,"operationId":operation_id}));
            }
        }
        let control = self
            .state
            .lock()
            .map_err(|_| HandlerError::internal())?
            .operations
            .iter()
            .find(|(candidate, control)| {
                control.session_id == session_id
                    && operation_id
                        .as_ref()
                        .is_none_or(|value| value == *candidate)
                    && turn_id.is_none_or(|value| value == control.turn_id)
            })
            .map(|(operation_id, control)| (operation_id.clone(), control.clone()));
        let Some((operation_id, control)) = control else {
            return Ok(json!({"cancelled":false}));
        };
        let cancellation = control.cancellation.clone();
        let control_turn_id = control.turn_id.clone();
        let _ = control.cancel.send(true);
        cancellation.cancel();
        if control.elicitation_id.is_some() {
            self.finish_claimed(
                &operation_id,
                &control.session_id,
                &control.turn_id,
                &control.connection_id,
                ExecutionOutcome {
                    state: OperationState::Cancelled,
                    code: "cancelled",
                    stop_reason: "cancelled".to_owned(),
                    text: None,
                },
            );
        } else if let Ok(session) = self.runtime_manager.get(session_id.clone()) {
            let _ = session.cancel_turn(control_turn_id.clone());
        }
        Ok(json!({"cancelled":true,"turnId":control_turn_id}))
    }

    fn operation_status(&self, params: &Value) -> Result<Value, HandlerError> {
        let operation_id =
            operation_id(params).ok_or_else(|| HandlerError::new(-32602, "缺少 operationId"))?;
        let operation_id = OperationId::new(operation_id)
            .map_err(|_| HandlerError::new(-32602, "operationId 无效"))?;
        let status = self
            .runtime
            .prompt_queue()
            .status(&operation_id)
            .map_err(map_core_error)?;
        serde_json::to_value(status).map_err(|_| HandlerError::internal())
    }

    async fn web_start(&self, params: &Value) -> Result<Value, HandlerError> {
        let port = params
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(32123);
        if port == 0 {
            return Err(HandlerError::new(-32602, "Web 端口无效"));
        }

        let mut web_runtime = self.web_runtime.lock().await;
        if let Some(runtime) = web_runtime.as_ref() {
            let status = runtime.owner.status();
            if status.port != port {
                return Err(HandlerError::new(-32010, "Web Host 已在其他固定端口运行"));
            }
            return serde_json::to_value(status).map_err(|_| HandlerError::internal());
        }

        let token = WebToken::from_env("KEENCODE_WEB_TOKEN")
            .map_err(|_| HandlerError::new(-32010, "KEENCODE_WEB_TOKEN 配置无效"))?;
        let static_root = resolve_web_static_root()
            .ok_or_else(|| HandlerError::new(-32010, "Web 生产静态资源不可用"))?;
        let upload_root = self.runtime.data_root().join("web-uploads");
        fs::create_dir_all(&upload_root)
            .map_err(|_| HandlerError::new(-32010, "Web 目录不可用"))?;
        let config = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            port,
            token,
            static_root,
            upload_root,
        )
        .map_err(|_| HandlerError::new(-32010, "Web Host 配置无效"))?;
        let host = Arc::new(
            WebHost::new(config).map_err(|_| HandlerError::new(-32010, "Web Host 创建失败"))?,
        );
        let business = Arc::new(HeadlessWebBusinessRouter::new(Arc::new(self.clone())));
        let adapter = Arc::new(
            HostWsAdapter::new_with_resources(
                Arc::clone(&business) as Arc<dyn HostBusinessRouter>,
                256,
                host.state().resource_registry(),
            )
            .map_err(|_| HandlerError::new(-32010, "Web ACP 适配器创建失败"))?,
        );
        business.attach_adapter(&adapter);
        let router = host.router_with_business(Arc::clone(&adapter));
        let owner = WebServerOwner::start(Arc::clone(&host), router)
            .map_err(|_| HandlerError::new(-32010, "Web Host 监听失败"))?;
        let status = owner.status();
        *web_runtime = Some(HeadlessWebRuntime {
            host,
            owner,
            adapter,
        });
        let mut state = self.state.lock().map_err(|_| HandlerError::internal())?;
        state.web = WebState {
            running: true,
            port,
        };
        serde_json::to_value(status).map_err(|_| HandlerError::internal())
    }

    async fn web_stop(&self) -> Result<Value, HandlerError> {
        let runtime = self.web_runtime.lock().await.take();
        let result = if let Some(mut runtime) = runtime {
            runtime.adapter.disconnect_all();
            let result = runtime
                .owner
                .stop()
                .await
                .map_err(|_| HandlerError::new(-32010, "Web Host 停止失败"));
            if let Ok(mut state) = self.state.lock() {
                state.web.running = false;
                state.web.port = runtime.host.status().port;
            }
            result
        } else {
            if let Ok(mut state) = self.state.lock() {
                state.web.running = false;
            }
            Ok(())
        };
        result?;
        self.web_status().await
    }

    async fn web_status(&self) -> Result<Value, HandlerError> {
        let web_runtime = self.web_runtime.lock().await;
        if let Some(runtime) = web_runtime.as_ref() {
            return serde_json::to_value(runtime.host.status())
                .map_err(|_| HandlerError::internal());
        }
        let state = self.state.lock().map_err(|_| HandlerError::internal())?;
        Ok(web_status(state.web))
    }

    async fn shutdown_web(&self) -> Result<(), WebError> {
        let runtime = self.web_runtime.lock().await.take();
        let Some(mut runtime) = runtime else {
            if let Ok(mut state) = self.state.lock() {
                state.web.running = false;
            }
            return Ok(());
        };
        runtime.adapter.disconnect_all();
        let result = runtime.owner.stop().await;
        if let Ok(mut state) = self.state.lock() {
            state.web.running = false;
            state.web.port = runtime.host.status().port;
        }
        result
    }
}

impl HostDispatch for HeadlessInner {
    fn dispatch(&self, connection_id: ConnectionId, message: Value) -> HostDispatchFuture<'_> {
        let inner = self.clone();
        Box::pin(async move { inner.dispatch_value(connection_id, message).await })
    }

    fn subscribe(&self, connection_id: &ConnectionId) -> Option<broadcast::Receiver<Value>> {
        let mut state = self.state.lock().ok()?;
        let sender = state
            .event_senders
            .entry(connection_id.clone())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone();
        Some(sender.subscribe())
    }

    fn disconnected(&self, connection_id: &ConnectionId) {
        let statuses = self
            .runtime
            .prompt_queue()
            .disconnect_report(connection_id)
            .unwrap_or_default();
        for status in statuses.into_iter().filter(|status| !status.detached) {
            let mut params = json!({
                "sessionId": status.session_id,
                "_meta": {"keencode/operationId": status.operation_id}
            });
            if let Some(execution) = status.execution {
                params["_meta"]["keencode/turnId"] = Value::String(execution.turn_id);
            }
            let _ = self.session_cancel(&params);
        }
        if let Ok(mut state) = self.state.lock() {
            state.connections.remove(connection_id);
            state.initialized.remove(connection_id);
            state.connection_sessions.remove(connection_id);
            state.event_senders.remove(connection_id);
        }
        let _ = self.runtime.detach(connection_id);
    }
}

impl HostActivity for HeadlessInner {
    fn has_active_work(&self) -> bool {
        let Ok(state) = self.state.lock() else {
            return true;
        };
        state.web.running || !state.operations.is_empty()
    }
}

#[derive(Debug)]
struct HandlerError {
    code: i64,
    message: &'static str,
}

impl HandlerError {
    const fn new(code: i64, message: &'static str) -> Self {
        Self { code, message }
    }

    const fn internal() -> Self {
        Self::new(-32603, "Host 内部错误")
    }
}

fn map_core_error(error: keencode_runtime::HostCoreError) -> HandlerError {
    match error {
        keencode_runtime::HostCoreError::OperationNotFound => {
            HandlerError::new(-32004, "operation 不存在")
        }
        keencode_runtime::HostCoreError::QueueFull => {
            HandlerError::new(-32001, "Session admission queue 已满")
        }
        keencode_runtime::HostCoreError::OperationConflict => {
            HandlerError::new(-32009, "operationId 冲突")
        }
        keencode_runtime::HostCoreError::InvalidRequest(_) => {
            HandlerError::new(-32602, "请求参数无效")
        }
        _ => HandlerError::internal(),
    }
}

fn map_runtime_error(error: RuntimeError) -> HandlerError {
    match error {
        RuntimeError::SessionNotCreated
        | RuntimeError::SessionNotRegistered
        | RuntimeError::SessionCorrupt
        | RuntimeError::SessionBusy => HandlerError::new(-32004, "Session 不存在或不可用"),
        RuntimeError::SessionAlreadyExists | RuntimeError::SessionAlreadyRegistered => {
            HandlerError::new(-32009, "Session 已被占用")
        }
        RuntimeError::InvalidCreateRequest | RuntimeError::InvalidControlOperation => {
            HandlerError::new(-32602, "请求参数无效")
        }
        RuntimeError::SessionClosed => HandlerError::new(-32000, "Session 已关闭"),
        RuntimeError::RecoveryRequired => HandlerError::new(-32000, "Session 需要恢复"),
        _ => HandlerError::internal(),
    }
}

fn session_metadata_value(metadata: StoredSessionMetadata) -> Value {
    json!({
        "sessionId": metadata.session_id,
        "cwd": metadata.project_root,
        "title": metadata.title,
        "status": metadata.status,
        "updatedAtUnixMs": metadata.updated_at_unix_ms,
        "corrupt": metadata.corrupt,
    })
}

fn stop_reason_for_state(state: OperationState) -> &'static str {
    match state {
        OperationState::Cancelled => "cancelled",
        OperationState::Failed | OperationState::RecoveryRequired => "refusal",
        OperationState::Completed => "end_turn",
        OperationState::NeedsInput => "needs_input",
        OperationState::Admitted | OperationState::Claimed | OperationState::Running => "unknown",
    }
}

fn endpoint_for(root: &Path, fingerprint: &str) -> (HostTransportKind, String) {
    #[cfg(windows)]
    {
        let _ = root;
        return (
            HostTransportKind::NamedPipe,
            format!(r"\\.\pipe\keencode-{fingerprint}"),
        );
    }
    #[cfg(unix)]
    {
        let _ = fingerprint;
        return (
            HostTransportKind::UnixSocket,
            root.join("host.sock").to_string_lossy().into_owned(),
        );
    }
    #[allow(unreachable_code)]
    (
        HostTransportKind::UnixSocket,
        root.join("host.sock").to_string_lossy().into_owned(),
    )
}

fn cleanup_stale_endpoint(transport: &HostTransportKind, endpoint: &str) {
    #[cfg(unix)]
    if *transport == HostTransportKind::UnixSocket {
        use std::os::unix::fs::FileTypeExt;
        let path = Path::new(endpoint);
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_socket()
        {
            let _ = fs::remove_file(path);
        }
    }
    let _ = (transport, endpoint);
}

fn canonical_directory(value: &str) -> Result<String, HandlerError> {
    let path =
        fs::canonicalize(value).map_err(|_| HandlerError::new(-32602, "cwd 必须是存在的目录"))?;
    if !path.is_dir() {
        return Err(HandlerError::new(-32602, "cwd 必须是目录"));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn string_param(params: &Value, name: &str) -> Result<String, HandlerError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| HandlerError::new(-32602, "请求参数无效"))
}

fn prompt_text(params: &Value) -> Result<String, HandlerError> {
    if let Some(text) = params.get("prompt").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    let Some(blocks) = params.get("prompt").and_then(Value::as_array) else {
        return Err(HandlerError::new(-32602, "Prompt 无效"));
    };
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err(HandlerError::new(-32602, "当前 headless 只支持文本 Prompt"));
        }
        text.push_str(
            block
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| HandlerError::new(-32602, "Prompt 文本无效"))?,
        );
    }
    if text.is_empty() {
        return Err(HandlerError::new(-32602, "Prompt 不能为空"));
    }
    Ok(text)
}

fn operation_id(params: &Value) -> Option<String> {
    params
        .pointer("/_meta/keencode~1operationId")
        .and_then(Value::as_str)
        .or_else(|| params.get("operationId").and_then(Value::as_str))
        .map(str::to_owned)
}

fn next_id(prefix: &str) -> String {
    let sequence = NEXT_HEADLESS_ID.fetch_add(1, Ordering::Relaxed);
    let millis = headless_unix_time_ms();
    format!("headless-{prefix}-{millis}-{sequence}")
}

/// 返回非零 UTC Unix 毫秒，系统时钟异常时使用一作为信封校验下界。
fn headless_unix_time_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |value| value.as_millis()),
    )
    .ok()
    .filter(|value| *value > 0)
    .unwrap_or(1)
}

fn web_status(state: WebState) -> Value {
    json!({
        "state": if state.running { "running" } else { "stopped" },
        "port": state.port,
        "activeConnections": 0,
    })
}

/// 单个 headless 工具调用参数在实时聚合中的内存上限；超出后只保留工具身份，
/// 避免恶意 Provider 通过无限参数增量占满 Host 内存。
const MAX_HEADLESS_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;

/// 将 Journal 中的一条权威记录投影为 attach/replay 可消费的 ACP delivery。
///
/// Journal sequence 和 headless delivery sequence 是两个不同的游标：前者用于
/// 权威历史恢复，后者只表示本次 Host 投递顺序。原子批次内的多个更新共享同一个
/// Journal sequence，但各自取得独立 delivery sequence。
fn headless_authoritative_deliveries(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    provider: Option<&HeadlessProvider>,
    provider_by_turn: &mut BTreeMap<String, ProviderSnapshot>,
    next_delivery_sequence: &mut u64,
) -> Result<Vec<Value>, RuntimeError> {
    headless_authoritative_event_deliveries(
        session,
        state,
        record,
        &record.event,
        provider,
        provider_by_turn,
        next_delivery_sequence,
    )
}

fn headless_authoritative_event_deliveries(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    event: &SessionEvent,
    provider: Option<&HeadlessProvider>,
    provider_by_turn: &mut BTreeMap<String, ProviderSnapshot>,
    next_delivery_sequence: &mut u64,
) -> Result<Vec<Value>, RuntimeError> {
    if let SessionEvent::AtomicBatch { events } = event {
        let mut deliveries = Vec::new();
        for nested in events {
            deliveries.extend(headless_authoritative_event_deliveries(
                session,
                state,
                record,
                nested,
                provider,
                provider_by_turn,
                next_delivery_sequence,
            )?);
        }
        return Ok(deliveries);
    }

    let mut deliveries = Vec::new();
    match event {
        SessionEvent::SessionCreated { title, .. } | SessionEvent::SessionRenamed { title } => {
            push_headless_authoritative_update(
                &mut deliveries,
                record,
                None,
                None,
                next_delivery_sequence,
                keencode_acp::schema::SessionUpdate::SessionInfoUpdate(
                    keencode_acp::schema::SessionInfoUpdate::new().title(title.clone()),
                ),
            )?;
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            ..
        } => {
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(turn_id.as_str()),
                Some(source_agent_id.as_str()),
                next_delivery_sequence,
                KeenCodeEvent::TurnStarted {
                    root_turn_id: root_turn_id.as_str().to_owned(),
                    parent_turn_id: parent_turn_id
                        .as_ref()
                        .map(|value| value.as_str().to_owned()),
                },
            )?;
        }
        SessionEvent::TurnCompleted { turn_id } => {
            let agent_id = headless_turn_agent_id(state, turn_id.as_str())?;
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(turn_id.as_str()),
                Some(&agent_id),
                next_delivery_sequence,
                KeenCodeEvent::TurnCompleted,
            )?;
        }
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            let agent_id = headless_turn_agent_id(state, turn_id.as_str())?;
            let event = if *reason == TurnStopReason::Cancelled {
                KeenCodeEvent::TurnCancelled
            } else {
                KeenCodeEvent::TurnFailed {
                    failure_kind: keencode_acp::TurnFailureKind::Internal,
                    message: keencode_model::redact_error_secrets_bounded(message, 4096),
                }
            };
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(turn_id.as_str()),
                Some(&agent_id),
                next_delivery_sequence,
                event,
            )?;
        }
        SessionEvent::MessageAdded { message } => {
            deliveries.extend(headless_persisted_message_deliveries(
                session,
                state,
                record,
                message,
                provider,
                next_delivery_sequence,
            )?);
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            for message in &segment.messages {
                deliveries.extend(headless_persisted_message_deliveries(
                    session,
                    state,
                    record,
                    message,
                    provider,
                    next_delivery_sequence,
                )?);
            }
        }
        SessionEvent::TurnProviderSnapshotRecorded {
            turn_id, provider, ..
        } => {
            provider_by_turn.insert(turn_id.as_str().to_owned(), provider.clone());
        }
        SessionEvent::ModelRoundCompleted {
            turn_id,
            source_agent_id,
            requested_model,
            usage,
            ..
        } => {
            let context_window = provider_by_turn
                .get(turn_id.as_str())
                .filter(|snapshot| snapshot.model == *requested_model)
                .and_then(|snapshot| snapshot.context_window)
                .filter(|value| *value > 0);
            let used = usage.total_tokens.or_else(|| {
                usage
                    .input_tokens
                    .zip(usage.output_tokens)
                    .and_then(|(input, output)| input.checked_add(output))
            });
            if let (Some(used), Some(context_window)) = (used, context_window) {
                push_headless_authoritative_update(
                    &mut deliveries,
                    record,
                    Some(turn_id.as_str()),
                    Some(source_agent_id.as_str()),
                    next_delivery_sequence,
                    keencode_acp::schema::SessionUpdate::UsageUpdate(
                        keencode_acp::schema::UsageUpdate::new(used, context_window),
                    ),
                )?;
            }
        }
        SessionEvent::ToolRequested { request } => {
            push_headless_authoritative_update(
                &mut deliveries,
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                next_delivery_sequence,
                keencode_acp::schema::SessionUpdate::ToolCall(
                    keencode_acp::schema::ToolCall::new(
                        request.model_tool_call_id.clone(),
                        request.tool_name.clone(),
                    )
                    .raw_input(request.arguments.clone()),
                ),
            )?;
        }
        SessionEvent::ToolExecutionStarted { request_id } => {
            let request = headless_tool_request(state, request_id.as_str())?;
            push_headless_tool_update(
                &mut deliveries,
                record,
                request,
                next_delivery_sequence,
                keencode_acp::schema::ToolCallUpdateFields::new()
                    .status(keencode_acp::schema::ToolCallStatus::InProgress),
            )?;
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => {
            let request = headless_tool_request(state, request_id.as_str())?;
            let status = match outcome.status {
                ResourceToolCompletionStatus::Succeeded => {
                    keencode_acp::schema::ToolCallStatus::Completed
                }
                ResourceToolCompletionStatus::Failed
                | ResourceToolCompletionStatus::SideEffectUnknown
                | ResourceToolCompletionStatus::Cancelled => {
                    keencode_acp::schema::ToolCallStatus::Failed
                }
            };
            let raw_output = serde_json::to_value(&outcome.result.content)
                .map_err(|_| RuntimeError::InvalidControlOperation)?;
            push_headless_tool_update(
                &mut deliveries,
                record,
                request,
                next_delivery_sequence,
                keencode_acp::schema::ToolCallUpdateFields::new()
                    .status(status)
                    .raw_output(raw_output),
            )?;
        }
        SessionEvent::ToolSideEffectUnknown { request_id, result } => {
            let request = headless_tool_request(state, request_id.as_str())?;
            let raw_output = serde_json::to_value(&result.content)
                .map_err(|_| RuntimeError::InvalidControlOperation)?;
            push_headless_tool_update(
                &mut deliveries,
                record,
                request,
                next_delivery_sequence,
                keencode_acp::schema::ToolCallUpdateFields::new()
                    .status(keencode_acp::schema::ToolCallStatus::Failed)
                    .raw_output(raw_output),
            )?;
        }
        SessionEvent::CompactionApplied {
            turn_id,
            source_agent_id,
            compaction,
            ..
        } => {
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(turn_id.as_str()),
                Some(source_agent_id.as_str()),
                next_delivery_sequence,
                KeenCodeEvent::ContextCompactionCompleted {
                    replaced_through_sequence: record.sequence.saturating_sub(1),
                    estimated_tokens: compaction.estimated_tokens_after,
                },
            )?;
        }
        SessionEvent::TodoReplaced {
            items, revision, ..
        } => {
            let mut meta = keencode_acp::schema::Meta::new();
            meta.insert("_keencode".to_owned(), json!({"todoRevision": revision}));
            push_headless_authoritative_update(
                &mut deliveries,
                record,
                None,
                None,
                next_delivery_sequence,
                keencode_acp::schema::SessionUpdate::Plan(
                    keencode_acp::schema::Plan::new(
                        items
                            .iter()
                            .map(|item| {
                                keencode_acp::schema::PlanEntry::new(
                                    item.content.clone(),
                                    keencode_acp::schema::PlanEntryPriority::Medium,
                                    match item.status {
                                        keencode_resources::TodoStatus::Pending => {
                                            keencode_acp::schema::PlanEntryStatus::Pending
                                        }
                                        keencode_resources::TodoStatus::InProgress => {
                                            keencode_acp::schema::PlanEntryStatus::InProgress
                                        }
                                        keencode_resources::TodoStatus::Completed => {
                                            keencode_acp::schema::PlanEntryStatus::Completed
                                        }
                                    },
                                )
                            })
                            .collect(),
                    )
                    .meta(meta),
                ),
            )?;
        }
        SessionEvent::PlanChanged { plan } => {
            push_headless_authoritative_update(
                &mut deliveries,
                record,
                None,
                None,
                next_delivery_sequence,
                keencode_acp::schema::SessionUpdate::CurrentModeUpdate(
                    keencode_acp::schema::CurrentModeUpdate::new(if plan.enabled {
                        "plan"
                    } else {
                        "default"
                    }),
                ),
            )?;
        }
        SessionEvent::SubAgentSpawned { agent } => {
            if let Some(turn_id) = agent.current_turn_id.as_ref()
                && let Some(turn) = state.turns.get(turn_id)
                && let Some(parent_turn_id) = turn.parent_turn_id.as_ref()
            {
                push_headless_authoritative_event(
                    &mut deliveries,
                    record,
                    Some(parent_turn_id.as_str()),
                    Some(agent.parent_agent_id.as_str()),
                    next_delivery_sequence,
                    KeenCodeEvent::AgentSpawned {
                        agent_id: agent.agent_id.as_str().to_owned(),
                        parent_agent_id: agent.parent_agent_id.as_str().to_owned(),
                        agent_path: agent.agent_path.clone(),
                        task: agent.task.clone(),
                        parent_turn_id: parent_turn_id.as_str().to_owned(),
                        root_turn_id: turn.root_turn_id.as_str().to_owned(),
                    },
                )?;
            }
        }
        SessionEvent::SubAgentStatusChanged {
            agent_id,
            turn_id: Some(turn_id),
            status,
            ..
        } => {
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(turn_id.as_str()),
                Some(agent_id.as_str()),
                next_delivery_sequence,
                KeenCodeEvent::AgentStatusChanged {
                    agent_id: agent_id.as_str().to_owned(),
                    status: headless_agent_status(status),
                },
            )?;
        }
        SessionEvent::SubAgentStatusChanged { turn_id: None, .. } => {}
        SessionEvent::MailboxMessageQueued { message } => {
            push_headless_authoritative_event(
                &mut deliveries,
                record,
                Some(message.related_turn_id.as_str()),
                Some(message.from.as_str()),
                next_delivery_sequence,
                KeenCodeEvent::AgentMessageQueued {
                    message_id: message.message_id.as_str().to_owned(),
                    from_agent_id: message.from.as_str().to_owned(),
                    to_agent_id: message.to.as_str().to_owned(),
                },
            )?;
        }
        SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::ToolFileChangePrepared { .. }
        | SessionEvent::ToolFileChangeApplied { .. }
        | SessionEvent::TerminalStarted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::DynamicInputReceiptCommitted { .. }
        | SessionEvent::OnErrorHookQueued { .. }
        | SessionEvent::OnErrorHookReceiptCommitted { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::TitleGenerated { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {}
        | SessionEvent::AtomicBatch { .. } => unreachable!("AtomicBatch 已在递归入口处理"),
    }
    Ok(deliveries)
}

fn headless_persisted_message_deliveries(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    message: &SessionMessage,
    provider: Option<&HeadlessProvider>,
    next_delivery_sequence: &mut u64,
) -> Result<Vec<Value>, RuntimeError> {
    if message.is_meta
        || matches!(
            message.role,
            ResourceMessageRole::System | ResourceMessageRole::Developer
        )
    {
        return Ok(Vec::new());
    }
    let materialized = session.materialize_message(message)?;
    let turn_id = message.turn_id.as_ref().map(|value| value.as_str());
    let agent_id = message
        .agent_id
        .as_ref()
        .map(|value| value.as_str().to_owned())
        .or_else(|| turn_id.and_then(|value| headless_turn_agent_id(state, value).ok()));
    let mut deliveries = Vec::new();
    for block in materialized.content {
        let update = match (&message.role, block) {
            (ResourceMessageRole::User, ContentBlock::Text { text }) => {
                let text = provider.map_or(text.clone(), |provider| provider.redact(&text));
                keencode_acp::schema::SessionUpdate::UserMessageChunk(
                    keencode_acp::schema::ContentChunk::new(
                        keencode_acp::schema::ContentBlock::from(text),
                    )
                    .meta(Some(headless_message_meta(&message.message_id))),
                )
            }
            (ResourceMessageRole::User, ContentBlock::Image { image }) => {
                let content = match image.source {
                    ImageSource::Base64 { media_type, data } => {
                        keencode_acp::schema::ContentBlock::Image(
                            keencode_acp::schema::ImageContent::new(data, media_type),
                        )
                    }
                    ImageSource::Url { url } => keencode_acp::schema::ContentBlock::ResourceLink(
                        keencode_acp::schema::ResourceLink::new("image", url),
                    ),
                };
                keencode_acp::schema::SessionUpdate::UserMessageChunk(
                    keencode_acp::schema::ContentChunk::new(content)
                        .meta(Some(headless_message_meta(&message.message_id))),
                )
            }
            (ResourceMessageRole::Assistant, ContentBlock::Text { text }) => {
                let text = provider.map_or(text.clone(), |provider| provider.redact(&text));
                keencode_acp::schema::SessionUpdate::AgentMessageChunk(
                    keencode_acp::schema::ContentChunk::new(
                        keencode_acp::schema::ContentBlock::from(text),
                    )
                    .meta(Some(headless_message_meta(&message.message_id))),
                )
            }
            (ResourceMessageRole::Assistant, ContentBlock::Reasoning { reasoning }) => {
                let text = provider.map_or(reasoning.text.clone(), |provider| {
                    provider.redact(&reasoning.text)
                });
                keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
                    keencode_acp::schema::ContentChunk::new(
                        keencode_acp::schema::ContentBlock::from(text),
                    )
                    .meta(Some(headless_message_meta(&message.message_id))),
                )
            }
            (ResourceMessageRole::Assistant, ContentBlock::ToolCall { tool_call }) => {
                keencode_acp::schema::SessionUpdate::ToolCall(
                    keencode_acp::schema::ToolCall::new(tool_call.id, tool_call.name)
                        .raw_input(tool_call.arguments),
                )
            }
            (ResourceMessageRole::Tool, ContentBlock::ToolResult { tool_result }) => {
                let status = if tool_result.is_error {
                    keencode_acp::schema::ToolCallStatus::Failed
                } else {
                    keencode_acp::schema::ToolCallStatus::Completed
                };
                let raw_output = serde_json::to_value(&tool_result.content)
                    .map_err(|_| RuntimeError::InvalidControlOperation)?;
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        tool_result.tool_call_id,
                        keencode_acp::schema::ToolCallUpdateFields::new()
                            .status(status)
                            .raw_output(raw_output),
                    ),
                )
            }
            _ => continue,
        };
        if !matches!(message.role, ResourceMessageRole::User)
            && (turn_id.is_none() || agent_id.is_none())
        {
            return Err(RuntimeError::InvalidControlOperation);
        }
        push_headless_authoritative_update(
            &mut deliveries,
            record,
            turn_id,
            agent_id.as_deref(),
            next_delivery_sequence,
            update,
        )?;
    }
    Ok(deliveries)
}

fn headless_message_meta(message_id: &str) -> keencode_acp::schema::Meta {
    serde_json::Map::from_iter([(
        "keencode/messageId".to_owned(),
        Value::String(message_id.to_owned()),
    )])
}

fn push_headless_authoritative_update(
    deliveries: &mut Vec<Value>,
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    next_delivery_sequence: &mut u64,
    update: keencode_acp::schema::SessionUpdate,
) -> Result<(), RuntimeError> {
    *next_delivery_sequence = next_delivery_sequence.saturating_add(1).max(1);
    let envelope = SessionUpdateDeliveryEnvelope::new(
        record.session.as_str(),
        turn_id.map(str::to_owned),
        source_agent_id.map(str::to_owned),
        *next_delivery_sequence,
        record.time_unix_ms.max(1),
        update,
    )
    .map_err(|_| RuntimeError::InvalidControlOperation)?;
    deliveries.push(json!({
        "jsonrpc":"2.0",
        "method":"acp://delivery",
        "params":{"type":"session_update","envelope":envelope}
    }));
    Ok(())
}

fn push_headless_authoritative_event(
    deliveries: &mut Vec<Value>,
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    next_delivery_sequence: &mut u64,
    event: KeenCodeEvent,
) -> Result<(), RuntimeError> {
    *next_delivery_sequence = next_delivery_sequence.saturating_add(1).max(1);
    let params = match (turn_id, source_agent_id) {
        (Some(turn_id), Some(source_agent_id)) => KeenCodeEventEnvelopeParams::for_turn(
            record.session.as_str(),
            turn_id,
            source_agent_id,
            *next_delivery_sequence,
            record.time_unix_ms.max(1),
            event,
        ),
        (None, None) => KeenCodeEventEnvelopeParams::for_session(
            record.session.as_str(),
            *next_delivery_sequence,
            record.time_unix_ms.max(1),
            event,
        ),
        _ => return Err(RuntimeError::InvalidControlOperation),
    };
    let envelope = KeenCodeEventEnvelope::new_authoritative(record.sequence, params)
        .map_err(|_| RuntimeError::InvalidControlOperation)?;
    deliveries.push(json!({
        "jsonrpc":"2.0",
        "method":"acp://delivery",
        "params":{"type":"keencode_event","envelope":envelope}
    }));
    Ok(())
}

fn push_headless_tool_update(
    deliveries: &mut Vec<Value>,
    record: &SessionEventRecord,
    request: &ToolRequest,
    next_delivery_sequence: &mut u64,
    fields: keencode_acp::schema::ToolCallUpdateFields,
) -> Result<(), RuntimeError> {
    push_headless_authoritative_update(
        deliveries,
        record,
        Some(request.turn_id.as_str()),
        Some(request.agent_id.as_str()),
        next_delivery_sequence,
        keencode_acp::schema::SessionUpdate::ToolCallUpdate(
            keencode_acp::schema::ToolCallUpdate::new(request.model_tool_call_id.clone(), fields),
        ),
    )
}

fn headless_turn_agent_id(state: &SessionState, turn_id: &str) -> Result<String, RuntimeError> {
    state
        .turns
        .iter()
        .find(|(known, _)| known.as_str() == turn_id)
        .map(|(_, turn)| turn.source_agent_id.as_str().to_owned())
        .ok_or(RuntimeError::InvalidControlOperation)
}

fn headless_tool_request<'a>(
    state: &'a SessionState,
    request_id: &str,
) -> Result<&'a ToolRequest, RuntimeError> {
    state
        .tools
        .iter()
        .find(|(known, _)| known.as_str() == request_id)
        .map(|(_, lifecycle): (&_, &ToolLifecycle)| &lifecycle.request)
        .ok_or(RuntimeError::InvalidControlOperation)
}

fn headless_agent_status(
    status: &keencode_resources::SubAgentStatus,
) -> keencode_acp::AgentLifecycleStatus {
    match status {
        keencode_resources::SubAgentStatus::Pending => keencode_acp::AgentLifecycleStatus::Pending,
        keencode_resources::SubAgentStatus::Running => keencode_acp::AgentLifecycleStatus::Running,
        keencode_resources::SubAgentStatus::Waiting => keencode_acp::AgentLifecycleStatus::Waiting,
        keencode_resources::SubAgentStatus::Completed => {
            keencode_acp::AgentLifecycleStatus::Completed
        }
        keencode_resources::SubAgentStatus::Failed => keencode_acp::AgentLifecycleStatus::Failed,
        keencode_resources::SubAgentStatus::Interrupted => {
            keencode_acp::AgentLifecycleStatus::Interrupted
        }
        keencode_resources::SubAgentStatus::Stopped => keencode_acp::AgentLifecycleStatus::Stopped,
    }
}

/// 将 Runtime 临时 Agent 事件映射为标准 ACP 或 KeenCode 扩展 delivery。
fn headless_transient_delivery(
    session_id: &str,
    delivery_sequence: u64,
    provider_snapshot: &ProviderSnapshot,
    provider: &HeadlessProvider,
    event: &AgentStreamEvent,
    tool_arguments: &mut BTreeMap<String, String>,
) -> Option<Value> {
    let turn_id = event.turn_id().as_str().to_owned();
    let source_agent_id = event.source_agent_id().as_str().to_owned();
    match event.kind() {
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::TextDelta { delta, .. },
        } => headless_session_update_delivery(
            session_id,
            &turn_id,
            &source_agent_id,
            delivery_sequence,
            keencode_acp::schema::SessionUpdate::AgentMessageChunk(
                keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                    provider.redact(delta),
                )),
            ),
        ),
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::ReasoningDelta { delta, .. }
                | ModelStreamEvent::ReasoningSummaryDelta { delta, .. },
        } => headless_session_update_delivery(
            session_id,
            &turn_id,
            &source_agent_id,
            delivery_sequence,
            keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
                keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                    provider.redact(delta),
                )),
            ),
        ),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallStart { id, name, .. },
        } => {
            tool_arguments.insert(id.clone(), String::new());
            headless_session_update_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                keencode_acp::schema::SessionUpdate::ToolCall(
                    keencode_acp::schema::ToolCall::new(id.clone(), provider.redact(name))
                        .status(keencode_acp::schema::ToolCallStatus::InProgress),
                ),
            )
        }
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallArgumentsDelta { id, delta, .. },
        } => {
            if let Some(arguments) = tool_arguments.get_mut(id) {
                if arguments.len().saturating_add(delta.len()) <= MAX_HEADLESS_TOOL_ARGUMENT_BYTES {
                    arguments.push_str(delta);
                } else {
                    // 保持工具调用状态可见，但丢弃超限参数正文。
                    arguments.clear();
                }
            }
            None
        }
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallEnd { id, .. },
        } => {
            let raw_input = tool_arguments.remove(id).and_then(|arguments| {
                serde_json::from_str::<Value>(&provider.redact(&arguments)).ok()
            });
            let fields = keencode_acp::schema::ToolCallUpdateFields::new()
                .status(keencode_acp::schema::ToolCallStatus::InProgress)
                .raw_input(raw_input);
            headless_session_update_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(id.clone(), fields),
                ),
            )
        }
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::Usage { usage },
        } => {
            let context_window = provider_snapshot
                .context_window
                .filter(|value| *value > 0)?;
            let used = usage.total_tokens.or_else(|| {
                usage
                    .input_tokens
                    .zip(usage.output_tokens)
                    .and_then(|(input, output)| input.checked_add(output))
            });
            let used = used?;
            headless_session_update_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                keencode_acp::schema::SessionUpdate::UsageUpdate(
                    keencode_acp::schema::UsageUpdate::new(used, context_window),
                ),
            )
        }
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageStart { .. },
        } => headless_keencode_event_delivery(
            session_id,
            &turn_id,
            &source_agent_id,
            delivery_sequence,
            KeenCodeEvent::ModelFirstStreamObserved,
        ),
        AgentStreamEventKind::ModelFailure { .. } => headless_keencode_event_delivery(
            session_id,
            &turn_id,
            &source_agent_id,
            delivery_sequence,
            KeenCodeEvent::SystemNotification {
                level: keencode_acp::SystemNotificationLevel::Error,
                message: "模型请求失败".to_owned(),
            },
        ),
        AgentStreamEventKind::ContextCompactionStarted { estimated_tokens } => {
            headless_keencode_event_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                KeenCodeEvent::ContextCompactionStarted {
                    estimated_tokens: *estimated_tokens,
                },
            )
        }
        AgentStreamEventKind::ContextCompactionFailed { failure_kind } => {
            headless_keencode_event_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                KeenCodeEvent::ContextCompactionFailed {
                    failure_kind: match failure_kind {
                        ContextCompactionFailureKind::Model => {
                            keencode_acp::CompactionFailureKind::Model
                        }
                        ContextCompactionFailureKind::Budget => {
                            keencode_acp::CompactionFailureKind::Budget
                        }
                        ContextCompactionFailureKind::Storage => {
                            keencode_acp::CompactionFailureKind::Storage
                        }
                        ContextCompactionFailureKind::InvalidResult => {
                            keencode_acp::CompactionFailureKind::InvalidResult
                        }
                    },
                },
            )
        }
        AgentStreamEventKind::ContextCompactionTruncated { estimated_tokens } => {
            headless_keencode_event_delivery(
                session_id,
                &turn_id,
                &source_agent_id,
                delivery_sequence,
                KeenCodeEvent::ContextCompactionTruncated {
                    estimated_tokens: *estimated_tokens,
                },
            )
        }
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::DecodeTiming { .. }
                | ModelStreamEvent::MessageEnd { .. }
                | ModelStreamEvent::ReasoningContinuation { .. },
        } => None,
    }
}

/// 通过 ACP crate 的严格构造器生成标准 SessionUpdate delivery。
fn headless_session_update_delivery(
    session_id: &str,
    turn_id: &str,
    source_agent_id: &str,
    delivery_sequence: u64,
    update: keencode_acp::schema::SessionUpdate,
) -> Option<Value> {
    let envelope = SessionUpdateDeliveryEnvelope::new(
        session_id,
        Some(turn_id.to_owned()),
        Some(source_agent_id.to_owned()),
        delivery_sequence,
        headless_unix_time_ms(),
        update,
    )
    .ok()?;
    Some(json!({
        "jsonrpc":"2.0",
        "method":"acp://delivery",
        "params":{"type":"session_update","envelope":envelope}
    }))
}

/// 通过 ACP crate 的严格构造器生成 KeenCode 临时生命周期 delivery。
fn headless_keencode_event_delivery(
    session_id: &str,
    turn_id: &str,
    source_agent_id: &str,
    delivery_sequence: u64,
    event: KeenCodeEvent,
) -> Option<Value> {
    let envelope = KeenCodeEventEnvelope::new_transient(KeenCodeEventEnvelopeParams::for_turn(
        session_id,
        turn_id,
        source_agent_id,
        delivery_sequence,
        headless_unix_time_ms(),
        event,
    ))
    .ok()?;
    Some(json!({
        "jsonrpc":"2.0",
        "method":"acp://delivery",
        "params":{"type":"keencode_event","envelope":envelope}
    }))
}

fn valid_rpc_id(value: &Value) -> bool {
    matches!(value, Value::String(value) if !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        || matches!(value, Value::Number(_))
        || value.is_null()
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn headless_runtime_transient_text_reaches_session_update_delivery() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session = RuntimeSession::create_session(
            keencode_runtime::RuntimeConfig::new(root.path()),
            CreateSessionRequest {
                session_id: "headless-event-session".to_owned(),
                title: "headless event test".to_owned(),
                project_root: root.path().to_string_lossy().into_owned(),
            },
        )
        .expect("Runtime Session 应创建");
        let provider = Arc::new(keencode_model::ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                max_context_tokens: Some(4096),
                ..ProviderCapabilities::default()
            },
            [keencode_model::ScriptedReply::events([
                ModelStreamEvent::MessageStart {
                    metadata: keencode_model::ResponseMetadata::default(),
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    delta: "headless 增量".to_owned(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: keencode_model::StopReason::Completed,
                },
            ])],
        ));
        let mut subscription = session.subscribe().expect("Session 应能订阅实时事件");
        let input = Message::text(MessageRole::User, "测试 headless 事件");
        let request = TurnRequest::new(
            AgentSessionId::new(session.session_id().as_str()).expect("Agent Session ID 有效"),
            AgentTurnId::new("headless-event-turn").expect("Agent Turn ID 有效"),
            AgentId::new(ROOT_AGENT_ID).expect("Agent ID 有效"),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let bound = session.bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ));
        let run = bound.run_turn(RuntimeTurnRequest::root(
            request,
            vec![input],
            "测试 headless 事件",
        ));
        tokio::pin!(run);
        let headless_provider = HeadlessProvider {
            registry: ProviderRegistry::new(),
            provider_id: "test".to_owned(),
            model: "test-model".to_owned(),
            secret: None,
        };
        let provider_snapshot = ProviderSnapshot {
            provider_id: "test".to_owned(),
            model: "test-model".to_owned(),
            context_window: Some(4096),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "test".to_owned(),
            reasoning_effort: None,
        };
        let result = run.await;
        let mut mapped = None;
        for _ in 0..8 {
            let Ok(Ok(delivery)) =
                tokio::time::timeout(Duration::from_secs(1), subscription.recv()).await
            else {
                break;
            };
            if let RuntimeEventPayload::Transient(event) = delivery.payload
                && matches!(
                    event.kind(),
                    AgentStreamEventKind::ModelEvent {
                        event: ModelStreamEvent::TextDelta { .. }
                    }
                )
            {
                mapped = headless_transient_delivery(
                    "headless-event-session",
                    delivery.delivery_sequence,
                    &provider_snapshot,
                    &headless_provider,
                    &event,
                    &mut BTreeMap::new(),
                );
                break;
            }
        }
        assert!(result.expect("Runtime Turn 应完成").is_success());
        let mapped = mapped.expect("TextDelta 应映射为标准 ACP delivery");
        assert_eq!(mapped["params"]["type"], "session_update");
        assert_eq!(
            mapped["params"]["envelope"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        assert_eq!(
            mapped["params"]["envelope"]["update"]["content"]["text"],
            "headless 增量"
        );
    }

    #[tokio::test]
    async fn session_load_replays_authoritative_journal_with_strict_envelopes() {
        let root = tempfile::tempdir().expect("临时数据根应创建");
        let session = RuntimeSession::create_session(
            keencode_runtime::RuntimeConfig::new(root.path()),
            CreateSessionRequest {
                session_id: "headless-replay-session".to_owned(),
                title: "回放测试".to_owned(),
                project_root: root.path().to_string_lossy().into_owned(),
            },
        )
        .expect("Runtime Session 应创建");
        let provider = Arc::new(keencode_model::ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                max_context_tokens: Some(4096),
                ..ProviderCapabilities::default()
            },
            [keencode_model::ScriptedReply::events([
                ModelStreamEvent::MessageStart {
                    metadata: keencode_model::ResponseMetadata::default(),
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    delta: "Journal 回放响应".to_owned(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: keencode_model::StopReason::Completed,
                },
            ])],
        ));
        let input = Message::text(MessageRole::User, "Journal 回放请求");
        let turn_id = "headless-replay-turn";
        let request = TurnRequest::new(
            AgentSessionId::new(session.session_id().as_str()).expect("Agent Session ID 有效"),
            AgentTurnId::new(turn_id).expect("Agent Turn ID 有效"),
            AgentId::new(ROOT_AGENT_ID).expect("Agent ID 有效"),
            "replay-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let provider_snapshot = ProviderSnapshot {
            provider_id: "headless-test".to_owned(),
            model: "replay-model".to_owned(),
            context_window: Some(4096),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "headless-test-config".to_owned(),
            reasoning_effort: None,
        };
        let runner = session.bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ));
        assert!(
            runner
                .run_turn(
                    RuntimeTurnRequest::root(request, vec![input], "Journal 回放请求")
                        .with_provider_snapshot(provider_snapshot),
                )
                .await
                .expect("Runtime Turn 应完成")
                .is_success()
        );

        let state = session.snapshot().expect("Snapshot 应可读取").state;
        let page = session
            .replay(None, keencode_resources::MAX_REPLAY_PAGE_RECORDS)
            .expect("Journal 页面应可读取");
        assert!(!page.records.is_empty());
        let mut provider_by_turn = BTreeMap::new();
        let mut delivery_sequence = 0;
        let mut deliveries = Vec::new();
        for record in &page.records {
            deliveries.extend(
                headless_authoritative_deliveries(
                    &session,
                    &state,
                    record,
                    None,
                    &mut provider_by_turn,
                    &mut delivery_sequence,
                )
                .expect("Journal 记录应能投影"),
            );
        }
        assert!(
            deliveries.len() >= 5,
            "应包含 Session、Turn、消息和终态投影"
        );
        assert!(deliveries.windows(2).all(|pair| {
            pair[1]["params"]["envelope"]["deliverySequence"].as_u64()
                > pair[0]["params"]["envelope"]["deliverySequence"].as_u64()
        }));
        assert!(
            deliveries.iter().any(|delivery| {
                delivery["params"]["envelope"]["event"]["type"] == "turn_started"
            })
        );
        assert!(deliveries.iter().any(|delivery| {
            delivery["params"]["envelope"]["update"]["sessionUpdate"] == "user_message_chunk"
                && delivery["params"]["envelope"]["update"]["content"]["text"] == "Journal 回放请求"
        }));
        assert!(deliveries.iter().any(|delivery| {
            delivery["params"]["envelope"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && delivery["params"]["envelope"]["update"]["content"]["text"] == "Journal 回放响应"
        }));
        assert!(deliveries.iter().any(|delivery| {
            delivery["params"]["envelope"]["event"]["type"] == "turn_completed"
        }));
        for delivery in &deliveries {
            let raw = serde_json::to_vec(&delivery["params"]["envelope"])
                .expect("delivery envelope 应可编码");
            match delivery["params"]["type"].as_str() {
                Some("session_update") => {
                    SessionUpdateDeliveryEnvelope::decode_raw(&raw)
                        .expect("标准历史 delivery 应通过严格恢复校验");
                }
                Some("keencode_event") => {
                    let envelope = KeenCodeEventEnvelope::decode_raw(&raw)
                        .expect("权威事件 delivery 应通过严格恢复校验");
                    assert!(envelope.journal_sequence().is_some());
                }
                _ => panic!("未知历史 delivery 类型"),
            }
        }
    }

    #[tokio::test]
    async fn session_load_publishes_journal_history_to_attach_connection() {
        let root = tempfile::tempdir().expect("临时数据根应创建");
        let project_path = root.path().join("project");
        fs::create_dir_all(&project_path).expect("临时项目目录应创建");
        let runtime = match HostRuntime::acquire(root.path(), HostOwnerKind::Headless)
            .expect("Host Runtime 应取得 owner")
        {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(_) => panic!("测试不应连接既有 Host"),
        };
        let inner = HeadlessInner::new(runtime, None).expect("headless backend 应创建");
        let project = fs::canonicalize(project_path)
            .expect("项目目录应能规范化")
            .to_string_lossy()
            .into_owned();
        let session = inner
            .runtime_manager
            .create(CreateSessionRequest {
                session_id: "headless-attach-replay".to_owned(),
                title: "Attach 回放".to_owned(),
                project_root: project.clone(),
            })
            .expect("Session 应创建");
        let provider = Arc::new(keencode_model::ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [keencode_model::ScriptedReply::events([
                ModelStreamEvent::MessageStart {
                    metadata: keencode_model::ResponseMetadata::default(),
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    delta: "attach history".to_owned(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: keencode_model::StopReason::Completed,
                },
            ])],
        ));
        let input = Message::text(MessageRole::User, "attach history request");
        let request = TurnRequest::new(
            AgentSessionId::new(session.session_id().as_str()).expect("Agent Session ID 有效"),
            AgentTurnId::new("headless-attach-turn").expect("Agent Turn ID 有效"),
            AgentId::new(ROOT_AGENT_ID).expect("Agent ID 有效"),
            "attach-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let runner = session.bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ));
        assert!(
            runner
                .run_turn(RuntimeTurnRequest::root(
                    request,
                    vec![input],
                    "attach history request",
                ))
                .await
                .expect("Turn 应完成")
                .is_success()
        );

        let connection_id = ConnectionId::new("attach-replay-connection").expect("连接标识有效");
        inner
            .ensure_connection(&connection_id)
            .expect("连接应 attach");
        let mut events = inner.subscribe(&connection_id).expect("连接应可订阅");
        let loaded = inner
            .session_load(
                &connection_id,
                &json!({"sessionId":session.session_id().as_str(),"cwd":project}),
            )
            .expect("session/load 应成功");
        assert_eq!(loaded["_meta"]["keencode/replay"]["hasMore"], false);
        assert!(
            loaded["_meta"]["keencode/replay"]["replayedEvents"]
                .as_u64()
                .is_some_and(|value| value > 0)
        );
        let mut saw_terminal = false;
        for _ in 0..16 {
            let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("attach 历史应及时投递")
                .expect("attach 事件 channel 应保持打开");
            if event["params"]["envelope"]["event"]["type"] == "turn_completed" {
                saw_terminal = true;
                break;
            }
        }
        assert!(saw_terminal, "attach 应收到 Journal 中的 TurnCompleted");
    }

    #[test]
    fn headless_delivery_uses_strict_acp_envelopes() {
        let session_update = headless_session_update_delivery(
            "session-test",
            "turn-test",
            "root",
            7,
            keencode_acp::schema::SessionUpdate::AgentMessageChunk(
                keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                    "增量文本",
                )),
            ),
        )
        .expect("标准 SessionUpdate 应能构造 delivery");
        assert_eq!(session_update["params"]["type"], "session_update");
        let session_envelope =
            serde_json::to_vec(&session_update["params"]["envelope"]).expect("信封应可编码");
        let restored = SessionUpdateDeliveryEnvelope::decode_raw(&session_envelope)
            .expect("标准 ACP 信封应通过严格恢复校验");
        assert_eq!(restored.session_id(), "session-test");
        assert_eq!(restored.delivery_sequence(), 7);

        let keencode_event = headless_keencode_event_delivery(
            "session-test",
            "turn-test",
            "root",
            8,
            KeenCodeEvent::ModelFirstStreamObserved,
        )
        .expect("KeenCode 生命周期事件应能构造 delivery");
        assert_eq!(keencode_event["params"]["type"], "keencode_event");
        let event_envelope =
            serde_json::to_vec(&keencode_event["params"]["envelope"]).expect("信封应可编码");
        let restored = KeenCodeEventEnvelope::decode_raw(&event_envelope)
            .expect("KeenCode 信封应通过严格恢复校验");
        assert_eq!(restored.session_id(), "session-test");
        assert_eq!(restored.delivery_sequence(), 8);
    }

    #[test]
    fn headless_registers_executable_local_tools() {
        let root = tempfile::tempdir().expect("临时项目目录应创建");
        let environment =
            Arc::new(ToolEnvironment::new(root.path()).expect("headless 工具环境应创建"));
        let mut tools = ToolRegistry::new();
        register_local_tools(&mut tools, environment).expect("本地工具应注册");
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<BTreeSet<_>>();
        assert!(names.contains("Read"));
        assert!(names.contains("Edit"));
        assert!(names.contains("Write"));
        assert!(names.contains("Glob"));
        assert!(names.contains("Grep"));
        assert!(names.contains("Bash"));
        if cfg!(windows) {
            assert!(names.contains("PowerShell"));
        }
    }

    #[tokio::test]
    async fn prompt_text_cannot_forge_needs_input_and_duplicate_operation_replays_identity() {
        let root = tempfile::tempdir().expect("临时数据根应创建");
        let runtime = match HostRuntime::acquire(root.path(), HostOwnerKind::Headless)
            .expect("Host Runtime 应取得 owner")
        {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(_) => panic!("测试不应连接既有 Host"),
        };
        let inner = HeadlessInner::new(runtime, None).expect("headless backend 应创建");
        let connection_id =
            ConnectionId::new("test-elicitation-connection").expect("测试连接标识应合法");
        let mut events = inner
            .subscribe(&connection_id)
            .expect("headless 应提供事件订阅");

        let initialize = inner
            .dispatch_value(
                connection_id.clone(),
                json!({
                    "jsonrpc":"2.0",
                    "id":"initialize",
                    "method":"initialize",
                    "params":{}
                }),
            )
            .await
            .expect("initialize 应成功")
            .expect("initialize 应返回响应");
        assert_eq!(initialize["result"]["protocolVersion"], 1);

        let session = inner
            .dispatch_value(
                connection_id.clone(),
                json!({
                    "jsonrpc":"2.0",
                    "id":"new",
                    "method":"session/new",
                    "params":{"cwd":root.path()}
                }),
            )
            .await
            .expect("session/new 应成功")
            .expect("session/new 应返回响应");
        let session_id = session["result"]["sessionId"]
            .as_str()
            .expect("session/new 应返回 sessionId")
            .to_owned();

        let admission = inner
            .dispatch_value(
                connection_id.clone(),
                json!({
                    "jsonrpc":"2.0",
                    "id":"admit",
                    "method":"keencode/operation/admit",
                    "params":{
                        "sessionId":session_id,
                        "prompt":"needs-input",
                        "detached":true,
                        "_meta":{"keencode/operationId":"test-needs-input-operation"}
                    }
                }),
            )
            .await
            .expect("operation/admit 应成功")
            .expect("operation/admit 应返回响应");
        let operation_id = admission["result"]["operationId"]
            .as_str()
            .expect("admission 应返回 operationId")
            .to_owned();
        let first_execution = admission["result"]["execution"].clone();

        let duplicate = inner
            .dispatch_value(
                connection_id.clone(),
                json!({
                    "jsonrpc":"2.0",
                    "id":"duplicate",
                    "method":"keencode/operation/admit",
                    "params":{
                        "sessionId":session_id,
                        "prompt":"needs-input",
                        "detached":true,
                        "_meta":{"keencode/operationId":"test-needs-input-operation"}
                    }
                }),
            )
            .await
            .expect("重复 admission 应成功")
            .expect("重复 admission 应返回响应");
        assert_eq!(duplicate["result"]["execution"], first_execution);

        let terminal = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = events.recv().await.expect("事件 channel 应保持打开");
                if event["params"]["envelope"]["event"]["type"] == "turn_failed" {
                    break event;
                }
            }
        })
        .await
        .expect("无 Provider 时应收到失败终态");
        assert_ne!(terminal["method"], "elicitation/create");
        assert_eq!(
            terminal["params"]["envelope"]["event"]["stopReason"],
            "refusal"
        );

        let status = inner
            .dispatch_value(
                connection_id,
                json!({
                    "jsonrpc":"2.0",
                    "id":"status",
                    "method":"keencode/operation/status",
                    "params":{"operationId":operation_id}
                }),
            )
            .await
            .expect("operation/status 应成功")
            .expect("operation/status 应返回响应");
        assert_eq!(status["result"]["state"], "failed");
        assert!(status["result"].get("elicitationId").is_none());
    }

    #[tokio::test]
    async fn disconnect_and_stop_respect_detached_and_exact_operation_identity() {
        let root = tempfile::tempdir().expect("临时数据根应创建");
        let runtime = match HostRuntime::acquire(root.path(), HostOwnerKind::Headless)
            .expect("Host Runtime 应取得 owner")
        {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(_) => panic!("测试不应连接既有 Host"),
        };
        let inner = HeadlessInner::new(runtime.clone(), None).expect("headless backend 应创建");
        let connection_id = ConnectionId::new("disconnect-lifecycle").expect("连接标识应合法");
        inner
            .ensure_connection(&connection_id)
            .expect("测试连接应 attach");

        let admit = |operation: &str, session: &str, detached: bool| {
            let operation_id = OperationId::new(operation).expect("operationId 应合法");
            runtime
                .prompt_queue()
                .admit(PromptAdmissionRequest {
                    connection_id: connection_id.clone(),
                    session_id: session.to_owned(),
                    operation_id: operation_id.clone(),
                    prompt: operation.to_owned(),
                    payload_digest: format!("{:x}", Sha256::digest(operation.as_bytes())),
                    detached,
                })
                .expect("admission 应成功");
            operation_id
        };

        let foreground = admit("foreground-operation", "foreground-session", false);
        let detached = admit("detached-operation", "detached-session", true);
        inner.disconnected(&connection_id);
        assert_eq!(
            runtime
                .prompt_queue()
                .status(&foreground)
                .expect("前台 operation 应保留终态")
                .state,
            OperationState::Cancelled
        );
        assert_eq!(
            runtime
                .prompt_queue()
                .status(&detached)
                .expect("detached operation 应继续存在")
                .state,
            OperationState::Admitted
        );

        inner
            .ensure_connection(&connection_id)
            .expect("测试连接应重新 attach");
        let first = admit("stop-first", "stop-session", true);
        let second = admit("stop-second", "stop-session", true);
        let response = inner
            .session_cancel(&json!({
                "sessionId":"stop-session",
                "_meta":{"keencode/operationId":second}
            }))
            .expect("精确 stop 应成功");
        assert_eq!(response["cancelled"], true);
        assert_ne!(
            runtime
                .prompt_queue()
                .status(&first)
                .expect("未命中的 operation 应保留")
                .state,
            OperationState::Cancelled
        );
        assert_eq!(
            runtime
                .prompt_queue()
                .status(&second)
                .expect("命中的 operation 应保留取消终态")
                .state,
            OperationState::Cancelled
        );
    }

    #[tokio::test]
    async fn session_events_reach_later_attach_connection() {
        let root = tempfile::tempdir().expect("临时数据根应创建");
        let runtime = match HostRuntime::acquire(root.path(), HostOwnerKind::Headless)
            .expect("Host Runtime 应取得 owner")
        {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(_) => panic!("测试不应连接既有 Host"),
        };
        let inner = HeadlessInner::new(runtime, None).expect("headless backend 应创建");
        let origin = ConnectionId::new("origin-connection").expect("连接标识应合法");
        let attached = ConnectionId::new("attached-connection").expect("连接标识应合法");
        let mut origin_events = inner.subscribe(&origin).expect("原连接应订阅");
        let mut attached_events = inner.subscribe(&attached).expect("attach 连接应订阅");
        inner
            .state
            .lock()
            .expect("状态锁应可用")
            .connection_sessions
            .insert(attached.clone(), "session-a".to_owned());

        inner.publish_session_event(
            &origin,
            "session-a",
            json!({"jsonrpc":"2.0","method":"acp://delivery","params":{"value":1}}),
        );
        assert_eq!(
            origin_events.recv().await.expect("原连接应收到事件")["params"]["value"],
            1
        );
        assert_eq!(
            attached_events.recv().await.expect("attach 应收到事件")["params"]["value"],
            1
        );
    }
}
