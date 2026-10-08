use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelError, ToolCall, ToolResult};

/// 用户显式选择的资源身份；解析、安装状态和读取权限由宿主校验，模型层不解释资源协议。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InputReference {
    /// 原输入框使用的显示名称。
    pub name: String,
    /// 宿主验证后的完整路径或 URI，保留同名资源的命名空间。
    pub path: String,
}

impl InputReference {
    /// 拒绝无界、重复和包含控制字符的身份；这些检查不代表已获得资源访问权限。
    pub fn validate_all(references: &[Self]) -> Result<(), ModelError> {
        let mut paths = std::collections::HashSet::new();
        if references.len() > 32
            || references.iter().any(|reference| {
                reference.name.trim().is_empty()
                    || reference.name.len() > 128
                    || reference.path.trim().is_empty()
                    || reference.path.len() > 2048
                    || reference.name.chars().any(char::is_control)
                    || reference.path.chars().any(char::is_control)
                    || !paths.insert(&reference.path)
            })
        {
            return Err(ModelError::InvalidRequest {
                message: "用户资源引用身份无效或重复".into(),
            });
        }
        Ok(())
    }
}

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

/// 从有序模型内容中读取最后一条非空普通文本。
///
/// Provider 可能在同一个响应中返回多个普通文本块（例如在工具调用或
/// 推理块前后各返回一段文本）。终态摘要只能代表响应的最后一条 Assistant
/// 正文，不能把前置块拼接成另一条结果。
pub fn last_non_empty_text(content: &[ContentBlock]) -> Option<&str> {
    content.iter().rev().find_map(|block| match block {
        ContentBlock::Text { text } => (!text.trim().is_empty()).then_some(text.as_str()),
        _ => None,
    })
}

/// 一条 Provider 中立的有序对话消息。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// 内部上下文：参与模型请求和持久化，但不作为用户发言展示。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_meta: bool,
    /// 与正文分开持久化的用户资源选择；不会改变原正文或赋予额外权限。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<InputReference>,
    /// 消息的语义角色。
    pub role: MessageRole,
    /// 保持原始顺序的内容块。
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// 创建一条包含指定内容块的消息。
    pub fn new(role: MessageRole, content: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content,
            is_meta: false,
            references: Vec::new(),
        }
    }

    /// 创建一条仅包含文本的消息。
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentBlock::text(text)])
    }

    /// 校验消息至少包含一个内容块且各内容块有效。
    pub fn validate(&self) -> Result<(), ModelError> {
        InputReference::validate_all(&self.references)?;
        if !self.references.is_empty() && self.role != MessageRole::User {
            return Err(ModelError::InvalidRequest {
                message: "只有用户消息可以携带资源选择".into(),
            });
        }
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

    /// 所有协议适配器共用的模型内容投影：完整资源身份以用户级上下文呈现，原消息内容保持不变。
    pub fn wire_content(&self) -> std::borrow::Cow<'_, [ContentBlock]> {
        if self.references.is_empty() {
            return std::borrow::Cow::Borrowed(&self.content);
        }
        let mut content = self.content.clone();
        content.push(ContentBlock::text(format!(
            "用户在本条消息中显式选择的资源引用（仅资源身份，不授予额外权限）：\n{}",
            serde_json::to_string(&self.references).expect("字符串资源身份可以序列化")
        )));
        std::borrow::Cow::Owned(content)
    }
}
