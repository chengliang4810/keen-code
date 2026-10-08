//! 桌面 Runtime 的 Native typed 结构化问答桥。

use crate::agent_runtime::AppRuntimePreferences;
use crate::client_request::{ClientRequestDisplayGate, ClientRequestDisplayPermit};
use keencode_acp::ConnectionId;
use keencode_agent::{CollaborationIdGenerator, UuidCollaborationIdGenerator};
use keencode_tools::{
    UserQuestion, UserQuestionAnswer, UserQuestionError, UserQuestionFuture, UserQuestionHandler,
    UserQuestionRequest, UserQuestionResponse,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
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

/// Elicitation 注册或响应违反桌面桥不变量时返回的稳定错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElicitationBridgeError {
    /// Native Host 尚未绑定可用的问答连接。
    ConnectionUnavailable,
    /// Runtime 已关闭，不再接受新问答。
    RuntimeClosed,
    /// Handler 收到了其他 Session 的请求。
    SessionMismatch,
    /// 同一 Session 已经存在一个未结束的可见问答。
    SessionBusy,
    /// Native Host 返回的 typed 答案不符合当前问题 Schema。
    InvalidResponse,
    /// 响应携带的请求标识不存在或已经结束。
    UnknownRequest,
    /// 响应来自非请求目标连接。
    ResponseConnectionMismatch,
    /// 用户拒绝或取消了本次问答。
    Cancelled,
    /// 内部等待通道或状态不变量被破坏。
    InternalState,
}

impl std::fmt::Display for ElicitationBridgeError {
    /// 输出不包含问题正文、选项或回答的安全错误说明。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionUnavailable => formatter.write_str("问答连接尚未绑定"),
            Self::RuntimeClosed => formatter.write_str("问答 Runtime 已关闭"),
            Self::SessionMismatch => formatter.write_str("问答请求与当前 Session 不匹配"),
            Self::SessionBusy => formatter.write_str("当前 Session 已有待回答问题"),
            Self::InvalidResponse => formatter.write_str("问答答案无效"),
            Self::UnknownRequest => formatter.write_str("待决问答不存在或已经结束"),
            Self::ResponseConnectionMismatch => formatter.write_str("问答答案来自非目标连接"),
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

/// 一个待决问答发生变化时发出的轻量路由通知。
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
/// Session。调用方仍必须按连接和 actor→parent 路由复核，不能把此结构直接下发 UI。
#[derive(Clone, Debug)]
pub(crate) struct PendingElicitationView {
    /// Native typed interaction 的稳定 ID。
    pub(crate) request_id: String,
    /// 真实发起问答的 Session，actor 问答不是父 Session。
    pub(crate) session_id: String,
    /// 允许展示问答的父 Session；普通问答为自身 Session。
    pub(crate) display_session_id: Option<String>,
    /// 原始、已由 core/tools 校验的问题 Schema。
    pub(crate) questions: Vec<UserQuestion>,
    /// 问答登记时的墙钟时间，供原生 UI 和诊断排序使用。
    pub(crate) created_at_unix_ms: u64,
}

/// 自动继续状态的 Rust 内部值，不作为第二份事实源。
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

/// 单个待决 typed interaction 的不可变绑定和一次性等待者。
struct PendingElicitation {
    /// Native typed interaction 的稳定 ID。
    request_id: String,
    /// 请求唯一允许响应的 Native 连接。
    connection_id: ConnectionId,
    /// 请求绑定的唯一 Session。
    session_id: String,
    /// 可选的父 Session；actor 问答展示到这里。
    display_session_id: Option<String>,
    /// 用于把结构化响应还原为工具答案的原始问题定义。
    questions: Vec<UserQuestion>,
    /// 问答登记时的墙钟时间。
    created_at_unix_ms: u64,
    /// 仅在 App 偏好开启期间存在的自动继续状态。
    auto_resolution: Option<PendingElicitationAutoResolution>,
    /// 到期自动继续任务；响应、断线、暂停或 Runtime 关闭时必须取消。
    auto_resolution_abort: Option<AbortHandle>,
    /// 从 pending 登记一直持有到响应或取消收口的 Session 展示许可。
    _display_permit: Option<ClientRequestDisplayPermit>,
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
    /// Native Host 的进程内连接身份。
    native_connections: HashSet<ConnectionId>,
    /// 当前 Session 正在执行的 Prompt 所属连接。
    session_connections: HashMap<String, ConnectionId>,
    /// 按稳定 interaction ID 保存的待决请求。
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
    /// 持有到响应终态的每 Session 展示串行门。
    client_request_gate: Arc<ClientRequestDisplayGate>,
    /// 与 AgentRuntime 共用的 App 偏好快照，保证设置变更能影响现有 pending。
    app_runtime_preferences: Arc<std::sync::RwLock<AppRuntimePreferences>>,
}

impl ElicitationCoordinator {
    /// 创建不恢复旧问答的 Native typed 协调器。
    pub fn new() -> Self {
        Self::with_gate(Arc::new(ClientRequestDisplayGate::new()))
    }

    /// 使用 Runtime 共享展示门创建 Native typed 协调器。
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
                    native_connections: HashSet::new(),
                    session_connections: HashMap::new(),
                    pending: HashMap::new(),
                    pending_by_session: HashMap::new(),
                    closed: false,
                }),
                changes: broadcast::channel(256).0,
            }),
            client_request_gate,
            app_runtime_preferences,
        }
    }

    /// Native Host 的指定连接始终使用 typed pending 问答。
    pub fn supports_form_for_connection(&self, connection_id: &ConnectionId) -> bool {
        self.is_native_connection(connection_id)
    }

    /// 返回连接是否由 Native Host 绑定。
    pub(crate) fn is_native_connection(&self, connection_id: &ConnectionId) -> bool {
        self.inner
            .state
            .lock()
            .native_connections
            .contains(connection_id)
    }

    /// 绑定 Native Host 当前 Prompt 使用的进程内连接。
    pub(crate) fn bind_native_session(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
    ) -> Result<(), ElicitationBridgeError> {
        let mut state = self.inner.state.lock();
        if state.closed {
            return Err(ElicitationBridgeError::RuntimeClosed);
        }
        state.native_connections.insert(connection_id.clone());
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
            .is_some_and(|connection_id| state.native_connections.contains(connection_id))
    }

    /// 为 Native Host 创建 typed 问答 Handler。
    pub(crate) fn handler_native(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        connection_id: ConnectionId,
    ) -> NativeQuestionHandler {
        self.handler_native_projected(session_id, None, connection_id)
    }

    /// 为 Native Host 创建把 actor 问答显示到父 Session 的 typed Handler。
    pub(crate) fn handler_native_projected(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        display_session_id: Option<String>,
        connection_id: ConnectionId,
    ) -> NativeQuestionHandler {
        NativeQuestionHandler {
            session_id,
            display_session_id,
            connection_id,
            coordinator: Arc::clone(self),
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

    /// 订阅 Native Host 的 pending 问答变化。
    pub(crate) fn subscribe_pending(&self) -> broadcast::Receiver<ElicitationChange> {
        self.subscribe_changes()
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
            .filter(|pending| &pending.connection_id == connection_id)
            .map(|pending| PendingElicitationView {
                request_id: pending.request_id.clone(),
                session_id: pending.session_id.clone(),
                display_session_id: pending.display_session_id.clone(),
                questions: pending.questions.clone(),
                created_at_unix_ms: pending.created_at_unix_ms,
            })
            .collect()
    }

    /// 读取 Native Host 当前连接和 Session 可见的 pending 问答。
    pub(crate) fn pending_for_native_session(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
    ) -> Vec<PendingElicitationView> {
        self.pending_views_for_connection(connection_id)
            .into_iter()
            .filter(|view| {
                view.display_session_id
                    .as_deref()
                    .unwrap_or(view.session_id.as_str())
                    == session_id
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

    /// 返回当前 pending 的展示 Session；通知泵丢帧恢复时只扫描真实待决项。
    pub(crate) fn pending_display_session_ids(&self) -> Vec<String> {
        let mut session_ids = self
            .inner
            .state
            .lock()
            .pending
            .values()
            .map(|pending| {
                pending
                    .display_session_id
                    .clone()
                    .unwrap_or_else(|| pending.session_id.clone())
            })
            .collect::<Vec<_>>();
        session_ids.sort_unstable();
        session_ids.dedup();
        session_ids
    }

    /// 返回指定 Session 当前唯一待决 interaction 的稳定标识。
    ///
    /// Host admission 只保存稳定标识和状态，不复制问答正文；该只读查询让
    /// HostPromptQueue 在 Agent 发出请求后记录 `NeedsInput`，实际回答仍由本
    /// 协调器负责严格校验和 exactly-once 收口。
    pub fn pending_request_id_for_session(&self, session_id: &str) -> Option<String> {
        self.inner
            .state
            .lock()
            .pending_by_session
            .get(session_id)
            .cloned()
    }

    /// Session 关闭时移除其 Native 连接绑定，并收口该 Session 可见的 pending 问答。
    ///
    /// 连接本身由 Native Host 共享，不能调用 `disconnect`；这里只清理 Session，
    /// 保留同一 `ConnectionId` 对其他活动窗口和 Session 的所有权。
    pub(crate) fn close_session(&self, session_id: &str) {
        let (pending, displays) = {
            let mut state = self.inner.state.lock();
            state.session_connections.remove(session_id);
            let request_ids = state
                .pending
                .iter()
                .filter(|(_, pending)| {
                    pending.session_id == session_id
                        || pending.display_session_id.as_deref() == Some(session_id)
                })
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            let mut pending_items = Vec::new();
            let mut displays = Vec::new();
            for request_id in request_ids {
                if let Some(item) = state.pending.remove(&request_id) {
                    state.pending_by_session.remove(&item.session_id);
                    displays.push((item.display_session_id.clone(), item.session_id.clone()));
                    pending_items.push(item);
                }
            }
            (pending_items, displays)
        };
        for mut pending in pending {
            if let Some(abort) = pending.auto_resolution_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::Cancelled.into_user_question_error()
            ));
        }
        for (display_session_id, pending_session_id) in displays {
            self.notify_change(display_session_id.as_deref(), &pending_session_id);
        }
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

    /// 判断稳定 interaction ID 是否属于一个待决问答。
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
            .map_err(|_| ElicitationBridgeError::InternalState)?;
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
            coordinator.auto_resolve_after_deadline(&task_request_id);
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

    /// 单测显式以每题空答案完成 AskUser；这与用户跳过问题使用同一 core 校验路径。
    #[cfg(test)]
    fn auto_resolve(&self, request_id: &str) {
        self.finish_auto_resolution(request_id, false);
    }

    /// 计时器到期时收口 AskUser；停用或暂停后的迟到计时不得继续自动回答。
    fn auto_resolve_after_deadline(&self, request_id: &str) {
        self.finish_auto_resolution(request_id, true);
    }

    /// 按调用来源决定是否要求 pending 仍处于 Active，再通过唯一 waiter 收口。
    fn finish_auto_resolution(&self, request_id: &str, require_active: bool) {
        let pending = {
            let mut state = self.inner.state.lock();
            let Some(pending) = state.pending.get(request_id) else {
                return;
            };
            if require_active
                && !matches!(
                    pending.auto_resolution,
                    Some(PendingElicitationAutoResolution::Active { .. })
                )
            {
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

    /// 将 Workflow/Interaction 控制面的 typed 答案按当前问题 Schema 收口。
    ///
    /// 转换只读取该 request 的原始 `pending.questions`，因此单题的
    /// `optionId/freeText`、多题 typed `answers` 以及 question-id content 都必须
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
        let response = {
            let state = self.inner.state.lock();
            let Some(pending) = state.pending.get(request_id) else {
                return Err(ElicitationBridgeError::UnknownRequest);
            };
            if &pending.connection_id != connection_id {
                return Err(ElicitationBridgeError::ResponseConnectionMismatch);
            }
            workflow_answer_response(&pending.questions, answer)?
        };
        self.finish_typed_response(connection_id, request_id, response)
    }

    /// Native Host typed 问答回执；答案正文仍按原始问题 Schema 严格校验。
    pub(crate) fn respond_native(
        &self,
        connection_id: &ConnectionId,
        request_id: &str,
        answer_json: &str,
    ) -> Result<(), ElicitationBridgeError> {
        self.respond_workflow_answer_from_connection(connection_id, request_id, answer_json)
    }

    /// 在唯一目标连接上 exactly-once 收口 typed pending；展示层只观察账本变化。
    fn finish_typed_response(
        &self,
        connection_id: &ConnectionId,
        request_id: &str,
        result: Result<UserQuestionResponse, ElicitationBridgeError>,
    ) -> Result<(), ElicitationBridgeError> {
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get(request_id) else {
            return Err(ElicitationBridgeError::UnknownRequest);
        };
        if &pending.connection_id != connection_id {
            return Err(ElicitationBridgeError::ResponseConnectionMismatch);
        }
        let Some(mut pending) = state.pending.remove(request_id) else {
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

    /// 关闭 Runtime，取消全部内存问答且不接受迟到响应。
    pub fn shutdown(&self) {
        let mut state = self.inner.state.lock();
        if state.closed && state.pending.is_empty() {
            return;
        }
        state.closed = true;
        state.native_connections.clear();
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
            if let Some(abort) = pending.auto_resolution_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::RuntimeClosed.into_user_question_error()
            ));
        }
    }

    /// 断开连接时取消其全部待决问答，并移除 Native Session 绑定。
    pub fn disconnect(&self, connection_id: &ConnectionId) {
        let mut state = self.inner.state.lock();
        state.native_connections.remove(connection_id);
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
            if let Some(abort) = pending.auto_resolution_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::Cancelled.into_user_question_error()
            ));
        }
    }

    /// 在 typed Native pending 账本中登记问答。
    async fn register_native(
        &self,
        handler_session_id: &keencode_agent::SessionId,
        connection_id: &ConnectionId,
        request: UserQuestionRequest,
        display_session_id: Option<&str>,
    ) -> Result<RegisteredElicitation, ElicitationBridgeError> {
        if &request.session_id != handler_session_id {
            return Err(ElicitationBridgeError::SessionMismatch);
        }
        let session_id = request.session_id.as_str().to_owned();
        {
            let state = self.inner.state.lock();
            if state.closed {
                return Err(ElicitationBridgeError::RuntimeClosed);
            }
            if !state.native_connections.contains(connection_id) {
                return Err(ElicitationBridgeError::ConnectionUnavailable);
            }
            if state.session_connections.get(&session_id) != Some(connection_id) {
                return Err(ElicitationBridgeError::SessionMismatch);
            }
        }
        let display_session_id = display_session_id.map(str::to_owned);
        let display_gate_session = display_session_id
            .as_deref()
            .unwrap_or(&session_id)
            .to_owned();
        let display_permit = self
            .client_request_gate
            .acquire(&display_gate_session)
            .await
            .ok_or(ElicitationBridgeError::InternalState)?;
        let request_id = next_request_id();
        let created_at_unix_ms = crate::agent_runtime::unix_time_ms();
        let auto_resolution_enabled = self
            .app_runtime_preferences
            .read()
            .map(|preferences| preferences.ask_user_question_auto_resolution_enabled)
            .unwrap_or(false);
        let auto_resolution =
            auto_resolution_enabled.then(|| active_auto_resolution(created_at_unix_ms));
        let (waiter, receiver) = oneshot::channel();
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return Err(ElicitationBridgeError::RuntimeClosed);
            }
            if !state.native_connections.contains(connection_id)
                || state.session_connections.get(&session_id) != Some(connection_id)
            {
                return Err(ElicitationBridgeError::SessionMismatch);
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
                    display_session_id: display_session_id.clone(),
                    questions: request.questions,
                    created_at_unix_ms,
                    auto_resolution,
                    auto_resolution_abort: None,
                    _display_permit: Some(display_permit),
                    waiter,
                },
            );
        }
        if auto_resolution_enabled
            && let Err(error) = self.schedule_auto_resolution(request_id.clone())
        {
            self.cancel_request(&request_id, error);
            return Err(error);
        }
        self.notify_change(display_session_id.as_deref(), &session_id);
        Ok(RegisteredElicitation {
            guard: PendingElicitationGuard {
                coordinator: self.clone(),
                request_id,
                armed: true,
            },
            receiver,
        })
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

/// Native Host 的 typed AskUser 实现；pending 正文只存在 Coordinator 账本中。
pub(crate) struct NativeQuestionHandler {
    /// 该 Handler 唯一允许接收的 Agent Session。
    session_id: keencode_agent::SessionId,
    /// 可选的父 Session；actor 问答展示到这里。
    display_session_id: Option<String>,
    /// Native Host 固定的进程内连接身份。
    connection_id: ConnectionId,
    /// 进程内共享的问答协调器。
    coordinator: Arc<ElicitationCoordinator>,
}

/// `ask()` 的登记任务在 Future 尚未被轮询时也必须可取消，避免调用方丢弃
/// Future 后遗留不可见 pending。生产入口通常已处于 Tokio Runtime，非 Tokio
/// 调用方则回退为惰性登记路径。
struct RegistrationTaskAbort(AbortHandle);

impl Drop for RegistrationTaskAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn wait_registered_elicitation(
    registration: Result<RegisteredElicitation, ElicitationBridgeError>,
) -> Result<UserQuestionResponse, UserQuestionError> {
    let registration = registration.map_err(ElicitationBridgeError::into_user_question_error)?;
    let RegisteredElicitation {
        mut guard,
        receiver,
    } = registration;
    let result = receiver
        .await
        .unwrap_or_else(|_| Err(ElicitationBridgeError::InternalState.into_user_question_error()));
    guard.armed = false;
    result
}

impl UserQuestionHandler for NativeQuestionHandler {
    /// 登记 typed pending 并等待 Native Host 通过 `respond_native` 回答。
    fn ask(&self, request: UserQuestionRequest) -> UserQuestionFuture<'_> {
        let coordinator = Arc::clone(&self.coordinator);
        let session_id = self.session_id.clone();
        let connection_id = self.connection_id.clone();
        let display_session_id = self.display_session_id.clone();
        let registration = async move {
            coordinator
                .register_native(
                    &session_id,
                    &connection_id,
                    request,
                    display_session_id.as_deref(),
                )
                .await
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return Box::pin(async move { wait_registered_elicitation(registration.await).await });
        };
        let task = handle.spawn(registration);
        let abort = RegistrationTaskAbort(task.abort_handle());
        Box::pin(async move {
            let registration = match task.await {
                Ok(registration) => registration,
                Err(_) => Err(ElicitationBridgeError::InternalState),
            };
            let _abort = abort;
            wait_registered_elicitation(registration).await
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
    /// 当前 Future 对应的 interaction 标识。
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

/// 依照待决问题的真实形状，把控制面答案收敛为内部 typed 响应对象。
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
                    "content": workflow_content_from_question_map(questions, &content)?,
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

/// 将已按问题 Schema 归一化的控制面答案转换为工具端 typed 响应。
fn workflow_answer_response(
    questions: &[UserQuestion],
    answer: Value,
) -> Result<Result<UserQuestionResponse, ElicitationBridgeError>, ElicitationBridgeError> {
    let normalized = workflow_answer_result(questions, answer)?;
    let action = normalized
        .get("action")
        .and_then(Value::as_str)
        .ok_or(ElicitationBridgeError::InvalidResponse)?;
    if action != "accept" {
        return Ok(Err(ElicitationBridgeError::Cancelled));
    }
    let content = normalized
        .get("content")
        .and_then(Value::as_object)
        .ok_or(ElicitationBridgeError::InvalidResponse)?;
    let mut answers = Vec::with_capacity(questions.len());
    for question in questions {
        let values = match content.get(&question.id) {
            None => Vec::new(),
            Some(Value::String(value)) if !question.multi_select => vec![value.clone()],
            Some(Value::Array(values)) if question.multi_select => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or(ElicitationBridgeError::InvalidResponse)
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err(ElicitationBridgeError::InvalidResponse),
        };
        answers.push(UserQuestionAnswer {
            id: question.id.clone(),
            values,
        });
    }
    if content
        .keys()
        .any(|key| !questions.iter().any(|question| question.id == *key))
    {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    Ok(Ok(UserQuestionResponse { answers }))
}

/// 将 AskUser 输出的 `answers:[{id,values}]` 映射为 typed content。
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

/// 直接使用 question id 的 typed content；未知字段和类型都拒绝，避免猜测。
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

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_agent::{AgentId, SessionId, ToolCallId, TurnId};
    use keencode_tools::UserQuestionOption;
    use serde_json::json;
    use tokio::time::{Duration, timeout};

    /// 构造测试 Native Host 的稳定连接标识。
    fn connection(value: &str) -> ConnectionId {
        ConnectionId::new(value).expect("测试连接标识应合法")
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

    /// Native Host 的 typed Handler 不需要额外 transport，且回答仍严格复用原问题 Schema。
    #[tokio::test]
    async fn native_question_handler_registers_typed_pending_without_transport_frame() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let connection_id = connection("native-typed");
        let session = SessionId::new("session-native-typed").expect("测试 Session 标识有效");
        coordinator
            .bind_native_session(session.as_str(), &connection_id)
            .expect("Native Session 应绑定进程内连接");
        assert!(coordinator.session_supports_form(session.as_str()));

        let handler = coordinator.handler_native(session.clone(), connection_id.clone());
        let mut request = request(session.as_str());
        request.questions.truncate(1);
        let answer = handler.ask(request);
        timeout(Duration::from_secs(1), async {
            loop {
                if !coordinator
                    .pending_for_native_session(session.as_str(), &connection_id)
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("typed pending 应及时登记");
        let request_id = coordinator
            .pending_request_id_for_session(session.as_str())
            .expect("Native pending 应保留请求标识");
        coordinator
            .respond_native(&connection_id, &request_id, r#"{"optionId":"直接实现"}"#)
            .expect("Native typed 回执应通过原问题 Schema");
        let response = answer.await.expect("Native AskUser 应收到答案");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Session 关闭只移除自身绑定，不能断开仍被其他 Native Session 使用的连接。
    #[tokio::test]
    async fn closing_native_session_cancels_pending_and_keeps_shared_connection() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let connection_id = connection("native-shared");
        let first = SessionId::new("session-native-first").unwrap();
        let second = SessionId::new("session-native-second").unwrap();
        coordinator
            .bind_native_session(first.as_str(), &connection_id)
            .unwrap();
        let handler = coordinator.handler_native(first.clone(), connection_id.clone());
        let answer = handler.ask(request(first.as_str()));
        timeout(Duration::from_secs(1), async {
            loop {
                if coordinator.pending_len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("关闭测试应先登记 pending");

        coordinator.close_session(first.as_str());

        assert!(answer.await.is_err());
        assert!(!coordinator.session_supports_form(first.as_str()));
        coordinator
            .bind_native_session(second.as_str(), &connection_id)
            .unwrap();
        assert!(coordinator.session_supports_form(second.as_str()));
    }

    /// Workflow 控制面只能按真实 pending Schema 翻译单题 optionId 和多题结果。
    #[tokio::test]
    async fn workflow_answer_helper_translates_single_and_multi_answers() {
        let coordinator = Arc::new(ElicitationCoordinator::new());
        let target = connection("workflow-answer-target");
        coordinator
            .bind_native_session("workflow-parent", &target)
            .unwrap();
        coordinator
            .bind_native_session("workflow-actor", &target)
            .unwrap();
        let handler = coordinator.handler_native_projected(
            SessionId::new("workflow-actor").unwrap(),
            Some("workflow-parent".to_owned()),
            target.clone(),
        );
        let mut changes = coordinator.subscribe_changes();

        let mut single_request = request("workflow-actor");
        single_request.questions.truncate(1);
        let single_answer = handler.ask(single_request);
        timeout(Duration::from_secs(1), async {
            loop {
                if !coordinator
                    .pending_for_native_session("workflow-parent", &target)
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Workflow typed pending 应及时登记");
        assert_eq!(
            changes.recv().await.unwrap().display_session_id,
            "workflow-parent"
        );
        let single_id = coordinator
            .pending_request_id_for_session("workflow-actor")
            .expect("actor pending 应保留请求标识");
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
        let multi_id = timeout(Duration::from_secs(1), async {
            loop {
                if let Some(request_id) =
                    coordinator.pending_request_id_for_session("workflow-actor")
                {
                    break request_id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("多题 pending 应及时登记");
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

    /// 标准 Workflow accept content 直接使用 Rust 生成的 question id，并保留多选数组。
    #[test]
    fn workflow_answer_content_accepts_question_ids() {
        let questions = request("workflow-question-map").questions;
        let result = workflow_answer_result(
            &questions,
            json!({
                "action": "accept",
                "content": {
                    "strategy": "直接实现",
                    "checks": ["测试", "Clippy"]
                }
            }),
        )
        .expect("question-id content 应按原问题 Schema 转换");
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

    /// 已退役 renderer 的题目文案、位置字段和单题裸 answer 必须拒绝。
    #[test]
    fn retired_renderer_answer_fields_are_rejected() {
        let mut single_questions = request("retired-renderer-single").questions;
        single_questions.truncate(1);
        assert_eq!(
            workflow_answer_result(
                &single_questions,
                json!({
                    "action": "accept",
                    "content": {"answer": "直接实现"}
                })
            ),
            Err(ElicitationBridgeError::InvalidResponse)
        );

        let questions = request("retired-renderer-multi").questions;
        assert_eq!(
            workflow_answer_result(
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

    /// 两个 Native Coordinator 模拟冷实例恢复时，新请求不能复用旧 ID。
    #[tokio::test]
    async fn elicitation_request_ids_stay_unique_across_native_coordinators() {
        let first_coordinator = Arc::new(ElicitationCoordinator::new());
        let first_connection = connection("cold-native-1");
        let first_session = SessionId::new("cold-native-session-1").unwrap();
        first_coordinator
            .bind_native_session(first_session.as_str(), &first_connection)
            .unwrap();
        let first_handler =
            first_coordinator.handler_native(first_session.clone(), first_connection.clone());
        let first_wait = first_handler.ask(request(first_session.as_str()));
        timeout(Duration::from_secs(1), async {
            loop {
                if first_coordinator.pending_len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let first_id = first_coordinator
            .pending_request_id_for_session(first_session.as_str())
            .unwrap();
        first_coordinator.shutdown();
        assert!(first_wait.await.is_err());

        let second_coordinator = Arc::new(ElicitationCoordinator::new());
        let second_connection = connection("cold-native-2");
        let second_session = SessionId::new("cold-native-session-2").unwrap();
        second_coordinator
            .bind_native_session(second_session.as_str(), &second_connection)
            .unwrap();
        let second_handler =
            second_coordinator.handler_native(second_session.clone(), second_connection);
        let second_wait = second_handler.ask(request(second_session.as_str()));
        timeout(Duration::from_secs(1), async {
            loop {
                if second_coordinator.pending_len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let second_id = second_coordinator
            .pending_request_id_for_session(second_session.as_str())
            .unwrap();
        assert_ne!(first_id, second_id);
        second_coordinator.shutdown();
        assert!(second_wait.await.is_err());
    }

    /// Native pending 的展示偏好会立即影响已有请求，自动继续仍共用同一账本。
    #[tokio::test]
    async fn native_pending_preference_and_auto_resolution_are_typed() {
        let preferences = Arc::new(std::sync::RwLock::new(AppRuntimePreferences::default()));
        let coordinator = Arc::new(ElicitationCoordinator::with_gate_and_preferences(
            Arc::new(ClientRequestDisplayGate::new()),
            preferences.clone(),
        ));
        let connection_id = connection("native-preference");
        let session = SessionId::new("native-preference-session").unwrap();
        coordinator
            .bind_native_session(session.as_str(), &connection_id)
            .unwrap();
        let handler = coordinator.handler_native(session.clone(), connection_id.clone());
        *preferences.write().unwrap() = AppRuntimePreferences {
            ask_user_question_auto_resolution_enabled: true,
        };
        coordinator.sync_auto_resolution_preference(true);
        let answer = handler.ask(request(session.as_str()));
        timeout(Duration::from_secs(1), async {
            loop {
                if !coordinator
                    .pending_for_native_session(session.as_str(), &connection_id)
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let request_id = coordinator
            .pending_request_id_for_session(session.as_str())
            .unwrap();
        assert!(matches!(
            coordinator
                .inner
                .state
                .lock()
                .pending
                .get(&request_id)
                .and_then(|pending| pending.auto_resolution.as_ref()),
            Some(PendingElicitationAutoResolution::Active { .. })
        ));
        *preferences.write().unwrap() = AppRuntimePreferences::default();
        coordinator.sync_auto_resolution_preference(false);
        assert!(
            coordinator
                .inner
                .state
                .lock()
                .pending
                .get(&request_id)
                .is_some_and(|pending| pending.auto_resolution.is_none())
        );
        coordinator.auto_resolve(&request_id);
        let response = timeout(Duration::from_secs(2), answer)
            .await
            .unwrap()
            .unwrap();
        assert!(
            response
                .answers
                .iter()
                .all(|answer| answer.values.is_empty())
        );
    }
}
