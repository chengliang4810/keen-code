use rmcp::model::ClientNotification;
use serde_json::Value;

use super::client::McpClientPool;

impl McpClientPool {
    /// Send a custom JSON-RPC notification to an MCP server.
    pub async fn send_custom_notification(
        &self,
        server_name: &str,
        method: &str,
        params: Value,
    ) -> Result<(), String> {
        let peer = {
            self.clients
                .read()
                .get(server_name)
                .and_then(|h| h.peer.clone())
                .ok_or_else(|| format!("server {} not connected", server_name))?
        };

        let notification = rmcp::model::CustomNotification {
            method: method.to_string(),
            params: Some(params),
            extensions: Default::default(),
        };

        peer.send_notification(ClientNotification::CustomNotification(notification))
            .await
            .map_err(|e| format!("send notification failed: {e}"))
    }
}

#[async_trait::async_trait]
impl peri_agent::interaction::ChannelNotificationSender for McpClientPool {
    async fn send_notification(
        &self,
        server_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String> {
        self.send_custom_notification(server_name, method, params)
            .await
    }
}
