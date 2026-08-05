use std::collections::BTreeMap;

use serde_json::{json, Value};
use url::Url;

use crate::{
    ContentBlock, DocumentSource, ImageSource, ModelError, ModelMessage, ModelRequest, ModelResult,
    PreparedModelRequest, ProviderProtocol, ToolDefinition,
};

use super::ResponsesConfig;

#[derive(Clone)]
pub(super) struct BuiltResponsesRequest {
    pub(super) endpoint: Url,
    pub(super) body: Value,
    model_id: String,
}

impl BuiltResponsesRequest {
    pub(super) fn observe(
        &self,
        runtime: &crate::ModelRuntimeConfig,
    ) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe_with_runtime(
            ProviderProtocol::OpenAiCompatible,
            self.model_id.clone(),
            self.endpoint.clone(),
            self.body.clone(),
            BTreeMap::new(),
            runtime,
        )
    }
}

pub(super) fn build_request(
    config: &ResponsesConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltResponsesRequest> {
    let mut body = json!({
        "model": config.model,
        "input": messages_to_input(&request.messages),
        // 中转网关只接受 `stream: true` 的 Responses 请求（非流式直接拒绝），
        // 因此流式标志必须常驻请求体。
        //
        // 注意：部分中转网关对 `max_output_tokens` 字段转发失败（存在即 400
        // Upstream request failed），因此不发送该字段，输出上限由服务端决定。
        "stream": true,
    });

    if let Some(system) =
        extract_system_message(&request.messages).filter(|content| !content.trim().is_empty())
    {
        body["instructions"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(tool_to_responses).collect());
        body["tool_choice"] = json!("auto");
    }
    if let Some(reasoning_effort) = &config.reasoning_effort {
        body["reasoning"] = json!({"effort": reasoning_effort});
    } else if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(session_id) = &request.session_id {
        body["metadata"] = json!({"session_id": session_id});
    }

    Ok(BuiltResponsesRequest {
        endpoint: responses_endpoint(&config.endpoint)?,
        body,
        model_id: config.model.clone(),
    })
}

pub(super) fn responses_endpoint(endpoint: &Url) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(ModelError::protocol(
            crate::ProtocolErrorKind::InvalidEndpoint,
        ));
    }

    let mut endpoint = endpoint.clone();
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let last_segment = endpoint
        .path_segments()
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
        .map(str::to_owned);
    let mut path_segments = endpoint
        .path_segments_mut()
        .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::InvalidEndpoint))?;
    path_segments.pop_if_empty();
    if last_segment.as_deref() != Some("responses") {
        if last_segment.as_deref() != Some("v1") {
            path_segments.push("v1");
        }
        path_segments.push("responses");
    }
    drop(path_segments);
    Ok(endpoint)
}

fn extract_system_message(messages: &[ModelMessage]) -> Option<String> {
    let parts = messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::System { content } => Some(content_text(content)),
            _ => None,
        })
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| {
        parts
            .join("\n\n")
            .replace("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__", "")
    })
}

/// 把内部消息序列转换为 Responses `input` items。
fn messages_to_input(messages: &[ModelMessage]) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        match message {
            ModelMessage::System { .. } => {}
            ModelMessage::User { content } => {
                input.push(json!({
                    "role": "user",
                    "content": content_to_responses(content),
                }));
            }
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => {
                let text = content_text(content);
                if !text.trim().is_empty() {
                    input.push(json!({
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                for tool_call in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tool_call.id(),
                        "name": tool_call.name(),
                        "arguments": serde_json::to_string(tool_call.arguments().as_map())
                            .expect("JsonObject always serializes"),
                    }));
                }
            }
            ModelMessage::ToolResult { result } => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": result.tool_call_id,
                    "output": content_text(&result.content),
                }));
            }
        }
    }
    input
}

fn tool_to_responses(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema.as_map(),
        "strict": false,
    })
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::text_content)
        .collect()
}

fn content_to_responses(content: &[ContentBlock]) -> Value {
    if let [ContentBlock::Text { text }] = content {
        return json!(text);
    }
    let parts = content
        .iter()
        .filter_map(block_to_responses_part)
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [] => json!(""),
        [Value::String(text)] => json!(text),
        _ => Value::Array(parts),
    }
}

fn block_to_responses_part(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({"type": "input_text", "text": text})),
        ContentBlock::Image { source } => {
            let image_url = match source {
                ImageSource::Url { url } => json!({ "url": url }),
                ImageSource::Base64 { media_type, data } => {
                    json!({ "url": format!("data:{};base64,{data}", media_type.as_str()) })
                }
            };
            Some(json!({"type": "input_image", "image_url": image_url}))
        }
        ContentBlock::Document { source, title } => {
            let source = match source {
                DocumentSource::Url { url } => json!({ "type": "url", "url": url }),
                DocumentSource::Text { text } => json!({ "type": "text", "text": text }),
                DocumentSource::Base64 { media_type, data } => json!({
                    "type": "base64",
                    "media_type": media_type.as_str(),
                    "data": data,
                }),
            };
            Some(json!({"type": "input_file", "source": source, "filename": title}))
        }
        ContentBlock::Reasoning { .. }
        | ContentBlock::RedactedReasoning { .. }
        | ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. } => None,
    }
}

#[cfg(test)]
pub(super) fn body_for_test(config: &ResponsesConfig, request: &ModelRequest) -> Value {
    build_request(config, request)
        .expect("test config is valid")
        .body
}
