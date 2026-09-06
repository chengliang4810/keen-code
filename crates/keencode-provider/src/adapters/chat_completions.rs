use std::collections::{BTreeMap, VecDeque};

use keencode_model::{
    ContentBlock, ImageSource, MessageRole, ModelError, ModelRequest, ModelStreamEvent,
    ReasoningEffort, ResponseMetadata, StopReason, TokenUsage, ToolChoice, ToolResultContent,
};
use serde_json::{Map, Value, json};

use crate::{http::classify_in_band_provider_error, sse::SseFrame};

/// Chat Completions 流中一个正在拼接的工具调用。
#[derive(Debug)]
struct PendingToolCall {
    content_index: u32,
    id: Option<String>,
    name: Option<String>,
    started: bool,
}

/// OpenAI Chat Completions 请求、JSON 响应和 SSE 事件的协议 Adapter。
pub(crate) struct ChatCompletionsAdapter {
    started: bool,
    ended: bool,
    finish_reason: Option<StopReason>,
    next_content_index: u32,
    text_index: Option<u32>,
    reasoning_index: Option<u32>,
    /// 是否已经观察到安全拒绝字段。
    saw_refusal: bool,
    tools: BTreeMap<u32, PendingToolCall>,
}

impl ChatCompletionsAdapter {
    /// 创建一次请求专用且没有残留流状态的 Adapter。
    pub fn new() -> Self {
        Self {
            started: false,
            ended: false,
            finish_reason: None,
            next_content_index: 0,
            text_index: None,
            reasoning_index: None,
            saw_refusal: false,
            tools: BTreeMap::new(),
        }
    }

    /// 把 Provider 中立请求编码为 Chat Completions 请求正文。
    pub fn encode_request(
        &self,
        request: &ModelRequest,
        streaming: bool,
    ) -> Result<Value, ModelError> {
        request.validate()?;
        let mut messages = Vec::new();
        for message in &request.messages {
            match message.role {
                MessageRole::System | MessageRole::Developer | MessageRole::User => {
                    messages.push(encode_chat_message(message.role, &message.content)?);
                }
                MessageRole::Assistant => {
                    messages.push(encode_assistant_message(&message.content)?);
                }
                MessageRole::Tool => {
                    for block in &message.content {
                        messages.push(encode_tool_message(block)?);
                    }
                }
            }
        }

        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(request.model.clone()));
        body.insert("messages".to_owned(), Value::Array(messages));
        body.insert("stream".to_owned(), Value::Bool(streaming));
        if streaming {
            body.insert(
                "stream_options".to_owned(),
                json!({ "include_usage": true }),
            );
        }
        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_completion_tokens".to_owned(), Value::from(max_tokens));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_owned(), Value::from(temperature));
        }
        if !request.stop_sequences.is_empty() {
            body.insert(
                "stop".to_owned(),
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
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_owned(),
                Value::Array(
                    request
                        .tools
                        .iter()
                        .map(|tool| {
                            json!({
                                "type": "function",
                                "function": {
                                    "name": tool.name,
                                    "description": tool.description,
                                    "parameters": tool.input_schema,
                                    "strict": true,
                                }
                            })
                        })
                        .collect(),
                ),
            );
            body.insert(
                "tool_choice".to_owned(),
                encode_tool_choice(&request.tool_choice),
            );
            if let Some(parallel) = request.parallel_tool_calls {
                body.insert("parallel_tool_calls".to_owned(), Value::Bool(parallel));
            }
        } else if !matches!(request.tool_choice, ToolChoice::Auto | ToolChoice::None) {
            return Err(invalid_request("Chat Completions 工具选择要求非空工具列表"));
        }
        if let Some(reasoning) = &request.reasoning {
            if let Some(effort) = reasoning.effort {
                body.insert(
                    "reasoning_effort".to_owned(),
                    Value::String(reasoning_effort(effort).to_owned()),
                );
            }
        }
        if let Some(structured) = &request.structured_output {
            body.insert(
                "response_format".to_owned(),
                json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": structured.name,
                        "description": structured.description,
                        "schema": structured.schema,
                        "strict": structured.strict,
                    }
                }),
            );
        }
        Ok(Value::Object(body))
    }

    /// 消费一条 Chat Completions SSE 帧并追加 Provider 中立事件。
    pub fn consume_sse(
        &mut self,
        frame: SseFrame,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.ended {
            if frame.data.trim() == "[DONE]" {
                return Ok(());
            }
            return Err(protocol_error("Chat Completions 响应结束后仍收到 SSE 数据"));
        }
        if frame.data.trim() == "[DONE]" {
            return self.emit_message_end(output);
        }
        if frame.data.trim().is_empty() {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&frame.data).map_err(|error| {
            protocol_error(format!("Chat Completions SSE data 不是有效 JSON：{error}"))
        })?;
        if value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(classify_provider_error(&value));
        }
        self.consume_chunk(&value, output)
    }

    /// 把一个非流式 Chat Completions JSON 响应转换为完整事件序列。
    pub fn decode_json(&mut self, value: Value) -> Result<Vec<ModelStreamEvent>, ModelError> {
        let response = value
            .as_object()
            .ok_or_else(|| protocol_error("Chat Completions 响应必须是 JSON 对象"))?;
        if response.get("error").is_some_and(|error| !error.is_null()) {
            return Err(classify_provider_error(&value));
        }
        let metadata = response_metadata(response)?;
        let mut events = vec![ModelStreamEvent::MessageStart { metadata }];
        self.started = true;

        let choices = response
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_error("Chat Completions 响应缺少 choices 数组"))?;
        if choices.len() != 1 {
            return Err(protocol_error(format!(
                "Chat Completions 期望一个 choice，实际为 {}",
                choices.len()
            )));
        }
        let choice = choices[0]
            .as_object()
            .ok_or_else(|| protocol_error("Chat Completions choice 必须是对象"))?;
        let message = choice
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("Chat Completions choice 缺少 message"))?;
        self.decode_reasoning_fields(message, &mut events)?;
        self.decode_content_value(message.get("content"), &mut events)?;
        self.decode_refusal_field(message, &mut events)?;
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                self.decode_complete_tool_call(tool_call, &mut events)?;
            }
        }
        if let Some(usage) = response.get("usage") {
            events.push(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        let stop_reason = if self.saw_refusal {
            StopReason::ContentFilter
        } else {
            map_finish_reason(choice.get("finish_reason").and_then(Value::as_str))
        };
        events.push(ModelStreamEvent::MessageEnd { stop_reason });
        self.ended = true;
        Ok(events)
    }

    /// 校验 Chat Completions SSE 流已经收到非空 `finish_reason`。
    pub fn finish_stream(
        &mut self,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.ended {
            Ok(())
        } else if self.finish_reason.is_some() {
            self.emit_message_end(output)
        } else {
            Err(protocol_error(
                "Chat Completions SSE 在 finish_reason 之前关闭",
            ))
        }
    }

    /// 解析一个流式 chunk 的元数据、内容、工具、Usage 和结束原因。
    fn consume_chunk(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let response = value
            .as_object()
            .ok_or_else(|| protocol_error("Chat Completions chunk 必须是对象"))?;
        if !self.started {
            let metadata = response_metadata(response)?;
            output.push_back(ModelStreamEvent::MessageStart { metadata });
            self.started = true;
        }
        let usage = response.get("usage").filter(|usage| !usage.is_null());
        let choices = response
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_error("Chat Completions chunk 缺少 choices"))?;
        if self.finish_reason.is_some()
            && !choices.is_empty()
            && !(usage.is_some() && is_inert_usage_choice(choices))
        {
            return Err(protocol_error(
                "Chat Completions 在 finish_reason 后仍返回非空 choices",
            ));
        }
        if let Some(usage) = usage {
            output.push_back(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        if choices.is_empty() {
            return Ok(());
        }
        if self.finish_reason.is_some() {
            return Ok(());
        }
        if choices.len() != 1 {
            return Err(protocol_error("Chat Completions 流式响应不支持多个 choice"));
        }
        let choice = choices[0]
            .as_object()
            .ok_or_else(|| protocol_error("Chat Completions choice 必须是对象"))?;
        if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
            return Err(protocol_error(
                "Chat Completions 只接受 index=0 的单一 choice",
            ));
        }
        if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
            let mut normalized = Vec::new();
            self.decode_reasoning_fields(delta, &mut normalized)?;
            self.decode_content_value(delta.get("content"), &mut normalized)?;
            self.decode_refusal_field(delta, &mut normalized)?;
            output.extend(normalized);
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    self.consume_tool_delta(tool_call, output)?;
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_tools(output)?;
            self.finish_reason = Some(if self.saw_refusal {
                StopReason::ContentFilter
            } else {
                map_finish_reason(Some(reason))
            });
        }
        Ok(())
    }

    /// 在 usage-only 尾块之后发出统一响应终态。
    fn emit_message_end(
        &mut self,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let stop_reason = self
            .finish_reason
            .take()
            .ok_or_else(|| protocol_error("Chat Completions 在 finish_reason 前收到 [DONE]"))?;
        output.push_back(ModelStreamEvent::MessageEnd { stop_reason });
        self.ended = true;
        Ok(())
    }

    /// 解析常见 Chat 推理文本字段并保持其与普通文本分离。
    fn decode_reasoning_fields(
        &mut self,
        object: &Map<String, Value>,
        output: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        for field in ["reasoning_content", "reasoning"] {
            if let Some(text) = object
                .get(field)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                let index = self.reasoning_content_index()?;
                output.push(ModelStreamEvent::ReasoningDelta {
                    index,
                    delta: text.to_owned(),
                });
            }
        }
        if let Some(details) = object.get("reasoning_details").and_then(Value::as_array) {
            for detail in details {
                let text = detail
                    .get("text")
                    .or_else(|| detail.get("delta"))
                    .and_then(Value::as_str);
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    let index = self.reasoning_content_index()?;
                    output.push(ModelStreamEvent::ReasoningDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// 解析 Structured Output 安全拒绝字段并标记非正常完成原因。
    fn decode_refusal_field(
        &mut self,
        object: &Map<String, Value>,
        output: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if let Some(text) = object
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.saw_refusal = true;
            let index = self.text_content_index()?;
            output.push(ModelStreamEvent::TextDelta {
                index,
                delta: text.to_owned(),
            });
        }
        Ok(())
    }

    /// 解析字符串或内容 part 数组形式的 Chat 文本。
    fn decode_content_value(
        &mut self,
        value: Option<&Value>,
        output: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let Some(value) = value else {
            return Ok(());
        };
        if value.is_null() {
            return Ok(());
        }
        if let Some(text) = value.as_str() {
            if !text.is_empty() {
                let index = self.text_content_index()?;
                output.push(ModelStreamEvent::TextDelta {
                    index,
                    delta: text.to_owned(),
                });
            }
            return Ok(());
        }
        let parts = value
            .as_array()
            .ok_or_else(|| protocol_error("Chat content 必须是字符串、数组或 null"))?;
        for part in parts {
            let part_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
            match part_type {
                "text" | "output_text" => {
                    if let Some(text) = part
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        let index = self.text_content_index()?;
                        output.push(ModelStreamEvent::TextDelta {
                            index,
                            delta: text.to_owned(),
                        });
                    }
                }
                other => {
                    return Err(protocol_error(format!(
                        "Chat content 包含未知 part 类型 {other}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// 消费一个流式函数调用增量并在字段齐全时发出开始事件。
    fn consume_tool_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let object = value
            .as_object()
            .ok_or_else(|| protocol_error("Chat tool_call delta 必须是对象"))?;
        let wire_index = required_u32(object, "index")?;
        if let std::collections::btree_map::Entry::Vacant(entry) = self.tools.entry(wire_index) {
            let content_index = self.next_content_index;
            self.next_content_index = self
                .next_content_index
                .checked_add(1)
                .ok_or_else(|| protocol_error("Chat 内容块序号溢出"))?;
            entry.insert(PendingToolCall {
                content_index,
                id: None,
                name: None,
                started: false,
            });
        }
        let pending = self
            .tools
            .get_mut(&wire_index)
            .ok_or_else(|| protocol_error("Chat 工具调用状态丢失"))?;
        if let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            if pending.id.as_deref().is_some_and(|existing| existing != id) {
                return Err(protocol_error("Chat 工具调用 ID 在流中发生变化"));
            }
            pending.id = Some(id.to_owned());
        }
        if let Some(name) = object
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            if pending
                .name
                .as_deref()
                .is_some_and(|existing| existing != name)
            {
                return Err(protocol_error("Chat 工具名称在流中发生变化"));
            }
            pending.name = Some(name.to_owned());
        }
        if !pending.started {
            if let (Some(id), Some(name)) = (pending.id.clone(), pending.name.clone()) {
                output.push_back(ModelStreamEvent::ToolCallStart {
                    index: pending.content_index,
                    id,
                    name,
                });
                pending.started = true;
            }
        }
        if let Some(arguments) = object
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .filter(|arguments| !arguments.is_empty())
        {
            if !pending.started {
                return Err(protocol_error("Chat 工具参数早于完整调用 ID 和名称"));
            }
            output.push_back(ModelStreamEvent::ToolCallArgumentsDelta {
                index: pending.content_index,
                id: pending
                    .id
                    .clone()
                    .ok_or_else(|| protocol_error("Chat 工具调用缺少 ID"))?,
                delta: arguments.to_owned(),
            });
        }
        Ok(())
    }

    /// 解析一个非流式完整工具调用。
    fn decode_complete_tool_call(
        &mut self,
        value: &Value,
        output: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let object = value
            .as_object()
            .ok_or_else(|| protocol_error("Chat tool_call 必须是对象"))?;
        let id = required_str(object, "id")?.to_owned();
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("Chat tool_call 缺少 function"))?;
        let index = self.allocate_content_index()?;
        output.push(ModelStreamEvent::ToolCallStart {
            index,
            id: id.clone(),
            name: required_str(function, "name")?.to_owned(),
        });
        output.push(ModelStreamEvent::ToolCallArgumentsDelta {
            index,
            id: id.clone(),
            delta: required_str(function, "arguments")?.to_owned(),
        });
        output.push(ModelStreamEvent::ToolCallEnd { index, id });
        Ok(())
    }

    /// 在响应结束时按 wire index 完成全部工具调用。
    fn finish_tools(&mut self, output: &mut VecDeque<ModelStreamEvent>) -> Result<(), ModelError> {
        for pending in self.tools.values() {
            if !pending.started {
                return Err(protocol_error(
                    "Chat 响应结束时存在缺少 ID 或名称的工具调用",
                ));
            }
            output.push_back(ModelStreamEvent::ToolCallEnd {
                index: pending.content_index,
                id: pending
                    .id
                    .clone()
                    .ok_or_else(|| protocol_error("Chat 工具调用缺少 ID"))?,
            });
        }
        self.tools.clear();
        Ok(())
    }

    /// 返回或分配普通文本内容块序号。
    fn text_content_index(&mut self) -> Result<u32, ModelError> {
        if let Some(index) = self.text_index {
            Ok(index)
        } else {
            let index = self.allocate_content_index()?;
            self.text_index = Some(index);
            Ok(index)
        }
    }

    /// 返回或分配推理文本内容块序号。
    fn reasoning_content_index(&mut self) -> Result<u32, ModelError> {
        if let Some(index) = self.reasoning_index {
            Ok(index)
        } else {
            let index = self.allocate_content_index()?;
            self.reasoning_index = Some(index);
            Ok(index)
        }
    }

    /// 分配不会与已有内容块冲突的序号。
    fn allocate_content_index(&mut self) -> Result<u32, ModelError> {
        let index = self.next_content_index;
        self.next_content_index = self
            .next_content_index
            .checked_add(1)
            .ok_or_else(|| protocol_error("Chat 内容块序号溢出"))?;
        Ok(index)
    }
}

/// 识别部分兼容网关在结束后附带 Usage 时返回的单个惰性占位 choice。
fn is_inert_usage_choice(choices: &[Value]) -> bool {
    let [choice] = choices else {
        return false;
    };
    let Some(choice) = choice.as_object() else {
        return false;
    };
    choice.keys().all(|key| {
        matches!(
            key.as_str(),
            "index" | "delta" | "finish_reason" | "logprobs"
        )
    }) && choice.get("index").and_then(Value::as_u64) == Some(0)
        && choice
            .get("delta")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
        && choice.get("finish_reason").is_none_or(Value::is_null)
        && choice.get("logprobs").is_none_or(Value::is_null)
}

/// 编码 system、developer 或 user Chat 消息。
fn encode_chat_message(role: MessageRole, blocks: &[ContentBlock]) -> Result<Value, ModelError> {
    let role = match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant | MessageRole::Tool => {
            return Err(invalid_request("Chat 消息角色编码调用错误"));
        }
    };
    let has_image = blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }));
    let content = if has_image {
        Value::Array(
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => Ok(json!({ "type": "text", "text": text })),
                    ContentBlock::Image { image } if role == "user" => Ok(json!({
                        "type": "image_url",
                        "image_url": { "url": image_url(&image.source) },
                    })),
                    _ => Err(invalid_request(
                        "Chat system/developer 只允许文本，user 只允许文本或图片",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    } else {
        Value::String(
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => Ok(text.as_str()),
                    _ => Err(invalid_request("Chat 文本消息包含不支持的内容块")),
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(""),
        )
    };
    Ok(json!({ "role": role, "content": content }))
}

/// 编码 Chat assistant 文本和工具调用；推理文本不作为普通历史重放。
fn encode_assistant_message(blocks: &[ContentBlock]) -> Result<Value, ModelError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::Reasoning { .. } => {}
            ContentBlock::ToolCall { tool_call } => tool_calls.push(json!({
                "id": tool_call.id,
                "type": "function",
                "function": {
                    "name": tool_call.name,
                    "arguments": serde_json::to_string(&tool_call.arguments).map_err(|error| {
                        invalid_request(format!("Chat 工具参数无法编码：{error}"))
                    })?,
                }
            })),
            ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
                return Err(invalid_request("Chat assistant 消息包含不支持的内容块"));
            }
        }
    }
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert(
        "content".to_owned(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Ok(Value::Object(message))
}

/// 编码一条 Chat tool 消息。
fn encode_tool_message(block: &ContentBlock) -> Result<Value, ModelError> {
    let ContentBlock::ToolResult { tool_result } = block else {
        return Err(invalid_request("Chat 工具消息只能包含工具结果"));
    };
    let mut text = String::new();
    for item in &tool_result.content {
        match item {
            ToolResultContent::Text { text: part } => text.push_str(part),
            ToolResultContent::Image { .. } => {
                return Err(ModelError::UnsupportedCapability {
                    capability: "tool_result_image".to_owned(),
                    message: "Chat Completions 标准工具结果不支持图片".to_owned(),
                });
            }
        }
    }
    Ok(json!({
        "role": "tool",
        "tool_call_id": tool_result.tool_call_id,
        "content": text,
    }))
}

/// 将图片来源转换为 Chat `image_url.url`。
fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
    }
}

/// 编码 Chat 工具选择策略。
fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Required => Value::String("required".to_owned()),
        ToolChoice::Specific { name } => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

/// 映射 Provider 中立推理强度到 Chat 字段。
fn reasoning_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
        ReasoningEffort::Maximum => "max",
    }
}

/// 从 Chat JSON 对象提取响应元数据。
fn response_metadata(response: &Map<String, Value>) -> Result<ResponseMetadata, ModelError> {
    let metadata = ResponseMetadata {
        response_id: response
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model: response
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    };
    metadata.validate()?;
    Ok(metadata)
}

/// 解析 Chat Usage，并保持缺失值为 `None`。
fn decode_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: value.get("prompt_tokens").and_then(Value::as_u64),
        output_tokens: value.get("completion_tokens").and_then(Value::as_u64),
        reasoning_tokens: value
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        cache_read_tokens: value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64),
        cache_write_tokens: None,
        total_tokens: value.get("total_tokens").and_then(Value::as_u64),
    }
}

/// 映射 Chat 完成原因为 Provider 中立枚举。
fn map_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("stop") => StopReason::Completed,
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxOutputTokens,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other {
            reason: other.to_owned(),
        },
        None => StopReason::Other {
            reason: "missing_finish_reason".to_owned(),
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
        .unwrap_or("Chat Completions Provider 返回未说明错误")
        .to_owned()
}

/// 只把具有明确上下文超限证据的 Chat Completions 错误归一为稳定错误类型。
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

/// 从 JSON 对象读取必需字符串字段。
fn required_str<'a>(value: &'a Map<String, Value>, field: &str) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error(format!("Chat 字段 {field} 必须是字符串")))
}

/// 从 JSON 对象读取可转换为 u32 的必需整数。
fn required_u32(value: &Map<String, Value>, field: &str) -> Result<u32, ModelError> {
    let number = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol_error(format!("Chat 字段 {field} 必须是非负整数")))?;
    u32::try_from(number).map_err(|_| protocol_error(format!("Chat 字段 {field} 超过 u32 范围")))
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
