//! 通过桌面交互向用户提出结构化问题的 Provider 中立工具。

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::future::{Either, select};
use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolConcurrency, ToolContext, ToolEffect, ToolError,
    ToolFuture, ToolOutput, TurnId,
};
use keencode_model::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 单次 AskUser 最多展示的问题数量。
const MAX_QUESTIONS: usize = 4;

/// 单个问题最多提供的预设选项数量。
const MAX_OPTIONS: usize = 8;

/// 问题、选项说明或自定义答案允许的最大 UTF-8 字符数。
const MAX_TEXT_CHARS: usize = 4_000;

/// 单个预设选项标签允许的最大 UTF-8 字符数。
const MAX_LABEL_CHARS: usize = 200;

/// 一个可供用户选择的答案选项。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UserQuestionOption {
    /// 展示给用户并作为答案值返回的非空标签。
    pub label: String,
    /// 帮助用户判断影响或取舍的可选说明。
    pub description: Option<String>,
}

/// AskUser 中一个具有稳定标识的问题。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UserQuestion {
    /// 仅由 ASCII 字母、数字、下划线和连字符组成的稳定标识。
    pub id: String,
    /// 展示给用户的完整问题文本。
    pub prompt: String,
    /// 零个或多个互不重复的预设选项。
    #[serde(default)]
    pub options: Vec<UserQuestionOption>,
    /// 是否允许用户选择多个答案。
    #[serde(default)]
    pub multi_select: bool,
    /// 是否允许用户输入预设选项之外的答案。
    #[serde(default = "default_true")]
    pub allow_custom: bool,
}

/// 桌面交互端口收到的一次完整 AskUser 请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserQuestionRequest {
    /// 发起交互的 Session。
    pub session_id: SessionId,
    /// 发起交互的当前 Turn。
    pub turn_id: TurnId,
    /// 发起交互的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 触发本次标准 Elicitation 的真实工具调用标识。
    pub tool_call_id: ToolCallId,
    /// 按模型输入顺序排列且已经严格校验的问题。
    pub questions: Vec<UserQuestion>,
}

/// 用户对一个问题提交的零个、一个或多个答案。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UserQuestionAnswer {
    /// 对应请求中的稳定问题标识。
    pub id: String,
    /// 用户选中的标签或允许的自定义文本；空数组表示跳过。
    pub values: Vec<String>,
}

/// 桌面交互端口返回的一次完整回答。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserQuestionResponse {
    /// 每个请求问题恰好一项的回答，输入顺序不影响最终规范顺序。
    pub answers: Vec<UserQuestionAnswer>,
}

/// 桌面交互通道关闭或无法展示问题时返回的安全错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserQuestionError {
    /// 不包含问题正文和用户答案的可展示说明。
    message: String,
}

impl UserQuestionError {
    /// 从不包含敏感正文的说明创建交互错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 返回安全错误说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for UserQuestionError {
    /// 输出不包含问题正文和答案的安全说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for UserQuestionError {}

/// 对象安全的异步用户问答返回值。
pub type UserQuestionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UserQuestionResponse, UserQuestionError>> + Send + 'a>>;

/// 由桌面界面实现的用户问答交互端口。
pub trait UserQuestionHandler: Send + Sync {
    /// 展示一次问题集合并等待用户提交或关闭交互。
    fn ask(&self, request: UserQuestionRequest) -> UserQuestionFuture<'_>;
}

/// 把结构化问题转交桌面交互端口的 AskUser 工具。
pub struct AskUserTool {
    /// 真正展示并收集回答的桌面端口。
    handler: Arc<dyn UserQuestionHandler>,
}

impl AskUserTool {
    /// 创建绑定到指定桌面问答端口的工具。
    pub fn new(handler: Arc<dyn UserQuestionHandler>) -> Self {
        Self { handler }
    }
}

impl AgentTool for AskUserTool {
    /// 返回最多四个结构化问题的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "AskUser",
            "当缺少会实质改变结果的用户选择时提出一到四个简短问题。预设选项用于明确取舍；允许自定义时用户也可以直接输入答案。",
            json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "minLength": 1, "maxLength": 64 },
                                "prompt": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS },
                                "options": {
                                    "type": "array",
                                    "maxItems": MAX_OPTIONS,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string", "minLength": 1, "maxLength": 200 },
                                            "description": { "type": ["string", "null"], "maxLength": MAX_TEXT_CHARS }
                                        },
                                        "required": ["label"],
                                        "additionalProperties": false
                                    },
                                    "default": []
                                },
                                "multiSelect": { "type": "boolean", "default": false },
                                "allowCustom": { "type": "boolean", "default": true }
                            },
                            "required": ["id", "prompt"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
        )
    }

    /// 问答只改变当前交互状态，不直接修改文件、进程或网络资源。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 同一 Agent 的可见问答必须按模型调用顺序逐个展示。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 严格校验问题，等待桌面回答并返回按请求顺序规范化的 JSON。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let handler = self.handler.clone();
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent(
                    "ask_user_cancelled",
                    "用户问答因当前 Turn 取消而关闭",
                ));
            }
            let questions = parse_questions(input)?;
            let request = UserQuestionRequest {
                session_id: context.session_id,
                turn_id: context.turn_id,
                source_agent_id: context.source_agent_id,
                tool_call_id: context.tool_call_id,
                questions: questions.clone(),
            };
            let cancelled = Box::pin(context.cancellation.cancelled());
            let response = handler.ask(request);
            let response = match select(cancelled, response).await {
                Either::Left(((), _pending_response)) => {
                    return Err(ToolError::permanent(
                        "ask_user_cancelled",
                        "用户问答因当前 Turn 取消而关闭",
                    ));
                }
                Either::Right((result, _pending_cancel)) => result.map_err(|error| {
                    ToolError::permanent("ask_user_failed", error.message().to_owned())
                })?,
            };
            let answers = validate_answers(&questions, response)?;
            let text = serde_json::to_string(&json!({ "answers": answers })).map_err(|error| {
                ToolError::permanent(
                    "ask_user_output_failed",
                    format!("用户问答结果无法序列化：{error}"),
                )
            })?;
            Ok(ToolOutput::text(text))
        })
    }
}

/// AskUser 的严格顶层输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserInput {
    /// 一到四个待展示问题。
    questions: Vec<UserQuestion>,
}

/// 反序列化并执行问题标识、文本和选项唯一性校验。
fn parse_questions(input: Value) -> Result<Vec<UserQuestion>, ToolError> {
    let input: AskUserInput = serde_json::from_value(input).map_err(|error| {
        ToolError::permanent("invalid_input", format!("AskUser 输入无效：{error}"))
    })?;
    if input.questions.is_empty() || input.questions.len() > MAX_QUESTIONS {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("AskUser 必须包含 1..={MAX_QUESTIONS} 个问题"),
        ));
    }
    let mut identifiers = HashSet::new();
    for question in &input.questions {
        if !valid_identifier(&question.id) || !identifiers.insert(question.id.clone()) {
            return Err(ToolError::permanent(
                "invalid_input",
                "AskUser 问题标识无效或重复",
            ));
        }
        validate_text(&question.prompt, "问题文本", false)?;
        if question.options.len() > MAX_OPTIONS {
            return Err(ToolError::permanent(
                "invalid_input",
                format!("单个问题选项不能超过 {MAX_OPTIONS} 项"),
            ));
        }
        if question.options.is_empty() && !question.allow_custom {
            return Err(ToolError::permanent(
                "invalid_input",
                "问题必须提供预设选项或允许自定义答案",
            ));
        }
        let mut labels = HashSet::new();
        for option in &question.options {
            validate_bounded_text(&option.label, "选项标签", false, MAX_LABEL_CHARS)?;
            if !labels.insert(option.label.clone()) {
                return Err(ToolError::permanent(
                    "invalid_input",
                    "问题选项标签不能重复",
                ));
            }
            if let Some(description) = &option.description {
                validate_text(description, "选项说明", true)?;
            }
        }
    }
    Ok(input.questions)
}

/// 校验回答完整性，并按请求问题顺序返回规范化结果。
fn validate_answers(
    questions: &[UserQuestion],
    response: UserQuestionResponse,
) -> Result<Vec<UserQuestionAnswer>, ToolError> {
    if response.answers.len() != questions.len() {
        return Err(ToolError::permanent(
            "invalid_user_response",
            "桌面问答响应没有为每个问题返回恰好一项答案",
        ));
    }
    let mut by_id = HashMap::new();
    for answer in response.answers {
        if by_id.insert(answer.id.clone(), answer).is_some() {
            return Err(ToolError::permanent(
                "invalid_user_response",
                "桌面问答响应包含重复问题标识",
            ));
        }
    }
    let mut ordered = Vec::with_capacity(questions.len());
    for question in questions {
        let answer = by_id.remove(&question.id).ok_or_else(|| {
            ToolError::permanent("invalid_user_response", "桌面问答响应缺少请求问题")
        })?;
        if !question.multi_select && answer.values.len() > 1 {
            return Err(ToolError::permanent(
                "invalid_user_response",
                "单选问题不能返回多个答案",
            ));
        }
        if answer.values.len() > MAX_OPTIONS {
            return Err(ToolError::permanent(
                "invalid_user_response",
                "单个问题返回的答案数量超过上限",
            ));
        }
        let known = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<HashSet<_>>();
        let mut unique = HashSet::new();
        for value in &answer.values {
            validate_text(value, "用户答案", false)?;
            if !unique.insert(value.as_str()) {
                return Err(ToolError::permanent(
                    "invalid_user_response",
                    "同一问题不能返回重复答案",
                ));
            }
            if !known.contains(value.as_str()) && !question.allow_custom {
                return Err(ToolError::permanent(
                    "invalid_user_response",
                    "问题不允许预设选项之外的答案",
                ));
            }
        }
        ordered.push(answer);
    }
    if !by_id.is_empty() {
        return Err(ToolError::permanent(
            "invalid_user_response",
            "桌面问答响应包含未知问题标识",
        ));
    }
    Ok(ordered)
}

/// 校验问题标识可安全用于前端 keyed state 和事件关联。
fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// 校验交互文本有界，且按字段规则允许或拒绝空白值。
fn validate_text(value: &str, label: &str, allow_empty: bool) -> Result<(), ToolError> {
    validate_bounded_text(value, label, allow_empty, MAX_TEXT_CHARS)
}

/// 使用字段自己的字符上限校验交互文本。
fn validate_bounded_text(
    value: &str,
    label: &str,
    allow_empty: bool,
    maximum_chars: usize,
) -> Result<(), ToolError> {
    let count = value.chars().count();
    if (!allow_empty && value.trim().is_empty()) || count > maximum_chars {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("{label}为空或超过 {maximum_chars} 个字符"),
        ));
    }
    Ok(())
}

/// Serde 默认值函数：AskUser 默认允许自定义答案。
const fn default_true() -> bool {
    true
}
