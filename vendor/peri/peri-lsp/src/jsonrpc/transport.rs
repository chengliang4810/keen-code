use std::{
    collections::HashMap,
    future::Future,
    io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    sync::Arc,
};

use parking_lot::Mutex;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper};
#[cfg(windows)]
use process_wrap::tokio::{JobObject, KillOnDrop};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{ChildStdin, ChildStdout},
    sync::{mpsc, oneshot},
};

use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};

type NotificationHandler = Box<dyn Fn(Value) + Send + Sync>;
type ErrorHandler = Box<dyn Fn(LspError) + Send + Sync>;

/// Windows 桌面进程创建标志：不为 LSP 子进程创建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows Job Object 创建时使用的挂起标志，确保绑定 Job Object 后再运行子进程。
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// 将无控制台标志与 Job Object 的挂起标志合并到 Tokio 命令。
///
/// `process-wrap::tokio::JobObject` 会在前置钩子中设置 `CREATE_SUSPENDED`；
/// 此 wrapper 必须在其后注册，并再次保留挂起标志，避免覆盖 Job Object 的初始化语义。
#[cfg(windows)]
#[derive(Debug)]
struct WindowsNoWindow;

#[cfg(windows)]
impl CommandWrapper for WindowsNoWindow {
    /// 为桌面应用启动的 LSP 子进程应用统一的无控制台创建选项。
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        _core: &CommandWrap,
    ) -> io::Result<()> {
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        Ok(())
    }
}

/// LSP 子进程 wrapper：负责在同步 Drop 路径终止整个进程组或 Job Object。
///
/// Tokio 的 `Child` Drop 只会在设置 `kill_on_drop` 时终止根进程，无法覆盖 Unix
/// 子进程组；该 wrapper 将 `start_kill` 转发给 `process-wrap` 的平台实现，确保
/// `MessageDispatcher`、启动失败路径和中途取消都不会遗留孙进程。
#[derive(Debug)]
struct LspChild {
    /// 被 process-wrap 平台 wrapper 包装的实际子进程。
    inner: Option<Box<dyn ChildWrapper>>,
    /// 尚未经过成功 wait 时，Drop 是否需要终止进程树。
    terminate_on_drop: bool,
}

impl LspChild {
    /// 从 process-wrap 返回的 child wrapper 创建带 Drop 清理语义的 LSP child。
    fn new(inner: Box<dyn ChildWrapper>) -> Self {
        Self {
            inner: Some(inner),
            terminate_on_drop: true,
        }
    }
}

impl ChildWrapper for LspChild {
    /// 返回底层 process-wrap child wrapper。
    fn inner(&self) -> &dyn ChildWrapper {
        self.inner
            .as_ref()
            .expect("LSP child wrapper is already consumed")
            .inner()
    }

    /// 返回可变的底层 process-wrap child wrapper。
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner
            .as_mut()
            .expect("LSP child wrapper is already consumed")
            .inner_mut()
    }

    /// 消费 wrapper 并转移底层 child 所有权，同时取消本 wrapper 的 Drop 终止动作。
    fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.terminate_on_drop = false;
        self.inner
            .take()
            .expect("LSP child wrapper is already consumed")
    }

    /// 终止并等待整棵进程树，然后标记为已完成以避免 Drop 重复终止。
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            let result = self
                .inner
                .as_mut()
                .expect("LSP child wrapper is already consumed")
                .wait()
                .await;
            if result.is_ok() {
                self.terminate_on_drop = false;
            }
            result
        })
    }

    /// 同步向整棵进程树发送强制终止请求。
    fn start_kill(&mut self) -> io::Result<()> {
        self.inner
            .as_mut()
            .expect("LSP child wrapper is already consumed")
            .start_kill()
    }
}

impl Drop for LspChild {
    /// 在任意同步 Drop 路径终止整棵 LSP 进程树，不能只依赖根 PID 的默认 Drop。
    fn drop(&mut self) {
        if self.terminate_on_drop {
            if let Some(inner) = self.inner.as_mut() {
                // `start_kill` 在 Unix 发送进程组信号，在 Windows 调用 Job Object；
                // 进程已自然退出时返回错误属于正常竞态，不能阻止资源释放。
                let _ = inner.start_kill();
            }
        }
    }
}

/// LSP 传输层：管理子进程的 stdin/stdout/stderr 管道
pub struct LspTransport {
    /// 带跨平台进程树清理语义的子进程 wrapper。
    child: LspChild,
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
        let mut cmd = CommandWrap::with_new(command, |command| {
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });

        for (key, value) in env {
            cmd.command_mut().env(key, value);
        }

        #[cfg(unix)]
        {
            // 独立进程组使 process-wrap 的 start_kill 能一次终止 LSP 及其孙进程。
            cmd.wrap(ProcessGroup::leader());
        }
        #[cfg(windows)]
        {
            // Job Object 负责整树归属，KillOnDrop 保证 Job 句柄关闭时仍能清理遗留进程。
            cmd.wrap(KillOnDrop);
            cmd.wrap(JobObject);
            // 必须在 JobObject 后注册；其创建标志包含 CREATE_SUSPENDED，避免覆盖 Job 设置。
            cmd.wrap(WindowsNoWindow);
        }

        let mut child = cmd
            .spawn()
            .map(LspChild::new)
            .map_err(|e| LspError::LaunchFailed {
                server: command.to_string(),
                reason: e.to_string(),
            })?;

        let stdin = child.stdin().take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdin".to_string(),
        })?;

        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| LspError::LaunchFailed {
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
    /// stdin 写入端 — dispatch 需向服务器回写响应（如未知请求的 -32601）时使用；
    /// 用 tokio::sync::Mutex 以支持跨 await 持有
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
}

/// 消息分发器：后台读取 stdout，分发到 pending_requests 或 notification_handlers
pub struct MessageDispatcher {
    /// 共享分发状态，供后台 dispatch loop 使用
    dispatch_state: Arc<DispatchState>,
    /// read loop 任务句柄
    read_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 子进程句柄（与 read task 共享）— close() 先 kill 再 abort read task，避免孤儿进程。
    child: Arc<tokio::sync::Mutex<Option<LspChild>>>,
}

impl MessageDispatcher {
    pub fn new(transport: LspTransport) -> (Self, mpsc::UnboundedReceiver<String>) {
        let stdin = transport.stdin;
        let mut stdout_reader = transport.stdout_reader;
        let mut child = transport.child;
        let stderr = child.stderr().take();

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

        // 子进程句柄与 read task 共享：EOF 或 close() 时都能 kill
        let child_handle = Arc::new(tokio::sync::Mutex::new(Some(child)));
        let task_child = Arc::clone(&child_handle);

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
            // EOF/读取失败：尝试 kill 子进程（若 close() 已 kill，此处失败无害）
            if let Some(child) = task_child.lock().await.as_mut() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        });

        let dispatcher = Self {
            dispatch_state: Arc::new(DispatchState {
                pending: Mutex::new(HashMap::new()),
                notification_handlers: Mutex::new(HashMap::new()),
                on_error: Mutex::new(None),
                stdin: tokio::sync::Mutex::new(Some(stdin)),
            }),
            read_task: Mutex::new(Some(read_handle)),
            child: child_handle,
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

    /// 取消 pending request（请求超时或发送失败时移除注册，避免 oneshot sender 残留）
    ///
    /// 若响应恰好已在途中、条目已被 dispatch 移除，此处为无副作用 no-op。
    pub fn cancel_request(&self, id: i64) {
        self.dispatch_state.pending.lock().remove(&id);
    }

    /// 发送消息到 transport
    pub async fn send_request(&self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let mut guard = self.dispatch_state.stdin.lock().await;
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
        let mut guard = self.dispatch_state.stdin.lock().await;
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

    /// 关闭 transport：先终止子进程（短等待），再 abort read task
    ///
    /// 顺序不能反：直接 abort read task 会跳过其中的 `child.kill()`，
    /// 子进程失去 stdout 消费者后继续存活，成为孤儿进程。
    pub async fn close(&self) {
        *self.dispatch_state.stdin.lock().await = None;
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let _ = child.start_kill();
                let _ = child.wait().await;
            })
            .await;
        }
        if let Some(handle) = self.read_task.lock().take() {
            handle.abort();
        }
    }
}

impl Drop for MessageDispatcher {
    /// Drop 时先停止 stdout 读取任务，再释放 child wrapper 触发整树终止。
    ///
    /// 不能只依赖 Tokio `Child` 的默认 Drop：它不会终止未设置 kill-on-drop 的
    /// 子进程，而且即使根进程被终止，Unix/Windows 的孙进程仍可能继续运行。
    fn drop(&mut self) {
        if let Some(handle) = self.read_task.get_mut().take() {
            handle.abort();
        }

        // 正常情况下锁空闲；若读取任务正在收尾，abort 后其 Arc 释放会继续触发
        // LspChild::drop。try_lock 避免同步 Drop 阻塞 Tokio 运行时线程。
        if let Ok(mut child) = self.child.try_lock() {
            drop(child.take());
        }
    }
}

impl DispatchState {
    async fn dispatch(&self, msg: String) {
        let value: Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "lsp", error = %e, "消息解析失败");
                return;
            }
        };

        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            let sender = self.pending.lock().remove(&id);
            match sender {
                Some(tx) => {
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
                None => {
                    // 服务器发起的请求（客户端未注册该 id）。按 JSON-RPC/LSP 规范，
                    // 带 id 的请求必须回响应；未知方法回 -32601 MethodNotFound，
                    // 否则服务器会同步等待响应，后续 textDocument 请求排队直至超时。
                    if value.get("method").and_then(|m| m.as_str()).is_some() {
                        self.respond_method_not_found(id).await;
                    }
                }
            }
        } else if let Some(method) = value.get("method").and_then(|m| m.as_str()) {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let handlers = self.notification_handlers.lock();
            if let Some(handler) = handlers.get(method) {
                handler(params);
            }
        }
    }

    /// 对服务器发起的未知请求回 -32601 MethodNotFound 错误响应（写回 stdin）
    async fn respond_method_not_found(&self, id: i64) {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" }
        });
        let body = match serde_json::to_string(&response) {
            Ok(body) => body,
            Err(_) => return,
        };
        if let Some(stdin) = self.stdin.lock().await.as_mut() {
            let _ = codec::encode_message(body.as_bytes(), stdin).await;
        }
    }

    /// 当前 pending 请求数（仅测试断言超时/发送失败后无残留）
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.lock().len()
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
        state.dispatch(msg).await;
    }
    // channel 关闭（stdout EOF 或读取错误），拒绝所有 pending 请求
    tracing::error!(target: "lsp", "LSP transport 断开：stdout EOF，拒绝所有 pending 请求");
    state.reject_all_pending("LSP 服务器已断开连接");
    // 通知上层服务器断开，更新 ServerState
    state.invoke_on_error(LspError::TransportClosed);
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
