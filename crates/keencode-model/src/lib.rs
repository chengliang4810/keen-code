//! KeenCode 的 Provider 中立模型领域层。
//!
//! 本 crate 只描述 Agent Runtime 可依赖的统一消息、请求、流事件、能力与错误，
//! 不包含任何具体远端接口的请求字段或解析逻辑。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod error;
mod message;
mod provider;
mod request;
mod scripted;
mod stream;
mod structured;
mod tool;
mod usage;

pub use error::{ModelError, StructuredOutputEnforcement, StructuredOutputFailureKind};
pub use message::{
    ContentBlock, ImageContent, ImageSource, Message, MessageRole, OpaqueReasoningState,
    ReasoningContent,
};
pub use provider::{
    ModelFuture, ModelProvider, ModelStream, ProviderCapabilities, ProviderProtocol,
    ReasoningCapability, StructuredOutputCapability,
};
pub use request::{
    ModelRequest, ModelResponse, ReasoningConfig, ReasoningEffort, ResponseMetadata,
    StructuredOutputConfig, ToolChoice,
};
pub use scripted::{ScriptedProvider, ScriptedReply};
pub use stream::{ModelStreamEvent, collect_model_stream};
pub use tool::{MAX_TOOL_NAME_BYTES, ToolCall, ToolDefinition, ToolResult, ToolResultContent};
pub use usage::{StopReason, TokenUsage};

#[cfg(test)]
mod tests;
