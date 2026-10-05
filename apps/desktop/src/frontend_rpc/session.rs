//! ZCode 会话、任务和 Agent channel 的本地适配层。
//!
//! 这个模块是 transport 与 `AgentRuntime` 之间的唯一业务入口。所有读写都先经过
//! Runtime 的项目/Session 授权，订阅只转发 Runtime publisher 的事实；网关本身不保存
//! Transcript、模型选择或 workflow 的第二份事实。V4 首帧在订阅 ACK 返回后通过动态
//! listener 发出，保证前端的 activation barrier 不会把提前到达的帧当成旧代数据。

mod command_handlers;
pub(crate) mod projection;
mod protocol;
pub(crate) mod task_extras;

/// 读取文件撤销事务的冷恢复归属，供附件详情与会话投影共用同一持久事实。
/// 该入口只转发已完成、带 turnId 的记录，不在 frontend_rpc 维护第二份状态。
pub(crate) fn completed_rewind_turn_ids(
    storage_root: &Path,
    session_id: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    command_handlers::completed_rewind_turn_ids(storage_root, session_id)
}

use super::attachments;
use super::command_receipts;
use super::controller;
use super::dispatch::{EventCallback, GatewayContext, Handler, RpcError, RpcFuture, Subscription};
use super::workspace_hook_review::{self, RequestContext, RespondContext, RevokeContext};
use crate::agent_runtime::{
    AgentRuntime, AppRuntimePreferences, RootTurnOptions, RuntimeAgentTemplateContext,
};
use crate::elicitation::ElicitationChange;
use crate::path_utils::path_to_frontend;
use crate::permissions::PermissionChange;
use crate::session_commands::{
    authorize_stored_root, close_session_for_mutation, open_authorized_session,
    restore_session_after_mutation,
};
use crate::workflows::types::progress_from_workflow_event;
use crate::workflows::{RuntimeWorkflowJournal, WorkflowJournalPort};
use keencode_agent::{AgentStreamEvent, AgentStreamEventKind};
use keencode_model::ModelStreamEvent;
use keencode_resources::CommandReceiptStatus;
use keencode_resources::{
    AssistantFeedback, FollowupMode, ROOT_AGENT_ID, SessionEvent, SessionInputDelivery,
    SessionInputDispatch, SessionInputKind, SessionInputQueueItem, SessionStatus, TitleSource,
    TurnStatus,
};
use keencode_runtime::{
    CommandReceiptAdmission, RuntimeEventPayload, RuntimeEventReceiveError,
    RuntimeEventSubscription, RuntimeModelRetryScheduled,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::AppHandle;
use tokio::task::AbortHandle;

const AGENT_CHANNEL: &str = "zcode-agent";
const SESSION_CHANNEL: &str = "zcode-session";
const TASK_CHANNEL: &str = "zcode-task";
const WINDOW_CHANNEL: &str = "window-controller";
const MAX_PENDING_ACTIVATION_FRAMES: usize = 256;
const MAX_RELEASED_SUBSCRIPTION_TOMBSTONES: usize = 2048;
const TOPIC_WIRE_VERSION: u64 = 3;
/// 临时流行不属于 Journal，使用独立的高位区间避免与后续持久消息 rowId 冲突。
/// 该区间只在当前进程内使用；重启后临时行不会从快照恢复，也就不会与事实行混用。
static NEXT_TRANSIENT_ROW_ID: AtomicU64 = AtomicU64::new(1_u64 << 52);
/// 仅记录已经由后端释放的订阅及其连接所有者，供同一连接的迟到 unsubscribe
/// 幂等收口。它不保存投影、Session 或 Journal 事实，并且有界，避免关闭竞态
/// 把生命周期记录变成第二份持久状态。
static RELEASED_SUBSCRIPTION_TOMBSTONES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn released_subscription_tombstones() -> &'static Mutex<HashMap<String, String>> {
    RELEASED_SUBSCRIPTION_TOMBSTONES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_released_subscription(subscription_id: &str, connection_id: &str) {
    if let Ok(mut tombstones) = released_subscription_tombstones().lock() {
        if tombstones.len() >= MAX_RELEASED_SUBSCRIPTION_TOMBSTONES
            && let Some(evicted) = tombstones.keys().next().cloned()
        {
            tombstones.remove(&evicted);
        }
        tombstones.insert(subscription_id.to_owned(), connection_id.to_owned());
    }
}

fn released_subscription_belongs_to(subscription_id: &str, connection_id: &str) -> bool {
    released_subscription_tombstones()
        .lock()
        .ok()
        .and_then(|tombstones| tombstones.get(subscription_id).cloned())
        .is_some_and(|owner| owner == connection_id)
}

/// 任务组织写入已提交后，统一刷新 Controller 与 legacy task 订阅。
///
/// 目标来自 `task_extras::AuthorizedTask`，因此 workspace/task 范围已经通过同一
/// 授权链校验；这里不重新解析前端参数，也不创建第二份未读状态。调用方必须在
/// task-groups 锁释放后调用，避免 Controller snapshot 回读事实源时发生锁重入。
pub(crate) fn notify_task_unread_mutation(
    ctx: &GatewayContext,
    runtime: &Arc<AgentRuntime>,
    workspace_path: &str,
    task_id: &str,
) {
    controller::emit_task_snapshots_for_app(&ctx.app);
    gateway().inner.emit_workspace_task_event(
        &ctx.app,
        runtime,
        workspace_path,
        task_id,
        "task_meta_changed",
    );
}

/// Source 只把选择的 User Agent 序列化成 `@name` 文本；这里按当前冻结的
/// Agent catalog 解析它，再以非 Transcript 的 meta 上下文提示根模型调用已有
/// `spawn_agent`。自定义 system prompt 仍只由 Runtime 在子 Agent 启动时装配，
/// 因而不会形成第二份前端身份或把配置正文注入根 Agent。
fn subagent_mention_candidates(text: &str) -> Vec<String> {
    fn is_name_char(character: char) -> bool {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
    }

    let mut candidates = Vec::new();
    for (index, character) in text.char_indices() {
        if character != '@' {
            continue;
        }
        let previous = text[..index].chars().next_back();
        if previous.is_some_and(is_name_char) {
            continue;
        }
        let remainder = &text[index + character.len_utf8()..];
        let end = remainder
            .char_indices()
            .find_map(|(offset, character)| (!is_name_char(character)).then_some(offset))
            .unwrap_or(remainder.len());
        if end != 0 {
            candidates.push(remainder[..end].to_owned());
        }
    }
    candidates
}

fn subagent_route_context(name: &str) -> String {
    format!(
        "<subagent_route>\nSource selected the enabled subagent @{name}. Treat this as a routing hint for this root turn. Use the existing `spawn_agent` tool with its `agent` parameter set to {name:?}, and pass the user's request as the child task. Keep the root agent identity and user text unchanged; do not expose the selected profile system prompt.\n</subagent_route>",
    )
}

fn subagent_route_from_resolver(
    text: &str,
    mut resolve: impl FnMut(&str) -> Result<Option<String>, RpcError>,
) -> Result<Option<String>, RpcError> {
    for candidate in subagent_mention_candidates(text) {
        if let Some(name) = resolve(&candidate)? {
            return Ok(Some(subagent_route_context(&name)));
        }
    }
    Ok(None)
}

fn selected_subagent_route(
    runtime: &AgentRuntime,
    project_root: &Path,
    session_id: &str,
    turn_id: &str,
    text: &str,
) -> Result<Option<String>, RpcError> {
    let parent = RuntimeAgentTemplateContext {
        session_id: session_id.to_owned(),
        parent_agent_id: ROOT_AGENT_ID.to_owned(),
        root_turn_id: turn_id.to_owned(),
    };
    subagent_route_from_resolver(text, |candidate| {
        runtime
            .resolve_extension_agent(project_root, candidate, &parent)
            .map_err(backend_error)
            .map(|template| template.map(|template| template.name))
    })
}

/// 根装配层使用的会话 RPC 调用入口。
pub async fn call_session(
    ctx: GatewayContext,
    channel: &str,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    gateway().call(ctx, channel, method, args).await
}

/// 根装配层使用的会话事件订阅入口。
pub fn listen_session(
    ctx: &GatewayContext,
    channel: &str,
    event: &str,
    args: Value,
    callback: EventCallback,
) -> Result<Subscription, RpcError> {
    gateway().listen(ctx, channel, event, args, callback)
}

/// Transport 关闭连接时清理网关中的易失绑定、监听器、订阅和命令收据。
///
/// 会话事实仍由 Runtime/Journal 持有；这里清理的是连接生命周期状态，避免旧连接
/// 在重连后继续收到帧，或把同一个 commandId 的响应泄漏给另一个连接。
pub fn dispose_connection(connection_id: &str) {
    gateway().dispose_connection(connection_id);
}

/// Transport 已成功写入 RPC response 后调用，用于打开对应订阅的 activation barrier。
///
/// `response` 是刚写入 Channel 的成功 body，订阅 ACK 中的 `subscriptionId` 是
/// 请求与连接级订阅的权威关联；`args` 只用于校验 topic 和无 ID 的兼容回退。
pub fn response_sent(
    ctx: &GatewayContext,
    channel: &str,
    method: &str,
    args: Value,
    response: Value,
) {
    if channel == AGENT_CHANNEL && is_subscription_method(method) {
        gateway().response_sent(ctx, method, &args, &response);
    }
}

/// 根装配层注入当前父会话的 WorkflowHost 调用面。
///
/// Session gateway 只负责从 Runtime/Journal 读取授权和冻结输入，Host 的具体实例
/// 仍由根装配按父 Session 创建。这样 workflow command 不会绕过 Session 所属关系，
/// 也不会在 frontend_rpc 内复制一份运行事实。
pub type WorkflowCall =
    Arc<dyn Fn(GatewayContext, String, Value) -> RpcFuture + Send + Sync + 'static>;

/// 安装生产 WorkflowHost 调用面；桌面进程只允许一次装配，Remote Client 不应安装。
pub fn install_workflow_call(handler: WorkflowCall) -> Result<(), RpcError> {
    gateway().install_workflow_call(handler)
}

/// 可直接交给 transport 聚合器的 handler，方便根模块按 channel 组合 local
/// service 与会话 service，而不引入第二个 Runtime。
#[derive(Default)]
pub struct SessionHandler;

impl Handler for SessionHandler {
    fn call(&self, ctx: GatewayContext, channel: String, method: String, args: Value) -> RpcFuture {
        Box::pin(async move { call_session(ctx, &channel, &method, args).await })
    }

    fn listen(
        &self,
        ctx: GatewayContext,
        channel: String,
        event: String,
        args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError> {
        listen_session(&ctx, &channel, &event, args, callback)
    }

    fn connection_closed(&self, ctx: GatewayContext) {
        attachments::connection_closed(&ctx.connection_id);
        if let Ok(connection_id) = keencode_acp::ConnectionId::new(ctx.connection_id.clone())
            && let Ok(runtime) = owned_runtime(&ctx.app)
        {
            // 连接关闭时让真实 ElicitationCoordinator 收口待决请求，禁止旧页面在
            // 重连后继续抢答；Session/Journal 事实仍保留，便于用户重新发起交互。
            runtime.elicitation_coordinator().disconnect(&connection_id);
            // 权限审批与 AskUser 分账本，但必须共享同一连接生命周期；关闭后
            // 正在执行的工具收到拒绝并在真实副作用边界前收口。
            runtime.disconnect_permission_connection(&connection_id);
        }
        if let Ok(coordinator) =
            workspace_hook_review::WorkspaceHookReviewCoordinator::for_app(&ctx.app)
        {
            // Hook review Journal 保留冷恢复 flow，但连接 claim 必须释放，避免旧窗口抢答。
            let _ = coordinator.connection_closed(&ctx.connection_id);
        }
        dispose_connection(&ctx.connection_id);
    }

    fn response_sent(
        &self,
        ctx: GatewayContext,
        channel: String,
        method: String,
        args: Value,
        response: Value,
    ) {
        response_sent(&ctx, &channel, &method, args, response);
    }
}

fn gateway() -> Arc<SessionGateway> {
    static INSTANCE: OnceLock<Arc<SessionGateway>> = OnceLock::new();
    Arc::clone(INSTANCE.get_or_init(|| Arc::new(SessionGateway::new())))
}

struct SessionGateway {
    inner: Arc<SessionGatewayInner>,
}

struct SessionGatewayInner {
    connections: Mutex<HashMap<String, ConnectionBinding>>,
    listeners: Mutex<HashMap<u64, ListenerBinding>>,
    subscriptions: Mutex<HashMap<String, ActiveSubscription>>,
    /// 同一 Runtime 的 snapshot 读取、stamp、排放必须保持 admission 顺序；否则
    /// 先读取的旧 Journal 可能在新快照之后覆盖同一个投影水位。
    snapshot_reads: Mutex<()>,
    command_results: Mutex<HashMap<String, Value>>,
    command_order: Mutex<VecDeque<String>>,
    /// 仅用于当前进程内把首次 promotion 的广播收口一次；任务成员本身仍由
    /// Journal/task-groups 持久事实决定，重启后列表首帧无需依赖这份易失集合。
    task_created_announcements: Mutex<HashSet<(String, String)>>,
    workflow_call: Mutex<Option<WorkflowCall>>,
    next_listener: AtomicU64,
    next_subscription: AtomicU64,
}

#[derive(Clone, Default)]
struct ConnectionBinding {
    initialized: bool,
    client_id: Option<String>,
    client_mode: String,
    workflow_run_deltas: bool,
    generation: u64,
}

#[derive(Clone)]
struct ListenerBinding {
    connection_id: String,
    channel: String,
    event: String,
    workspace_path: Option<String>,
    callback: EventCallback,
}

#[derive(Clone)]
struct ActiveSubscription {
    connection_id: String,
    topic: String,
    kind: String,
    identity: String,
    workspace_path: String,
    session_id: Option<String>,
    /// 普通 core subagent 的真实 Agent 身份；Runtime 事件仍订阅父 Session。
    agent_id: Option<keencode_resources::AgentId>,
    log_epoch: String,
    /// 传输投影水位。它同时绑定 logical frame 的 from/toSeq 和 snapshot.seq；
    /// Runtime delivery_sequence 仅用于临时行的稳定排序，不能直接冒充此水位。
    seq: u64,
    /// 最近一次纳入该订阅的 Journal 水位；用于说明 projection seq 的事实底座。
    journal_seq: u64,
    subscription_id: String,
    /// 同一订阅内把“读取快照/投影、分配水位、编号和发送”放进一个临界区；
    /// 否则旧快照可能在 transient 之后排放，却带着更高的连接水位覆盖新事实。
    frame_gate: Arc<Mutex<()>>,
    /// 物理 wire 的单调 ordinal，与 logical frame 的业务水位相互独立。
    wire_ordinal: u64,
    /// 首帧和 ACK 前收到的最新快照暂存于连接侧；ACK 写入失败时不会排放。
    initial_frame: Option<Value>,
    /// ACK 前的 delta 必须按顺序保留，不能只保留最后一帧，否则会丢失首个
    /// assistantText/toolCall 的 row.appended，后续 row.delta 将无目标行。
    pending_frames: Vec<Value>,
    transient: TransientProjection,
    /// 当前订阅显示的热重试所属 Turn；只用于清除临时 UI 投影，不保存业务事实。
    api_retry_turn_id: Option<String>,
    activated: bool,
    abort: Option<AbortHandle>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameDeliveryKind {
    Initial,
    Online,
    Recovery,
}

impl FrameDeliveryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Online => "online",
            Self::Recovery => "recovery",
        }
    }
}

/// 将一个已经通过 Runtime/Journal 投影的 logical frame 包装成 V4 complete wire。
/// native Channel 传输 JSON bytes，本层不能把 logical frame 直接当作事件载荷，
/// 否则前端 assembler 无法建立 deliveryKind/ordinal 代际边界。
fn topic_wire_complete(
    topic: &str,
    subscription_id: &str,
    frame: Value,
    ordinal: u64,
    delivery_kind: FrameDeliveryKind,
) -> Value {
    json!({
        "wireVersion": TOPIC_WIRE_VERSION,
        "kind": "complete",
        "deliveryKind": delivery_kind.as_str(),
        "logicalFrameId": format!("{subscription_id}:{ordinal}"),
        "logicalFrameOrdinal": ordinal.max(1),
        "topic": topic,
        "subscriptionId": subscription_id,
        "frame": frame,
    })
}

/// 队列入场所需的请求字段；把同一 admission 的载荷绑定在一起，避免调用方
/// 分别传递可能来自不同 command 的文本、模式和投递策略。
struct InputQueueRequest<'a> {
    payload: &'a Value,
    text: &'a str,
    kind: SessionInputKind,
    admitted_delivery: SessionInputDelivery,
}

/// 将 V4 命令的只读 envelope 视为一个整体，避免执行入口在参数扩展后继续
/// 增长；所有字段均来自同一次 admission，不能由下游重新拼接。
struct CommandExecutionRequest<'a> {
    command_id: &'a str,
    session_id: Option<&'a str>,
    workspace_path: &'a str,
    command_type: &'a str,
    payload: &'a Value,
    base_revision: Option<u64>,
    base_log_epoch: Option<&'a str>,
}

/// 工作流写入与运行控制只能从 V4 command admission 进入；这些 canonical 类型
/// 由 [`send_command`] 统一做 workspace、Session、busy gate 和持久 receipt 校验。
fn is_canonical_workflow_command(command_type: &str) -> bool {
    matches!(
        command_type,
        "cancelBackgroundWork"
            | "resumeWorkflowRun"
            | "startSavedWorkflow"
            | "amendWorkflowRunSettings"
    )
}

#[derive(Clone, Default)]
struct TransientProjection {
    response_id: Option<String>,
    assistant_row_id: Option<u64>,
    assistant_text: String,
    reasoning_rows: HashMap<u32, u64>,
    reasoning_text: HashMap<u32, String>,
    tool_rows: HashMap<String, u64>,
    tool_names: HashMap<String, String>,
    tool_inputs: HashMap<String, String>,
}

fn session_is_promoted(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    session_id: &str,
) -> Result<bool, RpcError> {
    let session = open_authorized_session(runtime, app, session_id).map_err(backend_error)?;
    let snapshot = session.snapshot().map_err(backend_error)?;
    Ok(projection::is_promoted_task_state(&snapshot.state))
}

fn task_promotion_transition(was_promoted: bool, is_promoted: bool) -> bool {
    !was_promoted && is_promoted
}

impl SessionGateway {
    fn new() -> Self {
        Self {
            inner: Arc::new(SessionGatewayInner {
                connections: Mutex::new(HashMap::new()),
                listeners: Mutex::new(HashMap::new()),
                subscriptions: Mutex::new(HashMap::new()),
                snapshot_reads: Mutex::new(()),
                command_results: Mutex::new(HashMap::new()),
                command_order: Mutex::new(VecDeque::new()),
                task_created_announcements: Mutex::new(HashSet::new()),
                workflow_call: Mutex::new(None),
                next_listener: AtomicU64::new(1),
                next_subscription: AtomicU64::new(1),
            }),
        }
    }

    fn dispose_connection(&self, connection_id: &str) {
        self.inner.dispose_connection(connection_id);
    }

    fn install_workflow_call(&self, handler: WorkflowCall) -> Result<(), RpcError> {
        let mut slot = self.inner.workflow_call.lock().map_err(|_| state_error())?;
        if slot.is_some() {
            return Err(RpcError::new(
                "fault.workflow.handlerAlreadyInstalled",
                "WorkflowHost 调用面已经完成装配",
            ));
        }
        *slot = Some(handler);
        Ok(())
    }

    async fn workflow_call(
        &self,
        ctx: GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        let handler = self
            .inner
            .workflow_call
            .lock()
            .map_err(|_| state_error())?
            .clone()
            .ok_or_else(|| {
                RpcError::new(
                    "fault.workflow.capabilityUnsupported",
                    "当前桌面 Runtime 没有装配 WorkflowHost",
                )
            })?;
        handler(ctx, method.to_owned(), args).await
    }

    fn response_sent(&self, ctx: &GatewayContext, method: &str, args: &Value, response: &Value) {
        let Some(subscription_id) =
            self.inner
                .subscription_id_for_response(&ctx.connection_id, method, args, response)
        else {
            return;
        };
        self.inner.activate_subscription(&subscription_id);
    }

    async fn call(
        &self,
        ctx: GatewayContext,
        channel: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        match channel {
            AGENT_CHANNEL => self.call_agent(&ctx, method, args).await,
            SESSION_CHANNEL => self.call_session_service(&ctx, method, args).await,
            TASK_CHANNEL => self.call_task_service(&ctx, method, args).await,
            WINDOW_CHANNEL => self.call_window_controller(&ctx, method, args).await,
            _ => Err(unknown_channel(channel)),
        }
    }

    fn listen(
        &self,
        ctx: &GatewayContext,
        channel: &str,
        event: &str,
        args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError> {
        let accepted = matches!(
            (channel, event),
            (AGENT_CHANNEL, "onDynamicConversationFrame")
                | (AGENT_CHANNEL, "onDynamicSessionsIndexFrame")
                | (AGENT_CHANNEL, "onDynamicWorkspaceConfigFrame")
                | (AGENT_CHANNEL, "onDynamicWorkflowRunProgress")
                | (AGENT_CHANNEL, "onAgentRuntimeLifecycle")
                | (AGENT_CHANNEL, "onAgentRuntimeRestarted")
                | (TASK_CHANNEL, "onDynamicWorkspaceEvent")
        );
        if !accepted {
            return Err(unknown_method(channel, event));
        }
        // subscribe 将工作区绑定到授权后的 canonical root；listener 也必须在登记时
        // 使用同一身份，否则 Windows 的短路径、反斜杠和 `\\?\` 前缀会让真实帧在
        // emit_frame 中被静默过滤。没有 workspacePath 的生命周期事件仍是全局监听。
        let workspace_path = if channel == TASK_CHANNEL {
            let requested = required_workspace_path(&args)?;
            Some(
                authorize_workspace_scope(&ctx.app, &requested)
                    .map(|root| root.to_string_lossy().into_owned())?,
            )
        } else {
            optional_string(&args, "workspacePath")?
                .map(|requested| {
                    authorize_workspace_scope(&ctx.app, &requested)
                        .map(|root| root.to_string_lossy().into_owned())
                })
                .transpose()?
        };
        let listener_id = self.inner.next_listener.fetch_add(1, Ordering::Relaxed);
        let initial_lifecycle_events = if event == "onAgentRuntimeLifecycle" {
            runtime_lifecycle_events(ctx, &args)?
        } else {
            Vec::new()
        };
        let initial_callback = Arc::clone(&callback);
        self.inner
            .listeners
            .lock()
            .map_err(|_| state_error())?
            .insert(
                listener_id,
                ListenerBinding {
                    connection_id: ctx.connection_id.clone(),
                    channel: channel.to_owned(),
                    event: event.to_owned(),
                    workspace_path,
                    callback,
                },
            );
        for event in initial_lifecycle_events {
            if let Err(error) = initial_callback(event) {
                if let Ok(mut listeners) = self.inner.listeners.lock() {
                    listeners.remove(&listener_id);
                }
                return Err(error);
            }
        }
        let inner = Arc::clone(&self.inner);
        Ok(Subscription::new(move || {
            if let Ok(mut listeners) = inner.listeners.lock() {
                listeners.remove(&listener_id);
            }
        }))
    }

    async fn call_agent(
        &self,
        ctx: &GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        match method {
            "helloConversationV4" => Ok(self.hello(ctx)),
            "initializeConversationV4" => self.initialize(ctx, &args),
            "subscribeConversationV4"
            | "subscribeSessionsIndexV4"
            | "subscribeWorkspaceConfigV4" => self.subscribe(ctx, method, &args),
            "resyncConversationV4" | "resyncSessionsIndexV4" | "resyncWorkspaceConfigV4" => {
                self.resync(ctx, &args)
            }
            "unsubscribeConversationV4"
            | "unsubscribeSessionsIndexV4"
            | "unsubscribeWorkspaceConfigV4" => self.unsubscribe(ctx, &args),
            "conversationRowsRangeV4" => self.rows_range(ctx, &args),
            "conversationPlansV4" => self.plans(ctx, &args),
            "conversationWorkflowRunEventsV4" => self.workflow_events(ctx, &args),
            "conversationWorkflowRunsV4" => self.workflow_runs(ctx, &args),
            "sendConversationCommandV4" => self.send_command(ctx, &args).await,
            "queryConversationCommandsV4" => self.query_commands(ctx, &args),
            method if attachments::is_v4_method(method) => {
                attachments::call_v4(ctx, method, args).await
            }
            "syncAppRuntimePreferences" => self.sync_app_runtime_preferences(ctx, &args),
            "setConnectionFlowStateV4" => self.require_initialized(ctx).map(|_| json!({})),
            _ => Err(unknown_method(AGENT_CHANNEL, method)),
        }
    }

    fn sync_app_runtime_preferences(
        &self,
        ctx: &GatewayContext,
        args: &Value,
    ) -> Result<Value, RpcError> {
        reject_unknown_fields(
            args,
            &["askUserQuestionAutoResolutionEnabled"],
            "syncAppRuntimePreferences",
        )?;
        let ask_user_question_auto_resolution_enabled = args
            .get("askUserQuestionAutoResolutionEnabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_params("askUserQuestionAutoResolutionEnabled 必须为布尔值"))?;
        let runtime = owned_runtime(&ctx.app)?;
        runtime
            .sync_app_runtime_preferences(AppRuntimePreferences {
                ask_user_question_auto_resolution_enabled,
            })
            .map_err(|error| backend_error(error.to_string()))?;
        Ok(Value::Null)
    }

    fn hello(&self, ctx: &GatewayContext) -> Value {
        json!({
            "kind": "hello",
            "protocolVersion": protocol::WIRE_PROTOCOL_VERSION,
            "connectionId": ctx.connection_id,
            "clientMode": "desktop-continuous",
            "deliveryProfile": "continuous",
            "serverTime": protocol::now_epoch_ms(),
            "capabilities": {
                "nativeDialogs": true,
                "localTerminal": true,
                "binaryFrames": false,
                "compression": "none",
                "workspaceHookReview": true,
                "independentPlanState": true,
                "workflowRunDeltas": true,
            },
            "auth": {},
        })
    }

    fn initialize(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        reject_unknown_fields(
            args,
            &[
                "kind",
                "protocolVersion",
                "clientId",
                "clientKind",
                "appVersion",
                "capabilities",
            ],
            "clientHello",
        )?;
        let kind = required_string(args, "kind")?;
        if kind != "clientHello" {
            return Err(invalid_params("kind 必须为 clientHello"));
        }
        let version = args
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_params("protocolVersion 必须为数字"))?;
        if version != protocol::WIRE_PROTOCOL_VERSION {
            return Err(RpcError::new(
                "fault.connection.protocolVersion",
                format!("不支持的 V4 wire protocolVersion: {version}"),
            ));
        }
        let client_id = required_string(args, "clientId")?;
        let _app_version = required_string(args, "appVersion")?;
        let client_kind = args
            .get("clientKind")
            .and_then(Value::as_str)
            .unwrap_or("desktop");
        if !matches!(
            client_kind,
            "desktop" | "web" | "mobileRemote" | "mobileApp"
        ) {
            return Err(invalid_params("clientKind 不是受支持的客户端类型"));
        }
        if client_kind != "desktop" {
            return Err(RpcError::new(
                "fault.connection.clientMode",
                "当前桌面 Runtime 只接受 desktop clientKind",
            ));
        }
        if let Some(capabilities) = args.get("capabilities") {
            reject_unknown_fields(
                capabilities,
                &["workspaceHookReviewUi", "workflowRunDeltas"],
                "clientHello.capabilities",
            )?;
        }
        let workflow_run_deltas = args
            .pointer("/capabilities/workflowRunDeltas")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut connections = self.inner.connections.lock().map_err(|_| state_error())?;
        let binding = connections
            .entry(ctx.connection_id.clone())
            .or_insert_with(ConnectionBinding::default);
        if binding.initialized
            && (binding.client_id.as_deref() != Some(client_id.as_str())
                || binding.workflow_run_deltas != workflow_run_deltas)
        {
            return Err(RpcError::new(
                "fault.connection.reinitializeMismatch",
                "同一连接不能使用不同的 clientHello 能力",
            ));
        }
        binding.initialized = true;
        binding.client_id = Some(client_id);
        binding.client_mode = "desktop-continuous".to_owned();
        binding.workflow_run_deltas = workflow_run_deltas;
        binding.generation = binding.generation.saturating_add(1);
        Ok(json!({}))
    }

    fn require_initialized(&self, ctx: &GatewayContext) -> Result<(), RpcError> {
        let connections = self.inner.connections.lock().map_err(|_| state_error())?;
        if connections
            .get(&ctx.connection_id)
            .is_some_and(|binding| binding.initialized)
        {
            Ok(())
        } else {
            Err(RpcError::new(
                "fault.connection.helloRequired",
                "必须先完成 helloConversationV4 和 initializeConversationV4",
            ))
        }
    }

    fn subscribe(
        &self,
        ctx: &GatewayContext,
        method: &str,
        args: &Value,
    ) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let topic = subscription_topic(method, args)?;
        let (kind, identity) = protocol::topic_parts(&topic).map_err(invalid_params)?;
        let expected_kind =
            subscription_kind(method).ok_or_else(|| unknown_method(AGENT_CHANNEL, method))?;
        if kind != expected_kind {
            return Err(invalid_params(format!(
                "{method} 的 topic 必须属于 {expected_kind}"
            )));
        }
        let requested_workspace_path = required_workspace_path(args)?;
        let runtime = owned_runtime(&ctx.app)?;
        let workspace_root = authorize_workspace_scope(&ctx.app, &requested_workspace_path)?;
        // 后续索引过滤、logEpoch 和 ActiveSubscription 都使用授权后的规范根，避免
        // 同一 Windows 目录的短路径/长路径别名产生空列表或新的日志世代。
        let workspace_path = workspace_root.to_string_lossy().into_owned();
        let workspace_id = workspace_identity(args, &workspace_path)?;
        let conversation_scope = if kind == "conversation" {
            Some(
                projection::resolve_conversation_scope(
                    &runtime,
                    &ctx.app,
                    identity,
                    &workspace_path,
                )
                .map_err(backend_error)?,
            )
        } else {
            None
        };
        if kind == "conversation" {
            // resolve_conversation_scope 已对真实父 Session 和 virtual agent 完成
            // 同一 workspace、Journal 注册及单层 parent 校验。
        }
        let log_epoch = log_epoch(kind, identity, &workspace_path);
        let subscription_id = format!(
            "v4-sub-{}-{}",
            ctx.connection_id,
            self.inner.next_subscription.fetch_add(1, Ordering::Relaxed)
        );
        // 先固定 Runtime publisher 的读取水位，再取快照。这样快照之后、ACK 之前提交
        // 的事件会由 forwarder 暂存，不会因为首帧建立时序而丢失。
        let events = if kind == "conversation" {
            let parent_session_id = conversation_scope
                .as_ref()
                .map(|scope| scope.parent_session_id.as_str())
                .unwrap_or(identity);
            let session = open_authorized_session(&runtime, &ctx.app, parent_session_id)
                .map_err(backend_error)?;
            Some(
                session
                    .subscribe()
                    .map_err(|error| backend_error(error.to_string()))?,
            )
        } else {
            None
        };
        if kind == "conversation"
            && let Some(scope) = conversation_scope
                .as_ref()
                .filter(|scope| scope.agent_id.is_none())
        {
            // 旧 V4 会话没有触发命名时，重新打开按同一首条用户消息补生成。
            runtime.schedule_automatic_title(&scope.parent_session_id);
        }
        // AskUser 与权限 pending 都不写入 Journal；订阅协调器的生命周期广播，收到后
        // 重新读取同一份 Runtime pending Schema，保证 V4 snapshot 能在答复/断开后清场。
        let interaction_events = if matches!(kind, "conversation" | "sessions-index") {
            Some(runtime.elicitation_coordinator().subscribe_changes())
        } else {
            None
        };
        let permission_events = if matches!(kind, "conversation" | "sessions-index") {
            Some(runtime.subscribe_permission_changes())
        } else {
            None
        };
        let initial = match kind {
            "conversation" => {
                let scope = conversation_scope
                    .as_ref()
                    .expect("conversation 订阅必须已经解析 scope");
                projection::conversation_frame_for_connection_with_agent(
                    &runtime,
                    &ctx.app,
                    scope,
                    &ctx.connection_id,
                    &topic,
                    &subscription_id,
                    &log_epoch,
                )
                .map_err(backend_error)?
            }
            "sessions-index" => {
                let connection_id = keencode_acp::ConnectionId::new(ctx.connection_id.clone())
                    .map_err(|error| backend_error(error.to_string()))?;
                projection::sessions_index_frame_for_connection(
                    &runtime,
                    &workspace_path,
                    &workspace_id,
                    &subscription_id,
                    &log_epoch,
                    Some(&connection_id),
                )?
            }
            "workspace-config" => {
                let snapshot = projection::workspace_config_snapshot(&ctx.app, &workspace_id)
                    .map_err(backend_error)?;
                json!({
                    "topic": topic,
                    "subscriptionId": subscription_id,
                    "fromSeq": 0,
                    "toSeq": 0,
                    "sentAt": protocol::now_epoch_ms(),
                    "payload": {"kind": "snapshot", "snapshot": snapshot},
                })
            }
            _ => unreachable!(),
        };
        let seq = initial.get("toSeq").and_then(Value::as_u64).unwrap_or(0);
        // 当前桌面 Runtime 没有可寻址的 logical-frame 回放缓冲。即使 epoch 相同，
        // 也只能排放完整快照；返回 resume 会让前端把首帧当作 base 之后的续传，
        // 进而在临时流/Journal 水位不同步时永久等待缺失 delta。
        let mode = "snapshot";
        let initial_retry_turn_id = if kind == "conversation" {
            let parent_session_id = conversation_scope
                .as_ref()
                .map(|scope| scope.parent_session_id.as_str())
                .unwrap_or(identity);
            let expected_agent = conversation_scope
                .as_ref()
                .and_then(|scope| scope.agent_id.as_ref())
                .map(|agent_id| agent_id.as_str())
                .unwrap_or(ROOT_AGENT_ID);
            open_authorized_session(&runtime, &ctx.app, parent_session_id)
                .ok()
                .and_then(|session| {
                    session
                        .model_retry_for_active_agent(expected_agent)
                        .ok()
                        .flatten()
                })
                .filter(|retry| retry.source_agent_id == expected_agent)
                .map(|retry| retry.turn_id)
        } else {
            None
        };
        let active = ActiveSubscription {
            connection_id: ctx.connection_id.clone(),
            topic: topic.clone(),
            kind: kind.to_owned(),
            identity: identity.to_owned(),
            workspace_path: workspace_path.clone(),
            session_id: (kind == "conversation").then(|| {
                conversation_scope
                    .as_ref()
                    .map(|scope| scope.parent_session_id.clone())
                    .unwrap_or_else(|| identity.to_owned())
            }),
            agent_id: conversation_scope
                .as_ref()
                .and_then(|scope| scope.agent_id.clone()),
            log_epoch: log_epoch.clone(),
            seq,
            journal_seq: seq,
            subscription_id: subscription_id.clone(),
            frame_gate: Arc::new(Mutex::new(())),
            wire_ordinal: 0,
            initial_frame: Some(initial),
            pending_frames: Vec::new(),
            transient: TransientProjection::default(),
            api_retry_turn_id: initial_retry_turn_id,
            activated: false,
            abort: None,
        };
        self.inner
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .insert(subscription_id.clone(), active);
        if let Some(events) = events {
            let interaction_events = interaction_events.expect("conversation 应有问答事件源");
            let permission_events = permission_events.expect("conversation 应有权限事件源");
            self.spawn_forwarder(
                ctx.app.clone(),
                runtime,
                subscription_id.clone(),
                events,
                interaction_events,
                permission_events,
            )?;
        } else if let Some(interaction_events) = interaction_events {
            let permission_events = permission_events.expect("sessions-index 应有权限事件源");
            self.spawn_interaction_forwarder(
                ctx.app.clone(),
                runtime,
                subscription_id.clone(),
                interaction_events,
                permission_events,
            )?;
        }
        Ok(json!({
            "ack": {"subscriptionId": subscription_id, "mode": mode, "logEpoch": log_epoch},
        }))
    }

    fn spawn_forwarder(
        &self,
        app: AppHandle,
        runtime: Arc<AgentRuntime>,
        subscription_id: String,
        mut events: RuntimeEventSubscription,
        mut interaction_events: tokio::sync::broadcast::Receiver<ElicitationChange>,
        mut permission_events: tokio::sync::broadcast::Receiver<PermissionChange>,
    ) -> Result<(), RpcError> {
        let inner = Arc::clone(&self.inner);
        let task_subscription_id = subscription_id.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = events.recv() => {
                        match result {
                            Ok(delivery) => {
                                inner.emit_runtime_event(&app, &runtime, &task_subscription_id, &delivery);
                                if matches!(delivery.payload, RuntimeEventPayload::Control(_)) {
                                    break;
                                }
                            }
                            Err(RuntimeEventReceiveError::Lagged(_)) => {
                                inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                            }
                            Err(RuntimeEventReceiveError::Closed) => break,
                        }
                    }
                    result = interaction_events.recv() => {
                        match result {
                            Ok(change) => {
                                let affects_subscription = inner
                                    .subscriptions
                                    .lock()
                                    .ok()
                                    .and_then(|subscriptions| subscriptions.get(&task_subscription_id).cloned())
                                    .is_some_and(|subscription| {
                                        subscription.kind == "conversation"
                                            && subscription.session_id.as_deref()
                                                == Some(change.display_session_id.as_str())
                                            && subscription.agent_id.is_none()
                                    });
                                if affects_subscription {
                                    // Snapshot 帧可重复应用，且包含完整 pendingInteractions；
                                    // 这类非 Journal 变化不伪造 Journal sequence。
                                    inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    result = permission_events.recv() => {
                        match result {
                            Ok(change) => {
                                let affects_subscription = inner
                                    .subscriptions
                                    .lock()
                                    .ok()
                                    .and_then(|subscriptions| subscriptions.get(&task_subscription_id).cloned())
                                    .is_some_and(|subscription| {
                                        subscription.kind == "conversation"
                                            && subscription.identity == change.display_session_id
                                    });
                                if affects_subscription {
                                    // 权限 pending 与 AskUser 一样不是 Journal 行；重新读取连接裁剪快照。
                                    inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
        let abort = task.abort_handle();
        let mut subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        if let Some(subscription) = subscriptions.get_mut(&subscription_id) {
            subscription.abort = Some(abort);
            Ok(())
        } else {
            abort.abort();
            Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅在启动实时投递前已被释放",
            ))
        }
    }

    /// sessions-index 没有 Session Journal 事件订阅，单独监听 Coordinator 的 pending
    /// 广播；快照只刷新侧栏摘要，完整问题仍由 conversation topic 返回。
    fn spawn_interaction_forwarder(
        &self,
        app: AppHandle,
        runtime: Arc<AgentRuntime>,
        subscription_id: String,
        mut interaction_events: tokio::sync::broadcast::Receiver<ElicitationChange>,
        mut permission_events: tokio::sync::broadcast::Receiver<PermissionChange>,
    ) -> Result<(), RpcError> {
        let inner = Arc::clone(&self.inner);
        let task_subscription_id = subscription_id.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = interaction_events.recv() => {
                        match result {
                            Ok(change) => {
                                let affects_subscription = inner
                                    .subscriptions
                                    .lock()
                                    .ok()
                                    .and_then(|subscriptions| subscriptions.get(&task_subscription_id).cloned())
                                    .is_some_and(|subscription| {
                                        subscription.kind == "sessions-index"
                                            || (subscription.kind == "conversation"
                                                && subscription.session_id.as_deref()
                                                    == Some(change.display_session_id.as_str())
                                                && subscription.agent_id.is_none())
                                    });
                                if affects_subscription {
                                    inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    result = permission_events.recv() => {
                        match result {
                            Ok(_change) => inner.emit_snapshot(&app, &runtime, &task_subscription_id),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                inner.emit_snapshot(&app, &runtime, &task_subscription_id);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
        let abort = task.abort_handle();
        let mut subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        if let Some(subscription) = subscriptions.get_mut(&subscription_id) {
            subscription.abort = Some(abort);
            Ok(())
        } else {
            abort.abort();
            Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅在启动 pending 投影前已被释放",
            ))
        }
    }

    fn resync(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let subscription_id = required_string(args, "subscriptionId")?;
        let subscription = self
            .inner
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .get(&subscription_id)
            .cloned();
        let Some(subscription) = subscription else {
            // Session close/delete、连接断开或先到的 duplicate unsubscribe 可能已经
            // 释放真实订阅；同一 owner 的迟到关闭必须幂等，foreign owner 仍拒绝。
            if released_subscription_belongs_to(&subscription_id, &ctx.connection_id) {
                return Ok(Value::Null);
            }
            return Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅不存在或已释放",
            ));
        };
        if subscription.connection_id != ctx.connection_id {
            return Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅不属于当前连接",
            ));
        }
        if let Some(topic) = optional_string(args, "topic")?
            && topic != subscription.topic
        {
            return Err(invalid_params("topic 与已登记订阅不一致"));
        }
        let runtime = owned_runtime(&ctx.app)?;
        let _snapshot_read = self
            .inner
            .snapshot_reads
            .lock()
            .map_err(|_| state_error())?;
        let frame_result = self
            .inner
            .with_frame_gate(&subscription_id, || {
                let frame = self.inner.normalize_snapshot_frame(
                    &subscription_id,
                    self.snapshot_for_subscription(&ctx.app, &runtime, &subscription)?,
                    false,
                );
                self.inner.refresh_retry_marker(
                    &ctx.app,
                    &runtime,
                    &subscription_id,
                    &subscription,
                );
                self.inner.queue_or_emit_frame(
                    &subscription_id,
                    frame,
                    FrameDeliveryKind::Recovery,
                );
                Ok::<(), RpcError>(())
            })
            .ok_or_else(state_error)?;
        frame_result?;
        Ok(json!({
            "ack": {"subscriptionId": subscription_id, "mode": "snapshot", "logEpoch": subscription.log_epoch},
        }))
    }

    fn unsubscribe(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let subscription_id = required_string(args, "subscriptionId")?;
        if self
            .inner
            .subscription_for_unsubscribe(&subscription_id, &ctx.connection_id)?
            .is_some()
        {
            self.inner.remove_subscription(&subscription_id);
        }
        Ok(Value::Null)
    }

    fn snapshot_for_subscription(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription: &ActiveSubscription,
    ) -> Result<Value, RpcError> {
        match subscription.kind.as_str() {
            "conversation" => {
                let scope = projection::ConversationScope {
                    requested_session_id: subscription.identity.clone(),
                    parent_session_id: subscription
                        .session_id
                        .clone()
                        .unwrap_or_else(|| subscription.identity.clone()),
                    agent_id: subscription.agent_id.clone(),
                };
                projection::conversation_frame_for_connection_with_agent(
                    runtime,
                    app,
                    &scope,
                    &subscription.connection_id,
                    &subscription.topic,
                    &subscription.subscription_id,
                    &subscription.log_epoch,
                )
                .map_err(backend_error)
            }
            "sessions-index" => projection::sessions_index_frame_for_connection(
                runtime,
                &subscription.workspace_path,
                &subscription.identity,
                &subscription.subscription_id,
                &subscription.log_epoch,
                Some(
                    &keencode_acp::ConnectionId::new(subscription.connection_id.clone())
                        .map_err(|error| backend_error(error.to_string()))?,
                ),
            )
            .map_err(backend_error),
            "workspace-config" => {
                let snapshot = projection::workspace_config_snapshot(app, &subscription.identity)
                    .map_err(backend_error)?;
                Ok(json!({
                    "topic": subscription.topic,
                    "subscriptionId": subscription.subscription_id,
                    "fromSeq": 0,
                    "toSeq": 0,
                    "sentAt": protocol::now_epoch_ms(),
                    "payload": {"kind": "snapshot", "snapshot": snapshot},
                }))
            }
            _ => Err(unknown_method("subscription", &subscription.kind)),
        }
    }

    fn rows_range(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let session_id = required_string(args, "sessionId")?;
        let workspace_path = required_workspace_path(args)?;
        let runtime = owned_runtime(&ctx.app)?;
        let scope = projection::resolve_conversation_scope(
            &runtime,
            &ctx.app,
            &session_id,
            &workspace_path,
        )
        .map_err(backend_error)?;
        let epoch = log_epoch("conversation", &session_id, &workspace_path);
        let before = args.get("beforeRowId").and_then(Value::as_u64);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_params("limit 必须为数字"))? as usize;
        let mut result =
            projection::rows_range_with_agent(&runtime, &ctx.app, &scope, &epoch, before, limit)
                .map_err(backend_error)?;
        if let Some(seq) = self
            .inner
            .wire_watermark_for_session(&ctx.connection_id, &session_id)
        {
            result["atSeq"] = Value::from(seq);
        }
        Ok(result)
    }

    fn plans(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let session_id = required_string(args, "sessionId")?;
        let workspace_path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&session_id)
            .map_err(backend_error)?
            .is_some()
        {
            return Err(RpcError::new(
                "agent.readOnly",
                "core subagent view 没有独立 Plan artifact",
            ));
        }
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, &session_id, &workspace_path)
            .map_err(backend_error)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, &session_id).map_err(backend_error)?;
        let mut result = projection::conversation_plans(
            &session,
            &log_epoch("conversation", &session_id, &workspace_path),
        )
        .map_err(backend_error)?;
        if let Some(seq) = self
            .inner
            .wire_watermark_for_session(&ctx.connection_id, &session_id)
        {
            result["atSeq"] = Value::from(seq);
        }
        Ok(result)
    }

    fn workflow_events(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let session_id = required_string(args, "sessionId")?;
        let run_id = required_string(args, "runId")?;
        let workspace_path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&session_id)
            .map_err(backend_error)?
            .is_some()
        {
            return Err(RpcError::new(
                "agent.readOnly",
                "core subagent view 不继承父 Workflow 事件",
            ));
        }
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, &session_id, &workspace_path)
            .map_err(backend_error)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, &session_id).map_err(backend_error)?;
        let snapshot = session.snapshot().map_err(backend_error)?;
        let after = args
            .get("afterSequence")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, 500) as usize;
        let events = snapshot
            .state
            .workflow_events
            .get(&run_id)
            .into_iter()
            .flatten()
            .filter(|event| event.sequence > after)
            .take(limit + 1)
            .map(|event| {
                json!({
                    "sequence": event.sequence,
                    "type": event.event_type,
                    "payload": event.payload,
                })
            })
            .collect::<Vec<_>>();
        let has_more = events.len() > limit;
        Ok(
            json!({"events": events.into_iter().take(limit).collect::<Vec<_>>(), "hasMore": has_more}),
        )
    }

    fn workflow_runs(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let session_id = required_string(args, "sessionId")?;
        let workspace_path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&session_id)
            .map_err(backend_error)?
            .is_some()
        {
            return Ok(json!({"runs": []}));
        }
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, &session_id, &workspace_path)
            .map_err(backend_error)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, &session_id).map_err(backend_error)?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(64)
            .clamp(1, 64) as usize;
        projection::workflow_runs_query(&session, limit).map_err(backend_error)
    }

    fn query_commands(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let commands = args
            .get("commands")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_params("commands 必须为数组"))?;
        if commands.is_empty() {
            return Err(invalid_params("commands 不能为空"));
        }
        if commands.len() > 64 {
            return Err(invalid_params("commands 超出单次查询上限"));
        }
        // Source 的 commandKeySchema 是 strict：查询结果中的 key 必须能原样通过
        // 同一 schema。先在入口校验，不把缺失 commandId 或非字符串 sessionId
        // 悄悄降级成 workspace/global 查询。
        let query_workspace = optional_string(args, "workspacePath")?
            .map(|path| authorize_workspace_scope(&ctx.app, &path))
            .transpose()?;
        let cache = self
            .inner
            .command_results
            .lock()
            .map_err(|_| state_error())?
            .clone();
        let mut results = Vec::with_capacity(commands.len());
        for command in commands {
            reject_unknown_fields(command, &["sessionId", "commandId"], "commands[]")?;
            let session_id = match command.get("sessionId") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
                    Some(value.clone())
                }
                _ => return Err(invalid_params("commands[].sessionId 必须为字符串或 null")),
            };
            let command_id = command
                .get("commandId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| invalid_params("commands[].commandId 必须为非空字符串"))?;
            let scope = session_id
                .as_deref()
                .map(|value| format!("session:{value}"))
                .unwrap_or_else(|| {
                    let workspace = query_workspace
                        .as_ref()
                        .map(|path| path.to_string_lossy())
                        .unwrap_or_default();
                    format!("workspace:{workspace}")
                });
            let key = command_key(&scope, command_id);
            let result = if let Some(session_id) = session_id.as_deref() {
                let runtime = owned_runtime(&ctx.app)?;
                let workspace = query_workspace
                    .as_ref()
                    .ok_or_else(|| invalid_params("查询 Session 命令必须携带 workspacePath"))?;
                let workspace_path = workspace.to_string_lossy();
                projection::validate_workspace(&runtime, session_id, &workspace_path)
                    .map_err(backend_error)?;
                let session = open_authorized_session(&runtime, &ctx.app, session_id)
                    .map_err(backend_error)?;
                match session
                    .command_receipt(&scope, command_id)
                    .map_err(backend_error)?
                {
                    Some(receipt) => command_receipt_query_result(&receipt),
                    None => Value::String("unknown".to_owned()),
                }
            } else {
                cache
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::String("unknown".to_owned()))
            };
            results.push(json!({
                "key": {"sessionId": session_id, "commandId": command_id},
                "result": result
            }));
        }
        Ok(json!({"results": results}))
    }

    async fn send_command(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        self.require_initialized(ctx)?;
        let envelope = args.get("envelope").unwrap_or(args);
        let command_id = required_string(envelope, "commandId")?;
        let session_id = match envelope.get("sessionId") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
                Some(value.as_str())
            }
            _ => return Err(invalid_params("sessionId 必须为非空字符串或 null")),
        };
        let requested_workspace_path =
            required_workspace_path(args).or_else(|_| required_workspace_path(envelope))?;
        let command_type = required_string(envelope, "type")?;
        let payload = envelope
            .get("payload")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // 所有命令先通过同一 workspace 授权。这样收据作用域不会在 admission 后
        // 才发现请求路径与真实 Session 不一致，也不会留下无法执行的 admitted 记录。
        let authorized_workspace = authorize_workspace_scope(&ctx.app, &requested_workspace_path)?;
        let workspace_path = authorized_workspace.to_string_lossy().into_owned();
        let create_root = if session_id.is_none() && command_type == "createSession" {
            Some(authorized_workspace.clone())
        } else {
            None
        };
        let workspace_scope = create_root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned())
            .unwrap_or_else(|| workspace_path.clone());
        // commandId 是 renderer 的幂等身份，不属于连接生命周期。Session 命令以
        // Session 为作用域，create/global 命令以规范 workspace 为作用域，重连后仍可
        // 命中同一收据；真正副作用仍由 Runtime/WorkflowHost 的 operationId Journal
        // 再次裁决。
        let cache_scope = session_id
            .filter(|value| !value.is_empty())
            .map(|value| format!("session:{value}"))
            .unwrap_or_else(|| format!("workspace:{workspace_scope}"));
        let cache_key = command_key(&cache_scope, &command_id);
        let payload_sha256 =
            command_receipts::command_candidate(&cache_scope, &command_id, &command_type, &payload)
                .map_err(|error| invalid_params(error.to_string()))?
                .payload_sha256
                .clone();
        let mut receipt_session = None;
        if let Some(session_id) = session_id.filter(|value| !value.is_empty()) {
            let runtime = owned_runtime(&ctx.app)?;
            projection::validate_workspace(&runtime, session_id, &workspace_path)
                .map_err(backend_error)?;
            let session =
                open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
            match session
                .admit_command_receipt(&cache_scope, &command_id, &command_type, &payload_sha256)
                .map_err(backend_error)?
            {
                CommandReceiptAdmission::Execute(_) => {
                    // 删除事实与收据分别落在现有 Journal 和 deleted-sessions 负向
                    // membership 中。admission 后再复核一次，覆盖另一个连接刚完成
                    // deleteSession 的窗口；否则这条新命令可能重新打开已删除会话。
                    // deleteSession 自身保留幂等重试语义，不会被这个保护挡住。
                    let session_deleted = if command_type == "deleteSession" {
                        false
                    } else {
                        match is_session_deleted(&runtime, session_id, &workspace_path) {
                            Ok(deleted) => deleted,
                            Err(_) => {
                                let _ = session.finish_command_receipt(
                                    &cache_scope,
                                    &command_id,
                                    &command_type,
                                    &payload_sha256,
                                    CommandReceiptStatus::Unknown {
                                        reason_code: "fault.command.resultUnknown".to_owned(),
                                    },
                                );
                                return Err(RpcError::new(
                                    "fault.command.resultUnknown",
                                    "Session 删除事实无法确认，命令未重放",
                                ));
                            }
                        }
                    };
                    if session_deleted {
                        let error = RpcError::new(
                            "fault.command.targetNotFound",
                            "Session 已删除，不能继续执行命令",
                        );
                        let revision = session
                            .snapshot()
                            .ok()
                            .map(|snapshot| snapshot.state.transcript_revision)
                            .unwrap_or(0);
                        let ack = rejected_command_ack(&command_id, &error, revision);
                        if let Err(persist_error) = session.finish_command_receipt(
                            &cache_scope,
                            &command_id,
                            &command_type,
                            &payload_sha256,
                            CommandReceiptStatus::Rejected { ack: ack.clone() },
                        ) {
                            let _ = session.finish_command_receipt(
                                &cache_scope,
                                &command_id,
                                &command_type,
                                &payload_sha256,
                                CommandReceiptStatus::Unknown {
                                    reason_code: "fault.command.resultUnknown".to_owned(),
                                },
                            );
                            return Err(RpcError::new(
                                "fault.command.resultUnknown",
                                format!("已删除 Session 的拒绝 ACK 持久化失败：{persist_error}"),
                            ));
                        }
                        self.remember_command_ack(&cache_key, &ack)?;
                        return Ok(ack);
                    }
                    receipt_session = Some(session);
                }
                CommandReceiptAdmission::Existing(receipt) => {
                    return command_receipt_replay(&receipt);
                }
            }
        } else if command_type == "createSession" {
            // createSession 的 deterministic sessionId 由同一个 commandId 派生。
            // 先通过 Runtime 起点屏障打开/创建该 Session，再在首条用户输入和配置
            // 副作用前写入收据；SessionCreated 本身已经由 Runtime 的同一 operation
            // 身份幂等保护，收据负责覆盖其后的整条 create 流程。
            let runtime = owned_runtime(&ctx.app)?;
            let root = create_root.as_deref().ok_or_else(state_error)?;
            let session = runtime
                .open_or_create_session_serialized(root, None, &command_id)
                .await
                .map_err(|error| backend_error(error.to_string()))?;
            match session
                .admit_command_receipt(&cache_scope, &command_id, &command_type, &payload_sha256)
                .map_err(backend_error)?
            {
                CommandReceiptAdmission::Execute(_) => {
                    receipt_session = Some(session);
                }
                CommandReceiptAdmission::Existing(receipt) => {
                    return command_receipt_replay(&receipt);
                }
            }
        } else if let Some(previous) = self
            .inner
            .command_results
            .lock()
            .map_err(|_| state_error())?
            .get(&cache_key)
            .cloned()
        {
            let mut duplicate = previous;
            if duplicate.get("status").and_then(Value::as_str) != Some("rejected") {
                duplicate["status"] = Value::String("duplicate".to_owned());
            }
            return Ok(duplicate);
        }
        let base_revision = envelope.get("baseRevision").and_then(Value::as_u64);
        let base_log_epoch = envelope.get("baseLogEpoch").and_then(Value::as_str);
        let result = match self
            .execute_command(
                ctx,
                CommandExecutionRequest {
                    command_id: &command_id,
                    session_id,
                    workspace_path: &workspace_path,
                    command_type: &command_type,
                    payload: &payload,
                    base_revision,
                    base_log_epoch,
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Some(session) = receipt_session.as_ref() {
                    if is_deterministic_command_rejection(&command_type, &error) {
                        let revision = session
                            .snapshot()
                            .ok()
                            .map(|snapshot| snapshot.state.transcript_revision)
                            .unwrap_or(0);
                        let ack = rejected_command_ack(&command_id, &error, revision);
                        if let Err(persist_error) = session.finish_command_receipt(
                            &cache_scope,
                            &command_id,
                            &command_type,
                            &payload_sha256,
                            CommandReceiptStatus::Rejected { ack: ack.clone() },
                        ) {
                            let _ = session.finish_command_receipt(
                                &cache_scope,
                                &command_id,
                                &command_type,
                                &payload_sha256,
                                CommandReceiptStatus::Unknown {
                                    reason_code: "fault.command.resultUnknown".to_owned(),
                                },
                            );
                            return Err(RpcError::new(
                                "fault.command.resultUnknown",
                                format!("命令拒绝 ACK 持久化失败：{persist_error}"),
                            ));
                        }
                        self.remember_command_ack(&cache_key, &ack)?;
                        return Ok(ack);
                    }
                    let _ = session.finish_command_receipt(
                        &cache_scope,
                        &command_id,
                        &command_type,
                        &payload_sha256,
                        CommandReceiptStatus::Unknown {
                            reason_code: "fault.command.resultUnknown".to_owned(),
                        },
                    );
                    return Err(RpcError::new(
                        "fault.command.resultUnknown",
                        "命令执行结果无法证明，已禁止自动重放",
                    ));
                }
                return Err(error);
            }
        };
        let revision = session_id
            .and_then(|id| {
                owned_runtime(&ctx.app)
                    .ok()
                    .and_then(|runtime| runtime.session_snapshot(id).ok())
            })
            .map(|snapshot| snapshot.state.transcript_revision)
            .unwrap_or(0);
        let result_rejected = result.get("accepted").and_then(Value::as_bool) == Some(false);
        let mut ack = json!({
            "commandId": command_id,
            "status": if result_rejected { "rejected" } else { "accepted" },
            "revisionAtDecision": revision,
        });
        if result_rejected
            && let Some(reason_code) = result.get("reasonCode").and_then(Value::as_str)
        {
            ack["reasonCode"] = Value::String(reason_code.to_owned());
        }
        // commandAck.result 是 optional；把 Rust 的无结果 `null` 省略掉，避免
        // ZCode strict result schema 将正常 stop/rename 等命令拒绝为 null 类型。
        if !result.is_null() {
            ack["result"] = result;
        }
        if let Some(session) = receipt_session.as_ref() {
            let terminal = if result_rejected {
                CommandReceiptStatus::Rejected { ack: ack.clone() }
            } else {
                CommandReceiptStatus::Completed { ack: ack.clone() }
            };
            if let Err(error) = session.finish_command_receipt(
                &cache_scope,
                &command_id,
                &command_type,
                &payload_sha256,
                terminal,
            ) {
                let _ = session.finish_command_receipt(
                    &cache_scope,
                    &command_id,
                    &command_type,
                    &payload_sha256,
                    CommandReceiptStatus::Unknown {
                        reason_code: "fault.command.resultUnknown".to_owned(),
                    },
                );
                return Err(RpcError::new(
                    "fault.command.resultUnknown",
                    format!("命令 ACK 持久化失败：{error}"),
                ));
            }
        }
        if !result_rejected
            && matches!(
                command_type.as_str(),
                "sendGoalCommand" | "pauseGoal" | "resumeGoal"
            )
            && let Some(session_id) = session_id
            && let Ok(runtime) = owned_runtime(&ctx.app)
        {
            // GoalFileStore 是唯一事实源；命令提交完成后立即重读同一连接的
            // snapshot，使 pause/resume 的 CAS 结果和 sendGoalCommand 的后续事实
            // 走现有订阅通道，不在前端建立乐观 Goal 缓存。
            self.inner.emit_snapshots_for_session(
                &ctx.app,
                &runtime,
                session_id,
                &ctx.connection_id,
            );
        }
        self.remember_command_ack(&cache_key, &ack)?;
        Ok(ack)
    }

    /// 旧进程内缓存只用于兼容 global/create 查询；Session 收据仍以 Journal 为准。
    fn remember_command_ack(&self, cache_key: &str, ack: &Value) -> Result<(), RpcError> {
        let mut order = self.inner.command_order.lock().map_err(|_| state_error())?;
        let mut cache = self
            .inner
            .command_results
            .lock()
            .map_err(|_| state_error())?;
        if !cache.contains_key(cache_key) {
            while cache.len() >= 2_048 {
                let Some(oldest) = order.pop_front() else {
                    // 只为兼容旧进程内状态保留有界性；正常路径 order 与 cache 同步。
                    let Some(oldest) = cache.keys().next().cloned() else {
                        break;
                    };
                    cache.remove(&oldest);
                    break;
                };
                cache.remove(&oldest);
            }
            cache.insert(cache_key.to_owned(), ack.clone());
            order.push_back(cache_key.to_owned());
        } else {
            cache.insert(cache_key.to_owned(), ack.clone());
        }
        Ok(())
    }

    async fn execute_command(
        &self,
        ctx: &GatewayContext,
        request: CommandExecutionRequest<'_>,
    ) -> Result<Value, RpcError> {
        let CommandExecutionRequest {
            command_id,
            session_id,
            workspace_path,
            command_type,
            payload,
            base_revision,
            base_log_epoch,
        } = request;
        if session_id.is_some_and(projection::is_virtual_agent_view_id) {
            return Err(RpcError::new(
                "agent.readOnly",
                "core subagent view 只支持读取和订阅，不能发送或修改父 Session",
            ));
        }
        match command_type {
            "createSession" => {
                let runtime = owned_runtime(&ctx.app)?;
                let root = authorize_workspace_scope(&ctx.app, workspace_path)?;
                let session = runtime
                    .open_or_create_session_serialized(&root, None, command_id)
                    .await
                    .map_err(|error| backend_error(error.to_string()))
                    .inspect_err(|error| {
                        log_send_stage_failure(
                            "create_session.open_or_create_session",
                            error.code(),
                        );
                    })?;
                let id = session.session_id().as_str().to_owned();
                runtime
                    .claim_deferred_session(&id)
                    .map_err(|error| backend_error(error.to_string()))
                    .inspect_err(|error| {
                        log_send_stage_failure(
                            "create_session.claim_deferred_session",
                            error.code(),
                        );
                    })?;
                prepare_session_reference_catalog(&ctx.app, &runtime, &id, &root)
                    .await
                    .inspect_err(|error| {
                        log_send_stage_failure(
                            "create_session.prepare_reference_catalog",
                            error.code(),
                        );
                    })?;
                apply_requested_config(&runtime, &ctx.app, &id, command_id, payload).inspect_err(
                    |error| {
                        log_send_stage_failure(
                            "create_session.apply_requested_config",
                            error.code(),
                        );
                    },
                )?;
                let input = payload
                    .get("firstInput")
                    .and_then(|value| value.get("text"));
                let input_result = if let Some(text) = input.and_then(Value::as_str) {
                    let mut first_input = payload
                        .get("firstInput")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    if let Some(object) = first_input.as_object_mut() {
                        object.insert("text".to_owned(), Value::String(text.to_owned()));
                        if !object.contains_key("planEnabled") && plan_enabled_in_payload(payload) {
                            object.insert("planEnabled".to_owned(), Value::Bool(true));
                        }
                    }
                    Some(
                        self.send_text(ctx, &id, workspace_path, command_id, &first_input)
                            .await
                            .inspect_err(|error| {
                                log_send_stage_failure(
                                    "create_session.first_input_send",
                                    error.code(),
                                );
                            })?,
                    )
                } else {
                    None
                };
                Ok(json!({"type": "createSession", "sessionId": id, "input": input_result}))
            }
            "createSelectionSideSession" => {
                let parent_id = session_id
                    .ok_or_else(|| invalid_params("createSelectionSideSession 缺少父 sessionId"))?;
                command_handlers::create_selection_side_session(
                    self,
                    ctx,
                    parent_id,
                    workspace_path,
                    command_id,
                    payload,
                )
                .await
            }
            "sendText" => {
                let id = session_id.ok_or_else(|| invalid_params("sendText 缺少 sessionId"))?;
                let input = self
                    .send_text(ctx, id, workspace_path, command_id, payload)
                    .await?;
                Ok(input)
            }
            "requestWorkspaceHookReview" => self.request_workspace_hook_review(
                ctx,
                session_id,
                workspace_path,
                command_id,
                payload,
            ),
            "respondWorkspaceHookReview" => self.respond_workspace_hook_review(
                ctx,
                session_id,
                workspace_path,
                command_id,
                payload,
            ),
            "revokeWorkspaceHookTrust" => self.revoke_workspace_hook_trust(
                ctx,
                session_id,
                workspace_path,
                command_id,
                payload,
            ),
            "stop" => {
                let id = session_id.ok_or_else(|| invalid_params("stop 缺少 sessionId"))?;
                self.stop_session(ctx, id, workspace_path)?;
                Ok(Value::Null)
            }
            "switchModelConfig" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("switchModelConfig 缺少 sessionId"))?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let provider = required_string(payload, "provider")?;
                let model = required_string(payload, "model")?;
                let _ = runtime
                    .set_session_model(id, command_id, &provider, &model)
                    .map_err(|error| backend_error(error.to_string()))?;
                if let Some(thought) = payload.get("thought").and_then(Value::as_str) {
                    runtime
                        .set_session_effort(id, command_id, thought)
                        .map_err(|error| backend_error(error.to_string()))?;
                }
                Ok(Value::Null)
            }
            "renameSession" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("renameSession 缺少 sessionId"))?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let session =
                    open_authorized_session(&runtime, &ctx.app, id).map_err(backend_error)?;
                let title = required_string(payload, "title")?;
                session
                    .rename(
                        command_id,
                        title,
                        Some(keencode_resources::TitleSource::Manual),
                    )
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "deleteSession" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("deleteSession 缺少 sessionId"))?;
                self.delete_session(ctx, id, workspace_path).await?;
                Ok(Value::Null)
            }
            "forkAssistant" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("forkAssistant 缺少 sessionId"))?;
                let target = payload
                    .get("target")
                    .ok_or_else(|| invalid_params("forkAssistant 缺少 target"))?;
                let through_turn_id = target.get("turnId").and_then(Value::as_str);
                let fork_id = self
                    .fork_session(ctx, id, workspace_path, command_id, through_turn_id)
                    .await?;
                Ok(json!({"type": "forkAssistant", "sessionId": fork_id}))
            }
            "editUserQuery" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("editUserQuery 缺少 sessionId"))?;
                let target = payload
                    .get("target")
                    .ok_or_else(|| invalid_params("editUserQuery 缺少 target"))?;
                let target_id = required_string(target, "entityId")?;
                let new_text = required_string(payload, "newText")?;
                self.edit_user(ctx, id, workspace_path, command_id, &target_id, &new_text)
                    .await?;
                Ok(json!({"type": "editUserQuery", "disposition": "rewind", "sessionId": id}))
            }
            "retryTurn" => {
                let id = session_id.ok_or_else(|| invalid_params("retryTurn 缺少 sessionId"))?;
                let target = payload
                    .get("target")
                    .ok_or_else(|| invalid_params("retryTurn 缺少 target"))?;
                let target_id = required_string(target, "entityId")?;
                let text = find_message_text(&ctx.app, id, workspace_path, &target_id)?;
                self.send_text(ctx, id, workspace_path, command_id, &json!({"text": text}))
                    .await
            }
            "applyFileRewind" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("applyFileRewind 缺少 sessionId"))?;
                let base_revision = base_revision
                    .ok_or_else(|| invalid_params("applyFileRewind 缺少 baseRevision"))?;
                let base_log_epoch = base_log_epoch
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid_params("applyFileRewind 缺少 baseLogEpoch"))?;
                command_handlers::apply_file_rewind(
                    ctx,
                    id,
                    workspace_path,
                    command_id,
                    payload,
                    base_revision,
                    base_log_epoch,
                )
                .await
            }
            "setAssistantFeedback" => {
                let id = session_id
                    .ok_or_else(|| invalid_params("setAssistantFeedback 缺少 sessionId"))?;
                let base_revision = base_revision
                    .ok_or_else(|| invalid_params("setAssistantFeedback 缺少 baseRevision"))?;
                let base_log_epoch = base_log_epoch
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid_params("setAssistantFeedback 缺少 baseLogEpoch"))?;
                let target = payload
                    .get("target")
                    .ok_or_else(|| invalid_params("setAssistantFeedback 缺少 target"))?;
                let row_id = target
                    .get("rowId")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| invalid_params("rowId 必须为非负整数"))?;
                let entity_id = required_string(target, "entityId")?;
                let feedback = match payload.get("feedback") {
                    Some(Value::Null) => None,
                    Some(Value::String(value)) => match value.as_str() {
                        "like" => Some(AssistantFeedback::Like),
                        "dislike" => Some(AssistantFeedback::Dislike),
                        _ => return Err(invalid_params("feedback 必须为 like、dislike 或 null")),
                    },
                    _ => return Err(invalid_params("feedback 必须为 like、dislike 或 null")),
                };
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let session =
                    open_authorized_session(&runtime, &ctx.app, id).map_err(backend_error)?;
                let snapshot = session.snapshot().map_err(backend_error)?;
                let state = snapshot.state;
                if state.transcript_revision != base_revision {
                    return Err(stale_revision(format!(
                        "Assistant 反馈基于 revision {base_revision}，当前为 {}",
                        state.transcript_revision
                    )));
                }
                if projection::conversation_log_epoch(id, &state.project_root) != base_log_epoch {
                    return Err(stale_revision(
                        "Assistant 反馈基于过期的 conversation log epoch",
                    ));
                }
                if !projection::conversation_row_is_assistant(&state, row_id, &entity_id) {
                    return Err(RpcError::new(
                        "fault.conversation.targetNotFound",
                        "目标行不是当前会话的 Assistant 回复",
                    ));
                }
                session
                    .set_assistant_feedback(
                        command_id,
                        row_id,
                        &entity_id,
                        feedback,
                        base_revision,
                        &state.project_root,
                    )
                    .map_err(|error| match error {
                        keencode_runtime::RuntimeError::StaleTranscriptRevision {
                            expected,
                            current,
                        } => stale_revision(format!(
                            "Assistant 反馈基于 revision {expected}，当前为 {current}"
                        )),
                        keencode_runtime::RuntimeError::StaleProjectRoot => {
                            stale_revision("Assistant 反馈基于过期的 conversation log epoch")
                        }
                        other => backend_error(other.to_string()),
                    })?;
                Ok(Value::Null)
            }
            "switchCollaborationMode" => {
                let id = session_id
                    .ok_or_else(|| invalid_params("switchCollaborationMode 缺少 sessionId"))?;
                let mode = required_string(payload, "mode")?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let session =
                    open_authorized_session(&runtime, &ctx.app, id).map_err(backend_error)?;
                let enabled = mode == "plan";
                session
                    .set_plan(
                        command_id,
                        keencode_resources::PlanState {
                            enabled,
                            plan_artifact: session
                                .snapshot()
                                .map_err(backend_error)?
                                .state
                                .plan
                                .plan_artifact,
                        },
                    )
                    .map_err(|error| backend_error(error.to_string()))?;
                runtime
                    .set_permission_mode_from_wire(id, Some(&mode))
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "setFollowupMode" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("setFollowupMode 缺少 sessionId"))?;
                let mode = parse_followup_mode(required_string(payload, "mode")?)?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                runtime
                    .set_session_followup_mode(id, command_id, mode)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(json!({"type": "setFollowupMode", "mode": match mode {
                    FollowupMode::Queue => "queue",
                    FollowupMode::Guide => "guide",
                }}))
            }
            "resolveInteraction" => {
                let id = session_id
                    .ok_or_else(|| invalid_params("resolveInteraction 缺少 sessionId"))?;
                self.resolve_interaction(ctx, id, workspace_path, payload)
                    .await
            }
            "snoozeInteractionAutoResolution" => {
                let id = session_id.ok_or_else(|| {
                    invalid_params("snoozeInteractionAutoResolution 缺少 sessionId")
                })?;
                let interaction_id = required_string(payload, "interactionId")?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let connection_id = keencode_acp::ConnectionId::new(ctx.connection_id.clone())
                    .map_err(|error| backend_error(error.to_string()))?;
                let changed = runtime
                    .snooze_interaction_auto_resolution(id, &connection_id, &interaction_id)
                    .map_err(|error| {
                        RpcError::new(
                            "fault.command.snoozeInteractionAutoResolution",
                            error.to_string(),
                        )
                    })?;
                Ok(json!({"type": "snoozeInteractionAutoResolution", "changed": changed}))
            }
            "sendQueuedNow" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("sendQueuedNow 缺少 sessionId"))?;
                self.send_queued_now(ctx, id, workspace_path, command_id, payload)
                    .await
            }
            "editQueueItem" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("editQueueItem 缺少 sessionId"))?;
                let queue_item_id = required_string(payload, "queueItemId")?;
                let new_text = required_string(payload, "newText")?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                runtime
                    .edit_session_input(id, command_id, &queue_item_id, new_text)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "reorderQueueItem" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("reorderQueueItem 缺少 sessionId"))?;
                let queue_item_id = required_string(payload, "queueItemId")?;
                let before = payload.get("beforeQueueItemId").and_then(Value::as_str);
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                runtime
                    .reorder_session_input(id, command_id, &queue_item_id, before)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "deleteQueueItem" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("deleteQueueItem 缺少 sessionId"))?;
                let queue_item_id = required_string(payload, "queueItemId")?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                runtime
                    .delete_session_input(id, command_id, &queue_item_id)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "setAutoDrain" => {
                let id = session_id.ok_or_else(|| invalid_params("setAutoDrain 缺少 sessionId"))?;
                let auto_drain = payload
                    .get("autoDrain")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid_params("setAutoDrain 缺少 autoDrain"))?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                runtime
                    .set_session_input_auto_drain(id, command_id, auto_drain)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(Value::Null)
            }
            "sendGoalCommand" => {
                let id =
                    session_id.ok_or_else(|| invalid_params("sendGoalCommand 缺少 sessionId"))?;
                self.send_goal_command(ctx, id, workspace_path, command_id, payload)
                    .await
            }
            "compact" => {
                let id = session_id.ok_or_else(|| invalid_params("compact 缺少 sessionId"))?;
                self.enqueue_maintenance_input(
                    ctx,
                    id,
                    workspace_path,
                    command_id,
                    payload,
                    SessionInputKind::Compact,
                )
                .await
            }
            "pauseGoal" => {
                let id = session_id.ok_or_else(|| invalid_params("pauseGoal 缺少 sessionId"))?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let change = runtime
                    .pause_session_goal(id, command_id)
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(json!({
                    "type": "pauseGoal",
                    "status": "paused",
                    "changed": change.changed,
                }))
            }
            "resumeGoal" => {
                let id = session_id.ok_or_else(|| invalid_params("resumeGoal 缺少 sessionId"))?;
                let runtime = authorized_runtime_session(&ctx.app, id, workspace_path)?;
                let change = runtime
                    .resume_session_goal(id, command_id)
                    .await
                    .map_err(|error| backend_error(error.to_string()))?;
                Ok(json!({
                    "type": "resumeGoal",
                    "status": "active",
                    "changed": change.changed,
                }))
            }
            command_type if is_canonical_workflow_command(command_type) => {
                let id = session_id
                    .ok_or_else(|| invalid_params(format!("{command_type} 缺少 sessionId")))?;
                self.execute_workflow_command(
                    ctx,
                    id,
                    workspace_path,
                    command_id,
                    command_type,
                    payload,
                )
                .await
            }
            _ => Err(unknown_method(AGENT_CHANNEL, command_type)),
        }
    }

    fn request_workspace_hook_review(
        &self,
        ctx: &GatewayContext,
        session_id: Option<&str>,
        workspace_path: &str,
        command_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let session_id = session_id
            .ok_or_else(|| invalid_params("requestWorkspaceHookReview 缺少 sessionId"))?;
        reject_unknown_fields(
            payload,
            &[
                "sessionId",
                "remoteSessionId",
                "workspaceIdentity",
                "bundleDigest",
            ],
            "requestWorkspaceHookReview",
        )?;
        if required_string(payload, "sessionId")? != session_id {
            return Err(invalid_params(
                "requestWorkspaceHookReview 不能跨 Session 请求",
            ));
        }
        let workspace_identity = required_string(payload, "workspaceIdentity")?;
        let bundle_digest = required_string(payload, "bundleDigest")?;
        let workspace = authorize_workspace_scope(&ctx.app, workspace_path)?;
        let runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
        let run_id = optional_string(payload, "runId")?.or_else(|| {
            session
                .active_turn_ids()
                .ok()
                .and_then(|turns| turns.into_iter().next())
                .map(|turn| turn.as_str().to_owned())
        });
        let run_id = run_id.unwrap_or_else(|| session_id.to_owned());
        let task_id = optional_string(payload, "taskId")?.unwrap_or_else(|| session_id.to_owned());
        let workspace_label = workspace
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(workspace_identity.as_str())
            .to_owned();
        let result = workspace_hook_review::WorkspaceHookReviewCoordinator::for_app(&ctx.app)
            .map_err(backend_error)?
            .request(RequestContext {
                connection_id: ctx.connection_id.clone(),
                command_id: command_id.to_owned(),
                session_id: session_id.to_owned(),
                task_id,
                run_id,
                remote_session_id: strict_optional_string(payload, "remoteSessionId")?,
                workspace_path: workspace,
                workspace_identity,
                workspace_label,
                bundle_digest,
                now_ms: protocol::now_epoch_ms(),
            })
            .map_err(backend_error)?;
        if let Ok(runtime) = owned_runtime(&ctx.app) {
            self.inner.emit_snapshots_for_session(
                &ctx.app,
                &runtime,
                session_id,
                &ctx.connection_id,
            );
        }
        Ok(result)
    }

    fn respond_workspace_hook_review(
        &self,
        ctx: &GatewayContext,
        session_id: Option<&str>,
        workspace_path: &str,
        command_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let session_id = session_id
            .ok_or_else(|| invalid_params("respondWorkspaceHookReview 缺少 sessionId"))?;
        let target_session_id = required_string(payload, "sessionId")?;
        if target_session_id != session_id {
            return Err(invalid_params(
                "respondWorkspaceHookReview 不能跨 Session 回答",
            ));
        }
        reject_unknown_fields(
            payload,
            &[
                "sessionId",
                "taskId",
                "runId",
                "remoteSessionId",
                "workspaceIdentity",
                "bundleDigest",
                "reviewFlowId",
                "generation",
                "interactionId",
                "decision",
            ],
            "respondWorkspaceHookReview",
        )?;
        let _runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let workspace = authorize_workspace_scope(&ctx.app, workspace_path)?;
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .filter(|generation| *generation > 0)
            .ok_or_else(|| invalid_params("generation 必须为正整数"))?;
        let decision = payload
            .get("decision")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_params("decision 必须为对象"))?;
        reject_unknown_fields(
            &Value::Object(decision.clone()),
            &["action", "reviewItemIds"],
            "respondWorkspaceHookReview.decision",
        )?;
        if decision.get("action").and_then(Value::as_str) != Some("trust_selected") {
            return Err(invalid_params("decision.action 必须为 trust_selected"));
        }
        let review_item_ids = decision
            .get("reviewItemIds")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_params("decision.reviewItemIds 必须为数组"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty() && value.trim() == *value)
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid_params("reviewItemIds 必须是非空字符串"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if review_item_ids.is_empty() {
            return Err(invalid_params("decision.reviewItemIds 不能为空"));
        }
        let mut unique = std::collections::BTreeSet::new();
        if review_item_ids.iter().any(|item| !unique.insert(item)) {
            return Err(invalid_params("decision.reviewItemIds 不允许重复"));
        }
        let result = workspace_hook_review::WorkspaceHookReviewCoordinator::for_app(&ctx.app)
            .map_err(backend_error)?
            .respond(RespondContext {
                connection_id: ctx.connection_id.clone(),
                command_id: command_id.to_owned(),
                session_id: session_id.to_owned(),
                task_id: required_string(payload, "taskId")?,
                run_id: required_string(payload, "runId")?,
                remote_session_id: strict_optional_string(payload, "remoteSessionId")?,
                workspace_path: workspace,
                workspace_identity: required_string(payload, "workspaceIdentity")?,
                bundle_digest: required_string(payload, "bundleDigest")?,
                review_flow_id: required_string(payload, "reviewFlowId")?,
                generation,
                interaction_id: required_string(payload, "interactionId")?,
                review_item_ids,
                now_ms: protocol::now_epoch_ms(),
            })
            .map_err(backend_error)?;
        if let Ok(runtime) = owned_runtime(&ctx.app) {
            self.inner.emit_snapshots_for_session(
                &ctx.app,
                &runtime,
                session_id,
                &ctx.connection_id,
            );
        }
        Ok(result)
    }

    fn revoke_workspace_hook_trust(
        &self,
        ctx: &GatewayContext,
        session_id: Option<&str>,
        workspace_path: &str,
        command_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let session_id =
            session_id.ok_or_else(|| invalid_params("revokeWorkspaceHookTrust 缺少 sessionId"))?;
        let target_session_id = required_string(payload, "sessionId")?;
        if target_session_id != session_id {
            return Err(invalid_params(
                "revokeWorkspaceHookTrust 不能跨 Session 操作",
            ));
        }
        let flow_target = payload.get("reviewFlowId").is_some();
        reject_unknown_fields(
            payload,
            if flow_target {
                &[
                    "sessionId",
                    "taskId",
                    "runId",
                    "remoteSessionId",
                    "workspaceIdentity",
                    "bundleDigest",
                    "reviewFlowId",
                    "generation",
                    "interactionId",
                    "reviewItemIds",
                ][..]
            } else {
                &[
                    "sessionId",
                    "remoteSessionId",
                    "workspaceIdentity",
                    "bundleDigest",
                    "hookDeclarationDigests",
                ][..]
            },
            "revokeWorkspaceHookTrust",
        )?;
        let _runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let workspace = authorize_workspace_scope(&ctx.app, workspace_path)?;
        let review_flow_id = optional_string(payload, "reviewFlowId")?;
        let hook_declaration_digests = payload
            .get("hookDeclarationDigests")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                            invalid_params("hookDeclarationDigests 必须是字符串数组")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let (generation, interaction_id, review_item_ids) = if review_flow_id.is_some() {
            let generation = payload
                .get("generation")
                .and_then(Value::as_u64)
                .filter(|generation| *generation > 0)
                .ok_or_else(|| invalid_params("generation 必须为正整数"))?;
            let review_item_ids = payload
                .get("reviewItemIds")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_params("reviewItemIds 必须为数组"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| invalid_params("reviewItemIds 必须是非空字符串"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if review_item_ids.is_empty() {
                return Err(invalid_params("reviewItemIds 不能为空"));
            }
            (
                Some(generation),
                Some(required_string(payload, "interactionId")?),
                review_item_ids,
            )
        } else {
            if hook_declaration_digests.is_empty() {
                return Err(invalid_params("hookDeclarationDigests 不能为空"));
            }
            (None, None, Vec::new())
        };
        let result = workspace_hook_review::WorkspaceHookReviewCoordinator::for_app(&ctx.app)
            .map_err(backend_error)?
            .revoke(RevokeContext {
                connection_id: ctx.connection_id.clone(),
                command_id: command_id.to_owned(),
                session_id: session_id.to_owned(),
                task_id: if flow_target {
                    Some(required_string(payload, "taskId")?)
                } else {
                    None
                },
                run_id: if flow_target {
                    Some(required_string(payload, "runId")?)
                } else {
                    None
                },
                workspace_path: workspace,
                workspace_identity: required_string(payload, "workspaceIdentity")?,
                bundle_digest: required_string(payload, "bundleDigest")?,
                hook_declaration_digests,
                review_flow_id,
                generation,
                interaction_id,
                review_item_ids,
                now_ms: protocol::now_epoch_ms(),
            })
            .map_err(backend_error)?;
        if let Ok(runtime) = owned_runtime(&ctx.app) {
            self.inner.emit_snapshots_for_session(
                &ctx.app,
                &runtime,
                session_id,
                &ctx.connection_id,
            );
        }
        Ok(result)
    }

    async fn send_text(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let text = required_string(payload, "text")?;
        let delivery = payload
            .get("requestedDelivery")
            .and_then(Value::as_str)
            .unwrap_or("startNow");
        if delivery != "startNow" && delivery != "queue" && delivery != "guide" {
            return Err(invalid_params("sendText.requestedDelivery 无效"));
        }
        // 首次发送先取得 deferred 生命周期认领，再应用模型/思考配置；旧草稿
        // cleanup 只能在线性化之前关闭，不能在配置与 Turn 起点之间释放 Runtime。
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, session_id, workspace_path).map_err(|error| {
            let error = backend_error(error);
            log_send_stage_failure("send_text.validate_workspace", error.code());
            error
        })?;
        runtime
            .claim_deferred_session(session_id)
            .map_err(|error| {
                let error = backend_error(error.to_string());
                log_send_stage_failure("send_text.claim_deferred_session", error.code());
                error
            })?;
        let root = authorize_workspace_scope(&ctx.app, workspace_path)?;
        runtime
            .open_or_create_session_serialized(&root, Some(session_id), operation_id)
            .await
            .map_err(|error| {
                let error = backend_error(error.to_string());
                log_send_stage_failure("send_text.open_or_create_session", error.code());
                error
            })?;
        prepare_session_reference_catalog(&ctx.app, &runtime, session_id, &root)
            .await
            .inspect_err(|error| {
                log_send_stage_failure("send_text.prepare_session_reference_catalog", error.code());
            })?;
        apply_requested_config(&runtime, &ctx.app, session_id, operation_id, payload).inspect_err(
            |error| {
                log_send_stage_failure("send_text.apply_requested_config", error.code());
            },
        )?;
        let was_promoted = session_is_promoted(&runtime, &ctx.app, session_id).unwrap_or(false);
        if delivery != "startNow" {
            let admitted_delivery = if delivery == "guide" {
                SessionInputDelivery::Guide
            } else {
                SessionInputDelivery::Queue
            };
            let result = self
                .enqueue_input_item(
                    ctx,
                    session_id,
                    workspace_path,
                    operation_id,
                    InputQueueRequest {
                        payload,
                        text: &text,
                        kind: SessionInputKind::SendText,
                        admitted_delivery,
                    },
                )
                .await?;
            self.emit_task_created_if_promoted(ctx, &runtime, &root, session_id, was_promoted);
            return Ok(result);
        }
        let attachments = payload
            .get("attachments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let prepared_attachments =
            attachments::prepare_send_attachments(ctx, session_id, &attachments).inspect_err(
                |error| {
                    log_send_stage_failure("send_text.prepare_send_attachments", error.code());
                },
            )?;
        let turn_id = format!("zcode-turn-{operation_id}");
        let developer_context =
            selected_subagent_route(&runtime, &root, session_id, &turn_id, &text)?;
        let options = RootTurnOptions {
            references: prepared_attachments.references,
            attachment_images: prepared_attachments.images,
            attachment_context: prepared_attachments.text_context,
            developer_context,
            plan_enabled: plan_enabled_in_payload(payload),
            elicitation_connection_id: Some(
                keencode_acp::ConnectionId::new(ctx.connection_id.clone()).map_err(|error| {
                    let error = backend_error(error.to_string());
                    log_send_stage_failure("send_text.connection_id", error.code());
                    error
                })?,
            ),
        };
        let outcome = runtime
            .start_root_turn(session_id, &turn_id, &text, options)
            .await
            .map_err(|error| {
                let error = backend_error(error.to_string());
                log_send_stage_failure("send_text.start_root_turn", error.code());
                error
            })?;
        self.emit_task_created_if_promoted(ctx, &runtime, &root, session_id, was_promoted);
        let input_id = operation_id.to_owned();
        runtime.schedule_automatic_title(session_id);
        Ok(json!({
            "type": "inputAccepted",
            "delivery": "startNow",
            "inputId": input_id,
            "deduplicated": matches!(outcome, crate::agent_runtime::RootTurnStartOutcome::Deduplicated),
        }))
    }

    /// 首条用户输入写入 Journal 并满足 promotion 条件后，才通知 tasks-index 增加成员。
    ///
    /// `SessionCreated`/配置写入只产生预热 draft，不应直接触发 task_created；事件的
    /// 去重键只存在网关进程内，持久成员仍由 task-groups 与 Journal 投影决定。
    fn emit_task_created_if_promoted(
        &self,
        ctx: &GatewayContext,
        runtime: &Arc<AgentRuntime>,
        root: &Path,
        session_id: &str,
        was_promoted: bool,
    ) {
        let is_promoted = session_is_promoted(runtime, &ctx.app, session_id).unwrap_or(false);
        if !task_promotion_transition(was_promoted, is_promoted) {
            return;
        }
        let workspace_path = root.to_string_lossy();
        let Ok(true) = self
            .inner
            .claim_task_created_announcement(&workspace_path, session_id)
        else {
            return;
        };
        self.inner.emit_workspace_task_event(
            &ctx.app,
            runtime,
            &workspace_path,
            session_id,
            "task_created",
        );
    }

    /// 在当前 Turn 收口后按权威队列状态自动提升队首；多个观察者并发触发时
    /// 由 Runtime 的 reserve 控制操作保证只有一个消费者成功。
    fn schedule_auto_drain(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        queue_item_id: &str,
    ) {
        let runtime = match owned_runtime(&ctx.app) {
            Ok(runtime) => runtime,
            Err(_) => return,
        };
        let mut events = match runtime.subscribe_session_events(session_id) {
            Ok(events) => events,
            Err(_) => return,
        };
        let ctx = ctx.clone();
        let session_id = session_id.to_owned();
        let workspace_path = workspace_path.to_owned();
        let queue_item_id = queue_item_id.to_owned();
        let drain_operation_id =
            format!("auto-drain-{}", queue_item_digest(queue_item_id.as_str()));
        let gateway = gateway();
        tokio::spawn(async move {
            loop {
                if ctx.is_closed() {
                    break;
                }
                let (_, queue) = match runtime.session_input_queue(&session_id) {
                    Ok(queue) => queue,
                    Err(_) => break,
                };
                let Some(item) = queue
                    .items
                    .iter()
                    .find(|item| item.queue_item_id == queue_item_id)
                else {
                    break;
                };
                if !queue.auto_drain
                    || item.admitted_delivery != SessionInputDelivery::Queue
                    || item.dispatch != SessionInputDispatch::Queued
                    || item.kind == SessionInputKind::Compact
                {
                    break;
                }
                let session = match open_authorized_session(&runtime, &ctx.app, &session_id) {
                    Ok(session) => session,
                    Err(_) => break,
                };
                let active = match session.active_turn_ids() {
                    Ok(turns) => !turns.is_empty(),
                    Err(_) => break,
                };
                if !active {
                    let result = gateway
                        .send_queued_now(
                            &ctx,
                            &session_id,
                            &workspace_path,
                            &drain_operation_id,
                            &json!({"queueItemId": queue_item_id}),
                        )
                        .await;
                    match result {
                        Ok(_) => break,
                        Err(error)
                            if serde_json::to_value(&error)
                                .ok()
                                .and_then(|value| {
                                    value.get("code").and_then(Value::as_str).map(|code| {
                                        code == "fault.command.sendQueuedNow.sessionBusy"
                                    })
                                })
                                .unwrap_or(false) =>
                        {
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                match events.recv().await {
                    Ok(_) | Err(RuntimeEventReceiveError::Lagged(_)) => continue,
                    Err(RuntimeEventReceiveError::Closed) => break,
                }
            }
        });
    }

    /// 将普通文本、Goal 命令或 compact 维护意图写入同一持久队列。
    async fn enqueue_input_item(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        request: InputQueueRequest<'_>,
    ) -> Result<Value, RpcError> {
        let runtime = authorized_runtime_session_serialized(
            &ctx.app,
            session_id,
            workspace_path,
            operation_id,
        )
        .await?;
        let client_id = self.inner.client_id(&ctx.connection_id)?;
        let attachments = request
            .payload
            .get("attachments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if matches!(request.kind, SessionInputKind::SendText) && !attachments.is_empty() {
            // 入队时先做一次授权和大小校验，消费时仍会在当前连接下重新绑定，
            // 这样队列不会把无权引用伪装成稍后可用的附件。
            attachments::prepare_send_attachments(ctx, session_id, &attachments)?;
        }
        let item = SessionInputQueueItem {
            queue_item_id: format!("queue:{operation_id}"),
            source_command_id: operation_id.to_owned(),
            client_id: Some(client_id),
            kind: request.kind,
            text: request.text.to_owned(),
            attachments,
            model_selection: request.payload.get("modelSelection").cloned(),
            mode: request
                .payload
                .get("mode")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            plan_enabled: plan_enabled_in_payload(request.payload),
            requested_delivery: request.admitted_delivery,
            admitted_delivery: request.admitted_delivery,
            admission_seq: 0,
            reserve_attempt: 0,
            dispatch: SessionInputDispatch::Queued,
            promoted_turn_id: None,
            admitted_at_unix_ms: protocol::now_epoch_ms(),
        };
        runtime
            .enqueue_session_input(session_id, operation_id, item)
            .map_err(|error| backend_error(error.to_string()))?;
        if matches!(request.admitted_delivery, SessionInputDelivery::Queue)
            && runtime
                .session_input_queue(session_id)
                .map_err(|error| backend_error(error.to_string()))?
                .1
                .auto_drain
        {
            self.schedule_auto_drain(
                ctx,
                session_id,
                workspace_path,
                &format!("queue:{operation_id}"),
            );
        }
        Ok(json!({
            "type": "inputAccepted",
            "delivery": match request.admitted_delivery {
                SessionInputDelivery::Queue => "queue",
                SessionInputDelivery::Guide => "guide",
            },
            "inputId": operation_id,
        }))
    }

    /// Goal 命令空闲时直接启动，运行中则保留为 typed queue intent。
    async fn send_goal_command(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let text = required_string(payload, "text")?;
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, session_id, workspace_path)
            .map_err(backend_error)?;
        runtime
            .claim_deferred_session(session_id)
            .map_err(|error| backend_error(error.to_string()))?;
        let root = authorize_workspace_scope(&ctx.app, workspace_path)?;
        runtime
            .open_or_create_session_serialized(&root, Some(session_id), operation_id)
            .await
            .map_err(|error| backend_error(error.to_string()))?;
        prepare_session_reference_catalog(&ctx.app, &runtime, session_id, &root).await?;
        let was_promoted = session_is_promoted(&runtime, &ctx.app, session_id).unwrap_or(false);
        let session =
            open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
        if !session
            .active_turn_ids()
            .map_err(|error| backend_error(error.to_string()))?
            .is_empty()
        {
            let (mode, _) = runtime
                .session_input_queue(session_id)
                .map_err(|error| backend_error(error.to_string()))?;
            let delivery = match mode {
                FollowupMode::Queue => SessionInputDelivery::Queue,
                FollowupMode::Guide => SessionInputDelivery::Guide,
            };
            let result = self
                .enqueue_input_item(
                    ctx,
                    session_id,
                    workspace_path,
                    operation_id,
                    InputQueueRequest {
                        payload,
                        text: &text,
                        kind: SessionInputKind::SendGoalCommand,
                        admitted_delivery: delivery,
                    },
                )
                .await?;
            self.emit_task_created_if_promoted(ctx, &runtime, &root, session_id, was_promoted);
            return Ok(result);
        }
        apply_requested_config(&runtime, &ctx.app, session_id, operation_id, payload)?;
        let options = RootTurnOptions {
            plan_enabled: plan_enabled_in_payload(payload),
            elicitation_connection_id: Some(
                keencode_acp::ConnectionId::new(ctx.connection_id.clone())
                    .map_err(|error| backend_error(error.to_string()))?,
            ),
            ..RootTurnOptions::default()
        };
        let outcome = runtime
            .start_root_turn(
                session_id,
                &format!("zcode-turn-{operation_id}"),
                &text,
                options,
            )
            .await
            .map_err(|error| backend_error(error.to_string()))?;
        self.emit_task_created_if_promoted(ctx, &runtime, &root, session_id, was_promoted);
        Ok(json!({
            "type": "inputAccepted",
            "delivery": "startNow",
            "inputId": operation_id,
            "deduplicated": matches!(outcome, crate::agent_runtime::RootTurnStartOutcome::Deduplicated),
        }))
    }

    /// compact 是可恢复的 typed maintenance intent；当前 Runtime 会先持久入队，
    /// 后续消费仍保留 kind，不把维护请求伪装成普通用户消息。
    async fn enqueue_maintenance_input(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        payload: &Value,
        kind: SessionInputKind,
    ) -> Result<Value, RpcError> {
        self.enqueue_input_item(
            ctx,
            session_id,
            workspace_path,
            operation_id,
            InputQueueRequest {
                payload,
                text: "",
                kind,
                admitted_delivery: SessionInputDelivery::Queue,
            },
        )
        .await
    }

    /// 显式消费一条队列项；Journal 先记录保留态，Turn 成功启动后再确认移除。
    async fn send_queued_now(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let queue_item_id = required_string(payload, "queueItemId")?;
        let runtime = authorized_runtime_session_serialized(
            &ctx.app,
            session_id,
            workspace_path,
            operation_id,
        )
        .await?;
        let root = authorize_workspace_scope(&ctx.app, workspace_path)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
        if !session
            .active_turn_ids()
            .map_err(|error| backend_error(error.to_string()))?
            .is_empty()
        {
            return Err(RpcError::new(
                "fault.command.sendQueuedNow.sessionBusy",
                "当前 Session 仍有活动 Turn，队列项不能越过正在运行的 Turn",
            ));
        }
        let (_, queue) = runtime
            .session_input_queue(session_id)
            .map_err(|error| backend_error(error.to_string()))?;
        if let Some(completion) = queue.completions.iter().find(|completion| {
            completion.queue_item_id == queue_item_id
                && completion.completion_operation_id == operation_id
        }) {
            return Ok(json!({
                "type": "inputAccepted",
                "delivery": match completion.admitted_delivery {
                    SessionInputDelivery::Queue => "queue",
                    SessionInputDelivery::Guide => "guide",
                },
                "inputId": completion.source_command_id,
                "deduplicated": true,
            }));
        }
        // 该 ID 是一次 sendQueuedNow 调用的稳定 Runtime Turn 身份；它在
        // reserve 阶段先持久化，冷恢复只按这个真实 ID 核对，不猜前端命名。
        let target_turn_id = queue_turn_id(operation_id);
        // reserve/release 使用内部尝试 ID，完成回执保留调用方 operationId，
        // 这样同一个 sendQueuedNow 重试可以命中完成 receipt，而不会与 reserve 事实冲突。
        let reserve_operation_id = format!("{operation_id}-reserve");
        let item = runtime
            .reserve_session_input(
                session_id,
                &reserve_operation_id,
                &queue_item_id,
                &target_turn_id,
            )
            .map_err(|error| backend_error(error.to_string()))?;
        if item.kind == SessionInputKind::Compact {
            let target_turn_id = item
                .promoted_turn_id
                .as_ref()
                .map(|turn_id| turn_id.as_str().to_owned())
                .ok_or_else(|| {
                    backend_error("compact 队列项缺少已持久化的维护 Turn 身份".to_owned())
                })?;
            match runtime
                .compact_session_context(session_id, operation_id, &target_turn_id)
                .await
            {
                Ok(()) => {
                    runtime
                        .complete_session_input(session_id, operation_id, &queue_item_id)
                        .map_err(|error| backend_error(error.to_string()))?;
                    return Ok(json!({
                        "type": "inputAccepted",
                        "delivery": "queue",
                        "inputId": item.source_command_id,
                        "deduplicated": false,
                    }));
                }
                Err(error) => {
                    let _ = runtime.release_session_input(
                        session_id,
                        &format!("{operation_id}-release"),
                        &queue_item_id,
                    );
                    return Err(backend_error(error.to_string()));
                }
            }
        }
        if let Some(selection) = item.model_selection.as_ref() {
            apply_model_value(&runtime, session_id, operation_id, selection)?;
        }
        let prepared_attachments =
            match attachments::prepare_send_attachments(ctx, session_id, &item.attachments) {
                Ok(prepared) => prepared,
                Err(error) => {
                    let _ = runtime.release_session_input(
                        session_id,
                        &format!("{operation_id}-release-attachments"),
                        &queue_item_id,
                    );
                    return Err(error);
                }
            };
        let turn_id = item
            .promoted_turn_id
            .as_ref()
            .map(|turn_id| turn_id.as_str().to_owned())
            .ok_or_else(|| backend_error("队列项缺少已持久化的 Runtime Turn 身份".to_owned()))?;
        let developer_context =
            selected_subagent_route(&runtime, &root, session_id, &turn_id, &item.text)?;
        let options = RootTurnOptions {
            references: prepared_attachments.references,
            attachment_images: prepared_attachments.images,
            attachment_context: prepared_attachments.text_context,
            developer_context,
            plan_enabled: item.plan_enabled,
            elicitation_connection_id: Some(
                keencode_acp::ConnectionId::new(ctx.connection_id.clone())
                    .map_err(|error| backend_error(error.to_string()))?,
            ),
        };
        let outcome = runtime
            .start_root_turn(session_id, &turn_id, &item.text, options)
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = runtime.release_session_input(
                    session_id,
                    &format!("{operation_id}-release"),
                    &queue_item_id,
                );
                return Err(backend_error(error.to_string()));
            }
        };
        runtime
            .complete_session_input(session_id, operation_id, &queue_item_id)
            .map_err(|error| backend_error(error.to_string()))?;
        Ok(json!({
            "type": "inputAccepted",
            "delivery": match item.admitted_delivery {
                SessionInputDelivery::Queue => "queue",
                SessionInputDelivery::Guide => "guide",
            },
            "inputId": item.source_command_id,
            "deduplicated": matches!(outcome, crate::agent_runtime::RootTurnStartOutcome::Deduplicated),
        }))
    }

    /// 将 V4 interaction 回执交给真实控制面。
    ///
    /// 权限与 AskUser answer 都交给 Runtime 的对应协调器严格解码；两者同时校验
    /// 请求连接和 actor→parent 路由，避免 gateway 维护第二份 pending interaction。
    async fn resolve_interaction(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let interaction_id = required_string(payload, "interactionId")?;
        let answer = payload
            .get("answer")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_params("resolveInteraction 缺少 answer"))?;
        // V4 answer 的 optionId/freeText/action/content 由 Runtime 的权限或
        // ElicitationCoordinator 按各自 Schema 解码；这里不把权限选项映射成猜测的
        // ACP content，也不允许绕过 actor->parent/session owner 校验。
        let answer_json =
            serde_json::to_string(&Value::Object(answer.clone())).map_err(|error| {
                invalid_params(format!("resolveInteraction.answer 无法编码: {error}"))
            })?;
        let connection_id = keencode_acp::ConnectionId::new(ctx.connection_id.clone())
            .map_err(|error| backend_error(error.to_string()))?;
        runtime
            .resolve_interaction(session_id, &connection_id, &interaction_id, &answer_json)
            .map_err(|error| {
                RpcError::new("fault.command.resolveInteraction", error.to_string())
            })?;
        let client_id = self.inner.client_id(&ctx.connection_id)?;
        let mut resolved_by = json!({"clientId": client_id});
        if let Some(option_id) = answer.get("optionId").and_then(Value::as_str) {
            resolved_by["optionId"] = Value::String(option_id.to_owned());
        }
        Ok(json!({
            "type": "resolveInteraction",
            "resolvedBy": resolved_by,
        }))
    }

    /// 执行需要 WorkflowHost 的 V4 控制命令。
    ///
    /// 所有身份字段都由本地 Runtime/Journal 生成或校验：前端只能提供工作流名、
    /// 输入和 runId，不能改写 parentSessionId、cwd、projectStorage 或已冻结模型。
    async fn execute_workflow_command(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        command_id: &str,
        command_type: &str,
        payload: &Value,
    ) -> Result<Value, RpcError> {
        let runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
        let root = crate::workspace::registered_project_root(&ctx.app, workspace_path)
            .map_err(backend_error)?;

        match command_type {
            "startSavedWorkflow" => {
                let active_turns = session
                    .active_turn_ids()
                    .map_err(|error| backend_error(error.to_string()))?;
                if !active_turns.is_empty() {
                    return Err(RpcError::new(
                        "fault.command.savedWorkflowStartRejected.session_busy",
                        "当前 Session 仍有活动 Turn，不能启动 Saved Workflow",
                    ));
                }
                let name = required_string(payload, "name")?;
                let scope = workflow_scope(payload.get("scope"))?;
                let inputs = payload.get("args").cloned().unwrap_or_else(|| json!({}));
                if !inputs.is_object() {
                    return Err(invalid_params("startSavedWorkflow.args 必须为 object"));
                }
                let was_promoted =
                    session_is_promoted(&runtime, &ctx.app, session_id).unwrap_or(false);
                let model_selection = frozen_model_selection(&session)?;
                let result = self
                    .workflow_call(
                        ctx.clone(),
                        "start",
                        json!({
                            "name": name,
                            "scope": scope,
                            "projectStorage": (scope == "project").then(|| root.clone()),
                            "parentSessionId": session_id,
                            "toolCallId": format!("launch-{command_id}"),
                            "launchInputId": command_id,
                            "inputs": inputs,
                            "cwd": root,
                            "modelSelection": model_selection,
                        }),
                    )
                    .await?;
                let run_id = workflow_result_string(&result, "runId", "run_id")?;
                self.emit_task_created_if_promoted(ctx, &runtime, &root, session_id, was_promoted);
                Ok(json!({
                    "type": "startSavedWorkflow",
                    "runId": run_id,
                    "toolCallId": format!("launch-{command_id}"),
                }))
            }
            "resumeWorkflowRun" => {
                let run_id = required_string(payload, "workId")?;
                ensure_owned_workflow_run(&session, &run_id)?;
                let result = self
                    .workflow_call(
                        ctx.clone(),
                        "resume",
                        json!({
                            "runId": run_id,
                            // resume/cancel/amend 也必须携带父身份；WorkflowHost 不能
                            // 只凭 runId 接受一个跨 Session 的控制请求。
                            "parentSessionId": session_id,
                            "workspacePath": workspace_path,
                            "cwd": root,
                            "projectStorage": root,
                        }),
                    )
                    .await?;
                let _ = workflow_result_string(&result, "runId", "run_id")?;
                // resumeWorkflowRun 的 shared schema 没有 result discriminant；accepted ACK
                // 只表示 Host 已写入 run-resumed 并启动真实执行。
                Ok(Value::Null)
            }
            "cancelBackgroundWork" => {
                let run_id = required_string(payload, "workId")?;
                ensure_owned_workflow_run(&session, &run_id)?;
                let result = self
                    .workflow_call(
                        ctx.clone(),
                        "cancel",
                        json!({
                            "runId": run_id,
                            "parentSessionId": session_id,
                            "workspacePath": workspace_path,
                            "cwd": root,
                            "projectStorage": root,
                        }),
                    )
                    .await?;
                if result.get("cancelled").and_then(Value::as_bool) != Some(true) {
                    return Err(RpcError::new(
                        "fault.command.backgroundWorkCancelRejected.not_running",
                        "WorkflowHost 没有取消正在运行的工作流",
                    ));
                }
                Ok(Value::Null)
            }
            "amendWorkflowRunSettings" => {
                let run_id = required_string(payload, "workId")?;
                ensure_owned_workflow_run(&session, &run_id)?;
                let mut args = serde_json::Map::new();
                args.insert("runId".to_owned(), Value::String(run_id.clone()));
                if let Some(model) = payload.get("subagentModel") {
                    let valid = model.is_null()
                        || model
                            .as_str()
                            .is_some_and(|value| !value.is_empty() && value.chars().count() <= 256);
                    if !valid {
                        return Err(invalid_params(
                            "amendWorkflowRunSettings.subagentModel 超出 schema 限制",
                        ));
                    }
                    args.insert("subagentModel".to_owned(), model.clone());
                }
                if let Some(concurrency) = payload.get("maxConcurrency") {
                    let valid = concurrency.is_null()
                        || concurrency
                            .as_u64()
                            .is_some_and(|value| (1..=1_024).contains(&value));
                    if !valid {
                        return Err(invalid_params(
                            "amendWorkflowRunSettings.maxConcurrency 超出 schema 限制",
                        ));
                    }
                    args.insert("maxConcurrency".to_owned(), concurrency.clone());
                }
                let has_settings = args.len() > 1;
                let frozen_model_selection = match payload.get("subagentModel") {
                    Some(Value::String(reference)) => {
                        runtime_workflow_model_selection_for_reference(
                            &runtime, &session, session_id, reference,
                        )?
                    }
                    Some(Value::Null) => {
                        // 显式清除覆盖时，Source 语义是绑定当前父 Session 模型。
                        runtime_workflow_model_selection(&runtime, &session, session_id)?
                    }
                    Some(_) => unreachable!("subagentModel 已在上方完成 schema 校验"),
                    None => {
                        // 只改并发或重发 ACK 时必须沿用 predecessor 的 models，不能被父
                        // Session 热切换后的当前 Provider 偷换。
                        frozen_workflow_model_selection(&session, &run_id)?
                    }
                };
                args.insert("parentSessionId".to_owned(), json!(session_id));
                args.insert("workspacePath".to_owned(), json!(workspace_path));
                args.insert("cwd".to_owned(), json!(root));
                args.insert("projectStorage".to_owned(), json!(root));
                args.insert("frozenModelSelection".to_owned(), frozen_model_selection);
                args.insert(
                    "toolCallId".to_owned(),
                    json!(format!("settings-{command_id}")),
                );
                args.insert("launchInputId".to_owned(), json!(command_id));
                if !has_settings {
                    return Err(RpcError::new(
                        "fault.command.workflowRunSettingsRejected.unchanged",
                        "没有提供任何需要修改的工作流设置",
                    ));
                }
                let result = self
                    .workflow_call(ctx.clone(), "amendSettings", Value::Object(args))
                    .await?;
                let amended_run_id = workflow_result_string(&result, "runId", "run_id")?;
                let tool_call_id = workflow_result_string(&result, "toolCallId", "tool_call_id")?;
                let mut output = json!({
                    "type": "amendWorkflowRunSettings",
                    "runId": amended_run_id,
                    "toolCallId": tool_call_id,
                });
                if let Some(previous) = result
                    .get("supersededRunId")
                    .or_else(|| result.get("superseded_run_id"))
                    .and_then(Value::as_str)
                {
                    output["supersededRunId"] = Value::String(previous.to_owned());
                }
                Ok(output)
            }
            _ => Err(unknown_method(AGENT_CHANNEL, command_type)),
        }
    }

    fn stop_session(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
    ) -> Result<(), RpcError> {
        let runtime = authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
        let session =
            open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
        let turn_ids = session
            .active_turn_ids()
            .map_err(|error| backend_error(error.to_string()))?;
        if turn_ids.is_empty() {
            return Err(RpcError::new(
                "fault.command.stop.notRunning",
                "当前 Session 没有可停止的运行",
            ));
        }
        for turn_id in turn_ids {
            runtime
                .cancel_turn(session_id, turn_id.as_str())
                .map_err(|error| backend_error(error.to_string()))?;
        }
        Ok(())
    }

    async fn delete_session(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
    ) -> Result<(), RpcError> {
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, session_id, workspace_path)
            .map_err(backend_error)?;
        let workspace_root = authorize_workspace_scope(&ctx.app, workspace_path)?;
        // 删除必须与 zcode-task 共用 Journal membership 负向记录；不能在此处再
        // 物理删除 Session，否则 V4、legacy task 和 window-controller 会出现不同事实。
        task_extras::call(
            ctx,
            "deleteTask",
            &json!({
                "sessionId": session_id,
                "workspacePath": workspace_path,
            }),
        )
        .await?
        .ok_or_else(|| RpcError::new("fault.backend", "任务删除 helper 未安装"))?;
        self.inner.remove_session_subscriptions(session_id);
        self.inner.emit_workspace_task_event(
            &ctx.app,
            &runtime,
            &workspace_root.to_string_lossy(),
            session_id,
            "task_deleted",
        );
        self.inner
            .forget_task_created_announcement(&workspace_root.to_string_lossy(), session_id);
        Ok(())
    }

    async fn fork_session(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        through_turn_id: Option<&str>,
    ) -> Result<String, RpcError> {
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, session_id, workspace_path)
            .map_err(backend_error)?;
        self.inner.remove_session_subscriptions(session_id);
        let context = close_session_for_mutation(&runtime, &ctx.app, session_id)
            .await
            .map_err(backend_error)?;
        let request = keencode_resources::SessionForkRequest {
            source_session_id: keencode_resources::SessionId::new(session_id)
                .map_err(|error| backend_error(error.to_string()))?,
            operation_id: operation_id.to_owned(),
            title: None,
            through_turn_id: through_turn_id
                .map(|value| {
                    keencode_resources::TurnId::new(value)
                        .map_err(|error| backend_error(error.to_string()))
                })
                .transpose()?,
        };
        let result = runtime.runtime_manager().fork_closed_session(request);
        let restore = restore_session_after_mutation(&runtime, session_id, &context);
        let result = result.map_err(|error| backend_error(error.to_string()))?;
        restore.map_err(backend_error)?;
        let _ = runtime
            .open_or_create_session(
                Path::new(workspace_path),
                Some(result.session_id.as_str()),
                "zcode-fork-open",
            )
            .map_err(|error| backend_error(error.to_string()))?;
        Ok(result.session_id.as_str().to_owned())
    }

    async fn edit_user(
        &self,
        ctx: &GatewayContext,
        session_id: &str,
        workspace_path: &str,
        operation_id: &str,
        target_message_id: &str,
        new_text: &str,
    ) -> Result<(), RpcError> {
        let runtime = owned_runtime(&ctx.app)?;
        projection::validate_workspace(&runtime, session_id, workspace_path)
            .map_err(backend_error)?;
        let expected_text =
            find_message_text(&ctx.app, session_id, workspace_path, target_message_id)?;
        if new_text.trim().is_empty() {
            return Err(invalid_params("newText 不能为空"));
        }
        self.inner.remove_session_subscriptions(session_id);
        let context = close_session_for_mutation(&runtime, &ctx.app, session_id)
            .await
            .map_err(backend_error)?;
        let request = keencode_resources::SessionEditUserRequest {
            source_session_id: keencode_resources::SessionId::new(session_id)
                .map_err(|error| backend_error(error.to_string()))?,
            target_message_id: target_message_id.to_owned(),
            expected_text,
            operation_id: operation_id.to_owned(),
        };
        let result = runtime
            .runtime_manager()
            .prepare_edit_user_closed_session(request);
        let restore = restore_session_after_mutation(&runtime, session_id, &context);
        let _archived = result.map_err(|error| backend_error(error.to_string()))?;
        restore.map_err(backend_error)?;
        let _ = runtime
            .open_or_create_session(
                Path::new(workspace_path),
                Some(session_id),
                "zcode-edit-open",
            )
            .map_err(|error| backend_error(error.to_string()))?;
        Ok(())
    }

    async fn call_session_service(
        &self,
        ctx: &GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        if matches!(
            method,
            "setModel"
                | "setThoughtLevel"
                | "setMode"
                | "promoteDeferredDraftSession"
                | "closeSession"
                | "closeDeferredDraftSession"
        ) && args
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(projection::is_virtual_agent_view_id)
        {
            return Err(RpcError::new(
                "agent.readOnly",
                "core subagent view 只支持读取和订阅，不能修改父 Session",
            ));
        }
        match method {
            "initializeWorkspace" => {
                let workspace_path = required_workspace_path(&args)?;
                let workspace_path = authorize_workspace_scope(&ctx.app, &workspace_path)?
                    .to_string_lossy()
                    .into_owned();
                Ok(json!({
                    "available": true,
                    "workspaceKey": workspace_identity(&args, &workspace_path)?,
                    "protocolName": "keencode",
                    "protocolVersion": protocol::WIRE_PROTOCOL_VERSION,
                    "transportKind": "stdio",
                }))
            }
            "getWorkspaceRuntimeIdentity" => {
                let workspace_path = required_workspace_path(&args)?;
                let workspace_path = authorize_workspace_scope(&ctx.app, &workspace_path)?
                    .to_string_lossy()
                    .into_owned();
                let generation = self.inner.connection_generation(&ctx.connection_id)?;
                Ok(json!({
                    "generation": generation,
                    "identity": log_epoch("workspace", &workspace_identity(&args, &workspace_path)?, &workspace_path),
                    "workspaceKey": workspace_identity(&args, &workspace_path)?,
                }))
            }
            "readWorkspacePresentation" => {
                let workspace_path = required_workspace_path(&args)?;
                let workspace_path = authorize_workspace_scope(&ctx.app, &workspace_path)?
                    .to_string_lossy()
                    .into_owned();
                let workspace = workspace_ref(&args, &workspace_path)?;
                Ok(projection::workspace_presentation(workspace))
            }
            "createSession" => self.create_legacy_session(ctx, &args).await,
            "resumeSession" => self.resume_legacy_session(ctx, &args).await,
            "listSessions" => self.list_legacy_sessions(ctx, &args),
            "readSession" => self.read_legacy_session(ctx, &args),
            "readSessionMessages" => self.read_session_messages(ctx, &args),
            "readSessionEvents" => self.read_session_events(ctx, &args),
            "setModel" => self.set_legacy_model(ctx, &args),
            "setThoughtLevel" => self.set_legacy_effort(ctx, &args),
            "setMode" => self.set_legacy_mode(ctx, &args),
            "promoteDeferredDraftSession" => {
                let id = required_string(&args, "sessionId")?;
                let path = required_workspace_path(&args)?;
                let runtime = owned_runtime(&ctx.app)?;
                projection::validate_workspace(&runtime, &id, &path).map_err(backend_error)?;
                let root = authorize_workspace_scope(&ctx.app, &path)?;
                runtime
                    .promote_deferred_session(&id)
                    .map_err(|error| backend_error(error.to_string()))?;
                // 目标 Runtime 没有 Node 进程里的 deferred registry；提升认领只协调
                // 当前连接的 cleanup 与发送竞态，正文仍由同一权威 Session Journal 承载。
                let _ = runtime
                    .open_or_create_session_serialized(&root, Some(&id), "deferred-promote-open")
                    .await
                    .map_err(|error| backend_error(error.to_string()))?;
                prepare_session_reference_catalog(&ctx.app, &runtime, &id, &root).await?;
                Ok(Value::Null)
            }
            "closeSession" | "closeDeferredDraftSession" => {
                let id = required_string(&args, "sessionId")?;
                let path = required_workspace_path(&args)?;
                let runtime = owned_runtime(&ctx.app)?;
                projection::validate_workspace(&runtime, &id, &path).map_err(backend_error)?;
                let root = authorize_workspace_scope(&ctx.app, &path)?;
                let deferred_close_claim = if method == "closeDeferredDraftSession" {
                    if !runtime
                        .begin_deferred_session_close(&id)
                        .map_err(|error| backend_error(error.to_string()))?
                    {
                        return Ok(Value::Bool(false));
                    }
                    true
                } else {
                    false
                };
                let session = match runtime
                    .open_or_create_session_serialized(&root, Some(&id), "deferred-close-open")
                    .await
                {
                    Ok(session) => session,
                    Err(error) => {
                        if deferred_close_claim {
                            let _ = runtime.cancel_deferred_session_close(&id);
                        }
                        return Err(backend_error(error));
                    }
                };
                if method == "closeDeferredDraftSession" {
                    // 条件关闭只能收口尚未产生用户事实的预热草稿。已提升或已经
                    // 产生 Turn/Transcript 的 Session 必须保留，避免旧 UI 的异步
                    // cleanup 把刚开始发送的正式任务关闭。
                    let snapshot = session.snapshot().map_err(backend_error)?;
                    let (_, queue) = session.input_queue_state().map_err(backend_error)?;
                    let empty_draft = session.transcript().map_err(backend_error)?.is_empty()
                        && snapshot.state.turns.is_empty()
                        && queue.items.is_empty()
                        && session.active_turn_ids().map_err(backend_error)?.is_empty()
                        && !runtime
                            .session_has_active_work(&id)
                            .map_err(|error| backend_error(error.to_string()))?;
                    if !empty_draft {
                        runtime
                            .cancel_deferred_session_close(&id)
                            .map_err(|error| backend_error(error.to_string()))?;
                        return Ok(Value::Bool(false));
                    }
                }
                self.inner.remove_session_subscriptions(&id);
                if let Err(error) = runtime.close_session(&id).await {
                    if deferred_close_claim {
                        let _ = runtime.cancel_deferred_session_close(&id);
                    }
                    return Err(backend_error(error.to_string()));
                }
                if method == "closeDeferredDraftSession" {
                    Ok(Value::Bool(true))
                } else {
                    Ok(Value::Null)
                }
            }
            _ => Err(unknown_method(SESSION_CHANNEL, method)),
        }
    }

    async fn create_legacy_session(
        &self,
        ctx: &GatewayContext,
        args: &Value,
    ) -> Result<Value, RpcError> {
        let path = required_workspace_path(args)?;
        let runtime = owned_runtime(&ctx.app)?;
        let root = authorize_workspace_scope(&ctx.app, &path)?;
        let operation_id = optional_string(args, "sessionTraceId")?
            .unwrap_or_else(|| legacy_operation_id(args, "legacy-create"));
        let requested_id = optional_string(args, "sessionId")?;
        let session = runtime
            .open_or_create_session_serialized(&root, requested_id.as_deref(), &operation_id)
            .await
            .map_err(|error| backend_error(error.to_string()))?;
        let id = session.session_id().as_str().to_owned();
        prepare_session_reference_catalog(&ctx.app, &runtime, &id, &root).await?;
        if let Some(model) = args.get("model") {
            apply_model_value(&runtime, &id, &operation_id, model)?;
        }
        if let Some(thought) = args.get("thoughtLevel").and_then(Value::as_str) {
            runtime
                .set_session_effort(&id, &operation_id, thought)
                .map_err(|error| backend_error(error.to_string()))?;
        }
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    async fn resume_legacy_session(
        &self,
        ctx: &GatewayContext,
        args: &Value,
    ) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        let operation_id = legacy_operation_id(args, "legacy-resume");
        let runtime =
            authorized_runtime_session_serialized(&ctx.app, &id, &path, &operation_id).await?;
        if let Some(model) = args.get("model") {
            let operation_id = legacy_operation_id(args, "legacy-resume-model");
            apply_model_value(&runtime, &id, &operation_id, model)?;
        }
        if let Some(thought) = args.get("thoughtLevel").and_then(Value::as_str) {
            let operation_id = legacy_operation_id(args, "legacy-resume-effort");
            runtime
                .set_session_effort(&id, &operation_id, thought)
                .map_err(|error| backend_error(error.to_string()))?;
        }
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    fn list_legacy_sessions(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let path = required_workspace_path(args)?;
        let path = authorize_workspace_scope(&ctx.app, &path)?
            .to_string_lossy()
            .into_owned();
        let runtime = owned_runtime(&ctx.app)?;
        let workspace_id = workspace_identity(args, &path)?;
        projection::sessions_index_snapshot(
            &runtime,
            &path,
            &workspace_id,
            &log_epoch("sessions-index", &workspace_id, &path),
        )
        .map(|snapshot| {
            snapshot
                .get("sessions")
                .cloned()
                .unwrap_or_else(|| json!([]))
        })
        .map_err(backend_error)
    }

    /// 从 Runtime Session Journal 生成旧 task-index 所需的任务行。
    ///
    /// ZCode 的本地实现没有第二份 SQLite tasks 表；置顶、归档、标题、模型和时间均
    /// 直接读取同一份 Session 元数据/快照。对尚未在进程内打开的历史 Session，
    /// `open_authorized_session` 会按授权项目根恢复真实 Journal，因此这里不会用空数组
    /// 或前端缓存掩盖读取失败。
    fn list_task_meta_values(
        &self,
        ctx: &GatewayContext,
        args: &Value,
        filter: impl Fn(&keencode_runtime::StoredSessionMetadata) -> bool,
    ) -> Result<Value, RpcError> {
        let path = required_workspace_path(args)?;
        let workspace_identity = optional_string(args, "workspaceIdentity")?;
        let root = authorize_stored_root(&ctx.app, &path).map_err(backend_error)?;
        let root_text = root.to_string_lossy().into_owned();
        let runtime = owned_runtime(&ctx.app)?;
        let metadata = runtime
            .stored_sessions_for_project(Some(&root_text))
            .map_err(backend_error)?;
        let mut tasks = Vec::with_capacity(metadata.len());
        for item in metadata
            .into_iter()
            .filter(|item| !item.corrupt && filter(item))
        {
            if let Some(task) = task_meta_for_stored_session(
                &runtime,
                &ctx.app,
                &item,
                workspace_identity.as_deref(),
            )? {
                tasks.push(task);
            }
        }
        Ok(Value::Array(tasks))
    }

    /// 返回全局 pinned ID 集合；ID 仍来自本地 Session Journal 的稳定身份。
    fn list_pinned_task_ids(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        if !args.is_object() && !args.is_null() {
            return Err(invalid_params("listPinnedTaskIds 参数必须为空或对象"));
        }
        let runtime = owned_runtime(&ctx.app)?;
        let metadata = runtime.stored_sessions().map_err(backend_error)?;
        let mut ids = Vec::new();
        for item in metadata
            .into_iter()
            .filter(|item| !item.corrupt && item.pinned && !item.archived)
        {
            // 全局 pinned 查询会看到 Runtime 存储中历史项目的 locator；当前窗口只
            // 能展示已登记且已授权的项目。跳过未授权 locator，不把跨项目隔离误报成
            // 当前项目的后端故障；已授权项目仍通过完整 Journal 投影校验。
            if authorize_stored_root(&ctx.app, &item.project_root).is_err() {
                continue;
            }
            if task_meta_for_stored_session(&runtime, &ctx.app, &item, None)?.is_some() {
                ids.push(Value::String(item.session_id.as_str().to_owned()));
            }
        }
        Ok(Value::Array(ids))
    }

    /// 按 workspace 读取 Runtime 删除 tombstone，供 sessions-index 的负向 join 使用。
    fn list_deleted_task_ids(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let path = required_workspace_path(args)?;
        let root = authorize_stored_root(&ctx.app, &path).map_err(backend_error)?;
        let runtime = owned_runtime(&ctx.app)?;
        let ids = keencode_resources::list_deleted_session_ids(
            runtime.storage_root(),
            &root.to_string_lossy(),
        )
        .map_err(backend_error)?;
        Ok(json!(
            ids.into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        ))
    }

    fn read_legacy_session(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&id)
            .map_err(backend_error)?
            .is_some()
        {
            let runtime = owned_runtime(&ctx.app)?;
            let scope = projection::resolve_conversation_scope(&runtime, &ctx.app, &id, &path)
                .map_err(backend_error)?;
            return legacy_snapshot_for_virtual_agent(&runtime, &ctx.app, &scope, &path);
        }
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    fn read_session_messages(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&id)
            .map_err(backend_error)?
            .is_some()
        {
            let runtime = owned_runtime(&ctx.app)?;
            let scope = projection::resolve_conversation_scope(&runtime, &ctx.app, &id, &path)
                .map_err(backend_error)?;
            let session = open_authorized_session(&runtime, &ctx.app, &scope.parent_session_id)
                .map_err(backend_error)?;
            let agent_id = scope.agent_id.as_ref().expect("virtual scope has agent");
            let state = session.snapshot().map_err(backend_error)?.state;
            let mut messages = state
                .effective_transcript(agent_id)
                .map_err(backend_error)?
                .into_iter()
                .map(|message| {
                    serde_json::to_value(message).map_err(|error| backend_error(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let after = optional_string(args, "afterMessageId")?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 500) as usize;
            if let Some(after) = after
                && let Some(index) = messages.iter().position(|message| {
                    message.get("messageId").and_then(Value::as_str) == Some(after.as_str())
                })
            {
                messages = messages.into_iter().skip(index + 1).collect();
            }
            messages.truncate(limit);
            return Ok(Value::Array(messages));
        }
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        let session = open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
        let after = optional_string(args, "afterMessageId")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, 500) as usize;
        let mut messages = session
            .transcript()
            .map_err(backend_error)?
            .into_iter()
            .map(|message| {
                serde_json::to_value(message).map_err(|error| backend_error(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(after) = after
            && let Some(index) = messages.iter().position(|message| {
                message.get("messageId").and_then(Value::as_str) == Some(after.as_str())
            })
        {
            messages = messages.into_iter().skip(index + 1).collect();
        }
        messages.truncate(limit);
        Ok(Value::Array(messages))
    }

    fn read_session_events(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        if projection::parse_virtual_agent_view_id(&id)
            .map_err(backend_error)?
            .is_some()
        {
            return Err(RpcError::new(
                "agent.readOnly",
                "core subagent view 只支持消息读取和订阅，不暴露父 Session Journal 事件",
            ));
        }
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        let session = open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
        let after = args.get("afterSeq").and_then(Value::as_u64);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, 500) as usize;
        let page = session.replay(after, limit).map_err(backend_error)?;
        let records = page
            .records
            .into_iter()
            .map(|record| {
                serde_json::to_value(record).map_err(|error| backend_error(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::Array(records))
    }

    fn set_legacy_model(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        let model = args
            .get("model")
            .ok_or_else(|| invalid_params("model 缺失"))?;
        let operation_id = legacy_operation_id(args, "legacy-set-model");
        apply_model_value(&runtime, &id, &operation_id, model)?;
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    fn set_legacy_effort(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        let effort = optional_string(args, "thoughtLevel")?.unwrap_or_default();
        let operation_id = legacy_operation_id(args, "legacy-set-effort");
        runtime
            .set_session_effort(&id, &operation_id, &effort)
            .map_err(|error| backend_error(error.to_string()))?;
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    fn set_legacy_mode(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let id = required_string(args, "sessionId")?;
        let path = required_workspace_path(args)?;
        let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
        let mode = required_string(args, "mode")?;
        let operation_id = legacy_operation_id(args, "legacy-set-mode");
        let session = open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
        let previous = session
            .snapshot()
            .map_err(backend_error)?
            .state
            .plan
            .plan_artifact;
        session
            .set_plan(
                &operation_id,
                keencode_resources::PlanState {
                    enabled: mode == "plan",
                    plan_artifact: previous,
                },
            )
            .map_err(|error| backend_error(error.to_string()))?;
        runtime
            .set_permission_mode_from_wire(&id, Some(&mode))
            .map_err(|error| backend_error(error.to_string()))?;
        legacy_snapshot(&runtime, &ctx.app, &id, &path)
    }

    async fn call_task_service(
        &self,
        ctx: &GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        if let Some(result) = task_extras::call(ctx, method, &args).await? {
            return Ok(result);
        }
        match method {
            "sendPrompt" | "sendText" | "send" => {
                let id = required_string(&args, "sessionId")
                    .or_else(|_| required_string(&args, "taskId"))?;
                let path = required_workspace_path(&args)?;
                let text = optional_string(&args, "text")?
                    .or_else(|| optional_string(&args, "prompt").ok().flatten())
                    .ok_or_else(|| invalid_params("text/prompt 缺失"))?;
                self.send_text(
                    ctx,
                    &id,
                    &path,
                    &legacy_operation_id(&args, "legacy-task-send"),
                    &json!({"text": text}),
                )
                .await
            }
            "stop" | "cancel" | "cancelTaskCommand" => {
                let id = required_string(&args, "sessionId")
                    .or_else(|_| required_string(&args, "taskId"))?;
                let path = required_workspace_path(&args)?;
                self.stop_session(ctx, &id, &path)?;
                Ok(json!({"canceled": true}))
            }
            "listTasks" => {
                self.list_task_meta_values(ctx, &args, |item| !item.pinned && !item.archived)
            }
            "listSessions" => self.list_legacy_sessions(ctx, &args),
            "listPinnedTaskIds" => self.list_pinned_task_ids(ctx, &args),
            "listPinnedTasks" => {
                self.list_task_meta_values(ctx, &args, |item| item.pinned && !item.archived)
            }
            "listArchivedTasks" => self.list_task_meta_values(ctx, &args, |item| item.archived),
            "listDeletedTaskIds" => self.list_deleted_task_ids(ctx, &args),
            "releaseWorkspacePreparation" => {
                self.release_workspace_resources(ctx, &args, false).await
            }
            "restartWorkspaceProcess" => self.release_workspace_resources(ctx, &args, true).await,
            "readTask" | "getTask" | "readSession" => self.read_legacy_session(ctx, &args),
            "renameTask" | "rename" => {
                let id = required_string(&args, "sessionId")
                    .or_else(|_| required_string(&args, "taskId"))?;
                let path = required_workspace_path(&args)?;
                let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
                let session =
                    open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
                let title =
                    required_string(&args, "title").or_else(|_| required_string(&args, "name"))?;
                session
                    .rename(
                        &legacy_operation_id(&args, "legacy-task-rename"),
                        title,
                        Some(keencode_resources::TitleSource::Manual),
                    )
                    .map_err(|error| backend_error(error.to_string()))?;
                let root = authorize_workspace_scope(&ctx.app, &path)?;
                self.inner.emit_workspace_task_event(
                    &ctx.app,
                    &runtime,
                    &root.to_string_lossy(),
                    &id,
                    "task_meta_changed",
                );
                // Source 的 IZCodeTaskService.renameTask Promise 类型是 ZCodeTaskMeta；
                // 事件只负责刷新其他投影，不能用 null 代替调用方立即需要的完整 ACK。
                let metadata = runtime
                    .runtime_manager()
                    .stored_session_metadata(&id)
                    .map_err(backend_error)?;
                let state = session.snapshot().map_err(backend_error)?.state;
                let workspace_identity = optional_string(&args, "workspaceIdentity")?;
                let unread_at = task_extras::unread_at_for_task(
                    &ctx.app,
                    &state.project_root,
                    workspace_identity.as_deref(),
                    &id,
                )?;
                task_extras::task_meta_value(
                    &runtime,
                    &metadata,
                    &state,
                    workspace_identity.as_deref(),
                    unread_at,
                )
            }
            "deleteTask" | "deleteSession" => {
                let id = required_string(&args, "sessionId")
                    .or_else(|_| required_string(&args, "taskId"))?;
                let path = required_workspace_path(&args)?;
                self.delete_session(ctx, &id, &path).await?;
                Ok(Value::Null)
            }
            "setTaskMeta" | "mutateTask" => {
                let id = required_string(&args, "sessionId")
                    .or_else(|_| required_string(&args, "taskId"))?;
                let path = required_workspace_path(&args)?;
                let runtime = authorized_runtime_session(&ctx.app, &id, &path)?;
                let session =
                    open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
                let pinned = args.get("pinned").and_then(Value::as_bool);
                let archived = args.get("archived").and_then(Value::as_bool);
                session
                    .set_preference(
                        &legacy_operation_id(&args, "legacy-task-meta"),
                        pinned,
                        archived,
                    )
                    .map_err(|error| backend_error(error.to_string()))?;
                let root = authorize_workspace_scope(&ctx.app, &path)?;
                self.inner.emit_workspace_task_event(
                    &ctx.app,
                    &runtime,
                    &root.to_string_lossy(),
                    &id,
                    "task_meta_changed",
                );
                Ok(Value::Null)
            }
            "compactSession" | "goalSession" | "answerPermission" | "answerUserInput" => {
                Err(unsupported_command(
                    method,
                    "该操作需要 Runtime 的专用控制面，当前没有安全的旁路实现",
                ))
            }
            _ => Err(unknown_method(TASK_CHANNEL, method)),
        }
    }

    /// 释放当前 workspace 的真实 Session 资源。
    ///
    /// 自研 Runtime 没有 Node 子进程可 dispose；释放边界是关闭 Manager 中的
    /// Session 句柄并保留 Journal。只有无活动工作且没有用户 Transcript 的空预热
    /// Session 会被普通 release 收口；restart 还会收口空闲历史句柄，再按
    /// `resumeTaskId` 重新登记目标 Session。任何有活动 Turn/工具的任务都保留，
    /// 避免把“重启”伪装成已取消而丢失真实执行状态。
    async fn release_workspace_resources(
        &self,
        ctx: &GatewayContext,
        args: &Value,
        restart: bool,
    ) -> Result<Value, RpcError> {
        let path = required_workspace_path(args)?;
        let workspace_root = authorize_workspace_scope(&ctx.app, &path)?;
        let workspace_path = workspace_root.to_string_lossy().into_owned();
        let runtime = owned_runtime(&ctx.app)?;
        let resume_task_id = optional_string(args, "resumeTaskId")?;
        if let Some(session_id) = resume_task_id.as_deref() {
            projection::validate_workspace(&runtime, session_id, &workspace_path)
                .map_err(backend_error)?;
        }

        let stored = runtime.stored_sessions().map_err(backend_error)?;
        for metadata in stored {
            let Ok(metadata_root) =
                crate::workspace::canonical_session_root(&metadata.project_root)
            else {
                continue;
            };
            if metadata_root != workspace_root
                || resume_task_id.as_deref() == Some(metadata.session_id.as_str())
            {
                continue;
            }
            let session = match runtime
                .open_or_create_session_serialized(
                    &workspace_root,
                    Some(metadata.session_id.as_str()),
                    if restart {
                        "workspace-restart-open"
                    } else {
                        "workspace-release-open"
                    },
                )
                .await
            {
                Ok(session) => session,
                Err(error) => {
                    return Err(backend_error(error.to_string()));
                }
            };
            if session.is_workflow_actor().map_err(backend_error)?
                || runtime
                    .session_has_active_work(metadata.session_id.as_str())
                    .map_err(|error| backend_error(error.to_string()))?
            {
                continue;
            }
            let transcript_empty = session.transcript().map_err(backend_error)?.is_empty();
            let (_, queue) = session.input_queue_state().map_err(backend_error)?;
            let has_turns = !session
                .snapshot()
                .map_err(backend_error)?
                .state
                .turns
                .is_empty();
            if !restart && (!transcript_empty || has_turns || !queue.items.is_empty()) {
                continue;
            }
            // deferred promotion/首次发送已经认领的 Session 不能被 workspace 释放
            // 误收口；空闲历史只有在这里取得独占 cleanup 认领后才加入关闭批次。
            if !runtime
                .begin_deferred_session_close(metadata.session_id.as_str())
                .map_err(|error| backend_error(error.to_string()))?
            {
                continue;
            }
            // 一次只认领并关闭一个 Session；若关闭失败，立即释放当前认领，
            // 后续项尚未进入 Closing，下一次 release 可以继续安全重试。
            let session_id = metadata.session_id.as_str();
            drop(session);
            self.inner.remove_session_subscriptions(session_id);
            if let Err(error) = runtime.close_session(session_id).await {
                let _ = runtime.cancel_deferred_session_close(session_id);
                return Err(backend_error(error.to_string()));
            }
        }
        if let Some(session_id) = resume_task_id {
            let _ = runtime
                .open_or_create_session_serialized(
                    &workspace_root,
                    Some(&session_id),
                    "workspace-resume",
                )
                .await
                .map_err(|error| backend_error(error.to_string()))?;
        }
        Ok(Value::Null)
    }

    async fn call_window_controller(
        &self,
        ctx: &GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        match method {
            "listTaskList" | "listTasks" => self.list_legacy_sessions(ctx, &args),
            "deleteArchivedTask" => self.call_task_service(ctx, method, args).await,
            "deleteArchivedTasks" => self.call_task_service(ctx, method, args).await,
            "mutateTask" => self.call_task_service(ctx, method, args).await,
            _ => Err(unknown_method(WINDOW_CHANNEL, method)),
        }
    }
}

/// 为 lifecycle observer 生成当前桌面 Runtime 的 workspace 绑定。
///
/// 自研 Runtime 与 ZCode CLI 一样按 workspace 发送事件，但没有独立子进程可报告；
/// generation=1 表示当前桌面进程代次，关闭桌面连接只释放 observer，不伪造
/// unavailable 状态。指定 workspace 时即使还没有 Session 也返回 available，保证
/// sessions-index observer 可以在首个 Session 创建前建立订阅。
fn runtime_lifecycle_events(ctx: &GatewayContext, args: &Value) -> Result<Vec<Value>, RpcError> {
    let runtime = owned_runtime(&ctx.app)?;
    let requested_path = optional_string(args, "workspacePath")?;
    let requested_identity =
        optional_string(args, "workspaceIdentity")?.filter(|identity| !identity.is_empty());
    let mut workspaces = Vec::new();
    if let Some(path) = requested_path {
        let root = authorize_stored_root(&ctx.app, &path).map_err(backend_error)?;
        workspaces.push((root.to_string_lossy().into_owned(), requested_identity));
    } else {
        for metadata in runtime.stored_sessions().map_err(backend_error)? {
            let root =
                authorize_stored_root(&ctx.app, &metadata.project_root).map_err(backend_error)?;
            let root = root.to_string_lossy().into_owned();
            if !workspaces.iter().any(|(path, _)| path == &root) {
                workspaces.push((root, None));
            }
        }
    }
    workspaces
        .into_iter()
        .map(|(workspace_path, workspace_identity)| {
            let internal_workspace_key = workspace_identity
                .clone()
                .unwrap_or_else(|| workspace_path.clone());
            let wire_workspace_key = workspace_identity
                .clone()
                .unwrap_or_else(|| path_to_frontend(Path::new(&workspace_path)));
            let runtime_identity = log_epoch("runtime", &internal_workspace_key, &workspace_path);
            let mut event = json!({
                // lifecycle 的路径和 workspaceKey 都是 wire 值；runtime identity
                // 仍用 canonical 路径计算，避免改变 Runtime 日志身份。
                "workspacePath": path_to_frontend(Path::new(&workspace_path)),
                "workspaceKey": wire_workspace_key.clone(),
                "runtimeIdentity": {
                    "generation": 1,
                    "identity": runtime_identity,
                    "workspaceKey": wire_workspace_key,
                },
                "state": "available",
            });
            if let Some(identity) = workspace_identity {
                event["workspaceIdentity"] = Value::String(identity);
            }
            Ok(event)
        })
        .collect()
}

/// 将一个健康的 Runtime Session 投影成 ZCode task meta；workflow actor 不属于用户任务。
fn task_meta_for_stored_session(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    metadata: &keencode_runtime::StoredSessionMetadata,
    workspace_identity: Option<&str>,
) -> Result<Option<Value>, RpcError> {
    let session = open_authorized_session(runtime, app, metadata.session_id.as_str())
        .map_err(backend_error)?;
    if session.is_workflow_actor().map_err(backend_error)? {
        return Ok(None);
    }
    let snapshot = session.snapshot().map_err(backend_error)?;
    let state = snapshot.state;
    // SessionCreated 只支持 workspace 预热；只有同一份 Journal 已产生用户输入、
    // Turn 或队列事实时才提升为 tasks-index 行，避免冷恢复重复制造空 draft。
    if !projection::is_promoted_task_state(&state) {
        return Ok(None);
    }
    // 旧 task-index 也必须读取同一份权限协调器状态；Plan artifact 只表示只读计划，
    // 不能把 edit/yolo 会话降级成 build。冷恢复时 Runtime 会按其权威恢复规则返回 build。
    let permission_mode = runtime
        .permission_mode(metadata.session_id.as_str())
        .map_err(|error| backend_error(error.to_string()))?;
    let mode = if state.plan.enabled {
        "plan"
    } else {
        projection::permission_mode_wire(permission_mode)
    };
    let active = runtime
        .session_has_active_work(metadata.session_id.as_str())
        .map_err(backend_error)?;
    // Journal 内部路径可能带 Windows extended-length 前缀，task-index 返回值必须
    // 与 Source 保存的项目路径保持同一 wire 表示，内部未读 key 仍使用原路径。
    let workspace_path = path_to_frontend(Path::new(&state.project_root));
    let mut task = json!({
        "taskId": metadata.session_id.as_str(),
        "traceId": format!("session:{}", metadata.session_id.as_str()),
        "title": state.title,
        "workspacePath": workspace_path,
        "createdAt": state.created_at_unix_ms,
        "updatedAt": state.updated_at_unix_ms,
        "mode": mode,
        "provider": "glm",
    });
    if let Some(identity) = workspace_identity.filter(|value| !value.is_empty()) {
        task["workspaceIdentity"] = Value::String(identity.to_owned());
    }
    // task-groups.json 是未读唯一持久事实源；readSession 快照 schema 保持冻结，
    // 这里只把同一值投影到 task meta，避免在前端或 Session snapshot 建第二份状态。
    if let Some(unread_at) = task_extras::unread_at_for_task(
        app,
        &state.project_root,
        workspace_identity,
        metadata.session_id.as_str(),
    )? {
        task["unreadAt"] = Value::Number(unread_at.into());
    }
    if state.title_source == TitleSource::Manual {
        task["titleOverridden"] = Value::Bool(true);
    }
    if let Some(provider) = state.provider {
        task["model"] = Value::String(provider.model);
        if let Some(effort) = provider.reasoning_effort
            && let Ok(value) = serde_json::to_value(effort)
            && let Some(value) = value.as_str()
        {
            task["thoughtLevel"] = Value::String(value.to_owned());
        }
    }
    if !state.turns.is_empty() {
        let failed = state
            .turns
            .values()
            .any(|turn| turn.status == TurnStatus::Failed);
        let running = active
            || matches!(
                state.status,
                SessionStatus::Running | SessionStatus::Waiting
            );
        task["status"] = Value::String(
            if running {
                "running"
            } else if failed {
                "error"
            } else {
                "completed"
            }
            .to_owned(),
        );
    }
    Ok(Some(task))
}

impl SessionGatewayInner {
    fn claim_task_created_announcement(
        &self,
        workspace_path: &str,
        task_id: &str,
    ) -> Result<bool, RpcError> {
        let mut announcements = self
            .task_created_announcements
            .lock()
            .map_err(|_| state_error())?;
        Ok(announcements.insert((workspace_path.to_owned(), task_id.to_owned())))
    }

    fn forget_task_created_announcement(&self, workspace_path: &str, task_id: &str) {
        if let Ok(mut announcements) = self.task_created_announcements.lock() {
            announcements.remove(&(workspace_path.to_owned(), task_id.to_owned()));
        }
    }

    fn client_id(&self, connection_id: &str) -> Result<String, RpcError> {
        self.connections
            .lock()
            .map_err(|_| state_error())?
            .get(connection_id)
            .and_then(|binding| binding.client_id.clone())
            .ok_or_else(|| {
                RpcError::new(
                    "fault.connection.helloRequired",
                    "必须先完成 helloConversationV4 和 initializeConversationV4",
                )
            })
    }

    fn connection_generation(&self, connection_id: &str) -> Result<u64, RpcError> {
        Ok(self
            .connections
            .lock()
            .map_err(|_| state_error())?
            .get(connection_id)
            .map(|binding| binding.generation.max(1))
            .unwrap_or(1))
    }

    fn dispose_connection(&self, connection_id: &str) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.remove(connection_id);
        }
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.retain(|_, listener| listener.connection_id != connection_id);
        }
        let subscription_ids = self
            .subscriptions
            .lock()
            .ok()
            .map(|subscriptions| {
                subscriptions
                    .iter()
                    .filter(|(_, subscription)| subscription.connection_id == connection_id)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for subscription_id in subscription_ids {
            self.remove_subscription(&subscription_id);
        }
        // command_results 只是当前进程的 ACK 查询缓存，不是幂等事实源；连接关闭时
        // 清掉它，避免旧连接的业务状态留在网关中。Session 命令的真正收据在
        // Runtime/Journal 中，重连 queryCommands 会优先读取该持久事实；全局 create
        // 命令没有可绑定的既有 Session 时才退回当前进程缓存。
        if let Ok(mut results) = self.command_results.lock() {
            results.clear();
        }
        if let Ok(mut order) = self.command_order.lock() {
            order.clear();
        }
    }

    fn remove_subscription(&self, subscription_id: &str) {
        if let Ok(mut subscriptions) = self.subscriptions.lock()
            && let Some(subscription) = subscriptions.remove(subscription_id)
        {
            remember_released_subscription(subscription_id, &subscription.connection_id);
            if let Some(abort) = subscription.abort {
                abort.abort();
            }
        }
    }

    /// 读取 unsubscribe 的 owner 校验结果。
    ///
    /// 连接清理可能先于 renderer 的迟到 close 请求完成。同一连接命中 release
    /// tombstone 时返回 `None` 表示幂等成功；活动订阅仍返回其快照，foreign owner
    /// 或未知 ID 继续返回 `notOwned`，避免把生命周期幂等误用成越权释放。
    fn subscription_for_unsubscribe(
        &self,
        subscription_id: &str,
        connection_id: &str,
    ) -> Result<Option<ActiveSubscription>, RpcError> {
        let subscription = self
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .get(subscription_id)
            .cloned();
        let Some(subscription) = subscription else {
            if released_subscription_belongs_to(subscription_id, connection_id) {
                return Ok(None);
            }
            return Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅不存在或已释放",
            ));
        };
        if subscription.connection_id != connection_id {
            return Err(RpcError::new(
                "fault.subscription.notOwned",
                "订阅不属于当前连接",
            ));
        }
        Ok(Some(subscription))
    }

    fn remove_session_subscriptions(&self, session_id: &str) {
        let ids = self
            .subscriptions
            .lock()
            .ok()
            .map(|subscriptions| {
                subscriptions
                    .iter()
                    .filter(|(_, subscription)| {
                        subscription.session_id.as_deref() == Some(session_id)
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for id in ids {
            self.remove_subscription(&id);
        }
    }

    /// 找出这次 subscribe 成功响应对应的单个待激活订阅。
    ///
    /// 只接受成功 response 的精确 `subscriptionId`；同 topic 并发重订阅时不按
    /// 候选顺序猜测，避免把首帧排放到错误请求。
    fn subscription_id_for_response(
        &self,
        connection_id: &str,
        method: &str,
        args: &Value,
        response: &Value,
    ) -> Option<String> {
        let topic = subscription_topic(method, args).ok();
        // 成功响应中的 ACK 才是请求与订阅的权威关联；请求参数可能同时对应
        // 多个尚未激活的同 topic 订阅，不能再从参数猜测目标。
        let explicit_id = response
            .pointer("/ack/subscriptionId")
            .and_then(Value::as_str)
            .or_else(|| response.get("subscriptionId").and_then(Value::as_str))?;
        let subscriptions = self.subscriptions.lock().ok()?;
        subscriptions.get(explicit_id).and_then(|subscription| {
            (subscription.connection_id == connection_id
                && !subscription.activated
                && topic.as_deref() == Some(subscription.topic.as_str()))
            .then(|| explicit_id.to_owned())
        })
    }

    fn activate_subscription(&self, subscription_id: &str) {
        let _ = self.with_frame_gate(subscription_id, || {
            let frames = {
                let Ok(mut subscriptions) = self.subscriptions.lock() else {
                    return;
                };
                let Some(subscription) = subscriptions.get_mut(subscription_id) else {
                    return;
                };
                if subscription.activated {
                    return;
                }
                subscription.activated = true;
                let mut frames = Vec::with_capacity(MAX_PENDING_ACTIVATION_FRAMES);
                if let Some(frame) = subscription.initial_frame.take() {
                    frames.push((frame, FrameDeliveryKind::Initial));
                }
                frames.extend(
                    subscription
                        .pending_frames
                        .drain(..)
                        .map(|frame| (frame, FrameDeliveryKind::Online)),
                );
                frames
            };
            for (frame, delivery_kind) in frames {
                self.emit_frame(subscription_id, frame, delivery_kind);
            }
        });
    }

    /// 在订阅级 frame gate 内执行一次投影事务。
    ///
    /// 调用方必须把 Runtime 快照的读取和后续 `queue_or_emit_frame` 放在同一个
    /// closure 中；只锁住编号步骤仍然允许旧 payload 在新 transient 之后入队。
    fn with_frame_gate<T>(
        &self,
        subscription_id: &str,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        let frame_gate = self
            .subscriptions
            .lock()
            .ok()?
            .get(subscription_id)
            .map(|subscription| Arc::clone(&subscription.frame_gate))?;
        let _frame_guard = frame_gate.lock().ok()?;
        Some(operation())
    }

    fn queue_or_emit_frame(
        &self,
        subscription_id: &str,
        frame: Value,
        delivery_kind: FrameDeliveryKind,
    ) {
        let Some(emit_now) = (|| {
            let mut subscriptions = self.subscriptions.lock().ok()?;
            let subscription = subscriptions.get_mut(subscription_id)?;
            if !subscription.activated {
                let is_snapshot =
                    frame.pointer("/payload/kind").and_then(Value::as_str) == Some("snapshot");
                if is_snapshot {
                    // Snapshot 会重新建立完整事实水位；丢弃它之前的临时 delta，
                    // 后续 ACK 前产生的 delta 仍会按序追加到这个 snapshot 后面。
                    subscription.pending_frames.clear();
                    subscription.pending_frames.push(frame);
                } else if subscription.pending_frames.len() < MAX_PENDING_ACTIVATION_FRAMES {
                    subscription.pending_frames.push(frame);
                } else {
                    // ACK 长时间未返回时保持有界。保留最新的一帧 delta，激活后
                    // forwarder 会继续提供权威 snapshot/stream；绝不让连接内存无界。
                    subscription.pending_frames.clear();
                    subscription.pending_frames.push(frame);
                }
                return Some(None);
            }
            Some(Some(frame))
        })() else {
            return;
        };
        if let Some(frame) = emit_now {
            self.emit_frame(subscription_id, frame, delivery_kind);
        }
    }

    fn emit_frame(&self, subscription_id: &str, frame: Value, delivery_kind: FrameDeliveryKind) {
        let Some((topic, workspace_path, connection_id, wire)) = (|| {
            let mut subscriptions = self.subscriptions.lock().ok()?;
            let subscription = subscriptions.get_mut(subscription_id)?;
            subscription.wire_ordinal = subscription.wire_ordinal.saturating_add(1).max(1);
            let ordinal = subscription.wire_ordinal;
            let topic = subscription.topic.clone();
            let workspace_path = subscription.workspace_path.clone();
            let connection_id = subscription.connection_id.clone();
            let wire = topic_wire_complete(&topic, subscription_id, frame, ordinal, delivery_kind);
            Some((topic, workspace_path, connection_id, wire))
        })() else {
            return;
        };
        let listeners = self
            .listeners
            .lock()
            .ok()
            .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for listener in listeners {
            if listener.connection_id != connection_id
                || listener.channel != AGENT_CHANNEL
                || listener.event
                    != if topic.starts_with("conversation/") {
                        "onDynamicConversationFrame"
                    } else if topic.starts_with("sessions-index/") {
                        "onDynamicSessionsIndexFrame"
                    } else {
                        "onDynamicWorkspaceConfigFrame"
                    }
                || !workspace_matches(listener.workspace_path.as_deref(), &workspace_path)
            {
                continue;
            }
            if let Err(error) = (listener.callback)(wire.clone()) {
                log_event_delivery_failure(listener.event.as_str(), &error);
            }
        }
    }

    /// 将 Runtime/Journal 快照对齐到当前订阅的连续投影水位。
    ///
    /// Runtime 的快照 `seq` 是 Journal 水位，而临时 delta 没有 Journal 记录；
    /// 临时 delta 之后再次读取快照时，不能把连接已经观察到的水位倒退。快照
    /// 仍然完全来自 Runtime，只把 V4 外层的读取水位和快照 `seq` 一起归一化。
    fn normalize_snapshot_frame(
        &self,
        subscription_id: &str,
        mut frame: Value,
        advance_if_equal: bool,
    ) -> Value {
        let journal_seq = frame.get("toSeq").and_then(Value::as_u64).unwrap_or(0);
        let is_conversation = self
            .subscriptions
            .lock()
            .ok()
            .and_then(|subscriptions| {
                subscriptions
                    .get(subscription_id)
                    .map(|subscription| subscription.kind == "conversation")
            })
            .unwrap_or(false);
        let seq = self
            .subscriptions
            .lock()
            .ok()
            .and_then(|mut subscriptions| {
                let subscription = subscriptions.get_mut(subscription_id)?;
                subscription.journal_seq = subscription.journal_seq.max(journal_seq);
                let seq = if advance_if_equal && journal_seq <= subscription.seq {
                    subscription.seq.saturating_add(1)
                } else {
                    subscription.seq.max(journal_seq)
                };
                subscription.seq = seq;
                if subscription.kind == "conversation" {
                    // Snapshot 不携带 transient rows；旧临时行索引不得继续更新
                    // 已被替换的 rowId。
                    subscription.transient = TransientProjection::default();
                }
                Some(seq)
            })
            .unwrap_or(journal_seq);
        frame["fromSeq"] = Value::from(0_u64);
        frame["toSeq"] = Value::from(seq);
        if is_conversation
            && frame.pointer("/payload/kind").and_then(Value::as_str) == Some("snapshot")
            && let Some(snapshot) = frame.pointer_mut("/payload/snapshot")
            && snapshot.is_object()
        {
            snapshot["seq"] = Value::from(seq);
        }
        frame
    }

    /// 查询某连接对某会话已经观察到的 V4 投影水位；没有活动订阅时返回 None，
    /// 调用方应继续使用 Runtime/Journal 的权威 `last_sequence`。
    fn wire_watermark_for_session(&self, connection_id: &str, session_id: &str) -> Option<u64> {
        self.subscriptions.lock().ok().and_then(|subscriptions| {
            subscriptions
                .values()
                .filter(|subscription| {
                    subscription.connection_id == connection_id
                        && subscription.kind == "conversation"
                        && (subscription.identity == session_id
                            || subscription.session_id.as_deref() == Some(session_id))
                })
                .map(|subscription| subscription.seq)
                .max()
        })
    }

    fn emit_transient_event(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        event: &AgentStreamEvent,
        delivery_sequence: u64,
    ) {
        let Ok(_snapshot_read) = self.snapshot_reads.lock() else {
            return;
        };
        let _ = self.with_frame_gate(subscription_id, || {
            // 首个同 Turn 的模型流事件代表重试已经重新进入执行；先用同一份
            // Runtime snapshot 清除 apiRetry，再排放本事件的 transient row，
            // 避免重试提示在新内容之后残留或因顺序错乱回滚。
            let should_clear_retry = self
                .subscriptions
                .lock()
                .ok()
                .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
                .is_some_and(|subscription| {
                    subscription.api_retry_turn_id.as_deref() == Some(event.turn_id().as_str())
                });
            if should_clear_retry
                && let Some(subscription) = self
                    .subscriptions
                    .lock()
                    .ok()
                    .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
                && let Some(mut frame) =
                    self.snapshot_frame_for_subscription(app, runtime, &subscription)
            {
                if let Some(control) = frame.pointer_mut("/payload/snapshot/control") {
                    control["apiRetry"] = Value::Null;
                }
                let frame = self.normalize_snapshot_frame(subscription_id, frame, true);
                if let Ok(mut subscriptions) = self.subscriptions.lock()
                    && let Some(subscription) = subscriptions.get_mut(subscription_id)
                {
                    subscription.api_retry_turn_id = None;
                }
                self.queue_or_emit_frame(subscription_id, frame, FrameDeliveryKind::Online);
            }

            let Some((topic, workspace_path, frame)) = (|| {
                let mut subscriptions = self.subscriptions.lock().ok()?;
                let subscription = subscriptions.get_mut(subscription_id)?;
                if subscription.kind != "conversation"
                    || subscription.session_id.as_deref() != Some(event.session_id().as_str())
                    || subscription.agent_id.as_ref().is_some_and(|agent_id| {
                        agent_id.as_str() != event.source_agent_id().as_str()
                    })
                {
                    return None;
                }
                let deltas =
                    project_transient_event(event, &mut subscription.transient, delivery_sequence);
                if deltas.is_empty() {
                    return None;
                }
                let from_seq = subscription.seq;
                // delivery_sequence 属于 Runtime 广播游标，可能跨越尚未进入该订阅
                // 投影的事件；把它写入 frame.toSeq 会制造无法恢复的人工断档。临时
                // delta 只推进当前连接的投影水位一步，行内 createdAtSeq 仍保留真实
                // delivery_sequence 供临时行排序。
                let to_seq = from_seq.saturating_add(1);
                subscription.seq = to_seq;
                Some((
                    subscription.topic.clone(),
                    subscription.workspace_path.clone(),
                    json!({
                        "topic": subscription.topic,
                        "subscriptionId": subscription.subscription_id,
                        "fromSeq": from_seq,
                        "toSeq": to_seq,
                        "sentAt": protocol::now_epoch_ms(),
                        "payload": {"kind": "deltas", "deltas": deltas},
                    }),
                ))
            })() else {
                return;
            };
            // queue_or_emit_frame 负责 activation barrier；这里不直接调用 listener，避免
            // ACK 尚未写入时 transient 首帧穿过订阅边界。
            let _ = (topic, workspace_path);
            self.queue_or_emit_frame(subscription_id, frame, FrameDeliveryKind::Online);
        });
    }

    fn emit_retry_event(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        retry: &RuntimeModelRetryScheduled,
        _delivery_sequence: u64,
    ) {
        let Ok(_snapshot_read) = self.snapshot_reads.lock() else {
            return;
        };
        let _ = self.with_frame_gate(subscription_id, || {
            let Some(subscription) = self
                .subscriptions
                .lock()
                .ok()
                .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
            else {
                return;
            };
            if !retry_targets_subscription(&subscription, retry)
                || !retry_is_active(runtime, app, &subscription, retry)
            {
                return;
            }
            let Some(mut frame) = self.snapshot_frame_for_subscription(app, runtime, &subscription)
            else {
                return;
            };
            let Some(snapshot) = frame.pointer_mut("/payload/snapshot") else {
                return;
            };
            // Runtime 状态可能已经在 publisher 读取后进入终态；只有仍可停止的
            // 当前 Turn 才能把重试提示投影到 V4，终态快照自然保持 null。
            if snapshot
                .pointer("/control/canStop")
                .and_then(Value::as_bool)
                != Some(true)
            {
                return;
            }
            snapshot["control"]["apiRetry"] = retry_state_value(retry);
            let frame = self.normalize_snapshot_frame(subscription_id, frame, true);
            if let Ok(mut subscriptions) = self.subscriptions.lock()
                && let Some(subscription) = subscriptions.get_mut(subscription_id)
            {
                subscription.api_retry_turn_id = Some(retry.turn_id.clone());
            }
            self.queue_or_emit_frame(subscription_id, frame, FrameDeliveryKind::Online);
        });
    }

    fn snapshot_frame_for_subscription(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription: &ActiveSubscription,
    ) -> Option<Value> {
        match subscription.kind.as_str() {
            "conversation" => {
                let scope = projection::ConversationScope {
                    requested_session_id: subscription.identity.clone(),
                    parent_session_id: subscription
                        .session_id
                        .clone()
                        .unwrap_or_else(|| subscription.identity.clone()),
                    agent_id: subscription.agent_id.clone(),
                };
                projection::conversation_frame_for_connection_with_agent(
                    runtime,
                    app,
                    &scope,
                    &subscription.connection_id,
                    &subscription.topic,
                    &subscription.subscription_id,
                    &subscription.log_epoch,
                )
                .ok()
            }
            "sessions-index" => keencode_acp::ConnectionId::new(subscription.connection_id.clone())
                .ok()
                .and_then(|connection_id| {
                    projection::sessions_index_frame_for_connection(
                        runtime,
                        &subscription.workspace_path,
                        &subscription.identity,
                        &subscription.subscription_id,
                        &subscription.log_epoch,
                        Some(&connection_id),
                    )
                    .ok()
                }),
            "workspace-config" => {
                projection::workspace_config_snapshot(app, &subscription.identity)
                    .ok()
                    .map(|snapshot| {
                        json!({
                            "topic": subscription.topic,
                            "subscriptionId": subscription.subscription_id,
                            "fromSeq": 0,
                            "toSeq": 0,
                            "sentAt": protocol::now_epoch_ms(),
                            "payload": {"kind": "snapshot", "snapshot": snapshot},
                        })
                    })
            }
            _ => None,
        }
    }

    fn refresh_retry_marker(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        subscription: &ActiveSubscription,
    ) {
        let marker = active_retry_turn_id(runtime, app, subscription);
        if let Ok(mut subscriptions) = self.subscriptions.lock()
            && let Some(subscription) = subscriptions.get_mut(subscription_id)
        {
            subscription.api_retry_turn_id = marker;
        }
    }

    fn emit_snapshot(&self, app: &AppHandle, runtime: &Arc<AgentRuntime>, subscription_id: &str) {
        let Ok(_snapshot_read) = self.snapshot_reads.lock() else {
            return;
        };
        let _ = self.with_frame_gate(subscription_id, || {
            let Some(subscription) = self
                .subscriptions
                .lock()
                .ok()
                .and_then(|map| map.get(subscription_id).cloned())
            else {
                return;
            };
            let retry_marker = active_retry_turn_id(runtime, app, &subscription);
            if let Some(frame) = self.snapshot_frame_for_subscription(app, runtime, &subscription) {
                let frame = self.normalize_snapshot_frame(subscription_id, frame, true);
                if let Ok(mut subscriptions) = self.subscriptions.lock()
                    && let Some(subscription) = subscriptions.get_mut(subscription_id)
                {
                    // 权威快照重新读取同一份 Runtime 热 retry；无活动 Turn 或终态
                    // 时 marker 为空，下一帧 transient 不会清除新的事实。
                    subscription.api_retry_turn_id = retry_marker;
                }
                self.queue_or_emit_frame(subscription_id, frame, FrameDeliveryKind::Online);
            }
        });
    }

    fn emit_snapshots_for_session(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        session_id: &str,
        connection_id: &str,
    ) {
        let subscription_ids = self
            .subscriptions
            .lock()
            .ok()
            .map(|subscriptions| {
                subscriptions
                    .iter()
                    .filter(|(_, subscription)| {
                        subscription.connection_id == connection_id
                            && subscription.kind == "conversation"
                            && subscription.agent_id.is_none()
                            && (subscription.identity == session_id
                                || subscription.session_id.as_deref() == Some(session_id))
                    })
                    .map(|(subscription_id, _)| subscription_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for subscription_id in subscription_ids {
            self.emit_snapshot(app, runtime, &subscription_id);
        }
    }

    fn emit_runtime_event(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        delivery: &keencode_runtime::RuntimeEventDelivery,
    ) {
        match &delivery.payload {
            RuntimeEventPayload::Transient(event) => {
                self.emit_transient_event(
                    app,
                    runtime,
                    subscription_id,
                    event,
                    delivery.delivery_sequence,
                );
                return;
            }
            RuntimeEventPayload::ModelRetryScheduled(retry) => {
                self.emit_retry_event(
                    app,
                    runtime,
                    subscription_id,
                    retry,
                    delivery.delivery_sequence,
                );
                return;
            }
            RuntimeEventPayload::Authoritative(_) | RuntimeEventPayload::Control(_) => {
                self.emit_snapshot(app, runtime, subscription_id);
            }
        }
        // Runtime 目前按 Session 发布事件。将同一 workspace 的 index/config 订阅
        // 绑定到这次真实事件，避免它们只收到一次静态首帧；后续若根装配提供 workspace
        // 级 publisher，可在此替换来源而不改变协议投影。
        let workspace_path = self.subscriptions.lock().ok().and_then(|subscriptions| {
            subscriptions
                .get(subscription_id)
                .map(|item| item.workspace_path.clone())
        });
        if let Some(workspace_path) = workspace_path {
            let task_id = self
                .subscriptions
                .lock()
                .ok()
                .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
                .and_then(|subscription| subscription.session_id);
            if let Some(task_id) = task_id {
                let unread_signal = self
                    .subscriptions
                    .lock()
                    .ok()
                    .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
                    .filter(|subscription| subscription.agent_id.is_none())
                    .and_then(|_| match &delivery.payload {
                        RuntimeEventPayload::Authoritative(record)
                            if has_root_terminal_turn(runtime, app, &task_id, &record.event) =>
                        {
                            Some("background_terminal")
                        }
                        _ => None,
                    });
                // task service 的 workspace event 是旧列表层唯一需要的实时失效信号；
                // 载荷来自同一份 Runtime metadata，不能由前端本地猜测任务状态。
                self.emit_workspace_task_event_with_signal(
                    app,
                    runtime,
                    &workspace_path,
                    &task_id,
                    "task_status_changed",
                    unread_signal,
                );
            }
            let related = self
                .subscriptions
                .lock()
                .ok()
                .map(|subscriptions| {
                    subscriptions
                        .iter()
                        .filter(|(_, item)| {
                            item.workspace_path == workspace_path && item.kind != "conversation"
                        })
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for related_id in related {
                self.emit_snapshot(app, runtime, &related_id);
            }
        }
        if let RuntimeEventPayload::Authoritative(record) = &delivery.payload {
            let Some(progress) = workflow_progress(record) else {
                return;
            };
            let Some(subscription) = self
                .subscriptions
                .lock()
                .ok()
                .and_then(|map| map.get(subscription_id).cloned())
            else {
                return;
            };
            if subscription.agent_id.is_some() || !subscription.activated {
                // 首帧会在 ACK 后携带这段时间内的 Journal 状态；在 barrier 打开前
                // 不单独排放 workflow progress，避免动态事件越过首帧。core
                // subagent view 只读取 Agent Transcript，不继承父 Workflow 事件。
                return;
            }
            let listeners = self
                .listeners
                .lock()
                .ok()
                .map(|map| map.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for listener in listeners {
                if listener.connection_id == subscription.connection_id
                    && listener.channel == AGENT_CHANNEL
                    && listener.event == "onDynamicWorkflowRunProgress"
                    && workspace_matches(
                        listener.workspace_path.as_deref(),
                        &subscription.workspace_path,
                    )
                    && let Err(error) = (listener.callback)(progress.clone())
                {
                    log_event_delivery_failure(listener.event.as_str(), &error);
                }
            }
        }
    }

    /// 向 task service 订阅者投递真实 Runtime 变更对应的 workspace 事件。
    ///
    /// 事件只按已授权的规范 workspace 路径匹配；task meta 若能从当前 Journal
    /// 读取则随事件带出，否则保留 taskId 让 UI 通过后续列表查询收敛，绝不构造
    /// 空任务行或伪造成功状态。
    fn emit_workspace_task_event(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        workspace_path: &str,
        task_id: &str,
        reason: &str,
    ) {
        self.emit_workspace_task_event_with_signal(
            app,
            runtime,
            workspace_path,
            task_id,
            reason,
            None,
        );
    }

    /// 投递 task 列表事件；只有真实根 Turn 终态才允许附带后台未读信号。
    ///
    /// 普通 Journal、Transient 流和虚拟 Agent 事件必须保持无 signal，未读写入仍由
    /// `task_extras::set_task_unread` 统一负责，避免 renderer 出现第二个持久 store。
    fn emit_workspace_task_event_with_signal(
        &self,
        app: &AppHandle,
        runtime: &Arc<AgentRuntime>,
        workspace_path: &str,
        task_id: &str,
        reason: &str,
        unread_signal: Option<&str>,
    ) {
        let task_meta = runtime
            .runtime_manager()
            .stored_session_metadata(task_id)
            .ok()
            .and_then(|metadata| {
                task_meta_for_stored_session(runtime, app, &metadata, None)
                    .ok()
                    .flatten()
            });
        let event =
            workspace_task_event_payload(workspace_path, task_id, reason, task_meta, unread_signal);
        let listeners = self
            .listeners
            .lock()
            .ok()
            .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for listener in listeners {
            if listener.channel == TASK_CHANNEL
                && listener.event == "onDynamicWorkspaceEvent"
                && workspace_matches(listener.workspace_path.as_deref(), workspace_path)
                && let Err(error) = (listener.callback)(event.clone())
            {
                log_event_delivery_failure(listener.event.as_str(), &error);
            }
        }
    }
}

/// 构造 task service workspace 事件的稳定 DTO；未读 mutation 不附带终态 signal，
/// 由 taskMeta 的持久 `unreadAt` 作为唯一投影值。
fn workspace_task_event_payload(
    workspace_path: &str,
    task_id: &str,
    reason: &str,
    task_meta: Option<Value>,
    unread_signal: Option<&str>,
) -> Value {
    let mut event = json!({
        "type": "workspace_task_list_changed",
        "workspacePath": path_to_frontend(Path::new(workspace_path)),
        "taskId": task_id,
        "reason": reason,
    });
    if let Some(unread_signal) = unread_signal {
        event["unreadSignal"] = Value::String(unread_signal.to_owned());
    }
    if let Some(task_meta) = task_meta {
        event["taskMeta"] = task_meta;
    }
    event
}

fn allocate_transient_row_id() -> u64 {
    NEXT_TRANSIENT_ROW_ID.fetch_add(1, Ordering::Relaxed)
}

/// 为队列提升生成不携带前端原始 command 字符的稳定 Runtime Turn 标识。
fn queue_turn_id(operation_id: &str) -> String {
    format!("queue-turn-{}", queue_item_digest(operation_id))
}

/// 队列自动排放使用固定长度的小写十六进制操作标识，满足资源层 ID 边界。
fn queue_item_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
}

fn transient_response_id(event: &AgentStreamEvent) -> String {
    format!("{}:round:{}", event.turn_id(), event.model_round())
}

fn transient_row_common(
    event: &AgentStreamEvent,
    row_id: u64,
    entity_id: &str,
    delivery_sequence: u64,
) -> Value {
    json!({
        "rowId": row_id,
        "turnId": event.turn_id().as_str(),
        "entityId": entity_id,
        "createdAt": protocol::now_epoch_ms(),
        "createdAtSeq": delivery_sequence.max(1),
        "visibility": "visible",
    })
}

fn flatten_transient_row(row: Value) -> Value {
    let mut flattened = row
        .get("row")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(object) = row.as_object() {
        for (key, value) in object {
            if key != "row" {
                flattened.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(flattened)
}

fn transient_assistant_row(
    event: &AgentStreamEvent,
    projection: &TransientProjection,
    state: &str,
    delivery_sequence: u64,
) -> Value {
    let response_id = projection
        .response_id
        .as_deref()
        .unwrap_or("stream-response");
    let row_id = projection
        .assistant_row_id
        .unwrap_or(allocate_transient_row_id());
    flatten_transient_row(json!({
        "kind": "assistantText",
        "assistantResponseId": response_id,
        "text": projection.assistant_text,
        "state": state,
        "model": event.model(),
        "actions": {"canFork": true, "canRetry": true},
        "row": transient_row_common(
            event,
            row_id,
            &format!("stream:{response_id}:assistant"),
            delivery_sequence,
        ),
    }))
}

fn transient_reasoning_row(
    event: &AgentStreamEvent,
    projection: &TransientProjection,
    index: u32,
    state: &str,
    delivery_sequence: u64,
) -> Option<Value> {
    let row_id = projection.reasoning_rows.get(&index).copied()?;
    let response_id = projection
        .response_id
        .as_deref()
        .unwrap_or("stream-response");
    Some(flatten_transient_row(json!({
        "kind": "reasoning",
        "assistantResponseId": response_id,
        "text": projection.reasoning_text.get(&index).cloned().unwrap_or_default(),
        "state": state,
        "row": transient_row_common(
            event,
            row_id,
            &format!("stream:{response_id}:reasoning:{index}"),
            delivery_sequence,
        ),
    })))
}

fn transient_tool_row(
    event: &AgentStreamEvent,
    projection: &TransientProjection,
    call_id: &str,
    status: &str,
    delivery_sequence: u64,
) -> Option<Value> {
    let row_id = projection.tool_rows.get(call_id).copied()?;
    let response_id = projection
        .response_id
        .as_deref()
        .unwrap_or("stream-response");
    let input_text = projection
        .tool_inputs
        .get(call_id)
        .cloned()
        .unwrap_or_default();
    let mut row = json!({
        "kind": "toolCall",
        "assistantResponseId": response_id,
        "toolCallId": call_id,
        "toolName": projection.tool_names.get(call_id).cloned().unwrap_or_else(|| "unknown".to_owned()),
        "status": status,
        "inputText": input_text,
        "row": transient_row_common(
            event,
            row_id,
            &format!("stream:{response_id}:tool:{call_id}"),
            delivery_sequence,
        ),
    });
    if let Some(arguments) = projection.tool_inputs.get(call_id)
        && let Ok(value) = serde_json::from_str::<Value>(arguments)
    {
        row["input"] = value;
    }
    Some(flatten_transient_row(row))
}

fn ensure_transient_response(
    event: &AgentStreamEvent,
    projection: &mut TransientProjection,
    deltas: &mut Vec<Value>,
    delivery_sequence: u64,
) {
    if projection.response_id.is_some() && projection.assistant_row_id.is_some() {
        return;
    }
    projection.response_id = Some(transient_response_id(event));
    let row_id = allocate_transient_row_id();
    projection.assistant_row_id = Some(row_id);
    deltas.push(json!({
        "op": "row.appended",
        "row": transient_assistant_row(event, projection, "streaming", delivery_sequence),
    }));
}

fn project_transient_event(
    event: &AgentStreamEvent,
    projection: &mut TransientProjection,
    delivery_sequence: u64,
) -> Vec<Value> {
    let mut deltas = Vec::new();
    // 临时行的 createdAtSeq 使用 Runtime 本地 delivery sequence；它不冒充 Journal
    // sequence，但在同一订阅内仍保持单调，便于 UI 诊断增量顺序。
    match event.kind() {
        AgentStreamEventKind::ModelEvent { event: model_event } => match model_event {
            ModelStreamEvent::MessageStart { .. } => {
                *projection = TransientProjection {
                    response_id: Some(transient_response_id(event)),
                    assistant_row_id: Some(allocate_transient_row_id()),
                    assistant_text: String::new(),
                    reasoning_rows: HashMap::new(),
                    reasoning_text: HashMap::new(),
                    tool_rows: HashMap::new(),
                    tool_names: HashMap::new(),
                    tool_inputs: HashMap::new(),
                };
                deltas.push(json!({
                    "op": "row.appended",
                    "row": transient_assistant_row(event, projection, "streaming", delivery_sequence),
                }));
            }
            ModelStreamEvent::TextDelta { delta, .. } => {
                ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
                projection.assistant_text.push_str(delta);
                if !delta.is_empty() {
                    deltas.push(json!({
                        "op": "row.delta",
                        "rowId": projection.assistant_row_id.unwrap_or_default(),
                        "path": "text",
                        "append": delta,
                    }));
                }
            }
            ModelStreamEvent::ReasoningDelta { index, delta }
            | ModelStreamEvent::ReasoningSummaryDelta { index, delta } => {
                ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
                let row_id = if let Some(row_id) = projection.reasoning_rows.get(index).copied() {
                    row_id
                } else {
                    let row_id = allocate_transient_row_id();
                    projection.reasoning_rows.insert(*index, row_id);
                    projection.reasoning_text.insert(*index, String::new());
                    if let Some(row) = transient_reasoning_row(
                        event,
                        projection,
                        *index,
                        "streaming",
                        delivery_sequence,
                    ) {
                        deltas.push(json!({
                            "op": "row.appended",
                            "row": row,
                        }));
                    }
                    row_id
                };
                projection
                    .reasoning_text
                    .entry(*index)
                    .or_default()
                    .push_str(delta);
                if !delta.is_empty() {
                    deltas.push(json!({
                        "op": "row.delta",
                        "rowId": row_id,
                        "path": "text",
                        "append": delta,
                    }));
                }
            }
            ModelStreamEvent::ToolCallStart { id, name, .. } => {
                ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
                let row_id = allocate_transient_row_id();
                projection.tool_rows.insert(id.clone(), row_id);
                projection.tool_names.insert(id.clone(), name.clone());
                projection.tool_inputs.insert(id.clone(), String::new());
                if let Some(row) =
                    transient_tool_row(event, projection, id, "inputStreaming", delivery_sequence)
                {
                    deltas.push(json!({"op": "row.appended", "row": row}));
                }
            }
            ModelStreamEvent::ToolCallArgumentsDelta { id, delta, .. } => {
                ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
                if !projection.tool_rows.contains_key(id) {
                    let row_id = allocate_transient_row_id();
                    projection.tool_rows.insert(id.clone(), row_id);
                    projection
                        .tool_names
                        .insert(id.clone(), "unknown".to_owned());
                    projection.tool_inputs.insert(id.clone(), String::new());
                    if let Some(row) = transient_tool_row(
                        event,
                        projection,
                        id,
                        "inputStreaming",
                        delivery_sequence,
                    ) {
                        deltas.push(json!({"op": "row.appended", "row": row}));
                    }
                }
                projection
                    .tool_inputs
                    .entry(id.clone())
                    .or_default()
                    .push_str(delta);
                if !delta.is_empty() {
                    deltas.push(json!({
                        "op": "row.delta",
                        "rowId": projection.tool_rows[id],
                        "path": "inputText",
                        "append": delta,
                    }));
                }
            }
            ModelStreamEvent::ToolCallEnd { id, .. } => {
                ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
                if let Some(row) =
                    transient_tool_row(event, projection, id, "running", delivery_sequence)
                {
                    deltas.push(json!({"op": "row.upserted", "row": row}));
                }
            }
            ModelStreamEvent::MessageEnd { .. } => {
                if projection.assistant_row_id.is_some() {
                    deltas.push(json!({
                        "op": "row.upserted",
                        "row": transient_assistant_row(event, projection, "complete", delivery_sequence),
                    }));
                }
                let reasoning_indices = projection
                    .reasoning_rows
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                for index in reasoning_indices {
                    if let Some(row) = transient_reasoning_row(
                        event,
                        projection,
                        index,
                        "complete",
                        delivery_sequence,
                    ) {
                        deltas.push(json!({"op": "row.upserted", "row": row}));
                    }
                }
                let tool_ids = projection.tool_rows.keys().cloned().collect::<Vec<_>>();
                for id in tool_ids {
                    if let Some(row) =
                        transient_tool_row(event, projection, &id, "running", delivery_sequence)
                    {
                        deltas.push(json!({"op": "row.upserted", "row": row}));
                    }
                }
            }
            ModelStreamEvent::ReasoningContinuation { .. }
            | ModelStreamEvent::Usage { .. }
            | ModelStreamEvent::DecodeTiming { .. } => {}
        },
        AgentStreamEventKind::ModelFailure { .. } => {
            ensure_transient_response(event, projection, &mut deltas, delivery_sequence);
            if projection.assistant_row_id.is_some() {
                deltas.push(json!({
                    "op": "row.upserted",
                    "row": transient_assistant_row(event, projection, "failed", delivery_sequence),
                }));
            }
            let reasoning_indices = projection
                .reasoning_rows
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for index in reasoning_indices {
                if let Some(row) = transient_reasoning_row(
                    event,
                    projection,
                    index,
                    "interrupted",
                    delivery_sequence,
                ) {
                    deltas.push(json!({"op": "row.upserted", "row": row}));
                }
            }
        }
        AgentStreamEventKind::ContextCompactionStarted { .. }
        | AgentStreamEventKind::ContextCompactionFailed { .. }
        | AgentStreamEventKind::ContextCompactionTruncated { .. } => {}
    }
    deltas
}

fn workflow_progress(record: &keencode_resources::SessionEventRecord) -> Option<Value> {
    fn find(value: &Value) -> Option<keencode_resources::WorkflowJournalEvent> {
        if let Ok(event) =
            serde_json::from_value::<keencode_resources::WorkflowJournalEvent>(value.clone())
        {
            return Some(event);
        }
        match value {
            Value::Object(map) => map.values().find_map(find),
            Value::Array(values) => values.iter().find_map(find),
            _ => None,
        }
    }
    let raw = serde_json::to_value(record).ok()?;
    let event = find(&raw)?;
    serde_json::to_value(progress_from_workflow_event(&event)).ok()
}

/// 递归查找一条权威事件中真正结束的 Turn；普通 Journal 事件和 transient 流不匹配。
///
/// `SessionEvent` 的有效 Journal 禁止嵌套 AtomicBatch，但这里仍递归处理，保证恢复、
/// 测试或未来兼容事件不会因批次包装层变化而漏掉唯一的终态边界。
fn has_terminal_turn_for<F>(event: &SessionEvent, is_root_turn: &F) -> bool
where
    F: Fn(&keencode_resources::TurnId) -> bool,
{
    match event {
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .any(|event| has_terminal_turn_for(event, is_root_turn)),
        SessionEvent::TurnCompleted { turn_id } | SessionEvent::TurnStopped { turn_id, .. } => {
            is_root_turn(turn_id)
        }
        _ => false,
    }
}

/// 只给当前主会话的根 Turn 终态投递 Source 约定的后台未读信号。
fn has_root_terminal_turn(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    session_id: &str,
    event: &SessionEvent,
) -> bool {
    let Ok(session) = open_authorized_session(runtime, app, session_id) else {
        return false;
    };
    let Ok(snapshot) = session.snapshot() else {
        return false;
    };
    has_terminal_turn_for(event, &|turn_id| {
        snapshot
            .state
            .turns
            .get(turn_id)
            .is_some_and(|turn| turn.source_agent_id.as_str() == ROOT_AGENT_ID)
    })
}

fn is_subscription_method(method: &str) -> bool {
    subscription_kind(method).is_some()
}

fn subscription_kind(method: &str) -> Option<&'static str> {
    match method {
        "subscribeConversationV4" => Some("conversation"),
        "subscribeSessionsIndexV4" => Some("sessions-index"),
        "subscribeWorkspaceConfigV4" => Some("workspace-config"),
        _ => None,
    }
}

fn subscription_topic(method: &str, args: &Value) -> Result<String, RpcError> {
    let kind = subscription_kind(method).ok_or_else(|| unknown_method(AGENT_CHANNEL, method))?;
    if let Some(topic) = optional_string(args, "topic")? {
        let (actual_kind, _) = protocol::topic_parts(&topic).map_err(invalid_params)?;
        if actual_kind != kind {
            return Err(invalid_params(format!("{method} 的 topic 必须属于 {kind}")));
        }
        return Ok(topic);
    }
    let identity = match kind {
        "conversation" => optional_string(args, "sessionId")?
            .ok_or_else(|| invalid_params("subscribeConversationV4 缺少 sessionId/topic"))?,
        "sessions-index" | "workspace-config" => {
            let path = required_workspace_path(args)?;
            workspace_identity(args, &path)?
        }
        _ => unreachable!(),
    };
    Ok(format!("{kind}/{identity}"))
}

fn authorized_runtime_session(
    app: &AppHandle,
    session_id: &str,
    workspace_path: &str,
) -> Result<Arc<AgentRuntime>, RpcError> {
    let runtime = owned_runtime(app)?;
    projection::validate_workspace(&runtime, session_id, workspace_path).map_err(backend_error)?;
    let _ = open_authorized_session(&runtime, app, session_id).map_err(backend_error)?;
    Ok(runtime)
}

/// 为已经授权并打开的 Session 发布当前项目的完整扩展候选，再冻结 Skill/Plugin
/// 引用目录。候选由 ExtensionsState 单飞构建；Session 只保存第一次成功冻结的
/// 副本，后续项目候选热替换不会改变已存在对话的 Composer 身份。
pub(crate) async fn prepare_session_reference_catalog(
    app: &AppHandle,
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    workspace_root: &Path,
) -> Result<(), RpcError> {
    crate::extensions::ensure_runtime_extension_candidate(app, workspace_root, runtime, false)
        .await
        .map_err(|error| {
            log_send_stage_failure(
                "prepare_reference_catalog.ensure_candidate",
                "extensions.stageFailed",
            );
            backend_error(format!("发布 Session 扩展候选失败：{error}"))
        })?;
    runtime
        .initialize_session_extension_reference_catalog(session_id, workspace_root)
        .map(|_| ())
        .map_err(|error| {
            log_send_stage_failure(
                "prepare_reference_catalog.initialize_catalog",
                "session.backendError",
            );
            backend_error(format!("冻结 Session 扩展引用目录失败：{error}"))
        })
}

/// 首次发送/释放路径使用的异步授权打开入口；打开、旧 lease 回收和后续配置
/// 必须落在同一个 Runtime 起点屏障内，避免另一个连接刚 close 时得到 SessionBusy。
async fn authorized_runtime_session_serialized(
    app: &AppHandle,
    session_id: &str,
    workspace_path: &str,
    operation_id: &str,
) -> Result<Arc<AgentRuntime>, RpcError> {
    let runtime = owned_runtime(app)?;
    projection::validate_workspace(&runtime, session_id, workspace_path).map_err(backend_error)?;
    let root = authorize_workspace_scope(app, workspace_path)?;
    runtime
        .open_or_create_session_serialized(&root, Some(session_id), operation_id)
        .await
        .map_err(|error| backend_error(error.to_string()))?;
    prepare_session_reference_catalog(app, &runtime, session_id, &root).await?;
    Ok(runtime)
}

fn legacy_snapshot(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    session_id: &str,
    workspace_path: &str,
) -> Result<Value, RpcError> {
    let session = open_authorized_session(runtime, app, session_id).map_err(backend_error)?;
    let snapshot = session.snapshot().map_err(backend_error)?;
    let transcript = session
        .transcript()
        .map_err(backend_error)?
        .into_iter()
        .map(|message| {
            serde_json::to_value(message).map_err(|error| backend_error(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let state = serde_json::to_value(&snapshot.state).map_err(backend_error)?;
    Ok(json!({
        "session": {
            "sessionId": session_id,
            "workspace": {"workspacePath": workspace_path},
            "title": snapshot.state.title,
            "status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
        },
        "runtime": {
            "eventSeq": snapshot.state.last_sequence,
            "stateRevision": snapshot.state.transcript_revision,
            "activeTurnId": state.get("turns").and_then(Value::as_object).and_then(|turns| turns.values().find(|turn| turn.get("status").and_then(Value::as_str) == Some("running")).and_then(|turn| turn.get("turnId")).cloned()),
        },
        "messages": transcript,
        "settings": {"model": state.get("provider").cloned().unwrap_or(Value::Null)},
    }))
}

/// 读取核心子 Agent 的兼容 Session 快照。子 Agent 没有独立 RuntimeSession，
/// 所有消息来自父 Session 的 effective transcript，状态只按对应 Agent/Turn 过滤。
fn legacy_snapshot_for_virtual_agent(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    scope: &projection::ConversationScope,
    workspace_path: &str,
) -> Result<Value, RpcError> {
    let agent_id = scope
        .agent_id
        .as_ref()
        .ok_or_else(|| RpcError::new("agent.invalid_scope", "缺少 virtual Agent 身份"))?;
    let session =
        open_authorized_session(runtime, app, &scope.parent_session_id).map_err(backend_error)?;
    let snapshot = session.snapshot().map_err(backend_error)?;
    let transcript = snapshot
        .state
        .effective_transcript(agent_id)
        .map_err(backend_error)?
        .into_iter()
        .map(|message| {
            serde_json::to_value(message).map_err(|error| backend_error(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let agent = snapshot
        .state
        .sub_agents
        .get(agent_id)
        .ok_or_else(|| RpcError::new("agent.not_found", "核心子 Agent 不存在"))?;
    let state = serde_json::to_value(&snapshot.state).map_err(backend_error)?;
    let active_turn_id = agent.current_turn_id.as_ref().and_then(|turn_id| {
        snapshot
            .state
            .turns
            .get(turn_id)
            .filter(|turn| turn.status == TurnStatus::Running)
            .map(|turn| Value::String(turn.turn_id.as_str().to_owned()))
    });
    let status = match agent.status {
        keencode_resources::SubAgentStatus::Pending
        | keencode_resources::SubAgentStatus::Waiting => "waiting",
        keencode_resources::SubAgentStatus::Running => "running",
        keencode_resources::SubAgentStatus::Completed => "success",
        keencode_resources::SubAgentStatus::Failed => "failed",
        keencode_resources::SubAgentStatus::Interrupted
        | keencode_resources::SubAgentStatus::Stopped => "cancelled",
    };
    Ok(json!({
        "session": {
            "sessionId": scope.requested_session_id,
            "workspace": {"workspacePath": workspace_path},
            "title": agent.agent_path,
            "status": status,
        },
        "runtime": {
            "eventSeq": snapshot.state.last_sequence,
            "stateRevision": snapshot.state.transcript_revision,
            "activeTurnId": active_turn_id,
        },
        "messages": transcript,
        "settings": {"model": state.get("provider").cloned().unwrap_or(Value::Null)},
    }))
}

fn apply_requested_config(
    runtime: &Arc<AgentRuntime>,
    _app: &AppHandle,
    session_id: &str,
    operation_id: &str,
    payload: &Value,
) -> Result<(), RpcError> {
    let config = payload.get("config").unwrap_or(payload);
    if let Some(selection) = config.get("modelSelection") {
        apply_model_value(runtime, session_id, operation_id, selection).inspect_err(|error| {
            log_send_stage_failure("apply_requested_config.model_selection", error.code());
        })?;
        apply_requested_effort(
            runtime,
            session_id,
            operation_id,
            selection
                .pointer("/options/reasoningLevel")
                .and_then(Value::as_str),
        )
        .inspect_err(|error| {
            log_send_stage_failure("apply_requested_config.selection_reasoning", error.code());
        })?;
    } else if let (Some(provider), Some(model)) = (
        config.get("provider").and_then(Value::as_str),
        config.get("model").and_then(Value::as_str),
    ) {
        let _ = runtime
            .set_session_model(session_id, operation_id, provider, model)
            .map_err(|error| backend_error(error.to_string()))
            .inspect_err(|error| {
                log_send_stage_failure("apply_requested_config.provider_model", error.code());
            })?;
    }
    apply_requested_effort(
        runtime,
        session_id,
        operation_id,
        config.get("thought").and_then(Value::as_str),
    )
    .inspect_err(|error| {
        log_send_stage_failure("apply_requested_config.thought", error.code());
    })?;
    Ok(())
}

/// Source 用空字符串表示没有覆盖当前 reasoning 配置；它不能被当作非法档位，
/// 否则草稿态 createSession 会在 ACK 前被收口为 resultUnknown。非空值仍交给
/// Runtime 的固定枚举解析，未知值必须拒绝，不能借空值规则兜底。
fn apply_requested_effort(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    operation_id: &str,
    effort: Option<&str>,
) -> Result<(), RpcError> {
    let Some(effort) = effort.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    runtime
        .set_session_effort(session_id, operation_id, effort)
        .map_err(|error| backend_error(error.to_string()))
}

/// 源 V4 command 允许 Plan 标记位于顶层、`config` 或 `firstInput`；三处都
/// 表示同一个本轮只读边界，不能只读取顶层字段而让 UI 的 Plan 选择失效。
fn plan_enabled_in_payload(payload: &Value) -> bool {
    payload
        .get("planEnabled")
        .and_then(Value::as_bool)
        .or_else(|| {
            payload
                .get("config")
                .and_then(|config| config.get("planEnabled"))
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            payload
                .get("firstInput")
                .and_then(|first_input| first_input.get("planEnabled"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn apply_model_value(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    operation_id: &str,
    value: &Value,
) -> Result<(), RpcError> {
    let provider = value
        .get("providerId")
        .or_else(|| value.get("provider"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("model/providerId 缺失"))?;
    let model = value
        .get("modelId")
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("model/modelId 缺失"))?;
    runtime
        .set_session_model(session_id, operation_id, provider, model)
        .map_err(|error| backend_error(error.to_string()))?;
    Ok(())
}

fn find_message_text(
    app: &AppHandle,
    session_id: &str,
    workspace_path: &str,
    message_id: &str,
) -> Result<String, RpcError> {
    let runtime = owned_runtime(app)?;
    projection::validate_workspace(&runtime, session_id, workspace_path).map_err(backend_error)?;
    let session = open_authorized_session(&runtime, app, session_id).map_err(backend_error)?;
    let message = session
        .transcript()
        .map_err(backend_error)?
        .into_iter()
        .find(|message| message.message_id == message_id)
        .ok_or_else(|| {
            RpcError::new("fault.command.targetNotFound", "目标消息不属于当前 Session")
        })?;
    let value = serde_json::to_value(message).map_err(|error| backend_error(error.to_string()))?;
    Ok(message_text(&value))
}

fn message_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.get("content").and_then(Value::as_str))
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn owned_runtime(app: &AppHandle) -> Result<Arc<AgentRuntime>, RpcError> {
    crate::require_owned_runtime(app).map_err(backend_error)
}

fn required_workspace_path(args: &Value) -> Result<String, RpcError> {
    optional_string(args, "workspacePath")?.ok_or_else(|| invalid_params("缺少 workspacePath"))
}

fn workspace_identity(args: &Value, workspace_path: &str) -> Result<String, RpcError> {
    Ok(optional_string(args, "workspaceIdentity")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| workspace_path.to_owned()))
}

fn workspace_identity_wire(args: &Value, workspace_path: &str) -> Result<String, RpcError> {
    Ok(optional_string(args, "workspaceIdentity")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| path_to_frontend(Path::new(workspace_path))))
}

fn workspace_ref(args: &Value, workspace_path: &str) -> Result<Value, RpcError> {
    let mut workspace = serde_json::Map::new();
    workspace.insert(
        "workspacePath".to_owned(),
        Value::String(path_to_frontend(Path::new(workspace_path))),
    );
    workspace.insert(
        "workspaceKey".to_owned(),
        Value::String(workspace_identity_wire(args, workspace_path)?),
    );
    for name in ["workspaceIdentity", "remoteSessionId"] {
        if let Some(value) = optional_string(args, name)? {
            workspace.insert(name.to_owned(), Value::String(value));
        }
    }
    Ok(Value::Object(workspace))
}

fn workspace_matches(expected: Option<&str>, actual: &str) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if expected == actual {
        return true;
    }
    normalize_workspace_identity(expected) == normalize_workspace_identity(actual)
}

/// 比较工作区身份时去除 Windows 表示差异，但不把任意字符串当成授权路径。
/// listener/subscribe 均已通过 `authorize_workspace_scope`，此处只负责让同一真实
/// 目录的短路径、分隔符和 extended-length 前缀拥有相同的投递身份。
fn normalize_workspace_identity(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    if normalized.len() >= 4 && normalized[..4].eq_ignore_ascii_case("//?/") {
        normalized.drain(..4);
        if normalized.len() >= 4 && normalized[..4].eq_ignore_ascii_case("unc/") {
            normalized.replace_range(..4, "//");
        }
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    let looks_like_windows_path =
        normalized.len() >= 2 && normalized.as_bytes()[1] == b':' || normalized.starts_with("//");
    if looks_like_windows_path {
        normalized.make_ascii_lowercase();
    }
    normalized
}

/// 授权 workspace 级读取和新 Session 创建。
///
/// 会话工作区只有两种来源：项目登记表中的真实项目，或文件服务创建的唯一
/// `chat-workspaces/conversation` 根。应用数据根仍兼容已有 ACP 默认 cwd；其余
/// 任意存在目录都必须先登记为项目，避免把路径规范化误当成访问授权。
fn authorize_workspace_scope(
    app: &AppHandle,
    workspace_path: &str,
) -> Result<std::path::PathBuf, RpcError> {
    let canonical =
        crate::workspace::canonical_session_root(workspace_path).map_err(backend_error)?;
    let app_data_root = crate::workspace::app_data_session_root(app).map_err(backend_error)?;
    let conversation_root =
        crate::workspace::conversation_workspace_root(app).map_err(backend_error)?;
    if canonical == app_data_root || canonical == conversation_root {
        return Ok(canonical);
    }
    let registered =
        crate::workspace::registered_project_root(app, workspace_path).map_err(backend_error)?;
    if registered != canonical {
        return Err(backend_error("workspace 路径规范化后与登记项目不一致"));
    }
    Ok(registered)
}

/// 读取现有 deleted-sessions 负向事实；不创建新的删除索引，避免收据恢复时把
/// 已删除会话重新当作可执行 Session。调用方必须已经完成 workspace 授权。
fn is_session_deleted(
    runtime: &AgentRuntime,
    session_id: &str,
    workspace_path: &str,
) -> Result<bool, RpcError> {
    let deleted =
        keencode_resources::list_deleted_session_ids(runtime.storage_root(), workspace_path)
            .map_err(backend_error)?;
    Ok(deleted
        .iter()
        .any(|deleted_id| deleted_id.as_str() == session_id))
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn log_epoch(kind: &str, identity: &str, workspace_path: &str) -> String {
    format!(
        "{}-{:016x}",
        kind,
        stable_hash(&format!("{workspace_path}\0{identity}"))
    )
}

fn command_key(scope: &str, command_id: &str) -> String {
    format!("{scope}\0{command_id}")
}

/// 把 Journal 中已经收口的 ACK 原样重放；在途或未知副作用绝不伪装成成功。
fn command_receipt_replay(receipt: &keencode_resources::CommandReceipt) -> Result<Value, RpcError> {
    match &receipt.status {
        CommandReceiptStatus::Completed { ack } | CommandReceiptStatus::Rejected { ack } => {
            Ok(ack.clone())
        }
        CommandReceiptStatus::Admitted => Err(RpcError::new(
            "fault.command.inFlight",
            "相同命令仍在执行，已禁止重复副作用",
        )),
        CommandReceiptStatus::Unknown { reason_code } => Err(RpcError::new(
            reason_code.clone(),
            "命令副作用状态未知，必须查询权威会话事实后处理",
        )),
    }
}

/// 将持久收据映射为协议允许的 query 结果；在途与未知统一保留 unknown。
fn command_receipt_query_result(receipt: &keencode_resources::CommandReceipt) -> Value {
    match &receipt.status {
        CommandReceiptStatus::Completed { ack } | CommandReceiptStatus::Rejected { ack } => {
            if command_ack_shape_is_queryable(ack) {
                ack.clone()
            } else {
                // Journal 中无法通过 Source commandAck 的最小严格形状时，
                // 只能报告未确认，不能把损坏 ACK 发给 renderer 触发 schema 例外。
                Value::String("unknown".to_owned())
            }
        }
        CommandReceiptStatus::Admitted | CommandReceiptStatus::Unknown { .. } => {
            Value::String("unknown".to_owned())
        }
    }
}

/// Source `commandAckSchema` 的 Rust 边界校验。完整 `result` 联合体仍由 Source
/// 解码器检查；此处只保证持久收据不会生成缺少核心字段的 query 响应。
fn command_ack_shape_is_queryable(ack: &Value) -> bool {
    let Some(object) = ack.as_object() else {
        return false;
    };
    let command_id = object.get("commandId").and_then(Value::as_str);
    let status = object.get("status").and_then(Value::as_str);
    let revision = object.get("revisionAtDecision").and_then(Value::as_f64);
    command_id.is_some_and(|value| !value.is_empty())
        && matches!(
            status,
            Some("accepted" | "rejected" | "stale" | "duplicate" | "noop" | "failed")
        )
        && revision.is_some_and(f64::is_finite)
}

/// 只有执行阶段能证明没有启动副作用时才把错误收口为 rejected；其余错误由
/// `send_command` 记录 Unknown，阻止同一 commandId 在重连后再次触发副作用。
fn is_deterministic_command_rejection(command_type: &str, error: &RpcError) -> bool {
    match error.code() {
        // 这些命令的参数检查都发生在业务写入/外部调用之前。create/side-session/
        // send/queue 另有 apply/转发阶段，参数错误可能在 Runtime 已经推进后出现，
        // 保守保留 Unknown，避免把已创建的 Session 或已入队输入误收口为拒绝。
        "rpc.invalidParams" => !matches!(
            command_type,
            "createSession"
                | "createSelectionSideSession"
                | "sendText"
                | "sendGoalCommand"
                | "sendQueuedNow"
        ),
        "rpc.unknownMethod"
        | "rpc.unsupportedMethod"
        | "agent.readOnly"
        | "fault.conversation.staleRevision"
        | "fault.conversation.targetNotFound"
        | "fault.command.targetNotFound"
        | "fault.workflow.runNotFound"
        | "fault.command.stop.notRunning"
        | "fault.command.sendQueuedNow.sessionBusy"
        | "fault.command.savedWorkflowStartRejected.session_busy"
        | "fault.command.savedWorkflowStartRejected.start_failed"
        | "fault.command.workflowRunSettingsRejected.not_found"
        | "fault.command.workflowRunSettingsRejected.not_configurable"
        | "fault.command.workflowRunSettingsRejected.unchanged"
        | "fault.command.workflowRunSettingsRejected.model_unavailable"
        | "fault.command.workflowRunSettingsRejected.compile_failed"
        | "fault.command.workflowRunSettingsRejected.missing_boundaries" => true,
        _ => false,
    }
}

fn rejected_command_ack(command_id: &str, error: &RpcError, revision: u64) -> Value {
    json!({
        "commandId": command_id,
        "status": "rejected",
        "reasonCode": error.code(),
        "message": error.message(),
        "revisionAtDecision": revision,
    })
}

/// 旧 Session facade 可能没有携带 operationId；用完整请求体派生稳定收据，
/// 让同一重试可重放，而不同模型、思考档位或权限模式不会共享固定 fallback。
fn legacy_operation_id(args: &Value, fallback: &str) -> String {
    if let Some(operation_id) = protocol::optional_string_field(args, "commandId")
        .or_else(|| protocol::optional_string_field(args, "operationId"))
    {
        return operation_id;
    }
    let body = serde_json::to_vec(args).unwrap_or_default();
    let digest = Sha256::digest(body);
    format!("{fallback}-{:x}", digest)
}

fn required_string(value: &Value, name: &str) -> Result<String, RpcError> {
    protocol::string_field(value, name).map_err(invalid_params)
}

fn strict_optional_string(value: &Value, name: &str) -> Result<Option<String>, RpcError> {
    let Some(raw) = value.get(name) else {
        return Ok(None);
    };
    let string = raw
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| invalid_params(format!("{name} 必须是非空字符串")))?;
    Ok(Some(string.to_owned()))
}

fn parse_followup_mode(value: String) -> Result<FollowupMode, RpcError> {
    match value.as_str() {
        "queue" => Ok(FollowupMode::Queue),
        "guide" => Ok(FollowupMode::Guide),
        _ => Err(invalid_params("followupMode 必须为 queue 或 guide")),
    }
}

fn reject_unknown_fields(
    value: &Value,
    allowed: &[&str],
    object_name: &str,
) -> Result<(), RpcError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_params(format!("{} 必须为对象", object_name)))?;
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(invalid_params(format!(
            "{} 包含未知字段 {}",
            object_name, field
        )));
    }
    Ok(())
}

fn workflow_scope(value: Option<&Value>) -> Result<&'static str, RpcError> {
    match value.and_then(Value::as_str).unwrap_or("project") {
        "project" => Ok("project"),
        "global" => Ok("global"),
        _ => Err(invalid_params(
            "startSavedWorkflow.scope 必须为 project 或 global",
        )),
    }
}

/// 从当前 Runtime Session 的 Provider Journal 快照生成工作流冻结选择。
///
/// workflow host 只接收这个无凭据选择值；Provider 的配置指纹、凭据和解析逻辑仍由
/// AgentRuntime 持有。没有已持久化 Provider 的 Session 拒绝启动，防止运行中默认模型
/// 改变后工作流的实际模型与起始快照不一致。
fn frozen_model_selection(session: &keencode_runtime::RuntimeSession) -> Result<Value, RpcError> {
    let provider = session
        .snapshot()
        .map_err(backend_error)?
        .state
        .provider
        .ok_or_else(|| {
            RpcError::new(
                "fault.command.savedWorkflowStartRejected.start_failed",
                "当前 Session 没有可冻结的 Provider 模型选择",
            )
        })?;
    let options = provider
        .reasoning_effort
        .map(|effort| {
            serde_json::to_value(effort)
                .map(|value| json!({"reasoningLevel": value}))
                .map_err(|error| backend_error(error.to_string()))
        })
        .transpose()?;
    Ok(json!({
        "providerId": provider.provider_id,
        "modelId": provider.model,
        "options": options,
    }))
}

fn runtime_workflow_model_selection(
    runtime: &AgentRuntime,
    session: &keencode_runtime::RuntimeSession,
    session_id: &str,
) -> Result<Value, RpcError> {
    let snapshot = session.snapshot().map_err(backend_error)?;
    let provider = runtime
        .workflow_provider_snapshot(session_id)
        .map_err(backend_error)?;
    Ok(json!({
        "provider": provider,
        "planEnabled": snapshot.state.plan.enabled,
    }))
}

fn runtime_workflow_model_selection_for_reference(
    runtime: &AgentRuntime,
    session: &keencode_runtime::RuntimeSession,
    session_id: &str,
    reference: &str,
) -> Result<Value, RpcError> {
    let (provider_id, model_and_reasoning) = reference.split_once('/').ok_or_else(|| {
        RpcError::new(
            "fault.command.workflowRunSettingsRejected.model_unavailable",
            "subagentModel 必须使用 providerId/modelId 规范串",
        )
    })?;
    let (model, reasoning) = model_and_reasoning
        .split_once('$')
        .map_or((model_and_reasoning, None), |(model, reasoning)| {
            (model, Some(reasoning))
        });
    if provider_id.is_empty()
        || model.is_empty()
        || provider_id.chars().any(char::is_control)
        || model.chars().any(char::is_control)
        || reasoning.is_some_and(|value| value.is_empty() || value.chars().any(char::is_control))
    {
        return Err(RpcError::new(
            "fault.command.workflowRunSettingsRejected.model_unavailable",
            "subagentModel 不是有效的规范模型串",
        ));
    }
    let snapshot = session.snapshot().map_err(backend_error)?;
    let provider = runtime
        .workflow_provider_snapshot_for_selection(session_id, provider_id, model, reasoning)
        .map_err(|_| {
            RpcError::new(
                "fault.command.workflowRunSettingsRejected.model_unavailable",
                "请求的子代理模型未在当前 Provider 注册表中启用",
            )
        })?;
    Ok(json!({
        "provider": provider,
        "planEnabled": snapshot.state.plan.enabled,
    }))
}

fn frozen_workflow_model_selection(
    session: &keencode_runtime::RuntimeSession,
    run_id: &str,
) -> Result<Value, RpcError> {
    let event = RuntimeWorkflowJournal::new(session.clone())
        .all_events(run_id)
        .map_err(backend_error)?
        .into_iter()
        .find(|event| event.event_type == "run-started")
        .ok_or_else(|| {
            RpcError::new(
                "fault.command.workflowRunSettingsRejected.missing_boundaries",
                "工作流运行缺少冻结启动事实",
            )
        })?;
    event
        .payload
        .get("models")
        .cloned()
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            RpcError::new(
                "fault.command.workflowRunSettingsRejected.missing_boundaries",
                "工作流运行缺少冻结模型快照",
            )
        })
}

fn ensure_owned_workflow_run(
    session: &keencode_runtime::RuntimeSession,
    run_id: &str,
) -> Result<(), RpcError> {
    let journal = RuntimeWorkflowJournal::new(session.clone());
    let owned = journal
        .run_summary(run_id)
        .map_err(backend_error)?
        .is_some();
    if owned {
        Ok(())
    } else {
        Err(RpcError::new(
            "fault.workflow.runNotFound",
            "工作流运行不存在或不属于当前 Session",
        ))
    }
}

fn workflow_result_string(value: &Value, camel: &str, snake: &str) -> Result<String, RpcError> {
    value
        .get(camel)
        .or_else(|| value.get(snake))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            RpcError::new(
                "fault.workflow.invalidHostResult",
                format!("WorkflowHost 返回缺少 {camel}"),
            )
        })
}

fn optional_string(value: &Value, name: &str) -> Result<Option<String>, RpcError> {
    if value.get(name).is_some() && value.get(name).and_then(Value::as_str).is_none() {
        return Err(invalid_params(format!("{name} 必须是字符串")));
    }
    Ok(protocol::optional_string_field(value, name))
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new("rpc.invalidParams", message)
}

fn stale_revision(message: impl Into<String>) -> RpcError {
    RpcError::new("fault.conversation.staleRevision", message)
}

fn backend_error(message: impl std::fmt::Display) -> RpcError {
    RpcError::new("session.backendError", message.to_string())
}

/// 命令阶段失败时只记录固定阶段名和安全错误码；不把正文、项目路径、附件、
/// Provider 或连接标识写入桌面诊断，避免网关把敏感输入带出 RPC 错误边界。
fn log_send_stage_failure(stage: &'static str, error_code: &str) {
    tracing::error!(
        target: "keencode_diagnostics",
        stage,
        error_code,
        "sendConversationCommandV4 阶段失败"
    );
}

/// 记录事件回调失败的协议元数据，不写入 payload、工作区路径或连接身份。
/// 回调错误不能被当作成功投递，否则前端只能在 recovery timeout 后才暴露故障。
fn log_event_delivery_failure(event: &str, error: &RpcError) {
    tracing::warn!(
        target: "keencode_diagnostics",
        channel = AGENT_CHANNEL,
        event,
        code = error.code(),
        "frontend_rpc 事件投递失败"
    );
}

fn state_error() -> RpcError {
    RpcError::new("session.stateUnavailable", "会话网关状态不可用")
}

fn unsupported_command(method: &str, message: &str) -> RpcError {
    RpcError::new("rpc.unsupportedMethod", format!("{method}: {message}"))
}

fn unknown_channel(channel: &str) -> RpcError {
    RpcError::new("rpc.unknownChannel", format!("未知会话 channel: {channel}"))
}

fn retry_targets_subscription(
    subscription: &ActiveSubscription,
    retry: &RuntimeModelRetryScheduled,
) -> bool {
    if subscription.kind != "conversation" {
        return false;
    }
    let expected_agent = subscription
        .agent_id
        .as_ref()
        .map(|agent_id| agent_id.as_str())
        .unwrap_or(ROOT_AGENT_ID);
    expected_agent == retry.source_agent_id
}

/// Retry transient 只能投影到当前仍在运行、且来源 Agent 完全相同的权威 Turn。
/// 这条检查同时隔离父会话与 virtual child view，并在终态竞态下丢弃迟到通知。
fn retry_is_active(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    subscription: &ActiveSubscription,
    retry: &RuntimeModelRetryScheduled,
) -> bool {
    let Some(session_id) = subscription.session_id.as_deref() else {
        return false;
    };
    let Ok(turn_id) = keencode_resources::TurnId::new(retry.turn_id.clone()) else {
        return false;
    };
    let Ok(session) = open_authorized_session(runtime, app, session_id) else {
        return false;
    };
    session
        .read_state(|state| {
            state.turns.get(&turn_id).is_some_and(|turn| {
                turn.status == TurnStatus::Running
                    && turn.source_agent_id.as_str() == retry.source_agent_id
            })
        })
        .unwrap_or(false)
}

fn active_retry_turn_id(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    subscription: &ActiveSubscription,
) -> Option<String> {
    if subscription.kind != "conversation" {
        return None;
    }
    let session_id = subscription.session_id.as_deref()?;
    let expected_agent = subscription
        .agent_id
        .as_ref()
        .map(|agent_id| agent_id.as_str())
        .unwrap_or(ROOT_AGENT_ID);
    let session = open_authorized_session(runtime, app, session_id).ok()?;
    session
        .model_retry_for_active_agent(expected_agent)
        .ok()
        .flatten()
        .filter(|retry| retry.source_agent_id == expected_agent)
        .map(|retry| retry.turn_id)
}

fn retry_state_value(retry: &RuntimeModelRetryScheduled) -> Value {
    json!({
        "attempt": retry.attempt,
        "maxAttempts": retry.max_attempts,
        "nextRetryAt": retry.occurred_at_ms.saturating_add(retry.delay_ms),
        "reasonCode": "provider_retry",
    })
}

fn unknown_method(channel: &str, method: &str) -> RpcError {
    RpcError::new("rpc.unknownMethod", format!("未知方法: {channel}.{method}"))
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_CHANNEL, ActiveSubscription, ConnectionBinding, FrameDeliveryKind, ListenerBinding,
        SessionGateway, SessionGatewayInner, apply_requested_effort,
        command_ack_shape_is_queryable, command_key, command_receipt_query_result,
        has_terminal_turn_for, is_canonical_workflow_command, is_deterministic_command_rejection,
        log_epoch, message_text, plan_enabled_in_payload, retry_state_value,
        retry_targets_subscription, subagent_mention_candidates, subagent_route_context,
        subagent_route_from_resolver, subscription_topic, task_promotion_transition,
        topic_wire_complete, workflow_result_string, workflow_scope, workspace_matches,
        workspace_ref, workspace_task_event_payload,
    };
    use crate::frontend_rpc::workflows;
    use keencode_resources::{
        COMMAND_RECEIPT_SCHEMA, CommandReceipt, CommandReceiptStatus, SessionEvent, SessionStatus,
        TurnId, TurnStopReason,
    };
    use keencode_runtime::RuntimeModelRetryScheduled;
    use serde_json::{Value, json};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::{Arc, Barrier, Mutex, atomic::AtomicU64};
    use std::thread;

    #[test]
    fn requested_effort_blank_is_unset_but_none_is_preserved() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test_with_responses_provider(
                storage.path(),
                "http://127.0.0.1:9/v1",
                "test-model",
            )
            .expect("控制面 Runtime 应创建");
        let session = runtime
            .open_or_create_session(project.path(), None, "requested-effort-boundary")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();

        apply_requested_effort(&runtime, &session_id, "blank-effort", Some(" \t\n"))
            .expect("空白 effort 应表示未覆盖");
        assert!(
            runtime
                .session_snapshot(&session_id)
                .expect("空白 effort 后应可读快照")
                .state
                .provider
                .is_none(),
            "空白 effort 不应提前绑定默认 Provider"
        );

        apply_requested_effort(&runtime, &session_id, "none-effort", Some("none"))
            .expect("显式 none 应是有效档位");
        let provider = runtime
            .session_snapshot(&session_id)
            .expect("none effort 后应可读快照")
            .state
            .provider
            .expect("显式 none 仍应绑定实际 Provider");
        assert_eq!(provider.reasoning_effort, None);

        assert!(
            apply_requested_effort(&runtime, &session_id, "unknown-effort", Some("unsupported"))
                .is_err(),
            "未知非空 effort 必须继续拒绝"
        );
    }

    #[test]
    fn workflow_mutations_require_canonical_v4_command_admission() {
        for command_type in [
            "startSavedWorkflow",
            "cancelBackgroundWork",
            "resumeWorkflowRun",
            "amendWorkflowRunSettings",
        ] {
            assert!(is_canonical_workflow_command(command_type));
            assert!(!workflows::is_workflow_method(command_type));
        }
        for raw_method in [
            "start",
            "run",
            "startWorkflow",
            "workflows.start",
            "cancel",
            "cancelWorkflow",
            "workflows.cancel",
            "resume",
            "resumeWorkflow",
            "workflows.resume",
            "resolveQuestion",
            "resolveWorkflowQuestion",
            "workflows.resolveQuestion",
        ] {
            assert!(!is_canonical_workflow_command(raw_method));
            assert!(!workflows::is_workflow_method(raw_method));
        }
    }

    #[test]
    fn source_subagent_mentions_are_parsed_without_treating_email_as_a_route() {
        assert_eq!(
            subagent_mention_candidates("请使用 @native-user-agent, 完成这项请求"),
            vec!["native-user-agent"]
        );
        assert_eq!(
            subagent_mention_candidates("联系 mail@example.com 后再处理"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn task_created_requires_the_first_persisted_promotion_transition() {
        assert!(!task_promotion_transition(false, false));
        assert!(task_promotion_transition(false, true));
        assert!(!task_promotion_transition(true, true));
    }

    #[test]
    fn task_created_announcement_is_once_per_workspace_and_task() {
        let gateway = SessionGateway::new();
        assert!(
            gateway
                .inner
                .claim_task_created_announcement("C:/workspace", "task-1")
                .unwrap()
        );
        assert!(
            !gateway
                .inner
                .claim_task_created_announcement("C:/workspace", "task-1")
                .unwrap()
        );
        assert!(
            gateway
                .inner
                .claim_task_created_announcement("C:/other", "task-1")
                .unwrap()
        );
    }

    #[test]
    fn legacy_unread_event_keeps_persisted_meta_without_terminal_signal() {
        let event = workspace_task_event_payload(
            r"\\?\C:\repo",
            "task-1",
            "task_meta_changed",
            Some(json!({"taskId": "task-1", "unreadAt": 42})),
            None,
        );
        assert_eq!(event["type"], "workspace_task_list_changed");
        assert_eq!(event["workspacePath"], "C:/repo");
        assert_eq!(event["taskId"], "task-1");
        assert_eq!(event["reason"], "task_meta_changed");
        assert_eq!(event["taskMeta"]["unreadAt"], 42);
        assert!(event.get("unreadSignal").is_none());
    }

    #[test]
    fn selected_subagent_route_is_meta_only_and_keeps_prompt_private() {
        let context = subagent_route_context("native-user-agent");
        assert!(context.contains("spawn_agent"));
        assert!(context.contains("native-user-agent"));
        assert!(context.contains("root agent identity"));
        assert!(!context.contains("NATIVE_USER_AGENT_PROMPT_42"));
        assert!(!context.contains("NATIVE_USER_AGENT_REPLY_42"));
    }

    #[test]
    fn selected_subagent_route_uses_only_an_enabled_catalog_entry() {
        let route = subagent_route_from_resolver(
            "请先看 @disabled-agent，再交给 @native-user-agent 处理。",
            |candidate| Ok((candidate == "native-user-agent").then(|| candidate.to_owned())),
        )
        .expect("目录解析不应失败")
        .expect("已启用 Agent 应产生路由提示");

        assert!(route.contains("agent` parameter set to \"native-user-agent\""));
        assert_eq!(
            subagent_route_from_resolver("请处理 @missing-agent。", |_| Ok(None)).unwrap(),
            None
        );
    }

    #[test]
    fn command_dedupe_key_is_session_scoped() {
        assert_ne!(
            command_key("session:session-a", "same"),
            command_key("session:session-b", "same")
        );
        assert_eq!(
            command_key("session:session", "same"),
            command_key("session:session", "same")
        );
    }

    #[test]
    fn command_query_uses_source_ack_or_unknown_contract() {
        let ack = json!({
            "commandId": "command-1",
            "status": "rejected",
            "reasonCode": "rpc.invalidParams",
            "revisionAtDecision": 7,
        });
        let receipt = |status| CommandReceipt {
            schema: COMMAND_RECEIPT_SCHEMA.to_owned(),
            scope: "session:session-1".to_owned(),
            command_id: "command-1".to_owned(),
            command_type: "renameSession".to_owned(),
            payload_sha256: "0".repeat(64),
            status,
        };
        assert_eq!(
            command_receipt_query_result(&receipt(CommandReceiptStatus::Rejected {
                ack: ack.clone(),
            })),
            ack
        );
        assert_eq!(
            command_receipt_query_result(&receipt(CommandReceiptStatus::Admitted)),
            json!("unknown")
        );
        assert_eq!(
            command_receipt_query_result(&receipt(CommandReceiptStatus::Unknown {
                reason_code: "fault.command.resultUnknown".to_owned(),
            })),
            json!("unknown")
        );
        assert_eq!(
            command_receipt_query_result(&receipt(CommandReceiptStatus::Completed {
                ack: json!({"status": "accepted"}),
            })),
            json!("unknown")
        );
        assert!(command_ack_shape_is_queryable(&ack));
    }

    #[test]
    fn deterministic_command_rejection_does_not_cover_ambiguous_send_stages() {
        let invalid = super::RpcError::new("rpc.invalidParams", "参数无效");
        let stale = super::RpcError::new("fault.conversation.staleRevision", "版本过期");
        let backend = super::RpcError::new("session.backendError", "后端错误");
        assert!(is_deterministic_command_rejection(
            "renameSession",
            &invalid
        ));
        assert!(!is_deterministic_command_rejection("sendText", &invalid));
        assert!(!is_deterministic_command_rejection(
            "createSelectionSideSession",
            &invalid
        ));
        assert!(is_deterministic_command_rejection("sendText", &stale));
        assert!(!is_deterministic_command_rejection("sendText", &backend));
    }

    #[test]
    fn workflow_settings_validation_is_rejected_before_mutation() {
        for reason in [
            "not_found",
            "not_configurable",
            "unchanged",
            "model_unavailable",
            "compile_failed",
            "missing_boundaries",
        ] {
            let error = super::RpcError::new(
                format!("fault.command.workflowRunSettingsRejected.{reason}"),
                "校验失败",
            );
            assert!(is_deterministic_command_rejection(
                "amendWorkflowRunSettings",
                &error
            ));
        }
        let start_failure = super::RpcError::new(
            "fault.command.workflowRunSettingsRejected.start_failed",
            "启动结果未知",
        );
        assert!(!is_deterministic_command_rejection(
            "amendWorkflowRunSettings",
            &start_failure
        ));
    }

    #[test]
    fn log_epoch_is_stable_for_same_workspace_binding() {
        assert_eq!(
            log_epoch("conversation", "s", "w"),
            log_epoch("conversation", "s", "w")
        );
        assert_ne!(
            log_epoch("conversation", "s", "w"),
            log_epoch("conversation", "t", "w")
        );
    }

    #[test]
    fn message_text_reads_only_content_parts() {
        assert_eq!(
            message_text(&json!({"content": [{"text": "a"}, {"content": "b"}]})),
            "ab"
        );
    }

    #[test]
    fn plan_flag_accepts_all_source_v4_payload_locations() {
        assert!(plan_enabled_in_payload(&json!({"planEnabled": true})));
        assert!(plan_enabled_in_payload(&json!({
            "config": {"planEnabled": true}
        })));
        assert!(plan_enabled_in_payload(&json!({
            "firstInput": {"planEnabled": true}
        })));
        assert!(!plan_enabled_in_payload(&json!({
            "config": {"planEnabled": false},
            "firstInput": {"planEnabled": false}
        })));
    }

    #[test]
    fn background_unread_signal_only_matches_recursive_root_turn_terminal_events() {
        let root_turn = TurnId::new("root-turn").unwrap();
        let child_turn = TurnId::new("child-turn").unwrap();
        let event = SessionEvent::AtomicBatch {
            events: vec![SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnStopped {
                        turn_id: child_turn.clone(),
                        reason: TurnStopReason::Cancelled,
                        message: "child stopped".to_owned(),
                    },
                    SessionEvent::TurnCompleted {
                        turn_id: root_turn.clone(),
                    },
                ],
            }],
        };

        assert!(has_terminal_turn_for(&event, &|turn_id| turn_id == &root_turn));
        assert!(has_terminal_turn_for(&event, &|turn_id| turn_id == &child_turn));
        assert!(!has_terminal_turn_for(
            &SessionEvent::SessionStatusChanged {
                status: SessionStatus::Idle,
            },
            &|turn_id| turn_id == &root_turn,
        ));
        assert!(!has_terminal_turn_for(
            &SessionEvent::TurnStopped {
                turn_id: child_turn,
                reason: TurnStopReason::Failed,
                message: "child failed".to_owned(),
            },
            &|turn_id| turn_id == &root_turn,
        ));
    }

    #[test]
    fn workflow_scope_defaults_to_project_and_rejects_unknown_values() {
        assert_eq!(workflow_scope(None).unwrap(), "project");
        assert_eq!(workflow_scope(Some(&json!("global"))).unwrap(), "global");
        assert!(workflow_scope(Some(&json!("workspace"))).is_err());
    }

    #[test]
    fn workflow_result_requires_a_non_empty_stable_run_id() {
        assert_eq!(
            workflow_result_string(&json!({"runId": "run-1"}), "runId", "run_id").unwrap(),
            "run-1"
        );
        assert_eq!(
            workflow_result_string(&json!({"run_id": "run-2"}), "runId", "run_id").unwrap(),
            "run-2"
        );
        assert!(workflow_result_string(&json!({"runId": ""}), "runId", "run_id").is_err());
    }

    #[test]
    fn subscribe_topic_uses_workspace_identity_for_index_topics() {
        assert_eq!(
            subscription_topic(
                "subscribeSessionsIndexV4",
                &json!({
                    "workspacePath": "C:/repo",
                    "workspaceIdentity": "local:C:/repo"
                })
            )
            .unwrap(),
            "sessions-index/local:C:/repo"
        );
        assert_eq!(
            subscription_topic(
                "subscribeConversationV4",
                &json!({"sessionId": "session-1", "workspacePath": "C:/repo"})
            )
            .unwrap(),
            "conversation/session-1"
        );
    }

    #[test]
    fn workspace_presentation_ref_keeps_stable_identity_fields() {
        assert_eq!(
            workspace_ref(
                &json!({
                    "workspaceIdentity": "ssh:repo",
                    "remoteSessionId": "remote-1"
                }),
                "C:/repo"
            )
            .unwrap(),
            json!({
                "workspacePath": "C:/repo",
                "workspaceKey": "ssh:repo",
                "workspaceIdentity": "ssh:repo",
                "remoteSessionId": "remote-1"
            })
        );
    }

    #[test]
    fn workspace_matching_accepts_windows_path_aliases_without_widening_scope() {
        assert!(workspace_matches(Some(r"\\?\C:\repo\"), r"C:/repo"));
        assert!(workspace_matches(Some(r"C:\repo\"), r"c:/repo"));
        assert!(workspace_matches(
            Some(r"\\server\share\repo"),
            r"//server/share/repo"
        ));
        assert!(!workspace_matches(Some(r"C:\repo\"), r"C:/other"));
    }

    #[test]
    fn concurrent_pending_subscriptions_require_an_exact_ack_identity() {
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([
                ("sub-1".to_owned(), pending_test_subscription("sub-1")),
                ("sub-2".to_owned(), pending_test_subscription("sub-2")),
            ])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };
        let args = json!({
            "topic": "conversation/session-1",
            "workspacePath": "C:/repo"
        });
        assert_eq!(
            inner.subscription_id_for_response(
                "connection",
                "subscribeConversationV4",
                &args,
                &Value::Null,
            ),
            None,
            "同 topic 并发 ACK 不能靠请求参数猜测"
        );
        let exact = json!({"ack": {"subscriptionId": "sub-2"}});
        assert_eq!(
            inner.subscription_id_for_response(
                "connection",
                "subscribeConversationV4",
                &args,
                &exact,
            ),
            Some("sub-2".to_owned())
        );
        inner.activate_subscription("sub-2");
        let subscriptions = inner.subscriptions.lock().unwrap();
        assert!(!subscriptions["sub-1"].activated);
        assert!(subscriptions["sub-2"].activated);
    }

    #[test]
    fn out_of_order_ack_responses_activate_the_matching_subscription() {
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([
                ("sub-1".to_owned(), pending_test_subscription("sub-1")),
                ("sub-2".to_owned(), pending_test_subscription("sub-2")),
            ])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };
        let args = json!({
            "topic": "conversation/session-1",
            "workspacePath": "C:/repo"
        });

        // sub-2 的响应先到达，随后 sub-1 到达；每个 ACK 都必须只打开自身首帧。
        for subscription_id in ["sub-2", "sub-1"] {
            let response = json!({"ack": {"subscriptionId": subscription_id}});
            let selected = inner.subscription_id_for_response(
                "connection",
                "subscribeConversationV4",
                &args,
                &response,
            );
            assert_eq!(selected.as_deref(), Some(subscription_id));
            inner.activate_subscription(subscription_id);
        }

        let subscriptions = inner.subscriptions.lock().unwrap();
        assert!(
            subscriptions
                .values()
                .all(|subscription| subscription.activated)
        );
    }

    #[test]
    fn topic_wire_complete_contains_v4_delivery_metadata() {
        let frame = json!({
            "topic": "conversation/session-1",
            "subscriptionId": "sub-1",
            "fromSeq": 0,
            "toSeq": 4,
            "sentAt": 1,
            "payload": {"kind": "snapshot", "snapshot": {"seq": 4}},
        });
        let wire = topic_wire_complete(
            "conversation/session-1",
            "sub-1",
            frame.clone(),
            1,
            FrameDeliveryKind::Initial,
        );
        assert_eq!(wire["wireVersion"], json!(3));
        assert_eq!(wire["kind"], json!("complete"));
        assert_eq!(wire["deliveryKind"], json!("initial"));
        assert_eq!(wire["logicalFrameId"], json!("sub-1:1"));
        assert_eq!(wire["logicalFrameOrdinal"], json!(1));
        assert_eq!(wire["topic"], json!("conversation/session-1"));
        assert_eq!(wire["subscriptionId"], json!("sub-1"));
        assert_eq!(wire["frame"], frame);
    }

    #[test]
    fn api_retry_projection_keeps_agent_scope_and_protocol_shape() {
        let retry = RuntimeModelRetryScheduled {
            turn_id: "turn-root".to_owned(),
            source_agent_id: "root".to_owned(),
            attempt: 3,
            max_attempts: 10,
            delay_ms: 500,
            occurred_at_ms: 1_000,
            message: "不进入 V4 状态字段".to_owned(),
        };
        let root = pending_test_subscription("retry-root");
        assert!(retry_targets_subscription(&root, &retry));
        assert_eq!(retry_state_value(&retry)["attempt"], json!(3));
        assert_eq!(retry_state_value(&retry)["maxAttempts"], json!(10));
        assert_eq!(retry_state_value(&retry)["nextRetryAt"], json!(1_500));
        assert_eq!(
            retry_state_value(&retry)["reasonCode"],
            json!("provider_retry")
        );

        let mut child = pending_test_subscription("retry-child");
        child.agent_id = Some(keencode_resources::AgentId::new("child-a").unwrap());
        assert!(!retry_targets_subscription(&child, &retry));
        let child_retry = RuntimeModelRetryScheduled {
            source_agent_id: "child-a".to_owned(),
            ..retry
        };
        assert!(retry_targets_subscription(&child, &child_retry));
    }

    #[test]
    fn activation_emits_initial_wire_before_pending_online_frames() {
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let callback_received = Arc::clone(&received);
        let leaked = Arc::new(Mutex::new(Vec::<Value>::new()));
        let leaked_callback = Arc::clone(&leaked);
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::from([
                (
                    1,
                    ListenerBinding {
                        connection_id: "connection".to_owned(),
                        channel: AGENT_CHANNEL.to_owned(),
                        event: "onDynamicConversationFrame".to_owned(),
                        // subscribe stores the canonical extended-length spelling on Windows;
                        // the listener must still receive frames registered with the short form.
                        workspace_path: Some(r"\\?\C:\repo\".to_owned()),
                        callback: Arc::new(move |value| {
                            callback_received.lock().unwrap().push(value);
                            Ok(())
                        }),
                    },
                ),
                (
                    2,
                    ListenerBinding {
                        connection_id: "other-connection".to_owned(),
                        channel: AGENT_CHANNEL.to_owned(),
                        event: "onDynamicConversationFrame".to_owned(),
                        workspace_path: Some("C:/repo".to_owned()),
                        callback: Arc::new(move |value| {
                            leaked_callback.lock().unwrap().push(value);
                            Ok(())
                        }),
                    },
                ),
            ])),
            subscriptions: Mutex::new(HashMap::from([(
                "sub-1".to_owned(),
                ActiveSubscription {
                    initial_frame: Some(json!({
                        "topic": "conversation/session-1",
                        "subscriptionId": "sub-1",
                        "fromSeq": 0,
                        "toSeq": 1,
                        "sentAt": 1,
                        "payload": {"kind": "snapshot", "snapshot": {"seq": 1}},
                    })),
                    pending_frames: vec![json!({
                        "topic": "conversation/session-1",
                        "subscriptionId": "sub-1",
                        "fromSeq": 1,
                        "toSeq": 2,
                        "sentAt": 2,
                        "payload": {"kind": "deltas", "deltas": []},
                    })],
                    ..pending_test_subscription("sub-1")
                },
            )])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };

        inner.activate_subscription("sub-1");

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0]["deliveryKind"], json!("initial"));
        assert_eq!(received[1]["deliveryKind"], json!("online"));
        assert_eq!(received[0]["logicalFrameOrdinal"], json!(1));
        assert_eq!(received[1]["logicalFrameOrdinal"], json!(2));
        assert!(leaked.lock().unwrap().is_empty());
    }

    #[test]
    fn snapshot_normalization_does_not_regress_projection_watermark() {
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([(
                "sub-1".to_owned(),
                ActiveSubscription {
                    seq: 41,
                    ..pending_test_subscription("sub-1")
                },
            )])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };
        let frame = json!({
            "topic": "conversation/session-1",
            "subscriptionId": "sub-1",
            "fromSeq": 0,
            "toSeq": 6,
            "sentAt": 1,
            "payload": {"kind": "snapshot", "snapshot": {"seq": 6}},
        });

        let normalized = inner.normalize_snapshot_frame("sub-1", frame, true);
        assert_eq!(normalized["fromSeq"], json!(0));
        assert_eq!(normalized["toSeq"], json!(42));
        assert_eq!(normalized["payload"]["snapshot"]["seq"], json!(42));
        assert_eq!(inner.subscriptions.lock().unwrap()["sub-1"].seq, 42);
    }

    #[test]
    fn frame_gate_keeps_stale_snapshot_before_new_transient_payload() {
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let callback_received = Arc::clone(&received);
        let mut subscription = pending_test_subscription("sub-1");
        subscription.activated = true;
        subscription.initial_frame = None;
        subscription.seq = 1;
        let inner = Arc::new(SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::from([(
                1,
                ListenerBinding {
                    connection_id: "connection".to_owned(),
                    channel: AGENT_CHANNEL.to_owned(),
                    event: "onDynamicConversationFrame".to_owned(),
                    workspace_path: Some("C:/repo".to_owned()),
                    callback: Arc::new(move |value| {
                        callback_received.lock().unwrap().push(value);
                        Ok(())
                    }),
                },
            )])),
            subscriptions: Mutex::new(HashMap::from([("sub-1".to_owned(), subscription)])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        });
        let stale_snapshot = json!({
            "topic": "conversation/session-1",
            "subscriptionId": "sub-1",
            "fromSeq": 0,
            "toSeq": 1,
            "sentAt": 1,
            "payload": {
                "kind": "snapshot",
                "snapshot": {"seq": 1, "rows": [{"text": "stale"}]}
            }
        });
        let transient_delta = json!({
            "topic": "conversation/session-1",
            "subscriptionId": "sub-1",
            "fromSeq": 2,
            "toSeq": 3,
            "sentAt": 2,
            "payload": {
                "kind": "deltas",
                "deltas": [{"op": "row.delta", "rowId": 1, "path": "text", "append": "new"}]
            }
        });
        let snapshot_started = Arc::new(Barrier::new(2));
        let release_snapshot = Arc::new(Barrier::new(2));
        let snapshot_inner = Arc::clone(&inner);
        let snapshot_started_by_thread = Arc::clone(&snapshot_started);
        let release_snapshot_by_thread = Arc::clone(&release_snapshot);
        let snapshot_thread = thread::spawn(move || {
            snapshot_inner
                .with_frame_gate("sub-1", || {
                    // 模拟已经读取到旧 Journal 快照但尚未完成 stamp；transient
                    // 线程必须等待整个 snapshot payload 事务，而不是只等编号。
                    snapshot_started_by_thread.wait();
                    release_snapshot_by_thread.wait();
                    let frame =
                        snapshot_inner.normalize_snapshot_frame("sub-1", stale_snapshot, true);
                    snapshot_inner.queue_or_emit_frame("sub-1", frame, FrameDeliveryKind::Online);
                })
                .expect("snapshot frame gate should be available");
        });

        snapshot_started.wait();
        let transient_inner = Arc::clone(&inner);
        let transient_thread = thread::spawn(move || {
            transient_inner
                .with_frame_gate("sub-1", || {
                    let from_seq = {
                        let mut subscriptions = transient_inner.subscriptions.lock().unwrap();
                        let subscription = subscriptions.get_mut("sub-1").unwrap();
                        let from_seq = subscription.seq;
                        subscription.seq = from_seq.saturating_add(1);
                        from_seq
                    };
                    assert_eq!(from_seq, 2);
                    transient_inner.queue_or_emit_frame(
                        "sub-1",
                        transient_delta,
                        FrameDeliveryKind::Online,
                    );
                })
                .expect("transient frame gate should be available");
        });
        release_snapshot.wait();
        snapshot_thread.join().unwrap();
        transient_thread.join().unwrap();

        let frames = received.lock().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["logicalFrameOrdinal"], json!(1));
        assert_eq!(
            frames[0]["frame"]["payload"]["snapshot"]["rows"][0]["text"],
            json!("stale")
        );
        assert_eq!(frames[0]["frame"]["toSeq"], json!(2));
        assert_eq!(frames[1]["logicalFrameOrdinal"], json!(2));
        assert_eq!(frames[1]["frame"]["fromSeq"], json!(2));
        assert_eq!(frames[1]["frame"]["toSeq"], json!(3));
        assert_eq!(
            frames[1]["frame"]["payload"]["deltas"][0]["append"],
            json!("new")
        );
    }

    #[test]
    fn non_conversation_snapshot_gets_a_wire_watermark_without_seq_field() {
        let mut subscription = pending_test_subscription("sub-1");
        subscription.kind = "sessions-index".to_owned();
        subscription.topic = "sessions-index/workspace".to_owned();
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([("sub-1".to_owned(), subscription)])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };
        let frame = json!({
            "topic": "sessions-index/workspace",
            "subscriptionId": "sub-1",
            "fromSeq": 0,
            "toSeq": 0,
            "sentAt": 1,
            "payload": {
                "kind": "snapshot",
                "snapshot": {"protocolVersion": 1, "workspaceId": "workspace", "logEpoch": "epoch", "sessions": []}
            },
        });

        let normalized = inner.normalize_snapshot_frame("sub-1", frame, true);
        assert_eq!(normalized["toSeq"], json!(1));
        assert!(normalized["payload"]["snapshot"].get("seq").is_none());
    }

    #[test]
    fn wire_watermark_follows_connection_and_session_ownership() {
        let mut first = pending_test_subscription("sub-1");
        first.seq = 6;
        let mut second = pending_test_subscription("sub-2");
        second.seq = 41;
        second.connection_id = "other-connection".to_owned();
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([
                ("sub-1".to_owned(), first),
                ("sub-2".to_owned(), second),
            ])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };

        assert_eq!(
            inner.wire_watermark_for_session("connection", "session-1"),
            Some(6)
        );
        assert_eq!(
            inner.wire_watermark_for_session("other-connection", "session-1"),
            Some(41)
        );
        assert_eq!(
            inner.wire_watermark_for_session("connection", "unknown"),
            None
        );
    }

    #[test]
    fn interaction_ack_uses_the_initialized_client_identity() {
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::from([(
                "connection".to_owned(),
                ConnectionBinding {
                    initialized: true,
                    client_id: Some("renderer-client".to_owned()),
                    ..ConnectionBinding::default()
                },
            )])),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::new()),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::new()),
            command_order: Mutex::new(VecDeque::new()),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };

        assert_eq!(inner.client_id("connection").unwrap(), "renderer-client");
        assert!(inner.client_id("other-connection").is_err());
    }

    #[test]
    fn dispose_connection_clears_ephemeral_gateway_state() {
        let inner = SessionGatewayInner {
            connections: Mutex::new(HashMap::from([(
                "connection".to_owned(),
                ConnectionBinding::default(),
            )])),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::from([(
                "sub-1".to_owned(),
                pending_test_subscription("sub-1"),
            )])),
            snapshot_reads: Mutex::new(()),
            command_results: Mutex::new(HashMap::from([(
                "session:session-1\0cmd-1".to_owned(),
                json!({"status": "accepted"}),
            )])),
            command_order: Mutex::new(VecDeque::from(["session:session-1\0cmd-1".to_owned()])),
            task_created_announcements: Mutex::new(HashSet::new()),
            workflow_call: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
        };

        assert!(
            inner
                .subscription_for_unsubscribe("sub-1", "connection")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            inner
                .subscription_for_unsubscribe("sub-1", "other-connection")
                .err()
                .unwrap()
                .code(),
            "fault.subscription.notOwned"
        );

        inner.dispose_connection("connection");

        assert!(inner.connections.lock().unwrap().is_empty());
        assert!(inner.subscriptions.lock().unwrap().is_empty());
        assert!(super::released_subscription_belongs_to(
            "sub-1",
            "connection"
        ));
        assert!(!super::released_subscription_belongs_to(
            "sub-1",
            "other-connection"
        ));
        // 迟到的同连接 unsubscribe 必须像正常 close 一样幂等成功；不同连接即使
        // 知道这个 subscriptionId 也不能借 tombstone 释放或确认它。
        assert!(
            inner
                .subscription_for_unsubscribe("sub-1", "connection")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            inner
                .subscription_for_unsubscribe("sub-1", "other-connection")
                .err()
                .unwrap()
                .code(),
            "fault.subscription.notOwned"
        );
        assert!(inner.command_results.lock().unwrap().is_empty());
        assert!(inner.command_order.lock().unwrap().is_empty());
    }

    #[test]
    fn concurrent_subscription_release_keeps_same_owner_tombstone_idempotent() {
        let gateway = SessionGateway::new();
        let inner = Arc::clone(&gateway.inner);
        let subscription_id = "sub-concurrent-release-test";
        inner.subscriptions.lock().unwrap().insert(
            subscription_id.to_owned(),
            pending_test_subscription(subscription_id),
        );

        let barrier = Arc::new(Barrier::new(3));
        let releasers = (0..2)
            .map(|_| {
                let inner = Arc::clone(&inner);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    inner.remove_subscription(subscription_id);
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for releaser in releasers {
            releaser.join().unwrap();
        }

        assert!(
            inner
                .subscription_for_unsubscribe(subscription_id, "connection")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            inner
                .subscription_for_unsubscribe(subscription_id, "other-connection")
                .err()
                .unwrap()
                .code(),
            "fault.subscription.notOwned"
        );
    }

    fn pending_test_subscription(subscription_id: &str) -> ActiveSubscription {
        ActiveSubscription {
            connection_id: "connection".to_owned(),
            topic: "conversation/session-1".to_owned(),
            kind: "conversation".to_owned(),
            identity: "session-1".to_owned(),
            workspace_path: "C:/repo".to_owned(),
            session_id: Some("session-1".to_owned()),
            agent_id: None,
            log_epoch: "epoch".to_owned(),
            seq: 0,
            journal_seq: 0,
            subscription_id: subscription_id.to_owned(),
            frame_gate: Arc::new(Mutex::new(())),
            wire_ordinal: 0,
            initial_frame: Some(json!({"subscriptionId": subscription_id})),
            pending_frames: Vec::new(),
            transient: super::TransientProjection::default(),
            api_retry_turn_id: None,
            activated: false,
            abort: None,
        }
    }
}
