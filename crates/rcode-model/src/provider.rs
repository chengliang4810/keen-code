use std::{future::Future, pin::Pin};

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent, collect_model_stream};

/// 用户配置中选择的远端交互协议族。
///
/// 此枚举只用于组合根选择 Adapter，不应由 Agent Loop 用来分支业务行为。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// 使用有序内容块表达消息的协议族。
    Messages,
    /// 使用对话补全语义的协议族。
    ChatCompletions,
    /// 使用统一响应对象语义的协议族。
    Responses,
}

/// Provider 对可配置推理能力的支持级别。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningCapability {
    /// 不接受推理配置，也不保证返回推理事件。
    #[default]
    Unsupported,
    /// 可能返回推理内容，但不能按统一配置控制推理强度。
    OutputOnly,
    /// 能够解释统一推理配置并返回归一化推理事件。
    Configurable,
}

/// Provider 对结构化输出能力的支持级别。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputCapability {
    /// 不支持结构化输出。
    #[default]
    Unsupported,
    /// 通过运行时合成工具提供结构化输出。
    ToolEmulated,
    /// 端点原生接受 JSON Schema 约束。
    Native,
}

/// Agent Runtime 可查询的 Provider 能力快照。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    /// 是否支持流式接收增量事件。
    pub streaming: bool,
    /// 是否支持模型发起工具调用。
    pub tool_calling: bool,
    /// 是否允许模型在一个响应内请求多个可并发工具调用。
    pub parallel_tool_calls: bool,
    /// 对推理内容与推理配置的支持级别。
    pub reasoning: ReasoningCapability,
    /// 对结构化输出的支持级别。
    pub structured_output: StructuredOutputCapability,
    /// 是否支持远端提示缓存并能够报告相关用量。
    pub prompt_caching: bool,
    /// 是否接受图片输入。
    pub image_input: bool,
    /// 已知的最大上下文 Token；未知或由模型决定时为 `None`。
    pub max_context_tokens: Option<u64>,
    /// 已知的最大输出 Token；未知或由模型决定时为 `None`。
    pub max_output_tokens: Option<u64>,
}

/// 对象安全的异步模型调用返回值。
pub type ModelFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 统一模型增量事件流。
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Agent Runtime 调用模型的唯一 Provider 中立边界。
pub trait ModelProvider: Send + Sync {
    /// 返回指定模型在当前 Provider 实例中的能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities;

    /// 校验并发起一次流式模型调用。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>>;

    /// 发起一次模型调用并将规范化事件收集为完整响应。
    fn complete(
        &self,
        request: ModelRequest,
    ) -> ModelFuture<'_, Result<ModelResponse, ModelError>> {
        let stream = self.stream(request);
        Box::pin(async move {
            let stream = stream.await?;
            collect_model_stream(stream).await
        })
    }
}
