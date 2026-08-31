use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::{
    runtime::stream::SseDecoderFactory, transport::SseEvent, ContentBlock, JsonObject,
    ModelMessage, ModelResponse, ModelResult, ModelStreamEvent, StopReason, TokenUsage, ToolCall,
};

use super::response::provider_protocol_error;

#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Default)]
struct StreamState {
    text: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    usage: Option<TokenUsage>,
    request_id: Option<String>,
    failed: Option<String>,
    next_tool_index: usize,
}

pub(super) fn decoders() -> SseDecoderFactory {
    Arc::new(|| {
        let state = Arc::new(Mutex::new(StreamState::default()));
        let decoder = {
            let state = Arc::clone(&state);
            Arc::new(move |event, _header_request_id: Option<String>| decode_event(&state, event))
        };
        let completion_decoder = Arc::new(move || complete_stream(&state));
        (decoder, completion_decoder)
    })
}

fn decode_event(state: &Mutex<StreamState>, event: SseEvent) -> ModelResult<Vec<ModelStreamEvent>> {
    let value: Value = serde_json::from_str(&event.data).map_err(|_| provider_protocol_error())?;
    let mut state = state.lock().map_err(|_| provider_protocol_error())?;
    let mut events = Vec::new();

    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            let text = value
                .get("delta")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty());
            if let Some(text) = text {
                state.text.push_str(text);
                events.push(ModelStreamEvent::TextDelta {
                    text: text.to_owned(),
                });
            }
        }
        Some("response.reasoning_summary_part.delta") => {
            let reasoning = value
                .get("delta")
                .and_then(Value::as_str)
                .filter(|reasoning| !reasoning.is_empty());
            if let Some(reasoning) = reasoning {
                state.reasoning.push_str(reasoning);
                events.push(ModelStreamEvent::ReasoningDelta {
                    text: reasoning.to_owned(),
                });
            }
        }
        Some("response.output_item.done") => {
            let item = &value["item"];
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let index = state.next_tool_index;
                state.next_tool_index += 1;
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let name = item.get("name").and_then(Value::as_str).map(str::to_owned);
                // Responses 协议在 output_item.done 时给出完整参数 JSON。
                let arguments_delta = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let accumulator = state.tool_calls.entry(index).or_default();
                if let Some(id) = &id {
                    accumulator.id = Some(id.clone());
                }
                if let Some(name) = &name {
                    accumulator.name = Some(name.clone());
                }
                accumulator.arguments.push_str(&arguments_delta);
                events.push(ModelStreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                });
            }
        }
        Some("response.completed") => {
            if let Some(response) = value.get("response") {
                if state.request_id.is_none() {
                    state.request_id = response
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                // Responses 的 usage 同时携带输入、输出和缓存明细。
                if let Some(usage) = response.get("usage").and_then(decode_usage) {
                    state.usage = Some(usage.clone());
                    events.push(ModelStreamEvent::Usage(usage));
                }
                // Responses 协议没有 [DONE] 结束标记，以 completed 事件收尾；
                // 与 Anthropic 的 message_stop 同模式，在此直接发出 Completed，
                // 避免流 EOF 被 retry 层误判为 stream interrupted。
                events.push(ModelStreamEvent::Completed(completed_response(&state)?));
            }
        }
        Some("response.failed") => {
            // 官方协议中 failed 是终态错误事件，流随后结束；
            // 立即返回错误，避免把错误误当成正常 EOF。
            let message = value["response"]["error"]["message"]
                .as_str()
                .unwrap_or("上游响应失败")
                .to_owned();
            return Err(crate::ModelError::protocol_with_summary(
                crate::ProtocolErrorKind::Provider,
                message,
            ));
        }
        _ => {}
    }
    Ok(events)
}

/// 解码 Responses usage，并把缓存读写字段归一化到跨协议 TokenUsage。
fn decode_usage(value: &Value) -> Option<TokenUsage> {
    let input_tokens = value.get("input_tokens")?.as_u64()?.try_into().ok()?;
    let output_tokens = value.get("output_tokens")?.as_u64()?.try_into().ok()?;
    let input_details = value.get("input_tokens_details");
    let cache_creation_input_tokens = input_details
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .and_then(|tokens| tokens.try_into().ok());
    let cache_read_input_tokens = input_details
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .and_then(|tokens| tokens.try_into().ok());
    let reasoning_output_tokens = value
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .and_then(|tokens| tokens.try_into().ok());
    Some(TokenUsage {
        input_tokens,
        output_tokens,
        reasoning_output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    })
}

/// 从流式累积状态构建 Completed 响应（completed 事件与 EOF 兜底共用）。
fn completed_response(state: &StreamState) -> ModelResult<ModelResponse> {
    if let Some(message) = &state.failed {
        return Err(crate::ModelError::protocol_with_summary(
            crate::ProtocolErrorKind::Provider,
            message,
        ));
    }

    let tool_calls = state
        .tool_calls
        .values()
        .map(|tool_call| {
            let id = tool_call
                .id
                .as_deref()
                .ok_or_else(provider_protocol_error)?;
            let name = tool_call
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(provider_protocol_error)?;
            let arguments: Value = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| provider_protocol_error())?;
            let arguments =
                JsonObject::from_value(arguments).map_err(|_| provider_protocol_error())?;
            Ok(ToolCall::new(id, name, arguments))
        })
        .collect::<ModelResult<Vec<_>>>()?;

    let mut content = Vec::new();
    if !state.reasoning.is_empty() {
        content.push(ContentBlock::reasoning(&state.reasoning));
    }
    if !state.text.is_empty() {
        content.push(ContentBlock::text(&state.text));
    }
    let stop_reason = if tool_calls.is_empty() {
        StopReason::EndTurn
    } else {
        StopReason::ToolUse
    };
    ModelResponse::new(
        ModelMessage::assistant(content, tool_calls),
        stop_reason,
        state.usage.clone(),
        state.request_id.clone(),
    )
}

fn complete_stream(state: &Mutex<StreamState>) -> ModelResult<Vec<ModelStreamEvent>> {
    let state = state.lock().map_err(|_| provider_protocol_error())?;
    Ok(vec![ModelStreamEvent::Completed(completed_response(
        &state,
    )?)])
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;
