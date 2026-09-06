use std::collections::{BTreeMap, VecDeque};

use keencode_model::{
    ContentBlock, ImageSource, MessageRole, ModelError, ModelRequest, ModelStreamEvent,
    OpaqueReasoningState, ReasoningEffort, ResponseMetadata, StopReason, TokenUsage, ToolChoice,
    ToolResultContent,
};
use serde_json::{Map, Value, json};

use crate::{http::classify_in_band_provider_error, sse::SseFrame};

const SIGNATURE_STATE_KIND: &str = "messages-thinking-signature-v1";
const REDACTED_STATE_KIND: &str = "messages-redacted-thinking-v1";

/// Anthropic Messages 流中已经打开、尚未收到 `content_block_stop` 的内容类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveContentBlock {
    /// 普通文本内容块。
    Text,
    /// 可展示推理内容块。
    Thinking,
    /// 经过 Provider 加密或隐藏的推理内容块。
    RedactedThinking,
    /// 模型发起的工具调用内容块。
    ToolUse,
}

/// Anthropic Messages 请求、JSON 响应和 SSE 事件的协议 Adapter。
pub(crate) struct MessagesAdapter {
    started: bool,
    ended: bool,
    stop_reason: Option<StopReason>,
    /// 按远端内容块序号记录尚未结束的内容，防止缺失或重复 stop 被静默接受。
    active_blocks: BTreeMap<u32, ActiveContentBlock>,
    tool_calls: BTreeMap<u32, String>,
    thinking_signatures: BTreeMap<u32, String>,
    /// 本次 SSE 响应是否打开过普通文本内容块。
    saw_text_block: bool,
    /// 本次 SSE 响应是否已经产生至少一个有意义的内容事件。
    saw_meaningful_content: bool,
}

impl MessagesAdapter {
    /// 创建一次请求专用且没有残留流状态的 Adapter。
    pub fn new() -> Self {
        Self {
            started: false,
            ended: false,
            stop_reason: None,
            active_blocks: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            thinking_signatures: BTreeMap::new(),
            saw_text_block: false,
            saw_meaningful_content: false,
        }
    }

    /// 把 Provider 中立请求编码为 Messages API 请求正文。
    pub fn encode_request(
        &self,
        request: &ModelRequest,
        streaming: bool,
    ) -> Result<Value, ModelError> {
        request.validate()?;
        let mut system = Vec::new();
        let mut messages = Vec::<Value>::new();

        for message in &request.messages {
            match message.role {
                MessageRole::System | MessageRole::Developer => {
                    for block in &message.content {
                        let ContentBlock::Text { text } = block else {
                            return Err(invalid_request("Messages 的系统和开发消息只允许文本内容"));
                        };
                        system.push(json!({ "type": "text", "text": text }));
                    }
                }
                MessageRole::User => {
                    let content = message
                        .content
                        .iter()
                        .map(encode_user_block)
                        .collect::<Result<Vec<_>, _>>()?;
                    append_message(&mut messages, "user", content);
                }
                MessageRole::Assistant => {
                    let mut content = Vec::new();
                    for block in &message.content {
                        if let Some(encoded) = encode_assistant_block(block)? {
                            content.push(encoded);
                        }
                    }
                    if !content.is_empty() {
                        append_message(&mut messages, "assistant", content);
                    }
                }
                MessageRole::Tool => {
                    let content = message
                        .content
                        .iter()
                        .map(encode_tool_result_block)
                        .collect::<Result<Vec<_>, _>>()?;
                    append_message(&mut messages, "user", content);
                }
            }
        }

        if messages.is_empty() {
            return Err(invalid_request(
                "Messages 请求至少需要一条用户、工具或 assistant 消息",
            ));
        }

        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(request.model.clone()));
        body.insert("messages".to_owned(), Value::Array(messages));
        body.insert("stream".to_owned(), Value::Bool(streaming));
        body.insert(
            "max_tokens".to_owned(),
            Value::from(request.max_output_tokens.unwrap_or(4096)),
        );
        if !system.is_empty() {
            body.insert("system".to_owned(), Value::Array(system));
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_owned(),
                Value::Array(
                    request
                        .tools
                        .iter()
                        .map(|tool| {
                            json!({
                                "name": tool.name,
                                "description": tool.description,
                                "input_schema": tool.input_schema,
                            })
                        })
                        .collect(),
                ),
            );
            body.insert(
                "tool_choice".to_owned(),
                encode_tool_choice(&request.tool_choice, request.parallel_tool_calls),
            );
        } else if !matches!(request.tool_choice, ToolChoice::Auto | ToolChoice::None) {
            return Err(invalid_request("Messages 工具选择要求非空工具列表"));
        }
        if let Some(reasoning) = &request.reasoning {
            let budget = reasoning
                .max_tokens
                .unwrap_or_else(|| reasoning_budget(reasoning.effort));
            let max_tokens = request
                .max_output_tokens
                .unwrap_or_else(|| budget.saturating_add(4096));
            if budget >= max_tokens {
                return Err(invalid_request(
                    "Messages 推理 Token 预算必须小于最大输出 Token",
                ));
            }
            body.insert(
                "thinking".to_owned(),
                json!({ "type": "enabled", "budget_tokens": budget }),
            );
        }
        if let Some(structured) = &request.structured_output {
            body.insert(
                "output_config".to_owned(),
                json!({
                    "format": {
                        "type": "json_schema",
                        "schema": structured.schema,
                    }
                }),
            );
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_owned(), Value::from(temperature));
        }
        if !request.stop_sequences.is_empty() {
            body.insert(
                "stop_sequences".to_owned(),
                Value::Array(
                    request
                        .stop_sequences
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        Ok(Value::Object(body))
    }

    /// 消费一条 Messages SSE 帧并追加 Provider 中立事件。
    pub fn consume_sse(
        &mut self,
        frame: SseFrame,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.ended {
            return Err(protocol_error("Messages 响应结束后仍收到 SSE 事件"));
        }
        if frame.data.is_empty() && frame.event.as_deref() == Some("ping") {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&frame.data)
            .map_err(|error| protocol_error(format!("Messages SSE data 不是有效 JSON：{error}")))?;
        let event_type = match frame
            .event
            .as_deref()
            .or_else(|| value.get("type").and_then(Value::as_str))
        {
            Some(event_type) => event_type,
            None if has_explicit_provider_error(&value) => {
                return Err(classify_provider_error(&value));
            }
            None => return Err(protocol_error("Messages SSE 缺少事件类型")),
        };
        if let (Some(frame_event_type), Some(data_event_type)) = (
            frame.event.as_deref(),
            value.get("type").and_then(Value::as_str),
        ) && frame_event_type != data_event_type
        {
            return Err(protocol_error("Messages SSE event 与 data.type 不一致"));
        }

        match event_type {
            "message_start" => self.consume_message_start(&value, output),
            "content_block_start" => self.consume_content_start(&value, output),
            "content_block_delta" => self.consume_content_delta(&value, output),
            "content_block_stop" => self.consume_content_stop(&value, output),
            "message_delta" => self.consume_message_delta(&value, output),
            "message_stop" => self.consume_message_stop(output),
            "ping" => Ok(()),
            "error" => Err(classify_provider_error(&value)),
            other => Err(protocol_error(format!("Messages SSE 包含未知事件 {other}"))),
        }
    }

    /// 把一个非流式 Messages JSON 响应转换为完整事件序列。
    pub fn decode_json(&mut self, value: Value) -> Result<Vec<ModelStreamEvent>, ModelError> {
        let response = value
            .as_object()
            .ok_or_else(|| protocol_error("Messages 响应必须是 JSON 对象"))?;
        if response.get("type").and_then(Value::as_str) == Some("error")
            || response.get("error").is_some_and(|error| !error.is_null())
        {
            return Err(classify_provider_error(&value));
        }

        let metadata = ResponseMetadata {
            response_id: optional_string(response.get("id")),
            model: optional_string(response.get("model")),
        };
        metadata.validate()?;
        let mut events = vec![ModelStreamEvent::MessageStart { metadata }];
        self.started = true;

        let content = response
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_error("Messages 响应缺少 content 数组"))?;
        let mut saw_text_block = false;
        let mut saw_meaningful_content = false;
        for (position, block) in content.iter().enumerate() {
            let index = u32::try_from(position)
                .map_err(|_| protocol_error("Messages 内容块数量超过 u32 范围"))?;
            if block.get("type").and_then(Value::as_str) == Some("text") {
                saw_text_block = true;
            }
            let event_count = events.len();
            decode_complete_content(index, block, &mut events)?;
            saw_meaningful_content |= events.len() > event_count;
        }
        if saw_text_block && !saw_meaningful_content {
            return Err(protocol_error("Messages 响应不能只有空文本内容"));
        }
        if let Some(usage) = response.get("usage") {
            events.push(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        let stop_reason = map_stop_reason(response.get("stop_reason").and_then(Value::as_str));
        events.push(ModelStreamEvent::MessageEnd { stop_reason });
        self.ended = true;
        Ok(events)
    }

    /// 校验 Messages SSE 流已经通过 `message_stop` 明确结束。
    pub fn finish_stream(&mut self) -> Result<(), ModelError> {
        if self.ended {
            Ok(())
        } else {
            Err(protocol_error("Messages SSE 在 message_stop 之前关闭"))
        }
    }

    /// 处理 `message_start` 并保存响应元数据及首个 Usage 快照。
    fn consume_message_start(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.started {
            return Err(protocol_error("Messages SSE 重复 message_start"));
        }
        let message = value
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("message_start 缺少 message 对象"))?;
        let metadata = ResponseMetadata {
            response_id: optional_string(message.get("id")),
            model: optional_string(message.get("model")),
        };
        metadata.validate()?;
        output.push_back(ModelStreamEvent::MessageStart { metadata });
        if let Some(usage) = message.get("usage") {
            output.push_back(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        self.started = true;
        Ok(())
    }

    /// 处理内容块开始事件及其可能携带的首段数据。
    fn consume_content_start(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32(value, "index")?;
        let block = value
            .get("content_block")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("content_block_start 缺少 content_block"))?;
        let block_type = required_str_from_map(block, "type")?;
        let block_kind = match block_type {
            "text" => ActiveContentBlock::Text,
            "thinking" => ActiveContentBlock::Thinking,
            "redacted_thinking" => ActiveContentBlock::RedactedThinking,
            "tool_use" => ActiveContentBlock::ToolUse,
            other => {
                return Err(protocol_error(format!(
                    "Messages 包含未知内容块类型 {other}"
                )));
            }
        };
        if self.active_blocks.insert(index, block_kind).is_some() {
            return Err(protocol_error(format!(
                "Messages 内容块序号 {index} 重复开始"
            )));
        }
        match block_type {
            "text" => {
                self.saw_text_block = true;
                let text = required_str_from_map(block, "text")?;
                if !text.is_empty() {
                    self.saw_meaningful_content = true;
                    output.push_back(ModelStreamEvent::TextDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
            }
            "thinking" => {
                if let Some(thinking) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|thinking| !thinking.is_empty())
                {
                    self.saw_meaningful_content = true;
                    output.push_back(ModelStreamEvent::ReasoningDelta {
                        index,
                        delta: thinking.to_owned(),
                    });
                }
                if let Some(signature) = block
                    .get("signature")
                    .and_then(Value::as_str)
                    .filter(|signature| !signature.is_empty())
                {
                    self.thinking_signatures.insert(index, signature.to_owned());
                }
            }
            "redacted_thinking" => {
                let data = block
                    .get("data")
                    .cloned()
                    .ok_or_else(|| protocol_error("redacted_thinking 内容块缺少不透明 data"))?;
                self.saw_meaningful_content = true;
                output.push_back(ModelStreamEvent::ReasoningContinuation {
                    index,
                    continuation: OpaqueReasoningState::new(REDACTED_STATE_KIND, data),
                });
            }
            "tool_use" => {
                let id = required_str_from_map(block, "id")?.to_owned();
                let name = required_str_from_map(block, "name")?.to_owned();
                let input = block
                    .get("input")
                    .and_then(Value::as_object)
                    .ok_or_else(|| protocol_error("流式 tool_use input 必须是对象"))?;
                self.tool_calls.insert(index, id.clone());
                self.saw_meaningful_content = true;
                output.push_back(ModelStreamEvent::ToolCallStart {
                    index,
                    id: id.clone(),
                    name,
                });
                if !input.is_empty() {
                    output.push_back(ModelStreamEvent::ToolCallArgumentsDelta {
                        index,
                        id,
                        delta: serde_json::to_string(input).map_err(|error| {
                            protocol_error(format!("tool_use input 无法编码：{error}"))
                        })?,
                    });
                }
            }
            other => unreachable!("Messages 内容块类型 {other} 已在前置匹配中校验"),
        }
        Ok(())
    }

    /// 处理文本、推理、签名和工具参数增量。
    fn consume_content_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32(value, "index")?;
        let delta = value
            .get("delta")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("content_block_delta 缺少 delta"))?;
        let delta_type = required_str_from_map(delta, "type")?;
        let active_kind = self
            .active_blocks
            .get(&index)
            .copied()
            .ok_or_else(|| protocol_error(format!("内容块 {index} 尚未开始")))?;
        match delta_type {
            "text_delta" => {
                if active_kind != ActiveContentBlock::Text {
                    return Err(index_type_error(index));
                }
                let text = required_str_from_map(delta, "text")?;
                if !text.is_empty() {
                    self.saw_meaningful_content = true;
                    output.push_back(ModelStreamEvent::TextDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
            }
            "thinking_delta" => {
                if active_kind != ActiveContentBlock::Thinking {
                    return Err(index_type_error(index));
                }
                output.push_back(ModelStreamEvent::ReasoningDelta {
                    index,
                    delta: required_str_from_map(delta, "thinking")?.to_owned(),
                });
                // 已发出的推理事件仍交给中立层验证，不能因同响应的空文本误判为无内容。
                self.saw_meaningful_content = true;
            }
            "signature_delta" => {
                if active_kind != ActiveContentBlock::Thinking {
                    return Err(index_type_error(index));
                }
                self.thinking_signatures
                    .entry(index)
                    .or_default()
                    .push_str(required_str_from_map(delta, "signature")?);
            }
            "input_json_delta" => {
                if active_kind != ActiveContentBlock::ToolUse {
                    return Err(index_type_error(index));
                }
                let id = self.tool_calls.get(&index).cloned().ok_or_else(|| {
                    protocol_error(format!("工具参数增量的内容块 {index} 尚未开始"))
                })?;
                output.push_back(ModelStreamEvent::ToolCallArgumentsDelta {
                    index,
                    id,
                    delta: required_str_from_map(delta, "partial_json")?.to_owned(),
                });
            }
            other => {
                return Err(protocol_error(format!(
                    "Messages 包含未知内容增量类型 {other}"
                )));
            }
        }
        Ok(())
    }

    /// 完成工具调用或提交完整推理签名。
    fn consume_content_stop(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32(value, "index")?;
        let block_kind = self
            .active_blocks
            .remove(&index)
            .ok_or_else(|| protocol_error(format!("内容块 {index} 尚未开始或已结束")))?;
        if let Some(id) = self.tool_calls.remove(&index) {
            if block_kind != ActiveContentBlock::ToolUse {
                return Err(index_type_error(index));
            }
            output.push_back(ModelStreamEvent::ToolCallEnd { index, id });
        }
        if let Some(signature) = self.thinking_signatures.remove(&index) {
            if block_kind != ActiveContentBlock::Thinking {
                return Err(index_type_error(index));
            }
            output.push_back(ModelStreamEvent::ReasoningContinuation {
                index,
                continuation: OpaqueReasoningState::new(
                    SIGNATURE_STATE_KIND,
                    Value::String(signature),
                ),
            });
            self.saw_meaningful_content = true;
        }
        Ok(())
    }

    /// 保存结束原因并合并增量 Usage。
    fn consume_message_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        if let Some(reason) = value
            .get("delta")
            .and_then(|delta| delta.get("stop_reason"))
            .and_then(Value::as_str)
        {
            self.stop_reason = Some(map_stop_reason(Some(reason)));
        }
        if let Some(usage) = value.get("usage") {
            output.push_back(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        Ok(())
    }

    /// 生成唯一响应结束事件。
    fn consume_message_stop(
        &mut self,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        if !self.active_blocks.is_empty()
            || !self.tool_calls.is_empty()
            || !self.thinking_signatures.is_empty()
        {
            return Err(protocol_error("Messages message_stop 前仍有未结束内容块"));
        }
        if self.saw_text_block && !self.saw_meaningful_content {
            return Err(protocol_error("Messages 响应不能只有空文本内容"));
        }
        output.push_back(ModelStreamEvent::MessageEnd {
            stop_reason: self
                .stop_reason
                .take()
                .unwrap_or_else(|| StopReason::Other {
                    reason: "missing_stop_reason".to_owned(),
                }),
        });
        self.ended = true;
        Ok(())
    }
}

/// 把相邻同角色消息合并为一个 Messages 消息。
fn append_message(messages: &mut Vec<Value>, role: &str, content: Vec<Value>) {
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(existing) = last.get_mut("content").and_then(Value::as_array_mut) {
                existing.extend(content);
                return;
            }
        }
    }
    messages.push(json!({ "role": role, "content": content }));
}

/// 编码 Messages 用户输入内容块。
fn encode_user_block(block: &ContentBlock) -> Result<Value, ModelError> {
    match block {
        ContentBlock::Text { text } => Ok(json!({ "type": "text", "text": text })),
        ContentBlock::Image { image } => Ok(encode_image_source(&image.source)),
        ContentBlock::Reasoning { .. }
        | ContentBlock::ToolCall { .. }
        | ContentBlock::ToolResult { .. } => {
            Err(invalid_request("Messages 用户消息包含不支持的内容块"))
        }
    }
}

/// 编码 Messages assistant 内容块；无法安全续传的纯推理文本不会重放。
fn encode_assistant_block(block: &ContentBlock) -> Result<Option<Value>, ModelError> {
    match block {
        ContentBlock::Text { text } => Ok(Some(json!({ "type": "text", "text": text }))),
        ContentBlock::ToolCall { tool_call } => Ok(Some(json!({
            "type": "tool_use",
            "id": tool_call.id,
            "name": tool_call.name,
            "input": tool_call.arguments,
        }))),
        ContentBlock::Reasoning { reasoning } => match &reasoning.continuation {
            Some(state) if state.kind == SIGNATURE_STATE_KIND => {
                let signature = state
                    .data
                    .as_str()
                    .ok_or_else(|| invalid_request("Messages 推理签名状态必须是字符串"))?;
                Ok(Some(json!({
                    "type": "thinking",
                    "thinking": reasoning.text,
                    "signature": signature,
                })))
            }
            Some(state) if state.kind == REDACTED_STATE_KIND => Ok(Some(json!({
                "type": "redacted_thinking",
                "data": state.data,
            }))),
            Some(state) => Err(invalid_request(format!(
                "Messages 无法解释推理续传状态 {}",
                state.kind
            ))),
            None => Ok(None),
        },
        ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
            Err(invalid_request("Messages assistant 消息包含不支持的内容块"))
        }
    }
}

/// 编码一条 Messages `tool_result` 内容块。
fn encode_tool_result_block(block: &ContentBlock) -> Result<Value, ModelError> {
    let ContentBlock::ToolResult { tool_result } = block else {
        return Err(invalid_request("Messages 工具消息只能包含工具结果"));
    };
    let content = tool_result
        .content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => json!({ "type": "text", "text": text }),
            ToolResultContent::Image { image } => encode_image_source(&image.source),
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "type": "tool_result",
        "tool_use_id": tool_result.tool_call_id,
        "content": content,
        "is_error": tool_result.is_error,
    }))
}

/// 编码 Messages 图片来源。
fn encode_image_source(source: &ImageSource) -> Value {
    match source {
        ImageSource::Url { url } => json!({
            "type": "image",
            "source": { "type": "url", "url": url },
        }),
        ImageSource::Base64 { media_type, data } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            },
        }),
    }
}

/// 编码 Messages 工具选择和并行工具约束。
fn encode_tool_choice(choice: &ToolChoice, parallel: Option<bool>) -> Value {
    let mut value = match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Specific { name } => json!({ "type": "tool", "name": name }),
    };
    if let Some(disable) = parallel.map(|allowed| !allowed) {
        if let Some(object) = value.as_object_mut() {
            object.insert("disable_parallel_tool_use".to_owned(), Value::Bool(disable));
        }
    }
    value
}

/// 将 Provider 中立推理强度映射为保守的 Messages Token 预算。
fn reasoning_budget(effort: Option<ReasoningEffort>) -> u32 {
    match effort.unwrap_or(ReasoningEffort::Medium) {
        ReasoningEffort::Minimal => 1024,
        ReasoningEffort::Low => 2048,
        ReasoningEffort::Medium => 4096,
        ReasoningEffort::High => 8192,
        ReasoningEffort::ExtraHigh => 16384,
        ReasoningEffort::Maximum => 32768,
    }
}

/// 把完整 Messages 内容块转换为中立事件。
fn decode_complete_content(
    index: u32,
    block: &Value,
    events: &mut Vec<ModelStreamEvent>,
) -> Result<(), ModelError> {
    let object = block
        .as_object()
        .ok_or_else(|| protocol_error("Messages content 元素必须是对象"))?;
    match required_str_from_map(object, "type")? {
        "text" => {
            let text = required_str_from_map(object, "text")?;
            if !text.is_empty() {
                events.push(ModelStreamEvent::TextDelta {
                    index,
                    delta: text.to_owned(),
                });
            }
        }
        "thinking" => {
            events.push(ModelStreamEvent::ReasoningDelta {
                index,
                delta: required_str_from_map(object, "thinking")?.to_owned(),
            });
            if let Some(signature) = object.get("signature").and_then(Value::as_str) {
                events.push(ModelStreamEvent::ReasoningContinuation {
                    index,
                    continuation: OpaqueReasoningState::new(
                        SIGNATURE_STATE_KIND,
                        Value::String(signature.to_owned()),
                    ),
                });
            }
        }
        "redacted_thinking" => {
            events.push(ModelStreamEvent::ReasoningContinuation {
                index,
                continuation: OpaqueReasoningState::new(
                    REDACTED_STATE_KIND,
                    object
                        .get("data")
                        .cloned()
                        .ok_or_else(|| protocol_error("redacted_thinking 缺少 data"))?,
                ),
            });
        }
        "tool_use" => {
            let id = required_str_from_map(object, "id")?.to_owned();
            events.push(ModelStreamEvent::ToolCallStart {
                index,
                id: id.clone(),
                name: required_str_from_map(object, "name")?.to_owned(),
            });
            events.push(ModelStreamEvent::ToolCallArgumentsDelta {
                index,
                id: id.clone(),
                delta: serde_json::to_string(
                    object
                        .get("input")
                        .ok_or_else(|| protocol_error("tool_use 缺少 input"))?,
                )
                .map_err(|error| protocol_error(format!("tool_use input 无法编码：{error}")))?,
            });
            events.push(ModelStreamEvent::ToolCallEnd { index, id });
        }
        other => {
            return Err(protocol_error(format!(
                "Messages 包含未知完整内容块 {other}"
            )));
        }
    }
    Ok(())
}

/// 解析 Messages Usage，并保持缺失值为 `None`。
fn decode_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
        reasoning_tokens: None,
        cache_read_tokens: value.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_write_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
        total_tokens: None,
    }
}

/// 映射 Messages 结束原因为 Provider 中立枚举。
fn map_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("end_turn" | "stop_sequence") => StopReason::Completed,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens" | "model_context_window_exceeded") => StopReason::MaxOutputTokens,
        Some("refusal") => StopReason::ContentFilter,
        Some(other) => StopReason::Other {
            reason: other.to_owned(),
        },
        None => StopReason::Other {
            reason: "missing_stop_reason".to_owned(),
        },
    }
}

/// 提取 Provider 错误对象中的安全文本摘要。
fn provider_error_message(value: &Value) -> String {
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("Messages Provider 返回未说明错误")
        .to_owned()
}

/// 判断顶层值是否包含 Anthropic 错误事件约定的嵌套错误对象。
fn has_explicit_provider_error(value: &Value) -> bool {
    value.get("error").is_some_and(Value::is_object)
}

/// 仅把具有明确结构或上下文超限语义的 Provider 错误归一为稳定错误类型。
fn classify_provider_error(value: &Value) -> ModelError {
    let message = provider_error_message(value);
    let code = value
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(Value::as_str)
        .or_else(|| value.get("code").and_then(Value::as_str));
    classify_in_band_provider_error(&message, code)
}

/// 从对象读取必需的字符串字段。
fn required_str_from_map<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error(format!("Messages 字段 {field} 必须是字符串")))
}

/// 从顶层对象读取可转换为 u32 的必需整数。
fn required_u32(value: &Value, field: &str) -> Result<u32, ModelError> {
    let number = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol_error(format!("Messages 字段 {field} 必须是非负整数")))?;
    u32::try_from(number)
        .map_err(|_| protocol_error(format!("Messages 字段 {field} 超过 u32 范围")))
}

/// 把可选 JSON 字符串复制为拥有所有权的值。
fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(ToOwned::to_owned)
}

/// 要求 SSE 已经收到响应开始事件。
fn require_started(started: bool) -> Result<(), ModelError> {
    if started {
        Ok(())
    } else {
        Err(protocol_error("Messages 内容事件早于 message_start"))
    }
}

/// 创建内容块序号混用类型时的统一协议错误。
fn index_type_error(index: u32) -> ModelError {
    protocol_error(format!("内容块序号 {index} 被用于不同内容类型"))
}

/// 创建统一请求校验错误。
fn invalid_request(message: impl Into<String>) -> ModelError {
    ModelError::InvalidRequest {
        message: message.into(),
    }
}

/// 创建统一协议解析错误。
fn protocol_error(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}
