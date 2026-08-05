use serde::{Deserialize, Serialize};

/// Channel 消息通知（MCP → Peri）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelNotification {
    pub source: String, // "plugin:weixin@anthropic" 或 "server:my-mcp"
    pub chat_id: String,
    pub text: String,
}

/// 权限请求（Peri → MCP Channel Server）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String, // short ID，用户手打 yes <id> 使用
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub source: String, // "peri"
}

/// 权限响应（MCP Channel Server → Peri）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub approved: bool,
    pub reason: String,
}

/// 生成短请求 ID（UUID v7 前 6 位 hex）
pub fn short_request_id() -> String {
    uuid::Uuid::now_v7().to_string().chars().take(6).collect()
}
