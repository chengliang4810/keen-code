//! 与模型提供商无关的协议 DTO 和流式优先模型接口。

pub mod anthropic;
pub mod openai_compatible;
pub mod protocol;
pub mod responses;
pub mod runtime;
mod transport;

pub use anthropic::{AnthropicConfig, AnthropicModel};
pub use openai_compatible::{OpenAiConfig, OpenAiModel};
pub use protocol::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, Model, ModelCallContext,
    ModelCapabilities, ModelMessage, ModelRequest, ModelRequestMode, ModelResponse, ModelStream,
    ModelStreamEvent, ProviderProtocol, StopReason, TokenUsage, ToolCall, ToolDefinition,
    ToolResult,
};
pub use responses::{ResponsesConfig, ResponsesModel};
pub use runtime::{
    ModelError, ModelResult, ModelRuntimeConfig, ObservedProviderBody, PreparedModelRequest,
    ProtocolError, ProtocolErrorKind, RequestErrorKind, RequestObservation,
    RequestObservationScope, RequestObservationState, RequestObserver, RetryConfig, RetryErrorKind,
    RetryObservation, RetryObserver, RetryableErrorClasses, TransportErrorKind,
};
