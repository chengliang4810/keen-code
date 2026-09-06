//! KeenCode Provider 中立 MCP 客户端核心。
//!
//! 本 crate 固定实现 MCP `2025-11-25`，支持 stdio 与 Streamable HTTP、工具和资源
//! 调用、有界分页、取消、会话恢复，以及带 PRM/AS discovery 和 PKCE 的 OAuth
//! 状态机。它不依赖桌面层、模型 Provider 或其他 Agent Runtime。

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

mod auth;
mod client;
mod config;
mod error;
mod oauth;
mod oauth_exchange;
mod process_tree;
mod protocol;
mod transport;
mod types;

pub use auth::{AuthToken, McpAuthProvider};
pub use client::McpClient;
pub use config::{McpClientOptions, McpServerConfig, StdioServerConfig, StreamableHttpConfig};
pub use error::McpError;
pub use oauth::{
    OAuthAuthorizationRequest, OAuthAuthorizationServerMetadata, OAuthCallback, OAuthChallenge,
    OAuthConfig, OAuthError, OAuthMachine, OAuthMetadataFetcher, OAuthProtectedResourceMetadata,
    OAuthSnapshot, OAuthStatus, OAuthTokenRequest, OAuthTokenSet, ReqwestOAuthMetadataFetcher,
    authorization_server_metadata_urls, discover_oauth_config, parse_www_authenticate,
    protected_resource_metadata_urls,
};
pub use oauth_exchange::ReqwestOAuthTokenExchanger;
pub use protocol::{JsonRpcError, McpNotification, RequestId};
pub use tokio_util::sync::CancellationToken;
pub use types::{
    ClientCapabilities, ImplementationInfo, InitializeResult, McpAnnotations, McpContent, McpIcon,
    McpResource, McpResourceContent, McpResourceTemplate, McpServerSession, McpTaskSupport,
    McpTool, McpToolAnnotations, McpToolEffect, McpToolExecution, McpToolSet, ResourceCapabilities,
    ServerCapabilities, ToolCallResult, ToolCapabilities,
};

/// 当前实现唯一支持的 MCP 公开协议版本。
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
