//! MCP 动态 HTTP 认证提供方边界。

use std::fmt;

use async_trait::async_trait;

use crate::error::McpError;

/// 一次 HTTP 请求实际发送的访问令牌及其认证代次。
///
/// 令牌只用于构造请求头；Debug 输出永远不会包含令牌正文。
#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken {
    /// 要放入 `Authorization: Bearer ...` 的不透明访问令牌。
    pub token: String,
    /// 令牌存储的单调代次；刷新后必须变化。
    pub generation: u64,
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthToken")
            .field("token", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

/// 为 Streamable HTTP 提供按请求读取和按 401 触发刷新的认证边界。
///
/// 具体 OAuth 状态机、持久化和并发 single-flight 由上层 registry 实现；MCP
/// 传输层只消费令牌及代次，不保存认证秘密。
#[async_trait]
pub trait McpAuthProvider: Send + Sync {
    /// 返回当前可用访问令牌；尚未授权时返回 `None`。
    async fn access_token(&self) -> Result<Option<AuthToken>, McpError>;

    /// 处理一次未授权响应，并由提供方负责协调刷新或重新授权。
    ///
    /// `sent_generation` 是触发 401 的请求实际使用的代次；challenge 只作为
    /// OAuth 发现提示传递，不应写入日志。
    async fn on_unauthorized(
        &self,
        sent_generation: u64,
        www_authenticate: Option<&str>,
    ) -> Result<(), McpError>;
}

#[cfg(test)]
mod tests {
    use super::AuthToken;

    /// 令牌 Debug 必须只展示脱敏占位符和代次。
    #[test]
    fn auth_token_debug_redacts_secret() {
        let token = AuthToken {
            token: "secret-access-token".to_owned(),
            generation: 7,
        };
        let rendered = format!("{token:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("generation: 7"));
        assert!(!rendered.contains("secret-access-token"));
    }
}
