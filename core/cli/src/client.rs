//! 基于本地 NDJSON 的 ACP Client。

use crate::discovery::{EndpointRecord, EndpointRecordError, read_endpoint_record};
use crate::transport::{IpcConnection, NdjsonError, NdjsonFrame, NdjsonReader, NdjsonWriter};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

/// Host Client 错误。
#[derive(Clone, Debug)]
pub enum ClientError {
    /// 发现记录不存在或损坏。
    Discovery(String),
    /// 本地 socket/pipe 无法连接或已断开。
    Transport(String),
    /// ACP JSON-RPC 信封或握手不符合约定。
    Protocol(String),
    /// Host 返回的 JSON-RPC 业务错误。
    Rpc {
        /// JSON-RPC 错误码。
        code: i64,
        /// Host 返回的错误摘要。
        message: String,
    },
    /// 等待响应超过本次调用的超时。
    Timeout,
    /// Host 已关闭连接。
    Closed,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(error) => write!(formatter, "Host 发现失败: {error}"),
            Self::Transport(error) => write!(formatter, "Host 连接失败: {error}"),
            Self::Protocol(error) => write!(formatter, "Host 协议错误: {error}"),
            Self::Rpc { code, message } => {
                write!(formatter, "Host 请求失败 code={code}: {message}")
            }
            Self::Timeout => formatter.write_str("等待 Host 响应超时"),
            Self::Closed => formatter.write_str("Host 连接已关闭"),
        }
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    /// 返回最接近本次错误的 CLI 退出码。
    pub const fn exit_code(&self) -> crate::ExitCode {
        match self {
            Self::Discovery(_) | Self::Transport(_) | Self::Closed => {
                crate::ExitCode::HostUnavailable
            }
            Self::Protocol(_) => crate::ExitCode::HostUnavailable,
            Self::Rpc { code: -32000, .. } => crate::ExitCode::AuthenticationFailed,
            Self::Rpc { code: -32800, .. } => crate::ExitCode::Cancelled,
            Self::Rpc { code: -32006, .. } => crate::ExitCode::UserInputRequired,
            Self::Rpc { .. } => crate::ExitCode::TaskFailed,
            Self::Timeout => crate::ExitCode::HostUnavailable,
        }
    }
}

impl From<EndpointRecordError> for ClientError {
    fn from(error: EndpointRecordError) -> Self {
        Self::Discovery(error.to_string())
    }
}

impl From<NdjsonError> for ClientError {
    fn from(error: NdjsonError) -> Self {
        Self::Transport(error.to_string())
    }
}

/// Host Client 配置。
#[derive(Clone, Debug)]
pub struct HostClientConfig {
    /// 与 Host 共享的数据根目录。
    pub data_root: PathBuf,
    /// 建立本地连接的超时时间。
    pub connect_timeout: Duration,
    /// 单次控制面请求的超时时间；Prompt 请求默认不在 Client 层超时。
    pub request_timeout: Duration,
    /// 是否要求 Host 在 initialize `_meta` 中回显数据根指纹。
    pub require_host_identity: bool,
}

impl HostClientConfig {
    /// 创建使用默认本地数据根的配置。
    pub fn from_default_data_root() -> Result<Self, ClientError> {
        let data_root = crate::default_data_root().map_err(ClientError::from)?;
        Ok(Self::new(data_root))
    }

    /// 创建指定数据根的配置。
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(30),
            require_host_identity: true,
        }
    }
}

/// Host 发送给 Client 的事件或 Client Request。
#[derive(Clone, Debug)]
pub struct HostEvent {
    /// JSON-RPC 方法名；标准投递事件固定为 `acp://delivery`。
    pub method: String,
    /// 服务器发送的可选请求标识；存在时表示等待 Client Response。
    pub id: Option<String>,
    /// 保留 JSON-RPC 请求标识的原始类型；数字 ID 不能在响应时被改写为字符串。
    pub request_id: Option<Value>,
    /// 事件参数。
    pub params: Value,
}

impl HostEvent {
    /// 将 Host 主动消息恢复为完整 JSON-RPC 请求或通知。
    ///
    /// `HostClient` 的 reader 会把 method、params 和原始 request id 分开保存，
    /// 便于 CLI/Tauri 分别处理事件和 Client Request。桥接层需要把消息转发到
    /// 另一个 ACP 事件总线时，应使用此方法保留数字 request id，不能只拼接
    /// `id` 的字符串投影。
    pub fn to_json_rpc(&self) -> Value {
        let mut object = Map::new();
        object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        if let Some(request_id) = &self.request_id {
            object.insert("id".to_owned(), request_id.clone());
        }
        object.insert("method".to_owned(), Value::String(self.method.clone()));
        object.insert("params".to_owned(), self.params.clone());
        Value::Object(object)
    }
}

/// 标准 Client 请求结果。
#[derive(Clone, Debug, PartialEq)]
pub struct RequestResult {
    /// JSON-RPC 请求标识。
    pub id: String,
    /// `result` 对象。
    pub result: Value,
}

struct SharedClient {
    writer: mpsc::Sender<NdjsonFrame>,
    /// 本地 Client transport 的幂等关闭信号；不代表远程 Host shutdown。
    close: watch::Sender<bool>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, ClientError>>>>,
    events: broadcast::Sender<HostEvent>,
    next_id: AtomicU64,
    request_timeout: Duration,
    root_fingerprint: String,
    owner_kind: String,
    require_host_identity: bool,
    /// `connect` 已完成的远端 initialize 结果；Tauri Client 分支复用该响应，
    /// 避免向既有 Host 重复发送不同客户端身份的握手。
    initialized_result: Mutex<Option<Value>>,
    /// 连接终止原因；只用于唤醒上层桥接重连，不会触发远端 operation 取消。
    connection_closed: watch::Sender<Option<ClientError>>,
    /// 重新读取同一数据根 discovery 所需的配置快照。
    config: HostClientConfig,
}

/// 可克隆的 ACP Host Client；每个连接只有一个 reader，多个调用共享响应路由。
#[derive(Clone)]
pub struct HostClient {
    shared: Arc<SharedClient>,
}

impl HostClient {
    /// 读取发现记录、建立本地连接并完成 ACP initialize 握手。
    pub async fn connect(config: HostClientConfig) -> Result<Self, ClientError> {
        let record = read_endpoint_record(&config.data_root)?;
        let connection =
            tokio::time::timeout(config.connect_timeout, IpcConnection::connect(&record))
                .await
                .map_err(|_| ClientError::Timeout)??;
        Self::from_connection(connection, &config, &record).await
    }

    /// 使用已经建立的连接创建 Client；测试和 Host 嵌入适配器可以复用。
    pub async fn from_connection(
        connection: IpcConnection,
        config: &HostClientConfig,
        record: &EndpointRecord,
    ) -> Result<Self, ClientError> {
        let (read, write) = connection.split();
        let (writer_tx, writer_rx) = mpsc::channel(128);
        let (events_tx, _) = broadcast::channel(256);
        let (connection_closed, _) = watch::channel(None);
        let (close, close_receiver) = watch::channel(false);
        let shared = Arc::new(SharedClient {
            writer: writer_tx,
            close,
            pending: Mutex::new(HashMap::new()),
            events: events_tx,
            next_id: AtomicU64::new(1),
            request_timeout: config.request_timeout,
            root_fingerprint: record.data_root_fingerprint.clone(),
            owner_kind: match record.owner_kind {
                crate::discovery::HostOwnerKind::Desktop => "desktop".to_owned(),
                crate::discovery::HostOwnerKind::Headless => "headless".to_owned(),
            },
            require_host_identity: config.require_host_identity,
            initialized_result: Mutex::new(None),
            connection_closed,
            config: config.clone(),
        });

        let writer_shared = Arc::clone(&shared);
        tokio::spawn(writer_loop(write, writer_rx, writer_shared, close_receiver));
        let reader_shared = Arc::clone(&shared);
        tokio::spawn(reader_loop(read, reader_shared));

        let client = Self { shared };
        client.initialize().await?;
        Ok(client)
    }

    /// 订阅当前连接的服务器事件；事件顺序与 Host 的单连接投递顺序一致。
    pub fn subscribe(&self) -> broadcast::Receiver<HostEvent> {
        self.shared.events.subscribe()
    }

    /// 等待当前连接断开并返回稳定错误分类。
    ///
    /// 断开只代表本地 transport 不再可用；Host 仍按自身 admission 语义托管
    /// detached operation。调用方要恢复界面时应重新 [`Self::reconnect`]，再用
    /// `session/load` 或 `keencode/operation/status` 对账，而不是隐式发送 cancel。
    pub async fn wait_for_disconnect(&self) -> ClientError {
        let mut receiver = self.shared.connection_closed.subscribe();
        loop {
            if let Some(reason) = receiver.borrow().clone() {
                return reason;
            }
            if receiver.changed().await.is_err() {
                return ClientError::Closed;
            }
        }
    }

    /// 从当前配置重新读取 discovery 并建立一个新的 ACP 连接。
    ///
    /// 返回新 Client 而不是替换旧句柄，避免并发调用在重连期间失去明确的
    /// transport 所有权。旧连接的挂起请求会按断线失败；远端 detached operation
    /// 不受影响，恢复后由调用方显式查询其状态。
    pub async fn reconnect(&self) -> Result<Self, ClientError> {
        Self::connect(self.shared.config.clone()).await
    }

    /// 发送 ACP 请求并等待对应响应。
    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<RequestResult, ClientError> {
        let id = self.next_request_id();
        self.request_with_id(id, method.into(), params).await
    }

    /// 使用调用方提供的稳定 ID 发送 ACP 请求，支持重试时保持业务幂等键。
    pub async fn request_with_id(
        &self,
        id: impl Into<String>,
        method: String,
        params: Value,
    ) -> Result<RequestResult, ClientError> {
        let id = id.into();
        self.request_with_id_timeout(id, method, params, Some(self.shared.request_timeout))
            .await
    }

    /// 发送需要等待 Host 长时间执行的请求；不在 Client 层设置固定超时。
    ///
    /// 调用方仍应提供用户取消或外层进程生命周期控制，避免把模型执行时间
    /// 错误地限制为控制面请求的超时时间。
    pub async fn request_without_timeout(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<RequestResult, ClientError> {
        let id = self.next_request_id();
        self.request_with_id_timeout(id, method.into(), params, None)
            .await
    }

    /// 转发一条完整 JSON-RPC 输入帧，并返回 Host 的完整响应帧。
    ///
    /// 该入口用于 Desktop Client adapter：它不解析或重建 ACP 请求，因而能保留
    /// 数字 request id、通知以及 Host 发回的 Client Request Response。业务调用方
    /// 仍应优先使用类型化的 [`Self::request`] / [`Self::notify`] API。
    pub async fn dispatch_message(&self, message: Value) -> Result<Option<Value>, ClientError> {
        self.dispatch_message_with_timeout(message, Some(self.shared.request_timeout))
            .await
    }

    /// 转发一条完整 JSON-RPC 输入帧且不设置 Client 层超时。
    ///
    /// Tauri 的 `acp_dispatch` 需要用此入口处理 `session/prompt` 等模型回合：
    /// 请求可以跨越多个 Provider 轮次，取消则通过同一 HostClient 的另一条
    /// 并发 `session/cancel` 通知到达。控制面仍应使用 [`Self::dispatch_message`]
    /// 的有限超时，避免错误 Host 永久占用调用方。
    pub async fn dispatch_message_without_timeout(
        &self,
        message: Value,
    ) -> Result<Option<Value>, ClientError> {
        self.dispatch_message_with_timeout(message, None).await
    }

    async fn dispatch_message_with_timeout(
        &self,
        message: Value,
        timeout: Option<Duration>,
    ) -> Result<Option<Value>, ClientError> {
        let object = message
            .as_object()
            .ok_or_else(|| ClientError::Protocol("JSON-RPC 帧必须是对象".to_owned()))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ClientError::Protocol(
                "JSON-RPC 帧 jsonrpc 必须是 2.0".to_owned(),
            ));
        }
        if let Some(method) = object.get("method").and_then(Value::as_str) {
            // 连接建立时已经完成一次远端握手。Tauri 前端仍会通过同一个
            // `acp_dispatch` 命令发送 initialize；在本地返回缓存结果，避免
            // 把不同的客户端身份再次发送到严格拒绝重复握手的既有 Host。
            if method == "initialize"
                && let Some(id) = object.get("id").cloned()
            {
                if !valid_rpc_id(&id) || id.is_null() {
                    return Err(ClientError::Protocol(
                        "JSON-RPC request id 类型无效".to_owned(),
                    ));
                }
                let result = self
                    .shared
                    .initialized_result
                    .lock()
                    .await
                    .clone()
                    .ok_or_else(|| ClientError::Protocol("Host initialize 尚未完成".to_owned()))?;
                return Ok(Some(json!({
                    "jsonrpc":"2.0",
                    "id":id,
                    "result":result,
                })));
            }
            let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
            if let Some(id) = object.get("id") {
                if !valid_rpc_id(id) || id.is_null() {
                    return Err(ClientError::Protocol(
                        "JSON-RPC request id 类型无效".to_owned(),
                    ));
                }
                let result = self
                    .request_value_id(id.clone(), method.to_owned(), params, timeout)
                    .await;
                return Ok(Some(match result {
                    Ok(result) => json!({"jsonrpc":"2.0", "id":id, "result":result}),
                    Err(error) => client_error_response(id.clone(), &error),
                }));
            }
            let mut forwarded = Map::new();
            forwarded.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
            forwarded.insert("method".to_owned(), Value::String(method.to_owned()));
            forwarded.insert("params".to_owned(), params);
            self.send_frame(Value::Object(forwarded)).await?;
            return Ok(None);
        }

        let id = object
            .get("id")
            .ok_or_else(|| ClientError::Protocol("JSON-RPC response 缺少 id".to_owned()))?
            .clone();
        if !valid_response_id(&id) {
            return Err(ClientError::Protocol(
                "JSON-RPC response id 类型无效".to_owned(),
            ));
        }
        if let Some(result) = object.get("result") {
            self.respond_value(id, result.clone()).await?;
            return Ok(None);
        }
        if let Some(error) = object.get("error") {
            if !error.is_object() {
                return Err(ClientError::Protocol(
                    "JSON-RPC response error 必须是对象".to_owned(),
                ));
            }
            self.send_frame(json!({"jsonrpc":"2.0", "id":id, "error":error}))
                .await?;
            return Ok(None);
        }
        Err(ClientError::Protocol(
            "JSON-RPC response 必须包含 result 或 error".to_owned(),
        ))
    }

    async fn request_with_id_timeout(
        &self,
        id: String,
        method: String,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<RequestResult, ClientError> {
        let id_value = Value::String(id.clone());
        let response = self
            .request_value_id(id_value, method, params, timeout)
            .await?;
        Ok(RequestResult {
            id,
            result: response,
        })
    }

    async fn request_value_id(
        &self,
        id: Value,
        method: String,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, ClientError> {
        if !valid_rpc_id(&id) || id.is_null() {
            return Err(ClientError::Protocol("request id 无效".to_owned()));
        }
        let key = rpc_id_key(&id);
        let mut object = Map::new();
        object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        object.insert("id".to_owned(), id);
        object.insert("method".to_owned(), Value::String(method));
        object.insert("params".to_owned(), params);
        let frame = NdjsonFrame::new(Value::Object(object))?;
        let (sender, receiver) = oneshot::channel();
        self.shared.pending.lock().await.insert(key.clone(), sender);
        if self.shared.writer.send(frame).await.is_err() {
            self.shared.pending.lock().await.remove(&key);
            return Err(ClientError::Closed);
        }
        let response = match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, receiver).await {
                Ok(response) => response.map_err(|_| ClientError::Closed)??,
                Err(_) => {
                    // 超时后清理相关性表，避免长期运行的 CLI 在重复重试时积累悬挂请求。
                    self.shared.pending.lock().await.remove(&key);
                    return Err(ClientError::Timeout);
                }
            },
            None => receiver.await.map_err(|_| ClientError::Closed)??,
        };
        Ok(response)
    }

    /// 发送 ACP 通知；通知不产生响应。
    pub async fn notify(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<(), ClientError> {
        let mut object = Map::new();
        object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        object.insert("method".to_owned(), Value::String(method.into()));
        object.insert("params".to_owned(), params);
        self.shared
            .writer
            .send(NdjsonFrame::new(Value::Object(object))?)
            .await
            .map_err(|_| ClientError::Closed)
    }

    /// 回答 Host 发来的 ACP Client Request，例如 `elicitation/create`。
    pub async fn respond(&self, id: &str, result: Value) -> Result<(), ClientError> {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(ClientError::Protocol("client response id 无效".to_owned()));
        }
        self.respond_value(Value::String(id.to_owned()), result)
            .await
    }

    /// 使用 Host 请求中的原始 JSON-RPC ID 回答 Client Request。
    pub async fn respond_value(&self, id: Value, result: Value) -> Result<(), ClientError> {
        if !valid_rpc_id(&id) {
            return Err(ClientError::Protocol("client response id 无效".to_owned()));
        }
        self.send_frame(json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await
    }

    /// 关闭当前本地 IPC 连接并释放连接级 Host 状态。
    ///
    /// 该操作只断开 Client transport，不发送 `session/cancel`，也不触发远端
    /// Host owner shutdown；远端 Host 是否继续托管 detached operation 由其自身
    /// admission 语义决定。方法可重复调用，适合 Desktop 退出收尾。
    pub fn disconnect(&self) {
        let _ = self.shared.close.send(true);
    }

    async fn send_frame(&self, value: Value) -> Result<(), ClientError> {
        self.shared
            .writer
            .send(NdjsonFrame::new(value)?)
            .await
            .map_err(|_| ClientError::Closed)
    }

    /// 执行 ACP 握手；仅验证版本与 Host 数据根身份，不携带任何 Token。
    async fn initialize(&self) -> Result<(), ClientError> {
        let result = self
            .request_with_id(
                "cli-initialize".to_owned(),
                "initialize".to_owned(),
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {"name":"KeenCode CLI","version":"0.1.0"},
                    "clientCapabilities": {"elicitation": {"form": {}}}
                }),
            )
            .await?
            .result;
        let object = result
            .as_object()
            .ok_or_else(|| ClientError::Protocol("initialize result 必须为对象".to_owned()))?;
        if object.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err(ClientError::Protocol(
                "Host ACP protocolVersion 不是 1".to_owned(),
            ));
        }
        let meta = object.get("_meta").and_then(Value::as_object);
        if self.shared.require_host_identity {
            let actual = meta
                .and_then(|meta| meta.get("keencode/host/dataRootFingerprint"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ClientError::Protocol("Host initialize 缺少数据根身份".to_owned())
                })?;
            if actual != self.shared.root_fingerprint {
                return Err(ClientError::Protocol("Host 数据根指纹不匹配".to_owned()));
            }
            let owner_kind = meta
                .and_then(|meta| meta.get("keencode/host/ownerKind"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ClientError::Protocol("Host initialize 缺少 owner 身份".to_owned())
                })?;
            if owner_kind != self.shared.owner_kind {
                return Err(ClientError::Protocol("Host owner 类型不匹配".to_owned()));
            }
        }
        *self.shared.initialized_result.lock().await = Some(result);
        Ok(())
    }

    /// 生成进程内唯一请求 ID；业务 operationId 仍由调用方通过 `_meta` 传递。
    fn next_request_id(&self) -> String {
        let sequence = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        format!("cli-{millis}-{sequence}")
    }
}

async fn writer_loop(
    write: crate::transport::BoxedWrite,
    mut queue: mpsc::Receiver<NdjsonFrame>,
    shared: Arc<SharedClient>,
    mut close: watch::Receiver<bool>,
) {
    let mut writer = NdjsonWriter::new(write);
    loop {
        tokio::select! {
            changed = close.changed() => {
                if changed.is_err() || *close.borrow() {
                    break;
                }
            }
            frame = queue.recv() => {
                let Some(frame) = frame else { break; };
                if let Err(error) = writer.send(&frame).await {
                    let reason = ClientError::Transport(error.to_string());
                    mark_closed(&shared, reason.clone());
                    fail_pending(&shared, reason).await;
                    break;
                }
            }
        }
    }
}

async fn reader_loop(read: crate::transport::BoxedRead, shared: Arc<SharedClient>) {
    let mut reader = NdjsonReader::new(read);
    let mut close = shared.close.subscribe();
    let reason = loop {
        let frame = tokio::select! {
            changed = close.changed() => {
                if changed.is_err() || *close.borrow() {
                    break ClientError::Closed;
                }
                continue;
            }
            frame = reader.next() => frame,
        };
        let frame = match frame {
            Ok(Some(frame)) => frame,
            Ok(None) => break ClientError::Closed,
            Err(error) => break ClientError::Transport(error.to_string()),
        };
        let Some(object) = frame.value.as_object() else {
            break ClientError::Protocol("Host 帧必须是 JSON 对象".to_owned());
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            break ClientError::Protocol("Host 帧 jsonrpc 必须是 2.0".to_owned());
        }
        // JSON-RPC request/notification 也允许带 ID；先按 method 区分 Client Request，
        // 不能因为存在 id 就把 Host 的 elicitation/create 当成响应。
        if object.get("method").is_none() {
            let id = object
                .get("id")
                .ok_or_else(|| ClientError::Protocol("Host 响应缺少 id".to_owned()));
            let id = match id {
                Ok(id) if valid_response_id(id) => rpc_id_key(id),
                Ok(_) => break ClientError::Protocol("Host 响应 id 类型无效".to_owned()),
                Err(error) => break error,
            };
            let sender = shared.pending.lock().await.remove(&id);
            let Some(sender) = sender else {
                // 请求超时会把 pending 条目移除，Host 的迟到响应随后到达属于
                // 正常竞态：丢弃这一帧即可，不能拆掉整条连接连坐其余在途请求。
                continue;
            };
            if let Some(result) = object.get("result") {
                let _ = sender.send(Ok(result.clone()));
            } else if let Some(error) = object.get("error").and_then(Value::as_object) {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("ACP 请求失败")
                    .to_owned();
                let _ = sender.send(Err(ClientError::Rpc { code, message }));
            } else {
                let _ = sender.send(Err(ClientError::Protocol(
                    "Host 响应必须包含 result 或 error".to_owned(),
                )));
            }
            continue;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            break ClientError::Protocol("Host 帧缺少 method".to_owned());
        };
        let Some(params) = object.get("params") else {
            break ClientError::Protocol("Host 通知缺少 params".to_owned());
        };
        let request_id = object.get("id").cloned();
        if let Some(id) = &request_id
            && !valid_rpc_id(id)
        {
            break ClientError::Protocol("Host Client Request id 类型无效".to_owned());
        }
        let id = request_id.as_ref().and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Null => Some("null".to_owned()),
            _ => None,
        });
        let _ = shared.events.send(HostEvent {
            method: method.to_owned(),
            id,
            request_id,
            params: params.clone(),
        });
    };

    mark_closed(&shared, reason.clone());
    fail_pending(&shared, reason).await;
}

fn mark_closed(shared: &SharedClient, reason: ClientError) {
    if shared.connection_closed.borrow().is_none() {
        shared.connection_closed.send_replace(Some(reason));
    }
}

fn valid_rpc_id(value: &Value) -> bool {
    matches!(value, Value::String(value) if !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        || matches!(value, Value::Number(_))
        || value.is_null()
}

fn valid_response_id(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_))
}

fn rpc_id_key(value: &Value) -> String {
    match value {
        // JSON-RPC 允许同时存在字符串 "1" 与数字 1；相关性 key 必须保留
        // 类型，否则并发请求会互相覆盖 pending sender。
        Value::String(value) => format!("s:{value}"),
        Value::Number(value) => format!("n:{value}"),
        Value::Null => "z:null".to_owned(),
        _ => String::new(),
    }
}

fn client_error_response(id: Value, error: &ClientError) -> Value {
    let (code, message) = match error {
        ClientError::Rpc { code, message } => (*code, message.as_str()),
        ClientError::Timeout => (-32001, "Host request timed out"),
        ClientError::Closed => (-32002, "Host connection closed"),
        ClientError::Discovery(_) | ClientError::Transport(_) => (-32003, "Host transport failed"),
        ClientError::Protocol(_) => (-32600, "Invalid JSON-RPC request"),
    };
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code, "message":message}})
}

async fn fail_pending(shared: &SharedClient, reason: ClientError) {
    let mut pending = shared.pending.lock().await;
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(reason.clone()));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::discovery::{EndpointRecord, HostOwnerKind, IpcTransport};
    use crate::transport::{IpcConnection, NdjsonReader, NdjsonWriter};

    #[cfg(unix)]
    #[tokio::test]
    async fn client完成握手并路由响应与通知() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let endpoint = directory.path().join("host.sock");
        let record = EndpointRecord::new(
            IpcTransport::UnixSocket,
            endpoint.to_string_lossy(),
            HostOwnerKind::Headless,
            std::process::id(),
            directory.path(),
            "2026-09-21T12:00:00Z",
        )
        .expect("记录应有效");
        let listener = tokio::net::UnixListener::bind(&endpoint).expect("socket 应绑定");
        let server_record = record.clone();
        let server =
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("应接受连接");
                let (read, write) = tokio::io::split(stream);
                let mut reader = NdjsonReader::new(read);
                let mut writer = NdjsonWriter::new(write);
                let initialize = reader.next().await.unwrap().unwrap().value;
                assert_eq!(initialize["method"], "initialize");
                writer
                .send(&crate::transport::NdjsonFrame::new(json!({
                    "jsonrpc":"2.0",
                    "id":initialize["id"],
                    "result":{"protocolVersion":1,"_meta":{
                        "keencode/host/dataRootFingerprint":server_record.data_root_fingerprint,
                        "keencode/host/ownerKind":"headless"
                    }}
                })).unwrap())
                .await
                .unwrap();
                let request = reader.next().await.unwrap().unwrap().value;
                writer
                    .send(&crate::transport::NdjsonFrame::new(json!({
                        "jsonrpc":"2.0","method":"acp://delivery","params":{"type":"session_update"}
                    })).unwrap())
                    .await
                    .unwrap();
                writer
                    .send(
                        &crate::transport::NdjsonFrame::new(json!({
                            "jsonrpc":"2.0",
                            "id":"elicitation-1",
                            "method":"elicitation/create",
                            "params":{"message":"继续吗?"}
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                writer
                    .send(
                        &crate::transport::NdjsonFrame::new(json!({
                            "jsonrpc":"2.0","id":request["id"],"result":{"ok":true}
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                let response = reader.next().await.unwrap().unwrap().value;
                assert_eq!(response["id"], "elicitation-1");
                assert_eq!(response["result"]["accepted"], true);
            });
        let connection = IpcConnection::connect(&record).await.unwrap();
        let mut config = HostClientConfig::new(directory.path());
        config.require_host_identity = true;
        config.request_timeout = Duration::from_secs(2);
        let client = HostClient::from_connection(connection, &config, &record)
            .await
            .unwrap();
        let mut events = client.subscribe();
        let initialize_response = client
            .dispatch_message(json!({
                "jsonrpc":"2.0",
                "id":7,
                "method":"initialize",
                "params":{"protocolVersion":1}
            }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initialize_response["id"], 7);
        assert_eq!(initialize_response["result"]["protocolVersion"], 1);
        let response = client.request("session/list", json!({})).await.unwrap();
        assert_eq!(response.result["ok"], true);
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "acp://delivery");
        assert_eq!(event.to_json_rpc()["method"], "acp://delivery");
        let elicitation = events.recv().await.unwrap();
        assert_eq!(elicitation.method, "elicitation/create");
        assert_eq!(elicitation.to_json_rpc()["id"], "elicitation-1");
        client
            .respond_value(
                elicitation
                    .request_id
                    .expect("Elicitation 应保留 request id"),
                json!({"accepted":true}),
            )
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[test]
    fn request_id_key_preserves_json_rpc_id_type() {
        assert_ne!(rpc_id_key(&json!("1")), rpc_id_key(&json!(1)));
        assert_ne!(rpc_id_key(&json!("null")), rpc_id_key(&Value::Null));
    }

    #[tokio::test]
    async fn dispatch_message_without_timeout_allows_slow_request() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let endpoint = directory.path().join("slow.sock");
        let record = EndpointRecord::new(
            IpcTransport::UnixSocket,
            endpoint.to_string_lossy(),
            HostOwnerKind::Headless,
            std::process::id(),
            directory.path(),
            "2026-09-21T12:00:00Z",
        )
        .expect("记录应有效");
        let listener = tokio::net::UnixListener::bind(&endpoint).expect("socket 应绑定");
        let server_record = record.clone();
        let server =
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("应接受连接");
                let (read, write) = tokio::io::split(stream);
                let mut reader = NdjsonReader::new(read);
                let mut writer = NdjsonWriter::new(write);
                let initialize = reader.next().await.unwrap().unwrap().value;
                writer
                .send(&crate::transport::NdjsonFrame::new(json!({
                    "jsonrpc":"2.0",
                    "id":initialize["id"],
                    "result":{"protocolVersion":1,"_meta":{
                        "keencode/host/dataRootFingerprint":server_record.data_root_fingerprint,
                        "keencode/host/ownerKind":"headless"
                    }}
                })).unwrap())
                .await
                .unwrap();
                let request = reader.next().await.unwrap().unwrap().value;
                tokio::time::sleep(Duration::from_millis(25)).await;
                writer
                    .send(
                        &crate::transport::NdjsonFrame::new(json!({
                            "jsonrpc":"2.0",
                            "id":request["id"],
                            "result":{"slow":true}
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            });
        let connection = IpcConnection::connect(&record).await.unwrap();
        let mut config = HostClientConfig::new(directory.path());
        config.request_timeout = Duration::from_millis(1);
        let client = HostClient::from_connection(connection, &config, &record)
            .await
            .unwrap();
        let response = client
            .dispatch_message_without_timeout(json!({
                "jsonrpc":"2.0",
                "id":"slow-request",
                "method":"session/prompt",
                "params":{"sessionId":"session-test"}
            }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response["id"], "slow-request");
        assert_eq!(response["result"]["slow"], true);
        server.await.unwrap();
    }
}
