use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ContentBlock, Message, ModelError, StopReason, StructuredOutputEnforcement, TokenUsage,
    ToolDefinition, structured,
};

/// 请求模型投入推理计算的相对强度。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// 最小推理强度。
    Minimal,
    /// 较低推理强度。
    Low,
    /// 中等推理强度。
    Medium,
    /// 较高推理强度。
    High,
    /// 极高推理强度。
    ExtraHigh,
    /// Provider 明确支持时使用其最大推理强度。
    Maximum,
}

/// Provider 中立的推理请求配置。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningConfig {
    /// 期望的相对推理强度；让端点采用默认值时为 `None`。
    pub effort: Option<ReasoningEffort>,
    /// 期望的最大推理 Token；让端点采用默认值时为 `None`。
    pub max_tokens: Option<u32>,
    /// 是否请求端点返回可展示的推理摘要。
    pub include_summary: bool,
}

impl ReasoningConfig {
    /// 校验显式 Token 预算大于零。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.max_tokens == Some(0) {
            return Err(ModelError::InvalidRequest {
                message: "推理 Token 上限必须大于零".to_owned(),
            });
        }
        Ok(())
    }
}

/// 要求模型最终生成指定 JSON Schema 的配置。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredOutputConfig {
    /// 非空且稳定的结果类型名称。
    pub name: String,
    /// 向模型解释结构化结果用途的可选说明。
    pub description: Option<String>,
    /// 结果必须满足的 JSON Schema 对象。
    pub schema: Value,
    /// 是否请求 Adapter 启用远端原生严格模式；无论该值如何，本地始终严格校验结果。
    pub strict: bool,
}

impl StructuredOutputConfig {
    /// 创建结构化输出配置。
    pub fn new(name: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            schema,
            strict: true,
        }
    }

    /// 校验名称和 Schema 形状。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.name.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "结构化输出名称不能为空".to_owned(),
            });
        }
        if !self.schema.is_object() {
            return Err(ModelError::InvalidRequest {
                message: "结构化输出 Schema 必须是 JSON 对象".to_owned(),
            });
        }
        structured::validate_schema(&self.schema)
    }

    /// 校验一个已解析 JSON 值满足当前 Schema。
    pub fn validate_value(
        &self,
        value: &Value,
        enforcement: StructuredOutputEnforcement,
    ) -> Result<(), ModelError> {
        self.validate()?;
        structured::validate_value_prechecked(&self.schema, value, enforcement)
    }

    /// 从最终模型响应提取唯一 JSON 值并执行当前 Schema 校验。
    pub fn parse_response(
        &self,
        response: &ModelResponse,
        enforcement: StructuredOutputEnforcement,
    ) -> Result<Value, ModelError> {
        self.validate()?;
        structured::parse_response_prechecked(&self.schema, response, enforcement)
    }
}

/// 模型在当前请求中选择工具的策略。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    /// 由模型决定是否调用工具。
    #[default]
    Auto,
    /// 禁止模型调用任何工具。
    None,
    /// 要求模型至少调用一个工具。
    Required,
    /// 要求模型调用一个指定工具。
    Specific {
        /// 必须调用的工具名称。
        name: String,
    },
}

/// Agent Runtime 提交给任意模型 Provider 的统一请求。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    /// Provider 配置中选择的模型标识。
    pub model: String,
    /// 按对话顺序排列的完整有效消息。
    pub messages: Vec<Message>,
    /// 当前调用允许模型使用的工具定义。
    pub tools: Vec<ToolDefinition>,
    /// 当前调用的工具选择策略。
    pub tool_choice: ToolChoice,
    /// 是否允许模型在一个响应中请求多个工具；采用端点默认值时为 `None`。
    pub parallel_tool_calls: Option<bool>,
    /// 推理配置；未启用或未指定时为 `None`。
    pub reasoning: Option<ReasoningConfig>,
    /// 最终结构化输出要求；自由文本响应时为 `None`。
    pub structured_output: Option<StructuredOutputConfig>,
    /// 最大输出 Token；采用模型默认值时为 `None`。
    pub max_output_tokens: Option<u32>,
    /// Provider 中立的采样温度；采用模型默认值时为 `None`。
    pub temperature: Option<f32>,
    /// 模型遇到其中任意文本时应停止继续生成。
    pub stop_sequences: Vec<String>,
    /// 仅用于追踪调用且不得改变模型语义的字符串元数据。
    pub metadata: BTreeMap<String, String>,
}

impl ModelRequest {
    /// 创建只包含模型和消息的最小请求。
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            reasoning: None,
            structured_output: None,
            max_output_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// 校验请求满足统一模型层的不变量。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.model.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "模型标识不能为空".to_owned(),
            });
        }
        if self.messages.is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "模型请求至少需要一条消息".to_owned(),
            });
        }
        for message in &self.messages {
            message.validate()?;
        }

        let mut tool_names = HashSet::with_capacity(self.tools.len());
        for tool in &self.tools {
            tool.validate()?;
            if !tool_names.insert(tool.name.as_str()) {
                return Err(ModelError::InvalidRequest {
                    message: format!("工具名称 {} 在同一请求中重复", tool.name),
                });
            }
        }

        match &self.tool_choice {
            ToolChoice::Specific { name } if name.trim().is_empty() => {
                return Err(ModelError::InvalidRequest {
                    message: "指定工具名称不能为空".to_owned(),
                });
            }
            ToolChoice::Specific { name } if !tool_names.contains(name.as_str()) => {
                return Err(ModelError::InvalidRequest {
                    message: format!("指定工具 {name} 不在当前工具列表中"),
                });
            }
            ToolChoice::Required if self.tools.is_empty() => {
                return Err(ModelError::InvalidRequest {
                    message: "要求调用工具时工具列表不能为空".to_owned(),
                });
            }
            ToolChoice::Auto
            | ToolChoice::None
            | ToolChoice::Required
            | ToolChoice::Specific { .. } => {}
        }

        if let Some(reasoning) = &self.reasoning {
            reasoning.validate()?;
        }
        if let Some(structured_output) = &self.structured_output {
            structured_output.validate()?;
        }
        if self.max_output_tokens == Some(0) {
            return Err(ModelError::InvalidRequest {
                message: "最大输出 Token 必须大于零".to_owned(),
            });
        }
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || temperature < 0.0 {
                return Err(ModelError::InvalidRequest {
                    message: "采样温度必须是大于等于零的有限数值".to_owned(),
                });
            }
        }
        if self.stop_sequences.iter().any(|item| item.is_empty()) {
            return Err(ModelError::InvalidRequest {
                message: "停止序列不能为空字符串".to_owned(),
            });
        }
        Ok(())
    }
}

/// 一次完整模型响应的 Provider 中立表示。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMetadata {
    /// Provider 返回的响应标识；未提供时为 `None`。
    pub response_id: Option<String>,
    /// Provider 实际报告的模型标识；未提供时为 `None`。
    pub model: Option<String>,
}

impl ResponseMetadata {
    /// 校验已报告的响应标识和模型标识均不是空字符串。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self
            .response_id
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ModelError::Protocol {
                message: "Provider 返回的响应标识不能为空".to_owned(),
            });
        }
        if self
            .model
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ModelError::Protocol {
                message: "Provider 返回的模型标识不能为空".to_owned(),
            });
        }
        Ok(())
    }
}

/// 一次完整模型响应的 Provider 中立表示。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelResponse {
    /// 响应级标识和实际模型信息。
    pub metadata: ResponseMetadata,
    /// 保持模型生成顺序的内容块。
    pub content: Vec<ContentBlock>,
    /// 端点报告的 Token 用量；所有字段都可能未知。
    pub usage: TokenUsage,
    /// 模型结束当前响应的原因。
    pub stop_reason: StopReason,
}

impl ModelResponse {
    /// 创建完整模型响应。
    pub fn new(
        metadata: ResponseMetadata,
        content: Vec<ContentBlock>,
        usage: TokenUsage,
        stop_reason: StopReason,
    ) -> Self {
        Self {
            metadata,
            content,
            usage,
            stop_reason,
        }
    }

    /// 校验响应中的每个内容块。
    pub fn validate(&self) -> Result<(), ModelError> {
        self.metadata.validate()?;
        for block in &self.content {
            block.validate()?;
        }
        Ok(())
    }
}
