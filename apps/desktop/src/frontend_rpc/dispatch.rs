//! 连接级 RPC 网关与 Tauri 命令。
//!
//! 该模块只负责 ZCode Channel RPC 的边界语义：连接身份、请求顺序、取消、
//! 事件订阅及响应编码。具体的 session/local service handler 通过 [`Handler`]
//! 注入，避免网关复制一份 Runtime 或 Journal 状态。

use super::codec::{self, CodecError, RpcValue};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tauri::{
    AppHandle, Manager, State, Webview,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request},
};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    task::AbortHandle,
};

pub const REQUEST_PROMISE: u32 = 100;
pub const REQUEST_PROMISE_CANCEL: u32 = 101;
pub const REQUEST_EVENT_LISTEN: u32 = 102;
pub const REQUEST_EVENT_DISPOSE: u32 = 103;

pub const RESPONSE_INITIALIZE: u32 = 200;
pub const RESPONSE_PROMISE_SUCCESS: u32 = 201;
pub const RESPONSE_PROMISE_ERROR: u32 = 202;
#[allow(dead_code)]
pub const RESPONSE_PROMISE_ERROR_OBJ: u32 = 203;
pub const RESPONSE_EVENT_FIRE: u32 = 204;

const MAIN_WINDOW_LABEL: &str = "main";

/// 入站 RPC 的硬上限，避免异常页面把 Tauri command 变成无界内存入口。
pub const MAX_RPC_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PENDING_REQUESTS: usize = 256;
pub const MAX_SUBSCRIPTIONS: usize = 256;
/// 只保留最近关闭连接的窗口所有者，供迟到 close 做幂等与归属校验；
/// 这是有界生命周期 tombstone，不保存连接状态，也不能无限增长。
const MAX_CLOSED_CONNECTION_TOMBSTONES: usize = 2048;

pub type RpcFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send + 'static>>;
pub type EventCallback = Arc<dyn Fn(Value) -> Result<(), RpcError> + Send + Sync + 'static>;

/// 前端一次 Tauri 连接对应的上下文。
///
/// AppHandle 只作为受信桌面状态的入口；权威会话、Runtime 和 Journal 仍由
/// handler 通过 `ctx.app.state()` 获取，不能在前端 RPC 层复制保存。
#[derive(Clone)]
pub struct GatewayContext {
    #[allow(dead_code)]
    pub app: AppHandle,
    #[allow(dead_code)]
    pub connection_id: String,
    pub window_label: String,
    /// 必须使用 `InvokeResponseBody::Raw`，否则 `Channel<Vec<u8>>` 会命中
    /// Tauri 的通用 Serialize 实现并把 payload 变成 JSON 数组。
    pub emitter: Channel<InvokeResponseBody>,
    handler: Arc<dyn Handler>,
    connection: Arc<ConnectionState>,
}

impl GatewayContext {
    /// 阻塞系统能力返回后再次检查，防止关闭连接期间新建的子进程成为孤儿。
    pub(crate) fn is_closed(&self) -> bool {
        self.connection.closed.load(Ordering::Acquire)
    }
}

/// RPC 业务实现必须由 session/local service 层提供。
///
/// `call` 使用拥有所有权的 Context，使异步业务可以安全地跨越 Tauri command
/// 的生命周期；对外的 [`call`] 函数保持用户约定的 `&GatewayContext` 形态。
pub trait Handler: Send + Sync + 'static {
    fn call(&self, ctx: GatewayContext, channel: String, method: String, args: Value) -> RpcFuture;

    fn listen(
        &self,
        ctx: GatewayContext,
        channel: String,
        event: String,
        args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError>;

    /// 连接显式关闭、页面重载或窗口销毁后的业务清理钩子。
    ///
    /// 默认实现保持纯服务 Handler 可用；session handler 可在此释放连接级
    /// subscription、pending question 和 command 去重缓存。
    fn connection_closed(&self, _ctx: GatewayContext) {}

    /// 成功响应已经写入前端 Channel 后调用。
    ///
    /// Session 订阅使用这个确认点排放延迟的首帧；如果 Channel 写入失败，
    /// 不会调用该钩子，避免把未激活的订阅标记为已建立。`response` 保留
    /// 刚写出的成功 body，使订阅能按 ACK 中的 subscriptionId 精确关联。
    fn response_sent(
        &self,
        _ctx: GatewayContext,
        _channel: String,
        _method: String,
        _args: Value,
        _response: Value,
    ) {
    }
}

/// 由业务事件源返回的真实订阅。drop/dispose 会调用底层清理函数。
pub struct Subscription {
    disposer: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl Subscription {
    #[allow(dead_code)]
    pub fn new(disposer: impl FnOnce() + Send + 'static) -> Self {
        Self {
            disposer: Some(Box::new(disposer)),
        }
    }

    pub fn dispose(mut self) {
        if let Some(disposer) = self.disposer.take() {
            disposer();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(disposer) = self.disposer.take() {
            disposer();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    #[serde(flatten)]
    inner: Box<RpcErrorFields>,
}

#[derive(Debug, Clone, Serialize)]
struct RpcErrorFields {
    code: String,
    message: String,
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stack: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Box<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Box<Value>>,
    #[serde(rename = "retryAfterMs", skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<Box<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Box<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Box<Value>>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(rename = "traceId", skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            inner: Box::new(RpcErrorFields {
                code: code.into(),
                message: message.into(),
                name: "RpcError".to_string(),
                kind: "rpc".to_string(),
                stack: None,
                data: None,
                status: None,
                retry_after_ms: None,
                detail: None,
                details: None,
                task_id: None,
                trace_id: None,
            }),
        }
    }

    #[allow(dead_code)]
    pub fn with_data(mut self, data: Value) -> Self {
        self.inner.data = Some(Box::new(data));
        self
    }

    #[allow(dead_code)]
    pub fn code(&self) -> &str {
        &self.inner.code
    }

    #[allow(dead_code)]
    pub fn message(&self) -> &str {
        &self.inner.message
    }

    fn response_value(&self) -> RpcValue {
        let mut fields = vec![
            (
                "message".to_string(),
                RpcValue::String(self.inner.message.clone()),
            ),
            (
                "name".to_string(),
                RpcValue::String(self.inner.name.clone()),
            ),
            (
                "code".to_string(),
                RpcValue::String(self.inner.code.clone()),
            ),
            (
                "kind".to_string(),
                RpcValue::String(self.inner.kind.clone()),
            ),
        ];
        if let Some(stack) = &self.inner.stack {
            fields.push((
                "stack".to_string(),
                RpcValue::Array(stack.iter().cloned().map(RpcValue::String).collect()),
            ));
        }
        if let Some(data) = &self.inner.data {
            fields.push(("data".to_string(), RpcValue::from_json(data.as_ref())));
        }
        if let Some(status) = &self.inner.status {
            fields.push(("status".to_string(), RpcValue::from_json(status.as_ref())));
        }
        if let Some(retry_after_ms) = &self.inner.retry_after_ms {
            fields.push((
                "retryAfterMs".to_string(),
                RpcValue::from_json(retry_after_ms.as_ref()),
            ));
        }
        if let Some(detail) = &self.inner.detail {
            fields.push(("detail".to_string(), RpcValue::from_json(detail.as_ref())));
        }
        if let Some(details) = &self.inner.details {
            fields.push(("details".to_string(), RpcValue::from_json(details.as_ref())));
        }
        if let Some(task_id) = &self.inner.task_id {
            fields.push(("taskId".to_string(), RpcValue::String(task_id.clone())));
        }
        if let Some(trace_id) = &self.inner.trace_id {
            fields.push(("traceId".to_string(), RpcValue::String(trace_id.clone())));
        }
        RpcValue::Object(fields)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.inner.code, self.inner.message)
    }
}

impl std::error::Error for RpcError {}

impl From<String> for RpcError {
    fn from(message: String) -> Self {
        // 业务路由仍可用更具体的 code；该转换只兜底旧 Rust 入口的 String 错误，
        // 确保它们继续作为 PromiseError 返回，不会被误包装成成功值。
        Self::new("rpc.handlerError", message)
    }
}

struct ConnectionState {
    accepting: AsyncMutex<()>,
    pending: AsyncMutex<HashMap<u32, AbortHandle>>,
    subscriptions: AsyncMutex<HashMap<u32, Subscription>>,
    closed: AtomicBool,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            accepting: AsyncMutex::new(()),
            pending: AsyncMutex::new(HashMap::new()),
            subscriptions: AsyncMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }

    async fn close(&self) {
        // 与入站 dispatch 共用接受锁，避免 close 摘除路由后仍有已取到
        // Context 的请求越过 closed 标记进入业务 Handler。
        let _accept_guard = self.accepting.lock().await;
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for handle in pending.into_values() {
            handle.abort();
        }
        let subscriptions = std::mem::take(&mut *self.subscriptions.lock().await);
        for subscription in subscriptions.into_values() {
            subscription.dispose();
        }
    }
}

#[derive(Default)]
struct ClosedConnectionTombstones {
    owners: HashMap<String, String>,
    order: VecDeque<String>,
}

impl ClosedConnectionTombstones {
    fn remember(&mut self, connection_id: &str, window_label: &str) {
        if self.owners.contains_key(connection_id) {
            return;
        }
        while self.owners.len() >= MAX_CLOSED_CONNECTION_TOMBSTONES {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if self.owners.remove(&evicted).is_some() {
                break;
            }
        }
        self.order.push_back(connection_id.to_owned());
        self.owners
            .insert(connection_id.to_owned(), window_label.to_owned());
    }

    fn owner(&self, connection_id: &str) -> Option<&str> {
        self.owners.get(connection_id).map(String::as_str)
    }
}

/// 所有前端连接的共享网关状态。根装配层将一个实例注册为 Tauri state。
pub struct RpcGateway {
    connections: Mutex<HashMap<String, GatewayContext>>,
    closed_connections: Mutex<ClosedConnectionTombstones>,
    next_connection_id: AtomicU64,
    handler: Arc<dyn Handler>,
}

impl RpcGateway {
    #[allow(dead_code)]
    pub fn new(handler: Arc<dyn Handler>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            closed_connections: Mutex::new(ClosedConnectionTombstones::default()),
            next_connection_id: AtomicU64::new(1),
            handler,
        }
    }

    pub fn open(
        &self,
        app: AppHandle,
        window_label: impl Into<String>,
        emitter: Channel<InvokeResponseBody>,
    ) -> Result<String, RpcError> {
        let connection_id = format!(
            "rpc-{}",
            self.next_connection_id.fetch_add(1, Ordering::Relaxed)
        );
        let context = GatewayContext {
            app,
            connection_id: connection_id.clone(),
            window_label: window_label.into(),
            emitter,
            handler: Arc::clone(&self.handler),
            connection: Arc::new(ConnectionState::new()),
        };

        let initialize = initialize_payload()?;
        register_desktop_form_capability(&context.app, &connection_id)?;
        if let Err(error) = context.emitter.send(InvokeResponseBody::Raw(initialize)) {
            // Channel 在连接表登记前就已失败，必须撤销能力快照，避免同一 Runtime
            // 把已经不可达的 connection_id 当成仍可回答的前端连接。
            disconnect_desktop_capability(&context.app, &connection_id);
            return Err(RpcError::new("rpc.channelClosed", error.to_string()));
        }
        let mut connections = match self.connections.lock() {
            Ok(connections) => connections,
            Err(_) => {
                disconnect_desktop_capability(&context.app, &connection_id);
                return Err(RpcError::new(
                    "rpc.statePoisoned",
                    "RPC connection state is poisoned",
                ));
            }
        };
        connections.insert(connection_id.clone(), context);
        Ok(connection_id)
    }

    pub async fn send(
        &self,
        connection_id: &str,
        window_label: &str,
        raw: &[u8],
    ) -> Result<(), RpcError> {
        let context = self.context_for(connection_id, window_label)?;
        dispatch(&context, raw).await
    }

    pub async fn close(&self, connection_id: &str, window_label: &str) -> Result<(), RpcError> {
        let context = {
            let mut connections = self.connections.lock().map_err(|_| {
                RpcError::new("rpc.statePoisoned", "RPC connection state is poisoned")
            })?;
            let mut closed_connections = self.closed_connections.lock().map_err(|_| {
                RpcError::new(
                    "rpc.statePoisoned",
                    "RPC closed-connection state is poisoned",
                )
            })?;
            let Some(context) = connections.get(connection_id).cloned() else {
                return close_unknown_connection(&closed_connections, connection_id, window_label);
            };
            ensure_window(&context, window_label)?;
            let context = connections
                .remove(connection_id)
                .expect("connection was present while holding the gateway lock");
            // 路由移除与 owner tombstone 在同一组锁内完成，避免两个并发 close
            // 之间出现短暂的 unknownConnection 窗口。
            closed_connections.remember(&context.connection_id, &context.window_label);
            context
        };
        context.connection.close().await;
        context.handler.connection_closed(context.clone());
        Ok(())
    }

    /// 页面重载或窗口销毁时回收该窗口的所有 RPC 连接。
    ///
    /// 先从路由表移除连接，再异步取消 pending task 和真实订阅；这样新页面
    /// 不能继续使用旧 connection id，且生命周期回调本身无需阻塞 Tauri 事件线程。
    pub fn close_window(&self, window_label: &str) -> Result<usize, RpcError> {
        let contexts = {
            let mut connections = self.connections.lock().map_err(|_| {
                RpcError::new("rpc.statePoisoned", "RPC connection state is poisoned")
            })?;
            let mut closed_connections = self.closed_connections.lock().map_err(|_| {
                RpcError::new(
                    "rpc.statePoisoned",
                    "RPC closed-connection state is poisoned",
                )
            })?;
            let connection_ids: Vec<String> = connections
                .iter()
                .filter(|(_, context)| context.window_label == window_label)
                .map(|(connection_id, _)| connection_id.clone())
                .collect();
            connection_ids
                .into_iter()
                .filter_map(|connection_id| {
                    let context = connections.remove(&connection_id)?;
                    closed_connections.remember(&context.connection_id, &context.window_label);
                    Some(context)
                })
                .collect::<Vec<_>>()
        };
        let count = contexts.len();
        for context in contexts {
            let connection = Arc::clone(&context.connection);
            let handler = Arc::clone(&context.handler);
            // 页面重载/销毁事件本身是同步回调，关闭动作放入 Tauri runtime；
            // 但业务清理必须排在 transport close 之后，确保 pending handler
            // 已经被标记取消且订阅已摘除，避免旧请求在业务解绑后继续产生副作用。
            tauri::async_runtime::spawn(close_connection_then(connection, move || {
                handler.connection_closed(context)
            }));
        }
        Ok(count)
    }

    fn context_for(
        &self,
        connection_id: &str,
        window_label: &str,
    ) -> Result<GatewayContext, RpcError> {
        let connections = self
            .connections
            .lock()
            .map_err(|_| RpcError::new("rpc.statePoisoned", "RPC connection state is poisoned"))?;
        let context = connections.get(connection_id).ok_or_else(|| {
            RpcError::new("rpc.unknownConnection", "RPC connection does not exist")
        })?;
        ensure_window(context, window_label)?;
        if context.connection.closed.load(Ordering::Acquire) {
            return Err(RpcError::new(
                "rpc.connectionClosed",
                "RPC connection is closed",
            ));
        }
        Ok(context.clone())
    }
}

/// 关闭连接并在 transport 状态稳定后运行业务侧生命周期清理。
///
/// 显式 close 路径可以直接 await；窗口销毁/page reload 只能把这个 future
/// 交给 Tauri runtime，因此两条生命周期入口共用同一顺序，避免回调顺序分叉。
async fn close_connection_then<F>(connection: Arc<ConnectionState>, cleanup: F)
where
    F: FnOnce() + Send + 'static,
{
    connection.close().await;
    cleanup();
}

/// 业务调用的共享入口。
pub async fn call(
    ctx: &GatewayContext,
    channel: &str,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    ctx.handler
        .call(ctx.clone(), channel.to_string(), method.to_string(), args)
        .await
}

/// 真实事件订阅的共享入口。
pub fn listen(
    ctx: &GatewayContext,
    channel: &str,
    event: &str,
    args: Value,
    callback: EventCallback,
) -> Result<Subscription, RpcError> {
    ctx.handler.listen(
        ctx.clone(),
        channel.to_string(),
        event.to_string(),
        args,
        callback,
    )
}

/// 按到达顺序接受一个 RPC payload。
pub async fn dispatch(ctx: &GatewayContext, raw: &[u8]) -> Result<(), RpcError> {
    let _accept_guard = ctx.connection.accepting.lock().await;
    if ctx.connection.closed.load(Ordering::Acquire) {
        return Err(RpcError::new(
            "rpc.connectionClosed",
            "RPC connection is closed",
        ));
    }
    if raw.len() > MAX_RPC_PAYLOAD_BYTES {
        return Err(RpcError::new(
            "rpc.payloadTooLarge",
            "RPC payload exceeds the connection limit",
        ));
    }

    let (header, body) = codec::decode_message(raw).map_err(codec_error)?;
    let request = parse_request(header, body)?;
    match request.kind {
        REQUEST_PROMISE => dispatch_promise(ctx, request).await,
        REQUEST_PROMISE_CANCEL => {
            if let Some(handle) = ctx.connection.pending.lock().await.remove(&request.id) {
                handle.abort();
            }
            Ok(())
        }
        REQUEST_EVENT_LISTEN => dispatch_event_listen(ctx, request).await,
        REQUEST_EVENT_DISPOSE => {
            if let Some(subscription) = ctx
                .connection
                .subscriptions
                .lock()
                .await
                .remove(&request.id)
            {
                subscription.dispose();
            }
            Ok(())
        }
        _ => Err(RpcError::new(
            "rpc.unknownRequestType",
            "RPC request type is not supported",
        )),
    }
}

struct RequestMessage {
    kind: u32,
    id: u32,
    channel: String,
    method: String,
    args: Value,
}

fn parse_request(header: RpcValue, body: RpcValue) -> Result<RequestMessage, RpcError> {
    let values = match header {
        RpcValue::Array(values) => values,
        _ => {
            return Err(RpcError::new(
                "rpc.invalidHeader",
                "RPC header must be an array",
            ));
        }
    };
    let kind = values
        .first()
        .and_then(RpcValue::as_u32)
        .ok_or_else(|| RpcError::new("rpc.invalidHeader", "RPC header has no request type"))?;
    let id = values.get(1).and_then(RpcValue::as_u32).unwrap_or_default();
    let (channel, method) = if matches!(kind, REQUEST_PROMISE | REQUEST_EVENT_LISTEN) {
        let channel = values
            .get(2)
            .and_then(RpcValue::as_str)
            .ok_or_else(|| RpcError::new("rpc.invalidHeader", "RPC header has no channel"))?;
        let method = values
            .get(3)
            .and_then(RpcValue::as_str)
            .ok_or_else(|| RpcError::new("rpc.invalidHeader", "RPC header has no method"))?;
        (channel.to_string(), method.to_string())
    } else {
        (String::new(), String::new())
    };
    Ok(RequestMessage {
        kind,
        id,
        channel,
        method,
        args: body.to_json(),
    })
}

async fn dispatch_promise(ctx: &GatewayContext, request: RequestMessage) -> Result<(), RpcError> {
    let mut pending = ctx.connection.pending.lock().await;
    if pending.contains_key(&request.id) {
        let error = RpcError::new("rpc.duplicateRequest", "RPC request id is already active");
        log_rpc_error(&request.channel, &request.method, &error);
        send_error(ctx, request.id, error)?;
        return Ok(());
    }
    if pending.len() >= MAX_PENDING_REQUESTS {
        let error = RpcError::new("rpc.rateLimited", "Too many active RPC requests");
        log_rpc_error(&request.channel, &request.method, &error);
        send_error(ctx, request.id, error)?;
        return Ok(());
    }

    let (start_sender, start_receiver) = oneshot::channel();
    let task_context = ctx.clone();
    let connection = Arc::clone(&ctx.connection);
    let task = tokio::spawn(async move {
        let _ = start_receiver.await;
        let response_channel = request.channel.clone();
        let response_method = request.method.clone();
        let response_args = request.args.clone();
        let result = call(
            &task_context,
            &request.channel,
            &request.method,
            request.args,
        )
        .await;
        let response = match result {
            Ok(value) => {
                let response = send_response(
                    &task_context,
                    RESPONSE_PROMISE_SUCCESS,
                    request.id,
                    RpcValue::from_json(&value),
                );
                if response.is_ok() {
                    task_context.handler.response_sent(
                        task_context.clone(),
                        response_channel,
                        response_method,
                        response_args,
                        value,
                    );
                }
                response
            }
            Err(error) => {
                log_rpc_error(&response_channel, &response_method, &error);
                send_response(
                    &task_context,
                    RESPONSE_PROMISE_ERROR,
                    request.id,
                    error.response_value(),
                )
            }
        };
        let _ = response;
        connection.pending.lock().await.remove(&request.id);
    });
    pending.insert(request.id, task.abort_handle());
    let _ = start_sender.send(());
    Ok(())
}

async fn dispatch_event_listen(
    ctx: &GatewayContext,
    request: RequestMessage,
) -> Result<(), RpcError> {
    let event_context = ctx.clone();
    let callback: EventCallback = Arc::new(move |value| {
        send_response(
            &event_context,
            RESPONSE_EVENT_FIRE,
            request.id,
            RpcValue::from_json(&value),
        )
    });
    let subscription = match listen(
        ctx,
        &request.channel,
        &request.method,
        request.args,
        callback,
    ) {
        Ok(subscription) => subscription,
        Err(error) => {
            log_rpc_error(&request.channel, &request.method, &error);
            send_error(ctx, request.id, error)?;
            return Ok(());
        }
    };
    let mut subscriptions = ctx.connection.subscriptions.lock().await;
    if subscriptions.len() >= MAX_SUBSCRIPTIONS && !subscriptions.contains_key(&request.id) {
        subscription.dispose();
        send_error(
            ctx,
            request.id,
            RpcError::new("rpc.rateLimited", "Too many active RPC subscriptions"),
        )?;
        return Ok(());
    }
    if let Some(previous) = subscriptions.insert(request.id, subscription) {
        previous.dispose();
    }
    Ok(())
}

fn send_error(ctx: &GatewayContext, id: u32, error: RpcError) -> Result<(), RpcError> {
    send_response(ctx, RESPONSE_PROMISE_ERROR, id, error.response_value())
}

/// 记录前端 RPC 失败的最小诊断上下文；绝不写入 args、Provider 配置、Key 或 URL。
fn log_rpc_error(channel: &str, method: &str, error: &RpcError) {
    tracing::warn!(
        target: "keencode_diagnostics",
        channel,
        method,
        code = error.code(),
        message = sanitize_rpc_diagnostic_message(error.message()),
        "frontend RPC request failed"
    );
}

fn sanitize_rpc_diagnostic_message(message: &str) -> String {
    let redacted = keencode_model::redact_error_secrets(message);
    let mut sanitized = redacted
        .split_whitespace()
        .map(|token| {
            if token.contains("://") || token.starts_with("http") || token.starts_with("ws") {
                "[URL]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if sanitized.len() > 512 {
        let mut end = 512;
        while !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized.truncate(end);
    }
    sanitized
}

fn send_response(
    ctx: &GatewayContext,
    response_type: u32,
    id: u32,
    body: RpcValue,
) -> Result<(), RpcError> {
    if ctx.connection.closed.load(Ordering::Acquire) {
        return Err(RpcError::new(
            "rpc.connectionClosed",
            "RPC connection is closed",
        ));
    }
    let header = if response_type == RESPONSE_INITIALIZE {
        RpcValue::Array(vec![RpcValue::Int(response_type as i64)])
    } else {
        RpcValue::Array(vec![
            RpcValue::Int(response_type as i64),
            RpcValue::Int(id as i64),
        ])
    };
    let payload = codec::encode_message(&header, &body).map_err(codec_error)?;
    ctx.emitter
        .send(InvokeResponseBody::Raw(payload))
        .map_err(|error| RpcError::new("rpc.channelClosed", error.to_string()))
}

fn codec_error(error: CodecError) -> RpcError {
    RpcError::new("rpc.invalidPayload", error.to_string())
}

/// 真实 Tauri Desktop 前端由 RPC 入口固定声明表单问答能力。
///
/// 该能力不是前端可伪造的权限，也不包含 URL 问答；它只让本地 WebView 能接收
/// Runtime 发出的标准 form Elicitation。
fn desktop_frontend_capabilities() -> keencode_acp::schema::ClientCapabilities {
    keencode_acp::schema::ClientCapabilities::new().elicitation(Some(
        keencode_acp::schema::ElicitationCapabilities::new().form(Some(
            keencode_acp::schema::ElicitationFormCapabilities::new(),
        )),
    ))
}

fn register_desktop_form_capability(app: &AppHandle, connection_id: &str) -> Result<(), RpcError> {
    let connection_id = keencode_acp::ConnectionId::new(connection_id.to_owned())
        .map_err(|error| RpcError::new("rpc.invalidConnection", error.to_string()))?;
    let runtime = app
        .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or_else(|| {
            RpcError::new(
                "rpc.runtimeUnavailable",
                "本地 Agent Runtime 不可用，不能建立前端 RPC 连接",
            )
        })?;
    runtime
        .elicitation_coordinator()
        .negotiate_connection_capabilities(&connection_id, &desktop_frontend_capabilities())
        .map_err(|error| RpcError::new("rpc.elicitationCapabilities", error.to_string()))
}

fn disconnect_desktop_capability(app: &AppHandle, connection_id: &str) {
    let Ok(connection_id) = keencode_acp::ConnectionId::new(connection_id.to_owned()) else {
        return;
    };
    if let Some(runtime) = app.try_state::<Arc<crate::agent_runtime::AgentRuntime>>() {
        runtime.elicitation_coordinator().disconnect(&connection_id);
    }
}

fn initialize_payload() -> Result<Vec<u8>, RpcError> {
    codec::encode_message(
        &RpcValue::Array(vec![RpcValue::Int(RESPONSE_INITIALIZE as i64)]),
        &RpcValue::Undefined,
    )
    .map_err(codec_error)
}

fn ensure_window(ctx: &GatewayContext, window_label: &str) -> Result<(), RpcError> {
    ensure_window_label(&ctx.window_label, window_label)
}

fn ensure_window_label(expected: &str, actual: &str) -> Result<(), RpcError> {
    if expected == actual {
        return Ok(());
    }
    Err(RpcError::new(
        "rpc.windowMismatch",
        "RPC connection belongs to a different window",
    ))
}

fn close_unknown_connection(
    closed_connections: &ClosedConnectionTombstones,
    connection_id: &str,
    window_label: &str,
) -> Result<(), RpcError> {
    match closed_connections.owner(connection_id) {
        // 关闭 tombstone 只保留窗口 owner；同一 owner 的迟到 close 幂等成功，
        // 其他窗口即使猜中 connection id 也必须继续经过归属校验。
        Some(owner) => ensure_window_label(owner, window_label),
        None => Err(RpcError::new(
            "rpc.unknownConnection",
            "RPC connection does not exist",
        )),
    }
}

fn invoke_body_bytes(request: &Request<'_>) -> Result<Vec<u8>, RpcError> {
    match request.body() {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(Value::Array(values)) => values
            .iter()
            .map(|value| {
                let byte = value.as_u64().ok_or_else(|| {
                    RpcError::new("rpc.invalidInvokeBody", "RPC JSON body must contain bytes")
                })?;
                u8::try_from(byte).map_err(|_| {
                    RpcError::new(
                        "rpc.invalidInvokeBody",
                        "RPC JSON body contains an invalid byte",
                    )
                })
            })
            .collect(),
        InvokeBody::Json(_) => Err(RpcError::new(
            "rpc.invalidInvokeBody",
            "RPC send requires a raw byte body",
        )),
    }
}

fn request_connection_id(request: &Request<'_>) -> Result<String, RpcError> {
    request
        .headers()
        .get("x-keencode-rpc-connection")
        .ok_or_else(|| RpcError::new("rpc.missingConnection", "RPC connection header is required"))?
        .to_str()
        .map(str::to_owned)
        .map_err(|_| RpcError::new("rpc.invalidConnection", "RPC connection header is invalid"))
}

fn is_main_webview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == MAIN_WINDOW_LABEL && window_label == MAIN_WINDOW_LABEL
}

/// RPC 只接受主 WebView 的调用者身份；子 WebView 与主界面共享宿主 Window，
/// 单独校验宿主 label 会把外部页面错误地提升为桌面前端。
fn require_main_webview(caller: &Webview) -> Result<String, RpcError> {
    let window = caller.window();
    if is_main_webview_identity(caller.label(), window.label()) {
        Ok(window.label().to_owned())
    } else {
        Err(RpcError::new(
            "rpc.windowMismatch",
            "RPC 连接只允许主 WebView 调用",
        ))
    }
}

/// TauriProtocol.open 对应的固定入口。
#[tauri::command(rename_all = "camelCase")]
pub fn zcode_rpc_open(
    app: AppHandle,
    caller: Webview,
    on_message: Channel<InvokeResponseBody>,
    state: State<'_, RpcGateway>,
) -> Result<String, RpcError> {
    let window_label = require_main_webview(&caller)?;
    state.open(app, window_label, on_message)
}

/// TauriProtocol.send 对应的固定入口。Request 保留原始 InvokeBody 和 header。
#[tauri::command(rename_all = "camelCase")]
pub async fn zcode_rpc_send(
    caller: Webview,
    request: Request<'_>,
    state: State<'_, RpcGateway>,
) -> Result<(), RpcError> {
    let window_label = require_main_webview(&caller)?;
    let connection_id = request_connection_id(&request)?;
    let raw = invoke_body_bytes(&request)?;
    state.send(&connection_id, &window_label, &raw).await
}

/// TauriProtocol.close 对应的固定入口。
#[tauri::command(rename_all = "camelCase")]
pub async fn zcode_rpc_close(
    caller: Webview,
    connection_id: String,
    state: State<'_, RpcGateway>,
) -> Result<(), RpcError> {
    let window_label = require_main_webview(&caller)?;
    state.close(&connection_id, &window_label).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[test]
    fn request_header_uses_protocol_numbers() {
        let bytes = codec::encode_message(
            &RpcValue::Array(vec![
                RpcValue::Int(REQUEST_PROMISE as i64),
                RpcValue::Int(9),
                RpcValue::String("known".into()),
                RpcValue::String("echo".into()),
            ]),
            &RpcValue::String("payload".into()),
        )
        .unwrap();
        let (header, body) = codec::decode_message(&bytes).unwrap();
        assert_eq!(header.as_u32(), None);
        assert_eq!(body.to_json(), json!("payload"));
        assert_eq!(bytes[0], 4);
    }

    #[test]
    fn initialize_payload_is_raw_channel_rpc_without_socket_header() {
        let payload = initialize_payload().unwrap();
        assert_eq!(payload, [4, 1, 6, 200, 1, 0]);
        let (header, body) = codec::decode_message(&payload).unwrap();
        assert_eq!(header, RpcValue::Array(vec![RpcValue::Int(200)]));
        assert_eq!(body, RpcValue::Undefined);
    }

    /// Desktop WebView 只接收表单问答，不能借 RPC 入口隐式打开 URL 能力。
    #[test]
    fn desktop_frontend_capabilities_are_form_only() {
        let router = keencode_acp::ElicitationRouter::from_client_capabilities(
            &desktop_frontend_capabilities(),
        );
        assert!(router.supports_form());
        assert!(!router.supports_url());
    }

    #[test]
    fn subscription_disposes_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let owned = Arc::clone(&count);
        let subscription = Subscription::new(move || {
            owned.fetch_add(1, Ordering::Relaxed);
        });
        subscription.dispose();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn connection_identity_rejects_a_different_window() {
        assert!(ensure_window_label("main", "main").is_ok());
        let error = ensure_window_label("main", "settings").unwrap_err();
        assert_eq!(error.code(), "rpc.windowMismatch");
    }

    #[test]
    fn late_close_is_idempotent_for_owner_but_rejects_a_foreign_window() {
        let mut tombstones = ClosedConnectionTombstones::default();
        tombstones.remember("rpc-1", "main");

        assert!(close_unknown_connection(&tombstones, "rpc-1", "main").is_ok());
        let error = close_unknown_connection(&tombstones, "rpc-1", "settings").unwrap_err();
        assert_eq!(error.code(), "rpc.windowMismatch");
    }

    #[test]
    fn closed_connection_owner_tombstones_are_bounded() {
        let mut tombstones = ClosedConnectionTombstones::default();
        for index in 0..=MAX_CLOSED_CONNECTION_TOMBSTONES {
            tombstones.remember(&format!("rpc-{index}"), "main");
        }

        assert_eq!(tombstones.owners.len(), MAX_CLOSED_CONNECTION_TOMBSTONES);
        assert!(tombstones.owner("rpc-0").is_none());
        assert_eq!(
            tombstones.owner(&format!("rpc-{MAX_CLOSED_CONNECTION_TOMBSTONES}")),
            Some("main")
        );
    }

    #[test]
    fn main_webview_identity_rejects_child_sharing_the_main_window() {
        assert!(is_main_webview_identity("main", "main"));
        assert!(!is_main_webview_identity("browser-child", "main"));
        assert!(!is_main_webview_identity("main", "settings"));
    }

    #[test]
    fn rpc_error_serializes_protocol_fields_without_nesting() {
        let error = RpcError::new("rpc.unknownMethod", "方法不存在")
            .with_data(json!({"channel": "zcode-agent"}));
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], json!("rpc.unknownMethod"));
        assert_eq!(value["message"], json!("方法不存在"));
        assert_eq!(value["data"]["channel"], json!("zcode-agent"));
        assert!(value.get("inner").is_none());
    }

    #[test]
    fn rpc_diagnostic_message_redacts_secrets_and_urls() {
        let message = sanitize_rpc_diagnostic_message(
            "request failed url=https://user:secret@example.test/v1 apiKey=private-key",
        );
        assert!(!message.contains("example.test"));
        assert!(!message.contains("private-key"));
        assert!(message.contains("[URL]"));
        assert!(message.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn close_waits_for_inbound_acceptance_before_marking_closed() {
        let state = Arc::new(ConnectionState::new());
        let acceptance_guard = state.accepting.lock().await;
        let closing_state = Arc::clone(&state);
        let close_task = tokio::spawn(async move {
            closing_state.close().await;
        });

        // close 必须等待正在接受的 payload 完成，否则旧页面请求可能越过
        // closed 标记进入 Handler。
        tokio::task::yield_now().await;
        assert!(!state.closed.load(Ordering::Acquire));
        drop(acceptance_guard);
        close_task.await.unwrap();
        assert!(state.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn window_lifecycle_cleanup_runs_after_transport_shutdown() {
        let state = Arc::new(ConnectionState::new());
        let (_release_sender, release_receiver) = oneshot::channel::<()>();
        let pending_task = tokio::spawn(async move {
            let _ = release_receiver.await;
        });
        state
            .pending
            .lock()
            .await
            .insert(7, pending_task.abort_handle());

        let disposed = Arc::new(AtomicUsize::new(0));
        let disposed_once = Arc::clone(&disposed);
        state.subscriptions.lock().await.insert(
            8,
            Subscription::new(move || {
                disposed_once.fetch_add(1, Ordering::Relaxed);
            }),
        );

        let cleanup_saw_closed = Arc::new(AtomicBool::new(false));
        let cleanup_saw_closed_once = Arc::clone(&cleanup_saw_closed);
        let state_for_cleanup = Arc::clone(&state);
        close_connection_then(Arc::clone(&state), move || {
            // close_window 使用的共享生命周期 helper 必须在业务解绑前完成
            // pending/订阅取消，页面重载不能留下旧连接的可运行任务。
            cleanup_saw_closed_once.store(
                state_for_cleanup.closed.load(Ordering::Acquire),
                Ordering::Release,
            );
        })
        .await;

        assert!(cleanup_saw_closed.load(Ordering::Acquire));
        assert!(state.pending.lock().await.is_empty());
        assert!(state.subscriptions.lock().await.is_empty());
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        assert!(pending_task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn disconnect_cleans_pending_and_subscriptions_for_reconnect() {
        let state = ConnectionState::new();
        let (_release_sender, release_receiver) = oneshot::channel::<()>();
        let pending_task = tokio::spawn(async move {
            let _ = release_receiver.await;
        });
        state
            .pending
            .lock()
            .await
            .insert(42, pending_task.abort_handle());

        let disposed = Arc::new(AtomicUsize::new(0));
        let disposed_once = Arc::clone(&disposed);
        state.subscriptions.lock().await.insert(
            7,
            Subscription::new(move || {
                disposed_once.fetch_add(1, Ordering::Relaxed);
            }),
        );

        state.close().await;
        assert!(state.closed.load(Ordering::Acquire));
        assert!(state.pending.lock().await.is_empty());
        assert!(state.subscriptions.lock().await.is_empty());
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        assert!(pending_task.await.unwrap_err().is_cancelled());

        // 重连必须拥有新的生命周期状态，不能复用已关闭连接的缓存。
        let replacement = ConnectionState::new();
        assert!(!replacement.closed.load(Ordering::Acquire));
    }

    #[test]
    fn channel_emitter_preserves_raw_binary_body() {
        let received = Arc::new(Mutex::new(None::<InvokeResponseBody>));
        let received_by_callback = Arc::clone(&received);
        let channel = Channel::<InvokeResponseBody>::new(move |body| {
            *received_by_callback.lock().unwrap() = Some(body);
            Ok(())
        });
        let payload = vec![4, 1, 6, 200, 1, 0];
        channel
            .send(InvokeResponseBody::Raw(payload.clone()))
            .unwrap();

        let body = received.lock().unwrap().take().unwrap();
        assert!(matches!(body, InvokeResponseBody::Raw(bytes) if bytes == payload));
    }
}
