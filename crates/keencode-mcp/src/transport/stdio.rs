//! 换行分帧的 stdio MCP 传输。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;

use super::McpTransport;
use crate::config::{McpClientOptions, StdioServerConfig};
use crate::error::McpError;
use crate::process_tree::ProcessTree;
use crate::protocol::{
    IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, McpNotification,
    RequestId, parse_incoming, server_request_response,
};

type PendingSender = oneshot::Sender<Result<JsonRpcResponse, McpError>>;
type PendingMap = Arc<StdMutex<HashMap<RequestId, PendingSender>>>;

pub(super) struct StdioTransport {
    writer: Arc<Mutex<Option<ChildStdin>>>,
    child: Mutex<Option<Child>>,
    process_tree: ProcessTree,
    reader_task: Mutex<Option<JoinHandle<()>>>,
    pending: PendingMap,
    issued_numeric_high_water: Arc<AtomicI64>,
    notifications: broadcast::Sender<McpNotification>,
    max_message_bytes: usize,
    shutdown_timeout: std::time::Duration,
    closed: AtomicBool,
}

impl StdioTransport {
    pub(super) async fn connect(
        config: StdioServerConfig,
        options: &McpClientOptions,
    ) -> Result<Self, McpError> {
        config.validate()?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !config.inherit_environment {
            command.env_clear();
        }
        command.envs(&config.environment);
        if let Some(current_dir) = &config.current_dir {
            command.current_dir(current_dir);
        }
        let mut process_tree = ProcessTree::prepare(&mut command)?;

        let mut child = command
            .spawn()
            .map_err(|error| McpError::Transport(format!("无法启动 stdio MCP 子进程：{error}")))?;
        if let Err(error) = process_tree.attach(&child) {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
        if let Err(error) = process_tree.resume(&child) {
            process_tree.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
        let writer = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("stdio MCP 子进程没有可写 stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("stdio MCP 子进程没有可读 stdout".to_owned()))?;
        let writer = Arc::new(Mutex::new(Some(writer)));
        let pending = Arc::new(StdMutex::new(HashMap::new()));
        let issued_numeric_high_water = Arc::new(AtomicI64::new(0));
        let (notifications, _) = broadcast::channel(options.notification_capacity);
        let reader_task = spawn_reader(
            stdout,
            Arc::clone(&writer),
            Arc::clone(&pending),
            Arc::clone(&issued_numeric_high_water),
            notifications.clone(),
            options.max_response_bytes,
        );

        Ok(Self {
            writer,
            child: Mutex::new(Some(child)),
            process_tree,
            reader_task: Mutex::new(Some(reader_task)),
            pending,
            issued_numeric_high_water,
            notifications,
            max_message_bytes: options.max_response_bytes,
            shutdown_timeout: options.shutdown_timeout,
            closed: AtomicBool::new(false),
        })
    }

    async fn write_message<T: Serialize>(&self, message: &T) -> Result<(), McpError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::NotReady("stdio 传输已经关闭".to_owned()));
        }
        write_serialized(&self.writer, message, self.max_message_bytes).await
    }

    fn force_close_resources(&self) {
        self.closed.store(true, Ordering::Release);
        self.process_tree.terminate();
        if let Ok(mut writer) = self.writer.try_lock() {
            writer.take();
        }
        if let Ok(mut child_slot) = self.child.try_lock() {
            if let Some(mut child) = child_slot.take() {
                let _ = child.start_kill();
            }
        }
        if let Ok(mut reader_task) = self.reader_task.try_lock() {
            if let Some(reader_task) = reader_task.take() {
                reader_task.abort();
            }
        }
        fail_all(
            &self.pending,
            McpError::NotReady("stdio 传输已经关闭".to_owned()),
        );
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, request: JsonRpcRequest) -> Result<Value, McpError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::NotReady("stdio 传输已经关闭".to_owned()));
        }
        let id = request.id.clone();
        if let RequestId::Number(value) = &id {
            self.issued_numeric_high_water
                .fetch_max(*value, Ordering::AcqRel);
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = lock_unpoisoned(&self.pending);
            if pending.contains_key(&id) {
                return Err(McpError::Protocol(format!(
                    "重复的 JSON-RPC 请求 ID：{id:?}"
                )));
            }
            pending.insert(id.clone(), sender);
        }
        let mut guard = PendingGuard::new(id.clone(), Arc::clone(&self.pending));
        self.write_message(&request).await?;
        let response = receiver.await.map_err(|_| {
            McpError::Transport(format!("stdio MCP 请求 {id:?} 的响应通道已关闭"))
        })??;
        guard.disarm();
        response.into_result(&id)
    }

    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), McpError> {
        self.write_message(&notification).await
    }

    fn subscribe(&self) -> broadcast::Receiver<McpNotification> {
        self.notifications.subscribe()
    }

    async fn close(&self) -> Result<(), McpError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let close = async {
            if let Some(mut writer) = self.writer.lock().await.take() {
                let _ = writer.shutdown().await;
            }

            let mut child_slot = self.child.lock().await;
            let close_error = if let Some(child) = child_slot.as_mut() {
                child.wait().await.err().map(|error| {
                    let _ = child.start_kill();
                    McpError::Transport(format!("等待 stdio MCP 子进程退出失败：{error}"))
                })
            } else {
                None
            };
            child_slot.take();
            drop(child_slot);
            self.process_tree.terminate();

            if let Some(reader_task) = self.reader_task.lock().await.take() {
                reader_task.abort();
                let _ = reader_task.await;
            }
            fail_all(
                &self.pending,
                McpError::NotReady("stdio 传输已经关闭".to_owned()),
            );
            close_error.map_or(Ok(()), Err)
        };
        match tokio::time::timeout(self.shutdown_timeout, close).await {
            Ok(result) => result,
            Err(_) => {
                self.force_close_resources();
                Err(McpError::Timeout {
                    method: "stdio close".to_owned(),
                    duration: self.shutdown_timeout,
                })
            }
        }
    }

    fn force_close(&self) {
        self.force_close_resources();
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.force_close_resources();
    }
}

fn spawn_reader(
    stdout: tokio::process::ChildStdout,
    writer: Arc<Mutex<Option<ChildStdin>>>,
    pending: PendingMap,
    issued_numeric_high_water: Arc<AtomicI64>,
    notifications: broadcast::Sender<McpNotification>,
    max_message_bytes: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let frame = match read_bounded_frame(&mut reader, max_message_bytes).await {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    fail_all(
                        &pending,
                        McpError::Transport("stdio MCP stdout 已关闭".to_owned()),
                    );
                    break;
                }
                Err(error) => {
                    fail_all(&pending, error);
                    break;
                }
            };
            if frame.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match parse_incoming(&frame) {
                Ok(IncomingMessage::Response(response)) => {
                    let id = response.id.clone();
                    if let Some(sender) = lock_unpoisoned(&pending).remove(&id) {
                        let _ = sender.send(Ok(response));
                    } else if !is_known_late_response(
                        &id,
                        issued_numeric_high_water.load(Ordering::Acquire),
                    ) {
                        fail_all(
                            &pending,
                            McpError::Protocol(format!(
                                "收到没有对应请求的 JSON-RPC 响应 ID：{id:?}"
                            )),
                        );
                        break;
                    }
                }
                Ok(IncomingMessage::Notification(notification)) => {
                    let _ = notifications.send(notification);
                }
                Ok(IncomingMessage::ServerRequest { id, method }) => {
                    let response = server_request_response(id, &method);
                    if let Err(error) =
                        write_serialized(&writer, &response, max_message_bytes).await
                    {
                        fail_all(&pending, error);
                        break;
                    }
                }
                Err(error) => {
                    fail_all(&pending, error);
                    break;
                }
            }
        }
    })
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Option<Vec<u8>>, McpError> {
    let mut frame = Vec::new();
    loop {
        let (available_len, newline_position) = {
            let available = reader.fill_buf().await.map_err(|error| {
                McpError::Transport(format!("读取 stdio MCP 响应失败：{error}"))
            })?;
            if available.is_empty() {
                if frame.is_empty() {
                    return Ok(None);
                }
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return Ok(Some(frame));
            }
            (
                available.len(),
                available.iter().position(|byte| *byte == b'\n'),
            )
        };
        let consume = newline_position.map_or(available_len, |position| position + 1);
        let copy = newline_position.unwrap_or(available_len);
        if frame.len().saturating_add(copy) > limit {
            return Err(McpError::ResponseTooLarge { limit });
        }
        {
            let available = reader.fill_buf().await.map_err(|error| {
                McpError::Transport(format!("读取 stdio MCP 响应失败：{error}"))
            })?;
            frame.extend_from_slice(&available[..copy]);
        }
        reader.consume(consume);
        if newline_position.is_some() {
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
    }
}

fn fail_all(pending: &PendingMap, error: McpError) {
    let senders = {
        let mut pending = lock_unpoisoned(pending);
        pending
            .drain()
            .map(|(_, sender)| sender)
            .collect::<Vec<_>>()
    };
    for sender in senders {
        let _ = sender.send(Err(error.clone()));
    }
}

async fn write_serialized<T: Serialize>(
    writer: &Mutex<Option<ChildStdin>>,
    message: &T,
    limit: usize,
) -> Result<(), McpError> {
    let mut bytes = serde_json::to_vec(message)
        .map_err(|error| McpError::Protocol(format!("JSON-RPC 序列化失败：{error}")))?;
    if bytes.len() > limit {
        return Err(McpError::ResponseTooLarge { limit });
    }
    bytes.push(b'\n');
    let mut writer = writer.lock().await;
    let writer = writer
        .as_mut()
        .ok_or_else(|| McpError::NotReady("stdio stdin 已关闭".to_owned()))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| McpError::Transport(format!("写入 stdio MCP 消息失败：{error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| McpError::Transport(format!("刷新 stdio MCP 消息失败：{error}")))
}

fn lock_unpoisoned<T>(mutex: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn is_known_late_response(id: &RequestId, issued_numeric_high_water: i64) -> bool {
    matches!(id, RequestId::Number(value) if *value <= issued_numeric_high_water)
}

struct PendingGuard {
    id: RequestId,
    pending: PendingMap,
    armed: bool,
}

impl PendingGuard {
    fn new(id: RequestId, pending: PendingMap) -> Self {
        Self {
            id,
            pending,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            lock_unpoisoned(&self.pending).remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_known_late_response;
    use crate::RequestId;

    #[test]
    fn numeric_late_responses_remain_recognized_beyond_old_fixed_cache() {
        assert!(is_known_late_response(&RequestId::Number(1), 10_000));
        assert!(is_known_late_response(&RequestId::Number(9_999), 10_000));
        assert!(!is_known_late_response(&RequestId::Number(10_001), 10_000));
        assert!(!is_known_late_response(
            &RequestId::String("unknown".to_owned()),
            10_000
        ));
    }
}
