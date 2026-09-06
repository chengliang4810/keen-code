use std::collections::{BTreeMap, BTreeSet, VecDeque};

use keencode_model::{
    ContentBlock, ImageSource, MessageRole, ModelError, ModelRequest, ModelStreamEvent,
    OpaqueReasoningState, ReasoningEffort, ResponseMetadata, StopReason, TokenUsage, ToolChoice,
    ToolResultContent,
};
use serde_json::{Map, Value, json};

use crate::{http::classify_in_band_provider_error, sse::SseFrame};

const REASONING_STATE_KIND: &str = "responses-reasoning-item-v1";

/// Responses 流中一个正在拼接的函数调用。
#[derive(Debug)]
struct PendingFunctionCall {
    /// Provider 中立事件使用的唯一内容块序号。
    output_index: u32,
    call_id: String,
    /// 远端函数名称，用于检测重复 output item 是否改变调用目标。
    name: String,
    /// 已经发出的参数 JSON 文本，用于和 done 事件的完整参数交叉校验。
    arguments: String,
    arguments_seen: bool,
    /// 是否已经收到一次 `function_call_arguments.done` 事件。
    arguments_done: bool,
    ended: bool,
}

/// Responses 网关流中需要映射为独立内容块的语义类型。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum StreamingBlockKind {
    /// 普通输出文本或安全拒绝文本。
    Text,
    /// 推理正文、摘要和续传状态。
    Reasoning,
    /// 函数工具调用。
    Tool,
}

/// OpenAI Responses 请求、JSON 响应和语义 SSE 事件的协议 Adapter。
pub(crate) struct ResponsesAdapter {
    started: bool,
    ended: bool,
    saw_tool_call: bool,
    /// 是否已经观察到安全拒绝内容。
    saw_refusal: bool,
    tools: BTreeMap<u32, PendingFunctionCall>,
    emitted_reasoning_states: BTreeSet<u32>,
    /// 把可能被兼容网关复用的远端序号按语义类型映射为唯一的本地序号。
    streaming_indices: BTreeMap<(u32, StreamingBlockKind), u32>,
    /// 下一个可分配的 Provider 中立内容块序号。
    next_content_index: u32,
}

impl ResponsesAdapter {
    /// 创建一次请求专用且没有残留流状态的 Adapter。
    pub fn new() -> Self {
        Self {
            started: false,
            ended: false,
            saw_tool_call: false,
            saw_refusal: false,
            tools: BTreeMap::new(),
            emitted_reasoning_states: BTreeSet::new(),
            streaming_indices: BTreeMap::new(),
            next_content_index: 0,
        }
    }

    /// 把 Provider 中立请求编码为 Responses API 请求正文。
    pub fn encode_request(
        &self,
        request: &ModelRequest,
        streaming: bool,
    ) -> Result<Value, ModelError> {
        request.validate()?;
        if !request.stop_sequences.is_empty() {
            return Err(ModelError::UnsupportedCapability {
                capability: "stop_sequences".to_owned(),
                message: "Responses API 没有标准 stop sequence 请求字段".to_owned(),
            });
        }
        let mut input = Vec::new();
        for message in &request.messages {
            encode_input_message(message.role, &message.content, &mut input)?;
        }
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(request.model.clone()));
        body.insert("input".to_owned(), Value::Array(input));
        body.insert("stream".to_owned(), Value::Bool(streaming));
        body.insert("store".to_owned(), Value::Bool(false));
        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_output_tokens".to_owned(), Value::from(max_tokens));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_owned(), Value::from(temperature));
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
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.input_schema,
                                "strict": true,
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
        } else if matches!(request.tool_choice, ToolChoice::None) {
            // Responses API 的隔离请求即使没有工具定义也可以显式发送 `none`，避免供应商
            // 对标题或记忆生成隐式套用工具策略。
            body.insert(
                "tool_choice".to_owned(),
                encode_tool_choice(&request.tool_choice),
            );
        } else if !matches!(request.tool_choice, ToolChoice::Auto | ToolChoice::None) {
            return Err(invalid_request("Responses 工具选择要求非空工具列表"));
        }
        if let Some(reasoning) = &request.reasoning {
            if reasoning.max_tokens.is_some() {
                return Err(ModelError::UnsupportedCapability {
                    capability: "reasoning_token_budget".to_owned(),
                    message: "Responses API 只接受推理强度，不接受标准推理 Token 预算".to_owned(),
                });
            }
            let mut config = Map::new();
            if let Some(effort) = reasoning.effort {
                config.insert(
                    "effort".to_owned(),
                    Value::String(reasoning_effort(effort).to_owned()),
                );
            }
            if reasoning.include_summary {
                config.insert("summary".to_owned(), Value::String("auto".to_owned()));
            }
            if !config.is_empty() {
                body.insert("reasoning".to_owned(), Value::Object(config));
            }
        }
        if let Some(structured) = &request.structured_output {
            body.insert(
                "text".to_owned(),
                json!({
                    "format": {
                        "type": "json_schema",
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

    /// 消费一条 Responses SSE 帧并追加 Provider 中立事件。
    pub fn consume_sse(
        &mut self,
        frame: SseFrame,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.ended {
            if frame.data.trim() == "[DONE]" {
                return Ok(());
            }
            return Err(protocol_error("Responses 结束后仍收到 SSE 事件"));
        }
        if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&frame.data).map_err(|error| {
            protocol_error(format!("Responses SSE data 不是有效 JSON：{error}"))
        })?;
        let frame_event_type = frame.event.as_deref();
        let data_event_type = value.get("type").and_then(Value::as_str);
        if let (Some(frame_event_type), Some(data_event_type)) = (frame_event_type, data_event_type)
        {
            if frame_event_type != data_event_type {
                return Err(protocol_error("Responses SSE event 与 data.type 不一致"));
            }
        }
        if has_explicit_provider_failure(&value) {
            return Err(classify_provider_error(&value));
        }
        let event_type = match frame_event_type.or(data_event_type) {
            Some(event_type) => event_type,
            None => return Err(protocol_error("Responses SSE 缺少事件类型")),
        };
        self.lazy_start_from_content_event(event_type, &value, output)?;
        match event_type {
            "response.created" => self.consume_response_created(&value, output),
            "response.queued" | "response.in_progress" => Ok(()),
            "response.output_item.added" => self.consume_output_item_added(&value, output),
            "response.output_item.done" => self.consume_output_item_done(&value, output),
            "response.content_part.added" => self.consume_content_part_added(&value, output),
            "response.output_text.delta" => self.consume_text_delta(&value, output),
            "response.refusal.delta" => {
                self.saw_refusal = true;
                self.consume_text_delta(&value, output)
            }
            "response.reasoning_summary_text.delta" => {
                self.consume_reasoning_summary_delta(&value, output)
            }
            "response.reasoning_text.delta" => self.consume_reasoning_delta(&value, output),
            "response.function_call_arguments.delta" => {
                self.consume_function_arguments_delta(&value, output)
            }
            "response.function_call_arguments.done" => {
                self.consume_function_arguments_done(&value, output)
            }
            "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done" => require_started(self.started),
            "response.completed" | "response.incomplete" | "response.cancelled" => {
                self.consume_response_terminal(event_type, &value, output)
            }
            "response.failed" | "error" => Err(classify_provider_error(&value)),
            other => Err(protocol_error(format!(
                "Responses SSE 包含未知事件 {other}"
            ))),
        }
    }

    /// 在兼容网关偶发省略 `response.created` 时，仅从带模型身份的内容首帧安全起始。
    fn lazy_start_from_content_event(
        &mut self,
        event_type: &str,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.started || !is_lazy_start_content_event(event_type) {
            return Ok(());
        }
        let model = value
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| protocol_error("Responses 内容事件早于 response.created"))?;
        let metadata = ResponseMetadata {
            response_id: None,
            model: Some(model.to_owned()),
        };
        metadata.validate()?;
        output.push_back(ModelStreamEvent::MessageStart { metadata });
        self.started = true;
        Ok(())
    }

    /// 把一个非流式 Responses JSON 响应转换为完整事件序列。
    pub fn decode_json(&mut self, value: Value) -> Result<Vec<ModelStreamEvent>, ModelError> {
        let response = value
            .as_object()
            .ok_or_else(|| protocol_error("Responses 响应必须是 JSON 对象"))?;
        if has_explicit_provider_failure(&value) {
            return Err(classify_provider_error(&value));
        }
        let metadata = response_metadata(response)?;
        let mut events = vec![ModelStreamEvent::MessageStart { metadata }];
        self.started = true;
        let output_items = response
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_error("Responses 响应缺少 output 数组"))?;
        for (position, item) in output_items.iter().enumerate() {
            let index = u32::try_from(position)
                .map_err(|_| protocol_error("Responses output 数量超过 u32 范围"))?;
            self.decode_complete_item(index, item, &mut events)?;
        }
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            events.push(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        events.push(ModelStreamEvent::MessageEnd {
            stop_reason: response_stop_reason(response, self.saw_tool_call, self.saw_refusal),
        });
        self.ended = true;
        Ok(events)
    }

    /// 校验 Responses SSE 流已经收到明确终态事件。
    pub fn finish_stream(&mut self) -> Result<(), ModelError> {
        if self.ended {
            Ok(())
        } else {
            Err(protocol_error("Responses SSE 在终态事件之前关闭"))
        }
    }

    /// 处理 `response.created` 并保存响应元数据。
    fn consume_response_created(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.started {
            return Err(protocol_error("Responses 重复 response.created"));
        }
        let response = value
            .get("response")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("response.created 缺少 response 对象"))?;
        output.push_back(ModelStreamEvent::MessageStart {
            metadata: response_metadata(response)?,
        });
        self.started = true;
        Ok(())
    }

    /// 处理 output item 开始并创建函数调用或推理状态。
    fn consume_output_item_added(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32_value(value, "output_index")?;
        let item = value
            .get("item")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("response.output_item.added 缺少 item"))?;
        match required_str(item, "type")? {
            "function_call" => self.start_function_call(index, item, output),
            "reasoning" => self
                .streaming_output_index(index, StreamingBlockKind::Reasoning)
                .map(|_| ()),
            "message" => self
                .streaming_output_index(index, StreamingBlockKind::Text)
                .map(|_| ()),
            other => Err(protocol_error(format!(
                "Responses 包含未知 output item 类型 {other}"
            ))),
        }
    }

    /// 处理 output item 完成并补齐函数参数、结束调用或保存推理状态。
    fn consume_output_item_done(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32_value(value, "output_index")?;
        let item = value
            .get("item")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("response.output_item.done 缺少 item"))?;
        match required_str(item, "type")? {
            "function_call" => {
                if !self.tools.contains_key(&index) {
                    self.start_function_call(index, item, output)?;
                }
                let pending = self
                    .tools
                    .get_mut(&index)
                    .ok_or_else(|| protocol_error("Responses 函数调用状态丢失"))?;
                if let Some(arguments) = optional_string_from_map(item, "arguments")? {
                    complete_function_arguments(pending, arguments, output)?;
                }
                if pending.ended {
                    return Err(protocol_error("Responses 函数调用重复结束"));
                }
                output.push_back(ModelStreamEvent::ToolCallEnd {
                    index: pending.output_index,
                    id: pending.call_id.clone(),
                });
                pending.ended = true;
                Ok(())
            }
            "reasoning" => {
                let local_index =
                    self.streaming_output_index(index, StreamingBlockKind::Reasoning)?;
                self.emit_reasoning_state(local_index, item, output)
            }
            "message" => self
                .streaming_output_index(index, StreamingBlockKind::Text)
                .map(|_| ()),
            other => Err(protocol_error(format!(
                "Responses 包含未知完成 item 类型 {other}"
            ))),
        }
    }

    /// 处理 content part 起始时已经包含的文本。
    fn consume_content_part_added(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32_value(value, "output_index")?;
        let part = value
            .get("part")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("response.content_part.added 缺少 part"))?;
        match required_str(part, "type")? {
            "output_text" => {
                let index = self.streaming_output_index(index, StreamingBlockKind::Text)?;
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    output.push_back(ModelStreamEvent::TextDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
                Ok(())
            }
            "refusal" => {
                self.saw_refusal = true;
                let index = self.streaming_output_index(index, StreamingBlockKind::Text)?;
                if let Some(text) = part
                    .get("refusal")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    output.push_back(ModelStreamEvent::TextDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
                Ok(())
            }
            "reasoning_text" => {
                let index = self.streaming_output_index(index, StreamingBlockKind::Reasoning)?;
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    output.push_back(ModelStreamEvent::ReasoningDelta {
                        index,
                        delta: text.to_owned(),
                    });
                }
                Ok(())
            }
            other => Err(protocol_error(format!(
                "Responses 包含未知 content part 类型 {other}"
            ))),
        }
    }

    /// 处理普通文本或拒绝文本增量。
    fn consume_text_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = self.streaming_output_index(
            required_u32_value(value, "output_index")?,
            StreamingBlockKind::Text,
        )?;
        output.push_back(ModelStreamEvent::TextDelta {
            index,
            delta: required_string_value(value, "delta")?.to_owned(),
        });
        Ok(())
    }

    /// 处理推理摘要增量。
    fn consume_reasoning_summary_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = self.streaming_output_index(
            required_u32_value(value, "output_index")?,
            StreamingBlockKind::Reasoning,
        )?;
        output.push_back(ModelStreamEvent::ReasoningSummaryDelta {
            index,
            delta: required_string_value(value, "delta")?.to_owned(),
        });
        Ok(())
    }

    /// 处理可展示推理文本增量。
    fn consume_reasoning_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = self.streaming_output_index(
            required_u32_value(value, "output_index")?,
            StreamingBlockKind::Reasoning,
        )?;
        output.push_back(ModelStreamEvent::ReasoningDelta {
            index,
            delta: required_string_value(value, "delta")?.to_owned(),
        });
        Ok(())
    }

    /// 处理函数参数字符串增量。
    fn consume_function_arguments_delta(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32_value(value, "output_index")?;
        let pending = self
            .tools
            .get_mut(&index)
            .ok_or_else(|| protocol_error("Responses 函数参数早于 output item"))?;
        if pending.ended || pending.arguments_done {
            return Err(protocol_error("Responses 函数参数结束后仍收到参数增量"));
        }
        let delta = required_string_value(value, "delta")?.to_owned();
        if delta.is_empty() {
            return Ok(());
        }
        output.push_back(ModelStreamEvent::ToolCallArgumentsDelta {
            index: pending.output_index,
            id: pending.call_id.clone(),
            delta: delta.clone(),
        });
        pending.arguments.push_str(&delta);
        pending.arguments_seen = true;
        Ok(())
    }

    /// 校验函数参数完成事件绑定既有调用，并在只提供完整参数时补齐唯一增量。
    fn consume_function_arguments_done(
        &mut self,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        let index = required_u32_value(value, "output_index")?;
        let pending = self
            .tools
            .get_mut(&index)
            .ok_or_else(|| protocol_error("Responses 函数参数完成事件早于 output item"))?;
        if pending.ended {
            return Err(protocol_error("Responses 函数调用结束后仍收到参数完成事件"));
        }
        if pending.arguments_done {
            return Err(protocol_error("Responses 函数参数完成事件重复"));
        }
        if let Some(arguments) = optional_string_value(value, "arguments")? {
            complete_function_arguments(pending, arguments, output)?;
        }
        pending.arguments_done = true;
        Ok(())
    }

    /// 处理 Responses 完成、不完整或取消终态。
    fn consume_response_terminal(
        &mut self,
        event_type: &str,
        value: &Value,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        require_started(self.started)?;
        for pending in self.tools.values() {
            if !pending.ended {
                return Err(protocol_error("Responses 终态前仍有未结束的函数调用"));
            }
        }
        let response = value
            .get("response")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("Responses 终态事件缺少 response"))?;
        validate_terminal_status(event_type, response)?;
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            output.push_back(ModelStreamEvent::Usage {
                usage: decode_usage(usage),
            });
        }
        let stop_reason = match event_type {
            "response.cancelled" => StopReason::Cancelled,
            _ => response_stop_reason(response, self.saw_tool_call, self.saw_refusal),
        };
        output.push_back(ModelStreamEvent::MessageEnd { stop_reason });
        self.ended = true;
        Ok(())
    }

    /// 创建一个 Responses 函数调用开始状态并输出开始事件。
    fn start_function_call(
        &mut self,
        remote_index: u32,
        item: &Map<String, Value>,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| protocol_error("Responses function_call 缺少 call_id"))?
            .to_owned();
        let name = required_str(item, "name")?.to_owned();
        if call_id.trim().is_empty() || name.trim().is_empty() {
            return Err(protocol_error(
                "Responses function_call 的 call_id 和 name 不能为空",
            ));
        }
        if let Some(existing) = self.tools.get(&remote_index) {
            if existing.call_id != call_id || existing.name != name {
                return Err(protocol_error(format!(
                    "Responses output_index {remote_index} 的函数调用标识或名称发生变化"
                )));
            }
            return Err(protocol_error(format!(
                "Responses output_index {remote_index} 重复开始函数调用"
            )));
        }
        let initial_arguments = optional_string_from_map(item, "arguments")?.unwrap_or_default();
        let output_index = self.streaming_output_index(remote_index, StreamingBlockKind::Tool)?;
        output.push_back(ModelStreamEvent::ToolCallStart {
            index: output_index,
            id: call_id.clone(),
            name: name.clone(),
        });
        self.tools.insert(
            remote_index,
            PendingFunctionCall {
                output_index,
                call_id,
                name,
                arguments: String::new(),
                arguments_seen: false,
                arguments_done: false,
                ended: false,
            },
        );
        self.saw_tool_call = true;
        if !initial_arguments.is_empty() {
            let pending = self
                .tools
                .get_mut(&remote_index)
                .ok_or_else(|| protocol_error("Responses 函数调用状态丢失"))?;
            complete_function_arguments(pending, initial_arguments, output)?;
        }
        Ok(())
    }

    /// 为一组远端序号与语义类型分配按首次出现顺序递增的唯一内容块序号。
    fn streaming_output_index(
        &mut self,
        remote_index: u32,
        kind: StreamingBlockKind,
    ) -> Result<u32, ModelError> {
        if let Some(index) = self.streaming_indices.get(&(remote_index, kind)) {
            return Ok(*index);
        }
        let index = self.next_content_index;
        self.next_content_index = self
            .next_content_index
            .checked_add(1)
            .ok_or_else(|| protocol_error("Responses 流内容块序号溢出"))?;
        self.streaming_indices.insert((remote_index, kind), index);
        Ok(index)
    }

    /// 保存 Responses reasoning output item 的完整不透明续传状态且避免重复输出。
    ///
    /// Responses 的无状态续传要求后续请求回放完整的 reasoning output item，不能只
    /// 保留加密正文。这里故意把整个协议对象放进 Provider 中立的 opaque JSON；Adapter
    /// 只在外层确认它来自 reasoning item，不把厂商 DTO 泄漏到模型层，也不猜测或过滤
    /// 未知字段。这样未来新增、但仍被 Responses 接受的字段会随原对象原样回放。
    fn emit_reasoning_state(
        &mut self,
        index: u32,
        item: &Map<String, Value>,
        output: &mut VecDeque<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        if self.emitted_reasoning_states.contains(&index) {
            return Ok(());
        }
        // 未知字段保持原样；Responses input item 本身就是协议边界上的不透明 JSON。
        // `type` 由调用方的 discriminator 校验保证为 `reasoning`，完整 item 仍需保留。
        let state = item.clone();
        if state.len() <= 1 {
            return Ok(());
        }
        output.push_back(ModelStreamEvent::ReasoningContinuation {
            index,
            continuation: OpaqueReasoningState::new(REASONING_STATE_KIND, Value::Object(state)),
        });
        self.emitted_reasoning_states.insert(index);
        Ok(())
    }

    /// 解码一个完整 Responses output item。
    fn decode_complete_item(
        &mut self,
        index: u32,
        item: &Value,
        events: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ModelError> {
        let object = item
            .as_object()
            .ok_or_else(|| protocol_error("Responses output item 必须是对象"))?;
        match required_str(object, "type")? {
            "message" => {
                if let Some(content) = object.get("content").filter(|value| !value.is_null()) {
                    let content = content
                        .as_array()
                        .ok_or_else(|| protocol_error("Responses message content 必须是数组"))?;
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => events.push(ModelStreamEvent::TextDelta {
                                index,
                                delta: part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| protocol_error("output_text 缺少 text"))?
                                    .to_owned(),
                            }),
                            Some("refusal") => {
                                self.saw_refusal = true;
                                events.push(ModelStreamEvent::TextDelta {
                                    index,
                                    delta: part
                                        .get("refusal")
                                        .and_then(Value::as_str)
                                        .ok_or_else(|| protocol_error("refusal 缺少文本"))?
                                        .to_owned(),
                                });
                            }
                            Some(other) => {
                                return Err(protocol_error(format!(
                                    "Responses message 包含未知 part {other}"
                                )));
                            }
                            None => {
                                return Err(protocol_error("Responses message part 缺少 type"));
                            }
                        }
                    }
                }
            }
            "reasoning" => {
                if let Some(content) = object.get("content").filter(|value| !value.is_null()) {
                    let content = content
                        .as_array()
                        .ok_or_else(|| protocol_error("Responses reasoning content 必须是数组"))?;
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("reasoning_text" | "output_text") => {
                                events.push(ModelStreamEvent::ReasoningDelta {
                                    index,
                                    delta: part
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .ok_or_else(|| protocol_error("reasoning_text 缺少 text"))?
                                        .to_owned(),
                                });
                            }
                            Some(other) => {
                                return Err(protocol_error(format!(
                                    "Responses reasoning 包含未知 content part {other}"
                                )));
                            }
                            None => {
                                return Err(protocol_error(
                                    "Responses reasoning content part 缺少 type",
                                ));
                            }
                        }
                    }
                }
                if let Some(summary) = object.get("summary").and_then(Value::as_array) {
                    for part in summary {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            events.push(ModelStreamEvent::ReasoningSummaryDelta {
                                index,
                                delta: text.to_owned(),
                            });
                        }
                    }
                }
                let mut queue = VecDeque::new();
                self.emit_reasoning_state(index, object, &mut queue)?;
                events.extend(queue);
            }
            "function_call" => {
                let call_id = object
                    .get("call_id")
                    .or_else(|| object.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol_error("Responses function_call 缺少 call_id"))?
                    .to_owned();
                let name = required_str(object, "name")?.to_owned();
                if call_id.trim().is_empty() || name.trim().is_empty() {
                    return Err(protocol_error(
                        "Responses function_call 的 call_id 和 name 不能为空",
                    ));
                }
                events.push(ModelStreamEvent::ToolCallStart {
                    index,
                    id: call_id.clone(),
                    name,
                });
                events.push(ModelStreamEvent::ToolCallArgumentsDelta {
                    index,
                    id: call_id.clone(),
                    delta: required_str(object, "arguments")?.to_owned(),
                });
                events.push(ModelStreamEvent::ToolCallEnd { index, id: call_id });
                self.saw_tool_call = true;
            }
            other => {
                return Err(protocol_error(format!(
                    "Responses 包含未知 output item {other}"
                )));
            }
        }
        Ok(())
    }
}

/// 只有能安全增量归并且不会丢弃完整终态正文的 Responses 事件可以触发惰性起始。
fn is_lazy_start_content_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.output_item.added"
            | "response.content_part.added"
            | "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
    )
}

/// 按消息角色把 Provider 中立历史编码为 Responses input items。
fn encode_input_message(
    role: MessageRole,
    blocks: &[ContentBlock],
    output: &mut Vec<Value>,
) -> Result<(), ModelError> {
    match role {
        MessageRole::System | MessageRole::Developer | MessageRole::User => {
            let role_name = match role {
                MessageRole::System => "system",
                MessageRole::Developer => "developer",
                MessageRole::User => "user",
                MessageRole::Assistant | MessageRole::Tool => unreachable!(),
            };
            let content = blocks
                .iter()
                .map(|block| encode_input_content(role, block))
                .collect::<Result<Vec<_>, _>>()?;
            output.push(json!({
                "type": "message",
                "role": role_name,
                "content": content,
            }));
        }
        MessageRole::Assistant => encode_assistant_items(blocks, output)?,
        MessageRole::Tool => {
            for block in blocks {
                output.push(encode_function_output(block)?);
            }
        }
    }
    Ok(())
}

/// 编码 Responses 普通输入消息的一个内容 part。
fn encode_input_content(role: MessageRole, block: &ContentBlock) -> Result<Value, ModelError> {
    match block {
        ContentBlock::Text { text } => Ok(json!({ "type": "input_text", "text": text })),
        ContentBlock::Image { image } if matches!(role, MessageRole::User) => Ok(json!({
            "type": "input_image",
            "image_url": image_url(&image.source),
        })),
        _ => Err(invalid_request(
            "Responses system/developer 只允许文本，user 只允许文本或图片",
        )),
    }
}

/// 按内容顺序编码 Responses assistant 消息、推理 item 和函数调用。
fn encode_assistant_items(
    blocks: &[ContentBlock],
    output: &mut Vec<Value>,
) -> Result<(), ModelError> {
    let mut pending_text = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => pending_text.push_str(text),
            ContentBlock::Reasoning { reasoning } => {
                flush_assistant_text(&mut pending_text, output);
                if let Some(state) = &reasoning.continuation {
                    if state.kind != REASONING_STATE_KIND {
                        return Err(invalid_request(format!(
                            "Responses 无法解释推理续传状态 {}",
                            state.kind
                        )));
                    }
                    let mut item = state
                        .data
                        .as_object()
                        .cloned()
                        .ok_or_else(|| invalid_request("Responses 推理状态必须是对象"))?;
                    item.insert("type".to_owned(), Value::String("reasoning".to_owned()));
                    output.push(Value::Object(item));
                }
            }
            ContentBlock::ToolCall { tool_call } => {
                flush_assistant_text(&mut pending_text, output);
                output.push(json!({
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": serde_json::to_string(&tool_call.arguments).map_err(|error| {
                        invalid_request(format!("Responses 工具参数无法编码：{error}"))
                    })?,
                }));
            }
            ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
                return Err(invalid_request(
                    "Responses assistant 消息包含不支持的内容块",
                ));
            }
        }
    }
    flush_assistant_text(&mut pending_text, output);
    Ok(())
}

/// 把累计的 assistant 文本输出为一个 Responses message item。
fn flush_assistant_text(text: &mut String, output: &mut Vec<Value>) {
    if text.is_empty() {
        return;
    }
    output.push(json!({
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": std::mem::take(text) }],
    }));
}

/// 编码 Responses `function_call_output` item。
fn encode_function_output(block: &ContentBlock) -> Result<Value, ModelError> {
    let ContentBlock::ToolResult { tool_result } = block else {
        return Err(invalid_request("Responses 工具消息只能包含工具结果"));
    };
    // 纯文本工具结果采用字符串；含图片时采用保持各内容块原始顺序的结构化数组。
    let output = if tool_result
        .content
        .iter()
        .any(|item| matches!(item, ToolResultContent::Image { .. }))
    {
        Value::Array(
            tool_result
                .content
                .iter()
                .map(|item| match item {
                    ToolResultContent::Text { text } => {
                        json!({ "type": "input_text", "text": text })
                    }
                    ToolResultContent::Image { image } => json!({
                        "type": "input_image",
                        "image_url": image_url(&image.source),
                    }),
                })
                .collect(),
        )
    } else {
        let mut text = String::new();
        for item in &tool_result.content {
            if let ToolResultContent::Text { text: part } = item {
                text.push_str(part);
            }
        }
        Value::String(text)
    };
    Ok(json!({
        "type": "function_call_output",
        "call_id": tool_result.tool_call_id,
        "output": output,
    }))
}

/// 将图片来源转换为 Responses `input_image.image_url`。
fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
    }
}

/// 编码 Responses 工具选择策略。
fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Required => Value::String("required".to_owned()),
        ToolChoice::Specific { name } => json!({ "type": "function", "name": name }),
    }
}

/// 映射 Provider 中立推理强度到 Responses 字段。
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

/// 从 Responses JSON 对象提取响应元数据。
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

/// 解析 Responses Usage，并保持缺失值为 `None`。
fn decode_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
        reasoning_tokens: value
            .get("output_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        cache_read_tokens: value
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64),
        cache_write_tokens: None,
        total_tokens: value.get("total_tokens").and_then(Value::as_u64),
    }
}

/// 根据 Responses status、incomplete details 和工具调用映射结束原因。
fn response_stop_reason(
    response: &Map<String, Value>,
    saw_tool_call: bool,
    saw_refusal: bool,
) -> StopReason {
    if saw_tool_call {
        return StopReason::ToolUse;
    }
    if saw_refusal {
        return StopReason::ContentFilter;
    }
    let status = response.get("status").and_then(Value::as_str);
    let detail = response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    match (status, detail) {
        (Some("completed"), _) => StopReason::Completed,
        (Some("cancelled"), _) => StopReason::Cancelled,
        (Some("incomplete"), Some("max_output_tokens" | "max_completion_tokens")) => {
            StopReason::MaxOutputTokens
        }
        (Some("incomplete"), Some("content_filter")) => StopReason::ContentFilter,
        (_, Some(reason)) => StopReason::Other {
            reason: reason.to_owned(),
        },
        (Some(status), None) => StopReason::Other {
            reason: status.to_owned(),
        },
        (None, None) => StopReason::Other {
            reason: "missing_status".to_owned(),
        },
    }
}

/// 提取 Provider 错误对象中的安全文本摘要。
fn provider_error_message(value: &Value) -> String {
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("Responses Provider 返回未说明错误")
        .to_owned()
}

/// 判断顶层或嵌套响应是否包含 Provider 明确失败事实。
fn has_explicit_provider_failure(value: &Value) -> bool {
    let nested_response = value.get("response");
    value.get("error").is_some_and(|error| !error.is_null())
        || nested_response
            .and_then(|response| response.get("error"))
            .is_some_and(|error| !error.is_null())
        || value.get("status").and_then(Value::as_str) == Some("failed")
        || nested_response
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            == Some("failed")
}

/// 校验终态事件与存在的 `response.status` 类型和语义完全一致。
fn validate_terminal_status(
    event_type: &str,
    response: &Map<String, Value>,
) -> Result<(), ModelError> {
    let Some(status) = response.get("status") else {
        return Ok(());
    };
    let status = status
        .as_str()
        .ok_or_else(|| protocol_error("Responses 终态 response.status 必须是字符串"))?;
    let expected = match event_type {
        "response.completed" => "completed",
        "response.incomplete" => "incomplete",
        "response.cancelled" => "cancelled",
        _ => return Err(protocol_error("Responses 终态事件类型无效")),
    };
    if status != expected {
        return Err(protocol_error(
            "Responses 终态事件与 response.status 不一致",
        ));
    }
    Ok(())
}

/// 只把具有明确上下文超限证据的 Responses 错误归一为稳定错误类型。
fn classify_provider_error(value: &Value) -> ModelError {
    let message = provider_error_message(value);
    let error = value
        .get("error")
        .filter(|error| error.is_object())
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
                .filter(|error| error.is_object())
        });
    let code = error
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
        .ok_or_else(|| protocol_error(format!("Responses 字段 {field} 必须是字符串")))
}

/// 从顶层 JSON 值读取必需字符串字段。
fn required_string_value<'a>(value: &'a Value, field: &str) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error(format!("Responses 字段 {field} 必须是字符串")))
}

/// 从对象读取允许缺失或显式 null 的可选字符串，并拒绝其他类型的伪字段。
fn optional_string_from_map<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, ModelError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(protocol_error(format!(
            "Responses 字段 {field} 必须是字符串或 null"
        ))),
    }
}

/// 从顶层 JSON 对象读取允许缺失或显式 null 的可选字符串。
fn optional_string_value<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, ModelError> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol_error("Responses 事件必须是 JSON 对象"))?;
    optional_string_from_map(object, field)
}

/// 合并一个函数调用的完整参数，并避免 done 事件重复发出相同参数。
fn complete_function_arguments(
    pending: &mut PendingFunctionCall,
    arguments: &str,
    output: &mut VecDeque<ModelStreamEvent>,
) -> Result<(), ModelError> {
    if pending.arguments_seen {
        if pending.arguments != arguments {
            return Err(protocol_error("Responses 函数参数增量与完成参数不一致"));
        }
        return Ok(());
    }
    pending.arguments_seen = true;
    pending.arguments.push_str(arguments);
    if !arguments.is_empty() {
        output.push_back(ModelStreamEvent::ToolCallArgumentsDelta {
            index: pending.output_index,
            id: pending.call_id.clone(),
            delta: arguments.to_owned(),
        });
    }
    Ok(())
}

/// 从顶层 JSON 值读取可转换为 u32 的必需整数。
fn required_u32_value(value: &Value, field: &str) -> Result<u32, ModelError> {
    let number = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol_error(format!("Responses 字段 {field} 必须是非负整数")))?;
    u32::try_from(number)
        .map_err(|_| protocol_error(format!("Responses 字段 {field} 超过 u32 范围")))
}

/// 要求 SSE 已经收到响应开始事件。
fn require_started(started: bool) -> Result<(), ModelError> {
    if started {
        Ok(())
    } else {
        Err(protocol_error("Responses 内容事件早于 response.created"))
    }
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
