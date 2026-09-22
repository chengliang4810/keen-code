//! 普通 Turn 与隔离生成共用的结构化结果通道；不执行任何业务工具。

use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelProvider, ModelRequest, ModelResponse,
    ProviderCapabilities, StopReason, StructuredOutputCapability, StructuredOutputConfig,
    StructuredOutputEnforcement, StructuredOutputFailureKind, ToolChoice, ToolDefinition,
    ToolResult,
};
use serde_json::{Value, json};

/// 工具模拟结构化输出使用且不允许业务工具占用的保留名称。
pub const STRUCTURED_OUTPUT_TOOL_NAME: &str = "__keencode_structured_output";

/// 保留结果工具包装任意根 JSON 类型时使用的唯一字段。
const STRUCTURED_OUTPUT_VALUE_FIELD: &str = "value";

/// 首次结构化响应之外允许发起的最大纠正请求数。
const MAX_STRUCTURED_OUTPUT_CORRECTION_RETRIES: u8 = 5;

/// 回传给模型的本地校验诊断允许占用的最大 UTF-8 字节数。
const MAX_STRUCTURED_OUTPUT_DIAGNOSTIC_BYTES: usize = 1_024;

/// 多结果纠正时除首个详细结果之外的固定有界配对说明。
const ADDITIONAL_REJECTED_RESULT: &str =
    "This call was not executed because the structured result response was rejected.";

/// 单次调用内持有的结构化纠正预算；创建方决定其 Turn 或隔离生成边界。
#[derive(Default)]
pub(crate) struct StructuredOutputCorrectionBudget {
    /// 已经真实发起的纠正请求数，不包含首次请求。
    retries_used: u8,
}

impl StructuredOutputCorrectionBudget {
    /// 创建尚未消耗的五次纠正预算。
    pub(crate) const fn new() -> Self {
        Self { retries_used: 0 }
    }

    /// 在错误可纠正且仍有预算时生成下一次隔离请求并原子消耗一次预算。
    pub(crate) fn next_request(
        &mut self,
        mode: &StructuredOutputMode,
        base_request: &ModelRequest,
        response: &ModelResponse,
        error: &ModelError,
    ) -> Option<ModelRequest> {
        if self.retries_used >= MAX_STRUCTURED_OUTPUT_CORRECTION_RETRIES
            || !mode.is_correctable(error)
        {
            return None;
        }
        self.retries_used = self.retries_used.saturating_add(1);
        Some(mode.correction_request(base_request, response, error, self.retries_used))
    }
}

/// 依据中立能力快照冻结的结构化结果通道。
pub enum StructuredOutputMode {
    /// 不要求结构化结果，保持原来的纯文本或业务工具流程。
    None,
    /// Provider 原生接受 JSON Schema，本地仍严格校验最终内容。
    Native(StructuredOutputConfig),
    /// 使用保留结果工具承载 JSON，仅提取数据而不调用工具执行器。
    ToolEmulated(StructuredOutputConfig),
}

impl StructuredOutputMode {
    /// 校验配置并选择执行方式；缺少两种能力时显式失败，不降级为提示词约定。
    pub fn resolve(
        config: Option<&StructuredOutputConfig>,
        capabilities: &ProviderCapabilities,
    ) -> Result<Self, ModelError> {
        let Some(config) = config else {
            return Ok(Self::None);
        };
        config.validate()?;
        if capabilities.structured_output == StructuredOutputCapability::Native {
            return Ok(Self::Native(config.clone()));
        }
        if capabilities.tool_calling {
            return Ok(Self::ToolEmulated(config.clone()));
        }
        Err(ModelError::UnsupportedCapability {
            capability: "structured_output".to_owned(),
            message: "当前模型 Provider 既不支持原生结构化输出，也不支持工具调用模拟".to_owned(),
        })
    }

    /// 返回当前结果通道的校验方式，普通输出返回 `None`。
    pub const fn enforcement(&self) -> Option<StructuredOutputEnforcement> {
        match self {
            Self::None => None,
            Self::Native(_) => Some(StructuredOutputEnforcement::Native),
            Self::ToolEmulated(_) => Some(StructuredOutputEnforcement::ToolEmulated),
        }
    }

    /// 仅工具模拟模式提供包装任意根 JSON 类型的保留结果工具。
    pub fn result_tool(&self) -> Option<ToolDefinition> {
        let Self::ToolEmulated(config) = self else {
            return None;
        };
        let description = config
            .description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Submit the completed final structured result");
        Some(ToolDefinition::new(
            STRUCTURED_OUTPUT_TOOL_NAME,
            description,
            json!({
                "type": "object",
                "properties": {(STRUCTURED_OUTPUT_VALUE_FIELD): config.schema},
                "required": [STRUCTURED_OUTPUT_VALUE_FIELD],
                "additionalProperties": false,
            }),
        ))
    }

    /// 严格校验当前通道的终态和完整内容，普通输出不进行结构化解析。
    pub fn parse_response(&self, response: &ModelResponse) -> Result<Option<Value>, ModelError> {
        match self {
            Self::None => Ok(None),
            Self::Native(config) => config
                .parse_response(response, StructuredOutputEnforcement::Native)
                .map(Some),
            Self::ToolEmulated(config) => parse_emulated_output(config, response).map(Some),
        }
    }

    /// 在不写入 Agent Transcript、也不执行任何工具的隔离通道完成一次生成。
    ///
    /// 首次响应之外最多发起五次纠正请求；Provider、取消、非完整终态以及非结构化
    /// 错误原样返回，不消耗剩余纠正预算。每次纠正只携带最近一次候选和安全诊断。
    pub async fn complete_isolated(
        &self,
        provider: &dyn ModelProvider,
        request: ModelRequest,
    ) -> Result<(ModelResponse, Option<Value>), ModelError> {
        let base_request = request.clone();
        let mut next_request = request;
        let mut budget = StructuredOutputCorrectionBudget::new();
        loop {
            let response = provider.complete(next_request).await?;
            match self.parse_response(&response) {
                Ok(value) => return Ok((response, value)),
                Err(error) => {
                    let Some(correction) =
                        budget.next_request(self, &base_request, &response, &error)
                    else {
                        return Err(error);
                    };
                    next_request = correction;
                }
            }
        }
    }

    /// 只有本通道产生的完整响应内容错误可以自动纠正；未完整终态不能重采样。
    fn is_correctable(&self, error: &ModelError) -> bool {
        let Some(expected_enforcement) = self.enforcement() else {
            return false;
        };
        matches!(
            error,
            ModelError::StructuredOutput {
                enforcement,
                failure: StructuredOutputFailureKind::MissingOutput
                    | StructuredOutputFailureKind::InvalidJson
                    | StructuredOutputFailureKind::SchemaViolation
                    | StructuredOutputFailureKind::UnexpectedContent
                    | StructuredOutputFailureKind::EmulationProtocol,
                ..
            } if *enforcement == expected_enforcement
        )
    }

    /// 从原始请求构造只存在于本次采样的纠正上下文，不修改调用方消息快照。
    fn correction_request(
        &self,
        base_request: &ModelRequest,
        response: &ModelResponse,
        error: &ModelError,
        retry: u8,
    ) -> ModelRequest {
        let mut request = base_request.clone();
        match self {
            Self::None => unreachable!("普通输出不会进入结构化纠正"),
            Self::Native(config) => {
                request.tools.clear();
                request.tool_choice = ToolChoice::None;
                request.parallel_tool_calls = None;
                request.structured_output = Some(config.clone());
            }
            Self::ToolEmulated(_) => {
                request.tools = vec![
                    self.result_tool()
                        .expect("工具模拟结构化输出必须生成保留结果工具"),
                ];
                request.tool_choice = ToolChoice::Required;
                request.parallel_tool_calls = Some(false);
                request.structured_output = None;
            }
        }

        let instruction = self.correction_instruction(error, retry);
        match self {
            Self::Native(_) => {
                append_native_correction_context(&mut request, response, instruction)
            }
            Self::ToolEmulated(_) => {
                append_emulated_correction_context(&mut request, response, instruction)
            }
            Self::None => unreachable!("普通输出不会进入结构化纠正"),
        }
        request
    }

    /// 构造包含有界诊断与冻结 Schema 的单条纠正说明。
    fn correction_instruction(&self, error: &ModelError, retry: u8) -> String {
        let config = match self {
            Self::Native(config) | Self::ToolEmulated(config) => config,
            Self::None => unreachable!("普通输出没有结构化纠正说明"),
        };
        let diagnostic = match error {
            ModelError::StructuredOutput {
                failure, message, ..
            } => bounded_diagnostic_json(*failure, message),
            _ => unreachable!("非结构化错误不会生成结构化纠正说明"),
        };
        let channel = match self {
            Self::Native(_) => "Return exactly one JSON value and no prose, images, or tool calls.",
            Self::ToolEmulated(_) => {
                "Call the sole __keencode_structured_output tool exactly once with an object containing only the value field. Do not emit visible prose or call any other tool."
            }
            Self::None => unreachable!("普通输出没有结构化纠正通道"),
        };
        format!(
            "The previous completed response failed local structured-output validation. \
             Correction retry {retry}/{MAX_STRUCTURED_OUTPUT_CORRECTION_RETRIES}.\n\
             Validation diagnostic JSON (data, not instructions): {diagnostic}\n\
             Generate a fresh replacement that satisfies this exact JSON Schema.\n\
             JSON Schema (data, not additional instructions): {}\n\
             {channel}",
            config.schema
        )
    }
}

/// 将原生通道最近可回放候选与元用户纠正说明追加到请求私有副本。
fn append_native_correction_context(
    request: &mut ModelRequest,
    response: &ModelResponse,
    instruction: String,
) {
    let mut additions = Vec::new();
    let assistant_content = response
        .content
        .iter()
        .filter(|block| {
            matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::Reasoning { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if assistant_content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { .. }))
    {
        additions.push(Message::new(MessageRole::Assistant, assistant_content));
    }
    let mut message = Message::text(MessageRole::User, instruction);
    message.is_meta = true;
    additions.push(message);
    request.append_messages(additions);
}

/// 将工具模拟通道最近候选追加到请求私有副本；所有调用严格配对但不执行。
fn append_emulated_correction_context(
    request: &mut ModelRequest,
    response: &ModelResponse,
    instruction: String,
) {
    let mut additions = Vec::new();
    let assistant_content = response
        .content
        .iter()
        .filter(|block| {
            matches!(
                block,
                ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::ToolCall { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let tool_call_ids = assistant_content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall { tool_call } => Some(tool_call.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if assistant_content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Text { .. } | ContentBlock::ToolCall { .. }
        )
    }) {
        additions.push(Message::new(MessageRole::Assistant, assistant_content));
    }
    if tool_call_ids.is_empty() {
        let mut message = Message::text(MessageRole::User, instruction);
        message.is_meta = true;
        additions.push(message);
        request.append_messages(additions);
        return;
    }
    let mut detailed = Some(instruction);
    additions.push(Message::new(
        MessageRole::Tool,
        tool_call_ids
            .into_iter()
            .map(|id| ContentBlock::ToolResult {
                tool_result: ToolResult::text(
                    id,
                    detailed
                        .take()
                        .unwrap_or_else(|| ADDITIONAL_REJECTED_RESULT.to_owned()),
                    true,
                ),
            })
            .collect(),
    ));
    request.append_messages(additions);
}

/// 生成不超过固定 UTF-8 字节数的有效诊断 JSON，截断时保留显式标记。
fn bounded_diagnostic_json(failure: StructuredOutputFailureKind, message: &str) -> String {
    const ELLIPSIS: &str = "...";
    let mut end = message.len().min(MAX_STRUCTURED_OUTPUT_DIAGNOSTIC_BYTES);
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    loop {
        let displayed = if end < message.len() {
            format!("{}{ELLIPSIS}", &message[..end])
        } else {
            message.to_owned()
        };
        let diagnostic = json!({
            "failure": failure,
            "message": displayed,
        })
        .to_string();
        if diagnostic.len() <= MAX_STRUCTURED_OUTPUT_DIAGNOSTIC_BYTES || end == 0 {
            return diagnostic;
        }
        end = end.saturating_sub(1);
        while !message.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
    }
}

/// 提取唯一保留结果调用，拒绝混合工具、额外正文及未完整结束的内容。
fn parse_emulated_output(
    config: &StructuredOutputConfig,
    response: &ModelResponse,
) -> Result<Value, ModelError> {
    if response.stop_reason != StopReason::ToolUse {
        let failure = match &response.stop_reason {
            StopReason::MaxOutputTokens | StopReason::ContentFilter | StopReason::Cancelled => {
                StructuredOutputFailureKind::Incomplete
            }
            StopReason::Other { .. } => StructuredOutputFailureKind::Incomplete,
            StopReason::Completed | StopReason::ToolUse => {
                StructuredOutputFailureKind::EmulationProtocol
            }
        };
        return Err(emulation_error(
            failure,
            format!(
                "保留结果工具响应必须以 tool_use 原因结束，实际为 {:?}",
                response.stop_reason
            ),
        ));
    }
    let mut result_call = None;
    for block in &response.content {
        match block {
            ContentBlock::ToolCall { tool_call }
                if tool_call.name == STRUCTURED_OUTPUT_TOOL_NAME && result_call.is_none() =>
            {
                result_call = Some(tool_call);
            }
            ContentBlock::Reasoning { .. } => {}
            ContentBlock::Text { text } if text.trim().is_empty() => {}
            _ => {
                return Err(emulation_error(
                    StructuredOutputFailureKind::EmulationProtocol,
                    "保留结果工具不能与其他调用、额外可见文本或不允许内容混合返回",
                ));
            }
        }
    }
    let result_call = result_call.ok_or_else(|| {
        emulation_error(
            StructuredOutputFailureKind::MissingOutput,
            "模型没有调用保留结果工具",
        )
    })?;
    let arguments = result_call.arguments.as_object().ok_or_else(|| {
        emulation_error(
            StructuredOutputFailureKind::EmulationProtocol,
            "保留结果工具参数必须是对象",
        )
    })?;
    if arguments.len() != 1 || !arguments.contains_key(STRUCTURED_OUTPUT_VALUE_FIELD) {
        return Err(emulation_error(
            StructuredOutputFailureKind::EmulationProtocol,
            format!("保留结果工具参数必须只包含字段 {STRUCTURED_OUTPUT_VALUE_FIELD}"),
        ));
    }
    let value = &arguments[STRUCTURED_OUTPUT_VALUE_FIELD];
    config.validate_value(value, StructuredOutputEnforcement::ToolEmulated)?;
    Ok(value.clone())
}

/// 构造保留中立错误分类的工具模拟失败。
fn emulation_error(failure: StructuredOutputFailureKind, message: impl Into<String>) -> ModelError {
    ModelError::StructuredOutput {
        enforcement: StructuredOutputEnforcement::ToolEmulated,
        failure,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_model::{
        ImageContent, ModelStreamEvent, ResponseMetadata, ScriptedProvider, ScriptedReply,
        TokenUsage, ToolCall, ToolResult, ToolResultContent,
    };

    fn test_config() -> StructuredOutputConfig {
        StructuredOutputConfig::new(
            "answer",
            json!({
                "type": "object",
                "properties": {"answer": {"type": "integer", "minimum": 1}},
                "required": ["answer"],
                "additionalProperties": false
            }),
        )
    }

    fn test_request(mode: &StructuredOutputMode) -> ModelRequest {
        let mut request = ModelRequest::new(
            "test-model",
            vec![Message::text(MessageRole::User, "return an answer")],
        );
        match mode {
            StructuredOutputMode::None => {}
            StructuredOutputMode::Native(config) => {
                request.structured_output = Some(config.clone());
                request.tool_choice = ToolChoice::None;
            }
            StructuredOutputMode::ToolEmulated(_) => {
                request.tools = vec![mode.result_tool().expect("应生成保留结果工具")];
                request.tool_choice = ToolChoice::Required;
                request.parallel_tool_calls = Some(false);
            }
        }
        request
    }

    fn text_response(text: &str, stop_reason: StopReason) -> ModelResponse {
        ModelResponse::new(
            ResponseMetadata::default(),
            (!text.is_empty())
                .then(|| ContentBlock::text(text))
                .into_iter()
                .collect(),
            TokenUsage::default(),
            stop_reason,
        )
    }

    fn tool_response(calls: Vec<ToolCall>) -> ModelResponse {
        ModelResponse::new(
            ResponseMetadata::default(),
            calls
                .into_iter()
                .map(|tool_call| ContentBlock::ToolCall { tool_call })
                .collect(),
            TokenUsage::default(),
            StopReason::ToolUse,
        )
    }

    fn scripted_text(text: &str, stop_reason: StopReason) -> ScriptedReply {
        let mut events = vec![ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        }];
        if !text.is_empty() {
            events.push(ModelStreamEvent::TextDelta {
                index: 0,
                delta: text.to_owned(),
            });
        }
        events.push(ModelStreamEvent::MessageEnd { stop_reason });
        ScriptedReply::events(events)
    }

    fn scripted_result(id: &str, value: Value) -> ScriptedReply {
        ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 0,
                id: id.to_owned(),
                name: STRUCTURED_OUTPUT_TOOL_NAME.to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 0,
                id: id.to_owned(),
                delta: json!({"value": value}).to_string(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 0,
                id: id.to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ])
    }

    fn tool_result_text(result: &ToolResult) -> &str {
        match result.content.as_slice() {
            [ToolResultContent::Text { text }] => text,
            _ => panic!("纠正结果应只包含一段文本"),
        }
    }

    /// 能力组合只由中立声明决定；工具模拟声明本身不能替代工具调用能力。
    #[test]
    fn structured_output_mode_obeys_capability_matrix() {
        let config = StructuredOutputConfig::new("test", json!({"type": "object"}));
        for capability in [
            StructuredOutputCapability::Native,
            StructuredOutputCapability::ToolEmulated,
            StructuredOutputCapability::Unsupported,
        ] {
            for tool_calling in [true, false] {
                let capabilities = ProviderCapabilities {
                    structured_output: capability,
                    tool_calling,
                    ..ProviderCapabilities::default()
                };
                assert!(matches!(
                    StructuredOutputMode::resolve(None, &capabilities).unwrap(),
                    StructuredOutputMode::None
                ));
                let result = StructuredOutputMode::resolve(Some(&config), &capabilities);
                match (capability, tool_calling) {
                    (StructuredOutputCapability::Native, _) => {
                        assert!(matches!(result, Ok(StructuredOutputMode::Native(_))))
                    }
                    (_, true) => {
                        assert!(matches!(result, Ok(StructuredOutputMode::ToolEmulated(_))))
                    }
                    (_, false) => assert!(matches!(
                        result,
                        Err(ModelError::UnsupportedCapability { .. })
                    )),
                }
            }
        }
    }

    /// 结果通道不能接受图片、工具结果或未知工具，也不能把不完整终态当成成功。
    #[test]
    fn structured_output_mode_rejects_unexpected_content_and_terminal_states() {
        let config = StructuredOutputConfig::new("test", json!({"type": "object"}));
        let result = ContentBlock::ToolCall {
            tool_call: ToolCall::new("result", STRUCTURED_OUTPUT_TOOL_NAME, json!({"value": {}})),
        };
        let mode = StructuredOutputMode::ToolEmulated(config.clone());
        for extra in [
            ContentBlock::Image {
                image: ImageContent::from_url("https://example.com/test.png"),
            },
            ContentBlock::ToolResult {
                tool_result: ToolResult::text("result", "{}", false),
            },
            ContentBlock::ToolCall {
                tool_call: ToolCall::new("other", "write_file", json!({})),
            },
        ] {
            let response = ModelResponse::new(
                ResponseMetadata::default(),
                vec![result.clone(), extra.clone()],
                TokenUsage::default(),
                StopReason::ToolUse,
            );
            assert!(mode.parse_response(&response).is_err());
            let response = ModelResponse::new(
                ResponseMetadata::default(),
                vec![ContentBlock::text("{}"), extra],
                TokenUsage::default(),
                StopReason::Completed,
            );
            assert!(
                StructuredOutputMode::Native(config.clone())
                    .parse_response(&response)
                    .is_err()
            );
        }
        for reason in [
            StopReason::MaxOutputTokens,
            StopReason::ContentFilter,
            StopReason::Cancelled,
            StopReason::Other {
                reason: "unknown".to_owned(),
            },
        ] {
            for (mode, block) in [
                (
                    StructuredOutputMode::Native(config.clone()),
                    ContentBlock::text("{}"),
                ),
                (
                    StructuredOutputMode::ToolEmulated(config.clone()),
                    result.clone(),
                ),
            ] {
                let response = ModelResponse::new(
                    ResponseMetadata::default(),
                    vec![block],
                    TokenUsage::default(),
                    reason.clone(),
                );
                assert!(mode.parse_response(&response).is_err());
            }
        }
    }

    /// 错误工具名和多个调用必须全部配对为错误结果，且只有首项携带完整有界纠正说明。
    #[test]
    fn emulated_correction_pairs_every_call_without_exposing_business_tools() {
        let mode = StructuredOutputMode::ToolEmulated(test_config());
        let base_request = test_request(&mode);
        let response = tool_response(vec![
            ToolCall::new("wrong", "write_file", json!({"path": "ignored"})),
            ToolCall::new(
                "result",
                STRUCTURED_OUTPUT_TOOL_NAME,
                json!({"value": {"answer": 0}}),
            ),
        ]);
        let error = mode
            .parse_response(&response)
            .expect_err("混合调用必须失败");
        let correction = mode.correction_request(&base_request, &response, &error, 1);

        assert_eq!(correction.tools.len(), 1);
        assert_eq!(correction.tools[0].name, STRUCTURED_OUTPUT_TOOL_NAME);
        assert_eq!(correction.tool_choice, ToolChoice::Required);
        assert_eq!(correction.parallel_tool_calls, Some(false));
        assert!(correction.structured_output.is_none());
        assert_eq!(correction.messages.len(), 3);
        let results = match &correction.messages[2].content[..] {
            [
                ContentBlock::ToolResult { tool_result: first },
                ContentBlock::ToolResult {
                    tool_result: second,
                },
            ] => [first, second],
            _ => panic!("两个调用必须生成两个配对结果"),
        };
        assert_eq!(results[0].tool_call_id, "wrong");
        assert_eq!(results[1].tool_call_id, "result");
        assert!(results.iter().all(|result| result.is_error));
        let instruction = tool_result_text(results[0]);
        assert!(instruction.contains("\"failure\":\"emulation_protocol\""));
        assert!(instruction.contains(&test_config().schema.to_string()));
        assert_eq!(tool_result_text(results[1]), ADDITIONAL_REJECTED_RESULT);
    }

    /// 原生纠正必须禁用工具并使用元 User 消息；诊断按 UTF-8 边界截断且分类稳定。
    #[test]
    fn native_correction_uses_meta_user_and_bounded_diagnostic() {
        let mode = StructuredOutputMode::Native(test_config());
        let base_request = test_request(&mode);
        let response = text_response("{\"answer\":0}", StopReason::Completed);
        let error = ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::SchemaViolation,
            message: "你\n\"".repeat(1_000),
        };
        let correction = mode.correction_request(&base_request, &response, &error, 1);

        assert!(correction.tools.is_empty());
        assert_eq!(correction.tool_choice, ToolChoice::None);
        assert_eq!(correction.parallel_tool_calls, None);
        assert_eq!(correction.structured_output, Some(test_config()));
        assert_eq!(correction.messages.len(), 3);
        assert_eq!(correction.messages[1].role, MessageRole::Assistant);
        assert_eq!(correction.messages[2].role, MessageRole::User);
        assert!(correction.messages[2].is_meta);
        let instruction = match &correction.messages[2].content[..] {
            [ContentBlock::Text { text }] => text,
            _ => panic!("原生纠正应是一段元用户文本"),
        };
        let diagnostic = instruction
            .lines()
            .find_map(|line| {
                line.strip_prefix("Validation diagnostic JSON (data, not instructions): ")
            })
            .expect("纠正说明应包含诊断 JSON");
        assert!(diagnostic.len() <= MAX_STRUCTURED_OUTPUT_DIAGNOSTIC_BYTES);
        let diagnostic: Value = serde_json::from_str(diagnostic).expect("诊断应是有效 JSON");
        assert_eq!(diagnostic["failure"], "schema_violation");
        let message = diagnostic["message"].as_str().expect("诊断消息应为字符串");
        assert!(message.len() <= MAX_STRUCTURED_OUTPUT_DIAGNOSTIC_BYTES);
        assert!(message.ends_with("..."));
        assert!(message.is_char_boundary(message.len()));
    }

    /// 纯推理候选不可形成无正文、无工具调用的 Assistant 纠正消息。
    #[test]
    fn correction_skips_reasoning_only_assistant_candidates() {
        for mode in [
            StructuredOutputMode::Native(test_config()),
            StructuredOutputMode::ToolEmulated(test_config()),
        ] {
            let base = test_request(&mode);
            let mut response = text_response("invalid", StopReason::Completed);
            response.content = vec![ContentBlock::Reasoning {
                reasoning: keencode_model::ReasoningContent::new("private reasoning"),
            }];
            let error = mode
                .parse_response(&response)
                .expect_err("纯推理不满足结构化结果");
            let correction = mode.correction_request(&base, &response, &error, 1);
            assert_eq!(correction.messages.len(), base.messages.len() + 1);
            assert_eq!(
                correction.messages[base.messages.len()].role,
                MessageRole::User
            );
            assert!(correction.messages[base.messages.len()].is_meta);
            if matches!(mode, StructuredOutputMode::Native(_)) {
                response.content.push(ContentBlock::ToolCall {
                    tool_call: ToolCall::new("discarded", "record", json!({})),
                });
                let correction = mode.correction_request(&base, &response, &error, 1);
                assert_eq!(correction.messages.len(), base.messages.len() + 1);
            }
        }
    }

    /// 保留结果工具的 value 包装不能把根 JSON 类型收窄为对象。
    #[test]
    fn emulated_output_accepts_every_json_root_type() {
        let mode =
            StructuredOutputMode::ToolEmulated(StructuredOutputConfig::new("any", json!({})));
        for value in [
            json!([1, 2]),
            json!("text"),
            json!(42),
            json!(true),
            Value::Null,
            json!({"nested": "value"}),
        ] {
            let response = tool_response(vec![ToolCall::new(
                "result",
                STRUCTURED_OUTPUT_TOOL_NAME,
                json!({"value": value.clone()}),
            )]);
            assert_eq!(mode.parse_response(&response), Ok(Some(value)));
        }
    }

    /// 未完整终态和非结构化错误都不能消耗或生成纠正请求。
    #[test]
    fn correction_budget_ignores_incomplete_and_non_structured_errors() {
        let mode = StructuredOutputMode::Native(test_config());
        let request = test_request(&mode);
        let response = text_response(
            "{\"answer\":1}",
            StopReason::Other {
                reason: "interrupted".to_owned(),
            },
        );
        let incomplete = mode
            .parse_response(&response)
            .expect_err("未知终态必须失败");
        let mut budget = StructuredOutputCorrectionBudget::new();
        assert!(
            budget
                .next_request(&mode, &request, &response, &incomplete)
                .is_none()
        );
        assert_eq!(budget.retries_used, 0);

        let provider_error = ModelError::ProviderUnavailable {
            message: "offline".to_owned(),
            status_code: None,
            retryable: true,
        };
        assert!(
            budget
                .next_request(&mode, &request, &response, &provider_error)
                .is_none()
        );
        assert_eq!(budget.retries_used, 0);
    }

    /// 第五次纠正仍可成功；初始响应加五次纠正恰好产生六次请求。
    #[tokio::test]
    async fn isolated_completion_succeeds_on_fifth_correction() {
        let mode = StructuredOutputMode::ToolEmulated(test_config());
        let provider = ScriptedProvider::new(
            ProviderCapabilities {
                tool_calling: true,
                structured_output: StructuredOutputCapability::ToolEmulated,
                ..ProviderCapabilities::default()
            },
            [
                scripted_text("bad-0", StopReason::Completed),
                scripted_text("bad-1", StopReason::Completed),
                scripted_text("bad-2", StopReason::Completed),
                scripted_text("bad-3", StopReason::Completed),
                scripted_text("bad-4", StopReason::Completed),
                scripted_result("result-5", json!({"answer": 42})),
            ],
        );

        let (_, value) = mode
            .complete_isolated(&provider, test_request(&mode))
            .await
            .expect("第五次纠正应成功");

        assert_eq!(value, Some(json!({"answer": 42})));
        assert_eq!(provider.requests().expect("请求应可读取").len(), 6);
        assert_eq!(provider.remaining_replies(), Ok(0));
    }

    /// 第六个坏响应耗尽预算后返回最后诊断，且不会消费第七段脚本。
    #[tokio::test]
    async fn isolated_completion_stops_after_six_bad_responses() {
        let mode = StructuredOutputMode::Native(test_config());
        let provider = ScriptedProvider::new(
            ProviderCapabilities {
                structured_output: StructuredOutputCapability::Native,
                ..ProviderCapabilities::default()
            },
            [
                scripted_text("not-json", StopReason::Completed),
                scripted_text("still-not-json", StopReason::Completed),
                scripted_text("{}", StopReason::Completed),
                scripted_text("{\"answer\":0}", StopReason::Completed),
                scripted_text("[]", StopReason::Completed),
                scripted_text("", StopReason::Completed),
                scripted_text("{\"answer\":42}", StopReason::Completed),
            ],
        );

        let error = mode
            .complete_isolated(&provider, test_request(&mode))
            .await
            .expect_err("六个坏响应必须耗尽预算");

        assert!(matches!(
            error,
            ModelError::StructuredOutput {
                enforcement: StructuredOutputEnforcement::Native,
                failure: StructuredOutputFailureKind::MissingOutput,
                ..
            }
        ));
        assert_eq!(provider.requests().expect("请求应可读取").len(), 6);
        assert_eq!(provider.remaining_replies(), Ok(1));
    }

    /// 纠正请求中的 Provider、取消和上下文错误必须立即传播，不继续消费脚本。
    #[tokio::test]
    async fn isolated_completion_propagates_non_structured_errors_immediately() {
        for expected in [
            ModelError::ProviderUnavailable {
                message: "offline".to_owned(),
                status_code: None,
                retryable: true,
            },
            ModelError::Cancelled {
                message: "cancelled".to_owned(),
            },
            ModelError::ContextLengthExceeded {
                message: "too large".to_owned(),
            },
        ] {
            let mode = StructuredOutputMode::Native(test_config());
            let provider = ScriptedProvider::new(
                ProviderCapabilities {
                    structured_output: StructuredOutputCapability::Native,
                    ..ProviderCapabilities::default()
                },
                [
                    scripted_text("not-json", StopReason::Completed),
                    ScriptedReply::new(vec![Err(expected.clone())]),
                    scripted_text("{\"answer\":42}", StopReason::Completed),
                ],
            );

            let error = mode
                .complete_isolated(&provider, test_request(&mode))
                .await
                .expect_err("非结构化错误必须立即传播");

            assert_eq!(error, expected);
            assert_eq!(provider.requests().expect("请求应可读取").len(), 2);
            assert_eq!(provider.remaining_replies(), Ok(1));
        }
    }
}
