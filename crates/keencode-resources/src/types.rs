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
pub const SESSION_EVENT_VERSION: u32 = 7;
/// Session 内唯一根 Agent 使用的固定标识。
pub const ROOT_AGENT_ID: &str = "root";
/// 冷恢复时返回给模型的唯一副作用未知错误文本。
pub const SIDE_EFFECT_UNKNOWN_RESULT_TEXT: &str =
    "工具在崩溃前已经开始，副作用状态未知，禁止自动重试";

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
    /// 变更后的文件快照。
    pub after: FileSnapshot,
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
    /// 提交前要求仍保持的 Transcript revision。
    pub expected_transcript_revision: u64,
    /// 本次压缩成功后形成的 Transcript revision。
    pub applied_transcript_revision: u64,
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
    /// 移除凭据后的配置摘要。
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
    /// 当前 Provider 配置快照。
    pub provider: Option<ProviderSnapshot>,
    /// 按 operationId 保存的标题生成结果缓存。
    pub generated_titles: BTreeMap<String, GeneratedTitleRecord>,
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
            status: SessionStatus::Idle,
            last_sequence: 0,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            transcript_revision: 0,
            turns: BTreeMap::new(),
            transcript: Vec::new(),
            dynamic_input_receipts: Vec::new(),
            model_rounds: Vec::new(),
            tools: BTreeMap::new(),
            terminals: BTreeMap::new(),
            todos: TodoSnapshot::default(),
            plan: PlanState::default(),
            provider: None,
            generated_titles: BTreeMap::new(),
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
