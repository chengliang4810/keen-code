use thiserror::Error;

#[derive(Error, Debug)]
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
    /// 返回用户可见的错误描述（脱敏后的消息）
    /// 对内部错误返回通用描述；LLM HTTP 错误优先显示经过过滤的供应商说明。
    pub fn user_facing_message(&self) -> String {
        match self {
            Self::Other(_) => "An internal error occurred. Check logs for details.".to_string(),
            Self::LlmError(_) => "LLM call failed. Check logs for details.".to_string(),
            Self::LlmHttpError {
                status,
                user_message,
                ..
            } => user_message
                .as_ref()
                .map(|message| format!("LLM HTTP error ({status}): {message}"))
                .unwrap_or_else(|| format!("LLM HTTP error ({status})")),
            Self::SerializationError(_) => {
                "Serialization error. Check logs for details.".to_string()
            }
            other => other.to_string(),
        }
    }
}
