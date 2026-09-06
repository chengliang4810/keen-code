use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 结构化输出约束实际由哪一层执行。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputEnforcement {
    /// Provider 声明原生接受并执行 JSON Schema。
    Native,
    /// Agent Runtime 通过保留工具调用收集结构化结果。
    ToolEmulated,
}

impl fmt::Display for StructuredOutputEnforcement {
    /// 输出适合错误消息的中文执行方式。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => formatter.write_str("Provider 原生约束"),
            Self::ToolEmulated => formatter.write_str("Runtime 工具模拟"),
        }
    }
}

/// 结构化输出在最终校验阶段失败的稳定分类。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputFailureKind {
    /// 模型正常结束但没有返回 JSON 文本或保留结果工具。
    MissingOutput,
    /// 返回文本不是唯一完整 JSON 值。
    InvalidJson,
    /// JSON 值不满足调用方提供的 Schema。
    SchemaViolation,
    /// 最终结果包含图片、普通工具调用或其他不允许内容。
    UnexpectedContent,
    /// 模型因长度、安全策略或其他原因没有正常完成结果。
    Incomplete,
    /// 工具模拟返回了混合调用、错误包装或其他协议级歧义。
    EmulationProtocol,
}

impl fmt::Display for StructuredOutputFailureKind {
    /// 输出适合错误消息的中文失败类型。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOutput => formatter.write_str("缺少输出"),
            Self::InvalidJson => formatter.write_str("JSON 无效"),
            Self::SchemaViolation => formatter.write_str("Schema 不匹配"),
            Self::UnexpectedContent => formatter.write_str("存在意外内容"),
            Self::Incomplete => formatter.write_str("输出未完成"),
            Self::EmulationProtocol => formatter.write_str("工具模拟协议无效"),
        }
    }
}

/// 统一模型层能够向上层报告的错误。
#[derive(Clone, Debug, Deserialize, Error, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelError {
    /// 认证信息缺失、无效或已过期。
    #[error("模型服务认证失败：{message}")]
    Authentication {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 凭据有效但不允许访问目标资源或模型。
    #[error("模型服务拒绝授权：{message}")]
    Authorization {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 账户余额、额度或套餐不足，不能继续调用。
    #[error("模型服务额度不足：{message}")]
    QuotaExceeded {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 目标模型不存在或不能在当前端点使用。
    #[error("模型不可用：{message}")]
    ModelNotFound {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 目标 HTTP 资源不支持当前协议。
    #[error("模型协议不受支持：{message}")]
    ProtocolUnsupported {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 调用受到速率或额度限制。
    #[error("模型服务限制了请求：{message}")]
    RateLimited {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 建议等待的毫秒数；远端未报告时为 `None`。
        retry_after_ms: Option<u64>,
        /// 远端返回的 HTTP 状态码；本地拒绝时为 `None`。
        status_code: Option<u16>,
    },
    /// 输入上下文超过模型可接收的上限。
    #[error("模型上下文超过限制：{message}")]
    ContextLengthExceeded {
        /// 可安全展示给调用方的错误说明。
        message: String,
    },
    /// 统一请求本身不满足模型层约束。
    #[error("模型请求无效：{message}")]
    InvalidRequest {
        /// 指出无效字段或不变量的错误说明。
        message: String,
    },
    /// 当前模型端点不支持调用方要求的能力。
    #[error("模型能力不受支持（{capability}）：{message}")]
    UnsupportedCapability {
        /// 稳定、Provider 中立的能力名称。
        capability: String,
        /// 可安全展示给调用方的错误说明。
        message: String,
    },
    /// Provider 或工具模拟返回的最终值不满足结构化输出契约。
    #[error("结构化输出失败（{enforcement}，{failure}）：{message}")]
    StructuredOutput {
        /// 端点原生执行还是 Runtime 使用工具模拟执行约束。
        enforcement: StructuredOutputEnforcement,
        /// 缺失、JSON 解析、Schema 或协议阶段的稳定失败分类。
        failure: StructuredOutputFailureKind,
        /// 不包含完整模型输出或凭据的安全失败说明。
        message: String,
    },
    /// 模型服务暂时不可用。
    #[error("模型服务不可用：{message}")]
    ProviderUnavailable {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 远端返回的 HTTP 状态码；连接前失败时为 `None`。
        status_code: Option<u16>,
        /// 是否适合由上层按退避策略重试。
        retryable: bool,
    },
    /// 网络、超时或底层连接发生错误。
    #[error("模型传输失败：{message}")]
    Transport {
        /// 可安全展示给调用方的错误说明。
        message: String,
        /// 是否适合由上层按退避策略重试。
        retryable: bool,
    },
    /// HTTP 成功后响应流在协议终止事件之前被截断。
    #[error("模型响应流中断：{message}")]
    StreamInterrupted {
        /// 不包含响应正文的稳定错误说明。
        message: String,
        /// 是否适合由上层按退避策略重新请求。
        retryable: bool,
    },
    /// 远端响应无法转换为统一事件。
    #[error("模型响应协议错误：{message}")]
    Protocol {
        /// 指出事件顺序、字段或内容问题的错误说明。
        message: String,
    },
    /// 调用被用户或上层运行时取消。
    #[error("模型调用已取消：{message}")]
    Cancelled {
        /// 可安全展示给调用方的取消原因。
        message: String,
    },
}

impl ModelError {
    /// 返回错误是否适合由上层自动重试。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::ProviderUnavailable { retryable, .. }
            | Self::Transport { retryable, .. }
            | Self::StreamInterrupted { retryable, .. } => *retryable,
            Self::Authentication { .. }
            | Self::Authorization { .. }
            | Self::QuotaExceeded { .. }
            | Self::ModelNotFound { .. }
            | Self::ProtocolUnsupported { .. }
            | Self::ContextLengthExceeded { .. }
            | Self::InvalidRequest { .. }
            | Self::UnsupportedCapability { .. }
            | Self::StructuredOutput { .. }
            | Self::Protocol { .. }
            | Self::Cancelled { .. } => false,
        }
    }

    /// 返回不包含认证信息的可展示错误说明。
    pub fn message(&self) -> &str {
        match self {
            Self::Authentication { message, .. }
            | Self::Authorization { message, .. }
            | Self::QuotaExceeded { message, .. }
            | Self::ModelNotFound { message, .. }
            | Self::ProtocolUnsupported { message, .. }
            | Self::RateLimited { message, .. }
            | Self::ContextLengthExceeded { message }
            | Self::InvalidRequest { message }
            | Self::UnsupportedCapability { message, .. }
            | Self::StructuredOutput { message, .. }
            | Self::ProviderUnavailable { message, .. }
            | Self::Transport { message, .. }
            | Self::StreamInterrupted { message, .. }
            | Self::Protocol { message }
            | Self::Cancelled { message } => message,
        }
    }
}
