use std::collections::HashMap;

use thiserror::Error;

use super::config::McpServerConfig;

/// 传输层配置枚举，从 McpServerConfig 派生
#[derive(Debug, Clone)]
pub enum TransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    StreamableHttp {
        url: String,
        headers: HashMap<String, String>,
        /// OAuth 配置（仅当服务器配置了 oauth 且 is_enabled() 时为 Some）
        oauth: Option<super::config::OAuthConfig>,
    },
}

/// 传输层构建错误
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("MCP 服务器配置无效: 缺少 command 或 url 字段")]
    InvalidConfig,
}

impl TryFrom<&McpServerConfig> for TransportConfig {
    type Error = TransportError;

    fn try_from(config: &McpServerConfig) -> Result<Self, Self::Error> {
        match (&config.command, &config.url) {
            (Some(command), _) => Ok(TransportConfig::Stdio {
                command: command.clone(),
                args: config.args.clone().unwrap_or_default(),
                env: config.env.clone().unwrap_or_default(),
            }),
            (_, Some(url)) => Ok(TransportConfig::StreamableHttp {
                url: url.clone(),
                headers: config.headers.clone().unwrap_or_default(),
                oauth: config.oauth.as_ref().filter(|o| o.is_enabled()).cloned(),
            }),
            (None, None) => Err(TransportError::InvalidConfig),
        }
    }
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
