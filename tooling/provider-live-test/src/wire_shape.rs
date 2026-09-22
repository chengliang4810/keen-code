//! Provider 线级响应的独立、非敏感结构证据检查器。
//!
//! 本模块只把原始响应暂存在调用栈中，并且只输出固定枚举、布尔值、HTTP 状态和
//! `zero/one/many` 基数。它不调用 Provider Adapter 或 SSE 解码器，也不持久化正文、
//! 字段值、未知名称、长度、摘要、工具参数、推理或错误文本。

use std::collections::BTreeMap;

use keencode_model::ProviderProtocol;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 当前独立结构证据的固定模式版本。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireShapeSchema {
    /// 第二版增加不含原文的 Provider 明确错误事实。
    V2,
}

/// 响应头声明的非敏感媒体类型类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeclaredContentType {
    /// JSON 或带 `+json` 后缀的媒体类型。
    Json,
    /// `text/event-stream` 媒体类型。
    Sse,
    /// 其他媒体类型；不会保存原始值或参数。
    Other,
    /// 响应没有可用的媒体类型。
    Missing,
}

/// 从捕获字节独立识别出的正文格式。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireBodyFormat {
    /// 正文是可完整解析的 JSON。
    Json,
    /// 正文按声明或固定 SSE 前缀作为 SSE 检查。
    Sse,
    /// 捕获正文为空。
    Empty,
    /// 捕获正文不是有效 UTF-8。
    InvalidUtf8,
    /// 有效 UTF-8 正文既不是完整 JSON，也没有 SSE 依据。
    Unknown,
}

/// 只暴露到 `zero/one/many` 的饱和基数。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SaturatedCardinality {
    /// 没有观察到目标。
    Zero,
    /// 恰好观察到一个目标。
    One,
    /// 观察到至少两个目标。
    Many,
}

impl SaturatedCardinality {
    /// 把一次新观察饱和合并到当前基数。
    fn observe(&mut self) {
        *self = match self {
            Self::Zero => Self::One,
            Self::One | Self::Many => Self::Many,
        };
    }

    /// 把一个容器的元素数量饱和合并到当前基数。
    fn observe_len(&mut self, length: usize) {
        match length {
            0 => {}
            1 => self.observe(),
            _ => *self = Self::Many,
        }
    }

    /// 返回是否至少观察到一个目标。
    fn is_present(self) -> bool {
        !matches!(self, Self::Zero)
    }
}

/// JSON 值的固定、无正文类型类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JsonValueType {
    /// 固定路径没有匹配值。
    Missing,
    /// 匹配值为 `null`。
    Null,
    /// 匹配值为布尔值。
    Boolean,
    /// 匹配值为数字。
    Number,
    /// 匹配值为字符串；不保存字符串内容。
    String,
    /// 匹配值为数组。
    Array,
    /// 匹配值为对象。
    Object,
    /// 同一固定通配路径观察到多种类型。
    Mixed,
}

impl JsonValueType {
    /// 从一个临时 JSON 值取得非敏感类型。
    fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Number(_) => Self::Number,
            Value::String(_) => Self::String,
            Value::Array(_) => Self::Array,
            Value::Object(_) => Self::Object,
        }
    }

    /// 合并同一固定路径上的另一种观察类型。
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Missing, value) => value,
            (left, right) if left == right => left,
            _ => Self::Mixed,
        }
    }
}

/// 三协议缓冲 JSON 中 Adapter 实际读取的固定字段位置。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KnownJsonFieldPath {
    /// Messages 顶层 `type`。
    MessagesRootType,
    /// Messages 顶层 `id`。
    MessagesRootId,
    /// Messages 顶层 `model`。
    MessagesRootModel,
    /// Messages 顶层 `content`。
    MessagesRootContent,
    /// Messages 顶层 `stop_reason`。
    MessagesRootStopReason,
    /// Messages 顶层 `usage`。
    MessagesRootUsage,
    /// Messages 顶层 `error`。
    MessagesRootError,
    /// Messages `content[*].type`。
    MessagesContentType,
    /// Messages `content[*].text`。
    MessagesContentText,
    /// Messages `content[*].thinking`。
    MessagesContentThinking,
    /// Messages `content[*].signature`。
    MessagesContentSignature,
    /// Messages `content[*].data`。
    MessagesContentData,
    /// Messages `content[*].id`。
    MessagesContentId,
    /// Messages `content[*].name`。
    MessagesContentName,
    /// Messages `content[*].input`。
    MessagesContentInput,
    /// Chat Completions 顶层 `id`。
    ChatRootId,
    /// Chat Completions 顶层 `model`。
    ChatRootModel,
    /// Chat Completions 顶层 `choices`。
    ChatRootChoices,
    /// Chat Completions 顶层 `usage`。
    ChatRootUsage,
    /// Chat Completions 顶层 `error`。
    ChatRootError,
    /// Chat Completions `choices[*].message`。
    ChatChoiceMessage,
    /// Chat Completions `choices[*].finish_reason`。
    ChatChoiceFinishReason,
    /// Chat Completions `message.content`。
    ChatMessageContent,
    /// Chat Completions `message.reasoning_content`。
    ChatMessageReasoningContent,
    /// Chat Completions `message.reasoning`。
    ChatMessageReasoning,
    /// Chat Completions `message.reasoning_details`。
    ChatMessageReasoningDetails,
    /// Chat Completions `message.refusal`。
    ChatMessageRefusal,
    /// Chat Completions `message.tool_calls`。
    ChatMessageToolCalls,
    /// Chat Completions 内容 part 的 `type`。
    ChatContentPartType,
    /// Chat Completions 内容 part 的 `text`。
    ChatContentPartText,
    /// Chat Completions 推理 detail 的 `text`。
    ChatReasoningDetailText,
    /// Chat Completions 推理 detail 的 `delta`。
    ChatReasoningDetailDelta,
    /// Chat Completions 工具调用的 `id`。
    ChatToolCallId,
    /// Chat Completions 工具调用的 `function`。
    ChatToolCallFunction,
    /// Chat Completions 工具函数的 `name`。
    ChatToolFunctionName,
    /// Chat Completions 工具函数的 `arguments`。
    ChatToolFunctionArguments,
    /// Responses 顶层 `id`。
    ResponsesRootId,
    /// Responses 顶层 `model`。
    ResponsesRootModel,
    /// Responses 顶层 `status`。
    ResponsesRootStatus,
    /// Responses 顶层 `output`。
    ResponsesRootOutput,
    /// Responses 顶层 `usage`。
    ResponsesRootUsage,
    /// Responses 顶层 `error`。
    ResponsesRootError,
    /// Responses 顶层 `incomplete_details`。
    ResponsesRootIncompleteDetails,
    /// Responses `output[*].type`。
    ResponsesOutputType,
    /// Responses `output[*].content`。
    ResponsesOutputContent,
    /// Responses `output[*].summary`。
    ResponsesOutputSummary,
    /// Responses `output[*].id`。
    ResponsesOutputId,
    /// Responses `output[*].encrypted_content`。
    ResponsesOutputEncryptedContent,
    /// Responses `output[*].call_id`。
    ResponsesOutputCallId,
    /// Responses `output[*].name`。
    ResponsesOutputName,
    /// Responses `output[*].arguments`。
    ResponsesOutputArguments,
    /// Responses 内容 part 的 `type`。
    ResponsesContentPartType,
    /// Responses 内容 part 的 `text`。
    ResponsesContentPartText,
    /// Responses 内容 part 的 `refusal`。
    ResponsesContentPartRefusal,
    /// Responses 推理摘要 part 的 `text`。
    ResponsesSummaryPartText,
    /// Responses `incomplete_details.reason`。
    ResponsesIncompleteReason,
}

/// 一个固定 JSON 字段位置的观察类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KnownJsonFieldType {
    /// 编译期固定的字段位置枚举。
    pub(crate) path: KnownJsonFieldPath,
    /// 该位置全部匹配值的合并类型。
    pub(crate) value_type: JsonValueType,
}

/// Adapter 实际遍历的固定 JSON 容器位置。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KnownJsonNestedPath {
    /// Messages 顶层内容数组。
    MessagesContentItems,
    /// Chat Completions 顶层 choices 数组。
    ChatChoices,
    /// Chat Completions message 内容 part 数组。
    ChatContentParts,
    /// Chat Completions message 推理 detail 数组。
    ChatReasoningDetails,
    /// Chat Completions message 工具调用数组。
    ChatToolCalls,
    /// Responses 顶层 output 数组。
    ResponsesOutputItems,
    /// Responses output item 内容 part 数组。
    ResponsesContentParts,
    /// Responses reasoning item 摘要 part 数组。
    ResponsesSummaryParts,
}

/// 一个固定 JSON 容器位置的饱和形态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KnownJsonNestedShape {
    /// 编译期固定的容器位置枚举。
    pub(crate) path: KnownJsonNestedPath,
    /// 所有匹配容器中观察到的总元素饱和基数。
    pub(crate) cardinality: SaturatedCardinality,
    /// 所有匹配元素的合并 JSON 类型。
    pub(crate) element_type: JsonValueType,
}

/// 缓冲 JSON 的固定契约违例位。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct JsonContractViolationBits {
    /// JSON 根不是协议要求的对象。
    pub(crate) root_type_mismatch: bool,
    /// 必需字段缺失。
    pub(crate) required_field_missing: bool,
    /// 已知字段存在但类型不符合固定契约。
    pub(crate) known_field_type_mismatch: bool,
    /// 数组元素或已知嵌套对象形态不符合固定契约。
    pub(crate) known_nested_shape_mismatch: bool,
    /// 要求唯一元素的数组为零个或多个。
    pub(crate) expected_singleton_mismatch: bool,
    /// 已知判别字段包含不受当前 Adapter 支持的值。
    pub(crate) unknown_discriminator_present: bool,
}

impl JsonContractViolationBits {
    /// 创建全部违例位均为假的初始值。
    fn empty() -> Self {
        Self {
            root_type_mismatch: false,
            required_field_missing: false,
            known_field_type_mismatch: false,
            known_nested_shape_mismatch: false,
            expected_singleton_mismatch: false,
            unknown_discriminator_present: false,
        }
    }

    /// 返回是否观察到任一固定契约违例。
    // 该方法只被非持久化责任归因 API 使用。
    #[allow(dead_code)]
    fn any(&self) -> bool {
        self.root_type_mismatch
            || self.required_field_missing
            || self.known_field_type_mismatch
            || self.known_nested_shape_mismatch
            || self.expected_singleton_mismatch
            || self.unknown_discriminator_present
    }
}

/// 一份完整缓冲 JSON 的非敏感结构证据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct JsonShapeEvidence {
    /// 固定结构证据模式版本。
    pub(crate) shape_schema: WireShapeSchema,
    /// JSON 根值类型。
    pub(crate) root_type: JsonValueType,
    /// 按协议固定顺序完整列出的字段类型。
    pub(crate) known_field_types: Vec<KnownJsonFieldType>,
    /// 任一受检查对象是否出现未知键；不保存键名。
    pub(crate) unknown_key_present: bool,
    /// 按协议固定顺序完整列出的嵌套容器形态。
    pub(crate) known_nested_path_shapes: Vec<KnownJsonNestedShape>,
    /// 是否观察到协议明确声明的错误响应；不保存错误正文或状态原文。
    pub(crate) provider_declared_error: bool,
    /// 固定契约违例位。
    pub(crate) violation_bits: JsonContractViolationBits,
}

/// 三协议 SSE 的固定语义事件位置。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KnownSseEvent {
    /// Messages `message_start`。
    MessagesMessageStart,
    /// Messages `content_block_start`。
    MessagesContentBlockStart,
    /// Messages `content_block_delta`。
    MessagesContentBlockDelta,
    /// Messages `content_block_stop`。
    MessagesContentBlockStop,
    /// Messages `message_delta`。
    MessagesMessageDelta,
    /// Messages `message_stop`。
    MessagesMessageStop,
    /// Messages `ping`。
    MessagesPing,
    /// Messages `error` 或显式错误对象。
    MessagesError,
    /// Chat Completions 普通 JSON chunk。
    ChatChunk,
    /// Responses `response.created`。
    ResponsesCreated,
    /// Responses `response.queued`。
    ResponsesQueued,
    /// Responses `response.in_progress`。
    ResponsesInProgress,
    /// Responses `response.output_item.added`。
    ResponsesOutputItemAdded,
    /// Responses `response.output_item.done`。
    ResponsesOutputItemDone,
    /// Responses `response.content_part.added`。
    ResponsesContentPartAdded,
    /// Responses `response.content_part.done`。
    ResponsesContentPartDone,
    /// Responses `response.output_text.delta`。
    ResponsesOutputTextDelta,
    /// Responses `response.output_text.done`。
    ResponsesOutputTextDone,
    /// Responses `response.refusal.delta`。
    ResponsesRefusalDelta,
    /// Responses `response.refusal.done`。
    ResponsesRefusalDone,
    /// Responses `response.reasoning_summary_part.added`。
    ResponsesReasoningSummaryPartAdded,
    /// Responses `response.reasoning_summary_part.done`。
    ResponsesReasoningSummaryPartDone,
    /// Responses `response.reasoning_summary_text.delta`。
    ResponsesReasoningSummaryTextDelta,
    /// Responses `response.reasoning_summary_text.done`。
    ResponsesReasoningSummaryTextDone,
    /// Responses `response.reasoning_text.delta`。
    ResponsesReasoningTextDelta,
    /// Responses `response.reasoning_text.done`。
    ResponsesReasoningTextDone,
    /// Responses `response.function_call_arguments.delta`。
    ResponsesFunctionArgumentsDelta,
    /// Responses `response.function_call_arguments.done`。
    ResponsesFunctionArgumentsDone,
    /// Responses `response.completed`。
    ResponsesCompleted,
    /// Responses `response.incomplete`。
    ResponsesIncomplete,
    /// Responses `response.cancelled`。
    ResponsesCancelled,
    /// Responses `response.failed`。
    ResponsesFailed,
    /// Responses `error` 或显式错误对象。
    ResponsesError,
}

/// 一个固定 SSE 事件位置的饱和出现次数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KnownSseEventCardinality {
    /// 编译期固定的 SSE 语义事件枚举。
    pub(crate) event: KnownSseEvent,
    /// 事件出现次数的饱和基数。
    pub(crate) cardinality: SaturatedCardinality,
}

/// 一个固定 SSE 事件的数据 JSON 根类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KnownSseDataRootType {
    /// 编译期固定的 SSE 语义事件枚举。
    pub(crate) event: KnownSseEvent,
    /// 该事件全部 JSON data 根类型的合并结果。
    pub(crate) root_type: JsonValueType,
}

/// Chat Completions SSE 终态的固定、无正文依据类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatTerminalEvidence {
    /// 没有观察到 Chat 终态依据。
    None,
    /// 至少观察到一个非空 `finish_reason`。
    FinishReason,
    /// 至少观察到一个显式错误对象。
    Error,
    /// 同时观察到 `finish_reason` 和显式错误对象。
    Both,
}

/// SSE 固定契约违例位。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SseContractViolationBits {
    /// 非空且非 `[DONE]` 的 data 不是有效 JSON。
    pub(crate) invalid_data_json: bool,
    /// 已知事件的 data JSON 根不是对象。
    pub(crate) data_root_type_mismatch: bool,
    /// 已知事件缺少必需字段。
    pub(crate) required_field_missing: bool,
    /// 已知事件字段存在但类型不符合固定契约。
    pub(crate) known_field_type_mismatch: bool,
    /// 已知判别字段包含不受当前 Adapter 支持的值。
    pub(crate) unknown_discriminator_present: bool,
    /// 当前协议不支持所观察到的事件名。
    pub(crate) unknown_event_present: bool,
    /// 既无受支持的 event 字段，也无可用的 data.type。
    pub(crate) missing_effective_event: bool,
    /// Responses event 字段与 data.type 不一致。
    pub(crate) event_data_type_mismatch: bool,
    /// 内容事件在合法开始之前出现。
    pub(crate) start_sequence_violation: bool,
    /// 开始事件重复或在惰性开始之后到达。
    pub(crate) duplicate_start: bool,
    /// 完整观察中没有协议要求的终态。
    pub(crate) terminal_missing: bool,
    /// 观察到多个协议终态。
    pub(crate) duplicate_terminal: bool,
    /// 终态之后仍出现 Adapter 不接受的事件。
    pub(crate) event_after_terminal: bool,
    /// `[DONE]` 在 Chat finish_reason 之前到达。
    pub(crate) done_before_terminal: bool,
    /// 工具或需关闭内容状态在增量、完成、终态时违反浅层顺序。
    pub(crate) tool_sequence_violation: bool,
}

impl SseContractViolationBits {
    /// 创建全部违例位均为假的初始值。
    fn empty() -> Self {
        Self {
            invalid_data_json: false,
            data_root_type_mismatch: false,
            required_field_missing: false,
            known_field_type_mismatch: false,
            unknown_discriminator_present: false,
            unknown_event_present: false,
            missing_effective_event: false,
            event_data_type_mismatch: false,
            start_sequence_violation: false,
            duplicate_start: false,
            terminal_missing: false,
            duplicate_terminal: false,
            event_after_terminal: false,
            done_before_terminal: false,
            tool_sequence_violation: false,
        }
    }

    /// 返回是否观察到任一固定契约违例。
    // 该方法只被非持久化责任归因 API 使用。
    #[allow(dead_code)]
    fn any(&self) -> bool {
        self.invalid_data_json
            || self.data_root_type_mismatch
            || self.required_field_missing
            || self.known_field_type_mismatch
            || self.unknown_discriminator_present
            || self.unknown_event_present
            || self.missing_effective_event
            || self.event_data_type_mismatch
            || self.start_sequence_violation
            || self.duplicate_start
            || self.terminal_missing
            || self.duplicate_terminal
            || self.event_after_terminal
            || self.done_before_terminal
            || self.tool_sequence_violation
    }
}

/// 一份 SSE 正文的非敏感结构证据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SseShapeEvidence {
    /// 固定结构证据模式版本。
    pub(crate) shape_schema: WireShapeSchema,
    /// 按协议固定顺序完整列出的事件基数。
    pub(crate) known_event_cardinality: Vec<KnownSseEventCardinality>,
    /// 是否观察到未知事件；不保存事件名。
    pub(crate) unknown_event_present: bool,
    /// 是否观察到没有显式 `event` 字段的帧。
    pub(crate) missing_event_name_present: bool,
    /// 是否在受检查的 data 对象中观察到未知键；不保存键名。
    pub(crate) unknown_data_key_present: bool,
    /// 按协议固定顺序完整列出的 data JSON 根类型。
    pub(crate) data_json_root_types: Vec<KnownSseDataRootType>,
    /// event 字段和 data.type 是否至少一次不一致。
    pub(crate) event_data_type_mismatch: bool,
    /// 是否观察到协议语义终态。
    pub(crate) terminal_observed: bool,
    /// 协议语义终态次数的饱和基数。
    pub(crate) terminal_cardinality: SaturatedCardinality,
    /// Chat SSE 的固定终态依据；非 Chat 协议必须为 `none`。
    pub(crate) chat_terminal_evidence: ChatTerminalEvidence,
    /// Chat 非空 `finish_reason` 终态依据的饱和基数。
    pub(crate) chat_finish_reason_cardinality: SaturatedCardinality,
    /// Chat 显式错误终态依据的饱和基数。
    pub(crate) chat_error_cardinality: SaturatedCardinality,
    /// 是否观察到字面量 `[DONE]` data。
    pub(crate) done_sentinel_observed: bool,
    /// Responses 是否在 `response.created` 前观察到惰性起始候选。
    pub(crate) lazy_start_observed: bool,
    /// 所观察到的 Responses 惰性起始候选是否具备固定模型身份形态。
    pub(crate) lazy_start_accepted: bool,
    /// Responses SSE 是否在顶层或 response 对象中明确声明错误；不保存原文。
    pub(crate) responses_provider_declared_error: bool,
    /// 是否观察到终态之后的非允许事件。
    pub(crate) event_after_terminal: bool,
    /// 是否观察到 Adapter 会在终态后拒绝的空 data 帧。
    pub(crate) empty_data_after_terminal_observed: bool,
    /// 捕获正文末尾是否还有未以空行分隔的 SSE 帧。
    pub(crate) trailing_partial_frame: bool,
    /// 固定契约违例位。
    pub(crate) violation_bits: SseContractViolationBits,
}

/// 单次 HTTP 交换的独立非敏感 Wire 响应结构证据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WireResponseShapeEvidence {
    /// 生成固定位置数组时采用的协议族。
    pub(crate) protocol: ProviderProtocol,
    /// 收到响应头时记录的 HTTP 状态；无响应头时为空。
    pub(crate) http_status: Option<u16>,
    /// 白名单化后的声明媒体类型类别。
    pub(crate) declared_content_type: DeclaredContentType,
    /// 独立识别出的捕获正文格式。
    pub(crate) body_format: WireBodyFormat,
    /// HTTP 读取器是否明确观察到远端正文 EOF。
    pub(crate) body_eof_observed: bool,
    /// 捕获是否因本地证据字节上限而截断。
    pub(crate) capture_truncated: bool,
    /// 仅当正文格式为 JSON 时存在的 JSON 结构证据。
    pub(crate) json_shape: Option<JsonShapeEvidence>,
    /// 仅当正文格式为 SSE 时存在的 SSE 结构证据。
    pub(crate) sse_shape: Option<SseShapeEvidence>,
}

/// decode 失败的非持久化责任边界判断。
// 该枚举只供测试和人工/离线分析，生产路径刻意不自动持久化责任归因。
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecodeFailureAttribution {
    /// 完整 Wire 事实违反当前固定支持契约。
    Nonconformant,
    /// 完整 Wire 事实是符合契约的供应商显式错误，不属于 decode 失败。
    ProviderDeclaredError,
    /// 完整 Wire 事实符合固定契约，Adapter 需要合成回归复核。
    AdapterSuspect,
    /// 未观察到 EOF、发生捕获截断或证据不足。
    Indeterminate,
}

/// Messages 缓冲 JSON 固定字段顺序。
const MESSAGES_JSON_FIELDS: &[KnownJsonFieldPath] = &[
    KnownJsonFieldPath::MessagesRootType,
    KnownJsonFieldPath::MessagesRootId,
    KnownJsonFieldPath::MessagesRootModel,
    KnownJsonFieldPath::MessagesRootContent,
    KnownJsonFieldPath::MessagesRootStopReason,
    KnownJsonFieldPath::MessagesRootUsage,
    KnownJsonFieldPath::MessagesRootError,
    KnownJsonFieldPath::MessagesContentType,
    KnownJsonFieldPath::MessagesContentText,
    KnownJsonFieldPath::MessagesContentThinking,
    KnownJsonFieldPath::MessagesContentSignature,
    KnownJsonFieldPath::MessagesContentData,
    KnownJsonFieldPath::MessagesContentId,
    KnownJsonFieldPath::MessagesContentName,
    KnownJsonFieldPath::MessagesContentInput,
];

/// Chat Completions 缓冲 JSON 固定字段顺序。
const CHAT_JSON_FIELDS: &[KnownJsonFieldPath] = &[
    KnownJsonFieldPath::ChatRootId,
    KnownJsonFieldPath::ChatRootModel,
    KnownJsonFieldPath::ChatRootChoices,
    KnownJsonFieldPath::ChatRootUsage,
    KnownJsonFieldPath::ChatRootError,
    KnownJsonFieldPath::ChatChoiceMessage,
    KnownJsonFieldPath::ChatChoiceFinishReason,
    KnownJsonFieldPath::ChatMessageContent,
    KnownJsonFieldPath::ChatMessageReasoningContent,
    KnownJsonFieldPath::ChatMessageReasoning,
    KnownJsonFieldPath::ChatMessageReasoningDetails,
    KnownJsonFieldPath::ChatMessageRefusal,
    KnownJsonFieldPath::ChatMessageToolCalls,
    KnownJsonFieldPath::ChatContentPartType,
    KnownJsonFieldPath::ChatContentPartText,
    KnownJsonFieldPath::ChatReasoningDetailText,
    KnownJsonFieldPath::ChatReasoningDetailDelta,
    KnownJsonFieldPath::ChatToolCallId,
    KnownJsonFieldPath::ChatToolCallFunction,
    KnownJsonFieldPath::ChatToolFunctionName,
    KnownJsonFieldPath::ChatToolFunctionArguments,
];

/// Responses 缓冲 JSON 固定字段顺序。
const RESPONSES_JSON_FIELDS: &[KnownJsonFieldPath] = &[
    KnownJsonFieldPath::ResponsesRootId,
    KnownJsonFieldPath::ResponsesRootModel,
    KnownJsonFieldPath::ResponsesRootStatus,
    KnownJsonFieldPath::ResponsesRootOutput,
    KnownJsonFieldPath::ResponsesRootUsage,
    KnownJsonFieldPath::ResponsesRootError,
    KnownJsonFieldPath::ResponsesRootIncompleteDetails,
    KnownJsonFieldPath::ResponsesOutputType,
    KnownJsonFieldPath::ResponsesOutputContent,
    KnownJsonFieldPath::ResponsesOutputSummary,
    KnownJsonFieldPath::ResponsesOutputId,
    KnownJsonFieldPath::ResponsesOutputEncryptedContent,
    KnownJsonFieldPath::ResponsesOutputCallId,
    KnownJsonFieldPath::ResponsesOutputName,
    KnownJsonFieldPath::ResponsesOutputArguments,
    KnownJsonFieldPath::ResponsesContentPartType,
    KnownJsonFieldPath::ResponsesContentPartText,
    KnownJsonFieldPath::ResponsesContentPartRefusal,
    KnownJsonFieldPath::ResponsesSummaryPartText,
    KnownJsonFieldPath::ResponsesIncompleteReason,
];

/// Messages 缓冲 JSON 固定容器顺序。
const MESSAGES_JSON_NESTED: &[KnownJsonNestedPath] = &[KnownJsonNestedPath::MessagesContentItems];

/// Chat Completions 缓冲 JSON 固定容器顺序。
const CHAT_JSON_NESTED: &[KnownJsonNestedPath] = &[
    KnownJsonNestedPath::ChatChoices,
    KnownJsonNestedPath::ChatContentParts,
    KnownJsonNestedPath::ChatReasoningDetails,
    KnownJsonNestedPath::ChatToolCalls,
];

/// Responses 缓冲 JSON 固定容器顺序。
const RESPONSES_JSON_NESTED: &[KnownJsonNestedPath] = &[
    KnownJsonNestedPath::ResponsesOutputItems,
    KnownJsonNestedPath::ResponsesContentParts,
    KnownJsonNestedPath::ResponsesSummaryParts,
];

/// Messages SSE 固定事件顺序。
const MESSAGES_SSE_EVENTS: &[KnownSseEvent] = &[
    KnownSseEvent::MessagesMessageStart,
    KnownSseEvent::MessagesContentBlockStart,
    KnownSseEvent::MessagesContentBlockDelta,
    KnownSseEvent::MessagesContentBlockStop,
    KnownSseEvent::MessagesMessageDelta,
    KnownSseEvent::MessagesMessageStop,
    KnownSseEvent::MessagesPing,
    KnownSseEvent::MessagesError,
];

/// Chat Completions SSE 固定事件顺序。
const CHAT_SSE_EVENTS: &[KnownSseEvent] = &[KnownSseEvent::ChatChunk];

/// Responses SSE 固定事件顺序。
const RESPONSES_SSE_EVENTS: &[KnownSseEvent] = &[
    KnownSseEvent::ResponsesCreated,
    KnownSseEvent::ResponsesQueued,
    KnownSseEvent::ResponsesInProgress,
    KnownSseEvent::ResponsesOutputItemAdded,
    KnownSseEvent::ResponsesOutputItemDone,
    KnownSseEvent::ResponsesContentPartAdded,
    KnownSseEvent::ResponsesContentPartDone,
    KnownSseEvent::ResponsesOutputTextDelta,
    KnownSseEvent::ResponsesOutputTextDone,
    KnownSseEvent::ResponsesRefusalDelta,
    KnownSseEvent::ResponsesRefusalDone,
    KnownSseEvent::ResponsesReasoningSummaryPartAdded,
    KnownSseEvent::ResponsesReasoningSummaryPartDone,
    KnownSseEvent::ResponsesReasoningSummaryTextDelta,
    KnownSseEvent::ResponsesReasoningSummaryTextDone,
    KnownSseEvent::ResponsesReasoningTextDelta,
    KnownSseEvent::ResponsesReasoningTextDone,
    KnownSseEvent::ResponsesFunctionArgumentsDelta,
    KnownSseEvent::ResponsesFunctionArgumentsDone,
    KnownSseEvent::ResponsesCompleted,
    KnownSseEvent::ResponsesIncomplete,
    KnownSseEvent::ResponsesCancelled,
    KnownSseEvent::ResponsesFailed,
    KnownSseEvent::ResponsesError,
];

/// 固定 JSON 路径的一段内部导航规则。
#[derive(Clone, Copy)]
enum JsonPathSegment {
    /// 进入一个编译期固定键。
    Key(&'static str),
    /// 遍历当前位置的数组元素。
    Each,
}

impl KnownJsonFieldPath {
    /// 返回该枚举对应的编译期固定导航路径。
    fn segments(self) -> &'static [JsonPathSegment] {
        use JsonPathSegment::{Each, Key};
        match self {
            Self::MessagesRootType => &[Key("type")],
            Self::MessagesRootId => &[Key("id")],
            Self::MessagesRootModel => &[Key("model")],
            Self::MessagesRootContent => &[Key("content")],
            Self::MessagesRootStopReason => &[Key("stop_reason")],
            Self::MessagesRootUsage => &[Key("usage")],
            Self::MessagesRootError => &[Key("error")],
            Self::MessagesContentType => &[Key("content"), Each, Key("type")],
            Self::MessagesContentText => &[Key("content"), Each, Key("text")],
            Self::MessagesContentThinking => &[Key("content"), Each, Key("thinking")],
            Self::MessagesContentSignature => &[Key("content"), Each, Key("signature")],
            Self::MessagesContentData => &[Key("content"), Each, Key("data")],
            Self::MessagesContentId => &[Key("content"), Each, Key("id")],
            Self::MessagesContentName => &[Key("content"), Each, Key("name")],
            Self::MessagesContentInput => &[Key("content"), Each, Key("input")],
            Self::ChatRootId => &[Key("id")],
            Self::ChatRootModel => &[Key("model")],
            Self::ChatRootChoices => &[Key("choices")],
            Self::ChatRootUsage => &[Key("usage")],
            Self::ChatRootError => &[Key("error")],
            Self::ChatChoiceMessage => &[Key("choices"), Each, Key("message")],
            Self::ChatChoiceFinishReason => &[Key("choices"), Each, Key("finish_reason")],
            Self::ChatMessageContent => &[Key("choices"), Each, Key("message"), Key("content")],
            Self::ChatMessageReasoningContent => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("reasoning_content"),
            ],
            Self::ChatMessageReasoning => &[Key("choices"), Each, Key("message"), Key("reasoning")],
            Self::ChatMessageReasoningDetails => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("reasoning_details"),
            ],
            Self::ChatMessageRefusal => &[Key("choices"), Each, Key("message"), Key("refusal")],
            Self::ChatMessageToolCalls => {
                &[Key("choices"), Each, Key("message"), Key("tool_calls")]
            }
            Self::ChatContentPartType => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("content"),
                Each,
                Key("type"),
            ],
            Self::ChatContentPartText => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("content"),
                Each,
                Key("text"),
            ],
            Self::ChatReasoningDetailText => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("reasoning_details"),
                Each,
                Key("text"),
            ],
            Self::ChatReasoningDetailDelta => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("reasoning_details"),
                Each,
                Key("delta"),
            ],
            Self::ChatToolCallId => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("tool_calls"),
                Each,
                Key("id"),
            ],
            Self::ChatToolCallFunction => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("tool_calls"),
                Each,
                Key("function"),
            ],
            Self::ChatToolFunctionName => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("tool_calls"),
                Each,
                Key("function"),
                Key("name"),
            ],
            Self::ChatToolFunctionArguments => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("tool_calls"),
                Each,
                Key("function"),
                Key("arguments"),
            ],
            Self::ResponsesRootId => &[Key("id")],
            Self::ResponsesRootModel => &[Key("model")],
            Self::ResponsesRootStatus => &[Key("status")],
            Self::ResponsesRootOutput => &[Key("output")],
            Self::ResponsesRootUsage => &[Key("usage")],
            Self::ResponsesRootError => &[Key("error")],
            Self::ResponsesRootIncompleteDetails => &[Key("incomplete_details")],
            Self::ResponsesOutputType => &[Key("output"), Each, Key("type")],
            Self::ResponsesOutputContent => &[Key("output"), Each, Key("content")],
            Self::ResponsesOutputSummary => &[Key("output"), Each, Key("summary")],
            Self::ResponsesOutputId => &[Key("output"), Each, Key("id")],
            Self::ResponsesOutputEncryptedContent => {
                &[Key("output"), Each, Key("encrypted_content")]
            }
            Self::ResponsesOutputCallId => &[Key("output"), Each, Key("call_id")],
            Self::ResponsesOutputName => &[Key("output"), Each, Key("name")],
            Self::ResponsesOutputArguments => &[Key("output"), Each, Key("arguments")],
            Self::ResponsesContentPartType => {
                &[Key("output"), Each, Key("content"), Each, Key("type")]
            }
            Self::ResponsesContentPartText => {
                &[Key("output"), Each, Key("content"), Each, Key("text")]
            }
            Self::ResponsesContentPartRefusal => {
                &[Key("output"), Each, Key("content"), Each, Key("refusal")]
            }
            Self::ResponsesSummaryPartText => {
                &[Key("output"), Each, Key("summary"), Each, Key("text")]
            }
            Self::ResponsesIncompleteReason => &[Key("incomplete_details"), Key("reason")],
        }
    }
}

impl KnownJsonNestedPath {
    /// 返回该容器枚举对应的编译期固定导航路径。
    fn segments(self) -> &'static [JsonPathSegment] {
        use JsonPathSegment::{Each, Key};
        match self {
            Self::MessagesContentItems => &[Key("content")],
            Self::ChatChoices => &[Key("choices")],
            Self::ChatContentParts => &[Key("choices"), Each, Key("message"), Key("content")],
            Self::ChatReasoningDetails => &[
                Key("choices"),
                Each,
                Key("message"),
                Key("reasoning_details"),
            ],
            Self::ChatToolCalls => &[Key("choices"), Each, Key("message"), Key("tool_calls")],
            Self::ResponsesOutputItems => &[Key("output")],
            Self::ResponsesContentParts => &[Key("output"), Each, Key("content")],
            Self::ResponsesSummaryParts => &[Key("output"), Each, Key("summary")],
        }
    }
}

/// 把任意媒体类型值立即降级为固定白名单类别。
pub(crate) fn classify_declared_content_type(content_type: Option<&str>) -> DeclaredContentType {
    let Some(content_type) = content_type else {
        return DeclaredContentType::Missing;
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    let json_suffix = media_type.split_once('/').is_some_and(|(kind, subtype)| {
        !kind.is_empty() && subtype.to_ascii_lowercase().ends_with("+json")
    });
    if media_type.eq_ignore_ascii_case("text/event-stream") {
        DeclaredContentType::Sse
    } else if media_type.eq_ignore_ascii_case("application/json")
        || media_type.eq_ignore_ascii_case("text/json")
        || json_suffix
    {
        DeclaredContentType::Json
    } else if media_type.is_empty() {
        DeclaredContentType::Missing
    } else {
        DeclaredContentType::Other
    }
}

/// 独立检查一次捕获交换并只返回固定非敏感结构事实。
pub(crate) fn inspect_wire_response_shape(
    protocol: ProviderProtocol,
    http_status: Option<u16>,
    response_content_type: Option<&str>,
    response_body: &[u8],
    body_eof_observed: bool,
    capture_truncated: bool,
) -> WireResponseShapeEvidence {
    let declared_content_type = classify_declared_content_type(response_content_type);
    let (body_format, json_shape, sse_shape) = if response_body.is_empty() {
        (WireBodyFormat::Empty, None, None)
    } else if std::str::from_utf8(response_body).is_err() {
        (WireBodyFormat::InvalidUtf8, None, None)
    } else {
        let body_without_bom = response_body
            .strip_prefix(&[0xEF, 0xBB, 0xBF])
            .unwrap_or(response_body);
        let parsed_json = serde_json::from_slice::<Value>(body_without_bom).ok();
        let use_sse = matches!(declared_content_type, DeclaredContentType::Sse)
            || body_without_bom.starts_with(b"data:")
            || body_without_bom.starts_with(b"event:")
            || body_without_bom.starts_with(b":");
        if let Some(value) = parsed_json {
            (
                WireBodyFormat::Json,
                Some(inspect_json_shape(protocol, &value)),
                None,
            )
        } else if use_sse {
            (
                WireBodyFormat::Sse,
                None,
                Some(inspect_sse_shape(protocol, response_body)),
            )
        } else {
            (WireBodyFormat::Unknown, None, None)
        }
    };
    WireResponseShapeEvidence {
        protocol,
        http_status,
        declared_content_type,
        body_format,
        body_eof_observed,
        capture_truncated,
        json_shape,
        sse_shape,
    }
}

/// 返回指定协议的固定 JSON 字段顺序。
fn expected_json_fields(protocol: ProviderProtocol) -> &'static [KnownJsonFieldPath] {
    match protocol {
        ProviderProtocol::Messages => MESSAGES_JSON_FIELDS,
        ProviderProtocol::ChatCompletions => CHAT_JSON_FIELDS,
        ProviderProtocol::Responses => RESPONSES_JSON_FIELDS,
    }
}

/// 返回指定协议的固定 JSON 容器顺序。
fn expected_json_nested(protocol: ProviderProtocol) -> &'static [KnownJsonNestedPath] {
    match protocol {
        ProviderProtocol::Messages => MESSAGES_JSON_NESTED,
        ProviderProtocol::ChatCompletions => CHAT_JSON_NESTED,
        ProviderProtocol::Responses => RESPONSES_JSON_NESTED,
    }
}

/// 返回指定协议的固定 SSE 事件顺序。
fn expected_sse_events(protocol: ProviderProtocol) -> &'static [KnownSseEvent] {
    match protocol {
        ProviderProtocol::Messages => MESSAGES_SSE_EVENTS,
        ProviderProtocol::ChatCompletions => CHAT_SSE_EVENTS,
        ProviderProtocol::Responses => RESPONSES_SSE_EVENTS,
    }
}

/// 检查完整缓冲 JSON 的固定字段、容器、未知键和契约位。
fn inspect_json_shape(protocol: ProviderProtocol, value: &Value) -> JsonShapeEvidence {
    let known_field_types = expected_json_fields(protocol)
        .iter()
        .copied()
        .map(|path| KnownJsonFieldType {
            path,
            value_type: observe_json_path_type(value, path.segments()),
        })
        .collect();
    let known_nested_path_shapes = expected_json_nested(protocol)
        .iter()
        .copied()
        .map(|path| observe_json_nested_shape(value, path))
        .collect();
    let unknown_key_present = json_unknown_key_present(protocol, value);
    let provider_declared_error = json_provider_declared_error(protocol, value);
    let violation_bits = match protocol {
        ProviderProtocol::Messages => validate_messages_json_contract(value),
        ProviderProtocol::ChatCompletions => validate_chat_json_contract(value),
        ProviderProtocol::Responses => validate_responses_json_contract(value),
    };
    JsonShapeEvidence {
        shape_schema: WireShapeSchema::V2,
        root_type: JsonValueType::of(value),
        known_field_types,
        unknown_key_present,
        known_nested_path_shapes,
        provider_declared_error,
        violation_bits,
    }
}

/// 判断缓冲 JSON 是否为协议明确声明的错误响应，不返回任何正文值。
fn json_provider_declared_error(protocol: ProviderProtocol, value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    match protocol {
        ProviderProtocol::Messages => {
            root.get("type").and_then(Value::as_str) == Some("error")
                || root.get("error").is_some_and(|error| !error.is_null())
        }
        ProviderProtocol::ChatCompletions => {
            root.get("error").is_some_and(|error| !error.is_null())
        }
        ProviderProtocol::Responses => {
            root.get("status").and_then(Value::as_str) == Some("failed")
                || root.get("error").is_some_and(|error| !error.is_null())
        }
    }
}

/// 合并固定导航路径匹配到的全部 JSON 类型。
fn observe_json_path_type(value: &Value, segments: &[JsonPathSegment]) -> JsonValueType {
    let mut observed = JsonValueType::Missing;
    visit_json_path(value, segments, &mut |matched| {
        observed = observed.merge(JsonValueType::of(matched));
    });
    observed
}

/// 递归遍历编译期固定路径，不接受来自响应的路径文本。
fn visit_json_path<'a>(
    value: &'a Value,
    segments: &[JsonPathSegment],
    visitor: &mut impl FnMut(&'a Value),
) {
    let Some((head, tail)) = segments.split_first() else {
        visitor(value);
        return;
    };
    match head {
        JsonPathSegment::Key(key) => {
            if let Some(child) = value.as_object().and_then(|object| object.get(*key)) {
                visit_json_path(child, tail, visitor);
            }
        }
        JsonPathSegment::Each => {
            if let Some(items) = value.as_array() {
                for item in items {
                    visit_json_path(item, tail, visitor);
                }
            }
        }
    }
}

/// 汇总固定容器位置的元素基数和元素类型。
fn observe_json_nested_shape(value: &Value, path: KnownJsonNestedPath) -> KnownJsonNestedShape {
    let mut cardinality = SaturatedCardinality::Zero;
    let mut element_type = JsonValueType::Missing;
    visit_json_path(value, path.segments(), &mut |container| {
        if let Some(items) = container.as_array() {
            cardinality.observe_len(items.len());
            for item in items {
                element_type = element_type.merge(JsonValueType::of(item));
            }
        }
    });
    KnownJsonNestedShape {
        path,
        cardinality,
        element_type,
    }
}

/// 校验 Messages 缓冲 JSON 的当前固定支持契约。
fn validate_messages_json_contract(value: &Value) -> JsonContractViolationBits {
    let mut bits = JsonContractViolationBits::empty();
    let Some(root) = value.as_object() else {
        bits.root_type_mismatch = true;
        return bits;
    };
    check_optional_type(root, "type", &[JsonValueType::String], true, &mut bits);
    check_optional_non_blank_string(root, "id", &mut bits);
    check_optional_non_blank_string(root, "model", &mut bits);
    check_optional_type(
        root,
        "stop_reason",
        &[JsonValueType::String],
        true,
        &mut bits,
    );
    check_optional_type(root, "usage", &[JsonValueType::Object], true, &mut bits);
    let explicit_error =
        root.get("type").and_then(Value::as_str) == Some("error") || root.contains_key("error");
    if explicit_error {
        return bits;
    }
    let Some(content) = required_array(root, "content", &mut bits) else {
        return bits;
    };
    for item in content {
        let Some(item) = item.as_object() else {
            bits.known_nested_shape_mismatch = true;
            continue;
        };
        let Some(item_type) = required_string(item, "type", &mut bits) else {
            continue;
        };
        match item_type {
            "text" => {
                required_string(item, "text", &mut bits);
            }
            "thinking" => {
                required_string(item, "thinking", &mut bits);
                check_optional_type(item, "signature", &[JsonValueType::String], true, &mut bits);
            }
            "redacted_thinking" => {
                if !item.contains_key("data") {
                    bits.required_field_missing = true;
                }
            }
            "tool_use" => {
                required_string(item, "id", &mut bits);
                required_string(item, "name", &mut bits);
                if !item.contains_key("input") {
                    bits.required_field_missing = true;
                }
            }
            _ => bits.unknown_discriminator_present = true,
        }
    }
    bits
}

/// 校验 Chat Completions 缓冲 JSON 的当前固定支持契约。
fn validate_chat_json_contract(value: &Value) -> JsonContractViolationBits {
    let mut bits = JsonContractViolationBits::empty();
    let Some(root) = value.as_object() else {
        bits.root_type_mismatch = true;
        return bits;
    };
    check_optional_non_blank_string(root, "id", &mut bits);
    check_optional_non_blank_string(root, "model", &mut bits);
    check_optional_type(root, "usage", &[JsonValueType::Object], true, &mut bits);
    if root.contains_key("error") {
        return bits;
    }
    let Some(choices) = required_array(root, "choices", &mut bits) else {
        return bits;
    };
    if choices.len() != 1 {
        bits.expected_singleton_mismatch = true;
    }
    for choice in choices.iter().take(1) {
        let Some(choice) = choice.as_object() else {
            bits.known_nested_shape_mismatch = true;
            continue;
        };
        check_optional_type(
            choice,
            "finish_reason",
            &[JsonValueType::String],
            true,
            &mut bits,
        );
        let Some(message) = required_object(choice, "message", &mut bits) else {
            continue;
        };
        check_chat_message_contract(message, false, &mut bits);
    }
    bits
}

/// 校验 Chat message 或流式 delta 中 Adapter 读取的固定字段。
fn check_chat_message_contract(
    message: &Map<String, Value>,
    streaming: bool,
    bits: &mut JsonContractViolationBits,
) {
    for field in ["reasoning_content", "reasoning", "refusal"] {
        check_optional_type(message, field, &[JsonValueType::String], true, bits);
    }
    if let Some(details) = message.get("reasoning_details") {
        if details.is_null() {
        } else if let Some(details) = details.as_array() {
            for detail in details {
                let Some(detail) = detail.as_object() else {
                    bits.known_nested_shape_mismatch = true;
                    continue;
                };
                for field in ["text", "delta"] {
                    check_optional_type(detail, field, &[JsonValueType::String], true, bits);
                }
            }
        } else {
            bits.known_field_type_mismatch = true;
        }
    }
    if let Some(content) = message.get("content") {
        match content {
            Value::Null | Value::String(_) => {}
            Value::Array(parts) => {
                for part in parts {
                    let Some(part) = part.as_object() else {
                        bits.known_nested_shape_mismatch = true;
                        continue;
                    };
                    if let Some(part_type) = part.get("type") {
                        match part_type.as_str() {
                            Some("text" | "output_text") => {}
                            Some(_) => bits.unknown_discriminator_present = true,
                            None => bits.known_field_type_mismatch = true,
                        }
                    }
                    check_optional_type(part, "text", &[JsonValueType::String], true, bits);
                }
            }
            _ => bits.known_field_type_mismatch = true,
        }
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        if tool_calls.is_null() {
        } else if let Some(tool_calls) = tool_calls.as_array() {
            for tool_call in tool_calls {
                let Some(tool_call) = tool_call.as_object() else {
                    bits.known_nested_shape_mismatch = true;
                    continue;
                };
                if streaming {
                    required_non_negative_integer(tool_call, "index", bits);
                    check_optional_type(tool_call, "id", &[JsonValueType::String], true, bits);
                    if let Some(function) = tool_call.get("function") {
                        if let Some(function) = function.as_object() {
                            for field in ["name", "arguments"] {
                                check_optional_type(
                                    function,
                                    field,
                                    &[JsonValueType::String],
                                    true,
                                    bits,
                                );
                            }
                        } else if !function.is_null() {
                            bits.known_field_type_mismatch = true;
                        }
                    }
                } else {
                    required_string(tool_call, "id", bits);
                    if let Some(function) = required_object(tool_call, "function", bits) {
                        required_string(function, "name", bits);
                        required_string(function, "arguments", bits);
                    }
                }
            }
        } else {
            bits.known_field_type_mismatch = true;
        }
    }
}

/// 校验 Responses 缓冲 JSON 的当前固定支持契约。
fn validate_responses_json_contract(value: &Value) -> JsonContractViolationBits {
    let mut bits = JsonContractViolationBits::empty();
    let Some(root) = value.as_object() else {
        bits.root_type_mismatch = true;
        return bits;
    };
    check_optional_non_blank_string(root, "id", &mut bits);
    check_optional_non_blank_string(root, "model", &mut bits);
    check_optional_type(root, "status", &[JsonValueType::String], true, &mut bits);
    check_optional_type(root, "usage", &[JsonValueType::Object], true, &mut bits);
    check_optional_type(
        root,
        "incomplete_details",
        &[JsonValueType::Object],
        true,
        &mut bits,
    );
    let explicit_error = root.get("error").is_some_and(|error| !error.is_null())
        || root.get("status").and_then(Value::as_str) == Some("failed");
    if explicit_error {
        return bits;
    }
    let Some(output) = required_array(root, "output", &mut bits) else {
        return bits;
    };
    for item in output {
        let Some(item) = item.as_object() else {
            bits.known_nested_shape_mismatch = true;
            continue;
        };
        let Some(item_type) = required_string(item, "type", &mut bits) else {
            continue;
        };
        match item_type {
            "message" => check_responses_message_item(item, &mut bits),
            "reasoning" => check_responses_reasoning_item(item, &mut bits),
            "function_call" => {
                if item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .is_none()
                {
                    if item.contains_key("call_id") || item.contains_key("id") {
                        bits.known_field_type_mismatch = true;
                    } else {
                        bits.required_field_missing = true;
                    }
                }
                required_string(item, "name", &mut bits);
                required_string(item, "arguments", &mut bits);
            }
            _ => bits.unknown_discriminator_present = true,
        }
    }
    bits
}

/// 校验 Responses 完整 message item 的内容数组。
fn check_responses_message_item(item: &Map<String, Value>, bits: &mut JsonContractViolationBits) {
    let Some(content) = item.get("content") else {
        return;
    };
    if content.is_null() {
        return;
    }
    let Some(content) = content.as_array() else {
        bits.known_field_type_mismatch = true;
        return;
    };
    for part in content {
        let Some(part) = part.as_object() else {
            bits.known_nested_shape_mismatch = true;
            continue;
        };
        let Some(part_type) = required_string(part, "type", bits) else {
            continue;
        };
        match part_type {
            "output_text" => {
                required_string(part, "text", bits);
            }
            "refusal" => {
                required_string(part, "refusal", bits);
            }
            _ => bits.unknown_discriminator_present = true,
        }
    }
}

/// 校验 Responses 完整 reasoning item 的内容与摘要数组。
fn check_responses_reasoning_item(item: &Map<String, Value>, bits: &mut JsonContractViolationBits) {
    if let Some(content) = item.get("content") {
        if content.is_null() {
        } else if let Some(content) = content.as_array() {
            for part in content {
                let Some(part) = part.as_object() else {
                    bits.known_nested_shape_mismatch = true;
                    continue;
                };
                let Some(part_type) = required_string(part, "type", bits) else {
                    continue;
                };
                match part_type {
                    "reasoning_text" | "output_text" => {
                        required_string(part, "text", bits);
                    }
                    _ => bits.unknown_discriminator_present = true,
                }
            }
        } else {
            bits.known_field_type_mismatch = true;
        }
    }
    if let Some(summary) = item.get("summary") {
        if summary.is_null() {
        } else if let Some(summary) = summary.as_array() {
            for part in summary {
                let Some(part) = part.as_object() else {
                    bits.known_nested_shape_mismatch = true;
                    continue;
                };
                check_optional_type(part, "text", &[JsonValueType::String], true, bits);
            }
        } else {
            bits.known_field_type_mismatch = true;
        }
    }
}

/// 读取必需数组，并以固定布尔位区分缺失与类型错误。
fn required_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut JsonContractViolationBits,
) -> Option<&'a [Value]> {
    match object.get(field) {
        Some(Value::Array(items)) => Some(items),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 读取必需对象，并以固定布尔位区分缺失与类型错误。
fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut JsonContractViolationBits,
) -> Option<&'a Map<String, Value>> {
    match object.get(field) {
        Some(Value::Object(value)) => Some(value),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 读取必需字符串，并且不返回或持久化除借用外的任何副本。
fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut JsonContractViolationBits,
) -> Option<&'a str> {
    match object.get(field) {
        Some(Value::String(value)) => Some(value),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 校验必需非负整数形态而不保存数值。
fn required_non_negative_integer(
    object: &Map<String, Value>,
    field: &str,
    bits: &mut JsonContractViolationBits,
) -> bool {
    match object.get(field) {
        Some(value)
            if value
                .as_u64()
                .is_some_and(|number| u32::try_from(number).is_ok()) =>
        {
            true
        }
        Some(_) => {
            bits.known_field_type_mismatch = true;
            false
        }
        None => {
            bits.required_field_missing = true;
            false
        }
    }
}

/// 校验可选字段是否属于固定类型集合，并按需接受 null。
fn check_optional_type(
    object: &Map<String, Value>,
    field: &str,
    accepted: &[JsonValueType],
    null_allowed: bool,
    bits: &mut JsonContractViolationBits,
) {
    let Some(value) = object.get(field) else {
        return;
    };
    if value.is_null() && null_allowed {
        return;
    }
    if !accepted.contains(&JsonValueType::of(value)) {
        bits.known_field_type_mismatch = true;
    }
}

/// 校验可选响应元数据字段缺失或为 null 时可接受，非空字符串以外均记为契约违例。
fn check_optional_non_blank_string(
    object: &Map<String, Value>,
    field: &str,
    bits: &mut JsonContractViolationBits,
) {
    match object.get(field) {
        None | Some(Value::Null) => {}
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        Some(_) => bits.known_field_type_mismatch = true,
    }
}

/// 校验 SSE 响应元数据字段缺失或为 null 时可接受，非空字符串以外均记为契约违例。
fn sse_check_optional_non_blank_string(
    object: &Map<String, Value>,
    field: &str,
    bits: &mut SseContractViolationBits,
) {
    match object.get(field) {
        None | Some(Value::Null) => {}
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        Some(_) => bits.known_field_type_mismatch = true,
    }
}

/// 检查协议固定对象位置上是否出现未知键，不保留键名或值。
fn json_unknown_key_present(protocol: ProviderProtocol, value: &Value) -> bool {
    match protocol {
        ProviderProtocol::Messages => messages_json_unknown_key_present(value),
        ProviderProtocol::ChatCompletions => chat_json_unknown_key_present(value),
        ProviderProtocol::Responses => responses_json_unknown_key_present(value),
    }
}

/// 检查 Messages 缓冲 JSON 的固定对象白名单。
fn messages_json_unknown_key_present(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = has_unknown_key(
        root,
        &[
            "type",
            "id",
            "model",
            "content",
            "stop_reason",
            "usage",
            "error",
        ],
    );
    if let Some(content) = root.get("content").and_then(Value::as_array) {
        for item in content.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(
                item,
                &[
                    "type",
                    "text",
                    "thinking",
                    "signature",
                    "data",
                    "id",
                    "name",
                    "input",
                ],
            );
        }
    }
    if let Some(usage) = root.get("usage").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            usage,
            &[
                "input_tokens",
                "output_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ],
        );
    }
    unknown
}

/// 检查 Chat Completions 缓冲 JSON 的固定对象白名单。
fn chat_json_unknown_key_present(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = has_unknown_key(root, &["id", "model", "choices", "usage", "error"]);
    if let Some(choices) = root.get("choices").and_then(Value::as_array) {
        for choice in choices.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(choice, &["message", "finish_reason"]);
            if let Some(message) = choice.get("message").and_then(Value::as_object) {
                unknown |= chat_message_unknown_key_present(message, false);
            }
        }
    }
    if let Some(usage) = root.get("usage").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            usage,
            &[
                "prompt_tokens",
                "completion_tokens",
                "total_tokens",
                "completion_tokens_details",
            ],
        );
    }
    unknown
}

/// 检查 Chat message 或 delta 的固定对象白名单。
fn chat_message_unknown_key_present(message: &Map<String, Value>, streaming: bool) -> bool {
    let mut allowed = vec![
        "content",
        "reasoning_content",
        "reasoning",
        "reasoning_details",
        "refusal",
        "tool_calls",
    ];
    if !streaming {
        allowed.push("role");
    }
    let mut unknown = has_unknown_key(message, &allowed);
    if let Some(parts) = message.get("content").and_then(Value::as_array) {
        for part in parts.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(part, &["type", "text"]);
        }
    }
    if let Some(details) = message.get("reasoning_details").and_then(Value::as_array) {
        for detail in details.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(detail, &["text", "delta"]);
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls.iter().filter_map(Value::as_object) {
            unknown |= if streaming {
                has_unknown_key(tool_call, &["index", "id", "type", "function"])
            } else {
                has_unknown_key(tool_call, &["id", "type", "function"])
            };
            if let Some(function) = tool_call.get("function").and_then(Value::as_object) {
                unknown |= has_unknown_key(function, &["name", "arguments"]);
            }
        }
    }
    unknown
}

/// 检查 Responses 缓冲 JSON 的固定对象白名单。
fn responses_json_unknown_key_present(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = has_unknown_key(
        root,
        &[
            "id",
            "model",
            "status",
            "output",
            "usage",
            "error",
            "incomplete_details",
        ],
    );
    if let Some(output) = root.get("output").and_then(Value::as_array) {
        for item in output.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(
                item,
                &[
                    "type",
                    "content",
                    "summary",
                    "id",
                    "encrypted_content",
                    "call_id",
                    "name",
                    "arguments",
                    "role",
                    "status",
                ],
            );
            for field in ["content", "summary"] {
                if let Some(parts) = item.get(field).and_then(Value::as_array) {
                    for part in parts.iter().filter_map(Value::as_object) {
                        unknown |= has_unknown_key(part, &["type", "text", "refusal"]);
                    }
                }
            }
        }
    }
    if let Some(details) = root.get("incomplete_details").and_then(Value::as_object) {
        unknown |= has_unknown_key(details, &["reason"]);
    }
    unknown
}

/// 返回对象是否含有固定白名单外的键，绝不返回未知键文本。
fn has_unknown_key(object: &Map<String, Value>, allowed: &[&str]) -> bool {
    object
        .keys()
        .any(|key| !allowed.iter().any(|allowed_key| key == allowed_key))
}

/// 一个只在检查调用栈中存在的独立 SSE 帧。
struct ParsedSseFrame {
    /// 最后一个 `event` 字段的临时值。
    event: Option<String>,
    /// 按 SSE 规则用换行临时连接的 data 字段。
    data: String,
}

/// 独立 SSE 浅层检查器的瞬时状态。
struct SseInspectionState {
    /// 当前协议的固定事件列表。
    events: &'static [KnownSseEvent],
    /// 固定事件的饱和次数。
    cardinalities: Vec<SaturatedCardinality>,
    /// 固定事件的 data 根类型。
    root_types: Vec<JsonValueType>,
    /// 是否观察到未知事件。
    unknown_event_present: bool,
    /// 是否观察到缺少显式 event 的帧。
    missing_event_name_present: bool,
    /// 是否在固定对象位置观察到未知键。
    unknown_data_key_present: bool,
    /// 是否观察到 event 与 data.type 不一致。
    event_data_type_mismatch: bool,
    /// 是否已经观察到协议开始语义。
    started: bool,
    /// 语义终态次数的饱和值。
    terminal_count: SaturatedCardinality,
    /// Chat finish_reason 终态依据的瞬时饱和基数。
    chat_finish_reason_cardinality: SaturatedCardinality,
    /// Chat 显式错误终态依据的瞬时饱和基数。
    chat_error_cardinality: SaturatedCardinality,
    /// Chat Adapter 是否已由带 finish_reason 的 `[DONE]` 真正结束。
    chat_adapter_ended: bool,
    /// 是否观察到 `[DONE]`。
    done_sentinel_observed: bool,
    /// 是否观察到 Adapter 会在终态后拒绝的空 data 帧。
    empty_data_after_terminal_observed: bool,
    /// 是否观察到 Responses 惰性起始候选。
    lazy_start_observed: bool,
    /// 所有已观察惰性起始候选是否都具备固定模型身份形态。
    lazy_start_all_accepted: bool,
    /// 是否观察到 Responses 顶层或 response 对象明确声明的错误。
    responses_provider_declared_error: bool,
    /// Responses 工具 output_index 到是否结束的瞬时状态。
    response_tools: BTreeMap<u64, bool>,
    /// Chat 工具 wire index 到 ID 与名称是否齐全的瞬时状态。
    chat_tools: BTreeMap<u64, (bool, bool)>,
    /// Messages 工具或签名状态按内容块 index 的瞬时开放状态。
    messages_open_blocks: BTreeMap<u64, MessagesOpenBlock>,
    /// 固定 SSE 契约违例位。
    violation_bits: SseContractViolationBits,
}

impl SseInspectionState {
    /// 为一个协议创建不含正文和任意字符串的检查状态。
    fn new(protocol: ProviderProtocol) -> Self {
        let events = expected_sse_events(protocol);
        Self {
            events,
            cardinalities: vec![SaturatedCardinality::Zero; events.len()],
            root_types: vec![JsonValueType::Missing; events.len()],
            unknown_event_present: false,
            missing_event_name_present: false,
            unknown_data_key_present: false,
            event_data_type_mismatch: false,
            started: false,
            terminal_count: SaturatedCardinality::Zero,
            chat_finish_reason_cardinality: SaturatedCardinality::Zero,
            chat_error_cardinality: SaturatedCardinality::Zero,
            chat_adapter_ended: false,
            done_sentinel_observed: false,
            empty_data_after_terminal_observed: false,
            lazy_start_observed: false,
            lazy_start_all_accepted: true,
            responses_provider_declared_error: false,
            response_tools: BTreeMap::new(),
            chat_tools: BTreeMap::new(),
            messages_open_blocks: BTreeMap::new(),
            violation_bits: SseContractViolationBits::empty(),
        }
    }

    /// 记录一个固定事件和其临时 JSON data 根类型。
    fn record_event(&mut self, event: KnownSseEvent, value: Option<&Value>) {
        let Some(index) = self.events.iter().position(|candidate| *candidate == event) else {
            self.unknown_event_present = true;
            return;
        };
        self.cardinalities[index].observe();
        if let Some(value) = value {
            self.root_types[index] = self.root_types[index].merge(JsonValueType::of(value));
        }
    }

    /// 记录一个语义终态，并在第二次观察时设置重复终态位。
    fn record_terminal(&mut self) {
        if self.terminal_count.is_present() {
            self.violation_bits.duplicate_terminal = true;
        }
        self.terminal_count.observe();
    }

    /// 记录 Chat 的固定终态依据并同步语义终态基数。
    fn record_chat_terminal(&mut self, evidence: ChatTerminalEvidence) {
        match evidence {
            ChatTerminalEvidence::FinishReason => self.chat_finish_reason_cardinality.observe(),
            ChatTerminalEvidence::Error => self.chat_error_cardinality.observe(),
            ChatTerminalEvidence::None | ChatTerminalEvidence::Both => {
                unreachable!("单帧只记录一种 Chat 终态依据")
            }
        }
        self.record_terminal();
    }

    /// 标记当前帧出现在语义终态之后。
    fn record_event_after_terminal(&mut self) {
        self.violation_bits.event_after_terminal = true;
    }

    /// 记录终态后不属于 `[DONE]` 的空 data 帧。
    fn record_empty_data_after_terminal(&mut self) {
        self.empty_data_after_terminal_observed = true;
        self.record_event_after_terminal();
    }

    /// 把瞬时状态转换为可持久化的固定结构证据。
    fn finish(mut self, trailing_partial_frame: bool) -> SseShapeEvidence {
        if !self.terminal_count.is_present() {
            self.violation_bits.terminal_missing = true;
        }
        self.violation_bits.unknown_event_present = match self.events.first() {
            Some(KnownSseEvent::ChatChunk) => false,
            _ => self.unknown_event_present,
        };
        self.violation_bits.event_data_type_mismatch = self.event_data_type_mismatch
            && matches!(self.events.first(), Some(KnownSseEvent::ResponsesCreated));
        let event_after_terminal = self.violation_bits.event_after_terminal;
        let chat_terminal_evidence = match (
            self.chat_finish_reason_cardinality.is_present(),
            self.chat_error_cardinality.is_present(),
        ) {
            (false, false) => ChatTerminalEvidence::None,
            (true, false) => ChatTerminalEvidence::FinishReason,
            (false, true) => ChatTerminalEvidence::Error,
            (true, true) => ChatTerminalEvidence::Both,
        };
        let known_event_cardinality = self
            .events
            .iter()
            .copied()
            .zip(self.cardinalities)
            .map(|(event, cardinality)| KnownSseEventCardinality { event, cardinality })
            .collect();
        let data_json_root_types = self
            .events
            .iter()
            .copied()
            .zip(self.root_types)
            .map(|(event, root_type)| KnownSseDataRootType { event, root_type })
            .collect();
        SseShapeEvidence {
            shape_schema: WireShapeSchema::V2,
            known_event_cardinality,
            unknown_event_present: self.unknown_event_present,
            missing_event_name_present: self.missing_event_name_present,
            unknown_data_key_present: self.unknown_data_key_present,
            data_json_root_types,
            event_data_type_mismatch: self.event_data_type_mismatch,
            terminal_observed: self.terminal_count.is_present(),
            terminal_cardinality: self.terminal_count,
            chat_terminal_evidence,
            chat_finish_reason_cardinality: self.chat_finish_reason_cardinality,
            chat_error_cardinality: self.chat_error_cardinality,
            done_sentinel_observed: self.done_sentinel_observed,
            lazy_start_observed: self.lazy_start_observed,
            lazy_start_accepted: self.lazy_start_observed && self.lazy_start_all_accepted,
            responses_provider_declared_error: self.responses_provider_declared_error,
            event_after_terminal,
            empty_data_after_terminal_observed: self.empty_data_after_terminal_observed,
            trailing_partial_frame,
            violation_bits: self.violation_bits,
        }
    }
}

/// Messages 在终态前必须关闭的固定内容块状态。
#[derive(Clone, Copy)]
enum MessagesOpenBlock {
    /// 尚未收到 content block stop 的工具调用。
    Tool,
    /// 含签名状态且尚未收到 content block stop 的推理块。
    ThinkingSignature,
}

/// 独立解析并检查 SSE；不调用 Provider 的 `SseDecoder` 或 Adapter。
fn inspect_sse_shape(protocol: ProviderProtocol, body: &[u8]) -> SseShapeEvidence {
    let text = std::str::from_utf8(body).unwrap_or_default();
    let (frames, trailing_partial_frame) = parse_sse_frames(text);
    let mut state = SseInspectionState::new(protocol);
    for frame in frames {
        state.missing_event_name_present |= frame.event.is_none();
        match protocol {
            ProviderProtocol::Messages => inspect_messages_sse_frame(&frame, &mut state),
            ProviderProtocol::ChatCompletions => inspect_chat_sse_frame(&frame, &mut state),
            ProviderProtocol::Responses => inspect_responses_sse_frame(&frame, &mut state),
        }
    }
    state.finish(trailing_partial_frame)
}

/// 按独立实现解析 UTF-8 SSE 字段和空行边界。
fn parse_sse_frames(text: &str) -> (Vec<ParsedSseFrame>, bool) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut frames = Vec::new();
    let mut event = None;
    let mut data = String::new();
    let mut has_data = false;
    let mut pending_frame_fields = false;
    for raw_line in text.split_inclusive('\n') {
        let had_newline = raw_line.ends_with('\n');
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() && had_newline {
            if event.is_some() || has_data {
                frames.push(ParsedSseFrame {
                    event: event.take(),
                    data: std::mem::take(&mut data),
                });
                has_data = false;
            }
            pending_frame_fields = false;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, field_value) = line.split_once(':').unwrap_or((line, ""));
        let field_value = field_value.strip_prefix(' ').unwrap_or(field_value);
        match field {
            "event" => {
                event = Some(field_value.to_owned());
                pending_frame_fields = true;
            }
            "data" => {
                if has_data {
                    data.push('\n');
                }
                data.push_str(field_value);
                has_data = true;
                pending_frame_fields = true;
            }
            "id" | "retry" => {}
            _ => {}
        }
    }
    if event.is_some() || has_data {
        frames.push(ParsedSseFrame { event, data });
    }
    (frames, pending_frame_fields)
}

/// 检查一条 Messages SSE 帧的固定事件、字段和顺序。
fn inspect_messages_sse_frame(frame: &ParsedSseFrame, state: &mut SseInspectionState) {
    if frame.data.trim() == "[DONE]" {
        state.done_sentinel_observed = true;
        state.violation_bits.invalid_data_json = true;
        return;
    }
    if frame.data.is_empty() && frame.event.as_deref() == Some("ping") {
        let event = KnownSseEvent::MessagesPing;
        if state.terminal_count.is_present() {
            state.record_event_after_terminal();
        }
        state.record_event(event, None);
        return;
    }
    let value = if frame.data.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<Value>(&frame.data) {
            Ok(value) => Some(value),
            Err(_) => {
                state.violation_bits.invalid_data_json = true;
                None
            }
        }
    };
    let data_type = value
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    if let (Some(frame_event), Some(data_type)) = (frame.event.as_deref(), data_type) {
        state.event_data_type_mismatch |= frame_event != data_type;
    }
    let explicit_error = value.as_ref().is_some_and(messages_explicit_error);
    let effective =
        frame
            .event
            .as_deref()
            .or(data_type)
            .or(if explicit_error { Some("error") } else { None });
    let Some(event) = effective.and_then(messages_known_event) else {
        if effective.is_some() {
            state.unknown_event_present = true;
        } else {
            state.violation_bits.missing_effective_event = true;
        }
        return;
    };
    if state.terminal_count.is_present() {
        state.record_event_after_terminal();
    }
    state.record_event(event, value.as_ref());
    let Some(value) = value.as_ref() else {
        if !matches!(event, KnownSseEvent::MessagesPing) {
            state.violation_bits.invalid_data_json = true;
        }
        return;
    };
    state.unknown_data_key_present |= messages_sse_unknown_key_present(event, value);
    if !value.is_object() {
        if matches!(event, KnownSseEvent::MessagesPing) {
            return;
        }
        if matches!(event, KnownSseEvent::MessagesMessageStop) {
            require_sse_started(state);
            if !state.messages_open_blocks.is_empty() {
                state.violation_bits.tool_sequence_violation = true;
            }
            state.record_terminal();
            return;
        }
        if matches!(event, KnownSseEvent::MessagesError) {
            state.record_terminal();
        }
        state.violation_bits.data_root_type_mismatch = true;
        return;
    }
    validate_messages_sse_event(event, value, state);
}

/// 把受支持的 Messages 事件名映射为固定枚举。
fn messages_known_event(name: &str) -> Option<KnownSseEvent> {
    match name {
        "message_start" => Some(KnownSseEvent::MessagesMessageStart),
        "content_block_start" => Some(KnownSseEvent::MessagesContentBlockStart),
        "content_block_delta" => Some(KnownSseEvent::MessagesContentBlockDelta),
        "content_block_stop" => Some(KnownSseEvent::MessagesContentBlockStop),
        "message_delta" => Some(KnownSseEvent::MessagesMessageDelta),
        "message_stop" => Some(KnownSseEvent::MessagesMessageStop),
        "ping" => Some(KnownSseEvent::MessagesPing),
        "error" => Some(KnownSseEvent::MessagesError),
        _ => None,
    }
}

/// 判断 Messages data 是否含 Adapter 识别的显式错误对象。
fn messages_explicit_error(value: &Value) -> bool {
    value.get("error").is_some_and(Value::is_object)
        || value.get("type").and_then(Value::as_str) == Some("error")
}

/// 校验一条已知 Messages SSE 事件的浅层固定契约。
fn validate_messages_sse_event(
    event: KnownSseEvent,
    value: &Value,
    state: &mut SseInspectionState,
) {
    let object = value.as_object().expect("调用方已校验对象根");
    match event {
        KnownSseEvent::MessagesMessageStart => {
            if state.started {
                state.violation_bits.duplicate_start = true;
            }
            if let Some(message) = sse_required_object(object, "message", &mut state.violation_bits)
            {
                sse_check_optional_non_blank_string(message, "id", &mut state.violation_bits);
                sse_check_optional_non_blank_string(message, "model", &mut state.violation_bits);
                state.started = true;
            }
        }
        KnownSseEvent::MessagesContentBlockStart => {
            require_sse_started(state);
            let index = sse_required_u64(object, "index", &mut state.violation_bits);
            if let Some(block) =
                sse_required_object(object, "content_block", &mut state.violation_bits)
            {
                if let Some(open) = validate_messages_stream_block(block, &mut state.violation_bits)
                {
                    if let Some(index) = index {
                        state.messages_open_blocks.insert(index, open);
                    }
                }
            }
        }
        KnownSseEvent::MessagesContentBlockDelta => {
            require_sse_started(state);
            let index = sse_required_u64(object, "index", &mut state.violation_bits);
            if let Some(delta) = sse_required_object(object, "delta", &mut state.violation_bits) {
                match validate_messages_stream_delta(delta, &mut state.violation_bits) {
                    Some(MessagesDeltaKind::InputJson) => {
                        if index.is_none_or(|index| {
                            !matches!(
                                state.messages_open_blocks.get(&index),
                                Some(MessagesOpenBlock::Tool)
                            )
                        }) {
                            state.violation_bits.tool_sequence_violation = true;
                        }
                    }
                    Some(MessagesDeltaKind::Signature) => {
                        if let Some(index) = index {
                            state
                                .messages_open_blocks
                                .insert(index, MessagesOpenBlock::ThinkingSignature);
                        }
                    }
                    Some(MessagesDeltaKind::Other) | None => {}
                }
            }
        }
        KnownSseEvent::MessagesContentBlockStop => {
            require_sse_started(state);
            if let Some(index) = sse_required_u64(object, "index", &mut state.violation_bits) {
                state.messages_open_blocks.remove(&index);
            }
        }
        KnownSseEvent::MessagesMessageDelta => {
            require_sse_started(state);
            sse_optional_type(
                object,
                "delta",
                &[JsonValueType::Object],
                true,
                &mut state.violation_bits,
            );
            sse_optional_type(
                object,
                "usage",
                &[JsonValueType::Object],
                true,
                &mut state.violation_bits,
            );
        }
        KnownSseEvent::MessagesMessageStop => {
            require_sse_started(state);
            if !state.messages_open_blocks.is_empty() {
                state.violation_bits.tool_sequence_violation = true;
            }
            state.record_terminal();
        }
        KnownSseEvent::MessagesPing => {}
        KnownSseEvent::MessagesError => state.record_terminal(),
        _ => unreachable!("Messages 只会传入 Messages 固定事件"),
    }
}

/// 校验 Messages `content_block_start` 的固定内容类型并返回需关闭状态。
fn validate_messages_stream_block(
    block: &Map<String, Value>,
    bits: &mut SseContractViolationBits,
) -> Option<MessagesOpenBlock> {
    let block_type = sse_required_string(block, "type", bits)?;
    match block_type {
        "text" => {
            sse_optional_type(block, "text", &[JsonValueType::String], true, bits);
            None
        }
        "thinking" => {
            for field in ["thinking", "signature"] {
                sse_optional_type(block, field, &[JsonValueType::String], true, bits);
            }
            block
                .get("signature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
                .map(|_| MessagesOpenBlock::ThinkingSignature)
        }
        "redacted_thinking" => {
            if !block.contains_key("data") {
                bits.required_field_missing = true;
            }
            None
        }
        "tool_use" => {
            sse_required_string(block, "id", bits);
            sse_required_string(block, "name", bits);
            sse_required_object(block, "input", bits);
            Some(MessagesOpenBlock::Tool)
        }
        _ => {
            bits.unknown_discriminator_present = true;
            None
        }
    }
}

/// Messages 增量对开放状态的固定影响类别。
#[derive(Clone, Copy)]
enum MessagesDeltaKind {
    /// 普通文本或推理文本，不改变开放状态。
    Other,
    /// 工具 JSON 参数增量，要求既有工具状态。
    InputJson,
    /// 推理签名增量，会创建待关闭签名状态。
    Signature,
}

/// 校验 Messages `content_block_delta` 的固定增量类型。
fn validate_messages_stream_delta(
    delta: &Map<String, Value>,
    bits: &mut SseContractViolationBits,
) -> Option<MessagesDeltaKind> {
    let delta_type = sse_required_string(delta, "type", bits)?;
    let (field, kind) = match delta_type {
        "text_delta" => ("text", MessagesDeltaKind::Other),
        "thinking_delta" => ("thinking", MessagesDeltaKind::Other),
        "signature_delta" => ("signature", MessagesDeltaKind::Signature),
        "input_json_delta" => ("partial_json", MessagesDeltaKind::InputJson),
        _ => {
            bits.unknown_discriminator_present = true;
            return None;
        }
    };
    sse_required_string(delta, field, bits);
    Some(kind)
}

/// 检查一条 Chat Completions SSE 帧的固定 chunk、终态和 `[DONE]` 顺序。
fn inspect_chat_sse_frame(frame: &ParsedSseFrame, state: &mut SseInspectionState) {
    if frame
        .event
        .as_deref()
        .is_some_and(|event| event != "message")
    {
        state.unknown_event_present = true;
    }
    let data = frame.data.trim();
    if data.is_empty() {
        if state.chat_adapter_ended {
            state.record_empty_data_after_terminal();
        }
        return;
    }
    if data == "[DONE]" {
        state.done_sentinel_observed = true;
        if !state.terminal_count.is_present() {
            state.violation_bits.done_before_terminal = true;
        } else if state.chat_finish_reason_cardinality.is_present() {
            state.chat_adapter_ended = true;
        }
        return;
    }
    let value = match serde_json::from_str::<Value>(&frame.data) {
        Ok(value) => value,
        Err(_) => {
            state.violation_bits.invalid_data_json = true;
            state.record_event(KnownSseEvent::ChatChunk, None);
            return;
        }
    };
    state.record_event(KnownSseEvent::ChatChunk, Some(&value));
    state.unknown_data_key_present |= chat_sse_unknown_key_present(&value);
    let Some(root) = value.as_object() else {
        state.violation_bits.data_root_type_mismatch = true;
        return;
    };
    if root.contains_key("error") {
        if state.terminal_count.is_present() {
            state.record_event_after_terminal();
        }
        state.record_chat_terminal(ChatTerminalEvidence::Error);
        return;
    }
    if !state.started {
        sse_check_optional_non_blank_string(root, "id", &mut state.violation_bits);
        sse_check_optional_non_blank_string(root, "model", &mut state.violation_bits);
        state.started = true;
    }
    let Some(choices) = sse_required_array(root, "choices", &mut state.violation_bits) else {
        return;
    };
    if state.terminal_count.is_present() {
        let usage_present = root.get("usage").is_some_and(|usage| !usage.is_null());
        let inert_usage_tail =
            choices.is_empty() || usage_present && is_chat_inert_usage_choice(choices);
        if !inert_usage_tail {
            state.record_event_after_terminal();
        }
        return;
    }
    sse_optional_type(
        root,
        "usage",
        &[JsonValueType::Object],
        true,
        &mut state.violation_bits,
    );
    if choices.is_empty() {
        return;
    }
    if choices.len() != 1 {
        state.violation_bits.known_field_type_mismatch = true;
        return;
    }
    let Some(choice) = choices[0].as_object() else {
        state.violation_bits.known_field_type_mismatch = true;
        return;
    };
    if choice
        .get("index")
        .and_then(Value::as_u64)
        .is_some_and(|index| index != 0)
    {
        state.violation_bits.known_field_type_mismatch = true;
    }
    if let Some(delta) = choice.get("delta") {
        if let Some(delta) = delta.as_object() {
            let mut json_bits = JsonContractViolationBits::empty();
            check_chat_message_contract(delta, true, &mut json_bits);
            merge_json_bits_into_sse(&json_bits, &mut state.violation_bits);
            observe_chat_tool_state(delta, state);
        } else if !delta.is_null() {
            state.violation_bits.known_field_type_mismatch = true;
        }
    }
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some()
    {
        if state.chat_tools.values().any(|(id, name)| !id || !name) {
            state.violation_bits.tool_sequence_violation = true;
        }
        state.record_chat_terminal(ChatTerminalEvidence::FinishReason);
    } else if choice
        .get("finish_reason")
        .is_some_and(|reason| !reason.is_null())
    {
        state.violation_bits.known_field_type_mismatch = true;
    }
}

/// 只以布尔状态跟踪 Chat 工具 ID 与名称是否齐全，不保存二者原文。
fn observe_chat_tool_state(delta: &Map<String, Value>, state: &mut SseInspectionState) {
    let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    for tool_call in tool_calls.iter().filter_map(Value::as_object) {
        let Some(index) = tool_call.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let entry = state.chat_tools.entry(index).or_insert((false, false));
        if tool_call
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
        {
            entry.0 = true;
        }
        let function = tool_call.get("function").and_then(Value::as_object);
        if function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
        {
            entry.1 = true;
        }
        let arguments_present = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .is_some_and(|arguments| !arguments.is_empty());
        if arguments_present && (!entry.0 || !entry.1) {
            state.violation_bits.tool_sequence_violation = true;
        }
    }
}

/// 判断 Chat finish_reason 后的 usage 尾块是否为 Adapter 接受的惰性 choice。
fn is_chat_inert_usage_choice(choices: &[Value]) -> bool {
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
            .is_some_and(Map::is_empty)
        && choice.get("finish_reason").is_none_or(Value::is_null)
        && choice.get("logprobs").is_none_or(Value::is_null)
}

/// 检查一条 Responses SSE 帧的固定事件、惰性开始、工具和终态顺序。
fn inspect_responses_sse_frame(frame: &ParsedSseFrame, state: &mut SseInspectionState) {
    let data = frame.data.trim();
    if data.is_empty() {
        if state.terminal_count.is_present() {
            state.record_empty_data_after_terminal();
        }
        return;
    }
    if data == "[DONE]" {
        state.done_sentinel_observed = true;
        return;
    }
    // Responses Adapter 在语义终态后会拒绝任意非空且非 `[DONE]` 的后续帧，
    // 因此必须在 JSON 解码和事件判别前留证，覆盖无效 JSON、缺 type 与未知 type。
    if state.terminal_count.is_present() {
        state.record_event_after_terminal();
    }
    let value = match serde_json::from_str::<Value>(&frame.data) {
        Ok(value) => value,
        Err(_) => {
            state.violation_bits.invalid_data_json = true;
            if let Some(event) = frame.event.as_deref().and_then(responses_known_event) {
                state.record_event(event, None);
            } else if frame.event.is_none() {
                state.violation_bits.missing_effective_event = true;
            } else {
                state.unknown_event_present = true;
            }
            return;
        }
    };
    let data_type = value.get("type").and_then(Value::as_str);
    if let (Some(frame_event), Some(data_type)) = (frame.event.as_deref(), data_type) {
        if frame_event != data_type {
            state.event_data_type_mismatch = true;
        }
    }
    let explicit_error = responses_explicit_error(&value);
    state.responses_provider_declared_error |= explicit_error;
    let effective =
        frame
            .event
            .as_deref()
            .or(data_type)
            .or(if explicit_error { Some("error") } else { None });
    let Some(event) = effective.and_then(responses_known_event) else {
        if effective.is_some() {
            state.unknown_event_present = true;
        } else {
            state.violation_bits.missing_effective_event = true;
        }
        return;
    };
    state.record_event(event, Some(&value));
    state.unknown_data_key_present |= responses_sse_unknown_key_present(event, &value);
    let Some(object) = value.as_object() else {
        if matches!(
            event,
            KnownSseEvent::ResponsesFailed | KnownSseEvent::ResponsesError
        ) {
            state.record_terminal();
        }
        state.violation_bits.data_root_type_mismatch = true;
        return;
    };
    maybe_start_responses_lazily(event, object, state);
    validate_responses_sse_event(event, object, state);
}

/// 把受支持的 Responses 事件名映射为固定枚举。
fn responses_known_event(name: &str) -> Option<KnownSseEvent> {
    match name {
        "response.created" => Some(KnownSseEvent::ResponsesCreated),
        "response.queued" => Some(KnownSseEvent::ResponsesQueued),
        "response.in_progress" => Some(KnownSseEvent::ResponsesInProgress),
        "response.output_item.added" => Some(KnownSseEvent::ResponsesOutputItemAdded),
        "response.output_item.done" => Some(KnownSseEvent::ResponsesOutputItemDone),
        "response.content_part.added" => Some(KnownSseEvent::ResponsesContentPartAdded),
        "response.content_part.done" => Some(KnownSseEvent::ResponsesContentPartDone),
        "response.output_text.delta" => Some(KnownSseEvent::ResponsesOutputTextDelta),
        "response.output_text.done" => Some(KnownSseEvent::ResponsesOutputTextDone),
        "response.refusal.delta" => Some(KnownSseEvent::ResponsesRefusalDelta),
        "response.refusal.done" => Some(KnownSseEvent::ResponsesRefusalDone),
        "response.reasoning_summary_part.added" => {
            Some(KnownSseEvent::ResponsesReasoningSummaryPartAdded)
        }
        "response.reasoning_summary_part.done" => {
            Some(KnownSseEvent::ResponsesReasoningSummaryPartDone)
        }
        "response.reasoning_summary_text.delta" => {
            Some(KnownSseEvent::ResponsesReasoningSummaryTextDelta)
        }
        "response.reasoning_summary_text.done" => {
            Some(KnownSseEvent::ResponsesReasoningSummaryTextDone)
        }
        "response.reasoning_text.delta" => Some(KnownSseEvent::ResponsesReasoningTextDelta),
        "response.reasoning_text.done" => Some(KnownSseEvent::ResponsesReasoningTextDone),
        "response.function_call_arguments.delta" => {
            Some(KnownSseEvent::ResponsesFunctionArgumentsDelta)
        }
        "response.function_call_arguments.done" => {
            Some(KnownSseEvent::ResponsesFunctionArgumentsDone)
        }
        "response.completed" => Some(KnownSseEvent::ResponsesCompleted),
        "response.incomplete" => Some(KnownSseEvent::ResponsesIncomplete),
        "response.cancelled" => Some(KnownSseEvent::ResponsesCancelled),
        "response.failed" => Some(KnownSseEvent::ResponsesFailed),
        "error" => Some(KnownSseEvent::ResponsesError),
        _ => None,
    }
}

/// 判断 Responses data 是否含 Adapter 识别的明确失败事实。
fn responses_explicit_error(value: &Value) -> bool {
    let response = value.get("response");
    value.get("error").is_some_and(|error| !error.is_null())
        || response
            .and_then(|response| response.get("error"))
            .is_some_and(|error| !error.is_null())
        || value.get("status").and_then(Value::as_str) == Some("failed")
        || response
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            == Some("failed")
}

/// 在 `response.created` 被兼容网关省略时检查固定惰性起始条件。
fn maybe_start_responses_lazily(
    event: KnownSseEvent,
    object: &Map<String, Value>,
    state: &mut SseInspectionState,
) {
    if state.started || !responses_lazy_start_event(event) {
        return;
    }
    state.lazy_start_observed = true;
    let accepted = object
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| !model.trim().is_empty());
    state.lazy_start_all_accepted &= accepted;
    if accepted {
        state.started = true;
    } else {
        state.violation_bits.start_sequence_violation = true;
    }
}

/// 返回固定白名单中允许触发 Responses 惰性开始的事件。
fn responses_lazy_start_event(event: KnownSseEvent) -> bool {
    matches!(
        event,
        KnownSseEvent::ResponsesOutputItemAdded
            | KnownSseEvent::ResponsesContentPartAdded
            | KnownSseEvent::ResponsesOutputTextDelta
            | KnownSseEvent::ResponsesRefusalDelta
            | KnownSseEvent::ResponsesReasoningSummaryTextDelta
            | KnownSseEvent::ResponsesReasoningTextDelta
    )
}

/// 校验一条已知 Responses SSE 事件的浅层固定契约。
fn validate_responses_sse_event(
    event: KnownSseEvent,
    object: &Map<String, Value>,
    state: &mut SseInspectionState,
) {
    match event {
        KnownSseEvent::ResponsesCreated => {
            if state.started {
                state.violation_bits.duplicate_start = true;
            }
            if let Some(response) =
                sse_required_object(object, "response", &mut state.violation_bits)
            {
                sse_check_optional_non_blank_string(response, "id", &mut state.violation_bits);
                sse_check_optional_non_blank_string(response, "model", &mut state.violation_bits);
                state.started = true;
            }
        }
        KnownSseEvent::ResponsesQueued | KnownSseEvent::ResponsesInProgress => {}
        KnownSseEvent::ResponsesOutputItemAdded => {
            require_sse_started(state);
            validate_responses_output_item_event(object, false, state);
        }
        KnownSseEvent::ResponsesOutputItemDone => {
            require_sse_started(state);
            validate_responses_output_item_event(object, true, state);
        }
        KnownSseEvent::ResponsesContentPartAdded => {
            require_sse_started(state);
            sse_required_u64(object, "output_index", &mut state.violation_bits);
            if let Some(part) = sse_required_object(object, "part", &mut state.violation_bits) {
                validate_responses_stream_part(part, &mut state.violation_bits);
            }
        }
        KnownSseEvent::ResponsesOutputTextDelta
        | KnownSseEvent::ResponsesRefusalDelta
        | KnownSseEvent::ResponsesReasoningSummaryTextDelta
        | KnownSseEvent::ResponsesReasoningTextDelta => {
            require_sse_started(state);
            sse_required_u64(object, "output_index", &mut state.violation_bits);
            sse_required_string(object, "delta", &mut state.violation_bits);
        }
        KnownSseEvent::ResponsesFunctionArgumentsDelta
        | KnownSseEvent::ResponsesFunctionArgumentsDone => {
            require_sse_started(state);
            let index = sse_required_u64(object, "output_index", &mut state.violation_bits);
            if matches!(event, KnownSseEvent::ResponsesFunctionArgumentsDelta) {
                sse_required_string(object, "delta", &mut state.violation_bits);
            } else {
                sse_optional_type(
                    object,
                    "arguments",
                    &[JsonValueType::String],
                    true,
                    &mut state.violation_bits,
                );
            }
            match index.and_then(|index| state.response_tools.get(&index).copied()) {
                None => state.violation_bits.tool_sequence_violation = true,
                Some(true) if matches!(event, KnownSseEvent::ResponsesFunctionArgumentsDone) => {
                    state.violation_bits.tool_sequence_violation = true;
                }
                Some(_) => {}
            }
        }
        KnownSseEvent::ResponsesContentPartDone
        | KnownSseEvent::ResponsesOutputTextDone
        | KnownSseEvent::ResponsesRefusalDone
        | KnownSseEvent::ResponsesReasoningSummaryPartAdded
        | KnownSseEvent::ResponsesReasoningSummaryPartDone
        | KnownSseEvent::ResponsesReasoningSummaryTextDone
        | KnownSseEvent::ResponsesReasoningTextDone => require_sse_started(state),
        KnownSseEvent::ResponsesCompleted
        | KnownSseEvent::ResponsesIncomplete
        | KnownSseEvent::ResponsesCancelled => {
            require_sse_started(state);
            sse_required_object(object, "response", &mut state.violation_bits);
            if state.response_tools.values().any(|ended| !ended) {
                state.violation_bits.tool_sequence_violation = true;
            }
            state.record_terminal();
        }
        KnownSseEvent::ResponsesFailed | KnownSseEvent::ResponsesError => {
            state.record_terminal();
        }
        _ => unreachable!("Responses 只会传入 Responses 固定事件"),
    }
}

/// 校验 Responses output item added/done 的固定 item 形态和工具状态。
fn validate_responses_output_item_event(
    object: &Map<String, Value>,
    done: bool,
    state: &mut SseInspectionState,
) {
    let index = sse_required_u64(object, "output_index", &mut state.violation_bits);
    let Some(item) = sse_required_object(object, "item", &mut state.violation_bits) else {
        return;
    };
    let Some(item_type) = sse_required_string(item, "type", &mut state.violation_bits) else {
        return;
    };
    match item_type {
        "message" | "reasoning" => {}
        "function_call" => {
            let already_started =
                index.is_some_and(|index| state.response_tools.contains_key(&index));
            if !already_started {
                let has_call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .is_some();
                if !has_call_id {
                    if item.contains_key("call_id") || item.contains_key("id") {
                        state.violation_bits.known_field_type_mismatch = true;
                    } else {
                        state.violation_bits.required_field_missing = true;
                    }
                }
                sse_required_string(item, "name", &mut state.violation_bits);
            }
            if let Some(index) = index {
                let already_ended = state.response_tools.get(&index).copied() == Some(true);
                if done && already_ended {
                    state.violation_bits.tool_sequence_violation = true;
                }
                state.response_tools.entry(index).or_insert(false);
                if done {
                    state.response_tools.insert(index, true);
                }
            }
        }
        _ => state.violation_bits.unknown_discriminator_present = true,
    }
}

/// 校验 Responses `content_part.added` 的固定 part 类型。
fn validate_responses_stream_part(part: &Map<String, Value>, bits: &mut SseContractViolationBits) {
    let Some(part_type) = sse_required_string(part, "type", bits) else {
        return;
    };
    match part_type {
        "output_text" | "reasoning_text" => {
            sse_optional_type(part, "text", &[JsonValueType::String], true, bits)
        }
        "refusal" => sse_optional_type(part, "refusal", &[JsonValueType::String], true, bits),
        _ => bits.unknown_discriminator_present = true,
    }
}

/// 要求 SSE 语义已经开始，否则设置固定顺序违例位。
fn require_sse_started(state: &mut SseInspectionState) {
    if !state.started {
        state.violation_bits.start_sequence_violation = true;
    }
}

/// 检查 Messages SSE data 的固定对象白名单。
fn messages_sse_unknown_key_present(event: KnownSseEvent, value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = match event {
        KnownSseEvent::MessagesMessageStart => has_unknown_key(root, &["type", "message"]),
        KnownSseEvent::MessagesContentBlockStart => {
            has_unknown_key(root, &["type", "index", "content_block"])
        }
        KnownSseEvent::MessagesContentBlockDelta => {
            has_unknown_key(root, &["type", "index", "delta"])
        }
        KnownSseEvent::MessagesContentBlockStop => has_unknown_key(root, &["type", "index"]),
        KnownSseEvent::MessagesMessageDelta => has_unknown_key(root, &["type", "delta", "usage"]),
        KnownSseEvent::MessagesMessageStop | KnownSseEvent::MessagesPing => {
            has_unknown_key(root, &["type"])
        }
        KnownSseEvent::MessagesError => has_unknown_key(root, &["type", "error"]),
        _ => false,
    };
    if let Some(message) = root.get("message").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            message,
            &["id", "model", "usage", "type", "role", "content"],
        );
    }
    if let Some(block) = root.get("content_block").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            block,
            &[
                "type",
                "text",
                "thinking",
                "signature",
                "data",
                "id",
                "name",
                "input",
            ],
        );
    }
    if let Some(delta) = root.get("delta").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            delta,
            &[
                "type",
                "text",
                "thinking",
                "signature",
                "partial_json",
                "stop_reason",
                "stop_sequence",
            ],
        );
    }
    unknown
}

/// 检查 Chat Completions SSE data 的固定对象白名单。
fn chat_sse_unknown_key_present(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = has_unknown_key(root, &["id", "model", "choices", "usage", "error"]);
    if let Some(choices) = root.get("choices").and_then(Value::as_array) {
        for choice in choices.iter().filter_map(Value::as_object) {
            unknown |= has_unknown_key(choice, &["index", "delta", "finish_reason", "logprobs"]);
            if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
                unknown |= chat_message_unknown_key_present(delta, true);
            }
        }
    }
    unknown
}

/// 检查 Responses SSE data 的固定对象白名单。
fn responses_sse_unknown_key_present(event: KnownSseEvent, value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    let mut unknown = has_unknown_key(
        root,
        &[
            "type",
            "sequence_number",
            "response",
            "output_index",
            "content_index",
            "item_id",
            "item",
            "part",
            "delta",
            "arguments",
            "model",
            "error",
            "code",
            "message",
            "param",
        ],
    );
    if let Some(response) = root.get("response").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            response,
            &[
                "id",
                "model",
                "status",
                "usage",
                "error",
                "incomplete_details",
                "output",
            ],
        );
    }
    if let Some(item) = root.get("item").and_then(Value::as_object) {
        unknown |= has_unknown_key(
            item,
            &[
                "type",
                "id",
                "call_id",
                "name",
                "arguments",
                "content",
                "summary",
                "encrypted_content",
                "role",
                "status",
            ],
        );
    }
    if let Some(part) = root.get("part").and_then(Value::as_object) {
        unknown |= has_unknown_key(part, &["type", "text", "refusal"]);
    }
    if matches!(event, KnownSseEvent::ResponsesError) {
        if let Some(error) = root.get("error").and_then(Value::as_object) {
            unknown |= has_unknown_key(error, &["type", "code", "message", "param"]);
        }
    }
    unknown
}

/// 读取 SSE 事件的必需对象字段，并只设置固定违例位。
fn sse_required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut SseContractViolationBits,
) -> Option<&'a Map<String, Value>> {
    match object.get(field) {
        Some(Value::Object(value)) => Some(value),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 读取 SSE 事件的必需数组字段，并只设置固定违例位。
fn sse_required_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut SseContractViolationBits,
) -> Option<&'a [Value]> {
    match object.get(field) {
        Some(Value::Array(values)) => Some(values),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 读取 SSE 事件的必需字符串字段且不复制字段值。
fn sse_required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    bits: &mut SseContractViolationBits,
) -> Option<&'a str> {
    match object.get(field) {
        Some(Value::String(value)) => Some(value),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 读取 SSE 事件的必需 u32 范围非负整数且只临时返回数值。
fn sse_required_u64(
    object: &Map<String, Value>,
    field: &str,
    bits: &mut SseContractViolationBits,
) -> Option<u64> {
    match object.get(field).and_then(Value::as_u64) {
        Some(value) if u32::try_from(value).is_ok() => Some(value),
        Some(_) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None if object.contains_key(field) => {
            bits.known_field_type_mismatch = true;
            None
        }
        None => {
            bits.required_field_missing = true;
            None
        }
    }
}

/// 校验 SSE 事件的可选字段是否属于固定类型集合。
fn sse_optional_type(
    object: &Map<String, Value>,
    field: &str,
    accepted: &[JsonValueType],
    null_allowed: bool,
    bits: &mut SseContractViolationBits,
) {
    let Some(value) = object.get(field) else {
        return;
    };
    if value.is_null() && null_allowed {
        return;
    }
    if !accepted.contains(&JsonValueType::of(value)) {
        bits.known_field_type_mismatch = true;
    }
}

/// 把 Chat 共享字段检查结果压缩为 SSE 的对应固定违例位。
fn merge_json_bits_into_sse(
    source: &JsonContractViolationBits,
    target: &mut SseContractViolationBits,
) {
    target.required_field_missing |= source.required_field_missing;
    target.known_field_type_mismatch |= source.known_field_type_mismatch
        || source.known_nested_shape_mismatch
        || source.expected_singleton_mismatch;
    target.unknown_discriminator_present |= source.unknown_discriminator_present;
}

impl WireResponseShapeEvidence {
    /// 严格校验持久化结构内部不变量，错误文本不拼接任何输入值。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.http_status.is_none()
            && (!matches!(self.declared_content_type, DeclaredContentType::Missing)
                || !matches!(self.body_format, WireBodyFormat::Empty)
                || self.body_eof_observed
                || self.capture_truncated)
        {
            return Err("无响应头的 Wire 证据不得携带响应正文事实".to_owned());
        }
        if self
            .http_status
            .is_some_and(|status| !(100..=999).contains(&status))
        {
            return Err("Wire 结构证据包含非法 HTTP 状态".to_owned());
        }
        if matches!(self.body_format, WireBodyFormat::Empty) && self.capture_truncated {
            return Err("空正文 Wire 证据不得标记捕获截断".to_owned());
        }
        match self.body_format {
            WireBodyFormat::Json => {
                let Some(shape) = &self.json_shape else {
                    return Err("Wire 结构证据缺少 JSON shape".to_owned());
                };
                if self.sse_shape.is_some() {
                    return Err("Wire 结构证据同时包含 JSON 与 SSE shape".to_owned());
                }
                validate_json_shape_layout(self.protocol, shape)?;
            }
            WireBodyFormat::Sse => {
                let Some(shape) = &self.sse_shape else {
                    return Err("Wire 结构证据缺少 SSE shape".to_owned());
                };
                if self.json_shape.is_some() {
                    return Err("Wire 结构证据同时包含 JSON 与 SSE shape".to_owned());
                }
                validate_sse_shape_layout(self.protocol, shape)?;
            }
            WireBodyFormat::Empty | WireBodyFormat::InvalidUtf8 | WireBodyFormat::Unknown => {
                if self.json_shape.is_some() || self.sse_shape.is_some() {
                    return Err("Wire 非结构正文不得包含 JSON 或 SSE shape".to_owned());
                }
            }
        }
        Ok(())
    }

    /// 对一次 decode 失败做非持久化四类责任边界判断。
    // 该方法只供测试和人工/离线分析，生产路径刻意不自动持久化责任归因。
    #[allow(dead_code)]
    pub(crate) fn classify_decode_failure(&self) -> DecodeFailureAttribution {
        if self.validate().is_err()
            || !self
                .http_status
                .is_some_and(|status| (200..300).contains(&status))
            || !self.body_eof_observed
            || self.capture_truncated
        {
            return DecodeFailureAttribution::Indeterminate;
        }
        let contract_violated = match self.body_format {
            WireBodyFormat::Json => {
                matches!(self.declared_content_type, DeclaredContentType::Sse)
                    || self
                        .json_shape
                        .as_ref()
                        .is_none_or(|shape| shape.violation_bits.any())
            }
            WireBodyFormat::Sse => self
                .sse_shape
                .as_ref()
                .is_none_or(|shape| shape.violation_bits.any()),
            WireBodyFormat::Empty | WireBodyFormat::InvalidUtf8 | WireBodyFormat::Unknown => true,
        };
        if contract_violated {
            DecodeFailureAttribution::Nonconformant
        } else if self.provider_declared_error_observed() {
            DecodeFailureAttribution::ProviderDeclaredError
        } else {
            DecodeFailureAttribution::AdapterSuspect
        }
    }

    /// 返回固定结构证据是否包含协议明确声明的供应商错误。
    fn provider_declared_error_observed(&self) -> bool {
        match self.body_format {
            WireBodyFormat::Json => self
                .json_shape
                .as_ref()
                .is_some_and(|shape| shape.provider_declared_error),
            WireBodyFormat::Sse => {
                self.sse_shape
                    .as_ref()
                    .is_some_and(|shape| match self.protocol {
                        ProviderProtocol::Messages => {
                            fixed_event_cardinality(shape, KnownSseEvent::MessagesError)
                                .is_present()
                        }
                        ProviderProtocol::ChatCompletions => {
                            shape.chat_error_cardinality.is_present()
                        }
                        ProviderProtocol::Responses => {
                            shape.responses_provider_declared_error
                                || fixed_event_cardinality(shape, KnownSseEvent::ResponsesFailed)
                                    .is_present()
                                || fixed_event_cardinality(shape, KnownSseEvent::ResponsesError)
                                    .is_present()
                        }
                    })
            }
            WireBodyFormat::Empty | WireBodyFormat::InvalidUtf8 | WireBodyFormat::Unknown => false,
        }
    }
}

/// 校验 JSON shape 的版本、根类型和固定位置数组顺序。
fn validate_json_shape_layout(
    protocol: ProviderProtocol,
    shape: &JsonShapeEvidence,
) -> Result<(), String> {
    if shape.shape_schema != WireShapeSchema::V2 {
        return Err("JSON shape schema 不受支持".to_owned());
    }
    let expected_fields = expected_json_fields(protocol);
    if shape.known_field_types.len() != expected_fields.len()
        || !shape
            .known_field_types
            .iter()
            .zip(expected_fields)
            .all(|(actual, expected)| actual.path == *expected)
    {
        return Err("JSON 固定字段位置数组不完整或顺序错误".to_owned());
    }
    let expected_nested = expected_json_nested(protocol);
    if shape.known_nested_path_shapes.len() != expected_nested.len()
        || !shape
            .known_nested_path_shapes
            .iter()
            .zip(expected_nested)
            .all(|(actual, expected)| actual.path == *expected)
    {
        return Err("JSON 固定嵌套位置数组不完整或顺序错误".to_owned());
    }
    if shape.known_nested_path_shapes.iter().any(|nested| {
        matches!(nested.cardinality, SaturatedCardinality::Zero)
            != matches!(nested.element_type, JsonValueType::Missing)
    }) {
        return Err("JSON 嵌套基数与元素类型不一致".to_owned());
    }
    if shape.violation_bits.root_type_mismatch != !matches!(shape.root_type, JsonValueType::Object)
    {
        return Err("JSON 根类型与违例位不一致".to_owned());
    }
    Ok(())
}

/// 校验 SSE shape 的版本、固定事件数组和布尔位内部一致性。
fn validate_sse_shape_layout(
    protocol: ProviderProtocol,
    shape: &SseShapeEvidence,
) -> Result<(), String> {
    if shape.shape_schema != WireShapeSchema::V2 {
        return Err("SSE shape schema 不受支持".to_owned());
    }
    if protocol != ProviderProtocol::Responses && shape.responses_provider_declared_error {
        return Err("非 Responses SSE 不得携带 Responses 供应商错误事实".to_owned());
    }
    let expected_events = expected_sse_events(protocol);
    if shape.known_event_cardinality.len() != expected_events.len()
        || !shape
            .known_event_cardinality
            .iter()
            .zip(expected_events)
            .all(|(actual, expected)| actual.event == *expected)
    {
        return Err("SSE 固定事件数组不完整或顺序错误".to_owned());
    }
    if shape.data_json_root_types.len() != expected_events.len()
        || !shape
            .data_json_root_types
            .iter()
            .zip(expected_events)
            .all(|(actual, expected)| actual.event == *expected)
    {
        return Err("SSE data 根类型数组不完整或顺序错误".to_owned());
    }
    if shape
        .known_event_cardinality
        .iter()
        .zip(&shape.data_json_root_types)
        .any(|(event, root)| {
            matches!(event.cardinality, SaturatedCardinality::Zero)
                && !matches!(root.root_type, JsonValueType::Missing)
        })
    {
        return Err("SSE 零事件基数不得携带 data 根类型".to_owned());
    }
    if shape.terminal_observed != shape.terminal_cardinality.is_present()
        || shape.violation_bits.terminal_missing
            != matches!(shape.terminal_cardinality, SaturatedCardinality::Zero)
        || shape.violation_bits.duplicate_terminal
            != matches!(shape.terminal_cardinality, SaturatedCardinality::Many)
    {
        return Err("SSE 终态基数与终态布尔位不一致".to_owned());
    }
    if shape.event_after_terminal != shape.violation_bits.event_after_terminal {
        return Err("SSE 终态后事件字段与违例位不一致".to_owned());
    }
    if shape.empty_data_after_terminal_observed {
        if !shape.event_after_terminal || protocol == ProviderProtocol::Messages {
            return Err("SSE 终态后空 data 事实与协议或终态后事件位不一致".to_owned());
        }
        if protocol == ProviderProtocol::ChatCompletions
            && (!shape.done_sentinel_observed || !shape.chat_finish_reason_cardinality.is_present())
        {
            return Err("Chat 终态后空 data 事实缺少 finish_reason 与 DONE 依据".to_owned());
        }
    }
    if (shape.violation_bits.duplicate_terminal || shape.event_after_terminal)
        && !shape.terminal_observed
    {
        return Err("SSE 终态相关违例缺少终态观察依据".to_owned());
    }
    if shape.terminal_observed && !has_semantic_terminal_event(protocol, shape) {
        return Err("SSE 终态观察缺少固定事件依据".to_owned());
    }
    if shape.event_after_terminal
        && !matches!(
            combined_known_event_cardinality(shape),
            SaturatedCardinality::Many
        )
        && !shape.empty_data_after_terminal_observed
        && !shape.violation_bits.invalid_data_json
        && !shape.violation_bits.unknown_event_present
        && !shape.violation_bits.missing_effective_event
    {
        return Err("SSE 终态后事件缺少固定事件、空 data 或不可判别帧依据".to_owned());
    }
    if shape.violation_bits.duplicate_start && !has_duplicate_start_evidence(protocol, shape) {
        return Err("SSE 重复开始缺少固定事件依据".to_owned());
    }
    if protocol != ProviderProtocol::ChatCompletions
        && matches!(shape.terminal_cardinality, SaturatedCardinality::Many)
        && !matches!(
            fixed_terminal_event_cardinality(protocol, shape),
            SaturatedCardinality::Many
        )
    {
        return Err("SSE 重复终态缺少多个固定终态事件依据".to_owned());
    }
    if shape.violation_bits.tool_sequence_violation && !has_tool_sequence_event(protocol, shape) {
        return Err("SSE 工具顺序违例缺少固定事件依据".to_owned());
    }
    if (shape.violation_bits.data_root_type_mismatch
        || shape.violation_bits.required_field_missing
        || shape.violation_bits.known_field_type_mismatch
        || shape.violation_bits.unknown_discriminator_present
        || shape.violation_bits.start_sequence_violation)
        && !shape
            .known_event_cardinality
            .iter()
            .any(|entry| entry.cardinality.is_present())
    {
        return Err("SSE 结构或顺序违例缺少固定事件依据".to_owned());
    }
    if shape.lazy_start_accepted && !shape.lazy_start_observed {
        return Err("SSE 惰性起始接受位缺少观察依据".to_owned());
    }
    if protocol != ProviderProtocol::Responses
        && (shape.lazy_start_observed || shape.lazy_start_accepted)
    {
        return Err("非 Responses SSE 不得包含惰性起始状态".to_owned());
    }
    validate_chat_terminal_evidence(protocol, shape)?;
    if protocol == ProviderProtocol::Responses {
        if shape.lazy_start_observed && !has_lazy_start_event(shape) {
            return Err("Responses 惰性起始缺少固定事件依据".to_owned());
        }
        if shape.lazy_start_observed
            && !shape.lazy_start_accepted
            && !shape.violation_bits.start_sequence_violation
        {
            return Err("Responses 被拒绝惰性起始缺少顺序违例".to_owned());
        }
    }
    if shape.unknown_data_key_present
        && !shape
            .known_event_cardinality
            .iter()
            .any(|entry| entry.cardinality.is_present())
    {
        return Err("SSE 未知 data 键缺少固定事件依据".to_owned());
    }
    if protocol == ProviderProtocol::Responses {
        if shape.violation_bits.event_data_type_mismatch != shape.event_data_type_mismatch {
            return Err("Responses event/data mismatch 字段与违例位不一致".to_owned());
        }
        if shape.violation_bits.unknown_event_present != shape.unknown_event_present {
            return Err("Responses 未知事件字段与违例位不一致".to_owned());
        }
    } else if protocol == ProviderProtocol::Messages
        && shape.violation_bits.unknown_event_present != shape.unknown_event_present
    {
        return Err("Messages 未知事件字段与违例位不一致".to_owned());
    } else if protocol == ProviderProtocol::ChatCompletions
        && shape.violation_bits.unknown_event_present
    {
        return Err("Chat 显式 event 名不得成为 Adapter 违例".to_owned());
    }
    if shape.violation_bits.done_before_terminal
        && (protocol != ProviderProtocol::ChatCompletions || !shape.done_sentinel_observed)
    {
        return Err("SSE DONE 顺序违例缺少对应观察".to_owned());
    }
    if protocol != ProviderProtocol::ChatCompletions && shape.violation_bits.done_before_terminal {
        return Err("非 Chat SSE 不得包含 DONE 顺序违例".to_owned());
    }
    Ok(())
}

/// 校验 Chat 终态类别、两个来源基数和统一终态基数的一致性。
fn validate_chat_terminal_evidence(
    protocol: ProviderProtocol,
    shape: &SseShapeEvidence,
) -> Result<(), String> {
    if protocol != ProviderProtocol::ChatCompletions {
        if shape.chat_terminal_evidence != ChatTerminalEvidence::None
            || shape.chat_finish_reason_cardinality != SaturatedCardinality::Zero
            || shape.chat_error_cardinality != SaturatedCardinality::Zero
        {
            return Err("非 Chat SSE 不得携带 Chat 终态依据".to_owned());
        }
        return Ok(());
    }
    let expected_kind = match (
        shape.chat_finish_reason_cardinality.is_present(),
        shape.chat_error_cardinality.is_present(),
    ) {
        (false, false) => ChatTerminalEvidence::None,
        (true, false) => ChatTerminalEvidence::FinishReason,
        (false, true) => ChatTerminalEvidence::Error,
        (true, true) => ChatTerminalEvidence::Both,
    };
    if shape.chat_terminal_evidence != expected_kind
        || combine_cardinality(
            shape.chat_finish_reason_cardinality,
            shape.chat_error_cardinality,
        ) != shape.terminal_cardinality
    {
        return Err("Chat 终态依据与统一终态基数不一致".to_owned());
    }
    Ok(())
}

/// 合并两个饱和基数而不恢复精确计数。
fn combine_cardinality(
    left: SaturatedCardinality,
    right: SaturatedCardinality,
) -> SaturatedCardinality {
    match (left, right) {
        (SaturatedCardinality::Many, _) | (_, SaturatedCardinality::Many) => {
            SaturatedCardinality::Many
        }
        (SaturatedCardinality::One, SaturatedCardinality::One) => SaturatedCardinality::Many,
        (SaturatedCardinality::One, SaturatedCardinality::Zero)
        | (SaturatedCardinality::Zero, SaturatedCardinality::One) => SaturatedCardinality::One,
        (SaturatedCardinality::Zero, SaturatedCardinality::Zero) => SaturatedCardinality::Zero,
    }
}

/// 合并全部固定事件的饱和基数。
fn combined_known_event_cardinality(shape: &SseShapeEvidence) -> SaturatedCardinality {
    shape
        .known_event_cardinality
        .iter()
        .fold(SaturatedCardinality::Zero, |combined, entry| {
            combine_cardinality(combined, entry.cardinality)
        })
}

/// 返回指定固定事件的饱和基数。
fn fixed_event_cardinality(shape: &SseShapeEvidence, event: KnownSseEvent) -> SaturatedCardinality {
    shape
        .known_event_cardinality
        .iter()
        .find(|entry| entry.event == event)
        .map_or(SaturatedCardinality::Zero, |entry| entry.cardinality)
}

/// 合并 Messages 或 Responses 固定终态事件的饱和基数。
fn fixed_terminal_event_cardinality(
    protocol: ProviderProtocol,
    shape: &SseShapeEvidence,
) -> SaturatedCardinality {
    shape
        .known_event_cardinality
        .iter()
        .filter(|entry| match protocol {
            ProviderProtocol::Messages => matches!(
                entry.event,
                KnownSseEvent::MessagesMessageStop | KnownSseEvent::MessagesError
            ),
            ProviderProtocol::ChatCompletions => false,
            ProviderProtocol::Responses => matches!(
                entry.event,
                KnownSseEvent::ResponsesCompleted
                    | KnownSseEvent::ResponsesIncomplete
                    | KnownSseEvent::ResponsesCancelled
                    | KnownSseEvent::ResponsesFailed
                    | KnownSseEvent::ResponsesError
            ),
        })
        .fold(SaturatedCardinality::Zero, |combined, entry| {
            combine_cardinality(combined, entry.cardinality)
        })
}

/// 返回重复开始违例是否具有当前协议的最小固定事件依据。
fn has_duplicate_start_evidence(protocol: ProviderProtocol, shape: &SseShapeEvidence) -> bool {
    match protocol {
        ProviderProtocol::Messages => matches!(
            fixed_event_cardinality(shape, KnownSseEvent::MessagesMessageStart),
            SaturatedCardinality::Many
        ),
        ProviderProtocol::ChatCompletions => false,
        ProviderProtocol::Responses => {
            matches!(
                fixed_event_cardinality(shape, KnownSseEvent::ResponsesCreated),
                SaturatedCardinality::Many
            ) || shape.lazy_start_accepted
                && fixed_event_cardinality(shape, KnownSseEvent::ResponsesCreated).is_present()
        }
    }
}

/// 返回工具或需关闭内容状态违例是否具有最小固定事件依据。
fn has_tool_sequence_event(protocol: ProviderProtocol, shape: &SseShapeEvidence) -> bool {
    let has = |event| fixed_event_cardinality(shape, event).is_present();
    match protocol {
        ProviderProtocol::Messages => {
            has(KnownSseEvent::MessagesContentBlockDelta) || has(KnownSseEvent::MessagesMessageStop)
        }
        ProviderProtocol::ChatCompletions => has(KnownSseEvent::ChatChunk),
        ProviderProtocol::Responses => {
            has(KnownSseEvent::ResponsesFunctionArgumentsDelta)
                || has(KnownSseEvent::ResponsesFunctionArgumentsDone)
                || has(KnownSseEvent::ResponsesOutputItemDone)
                || has(KnownSseEvent::ResponsesCompleted)
                || has(KnownSseEvent::ResponsesIncomplete)
                || has(KnownSseEvent::ResponsesCancelled)
        }
    }
}

/// 返回 SSE 证据是否包含当前协议的语义终态固定事件。
fn has_semantic_terminal_event(protocol: ProviderProtocol, shape: &SseShapeEvidence) -> bool {
    shape.known_event_cardinality.iter().any(|entry| {
        entry.cardinality.is_present()
            && match protocol {
                ProviderProtocol::Messages => matches!(
                    entry.event,
                    KnownSseEvent::MessagesMessageStop | KnownSseEvent::MessagesError
                ),
                ProviderProtocol::ChatCompletions => false,
                ProviderProtocol::Responses => matches!(
                    entry.event,
                    KnownSseEvent::ResponsesCompleted
                        | KnownSseEvent::ResponsesIncomplete
                        | KnownSseEvent::ResponsesCancelled
                        | KnownSseEvent::ResponsesFailed
                        | KnownSseEvent::ResponsesError
                ),
            }
    }) || protocol == ProviderProtocol::ChatCompletions
        && (shape.chat_finish_reason_cardinality.is_present()
            || shape.chat_error_cardinality.is_present())
}

/// 返回 Responses 证据是否包含惰性起始白名单中的固定事件。
fn has_lazy_start_event(shape: &SseShapeEvidence) -> bool {
    shape.known_event_cardinality.iter().any(|entry| {
        entry.cardinality.is_present()
            && matches!(
                entry.event,
                KnownSseEvent::ResponsesOutputItemAdded
                    | KnownSseEvent::ResponsesContentPartAdded
                    | KnownSseEvent::ResponsesOutputTextDelta
                    | KnownSseEvent::ResponsesRefusalDelta
                    | KnownSseEvent::ResponsesReasoningSummaryTextDelta
                    | KnownSseEvent::ResponsesReasoningTextDelta
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 创建一份完整 EOF、未截断的测试证据。
    fn inspect(
        protocol: ProviderProtocol,
        content_type: Option<&str>,
        body: &[u8],
    ) -> WireResponseShapeEvidence {
        inspect_wire_response_shape(protocol, Some(200), content_type, body, true, false)
    }

    /// 返回证据中指定固定事件的基数。
    fn event_cardinality(
        evidence: &WireResponseShapeEvidence,
        event: KnownSseEvent,
    ) -> SaturatedCardinality {
        evidence
            .sse_shape
            .as_ref()
            .expect("测试响应应为 SSE")
            .known_event_cardinality
            .iter()
            .find(|entry| entry.event == event)
            .expect("固定事件位置必须存在")
            .cardinality
    }

    /// 验证媒体类型参数、大小写和未知值只形成固定类别。
    #[test]
    fn declared_content_type_只保留固定类别() {
        assert_eq!(
            classify_declared_content_type(Some(" Application/Problem+JSON ; secret=value")),
            DeclaredContentType::Json
        );
        assert_eq!(
            classify_declared_content_type(Some("TEXT/EVENT-STREAM; charset=utf-8")),
            DeclaredContentType::Sse
        );
        assert_eq!(
            classify_declared_content_type(Some("secret+json")),
            DeclaredContentType::Other
        );
        assert_eq!(
            classify_declared_content_type(None),
            DeclaredContentType::Missing
        );
    }

    /// 验证三协议合法缓冲 JSON 都产生可校验的固定结构。
    #[test]
    fn 三协议合法_json_结构可校验() {
        let cases = [
            (
                ProviderProtocol::Messages,
                json!({
                    "id": "response-value",
                    "model": "model-value",
                    "content": [
                        {"type": "text", "text": "text-value"},
                        {"type": "thinking", "thinking": "reasoning-value", "signature": "state-value"},
                        {"type": "tool_use", "id": "tool-id", "name": "tool-name", "input": {"argument": "value"}}
                    ],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }),
            ),
            (
                ProviderProtocol::ChatCompletions,
                json!({
                    "id": "response-value",
                    "model": "model-value",
                    "choices": [{
                        "message": {
                            "content": [{"type": "output_text", "text": "text-value"}],
                            "reasoning_content": "reasoning-value",
                            "tool_calls": [{"id": "tool-id", "function": {"name": "tool-name", "arguments": "{}"}}]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {}
                }),
            ),
            (
                ProviderProtocol::Responses,
                json!({
                    "id": "response-value",
                    "model": "model-value",
                    "status": "completed",
                    "output": [
                        {"type": "message", "content": [{"type": "output_text", "text": "text-value"}]},
                        {"type": "reasoning", "content": [{"type": "reasoning_text", "text": "reasoning-value"}], "summary": [{"text": "summary-value"}]},
                        {"type": "function_call", "call_id": "tool-id", "name": "tool-name", "arguments": "{}"}
                    ],
                    "usage": {}
                }),
            ),
        ];
        for (protocol, body) in cases {
            let evidence = inspect(
                protocol,
                Some("application/json"),
                &serde_json::to_vec(&body).expect("测试 JSON 应可编码"),
            );
            assert_eq!(evidence.body_format, WireBodyFormat::Json);
            evidence.validate().expect("合法 JSON 证据应通过内部校验");
            assert_eq!(
                evidence.classify_decode_failure(),
                DecodeFailureAttribution::AdapterSuspect
            );
        }
    }

    /// 验证三协议缓冲响应的空白 ID 或模型身份属于 Wire 契约问题而不是 Adapter 嫌疑。
    #[test]
    fn 三协议空白响应元数据归为_nonconformant() {
        let cases = [
            (
                ProviderProtocol::Messages,
                json!({"id": "response", "model": "model", "content": []}),
            ),
            (
                ProviderProtocol::ChatCompletions,
                json!({
                    "id": "response",
                    "model": "model",
                    "choices": [{"message": {"content": "text"}, "finish_reason": "stop"}]
                }),
            ),
            (
                ProviderProtocol::Responses,
                json!({
                    "id": "response",
                    "model": "model",
                    "status": "completed",
                    "output": []
                }),
            ),
        ];
        for (protocol, body) in cases {
            for field in ["id", "model"] {
                let mut invalid = body.clone();
                invalid[field] = Value::String(" \t".to_owned());
                let evidence = inspect(
                    protocol,
                    Some("application/json"),
                    &serde_json::to_vec(&invalid).expect("测试 JSON 应可编码"),
                );
                evidence.validate().expect("违例证据自身布局必须有效");
                assert!(
                    evidence
                        .json_shape
                        .as_ref()
                        .expect("应为 JSON")
                        .violation_bits
                        .known_field_type_mismatch
                );
                assert_eq!(
                    evidence.classify_decode_failure(),
                    DecodeFailureAttribution::Nonconformant
                );
            }
        }
    }

    /// 验证 JSON 键顺序、空白和 UTF-8 BOM 不改变结构证据。
    #[test]
    fn json_键顺序空白与_bom_不改变证据() {
        let first = br#"{"id":"id","model":"model","content":[{"type":"text","text":"value"}],"stop_reason":"end_turn"}"#;
        let second = br#" { "stop_reason" : "different", "content" : [ { "text" : "different", "type" : "text" } ], "model" : "different", "id" : "different" } "#;
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(second);
        let first = inspect(ProviderProtocol::Messages, Some("application/json"), first);
        let second = inspect(ProviderProtocol::Messages, Some("application/json"), &bom);
        assert_eq!(first, second);
    }

    /// 验证未知 JSON 键名和值不同只产生同一个 present 事实。
    #[test]
    fn 未知_json_键名和值不进入证据() {
        let first = br#"{"content":[],"private_key_alpha":"secret-alpha"}"#;
        let second = br#"{"content":[],"private_key_beta":{"nested":"secret-beta"}}"#;
        let first = inspect(ProviderProtocol::Messages, Some("application/json"), first);
        let second = inspect(ProviderProtocol::Messages, Some("application/json"), second);
        assert_eq!(first, second);
        assert!(
            first
                .json_shape
                .as_ref()
                .expect("应为 JSON")
                .unknown_key_present
        );
    }

    /// 验证三协议合法 SSE、CRLF 和终态均产生固定结构。
    #[test]
    fn 三协议合法_sse_结构可校验() {
        let messages = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"a\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let chat = concat!(
            "data: {\"id\":\"id\",\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let responses = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"a\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n"
        );
        for (protocol, body) in [
            (ProviderProtocol::Messages, messages),
            (ProviderProtocol::ChatCompletions, chat),
            (ProviderProtocol::Responses, responses),
        ] {
            let lf = inspect(protocol, Some("text/event-stream"), body.as_bytes());
            let crlf_body = body.replace('\n', "\r\n");
            let crlf = inspect(protocol, Some("text/event-stream"), crlf_body.as_bytes());
            assert_eq!(lf, crlf);
            lf.validate().expect("合法 SSE 证据应通过内部校验");
            let shape = lf.sse_shape.as_ref().expect("应为 SSE");
            assert!(shape.terminal_observed);
            assert!(!shape.violation_bits.any());
        }
    }

    /// 验证 Chat Adapter 接受的空 choices Usage 尾块不会被证据层误判为终态后事件。
    #[tokio::test]
    async fn chat_sse_终态后空choices_usage尾块保持一致() {
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"KC_OK\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat-1\",\"model\":\"test-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n\n"
        );

        let evidence = inspect(
            ProviderProtocol::ChatCompletions,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        evidence.validate().expect("合法 Usage 尾块证据应通过校验");
        let shape = evidence.sse_shape.as_ref().expect("应为 Chat SSE");
        assert!(!shape.event_after_terminal);
        assert!(!shape.violation_bits.event_after_terminal);
        assert!(
            keencode_provider::replay_wire_response(
                ProviderProtocol::ChatCompletions,
                "text/event-stream",
                body.as_bytes(),
                64 * 1024,
            )
            .await
            .is_ok()
        );
    }

    /// 验证 Chat 只在 `[DONE]` 后进入 ended，Responses 则在协议终态后立即拒绝空 data。
    #[tokio::test]
    async fn chat与responses_sse_终态后空data归为_nonconformant() {
        let chat_before_done = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data:\n\n"
        );
        let accepted = inspect(
            ProviderProtocol::ChatCompletions,
            Some("text/event-stream"),
            chat_before_done.as_bytes(),
        );
        accepted
            .validate()
            .expect("Chat finish_reason 后的空 data 应被接受");
        let accepted_shape = accepted.sse_shape.as_ref().expect("应为 Chat SSE");
        assert!(!accepted_shape.empty_data_after_terminal_observed);
        assert!(!accepted_shape.event_after_terminal);
        assert!(
            keencode_provider::replay_wire_response(
                ProviderProtocol::ChatCompletions,
                "text/event-stream",
                chat_before_done.as_bytes(),
                64 * 1024,
            )
            .await
            .is_ok()
        );

        let chat = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
            "data:\n\n"
        );
        let responses = concat!(
            "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data:\n\n"
        );
        for (protocol, body) in [
            (ProviderProtocol::ChatCompletions, chat),
            (ProviderProtocol::Responses, responses),
        ] {
            assert!(
                keencode_provider::replay_wire_response(
                    protocol,
                    "text/event-stream",
                    body.as_bytes(),
                    64 * 1024,
                )
                .await
                .is_err()
            );
            let evidence = inspect(protocol, Some("text/event-stream"), body.as_bytes());
            evidence.validate().expect("终态后空 data 证据必须自洽");
            let shape = evidence.sse_shape.as_ref().expect("应为 SSE");
            assert!(shape.empty_data_after_terminal_observed);
            assert!(shape.event_after_terminal);
            assert!(shape.violation_bits.event_after_terminal);
            assert_eq!(
                evidence.classify_decode_failure(),
                DecodeFailureAttribution::Nonconformant
            );
        }
    }

    /// 验证三协议 SSE 开始元数据的空白 ID 或模型身份都与 Adapter 拒绝规则一致。
    #[tokio::test]
    async fn 三协议_sse_空白响应元数据归为_nonconformant() {
        let cases = [
            (
                ProviderProtocol::Messages,
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{\"id\":\" \",\"model\":\"model\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n"
                ),
            ),
            (
                ProviderProtocol::ChatCompletions,
                concat!(
                    "data: {\"id\":\" \",\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            ),
            (
                ProviderProtocol::Responses,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\" \",\"model\":\"model\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
                ),
            ),
        ];
        for (protocol, body) in cases {
            assert!(
                keencode_provider::replay_wire_response(
                    protocol,
                    "text/event-stream",
                    body.as_bytes(),
                    64 * 1024,
                )
                .await
                .is_err()
            );
            let evidence = inspect(protocol, Some("text/event-stream"), body.as_bytes());
            evidence.validate().expect("SSE 空白元数据违例证据必须自洽");
            assert!(
                evidence
                    .sse_shape
                    .as_ref()
                    .expect("应为 SSE")
                    .violation_bits
                    .known_field_type_mismatch
            );
            assert_eq!(
                evidence.classify_decode_failure(),
                DecodeFailureAttribution::Nonconformant
            );
        }
    }

    /// 验证 Messages Adapter 不读取对象字段的 ping/message_stop 接受有效 JSON null。
    #[test]
    fn messages_sse_ping与message_stop允许null_data() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: ping\n",
            "data: null\n\n",
            "event: message_stop\n",
            "data: null\n\n"
        );
        let evidence = inspect(
            ProviderProtocol::Messages,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        evidence
            .validate()
            .expect("Adapter 接受的 null data 应形成自洽证据");
        let shape = evidence.sse_shape.as_ref().expect("应为 SSE");
        assert!(shape.terminal_observed);
        assert!(!shape.violation_bits.data_root_type_mismatch);
        assert!(!shape.violation_bits.any());
    }

    /// 验证 Responses 可从固定白名单内容事件安全惰性起始。
    #[test]
    fn responses_sse_允许带模型身份的惰性起始() {
        let body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"model\":\"model-value\",\"output_index\":0,\"delta\":\"text\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        );
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        let shape = evidence.sse_shape.as_ref().expect("应为 SSE");
        assert!(shape.lazy_start_observed);
        assert!(shape.lazy_start_accepted);
        assert!(!shape.violation_bits.start_sequence_violation);
        assert!(!shape.violation_bits.any());
    }

    /// 验证 Responses 文本、推理、拒绝、工具参数和终态事件都落到固定位置。
    #[test]
    fn responses_sse_覆盖文本推理工具与错误终态() {
        let body = concat!(
            "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
            "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"text\"}}\n\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":1,\"delta\":\"reasoning\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":1,\"delta\":\"summary\"}\n\n",
            "data: {\"type\":\"response.refusal.delta\",\"output_index\":2,\"delta\":\"refusal\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":3,\"item\":{\"type\":\"function_call\",\"call_id\":\"tool-id\",\"name\":\"tool-name\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":3,\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":3,\"arguments\":\"{}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":3,\"item\":{\"type\":\"function_call\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        );
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        let shape = evidence.sse_shape.as_ref().expect("应为 SSE");
        assert!(!shape.violation_bits.any());
        assert_eq!(
            event_cardinality(&evidence, KnownSseEvent::ResponsesFunctionArgumentsDelta),
            SaturatedCardinality::One
        );
        assert_eq!(
            event_cardinality(&evidence, KnownSseEvent::ResponsesCompleted),
            SaturatedCardinality::One
        );

        let error = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            b"data: {\"type\":\"error\",\"error\":{}}\n\n",
        );
        let error_shape = error.sse_shape.as_ref().expect("应为 SSE");
        assert!(error_shape.terminal_observed);
        assert!(!error_shape.violation_bits.any());
    }

    /// 验证三协议工具增量缺少既有状态时只设置固定顺序违例位。
    #[test]
    fn 三协议工具顺序违例可独立识别() {
        let messages = inspect(
            ProviderProtocol::Messages,
            Some("text/event-stream"),
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
            .as_bytes(),
        );
        assert!(
            messages
                .sse_shape
                .as_ref()
                .expect("应为 SSE")
                .violation_bits
                .tool_sequence_violation
        );
        let chat = inspect(
            ProviderProtocol::ChatCompletions,
            Some("text/event-stream"),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        );
        assert!(
            chat.sse_shape
                .as_ref()
                .expect("应为 SSE")
                .violation_bits
                .tool_sequence_violation
        );
        let responses = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            concat!(
                "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{}\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
            )
            .as_bytes(),
        );
        assert!(
            responses
                .sse_shape
                .as_ref()
                .expect("应为 SSE")
                .violation_bits
                .tool_sequence_violation
        );
    }

    /// 验证缺模型身份的惰性起始、event/data mismatch 和终态后事件均被区分。
    #[test]
    fn responses_sse_区分顺序_mismatch_和终态后事件() {
        let body = concat!(
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"text\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "event: response.in_progress\n",
            "data: {\"type\":\"response.in_progress\"}\n\n"
        );
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        let shape = evidence.sse_shape.as_ref().expect("应为 SSE");
        assert!(shape.event_data_type_mismatch);
        assert!(shape.violation_bits.event_data_type_mismatch);
        assert!(shape.violation_bits.start_sequence_violation);
        assert!(shape.event_after_terminal);
        assert_eq!(
            evidence.classify_decode_failure(),
            DecodeFailureAttribution::Nonconformant
        );
    }

    /// 验证 Responses 终态后的未知、缺失和不可解码事件都与 Adapter 拒绝行为一致。
    #[tokio::test]
    async fn responses_sse_终态后任意非空非_done_帧均留证() {
        let suffixes = [
            "data: {\"type\":\"private-event-name\",\"private\":\"private-value\"}\n\n",
            "data: {\"private\":\"private-value\"}\n\n",
            "data: private-invalid-json\n\n",
        ];
        for suffix in suffixes {
            let body = format!(
                concat!(
                    "data: {{\"type\":\"response.created\",\"response\":{{}}}}\n\n",
                    "data: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n",
                    "{}"
                ),
                suffix
            );
            assert!(
                keencode_provider::replay_wire_response(
                    ProviderProtocol::Responses,
                    "text/event-stream",
                    body.as_bytes(),
                    64 * 1024,
                )
                .await
                .is_err()
            );
            let evidence = inspect(
                ProviderProtocol::Responses,
                Some("text/event-stream"),
                body.as_bytes(),
            );
            evidence.validate().expect("终态后不可接受帧证据必须自洽");
            let shape = evidence.sse_shape.as_ref().expect("应为 Responses SSE");
            assert!(shape.event_after_terminal);
            assert!(shape.violation_bits.event_after_terminal);
            assert_eq!(
                evidence.classify_decode_failure(),
                DecodeFailureAttribution::Nonconformant
            );
            let persisted = serde_json::to_string(&evidence).expect("证据应可序列化");
            assert!(!persisted.contains("private-event-name"));
            assert!(!persisted.contains("private-value"));
            assert!(!persisted.contains("private-invalid-json"));
        }
    }

    /// 验证未知 SSE event 的不同原文只保存同一个 present 事实。
    #[test]
    fn 未知_sse_event_名称不进入证据() {
        let first = b"event: private-event-alpha\ndata: {\"type\":\"private-event-alpha\"}\n\n";
        let second = b"event: private-event-beta\ndata: {\"type\":\"private-event-beta\"}\n\n";
        let first = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            first,
        );
        let second = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            second,
        );
        assert_eq!(first, second);
        assert!(
            first
                .sse_shape
                .as_ref()
                .expect("应为 SSE")
                .unknown_event_present
        );
    }

    /// 验证缺失、重复终态和终态后事件分别设置固定违例位。
    #[test]
    fn sse_终态规则分别留证() {
        let missing = inspect(
            ProviderProtocol::Messages,
            Some("text/event-stream"),
            b"data: {\"type\":\"message_start\",\"message\":{}}\n\n",
        );
        assert!(
            missing
                .sse_shape
                .as_ref()
                .expect("应为 SSE")
                .violation_bits
                .terminal_missing
        );
        let duplicate = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            concat!(
                "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
                "data: {\"type\":\"response.cancelled\",\"response\":{}}\n\n"
            )
            .as_bytes(),
        );
        let bits = &duplicate
            .sse_shape
            .as_ref()
            .expect("应为 SSE")
            .violation_bits;
        assert!(bits.duplicate_terminal);
        assert!(bits.event_after_terminal);
    }

    /// 验证 SSE 未以空行分帧只记录 trailing partial，不保存片段。
    #[test]
    fn sse_区分尾部半帧() {
        let body = concat!(
            "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}"
        );
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            body.as_bytes(),
        );
        assert!(
            evidence
                .sse_shape
                .as_ref()
                .expect("应为 SSE")
                .trailing_partial_frame
        );
        evidence.validate().expect("EOF 可合法结束 SSE 半帧");
    }

    /// 验证大量数组元素和重复事件只饱和为 many。
    #[test]
    fn 大量元素与事件只饱和为_many() {
        let content = (0..1024)
            .map(|_| json!({"type": "text", "text": "value"}))
            .collect::<Vec<_>>();
        let json_body = serde_json::to_vec(&json!({"content": content})).expect("应可编码");
        let json_evidence = inspect(
            ProviderProtocol::Messages,
            Some("application/json"),
            &json_body,
        );
        assert_eq!(
            json_evidence
                .json_shape
                .as_ref()
                .expect("应为 JSON")
                .known_nested_path_shapes[0]
                .cardinality,
            SaturatedCardinality::Many
        );
        let mut sse_body = String::new();
        for _ in 0..1024 {
            sse_body.push_str("data: {\"type\":\"response.in_progress\"}\n\n");
        }
        sse_body.push_str("data: {\"type\":\"error\",\"error\":{}}\n\n");
        let sse_evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            sse_body.as_bytes(),
        );
        assert_eq!(
            event_cardinality(&sse_evidence, KnownSseEvent::ResponsesInProgress),
            SaturatedCardinality::Many
        );
    }

    /// 验证正文值、未知键、工具参数和媒体类型参数都不会进入持久化或调试输出。
    #[test]
    fn 结构证据不泄露任何秘密原文() {
        let secrets = [
            "wire-private-key-name",
            "wire-private-string-value",
            "wire-private-tool-arguments",
            "wire-private-content-type-parameter",
        ];
        let body = format!(
            "{{\"status\":\"completed\",\"output\":[{{\"type\":\"function_call\",\"call_id\":\"id\",\"name\":\"name\",\"arguments\":\"{}\"}}],\"{}\":\"{}\"}}",
            secrets[2], secrets[0], secrets[1]
        );
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("application/json; boundary=wire-private-content-type-parameter"),
            body.as_bytes(),
        );
        let serialized = serde_json::to_string(&evidence).expect("证据应可序列化");
        let debug = format!("{evidence:?}");
        for secret in secrets {
            assert!(!serialized.contains(secret));
            assert!(!debug.contains(secret));
        }
        let mut tampered = evidence.clone();
        tampered
            .json_shape
            .as_mut()
            .expect("应为 JSON")
            .known_field_types
            .clear();
        let validation_error = tampered.validate().expect_err("篡改应被拒绝");
        for secret in secrets {
            assert!(!validation_error.contains(secret));
        }
    }

    /// 验证符合契约的供应商错误由 shape 与 Adapter replay 一致归因且不保存正文。
    #[tokio::test]
    async fn responses_供应商显式错误不归因于_adapter() {
        let cases = [
            (
                "application/json",
                br#"{"error":{"message":"private-buffered-error"}}"#.as_slice(),
                Some("private-buffered-error"),
            ),
            (
                "application/json",
                br#"{"status":"failed"}"#.as_slice(),
                None,
            ),
            (
                "text/event-stream",
                b"data: {\"type\":\"error\",\"error\":{\"message\":\"private-sse-error\"}}\n\n".as_slice(),
                Some("private-sse-error"),
            ),
            (
                "text/event-stream",
                b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"private-failed-error\"}}}\n\n".as_slice(),
                Some("private-failed-error"),
            ),
            (
                "text/event-stream",
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{},\"error\":{\"message\":\"private-typed-top-error\"}}\n\n"
                )
                .as_bytes(),
                Some("private-typed-top-error"),
            ),
            (
                "text/event-stream",
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"error\":{\"message\":\"private-typed-response-error\"}}}\n\n"
                )
                .as_bytes(),
                Some("private-typed-response-error"),
            ),
            (
                "text/event-stream",
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"text\",\"status\":\"failed\",\"message\":\"private-typed-top-status\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
                )
                .as_bytes(),
                Some("private-typed-top-status"),
            ),
            (
                "text/event-stream",
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"text\",\"response\":{\"status\":\"failed\",\"model\":\"private-typed-response-status\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
                )
                .as_bytes(),
                Some("private-typed-response-status"),
            ),
        ];
        for (content_type, body, secret) in cases {
            assert!(
                keencode_provider::replay_wire_response(
                    ProviderProtocol::Responses,
                    content_type,
                    body,
                    64 * 1024,
                )
                .await
                .is_err()
            );
            let evidence = inspect(ProviderProtocol::Responses, Some(content_type), body);
            evidence.validate().expect("供应商显式错误证据必须自洽");
            if content_type == "application/json" {
                assert!(
                    evidence
                        .json_shape
                        .as_ref()
                        .expect("应为 Responses JSON")
                        .provider_declared_error
                );
            } else {
                assert!(
                    evidence
                        .sse_shape
                        .as_ref()
                        .expect("应为 Responses SSE")
                        .responses_provider_declared_error
                );
            }
            assert_eq!(
                evidence.classify_decode_failure(),
                DecodeFailureAttribution::ProviderDeclaredError
            );
            let persisted = serde_json::to_string(&evidence).expect("证据应可序列化");
            let debug = format!("{evidence:?}");
            if let Some(secret) = secret {
                assert!(!persisted.contains(secret));
                assert!(!debug.contains(secret));
            }
        }
    }

    /// 验证 EOF、证据截断、固定契约违例和供应商显式错误形成四类归因。
    #[test]
    fn decode_失败归因严格区分四类() {
        let valid = br#"{"content":[{"type":"text","text":"value"}]}"#;
        let complete = inspect(ProviderProtocol::Messages, Some("application/json"), valid);
        assert_eq!(
            complete.classify_decode_failure(),
            DecodeFailureAttribution::AdapterSuspect
        );
        let invalid = inspect(
            ProviderProtocol::Messages,
            Some("application/json"),
            br#"{"content":"wrong"}"#,
        );
        assert_eq!(
            invalid.classify_decode_failure(),
            DecodeFailureAttribution::Nonconformant
        );
        let mut no_eof = complete.clone();
        no_eof.body_eof_observed = false;
        assert_eq!(
            no_eof.classify_decode_failure(),
            DecodeFailureAttribution::Indeterminate
        );
        let mut truncated = complete;
        truncated.capture_truncated = true;
        assert_eq!(
            truncated.classify_decode_failure(),
            DecodeFailureAttribution::Indeterminate
        );
        let mut no_status = truncated;
        no_status.capture_truncated = false;
        no_status.http_status = None;
        assert_eq!(
            no_status.classify_decode_failure(),
            DecodeFailureAttribution::Indeterminate
        );
    }

    /// 验证声明为 SSE 的完整 JSON 错误仍识别为 JSON 并暴露声明错配。
    #[test]
    fn 完整_json_优先于_sse_声明识别() {
        let evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream; private=secret"),
            br#"{"error":{"message":"private-error"}}"#,
        );
        assert_eq!(evidence.declared_content_type, DeclaredContentType::Sse);
        assert_eq!(evidence.body_format, WireBodyFormat::Json);
        assert_eq!(
            evidence.classify_decode_failure(),
            DecodeFailureAttribution::Nonconformant
        );
        assert!(
            !serde_json::to_string(&evidence)
                .expect("应可序列化")
                .contains("private-error")
        );
    }

    /// 验证空正文、未知文本和无效 UTF-8 保持三个互斥格式且不附带 shape。
    #[test]
    fn 非结构正文格式保持互斥() {
        for (body, expected) in [
            (&b""[..], WireBodyFormat::Empty),
            (&b"plain-private-text"[..], WireBodyFormat::Unknown),
            (&b"\xff\xfe"[..], WireBodyFormat::InvalidUtf8),
        ] {
            let evidence = inspect(ProviderProtocol::Responses, None, body);
            assert_eq!(evidence.body_format, expected);
            assert!(evidence.json_shape.is_none());
            assert!(evidence.sse_shape.is_none());
            evidence.validate().expect("互斥格式应通过内部校验");
        }
    }

    /// 验证 serde 严格拒绝 Evidence 未知字段和未知固定枚举值。
    #[test]
    fn evidence_严格拒绝未知字段与枚举() {
        let evidence = inspect(
            ProviderProtocol::Messages,
            Some("application/json"),
            br#"{"content":[]}"#,
        );
        let mut unknown_field = serde_json::to_value(&evidence).expect("应可编码");
        unknown_field
            .as_object_mut()
            .expect("应为对象")
            .insert("unexpected".to_owned(), Value::Bool(true));
        assert!(serde_json::from_value::<WireResponseShapeEvidence>(unknown_field).is_err());
        let mut unknown_enum = serde_json::to_value(&evidence).expect("应可编码");
        unknown_enum["declaredContentType"] = Value::String("private_type".to_owned());
        assert!(serde_json::from_value::<WireResponseShapeEvidence>(unknown_enum).is_err());

        let mut previous_shape = serde_json::to_value(&evidence).expect("应可编码");
        previous_shape["jsonShape"]["shapeSchema"] = Value::String("v1".to_owned());
        assert!(serde_json::from_value::<WireResponseShapeEvidence>(previous_shape).is_err());
    }

    /// 验证反序列化后的 HTTP 状态仍由内部校验限制在可构造范围。
    #[test]
    fn evidence_validate_严格校验_http_状态范围() {
        let evidence = inspect(
            ProviderProtocol::Messages,
            Some("application/json"),
            br#"{"content":[]}"#,
        );
        for status in [99_u64, 1000_u64] {
            let mut encoded = serde_json::to_value(&evidence).expect("证据应可编码");
            encoded["httpStatus"] = Value::from(status);
            let decoded = serde_json::from_value::<WireResponseShapeEvidence>(encoded)
                .expect("u16 范围内状态应先完成反序列化");
            assert!(decoded.validate().is_err());
        }
        for status in [100_u64, 200_u64, 999_u64] {
            let mut encoded = serde_json::to_value(&evidence).expect("证据应可编码");
            encoded["httpStatus"] = Value::from(status);
            let decoded = serde_json::from_value::<WireResponseShapeEvidence>(encoded)
                .expect("合法状态应完成反序列化");
            decoded.validate().expect("合法状态应通过校验");
        }

        let missing_status =
            inspect_wire_response_shape(ProviderProtocol::Messages, None, None, b"", false, false);
        missing_status
            .validate()
            .expect("没有响应头和正文事实的证据应通过校验");

        let mut impossible_body = evidence.clone();
        impossible_body.http_status = None;
        assert!(impossible_body.validate().is_err());

        let mut impossible_truncation = inspect_wire_response_shape(
            ProviderProtocol::Messages,
            Some(200),
            None,
            b"",
            true,
            false,
        );
        impossible_truncation.capture_truncated = true;
        assert!(impossible_truncation.validate().is_err());
    }

    /// 验证可伪造的 SSE 终态与惰性起始布尔位必须具备固定事件依据。
    #[test]
    fn evidence_validate_拒绝缺少事件依据的布尔位() {
        let mut evidence = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            b"data: {\"type\":\"response.in_progress\"}\n\n",
        );
        let shape = evidence.sse_shape.as_mut().expect("应为 SSE");
        shape.terminal_observed = true;
        shape.violation_bits.terminal_missing = false;
        assert!(evidence.validate().is_err());

        let mut lazy = inspect(
            ProviderProtocol::Responses,
            Some("text/event-stream"),
            b"data: {\"type\":\"response.in_progress\"}\n\n",
        );
        let shape = lazy.sse_shape.as_mut().expect("应为 SSE");
        shape.lazy_start_observed = true;
        shape.lazy_start_accepted = true;
        assert!(lazy.validate().is_err());

        let mut chat = inspect(
            ProviderProtocol::ChatCompletions,
            Some("text/event-stream"),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
        );
        let shape = chat.sse_shape.as_mut().expect("应为 Chat SSE");
        shape.terminal_observed = true;
        shape.terminal_cardinality = SaturatedCardinality::One;
        shape.violation_bits.terminal_missing = false;
        assert!(chat.validate().is_err());

        let mut duplicate_start = inspect(
            ProviderProtocol::Messages,
            Some("text/event-stream"),
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
        );
        duplicate_start
            .sse_shape
            .as_mut()
            .expect("应为 Messages SSE")
            .violation_bits
            .duplicate_start = true;
        assert!(duplicate_start.validate().is_err());

        let mut nested = inspect(
            ProviderProtocol::Messages,
            Some("application/json"),
            br#"{"content":[{"type":"text","text":"a"}]}"#,
        );
        nested
            .json_shape
            .as_mut()
            .expect("应为 JSON")
            .known_nested_path_shapes[0]
            .cardinality = SaturatedCardinality::Zero;
        assert!(nested.validate().is_err());
    }

    /// 验证 Chat 终态后空 data 事实不能仅凭错误终态伪造，必须同时具备 finish_reason 与 DONE。
    #[test]
    fn evidence_validate_拒绝缺少_chat_ended_依据的空data事实() {
        for body in [
            "data: {\"error\":{}}\n\n",
            "data: {\"error\":{}}\n\ndata: [DONE]\n\n",
        ] {
            let mut evidence = inspect(
                ProviderProtocol::ChatCompletions,
                Some("text/event-stream"),
                body.as_bytes(),
            );
            let shape = evidence.sse_shape.as_mut().expect("应为 Chat SSE");
            shape.event_after_terminal = true;
            shape.empty_data_after_terminal_observed = true;
            shape.violation_bits.event_after_terminal = true;
            assert!(evidence.validate().is_err());
        }
    }
}
