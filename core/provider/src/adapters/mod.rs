mod chat_completions;
mod messages;
mod responses;
mod wire;

use std::collections::VecDeque;

use keencode_model::{ModelError, ModelRequest, ModelStreamEvent, ProviderProtocol};
use serde_json::Value;

use crate::sse::SseFrame;

pub(crate) use chat_completions::ChatCompletionsAdapter;
pub(crate) use messages::MessagesAdapter;
pub(crate) use responses::ResponsesAdapter;

/// 三种协议 Adapter 的内部统一分派器。
pub(crate) enum Adapter {
    /// Anthropic Messages 线格式。
    Messages(MessagesAdapter),
    /// OpenAI Chat Completions 线格式。
    ChatCompletions(ChatCompletionsAdapter),
    /// OpenAI Responses 线格式。
    Responses(ResponsesAdapter),
}

impl Adapter {
    /// 网关参数策略由 Chat Adapter 消费，其他协议维持各自标准字段。
    pub fn configure_chat_output_tokens(&mut self, field: crate::config::ChatOutputTokenField) {
        if let Self::ChatCompletions(adapter) = self {
            adapter.output_token_field = field;
        }
    }
    /// 提示缓存断点策略由 Messages Adapter 消费，其他协议维持标准线格式。
    pub fn configure_prompt_caching(&mut self, enabled: bool) {
        if let Self::Messages(adapter) = self {
            adapter.configure_prompt_caching(enabled);
        }
    }
    /// 为指定协议创建没有跨请求共享状态的 Adapter。
    pub fn new(protocol: ProviderProtocol) -> Self {
        match protocol {
            ProviderProtocol::Messages => Self::Messages(MessagesAdapter::new()),
            ProviderProtocol::ChatCompletions => {
                Self::ChatCompletions(ChatCompletionsAdapter::new())
            }
            ProviderProtocol::Responses => Self::Responses(ResponsesAdapter::new()),
        }
    }

    /// 把 Provider 中立请求编码为当前协议的 JSON 正文。
    pub fn encode_request(
        &self,
        request: &ModelRequest,
        streaming: bool,
    ) -> Result<Value, ModelError> {
        match self {
            Self::Messages(adapter) => adapter.encode_request(request, streaming),
            Self::ChatCompletions(adapter) => adapter.encode_request(request, streaming),
            Self::Responses(adapter) => adapter.encode_request(request, streaming),
        }
    }

    /// 消费一条 SSE 帧并追加归一化事件。
    pub fn consume_sse(
        &mut self,
        frame: SseFrame,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        match self {
            Self::Messages(adapter) => adapter.consume_sse(frame, output),
            Self::ChatCompletions(adapter) => adapter.consume_sse(frame, output),
            Self::Responses(adapter) => adapter.consume_sse(frame, output),
        }
    }

    /// 把一个非流式 JSON 响应归一化为完整事件序列。
    pub fn decode_json(&mut self, value: Value) -> Result<Vec<ModelStreamEvent>, ModelError> {
        match self {
            Self::Messages(adapter) => adapter.decode_json(value),
            Self::ChatCompletions(adapter) => adapter.decode_json(value),
            Self::Responses(adapter) => adapter.decode_json(value),
        }
    }

    /// 在 HTTP 正文结束时校验协议级终止条件。
    pub fn finish_stream(
        &mut self,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let result = match self {
            Self::Messages(adapter) => adapter.finish_stream(),
            Self::ChatCompletions(adapter) => adapter.finish_stream(output),
            Self::Responses(adapter) => adapter.finish_stream(),
        };
        result.map_err(|error| match error {
            ModelError::Protocol { message } => ModelError::StreamInterrupted {
                message,
                retryable: true,
                // Adapter 只做事件级校验，不累积正文；部分产出由消费方
                // `collect_model_stream` 在收流结束时统一挂载。
                partial_text: None,
            },
            other => other,
        })
    }
}
