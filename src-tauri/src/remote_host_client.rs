//! Desktop 作为 Host Client 时的远程 ACP 桥接。
//!
//! 该模块只负责把公开的 [`keencode_cli::HostClient`] 接到 Tauri 事件和命令
//! 边界。它不创建 Agent Runtime、Session Store 或 Provider；当数据根已经由
//! headless Host 持有时，Desktop 只通过本地 IPC 复用远端 Host 的权威状态。

use crate::acp_host::{AcpHostBridge, AcpHostBridgeFuture};
use keencode_acp::ConnectionId;
use keencode_cli::{ClientError, HostClient, HostClientConfig, HostEvent};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{RwLock, broadcast, watch};

/// Desktop 前端消费 ACP 标准投递的唯一 Tauri 事件名称。
pub const ACP_DELIVERY_EVENT: &str = "acp://delivery";
/// 远端 Host 发起的 ACP Client Request 方法。
const ELICITATION_CREATE_METHOD: &str = "elicitation/create";

/// 远程 Host Client 桥接的安全错误摘要。
#[derive(Debug)]
pub struct RemoteHostClientError(String);

impl RemoteHostClientError {
    fn client(error: ClientError) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for RemoteHostClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RemoteHostClientError {}

/// Remote Host Client 的 Tauri 桥接状态。
///
/// 装配层应将该结构放进 `Arc`，在 `acp_dispatch` 命令中调用 [`Self::dispatch`]。
/// 请求只短暂读取并 clone `HostClient`，不会因为一个长 Prompt 阻塞并发 cancel
/// 或 Elicitation response；请求 ID、远端 Client Request response 和 delivery
/// 水位均由 HostClient/远端 Host 保持。
pub struct RemoteHostClient {
    app: AppHandle,
    client: RwLock<HostClient>,
    event_pump_stop: Mutex<watch::Sender<bool>>,
    event_sinks: Arc<Mutex<BTreeMap<String, broadcast::Sender<Value>>>>,
    shutdown: AtomicBool,
}

impl RemoteHostClient {
    /// 连接数据根中已经发布的 Host discovery，并启动 Tauri delivery 事件泵。
    pub async fn connect(
        app: AppHandle,
        config: HostClientConfig,
    ) -> Result<Self, RemoteHostClientError> {
        let client = HostClient::connect(config)
            .await
            .map_err(RemoteHostClientError::client)?;
        let (event_pump_stop, stop_receiver) = watch::channel(false);
        let event_sinks = Arc::new(Mutex::new(BTreeMap::new()));
        spawn_event_pump(
            app.clone(),
            client.clone(),
            client.subscribe(),
            stop_receiver,
            Arc::clone(&event_sinks),
        );
        Ok(Self {
            app,
            client: RwLock::new(client),
            event_pump_stop: Mutex::new(event_pump_stop),
            event_sinks,
            shutdown: AtomicBool::new(false),
        })
    }

    async fn dispatch_for_connection(
        &self,
        connection_id: Option<&ConnectionId>,
        message: Value,
    ) -> Result<Option<Value>, RemoteHostClientError> {
        let client = self.client.read().await.clone();
        let long_running = message
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(is_long_running_method);
        if long_running
            && let (Some(connection_id), Some(session_id)) = (
                connection_id,
                message.pointer("/params/sessionId").and_then(Value::as_str),
            )
            && let Some(web_host) = self.app.try_state::<Arc<crate::web_host::WebHostManager>>()
        {
            web_host
                .bind_connection_session(connection_id, session_id)
                .map_err(|error| RemoteHostClientError(error.to_string()))?;
        }
        let result = if long_running {
            client.dispatch_message_without_timeout(message).await
        } else {
            client.dispatch_message(message).await
        };
        result.map_err(RemoteHostClientError::client)
    }

    /// 请求事件泵停止；不发送 ACP cancel，也不改变远端 Host 生命周期。
    pub fn shutdown(&self) {
        self.stop_event_pump();
        // 退出时同时释放本地 IPC 写端，促使既有 Host 仅执行连接 detach；
        // HostClient::disconnect 不会发送 session/cancel 或 owner shutdown。
        if let Ok(client) = self.client.try_read() {
            client.disconnect();
        }
    }

    fn stop_event_pump(&self) {
        if !self.shutdown.swap(true, Ordering::AcqRel)
            && let Ok(stop_sender) = self.event_pump_stop.lock()
        {
            let _ = stop_sender.send(true);
        }
    }
}

impl Drop for RemoteHostClient {
    fn drop(&mut self) {
        self.stop_event_pump();
    }
}

/// 判断是否必须绕过 Client 层固定超时的 ACP 方法。
fn is_long_running_method(method: &str) -> bool {
    matches!(method, "session/prompt")
}

/// 将 HostEvent 转换为前端唯一 `acp://delivery` 事件载荷。
///
/// 标准 delivery 已经是 Tauri 事件所需的 `{type,envelope}`；Elicitation 仍是
/// JSON-RPC Client Request，因此包装为现有前端严格解析的 `client_request` 形状。
fn delivery_payload(event: &HostEvent) -> Option<Value> {
    match event.method.as_str() {
        ACP_DELIVERY_EVENT => Some(event.params.clone()),
        ELICITATION_CREATE_METHOD => Some(json!({
            "type": "client_request",
            "request": event.to_json_rpc(),
        })),
        _ => None,
    }
}

fn spawn_event_pump(
    app: AppHandle,
    client: HostClient,
    mut events: tokio::sync::broadcast::Receiver<HostEvent>,
    mut stop: watch::Receiver<bool>,
    event_sinks: Arc<Mutex<BTreeMap<String, broadcast::Sender<Value>>>>,
) {
    tauri::async_runtime::spawn(async move {
        let disconnected = client.wait_for_disconnect();
        tokio::pin!(disconnected);
        loop {
            tokio::select! {
                reason = &mut disconnected => {
                    tracing::warn!(error = %reason, "Remote Host connection closed; delivery pump stopped");
                    break;
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                event = events.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "Remote Host delivery subscriber lagged; UI should reload Session history");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let Some(payload) = delivery_payload(&event) else {
                        tracing::debug!(method = %event.method, "Ignoring unsupported Remote Host notification");
                        continue;
                    };
                    if let Err(error) = app.emit(ACP_DELIVERY_EVENT, payload.clone()) {
                        tracing::debug!(%error, "Remote Host delivery event emit failed");
                        break;
                    }
                    if let Some(web_host) = app.try_state::<Arc<crate::web_host::WebHostManager>>()
                        && let Err(error) = web_host.publish_remote_delivery(&payload)
                    {
                        tracing::debug!(%error, "Remote Host Web delivery fanout skipped");
                    }
                    let frame = event.to_json_rpc();
                    if let Ok(sinks) = event_sinks.lock() {
                        for sender in sinks.values() {
                            let _ = sender.send(frame.clone());
                        }
                    }
                }
            }
        }
    });
}

impl AcpHostBridge for RemoteHostClient {
    fn dispatch<'a>(
        &'a self,
        connection_id: &'a ConnectionId,
        message: Value,
    ) -> AcpHostBridgeFuture<'a> {
        Box::pin(async move {
            self.dispatch_for_connection(Some(connection_id), message)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn subscribe(&self, connection_id: &ConnectionId) -> Option<broadcast::Receiver<Value>> {
        let mut sinks = self.event_sinks.lock().ok()?;
        Some(
            sinks
                .entry(connection_id.as_str().to_owned())
                .or_insert_with(|| broadcast::channel(256).0)
                .subscribe(),
        )
    }

    fn disconnect(&self, connection_id: &ConnectionId) {
        if let Ok(mut sinks) = self.event_sinks.lock() {
            sinks.remove(connection_id.as_str());
        }
    }
}

/// 仅用于测试/装配前检查：把 Path 规范化为 HostClient 配置。
pub fn config_for_root(data_root: &Path) -> HostClientConfig {
    HostClientConfig::new(data_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_cli::HostClient;

    #[test]
    fn long_running_method_only_covers_prompt() {
        assert!(is_long_running_method("session/prompt"));
        assert!(!is_long_running_method("session/load"));
        assert!(!is_long_running_method("session/cancel"));
    }

    #[test]
    fn delivery_and_elicitation_keep_wire_shapes() {
        let delivery = HostEvent {
            method: ACP_DELIVERY_EVENT.to_owned(),
            id: None,
            request_id: None,
            params: json!({"type":"session_update","envelope":{"deliverySequence":3}}),
        };
        assert_eq!(
            delivery_payload(&delivery).unwrap()["type"],
            "session_update"
        );

        let elicitation = HostEvent {
            method: ELICITATION_CREATE_METHOD.to_owned(),
            id: Some("9".to_owned()),
            request_id: Some(json!(9)),
            params: json!({"message":"继续吗?"}),
        };
        let payload = delivery_payload(&elicitation).unwrap();
        assert_eq!(payload["type"], "client_request");
        assert_eq!(payload["request"]["id"], 9);
        assert_eq!(payload["request"]["method"], ELICITATION_CREATE_METHOD);
    }

    #[test]
    fn config_uses_the_given_data_root_without_creating_runtime() {
        let path = Path::new("C:/isolated-keencode-root");
        let config = config_for_root(path);
        assert_eq!(config.data_root, path);
        let _: fn(HostClient) = |client| drop(client);
    }
}
