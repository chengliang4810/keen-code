//! 层边界错误契约（§9 错误模型：边界类型化，层内 anyhow）。
//!
//! `AgentError` 为 Agent 层边界错误枚举（终止类语义：Interrupted 等防 `?`
//! 误报失败），事实源归契约层；`peri-agent::error` 保留 re-export。

/// Agent 层边界错误
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Max iterations exceeded ({0})")]
    MaxIterationsExceeded(usize),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool execution failed: {tool} - {reason}")]
    ToolExecutionFailed { tool: String, reason: String },

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("LLM HTTP 错误 ({status}): {message}")]
    LlmHttpError {
        /// HTTP 状态码。
        status: u16,
        /// 仅用于日志的安全技术说明。
        message: String,
        /// 经过模型层过滤、可直接呈现给用户的供应商错误说明。
        user_message: Option<String>,
    },

    #[error("Middleware error: {middleware} - {reason}")]
    MiddlewareError { middleware: String, reason: String },

    #[error("Tool rejected: {tool} - {reason}")]
    ToolRejected { tool: String, reason: String },

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// 用户主动中断（Ctrl+C）
    #[error("Interrupted by user")]
    Interrupted,

    #[error("Full Compact requires LLM instance")]
    CompactNoLlm,

    #[error("Full Compact failed: LLM returned empty summary")]
    CompactEmptyResponse,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AgentResult<T> = Result<T, AgentError>;

impl AgentError {
    /// 返回跨协议稳定的错误码；界面必须按当前语言渲染，不得直接展示技术文本。
    pub fn user_facing_code(&self) -> &'static str {
        match self {
            Self::Other(_) => "internal_error",
            Self::LlmError(message) if message.starts_with("model stream interrupted") => {
                "model_stream_interrupted"
            }
            Self::LlmError(_) => "model_request_failed",
            Self::LlmHttpError { .. } => "model_http_error",
            Self::SerializationError(_) => "serialization_error",
            Self::MaxIterationsExceeded(_) => "max_iterations_exceeded",
            Self::ToolNotFound(_) => "tool_not_found",
            Self::ToolExecutionFailed { .. } => "tool_execution_failed",
            Self::MiddlewareError { .. } => "middleware_error",
            Self::ToolRejected { .. } => "tool_rejected",
            Self::Interrupted => "cancelled",
            Self::CompactNoLlm => "compact_unavailable",
            Self::CompactEmptyResponse => "compact_empty_response",
        }
    }

    /// 返回用户可见的错误描述（脱敏后的消息）。
    ///
    /// 内部错误返回通用说明；LLM HTTP 错误优先显示模型层过滤后的供应商说明。
    pub fn user_facing_message(&self) -> String {
        match self {
            Self::Other(_) => "An internal error occurred. Check logs for details.".to_string(),
            Self::LlmError(message) if message.starts_with("model stream interrupted") => {
                "The model response stream ended unexpectedly after output had started. Retry the request. If this keeps happening, check your network or switch the provider or model."
                    .to_string()
            }
            Self::LlmError(_) => {
                "The model request failed. Please retry; if this keeps happening, check the provider or model status."
                    .to_string()
            }
            Self::LlmHttpError {
                status,
                user_message,
                ..
            } => user_message
                .as_ref()
                .map(|message| format!("LLM HTTP error ({status}): {message}"))
                .unwrap_or_else(|| format!("LLM HTTP error ({status})")),
            Self::SerializationError(_) => {
                "A serialization error occurred. Please try again.".to_string()
            }
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
