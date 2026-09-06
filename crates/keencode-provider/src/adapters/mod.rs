mod chat_completions;
mod messages;
mod responses;

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
            },
            other => other,
        })
    }
}
