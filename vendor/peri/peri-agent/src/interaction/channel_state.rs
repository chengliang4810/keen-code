use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex as SyncMutex;
use tokio::sync::{mpsc, oneshot};

use crate::interaction::channel_types::{ChannelNotification, PermissionResponse};

/// Channel 共享状态 — 桥接 MCP handler 与 TUI/broker
///
/// 单一实例，由 ServiceRegistry 持有，为 ChannelHandler、ChannelBroker、
/// `/channel` 命令提供共享的授权表、待审批 Map 和消息发送器注册表。
pub struct ChannelState {
    /// 已授权的 server → source 映射
    /// key: MCP server name，value: source 标识（如 "plugin:weixin@anthropic:weixin" 或 "server:my-mcp"）
    pub authorized: parking_lot::RwLock<HashMap<String, String>>,
    /// 待审批的权限请求：short_request_id → oneshot sender
    pub pending_permissions: SyncMutex<HashMap<String, oneshot::Sender<PermissionResponse>>>,
    /// 各 session 的消息发送器：session_id → mpsc sender
    pub channel_msg_txs:
        parking_lot::RwLock<HashMap<String, mpsc::UnboundedSender<ChannelNotification>>>,
}

impl ChannelState {
    /// Create a new shared ChannelState instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            authorized: parking_lot::RwLock::new(HashMap::new()),
            pending_permissions: SyncMutex::new(HashMap::new()),
            channel_msg_txs: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    /// Authorize a channel server, return the source identifier
    pub fn authorize(&self, server_name: &str, source: String) {
        self.authorized
            .write()
            .insert(server_name.to_string(), source);
    }

    /// Revoke authorization for a channel server
    pub fn revoke(&self, server_name: &str) {
        self.authorized.write().remove(server_name);
    }

    /// Close all authorized channels
    pub fn close_all(&self) {
        self.authorized.write().clear();
    }

    /// Register a session's message receiver for channel notifications
    pub fn register_session(
        &self,
        session_id: String,
        tx: mpsc::UnboundedSender<ChannelNotification>,
    ) {
        self.channel_msg_txs.write().insert(session_id, tx);
    }

    /// Unregister a session's message receiver
    pub fn unregister_session(&self, session_id: &str) {
        self.channel_msg_txs.write().remove(session_id);
    }
}
