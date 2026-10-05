//! 桌面 Runtime 的标准 ACP 结构化问答桥。

use crate::agent_runtime::{AppRuntimePreferences, ClientRequestRouter};
use crate::client_request::{
    ClientRequestDisplayGate, ClientRequestDisplayPermit, ClientRequestSink,
};
use keencode_acp::schema::{
    ClientCapabilities, CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
    ElicitationContentValue, ElicitationFormMode, ElicitationSchema, ElicitationSessionScope,
    EnumOption, Meta, MultiSelectPropertySchema, RequestId, StringPropertySchema,
};
use keencode_acp::{
    AcpClientRequestEncoder, AcpClientRequestFrame, AcpResponseDecoder, ConnectionId,
    ElicitationRouter,
};
use keencode_agent::{CollaborationIdGenerator, UuidCollaborationIdGenerator};
use keencode_tools::{
    UserQuestion, UserQuestionAnswer, UserQuestionError, UserQuestionFuture, UserQuestionHandler,
    UserQuestionRequest, UserQuestionResponse,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};
use tokio::task::AbortHandle;
use tokio::time::Duration;

/// AskUser 自动继续的产品时限；只有 App 偏好开启时才会创建该计时器。
const ASK_USER_AUTO_RESOLUTION_TOTAL: Duration = Duration::from_secs(5 * 60);
/// 倒计时对用户可见前保留的静默宽限期；剩余四分钟沿用 UI 的进度动画。
const ASK_USER_AUTO_RESOLUTION_HIDDEN_GRACE: Duration = Duration::from_secs(60);
/// 与 shared `ASK_USER_QUESTION_E2E_CLOCK_SCALE_ENV` 同名的 native 测试时钟缩放变量。
const ASK_USER_QUESTION_E2E_CLOCK_SCALE_ENV: &str = "ZCODE_E2E_ASK_USER_QUESTION_CLOCK_SCALE";

/// 标准 ACP `_meta` 中承载 KeenCode 交互扩展的唯一命名空间。
const KEENCODE_META_KEY: &str = "_keencode";

/// Elicitation 注册、投递或响应违反桌面桥不变量时返回的稳定错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElicitationBridgeError {
    /// Client 尚未协商能力，或重复握手试图改变已固定的能力。
    CapabilitiesUnavailable,
    /// Runtime 已关闭，不再接受新问答。
    RuntimeClosed,
    /// Handler 收到了其他 Session 的请求。
    SessionMismatch,
    /// 同一 Session 已经存在一个未结束的可见问答。
    SessionBusy,
    /// 标准 ACP 请求无法构造或越过资源边界。
    RegistrationRejected,
    /// 当前 Session 的 Client Request 无法送达桌面。
    DeliveryUnavailable,
    /// Client 返回的完整 JSON-RPC 响应不符合 Elicitation DTO。
    InvalidResponse,
    /// 响应携带的请求标识不存在或已经结束。
    UnknownRequest,
    /// 响应来自非请求目标连接。
    ResponseConnectionMismatch,
    /// 响应到达时请求尚未被投递泵确认送达。
    RequestNotDelivered,
    /// 用户拒绝或取消了本次问答。
    Cancelled,
    /// 内部等待通道或状态不变量被破坏。
    InternalState,
}

impl std::fmt::Display for ElicitationBridgeError {
    /// 输出不包含问题正文、选项或回答的安全错误说明。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapabilitiesUnavailable => {
                formatter.write_str("ACP 问答能力尚未协商或与既有连接不一致")
            }
            Self::RuntimeClosed => formatter.write_str("问答 Runtime 已关闭"),
            Self::SessionMismatch => formatter.write_str("问答请求与当前 Session 不匹配"),
            Self::SessionBusy => formatter.write_str("当前 Session 已有待回答问题"),
            Self::RegistrationRejected => formatter.write_str("问答请求无法安全登记"),
            Self::DeliveryUnavailable => formatter.write_str("问答请求无法送达桌面"),
            Self::InvalidResponse => formatter.write_str("ACP 问答响应无效"),
            Self::UnknownRequest => formatter.write_str("ACP 待决问答不存在或已经结束"),
            Self::ResponseConnectionMismatch => formatter.write_str("ACP 问答响应来自非目标连接"),
            Self::RequestNotDelivered => formatter.write_str("ACP 待决问答尚未送达桌面"),
            Self::Cancelled => formatter.write_str("用户取消了本次问答"),
            Self::InternalState => formatter.write_str("问答 Runtime 内部状态不一致"),
        }
    }
}

impl std::error::Error for ElicitationBridgeError {}

impl ElicitationBridgeError {
    /// 转成不会泄露问答内容的工具端错误。
    fn into_user_question_error(self) -> UserQuestionError {
        UserQuestionError::new(self.to_string())
    }
}

/// 一个待决问答在 V4 会话投影中发生变化时发出的轻量路由通知。
///
/// 通知不携带问题正文；投影层收到后必须按订阅连接重新读取
/// [`PendingElicitationView`]，避免广播通道成为第二份 pending 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ElicitationChange {
    /// UI 应刷新哪个父 Session；actor 问答使用父 Session 映射。
    pub(crate) display_session_id: String,
}

/// ElicitationCoordinator 对受信 Runtime 投影层提供的只读 pending 视图。
///
/// `session_id` 保留 actor 的真实归属，`display_session_id` 只表示允许展示的父
/// Session。调用方仍必须按连接和 actor→parent 路由复核，不能把此结构直接下发前端。
#[derive(Clone, Debug)]
pub(crate) struct PendingElicitationView {
    /// ACP 请求的稳定 interaction ID。
    pub(crate) request_id: String,
    /// 真实发起问答的 Session，actor 问答不是父 Session。
    pub(crate) session_id: String,
    /// 允许在 V4 中展示问答的父 Session；普通问答为自身 Session。
    pub(crate) display_session_id: Option<String>,
    /// 唯一允许回答该请求的连接。
    pub(crate) connection_id: ConnectionId,
    /// 原始、已由 core/tools 校验的问题 Schema。
    pub(crate) questions: Vec<UserQuestion>,
    /// 问答登记时的墙钟时间，供 V4 `createdAt` 使用。
    pub(crate) created_at_unix_ms: u64,
    /// AskUser 工具调用的真实标识，供 UI 行锚定和诊断使用。
    pub(crate) tool_call_id: String,
    /// 当前请求的绝对倒计时状态；关闭偏好或用户首次操作后为空/暂停。
    pub(crate) auto_resolution: Option<PendingElicitationAutoResolution>,
}

/// V4 `pendingInteraction.autoResolution` 的 Rust 内部值，不作为第二份事实源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingElicitationAutoResolution {
    /// 请求仍在静默宽限或可见倒计时阶段。
    Active {
        started_at_unix_ms: u64,
        visible_at_unix_ms: u64,
        deadline_at_unix_ms: u64,
    },
    /// 用户已经通过弹窗、侧栏或导航暂停本次自动继续。
    Snoozed {
        started_at_unix_ms: u64,
        snoozed_at_unix_ms: u64,
    },
}

/// 一个待决问答从登记到桌面确认送达之间的阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElicitationDeliveryStage {
    /// 请求已经登记，正在等待共享展示许可。
    Dispatching,
    /// 请求已经向桌面投递泵提交，正在等待同步投递回执。
    Emitting,
    /// 串行投递泵已经确认请求送达桌面。
    Delivered,
}

/// 单个待决标准 Elicitation 的不可变绑定和一次性等待者。
struct PendingElicitation {
    /// ACP 请求的稳定 interaction ID。
    request_id: String,
    /// 请求唯一允许投递和响应的 ACP 连接。
    connection_id: ConnectionId,
    /// 请求绑定的唯一 Session。
    session_id: String,
    /// 可选的父 Session；actor 问答的 V4 frame 投影到这里。
    display_session_id: Option<String>,
    /// 用于把结构化响应还原为工具答案的原始问题定义。
    questions: Vec<UserQuestion>,
    /// 问答登记时的墙钟时间。
    created_at_unix_ms: u64,
    /// 触发本次 AskUser 的真实工具调用标识。
    tool_call_id: String,
    /// 仅在 App 偏好开启期间存在的自动继续状态。
    auto_resolution: Option<PendingElicitationAutoResolution>,
    /// 到期自动继续任务；响应、断线、暂停或 Runtime 关闭时必须取消。
    auto_resolution_abort: Option<AbortHandle>,
    /// 当前投递阶段。
    delivery_stage: ElicitationDeliveryStage,
    /// 投递未确认时可取消的异步任务句柄。
    dispatch_abort: Option<AbortHandle>,
    /// 从实际投递前一直持有到响应或取消收口的跨路由展示许可。
    display_permit: Option<ClientRequestDisplayPermit>,
    /// AskUser 工具等待的一次性结果通道。
    waiter: oneshot::Sender<Result<UserQuestionResponse, UserQuestionError>>,
}

/// Elicitation 协调器中不随克隆复制的共享状态。
struct ElicitationCoordinatorInner {
    /// 保护待决映射、Session 占用和关闭状态的短临界区。
    state: Mutex<ElicitationState>,
    /// pending 生命周期通知；载荷只带展示 Session，正文始终从 state 读取。
    changes: broadcast::Sender<ElicitationChange>,
}

/// 一个 Runtime 内全部待决结构化问答。
struct ElicitationState {
    /// 每条已初始化连接不可变的能力协商快照。
    routers: HashMap<ConnectionId, ElicitationRouter>,
    /// 当前 Session 正在执行的 Prompt 所属连接。
    session_connections: HashMap<String, ConnectionId>,
    /// 按字符串 JSON-RPC 标识保存的待决请求。
    pending: HashMap<String, PendingElicitation>,
    /// 每个 Session 当前唯一待决请求，防止前端被并发弹窗覆盖。
    pending_by_session: HashMap<String, String>,
    /// Runtime 是否已经永久停止接受问答。
    closed: bool,
}

/// 跨 Session 共享、只在当前进程保存待决问答的协调器。
#[derive(Clone)]
pub struct ElicitationCoordinator {
    /// 唯一共享的待决状态。
    inner: Arc<ElicitationCoordinatorInner>,
    /// 标准 ACP Client Request 编码器。
    request_encoder: AcpClientRequestEncoder,
    /// 标准 ACP Client Response 严格解码器。
    response_decoder: AcpResponseDecoder,
    /// 持有到响应终态的每 Session 展示串行门。
    client_request_gate: Arc<ClientRequestDisplayGate>,
    /// 与 AgentRuntime 共用的 App 偏好快照，保证设置变更能影响现有 pending。
    app_runtime_preferences: Arc<std::sync::RwLock<AppRuntimePreferences>>,
}

impl ElicitationCoordinator {
    /// 创建不恢复旧问答、等待 Client 协商能力的协调器。
    pub fn new() -> Self {
        Self::with_gate(Arc::new(ClientRequestDisplayGate::new()))
    }

    /// 使用 Runtime 共享展示门创建尚未协商能力的协调器。
    pub(crate) fn with_gate(client_request_gate: Arc<ClientRequestDisplayGate>) -> Self {
        Self::with_gate_and_preferences(
            client_request_gate,
            Arc::new(std::sync::RwLock::new(AppRuntimePreferences::default())),
        )
    }

    /// 使用 Runtime 共享展示门和偏好快照创建协调器。
    pub(crate) fn with_gate_and_preferences(
        client_request_gate: Arc<ClientRequestDisplayGate>,
        app_runtime_preferences: Arc<std::sync::RwLock<AppRuntimePreferences>>,
    ) -> Self {
        Self {
            inner: Arc::new(ElicitationCoordinatorInner {
                state: Mutex::new(ElicitationState {
                    routers: HashMap::new(),
                    session_connections: HashMap::new(),
                    pending: HashMap::new(),
                    pending_by_session: HashMap::new(),
                    closed: false,
                }),
                changes: broadcast::channel(256).0,
            }),
            request_encoder: AcpClientRequestEncoder::new(),
            response_decoder: AcpResponseDecoder::new(),
            client_request_gate,
            app_runtime_preferences,
        }
    }

    /// 为嵌入式桌面连接协商能力；保留单连接调用方兼容入口。
    #[cfg(test)]
    pub fn negotiate_client_capabilities(
        &self,
        capabilities: &ClientCapabilities,
    ) -> Result<(), ElicitationBridgeError> {
        self.negotiate_connection_capabilities(&embedded_desktop_connection(), capabilities)
    }

    /// 首次握手固定指定连接的能力；同连接重复握手必须完全一致。
    pub fn negotiate_connection_capabilities(
        &self,
        connection_id: &ConnectionId,
        capabilities: &ClientCapabilities,
    ) -> Result<(), ElicitationBridgeError> {
        let negotiated = ElicitationRouter::from_client_capabilities(capabilities);
        let mut state = self.inner.state.lock();
        match state.routers.get(connection_id) {
            Some(current) if current != &negotiated => {
                Err(ElicitationBridgeError::CapabilitiesUnavailable)
            }
            Some(_) => Ok(()),
            None => {
                state.routers.insert(connection_id.clone(), negotiated);
                Ok(())
            }
        }
    }

    /// 返回嵌入式桌面连接是否支持表单；保留单连接调用方兼容入口。
    #[cfg(test)]
    pub fn supports_form(&self) -> bool {
        self.supports_form_for_connection(&embedded_desktop_connection())
    }

    /// 仅在指定连接明确协商支持表单时返回 true。
    pub fn supports_form_for_connection(&self, connection_id: &ConnectionId) -> bool {
        self.inner
            .state
            .lock()
            .routers
            .get(connection_id)
            .is_some_and(ElicitationRouter::supports_form)
    }

    /// 将当前 Session 的交互问答绑定到发起本轮 Prompt 的连接。
    pub fn bind_session_connection(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
    ) -> Result<(), ElicitationBridgeError> {
        let mut state = self.inner.state.lock();
        if !state.routers.contains_key(connection_id) {
            return Err(ElicitationBridgeError::CapabilitiesUnavailable);
        }
        state
            .session_connections
            .insert(session_id.to_owned(), connection_id.clone());
        Ok(())
    }

    /// 返回当前 Session 绑定连接是否声明表单问答能力。
    pub fn session_supports_form(&self, session_id: &str) -> bool {
        let state = self.inner.state.lock();
        state
            .session_connections
            .get(session_id)
            .and_then(|connection_id| state.routers.get(connection_id))
            .is_some_and(ElicitationRouter::supports_form)
    }

    /// 为一个已经建立投递泵的 Session 创建 AskUser Handler。
    #[cfg(test)]
    pub fn handler(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        sink: Arc<dyn ClientRequestSink>,
    ) -> DesktopQuestionHandler {
        self.handler_for_connection(session_id, embedded_desktop_connection(), sink)
    }

    /// 为一个 Session 创建只允许目标连接投递与响应的 AskUser Handler。
    pub fn handler_for_connection(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        connection_id: ConnectionId,
        sink: Arc<dyn ClientRequestSink>,
    ) -> DesktopQuestionHandler {
        DesktopQuestionHandler {
            session_id,
            display_session_id: None,
            connection_id,
            coordinator: Arc::clone(self),
            sink,
        }
    }

    /// 创建一个保留 actor pending 身份、但把 ACP frame 投影到父 Session 的问答 Handler。
    pub fn handler_for_connection_projected(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        display_session_id: String,
        connection_id: ConnectionId,
        sink: Arc<dyn ClientRequestSink>,
    ) -> DesktopQuestionHandler {
        DesktopQuestionHandler {
            session_id,
            display_session_id: Some(display_session_id),
            connection_id,
            coordinator: Arc::clone(self),
            sink,
        }
    }

    /// 返回 Session 当前绑定的 Prompt 连接。
    pub fn session_connection(&self, session_id: &str) -> Option<ConnectionId> {
        self.inner
            .state
            .lock()
            .session_connections
            .get(session_id)
            .cloned()
    }

    /// 订阅 pending 生命周期变化；正文和连接权限仍需通过
    /// [`Self::pending_views_for_connection`] 重新读取。
    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<ElicitationChange> {
        self.inner.changes.subscribe()
    }

    /// 按连接读取当前待决问答的只读视图。
    ///
    /// 该方法只供 Runtime 投影层调用；它不接受前端传入的任意 Session 过滤条件，
    /// 后续必须由 `AgentRuntime` 再按父 Session 和 actor 路由做一次授权过滤。
    pub(crate) fn pending_views_for_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Vec<PendingElicitationView> {
        self.inner
            .state
            .lock()
            .pending
            .values()
            .filter(|pending| {
                &pending.connection_id == connection_id
                    && matches!(
                        pending.delivery_stage,
                        ElicitationDeliveryStage::Emitting | ElicitationDeliveryStage::Delivered
                    )
            })
            .map(|pending| PendingElicitationView {
                request_id: pending.request_id.clone(),
                session_id: pending.session_id.clone(),
                display_session_id: pending.display_session_id.clone(),
                connection_id: pending.connection_id.clone(),
                questions: pending.questions.clone(),
                created_at_unix_ms: pending.created_at_unix_ms,
                tool_call_id: pending.tool_call_id.clone(),
                auto_resolution: pending.auto_resolution.clone(),
            })
            .collect()
    }

    /// 向当前会话投影广播一次 pending 变化；广播失败只表示没有订阅者。
    fn notify_change(&self, display_session_id: Option<&str>, session_id: &str) {
        let display_session_id = display_session_id.unwrap_or(session_id);
        let _ = self.inner.changes.send(ElicitationChange {
            display_session_id: display_session_id.to_owned(),
        });
    }

    /// 返回当前进程尚未收口的问答数量。
    pub fn pending_len(&self) -> usize {
        self.inner.state.lock().pending.len()
    }

    /// 返回指定 Session 当前唯一待决 Elicitation 的请求标识。
    ///
    /// Host admission 只保存稳定标识和状态，不复制问答正文；该只读查询让
    /// HostPromptQueue 在 Agent 发出请求后记录 `NeedsInput`，实际回答仍由本
    /// 协调器负责严格解码和 exactly-once 收口。
    pub fn pending_request_id_for_session(&self, session_id: &str) -> Option<String> {
        self.inner
            .state
            .lock()
            .pending_by_session
            .get(session_id)
            .cloned()
    }

    /// 返回一个待决请求所属的 Session；用于回答竞态下先登记 Host admission 状态。
    pub fn pending_session_id_for_request(&self, request_id: &str) -> Option<String> {
        self.inner
            .state
            .lock()
            .pending
            .get(request_id)
            .map(|pending| pending.session_id.clone())
    }

    /// 返回待决请求唯一允许响应的连接。
    pub fn pending_connection_for_request(&self, request_id: &str) -> Option<ConnectionId> {
        self.inner
            .state
            .lock()
            .pending
            .get(request_id)
            .map(|pending| pending.connection_id.clone())
    }

    /// 判断字符串 JSON-RPC 标识是否属于一个待决问答。
    pub fn contains_pending(&self, request_id: &str) -> bool {
        self.inner.state.lock().pending.contains_key(request_id)
    }

    /// 应用 App 偏好到现有和后续 AskUser 请求。
    ///
    /// 关闭开关会立即移除现有请求的倒计时；重新开启时从当前时刻为仍在等待的
    /// 请求重新计时。这样“当前和后续提问会一直等待”不会只对下一次请求生效。
    pub(crate) fn sync_auto_resolution_preference(&self, enabled: bool) {
        let now = crate::agent_runtime::unix_time_ms();
        let mut aborts = Vec::new();
        let mut notifications = Vec::new();
        let mut schedule = Vec::new();
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return;
            }
            for pending in state.pending.values_mut() {
                if enabled {
                    if pending.auto_resolution.is_none() {
                        pending.auto_resolution = Some(active_auto_resolution(now));
                        schedule.push(pending.request_id.clone());
                        notifications.push((
                            pending.display_session_id.clone(),
                            pending.session_id.clone(),
                        ));
                    }
                } else {
                    pending.auto_resolution = None;
                    if let Some(abort) = pending.auto_resolution_abort.take() {
                        aborts.push(abort);
                    }
                    notifications.push((
                        pending.display_session_id.clone(),
                        pending.session_id.clone(),
                    ));
                }
            }
        }
        for abort in aborts {
            abort.abort();
        }
        for (display_session_id, session_id) in notifications {
            self.notify_change(display_session_id.as_deref(), &session_id);
        }
        for request_id in schedule {
            // 偏好同步发生在 Tokio Runtime 外时仍保留“等待用户”语义，不能
            // 因无法启动计时器而把一次合法的 AskUser 登记判成投递失败。
            let _ = self.schedule_auto_resolution(request_id);
        }
    }

    /// 校验连接与 pending 身份后暂停一次 AskUser 自动继续；重复调用幂等成功。
    pub fn snooze_auto_resolution_from_connection(
        &self,
        connection_id: &ConnectionId,
        request_id: &str,
    ) -> Result<bool, ElicitationBridgeError> {
        if request_id.trim().is_empty() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let (abort, notify) = {
            let mut state = self.inner.state.lock();
            let Some(pending) = state.pending.get_mut(request_id) else {
                return Err(ElicitationBridgeError::UnknownRequest);
            };
            if &pending.connection_id != connection_id {
                return Err(ElicitationBridgeError::ResponseConnectionMismatch);
            }
            let Some(auto_resolution) = pending.auto_resolution.as_mut() else {
                return Ok(false);
            };
            let started_at_unix_ms = match auto_resolution {
                PendingElicitationAutoResolution::Active {
                    started_at_unix_ms, ..
                } => *started_at_unix_ms,
                PendingElicitationAutoResolution::Snoozed { .. } => return Ok(false),
            };
            *auto_resolution = PendingElicitationAutoResolution::Snoozed {
                started_at_unix_ms,
                snoozed_at_unix_ms: crate::agent_runtime::unix_time_ms(),
            };
            (
                pending.auto_resolution_abort.take(),
                Some((
                    pending.display_session_id.clone(),
                    pending.session_id.clone(),
                )),
            )
        };
        if let Some(abort) = abort {
            abort.abort();
        }
        if let Some((display_session_id, session_id)) = notify {
            self.notify_change(display_session_id.as_deref(), &session_id);
        }
        Ok(true)
    }

    /// 为一个 active pending 建立到期任务；任务只通过协调器移除 pending，不能直接
    /// 唤醒工具 Future，避免把连接响应和自动继续分成两套收口路径。
    fn schedule_auto_resolution(&self, request_id: String) -> Result<(), ElicitationBridgeError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ElicitationBridgeError::DeliveryUnavailable)?;
        let deadline = {
            let state = self.inner.state.lock();
            let Some(pending) = state.pending.get(&request_id) else {
                return Ok(());
            };
            let Some(PendingElicitationAutoResolution::Active {
                deadline_at_unix_ms,
                ..
            }) = pending.auto_resolution.as_ref()
            else {
                return Ok(());
            };
            *deadline_at_unix_ms
        };
        let delay =
            Duration::from_millis(deadline.saturating_sub(crate::agent_runtime::unix_time_ms()));
        let coordinator = self.clone();
        let task_request_id = request_id.clone();
        let task = runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            coordinator.auto_resolve(&task_request_id);
        });
        let abort = task.abort_handle();
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(&request_id) else {
            abort.abort();
            return Ok(());
        };
        if !matches!(
            pending.auto_resolution,
            Some(PendingElicitationAutoResolution::Active { .. })
        ) {
            abort.abort();
            return Ok(());
        }
        if let Some(previous) = pending.auto_resolution_abort.replace(abort) {
            previous.abort();
        }
        Ok(())
    }

    /// 到期时以每题空答案完成 AskUser；这与用户跳过问题使用同一 core 校验路径。
    fn auto_resolve(&self, request_id: &str) {
        let pending = {
            let mut state = self.inner.state.lock();
            let Some(pending) = state.pending.get(request_id) else {
                return;
            };
            if !matches!(
                pending.auto_resolution,
                Some(PendingElicitationAutoResolution::Active { .. })
            ) {
                return;
            }
            let Some(pending) = state.pending.remove(request_id) else {
                return;
            };
            state.pending_by_session.remove(&pending.session_id);
            pending
        };
        let answers = pending
            .questions
            .iter()
            .map(|question| UserQuestionAnswer {
                id: question.id.clone(),
                values: Vec::new(),
            })
            .collect();
        self.notify_change(pending.display_session_id.as_deref(), &pending.session_id);
        // 当前任务正在执行，不需要再次 abort 自己；丢弃句柄即可释放它的所有权。
        let _ = pending.auto_resolution_abort;
        let _ = pending.waiter.send(Ok(UserQuestionResponse { answers }));
    }

    /// 严格解析并 exactly-once 收口一个完整 ACP Elicitation 响应。
    #[cfg(test)]
    pub fn respond(&self, response_json: &str) -> Result<(), ElicitationBridgeError> {
        self.respond_from_connection(&embedded_desktop_connection(), response_json)
    }

    /// 校验来源连接后严格解析并 exactly-once 收口一个完整响应。
    pub fn respond_from_connection(
        &self,
        connection_id: &ConnectionId,
        response_json: &str,
    ) -> Result<(), ElicitationBridgeError> {
        if response_json.len() > self.response_decoder.limits().max_payload_bytes() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let routed_request_id = response_request_id(response_json)?;
        {
            let state = self.inner.state.lock();
            let Some(pending) = state.pending.get(&routed_request_id) else {
                return Err(ElicitationBridgeError::UnknownRequest);
            };
            if &pending.connection_id != connection_id {
                return Err(ElicitationBridgeError::ResponseConnectionMismatch);
            }
        }
        let decoded = match self
            .response_decoder
            .decode_result::<CreateElicitationResponse>(response_json.as_bytes())
        {
            Ok(decoded) => decoded,
            Err(_) => {
                self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
                return Err(ElicitationBridgeError::InvalidResponse);
            }
        };
        let (response_id, response) = decoded.into_parts();
        let RequestId::Str(request_id) = response_id else {
            self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
            return Err(ElicitationBridgeError::InvalidResponse);
        };
        if request_id != routed_request_id {
            self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
            return Err(ElicitationBridgeError::InvalidResponse);
        }

        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get(&request_id) else {
            return Err(ElicitationBridgeError::UnknownRequest);
        };
        if !matches!(
            pending.delivery_stage,
            ElicitationDeliveryStage::Emitting | ElicitationDeliveryStage::Delivered
        ) {
            return Err(ElicitationBridgeError::RequestNotDelivered);
        }
        let result = response_to_answers(&pending.questions, response);
        let Some(mut pending) = state.pending.remove(&request_id) else {
            return Err(ElicitationBridgeError::InternalState);
        };
        state.pending_by_session.remove(&pending.session_id);
        let display_session_id = pending.display_session_id.clone();
        let pending_session_id = pending.session_id.clone();
        let auto_resolution_abort = pending.auto_resolution_abort.take();
        drop(state);
        if let Some(abort) = auto_resolution_abort {
            abort.abort();
        }
        self.notify_change(display_session_id.as_deref(), &pending_session_id);
        match result {
            Ok(response) => pending
                .waiter
                .send(Ok(response))
                .map_err(|_| ElicitationBridgeError::InternalState),
            Err(error) => {
                pending
                    .waiter
                    .send(Err(error.into_user_question_error()))
                    .map_err(|_| ElicitationBridgeError::InternalState)?;
                if error == ElicitationBridgeError::Cancelled {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// 将 Workflow/Interaction 控制面的答案转换为当前待决问答的 ACP 响应。
    ///
    /// 转换只读取该 request 的原始 `pending.questions`，因此单题的
    /// `optionId/freeText`、多题 `answers` 以及完整 ACP JSON-RPC 回执都必须
    /// 通过同一份问题 Schema 校验；调用方不允许按猜测拼接 question id 或字段。
    pub fn respond_workflow_answer_from_connection(
        &self,
        connection_id: &ConnectionId,
        request_id: &str,
        answer_json: &str,
    ) -> Result<(), ElicitationBridgeError> {
        if request_id.trim().is_empty() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let answer = serde_json::from_str::<Value>(answer_json)
            .unwrap_or_else(|_| Value::String(answer_json.to_owned()));
        let is_complete_rpc = answer.as_object().is_some_and(|object| {
            object.contains_key("jsonrpc")
                || (object.contains_key("id") && object.contains_key("result"))
        });
        if is_complete_rpc {
            return self.respond_from_connection(connection_id, answer_json);
        }

        let result = {
            let state = self.inner.state.lock();
            let Some(pending) = state.pending.get(request_id) else {
                return Err(ElicitationBridgeError::UnknownRequest);
            };
            if &pending.connection_id != connection_id {
                return Err(ElicitationBridgeError::ResponseConnectionMismatch);
            }
            workflow_answer_result(&pending.questions, answer)?
        };
        let response = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": result,
        });
        self.respond_from_connection(connection_id, &response.to_string())
    }

    /// 关闭 Runtime，取消全部内存问答且不接受迟到响应。
    pub fn shutdown(&self) {
        let mut state = self.inner.state.lock();
        if state.closed && state.pending.is_empty() {
            return;
        }
        state.closed = true;
        state.routers.clear();
        state.session_connections.clear();
        let pending = state
            .pending
            .drain()
            .map(|(_, pending)| pending)
            .collect::<Vec<_>>();
        state.pending_by_session.clear();
        drop(state);
        for mut pending in pending {
            self.notify_change(pending.display_session_id.as_deref(), &pending.session_id);
            if let Some(abort) = pending.dispatch_abort.take() {
                abort.abort();
            }
            if let Some(abort) = pending.auto_resolution_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::RuntimeClosed.into_user_question_error()
            ));
        }
    }

    /// 断开连接时取消其全部待决问答，并移除能力和 Session 绑定。
    pub fn disconnect(&self, connection_id: &ConnectionId) {
        let mut state = self.inner.state.lock();
        state.routers.remove(connection_id);
        state
            .session_connections
            .retain(|_, current| current != connection_id);
        let request_ids = state
            .pending
            .iter()
            .filter(|(_, pending)| &pending.connection_id == connection_id)
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        let mut cancelled = Vec::with_capacity(request_ids.len());
        for request_id in request_ids {
            if let Some(pending) = state.pending.remove(&request_id) {
                state.pending_by_session.remove(&pending.session_id);
                cancelled.push(pending);
            }
        }
        drop(state);
        for mut pending in cancelled {
            self.notify_change(pending.display_session_id.as_deref(), &pending.session_id);
            if let Some(abort) = pending.dispatch_abort.take() {
                abort.abort();
            }
            if let Some(abort) = pending.auto_resolution_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::Cancelled.into_user_question_error()
            ));
        }
    }

    /// 同步登记一次问答并启动当前 Session 的唯一异步投递。
    fn register(
        &self,
        handler_session_id: &keencode_agent::SessionId,
        connection_id: &ConnectionId,
        request: UserQuestionRequest,
        sink: Arc<dyn ClientRequestSink>,
    ) -> Result<RegisteredElicitation, ElicitationBridgeError> {
        self.register_with_display_session(handler_session_id, connection_id, request, None, sink)
    }

    /// 登记 pending 的 actor 身份，并可将实际 ACP 请求投影到父 Session。
    fn register_with_display_session(
        &self,
        handler_session_id: &keencode_agent::SessionId,
        connection_id: &ConnectionId,
        request: UserQuestionRequest,
        display_session_id: Option<&str>,
        sink: Arc<dyn ClientRequestSink>,
    ) -> Result<RegisteredElicitation, ElicitationBridgeError> {
        if &request.session_id != handler_session_id {
            return Err(ElicitationBridgeError::SessionMismatch);
        }
        let router = {
            let state = self.inner.state.lock();
            state
                .routers
                .get(connection_id)
                .copied()
                .ok_or(ElicitationBridgeError::CapabilitiesUnavailable)?
        };
        let request_id = next_request_id();
        let mut display_request = request.clone();
        if let Some(display_session_id) = display_session_id {
            display_request.session_id = keencode_agent::SessionId::new(display_session_id)
                .map_err(|_| ElicitationBridgeError::SessionMismatch)?;
        }
        let frame = self
            .request_encoder
            .elicitation_request_frame(
                RequestId::Str(request_id.clone()),
                &router,
                create_request(&display_request),
            )
            .map_err(|_| ElicitationBridgeError::RegistrationRejected)?;
        let (waiter, receiver) = oneshot::channel();
        let session_id = request.session_id.as_str().to_owned();
        let tool_call_id = request.tool_call_id.as_str().to_owned();
        let created_at_unix_ms = crate::agent_runtime::unix_time_ms();
        let auto_resolution_enabled = self
            .app_runtime_preferences
            .read()
            .map(|preferences| preferences.ask_user_question_auto_resolution_enabled)
            .unwrap_or(false);
        let auto_resolution =
            auto_resolution_enabled.then(|| active_auto_resolution(created_at_unix_ms));
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return Err(ElicitationBridgeError::RuntimeClosed);
            }
            if state.pending_by_session.contains_key(&session_id) {
                return Err(ElicitationBridgeError::SessionBusy);
            }
            state
                .pending_by_session
                .insert(session_id.clone(), request_id.clone());
            state.pending.insert(
                request_id.clone(),
                PendingElicitation {
                    request_id: request_id.clone(),
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    display_session_id: display_session_id.map(str::to_owned),
                    questions: request.questions,
                    created_at_unix_ms,
                    tool_call_id,
                    auto_resolution,
                    auto_resolution_abort: None,
                    delivery_stage: ElicitationDeliveryStage::Dispatching,
                    dispatch_abort: None,
                    display_permit: None,
                    waiter,
                },
            );
        }
        let display_gate_session = display_session_id.unwrap_or(&session_id).to_owned();
        if auto_resolution_enabled {
            // 计时器与 pending 同时登记；即使连接尚未回执，后续设置/断线仍可
            // 通过同一 pending 身份取消它，避免悬挂 AskUser Future。
            let _ = self.schedule_auto_resolution(request_id.clone());
        }
        self.schedule_dispatch(request_id.clone(), display_gate_session, sink, frame)?;
        Ok(RegisteredElicitation {
            guard: PendingElicitationGuard {
                coordinator: self.clone(),
                request_id,
                armed: true,
            },
            receiver,
        })
    }

    /// 创建投递任务，并在请求仍处于 Dispatching 时才允许其开始。
    fn schedule_dispatch(
        &self,
        request_id: String,
        session_id: String,
        sink: Arc<dyn ClientRequestSink>,
        frame: AcpClientRequestFrame,
    ) -> Result<(), ElicitationBridgeError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ElicitationBridgeError::DeliveryUnavailable)?;
        let coordinator = self.clone();
        let client_request_gate = Arc::clone(&self.client_request_gate);
        let task_request_id = request_id.clone();
        let (start, started) = oneshot::channel();
        let task = runtime.spawn(async move {
            if started.await.is_err() {
                return;
            }
            let permit = match client_request_gate.acquire(&session_id).await {
                Some(permit) => permit,
                None => {
                    coordinator.finish_delivery(
                        &task_request_id,
                        Err(ElicitationBridgeError::InternalState),
                    );
                    return;
                }
            };
            if !coordinator.begin_emit_with_display_permit(&task_request_id, permit) {
                return;
            }
            let result = sink
                .send_client_request(frame)
                .await
                .map_err(|_| ElicitationBridgeError::DeliveryUnavailable);
            coordinator.finish_delivery(&task_request_id, result);
        });
        let abort = task.abort_handle();
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(&request_id) else {
            abort.abort();
            return Err(ElicitationBridgeError::InternalState);
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Dispatching {
            abort.abort();
            return Err(ElicitationBridgeError::InternalState);
        }
        pending.dispatch_abort = Some(abort);
        drop(state);
        let _ = start.send(());
        Ok(())
    }

    /// 保存跨路由许可并在调用投递泵前原子进入可响应的发送阶段。
    fn begin_emit_with_display_permit(
        &self,
        request_id: &str,
        permit: ClientRequestDisplayPermit,
    ) -> bool {
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(request_id) else {
            return false;
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Dispatching
            || pending.display_permit.is_some()
        {
            return false;
        }
        pending.display_permit = Some(permit);
        pending.delivery_stage = ElicitationDeliveryStage::Emitting;
        true
    }

    /// 记录桌面投递结果；失败时移除占用并唤醒 AskUser Future。
    fn finish_delivery(&self, request_id: &str, result: Result<(), ElicitationBridgeError>) {
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(request_id) else {
            return;
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Emitting {
            return;
        }
        pending.dispatch_abort = None;
        if result.is_ok() {
            pending.delivery_stage = ElicitationDeliveryStage::Delivered;
            let display_session_id = pending.display_session_id.clone();
            let pending_session_id = pending.session_id.clone();
            drop(state);
            self.notify_change(display_session_id.as_deref(), &pending_session_id);
            return;
        }
        let Some(mut pending) = state.pending.remove(request_id) else {
            return;
        };
        state.pending_by_session.remove(&pending.session_id);
        let display_session_id = pending.display_session_id.clone();
        let pending_session_id = pending.session_id.clone();
        let auto_resolution_abort = pending.auto_resolution_abort.take();
        drop(state);
        if let Some(abort) = auto_resolution_abort {
            abort.abort();
        }
        self.notify_change(display_session_id.as_deref(), &pending_session_id);
        let _ = pending.waiter.send(Err(
            ElicitationBridgeError::DeliveryUnavailable.into_user_question_error()
        ));
    }

    /// Future 被取消或丢弃时 exactly-once 清理待决问答。
    fn cancel_request(&self, request_id: &str, error: ElicitationBridgeError) {
        let mut state = self.inner.state.lock();
        let Some(mut pending) = state.pending.remove(request_id) else {
            return;
        };
        state.pending_by_session.remove(&pending.session_id);
        let display_session_id = pending.display_session_id.clone();
        let pending_session_id = pending.session_id.clone();
        drop(state);
        self.notify_change(display_session_id.as_deref(), &pending_session_id);
        if let Some(abort) = pending.dispatch_abort.take() {
            abort.abort();
        }
        if let Some(abort) = pending.auto_resolution_abort.take() {
            abort.abort();
        }
        let _ = pending.waiter.send(Err(error.into_user_question_error()));
    }
}

impl Default for ElicitationCoordinator {
    /// 创建默认的空结构化问答协调器。
    fn default() -> Self {
        Self::new()
    }
}

impl ClientRequestRouter for ElicitationCoordinator {
    /// 让 AgentRuntime 只把本协调器登记的请求交给 Elicitation DTO。
    fn contains_pending(&self, request_id: &str) -> bool {
        ElicitationCoordinator::contains_pending(self, request_id)
    }

    /// 生产路由必须验证 Client Response 的传输连接身份。
    fn respond_from_connection(
        &self,
        connection_id: &ConnectionId,
        response_json: &str,
    ) -> Result<(), String> {
        ElicitationCoordinator::respond_from_connection(self, connection_id, response_json)
            .map_err(|error| error.to_string())
    }
}

/// 绑定一个 Session 和其唯一投递泵的 AskUser 实现。
pub struct DesktopQuestionHandler {
    /// 该 Handler 唯一允许接收的 Session。
    session_id: keencode_agent::SessionId,
    /// 可选的显示投影 Session；缺省时与 pending Session 相同。
    display_session_id: Option<String>,
    /// 本轮 Prompt 唯一允许交互的 ACP 连接。
    connection_id: ConnectionId,
    /// 进程内共享的问答协调器。
    coordinator: Arc<ElicitationCoordinator>,
    /// 当前 Session 的 ACP 投递泵。
    sink: Arc<dyn ClientRequestSink>,
}

impl UserQuestionHandler for DesktopQuestionHandler {
    /// 同步登记问题并等待一次严格 Client 响应。
    fn ask(&self, request: UserQuestionRequest) -> UserQuestionFuture<'_> {
        let registration = if let Some(display_session_id) = self.display_session_id.as_deref() {
            self.coordinator.register_with_display_session(
                &self.session_id,
                &self.connection_id,
                request,
                Some(display_session_id),
                Arc::clone(&self.sink),
            )
        } else {
            self.coordinator.register(
                &self.session_id,
                &self.connection_id,
                request,
                Arc::clone(&self.sink),
            )
        };
        Box::pin(async move {
            let RegisteredElicitation {
                mut guard,
                receiver,
            } = registration.map_err(ElicitationBridgeError::into_user_question_error)?;
            let result = receiver.await.unwrap_or_else(|_| {
                Err(ElicitationBridgeError::InternalState.into_user_question_error())
            });
            guard.armed = false;
            result
        })
    }
}

/// Handler 登记成功后持有的取消守卫和一次性接收端。
struct RegisteredElicitation {
    /// Future 被丢弃时负责清理待决映射的守卫。
    guard: PendingElicitationGuard,
    /// 等待桌面回答的一次性接收端。
    receiver: oneshot::Receiver<Result<UserQuestionResponse, UserQuestionError>>,
}

/// AskUser Future 未正常完成时执行 exactly-once 清理。
struct PendingElicitationGuard {
    /// 负责移除待决请求的共享协调器。
    coordinator: ElicitationCoordinator,
    /// 当前 Future 对应的 JSON-RPC 标识。
    request_id: String,
    /// 正常完成后关闭自动清理。
    armed: bool,
}

impl Drop for PendingElicitationGuard {
    /// 取消尚未完成的桌面问答并拒绝迟到响应。
    fn drop(&mut self) {
        if self.armed {
            self.coordinator
                .cancel_request(&self.request_id, ElicitationBridgeError::Cancelled);
        }
    }
}

/// 为一次问答分配跨冷启动不复用的全局标识。
fn next_request_id() -> String {
    let message_id = UuidCollaborationIdGenerator.next_message_id();
    let suffix = message_id
        .as_str()
        .strip_prefix("message-")
        .unwrap_or_else(|| message_id.as_str());
    format!("elicitation-{suffix}")
}

/// 构造一次 AskUser 的绝对倒计时；测试缩放只缩短本地等待，不改变 wire 形状。
fn active_auto_resolution(now_unix_ms: u64) -> PendingElicitationAutoResolution {
    let total = scaled_auto_resolution_duration(ASK_USER_AUTO_RESOLUTION_TOTAL);
    let hidden_grace = scaled_auto_resolution_duration(ASK_USER_AUTO_RESOLUTION_HIDDEN_GRACE)
        .min(total.saturating_sub(Duration::from_millis(1)));
    let total_ms = u64::try_from(total.as_millis()).unwrap_or(u64::MAX).max(1);
    let hidden_ms = u64::try_from(hidden_grace.as_millis()).unwrap_or(0);
    PendingElicitationAutoResolution::Active {
        started_at_unix_ms: now_unix_ms,
        visible_at_unix_ms: now_unix_ms.saturating_add(hidden_ms),
        deadline_at_unix_ms: now_unix_ms.saturating_add(total_ms),
    }
}

/// 读取前端 shared 契约约定的 E2E 时钟缩放；异常值按生产时钟处理。
fn scaled_auto_resolution_duration(duration: Duration) -> Duration {
    let scale = std::env::var(ASK_USER_QUESTION_E2E_CLOCK_SCALE_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0);
    let Some(scale) = scale else {
        return duration;
    };
    Duration::from_secs_f64((duration.as_secs_f64() / scale).max(0.001))
}

/// 返回进程内桌面 transport 使用的稳定连接身份。
#[cfg(test)]
fn embedded_desktop_connection() -> ConnectionId {
    ConnectionId::new("embedded-desktop").expect("固定桌面连接标识应合法")
}

/// 把 Provider 中立问题集合转换为标准 ACP form schema。
fn create_request(request: &UserQuestionRequest) -> CreateElicitationRequest {
    let mut schema = ElicitationSchema::new().title("KeenCode AskUser");
    let mut allow_custom_by_question = serde_json::Map::new();
    let question_order = request
        .questions
        .iter()
        .map(|question| question.id.clone())
        .collect::<Vec<_>>();
    for question in &request.questions {
        allow_custom_by_question.insert(question.id.clone(), Value::Bool(question.allow_custom));
        let options = question
            .options
            .iter()
            .map(|option| EnumOption::new(option.label.clone(), option.label.clone()))
            .collect::<Vec<_>>();
        let description = question_description(question);
        if question.multi_select {
            schema = schema.property(
                question.id.clone(),
                MultiSelectPropertySchema::titled(options)
                    .description(description)
                    .min_items(0_u64)
                    .max_items(question.options.len() as u64),
                false,
            );
        } else {
            let mut property = StringPropertySchema::new()
                .description(description)
                .max_length(4_000_u32);
            if !options.is_empty() {
                property = property.one_of(options);
            }
            schema = schema.property(question.id.clone(), property, false);
        }
    }
    let scope = ElicitationSessionScope::new(request.session_id.as_str().to_owned())
        .tool_call_id(request.tool_call_id.as_str());
    let mut meta = Meta::new();
    meta.insert(
        KEENCODE_META_KEY.to_owned(),
        serde_json::json!({
            "askUser": {
                "allowCustomByQuestion": allow_custom_by_question,
                "questionOrder": question_order
            }
        }),
    );
    CreateElicitationRequest::new(
        ElicitationFormMode::new(scope, schema),
        "请回答编码 Agent 继续执行所需的问题",
    )
    .meta(meta)
}

/// 把选项说明附加到问题正文，使标准 Schema 不丢失决策取舍信息。
fn question_description(question: &UserQuestion) -> String {
    let described = question
        .options
        .iter()
        .filter_map(|option| {
            option
                .description
                .as_deref()
                .map(|description| format!("{}：{description}", option.label))
        })
        .collect::<Vec<_>>();
    if described.is_empty() {
        question.prompt.clone()
    } else {
        format!(
            "{}\n\n选项说明：\n{}",
            question.prompt,
            described.join("\n")
        )
    }
}

/// 将严格 ACP Elicitation 响应还原为 AskUser 的有序答案。
fn response_to_answers(
    questions: &[UserQuestion],
    response: CreateElicitationResponse,
) -> Result<UserQuestionResponse, ElicitationBridgeError> {
    let ElicitationAction::Accept(accepted) = response.action else {
        return Err(ElicitationBridgeError::Cancelled);
    };
    let mut content = accepted.content.unwrap_or_default();
    let mut answers = Vec::with_capacity(questions.len());
    for question in questions {
        let values = match content.remove(&question.id) {
            None => Vec::new(),
            Some(ElicitationContentValue::String(value)) if !question.multi_select => vec![value],
            Some(ElicitationContentValue::StringArray(values)) if question.multi_select => values,
            Some(_) => return Err(ElicitationBridgeError::InvalidResponse),
        };
        answers.push(UserQuestionAnswer {
            id: question.id.clone(),
            values,
        });
    }
    if !content.is_empty() {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    Ok(UserQuestionResponse { answers })
}

/// 依照待决问题的真实形状，把控制面答案收敛为 ACP `result` 对象。
fn workflow_answer_result(
    questions: &[UserQuestion],
    answer: Value,
) -> Result<Value, ElicitationBridgeError> {
    if let Value::String(value) = answer {
        if questions.len() != 1 || value.trim().is_empty() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        return Ok(json!({
            "action": "accept",
            "content": { questions[0].id.clone(): value },
        }));
    }

    let Value::Object(object) = answer else {
        return Err(ElicitationBridgeError::InvalidResponse);
    };
    if let Some(action) = object.get("action").and_then(Value::as_str) {
        if object.keys().any(|key| key != "action" && key != "content")
            || (action != "accept" && object.len() != 1)
        {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        return match action {
            "decline" | "cancel" => Ok(json!({ "action": action })),
            "accept" => {
                let content = object
                    .get("content")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or(ElicitationBridgeError::InvalidResponse)?;
                Ok(json!({
                    "action": "accept",
                    "content": workflow_accept_content(questions, &content)?,
                }))
            }
            _ => Err(ElicitationBridgeError::InvalidResponse),
        };
    }
    if let Some(answers) = object.get("answers") {
        if object.len() != 1 {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        return Ok(json!({
            "action": "accept",
            "content": workflow_answers_content(questions, answers)?,
        }));
    }
    if object.contains_key("optionId") || object.contains_key("freeText") {
        if questions.len() != 1
            || object
                .keys()
                .any(|key| key != "optionId" && key != "freeText")
            || (object.contains_key("optionId") && object.contains_key("freeText"))
        {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let value = object
            .get("optionId")
            .or_else(|| object.get("freeText"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        return Ok(json!({
            "action": "accept",
            "content": { questions[0].id.clone(): value },
        }));
    }

    Ok(json!({
        "action": "accept",
        "content": workflow_content_from_question_map(questions, &object)?,
    }))
}

/// 判断是否为 Source `ElicitationDialog` 生成的 renderer-local 回答字段。
fn is_source_form_answer_key(key: &str, questions: &[UserQuestion]) -> bool {
    key == "answers"
        || (key == "answer" && !questions.iter().any(|question| question.id == key))
        || key
            .strip_prefix("answer_")
            .is_some_and(|index| !index.is_empty() && index.parse::<usize>().is_ok())
}

/// 将 V4 Source 对话框的文案/位置字段映射为标准 ACP question id。
///
/// Source UI 不接触 Rust 的内部问题 id：`answers` 以题目文案为键，
/// `answer_N` 才是无歧义的顺序值。优先采用 `answer_N`，因为多选的
/// `answers` 文案值会被 UI 拼接成展示字符串；所有未识别或冲突字段仍拒绝。
fn workflow_accept_content(
    questions: &[UserQuestion],
    object: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, ElicitationBridgeError> {
    let source_shape = object
        .keys()
        .any(|key| is_source_form_answer_key(key, questions));
    if !source_shape {
        return workflow_content_from_question_map(questions, object);
    }
    if object
        .keys()
        .any(|key| questions.iter().any(|question| question.id == *key))
    {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    workflow_content_from_source_form(questions, object)
}

/// 读取 Source 表单的 `answers`、`answer_N` 和单题兼容 `answer` 字段。
fn workflow_content_from_source_form(
    questions: &[UserQuestion],
    object: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, ElicitationBridgeError> {
    let allowed = object.keys().all(|key| {
        is_source_form_answer_key(key, questions)
            && (key != "answer" || questions.len() == 1)
            && key.strip_prefix("answer_").is_none_or(|index| {
                index
                    .parse::<usize>()
                    .is_ok_and(|value| value < questions.len())
            })
    });
    if !allowed {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    let answers_summary = object.get("answers").map(|value| {
        value
            .as_object()
            .ok_or(ElicitationBridgeError::InvalidResponse)
    });
    let answers_summary = answers_summary.transpose()?;

    let mut content = serde_json::Map::new();
    for (index, question) in questions.iter().enumerate() {
        let indexed = object.get(&format!("answer_{index}"));
        let legacy = (questions.len() == 1)
            .then(|| object.get("answer"))
            .flatten();
        let prompt_value = answers_summary.and_then(|answers| {
            let matching = questions
                .iter()
                .filter(|candidate| candidate.prompt == question.prompt)
                .count();
            (matching == 1)
                .then(|| answers.get(&question.prompt))
                .flatten()
        });
        let value = indexed.or(legacy).or(prompt_value);
        let Some(value) = value else {
            continue;
        };
        let normalized = source_form_value(question, value)?;
        if let Some(existing) = content.get(&question.id)
            && existing != &normalized
        {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        content.insert(question.id.clone(), normalized);
    }

    if let Some(answers) = answers_summary {
        for (prompt, value) in answers {
            let matches = questions
                .iter()
                .filter(|question| question.prompt == *prompt)
                .count();
            // Source 的 `answers` 是给旧 renderer 的展示摘要，始终是拼接后的文本；
            // 多选的无损数组只能来自同一回执中的 `answer_N`。
            if matches != 1 || !value.is_string() {
                return Err(ElicitationBridgeError::InvalidResponse);
            }
        }
    }
    Ok(content)
}

/// 规范化一个 Source 表单字段，同时保留多选数组而不解析拼接文案。
fn source_form_value(
    question: &UserQuestion,
    value: &Value,
) -> Result<Value, ElicitationBridgeError> {
    if question.multi_select {
        let values = value
            .as_array()
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        if values
            .iter()
            .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
        {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        return Ok(Value::Array(values.to_vec()));
    }
    let value = value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or(ElicitationBridgeError::InvalidResponse)?;
    Ok(Value::String(value.to_owned()))
}

/// 将 AskUser 输出的 `answers:[{id,values}]` 映射为 ACP form content。
fn workflow_answers_content(
    questions: &[UserQuestion],
    value: &Value,
) -> Result<serde_json::Map<String, Value>, ElicitationBridgeError> {
    let answers = value
        .as_array()
        .ok_or(ElicitationBridgeError::InvalidResponse)?;
    if answers.len() != questions.len() {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    let mut content = serde_json::Map::new();
    for answer in answers {
        let object = answer
            .as_object()
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        if object.len() != 2 {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        let question = questions
            .iter()
            .find(|question| question.id == id)
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        if content.contains_key(id) {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let values = object
            .get("values")
            .and_then(Value::as_array)
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        let values = values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or(ElicitationBridgeError::InvalidResponse)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !question.multi_select && values.len() > 1 {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        if question.multi_select {
            content.insert(
                id.to_owned(),
                Value::Array(values.into_iter().map(Value::String).collect()),
            );
        } else if let Some(value) = values.into_iter().next() {
            content.insert(id.to_owned(), Value::String(value));
        }
    }
    if questions
        .iter()
        .any(|question| !content.contains_key(&question.id))
        && questions.iter().any(|question| {
            !question.multi_select
                && answers.iter().any(|answer| {
                    answer
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id == question.id)
                })
        })
    {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    Ok(content)
}

/// 直接使用 question id 的 ACP content；未知字段和类型都拒绝，避免猜测。
fn workflow_content_from_question_map(
    questions: &[UserQuestion],
    object: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, ElicitationBridgeError> {
    if object.len() != questions.len()
        || object
            .keys()
            .any(|key| !questions.iter().any(|question| question.id == *key))
    {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    let mut content = serde_json::Map::new();
    for question in questions {
        let value = object
            .get(&question.id)
            .ok_or(ElicitationBridgeError::InvalidResponse)?;
        if question.multi_select {
            if !value.is_array()
                || value
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value.as_str().is_none()))
            {
                return Err(ElicitationBridgeError::InvalidResponse);
            }
        } else if value.as_str().is_none() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        content.insert(question.id.clone(), value.clone());
    }
    Ok(content)
}

/// 从一个有界 JSON 视图读取字符串响应 ID；严格 DTO 校验仍由对应路由完成。
fn response_request_id(response_json: &str) -> Result<String, ElicitationBridgeError> {
    let value = serde_json::from_str::<Value>(response_json)
        .map_err(|_| ElicitationBridgeError::InvalidResponse)?;
    value
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or(ElicitationBridgeError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_request::ClientRequestBridgeError;
    use keencode_acp::schema::{ElicitationCapabilities, ElicitationFormCapabilities};
    use keencode_agent::{AgentId, SessionId, ToolCallId, TurnId};
    use keencode_tools::UserQuestionOption;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::{Notify, mpsc};
    use tokio::time::{Duration, timeout};

    /// 模拟 Client 通过 initialize 声明表单能力，测试不依赖生产默认值。
    fn negotiated_coordinator() -> ElicitationCoordinator {
        let coordinator = ElicitationCoordinator::new();
        coordinator
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        coordinator
    }

    /// 构造测试 Client 显式声明的表单能力。
    fn form_capabilities() -> ClientCapabilities {
        ClientCapabilities::new().elicitation(Some(
            ElicitationCapabilities::new().form(Some(ElicitationFormCapabilities::new())),
        ))
    }

    /// 构造测试 transport 的稳定连接标识。
    fn connection(value: &str) -> ConnectionId {
        ConnectionId::new(value).expect("测试连接标识应合法")
    }

    /// 未协商时不支持表单；缺少表单能力的 Client 不能触发任何投递。
    #[test]
    fn form_elicitation_requires_actual_client_capability() {
        let coordinator = ElicitationCoordinator::new();
        assert!(!coordinator.supports_form());
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = Arc::new(RecordingSink { sender });
        let session = SessionId::new("session-capability").unwrap();
        assert!(matches!(
            coordinator.register(
                &session,
                &embedded_desktop_connection(),
                request(session.as_str()),
                sink.clone()
            ),
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        ));
        coordinator
            .negotiate_client_capabilities(&ClientCapabilities::new())
            .unwrap();
        assert!(matches!(
            coordinator.register(
                &session,
                &embedded_desktop_connection(),
                request(session.as_str()),
                sink
            ),
            Err(ElicitationBridgeError::RegistrationRejected)
        ));
        assert_eq!(coordinator.pending_len(), 0);
        assert!(receiver.try_recv().is_err());
    }

    /// WebView 同能力刷新允许重握手，但不能改变正在使用的能力快照。
    #[test]
    fn repeated_capability_negotiation_is_idempotent_and_shared() {
        let coordinator = ElicitationCoordinator::new();
        let cloned = coordinator.clone();
        coordinator
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        cloned
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        assert!(cloned.supports_form());
        assert_eq!(
            cloned.negotiate_client_capabilities(&ClientCapabilities::new()),
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        );
        assert!(coordinator.supports_form());
    }

    /// 不同连接独立协商能力，同一连接重复 initialize 不得改变既有快照。
    #[test]
    fn capability_negotiation_is_isolated_per_connection() {
        let coordinator = ElicitationCoordinator::new();
        let desktop = connection("desktop-a");
        let web = connection("web-b");
        coordinator
            .negotiate_connection_capabilities(&desktop, &form_capabilities())
            .expect("Desktop 应协商表单能力");
        coordinator
            .negotiate_connection_capabilities(&web, &ClientCapabilities::new())
            .expect("Web 应独立协商无表单能力");
        coordinator
            .negotiate_connection_capabilities(&desktop, &form_capabilities())
            .expect("同连接相同能力重复 initialize 应幂等");

        assert!(coordinator.supports_form_for_connection(&desktop));
        assert!(!coordinator.supports_form_for_connection(&web));
        assert_eq!(
            coordinator.negotiate_connection_capabilities(&desktop, &ClientCapabilities::new()),
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        );
        coordinator
            .bind_session_connection("shared-session", &web)
            .expect("Session 应可绑定 Web 连接");
        assert!(!coordinator.session_supports_form("shared-session"));
        coordinator
            .bind_session_connection("shared-session", &desktop)
            .expect("同 Session 下一轮应可绑定 Desktop 连接");
        assert!(coordinator.session_supports_form("shared-session"));
    }

    /// 把标准 Client Request 交给测试接收端。
    struct RecordingSink {
        /// 每次投递保存完整类型化 frame。
        sender: mpsc::Sender<AcpClientRequestFrame>,
    }

    impl ClientRequestSink for RecordingSink {
        /// 异步发送一帧并把关闭接收端映射为稳定投递错误。
        fn send_client_request(
            &self,
            request: AcpClientRequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<(), ClientRequestBridgeError>> + Send + '_>>
        {
            Box::pin(async move {
                self.sender
                    .send(request)
                    .await
                    .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)
            })
        }
    }

    /// 先公开请求、再等待测试显式回执的问答投递泵。
    struct BlockingAcknowledgementSink {
        /// 已经对桌面可见但尚未返回投递回执的请求。
        sender: mpsc::Sender<AcpClientRequestFrame>,
        /// 测试在验证响应后释放投递 Future。
        release: Arc<Notify>,
    }

    impl ClientRequestSink for BlockingAcknowledgementSink {
        /// 发送请求后阻塞回执，模拟 Tauri emit 与 oneshot 回执之间的调度窗口。
        fn send_client_request(
            &self,
            request: AcpClientRequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<(), ClientRequestBridgeError>> + Send + '_>>
        {
            let sender = self.sender.clone();
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                sender
                    .send(request)
                    .await
                    .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)?;
                release.notified().await;
                Ok(())
            })
        }
    }

    /// 构造包含单选、多选和说明的完整测试问答。
    fn request(session_id: &str) -> UserQuestionRequest {
        UserQuestionRequest {
            session_id: SessionId::new(session_id).expect("测试 Session 标识有效"),
            turn_id: TurnId::new("turn-a").expect("测试 Turn 标识有效"),
            source_agent_id: AgentId::new("agent-root").expect("测试 Agent 标识有效"),
            tool_call_id: ToolCallId::new("tool-ask-a").expect("测试 ToolCall 标识有效"),
            questions: vec![
                UserQuestion {
                    id: "strategy".to_owned(),
                    prompt: "选择实现策略".to_owned(),
                    options: vec![UserQuestionOption {
                        label: "直接实现".to_owned(),
                        description: Some("立即修改当前模块".to_owned()),
                    }],
                    multi_select: false,
                    allow_custom: true,
                },
                UserQuestion {
                    id: "checks".to_owned(),
                    prompt: "选择验证项".to_owned(),
                    options: vec![
                        UserQuestionOption {
                            label: "测试".to_owned(),
                            description: None,
                        },
                        UserQuestionOption {
                            label: "Clippy".to_owned(),
                            description: None,
                        },
                    ],
                    multi_select: true,
                    allow_custom: false,
                },
            ],
        }
    }

    /// 构造字典序与用户输入顺序不同的三题问答。
    fn non_dictionary_order_request(session_id: &str) -> UserQuestionRequest {
        let mut request = request(session_id);
        request.questions = vec![
            UserQuestion {
                id: "target".to_owned(),
                prompt: "选择部署目标".to_owned(),
                options: vec![UserQuestionOption {
                    label: "服务器".to_owned(),
                    description: None,
                }],
                multi_select: false,
                allow_custom: true,
            },
            UserQuestion {
                id: "checks".to_owned(),
                prompt: "选择检查项".to_owned(),
                options: vec![
                    UserQuestionOption {
                        label: "类型检查".to_owned(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "测试".to_owned(),
                        description: None,
                    },
                ],
                multi_select: true,
                allow_custom: false,
            },
            UserQuestion {
                id: "note".to_owned(),
                prompt: "补充说明".to_owned(),
                options: Vec::new(),
                multi_select: false,
                allow_custom: true,
            },
        ];
        request
    }

    /// 从任意标准 Client Request frame 读取字符串请求标识。
    fn frame_request_id(frame: &AcpClientRequestFrame) -> String {
        serde_json::to_value(frame).expect("Client Request 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned()
    }

    /// 标准请求必须携带 Session、ToolCall、问题 Schema，并按答案类型完成 round-trip。
    #[tokio::test]
    async fn question_handler_round_trips_standard_form_elicitation() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("应收到标准问答请求");
        let value = serde_json::to_value(&frame).expect("标准问答请求应可序列化");
        assert_eq!(value["method"], json!("elicitation/create"));
        assert_eq!(value["params"]["sessionId"], json!("session-a"));
        assert_eq!(value["params"]["toolCallId"], json!("tool-ask-a"));
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["allowCustomByQuestion"],
            json!({ "strategy": true, "checks": false })
        );
        assert_eq!(
            value["params"]["requestedSchema"]["properties"]["checks"]["type"],
            json!("array")
        );
        let request_id = value["id"].as_str().expect("请求 ID 应为字符串");
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "strategy": "直接实现",
                            "checks": ["测试", "Clippy"]
                        }
                    }
                })
                .to_string(),
            )
            .expect("严格问答响应应完成");
        let response = answer.await.expect("AskUser 应收到答案");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(response.answers[1].values, ["测试", "Clippy"]);
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// 本地 V4 RPC 不经 WebHost 发送 ACP Client Request；pending 进入 Delivered 后由
    /// conversation snapshot 展示，`resolveInteraction` 仍须经过同一严格问题 Schema。
    #[tokio::test]
    async fn frontend_v4_projection_route_keeps_pending_until_resolve() {
        let storage = tempfile::tempdir().expect("应创建测试 Runtime 存储目录");
        let runtime = crate::agent_runtime::AgentRuntime::new_for_control_test(storage.path())
            .expect("测试 Runtime 应创建");
        let coordinator = Arc::clone(runtime.elicitation_coordinator());
        let connection_id = connection("rpc-41");
        coordinator
            .negotiate_connection_capabilities(&connection_id, &form_capabilities())
            .expect("前端连接能力协商应成功");
        let session = SessionId::new("session-v4-projection").expect("测试 Session 标识有效");
        coordinator
            .bind_session_connection(session.as_str(), &connection_id)
            .expect("测试 Session 应绑定前端连接");
        let sink = Arc::new(crate::client_request::SessionDeliverySink::for_projection(
            Arc::downgrade(&runtime),
            session.as_str().to_owned(),
            connection_id.clone(),
        ));
        let handler =
            coordinator.handler_for_connection(session.clone(), connection_id.clone(), sink);
        let answer = handler.ask(request(session.as_str()));

        timeout(Duration::from_secs(1), async {
            loop {
                if !coordinator
                    .pending_views_for_connection(&connection_id)
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("投影型问答应进入可回答 pending");
        let request_id = coordinator
            .pending_request_id_for_session(session.as_str())
            .expect("pending 应保留稳定请求标识");
        coordinator
            .respond_workflow_answer_from_connection(
                &connection_id,
                &request_id,
                &json!({
                    "action": "accept",
                    "content": {
                        "answers": {
                            "选择实现策略": "直接实现",
                            "选择验证项": "测试, Clippy"
                        },
                        "answer_0": "直接实现",
                        "answer_1": ["测试", "Clippy"]
                    }
                })
                .to_string(),
            )
            .expect("Source V4 回执应由 Coordinator 严格解析");
        let response = answer.await.expect("投影型 AskUser 应收到答案");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(coordinator.pending_len(), 0);

        let unbound_connection = connection("rpc-42");
        coordinator
            .negotiate_connection_capabilities(&unbound_connection, &form_capabilities())
            .expect("未绑定连接也应完成能力协商");
        let unbound_session = SessionId::new("session-v4-unbound").expect("测试 Session 标识有效");
        let unbound_sink = Arc::new(crate::client_request::SessionDeliverySink::for_projection(
            Arc::downgrade(&runtime),
            unbound_session.as_str().to_owned(),
            unbound_connection.clone(),
        ));
        let unbound_handler = coordinator.handler_for_connection(
            unbound_session.clone(),
            unbound_connection,
            unbound_sink,
        );
        let unbound_result = timeout(
            Duration::from_secs(1),
            unbound_handler.ask(request(unbound_session.as_str())),
        )
        .await
        .expect("未绑定连接不应悬挂 AskUser");
        assert!(unbound_result.is_err());
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// App 偏好必须控制现有 pending，而不是只影响下一次工具装配。
    #[tokio::test]
    async fn runtime_preference_updates_existing_auto_resolution() {
        let preferences = Arc::new(std::sync::RwLock::new(AppRuntimePreferences::default()));
        let coordinator = Arc::new(ElicitationCoordinator::with_gate_and_preferences(
            Arc::new(ClientRequestDisplayGate::new()),
            preferences.clone(),
        ));
        coordinator
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-preference").unwrap(),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-preference"));
        let frame = receiver.recv().await.expect("偏好测试请求应送达");
        let request_id = frame_request_id(&frame);
        assert!(matches!(
            coordinator.pending_views_for_connection(&embedded_desktop_connection())[0]
                .auto_resolution,
            Some(PendingElicitationAutoResolution::Active { .. })
        ));

        *preferences.write().unwrap() = AppRuntimePreferences {
            ask_user_question_auto_resolution_enabled: false,
        };
        coordinator.sync_auto_resolution_preference(false);
        assert!(
            coordinator.pending_views_for_connection(&embedded_desktop_connection())[0]
                .auto_resolution
                .is_none()
        );

        *preferences.write().unwrap() = AppRuntimePreferences::default();
        coordinator.sync_auto_resolution_preference(true);
        assert!(matches!(
            coordinator.pending_views_for_connection(&embedded_desktop_connection())[0]
                .auto_resolution,
            Some(PendingElicitationAutoResolution::Active { .. })
        ));
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"action": "cancel"}
                })
                .to_string(),
            )
            .unwrap();
        assert!(answer.await.is_err());
    }

    /// 到期自动继续必须通过 core 合法的空答案完成每个问题，不能悬挂工具 Future。
    #[tokio::test]
    async fn auto_resolution_finishes_with_empty_answers() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-auto").unwrap(),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-auto"));
        let frame = receiver.recv().await.expect("自动继续测试请求应送达");
        let request_id = frame_request_id(&frame);
        coordinator.auto_resolve(&request_id);
        let response = timeout(Duration::from_secs(2), answer)
            .await
            .expect("自动继续应唤醒 AskUser")
            .expect("空答案应通过工具响应校验");
        assert_eq!(response.answers.len(), 2);
        assert!(
            response
                .answers
                .iter()
                .all(|answer| answer.values.is_empty())
        );
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Workflow 控制面只能按真实 pending Schema 翻译单题兼容答案和多题结果。
    #[tokio::test]
    async fn workflow_answer_helper_translates_single_and_multi_answers() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let target = connection("workflow-answer-target");
        coordinator
            .negotiate_connection_capabilities(&target, &form_capabilities())
            .unwrap();
        coordinator
            .bind_session_connection("workflow-parent", &target)
            .unwrap();
        coordinator
            .bind_session_connection("workflow-actor", &target)
            .unwrap();
        let (sender, mut receiver) = mpsc::channel(2);
        let handler = coordinator.handler_for_connection_projected(
            SessionId::new("workflow-actor").unwrap(),
            "workflow-parent".to_owned(),
            target.clone(),
            Arc::new(RecordingSink { sender }),
        );
        let mut changes = coordinator.subscribe_changes();

        let mut single_request = request("workflow-actor");
        single_request.questions.truncate(1);
        let single_answer = handler.ask(single_request);
        let single_frame = receiver.recv().await.unwrap();
        assert_eq!(
            changes.recv().await.unwrap().display_session_id,
            "workflow-parent"
        );
        assert_eq!(
            serde_json::to_value(&single_frame).unwrap()["params"]["sessionId"],
            "workflow-parent"
        );
        let single_id = frame_request_id(&single_frame);
        let pending_views = coordinator.pending_views_for_connection(&target);
        assert_eq!(pending_views.len(), 1);
        assert_eq!(pending_views[0].request_id, single_id);
        assert_eq!(pending_views[0].session_id, "workflow-actor");
        assert_eq!(
            pending_views[0].display_session_id.as_deref(),
            Some("workflow-parent")
        );
        assert_eq!(
            coordinator
                .pending_session_id_for_request(&single_id)
                .as_deref(),
            Some("workflow-actor")
        );
        coordinator
            .respond_workflow_answer_from_connection(
                &target,
                &single_id,
                r#"{"optionId":"直接实现"}"#,
            )
            .expect("单题 optionId 应按原问题 Schema 转换");
        assert_eq!(single_answer.await.unwrap().answers[0].values, ["直接实现"]);

        let multi_answer = handler.ask(request("workflow-actor"));
        let multi_id = frame_request_id(&receiver.recv().await.unwrap());
        coordinator
            .respond_workflow_answer_from_connection(
                &target,
                &multi_id,
                &json!({
                    "answers": [
                        {"id": "strategy", "values": ["直接实现"]},
                        {"id": "checks", "values": ["测试", "Clippy"]}
                    ]
                })
                .to_string(),
            )
            .expect("多题 answers 应按原问题 Schema 转换");
        let response = multi_answer.await.unwrap();
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(response.answers[1].values, ["测试", "Clippy"]);
    }

    /// Source ElicitationDialog 的真实回执使用题目文案和 answer_N，必须还原到
    /// Rust 生成的 ACP question id，不能把 renderer-local 字段原样送进 ACP。
    #[test]
    fn source_form_answer_content_maps_to_question_ids() {
        let questions = request("source-answer").questions;
        let result = workflow_answer_result(
            &questions,
            json!({
                "action": "accept",
                "content": {
                    "answers": {
                        "选择实现策略": "直接实现",
                        "选择验证项": "测试, Clippy"
                    },
                    "answer_0": "直接实现",
                    "answer_1": ["测试", "Clippy"]
                }
            }),
        )
        .expect("Source 表单回答应按位置映射");
        assert_eq!(
            result,
            json!({
                "action": "accept",
                "content": {
                    "strategy": "直接实现",
                    "checks": ["测试", "Clippy"]
                }
            })
        );
    }

    /// Source 回执的未知字段或多选错误类型必须拒绝，避免用猜测继续模型回合。
    #[test]
    fn source_form_answer_content_rejects_unknown_or_ambiguous_values() {
        let questions = request("source-answer-invalid").questions;
        assert_eq!(
            workflow_answer_result(
                &questions,
                json!({
                    "action": "accept",
                    "content": {
                        "answers": {},
                        "answer_0": "直接实现",
                        "unexpected": "旁路"
                    }
                })
            ),
            Err(ElicitationBridgeError::InvalidResponse)
        );
        assert_eq!(
            workflow_answer_result(
                &questions,
                json!({
                    "action": "accept",
                    "content": {
                        "answers": {},
                        "answer_0": "直接实现",
                        "answer_1": "测试, Clippy"
                    }
                })
            ),
            Err(ElicitationBridgeError::InvalidResponse)
        );
    }

    /// UUID v7 交互标识跨 Coordinator/冷启动边界不依赖会重置的进程序号。
    #[test]
    fn elicitation_request_ids_are_uuid_backed_and_not_reused() {
        let first = next_request_id();
        let second = next_request_id();
        assert_ne!(first, second);
        for id in [first, second] {
            let suffix = id
                .strip_prefix("elicitation-")
                .expect("问答 ID 应带类型前缀");
            assert_eq!(suffix.len(), 36);
            assert_eq!(suffix.as_bytes()[8], b'-');
            assert_eq!(suffix.as_bytes()[13], b'-');
            assert_eq!(suffix.as_bytes()[18], b'-');
            assert_eq!(suffix.as_bytes()[23], b'-');
        }
    }

    /// 两个独立问答 Coordinator 模拟冷实例恢复时，新请求不能复用旧 ID。
    #[tokio::test]
    async fn elicitation_request_ids_stay_unique_across_coordinator_instances() {
        let first_coordinator = Arc::new(negotiated_coordinator());
        let (first_sender, mut first_receiver) = mpsc::channel(1);
        let first_handler = first_coordinator.handler(
            SessionId::new("cold-elicitation-1").unwrap(),
            Arc::new(RecordingSink {
                sender: first_sender,
            }),
        );
        let first_wait = first_handler.ask(request("cold-elicitation-1"));
        let first_id =
            frame_request_id(&first_receiver.recv().await.expect("首个实例应发出问答请求"));
        first_coordinator.shutdown();
        assert!(first_wait.await.is_err());

        let second_coordinator = Arc::new(negotiated_coordinator());
        let (second_sender, mut second_receiver) = mpsc::channel(1);
        let second_handler = second_coordinator.handler(
            SessionId::new("cold-elicitation-2").unwrap(),
            Arc::new(RecordingSink {
                sender: second_sender,
            }),
        );
        let second_wait = second_handler.ask(request("cold-elicitation-2"));
        let second_id = frame_request_id(
            &second_receiver
                .recv()
                .await
                .expect("恢复实例应发出新的问答请求"),
        );
        assert_ne!(first_id, second_id);
        second_coordinator.shutdown();
        assert!(second_wait.await.is_err());
    }

    /// 同 Session 的其他连接不能抢答，拒绝后原请求仍可由目标连接完成。
    #[tokio::test]
    async fn response_is_restricted_to_the_target_connection() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let target = connection("web-target");
        let other = connection("web-other");
        coordinator
            .negotiate_connection_capabilities(&target, &form_capabilities())
            .unwrap();
        coordinator
            .negotiate_connection_capabilities(&other, &form_capabilities())
            .unwrap();
        coordinator
            .bind_session_connection("session-shared", &target)
            .unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler_for_connection(
            SessionId::new("session-shared").unwrap(),
            target.clone(),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-shared"));
        let frame = receiver.recv().await.expect("目标连接应收到请求");
        let request_id = frame_request_id(&frame);
        let response = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "action": "accept",
                "content": { "strategy": "直接实现", "checks": ["测试"] }
            }
        })
        .to_string();

        assert_eq!(
            coordinator.respond_from_connection(&other, &response),
            Err(ElicitationBridgeError::ResponseConnectionMismatch)
        );
        assert_eq!(coordinator.pending_len(), 1);
        coordinator
            .respond_from_connection(&target, &response)
            .expect("目标连接仍应完成原请求");
        assert!(answer.await.is_ok());
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// 断线取消旧请求；同标识重连后迟到旧响应不能命中新请求。
    #[tokio::test]
    async fn disconnect_cancels_pending_and_reconnect_does_not_reuse_old_response() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let target = connection("web-reconnect");
        coordinator
            .negotiate_connection_capabilities(&target, &form_capabilities())
            .unwrap();
        let (sender, mut receiver) = mpsc::channel(2);
        let sink: Arc<dyn ClientRequestSink> = Arc::new(RecordingSink { sender });
        let handler = coordinator.handler_for_connection(
            SessionId::new("session-reconnect").unwrap(),
            target.clone(),
            Arc::clone(&sink),
        );
        let old_answer = handler.ask(request("session-reconnect"));
        let old_frame = receiver.recv().await.expect("旧连接应收到请求");
        let old_request_id = frame_request_id(&old_frame);
        let old_response = json!({
            "jsonrpc": "2.0",
            "id": old_request_id.clone(),
            "result": { "action": "cancel" }
        })
        .to_string();

        coordinator.disconnect(&target);
        assert!(old_answer.await.is_err());
        assert_eq!(coordinator.pending_len(), 0);
        assert_eq!(
            coordinator.respond_from_connection(&target, &old_response),
            Err(ElicitationBridgeError::UnknownRequest)
        );

        coordinator
            .negotiate_connection_capabilities(&target, &form_capabilities())
            .expect("重连必须重新协商能力");
        coordinator
            .bind_session_connection("session-reconnect", &target)
            .unwrap();
        let reconnected = coordinator.handler_for_connection(
            SessionId::new("session-reconnect").unwrap(),
            target.clone(),
            sink,
        );
        let new_answer = reconnected.ask(request("session-reconnect"));
        let new_frame = receiver.recv().await.expect("重连后应收到新请求");
        let new_request_id = frame_request_id(&new_frame);
        assert_ne!(old_request_id, new_request_id);
        assert_eq!(
            coordinator.respond_from_connection(&target, &old_response),
            Err(ElicitationBridgeError::UnknownRequest)
        );
        coordinator
            .respond_from_connection(
                &target,
                &json!({
                    "jsonrpc": "2.0",
                    "id": new_request_id,
                    "result": { "action": "cancel" }
                })
                .to_string(),
            )
            .expect("新请求应由重连后的连接正常收口");
        assert!(new_answer.await.is_err());
    }

    /// 目标连接与其他连接并发响应时，非目标响应永远不能消费请求。
    #[tokio::test]
    async fn concurrent_cross_connection_response_cannot_win() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let target = connection("web-race-target");
        let other = connection("web-race-other");
        for connection_id in [&target, &other] {
            coordinator
                .negotiate_connection_capabilities(connection_id, &form_capabilities())
                .unwrap();
        }
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler_for_connection(
            SessionId::new("session-race").unwrap(),
            target.clone(),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-race"));
        let request_id = frame_request_id(&receiver.recv().await.unwrap());
        let response = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "action": "accept",
                "content": { "strategy": "直接实现", "checks": ["测试"] }
            }
        })
        .to_string();
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let wrong = {
            let coordinator = Arc::clone(&coordinator);
            let connection_id = other.clone();
            let response = response.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                coordinator.respond_from_connection(&connection_id, &response)
            })
        };
        let correct = {
            let coordinator = Arc::clone(&coordinator);
            let connection_id = target.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                coordinator.respond_from_connection(&connection_id, &response)
            })
        };
        barrier.wait().await;
        assert_eq!(correct.await.unwrap(), Ok(()));
        assert!(matches!(
            wrong.await.unwrap(),
            Err(ElicitationBridgeError::ResponseConnectionMismatch
                | ElicitationBridgeError::UnknownRequest)
        ));
        assert!(answer.await.is_ok());
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Schema 属性按字典序序列化时，questionOrder 仍保留用户问题和答案顺序。
    #[tokio::test]
    async fn question_order_meta_preserves_input_order_when_schema_properties_are_sorted() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(non_dictionary_order_request("session-a"));
        let frame = receiver.recv().await.expect("三题问答请求应送达");
        let value = serde_json::to_value(&frame).expect("标准问答请求应可序列化");
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["allowCustomByQuestion"],
            json!({ "target": true, "checks": false, "note": true })
        );
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["questionOrder"],
            json!(["target", "checks", "note"])
        );

        let serialized = serde_json::to_string(&frame).expect("标准问答请求应可序列化");
        let properties_start = serialized
            .find("\"properties\":")
            .expect("序列化请求应包含 properties");
        let serialized_properties = &serialized[properties_start..];
        let checks_position = serialized_properties
            .find("\"checks\":")
            .expect("properties 应包含 checks");
        let note_position = serialized_properties
            .find("\"note\":")
            .expect("properties 应包含 note");
        let target_position = serialized_properties
            .find("\"target\":")
            .expect("properties 应包含 target");
        assert!(
            checks_position < note_position && note_position < target_position,
            "properties 应按字典序序列化: {serialized}"
        );

        let request_id = value["id"].as_str().expect("请求 ID 应为字符串");
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "target": "服务器",
                            "checks": ["测试"],
                            "note": "保留默认配置"
                        }
                    }
                })
                .to_string(),
            )
            .expect("三题合法答案应完成");
        let response = answer.await.expect("AskUser 应收到三题答案");
        assert_eq!(
            response
                .answers
                .iter()
                .map(|answer| answer.id.as_str())
                .collect::<Vec<_>>(),
            vec!["target", "checks", "note"]
        );
        assert_eq!(response.answers[0].values, ["服务器"]);
        assert_eq!(response.answers[1].values, ["测试"]);
        assert_eq!(response.answers[2].values, ["保留默认配置"]);
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// 错 Session、并发弹窗、迟到响应和 Future 丢弃都必须失败关闭。
    #[tokio::test]
    async fn question_handler_rejects_wrong_session_concurrency_and_late_response() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(2);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        assert!(handler.ask(request("session-b")).await.is_err());
        let pending = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("首个请求应送达");
        assert!(handler.ask(request("session-a")).await.is_err());
        let request_id = serde_json::to_value(frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        drop(pending);
        tokio::task::yield_now().await;
        assert_eq!(coordinator.pending_len(), 0);
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": { "action": "cancel" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::UnknownRequest)
        );
    }

    /// 用户取消应被当作已处理响应，而已知请求的畸形结果必须失败关闭并唤醒工具。
    #[tokio::test]
    async fn question_handler_finishes_cancel_and_malformed_response_exactly_once() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(2);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );

        let cancelled = handler.ask(request("session-a"));
        let cancelled_frame = receiver.recv().await.expect("取消请求应先送达");
        let cancelled_id = serde_json::to_value(cancelled_frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": cancelled_id,
                    "result": { "action": "cancel" }
                })
                .to_string(),
            )
            .expect("标准取消响应应被成功收口");
        assert!(cancelled.await.is_err());
        assert_eq!(coordinator.pending_len(), 0);

        let malformed = handler.ask(request("session-a"));
        let malformed_frame = receiver.recv().await.expect("畸形响应请求应先送达");
        let malformed_id = serde_json::to_value(malformed_frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": malformed_id,
                    "result": { "action": "unsupported" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::InvalidResponse)
        );
        assert!(malformed.await.is_err());
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Client 在 emit 后、投递回执前立即回答时不能丢失真实答案。
    #[tokio::test]
    async fn response_visible_before_delivery_ack_is_accepted_exactly_once() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let release = Arc::new(Notify::new());
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(BlockingAcknowledgementSink {
                sender,
                release: Arc::clone(&release),
            }),
        );
        let answer = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("问答请求应先对 Client 可见");
        let request_id = frame_request_id(&frame);

        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "strategy": "直接实现",
                            "checks": ["测试"]
                        }
                    }
                })
                .to_string(),
            )
            .expect("已经对 Client 可见的问答应接受立即响应");
        let response = timeout(Duration::from_secs(2), answer)
            .await
            .expect("立即响应应在投递回执前唤醒 AskUser")
            .expect("合法答案应返回工具");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(response.answers[1].values, ["测试"]);
        assert_eq!(coordinator.pending_len(), 0);
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": { "action": "cancel" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::UnknownRequest)
        );
        release.notify_one();
    }
}
