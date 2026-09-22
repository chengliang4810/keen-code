//! Host Core 注入的 ACP 业务路由和 WebSocket 投递边界。
//!
//! 这里仅保存连接级的短生命周期状态：连接上下文、delivery 游标和有界出站队列。
//! Session、Journal、Runtime 和 Snapshot 的事实仍由注入的 [`HostBusinessRouter`] 持有。

use keencode_acp::{AcpIncomingFrame, AcpRequestDecoder, ConnectionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::Notify;

use crate::{CapabilityPolicy, MobileCapability, ResourceId, ResourceRegistry};

/// 业务路由异步结果的统一类型，避免 transport crate 依赖 async-trait。
pub type HostBusinessFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HostBusinessError>> + Send + 'a>>;

/// Host 业务路由或连接边界错误。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum HostBusinessError {
    #[error("ACP 消息无效")]
    InvalidAcp,
    #[error("Host 连接不存在")]
    ConnectionNotFound,
    #[error("Host 连接已关闭")]
    ConnectionClosed,
    #[error("Host 业务路由失败")]
    Router,
    #[error("快照不可用")]
    SnapshotUnavailable,
    #[error("WebSocket ACP 方法不在移动端能力白名单中")]
    CapabilityDenied,
    #[error("Host 业务配置无效：{0}")]
    InvalidConfig(String),
}

/// 业务路由收到的非秘密连接上下文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostConnectionContext {
    /// transport 为本次连接分配的稳定连接标识。
    pub connection_id: ConnectionId,
    /// Web 登录会话标识；它与 Agent Session 使用不同命名空间，不参与事件路由。
    pub auth_session_id: String,
    /// 创建该连接时使用的 Web Token 版本。
    pub token_version: u64,
}

impl HostConnectionContext {
    /// 创建连接上下文；空 Session 或零 Token 版本不允许进入业务路由。
    pub fn new(
        connection_id: ConnectionId,
        auth_session_id: impl Into<String>,
        token_version: u64,
    ) -> Result<Self, HostBusinessError> {
        let auth_session_id = auth_session_id.into();
        if auth_session_id.is_empty() || auth_session_id.len() > 256 || token_version == 0 {
            return Err(HostBusinessError::InvalidConfig(
                "连接 Session 或 Token 版本无效".to_owned(),
            ));
        }
        Ok(Self {
            connection_id,
            auth_session_id,
            token_version,
        })
    }
}

/// 单个连接的 Journal 水位和 delivery 序号。
///
/// `journal_sequence` 来自 Host Core 的权威 Journal；`delivery_sequence` 由本 crate
/// 为每条连接递增。两者故意分开，断线恢复只能依赖 Journal 水位。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryCursor {
    /// Host Core 的持久 Journal 水位。
    pub journal_sequence: u64,
    /// 当前连接内单调递增的易失投递序号。
    pub delivery_sequence: u64,
}

impl DeliveryCursor {
    /// 创建一个连接投递游标。
    pub const fn new(journal_sequence: u64, delivery_sequence: u64) -> Self {
        Self {
            journal_sequence,
            delivery_sequence,
        }
    }
}

/// 快照完成后 Host Core 返回的恢复游标。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotCursor {
    /// 快照覆盖到的权威 Journal 水位。
    pub journal_sequence: u64,
    /// 快照之后下一条实时消息要使用的 delivery 序号。
    pub next_delivery_sequence: u64,
}

/// 触发快照/追赶的原因。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    /// 客户端主动带游标重连。
    Reconnect,
    /// 当前连接的出站队列已满，无法保证事件连续性。
    QueueOverflow,
    /// 业务层提供的 Journal 游标出现回退。
    JournalRegression,
}

/// 交给 Host Core 的快照请求；transport 不读取或缓存 Journal 内容。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    /// 需要恢复的连接。
    pub connection_id: ConnectionId,
    /// 客户端最后确认的游标；无游标表示完整快照。
    pub from: Option<DeliveryCursor>,
    /// 要求快照的原因。
    pub reason: SnapshotReason,
}

/// Host Core 生成的完整状态快照载荷。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotEnvelope {
    /// 快照覆盖到的恢复游标。
    pub cursor: SnapshotCursor,
    /// 已由 Host Core 编码的 ACP/业务 JSON 字节；transport 不解释正文。
    pub payload: Vec<u8>,
}

/// 业务事件发送到单个 WebSocket 连接时的 envelope。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundEvent {
    /// 当前连接级游标。
    pub cursor: DeliveryCursor,
    /// 已由 Host Core 编码的 JSON/ACP 字节。
    pub payload: Vec<u8>,
}

/// 连接出站发生丢序时发送给客户端的明确 gap 信号。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboundGap {
    /// 丢失区间的第一个游标。
    pub expected: DeliveryCursor,
    /// transport 已观察到的最新游标；其中间内容必须从 Snapshot/Journal 恢复。
    pub latest: DeliveryCursor,
    /// 恢复原因。
    pub reason: SnapshotReason,
}

/// WebSocket 出站队列中的三类控制项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboundEnvelope {
    /// 正常事件。
    Event(OutboundEvent),
    /// 丢序控制信号；客户端收到后必须请求 Snapshot/Journal catch-up。
    Gap(OutboundGap),
    /// Host Core 已完成的快照。
    Snapshot(SnapshotEnvelope),
}

/// 投递结果；出现 gap 时调用方不能把本次事件当作已经可靠送达。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    /// 事件进入有界队列。
    Enqueued(DeliveryCursor),
    /// 队列无法保持连续性，客户端必须走 Snapshot/Journal 恢复。
    Gap(OutboundGap),
}

struct QueueState {
    items: VecDeque<OutboundEnvelope>,
    pending_gap: Option<OutboundGap>,
    closed: bool,
}

/// 单连接有界出站队列。
///
/// 队列满时只保留一个 gap 控制项和有限的队列空间，不把慢客户端变成无界内存
/// 消耗。gap 会优先于后续事件返回，业务事件不会在 transport 内被重建或合并。
pub struct BoundedOutboundQueue {
    capacity: usize,
    state: Mutex<QueueState>,
    notify: Notify,
}

impl fmt::Debug for BoundedOutboundQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (length, gap, closed) = self
            .state
            .lock()
            .map(|state| (state.items.len(), state.pending_gap.is_some(), state.closed))
            .unwrap_or((0, false, true));
        formatter
            .debug_struct("BoundedOutboundQueue")
            .field("capacity", &self.capacity)
            .field("length", &length)
            .field("pending_gap", &gap)
            .field("closed", &closed)
            .finish()
    }
}

impl BoundedOutboundQueue {
    /// 创建正数容量的有界队列。
    pub fn new(capacity: usize) -> Result<Self, HostBusinessError> {
        if capacity == 0 {
            return Err(HostBusinessError::InvalidConfig(
                "出站队列容量必须大于 0".to_owned(),
            ));
        }
        Ok(Self {
            capacity,
            state: Mutex::new(QueueState {
                items: VecDeque::with_capacity(capacity),
                pending_gap: None,
                closed: false,
            }),
            notify: Notify::new(),
        })
    }

    /// 返回事件队列容量，不包含额外的单个 gap 控制项。
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    fn try_push(&self, item: OutboundEnvelope) -> Result<(), QueuePushError> {
        let mut state = self.state.lock().map_err(|_| QueuePushError::Closed)?;
        if state.closed {
            return Err(QueuePushError::Closed);
        }
        if state.pending_gap.is_some() || state.items.len() >= self.capacity {
            return Err(QueuePushError::Full);
        }
        state.items.push_back(item);
        drop(state);
        self.notify.notify_one();
        Ok(())
    }

    fn replace_with_gap(&self, gap: OutboundGap) -> Result<OutboundGap, HostBusinessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        if state.closed {
            return Err(HostBusinessError::ConnectionClosed);
        }
        let first_queued_cursor = state.items.front().and_then(|item| match item {
            OutboundEnvelope::Event(event) => Some(event.cursor),
            _ => None,
        });
        state.items.clear();
        let gap = match state.pending_gap.take() {
            Some(mut previous) => {
                previous.latest = gap.latest;
                previous
            }
            None => {
                first_queued_cursor.map_or(gap.clone(), |expected| OutboundGap { expected, ..gap })
            }
        };
        state.pending_gap = Some(gap.clone());
        drop(state);
        self.notify.notify_waiters();
        Ok(gap)
    }

    fn replace_with_snapshot(&self, snapshot: SnapshotEnvelope) -> Result<(), HostBusinessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        if state.closed {
            return Err(HostBusinessError::ConnectionClosed);
        }
        state.items.clear();
        state.pending_gap = None;
        state.items.push_back(OutboundEnvelope::Snapshot(snapshot));
        drop(state);
        self.notify.notify_one();
        Ok(())
    }

    /// Session 切换时丢弃旧会话的排队事件和 gap；连接本身保持打开。
    fn reset(&self) -> Result<(), HostBusinessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        if state.closed {
            return Err(HostBusinessError::ConnectionClosed);
        }
        state.items.clear();
        state.pending_gap = None;
        Ok(())
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            state.items.clear();
            state.pending_gap = None;
        }
        self.notify.notify_waiters();
    }

    /// 等待并取出下一项；连接关闭且队列排空后返回 `None`。
    pub async fn recv(&self) -> Option<OutboundEnvelope> {
        loop {
            let notified = self.notify.notified();
            if let Ok(mut state) = self.state.lock() {
                if let Some(gap) = state.pending_gap.take() {
                    return Some(OutboundEnvelope::Gap(gap));
                }
                if let Some(item) = state.items.pop_front() {
                    return Some(item);
                }
                if state.closed {
                    return None;
                }
            } else {
                return None;
            }
            notified.await;
        }
    }
}

enum QueuePushError {
    Full,
    Closed,
}

struct ConnectionState {
    context: HostConnectionContext,
    queue: Arc<BoundedOutboundQueue>,
    cursor: Mutex<ConnectionCursorState>,
    /// 最近一次成功 load/prompt/new 后绑定的 Agent Session；切换时原子替换旧值。
    active_session_id: Mutex<Option<String>>,
}

fn valid_session_id(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 256).then(|| value.to_owned())
}

/// WebSocket 只开放完整 Web 工作台当前调用的方法。
///
/// 这不是另一套 Session 协议：Desktop Web Host 仍把通过 allowlist 的 ACP
/// 请求交给同一进程内 Host Core。终端、项目写操作、后台任务与 MCP 管理
/// 等没有 Web capability 的入口继续保持拒绝。
fn method_capability(method: &str) -> Result<Option<MobileCapability>, HostBusinessError> {
    let capability = match method {
        "initialize" => return Ok(None),
        "session/list" | "session/load" => MobileCapability::SessionRead,
        "session/new"
        | "session/prompt"
        | "session/set_config_option"
        | "session/set_mode"
        | "keencode/session/rename"
        | "keencode/session/title"
        | "keencode/goal/get"
        | "keencode/goal/upsert"
        | "keencode/goal/transition"
        | "keencode/goal/clear" => MobileCapability::SessionWrite,
        "keencode/session/steer" => MobileCapability::Steer,
        "session/cancel" => MobileCapability::Stop,
        _ => return Err(HostBusinessError::CapabilityDenied),
    };
    Ok(Some(capability))
}

/// 只提取能够切换活动会话的请求目标；普通 Session 操作不能扩大事件订阅范围。
fn requested_binding_session_id(value: &Value) -> Option<String> {
    let method = value.get("method")?.as_str()?;
    if !matches!(method, "session/load" | "session/prompt") {
        return None;
    }
    value
        .get("params")
        .and_then(|params| params.get("sessionId"))
        .and_then(Value::as_str)
        .and_then(valid_session_id)
}

/// 把标准 ACP `ResourceLink` 绑定为现有 Host 附件文本契约。
///
/// Web 客户端只能提交当前认证会话签发的 `/api/resources/{id}` URI；资源索引
/// 返回的 canonical path 才能进入 Prompt，客户端声明的 name/mime/size 不参与
/// 授权，也不会形成第二套附件协议。
fn bind_prompt_resources(
    value: &mut Value,
    owner_session: &str,
    resources: Option<&ResourceRegistry>,
    capabilities: &CapabilityPolicy,
) -> Result<(), HostBusinessError> {
    let Some(blocks) = value
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .and_then(|params| params.get_mut("prompt"))
        .and_then(Value::as_array_mut)
    else {
        return Err(HostBusinessError::InvalidAcp);
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("resource_link") {
            continue;
        }
        capabilities
            .require(MobileCapability::AttachmentRead)
            .map_err(|_| HostBusinessError::CapabilityDenied)?;
        let uri = block
            .get("uri")
            .and_then(Value::as_str)
            .ok_or(HostBusinessError::CapabilityDenied)?;
        let resource_id = resource_id_from_uri(uri)?;
        let resource = resources
            .ok_or(HostBusinessError::CapabilityDenied)?
            .authorize(owner_session, resource_id)
            .map_err(|_| HostBusinessError::CapabilityDenied)?;
        let path = resource
            .path
            .to_str()
            .filter(|path| !path.chars().any(|ch| matches!(ch, '\r' | '\n')))
            .ok_or(HostBusinessError::CapabilityDenied)?;
        let directive = if resource
            .content_type
            .to_ascii_lowercase()
            .starts_with("image/")
        {
            format!("@image {path}")
        } else {
            format!("@{path}")
        };
        *block = serde_json::json!({"type": "text", "text": directive});
    }
    Ok(())
}

fn resource_id_from_uri(uri: &str) -> Result<&str, HostBusinessError> {
    let id = uri
        .strip_prefix("/api/resources/")
        .filter(|id| !id.is_empty() && !id.contains(['/', '?', '#', '%']))
        .ok_or(HostBusinessError::CapabilityDenied)?;
    ResourceId::try_from(id)
        .map(|_| id)
        .map_err(|_| HostBusinessError::CapabilityDenied)
}

/// 成功响应才能更新绑定；`session/new` 从结果取 ID，load/prompt 使用已校验请求目标。
fn successful_binding_session_id(
    payload: &[u8],
    requested_session_id: Option<String>,
) -> Option<String> {
    let value = serde_json::from_slice::<Value>(payload).ok()?;
    let result = value.get("result")?;
    result
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(valid_session_id)
        .or(requested_session_id)
}

fn bind_session_id(state: &ConnectionState, session_id: String) -> Result<(), HostBusinessError> {
    let mut active_session_id = state
        .active_session_id
        .lock()
        .map_err(|_| HostBusinessError::ConnectionClosed)?;
    if active_session_id.as_deref() == Some(session_id.as_str()) {
        return Ok(());
    }
    *active_session_id = Some(session_id);
    *state
        .cursor
        .lock()
        .map_err(|_| HostBusinessError::ConnectionClosed)? = ConnectionCursorState::default();
    state.queue.reset()?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ConnectionCursorState {
    next_delivery_sequence: u64,
    last_journal_sequence: u64,
    last_cursor: Option<DeliveryCursor>,
}

impl Default for ConnectionCursorState {
    fn default() -> Self {
        Self {
            next_delivery_sequence: 1,
            last_journal_sequence: 0,
            last_cursor: None,
        }
    }
}

/// 一个已经注册到 [`HostWsAdapter`] 的 WebSocket 连接。
pub struct HostWsConnection {
    state: Arc<ConnectionState>,
}

impl Clone for HostWsConnection {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl fmt::Debug for HostWsConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostWsConnection")
            .field("connection_id", &self.state.context.connection_id)
            .field("queue", &self.state.queue)
            .finish()
    }
}

impl HostWsConnection {
    /// 返回业务上下文的只读引用。
    pub fn context(&self) -> &HostConnectionContext {
        &self.state.context
    }

    /// 返回当前连接成功选择的 Agent Session；Web 登录会话 ID 永远不会从这里返回。
    pub fn active_session_id(&self) -> Option<String> {
        self.state
            .active_session_id
            .lock()
            .ok()
            .and_then(|session_id| session_id.clone())
    }

    /// 返回当前已分配的最后一个 delivery 游标。
    pub fn last_cursor(&self) -> Option<DeliveryCursor> {
        self.state
            .cursor
            .lock()
            .ok()
            .and_then(|state| state.last_cursor)
    }

    /// 等待下一个出站事件、gap 或 snapshot。
    pub async fn recv(&self) -> Option<OutboundEnvelope> {
        self.state.queue.recv().await
    }

    /// 关闭本连接的出站队列。
    pub fn close(&self) {
        self.state.queue.close();
    }
}

/// Host Core 到 Web transport 的业务路由注入点。
///
/// `dispatch` 收到已由 `keencode-acp` 严格解码的请求或通知，并返回已经由 Host
/// Core 编码的 JSON-RPC 响应。`snapshot` 是丢序/重连的恢复契约；默认返回不可用，
/// 这样没有持久 Journal 的 adapter 不会错误地伪造恢复结果。
pub trait HostBusinessRouter: Send + Sync + 'static {
    /// 把一个已验证的 ACP 请求交给 Host Core。
    fn dispatch<'a>(
        &'a self,
        context: HostConnectionContext,
        frame: AcpIncomingFrame,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>>;

    /// 把已经通过 JSON-RPC response 外层校验的 Client Response 交给 Host Core。
    ///
    /// Client Response 没有 `method`，不能伪装成 [`AcpIncomingFrame`] 请求解码；
    /// 默认实现保持 fail-closed，只有真正支持 Elicitation/Client Request 的
    /// 宿主 adapter 才显式接入该路径。
    fn dispatch_client_response<'a>(
        &'a self,
        _context: HostConnectionContext,
        _response: Value,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { Err(HostBusinessError::InvalidAcp) })
    }

    /// 通知业务 Host 连接已经关闭，以便清理连接级握手和挂起请求账本。
    fn disconnect(&self, _context: HostConnectionContext) {}

    /// 根据权威 Journal/Snapshot 生成恢复载荷。
    fn snapshot<'a>(
        &'a self,
        _context: HostConnectionContext,
        _request: SnapshotRequest,
    ) -> HostBusinessFuture<'a, SnapshotEnvelope> {
        Box::pin(async { Err(HostBusinessError::SnapshotUnavailable) })
    }
}

/// 业务 WebSocket adapter；只拥有连接级队列和游标，不拥有 Runtime 事实。
pub struct HostWsAdapter {
    router: Arc<dyn HostBusinessRouter>,
    capabilities: CapabilityPolicy,
    queue_capacity: usize,
    /// Web 认证会话拥有的资源索引；没有注入时仍支持无附件的测试/headless adapter。
    resources: Option<Arc<ResourceRegistry>>,
    connections: Mutex<std::collections::HashMap<ConnectionId, Arc<ConnectionState>>>,
}

impl fmt::Debug for HostWsAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let connection_count = self
            .connections
            .lock()
            .map(|value| value.len())
            .unwrap_or(0);
        formatter
            .debug_struct("HostWsAdapter")
            .field("queue_capacity", &self.queue_capacity)
            .field("connection_count", &connection_count)
            .finish()
    }
}

impl HostWsAdapter {
    /// 创建 ACP WebSocket adapter。
    pub fn new(
        router: Arc<dyn HostBusinessRouter>,
        queue_capacity: usize,
    ) -> Result<Self, HostBusinessError> {
        Self::new_inner(router, queue_capacity, None)
    }

    /// 创建带上传资源授权能力的 ACP WebSocket adapter。
    pub fn new_with_resources(
        router: Arc<dyn HostBusinessRouter>,
        queue_capacity: usize,
        resources: Arc<ResourceRegistry>,
    ) -> Result<Self, HostBusinessError> {
        Self::new_inner(router, queue_capacity, Some(resources))
    }

    fn new_inner(
        router: Arc<dyn HostBusinessRouter>,
        queue_capacity: usize,
        resources: Option<Arc<ResourceRegistry>>,
    ) -> Result<Self, HostBusinessError> {
        if queue_capacity == 0 {
            return Err(HostBusinessError::InvalidConfig(
                "出站队列容量必须大于 0".to_owned(),
            ));
        }
        Ok(Self {
            router,
            capabilities: CapabilityPolicy::for_mobile(),
            queue_capacity,
            resources,
            connections: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 从具体业务路由创建 adapter，便于 Desktop/headless 直接注入。
    pub fn from_router<R>(router: Arc<R>, queue_capacity: usize) -> Result<Self, HostBusinessError>
    where
        R: HostBusinessRouter,
    {
        Self::new(router, queue_capacity)
    }

    /// 从具体业务路由创建带资源授权的 adapter。
    pub fn from_router_with_resources<R>(
        router: Arc<R>,
        queue_capacity: usize,
        resources: Arc<ResourceRegistry>,
    ) -> Result<Self, HostBusinessError>
    where
        R: HostBusinessRouter,
    {
        Self::new_with_resources(router, queue_capacity, resources)
    }

    /// 注册一条 WebSocket 连接；同一 ConnectionId 不可复用。
    pub fn connect(
        &self,
        context: HostConnectionContext,
    ) -> Result<HostWsConnection, HostBusinessError> {
        self.connect_inner(context)
            .map(|(connection, _)| connection)
    }

    /// 注册带恢复游标的重连，并返回必须交给 Host Core 的 Snapshot 请求。
    ///
    /// transport 不判断 Journal 是否连续；Host Core 必须使用 `request_snapshot`
    /// 读取权威快照/Journal，再通过 `publish_snapshot` 清除 gap。
    pub fn reconnect(
        &self,
        context: HostConnectionContext,
        from: Option<DeliveryCursor>,
    ) -> Result<(HostWsConnection, SnapshotRequest), HostBusinessError> {
        self.connect_inner(context)
            .map(|(connection, request)| (connection, request.with_from(from)))
    }

    fn connect_inner(
        &self,
        context: HostConnectionContext,
    ) -> Result<(HostWsConnection, SnapshotRequest), HostBusinessError> {
        let queue = Arc::new(BoundedOutboundQueue::new(self.queue_capacity)?);
        let state = Arc::new(ConnectionState {
            context: context.clone(),
            queue,
            cursor: Mutex::new(ConnectionCursorState {
                next_delivery_sequence: 1,
                last_journal_sequence: 0,
                last_cursor: None,
            }),
            active_session_id: Mutex::new(None),
        });
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        if connections.contains_key(&context.connection_id) {
            return Err(HostBusinessError::InvalidConfig(
                "ConnectionId 已被占用".to_owned(),
            ));
        }
        connections.insert(context.connection_id.clone(), Arc::clone(&state));
        Ok((
            HostWsConnection { state },
            SnapshotRequest {
                connection_id: context.connection_id,
                from: None,
                reason: SnapshotReason::Reconnect,
            },
        ))
    }

    /// 注销连接并唤醒等待中的 writer。
    pub fn disconnect(&self, connection_id: &ConnectionId) {
        if let Ok(mut connections) = self.connections.lock() {
            if let Some(state) = connections.remove(connection_id) {
                self.router.disconnect(state.context.clone());
                state.queue.close();
            }
        }
    }

    /// 关闭当前 WebSocket 连接，并通知业务 Host 清理连接级状态。
    ///
    /// Server owner 停止时必须先调用该入口，避免 headless/Desktop Host 保留
    /// 事件订阅、挂起请求或连接级 Session 绑定。先复制 ID 再逐条断开，避免
    /// 在持有连接表锁时调用业务路由。
    pub fn disconnect_all(&self) {
        let connection_ids = self
            .connections
            .lock()
            .map(|connections| connections.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for connection_id in connection_ids {
            self.disconnect(&connection_id);
        }
    }

    /// 在授权完成后显式绑定连接的 Agent Session。
    ///
    /// 首次 `session/prompt` 的响应要等根 Turn 结束后才能返回；如果只在响应
    /// 成功后绑定，执行期间的增量事件会被 `publish_for_session` 正确过滤掉。
    /// 该入口只由已完成授权的 Host Core 调用，不能由 Web transport 根据原始请求
    /// 自行绑定。
    pub fn bind_session(
        &self,
        connection_id: &ConnectionId,
        session_id: &str,
    ) -> Result<(), HostBusinessError> {
        let session_id = valid_session_id(session_id)
            .ok_or_else(|| HostBusinessError::InvalidConfig("Session 标识无效".to_owned()))?;
        let state = self.connection(connection_id)?;
        bind_session_id(&state, session_id)
    }

    fn connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Arc<ConnectionState>, HostBusinessError> {
        self.connections
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .get(connection_id)
            .cloned()
            .ok_or(HostBusinessError::ConnectionNotFound)
    }

    /// 解码并把原始 ACP JSON 交给注入的 Host Core。
    pub async fn dispatch_acp(
        &self,
        connection_id: &ConnectionId,
        raw: &[u8],
    ) -> Result<Option<Vec<u8>>, HostBusinessError> {
        let state = self.connection(connection_id)?;
        // 先从原始 JSON 读取可能切换绑定的目标 Session，再交给严格 decoder；
        // 只有业务成功响应才会提交绑定，失败和通知都不能扩大事件订阅范围。
        let mut raw_value =
            serde_json::from_slice::<Value>(raw).map_err(|_| HostBusinessError::InvalidAcp)?;
        if is_client_response(&raw_value) {
            let response = self
                .router
                .dispatch_client_response(state.context.clone(), raw_value)
                .await?;
            // Client Response 本身不应产生第二个 JSON-RPC response；宿主实现若
            // 需要发送事件，应走 publish，而不是把响应伪装成请求结果。
            if response.is_some() {
                return Err(HostBusinessError::InvalidAcp);
            }
            return Ok(None);
        }
        let method = raw_value
            .get("method")
            .and_then(Value::as_str)
            .ok_or(HostBusinessError::InvalidAcp)?;
        if let Some(capability) = method_capability(method)? {
            self.capabilities
                .require(capability)
                .map_err(|_| HostBusinessError::CapabilityDenied)?;
        }
        let requested_session_id = requested_binding_session_id(&raw_value);
        // 先按原始 ACP 形状严格解码，避免资源重写意外放宽 ResourceLink 的字段校验；
        // 之后仅对 Prompt 中当前 Web 会话拥有的资源做本地附件绑定。
        let decoder = AcpRequestDecoder::new();
        let _validated = decoder
            .decode_raw(raw)
            .map_err(|_| HostBusinessError::InvalidAcp)?;
        if method == "session/prompt" {
            bind_prompt_resources(
                &mut raw_value,
                &state.context.auth_session_id,
                self.resources.as_deref(),
                &self.capabilities,
            )?;
        }
        let rewritten_raw =
            serde_json::to_vec(&raw_value).map_err(|_| HostBusinessError::InvalidAcp)?;
        let frame = decoder
            .decode_raw(&rewritten_raw)
            .map_err(|_| HostBusinessError::InvalidAcp)?;
        let response = self.router.dispatch(state.context.clone(), frame).await?;
        if let Some(session_id) = response
            .as_deref()
            .and_then(|payload| successful_binding_session_id(payload, requested_session_id))
        {
            bind_session_id(&state, session_id)?;
        }
        Ok(response)
    }

    /// 只把已由该连接成功发起或创建的 ACP Session 绑定到连接广播范围。
    fn is_session_bound(state: &ConnectionState, session_id: &str) -> bool {
        state
            .active_session_id
            .lock()
            .map(|active| active.as_deref() == Some(session_id))
            .unwrap_or(false)
    }

    /// 向一条连接发布由 Host Core 编码的事件。
    pub fn publish(
        &self,
        connection_id: &ConnectionId,
        journal_sequence: u64,
        payload: Vec<u8>,
    ) -> Result<DeliveryOutcome, HostBusinessError> {
        let state = self.connection(connection_id)?;
        let mut cursor = state
            .cursor
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        let delivery_sequence = cursor.next_delivery_sequence;
        cursor.next_delivery_sequence = cursor.next_delivery_sequence.saturating_add(1);
        let current = DeliveryCursor::new(journal_sequence, delivery_sequence);
        if journal_sequence < cursor.last_journal_sequence {
            let gap = OutboundGap {
                expected: cursor.last_cursor.unwrap_or(current),
                latest: current,
                reason: SnapshotReason::JournalRegression,
            };
            cursor.last_journal_sequence = journal_sequence;
            cursor.last_cursor = Some(current);
            drop(cursor);
            let gap = state.queue.replace_with_gap(gap)?;
            return Ok(DeliveryOutcome::Gap(gap));
        }
        cursor.last_journal_sequence = journal_sequence;
        cursor.last_cursor = Some(current);
        drop(cursor);

        match state.queue.try_push(OutboundEnvelope::Event(OutboundEvent {
            cursor: current,
            payload,
        })) {
            Ok(()) => Ok(DeliveryOutcome::Enqueued(current)),
            Err(QueuePushError::Full) => {
                let gap = state.queue.replace_with_gap(OutboundGap {
                    expected: current,
                    latest: current,
                    reason: SnapshotReason::QueueOverflow,
                })?;
                Ok(DeliveryOutcome::Gap(gap))
            }
            Err(QueuePushError::Closed) => Err(HostBusinessError::ConnectionClosed),
        }
    }

    /// 向指定 Session 的全部当前连接广播一条 Web 事件。
    ///
    /// 临时事件没有 Journal 序号时沿用该连接最近的权威水位，避免把正常的
    /// 非 Journal 事件误判为 Journal 回退；每条连接仍独立维护 delivery 游标。
    pub fn publish_for_session(
        &self,
        session_id: &str,
        journal_sequence: Option<u64>,
        payload: Vec<u8>,
    ) -> Result<usize, HostBusinessError> {
        if session_id.is_empty() {
            return Err(HostBusinessError::InvalidConfig(
                "Session 标识不能为空".to_owned(),
            ));
        }
        let connection_ids = self
            .connections
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .values()
            .filter(|state| Self::is_session_bound(state, session_id))
            .map(|state| state.context.connection_id.clone())
            .collect::<Vec<_>>();
        let mut published = 0;
        for connection_id in connection_ids {
            let sequence = if let Some(sequence) = journal_sequence {
                sequence
            } else {
                self.connection(&connection_id)?
                    .cursor
                    .lock()
                    .map_err(|_| HostBusinessError::ConnectionClosed)?
                    .last_journal_sequence
            };
            match self.publish(&connection_id, sequence, payload.clone()) {
                Ok(_) => published += 1,
                Err(
                    HostBusinessError::ConnectionNotFound | HostBusinessError::ConnectionClosed,
                ) => {
                    // 连接可能在快照列表和广播之间关闭；其生命周期不应影响 Runtime。
                }
                Err(error) => return Err(error),
            }
        }
        Ok(published)
    }

    /// 向一条连接投递已完成的 Snapshot，并清除此前的 gap。
    pub fn publish_snapshot(
        &self,
        connection_id: &ConnectionId,
        snapshot: SnapshotEnvelope,
    ) -> Result<DeliveryOutcome, HostBusinessError> {
        if snapshot.cursor.next_delivery_sequence == 0 {
            return Err(HostBusinessError::InvalidConfig(
                "Snapshot 下一条 delivery 序号必须大于 0".to_owned(),
            ));
        }
        let state = self.connection(connection_id)?;
        let mut cursor = state
            .cursor
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?;
        cursor.next_delivery_sequence = snapshot.cursor.next_delivery_sequence;
        cursor.last_journal_sequence = snapshot.cursor.journal_sequence;
        cursor.last_cursor = snapshot
            .cursor
            .next_delivery_sequence
            .checked_sub(1)
            .filter(|delivery| *delivery > 0)
            .map(|delivery| DeliveryCursor::new(snapshot.cursor.journal_sequence, delivery));
        drop(cursor);
        let cursor = DeliveryCursor::new(
            snapshot.cursor.journal_sequence,
            snapshot.cursor.next_delivery_sequence.saturating_sub(1),
        );
        state.queue.replace_with_snapshot(snapshot)?;
        Ok(DeliveryOutcome::Enqueued(cursor))
    }

    /// 向 Host Core 请求恢复快照；返回值不会被 transport 缓存。
    pub async fn request_snapshot(
        &self,
        connection_id: &ConnectionId,
        from: Option<DeliveryCursor>,
        reason: SnapshotReason,
    ) -> Result<SnapshotEnvelope, HostBusinessError> {
        let state = self.connection(connection_id)?;
        self.router
            .snapshot(
                state.context.clone(),
                SnapshotRequest {
                    connection_id: connection_id.clone(),
                    from,
                    reason,
                },
            )
            .await
    }

    /// 当前注册连接数；不代表 Runtime Session 数量。
    pub fn connection_count(&self) -> usize {
        self.connections
            .lock()
            .map(|value| value.len())
            .unwrap_or(0)
    }
}

/// 严格识别 Client Response 外层；完整 Elicitation result/error 仍由 Host Core
/// 按对应 Client Request DTO 再次校验，未知 request id 不得产生业务副作用。
fn is_client_response(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_str).is_none()
        || object.contains_key("method")
        || object.contains_key("params")
    {
        return false;
    }
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return false;
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "result" | "error"))
    {
        return false;
    }
    if let Some(error) = object.get("error") {
        error.is_object()
    } else {
        true
    }
}

impl SnapshotRequest {
    fn with_from(mut self, from: Option<DeliveryCursor>) -> Self {
        self.from = from;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRouter;

    impl HostBusinessRouter for TestRouter {
        fn dispatch<'a>(
            &'a self,
            _context: HostConnectionContext,
            _frame: AcpIncomingFrame,
        ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
    }

    struct ResponseRouter {
        responses: Mutex<VecDeque<Option<Vec<u8>>>>,
    }

    impl ResponseRouter {
        fn new(responses: impl IntoIterator<Item = &'static [u8]>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|response| Some(response.to_vec()))
                        .collect(),
                ),
            }
        }
    }

    impl HostBusinessRouter for ResponseRouter {
        fn dispatch<'a>(
            &'a self,
            _context: HostConnectionContext,
            _frame: AcpIncomingFrame,
        ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
            Box::pin(async move {
                self.responses
                    .lock()
                    .map_err(|_| HostBusinessError::Router)?
                    .pop_front()
                    .ok_or(HostBusinessError::Router)
            })
        }
    }

    struct ClientResponseRouter {
        received: Mutex<Vec<Value>>,
    }

    impl HostBusinessRouter for ClientResponseRouter {
        fn dispatch<'a>(
            &'a self,
            _context: HostConnectionContext,
            _frame: AcpIncomingFrame,
        ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Err(HostBusinessError::Router) })
        }

        fn dispatch_client_response<'a>(
            &'a self,
            _context: HostConnectionContext,
            response: Value,
        ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
            Box::pin(async move {
                self.received
                    .lock()
                    .map_err(|_| HostBusinessError::Router)?
                    .push(response);
                Ok(None)
            })
        }
    }

    struct CaptureRouter {
        received: Mutex<Vec<Value>>,
    }

    impl HostBusinessRouter for CaptureRouter {
        fn dispatch<'a>(
            &'a self,
            _context: HostConnectionContext,
            frame: AcpIncomingFrame,
        ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
            Box::pin(async move {
                let value = frame
                    .into_json_rpc_value()
                    .map_err(|_| HostBusinessError::InvalidAcp)?;
                self.received
                    .lock()
                    .map_err(|_| HostBusinessError::Router)?
                    .push(value);
                Ok(Some(
                    br#"{"jsonrpc":"2.0","id":"prompt-1","result":{"stopReason":"end_turn"}}"#
                        .to_vec(),
                ))
            })
        }
    }

    fn context(id: &str) -> HostConnectionContext {
        HostConnectionContext::new(ConnectionId::new(id).unwrap(), "session-1", 1).unwrap()
    }

    #[test]
    fn complete_web_workspace_method_allowlist_uses_narrow_capabilities() {
        for method in ["session/list", "session/load"] {
            assert_eq!(
                method_capability(method).unwrap(),
                Some(MobileCapability::SessionRead),
                "{method} 应只读取 Session",
            );
        }
        for method in [
            "session/new",
            "session/prompt",
            "session/set_config_option",
            "session/set_mode",
            "keencode/session/rename",
            "keencode/session/title",
            "keencode/goal/get",
            "keencode/goal/upsert",
            "keencode/goal/transition",
            "keencode/goal/clear",
        ] {
            assert_eq!(
                method_capability(method).unwrap(),
                Some(MobileCapability::SessionWrite),
                "{method} 应使用 SessionWrite capability",
            );
        }
        assert_eq!(
            method_capability("keencode/session/steer").unwrap(),
            Some(MobileCapability::Steer),
        );
        assert_eq!(
            method_capability("session/cancel").unwrap(),
            Some(MobileCapability::Stop),
        );
        assert_eq!(method_capability("initialize").unwrap(), None);
        assert_eq!(
            method_capability("session/delete"),
            Err(HostBusinessError::CapabilityDenied),
        );
        assert_eq!(
            method_capability("keencode/background/list"),
            Err(HostBusinessError::CapabilityDenied),
        );
    }

    #[tokio::test]
    async fn bounded_queue_full_produces_gap_and_does_not_grow() {
        let adapter = HostWsAdapter::from_router(Arc::new(TestRouter), 1).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        assert!(matches!(
            adapter.publish(&id, 1, b"one".to_vec()).unwrap(),
            DeliveryOutcome::Enqueued(_)
        ));
        let gap = adapter.publish(&id, 2, b"two".to_vec()).unwrap();
        assert!(matches!(
            gap,
            DeliveryOutcome::Gap(OutboundGap {
                expected: DeliveryCursor {
                    journal_sequence: 1,
                    delivery_sequence: 1,
                },
                latest: DeliveryCursor {
                    journal_sequence: 2,
                    delivery_sequence: 2,
                },
                reason: SnapshotReason::QueueOverflow,
            })
        ));
        assert!(matches!(
            connection.recv().await,
            Some(OutboundEnvelope::Gap(_))
        ));
        adapter.disconnect(&id);
        assert!(connection.recv().await.is_none());
    }

    #[tokio::test]
    async fn disconnect_closes_waiting_reader() {
        let adapter = HostWsAdapter::from_router(Arc::new(TestRouter), 2).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        adapter.disconnect(&id);
        assert!(connection.recv().await.is_none());
    }

    #[tokio::test]
    async fn publish_for_session_broadcasts_with_independent_delivery_cursors() {
        let adapter = HostWsAdapter::from_router(
            Arc::new(ResponseRouter::new([
                br#"{"jsonrpc":"2.0","id":"load-1","result":{}}"# as &[u8],
                br#"{"jsonrpc":"2.0","id":"load-2","result":{}}"# as &[u8],
                br#"{"jsonrpc":"2.0","id":"load-3","result":{}}"# as &[u8],
            ])),
            2,
        )
        .unwrap();
        let first = adapter.connect(context("c1")).unwrap();
        let second = adapter.connect(context("c2")).unwrap();
        let other = adapter
            .connect(
                HostConnectionContext::new(ConnectionId::new("c3").unwrap(), "session-2", 1)
                    .unwrap(),
            )
            .unwrap();
        for (index, connection, session_id) in [
            (1, &first, "session-1"),
            (2, &second, "session-1"),
            (3, &other, "session-2"),
        ] {
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":"load-{index}","method":"session/load","params":{{"sessionId":"{session_id}","cwd":"D:/fixture","mcpServers":[]}}}}"#,
            );
            adapter
                .dispatch_acp(&connection.context().connection_id, request.as_bytes())
                .await
                .unwrap();
        }

        assert_eq!(
            adapter
                .publish_for_session("session-1", Some(7), b"event".to_vec())
                .unwrap(),
            2
        );
        for connection in [first, second] {
            assert!(matches!(
                connection.recv().await,
                Some(OutboundEnvelope::Event(OutboundEvent {
                    cursor: DeliveryCursor {
                        journal_sequence: 7,
                        delivery_sequence: 1,
                    },
                    payload,
                })) if payload == b"event"
            ));
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), other.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn dispatch_rejects_invalid_acp_before_business_router() {
        let adapter = HostWsAdapter::from_router(Arc::new(TestRouter), 2).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        assert_eq!(
            adapter.dispatch_acp(&id, br#"{"jsonrpc":"2.0"}"#).await,
            Err(HostBusinessError::InvalidAcp)
        );
    }

    #[tokio::test]
    async fn dispatch_routes_client_response_without_method_to_host_core() {
        let router = Arc::new(ClientResponseRouter {
            received: Mutex::new(Vec::new()),
        });
        let adapter = HostWsAdapter::from_router(Arc::clone(&router), 2).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        assert_eq!(
            adapter
                .dispatch_acp(
                    &id,
                    br#"{"jsonrpc":"2.0","id":"elicitation-1","result":{"action":"cancel"}}"#,
                )
                .await,
            Ok(None)
        );
        let received = router.received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["id"], "elicitation-1");
        assert_eq!(received[0]["result"]["action"], "cancel");
    }

    #[tokio::test]
    async fn websocket_method_allowlist_rejects_unknown_and_desktop_only_requests() {
        let adapter = HostWsAdapter::from_router(
            Arc::new(ResponseRouter::new([
                br#"{"jsonrpc":"2.0","id":"init-1","result":{"protocolVersion":1}}"# as &[u8],
            ])),
            2,
        )
        .unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        for request in [
            br#"{"jsonrpc":"2.0","id":"mcp-1","method":"keencode/mcp/list","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"host-1","method":"keencode/web/start","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"delete-1","method":"session/delete","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"fork-1","method":"session/fork","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"replay-1","method":"keencode/session/replay","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"background-1","method":"keencode/background/list","params":{}}"#
                as &[u8],
            br#"{"jsonrpc":"2.0","id":"unknown-1","method":"unknown/action","params":{}}"#
                as &[u8],
        ] {
            assert_eq!(
                adapter.dispatch_acp(&id, request).await,
                Err(HostBusinessError::CapabilityDenied)
            );
        }
        let initialize = br#"{
            "jsonrpc":"2.0","id":"init-1","method":"initialize",
            "params":{"protocolVersion":1,"clientCapabilities":{}}
        }"#;
        assert!(
            adapter
                .dispatch_acp(&id, initialize)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn successful_load_binds_one_active_agent_session_and_switch_replaces_it() {
        let adapter = HostWsAdapter::from_router(
            Arc::new(ResponseRouter::new([
                br#"{"jsonrpc":"2.0","id":"load-1","result":{}}"# as &[u8],
                br#"{"jsonrpc":"2.0","id":"load-2","result":{}}"# as &[u8],
            ])),
            2,
        )
        .unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        let first_load = br#"{
            "jsonrpc":"2.0",
            "id":"load-1",
            "method":"session/load",
            "params":{"sessionId":"session-1","cwd":"D:/fixture","mcpServers":[]}
        }"#;

        assert!(
            adapter
                .dispatch_acp(&id, first_load)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(connection.active_session_id().as_deref(), Some("session-1"));
        assert_eq!(
            adapter
                .publish_for_session("session-1", Some(1), b"first".to_vec())
                .unwrap(),
            1
        );

        let second_load = br#"{
            "jsonrpc":"2.0",
            "id":"load-2",
            "method":"session/load",
            "params":{"sessionId":"session-2","cwd":"D:/fixture","mcpServers":[]}
        }"#;
        assert!(
            adapter
                .dispatch_acp(&id, second_load)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(connection.active_session_id().as_deref(), Some("session-2"));
        assert_eq!(
            adapter
                .publish_for_session("session-1", Some(2), b"stale".to_vec())
                .unwrap(),
            0
        );
        assert_eq!(
            adapter
                .publish_for_session("session-2", Some(1), b"second".to_vec())
                .unwrap(),
            1
        );
        assert!(matches!(
            connection.recv().await,
            Some(OutboundEnvelope::Event(OutboundEvent {
                cursor: DeliveryCursor { delivery_sequence: 1, .. },
                payload,
            })) if payload == b"second"
        ));
    }

    #[tokio::test]
    async fn failed_load_and_unrelated_session_method_do_not_bind() {
        let adapter = HostWsAdapter::from_router(
            Arc::new(ResponseRouter::new([
                br#"{"jsonrpc":"2.0","id":"load-1","error":{"code":-32602,"message":"invalid"}}"#
                    as &[u8],
                br#"{"jsonrpc":"2.0","id":"mode-1","result":{}}"# as &[u8],
            ])),
            2,
        )
        .unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        let failed_load = br#"{
            "jsonrpc":"2.0","id":"load-1","method":"session/load",
            "params":{"sessionId":"session-secret","cwd":"D:/fixture","mcpServers":[]}
        }"#;
        adapter.dispatch_acp(&id, failed_load).await.unwrap();
        assert_eq!(connection.active_session_id(), None);

        let unrelated = br#"{
            "jsonrpc":"2.0","id":"mode-1","method":"session/set_mode",
            "params":{"sessionId":"session-secret","modeId":"default"}
        }"#;
        adapter.dispatch_acp(&id, unrelated).await.unwrap();
        assert_eq!(connection.active_session_id(), None);
        assert_eq!(
            adapter
                .publish_for_session("session-secret", Some(1), b"private".to_vec())
                .unwrap(),
            0
        );
        assert_eq!(
            adapter
                .publish_for_session("session-1", Some(1), b"auth-id".to_vec())
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn successful_prompt_binds_and_disconnect_removes_the_route() {
        let adapter = HostWsAdapter::from_router(
            Arc::new(ResponseRouter::new([
                br#"{"jsonrpc":"2.0","id":"prompt-1","result":{"stopReason":"end_turn"}}"#
                    as &[u8],
            ])),
            2,
        )
        .unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        let prompt = br#"{
            "jsonrpc":"2.0","id":"prompt-1","method":"session/prompt",
            "params":{"sessionId":"session-prompt","prompt":[{"type":"text","text":"hello"}]}
        }"#;
        adapter.dispatch_acp(&id, prompt).await.unwrap();
        assert_eq!(
            connection.active_session_id().as_deref(),
            Some("session-prompt")
        );
        adapter.disconnect(&id);
        assert_eq!(
            adapter
                .publish_for_session("session-prompt", Some(1), b"late".to_vec())
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn explicit_prompt_binding_receives_events_before_response() {
        let adapter = HostWsAdapter::from_router(Arc::new(TestRouter), 2).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();

        adapter.bind_session(&id, "session-prompt").unwrap();
        assert_eq!(
            adapter
                .publish_for_session("session-prompt", Some(1), b"delta".to_vec())
                .unwrap(),
            1
        );
        match connection.recv().await {
            Some(OutboundEnvelope::Event(event)) => assert_eq!(event.payload, b"delta"),
            other => panic!("预绑定连接必须收到 Prompt 增量事件: {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_prompt_response_does_not_bind_session() {
        let adapter = HostWsAdapter::from_router(Arc::new(TestRouter), 2).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        let prompt = br#"{
            "jsonrpc":"2.0","id":"prompt-1","method":"session/prompt",
            "params":{"sessionId":"session-prompt","prompt":[{"type":"text","text":"hello"}]}
        }"#;

        assert!(adapter.dispatch_acp(&id, prompt).await.unwrap().is_none());
        assert_eq!(connection.active_session_id(), None);
        assert_eq!(
            adapter
                .publish_for_session("session-prompt", Some(1), b"late".to_vec())
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn prompt_resource_link_binds_owned_upload_to_existing_attachment_directive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("photo.png");
        std::fs::write(&path, b"image").unwrap();
        let resources = Arc::new(ResourceRegistry::new());
        let resource_id = resources
            .register("session-1", root.path(), &path, "image/png", "photo.png")
            .unwrap();
        let authorized_path = resources
            .authorize("session-1", resource_id.as_str())
            .unwrap()
            .path;
        let router = Arc::new(CaptureRouter {
            received: Mutex::new(Vec::new()),
        });
        let adapter =
            HostWsAdapter::from_router_with_resources(Arc::clone(&router), 2, resources).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "prompt-1",
            "method": "session/prompt",
            "params": {
                "sessionId": "session-prompt",
                "prompt": [
                    {"type": "text", "text": "请分析"},
                    {
                        "type": "resource_link",
                        "name": "photo.png",
                        "uri": format!("/api/resources/{}", resource_id.as_str()),
                        "mimeType": "image/png",
                        "size": 5
                    }
                ]
            }
        });
        assert!(
            adapter
                .dispatch_acp(&id, &serde_json::to_vec(&request).unwrap())
                .await
                .unwrap()
                .is_some()
        );
        let received = router.received.lock().unwrap();
        let prompt = received[0]
            .pointer("/params/prompt")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(prompt[0]["text"], "请分析");
        assert_eq!(prompt[1]["type"], "text");
        assert_eq!(
            prompt[1]["text"],
            format!("@image {}", authorized_path.display())
        );
    }

    #[tokio::test]
    async fn prompt_resource_link_rejects_external_and_cross_session_resources() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.txt");
        std::fs::write(&path, b"note").unwrap();
        let resources = Arc::new(ResourceRegistry::new());
        let resource_id = resources
            .register(
                "other-session",
                root.path(),
                &path,
                "text/plain",
                "note.txt",
            )
            .unwrap();
        let adapter =
            HostWsAdapter::new_with_resources(Arc::new(TestRouter), 2, resources).unwrap();
        let connection = adapter.connect(context("c1")).unwrap();
        let id = connection.context().connection_id.clone();
        for uri in [
            "file:///etc/passwd".to_owned(),
            format!("/api/resources/{}", resource_id.as_str()),
        ] {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": "prompt-1",
                "method": "session/prompt",
                "params": {
                    "sessionId": "session-prompt",
                    "prompt": [{
                        "type": "resource_link",
                        "name": "note.txt",
                        "uri": uri,
                        "mimeType": "text/plain",
                        "size": 4
                    }]
                }
            });
            assert_eq!(
                adapter
                    .dispatch_acp(&id, &serde_json::to_vec(&request).unwrap())
                    .await,
                Err(HostBusinessError::CapabilityDenied)
            );
        }
    }
}
