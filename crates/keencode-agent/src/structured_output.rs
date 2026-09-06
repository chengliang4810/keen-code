//! 普通 Turn 与隔离生成共用的结构化结果通道；不执行任何业务工具。

use keencode_model::{
    ContentBlock, ModelError, ModelResponse, ProviderCapabilities, StopReason,
    StructuredOutputCapability, StructuredOutputConfig, StructuredOutputEnforcement,
    StructuredOutputFailureKind, ToolDefinition,
};
use serde_json::{Value, json};

/// 工具模拟结构化输出使用且不允许业务工具占用的保留名称。
pub const STRUCTURED_OUTPUT_TOOL_NAME: &str = "__keencode_structured_output";

/// 保留结果工具包装任意根 JSON 类型时使用的唯一字段。
const STRUCTURED_OUTPUT_VALUE_FIELD: &str = "value";

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
            .unwrap_or("提交已经完成的最终结构化结果");
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
            StopReason::Completed | StopReason::Other { .. } | StopReason::ToolUse => {
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
    use keencode_model::{ImageContent, ResponseMetadata, TokenUsage, ToolCall, ToolResult};

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
}
