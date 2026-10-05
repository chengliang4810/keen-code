use std::collections::BTreeMap;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentId, ArtifactId, FileSnapshot, MailboxMessageId, RequestId, SessionEventId, SessionId,
    TerminalId, TurnId,
};

/// 当前全新 Session 事件格式的固定 schema 名称。
pub const SESSION_EVENT_SCHEMA: &str = "keencode/session-event";
/// 当前全新 Session 事件格式版本。
pub const SESSION_EVENT_VERSION: u32 = 8;
/// Session 内唯一根 Agent 使用的固定标识。
pub const ROOT_AGENT_ID: &str = "root";
/// Runtime 通用命令收据的稳定 schema 名称。
pub const COMMAND_RECEIPT_SCHEMA: &str = "keencode.command-receipt.v1";
/// 单条命令收据允许保留的 ACK 最大 JSON 字节数。
pub const MAX_COMMAND_RECEIPT_BYTES: usize = 64 * 1024;
/// 冷恢复时返回给模型的唯一副作用未知错误文本。
pub const SIDE_EFFECT_UNKNOWN_RESULT_TEXT: &str =
    "工具在崩溃前已经开始，副作用状态未知，禁止自动重试";
/// 工作流事件 payload 使用的独立递归 JSON 集合预算。
///
/// 普通会话继续受 `JournalConfig::max_state_collection_items`（默认 50,000）约束；
/// 1,024 次有界节点执行会产生多条生命周期事件，需要更大的固定上限，但仍拒绝
/// 无界的深度或恶意大 payload。
pub const MAX_WORKFLOW_JSON_COLLECTION_ITEMS: usize = 250_000;

/// Session 当前生命周期状态。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum SessionStatus {
    /// Session 已创建但当前没有运行 Turn。
    #[default]
    Idle,
    /// Session 正在处理一个或多个 Turn。
    Running,
    /// Session 等待标准 Elicitation 用户输入。
    Waiting,
    /// Session 已明确关闭。
    Closed,
}

/// 一个 Turn 的生命周期状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum TurnStatus {
    /// Turn 已开始且尚未结束。
    Running,
    /// Turn 正常完成。
    Completed,
    /// Turn 失败并保留安全错误摘要。
    Failed,
    /// Turn 被用户或 Runtime 取消。
    Cancelled,
}

/// Turn 非正常停止的结构化原因。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum TurnStopReason {
    /// Turn 被用户或 Runtime 主动取消。
    Cancelled,
    /// Turn 因执行错误停止。
    Failed,
    /// Turn 因达到 Runtime 模型轮次或工具调用上限停止。
    LimitReached,
    /// Turn 因上下文无法继续压缩或提交而停止。
    ContextBlocked,
    /// 模型响应达到输出 Token 上限，不能视为完整完成。
    ModelOutputLimit,
    /// 模型因内容策略或拒答停止，不能视为完整完成。
    ModelRefusal,
}

impl TurnStopReason {
    /// 推导用于列表和 Session 生命周期判断的粗粒度 Turn 状态。
    pub const fn status(self) -> TurnStatus {
        match self {
            Self::Cancelled => TurnStatus::Cancelled,
            Self::Failed
            | Self::LimitReached
            | Self::ContextBlocked
            | Self::ModelOutputLimit
            | Self::ModelRefusal => TurnStatus::Failed,
        }
    }
}

/// Session 消息的语义角色。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MessageRole {
    /// 用户输入。
    User,
    /// Agent 生成内容。
    Assistant,
    /// Runtime 注入且可审计的系统说明。
    System,
    /// 应用注入且优先于普通用户输入的开发约束。
    Developer,
    /// 工具执行结果。
    Tool,
}

/// Assistant 消息的本地反馈；只服务于会话行投影，不会进入模型上下文。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum AssistantFeedback {
    /// 用户认为该 Assistant 回复有帮助。
    Like,
    /// 用户认为该 Assistant 回复没有帮助。
    Dislike,
}

/// 按 V4 行稳定身份保存的一条 Assistant 反馈事实。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistantFeedbackRecord {
    /// 会话内稳定、不可复用的展示行标识。
    pub row_id: u64,
    /// 与 rowId 成对校验的持久实体标识。
    pub entity_id: String,
    /// 当前保留的反馈值；取消反馈时整条记录删除。
    pub feedback: AssistantFeedback,
}

/// 持久化图片可恢复的来源。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum MessageImageSource {
    /// 由模型服务读取的绝对网络地址。
    Url {
        /// 完整图片地址。
        url: String,
    },
    /// 已写入当前 Session ArtifactStore 的图片字节。
    Artifact {
        /// 带媒体类型的内容寻址引用。
        artifact: ArtifactUse,
    },
}

/// Provider Adapter 管理且 Runtime 不解释的推理续传状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReasoningContinuation {
    /// 标识状态编码方式的稳定名称。
    pub kind: String,
    /// 后续模型请求需要原样带回的不透明 JSON。
    pub data: Value,
}

/// 工具结果内部保持原始顺序的内容块。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    /// UTF-8 文本结果。
    Text {
        /// 工具返回的完整小型文本。
        text: String,
    },
    /// URL 或内容寻址 Artifact 图片。
    Image {
        /// 可在恢复后重新构造模型输入的图片来源。
        source: MessageImageSource,
    },
    /// 超过内联预算的通用工具结果。
    Artifact {
        /// 当前 Session 内的内容寻址引用。
        artifact: ArtifactUse,
        /// 读取 Artifact 后应恢复成的模型内容类型。
        materialization: ArtifactMaterialization,
    },
}

/// Artifact 恢复到模型消息时采用的明确内容类型。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ArtifactMaterialization {
    /// Artifact 字节必须是完整 UTF-8 文本。
    Utf8Text,
    /// Artifact 字节必须按媒体类型恢复为图片。
    Image,
    /// Artifact 只用于审计或下载，不直接进入模型消息。
    Binary,
}

/// 一个消息内按顺序保存的类型化内容。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    /// 普通 UTF-8 文本。
    Text {
        /// 完整文本正文。
        text: String,
    },
    /// 模型生成的可展示推理和可选续传状态。
    Reasoning {
        /// 可展示或可审计的推理文本。
        text: String,
        /// Provider 提供的可选短摘要。
        summary: Option<String>,
        /// 后续 Round 需要原样回传的可选不透明状态。
        continuation: Option<ReasoningContinuation>,
    },
    /// 用户输入或工具结果引用的图片。
    Image {
        /// 可在恢复后安全重建的图片来源。
        source: MessageImageSource,
    },
    /// 模型请求执行的完整工具调用。
    ToolCall {
        /// 当前模型响应内唯一的调用标识。
        tool_call_id: String,
        /// Runtime 注册表中的精确工具名称。
        tool_name: String,
        /// 已完成解析且经过验证的 JSON 对象参数。
        arguments: Value,
    },
    /// 与先前工具调用严格配对的完整结果。
    ToolResult {
        /// 对应工具调用的稳定标识。
        tool_call_id: String,
        /// 按工具返回顺序保存的文本、图片或大结果引用。
        content: Vec<ToolResultPart>,
        /// 工具是否以模型可处理的错误结束。
        is_error: bool,
    },
    /// 大内容的 Artifact 引用。
    Artifact {
        /// 已校验的内容寻址引用。
        artifact: ArtifactUse,
        /// 读取 Artifact 后应恢复成的模型内容类型。
        materialization: ArtifactMaterialization,
    },
}

/// 事件中保存的 Artifact 使用信息。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactUse {
    /// 内容寻址 Artifact 标识。
    pub artifact_id: ArtifactId,
    /// 小写十六进制 SHA-256。
    pub sha256: String,
    /// Artifact 原始字节数。
    pub size_bytes: u64,
    /// 可选标准媒体类型。
    pub media_type: Option<String>,
}

/// 一条权威 Session 消息。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionMessage {
    /// 内部上下文不进入对话界面投影。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_meta: bool,
    /// 用户已选择的资源身份，随权威消息复制、回放和恢复，不建立独立 UI 消息存储。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<keencode_model::InputReference>,
    /// 消息稳定标识。
    pub message_id: String,
    /// 消息所属 Turn；Session 级消息为 `None`。
    pub turn_id: Option<TurnId>,
    /// 发送消息的 Agent；用户或系统消息为 `None`。
    pub agent_id: Option<AgentId>,
    /// 消息语义角色。
    pub role: MessageRole,
    /// 保持生成顺序的内容列表。
    pub content: Vec<MessagePart>,
}

/// 工具请求的权威输入快照。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolRequest {
    /// 工具请求稳定标识。
    pub request_id: RequestId,
    /// 发起请求的 Turn。
    pub turn_id: TurnId,
    /// 发起请求的 Agent。
    pub agent_id: AgentId,
    /// 产生请求的逻辑模型 Round。
    pub model_round: u32,
    /// 当前模型 Round 内的原始工具调用下标；允许因未进入生命周期的调用而存在间隙。
    pub request_index: u32,
    /// Provider 返回且必须与 Transcript 工具块配对的原始调用标识。
    pub model_tool_call_id: String,
    /// 统一工具名称。
    pub tool_name: String,
    /// 调用时完整 JSON 参数。
    pub arguments: Value,
    /// 工具对文件、进程、网络或其他外部状态的影响分类。
    pub effect: ToolEffect,
}

/// 工具调用对外部状态的影响分类。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ToolEffect {
    /// 工具只读取状态且不会产生可观察副作用。
    ReadOnly,
    /// 工具可能改变文件、进程、网络或其他外部状态。
    ChangesState,
}

/// 工具调用的最终结果状态。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ToolCompletionStatus {
    /// 工具实现成功返回。
    Succeeded,
    /// 工具实现或 Runtime 在执行后失败。
    Failed,
    /// 工具未执行或因取消停止。
    Cancelled,
    /// 工具已经越过副作用执行起点，但崩溃恢复无法证明最终结果。
    SideEffectUnknown,
}

/// 可完整恢复到模型消息的工具结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PersistedToolResult {
    /// Provider 原始工具调用标识。
    pub tool_call_id: String,
    /// 按原始顺序保存的文本、图片或 Artifact 内容。
    pub content: Vec<ToolResultPart>,
    /// 结果是否应作为模型可处理错误。
    pub is_error: bool,
}

/// 按原始模型工具调用标识构造唯一、可重放的副作用未知错误结果。
pub fn side_effect_unknown_result(tool_call_id: &str) -> PersistedToolResult {
    PersistedToolResult {
        tool_call_id: tool_call_id.to_owned(),
        content: vec![ToolResultPart::Text {
            text: SIDE_EFFECT_UNKNOWN_RESULT_TEXT.to_owned(),
        }],
        is_error: true,
    }
}

/// 工具调用的唯一终态与完整模型可见结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolOutcome {
    /// 工具成功、失败或取消的明确分类。
    pub status: ToolCompletionStatus,
    /// 可在崩溃恢复后原样重建的完整结果。
    pub result: PersistedToolResult,
}

/// 一次已执行文件工具的前后原始字节快照及其应用状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolFileChange {
    /// 发生变更的跨平台绝对文件路径。
    pub path: String,
    /// 变更前的文件快照；`None` 明确表示原文件不存在。
    pub before: Option<FileSnapshot>,
    /// 变更前的 Windows `FILE_ATTRIBUTE_READONLY` 状态；其他平台为 `None`。
    pub before_readonly: Option<bool>,
    /// 变更后的文件快照。
    pub after: FileSnapshot,
    /// 变更后预期的 Windows `FILE_ATTRIBUTE_READONLY` 状态；其他平台为 `None`。
    pub after_readonly: Option<bool>,
    /// 文件变更是否已经实际应用到工作区。
    pub applied: bool,
}

/// 工具请求在归约状态中的完整生命周期。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolLifecycle {
    /// 原始请求快照。
    pub request: ToolRequest,
    /// 工具请求形成权威记录时的 Unix Epoch 毫秒时间。
    pub requested_at_unix_ms: u64,
    /// 副作用可能已经发生的工具是否已越过执行起点。
    pub execution_started: bool,
    /// 工具越过执行起点时的 Unix Epoch 毫秒时间；未执行时为 `None`。
    pub execution_started_at_unix_ms: Option<u64>,
    /// 工具最终结果；尚未结束时为 `None`。
    pub outcome: Option<ToolOutcome>,
    /// 工具形成唯一终态时的 Unix Epoch 毫秒时间；尚未结束时为 `None`。
    pub completed_at_unix_ms: Option<u64>,
    /// 已准备或应用的文件变更证据；正文只通过快照 Artifact 引用恢复。
    pub file_change: Option<ToolFileChange>,
    /// 已消费当前生命周期的唯一 Transcript 段；尚未物化时为 `None`。
    pub transcript_segment: Option<TranscriptSegmentReference>,
}

/// 一个终端执行在 Session 状态中的权威记录。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TerminalRecord {
    /// 终端执行稳定标识。
    pub terminal_id: TerminalId,
    /// 发起终端的工具请求。
    pub request_id: RequestId,
    /// 脱敏后的命令展示文本。
    pub command_display: String,
    /// 进程工作目录展示文本。
    pub working_directory: String,
    /// 按事件顺序保存的大输出引用。
    pub output_artifacts: Vec<ArtifactUse>,
    /// 退出码；仍在运行或进程未报告退出码时为 `None`。
    pub exit_code: Option<i32>,
    /// 是否因取消或终止信号退出。
    pub cancelled: bool,
    /// 终端是否已经结束；用于区分运行中和无退出码的正常结束。
    pub exited: bool,
}

/// 一次上下文压缩的权威结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompactionRecord {
    /// 本次压缩的稳定触发原因。
    pub trigger: ContextCompressionTrigger,
    /// Runtime 记录的压缩前估算 Token 数，仅用于审计且不由资源层复算。
    pub estimated_tokens_before: u64,
    /// Runtime 记录的压缩后估算 Token 数，仅用于审计且不由资源层复算。
    pub estimated_tokens_after: u64,
    /// 被摘要替换的第一条有效 Transcript 消息下标。
    pub replaced_start_index: usize,
    /// 被摘要替换区间的排他结束下标。
    pub replaced_end_index_exclusive: usize,
    /// 被替换的原始消息数量。
    pub replaced_message_count: usize,
    /// 压缩后仍保留的有效消息数量。
    pub retained_message_count: usize,
    /// 带 Session/Turn/Agent/Round/revision/范围域的实际 SessionMessage 规范 JSON SHA-256。
    pub source_digest_sha256: String,
    /// 压缩后的完整摘要正文。
    pub summary: String,
    /// Micro Compact 对 ToolResult 文本执行的原位投影；摘要压缩形态为空列表。
    pub projections: Vec<ToolResultProjection>,
    /// 提交前要求仍保持的 Transcript revision。
    pub expected_transcript_revision: u64,
    /// 本次压缩成功后形成的 Transcript revision。
    pub applied_transcript_revision: u64,
}

/// 一次 Micro Compact 对 ToolResult 文本的确定性原位投影。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolResultProjection {
    /// 投影目标消息在有效 Transcript 中的下标。
    pub message_index: usize,
    /// 投影目标消息内容块下标。
    pub block_index: usize,
    /// 投影目标 ToolResult 内容下标。
    pub content_index: usize,
    /// 投影后的完整文本。
    pub projected_text: String,
}

/// 一次已持久化、等待运行时执行的 OnError 观察 Hook 调用。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OnErrorHookInvocation {
    /// 由 Session 与 Turn 身份派生的稳定调用标识。
    pub invocation_id: String,
    /// 失败 Turn 标识。
    pub turn_id: TurnId,
    /// 失败 Turn 的 Agent 标识。
    pub source_agent_id: AgentId,
    /// 已写入权威 TurnStopped 的终态原因。
    pub terminal_reason: TurnStopReason,
    /// Provider 中立且稳定的错误分类。
    pub error_category: String,
    /// 经过脱敏和有界截断的错误说明。
    pub error_message: String,
}

/// 触发上下文压缩的稳定原因。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ContextCompressionTrigger {
    /// 模型请求前的预算估算触发压缩。
    Budget,
    /// Provider 明确报告上下文超限后触发唯一恢复压缩。
    ProviderOverflow,
}

/// 一段按单个 JSONL 事件原子提交的 Transcript 消息。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TranscriptSegment {
    /// 当前段所属 Turn。
    pub turn_id: TurnId,
    /// 产生当前段的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 当前段所属逻辑模型 Round。
    pub model_round: u32,
    /// 同一模型 Round 内从零开始的段序号。
    pub segment_index: u32,
    /// 提交前要求仍保持的 Transcript revision。
    pub expected_transcript_revision: u64,
    /// 按模型生成与 Hook 注入顺序保存的完整消息。
    pub messages: Vec<SessionMessage>,
}

/// 一个已提交 Transcript 段的稳定引用。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TranscriptSegmentReference {
    /// 段所属 Turn。
    pub turn_id: TurnId,
    /// 生成段的 Agent。
    pub source_agent_id: AgentId,
    /// 段所属逻辑模型 Round。
    pub model_round: u32,
    /// 同一模型 Round 内的段序号。
    pub segment_index: u32,
    /// 段提交后形成的全局 Transcript revision。
    pub transcript_revision: u64,
}

/// 一次带完整作用域身份的已应用上下文压缩。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppliedCompaction {
    /// 压缩所属 Turn。
    pub turn_id: TurnId,
    /// 发起压缩的 Agent。
    pub source_agent_id: AgentId,
    /// 压缩关联的逻辑模型 Round。
    pub model_round: u32,
    /// 已验证摘要、范围、Digest 与 revision。
    pub record: CompactionRecord,
}

/// 按事件顺序保存且不重复持有消息正文的 Transcript 变更历史。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "type",
    content = "payload",
    rename_all = "snake_case"
)]
pub enum TranscriptRecord {
    /// 一条不含工具交换的独立消息。
    MessageAdded(SessionMessage),
    /// 一个原子提交的完整模型 Round 段。
    SegmentCommitted(TranscriptSegment),
    /// 一次已验证并应用的上下文压缩。
    CompactionApplied(AppliedCompaction),
}

/// 根 Session 唯一 Todo 列表中的条目状态。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum TodoStatus {
    /// 尚未开始处理。
    Pending,
    /// 当前正在处理；完整列表最多只能有一项。
    InProgress,
    /// 已经完成。
    Completed,
}

/// 根 Session 唯一 Todo 列表中的可展示步骤。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TodoItem {
    /// 完成后展示的祈使式任务内容。
    pub content: String,
    /// 当前任务状态。
    pub status: TodoStatus,
    /// 任务进行中用于界面展示的现在进行时文本。
    pub active_form: String,
}

/// 从权威事件确定性归约得到的 Session Todo 快照。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TodoSnapshot {
    /// 每次实际变化递增的版本号。
    pub revision: u64,
    /// 当前仍需展示和恢复的完整 Todo 列表。
    pub items: Vec<TodoItem>,
}

/// 会话级 Plan 模式状态。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanState {
    /// 是否处于始终只读的 Plan 模式。
    pub enabled: bool,
    /// 只读调研生成的可选方案 Artifact。
    pub plan_artifact: Option<ArtifactUse>,
}

/// Session 后续输入的处理模式。
///
/// 该值属于 Session Journal，前端只能请求切换，不能在本地保留第二份模式状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum FollowupMode {
    /// 忙碌时把后续输入加入持久队列。
    #[default]
    Queue,
    /// 忙碌时把后续输入作为引导项加入队列，等待用户明确发送。
    Guide,
}

/// 可持久化的后续输入类型。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub enum SessionInputKind {
    /// 普通用户文本。
    #[serde(rename = "sendText")]
    SendText,
    /// Goal/任务控制面的用户命令。
    #[serde(rename = "sendGoalCommand")]
    SendGoalCommand,
    /// 请求一次上下文压缩。
    Compact,
}

/// 后续输入在 admission 时采用的交付方式。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum SessionInputDelivery {
    /// 加入队列并按队列策略等待发送。
    Queue,
    /// 加入引导队列，等待显式发送。
    Guide,
}

/// 后续输入当前在持久队列中的调度状态。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum SessionInputDispatch {
    /// 等待被发送。
    Queued,
    /// 已被一个显式发送操作保留。
    Reserved,
    /// 已进入启动流程，尚未完成消费确认。
    Promoting,
}

/// 一条可冷恢复的用户后续输入。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionInputQueueItem {
    /// 由 admission 操作生成且永不复用的队列项标识。
    pub queue_item_id: String,
    /// 产生该项的前端 commandId，用于响应丢失后的幂等核对。
    pub source_command_id: String,
    /// 可选前端 Client 标识，不用于权限判断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// 输入类型。
    pub kind: SessionInputKind,
    /// 用户输入正文；compact 项为空字符串。
    pub text: String,
    /// 已通过边界校验的附件引用。
    #[serde(default)]
    pub attachments: Vec<Value>,
    /// admission 时冻结的模型选择。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<Value>,
    /// admission 时冻结的协作模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// admission 时冻结的 Plan 开关。
    #[serde(default)]
    pub plan_enabled: bool,
    /// 原始请求希望采用的交付方式。
    pub requested_delivery: SessionInputDelivery,
    /// 经过当前 Session 策略实际采用的交付方式。
    pub admitted_delivery: SessionInputDelivery,
    /// Session 内单调递增的 admission 序号。
    pub admission_seq: u64,
    /// 已经发生过的 reserve 尝试数；release 后重试必须生成新的持久操作身份。
    #[serde(default)]
    pub reserve_attempt: u64,
    /// 当前队列调度状态。
    pub dispatch: SessionInputDispatch,
    /// 队列项进入提升流程后绑定的真实 Runtime Turn；用于冷恢复归属校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_turn_id: Option<TurnId>,
    /// admission 的 Unix Epoch 毫秒时间。
    pub admitted_at_unix_ms: u64,
}

/// 一条已成功提升为 Turn 的有界消费收据。
///
/// 队列项从当前列表移除后仍保留最小结果事实，供 commandId 丢失响应后的
/// 冷重试返回真实的去重结果；该列表有硬上限，不能演变成第二份历史。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionInputCompletion {
    /// 已消费队列项的稳定标识。
    pub queue_item_id: String,
    /// 原 admission commandId，亦用于绑定 sendQueuedNow 的重试。
    pub source_command_id: String,
    /// admission 时冻结的稳定输入摘要；不包含连接、时间和调度字段，供跨重连冲突检测。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_input_payload_sha256: Option<String>,
    /// 完成该项的 sendQueuedNow commandId，用于只对同一命令去重。
    pub completion_operation_id: String,
    /// 原队列项实际采用的交付模式。
    pub admitted_delivery: SessionInputDelivery,
    /// 消费确认的 Unix 毫秒时间。
    pub completed_at_unix_ms: u64,
}

/// Session 后续输入队列的权威快照。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionInputQueueState {
    /// 按 admissionSeq 排序的可恢复输入。
    #[serde(default)]
    pub items: Vec<SessionInputQueueItem>,
    /// 已成功提升项的有界收据，仅用于 command 重试去重，不是可展示历史。
    #[serde(default)]
    pub completions: Vec<SessionInputCompletion>,
    /// 是否允许空闲时自动消费队首。
    #[serde(default = "default_true")]
    pub auto_drain: bool,
    /// 队列暂停原因；正常可消费时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    /// 下一个 admission 所需的序号水位。
    #[serde(default = "default_one")]
    pub next_admission_seq: u64,
}

impl Default for SessionInputQueueState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            completions: Vec::new(),
            auto_drain: true,
            pause_reason: None,
            next_admission_seq: 1,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_one() -> u64 {
    1
}

/// Provider Snapshot 使用的三种厂商协议。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ProviderProtocolSnapshot {
    /// Anthropic Messages API。
    AnthropicMessages,
    /// OpenAI Chat Completions API。
    OpenAiChatCompletions,
    /// OpenAI Responses API。
    OpenAiResponses,
}

/// 一次 Turn 实际使用且不含凭据的 Provider 配置快照。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderSnapshot {
    /// 用户配置中的 Provider 标识。
    pub provider_id: String,
    /// 精确模型标识。
    pub model: String,
    /// 当前模型已知的上下文窗口 Token 上限；未知时保持 `None`。
    pub context_window: Option<u64>,
    /// 实际使用的厂商协议。
    pub protocol: ProviderProtocolSnapshot,
    /// 同时绑定传输配置与凭据修订、但不含凭据正文的配置身份摘要。
    pub config_fingerprint: String,
    /// 当前 Session 每个 Agent 模型 Round 使用的推理强度；`None` 表示关闭。
    pub reasoning_effort: Option<ReasoningEffortSnapshot>,
}

/// Session 持久格式使用的 Provider 中立推理强度。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ReasoningEffortSnapshot {
    /// 最小推理强度。
    Minimal,
    /// 较低推理强度。
    Low,
    /// 中等推理强度。
    Medium,
    /// 较高推理强度。
    High,
    /// 极高推理强度，对外稳定编码为 `xhigh`。
    #[serde(rename = "xhigh")]
    ExtraHigh,
    /// Provider 最大推理强度，对外稳定编码为 `max`。
    #[serde(rename = "max")]
    Maximum,
}

/// 一次标题生成操作的持久结果，用于在响应丢失后避免重复模型计费。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GeneratedTitleRecord {
    /// 前端在同一逻辑请求重试时复用的稳定操作标识。
    pub operation_id: String,
    /// 标题输入的规范 SHA-256，小写十六进制且不保存用户正文。
    pub input_sha256: String,
    /// 已成功生成且去除首尾空白的标题。
    pub title: String,
}

/// 一条命令收据在 Journal 中的生命周期状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum CommandReceiptStatus {
    /// 已原子登记命令，但副作用尚未产生可确认终态。
    Admitted,
    /// 副作用及其 RPC ACK 已被确认并持久化。
    Completed {
        /// 可在响应丢失后原样重放的 ACK。
        ack: Value,
    },
    /// 命令被确定拒绝，重试只能重放该拒绝结果。
    Rejected {
        /// 可在响应丢失后原样重放的拒绝 ACK。
        ack: Value,
    },
    /// 副作用可能已经发生，但没有可证明的终态；禁止自动重放。
    Unknown {
        /// 要求上层查询或人工处理的稳定原因码。
        reason_code: String,
    },
}

impl CommandReceiptStatus {
    /// 判断该状态是否已经离开可执行的 admission 阶段。
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Admitted)
    }
}

// serde_json::Value 只表示 RFC 8259 JSON（不包含 NaN/Infinity），因此其
// PartialEq 在收据已通过 is_valid 校验后满足 Eq 的语义要求。
impl Eq for CommandReceiptStatus {}

/// 可跨连接、进程和冷恢复复用的命令收据。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandReceipt {
    /// 收据契约版本。
    pub schema: String,
    /// 连接外稳定作用域；同一 commandId 在不同 Session/Workspace 不冲突。
    pub scope: String,
    /// Renderer 生成的稳定命令身份。
    pub command_id: String,
    /// 业务命令类型。
    pub command_type: String,
    /// 规范化命令 payload 的小写 SHA-256。
    pub payload_sha256: String,
    /// 当前收据状态。
    pub status: CommandReceiptStatus,
}

// 收据 ACK 经过 JSON 有界校验后是确定性的 JSON 值，可作为 SessionEvent 的 Eq 成员。
impl Eq for CommandReceipt {}

impl CommandReceipt {
    /// 返回 Journal 状态表使用的稳定键。
    pub fn storage_key(&self) -> String {
        format!("{}\0{}", self.scope, self.command_id)
    }

    /// 判断两个收据是否绑定相同命令意图，不比较生命周期状态。
    pub fn same_identity(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.scope == other.scope
            && self.command_id == other.command_id
            && self.command_type == other.command_type
            && self.payload_sha256 == other.payload_sha256
    }

    /// 检查收据是否满足资源层有界契约。
    pub fn is_valid(&self) -> bool {
        self.schema == COMMAND_RECEIPT_SCHEMA
            && valid_receipt_text(&self.scope, 4096)
            && valid_receipt_text(&self.command_id, 128)
            && valid_receipt_text(&self.command_type, 128)
            && valid_receipt_sha256(&self.payload_sha256)
            && match &self.status {
                CommandReceiptStatus::Admitted => true,
                CommandReceiptStatus::Completed { ack }
                | CommandReceiptStatus::Rejected { ack } => serde_json::to_vec(ack)
                    .is_ok_and(|bytes| bytes.len() <= MAX_COMMAND_RECEIPT_BYTES),
                CommandReceiptStatus::Unknown { reason_code } => {
                    valid_receipt_text(reason_code, 128)
                }
            }
    }
}

fn valid_receipt_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_receipt_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// 子 Agent 生命周期状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum SubAgentStatus {
    /// 子 Agent 已创建但尚未运行。
    Pending,
    /// 子 Agent 正在运行。
    Running,
    /// 子 Agent 等待输入。
    Waiting,
    /// 子 Agent 正常完成。
    Completed,
    /// 子 Agent 执行失败。
    Failed,
    /// 最近 Turn 被取消或中断，Agent 身份仍可接收后续任务。
    Interrupted,
    /// 子 Agent 已停止。
    Stopped,
}

/// 单层子 Agent 的权威状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubAgentState {
    /// 子 Agent 稳定标识。
    pub agent_id: AgentId,
    /// 父 Agent 稳定标识。
    pub parent_agent_id: AgentId,
    /// 在根树内稳定且只允许一层的 `/root/<name>` 路径。
    pub agent_path: String,
    /// 分派任务正文。
    pub task: String,
    /// 当前生命周期状态。
    pub status: SubAgentStatus,
    /// 当前或最近一次 Turn；尚未启动的 Pending Agent 为 `None`。
    pub current_turn_id: Option<TurnId>,
    /// 完成时可缺省、失败时必填的安全摘要。
    pub result_summary: Option<String>,
}

/// 子 Agent 邮箱消息的投递状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MailboxState {
    /// 消息已排队等待接收方读取。
    Queued,
    /// 消息已由接收方确认读取。
    Delivered,
}

/// 主 Agent 与单层子 Agent 之间的一条权威邮箱消息。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MailboxMessage {
    /// 邮箱消息稳定标识。
    pub message_id: MailboxMessageId,
    /// 发送方 Agent。
    pub from: AgentId,
    /// 接收方 Agent。
    pub to: AgentId,
    /// 产生该消息且来源 Agent 与之绑定的权威 Turn。
    pub related_turn_id: TurnId,
    /// 小型消息正文。
    pub body: String,
    /// 可选大消息 Artifact。
    pub artifact: Option<ArtifactUse>,
    /// 当前投递状态。
    pub state: MailboxState,
}

/// 采样前动态输入的权威来源类别。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum DynamicInputKind {
    /// 来自 Agent 间持久 mailbox 的输入。
    Mailbox,
    /// 来自用户在当前 Turn 中追加的 Steer 输入。
    UserSteer,
}

/// 已写入 Transcript 且等待外部 Coordinator 确认的动态输入消费回执。
/// 追加原文只用于展示；模型上下文仍由同批 Transcript 信封承担。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DynamicUserInput {
    /// 所属 Coordinator 队列中的稳定序号。
    pub sequence: u64,
    /// 用户输入的原始正文，不含内部协议说明。
    pub text: String,
    /// 原输入框显式选择的准确资源身份。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<keencode_model::InputReference>,
}

/// 已写入 Transcript 且等待外部 Coordinator 确认的动态输入消费回执。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DynamicInputReceipt {
    /// 动态输入所属 Turn。
    pub turn_id: TurnId,
    /// 实际消费动态输入的 Agent。
    pub source_agent_id: AgentId,
    /// 动态输入所属模型 Round。
    pub model_round: u32,
    /// 动态输入对应的 Transcript 段序号。
    pub segment_index: u32,
    /// 动态输入的权威来源类别。
    pub kind: DynamicInputKind,
    /// 本批实际写入的最大单调序号。
    pub through_sequence: u64,
    /// 已消费的用户原文和引用；不会作为第二条模型消息再次采样。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_inputs: Vec<DynamicUserInput>,
    /// 关联 Transcript 段应用后的全局 revision。
    pub transcript_revision: u64,
}

/// 子 Agent 使用的工作树绑定。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorktreeRecord {
    /// 使用该工作树的 Agent。
    pub agent_id: AgentId,
    /// 工作树绝对路径展示文本。
    pub path: String,
    /// 对应 Git 分支名称。
    pub branch: String,
    /// 工作树是否已释放。
    pub released: bool,
}

/// 一个 Turn 的归约状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TurnState {
    /// Turn 稳定标识。
    pub turn_id: TurnId,
    /// 执行当前 Turn 的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 当前任务链最初的根 Turn；根 Agent 用户 Turn 与自身标识相同。
    pub root_turn_id: TurnId,
    /// 触发当前 Turn 的直接父 Turn；根 Agent 用户 Turn 为 `None`。
    pub parent_turn_id: Option<TurnId>,
    /// 发起 Turn 的用户输入摘要。
    pub prompt_summary: String,
    /// Turn 起点形成权威记录时的 Unix Epoch 毫秒时间。
    pub started_at_unix_ms: u64,
    /// Turn 形成唯一终态时的 Unix Epoch 毫秒时间；运行中为 `None`。
    pub completed_at_unix_ms: Option<u64>,
    /// 当前生命周期状态。
    pub status: TurnStatus,
    /// 非正常停止时的精确原因；Running 和 Completed Turn 必须为 `None`。
    pub stop_reason: Option<TurnStopReason>,
    /// 失败或取消时的安全说明。
    pub outcome_message: Option<String>,
}

/// 一个 Provider 模型 Round 的可恢复元数据与 Token 用量。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelRoundState {
    /// Round 所属 Turn。
    pub turn_id: TurnId,
    /// 执行 Round 的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 当前 Turn 内从一开始严格递增的模型 Round 序号。
    pub model_round: u32,
    /// 请求发往 Provider 抽象层时使用的模型标识。
    pub requested_model: String,
    /// Provider 返回的响应标识与实际模型；缺失字段保持 `None`。
    pub metadata: ResponseMetadata,
    /// Provider 明确报告的可空 Token 用量；未知值不得写成零。
    pub usage: TokenUsage,
    /// Provider 中立的响应结束原因。
    pub stop_reason: StopReason,
    /// 完整模型响应形成权威记录时的 Unix Epoch 毫秒时间。
    pub completed_at_unix_ms: u64,
}

/// 会话标题的写入来源，用于约束自动标题不覆盖手动标题。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// 未记录来源（历史事件缺省）；按可被显式来源覆盖处理。
    #[default]
    Unspecified,
    /// 用户手动改名。
    Manual,
    /// 模型生成的自动标题。
    Automatic,
    /// 首条用户消息前缀派生。
    MessagePrefix,
}

/// 工作流宿主提交到父会话的有序事实；产物引用显式列出以参与冷恢复和归属校验。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkflowJournalEvent {
    /// 同一个父会话内唯一的工作流运行身份。
    pub run_id: String,
    /// 原始启动工具或显式界面启动的稳定关联身份。
    pub tool_call_id: String,
    /// 从 1 开始的运行内序号，独立于 Session Journal 的物理序号。
    pub sequence: u64,
    /// 由工作流领域定义的事件类型；界面只接收对应的展示投影。
    pub event_type: String,
    /// 已冻结定义、输入或节点执行结果；不得携带供应商凭据。
    pub payload: serde_json::Value,
    /// 当前事件引用的内容寻址产物，不能仅把摘要藏在 JSON 正文中。
    pub artifacts: Vec<ArtifactUse>,
    /// 可选的单层执行者会话身份。
    pub actor_session_id: Option<String>,
    /// 与问答或节点启动输入对应的稳定身份。
    pub launch_input_id: Option<String>,
}

/// 事件记录中 `type` 与 `payload` 对应的类型化权威事件。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "type",
    content = "payload",
    rename_all = "snake_case"
)]
pub enum SessionEvent {
    /// 创建全新 Session。
    SessionCreated {
        /// 用户可见标题。
        title: String,
        /// 项目根目录展示文本。
        project_root: String,
    },
    /// 更新已创建且尚未关闭的 Session 用户可见标题。
    SessionRenamed {
        /// 去除首尾空白后必须非空的新标题。
        title: String,
        /// 本次标题写入来源；历史事件缺省为未记录。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<TitleSource>,
    },
    /// 在会话静止且持久变更事务保护下切换执行目录；不改变存储位置和历史身份。
    SessionWorkspaceChanged {
        /// 乐观校验的旧执行目录，防止并发操作覆盖其他切换。
        expected_project_root: String,
        /// 宿主已经授权并规范化的新执行目录。
        project_root: String,
    },
    /// 更新用户会话偏好（置顶/归档）。
    SessionPreferenceSet {
        /// 新的置顶标记；`None` 表示保持不变。
        pinned: Option<bool>,
        /// 新的归档标记；`None` 表示保持不变。
        archived: Option<bool>,
    },
    /// 设置或取消一个 Assistant 展示行的本地反馈。
    ///
    /// 反馈只写入 Session Journal 的投影元数据，不会复制到 Transcript 或 Provider
    /// 请求；`None` 表示删除既有反应，确保取消反应在冷恢复后仍然生效。
    AssistantFeedbackSet {
        /// V4 行标识，必须和 entity_id 共同定位同一条 Assistant 行。
        row_id: u64,
        /// V4 持久实体标识。
        entity_id: String,
        /// 新反馈；空值代表取消反应。
        feedback: Option<AssistantFeedback>,
    },
    /// 更新 Session 生命周期状态。
    SessionStatusChanged {
        /// 新状态。
        status: SessionStatus,
    },
    /// 开始一个 Turn。
    TurnStarted {
        /// Turn 标识。
        turn_id: TurnId,
        /// 执行当前 Turn 的根 Agent 或单层子 Agent。
        source_agent_id: AgentId,
        /// 当前任务链最初的根 Turn。
        root_turn_id: TurnId,
        /// 触发当前 Turn 的直接父 Turn。
        parent_turn_id: Option<TurnId>,
        /// 用户输入摘要。
        prompt_summary: String,
    },
    /// 原子记录一个 Turn 实际解析出的 Provider 配置身份。
    ///
    /// 该事件只绑定当前 Turn，不改变 Session 后续默认 Provider；因此子 Agent
    /// 的显式模型覆盖不会污染根 Session 的配置状态。事件必须与 `TurnStarted`
    /// 位于同一原子批次，冷恢复时从 Journal 按 Turn 重建。
    TurnProviderSnapshotRecorded {
        /// Turn 标识。
        turn_id: TurnId,
        /// 执行当前 Turn 的根 Agent 或单层子 Agent。
        source_agent_id: AgentId,
        /// 当前 Turn 实际使用的 Provider、模型、协议和配置身份。
        provider: ProviderSnapshot,
    },
    /// 在一条物理 Journal 记录中原子应用一组不可分割事件。
    AtomicBatch {
        /// 按顺序应用的事件；禁止为空、嵌套批次或再次创建 Session。
        events: Vec<SessionEvent>,
    },
    /// 完成一个 Turn。
    TurnCompleted {
        /// Turn 标识。
        turn_id: TurnId,
    },
    /// 记录 Turn 失败或取消。
    TurnStopped {
        /// Turn 标识。
        turn_id: TurnId,
        /// 非正常停止的精确原因。
        reason: TurnStopReason,
        /// 安全结果说明。
        message: String,
    },
    /// 与非取消 TurnStopped 原子提交、等待执行一次 OnError 观察 Hook。
    OnErrorHookQueued {
        /// 已绑定失败 Turn 身份和错误分类的稳定调用。
        invocation: OnErrorHookInvocation,
    },
    /// 确认一次 OnError 观察 Hook 调用已经执行完毕；观察失败也必须确认。
    OnErrorHookReceiptCommitted {
        /// 待确认调用的稳定标识。
        invocation_id: String,
    },
    /// 追加一条不含工具调用或结果的独立消息。
    MessageAdded {
        /// 完整类型化消息。
        message: SessionMessage,
    },
    /// 原子提交一个逻辑模型 Round 的不可分割 Transcript 段。
    TranscriptSegmentCommitted {
        /// 完整段身份、CAS 水位和消息。
        segment: TranscriptSegment,
    },
    /// 原子记录一批采样前动态输入的权威消费水位。
    DynamicInputReceiptCommitted {
        /// 动态输入所属 Turn。
        turn_id: TurnId,
        /// 实际消费动态输入的 Agent。
        source_agent_id: AgentId,
        /// 动态输入所属模型 Round。
        model_round: u32,
        /// 动态输入对应的 Transcript 段序号。
        segment_index: u32,
        /// 动态输入的权威来源类别。
        kind: DynamicInputKind,
        /// 本批实际写入的最大单调序号。
        through_sequence: u64,
        /// 同批消费的用户原文；mailbox 回执不得携带此字段。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        user_inputs: Vec<DynamicUserInput>,
    },
    /// 原子记录一个完整 Provider 模型 Round 的元数据与用量。
    ModelRoundCompleted {
        /// Round 所属 Turn。
        turn_id: TurnId,
        /// 执行 Round 的根 Agent 或单层子 Agent。
        source_agent_id: AgentId,
        /// 当前 Turn 内从一开始严格递增的模型 Round 序号。
        model_round: u32,
        /// 请求发往 Provider 抽象层时使用的模型标识。
        requested_model: String,
        /// Provider 返回的响应标识与实际模型；缺失字段保持 `None`。
        metadata: ResponseMetadata,
        /// Provider 明确报告的可空 Token 用量；未知值不得写成零。
        usage: TokenUsage,
        /// Provider 中立的响应结束原因。
        stop_reason: StopReason,
    },
    /// 记录一个待执行工具请求。
    ToolRequested {
        /// 完整工具请求。
        request: ToolRequest,
    },
    /// 在调用实际工具实现之前持久化执行起点。
    ToolExecutionStarted {
        /// 已通过计划守卫的工具请求标识。
        request_id: RequestId,
    },
    /// 在实际写入文件前记录前后原始字节快照。
    ToolFileChangePrepared {
        /// 对应已开始执行的副作用工具请求标识。
        request_id: RequestId,
        /// 不含文件正文的变更路径与快照证据。
        change: ToolFileChange,
    },
    /// 记录已准备的文件快照确实应用到工作区。
    ToolFileChangeApplied {
        /// 对应文件变更工具请求标识。
        request_id: RequestId,
    },
    /// 记录工具最终结果。
    ToolCompleted {
        /// 工具请求标识。
        request_id: RequestId,
        /// 最终结果。
        outcome: ToolOutcome,
    },
    /// 在冷恢复期间把已开始但结果未知的副作用工具收敛为禁止自动重试的终态。
    ToolSideEffectUnknown {
        /// 已经越过执行起点的工具请求标识。
        request_id: RequestId,
        /// 返回给后续模型的明确错误结果。
        result: PersistedToolResult,
    },
    /// 记录终端进程已启动。
    TerminalStarted {
        /// 完整终端初始记录。
        terminal: TerminalRecord,
    },
    /// 为终端追加一个大输出 Artifact。
    TerminalOutputRecorded {
        /// 终端标识。
        terminal_id: TerminalId,
        /// 输出 Artifact。
        artifact: ArtifactUse,
    },
    /// 记录终端退出。
    TerminalExited {
        /// 终端标识。
        terminal_id: TerminalId,
        /// 退出码。
        exit_code: Option<i32>,
        /// 是否由取消导致。
        cancelled: bool,
    },
    /// 应用一次上下文压缩结果。
    CompactionApplied {
        /// 压缩所属 Turn。
        turn_id: TurnId,
        /// 发起压缩的 Agent。
        source_agent_id: AgentId,
        /// 压缩关联的逻辑模型 Round。
        model_round: u32,
        /// 压缩记录。
        compaction: CompactionRecord,
    },
    /// 原子替换会话 Todo 列表。
    TodoReplaced {
        /// 规范化后实际保存的新完整 Todo 列表；全部完成时为空。
        items: Vec<TodoItem>,
        /// 规范化提交载荷的 SHA-256；即使完成项收起为空，也用于区分不同幂等请求。
        operation_payload_sha256: String,
        /// 归约该事件后形成的 Todo revision；无变化事件保持原 revision。
        revision: u64,
    },
    /// 更新只读 Plan 状态。
    PlanChanged {
        /// 新 Plan 状态。
        plan: PlanState,
    },
    /// 记录后续输入模式的持久切换。
    FollowupModeChanged {
        /// 绑定本次命令的稳定 operationId；控制层使用同一域做幂等去重。
        operation_id: String,
        /// 命令参数摘要，用于拒绝 operationId 绑定不同正文。
        operation_payload_sha256: String,
        /// 新的后续输入处理模式。
        mode: FollowupMode,
    },
    /// 原子替换后续输入队列，覆盖入队、编辑、排序、删除和消费状态变化。
    InputQueueChanged {
        /// 绑定本次命令的稳定 operationId；不能被不同正文复用。
        operation_id: String,
        /// 命令参数摘要，用于拒绝 operationId 绑定不同正文。
        operation_payload_sha256: String,
        /// 变更后的完整队列快照，便于冷恢复而不依赖进程内缓存。
        queue: SessionInputQueueState,
    },
    /// 保存当前实际使用的 Provider 快照。
    ProviderSnapshotUpdated {
        /// 不含凭据的配置快照。
        provider: ProviderSnapshot,
    },
    /// 保存一次已成功完成的独立标题生成结果。
    TitleGenerated {
        /// 可按 operationId 跨重启复用的完整结果。
        result: GeneratedTitleRecord,
    },
    /// 原子登记或完成一条跨连接命令收据。
    CommandReceiptCommitted {
        /// 绑定命令意图和 ACK 的完整持久记录。
        receipt: CommandReceipt,
    },
    /// 工作流事件先获得 Journal 确认，再向订阅者发布。
    WorkflowEventCommitted {
        /// 有序、可恢复且归属于当前 Session 的工作流事实。
        record: WorkflowJournalEvent,
    },
    /// 创建一个单层子 Agent。
    SubAgentSpawned {
        /// 子 Agent 初始状态。
        agent: SubAgentState,
    },
    /// 更新子 Agent 生命周期状态。
    SubAgentStatusChanged {
        /// 子 Agent 标识。
        agent_id: AgentId,
        /// 目标状态绑定的当前或最近 Turn；未启动即停止时为 `None`。
        turn_id: Option<TurnId>,
        /// 新状态。
        status: SubAgentStatus,
        /// 完成时可缺省、失败时必填的结果摘要。
        result_summary: Option<String>,
    },
    /// 向 Agent 邮箱加入消息。
    MailboxMessageQueued {
        /// 完整邮箱消息。
        message: MailboxMessage,
    },
    /// 确认一条邮箱消息已投递。
    MailboxMessageDelivered {
        /// 邮箱消息标识。
        message_id: MailboxMessageId,
    },
    /// 为 Agent 绑定工作树。
    WorktreeAssigned {
        /// 工作树绑定。
        worktree: WorktreeRecord,
    },
    /// 释放 Agent 工作树。
    WorktreeReleased {
        /// Agent 标识。
        agent_id: AgentId,
    },
    /// 关闭当前 Session；空对象 payload 保持统一事件 envelope。
    SessionClosed {},
}

/// JSONL 中一行完整且自描述的 Session 事件。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionEventRecord {
    /// 固定 schema 名称。
    pub schema: String,
    /// 固定 schema 版本。
    pub version: u32,
    /// 跨重启保持稳定的幂等提交标识。
    pub event_id: SessionEventId,
    /// 事件所属 Session。
    pub session: SessionId,
    /// 从 1 开始严格递增的 sequence。
    pub sequence: u64,
    /// Unix Epoch 毫秒时间。
    pub time_unix_ms: u64,
    /// 类型化事件，序列化为顶层 `type` 与 `payload`。
    #[serde(flatten)]
    pub event: SessionEvent,
}

impl SessionEventRecord {
    /// 创建使用当前 schema/version 的事件记录。
    pub(crate) fn new(
        event_id: SessionEventId,
        session: SessionId,
        sequence: u64,
        time_unix_ms: u64,
        event: SessionEvent,
    ) -> Self {
        Self {
            schema: SESSION_EVENT_SCHEMA.to_owned(),
            version: SESSION_EVENT_VERSION,
            event_id,
            session,
            sequence,
            time_unix_ms,
            event,
        }
    }
}

/// 从事件日志确定性归约得到的完整 Session 权威状态。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionState {
    /// Session 稳定标识。
    pub session_id: SessionId,
    /// 是否已应用 SessionCreated。
    pub created: bool,
    /// 用户可见标题。
    pub title: String,
    /// 项目根目录展示文本。
    pub project_root: String,
    /// 用户置顶标记；由 `SessionPreferenceSet` 维护的权威状态。
    #[serde(default)]
    pub pinned: bool,
    /// 用户归档标记；由 `SessionPreferenceSet` 维护的权威状态。
    #[serde(default)]
    pub archived: bool,
    /// 按 V4 row/entity 身份保存的 Assistant 反馈；旧快照缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assistant_feedback: Vec<AssistantFeedbackRecord>,
    /// 当前标题的写入来源；自动标题不得覆盖手动标题。
    #[serde(default)]
    pub title_source: TitleSource,
    /// 当前生命周期状态。
    pub status: SessionStatus,
    /// 已应用的最后一个 sequence。
    pub last_sequence: u64,
    /// `SessionCreated` 物理事件记录携带的 Unix Epoch 毫秒时间。
    pub created_at_unix_ms: u64,
    /// 最近一条成功应用的顶层物理事件记录携带的 Unix Epoch 毫秒时间。
    pub updated_at_unix_ms: u64,
    /// 每次消息段、单条消息或压缩成功提交后递增的 Transcript revision。
    pub transcript_revision: u64,
    /// 按标识保存的全部 Turn。
    pub turns: BTreeMap<TurnId, TurnState>,
    /// 按事件顺序保存的消息、段和压缩，正文在状态中只保留一份。
    pub transcript: Vec<TranscriptRecord>,
    /// 已写入动态输入段但尚未由 Coordinator 确认消费的可恢复回执历史。
    pub dynamic_input_receipts: Vec<DynamicInputReceipt>,
    /// 已与失败 Turn 终态原子提交、尚未完成观察 Hook 的持久 outbox。
    pub on_error_hook_outbox: Vec<OnErrorHookInvocation>,
    /// 按权威提交顺序保存的完整模型 Round 元数据与用量。
    pub model_rounds: Vec<ModelRoundState>,
    /// 按请求标识保存的工具生命周期。
    pub tools: BTreeMap<RequestId, ToolLifecycle>,
    /// 按终端标识保存的终端生命周期。
    pub terminals: BTreeMap<TerminalId, TerminalRecord>,
    /// 当前根 Session 唯一权威 Todo 快照。
    pub todos: TodoSnapshot,
    /// 当前 Plan 模式状态。
    pub plan: PlanState,
    /// 当前后续输入处理模式；由 Journal 事件恢复。
    #[serde(default)]
    pub followup_mode: FollowupMode,
    /// 当前后续输入队列；由 Journal 事件恢复。
    #[serde(default)]
    pub input_queue: SessionInputQueueState,
    /// 当前 Provider 配置快照。
    pub provider: Option<ProviderSnapshot>,
    /// 按 operationId 保存的标题生成结果缓存。
    pub generated_titles: BTreeMap<String, GeneratedTitleRecord>,
    /// 按作用域和 commandId 保存的跨重启命令收据。
    #[serde(default)]
    pub command_receipts: BTreeMap<String, CommandReceipt>,
    /// 工作流恢复与展示的唯一事实源；每次运行内按序保存确认事件。
    #[serde(default)]
    pub workflow_events: BTreeMap<String, Vec<WorkflowJournalEvent>>,
    /// 单层子 Agent 状态。
    pub sub_agents: BTreeMap<AgentId, SubAgentState>,
    /// 尚在状态历史中的邮箱消息。
    pub mailbox: BTreeMap<MailboxMessageId, MailboxMessage>,
    /// Agent 与工作树的绑定。
    pub worktrees: BTreeMap<AgentId, WorktreeRecord>,
}

impl SessionState {
    /// 创建尚未应用任何事件的空白归约状态。
    pub fn empty(session_id: SessionId) -> Self {
        Self {
            session_id,
            created: false,
            title: String::new(),
            project_root: String::new(),
            pinned: false,
            archived: false,
            assistant_feedback: Vec::new(),
            title_source: TitleSource::default(),
            status: SessionStatus::Idle,
            last_sequence: 0,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            transcript_revision: 0,
            turns: BTreeMap::new(),
            transcript: Vec::new(),
            dynamic_input_receipts: Vec::new(),
            on_error_hook_outbox: Vec::new(),
            model_rounds: Vec::new(),
            tools: BTreeMap::new(),
            terminals: BTreeMap::new(),
            todos: TodoSnapshot::default(),
            plan: PlanState::default(),
            followup_mode: FollowupMode::default(),
            input_queue: SessionInputQueueState::default(),
            provider: None,
            generated_titles: BTreeMap::new(),
            command_receipts: BTreeMap::new(),
            workflow_events: BTreeMap::new(),
            sub_agents: BTreeMap::new(),
            mailbox: BTreeMap::new(),
            worktrees: BTreeMap::new(),
        }
    }

    /// 判断标识是否属于固定根 Agent 或当前已注册的单层子 Agent。
    pub(crate) fn is_registered_agent(&self, agent_id: &AgentId) -> bool {
        agent_id.as_str() == ROOT_AGENT_ID || self.sub_agents.contains_key(agent_id)
    }
}
