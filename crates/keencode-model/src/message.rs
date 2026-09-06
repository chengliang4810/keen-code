use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelError, ToolCall, ToolResult};

/// 一条消息在对话中的语义角色。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// 约束整个模型调用的系统级指令。
    System,
    /// 由应用注入、优先于普通用户输入的开发约束。
    Developer,
    /// 用户或运行时代表用户提供的输入。
    User,
    /// 模型生成的文本、推理或工具调用。
    Assistant,
    /// 一个或多个工具调用的执行结果。
    Tool,
}

/// 图片内容的来源。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// 可由模型服务读取的绝对网络地址。
    Url {
        /// 图片地址。
        url: String,
    },
    /// 已编码为 Base64 文本的内联图片。
    Base64 {
        /// 图片的标准媒体类型。
        media_type: String,
        /// 不包含 data URL 前缀的 Base64 文本。
        data: String,
    },
}

/// 可作为输入或工具结果的图片。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ImageContent {
    /// 图片的可移植来源。
    pub source: ImageSource,
}

impl ImageContent {
    /// 创建一个通过网络地址引用的图片。
    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            source: ImageSource::Url { url: url.into() },
        }
    }

    /// 创建一个 Base64 内联图片。
    pub fn from_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            source: ImageSource::Base64 {
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }

    /// 校验图片来源包含可用地址或数据。
    pub fn validate(&self) -> Result<(), ModelError> {
        match &self.source {
            ImageSource::Url { url } if url.trim().is_empty() => Err(ModelError::InvalidRequest {
                message: "图片地址不能为空".to_owned(),
            }),
            ImageSource::Base64 { media_type, .. } if media_type.trim().is_empty() => {
                Err(ModelError::InvalidRequest {
                    message: "Base64 图片的媒体类型不能为空".to_owned(),
                })
            }
            ImageSource::Base64 { data, .. } if data.trim().is_empty() => {
                Err(ModelError::InvalidRequest {
                    message: "Base64 图片数据不能为空".to_owned(),
                })
            }
            ImageSource::Url { .. } | ImageSource::Base64 { .. } => Ok(()),
        }
    }
}

/// 模型返回的可展示推理内容。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaqueReasoningState {
    /// 由对应 Adapter 定义、用于识别状态编码方式的稳定名称。
    pub kind: String,
    /// Agent Runtime 只负责原样持久化和回传、不得解释的不透明数据。
    pub data: Value,
}

impl OpaqueReasoningState {
    /// 创建一份由协议 Adapter 管理的不透明推理续传状态。
    pub fn new(kind: impl Into<String>, data: Value) -> Self {
        Self {
            kind: kind.into(),
            data,
        }
    }

    /// 校验状态编码名称和数据均可用于后续回传。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.kind.trim().is_empty() {
            return Err(ModelError::Protocol {
                message: "不透明推理状态的编码名称不能为空".to_owned(),
            });
        }
        if self.data.is_null() {
            return Err(ModelError::Protocol {
                message: "不透明推理状态的数据不能为空".to_owned(),
            });
        }
        Ok(())
    }
}

/// 模型返回的可展示推理内容及可选续传状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContent {
    /// 可展示或可持久化的推理文本。
    pub text: String,
    /// Provider 已提供的简短推理摘要；未提供时为 `None`。
    pub summary: Option<String>,
    /// 由 Adapter 原样恢复到后续请求的不透明推理续传状态。
    pub continuation: Option<OpaqueReasoningState>,
}

impl ReasoningContent {
    /// 创建一段不带摘要的推理内容。
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            summary: None,
            continuation: None,
        }
    }

    /// 校验推理至少包含一种有效载荷，并校验可选的不透明续传状态。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.summary.as_ref().is_some_and(String::is_empty) {
            return Err(ModelError::Protocol {
                message: "推理摘要不能是空字符串".to_owned(),
            });
        }
        if self.text.is_empty() && self.summary.is_none() && self.continuation.is_none() {
            return Err(ModelError::Protocol {
                message: "推理内容至少需要文本、摘要或续传状态中的一项".to_owned(),
            });
        }
        if let Some(continuation) = &self.continuation {
            continuation.validate()?;
        }
        Ok(())
    }
}

/// Provider 中立的消息内容块。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 普通文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 模型推理内容。
    Reasoning {
        /// 已归一化的推理内容。
        reasoning: ReasoningContent,
    },
    /// 图片内容。
    Image {
        /// 已归一化的图片。
        image: ImageContent,
    },
    /// 模型发起的工具调用。
    ToolCall {
        /// 已完整解析的工具调用。
        tool_call: ToolCall,
    },
    /// 工具执行完成后的结果。
    ToolResult {
        /// 与先前工具调用关联的结果。
        tool_result: ToolResult,
    },
}

impl ContentBlock {
    /// 创建一个普通文本内容块。
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// 校验内容块中需要稳定关联或可安全读取的字段。
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::Text { text } if text.is_empty() => Err(ModelError::InvalidRequest {
                message: "文本内容块不能是空字符串".to_owned(),
            }),
            Self::Text { .. } => Ok(()),
            Self::Reasoning { reasoning } => reasoning.validate(),
            Self::Image { image } => image.validate(),
            Self::ToolCall { tool_call } => tool_call.validate(),
            Self::ToolResult { tool_result } => tool_result.validate(),
        }
    }
}

/// 一条 Provider 中立的有序对话消息。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// 消息的语义角色。
    pub role: MessageRole,
    /// 保持原始顺序的内容块。
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// 创建一条包含指定内容块的消息。
    pub fn new(role: MessageRole, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    /// 创建一条仅包含文本的消息。
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentBlock::text(text)])
    }

    /// 校验消息至少包含一个内容块且各内容块有效。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.content.is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "消息内容不能为空".to_owned(),
            });
        }
        for block in &self.content {
            block.validate()?;
        }
        let role_matches = self.content.iter().all(|block| match self.role {
            MessageRole::System | MessageRole::Developer => {
                matches!(block, ContentBlock::Text { .. })
            }
            MessageRole::User => {
                matches!(
                    block,
                    ContentBlock::Text { .. } | ContentBlock::Image { .. }
                )
            }
            MessageRole::Assistant => matches!(
                block,
                ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::ToolCall { .. }
            ),
            MessageRole::Tool => matches!(block, ContentBlock::ToolResult { .. }),
        });
        if !role_matches {
            return Err(ModelError::InvalidRequest {
                message: format!("消息角色 {:?} 包含了不允许的内容类型", self.role),
            });
        }
        Ok(())
    }
}
