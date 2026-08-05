use std::{collections::HashMap, process::Stdio, sync::Arc};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::{mpsc, oneshot},
};

use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};

type NotificationHandler = Box<dyn Fn(Value) + Send + Sync>;
type ErrorHandler = Box<dyn Fn(LspError) + Send + Sync>;

/// LSP 传输层：管理子进程的 stdin/stdout/stderr 管道
pub struct LspTransport {
    child: Child,
    stdin: ChildStdin,
    stdout_reader: BufReader<ChildStdout>,
}

impl LspTransport {
    /// 启动 LSP 服务器子进程
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, LspError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| LspError::LaunchFailed {
            server: command.to_string(),
            reason: e.to_string(),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdin".to_string(),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdout".to_string(),
        })?;

        // 启动后立即检查进程是否存活（捕获参数错误等立即退出的情况）
        // 对参数无效等场景，进程退出极快，try_wait 通常能立即捕获
        if let Some(status) = child.try_wait().ok().flatten() {
            let code = status.code().unwrap_or(-1);
            let reason = format!("进程立即退出 (exit code: {code})，请检查命令和参数是否正确");
            return Err(LspError::LaunchFailed {
                server: command.to_string(),
                reason,
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout_reader: BufReader::new(stdout),
        })
    }

    /// 发送 JSON-RPC 请求
    pub async fn send_request(&mut self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let body = serde_json::to_string(request)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 发送 JSON-RPC 通知
    pub async fn send_notification(
        &mut self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        let body = serde_json::to_string(notification)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 读取单条 JSON-RPC 消息
    pub async fn read_message(&mut self) -> Result<Option<String>, LspError> {
        codec::decode_message(&mut self.stdout_reader).await
    }

    /// 检查子进程是否存活
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// 获取子进程 ID
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// 终止子进程
    pub async fn kill(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// 分发所需共享状态（从 MessageDispatcher 中提取，供后台 task 使用）
pub struct DispatchState {
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>,
    notification_handlers: Mutex<HashMap<String, NotificationHandler>>,
    on_error: Mutex<Option<ErrorHandler>>,
}

/// 消息分发器：后台读取 stdout，分发到 pending_requests 或 notification_handlers
pub struct MessageDispatcher {
    /// stdin 写入端 — 使用 tokio::sync::Mutex 以支持跨 await 持有
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    /// 共享分发状态，供后台 dispatch loop 使用
    dispatch_state: Arc<DispatchState>,
    /// read loop 任务句柄
    read_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl MessageDispatcher {
    pub fn new(transport: LspTransport) -> (Self, mpsc::UnboundedReceiver<String>) {
        let stdin = transport.stdin;
        let mut stdout_reader = transport.stdout_reader;
        let mut child = transport.child;
        let stderr = child.stderr.take();

        // 启动 stderr drain 任务
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            tracing::debug!(target: "lsp::stderr", "{}", line.trim());
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // 用 mpsc channel 连接 stdout 读取任务和分发逻辑
        let (tx, rx) = mpsc::unbounded_channel::<String>();

        // 启动 stdout 读取任务（独立 task）
        let read_handle = tokio::spawn(async move {
            loop {
                match codec::decode_message(&mut stdout_reader).await {
                    Ok(Some(msg)) => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(target: "lsp", "transport EOF");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(target: "lsp", error = %e, "读取消息失败");
                        break;
                    }
                }
            }
            let _ = child.kill().await;
        });

        let dispatcher = Self {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            dispatch_state: Arc::new(DispatchState {
                pending: Mutex::new(HashMap::new()),
                notification_handlers: Mutex::new(HashMap::new()),
                on_error: Mutex::new(None),
            }),
            read_task: Mutex::new(Some(read_handle)),
        };

        (dispatcher, rx)
    }

    /// 注册通知处理器
    pub fn on_notification(&self, method: &str, handler: NotificationHandler) {
        self.dispatch_state
            .notification_handlers
            .lock()
            .insert(method.to_string(), handler);
    }

    /// 注册错误回调
    pub fn set_on_error(&self, handler: ErrorHandler) {
        *self.dispatch_state.on_error.lock() = Some(handler);
    }

    /// 注册 pending request（返回 oneshot receiver）
    pub fn register_request(&self, id: i64) -> oneshot::Receiver<Result<Value, LspError>> {
        let (tx, rx) = oneshot::channel();
        self.dispatch_state.pending.lock().insert(id, tx);
        rx
    }

    /// 发送消息到 transport
    pub async fn send_request(&self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| LspError::JsonRpcError {
            code: -32002,
            message: "transport 已关闭".to_string(),
        })?;
        let body = serde_json::to_string(request)?;
        codec::encode_message(body.as_bytes(), stdin).await
    }

    /// 发送通知到 transport
    pub async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| LspError::JsonRpcError {
            code: -32002,
            message: "transport 已关闭".to_string(),
        })?;
        let body = serde_json::to_string(notification)?;
        codec::encode_message(body.as_bytes(), stdin).await
    }

    /// 获取共享分发状态的 Arc（供后台 dispatch loop 使用，不持有 tokio::sync::Mutex）
    pub fn dispatch_state(&self) -> Arc<DispatchState> {
        Arc::clone(&self.dispatch_state)
    }

    /// 关闭 transport
    pub async fn close(&self) {
        *self.stdin.lock().await = None;
        if let Some(handle) = self.read_task.lock().take() {
            handle.abort();
        }
    }
}

impl DispatchState {
    fn dispatch(&self, msg: String) {
        let value: Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "lsp", error = %e, "消息解析失败");
                return;
            }
        };

        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            let sender = self.pending.lock().remove(&id);
            if let Some(tx) = sender {
                let result = if let Some(error) = value.get("error") {
                    let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000);
                    let message = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    Err(LspError::JsonRpcError { code, message })
                } else {
                    Ok(value.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
        } else if let Some(method) = value.get("method").and_then(|m| m.as_str()) {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let handlers = self.notification_handlers.lock();
            if let Some(handler) = handlers.get(method) {
                handler(params);
            }
        }
    }

    /// 拒绝所有待处理请求（transport EOF 或错误时调用）
    fn reject_all_pending(&self, reason: &str) {
        let mut pending = self.pending.lock();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(LspError::RequestFailed {
                method: "transport".to_string(),
                reason: reason.to_string(),
            }));
        }
    }

    /// 调用 on_error 回调通知上层服务器断开
    fn invoke_on_error(&self, error: LspError) {
        if let Some(handler) = self.on_error.lock().take() {
            handler(error);
        }
    }
}

/// 独立的消息分发循环——接收 Arc<DispatchState> + rx，不持有 tokio::sync::Mutex
pub async fn run_dispatch_loop(state: Arc<DispatchState>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(msg) = rx.recv().await {
        state.dispatch(msg);
    }
    // channel 关闭（stdout EOF 或读取错误），拒绝所有 pending 请求
    tracing::error!(target: "lsp", "LSP transport 断开：stdout EOF，拒绝所有 pending 请求");
    state.reject_all_pending("LSP 服务器已断开连接");
    // 通知上层服务器断开，更新 ServerState
    state.invoke_on_error(LspError::TransportClosed);
}
