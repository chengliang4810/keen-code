//! MCP 客户端统一错误。

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::oauth::OAuthError;

/// MCP 客户端在配置、传输、协议与调用阶段产生的统一错误。
#[derive(Clone, PartialEq)]
pub enum McpError {
    /// 服务端配置无效。
    Configuration(String),
    /// 底层传输失败。
    Transport(String),
    /// 收到不符合 JSON-RPC 或 MCP 约定的数据。
    Protocol(String),
    /// MCP 服务端返回 JSON-RPC 错误。
    Rpc {
        /// JSON-RPC 错误码。
        code: i64,
        /// 服务端提供的错误消息。
        message: String,
        /// 服务端提供的可选错误数据。
        data: Option<Value>,
    },
    /// 请求超过配置的等待时间。
    Timeout {
        /// 超时请求的方法名。
        method: String,
        /// 实际采用的超时时间。
        duration: Duration,
    },
    /// 请求被调用方取消。
    Cancelled {
        /// 被取消请求的方法名。
        method: String,
        /// 可选的取消原因。
        reason: Option<String>,
    },
    /// 传输返回的数据超过配置上限。
    ResponseTooLarge {
        /// 允许的最大字节数。
        limit: usize,
    },
    /// Stateful HTTP 会话已失效，调用方需要重新初始化后重试。
    SessionExpired,
    /// 分页响应无法安全地继续读取。
    Pagination {
        /// 发生问题的 MCP 方法名。
        method: String,
        /// 分页保护触发的原因。
        message: String,
    },
    /// 客户端已经关闭或尚未完成初始化。
    NotReady(String),
    /// OAuth 本地状态机失败。
    OAuth(OAuthError),
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => {
                write!(formatter, "MCP 配置错误：")?;
                write_untrusted(formatter, message)
            }
            Self::Transport(message) => {
                write!(formatter, "MCP 传输错误：")?;
                write_untrusted(formatter, message)
            }
            Self::Protocol(message) => {
                write!(formatter, "MCP 协议错误：")?;
                write_untrusted(formatter, message)
            }
            Self::Rpc {
                code,
                message: _,
                data: _,
            } => write!(formatter, "MCP RPC 错误 {code}：服务端返回错误"),
            Self::Timeout { method, duration } => {
                write!(formatter, "MCP 请求 ")?;
                write_untrusted(formatter, method)?;
                write!(formatter, " 在 {duration:?} 后超时")
            }
            Self::Cancelled { method, reason: _ } => {
                write!(formatter, "MCP 请求 ")?;
                write_untrusted(formatter, method)?;
                write!(formatter, " 已取消")
            }
            Self::ResponseTooLarge { limit } => {
                write!(formatter, "MCP 响应超过 {limit} 字节上限")
            }
            Self::SessionExpired => write!(formatter, "MCP HTTP 会话已经失效"),
            Self::Pagination { method, message } => {
                write!(formatter, "MCP 方法 ")?;
                write_untrusted(formatter, method)?;
                write!(formatter, " 的分页响应无效：")?;
                write_untrusted(formatter, message)
            }
            Self::NotReady(message) => {
                write!(formatter, "MCP 客户端不可用：")?;
                write_untrusted(formatter, message)
            }
            Self::OAuth(error) => write!(formatter, "MCP OAuth 错误：{error}"),
        }
    }
}

impl fmt::Debug for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

pub(crate) fn write_untrusted(formatter: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    formatter.write_str(&sanitize_untrusted(value))
}

/// 把外部文本压缩为单行且有界的用户可见内容，避免日志控制字符注入与无界输出。
pub(crate) fn sanitize_untrusted(value: &str) -> String {
    const MAX_CHARACTERS: usize = 512;

    let mut characters = value.chars();
    let mut sanitized = String::with_capacity(value.len().min(MAX_CHARACTERS));
    for character in characters.by_ref().take(MAX_CHARACTERS) {
        if character.is_control() {
            sanitized.push('\u{fffd}');
        } else {
            sanitized.push(character);
        }
    }
    if characters.next().is_some() {
        sanitized.push('…');
    }
    sanitized
}

impl std::error::Error for McpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OAuth(error) => Some(error),
            _ => None,
        }
    }
}

impl From<OAuthError> for McpError {
    fn from(value: OAuthError) -> Self {
        Self::OAuth(value)
    }
}
