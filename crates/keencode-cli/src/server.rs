//! 平台无关的本地 Host IPC Server。
//!
//! 该模块只负责把本地 NDJSON 连接适配到 `HostDispatch`；Host lease、discovery、
//! Prompt admission 和 Session 状态仍由上层 HostRuntime/adapter 持有。这样
//! Desktop 和 headless 可以共享同一传输生命周期，而不会在 CLI crate 中复制
//! 一套 Agent Runtime。

use crate::discovery::EndpointRecord;
use crate::transport::{
    IpcConnection, IpcListener, NdjsonError, NdjsonFrame, NdjsonReader, NdjsonWriter,
};
use keencode_acp::ConnectionId;
use serde_json::{Value, json};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, oneshot, watch};
use tokio::task::JoinSet;

/// Host dispatch 的异步返回类型。
pub type HostDispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Value>, HostDispatchError>> + Send + 'a>>;

/// 上层 Host 对一个 JSON-RPC 输入帧的唯一业务入口。
///
/// `ConnectionId` 与 JSON-RPC request id 完全独立，用于 operation admission、断开
/// 处理和审计绑定。实现可以忽略它，但不能用 request id 替代它。
pub trait HostDispatch: Send + Sync + 'static {
    /// 分发一条输入帧；通知或 Client Response 应返回 `Ok(None)`。
    fn dispatch(&self, connection_id: ConnectionId, message: Value) -> HostDispatchFuture<'_>;

    /// 订阅当前连接的 Host 主动消息；不需要主动投递的 adapter 返回 `None`。
    ///
    /// Receiver 有界且只服务单个本地连接。生产 adapter 必须把事件正文控制在
    /// ACP/NDJSON 单帧上限内，落后时可以丢弃并由上层通过 Session Journal 追赶。
    fn subscribe(&self, _connection_id: &ConnectionId) -> Option<broadcast::Receiver<Value>> {
        None
    }

    /// 连接断开后释放连接级投递状态；不得因此取消 detached operation。
    fn disconnected(&self, _connection_id: &ConnectionId) {}
}

/// Host dispatch 失败；具体内部错误不得直接回显给 IPC Client。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostDispatchError {
    /// Host 业务或 Runtime 失败。
    Internal,
}

impl fmt::Display for HostDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Internal => formatter.write_str("Host dispatch failed"),
        }
    }
}

impl std::error::Error for HostDispatchError {}

/// Host Activity 查询边界；不依赖 Tauri `AppHandle`。
///
/// 只要仍有活动 Turn、后台任务或待决用户输入，idle policy 就不能停止 Host。
pub trait HostActivity: Send + Sync + 'static {
    /// 当前是否存在需要 Host 继续存活的权威工作。
    fn has_active_work(&self) -> bool;
}

/// 默认无活动实现，适合纯传输测试或不支持后台任务查询的 adapter。
#[derive(Debug, Default)]
pub struct NoopHostActivity;

impl HostActivity for NoopHostActivity {
    fn has_active_work(&self) -> bool {
        false
    }
}

/// Idle policy 查询边界。
pub trait HostIdlePolicy: Send + Sync + 'static {
    /// 返回下一次 idle 检查的时间边界；`None` 表示不自动停止。
    fn timeout(&self) -> Option<Duration>;

    /// 判断 Host 是否可以在当前 idle 时长后停止。
    fn should_stop(
        &self,
        idle_for: Duration,
        active_connections: usize,
        activity: &dyn HostActivity,
    ) -> bool;
}

/// 固定时长的 idle policy；默认构造为产品要求的 30 秒。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedIdlePolicy {
    timeout: Duration,
}

impl FixedIdlePolicy {
    /// 创建一个指定空闲时长的 policy。
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// 创建 30 秒无连接、无活动工作的默认 policy。
    pub const fn thirty_seconds() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl HostIdlePolicy for FixedIdlePolicy {
    fn timeout(&self) -> Option<Duration> {
        Some(self.timeout)
    }

    fn should_stop(
        &self,
        idle_for: Duration,
        active_connections: usize,
        activity: &dyn HostActivity,
    ) -> bool {
        idle_for >= self.timeout && active_connections == 0 && !activity.has_active_work()
    }
}

/// 本地 server 配置；所有队列和并发均有明确上限。
#[derive(Clone)]
pub struct LocalHostServerConfig {
    /// 尚未交给连接 handler 的连接队列容量。
    pub connection_queue_capacity: usize,
    /// 每条连接同时执行的 dispatch 数量上限；允许 cancel 通知穿过长 Prompt。
    pub max_inflight_requests_per_connection: usize,
    /// graceful stop 等待活动连接收敛的最大时长。
    pub graceful_shutdown_timeout: Duration,
    /// 可选 idle policy；headless 通常使用 30 秒实现，Desktop 传 `None`。
    pub idle_policy: Option<Arc<dyn HostIdlePolicy>>,
}

impl fmt::Debug for LocalHostServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalHostServerConfig")
            .field("connection_queue_capacity", &self.connection_queue_capacity)
            .field(
                "max_inflight_requests_per_connection",
                &self.max_inflight_requests_per_connection,
            )
            .field("graceful_shutdown_timeout", &self.graceful_shutdown_timeout)
            .field(
                "idle_policy",
                &self.idle_policy.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

impl Default for LocalHostServerConfig {
    fn default() -> Self {
        Self {
            connection_queue_capacity: 32,
            max_inflight_requests_per_connection: 32,
            graceful_shutdown_timeout: Duration::from_secs(5),
            idle_policy: None,
        }
    }
}

impl LocalHostServerConfig {
    fn validate(&self) -> Result<(), LocalHostServerError> {
        if self.connection_queue_capacity == 0
            || self.connection_queue_capacity > 1024
            || self.max_inflight_requests_per_connection == 0
            || self.max_inflight_requests_per_connection > 1024
            || self.graceful_shutdown_timeout.is_zero()
        {
            return Err(LocalHostServerError::InvalidConfig);
        }
        Ok(())
    }
}

/// 本地 server 绑定或运行失败。
#[derive(Debug)]
pub enum LocalHostServerError {
    /// server 配置越过固定资源边界。
    InvalidConfig,
    /// 本地 listener/NDJSON 传输错误。
    Transport(String),
    /// server task 被 JoinHandle 中止或 panic。
    Task(String),
    /// graceful stop 在边界内未能等待活动连接结束。
    ShutdownTimeout,
}

impl fmt::Display for LocalHostServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => formatter.write_str("本地 Host server 配置无效"),
            Self::Transport(error) => write!(formatter, "本地 Host transport 失败: {error}"),
            Self::Task(error) => write!(formatter, "本地 Host server task 失败: {error}"),
            Self::ShutdownTimeout => formatter.write_str("本地 Host server graceful stop 超时"),
        }
    }
}

impl std::error::Error for LocalHostServerError {}

impl From<NdjsonError> for LocalHostServerError {
    fn from(error: NdjsonError) -> Self {
        Self::Transport(error.to_string())
    }
}

/// 已绑定但尚未发布 discovery 的本地 server。
pub struct LocalHostServer {
    listener: IpcListener,
    config: LocalHostServerConfig,
    dispatch: Arc<dyn HostDispatch>,
    activity: Arc<dyn HostActivity>,
}

impl LocalHostServer {
    /// 绑定 discovery 指定的本地端点。
    ///
    /// 调用方应在该方法成功后发布 discovery，再调用 [`Self::spawn`]，避免客户端
    /// 看到已发布但尚未监听的端点。
    pub async fn bind(
        record: &EndpointRecord,
        config: LocalHostServerConfig,
        dispatch: Arc<dyn HostDispatch>,
        activity: Arc<dyn HostActivity>,
    ) -> Result<Self, LocalHostServerError> {
        config.validate()?;
        let listener = IpcListener::bind(record).await?;
        Ok(Self {
            listener,
            config,
            dispatch,
            activity,
        })
    }

    /// 启动 server task，并返回可重复调用 stop 的控制句柄。
    pub fn spawn(self) -> LocalHostServerHandle {
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(serve(
            self.listener,
            self.config,
            self.dispatch,
            self.activity,
            stop_rx,
        ));
        LocalHostServerHandle {
            stop: Some(stop_tx),
            task: Some(task),
        }
    }
}

/// 可等待的 server 生命周期控制句柄。
pub struct LocalHostServerHandle {
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<Result<(), LocalHostServerError>>>,
}

impl LocalHostServerHandle {
    /// 请求停止接受新连接，并等待活动连接收敛。
    pub async fn stop(mut self) -> Result<(), LocalHostServerError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let task = self
            .task
            .take()
            .ok_or_else(|| LocalHostServerError::Task("server task already joined".to_owned()))?;
        match task.await {
            Ok(result) => result,
            Err(error) => Err(LocalHostServerError::Task(error.to_string())),
        }
    }

    /// 等待 server 因 idle policy、错误或外部 stop 结束。
    pub async fn wait(mut self) -> Result<(), LocalHostServerError> {
        let task = self
            .task
            .take()
            .ok_or_else(|| LocalHostServerError::Task("server task already joined".to_owned()))?;
        match task.await {
            Ok(result) => result,
            Err(error) => Err(LocalHostServerError::Task(error.to_string())),
        }
    }

    /// 等待 server 结束；收到 Ctrl+C 时先走与显式 stop 相同的 graceful drain。
    pub async fn wait_for_ctrl_c(mut self) -> Result<(), LocalHostServerError> {
        let mut task = self
            .task
            .take()
            .ok_or_else(|| LocalHostServerError::Task("server task already joined".to_owned()))?;
        tokio::select! {
            result = &mut task => match result {
                Ok(result) => result,
                Err(error) => Err(LocalHostServerError::Task(error.to_string())),
            },
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| LocalHostServerError::Task(error.to_string()))?;
                if let Some(stop) = self.stop.take() {
                    let _ = stop.send(());
                }
                match task.await {
                    Ok(result) => result,
                    Err(error) => Err(LocalHostServerError::Task(error.to_string())),
                }
            }
        }
    }
}

impl Drop for LocalHostServerHandle {
    fn drop(&mut self) {
        // 未持有句柄意味着调用方不再管理 Host 生命周期；关闭 task 也能释放
        // listener，避免 endpoint 继续对外可连接。
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct ServerActivity {
    last_activity: Mutex<Instant>,
    active_connections: AtomicUsize,
    changed: Notify,
}

impl ServerActivity {
    fn new() -> Self {
        Self {
            last_activity: Mutex::new(Instant::now()),
            active_connections: AtomicUsize::new(0),
            changed: Notify::new(),
        }
    }

    async fn touch(&self) {
        *self.last_activity.lock().await = Instant::now();
        self.changed.notify_waiters();
    }

    async fn idle_for(&self) -> Duration {
        Instant::now().saturating_duration_since(*self.last_activity.lock().await)
    }
}

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

async fn serve(
    listener: IpcListener,
    config: LocalHostServerConfig,
    dispatch: Arc<dyn HostDispatch>,
    activity: Arc<dyn HostActivity>,
    stop_rx: oneshot::Receiver<()>,
) -> Result<(), LocalHostServerError> {
    let server_activity = Arc::new(ServerActivity::new());
    let (connection_tx, mut connection_rx) =
        tokio::sync::mpsc::channel(config.connection_queue_capacity);
    let accept_task = tokio::spawn(accept_loop(
        listener,
        connection_tx,
        stop_rx,
        Arc::clone(&server_activity),
    ));
    tokio::pin!(accept_task);
    let (connection_stop_tx, connection_stop_rx) = watch::channel(false);
    let mut handlers = JoinSet::new();
    let mut idle = Box::pin(idle_wait(
        Arc::clone(&server_activity),
        config.idle_policy.clone(),
        Arc::clone(&activity),
    ));
    let mut stop_requested = false;
    let mut accept_result = None;

    loop {
        tokio::select! {
            result = &mut accept_task => {
                accept_result = Some(result);
                stop_requested = true;
            }
            connection = connection_rx.recv(), if !stop_requested => {
                if let Some(connection) = connection {
                    let dispatch = Arc::clone(&dispatch);
                    let activity = Arc::clone(&server_activity);
                    let external_activity = Arc::clone(&activity);
                    let max_inflight = config.max_inflight_requests_per_connection;
                    let connection_stop_rx = connection_stop_rx.clone();
                    handlers.spawn(async move {
                        handle_connection(
                            connection,
                            dispatch,
                            external_activity,
                            max_inflight,
                            connection_stop_rx,
                        ).await
                    });
                } else {
                    stop_requested = true;
                }
            }
            _ = &mut idle, if !stop_requested => {
                stop_requested = true;
            }
            Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                if let Err(error) = result {
                    tracing_log_task_error(error);
                }
            }
        }
        if stop_requested {
            break;
        }
    }

    drop(connection_rx);
    if accept_result.is_none() {
        // idle policy 直接触发时没有消费 stop receiver，主动中止 accept task
        // 以释放 listener；显式 stop 则 accept_loop 已经自然返回。
        accept_task.as_ref().get_ref().abort();
    }
    let accept_was_aborted = accept_result.is_none();
    let accept_result = match accept_result {
        Some(result) => result,
        None => accept_task.await,
    };
    let accept_error = accept_result
        .err()
        .filter(|error| !(accept_was_aborted && error.is_cancelled()));
    let _ = connection_stop_tx.send(true);

    let drain = async {
        while let Some(result) = handlers.join_next().await {
            if let Err(error) = result {
                tracing_log_task_error(error);
            }
        }
    };
    if tokio::time::timeout(config.graceful_shutdown_timeout, drain)
        .await
        .is_err()
    {
        return Err(LocalHostServerError::ShutdownTimeout);
    }
    if let Some(error) = accept_error {
        return Err(LocalHostServerError::Task(error.to_string()));
    }
    Ok(())
}

async fn accept_loop(
    mut listener: IpcListener,
    connection_tx: tokio::sync::mpsc::Sender<IpcConnection>,
    mut stop_rx: oneshot::Receiver<()>,
    activity: Arc<ServerActivity>,
) -> Result<(), LocalHostServerError> {
    loop {
        let connection = tokio::select! {
            _ = &mut stop_rx => return Ok(()),
            result = listener.accept() => result?,
        };
        activity.active_connections.fetch_add(1, Ordering::AcqRel);
        activity.touch().await;
        // 满载时拒绝新连接而不是无限制积压；客户端可以通过 discovery 重试。
        if connection_tx.try_send(connection).is_err() {
            activity.active_connections.fetch_sub(1, Ordering::AcqRel);
            activity.touch().await;
        }
    }
}

async fn handle_connection(
    connection: IpcConnection,
    dispatch: Arc<dyn HostDispatch>,
    activity: Arc<ServerActivity>,
    max_inflight: usize,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<(), LocalHostServerError> {
    let connection_id = ConnectionId::new(format!(
        "ipc-{}",
        NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed)
    ))
    .map_err(|error| LocalHostServerError::Task(error.to_string()))?;
    let (read, write) = connection.split();
    let mut reader = NdjsonReader::new(read);
    let writer = Arc::new(Mutex::new(NdjsonWriter::new(write)));
    let mut events = dispatch.subscribe(&connection_id);
    let permits = Arc::new(Semaphore::new(max_inflight));
    let mut requests = JoinSet::new();

    let result = loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_err() {
                    break Err(LocalHostServerError::Task(
                        "connection stop channel closed".to_owned(),
                    ));
                }
                if *stop_rx.borrow() {
                    requests.abort_all();
                    break Ok(());
                }
            }
            frame = reader.next() => {
                let frame = match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(LocalHostServerError::from(error)),
                };
                activity.touch().await;
                let permit = tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() {
                            break Err(LocalHostServerError::Task(
                                "connection stop channel closed".to_owned(),
                            ));
                        }
                        requests.abort_all();
                        break Ok(());
                    }
                    permit = Arc::clone(&permits).acquire_owned() => {
                        match permit {
                            Ok(permit) => permit,
                            Err(_) => break Err(LocalHostServerError::Task(
                                "request semaphore closed".to_owned(),
                            )),
                        }
                    }
                };
                let dispatch = Arc::clone(&dispatch);
                let writer = Arc::clone(&writer);
                let connection_id = connection_id.clone();
                let activity = Arc::clone(&activity);
                requests.spawn(async move {
                    let _permit = permit;
                    dispatch_frame(
                        dispatch,
                        writer,
                        connection_id,
                        frame,
                    ).await?;
                    activity.touch().await;
                    Ok::<(), LocalHostServerError>(())
                });
            }
            event = async {
                match events.as_mut() {
                    Some(receiver) => receiver.recv().await.ok(),
                    None => std::future::pending::<Option<Value>>().await,
                }
            }, if events.is_some() => {
                if let Some(event) = event {
                    let frame = NdjsonFrame::new(event)?;
                    writer.lock().await.send(&frame).await?;
                } else {
                    events = None;
                }
            }
            Some(result) = requests.join_next(), if !requests.is_empty() => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break Err(error),
                    Err(error) => tracing_log_task_error(error),
                }
            }
        }
    };

    while requests.join_next().await.is_some() {}
    dispatch.disconnected(&connection_id);
    activity.active_connections.fetch_sub(1, Ordering::AcqRel);
    activity.touch().await;
    result
}

async fn dispatch_frame(
    dispatch: Arc<dyn HostDispatch>,
    writer: Arc<Mutex<NdjsonWriter<crate::transport::BoxedWrite>>>,
    connection_id: ConnectionId,
    frame: NdjsonFrame,
) -> Result<(), LocalHostServerError> {
    let request = frame.value.clone();
    match dispatch.dispatch(connection_id, frame.value).await {
        Ok(Some(response)) => {
            let response = NdjsonFrame::new(response)?;
            writer.lock().await.send(&response).await?;
        }
        Ok(None) => {}
        Err(_) => {
            // 业务错误只向带合法 request id 的调用返回通用 JSON-RPC 错误；不把
            // Provider、路径或内部错误正文泄露到本地 IPC wire。
            if let Some(id) = request.get("id").filter(|value| valid_rpc_id(value)) {
                let response = NdjsonFrame::new(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32603, "message": "Host dispatch failed"}
                }))?;
                writer.lock().await.send(&response).await?;
            }
        }
    }
    Ok(())
}

async fn idle_wait(
    activity: Arc<ServerActivity>,
    policy: Option<Arc<dyn HostIdlePolicy>>,
    host_activity: Arc<dyn HostActivity>,
) {
    let Some(policy) = policy else {
        std::future::pending::<()>().await;
        return;
    };
    let Some(timeout) = policy.timeout() else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        let idle_for = activity.idle_for().await;
        let wait_for = timeout.saturating_sub(idle_for);
        tokio::select! {
            _ = tokio::time::sleep(wait_for) => {
                let idle_for = activity.idle_for().await;
                let active_connections = activity.active_connections.load(Ordering::Acquire);
                if policy.should_stop(idle_for, active_connections, host_activity.as_ref()) {
                    return;
                }
            }
            _ = activity.changed.notified() => {}
        }
    }
}

fn valid_rpc_id(value: &Value) -> bool {
    matches!(value, Value::String(value) if !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        || matches!(value, Value::Number(_))
}

fn tracing_log_task_error(error: tokio::task::JoinError) {
    // 连接级 dispatch 已经把错误归一为安全 JSON-RPC 响应；这里仅保留
    // 一个不含请求正文的 stderr 诊断，避免 server task 静默丢失。
    eprintln!("keencode host connection task failed: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::discovery::{HostOwnerKind, IpcTransport};
    #[cfg(windows)]
    use crate::{HostClient, HostClientConfig};
    #[cfg(windows)]
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(windows)]
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    #[cfg(windows)]
    use tokio::sync::{Notify, broadcast};

    #[cfg(unix)]
    struct EchoDispatch;

    #[cfg(unix)]
    impl HostDispatch for EchoDispatch {
        fn dispatch(&self, _connection_id: ConnectionId, message: Value) -> HostDispatchFuture<'_> {
            Box::pin(async move {
                if message.get("method").and_then(Value::as_str) == Some("notify") {
                    Ok(None)
                } else {
                    Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id").cloned().unwrap_or(Value::Null),
                        "result": {"ok": true}
                    })))
                }
            })
        }
    }

    #[cfg(windows)]
    struct DesktopClientIsolationState {
        /// 真实 headless owner 的根指纹；Client 握手必须验证该身份。
        data_root_fingerprint: String,
        /// 每个 named pipe 连接的事件出口。
        event_sinks: std::sync::Mutex<BTreeMap<String, broadcast::Sender<Value>>>,
        /// 进入 Prompt 的次数；测试第一轮 Prompt 的 Elicitation/cancel 路径。
        prompt_calls: AtomicUsize,
        /// 显式 `session/cancel` 的次数；断开不能隐式增加该计数。
        cancel_calls: AtomicUsize,
        /// Elicitation response 的次数。
        elicitation_responses: AtomicUsize,
        /// Host 观察到连接断开的次数。
        disconnects: AtomicUsize,
        prompt_started: AtomicBool,
        prompt_started_notify: Notify,
        elicitation_answered: AtomicBool,
        elicitation_answered_notify: Notify,
        cancel_requested: AtomicBool,
        cancel_requested_notify: Notify,
        detached: AtomicBool,
        detached_notify: Notify,
    }

    #[cfg(windows)]
    impl DesktopClientIsolationState {
        fn new(data_root_fingerprint: String) -> Self {
            Self {
                data_root_fingerprint,
                event_sinks: std::sync::Mutex::new(BTreeMap::new()),
                prompt_calls: AtomicUsize::new(0),
                cancel_calls: AtomicUsize::new(0),
                elicitation_responses: AtomicUsize::new(0),
                disconnects: AtomicUsize::new(0),
                prompt_started: AtomicBool::new(false),
                prompt_started_notify: Notify::new(),
                elicitation_answered: AtomicBool::new(false),
                elicitation_answered_notify: Notify::new(),
                cancel_requested: AtomicBool::new(false),
                cancel_requested_notify: Notify::new(),
                detached: AtomicBool::new(false),
                detached_notify: Notify::new(),
            }
        }

        async fn wait_for(flag: &AtomicBool, notify: &Notify, label: &str) {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !flag.load(Ordering::Acquire) {
                    notify.notified().await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("named pipe 隔离测试等待 {label} 不应超时"));
        }

        fn send_event(&self, connection_id: &ConnectionId, event: Value) {
            let sinks = self
                .event_sinks
                .lock()
                .expect("test event sink lock should not be poisoned");
            if let Some(sender) = sinks.get(connection_id.as_str()) {
                sender.send(event).expect("named pipe 事件接收端应存在");
            }
        }
    }

    #[cfg(windows)]
    struct DesktopClientIsolationDispatch {
        state: Arc<DesktopClientIsolationState>,
    }

    #[cfg(windows)]
    impl HostDispatch for DesktopClientIsolationDispatch {
        fn dispatch(&self, connection_id: ConnectionId, message: Value) -> HostDispatchFuture<'_> {
            let state = Arc::clone(&self.state);
            Box::pin(async move {
                let method = message.get("method").and_then(Value::as_str);
                match method {
                    Some("initialize") => Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id").cloned().unwrap_or(Value::Null),
                        "result": {
                            "protocolVersion": 1,
                            "_meta": {
                                "keencode/host/dataRootFingerprint": state.data_root_fingerprint.clone(),
                                "keencode/host/ownerKind": "headless"
                            }
                        }
                    }))),
                    Some("session/prompt") => {
                        let prompt_number = state.prompt_calls.fetch_add(1, Ordering::AcqRel) + 1;
                        if prompt_number == 1 {
                            state.prompt_started.store(true, Ordering::Release);
                            state.prompt_started_notify.notify_waiters();
                            state.send_event(
                                &connection_id,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": "elicitation-e2e-1",
                                    "method": "elicitation/create",
                                    "params": {
                                        "sessionId": "desktop-client-e2e",
                                        "message": "继续执行吗?"
                                    }
                                }),
                            );
                            DesktopClientIsolationState::wait_for(
                                &state.elicitation_answered,
                                &state.elicitation_answered_notify,
                                "Elicitation response",
                            )
                            .await;
                            DesktopClientIsolationState::wait_for(
                                &state.cancel_requested,
                                &state.cancel_requested_notify,
                                "Prompt cancel",
                            )
                            .await;
                        }
                        Ok(Some(json!({
                            "jsonrpc": "2.0",
                            "id": message.get("id").cloned().unwrap_or(Value::Null),
                            "result": {"cancelled": prompt_number == 1}
                        })))
                    }
                    Some("session/cancel") => {
                        state.cancel_calls.fetch_add(1, Ordering::AcqRel);
                        state.cancel_requested.store(true, Ordering::Release);
                        state.cancel_requested_notify.notify_waiters();
                        Ok(None)
                    }
                    Some("session/list") => Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id").cloned().unwrap_or(Value::Null),
                        "result": {"sessions": []}
                    }))),
                    None if message.get("id").is_some() => {
                        state.elicitation_responses.fetch_add(1, Ordering::AcqRel);
                        state.elicitation_answered.store(true, Ordering::Release);
                        state.elicitation_answered_notify.notify_waiters();
                        Ok(None)
                    }
                    _ => Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id").cloned().unwrap_or(Value::Null),
                        "error": {"code": -32601, "message": "method not found"}
                    }))),
                }
            })
        }

        fn subscribe(&self, connection_id: &ConnectionId) -> Option<broadcast::Receiver<Value>> {
            let mut sinks = self
                .state
                .event_sinks
                .lock()
                .expect("test event sink lock should not be poisoned");
            Some(
                sinks
                    .entry(connection_id.as_str().to_owned())
                    .or_insert_with(|| broadcast::channel(16).0)
                    .subscribe(),
            )
        }

        fn disconnected(&self, connection_id: &ConnectionId) {
            self.state
                .event_sinks
                .lock()
                .expect("test event sink lock should not be poisoned")
                .remove(connection_id.as_str());
            self.state.disconnects.fetch_add(1, Ordering::AcqRel);
            self.state.detached.store(true, Ordering::Release);
            self.state.detached_notify.notify_waiters();
        }
    }

    #[derive(Default)]
    struct Inactive;

    impl HostActivity for Inactive {
        fn has_active_work(&self) -> bool {
            false
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server支持通知无响应并保留request_id() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = root.path().join("host.sock");
        let record = EndpointRecord::new(
            IpcTransport::UnixSocket,
            endpoint.to_string_lossy(),
            HostOwnerKind::Headless,
            std::process::id(),
            root.path(),
            "2026-09-21T12:00:00Z",
        )
        .unwrap();
        let server = LocalHostServer::bind(
            &record,
            LocalHostServerConfig {
                graceful_shutdown_timeout: Duration::from_secs(1),
                ..LocalHostServerConfig::default()
            },
            Arc::new(EchoDispatch),
            Arc::new(Inactive),
        )
        .await
        .unwrap()
        .spawn();
        let connection = IpcConnection::connect(&record).await.unwrap();
        let (read, write) = connection.split();
        let mut reader = NdjsonReader::new(read);
        let mut writer = NdjsonWriter::new(write);
        writer
            .send(
                &NdjsonFrame::new(json!({
                    "jsonrpc":"2.0","method":"notify","params":{}
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        writer
            .send(
                &NdjsonFrame::new(json!({
                    "jsonrpc":"2.0","id":7,"method":"echo","params":{}
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), reader.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.value["id"], 7);
        #[cfg(unix)]
        let mode = std::fs::metadata(&endpoint).unwrap().permissions().mode() & 0o777;
        drop(writer);
        drop(reader);
        server.stop().await.unwrap();
        #[cfg(unix)]
        {
            assert_eq!(mode, 0o600);
            assert!(!Path::new(&record.endpoint).exists());
        }
    }

    #[test]
    fn fixed_idle_policy默认三十秒且活动连接不停止() {
        let policy = FixedIdlePolicy::thirty_seconds();
        assert_eq!(policy.timeout(), Some(Duration::from_secs(30)));
        assert!(!policy.should_stop(Duration::from_secs(30), 1, &Inactive));
        assert!(policy.should_stop(Duration::from_secs(30), 0, &Inactive));
    }

    /// Windows Desktop Client 必须通过 headless owner 的 named pipe 复用同一 Host。
    ///
    /// 该测试覆盖真实 discovery、ACP initialize、事件/Client Request、Prompt 取消、
    /// 连接断开和再次连接。Desktop 取得的只能是 `HostRuntimeAcquire::Client`；断开
    /// 只释放连接级出口，不能替 headless owner 取消操作或释放根级 lease。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn windows_named_pipe_desktop_client_reuses_headless_and_detaches() {
        use crate::discovery::{HostOwnerKind, IpcTransport};
        use keencode_runtime::{HostRuntime, HostRuntimeAcquire, HostRuntimeMode};
        use std::time::SystemTime;

        static NEXT_E2E_PIPE: AtomicU64 = AtomicU64::new(1);

        let data_root = tempfile::tempdir().expect("测试数据根应创建");
        let headless = match HostRuntime::acquire(data_root.path(), HostOwnerKind::Headless)
            .expect("headless owner 应取得根级 lease")
        {
            HostRuntimeAcquire::Owned(runtime) => runtime,
            HostRuntimeAcquire::Client(_) => panic!("首次 Host 必须是 headless owner"),
        };
        let pipe_id = NEXT_E2E_PIPE.fetch_add(1, Ordering::Relaxed);
        let endpoint = format!(
            r"\\.\pipe\keencode-desktop-client-e2e-{}-{pipe_id}",
            std::process::id()
        );
        let record = EndpointRecord::new_with_host_id(
            IpcTransport::NamedPipe,
            endpoint.clone(),
            HostOwnerKind::Headless,
            std::process::id(),
            headless.host_id().to_owned(),
            headless.data_root_fingerprint().to_owned(),
            format!(
                "e2e-{}",
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("系统时间应有效")
                    .as_nanos()
            ),
        )
        .expect("named pipe discovery 应有效");
        let state = Arc::new(DesktopClientIsolationState::new(
            headless.data_root_fingerprint().to_owned(),
        ));
        let dispatch = Arc::new(DesktopClientIsolationDispatch {
            state: Arc::clone(&state),
        });
        let server = LocalHostServer::bind(
            &record,
            LocalHostServerConfig {
                graceful_shutdown_timeout: Duration::from_secs(2),
                ..LocalHostServerConfig::default()
            },
            dispatch,
            Arc::new(Inactive),
        )
        .await
        .expect("headless named pipe server 应绑定");
        headless
            .mark_ready_and_publish(IpcTransport::NamedPipe, endpoint)
            .expect("headless discovery 应在监听器就绪后发布");
        let server = server.spawn();

        let desktop_runtime = HostRuntime::acquire(data_root.path(), HostOwnerKind::Desktop)
            .expect("Desktop Client 应读取既有 headless owner");
        let desktop_runtime = match desktop_runtime {
            HostRuntimeAcquire::Client(runtime) => runtime,
            HostRuntimeAcquire::Owned(_) => panic!("Desktop Client 不得构造第二个 Host owner"),
        };
        assert_eq!(desktop_runtime.mode(), HostRuntimeMode::Client);
        assert_eq!(desktop_runtime.owner_kind(), HostOwnerKind::Headless);
        assert_eq!(
            headless.phase().unwrap(),
            keencode_acp::HostLifecyclePhase::Ready
        );

        let mut client_config = HostClientConfig::new(data_root.path());
        client_config.request_timeout = Duration::from_secs(2);
        let client = HostClient::connect(client_config)
            .await
            .expect("Desktop Client 应通过 headless named pipe 完成握手");
        let mut events = client.subscribe();
        let prompt_client = client.clone();
        let prompt = tokio::spawn(async move {
            prompt_client
                .dispatch_message_without_timeout(json!({
                    "jsonrpc":"2.0",
                    "id":"prompt-e2e-1",
                    "method":"session/prompt",
                    "params":{"sessionId":"desktop-client-e2e","prompt":[{"text":"测试"}]}
                }))
                .await
        });
        DesktopClientIsolationState::wait_for(
            &state.prompt_started,
            &state.prompt_started_notify,
            "Prompt start",
        )
        .await;
        let elicitation = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("Elicitation 事件应到达 Desktop Client")
            .expect("Host 事件总线不应提前关闭");
        assert_eq!(elicitation.method, "elicitation/create");
        assert_eq!(elicitation.params["sessionId"], "desktop-client-e2e");
        client
            .respond_value(
                elicitation
                    .request_id
                    .expect("Elicitation request id 应保留原始类型"),
                json!({"action":"accept"}),
            )
            .await
            .expect("Desktop Client 应能回答 Elicitation");
        DesktopClientIsolationState::wait_for(
            &state.elicitation_answered,
            &state.elicitation_answered_notify,
            "Elicitation response",
        )
        .await;
        assert_eq!(state.elicitation_responses.load(Ordering::Acquire), 1);
        client
            .notify(
                "session/cancel",
                json!({"sessionId":"desktop-client-e2e","turnId":"turn-e2e-1"}),
            )
            .await
            .expect("显式取消通知应发送到既有 Host");
        let prompt_response = tokio::time::timeout(Duration::from_secs(2), prompt)
            .await
            .expect("取消后的 Prompt 应返回")
            .expect("Prompt dispatch task 不应 panic")
            .expect("Prompt dispatch 应成功")
            .expect("Prompt response 应成功编码");
        assert_eq!(prompt_response["result"]["cancelled"], true);
        assert_eq!(state.cancel_calls.load(Ordering::Acquire), 1);

        client.disconnect();
        drop(client);
        DesktopClientIsolationState::wait_for(&state.detached, &state.detached_notify, "detach")
            .await;
        assert_eq!(state.disconnects.load(Ordering::Acquire), 1);
        // Desktop Client 退出只 detach：没有额外 cancel，headless owner 仍保持 Ready。
        assert_eq!(state.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            headless.phase().unwrap(),
            keencode_acp::HostLifecyclePhase::Ready
        );
        assert!(
            HostRuntime::acquire(data_root.path(), HostOwnerKind::Desktop)
                .expect("headless owner 仍应持有 lease")
                .mode()
                == HostRuntimeMode::Client
        );

        let second_client = HostClient::connect(HostClientConfig::new(data_root.path()))
            .await
            .expect("detach 后 Desktop Client 应能重新连接同一 Host");
        let sessions = second_client
            .request("session/list", json!({}))
            .await
            .expect("重连后的控制面请求应复用既有 Host");
        assert_eq!(sessions.result["sessions"], json!([]));
        drop(second_client);

        server.stop().await.expect("named pipe server 应优雅停止");
        headless
            .explicit_shutdown()
            .expect("测试结束时 headless owner 应释放 lease");
        drop(headless);
        assert!(matches!(
            HostRuntime::acquire(data_root.path(), HostOwnerKind::Desktop)
                .expect("owner shutdown 后数据根应可重新取得"),
            HostRuntimeAcquire::Owned(_)
        ));
    }
}
