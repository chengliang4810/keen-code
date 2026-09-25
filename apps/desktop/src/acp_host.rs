//! 进程内 ACP Host：把标准 JSON-RPC 请求路由到唯一 Agent Runtime。
//!
//! 该模块只负责协议边界、Session 控制面和响应编码。模型协议、工具执行、
//! Journal 归约与桌面实时投递仍由 [`AgentRuntime`] 和其下游组件负责。

use crate::agent_runtime::{
    AgentRuntime, AgentRuntimeError, RootTurnOptions, RootTurnStartOutcome, TaskNoticeKind,
    TaskTerminalNotice, unix_time_ms,
};
use crate::session_commands::{
    PLAN_MODE_CONTRACT_EN, ULTRA_MODE_CONTRACT_EN, authorize_stored_session_root,
    authorized_metadata, close_session_for_mutation, restore_session_after_mutation,
    retry_session_mutation, session_mode_state,
};
use keencode_acp::schema;
use keencode_acp::{
    AcpBoundaryError, AcpIncomingFrame, AcpNotification, AcpRequest, AcpRequestDecoder,
    AcpResponseEncoder, AcpResponseLimits, AcpResponsePayload, OperationId,
};
use keencode_agent::{CollaborationIdGenerator, UuidCollaborationIdGenerator};
use keencode_resources::{
    ROOT_AGENT_ID, ReasoningEffortSnapshot, SessionEvent, SessionForkRequest as RuntimeForkRequest,
    SessionId, TurnStatus, TurnStopReason,
};
use keencode_runtime::{
    AdmissionDisposition, ExecutionIdentity, HostPromptQueue, HostRuntime, OperationState,
    OperationStatus, OperationTerminal, PromptAdmissionRequest, RuntimeError, RuntimeEventPayload,
    RuntimeEventReceiveError, RuntimeEventSubscription, RuntimeSession, RuntimeSnapshot,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::{Notify, broadcast};
use tracing::Instrument;

#[cfg(feature = "benchmark")]
pub(crate) mod benchmark;
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
/// 历史页必须保持根回合完整；长工具链单轮可超过协议默认的 1 MiB。
const ACP_RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// 单个 Session 加载阶段超过该时长时记录结构化慢日志。
const SLOW_SESSION_LOAD_PHASE: Duration = Duration::from_millis(500);
/// 完整 Session 加载超过该时长时记录结构化慢日志。
const SLOW_SESSION_LOAD_TOTAL: Duration = Duration::from_secs(1);
/// ACP `_meta` 中可选的稳定创建操作标识。
const META_OPERATION_ID: &str = "keencode/operationId";
/// ACP `_meta` 中可选的精确 Turn 标识。
const META_TURN_ID: &str = "keencode/turnId";
/// ACP `_meta` 中可选的本轮 Ultra 开关。
const META_ULTRA_MODE: &str = "keencode/ultraMode";
/// Prompt 是否要求客户端断开后继续由 Host 托管；CLI detach 使用该元数据。
const META_DETACHED: &str = "keencode/detached";
/// 非交互 CLI 在绑定稳定执行身份后即可断开的 admission 方法。
const OPERATION_ADMIT_METHOD: &str = "keencode/operation/admit";
/// 跨连接查询 Prompt operation 状态的方法。
const OPERATION_STATUS_METHOD: &str = "keencode/operation/status";
/// ACP `_meta` 中可选的 Fork 标题。
const META_TITLE: &str = "keencode/title";
/// ACP 响应 `_meta` 中的最小 Session 快照键。
const META_SNAPSHOT: &str = "keencode/snapshot";
/// ACP 响应 `_meta` 中完整 `session/load` 历史恢复的最终游标事实。
const META_REPLAY: &str = "keencode/replay";
/// ACP 初始化响应 `_meta` 中的默认 Session cwd。
const META_DEFAULT_CWD: &str = "keencode/defaultCwd";
/// ACP `session/list` 每项 `_meta` 中的最近用户消息时间。
const META_LAST_USER_MESSAGE_AT: &str = "keencode/lastUserMessageAt";
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

/// Host bridge 的异步请求边界；实现不能把 JSON-RPC request id 改写成连接 id。
pub(crate) type AcpHostBridgeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + 'a>>;

/// Desktop 本地 Host 与 Desktop Client 共用的 ACP 业务边界。
///
/// Tauri/Web 只依赖这组最小操作，不直接知道 Host 是当前进程还是 discovery
/// 找到的另一个进程。事件订阅按 transport connection 隔离，避免把定向的
/// elicitation 请求广播给无关连接。
pub(crate) trait AcpHostBridge: Send + Sync + 'static {
    /// 分发一条完整 JSON-RPC 请求、通知或 Client Response。
    fn dispatch<'a>(
        &'a self,
        connection_id: &'a keencode_acp::ConnectionId,
        message: Value,
    ) -> AcpHostBridgeFuture<'a>;

    /// 订阅该连接对应的 Host 主动消息。
    fn subscribe(
        &self,
        _connection_id: &keencode_acp::ConnectionId,
    ) -> Option<broadcast::Receiver<Value>> {
        None
    }

    /// 清理连接级状态；不得因为 UI/CLI 断开而停止共享 Host。
    fn disconnect(&self, connection_id: &keencode_acp::ConnectionId);

    /// 发布由 Runtime 编码的 delivery；Remote bridge 不直接产生本地 Runtime 事件。
    fn publish_delivery(
        &self,
        _payload: Value,
        _target_connection_id: Option<&keencode_acp::ConnectionId>,
    ) {
    }
}

/// 全局唯一的当前进程 ACP bridge。
static ACP_HOST: OnceLock<Arc<dyn AcpHostBridge>> = OnceLock::new();

/// 记录 Session 加载阶段耗时；慢路径提升为 warn，便于在用户感知卡顿前发现回归。
fn record_session_load_phase(session_id: &str, phase: &str, elapsed: Duration) {
    let elapsed_ms = elapsed.as_millis();
    if elapsed >= SLOW_SESSION_LOAD_PHASE {
        tracing::warn!(
            target: "keencode_diagnostics",
            session_id,
            phase,
            elapsed_ms,
            "slow session load phase"
        );
    } else {
        tracing::info!(
            target: "keencode_diagnostics",
            session_id,
            phase,
            elapsed_ms,
            "session load phase completed"
        );
    }
}

/// ACP 握手状态；协议版本只在成功 initialize 后固定。
#[derive(Default)]
struct HandshakeState {
    /// 已经完成握手的协议版本。
    protocol_version: Option<schema::ProtocolVersion>,
    /// 首次握手协商出的完整 Client 能力；重复握手必须完全一致。
    client_capabilities: Option<schema::ClientCapabilities>,
}

/// 一个传输连接独立持有的 ACP 状态；不同连接不能共享能力协商。
#[derive(Default)]
struct ConnectionState {
    /// 当前连接的握手状态。
    handshake: HandshakeState,
}

/// 协议方法执行失败时使用的固定安全错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostFailure {
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
            Self::ProviderNotConfigured => schema::Error::internal_error()
                .data(serde_json::json!({"keencode/errorCode": "provider_not_configured"})),
            Self::ProviderReloadFailed => schema::Error::internal_error()
                .data(serde_json::json!({"keencode/errorCode": "provider_reload_failed"})),
            Self::Internal => schema::Error::internal_error(),
        }
    }
}

/// 当前桌面进程中唯一的 ACP Host 实例。
pub(crate) struct AcpHost {
    /// 用于读取应用授权目录、本地设置和扩展状态的 Tauri 句柄。
    app: AppHandle,
    /// 唯一的 Session/Turn Runtime。
    runtime: Arc<AgentRuntime>,
    /// 严格解码 JSON-RPC 请求。
    decoder: AcpRequestDecoder,
    /// 封闭类型化响应编码器。
    encoder: AcpResponseEncoder,
    /// 按传输连接隔离的握手状态；key 不能使用 JSON-RPC request id。
    connections: Mutex<BTreeMap<String, ConnectionState>>,
    /// 跨 Desktop/Web/CLI 共享的 Prompt admission 账本。
    prompt_queue: Arc<HostPromptQueue>,
    /// Desktop 所有权、连接生命周期与 Prompt queue 的统一事实源。
    host_runtime: Arc<HostRuntime>,
    /// Session active slot 释放后唤醒排队 Prompt。
    queue_wakeup: Arc<Notify>,
    /// 序列化新 Session 创建；已知 Session 的控制操作使用独立锁。
    control_gate: tokio::sync::Mutex<()>,
    /// 不同 Session 可并行恢复；同一 Session 的重放和修改仍有序。
    session_controls: Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// 每个本地 IPC 连接独立的事件出口；Session 更新广播给全部连接，
    /// Client Request 只投递给其 connection_id 指向的连接。
    event_sinks: Mutex<BTreeMap<String, broadcast::Sender<Value>>>,
    /// 让 trait object 入口仍能调用需要 `Arc<Self>` 的后台 Prompt driver。
    self_ref: Weak<AcpHost>,
}

/// 取得目标 Session 的共享锁；其他 Session 不会受其长时间历史投递影响。
fn session_control_lock(
    controls: &Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
    session_id: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, HostFailure> {
    SessionId::new(session_id.to_owned()).map_err(|_| HostFailure::InvalidParams)?;
    let mut controls = controls.lock().map_err(|_| HostFailure::Internal)?;
    if let Some(lock) = controls.get(session_id).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    controls.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    controls.insert(session_id.to_owned(), Arc::downgrade(&lock));
    Ok(lock)
}

/// 安装当前应用唯一 ACP Host；必须在 Agent Runtime 已进入 Tauri State 后调用。
pub(crate) fn install(
    app: &AppHandle,
    runtime: Arc<AgentRuntime>,
    host_runtime: Arc<HostRuntime>,
) -> Result<Arc<AcpHost>, String> {
    let response_limits = AcpResponseLimits::new(ACP_RESPONSE_MAX_BYTES, 64, 65_536)
        .map_err(|error| error.to_string())?;
    let encoder =
        AcpResponseEncoder::with_limits(response_limits).map_err(|error| error.to_string())?;
    let prompt_queue = Arc::clone(host_runtime.prompt_queue());
    let notification_runtime = Arc::clone(&runtime);
    let host = Arc::new_cyclic(|self_ref| AcpHost {
        app: app.clone(),
        runtime,
        decoder: AcpRequestDecoder::new(),
        encoder,
        connections: Mutex::new(BTreeMap::new()),
        prompt_queue,
        host_runtime,
        queue_wakeup: Arc::new(Notify::new()),
        control_gate: tokio::sync::Mutex::new(()),
        session_controls: Mutex::new(BTreeMap::new()),
        event_sinks: Mutex::new(BTreeMap::new()),
        self_ref: self_ref.clone(),
    });
    ACP_HOST
        .set(Arc::clone(&host) as Arc<dyn AcpHostBridge>)
        .map_err(|_| "ACP Host 已经初始化".to_owned())?;
    // 后台任务完成 → 主对话通知泵（ZCode 语义）：任务终态格式化为
    // task-notification 并以 detached Prompt 注入会话，忙时自动排队。
    // 必须用 Tauri 的 async runtime：install 在 setup 阶段执行，此时没有
    // Tokio reactor 上下文，tokio::spawn 会直接 panic 并 abort 进程。
    let notification_rx = notification_runtime.subscribe_task_completions();
    tauri::async_runtime::spawn(run_task_notification_pump(
        Arc::clone(&host),
        notification_rx,
    ));
    Ok(host)
}

/// 后台任务通知泉使用的固定连接标识；只用于 admission 归属，不代表真实连接。
const TASK_NOTIFICATION_CONNECTION: &str = "keencode-task-notification-pump";
/// 批处理窗口：首条通知到达后继续收集同窗终态的时长（ZCode 合批语义）。
const TASK_NOTIFICATION_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
/// /workflow 斜杠命令前缀。
const WORKFLOW_COMMAND_PREFIX: &str = "/workflow";

/// 解析 /workflow 后面的步骤规格：`{"steps":["a","b"]}` 或 `["a","b"]`。
fn parse_workflow_steps(spec: &str) -> Result<Vec<String>, HostFailure> {
    let trimmed = spec.trim();
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|_error| HostFailure::InvalidParams)?;
    let raw_steps = match &value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(object) => object
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .ok_or(HostFailure::InvalidParams)?,
        _ => return Err(HostFailure::InvalidParams),
    };
    let mut steps = Vec::with_capacity(raw_steps.len());
    for item in raw_steps {
        let Some(prompt) = item.as_str() else {
            return Err(HostFailure::InvalidParams);
        };
        if prompt.trim().is_empty() {
            return Err(HostFailure::InvalidParams);
        }
        steps.push(prompt.to_owned());
    }
    if steps.is_empty() {
        return Err(HostFailure::InvalidParams);
    }
    Ok(steps)
}

/// 顺序工作流步骤的执行结果。
#[derive(serde::Serialize)]
pub(crate) struct WorkflowStepOutcome {
    /// 步骤序号（从 0 开始）。
    pub step_index: usize,
    /// 该步 operation 标识。
    pub operation_id: String,
    /// 该步根 Turn 标识。
    pub turn_id: String,
    /// 归一化结束原因；Cancelled/Failed 时为诊断值。
    pub stop_reason: Option<schema::StopReason>,
}

/// 工作流 Journal 的单步记录：resume 按它跳过已完成步骤，amend 导入它们。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkflowJournalStep {
    pub index: usize,
    pub prompt: String,
    /// pending | running | succeeded | failed | cancelled
    pub status: String,
    #[serde(default)]
    pub operation_id: String,
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

/// 一个工作流 run 的持久化 Journal（app 数据根 workflows/<id>.json）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkflowJournal {
    pub workflow_id: String,
    pub session_id: String,
    pub steps_total: usize,
    pub steps: Vec<WorkflowJournalStep>,
}

impl WorkflowJournal {
    pub fn new(workflow_id: &str, session_id: &str, prompts: &[String]) -> Self {
        Self {
            workflow_id: workflow_id.to_owned(),
            session_id: session_id.to_owned(),
            steps_total: prompts.len(),
            steps: prompts
                .iter()
                .enumerate()
                .map(|(index, prompt)| WorkflowJournalStep {
                    index,
                    prompt: prompt.clone(),
                    status: "pending".to_owned(),
                    operation_id: String::new(),
                    turn_id: String::new(),
                    stop_reason: None,
                })
                .collect(),
        }
    }

    pub fn load(dir: &std::path::Path, workflow_id: &str) -> Option<Self> {
        let raw = std::fs::read_to_string(dir.join(format!("{workflow_id}.json"))).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self, dir: &std::path::Path) -> Result<(), HostFailure> {
        std::fs::create_dir_all(dir).map_err(|_| HostFailure::Internal)?;
        let path = dir.join(format!("{}.json", self.workflow_id));
        let tmp = dir.join(format!("{}.tmp", self.workflow_id));
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| HostFailure::Internal)?;
        std::fs::write(&tmp, bytes).map_err(|_| HostFailure::Internal)?;
        std::fs::rename(&tmp, &path).map_err(|_| HostFailure::Internal)?;
        Ok(())
    }

    pub fn step(&self, index: usize) -> Option<&WorkflowJournalStep> {
        self.steps.get(index)
    }

    pub fn set_status(&mut self, index: usize, status: &str) {
        if let Some(step) = self.steps.get_mut(index) {
            step.status = status.to_owned();
        }
    }
}

/// 解析 workflow Journal 目录：app 数据根下的 workflows/。
pub(crate) fn workflow_journal_dir(app: &AppHandle) -> Result<std::path::PathBuf, HostFailure> {
    let root = crate::storage::root_dir(app).map_err(|_| HostFailure::Internal)?;
    let dir = root.join("workflows");
    std::fs::create_dir_all(&dir).map_err(|_| HostFailure::Internal)?;
    Ok(dir)
}

/// 把一次后台任务终态格式化为 ZCode 风格的 task-notification 文本。
/// 该文本会作为合成用户输入开启（或排队进入）主对话的一个模型轮。
pub(crate) fn format_task_notification_text(notice: &TaskTerminalNotice) -> String {
    let mut lines = Vec::with_capacity(8);
    lines.push("<task-notification>".to_owned());
    lines.push(format!("<task-id>{}</task-id>", notice.task_id));
    lines.push(format!(
        "<kind>{}</kind>",
        match notice.kind {
            TaskNoticeKind::Shell => "shell",
            TaskNoticeKind::Agent => "agent",
        }
    ));
    if let Some(agent_id) = &notice.agent_id {
        lines.push(format!("<agent-id>{agent_id}</agent-id>"));
    }
    lines.push(format!("<status>{}</status>", notice.status_text));
    lines.push(format!("<duration-ms>{}</duration-ms>", notice.duration_ms));
    if let Some(summary) = &notice.summary {
        lines.push(format!("<summary>{summary}</summary>"));
    }
    lines.push("</task-notification>".to_owned());
    lines.push(match notice.kind {
        TaskNoticeKind::Shell => {
            "后台任务已结束：可用 TaskOutput(task_id) 读取增量输出；如需继续处理请直接行动，不要轮询。"
                .to_owned()
        }
        TaskNoticeKind::Agent => {
            "子代理已完成：权威结果已进入你的 mailbox，请读取结果并继续推进任务；如需继续与其协作可使用 send_message 或 followup_task。".to_owned()
        }
    });
    lines.join("\n")
}

/// 后台任务完成通知泉：把全局完成中继里的终态注入主对话。
///
/// 幂等性：同一 (session, task) 只通知一次；admission 忙时会自动排队
/// （next 优先级语义），EOF 即退出。
async fn run_task_notification_pump(
    host: Arc<AcpHost>,
    rx: tokio::sync::broadcast::Receiver<TaskTerminalNotice>,
) {
    let mut receiver = rx;
    let mut notified: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        // 首条通知到达后再收集一个短窗口内的后续终态：连续完成多个任务时
        // 合并为一条合成输入、只开启一个模型轮（ZCode 批处理语义）。
        let mut batch = vec![match receiver.recv().await {
            Ok(notice) => notice,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }];
        let closed = loop {
            match tokio::time::timeout(TASK_NOTIFICATION_BATCH_WINDOW, receiver.recv()).await {
                Ok(Ok(notice)) => batch.push(notice),
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break true,
                Err(_elapsed) => break false,
            }
        };
        batch.retain(|notice| notified.insert(format!("{}:{}", notice.session_id, notice.task_id)));
        if batch.is_empty() {
            if closed {
                break;
            }
            continue;
        }
        let session_id = batch[0].session_id.clone();
        let text = batch
            .iter()
            .map(format_task_notification_text)
            .collect::<Vec<_>>()
            .join("\n\n");
        let unix_ms = unix_time_ms();
        let operation_id = match OperationId::new(format!(
            "task-notify-{}-{}-{unix_ms}",
            batch[0].session_id, batch[0].task_id
        )) {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(session_id = %batch[0].session_id, task_id = %batch[0].task_id, %error, "invalid notification operation id");
                if closed {
                    break;
                }
                continue;
            }
        };
        let turn_id = format!("turn-notify-{}-{unix_ms}", batch[0].task_id);
        let connection_id = match keencode_acp::ConnectionId::new(TASK_NOTIFICATION_CONNECTION) {
            Ok(id) => id,
            Err(_) => break,
        };
        let project_root = match authorized_metadata(&host.runtime, &host.app, &session_id)
            .map(|(_, root)| root)
        {
            Ok(root) => root,
            Err(_) => {
                tracing::error!(session_id = %session_id, "cannot authorize notification session");
                continue;
            }
        };
        let payload_digest = prompt_payload_digest(&session_id, &turn_id, &text, false);
        let request = PromptDriveRequest {
            connection_id,
            session_id: session_id.clone(),
            operation_id,
            turn_id,
            text,
            project_root,
            payload_digest,
            ultra_mode: false,
            detached: true,
        };
        if let Err(failure) = host.admit_prompt(request).await {
            tracing::error!(
                session_id = %session_id,
                steps = batch.len(),
                ?failure,
                "failed to admit task notification prompt"
            );
        } else {
            tracing::info!(
                session_id = %session_id,
                steps = batch.len(),
                "task notification prompt admitted"
            );
        }
    }
}

/// 安装 Desktop Client 的远程 bridge；Remote 分支不能初始化本地 Runtime。
pub(crate) fn install_remote_bridge(bridge: Arc<dyn AcpHostBridge>) -> Result<(), String> {
    ACP_HOST
        .set(bridge)
        .map_err(|_| "ACP Host 已经初始化".to_owned())
}

/// Tauri 唯一 ACP 请求入口；标准通知成功或失败都不产生返回值。
#[tauri::command]
pub async fn acp_dispatch(message: serde_json::Value) -> Result<Option<serde_json::Value>, String> {
    let connection_id =
        keencode_acp::ConnectionId::new("embedded-desktop").map_err(|error| error.to_string())?;
    acp_dispatch_value_for_connection(&connection_id, message).await
}

/// 使用 transport 生成的稳定 ConnectionId 分发 ACP 请求。
pub(crate) async fn acp_dispatch_value_for_connection(
    connection_id: &keencode_acp::ConnectionId,
    message: serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    let host = ACP_HOST
        .get()
        .ok_or_else(|| "ACP Host 尚未初始化".to_owned())?;
    host.dispatch(connection_id, message).await
}

/// 传输断开时清理连接级握手；不会取消 detached operation 或停止 Host。
pub(crate) fn acp_disconnect(connection_id: &keencode_acp::ConnectionId) {
    if let Some(host) = ACP_HOST.get() {
        host.disconnect(connection_id);
    }
}

/// 将 Runtime 产生的标准 delivery 发布到当前 ACP bridge。
///
/// Owned Host 由本地 Runtime 调用，Remote Host 由 Remote Client 事件泵调用。
/// payload 已经是脱敏的 ACP delivery；这里不记录正文，也不重建 request id。
pub(crate) fn publish_delivery(
    payload: Value,
    target_connection_id: Option<&keencode_acp::ConnectionId>,
) {
    if let Some(host) = ACP_HOST.get() {
        host.publish_delivery(payload, target_connection_id);
    }
}

impl AcpHost {
    /// 传输连接断开只清理握手状态；HostPromptQueue 保留 operation 供重连恢复。
    fn disconnect(&self, connection_id: &keencode_acp::ConnectionId) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.remove(connection_id.as_str());
        }
        if let Ok(mut event_sinks) = self.event_sinks.lock() {
            event_sinks.remove(connection_id.as_str());
        }
        self.runtime
            .elicitation_coordinator()
            .disconnect(connection_id);
        if let Ok(reports) = self.prompt_queue.disconnect_report(connection_id)
            && !reports.is_empty()
        {
            tracing::info!(
                target: "keencode_diagnostics",
                connection_id = %connection_id,
                operations = reports.len(),
                "ACP connection detached; operations remain hosted"
            );
        }
        if let Err(error) = self.host_runtime.detach(connection_id) {
            tracing::debug!(
                target: "keencode_diagnostics",
                connection_id = %connection_id,
                %error,
                "ACP connection lifecycle was already detached"
            );
        }
    }

    /// 创建/取得一个连接级事件出口；广播 sender 只由 Host 持有。
    fn subscribe_events(
        &self,
        connection_id: &keencode_acp::ConnectionId,
    ) -> broadcast::Receiver<Value> {
        let mut event_sinks = self
            .event_sinks
            .lock()
            .expect("ACP event sink mutex poisoned");
        event_sinks
            .entry(connection_id.as_str().to_owned())
            .or_insert_with(|| broadcast::channel(256).0)
            .subscribe()
    }

    /// 发送一个 ACP delivery notification 或定向 Client Request。
    fn publish_delivery(
        &self,
        payload: Value,
        target_connection_id: Option<&keencode_acp::ConnectionId>,
    ) {
        let frame = if payload.get("type").and_then(Value::as_str) == Some("client_request")
            && let Some(request) = payload.get("request").filter(|value| value.is_object())
        {
            // CLI/NDJSON 客户端按原始 ACP Client Request 处理 elicitation；
            // Tauri/WebView 由 Remote Client 事件泵包装成统一 delivery。
            request.clone()
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "acp://delivery",
                "params": payload,
            })
        };
        let Ok(event_sinks) = self.event_sinks.lock() else {
            return;
        };
        match target_connection_id {
            Some(connection_id) => {
                if let Some(sender) = event_sinks.get(connection_id.as_str()) {
                    let _ = sender.send(frame);
                }
            }
            None => {
                for sender in event_sinks.values() {
                    let _ = sender.send(frame.clone());
                }
            }
        }
    }

    /// 将 Client Response 路由给既有 ElicitationCoordinator，并同步 admission 状态。
    fn route_client_response(
        &self,
        connection_id: &keencode_acp::ConnectionId,
        response_json: &str,
    ) -> Result<(), String> {
        let request_id = serde_json::from_str::<Value>(response_json)
            .ok()
            .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned));
        let request_id = request_id.ok_or_else(|| "ACP Client Response 缺少请求标识".to_owned())?;
        let coordinator = self.runtime.elicitation_coordinator();
        match coordinator.pending_connection_for_request(&request_id) {
            Some(target) if &target == connection_id => {}
            Some(_) => {
                tracing::error!(target: "keencode_diagnostics", request_id = %request_id, "ACP Client Response 来自非目标连接");
                return Err("ACP Client Response 来自非目标连接".to_owned());
            }
            None => {
                // 响应迟到或对应问答已被其他路径收口：记日志供挂死排查，
                // 迟到响应本身无需再唤醒任何等待方。
                tracing::warn!(target: "keencode_diagnostics", request_id = %request_id, "ACP Client Response 无匹配待决请求（迟到或已收口）");
                return Err("ACP 待决请求不存在或已经结束".to_owned());
            }
        }
        // 严格响应路由会移除 pending，先只读取 operation 身份；任何错误、迟到或
        // 非目标连接响应都必须在改变 HostPromptQueue 前失败。
        let operation = Some(request_id.as_str()).and_then(|request_id| {
            coordinator
                .pending_session_id_for_request(request_id)
                .and_then(|session_id| {
                    self.prompt_queue
                        .operation_for_session(&session_id)
                        .ok()
                        .flatten()
                        .map(|status| (status, session_id))
                })
        });
        crate::client_request::route_client_response_from_connection(
            self.runtime.as_ref(),
            connection_id,
            response_json,
        )?;
        if let Some((status, _session_id)) = operation.as_ref()
            && matches!(
                status.state,
                OperationState::Claimed | OperationState::Running
            )
        {
            let _ = self
                .prompt_queue
                .mark_needs_input(&status.operation_id, request_id.clone())
                .map_err(|error| error.to_string())?;
            let digest = format!("{:x}", Sha256::digest(response_json.as_bytes()));
            let answer_id =
                OperationId::new(format!("answer-{digest}")).map_err(|error| error.to_string())?;
            let _ = self
                .prompt_queue
                .answer_elicitation(&status.operation_id, request_id, answer_id, digest)
                .map_err(|error| error.to_string())?;
            self.queue_wakeup.notify_waiters();
        }
        Ok(())
    }

    /// 只等待当前 Session 的控制操作，弱引用表不永久保留历史锁。
    async fn lock_session_control(
        &self,
        session_id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, HostFailure> {
        let started = std::time::Instant::now();
        let lock = session_control_lock(&self.session_controls, session_id)?;
        let guard = lock.lock_owned().await;
        tracing::info!(target: "keencode_diagnostics", phase = "session_control_wait", elapsed_ms = started.elapsed().as_millis(), "session phase completed");
        Ok(guard)
    }

    /// 严格解码并分发一个 JSON-RPC 值，同时尽可能原样保留合法请求 ID。
    async fn dispatch(
        self: &Arc<Self>,
        connection_id: &keencode_acp::ConnectionId,
        message: Value,
    ) -> Result<Option<Value>, String> {
        let started = std::time::Instant::now();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("client_response")
            .to_owned();
        let observability = self
            .app
            .try_state::<Arc<crate::diagnostics::Diagnostics>>()
            .map(|diagnostics| diagnostics.observability());
        let span = tracing::info_span!(target: "keencode_diagnostics", "acp.request",
            method = %method,
            request_id = %message.get("id").filter(|id| id.is_string() || id.is_number()).unwrap_or(&serde_json::Value::Null),
            session_id = message.pointer("/params/sessionId").and_then(serde_json::Value::as_str).unwrap_or(""),
            turn_id = message.pointer("/params/_meta/keencode~1turnId").and_then(serde_json::Value::as_str).unwrap_or(""),
            operation_id = message.pointer("/params/_meta/keencode~1operationId").and_then(serde_json::Value::as_str).unwrap_or(""));
        async {
            tracing::info!(target: "keencode_diagnostics", "request started");
            let result = self.dispatch_inner(connection_id, message).await;
            match &result {
                Err(error) => tracing::error!(%error, elapsed_ms = started.elapsed().as_millis(), "ACP transport failed"),
                Ok(Some(value)) if value.get("error").is_some() => {
                    tracing::error!(code = %value.pointer("/error/code").unwrap_or(&serde_json::Value::Null), elapsed_ms = started.elapsed().as_millis(), "ACP request failed");
                }
                Ok(value) => tracing::info!(target: "keencode_diagnostics", session_id = value.as_ref().and_then(|v| v.pointer("/result/sessionId")).and_then(serde_json::Value::as_str).unwrap_or(""), elapsed_ms = started.elapsed().as_millis(), "request completed"),
            }
            if let Some(observability) = observability.as_deref() {
                let status = match &result {
                    Err(_) => "error",
                    Ok(Some(value)) if value.get("error").is_some() => "error",
                    Ok(_) => "ok",
                };
                let elapsed_ms = started.elapsed().as_millis() as u64;
                observability.increment_counter(&format!("host.acp.requests.{status}"), 1);
                observability.record_histogram("host.acp.request_duration_ms", elapsed_ms as f64);
                observability.record_trace(crate::diagnostics::observability::TraceSample {
                    trace_id: format!("acp:{}:{}:{}", connection_id, method, started.elapsed().as_nanos()),
                    span_id: format!("acp:{}:{}", connection_id, method),
                    parent_span_id: None,
                    name: "host.acp.request".to_owned(),
                    started_at_ms: crate::diagnostics::observability::now_epoch_ms()
                        .saturating_sub(elapsed_ms),
                    duration_ms: Some(elapsed_ms),
                    ttft_ms: None,
                    status: status.to_owned(),
                    attributes: BTreeMap::from([
                        ("method".to_owned(), method.clone()),
                        ("connection_scope".to_owned(), "host".to_owned()),
                    ]),
                });
            }
            result
        }.instrument(span).await
    }

    async fn dispatch_inner(
        self: &Arc<Self>,
        connection_id: &keencode_acp::ConnectionId,
        message: Value,
    ) -> Result<Option<Value>, String> {
        // WebSocket ACP 只允许标准 ACP/Session 方法；本地 Web Host 生命周期控制
        // 属于 Tauri facade，不能借由已认证浏览器连接获得宿主控制权。
        if connection_id.as_str() == "embedded-desktop"
            && let Some(response) = crate::web_host::dispatch_control(&self.app, &message).await?
        {
            return Ok(Some(response));
        }
        if looks_like_client_response(&message) {
            let response_json = serde_json::to_string(&message)
                .map_err(|_| "ACP Client Response 无法序列化".to_owned())?;
            self.route_client_response(connection_id, &response_json)?;
            return Ok(None);
        }
        if matches!(
            message.get("method").and_then(Value::as_str),
            Some(OPERATION_ADMIT_METHOD | OPERATION_STATUS_METHOD)
        ) {
            let id = request_id_from_value(&message).unwrap_or(schema::RequestId::Null);
            if matches!(id, schema::RequestId::Null) {
                return self.error_value(id, HostFailure::InvalidRequest).map(Some);
            }
            if !self.is_initialized(connection_id) {
                return self.error_value(id, HostFailure::AuthRequired).map(Some);
            }
            let result = self
                .dispatch_operation_method(connection_id, &message)
                .await;
            return match result {
                Ok(result) => {
                    let id = serde_json::to_value(id)
                        .map_err(|_| "ACP 请求 ID 无法序列化".to_owned())?;
                    Ok(Some(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result,
                    })))
                }
                Err(failure) => self.error_value(id, failure).map(Some),
            };
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
                if !matches!(&request, AcpRequest::Initialize(_))
                    && !self.is_initialized(connection_id)
                {
                    return self.error_value(id, HostFailure::AuthRequired).map(Some);
                }
                match self
                    .dispatch_request(connection_id, id.clone(), request)
                    .await
                {
                    Ok(value) => Ok(Some(value)),
                    Err(failure) => self.error_value(id, failure).map(Some),
                }
            }
            AcpIncomingFrame::Notification(notification) => {
                // 握手前的通知不改变 Host 状态；按 JSON-RPC 约定静默丢弃。
                if self.is_initialized(connection_id) {
                    self.dispatch_notification(connection_id, notification)
                        .await;
                }
                Ok(None)
            }
        }
    }

    /// 分发不属于标准 ACP Schema、但由本机 CLI 共享的 operation 生命周期方法。
    async fn dispatch_operation_method(
        self: &Arc<Self>,
        connection_id: &keencode_acp::ConnectionId,
        message: &Value,
    ) -> Result<Value, HostFailure> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or(HostFailure::InvalidRequest)?;
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or(HostFailure::InvalidParams)?;
        match method {
            OPERATION_ADMIT_METHOD => {
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or(HostFailure::InvalidParams)?
                    .to_owned();
                let text = params
                    .get("prompt")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(HostFailure::InvalidParams)?
                    .to_owned();
                if params.get("detached").and_then(Value::as_bool) != Some(true) {
                    return Err(HostFailure::InvalidParams);
                }
                let operation_id = params
                    .get("_meta")
                    .and_then(Value::as_object)
                    .and_then(|meta| meta.get(META_OPERATION_ID))
                    .and_then(Value::as_str)
                    .ok_or(HostFailure::InvalidParams)?;
                let operation_id = OperationId::new(operation_id.to_owned())
                    .map_err(|_| HostFailure::InvalidParams)?;
                let turn_id = params
                    .get("_meta")
                    .and_then(Value::as_object)
                    .and_then(|meta| meta.get(META_TURN_ID))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(default_turn_id);
                let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
                    .map_err(|_| HostFailure::ResourceNotFound)?;
                // 与 headless adapter 保持一致：detach 重试的指纹只由 Prompt 正文和
                // HostCore 的 sessionId 共同决定，随机分配的 turnId 不参与冲突判断。
                let payload_digest = format!("{:x}", Sha256::digest(text.as_bytes()));
                let status = self
                    .admit_prompt(PromptDriveRequest {
                        connection_id: connection_id.clone(),
                        session_id,
                        operation_id,
                        turn_id,
                        text,
                        project_root,
                        payload_digest,
                        ultra_mode: false,
                        detached: true,
                    })
                    .await?;
                if status.execution.is_none() {
                    return Err(HostFailure::Internal);
                }
                let status_value =
                    serde_json::to_value(&status).map_err(|error| internal_failure(error))?;
                Ok(serde_json::json!({
                    "operationId": status.operation_id,
                    "sessionId": status.session_id,
                    "execution": status.execution,
                    "state": status.state,
                    "status": status_value,
                }))
            }
            OPERATION_STATUS_METHOD => {
                let operation_id = params
                    .get("operationId")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        params
                            .get("_meta")
                            .and_then(Value::as_object)
                            .and_then(|meta| meta.get(META_OPERATION_ID))
                            .and_then(Value::as_str)
                    })
                    .ok_or(HostFailure::InvalidParams)?;
                let operation_id = OperationId::new(operation_id.to_owned())
                    .map_err(|_| HostFailure::InvalidParams)?;
                let status = self
                    .prompt_queue
                    .status(&operation_id)
                    .map_err(map_prompt_queue_failure)?;
                serde_json::to_value(status).map_err(|error| internal_failure(error))
            }
            _ => Err(HostFailure::MethodNotFound),
        }
    }

    /// 分发一个已严格解码且带请求 ID 的标准 ACP 请求。
    async fn dispatch_request(
        self: &Arc<Self>,
        connection_id: &keencode_acp::ConnectionId,
        id: schema::RequestId,
        request: AcpRequest,
    ) -> Result<Value, HostFailure> {
        match request {
            AcpRequest::Initialize(request) => {
                let response = self.handle_initialize(connection_id, request)?;
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
                let response = self.handle_prompt(connection_id, request).await?;
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
    async fn dispatch_notification(
        &self,
        connection_id: &keencode_acp::ConnectionId,
        notification: AcpNotification,
    ) {
        match notification {
            AcpNotification::Cancel(notification) => {
                self.handle_cancel(connection_id, notification).await
            }
            AcpNotification::SessionConfigUpdate(_) => {
                // 配置通知只作为 ACP 输入边界保留；配置刷新由现有 Tauri 控制面完成。
            }
        }
    }

    /// 返回当前是否已完成初始化握手。
    fn is_initialized(&self, connection_id: &keencode_acp::ConnectionId) -> bool {
        self.connections
            .lock()
            .ok()
            .and_then(|states| {
                states
                    .get(connection_id.as_str())
                    .and_then(|state| state.handshake.protocol_version.clone())
            })
            .is_some()
    }

    /// 完成一次只支持协议版本 1 的初始化握手。
    fn handle_initialize(
        &self,
        connection_id: &keencode_acp::ConnectionId,
        request: schema::InitializeRequest,
    ) -> Result<keencode_acp::InitializeResponseDto, HostFailure> {
        if request.protocol_version != SUPPORTED_PROTOCOL_VERSION
            && request.protocol_version != schema::ProtocolVersion::LATEST
        {
            return Err(HostFailure::InvalidParams);
        }
        let default_cwd = crate::workspace::app_data_session_root(&self.app)
            .map_err(|error| internal_failure(error))?
            .to_string_lossy()
            .into_owned();
        let mut connections = self
            .connections
            .lock()
            .map_err(|error| internal_failure(error))?;
        let state = connections
            .entry(connection_id.as_str().to_owned())
            .or_default();
        if state.handshake.protocol_version.is_some() {
            if state.handshake.client_capabilities.as_ref() != Some(&request.client_capabilities) {
                return Err(HostFailure::InvalidParams);
            }
            self.runtime
                .elicitation_coordinator()
                .negotiate_connection_capabilities(connection_id, &request.client_capabilities)
                .map_err(|_| HostFailure::InvalidParams)?;
            return initialize_response(default_cwd, &self.host_runtime);
        }
        self.runtime
            .elicitation_coordinator()
            .negotiate_connection_capabilities(connection_id, &request.client_capabilities)
            .map_err(|_| HostFailure::InvalidParams)?;
        let response = initialize_response(default_cwd, &self.host_runtime)?;
        self.host_runtime
            .attach_client(connection_id.clone())
            .map_err(|error| internal_failure(error))?;
        state.handshake.protocol_version = Some(SUPPORTED_PROTOCOL_VERSION);
        state.handshake.client_capabilities = Some(request.client_capabilities);
        Ok(response)
    }

    /// 创建新 Session，并在返回前建立唯一实时投递世代。
    async fn handle_new_session(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, HostFailure> {
        let _control = self.control_gate.lock().await;
        let mcp_servers = request.mcp_servers.clone();
        let project_root = self.authorized_cwd(&request.cwd)?;
        if !mcp_servers.is_empty() {
            self.ensure_extensions(&project_root).await?;
        }
        let operation_id = operation_id(request.meta.as_ref())?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, None, &operation_id)
            .map_err(map_runtime_failure)?;
        let session_id = session.session_id().as_str().to_owned();
        initialize_unpublished_session_mcp(&self.runtime, &session_id, &project_root, mcp_servers)
            .await?;
        self.runtime
            .focus_session(&session_id)
            .map_err(map_runtime_failure)?;
        self.runtime
            .ensure_session_delivery(&session_id)
            .map_err(map_runtime_failure)?;
        let snapshot = session
            .snapshot()
            .map_err(|error| internal_failure(error))?;
        let config_options = self.config_options(&snapshot)?;
        Ok(
            schema::NewSessionResponse::new(schema::SessionId::new(session_id))
                .modes(session_mode_state(snapshot.state.plan.enabled))
                .config_options(config_options)
                .meta(Some(snapshot_meta(&self.app, &snapshot, None))),
        )
    }

    /// 标准 load 保持完整重放；显式 history 扩展先投递最近窗口。
    async fn handle_load_session(
        &self,
        request: schema::LoadSessionRequest,
    ) -> Result<schema::LoadSessionResponse, HostFailure> {
        let mcp_servers = request.mcp_servers.clone();
        let session_id = request.session_id.0.as_ref().to_owned();
        let _control = self.lock_session_control(&session_id).await?;
        let load_started = std::time::Instant::now();
        let requested_root = self.authorized_cwd(&request.cwd)?;
        let history = request
            .meta
            .as_ref()
            .and_then(|meta| meta.get("keencode/history"))
            .map(|value| {
                serde_json::from_value::<crate::agent_runtime::HistoryLoadRequest>(value.clone())
                    .map_err(|_| HostFailure::InvalidParams)
            })
            .transpose()?;
        if history.as_ref().is_some_and(|request| !request.validate()) {
            return Err(HostFailure::InvalidParams);
        }
        if history
            .as_ref()
            .is_some_and(|request| request.cursor.is_some())
        {
            if !mcp_servers.is_empty() {
                return Err(HostFailure::InvalidParams);
            }
            // 后续页只校验绑定并读取窗口，不能重新 open 或重置实时投递。
            let (_, stored_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
                .map_err(|_| HostFailure::ResourceNotFound)?;
            if requested_root != stored_root {
                return Err(HostFailure::ResourceNotFound);
            }
            let page = self
                .runtime
                .load_history_page(&session_id, history.unwrap())
                .await
                .map_err(map_runtime_failure)?;
            let mut meta = Map::new();
            meta.insert(
                "keencode/history".to_owned(),
                serde_json::to_value(page).map_err(internal_failure)?,
            );
            return Ok(schema::LoadSessionResponse::new().meta(Some(meta)));
        }
        let mcp_project_root = requested_root.clone();
        let runtime = Arc::clone(&self.runtime);
        let app = self.app.clone();
        let id = session_id.clone();
        let span = tracing::Span::current();
        let session = tokio::task::spawn_blocking(move || {
            let _span = span.enter();
            let started = std::time::Instant::now();
            let stored_root = authorize_stored_session_root(&runtime, &app, &id)
                .map_err(|_| HostFailure::ResourceNotFound)?;
            if requested_root != stored_root {
                return Err(HostFailure::ResourceNotFound);
            }
            record_session_load_phase(&id, "session_authorize", started.elapsed());
            let started = std::time::Instant::now();
            // 仅恢复目标日志，查看历史不需要 MCP/LSP 或供应商网络连接。
            let session = runtime
                .open_or_create_session(&stored_root, Some(&id), "acp-load")
                .map_err(map_runtime_failure)?;
            record_session_load_phase(&id, "session_open", started.elapsed());
            Ok::<_, HostFailure>(session)
        })
        .await
        .map_err(internal_failure)??;
        record_session_load_phase(&session_id, "session_restore", load_started.elapsed());
        if !mcp_servers.is_empty() {
            self.ensure_extensions(&mcp_project_root).await?;
        }
        self.runtime
            .replace_session_mcp_servers(&session_id, &mcp_project_root, mcp_servers)
            .await
            .map_err(map_session_mcp_failure)?;
        self.runtime
            .ensure_healthy_session_delivery(&session_id)
            .await
            .map_err(map_runtime_failure)?;
        let started = std::time::Instant::now();
        let history_page = match history {
            Some(request) => Some(
                self.runtime
                    .load_history_page(&session_id, request)
                    .await
                    .map_err(map_runtime_failure)?,
            ),
            None => None,
        };
        let replay = match history_page.as_ref() {
            Some(page) => page.replay.clone().ok_or(HostFailure::Internal)?,
            None => self.replay_full_session(&session_id).await?,
        };
        record_session_load_phase(&session_id, "session_history_delivery", started.elapsed());
        let started = std::time::Instant::now();
        let snapshot = session
            .snapshot()
            .map_err(|error| internal_failure(error))?;
        let config_options = self.config_options(&snapshot)?;
        let mut meta = snapshot_meta(&self.app, &snapshot, None);
        if let Some(page) = history_page {
            meta.insert(
                "keencode/history".to_owned(),
                serde_json::to_value(page).map_err(internal_failure)?,
            );
        }
        meta.insert(
            META_REPLAY.to_owned(),
            serde_json::to_value(&replay).map_err(|error| internal_failure(error))?,
        );
        record_session_load_phase(&session_id, "session_response", started.elapsed());
        let total_elapsed = load_started.elapsed();
        let total_elapsed_ms = total_elapsed.as_millis();
        let state = &snapshot.state;
        if total_elapsed >= SLOW_SESSION_LOAD_TOTAL {
            tracing::warn!(
                target: "keencode_diagnostics",
                session_id,
                phase = "session_load_total",
                elapsed_ms = total_elapsed_ms,
                journal_bytes = snapshot.journal_bytes,
                event_records = state.last_sequence,
                transcript_records = state.transcript.len(),
                turns = state.turns.len(),
                model_rounds = state.model_rounds.len(),
                tools = state.tools.len(),
                sub_agents = state.sub_agents.len(),
                mailbox_messages = state.mailbox.len(),
                "slow session load"
            );
        } else {
            tracing::info!(
                target: "keencode_diagnostics",
                session_id,
                phase = "session_load_total",
                elapsed_ms = total_elapsed_ms,
                journal_bytes = snapshot.journal_bytes,
                event_records = state.last_sequence,
                transcript_records = state.transcript.len(),
                turns = state.turns.len(),
                model_rounds = state.model_rounds.len(),
                tools = state.tools.len(),
                sub_agents = state.sub_agents.len(),
                mailbox_messages = state.mailbox.len(),
                "session load completed"
            );
        }
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

    /// 合并文本 Prompt、交给 Host 后台 driver，并等待可重放的 operation 终态。
    async fn handle_prompt(
        self: &Arc<Self>,
        connection_id: &keencode_acp::ConnectionId,
        request: schema::PromptRequest,
    ) -> Result<schema::PromptResponse, HostFailure> {
        let session_id = request.session_id.0.as_ref().to_owned();
        let text = prompt_text(request.prompt)?;
        // /workflow 斜杠命令：后续文本是 {"steps":[...]} 或直接字符串数组。
        if let Some(spec) = text.strip_prefix(WORKFLOW_COMMAND_PREFIX) {
            let steps = parse_workflow_steps(spec)?;
            let workflow_id = format!("{:x}", unix_time_ms());
            let journal_dir = workflow_journal_dir(&self.app).map_err(|_| HostFailure::Internal)?;
            let outcomes = self
                .run_workflow(&session_id, &workflow_id, steps, &journal_dir)
                .await?;
            let report = serde_json::json!({
                "workflowId": workflow_id,
                "steps": outcomes,
            });
            let mut meta = Map::new();
            meta.insert(
                "keencode/workflow".to_owned(),
                serde_json::Value::String(serde_json::to_string(&report).unwrap_or_default()),
            );
            return Ok(schema::PromptResponse::new(schema::StopReason::EndTurn).meta(Some(meta)));
        }
        let turn_id = prompt_turn_id(request.meta.as_ref())?;
        let ultra_mode = meta_bool(request.meta.as_ref(), META_ULTRA_MODE)?;
        let detached = meta_bool(request.meta.as_ref(), META_DETACHED)?;
        let operation_id = OperationId::new(operation_id(request.meta.as_ref())?)
            .map_err(|_| HostFailure::InvalidParams)?;
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        // 授权成功后立即绑定 Web 连接，覆盖 Prompt 响应返回前产生的实时增量；
        // Desktop 连接不在 Web adapter 中，WebHostManager 会安全跳过该绑定。
        if let Some(web_host) = self.app.try_state::<Arc<crate::web_host::WebHostManager>>()
            && let Err(error) = web_host.bind_connection_session(connection_id, &session_id)
        {
            tracing::error!(
                target: "keencode_diagnostics",
                connection_id = %connection_id,
                session_id,
                %error,
                "failed to bind Web connection before Prompt admission"
            );
            return Err(HostFailure::Internal);
        }
        let payload_digest = prompt_payload_digest(&session_id, &turn_id, &text, ultra_mode);
        let status = self
            .admit_prompt(PromptDriveRequest {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                turn_id,
                text,
                project_root,
                payload_digest,
                ultra_mode,
                detached,
            })
            .await?;
        let execution = status.execution.ok_or(HostFailure::Internal)?;
        self.wait_for_operation_terminal(&operation_id).await?;
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-prompt-result")
            .map_err(map_runtime_failure)?;
        let terminal = self
            .snapshot_terminal(&session, &execution.turn_id)?
            .ok_or(HostFailure::Internal)?;
        let final_snapshot = self
            .runtime
            .session_snapshot(&session_id)
            .map_err(map_runtime_failure)?;
        let stop_reason = prompt_stop_reason(&terminal)?;
        Ok(
            schema::PromptResponse::new(stop_reason).meta(Some(snapshot_meta(
                &self.app,
                &final_snapshot,
                Some(&execution.turn_id),
            ))),
        )
    }

    /// 将 Prompt admission 与请求连接解耦；driver 一旦创建就由 Host 持有到终态。
    async fn admit_prompt(
        self: &Arc<Self>,
        request: PromptDriveRequest,
    ) -> Result<OperationStatus, HostFailure> {
        let admission_started = std::time::Instant::now();
        let admission = self
            .prompt_queue
            .admit(PromptAdmissionRequest {
                connection_id: request.connection_id.clone(),
                session_id: request.session_id.clone(),
                operation_id: request.operation_id.clone(),
                prompt: request.text.clone(),
                payload_digest: request.payload_digest.clone(),
                detached: request.detached,
            })
            .map_err(map_prompt_queue_failure)?;
        if let Some(observability) = self
            .app
            .try_state::<Arc<crate::diagnostics::Diagnostics>>()
            .map(|diagnostics| diagnostics.observability())
        {
            let disposition = admission.disposition.to_string();
            observability.increment_counter(&format!("host.prompt.admission.{disposition}"), 1);
            observability.record_histogram(
                "host.prompt.admission_duration_ms",
                admission_started.elapsed().as_millis() as f64,
            );
        }
        if admission.disposition != AdmissionDisposition::Duplicate {
            let host = Arc::clone(self);
            tokio::spawn(async move {
                host.drive_prompt(request).await;
            });
        }
        self.wait_for_operation_execution(&admission.operation_id)
            .await
    }

    /// 启动并观察一个已 admission 的 Prompt；传输断开不会取消该后台任务。
    async fn drive_prompt(self: Arc<Self>, request: PromptDriveRequest) {
        if let Err((failure, code)) = self.drive_prompt_inner(&request).await {
            tracing::error!(
                target: "keencode_diagnostics",
                operation_id = %request.operation_id,
                ?failure,
                "Prompt background driver failed"
            );
            self.finish_operation_failure(&request.operation_id, code);
        }
    }

    /// 执行后台 Prompt 的单一生命周期；错误码只记录阶段，不包含 Prompt 正文。
    async fn drive_prompt_inner(
        &self,
        request: &PromptDriveRequest,
    ) -> Result<(), (HostFailure, &'static str)> {
        loop {
            let notified = self.queue_wakeup.notified();
            match self
                .prompt_queue
                .claim_operation(&request.session_id, &request.operation_id)
                .map_err(|error| (map_prompt_queue_failure(error), "operation_claim_failed"))?
            {
                Some(_) => break,
                None => {
                    let status =
                        self.prompt_queue
                            .status(&request.operation_id)
                            .map_err(|error| {
                                (map_prompt_queue_failure(error), "operation_status_failed")
                            })?;
                    if status.state.is_terminal() {
                        return Ok(());
                    }
                    notified.await;
                }
            }
        }
        self.ensure_extensions(&request.project_root)
            .await
            .map_err(|failure| (failure, "extension_setup_failed"))?;
        let prompt_start_control = self
            .lock_session_control(&request.session_id)
            .await
            .map_err(|failure| (failure, "session_control_failed"))?;
        let session = self
            .runtime
            .open_or_create_session(
                &request.project_root,
                Some(&request.session_id),
                "acp-prompt",
            )
            .map_err(|error| (map_runtime_failure(error), "session_open_failed"))?;
        self.runtime
            .ensure_session_delivery(&request.session_id)
            .map_err(|error| (map_runtime_failure(error), "delivery_setup_failed"))?;
        let snapshot = session
            .snapshot()
            .map_err(|error| (internal_failure(error), "session_snapshot_failed"))?;
        let developer_context = self
            .developer_context(snapshot.state.plan.enabled, request.ultra_mode)
            .map_err(|failure| (failure, "developer_context_failed"))?;
        let memory_settings = crate::app_settings::get(&self.app)
            .map_err(|error| (internal_failure(error), "settings_failed"))?;
        // 先订阅再启动，保证快速终态也能由 driver 归约到 operation 账本。
        let mut events = session
            .subscribe()
            .map_err(|error| (internal_failure(error), "subscription_failed"))?;
        let outcome = self
            .runtime
            .start_root_turn(
                &request.session_id,
                &request.turn_id,
                &request.text,
                RootTurnOptions {
                    developer_context,
                    plan_enabled: snapshot.state.plan.enabled,
                    elicitation_connection_id: Some(request.connection_id.clone()),
                },
            )
            .await
            .map_err(|error| (map_runtime_failure(error), "runtime_start_failed"))?;
        let execution = ExecutionIdentity::new(
            request.session_id.clone(),
            request.turn_id.clone(),
            request.turn_id.clone(),
        )
        .map_err(|error| (internal_failure(error), "execution_identity_failed"))?;
        self.prompt_queue
            .bind_execution(&request.operation_id, execution)
            .map_err(|error| (internal_failure(error), "execution_bind_failed"))?;
        self.queue_wakeup.notify_waiters();
        drop(prompt_start_control);
        if matches!(outcome, RootTurnStartOutcome::Started)
            && memory_settings.local_memories
            && let Some(memories) = self.app.try_state::<Arc<crate::memories::MemoryService>>()
        {
            memories.trigger(
                Arc::clone(&self.runtime),
                Some(request.session_id.clone()),
                memory_settings.interface_language,
                false,
            );
        }
        let terminal = self
            .wait_for_turn_terminal(
                &session,
                &request.turn_id,
                &request.operation_id,
                &mut events,
            )
            .await
            .map_err(|failure| (failure, "runtime_terminal_wait_failed"))?;
        self.finish_operation_terminal(&request.operation_id, &terminal)
            .map_err(|failure| (failure, "operation_finish_failed"))
    }

    /// 等到 operation 已绑定稳定执行身份，供 detach 调用安全返回。
    async fn wait_for_operation_execution(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HostFailure> {
        loop {
            let notified = self.queue_wakeup.notified();
            let status = self
                .prompt_queue
                .status(operation_id)
                .map_err(map_prompt_queue_failure)?;
            if status.execution.is_some() || status.state.is_terminal() {
                return Ok(status);
            }
            notified.await;
        }
    }

    /// 顺序执行一个 /workflow 的全部步骤。
    ///
    /// 每个步骤都是一个真实的 detached Prompt 回合：写权威 Journal、在会话中
    /// 可见、可取消；后续步骤天然继承同一会话的历史上下文。任一步骤未正常
    /// 完成即中止剩余步骤。Journal 持久化每步状态：resume 时已完成步骤按
    /// Journal 记录跳过（只回放结果），running 视为中断重跑该步。
    pub(crate) async fn run_workflow(
        self: &Arc<Self>,
        session_id: &str,
        workflow_id: &str,
        steps: Vec<String>,
        journal_dir: &std::path::Path,
    ) -> Result<Vec<WorkflowStepOutcome>, HostFailure> {
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let mut journal = WorkflowJournal::load(journal_dir, workflow_id)
            .unwrap_or_else(|| WorkflowJournal::new(workflow_id, session_id, &steps));
        journal.steps_total = journal.steps.len();
        let connection_id = keencode_acp::ConnectionId::new(TASK_NOTIFICATION_CONNECTION)
            .map_err(|_| HostFailure::InvalidParams)?;
        let mut outcomes = Vec::new();
        for (index, prompt) in steps.into_iter().enumerate() {
            if journal
                .step(index)
                .is_some_and(|step| step.status == "succeeded")
            {
                // resume：已完成步骤按 Journal 记录回放，不再执行。
                let record = journal.step(index).expect("上条件已确认存在");
                outcomes.push(WorkflowStepOutcome {
                    step_index: index,
                    operation_id: record.operation_id.clone(),
                    turn_id: record.turn_id.clone(),
                    stop_reason: Some(schema::StopReason::EndTurn),
                });
                continue;
            }
            journal.set_status(index, "running");
            let _ = journal.save(journal_dir);
            let unix_ms = unix_time_ms();
            let operation_id =
                OperationId::new(format!("workflow-{workflow_id}-{index}-{unix_ms}"))
                    .map_err(|_| HostFailure::InvalidParams)?;
            let turn_id = format!("turn-workflow-{workflow_id}-{index}-{unix_ms}");
            let payload_digest = prompt_payload_digest(session_id, &turn_id, &prompt, false);
            let admitted = self
                .admit_prompt(PromptDriveRequest {
                    connection_id: connection_id.clone(),
                    session_id: session_id.to_owned(),
                    operation_id: operation_id.clone(),
                    turn_id,
                    text: prompt,
                    project_root: project_root.clone(),
                    payload_digest,
                    ultra_mode: false,
                    detached: true,
                })
                .await?;
            let terminal_status = self
                .wait_for_operation_terminal(&admitted.operation_id)
                .await?;
            let executed_turn_id = terminal_status
                .execution
                .as_ref()
                .map(|identity| identity.turn_id.clone())
                .ok_or(HostFailure::Internal)?;
            let session = self
                .runtime
                .open_or_create_session(&project_root, Some(session_id), "workflow-step")
                .map_err(map_runtime_failure)?;
            let stop_reason = self
                .snapshot_terminal(&session, executed_turn_id.as_str())
                .ok()
                .flatten()
                .and_then(|terminal| prompt_stop_reason(&terminal).ok());
            let finished_cleanly = terminal_status.state.is_terminal()
                && stop_reason == Some(schema::StopReason::EndTurn);
            journal.set_status(
                index,
                if finished_cleanly {
                    "succeeded"
                } else if stop_reason == Some(schema::StopReason::Cancelled) {
                    "cancelled"
                } else {
                    "failed"
                },
            );
            if let Some(step) = journal.steps.get_mut(index) {
                step.operation_id = admitted.operation_id.as_str().to_owned();
                step.turn_id = executed_turn_id.clone();
                step.stop_reason = stop_reason.as_ref().map(|reason| format!("{reason:?}"));
            }
            let _ = journal.save(journal_dir);
            outcomes.push(WorkflowStepOutcome {
                step_index: index,
                operation_id: admitted.operation_id.as_str().to_owned(),
                turn_id: executed_turn_id.clone(),
                stop_reason,
            });
            if !finished_cleanly {
                break;
            }
        }
        let _ = journal.save(journal_dir);
        Ok(outcomes)
    }

    /// 等到后台 driver 写入终态；连接 future 被取消不影响 driver 自身。
    async fn wait_for_operation_terminal(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HostFailure> {
        loop {
            let notified = self.queue_wakeup.notified();
            let status = self
                .prompt_queue
                .status(operation_id)
                .map_err(map_prompt_queue_failure)?;
            if status.state.is_terminal() {
                return Ok(status);
            }
            notified.await;
        }
    }

    /// 将启动/等待阶段失败收口为 operation 终态；失败摘要不包含 Prompt 正文。
    fn finish_operation_failure(&self, operation_id: &OperationId, code: &str) {
        let Ok(terminal) = OperationTerminal::new(code, None::<String>) else {
            return;
        };
        match self
            .prompt_queue
            .finish(operation_id, OperationState::Failed, terminal)
        {
            Ok(_) => self.queue_wakeup.notify_waiters(),
            Err(error) => tracing::error!(
                target: "keencode_diagnostics",
                operation_id = %operation_id,
                %error,
                "failed to finalize Prompt operation"
            ),
        }
    }

    /// 把 Runtime 根 Turn 的权威终态映射到 Host admission 账本。
    fn finish_operation_terminal(
        &self,
        operation_id: &OperationId,
        terminal: &TerminalTurn,
    ) -> Result<(), HostFailure> {
        if self
            .prompt_queue
            .status(operation_id)
            .map_err(map_prompt_queue_failure)?
            .state
            .is_terminal()
        {
            return Ok(());
        }
        let state = match terminal.status {
            TurnStatus::Completed => OperationState::Completed,
            TurnStatus::Cancelled => OperationState::Cancelled,
            TurnStatus::Failed => OperationState::Failed,
            TurnStatus::Running => return Err(HostFailure::Internal),
        };
        let code = match (terminal.status.clone(), terminal.stop_reason) {
            (TurnStatus::Completed, None) => "completed",
            (TurnStatus::Cancelled, Some(TurnStopReason::Cancelled)) => "cancelled",
            (TurnStatus::Failed, Some(TurnStopReason::LimitReached)) => "limit_reached",
            (TurnStatus::Failed, Some(TurnStopReason::ModelOutputLimit)) => "max_tokens",
            (TurnStatus::Failed, Some(TurnStopReason::ModelRefusal)) => "refusal",
            (TurnStatus::Failed, Some(TurnStopReason::ContextBlocked)) => "context_blocked",
            (TurnStatus::Failed, Some(TurnStopReason::Failed)) => "failed",
            _ => "failed",
        };
        let receipt = OperationTerminal::new(code, None::<String>)
            .map_err(|error| internal_failure(error))?;
        self.prompt_queue
            .finish(operation_id, state, receipt)
            .map_err(|error| internal_failure(error))?;
        self.queue_wakeup.notify_waiters();
        Ok(())
    }

    /// 构造标准 Session 配置目录；只公开无凭据的 Provider、模型和推理强度。
    fn config_options(
        &self,
        snapshot: &RuntimeSnapshot,
    ) -> Result<Vec<schema::SessionConfigOption>, HostFailure> {
        let catalog =
            crate::acp_provider_catalog(&self.app).map_err(|error| internal_failure(error))?;
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
        let catalog =
            crate::acp_provider_catalog(&self.app).map_err(|error| internal_failure(error))?;
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
        let session_id = request.session_id.0.as_ref().to_owned();
        let _control = self.lock_session_control(&session_id).await?;
        let metadata = match self
            .runtime
            .runtime_manager()
            .stored_session_metadata(&session_id)
        {
            Ok(metadata) => metadata,
            // 删除响应丢失后允许客户端幂等重试。
            Err(RuntimeError::SessionNotCreated) => {
                return Ok(keencode_acp::DeleteSessionResponse::new());
            }
            Err(error) => return Err(internal_failure(error)),
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
                    .map_err(|error| internal_failure(error))?
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
            .map_err(|error| internal_failure(error))?;
        if self
            .runtime
            .focused_session_id()
            .map_err(|error| internal_failure(error))?
            .as_deref()
            == Some(session_id.as_str())
        {
            self.runtime.clear_focus();
        }
        Ok(keencode_acp::DeleteSessionResponse::new())
    }

    /// 通过标准配置项显式更新模型绑定或推理强度，拒绝未知配置。
    ///
    /// 推理强度只在下一次模型请求读取，运行中会话也可直接修改；模型绑定会改写
    /// 正在运行 Turn 使用的 Provider，因此仍要求会话空闲。
    async fn handle_set_config_option(
        &self,
        request: schema::SetSessionConfigOptionRequest,
    ) -> Result<schema::SetSessionConfigOptionResponse, HostFailure> {
        let session_id = request.session_id.0.as_ref().to_owned();
        let _control = self.lock_session_control(&session_id).await?;
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-config")
            .map_err(map_runtime_failure)?;
        let operation_id = operation_id(request.meta.as_ref())?;
        let config_id = request.config_id.0.as_ref();
        let value = request.value.0.as_ref();
        match config_id {
            CONFIG_MODEL_ID => {
                if session
                    .has_active_work()
                    .map_err(|error| internal_failure(error))?
                {
                    return Err(HostFailure::InvalidParams);
                }
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
        let session_id = request.session_id.0.as_ref().to_owned();
        let _control = self.lock_session_control(&session_id).await?;
        let (_, project_root) = authorized_metadata(&self.runtime, &self.app, &session_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        let session = self
            .runtime
            .open_or_create_session(&project_root, Some(&session_id), "acp-mode")
            .map_err(map_runtime_failure)?;
        let snapshot = session
            .snapshot()
            .map_err(|error| internal_failure(error))?;
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
            .map_err(|error| internal_failure(error))?
        {
            let same_request = matches!(
                &record.event,
                SessionEvent::PlanChanged { plan } if plan.enabled == requested_plan_enabled
            );
            if !same_request {
                return Err(HostFailure::InvalidParams);
            }
            let current = session
                .snapshot()
                .map_err(|error| internal_failure(error))?;
            return Ok(schema::SetSessionModeResponse::new()
                .meta(Some(snapshot_meta(&self.app, &current, None))));
        }

        if session
            .has_active_work()
            .map_err(|error| internal_failure(error))?
        {
            return Err(HostFailure::InvalidParams);
        }
        let mut plan = snapshot.state.plan.clone();
        plan.enabled = requested_plan_enabled;
        session
            .set_plan(&operation_id, plan)
            .map_err(|error| internal_failure(error))?;
        let updated = session
            .snapshot()
            .map_err(|error| internal_failure(error))?;
        Ok(schema::SetSessionModeResponse::new()
            .meta(Some(snapshot_meta(&self.app, &updated, None))))
    }

    /// 列出当前授权范围内的健康持久 Session，并支持固定大小的偏移游标。
    async fn handle_list_sessions(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, HostFailure> {
        let cwd_filter = request
            .cwd
            .as_deref()
            .map(|path| self.authorized_cwd(path))
            .transpose()?;
        let start = parse_cursor(request.cursor.as_deref())?;
        let mut sessions = Vec::new();
        let started = std::time::Instant::now();
        let runtime = Arc::clone(&self.runtime);
        let project_filter = cwd_filter
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let metadata = tokio::task::spawn_blocking(move || {
            runtime.stored_sessions_for_project(project_filter.as_deref())
        })
        .await
        .map_err(internal_failure)?
        .map_err(internal_failure)?;
        tracing::info!(target: "keencode_diagnostics", phase = "session_list_index", elapsed_ms = started.elapsed().as_millis(), sessions = metadata.len(), "session phase completed");
        for metadata in metadata {
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
                .map_err(|error| internal_failure(error))?;
            let mut info = schema::SessionInfo::new(
                schema::SessionId::new(metadata.session_id.as_str().to_owned()),
                root,
            )
            .title(Some(metadata.title))
            .updated_at(Some(updated_at));
            // 从未发送消息的 Session 不伪造用户消息时间，由客户端回退到更新时间。
            if metadata.last_user_message_at_unix_ms > 0 {
                let last_user_message_at =
                    crate::session_commands::rfc3339_from_ms(metadata.last_user_message_at_unix_ms)
                        .map_err(|error| internal_failure(error))?;
                let mut meta = Map::new();
                meta.insert(
                    META_LAST_USER_MESSAGE_AT.to_owned(),
                    Value::String(last_user_message_at),
                );
                info = info.meta(Some(meta));
            }
            sessions.push(info);
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
        let mcp_servers = request.mcp_servers.clone();
        let source_id = request.session_id.0.as_ref().to_owned();
        let _control = self.lock_session_control(&source_id).await?;
        let requested_root = self.authorized_cwd(&request.cwd)?;
        let (_, source_root) = authorized_metadata(&self.runtime, &self.app, &source_id)
            .map_err(|_| HostFailure::ResourceNotFound)?;
        if requested_root != source_root {
            return Err(HostFailure::ResourceNotFound);
        }
        if !mcp_servers.is_empty() {
            self.ensure_extensions(&source_root).await?;
        }
        let operation_id = operation_id(request.meta.as_ref())?;
        let title = meta_text(request.meta.as_ref(), META_TITLE, 512)?;
        let source = self
            .runtime
            .open_or_create_session(&source_root, Some(&source_id), "acp-fork")
            .map_err(map_runtime_failure)?;
        if source
            .has_active_work()
            .map_err(|error| internal_failure(error))?
        {
            return Err(HostFailure::InvalidParams);
        }
        drop(source);
        let context = close_session_for_mutation(&self.runtime, &self.app, &source_id)
            .await
            .map_err(|error| internal_failure(error))?;
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
        let fork_result = fork_result.map_err(|error| internal_failure(error))?;
        let target_id = fork_result.session_id.as_str().to_owned();
        let target = self
            .runtime
            .open_or_create_session(&context.project_root, Some(&target_id), "acp-fork-open")
            .map_err(map_runtime_failure)?;
        initialize_unpublished_session_mcp(
            &self.runtime,
            &target_id,
            &context.project_root,
            mcp_servers,
        )
        .await?;
        self.runtime
            .ensure_session_delivery(&target_id)
            .map_err(map_runtime_failure)?;
        let snapshot = target.snapshot().map_err(|error| internal_failure(error))?;
        let config_options = self.config_options(&snapshot)?;
        Ok(
            schema::ForkSessionResponse::new(schema::SessionId::new(target_id))
                .modes(session_mode_state(snapshot.state.plan.enabled))
                .config_options(config_options)
                .meta(Some(snapshot_meta(&self.app, &snapshot, None))),
        )
    }

    /// 按标准 `session/cancel` 语义向精确根 Turn 树发出级联取消。
    async fn handle_cancel(
        &self,
        _connection_id: &keencode_acp::ConnectionId,
        notification: schema::CancelNotification,
    ) {
        let session_id = notification.session_id.0.as_ref().to_owned();
        let explicit_turn = match meta_string(notification.meta.as_ref(), META_TURN_ID) {
            Ok(turn) => turn,
            Err(_) => return,
        };
        let explicit_operation = match meta_string(notification.meta.as_ref(), META_OPERATION_ID) {
            Ok(Some(value)) => match OperationId::new(value) {
                Ok(operation) => Some(operation),
                Err(_) => return,
            },
            Ok(None) => None,
            Err(_) => return,
        };
        if let Some(operation_id) = explicit_operation
            && let Ok(status) = self.prompt_queue.status(&operation_id)
        {
            if status.session_id != session_id {
                return;
            }
            if status.state == OperationState::Admitted {
                if let Ok(terminal) = OperationTerminal::new("cancelled", None::<String>) {
                    match self.prompt_queue.cancel_pending(&operation_id, terminal) {
                        Ok(_) => self.queue_wakeup.notify_waiters(),
                        Err(error) => tracing::error!(
                            target: "keencode_diagnostics",
                            operation_id = %operation_id,
                            %error,
                            "取消排队 Prompt 失败"
                        ),
                    }
                }
                return;
            }
            if status.execution.is_none() {
                return;
            }
            if let Some(execution) = status.execution {
                if let Err(error) = self.runtime.cancel_turn(&session_id, &execution.turn_id) {
                    tracing::error!(session_id, turn_id = %execution.turn_id, %error, "取消回合失败");
                }
                return;
            }
        }
        let turn_id = match explicit_turn {
            Some(turn) => Some(turn),
            None => self
                .runtime
                .session_snapshot(&session_id)
                .ok()
                .and_then(|snapshot| active_root_turn(&snapshot)),
        };
        if let Some(turn_id) = turn_id
            && let Err(error) = self.runtime.cancel_turn(&session_id, &turn_id)
        {
            tracing::error!(session_id, turn_id, %error, "取消回合失败");
        }
    }

    /// 把 Runtime 当前待决 Elicitation 映射到 Host operation 状态。
    fn sync_prompt_needs_input(
        &self,
        operation_id: &OperationId,
        session_id: &str,
    ) -> Result<(), HostFailure> {
        let Some(request_id) = self
            .runtime
            .elicitation_coordinator()
            .pending_request_id_for_session(session_id)
        else {
            return Ok(());
        };
        let status = self
            .prompt_queue
            .status(operation_id)
            .map_err(map_prompt_queue_failure)?;
        if matches!(
            status.state,
            OperationState::Claimed | OperationState::Running
        ) {
            match self.prompt_queue.mark_needs_input(operation_id, request_id) {
                Ok(_) => self.queue_wakeup.notify_waiters(),
                Err(error) => tracing::debug!(
                    target: "keencode_diagnostics",
                    operation_id = %operation_id,
                    %error,
                    "Prompt NeedsInput 状态已由其他路径收口"
                ),
            }
        }
        Ok(())
    }

    /// 等待指定根 Turn 的权威终态；慢订阅者 Lag 后回到 Snapshot 检查。
    async fn wait_for_turn_terminal(
        &self,
        session: &RuntimeSession,
        turn_id: &str,
        operation_id: &OperationId,
        subscription: &mut RuntimeEventSubscription,
    ) -> Result<TerminalTurn, HostFailure> {
        self.sync_prompt_needs_input(operation_id, session.session_id().as_str())?;
        if let Some(terminal) = self.snapshot_terminal(session, turn_id)? {
            return Ok(terminal);
        }
        loop {
            self.sync_prompt_needs_input(operation_id, session.session_id().as_str())?;
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
        let snapshot = session
            .snapshot()
            .map_err(|error| internal_failure(error))?;
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
        let started = std::time::Instant::now();
        let result = crate::extensions::ensure_runtime_extension_candidate(
            &self.app,
            project_root,
            &self.runtime,
            false,
        )
        .await
        .map(|_| ())
        .map_err(|error| internal_failure(error));
        tracing::info!(target: "keencode_diagnostics", phase = "session_extensions", elapsed_ms = started.elapsed().as_millis(), success = result.is_ok(), "session phase completed");
        result
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
                crate::app_settings::get(&self.app).map_err(|error| internal_failure(error))?;
            if let Some(memory) = memories
                .prompt_context(settings.local_memories)
                .map_err(|error| internal_failure(error))?
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
            .map_err(|error| internal_failure(error))?;
        serde_json::from_slice(&raw).map_err(|error| internal_failure(error))
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

impl AcpHostBridge for AcpHost {
    fn dispatch<'a>(
        &'a self,
        connection_id: &'a keencode_acp::ConnectionId,
        message: Value,
    ) -> AcpHostBridgeFuture<'a> {
        let host = self.self_ref.upgrade();
        Box::pin(async move {
            let host = host.ok_or_else(|| "ACP Host 已经释放".to_owned())?;
            host.dispatch(connection_id, message).await
        })
    }

    fn subscribe(
        &self,
        connection_id: &keencode_acp::ConnectionId,
    ) -> Option<broadcast::Receiver<Value>> {
        Some(self.subscribe_events(connection_id))
    }

    fn disconnect(&self, connection_id: &keencode_acp::ConnectionId) {
        self.disconnect(connection_id);
    }

    fn publish_delivery(
        &self,
        payload: Value,
        target_connection_id: Option<&keencode_acp::ConnectionId>,
    ) {
        self.publish_delivery(payload, target_connection_id);
    }
}

impl keencode_cli::HostDispatch for AcpHost {
    /// 把本地 NDJSON transport 的连接身份交给同一个 Desktop ACP Host。
    fn dispatch(
        &self,
        connection_id: keencode_acp::ConnectionId,
        message: Value,
    ) -> keencode_cli::HostDispatchFuture<'_> {
        Box::pin(async move {
            AcpHostBridge::dispatch(self, &connection_id, message)
                .await
                .map_err(|_| keencode_cli::HostDispatchError::Internal)
        })
    }

    /// IPC 连接关闭只释放连接级状态；后台 operation 和 Desktop owner 生命周期保持不变。
    fn disconnected(&self, connection_id: &keencode_acp::ConnectionId) {
        AcpHostBridge::disconnect(self, connection_id);
    }

    /// 给 CLI 复用同一 Host delivery 总线；慢客户端由 broadcast lag 后走重连恢复。
    fn subscribe(
        &self,
        connection_id: &keencode_acp::ConnectionId,
    ) -> Option<broadcast::Receiver<Value>> {
        AcpHostBridge::subscribe(self, connection_id)
    }
}

/// 一个根 Turn 的终态快照。
struct TerminalTurn {
    /// Runtime 归约后的粗粒度状态。
    status: TurnStatus,
    /// 非正常终态的精确资源层原因。
    stop_reason: Option<TurnStopReason>,
}

/// 后台 Prompt driver 所需的稳定输入；创建后不再借用请求连接。
struct PromptDriveRequest {
    /// admission 时的连接身份，同时约束本轮 Elicitation 的投递与响应来源。
    connection_id: keencode_acp::ConnectionId,
    /// 权威 Session 标识。
    session_id: String,
    /// 跨连接幂等操作标识。
    operation_id: OperationId,
    /// 本轮根 Turn 标识。
    turn_id: String,
    /// 完整 Prompt 正文；不会写入诊断日志。
    text: String,
    /// admission 时已经授权的项目目录。
    project_root: PathBuf,
    /// 请求内容摘要，用于 operationId 冲突判断。
    payload_digest: String,
    /// 本轮 Ultra 模式选择。
    ultra_mode: bool,
    /// 客户端断开后是否继续托管。
    detached: bool,
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
fn initialize_response(
    default_cwd: String,
    host_runtime: &HostRuntime,
) -> Result<keencode_acp::InitializeResponseDto, HostFailure> {
    let session_capabilities = keencode_acp::InitializeSessionCapabilitiesDto::new()
        .list(Some(schema::SessionListCapabilities::new()))
        .fork(Some(schema::SessionForkCapabilities::new()));
    let capabilities = keencode_acp::InitializeAgentCapabilitiesDto::new()
        .load_session(true)
        .prompt_capabilities(schema::PromptCapabilities::default())
        .mcp_capabilities(schema::McpCapabilities::default().http(true))
        .session_capabilities(session_capabilities);
    let mut meta = Map::new();
    meta.insert(META_DEFAULT_CWD.to_owned(), Value::String(default_cwd));
    meta.extend(
        host_runtime
            .initialize_meta()
            .map_err(|error| internal_failure(error))?,
    );
    Ok(
        keencode_acp::InitializeResponseDto::new(SUPPORTED_PROTOCOL_VERSION)
            .agent_capabilities(capabilities)
            .agent_info(Some(schema::Implementation::new("KeenCode", "0.0.1")))
            .meta(Some(meta)),
    )
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

/// 在公开错误分类前保留具体失败原因和源位置，正文仍不进入 RPC 响应。
#[track_caller]
fn internal_failure(error: impl std::fmt::Display) -> HostFailure {
    tracing::error!(error = %format_args!("{error:#}"), source = %std::panic::Location::caller(), "ACP internal operation failed");
    HostFailure::Internal
}

/// Runtime 错误的公开 ACP 映射；不使用内部错误正文。
fn map_runtime_failure(error: AgentRuntimeError) -> HostFailure {
    tracing::error!(%error, "Agent runtime operation failed");
    match error {
        AgentRuntimeError::InvalidSession => HostFailure::InvalidParams,
        AgentRuntimeError::InvalidResumeTarget => HostFailure::InvalidParams,
        AgentRuntimeError::SessionUnavailable | AgentRuntimeError::SessionProjectMismatch => {
            HostFailure::ResourceNotFound
        }
        AgentRuntimeError::ProviderNotConfigured => HostFailure::ProviderNotConfigured,
        AgentRuntimeError::ProviderReloadFailed => HostFailure::ProviderReloadFailed,
        _ => HostFailure::Internal,
    }
}

/// 将 Host admission 错误压缩为稳定 ACP 分类；operation 冲突不允许静默重启。
fn map_prompt_queue_failure(error: keencode_runtime::HostCoreError) -> HostFailure {
    tracing::error!(target: "keencode_diagnostics", %error, "Host Prompt admission operation failed");
    match error {
        keencode_runtime::HostCoreError::InvalidRequest(_)
        | keencode_runtime::HostCoreError::OperationConflict
        | keencode_runtime::HostCoreError::QueueFull => HostFailure::InvalidParams,
        keencode_runtime::HostCoreError::OperationNotFound => HostFailure::ResourceNotFound,
        _ => HostFailure::Internal,
    }
}

/// 将 Session MCP 控制错误映射为不泄露连接配置的公开 ACP 分类。
fn map_session_mcp_failure(error: crate::agent_runtime::SessionMcpError) -> HostFailure {
    use crate::agent_runtime::SessionMcpError;
    tracing::error!(%error, "Session MCP operation failed");
    match error {
        SessionMcpError::InvalidConfiguration
        | SessionMcpError::OperationConflict
        | SessionMcpError::CatalogConflict => HostFailure::InvalidParams,
        SessionMcpError::SessionUnavailable | SessionMcpError::ProjectMismatch => {
            HostFailure::ResourceNotFound
        }
        SessionMcpError::StateUnavailable | SessionMcpError::Closed => HostFailure::Internal,
    }
}

/// 为尚未向客户端公开的 new/fork 目标初始化完整 MCP 配置。
///
/// 初始化失败时只关闭进程内 Session 和连接，保留确定性持久目标供相同
/// operationId 重试；不能让客户端尚未知晓的 Session 继续占用运行时资源。
async fn initialize_unpublished_session_mcp(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    project_root: &Path,
    mcp_servers: Vec<schema::McpServer>,
) -> Result<(), HostFailure> {
    let failure = match runtime
        .replace_session_mcp_servers(session_id, project_root, mcp_servers)
        .await
    {
        Ok(()) => return Ok(()),
        Err(error) => map_session_mcp_failure(error),
    };
    runtime
        .close_session(session_id)
        .await
        .map_err(map_runtime_failure)?;
    Err(failure)
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

/// 为 Prompt admission 计算有界稳定指纹；只写入摘要，不把正文放进诊断日志。
fn prompt_payload_digest(session_id: &str, turn_id: &str, text: &str, ultra_mode: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"keencode/session/prompt/v1");
    for value in [session_id, turn_id, text] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update([u8::from(ultra_mode)]);
    format!("{:x}", hasher.finalize())
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

#[cfg(test)]
mod session_control_tests {
    use super::*;

    #[tokio::test]
    async fn controls_serialize_only_the_same_session() {
        let controls = Mutex::new(BTreeMap::new());
        let first = session_control_lock(&controls, "first").unwrap();
        let same = session_control_lock(&controls, "first").unwrap();
        let guard = first.lock_owned().await;
        assert!(same.try_lock().is_err());
        let other = session_control_lock(&controls, "other").unwrap();
        assert!(other.try_lock().is_ok());
        drop(guard);
        assert!(same.try_lock().is_ok());
        drop(same);
        drop(other);
        let _next = session_control_lock(&controls, "next").unwrap();
        assert_eq!(controls.lock().unwrap().len(), 1);
    }
}

/// task-notification 文本契约：ZCode 风格 XML 块 + 明确的行动指引。
#[cfg(test)]
mod task_notification_tests {
    use super::*;

    #[test]
    fn formats_shell_completion_with_dedupable_fields() {
        let notice = TaskTerminalNotice {
            session_id: "session-1".to_owned(),
            task_id: "task-7".to_owned(),
            kind: TaskNoticeKind::Shell,
            status_text: "succeeded",
            duration_ms: 4_200,
            summary: Some("npm test: 12 passed".to_owned()),
            agent_id: None,
        };
        let text = format_task_notification_text(&notice);
        assert!(text.contains("<task-notification>"));
        assert!(text.contains("<task-id>task-7</task-id>"));
        assert!(text.contains("<kind>shell</kind>"));
        assert!(text.contains("<status>succeeded</status>"));
        assert!(text.contains("<duration-ms>4200</duration-ms>"));
        assert!(text.contains("<summary>npm test: 12 passed</summary>"));
        assert!(text.contains("</task-notification>"));
        assert!(text.contains("TaskOutput(task_id)"));
    }

    #[test]
    fn formats_agent_completion_with_mailbox_guidance() {
        let notice = TaskTerminalNotice {
            session_id: "session-1".to_owned(),
            task_id: "turn-9".to_owned(),
            kind: TaskNoticeKind::Agent,
            status_text: "failed",
            duration_ms: 900,
            summary: None,
            agent_id: Some("agent-3".to_owned()),
        };
        let text = format_task_notification_text(&notice);
        assert!(text.contains("<kind>agent</kind>"));
        assert!(text.contains("<agent-id>agent-3</agent-id>"));
        assert!(text.contains("<status>failed</status>"));
        assert!(!text.contains("<summary>"), "空摘要不得输出空标签：{text}");
        assert!(text.contains("mailbox"));
    }
}
