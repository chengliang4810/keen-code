//! 人机交互契约（自 peri-agent 迁入；`peri-agent::interaction` 保留 re-export）。
//!
//! AskUser（问答）与 Channel 通知的跨层契约。

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ─── QuestionItem ──────────────────────────────────────────────────────────────

/// 问题选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单个问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

// ─── InteractionContext ────────────────────────────────────────────────────────

/// 人机交互上下文（描述需要用户响应的场景）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionContext {
    /// 向用户提问（原 AskUserBatchRequest）
    Questions { requests: Vec<QuestionItem> },
}

// ─── InteractionResponse ───────────────────────────────────────────────────────

/// 问题答案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub selected: Vec<String>,
    pub text: Option<String>,
}

/// 交互响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionResponse {
    Answers(Vec<QuestionAnswer>),
    /// 用户明确拒绝了交互（如在 AskUserQuestion 确认弹窗中选择拒绝）
    Rejected,
}

// ─── UserInteractionBroker ─────────────────────────────────────────────────────

/// 用户交互 broker trait。
/// 应用层（TUI / CLI / 测试）实现此 trait，通过 `request` 方法挂起等待用户响应。
///
/// # 使用示例
///
/// ```rust,ignore
/// let broker: Arc<dyn UserInteractionBroker> = Arc::new(TuiInteractionBroker::new(tx));
/// let ask_user_tool = AskUserTool::new(broker);
/// ```
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    /// 发起一次人机交互，挂起直到用户响应
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse;
}

// ─── ChannelNotificationSender ─────────────────────────────────────────────────

/// 发送 channel 通知的抽象（由 McpClientPool 在 peri-middlewares 中实现）
#[async_trait]
pub trait ChannelNotificationSender: Send + Sync {
    async fn send_notification(
        &self,
        server_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String>;
}

// ─── channel_types ─────────────────────────────────────────────────────────────

/// 一条来自外部 channel 的通知
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelNotification {
    pub source: String, // "plugin:weixin@anthropic" 或 "server:my-mcp"
    pub chat_id: String,
    pub text: String,
}

// ─── ChannelState ──────────────────────────────────────────────────────────────

/// Channel 共享状态 — 桥接 MCP handler 与 TUI/broker
///
/// 单一实例，由 ServiceRegistry 持有，为 ChannelHandler 与 `/channel` 命令
/// 提供共享的授权表和消息发送器注册表。
pub struct ChannelState {
    /// 已授权的 server → source 映射
    /// key: MCP server name，value: source 标识（如 "plugin:weixin@anthropic:weixin" 或 "server:my-mcp"）
    pub authorized: parking_lot::RwLock<HashMap<String, String>>,
    /// 各 session 的消息发送器：session_id → mpsc sender
    pub channel_msg_txs:
        parking_lot::RwLock<HashMap<String, mpsc::UnboundedSender<ChannelNotification>>>,
}

impl ChannelState {
    /// Create a new shared ChannelState instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            authorized: parking_lot::RwLock::new(HashMap::new()),
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
