use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ImageContent, ModelError, structured};

/// 三种目标模型协议共同接受的工具名称最大 ASCII 字节数。
pub const MAX_TOOL_NAME_BYTES: usize = 64;

/// 判断工具名称能否在三种目标模型协议间无损往返。
fn is_portable_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// 模型可调用工具的 Provider 中立定义。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// 在一次请求内唯一且非空的工具名称。
    pub name: String,
    /// 向模型解释用途、边界与输入要求的说明。
    pub description: String,
    /// 描述工具输入的 JSON Schema 对象。
    pub input_schema: Value,
}

impl ToolDefinition {
    /// 创建一个工具定义。
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    /// 校验名称、说明和输入 Schema 是否满足统一层不变量。
    pub fn validate(&self) -> Result<(), ModelError> {
        if !is_portable_tool_name(&self.name) {
            return Err(ModelError::InvalidRequest {
                message: "工具名称必须是 1 到 64 字节的 ASCII 字母、数字、下划线或短横线"
                    .to_owned(),
            });
        }
        if self.description.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: format!("工具 {} 的说明不能为空", self.name),
            });
        }
        if !self.input_schema.is_object() {
            return Err(ModelError::InvalidRequest {
                message: format!("工具 {} 的输入 Schema 必须是 JSON 对象", self.name),
            });
        }
        structured::validate_schema(&self.input_schema).map_err(|error| {
            ModelError::InvalidRequest {
                message: format!("工具 {} 的输入 Schema 无效：{error}", self.name),
            }
        })
    }

    /// 使用注册时同一套受限 JSON Schema 规则校验一次模型工具输入。
    pub fn validate_input(&self, input: &Value) -> Result<(), ModelError> {
        self.validate()?;
        structured::validate_value_prechecked(
            &self.input_schema,
            input,
            crate::StructuredOutputEnforcement::ToolEmulated,
        )
        .map_err(|error| ModelError::InvalidRequest {
            message: format!("工具 {} 的输入不符合 Schema：{error}", self.name),
        })
    }
}

/// 模型请求执行一次工具时产生的统一调用。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// 在当前模型响应内唯一的调用标识。
    pub id: String,
    /// 对应 [`ToolDefinition`] 的工具名称。
    pub name: String,
    /// 已完成解析的 JSON 对象参数。
    pub arguments: Value,
}

impl ToolCall {
    /// 创建一次完整工具调用。
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// 校验调用标识、名称和参数形状。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.id.trim().is_empty() {
            return Err(ModelError::Protocol {
                message: "工具调用标识不能为空".to_owned(),
            });
        }
        if !is_portable_tool_name(&self.name) {
            return Err(ModelError::Protocol {
                message: "工具调用名称不满足跨协议可移植格式".to_owned(),
            });
        }
        if !self.arguments.is_object() {
            return Err(ModelError::Protocol {
                message: format!("工具调用 {} 的参数必须是 JSON 对象", self.id),
            });
        }
        Ok(())
    }
}

/// 工具结果中可返回给模型的内容。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    /// UTF-8 文本结果。
    Text {
        /// 工具生成的文本。
        text: String,
    },
    /// 图片结果。
    Image {
        /// 工具生成或读取的图片。
        image: ImageContent,
    },
}

/// 工具执行完成后回传给模型的统一结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// 与 [`ToolCall::id`] 对应的调用标识。
    pub tool_call_id: String,
    /// 返回给模型的有序内容块。
    pub content: Vec<ToolResultContent>,
    /// `true` 表示工具执行失败，内容中包含可供模型处理的错误说明。
    pub is_error: bool,
}

impl ToolResult {
    /// 创建一次工具执行结果。
    pub fn new(
        tool_call_id: impl Into<String>,
        content: Vec<ToolResultContent>,
        is_error: bool,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content,
            is_error,
        }
    }

    /// 创建仅包含文本的工具执行结果。
    pub fn text(tool_call_id: impl Into<String>, text: impl Into<String>, is_error: bool) -> Self {
        Self::new(
            tool_call_id,
            vec![ToolResultContent::Text { text: text.into() }],
            is_error,
        )
    }

    /// 校验结果是否能够与先前工具调用稳定关联。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.tool_call_id.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "工具结果的调用标识不能为空".to_owned(),
            });
        }
        for item in &self.content {
            if let ToolResultContent::Image { image } = item {
                image.validate()?;
            }
        }
        Ok(())
    }
}
