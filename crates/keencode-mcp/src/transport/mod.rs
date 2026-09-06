//! MCP stdio 与 Streamable HTTP 传输。

mod http;
mod stdio;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::{McpClientOptions, McpServerConfig};
use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, McpNotification};

#[async_trait]
pub(crate) trait McpTransport: Send + Sync {
    async fn request(&self, request: JsonRpcRequest) -> Result<Value, McpError>;

    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), McpError>;

    fn subscribe(&self) -> broadcast::Receiver<McpNotification>;

    async fn start_listening(&self) -> Result<(), McpError> {
        Ok(())
    }

    async fn restart_listening(&self) -> Result<(), McpError> {
        Ok(())
    }

    async fn close(&self) -> Result<(), McpError>;

    fn force_close(&self);
}

pub(crate) async fn connect_transport(
    config: McpServerConfig,
    options: &McpClientOptions,
) -> Result<Arc<dyn McpTransport>, McpError> {
    match config {
        McpServerConfig::Stdio(config) => Ok(Arc::new(
            stdio::StdioTransport::connect(config, options).await?,
        )),
        McpServerConfig::StreamableHttp(config) => Ok(Arc::new(
            http::StreamableHttpTransport::connect(config, options)?,
        )),
    }
}
