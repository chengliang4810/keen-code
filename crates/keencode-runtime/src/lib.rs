//! KeenCode 进程内 Session Runtime、崩溃恢复与 Agent 权威提交组合层。
//!
//! 本 crate 只组合全新 Session 格式，不读取旧协议或旧会话。公开边界负责独占所有权、
//! Provider 中立 Agent Loop 装配、Turn 生命周期、可靠提交和冷恢复。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod file_changes;
mod manager;
mod persistent_state;
mod publisher;

#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod persistent_state_tests;

pub use manager::RuntimeManager;
pub use persistent_state::PersistentAgentState;
pub use publisher::{
    RuntimeCatchUpDirective, RuntimeControlEvent, RuntimeEventDelivery, RuntimeEventLag,
    RuntimeEventPayload, RuntimeEventReceiveError, RuntimeEventSubscription,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use keencode_agent::{
    AgentCommitEvent, AgentCommitEventKind, AgentCommitSink, AgentCommitSinkError,
    AgentDynamicInputKind, AgentRunner, AgentToolRoundPreflight, AgentToolRoundPreflightError,
    AgentToolRoundReservation, ContextCompressionTrigger as AgentCompactionTrigger,
    ModelRoundCompletion, ModelRoundUsage, TOOL_OUTPUT_LIMITS, TerminalReason,
    ToolCompletionStatus as AgentToolCompletionStatus, ToolEffect as AgentToolEffect,
    TurnCancellation, TurnRequest, TurnResult, is_canonical_image_media_type,
    is_canonical_remote_image_url,
};
use keencode_model::{
    ContentBlock, ImageContent, ImageSource, Message, MessageRole as ModelMessageRole,
    ModelResponse, OpaqueReasoningState, ReasoningContent, ToolCall, ToolResult, ToolResultContent,
};
use keencode_resources::{
    ArtifactId, ArtifactLimits, ArtifactMaterialization, ArtifactRef, ArtifactStore, ArtifactUse,
    CompactionRecord, ContextCompressionTrigger, DynamicInputKind, GeneratedTitleRecord,
    IdempotentAppendOutcome, JournalConfig, MAX_REPLAY_PAGE_RECORDS, MailboxMessage,
    MailboxMessageId, MessageImageSource, MessagePart, MessageRole, PersistedToolResult, PlanState,
    ProviderSnapshot, ReadOnlySessionReport, ReplayPage, RequestId, ResourceError,
    SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION, SessionEvent, SessionEventId, SessionEventRecord,
    SessionId, SessionJournal, SessionLease, SessionLeaseAcquire, SessionMessage, SessionOpen,
    SessionState, SessionStatus, SubAgentState, SubAgentStatus, ToolCompletionStatus, ToolEffect,
    ToolOutcome, ToolResultPart, TranscriptSegment, TurnId, TurnStatus, TurnStopReason,
    reduce_record, side_effect_unknown_result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use publisher::{RuntimeEventFanoutSink, SessionEventPublisher};

/// 冷恢复取消未开始或只读工具时写入模型结果的固定说明。
const RECOVERY_CANCELLED_RESULT: &str =
    "工具在 Runtime 异常退出前未形成可证明终态，已取消且禁止自动重试";
/// 冷恢复停止遗留 Running Turn 时写入的固定安全说明。
const RECOVERY_TURN_STOP_MESSAGE: &str = "Runtime 异常退出后已完成保守恢复，原 Turn 已停止";
/// 冷恢复无法从独立工具生命周期证明原请求模型时使用的显式未知标识。
const RECOVERY_UNKNOWN_MODEL: &str = "recovery-unknown-model";
/// 持久化到 Journal 的单个图片 URL 最大 UTF-8 字节数。
const MAX_PERSISTED_IMAGE_URL_BYTES: usize = 16 * 1024;
/// 工具 Round 最终事件分配器生成的固定宽度占位身份。
const PREFLIGHT_EVENT_ID: &str = "agent-event-00000000-0000-0000-0000-000000000000";
/// Runtime 默认允许容纳 Agent 提交出口最坏有界工具事件的单事件大小。
const RUNTIME_DEFAULT_MAX_EVENT_BYTES: u64 = 48 * 1024 * 1024;
/// Provider 内容块映射为带双摘要 Artifact 引用时的保守 JSON 膨胀余量。
const PERSISTED_CONTENT_BLOCK_EXPANSION_BYTES: u64 = 1_024;
/// 最终工具 Round 的消息包装、固定摘要指令和未来小字段使用的保守余量。
const TOOL_ROUND_FIXED_WIRE_SLACK_BYTES: u64 = 1024 * 1024;
/// 工具 Round 达到 Step 上限时可能追加的一条固定总结指令消息。
const TOOL_ROUND_OPTIONAL_SUMMARY_MESSAGES: usize = 1;
/// 超限普通文本 Artifact 使用的固定规范媒体类型。
const UTF8_TEXT_MEDIA_TYPE: &str = "text/plain";
/// Runtime Turn 终态和子 Agent 结果摘要允许持久化的最大 UTF-8 字节数。
const MAX_RUNTIME_TERMINAL_MESSAGE_BYTES: usize = 64 * 1024;
/// 每个 Session 默认保留的最近实时事件数量，慢订阅者超出后转向 Journal 追赶。
const DEFAULT_LIVE_EVENT_CAPACITY: usize = 256;
/// 单个 Session 实时缓冲允许配置的硬上限，限制大事件克隆造成的常驻内存。
const MAX_LIVE_EVENT_CAPACITY: usize = 4_096;

/// Session Runtime 的不可变本地存储配置。
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// `sessions/<session_id>` 等全新资源目录的共同根目录。
    pub storage_root: PathBuf,
    /// 权威 JSONL 日志、Snapshot 与容量限制。
    pub journal: JournalConfig,
    /// 内容寻址 Artifact 的大小、数量与预览限制。
    pub artifacts: ArtifactLimits,
    /// 消息或工具结果中允许直接进入 Journal 的单个 UTF-8 文本最大字节数。
    pub max_inline_text_bytes: usize,
    /// 每个 Session 实时 Publisher 为慢订阅者保留的最大事件数量。
    pub live_event_capacity: usize,
}

impl RuntimeConfig {
    /// 使用资源层默认限制创建指定存储根的配置。
    pub fn new(storage_root: impl Into<PathBuf>) -> Self {
        let journal = JournalConfig {
            max_event_bytes: RUNTIME_DEFAULT_MAX_EVENT_BYTES,
            ..JournalConfig::default()
        };
        Self {
            storage_root: storage_root.into(),
            journal,
            artifacts: ArtifactLimits::default(),
            max_inline_text_bytes: 64 * 1024,
            live_event_capacity: DEFAULT_LIVE_EVENT_CAPACITY,
        }
    }

    /// 校验内联文本阈值非零且不会大于单个 Artifact 的保存能力。
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.live_event_capacity == 0 || self.live_event_capacity > MAX_LIVE_EVENT_CAPACITY {
            return Err(RuntimeError::InvalidRuntimeConfig);
        }
        if self.max_inline_text_bytes == 0
            || u64::try_from(self.max_inline_text_bytes).unwrap_or(u64::MAX)
                > self.artifacts.max_artifact_bytes
        {
            return Err(RuntimeError::InvalidRuntimeConfig);
        }
        Ok(())
    }
}

/// 创建全新 Session 所需的稳定元数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSessionRequest {
    /// 可安全映射到单一目录段的 Session 标识。
    pub session_id: String,
    /// 用户可见且非空的 Session 标题。
    pub title: String,
    /// 用户可见且非空的项目根目录文本。
    pub project_root: String,
}

/// 打开现有 Session 后的显式健康边界。
pub enum OpenSessionResult {
    /// 权威日志健康，冷恢复已完整收敛，可以绑定受控 Agent Runner。
    Ready(RuntimeSession),
    /// 权威日志损坏，只返回首个损坏点之前的只读事实。
    Corrupt(Box<ReadOnlySessionReport>),
}

/// Runtime 从内部 Journal 安全复制出的一页类型化权威事件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeReplayPage {
    /// 严格位于请求游标之后、按 Journal sequence 升序排列的事件。
    pub records: Vec<SessionEventRecord>,
    /// 当前页最后一个 Journal sequence；空页没有下一游标。
    pub next_after: Option<u64>,
    /// 本次读取观察到的权威 Journal 末尾 sequence。
    pub through_sequence: u64,
    /// 当前页之后是否仍有不晚于 `through_sequence` 的权威事件。
    pub has_more: bool,
}

impl From<ReplayPage> for RuntimeReplayPage {
    /// 从资源层只读页复制相同的有界重放事实。
    fn from(page: ReplayPage) -> Self {
        Self {
            records: page.records,
            next_after: page.next_after,
            through_sequence: page.through_sequence,
            has_more: page.has_more,
        }
    }
}

/// 对指定 Session 与 Turn 发起幂等取消后的精确结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnCancellationOutcome {
    /// 当前调用首次触发了 Runtime 权威取消令牌。
    Requested,
    /// 相同 Runtime 权威令牌先前已经被触发，本次没有重复副作用。
    AlreadyRequested,
    /// 指定 Turn 当前没有运行中的 Runtime 权威取消令牌。
    NotRunning,
}

/// Runtime 当前可供桌面层读取的一致快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    /// 从完整权威日志归约得到的 Session 状态。
    pub state: SessionState,
    /// 当前共享 Runtime Session 是否已被 Manager 关闭并禁止启动新工作。
    pub closed: bool,
    /// 当前进程是否因不确定提交或硬持久化错误而冻结后续权威工作。
    pub recovery_required: bool,
    /// 尚未释放、消费或转入恢复保留的工具 Round 数量。
    pub active_reservations: usize,
    /// 已连同完整事件保留、等待恢复对账的工具 Round 数量。
    pub retained_reservations: usize,
    /// 首次提交结果不确定、仅允许相同事件身份继续有界对账的事件数量。
    pub pending_indeterminate_events: usize,
}

/// 持久 Session 列表使用且不包含 Transcript 正文的元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSessionMetadata {
    /// Session 稳定标识。
    pub session_id: SessionId,
    /// 当前用户可见标题。
    pub title: String,
    /// Session 创建时绑定的项目根目录。
    pub project_root: String,
    /// 权威日志归约得到的当前状态。
    pub status: SessionStatus,
    /// SessionCreated 事件的 Unix Epoch 毫秒时间。
    pub created_at_unix_ms: u64,
    /// 最近一条有效权威事件的 Unix Epoch 毫秒时间。
    pub updated_at_unix_ms: u64,
    /// 最近一条有效权威事件的 Journal sequence。
    pub last_sequence: u64,
    /// 事件日志是否在首个无效记录处进入只读损坏状态。
    pub corrupt: bool,
}

impl StoredSessionMetadata {
    /// 从健康或损坏日志的最后有效状态创建不含正文的元数据。
    fn from_state(state: &SessionState, corrupt: bool) -> Result<Self, RuntimeError> {
        if !state.created {
            return Err(RuntimeError::SessionNotCreated);
        }
        Ok(Self {
            session_id: state.session_id.clone(),
            title: state.title.clone(),
            project_root: state.project_root.clone(),
            status: state.status.clone(),
            created_at_unix_ms: state.created_at_unix_ms,
            updated_at_unix_ms: state.updated_at_unix_ms,
            last_sequence: state.last_sequence,
            corrupt,
        })
    }
}

/// Runtime 当前仍未形成终态的一个 Session Turn 引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveRuntimeTurn {
    /// Turn 所属 Session。
    pub session_id: SessionId,
    /// 尚未完成的 Turn 标识。
    pub turn_id: TurnId,
}

/// 接收已完成模型 Round 明确用量与真实墙钟耗时的同步持久出口。
///
/// Runtime 在任何响应工具执行前调用该接口；流失败但已经收到明确 Usage 时，也会在
/// 不完整 Transcript 提交前调用。实现必须使用事件携带的稳定 Session、Turn、Agent、
/// Round 与调用尝试身份执行跨重启幂等提交，并且不得把工作转交后台线程。
pub trait RuntimeModelRoundUsageSink: Send + Sync {
    /// 同步提交一次不可变模型 Round 用量事实。
    fn commit(&self, usage: &ModelRoundUsage) -> Result<(), AgentCommitSinkError>;
}

/// 未配置项目 Goal 用量持久化时显式丢弃模型 Round 用量的默认实现。
#[derive(Clone, Copy, Debug, Default)]
struct NoopRuntimeModelRoundUsageSink;

impl RuntimeModelRoundUsageSink for NoopRuntimeModelRoundUsageSink {
    /// 默认 Runtime 不维护项目 Goal，因此立即确认用量事实。
    fn commit(&self, _usage: &ModelRoundUsage) -> Result<(), AgentCommitSinkError> {
        Ok(())
    }
}

/// 交给绑定 Session 的 Agent Runner 执行的一次完整 Turn。
pub struct RuntimeTurnRequest {
    /// Provider 中立模型请求、权限、计划守卫和取消令牌。
    request: TurnRequest,
    /// 本 Turn 新增且必须已位于模型请求末尾的用户、系统或开发者消息。
    input_messages: Vec<Message>,
    /// 当前任务链最初的根 Turn 标识。
    root_turn_id: String,
    /// 触发当前 Turn 的直接父 Turn；根用户 Turn 固定为空。
    parent_turn_id: Option<String>,
    /// 进入权威 Session 状态的非空用户输入摘要。
    prompt_summary: String,
    /// 首次启动子 Agent 时需要与 TurnStarted 原子提交的身份；后续 Turn 固定为空。
    spawned_agent: Option<SubAgentState>,
}

impl RuntimeTurnRequest {
    /// 创建根 Agent 直接处理用户输入的 Turn，并令根谱系指向自身。
    pub fn root(
        request: TurnRequest,
        input_messages: Vec<Message>,
        prompt_summary: impl Into<String>,
    ) -> Self {
        let root_turn_id = request.turn_id().as_str().to_owned();
        Self {
            request,
            input_messages,
            root_turn_id,
            parent_turn_id: None,
            prompt_summary: prompt_summary.into(),
            spawned_agent: None,
        }
    }

    /// 创建由已存在父 Turn 触发的根后续 Turn 或单层子 Agent Turn。
    pub fn child(
        request: TurnRequest,
        input_messages: Vec<Message>,
        root_turn_id: impl Into<String>,
        parent_turn_id: impl Into<String>,
        prompt_summary: impl Into<String>,
    ) -> Self {
        Self {
            request,
            input_messages,
            root_turn_id: root_turn_id.into(),
            parent_turn_id: Some(parent_turn_id.into()),
            prompt_summary: prompt_summary.into(),
            spawned_agent: None,
        }
    }

    /// 创建首次子 Agent Turn，并把 Pending 身份与 Running 起点绑定为同一 Journal 原子批次。
    pub fn initial_child(
        request: TurnRequest,
        input_messages: Vec<Message>,
        root_turn_id: impl Into<String>,
        parent_turn_id: impl Into<String>,
        prompt_summary: impl Into<String>,
        spawned_agent: SubAgentState,
    ) -> Self {
        Self {
            request,
            input_messages,
            root_turn_id: root_turn_id.into(),
            parent_turn_id: Some(parent_turn_id.into()),
            prompt_summary: prompt_summary.into(),
            spawned_agent: Some(spawned_agent),
        }
    }
}

/// 尚未进入执行端的子 Agent Turn 终态。
///
/// Collaboration Store 是该类请求的触发来源，Runtime Journal 仍是 Turn 的权威来源。
/// 该终态只描述已确认的生命周期结果，不会调用模型或工具。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum UnstartedTurnTermination {
    /// Turn 在获得执行容量并启动前被取消，沿用固定的 Interrupted 说明。
    Interrupted,
    /// Turn 在获得执行容量并启动前失败，并保存调用方提供的稳定安全说明。
    Failed {
        /// 失败 Turn 的稳定安全说明。
        message: String,
    },
}

/// 由 Collaboration 对账转换生成的尚未启动 Runtime Turn 请求。
///
/// Collaboration Store 是该转换的触发来源，Runtime Journal 仍是 Turn 的权威来源。
/// 调用方必须只在同一批次已经确认对应的未启动终态后提交本请求；`initial_task`
/// 只决定 Journal 尚无该 Agent 时是否把 Pending 身份放入同一原子批次。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnstartedTurnTerminationRequest {
    /// 已终止的单层子 Agent 身份与不可变任务定义；其生命周期字段会被 Runtime 重建。
    pub agent: SubAgentState,
    /// 已由 Collaboration 确认终态、但尚未进入执行端的 Turn 标识。
    pub turn_id: TurnId,
    /// 当前任务链最初的根 Turn 标识。
    pub root_turn_id: TurnId,
    /// 触发该子 Turn 的直接父 Turn 标识。
    pub parent_turn_id: TurnId,
    /// 与正常 Runtime 起点相同的有界输入摘要。
    pub prompt_summary: String,
    /// 当前 Turn 是否为该 Agent 的首次 `InitialTask`；只允许首次缺失 Agent 时创建身份。
    pub initial_task: bool,
    /// 尚未启动 Turn 已确认的 Interrupted 或 Failed 终态。
    pub termination: UnstartedTurnTermination,
}

/// 尚未启动 Turn 终态批次写入 Runtime Journal 后的幂等结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnstartedTurnTerminationOutcome {
    /// 本次首次追加了完整原子生命周期批次。
    Committed,
    /// 相同批次标识和正文已经由本次或先前 Runtime 提交。
    AlreadyCommitted,
}

/// 已强制绑定单一 Session 提交出口的 Provider 中立 Agent Runner。
pub struct RuntimeAgentRunner {
    /// 与运行期间全部权威提交共享同一 lease 和 Journal 的 Session 内核。
    inner: Arc<RuntimeSessionInner>,
    /// 负责模型流、上下文压缩、计划守卫、工具循环和终态归一的 Agent Runner。
    runner: AgentRunner,
}

/// Session Runtime 组合、打开和冷恢复失败。
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// 底层资源路径、日志、Artifact 或文件锁操作失败。
    #[error(transparent)]
    Resource(#[from] ResourceError),
    /// 另一个进程或句柄已经持有目标 Session 的独占 Runtime lease。
    #[error("Session Runtime 正被另一个进程或句柄占用")]
    SessionBusy,
    /// create_session 指向已经完成 SessionCreated 的现有 Session。
    #[error("Session 已存在，不能再次创建")]
    SessionAlreadyExists,
    /// 同一 RuntimeManager 已经登记目标 Session，拒绝进程内重复所有权。
    #[error("Session 已在当前 RuntimeManager 中注册")]
    SessionAlreadyRegistered,
    /// 指定 Session 没有登记在当前 RuntimeManager，不能执行跨 Session 控制操作。
    #[error("Session 未在当前 RuntimeManager 中注册")]
    SessionNotRegistered,
    /// 永久删除只接受没有登记在当前 RuntimeManager 的 Session。
    #[error("Session 仍由当前 RuntimeManager 打开，不能永久删除")]
    SessionOpenForDeletion,
    /// Session 控制操作缺少可跨响应丢失重试的受信请求标识。
    #[error("Session 控制操作标识无效")]
    InvalidControlOperation,
    /// 相同 Session 控制操作标识已经绑定到不同方法或正文。
    #[error("Session 控制操作标识与既有正文冲突")]
    ControlOperationConflict,
    /// 标题生成缓存的输入摘要或标题正文不满足持久化约束。
    #[error("标题生成缓存参数无效")]
    InvalidTitleGeneration,
    /// open_session 指向尚未完成 SessionCreated 的空目录。
    #[error("Session 尚未创建")]
    SessionNotCreated,
    /// 只读调用指向已经在首个无效日志记录处截断的 Session。
    #[error("Session 日志已损坏，只能读取恢复报告")]
    SessionCorrupt,
    /// 创建请求缺少标题或项目根目录。
    #[error("Session 标题和项目根目录不能为空")]
    InvalidCreateRequest,
    /// Turn 请求的 Session、谱系或输入摘要与当前 Runtime 不一致。
    #[error("Runtime Turn 请求的 Session、谱系或输入摘要无效")]
    InvalidTurnRequest,
    /// RuntimeManager 已关闭当前 Session 句柄，禁止启动新的工作或订阅。
    #[error("Runtime Session 已关闭")]
    SessionClosed,
    /// 相同 Turn 已经由当前 Runtime 执行，不能并发重入 Provider。
    #[error("Runtime Turn 正在执行，不能重复启动")]
    TurnAlreadyRunning,
    /// 相同 Turn 已经存在权威终态，不能重新执行 Provider。
    #[error("Runtime Turn 已经结束，不能重复启动")]
    TurnAlreadyFinished,
    /// Turn 输入与唯一终态无法在当前 Session 容量内可靠持久化。
    #[error("Runtime Turn 无法在当前 Session 容量内可靠持久化")]
    TurnUnpersistable,
    /// Runtime 内联文本阈值或实时事件缓冲容量不在安全范围内。
    #[error("Runtime 内联文本限制或实时事件缓冲容量不在安全范围内")]
    InvalidRuntimeConfig,
    /// 推理文本超过内联限制，但资源模型没有可保持推理语义的 Artifact 类型。
    #[error("推理内容超过 Runtime 内联文本限制，无法保持推理语义")]
    ReasoningTooLarge,
    /// 图片 URL 不满足本地持久化的有界单行约束。
    #[error("图片地址不满足 Runtime 持久化约束")]
    InvalidImageUrl,
    /// Base64 或 data URL 图片不满足规范媒体类型、编码或大小约束。
    #[error("图片内联数据不满足 Runtime 持久化约束")]
    InvalidImageData,
    /// 同步提交的结果无法证明成功或失败，必须重新打开并恢复。
    #[error("Session 需要恢复后才能继续权威工作")]
    RecoveryRequired,
    /// Runtime 内部互斥状态已经损坏，不能继续提交。
    #[error("Session Runtime 内部状态不可用")]
    StateUnavailable,
}

/// 持有同一 Session lease、Journal、ArtifactStore 与提交账本的进程内句柄。
#[derive(Clone)]
pub struct RuntimeSession {
    /// 允许绑定 Runner、控制操作与快照句柄共享同一独占资源所有权。
    inner: Arc<RuntimeSessionInner>,
}

/// RuntimeSession 中必须共同存活且共享同一 Session 身份的资源。
struct RuntimeSessionInner {
    /// 跨进程独占 Runtime 所有权；字段存活即保持操作系统锁。
    lease: SessionLease,
    /// 权威 append-only Journal 与确定性状态归约器。
    journal: SessionJournal,
    /// Base64 图片和大结果使用的 Session 隔离内容寻址存储。
    artifacts: Arc<ArtifactStore>,
    /// Journal 和 Artifact 容量检查使用的不可变配置。
    config: RuntimeConfig,
    /// 为当前 Session 的实时 Agent 事件分配独立递增投递序号。
    publisher: SessionEventPublisher,
    /// 提交、幂等提示、恢复栅栏与一次性 reservation 的串行控制面。
    control: Mutex<ControlState>,
    /// 串行化计划沙箱写入与根 Session `PlanChanged` 权威事件的提交顺序。
    ///
    /// 计划正文和 Session Journal 分属两个持久化边界，必须在同一个进程内
    /// 按写入、发布的顺序完成，避免并发根 Agent 让较旧 Artifact 覆盖较新计划。
    plan_commit_gate: Mutex<()>,
}

/// 同一进程内所有权威提交共享的可变控制状态。
#[derive(Default)]
struct ControlState {
    /// RuntimeManager 对当前共享 Session 的进程内打开、收尾或关闭阶段。
    lifecycle: RuntimeSessionLifecycle,
    /// 无法确认一次提交结果后立即冻结后续工作。
    recovery_required: bool,
    /// Journal 已损坏或控制面无法通过同句柄继续对账时保持的硬恢复栅栏。
    hard_recovery_required: bool,
    /// 首次结果不确定且只允许相同事件身份继续同步对账的事件集合。
    pending_indeterminate: BTreeSet<String>,
    /// 已观察事件的稳定映射提示与内容摘要。
    mappings: BTreeMap<String, MappingRecord>,
    /// 按不可变 Round 身份保存的一次性容量 reservation。
    reservations: BTreeMap<RoundKey, ReservationEntry>,
    /// 为已开始的状态变更工具保存文件快照两阶段事件的专用容量 reservation。
    file_change_reservations: BTreeMap<RequestId, file_changes::FileChangeReservation>,
    /// 为同一进程内的 reservation 分配不复用令牌。
    next_reservation_token: u64,
    /// Runtime 自己负责的 Turn 起点、执行中和精确终态恢复账本。
    turn_executions: BTreeMap<String, RuntimeTurnExecution>,
    /// 为当前进程中的每次实际 Agent 执行分配不复用身份。
    next_turn_execution_id: u64,
}

/// 共享 Runtime Session 的进程内关闭栅栏阶段。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RuntimeSessionLifecycle {
    /// 允许启动 Turn、预检工具 Round 和创建实时订阅。
    #[default]
    Open,
    /// 已拒绝新工作，正在等待 close 前登记的 Running Turn 写入唯一终态。
    Closing,
    /// 不再允许任何新执行或权威 Agent 提交，旧句柄只可读取。
    Closed,
}

/// 单个 Runtime Turn 在当前 lease 生命周期内的可恢复执行阶段。
#[derive(Clone)]
enum RuntimeTurnExecution {
    /// 输入已完成无副作用预检，正在首次提交或对账原子起点。
    Starting {
        /// 冻结全部调用语义的规范摘要。
        request_sha256: String,
        /// 为唯一终态保留的 Journal 字节数。
        terminal_journal_bytes: u64,
    },
    /// 原子起点已确认，Provider 与 Agent Loop 正在执行。
    Running {
        /// 冻结全部调用语义的规范摘要。
        request_sha256: String,
        /// 为唯一终态保留的 Journal 字节数。
        terminal_journal_bytes: u64,
        /// 防止旧 Future 清理后来执行状态的不复用进程内身份。
        execution_id: u64,
        /// 由 Runtime 创建并精确传给本次 Agent Loop 的权威取消令牌。
        cancellation: TurnCancellation,
    },
    /// Agent Loop 已结束，只允许用同一事件身份和正文对账唯一终态。
    TerminalPending {
        /// 冻结全部调用语义的规范摘要。
        request_sha256: String,
        /// 已为该终态保留的 Journal 字节数。
        terminal_journal_bytes: u64,
        /// 精确且稳定的终态事件身份。
        event_id: SessionEventId,
        /// 首次提交前冻结的精确终态正文。
        event: Box<SessionEvent>,
        /// 终态确认后原样返回且不得通过重跑 Provider 重建的结果。
        result: Box<TurnResult>,
    },
    /// 执行 Future 被释放或 Agent 提交已进入其他恢复路径，禁止热继续。
    Abandoned {
        /// 冻结全部调用语义的规范摘要。
        request_sha256: String,
    },
}

impl RuntimeTurnExecution {
    /// 返回用于拒绝同一 Turn 不同正文重用的冻结请求摘要。
    fn request_sha256(&self) -> &str {
        match self {
            Self::Starting { request_sha256, .. }
            | Self::Running { request_sha256, .. }
            | Self::TerminalPending { request_sha256, .. }
            | Self::Abandoned { request_sha256 } => request_sha256,
        }
    }

    /// 返回当前执行尚需保护的唯一终态 Journal 字节数。
    const fn terminal_journal_bytes(&self) -> u64 {
        match self {
            Self::Starting {
                terminal_journal_bytes,
                ..
            }
            | Self::Running {
                terminal_journal_bytes,
                ..
            }
            | Self::TerminalPending {
                terminal_journal_bytes,
                ..
            } => *terminal_journal_bytes,
            Self::Abandoned { .. } => 0,
        }
    }

    /// 返回当前执行是否仍保留一条唯一终态 Journal 记录。
    const fn reserves_terminal_record(&self) -> bool {
        matches!(
            self,
            Self::Starting { .. } | Self::Running { .. } | Self::TerminalPending { .. }
        )
    }

    /// 判断当前执行账本是否仍代表尚未形成可确认终态的工作。
    fn is_active(&self) -> bool {
        !matches!(self, Self::Abandoned { .. })
    }
}

/// 在 Closing 阶段最后一个 Running Turn 收尾后推进 Closed，并告知调用方关闭 Publisher。
fn finalize_runtime_close_if_idle(control: &mut ControlState) -> bool {
    if control.lifecycle != RuntimeSessionLifecycle::Closing
        || control
            .turn_executions
            .values()
            .any(|execution| matches!(execution, RuntimeTurnExecution::Running { .. }))
    {
        return false;
    }
    control.lifecycle = RuntimeSessionLifecycle::Closed;
    true
}

/// 在控制态进入 Closed 的同一临界区发送 Publisher 最终关闭信号。
fn close_runtime_publisher_if_idle(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
) -> Result<(), RuntimeError> {
    if finalize_runtime_close_if_idle(control) {
        inner.publisher.close()?;
    }
    Ok(())
}

/// 已确认起点后在 Future Drop 时冻结热路径的同步 RAII 栅栏。
struct RuntimeTurnGuard {
    /// 不延长 Session 生命周期的共享控制面弱引用。
    inner: Weak<RuntimeSessionInner>,
    /// 当前 Turn 的稳定标识。
    turn_id: String,
    /// 防止旧 Future 修改同标识后来执行的冻结摘要。
    request_sha256: String,
    /// 防止旧 Future 修改相同 TurnId 后来执行的不复用身份。
    execution_id: u64,
    /// 正常终态或显式恢复登记完成后解除 Drop 动作。
    armed: bool,
}

impl RuntimeTurnGuard {
    /// 为已经确认原子起点的 Turn 创建热恢复栅栏。
    fn new(
        inner: &Arc<RuntimeSessionInner>,
        turn_id: &TurnId,
        request_sha256: String,
        execution_id: u64,
    ) -> Self {
        Self {
            inner: Arc::downgrade(inner),
            turn_id: turn_id.as_str().to_owned(),
            request_sha256,
            execution_id,
            armed: true,
        }
    }

    /// 正常终态已经提交或精确恢复状态已经登记时解除 Drop 冻结。
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RuntimeTurnGuard {
    /// Future 异常释放时只冻结热路径，交给既有冷恢复按工具顺序安全收敛。
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut control) = inner.control.lock() else {
            return;
        };
        let matches_running = control
            .turn_executions
            .get(&self.turn_id)
            .is_some_and(|execution| {
                execution.request_sha256() == self.request_sha256
                    && matches!(
                        execution,
                        RuntimeTurnExecution::Running { execution_id, .. }
                            if *execution_id == self.execution_id
                    )
            });
        if matches_running {
            if let Some(RuntimeTurnExecution::Running { cancellation, .. }) =
                control.turn_executions.get(&self.turn_id)
            {
                cancellation.cancel();
            }
            control.turn_executions.insert(
                self.turn_id.clone(),
                RuntimeTurnExecution::Abandoned {
                    request_sha256: self.request_sha256.clone(),
                },
            );
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            if finalize_runtime_close_if_idle(&mut control) {
                let _ = inner.publisher.close();
            }
        }
    }
}

/// 同一 Agent 事件重投时重建完全相同资源事件所需的小型记录。
struct MappingRecord {
    /// 首次映射得到的资源事件规范 JSON SHA-256。
    event_sha256: String,
    /// Transcript 或压缩首次映射时冻结的 revision。
    transcript_revision: Option<u64>,
    /// 压缩首次映射时冻结的资源层作用域摘要。
    compaction_digest: Option<String>,
}

/// 工具 Round 预检、Permit 与最终提交共同使用的不可变身份。
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RoundKey {
    /// 根 Session 标识。
    session_id: String,
    /// 用户 Turn 标识。
    turn_id: String,
    /// 根 Agent 或单层子 Agent 标识。
    agent_id: String,
    /// Provider 中立模型标识。
    model: String,
    /// 当前逻辑模型 Round。
    model_round: u32,
    /// 当前 Round 的 Transcript 段序号。
    segment_index: u32,
}

/// 一次工具 Round 已知内容占用的保守容量账本项。
struct ReservationEntry {
    /// 防止旧 Permit 释放同一身份后来创建的新 reservation。
    token: u64,
    /// 冻结 Assistant 与 PreToolUse 内容的规范 JSON 摘要。
    known_content_sha256: String,
    /// 最终事件中从下标二开始必须匹配的 PreToolUse 消息数量。
    pre_tool_context_count: usize,
    /// 尚未由已确认事件消费的编码后 Journal 字节。
    reserved_journal_bytes: u64,
    /// 尚未由已确认事件消费的 Journal 记录数。
    reserved_journal_records: u64,
    /// 预检 Assistant 中每个工具调用的原始位置与内容摘要。
    tool_request_sha256: BTreeMap<String, String>,
    /// 已知 Base64 图片尚需占用的唯一 Artifact 槽位。
    missing_artifact_ids: BTreeSet<String>,
    /// 已形成完整 Artifact pair 的唯一引用，用于失败清理时保护并发 reservation。
    materialized_artifact_uses: BTreeMap<String, ArtifactUse>,
    /// 工具执行和 PostToolUse 尚未知内容最多可能新建的 Artifact 槽位。
    reserved_unknown_artifacts: usize,
    /// 已在 ArtifactStore 形成完整 pair 并从 reservation 扣除的内容身份。
    materialized_artifact_ids: BTreeSet<String>,
    /// 已经从本 reservation 扣除容量的幂等事件身份。
    committed_event_ids: BTreeSet<String>,
    /// 尚未由已确认事件消费的十六维 Session 状态集合增长预算。
    reserved_state_items: StateCollectionItems,
    /// 不确定提交后必须保留到 Session 被重新打开的完整事件。
    retained_event: Option<AgentCommitEvent>,
    /// Permit 在已经产生持久进度后提前释放，当前句柄必须冻结到重新打开恢复。
    abandoned_after_progress: bool,
}

/// 相同 Agent 事件重复映射时复用的 Transcript 与压缩提示。
#[derive(Clone, Default)]
struct MappingHints {
    /// Transcript 或压缩的固定 revision。
    transcript_revision: Option<u64>,
    /// 资源层压缩来源的固定作用域摘要。
    compaction_digest: Option<String>,
}

/// 消息映射时是只验证 Artifact 还是实际原子保存 Artifact。
#[derive(Clone, Copy)]
enum ArtifactMode {
    /// 只解码、校验并构造内容寻址引用，不产生磁盘写入。
    Probe,
    /// 使用 ArtifactStore 原子保存内容并返回实际引用。
    Commit,
}

/// 事件映射过程中发现的尚不存在 Artifact 集合。
#[derive(Default)]
struct ArtifactProbe {
    /// 按内容摘要去重的缺失 Artifact 标识。
    missing_ids: BTreeSet<String>,
    /// 缺失 Artifact 的完整引用，用于物化异常后重新核对实际落盘集合。
    missing_uses: BTreeMap<String, ArtifactUse>,
}

/// 工具副作用开始前为 Agent 提交出口可见生命周期计算的可证明持久化预算。
///
/// Shell 等工具未来通过 Runtime Terminal API 写入的终端事件和输出 Artifact 不属于当前
/// 尚未暴露的提交路径；接入 `run_turn` 与终端出口前必须把该旁路纳入同一 reservation。
struct ToolRoundPersistenceBudget {
    /// ToolRequested 到最终 Transcript 及必要恢复终止事件的 Journal 字节上界。
    journal_bytes: u64,
    /// 每个工具最坏四个生命周期事件、Transcript 与恢复 Turn 终态的记录数。
    journal_records: u64,
    /// 未知 ToolResult 与 PostToolUse 内容最多创建的唯一 Artifact 数量。
    unknown_artifacts: usize,
    /// 当前 Runtime 可见生命周期和最终 Transcript 的状态集合增长上界。
    state_items: StateCollectionItems,
}

/// 与资源层 `max_state_collection_items` 一一对应的十九维状态集合计量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StateCollectionItems {
    /// Turn 状态映射项数。
    turns: usize,
    /// 原始 Transcript 记录项数。
    transcript: usize,
    /// 原始 Transcript 消息项数。
    messages: usize,
    /// Transcript Segment 项数。
    transcript_segments: usize,
    /// 完整模型 Round 记录项数。
    model_rounds: usize,
    /// 工具生命周期映射项数。
    tools: usize,
    /// 已准备的文件变更证据项数。
    file_changes: usize,
    /// 文件前后快照引用的块项数。
    file_snapshot_chunks: usize,
    /// 终端生命周期映射项数。
    terminals: usize,
    /// 上下文压缩记录项数。
    compactions: usize,
    /// 会话 Todo 项数。
    todos: usize,
    /// 单层子 Agent 映射项数。
    sub_agents: usize,
    /// 邮箱消息映射项数。
    mailbox: usize,
    /// Agent 工作树映射项数。
    worktrees: usize,
    /// 独立标题生成结果缓存项数。
    generated_titles: usize,
    /// 动态输入权威消费回执项数。
    dynamic_input_receipts: usize,
    /// 全部原始消息的内容块项数。
    message_parts: usize,
    /// Transcript 内 ToolResult 的结果内容项数。
    message_tool_result_content: usize,
    /// 工具生命周期终态结果的内容项数。
    tool_outcome_result_content: usize,
    /// 终端输出 Artifact 引用项数。
    terminal_output_artifacts: usize,
    /// 工具参数与推理续传 JSON 的递归集合成员项数。
    json_collection_items: usize,
}

/// Runtime Round reservation 的三种显式结束方式。
enum ReservationFinish {
    /// 最终 Round 已确认，剩余保守预算可以安全移除。
    Consume,
    /// 尚未形成任何持久进度时释放，否则保留并冻结恢复。
    Release,
    /// 提交结果不确定，连同完整事件保留到重新打开恢复。
    RetainIndeterminate(Box<AgentCommitEvent>),
}

impl StateCollectionItems {
    /// 对每个独立维度执行饱和相加，溢出会稳定变为不可通过的最大值。
    fn saturating_add(self, other: Self) -> Self {
        Self {
            turns: self.turns.saturating_add(other.turns),
            transcript: self.transcript.saturating_add(other.transcript),
            messages: self.messages.saturating_add(other.messages),
            transcript_segments: self
                .transcript_segments
                .saturating_add(other.transcript_segments),
            model_rounds: self.model_rounds.saturating_add(other.model_rounds),
            tools: self.tools.saturating_add(other.tools),
            file_changes: self.file_changes.saturating_add(other.file_changes),
            file_snapshot_chunks: self
                .file_snapshot_chunks
                .saturating_add(other.file_snapshot_chunks),
            terminals: self.terminals.saturating_add(other.terminals),
            compactions: self.compactions.saturating_add(other.compactions),
            todos: self.todos.saturating_add(other.todos),
            sub_agents: self.sub_agents.saturating_add(other.sub_agents),
            mailbox: self.mailbox.saturating_add(other.mailbox),
            worktrees: self.worktrees.saturating_add(other.worktrees),
            generated_titles: self.generated_titles.saturating_add(other.generated_titles),
            dynamic_input_receipts: self
                .dynamic_input_receipts
                .saturating_add(other.dynamic_input_receipts),
            message_parts: self.message_parts.saturating_add(other.message_parts),
            message_tool_result_content: self
                .message_tool_result_content
                .saturating_add(other.message_tool_result_content),
            tool_outcome_result_content: self
                .tool_outcome_result_content
                .saturating_add(other.tool_outcome_result_content),
            terminal_output_artifacts: self
                .terminal_output_artifacts
                .saturating_add(other.terminal_output_artifacts),
            json_collection_items: self
                .json_collection_items
                .saturating_add(other.json_collection_items),
        }
    }

    /// 判断每个独立集合维度都没有超过资源层共同上限。
    fn fits_limit(self, limit: usize) -> bool {
        [
            self.turns,
            self.transcript,
            self.messages,
            self.transcript_segments,
            self.model_rounds,
            self.tools,
            self.file_changes,
            self.file_snapshot_chunks,
            self.terminals,
            self.compactions,
            self.todos,
            self.sub_agents,
            self.mailbox,
            self.worktrees,
            self.generated_titles,
            self.dynamic_input_receipts,
            self.message_parts,
            self.message_tool_result_content,
            self.tool_outcome_result_content,
            self.terminal_output_artifacts,
            self.json_collection_items,
        ]
        .into_iter()
        .all(|actual| actual <= limit)
    }

    /// 从剩余预算原子扣除一个已确认事件的实际集合增长，任一维度不足则不修改。
    fn try_consume(&mut self, consumed: Self) -> Result<(), ()> {
        let remaining = Self {
            turns: self.turns.checked_sub(consumed.turns).ok_or(())?,
            transcript: self.transcript.checked_sub(consumed.transcript).ok_or(())?,
            messages: self.messages.checked_sub(consumed.messages).ok_or(())?,
            transcript_segments: self
                .transcript_segments
                .checked_sub(consumed.transcript_segments)
                .ok_or(())?,
            model_rounds: self
                .model_rounds
                .checked_sub(consumed.model_rounds)
                .ok_or(())?,
            tools: self.tools.checked_sub(consumed.tools).ok_or(())?,
            file_changes: self
                .file_changes
                .checked_sub(consumed.file_changes)
                .ok_or(())?,
            file_snapshot_chunks: self
                .file_snapshot_chunks
                .checked_sub(consumed.file_snapshot_chunks)
                .ok_or(())?,
            terminals: self.terminals.checked_sub(consumed.terminals).ok_or(())?,
            compactions: self
                .compactions
                .checked_sub(consumed.compactions)
                .ok_or(())?,
            todos: self.todos.checked_sub(consumed.todos).ok_or(())?,
            sub_agents: self.sub_agents.checked_sub(consumed.sub_agents).ok_or(())?,
            mailbox: self.mailbox.checked_sub(consumed.mailbox).ok_or(())?,
            worktrees: self.worktrees.checked_sub(consumed.worktrees).ok_or(())?,
            generated_titles: self
                .generated_titles
                .checked_sub(consumed.generated_titles)
                .ok_or(())?,
            dynamic_input_receipts: self
                .dynamic_input_receipts
                .checked_sub(consumed.dynamic_input_receipts)
                .ok_or(())?,
            message_parts: self
                .message_parts
                .checked_sub(consumed.message_parts)
                .ok_or(())?,
            message_tool_result_content: self
                .message_tool_result_content
                .checked_sub(consumed.message_tool_result_content)
                .ok_or(())?,
            tool_outcome_result_content: self
                .tool_outcome_result_content
                .checked_sub(consumed.tool_outcome_result_content)
                .ok_or(())?,
            terminal_output_artifacts: self
                .terminal_output_artifacts
                .checked_sub(consumed.terminal_output_artifacts)
                .ok_or(())?,
            json_collection_items: self
                .json_collection_items
                .checked_sub(consumed.json_collection_items)
                .ok_or(())?,
        };
        *self = remaining;
        Ok(())
    }

    /// 判断当前剩余预算能否完整覆盖一个事件的集合增长。
    fn covers(self, required: Self) -> bool {
        let mut remaining = self;
        remaining.try_consume(required).is_ok()
    }
}

impl RuntimeSession {
    /// 创建并独占一个全新 Session，首条事件原子写入 SessionCreated。
    pub fn create_session(
        config: RuntimeConfig,
        request: CreateSessionRequest,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        if request.title.trim().is_empty() || request.project_root.trim().is_empty() {
            return Err(RuntimeError::InvalidCreateRequest);
        }
        let session_id = SessionId::new(request.session_id)?;
        let (lease, artifacts, journal) = open_resources(&config, &session_id)?;
        let state = journal.state()?;
        if state.created {
            return Err(RuntimeError::SessionAlreadyExists);
        }
        artifacts.recover_for_state(&lease, &state)?;
        append_resource_event(
            &journal,
            SessionEventId::new("session-created")?,
            SessionEvent::SessionCreated {
                title: request.title,
                project_root: request.project_root,
            },
        )?;
        Ok(Self::from_parts(config, lease, artifacts, journal))
    }

    /// 打开并独占一个现有 Session；损坏只返回报告，健康日志先完成冷恢复。
    pub fn open_session(
        config: RuntimeConfig,
        session_id: impl Into<String>,
    ) -> Result<OpenSessionResult, RuntimeError> {
        config.validate()?;
        let session_id = SessionId::new(session_id.into())?;
        let lease = acquire_lease(&config.storage_root, &session_id)?;
        let artifacts = Arc::new(ArtifactStore::open(
            &config.storage_root,
            session_id.clone(),
            config.artifacts,
        )?);
        let journal = match SessionJournal::open_with_artifact_validator(
            &config.storage_root,
            session_id,
            config.journal,
            artifacts.clone(),
        )? {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(report) => {
                return Ok(OpenSessionResult::Corrupt(Box::new(report)));
            }
        };
        let state = journal.state()?;
        if !state.created {
            return Err(RuntimeError::SessionNotCreated);
        }
        artifacts.recover_for_state(&lease, &state)?;
        let session = Self::from_parts(config, lease, artifacts, journal);
        session.recover_cold_state()?;
        Ok(OpenSessionResult::Ready(session))
    }

    /// 返回当前 Session 的稳定资源标识。
    pub fn session_id(&self) -> &SessionId {
        self.inner.artifacts.session_id()
    }

    /// 在当前独占 Session 的 ArtifactStore 中原子保存一份应用数据产物。
    ///
    /// 该入口供 Plan 等 Runtime 内部产物复用同一内容寻址、容量和冷恢复边界；
    /// 调用方仍须把返回引用提交到对应的权威 Session 事件后，产物才会被保留。
    pub fn put_artifact(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
    ) -> Result<ArtifactRef, RuntimeError> {
        self.inner
            .artifacts
            .put(bytes, media_type)
            .map_err(Into::into)
    }

    /// 将 Agent Runner 的权威提交出口强制替换为当前 Session，并返回完整生命周期入口。
    pub fn bind_agent_runner(&self, runner: AgentRunner) -> RuntimeAgentRunner {
        self.bind_agent_runner_with_usage_sink(runner, Arc::new(NoopRuntimeModelRoundUsageSink))
    }

    /// 绑定 Agent Runner，并注入必须在工具执行前同步确认的模型 Round 用量出口。
    pub fn bind_agent_runner_with_usage_sink(
        &self,
        runner: AgentRunner,
        usage_sink: Arc<dyn RuntimeModelRoundUsageSink>,
    ) -> RuntimeAgentRunner {
        let downstream = runner.event_sink().clone();
        let event_sink = Arc::new(RuntimeEventFanoutSink::new(
            self.inner.publisher.clone(),
            downstream,
        ));
        let commit_sink = Arc::new(RuntimeCommitSink {
            inner: self.inner.clone(),
            usage_sink,
        });
        let runner = runner
            .with_commit_sink(commit_sink)
            .with_event_sink(event_sink);
        RuntimeAgentRunner {
            inner: self.inner.clone(),
            runner,
        }
    }

    /// 读取与权威 Journal 一致的 Session 状态和 reservation 恢复状态。
    pub fn snapshot(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let state = self.inner.journal.state()?;
        let retained_reservations = control
            .reservations
            .values()
            .filter(|entry| entry.retained_event.is_some() || entry.abandoned_after_progress)
            .count();
        Ok(RuntimeSnapshot {
            state,
            closed: control.lifecycle != RuntimeSessionLifecycle::Open,
            recovery_required: control.recovery_required,
            active_reservations: control.reservations.len(),
            retained_reservations,
            pending_indeterminate_events: control.pending_indeterminate.len(),
        })
    }

    /// 从可选 Journal sequence 独占游标之后读取一页有界类型化权威事件。
    pub fn replay(
        &self,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<RuntimeReplayPage, RuntimeError> {
        Ok(self.inner.journal.read_page(after_sequence, limit)?.into())
    }

    /// 返回当前根 Session 按权威提交顺序保存的完整原始 Transcript 消息。
    pub fn transcript(&self) -> Result<Vec<SessionMessage>, RuntimeError> {
        let state = self.inner.journal.state()?;
        state.validate_transcript_history()?;
        Ok(state
            .raw_transcript_messages()
            .into_iter()
            .cloned()
            .collect())
    }

    /// 物化当前权威 Transcript，恢复为可直接发送给统一 Provider 的完整模型消息。
    pub fn model_transcript(&self) -> Result<Vec<Message>, RuntimeError> {
        let state = self.inner.journal.state()?;
        state.validate_transcript_history()?;
        state
            .raw_transcript_messages()
            .into_iter()
            .map(|message| materialize_model_message(&self.inner.artifacts, message))
            .collect()
    }

    /// 物化一个已注册 Agent 的有效 Transcript，隔离其他 Agent 的消息与压缩历史。
    pub fn model_transcript_for_agent(
        &self,
        source_agent_id: &keencode_resources::AgentId,
    ) -> Result<Vec<Message>, RuntimeError> {
        let state = self.inner.journal.state()?;
        state.validate_transcript_history()?;
        state
            .effective_transcript(source_agent_id)?
            .iter()
            .map(|message| materialize_model_message(&self.inner.artifacts, message))
            .collect()
    }

    /// 将一条来自本 Session 权威记录的消息及其 Artifact 物化为 Provider 中立消息。
    pub fn materialize_message(&self, message: &SessionMessage) -> Result<Message, RuntimeError> {
        materialize_model_message(&self.inner.artifacts, message)
    }

    /// 返回当前进程中尚未形成终态的全部 Turn 标识。
    pub fn active_turn_ids(&self) -> Result<Vec<TurnId>, RuntimeError> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let mut turn_ids = control
            .turn_executions
            .iter()
            .filter(|(_, execution)| execution.is_active())
            .map(|(turn_id, _)| TurnId::new(turn_id.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        turn_ids.sort();
        Ok(turn_ids)
    }

    /// 判断 Session 是否仍有根 Turn、子 Agent、工具、终端或工作树需要退出清理。
    pub fn has_active_work(&self) -> Result<bool, RuntimeError> {
        if !self.active_turn_ids()?.is_empty() {
            return Ok(true);
        }
        let state = self.inner.journal.state()?;
        Ok(state
            .turns
            .values()
            .any(|turn| turn.status == TurnStatus::Running)
            || state.tools.values().any(|tool| tool.outcome.is_none())
            || state.terminals.values().any(|terminal| !terminal.exited)
            || state.sub_agents.values().any(|agent| {
                matches!(
                    agent.status,
                    SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting
                )
            })
            || state.worktrees.values().any(|worktree| !worktree.released))
    }

    /// 追加一个新的用户可见标题并返回提交后的权威状态。
    pub fn rename(
        &self,
        operation_id: &str,
        title: impl Into<String>,
    ) -> Result<SessionState, RuntimeError> {
        self.commit_control_event(
            operation_id,
            SessionEvent::SessionRenamed {
                title: title.into(),
            },
        )
    }

    /// 原子替换 Session 的只读 Plan 状态并返回提交后的权威状态。
    pub fn set_plan(
        &self,
        operation_id: &str,
        plan: PlanState,
    ) -> Result<SessionState, RuntimeError> {
        self.commit_control_event(operation_id, SessionEvent::PlanChanged { plan })
    }

    /// 获取计划沙箱与权威 `PlanChanged` 事件共用的进程内提交锁。
    ///
    /// 调用方必须在完成计划文档 CAS 和对应权威事件发布后释放该锁；Session
    /// lease 保证同一 Session 的生产写入不会由另一个 Runtime 进程绕过此顺序。
    pub(crate) fn lock_plan_commit(&self) -> Result<std::sync::MutexGuard<'_, ()>, RuntimeError> {
        self.inner
            .plan_commit_gate
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)
    }

    /// 原子保存当前实际解析出的无凭据 Provider 快照。
    pub fn set_provider_snapshot(
        &self,
        operation_id: &str,
        provider: ProviderSnapshot,
    ) -> Result<SessionState, RuntimeError> {
        self.commit_control_event(
            operation_id,
            SessionEvent::ProviderSnapshotUpdated { provider },
        )
    }

    /// 在指定控制操作域内原子保存当前实际解析出的无凭据 Provider 快照。
    ///
    /// 模型和推理强度虽然共享同一个 Provider 快照事件类型，但它们的重试意图
    /// 不同，调用方必须用不同的稳定域标识，避免相同 operationId 在两个配置项
    /// 之间误判为同一条事件。
    pub fn set_provider_snapshot_in_domain(
        &self,
        operation_domain: &str,
        operation_id: &str,
        provider: ProviderSnapshot,
    ) -> Result<SessionState, RuntimeError> {
        self.commit_control_event_in_domain(
            operation_domain,
            operation_id,
            SessionEvent::ProviderSnapshotUpdated { provider },
        )
    }

    /// 原子排队一条根 Agent 与单层子 Agent 之间的权威邮箱消息。
    ///
    /// 事件身份只由 Session、排队动作和消息 ID 派生；相同消息 ID 与相同正文可安全重试，
    /// 相同消息 ID 绑定不同正文会明确返回控制操作冲突。
    pub fn queue_mailbox_message(
        &self,
        message: MailboxMessage,
    ) -> Result<SessionState, RuntimeError> {
        let message_id = message.message_id.clone();
        self.commit_mailbox_event(
            "queue",
            &message_id,
            SessionEvent::MailboxMessageQueued { message },
        )
    }

    /// 原子确认一条权威邮箱消息已经由接收方投递。
    ///
    /// 相同消息 ID 的重复确认复用固定事件身份，因此在桥接层响应丢失后可安全重试。
    pub fn deliver_mailbox_message(
        &self,
        message_id: MailboxMessageId,
    ) -> Result<SessionState, RuntimeError> {
        self.commit_mailbox_event(
            "deliver",
            &message_id,
            SessionEvent::MailboxMessageDelivered {
                message_id: message_id.clone(),
            },
        )
    }

    /// 按 operationId 查询已经持久化的标题结果，并在跨方法复用时明确报冲突。
    pub fn cached_generated_title(
        &self,
        operation_id: &str,
        input_sha256: &str,
    ) -> Result<Option<String>, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        if !valid_sha256_hex(input_sha256) {
            return Err(RuntimeError::InvalidTitleGeneration);
        }
        let event_id = runtime_control_event_id(self.session_id(), operation_id)?;
        let state = self.inner.journal.state()?;
        if let Some(result) = state.generated_titles.get(operation_id) {
            return if result.input_sha256 == input_sha256 {
                Ok(Some(result.title.clone()))
            } else {
                Err(RuntimeError::ControlOperationConflict)
            };
        }
        if self.inner.journal.contains_event_id(&event_id)? {
            return Err(RuntimeError::ControlOperationConflict);
        }
        Ok(None)
    }

    /// 查询指定控制操作已经提交的真实 Journal 事件记录。
    ///
    /// 该查询只读取权威日志，不依赖进程内缓存；调用方收到响应丢失后可以据此
    /// 判断原操作是否已经提交，并从记录正文核对请求意图。尚未提交时返回 `None`。
    pub fn committed_control_event(
        &self,
        operation_id: &str,
    ) -> Result<Option<SessionEventRecord>, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        let event_id = runtime_control_event_id(self.session_id(), operation_id)?;
        find_committed_event(&self.inner.journal, &event_id)
    }

    /// 查询指定控制操作域内已经提交的真实 Journal 事件记录。
    ///
    /// 同一个原始 operationId 在不同域内代表不同的重试意图。域值和原始
    /// operationId 均按控制操作的现有有界规则校验。
    pub fn committed_control_event_in_domain(
        &self,
        operation_domain: &str,
        operation_id: &str,
    ) -> Result<Option<SessionEventRecord>, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        validate_control_operation_domain(operation_domain)?;
        let event_id =
            runtime_control_event_id_in_domain(self.session_id(), operation_domain, operation_id)?;
        find_committed_event(&self.inner.journal, &event_id)
    }

    /// 原子保存一次成功标题生成结果；相同 operationId 和输入只会保留一个结果。
    pub fn cache_generated_title(
        &self,
        operation_id: &str,
        input_sha256: &str,
        title: impl Into<String>,
    ) -> Result<String, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        let title = title.into();
        if !valid_sha256_hex(input_sha256)
            || title.trim().is_empty()
            || title.trim() != title
            || title.len() > 512
        {
            return Err(RuntimeError::InvalidTitleGeneration);
        }
        let state = self.commit_control_event(
            operation_id,
            SessionEvent::TitleGenerated {
                result: GeneratedTitleRecord {
                    operation_id: operation_id.to_owned(),
                    input_sha256: input_sha256.to_owned(),
                    title,
                },
            },
        )?;
        state
            .generated_titles
            .get(operation_id)
            .map(|result| result.title.clone())
            .ok_or(RuntimeError::RecoveryRequired)
    }

    /// 取消当前 Session 中所有已经取得权威取消令牌的运行 Turn。
    pub fn cancel_all_turns(&self) -> Result<usize, RuntimeError> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let mut cancelled = 0_usize;
        for execution in control.turn_executions.values() {
            if let RuntimeTurnExecution::Running { cancellation, .. } = execution {
                if !cancellation.is_cancelled() {
                    cancellation.cancel();
                    cancelled = cancelled.saturating_add(1);
                }
            }
        }
        Ok(cancelled)
    }

    /// 从当前实时水位开始订阅本 Session 后续 Agent 事件。
    pub fn subscribe(&self) -> Result<RuntimeEventSubscription, RuntimeError> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        let subscription = self.inner.publisher.subscribe();
        drop(control);
        subscription
    }

    /// 对当前 Session 内正在运行的指定 Turn 触发 Runtime 权威取消令牌。
    pub fn cancel_turn(
        &self,
        turn_id: impl Into<String>,
    ) -> Result<TurnCancellationOutcome, RuntimeError> {
        let turn_id = TurnId::new(turn_id.into())?;
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        let Some(RuntimeTurnExecution::Running { cancellation, .. }) =
            control.turn_executions.get(turn_id.as_str())
        else {
            return Ok(TurnCancellationOutcome::NotRunning);
        };
        if cancellation.is_cancelled() {
            return Ok(TurnCancellationOutcome::AlreadyRequested);
        }
        cancellation.cancel();
        Ok(TurnCancellationOutcome::Requested)
    }

    /// 把已由 Collaboration 确认的尚未启动 Turn 终态补偿为完整 Runtime 原子生命周期。
    ///
    /// 首次 `InitialTask` 且 Journal 尚无该 Agent 时，批次依次写入
    /// `SubAgentSpawned(Pending)`、`TurnStarted`、`Running`、对应的 `TurnStopped`
    /// 和终态 Agent 状态；后续
    /// Followup/Retry 或已有 Agent 只省略 Spawned。这里的 `TurnStarted` 与 `Running`
    /// 是为了让权威 Journal 可重放而合成的零时长逻辑生命周期，只表示“曾预约但在
    /// 启动前终止”，不表示已进入 Runner、调用执行端口、启动 Provider 或调用工具；
    /// 上游是否已经预约容量由其自身权威状态表示。稳定批次标识只绑定 Session、Agent
    /// 和 Turn，因此相同正文可安全重试；终态或正文冲突会硬冻结 Runtime 并返回
    /// `RecoveryRequired`。该方法不写入模型、工具或 Transcript 事实。
    pub fn record_unstarted_turn_termination(
        &self,
        request: UnstartedTurnTerminationRequest,
    ) -> Result<UnstartedTurnTerminationOutcome, RuntimeError> {
        validate_unstarted_turn_termination_request(self.session_id(), &request)?;
        let event_id = runtime_unstarted_turn_termination_event_id(
            self.session_id(),
            &request.agent.agent_id,
            &request.turn_id,
        )?;
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        let state = self.inner.journal.state()?;
        let event_id_known = self.inner.journal.contains_event_id(&event_id)?;
        if event_id_known {
            if let Err(error) = validate_known_unstarted_turn_termination(
                &self.inner.journal,
                &state,
                &request,
                &event_id,
            ) {
                if matches!(error, RuntimeError::RecoveryRequired) {
                    control.hard_recovery_required = true;
                    refresh_recovery_required(&mut control);
                }
                return Err(error);
            }
        } else if let Err(error) = validate_unstarted_turn_termination_state(&state, &request) {
            if matches!(error, RuntimeError::RecoveryRequired) {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
            }
            return Err(error);
        }
        let event_key = event_id.as_str().to_owned();
        if !recovery_gate_allows_event(&control, &event_key) {
            return Err(RuntimeError::RecoveryRequired);
        }
        let include_spawn = !state.sub_agents.contains_key(&request.agent.agent_id);
        let events = unstarted_turn_termination_events(&request, include_spawn)?;
        let fallback_events = if !include_spawn && request.initial_task {
            Some(unstarted_turn_termination_events(&request, true)?)
        } else {
            None
        };
        let mut candidates = vec![events];
        if let Some(fallback) = fallback_events {
            candidates.push(fallback);
        }
        for event in candidates {
            let mut expected_sequence = state.last_sequence;
            let mut event_id_conflict = false;
            for _ in 0..2 {
                #[cfg(test)]
                if take_runtime_lifecycle_failure(&event_id) {
                    return Err(ResourceError::Json(
                        "测试注入 Runtime 生命周期明确失败".to_owned(),
                    )
                    .into());
                }
                match self.inner.journal.append_idempotent(
                    event_id.clone(),
                    expected_sequence,
                    event.clone(),
                ) {
                    Ok(IdempotentAppendOutcome::Appended(receipt)) => {
                        if self
                            .inner
                            .publisher
                            .publish_authoritative(receipt.record)
                            .is_err()
                        {
                            control.hard_recovery_required = true;
                            mark_event_indeterminate(&mut control, &event_key);
                            return Err(RuntimeError::RecoveryRequired);
                        }
                        mark_event_confirmed(&mut control, &event_key);
                        return Ok(UnstartedTurnTerminationOutcome::Committed);
                    }
                    Ok(IdempotentAppendOutcome::AlreadyCommitted { .. }) => {
                        mark_event_confirmed(&mut control, &event_key);
                        return Ok(UnstartedTurnTerminationOutcome::AlreadyCommitted);
                    }
                    Ok(IdempotentAppendOutcome::EventIdConflict { .. }) => {
                        event_id_conflict = true;
                        break;
                    }
                    Ok(IdempotentAppendOutcome::SequenceConflict {
                        actual_sequence, ..
                    }) => {
                        expected_sequence = actual_sequence;
                    }
                    Ok(IdempotentAppendOutcome::Indeterminate { .. }) => {
                        mark_event_indeterminate(&mut control, &event_key);
                        return Err(RuntimeError::RecoveryRequired);
                    }
                    Err(ResourceError::CorruptReadOnly) => {
                        control.hard_recovery_required = true;
                        mark_event_indeterminate(&mut control, &event_key);
                        return Err(RuntimeError::RecoveryRequired);
                    }
                    Err(ResourceError::Reduction(_)) => {
                        // Reducer 拒绝通常表示 Journal 已有 Running/终态 Turn 或谱系破坏；
                        // 不允许把 Collaboration 的中断状态静默覆盖到 Runtime。
                        control.hard_recovery_required = true;
                        refresh_recovery_required(&mut control);
                        return Err(RuntimeError::RecoveryRequired);
                    }
                    Err(error) => return Err(RuntimeError::Resource(error)),
                }
            }
            if !event_id_conflict {
                control.hard_recovery_required = true;
                mark_event_indeterminate(&mut control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
        }
        // 同一稳定事件身份已绑定了不同正文；这不是可以忽略的幂等重放。
        control.hard_recovery_required = true;
        mark_event_indeterminate(&mut control, &event_key);
        Err(RuntimeError::RecoveryRequired)
    }

    /// 在 Session 控制锁内提交一个桌面控制事件，避免与 Agent 生命周期交错。
    fn commit_control_event(
        &self,
        operation_id: &str,
        event: SessionEvent,
    ) -> Result<SessionState, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        let event_id = runtime_control_event_id(self.session_id(), operation_id)?;
        self.commit_control_event_with_id(event_id, event)
    }

    /// 在指定控制操作域内提交一个桌面控制事件。
    fn commit_control_event_in_domain(
        &self,
        operation_domain: &str,
        operation_id: &str,
        event: SessionEvent,
    ) -> Result<SessionState, RuntimeError> {
        validate_control_operation_id(operation_id)?;
        validate_control_operation_domain(operation_domain)?;
        let event_id =
            runtime_control_event_id_in_domain(self.session_id(), operation_domain, operation_id)?;
        self.commit_control_event_with_id(event_id, event)
    }

    /// 在 Session 控制锁内提交已经派生出稳定身份的桌面控制事件。
    fn commit_control_event_with_id(
        &self,
        event_id: SessionEventId,
        event: SessionEvent,
    ) -> Result<SessionState, RuntimeError> {
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        commit_runtime_lifecycle_event(&self.inner, &mut control, event_id, event, true)?;
        self.inner.journal.state().map_err(RuntimeError::from)
    }

    /// 在 Session 控制锁内以消息动作和强类型消息 ID 提交邮箱事件。
    fn commit_mailbox_event(
        &self,
        action: &'static str,
        message_id: &MailboxMessageId,
        event: SessionEvent,
    ) -> Result<SessionState, RuntimeError> {
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        let event_id = runtime_mailbox_event_id(self.session_id(), action, message_id)?;
        commit_runtime_lifecycle_event(&self.inner, &mut control, event_id, event, true)?;
        self.inner.journal.state().map_err(RuntimeError::from)
    }

    /// 原子关闭共享 Session，并触发所有正在运行 Turn 的 Runtime 权威取消令牌。
    fn close_runtime(&self) -> Result<(), RuntimeError> {
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        match control.lifecycle {
            RuntimeSessionLifecycle::Open => {
                control.lifecycle = RuntimeSessionLifecycle::Closing;
            }
            RuntimeSessionLifecycle::Closing => {}
            RuntimeSessionLifecycle::Closed => return self.inner.publisher.close(),
        }
        for execution in control.turn_executions.values() {
            if let RuntimeTurnExecution::Running { cancellation, .. } = execution {
                cancellation.cancel();
            }
        }
        close_runtime_publisher_if_idle(&self.inner, &mut control)?;
        Ok(())
    }

    /// 判断当前句柄是否已经因不确定提交冻结。
    pub fn is_recovery_required(&self) -> bool {
        self.inner
            .control
            .lock()
            .map_or(true, |control| control.recovery_required)
    }

    /// 使用已经取得且身份一致的三个资源创建共享组合根。
    fn from_parts(
        config: RuntimeConfig,
        lease: SessionLease,
        artifacts: Arc<ArtifactStore>,
        journal: SessionJournal,
    ) -> Self {
        let publisher =
            SessionEventPublisher::new(artifacts.session_id().as_str(), config.live_event_capacity);
        Self {
            inner: Arc::new(RuntimeSessionInner {
                lease,
                journal,
                artifacts,
                config,
                publisher,
                control: Mutex::new(ControlState::default()),
                plan_commit_gate: Mutex::new(()),
            }),
        }
    }

    /// 按终端、工具终态、合成 Transcript、Turn 和未启动子 Agent 的固定顺序收敛冷恢复状态。
    fn recover_cold_state(&self) -> Result<(), RuntimeError> {
        recover_terminals(&self.inner)?;
        recover_tool_outcomes(&self.inner)?;
        recover_tool_transcript(&self.inner)?;
        recover_turns(&self.inner)?;
        recover_pending_sub_agents(&self.inner)?;
        Ok(())
    }
}

impl RuntimeAgentRunner {
    /// 从 TurnStarted 到唯一终态完整执行一次模型与工具循环并同步提交全部权威事实。
    pub async fn run_turn(&self, turn: RuntimeTurnRequest) -> Result<TurnResult, RuntimeError> {
        let session_id = turn.request.session_id().as_str().to_owned();
        if session_id != self.inner.artifacts.session_id().as_str()
            || turn.prompt_summary.trim().is_empty()
            || !turn
                .request
                .model_request()
                .messages
                .ends_with(&turn.input_messages)
            || turn.input_messages.iter().any(|message| {
                matches!(
                    message.role,
                    ModelMessageRole::Assistant | ModelMessageRole::Tool
                )
            })
        {
            return Err(RuntimeError::InvalidTurnRequest);
        }
        let turn_id = TurnId::new(turn.request.turn_id().as_str())?;
        let source_agent_id =
            keencode_resources::AgentId::new(turn.request.source_agent_id().as_str())?;
        let root_turn_id = TurnId::new(turn.root_turn_id.clone())?;
        let parent_turn_id = turn.parent_turn_id.clone().map(TurnId::new).transpose()?;
        if let Some(agent) = turn.spawned_agent.as_ref()
            && (source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                || parent_turn_id.is_none()
                || agent.agent_id != source_agent_id
                || agent.task.trim().is_empty()
                || agent.status != SubAgentStatus::Pending
                || agent.current_turn_id.is_some()
                || agent.result_summary.is_some())
        {
            return Err(RuntimeError::InvalidTurnRequest);
        }
        let cancellation = turn.request.cancellation().clone();
        let request_sha256 = runtime_turn_request_sha256(&turn)?;
        let start_event_id = runtime_lifecycle_event_id(
            self.inner.artifacts.session_id(),
            &turn_id,
            "turn-started",
        )?;
        let terminal_event_id = runtime_lifecycle_event_id(
            self.inner.artifacts.session_id(),
            &turn_id,
            "turn-terminal",
        )?;
        let input_key = RoundKey {
            session_id: session_id.clone(),
            turn_id: turn_id.as_str().to_owned(),
            agent_id: source_agent_id.as_str().to_owned(),
            model: turn.request.model_request().model.clone(),
            model_round: 0,
            segment_index: 0,
        };
        let terminal_journal_bytes = runtime_terminal_reservation_bytes(
            self.inner.artifacts.session_id(),
            &terminal_event_id,
            &turn_id,
            &source_agent_id,
        )?;

        let execution_id;
        {
            let mut control = self
                .inner
                .control
                .lock()
                .map_err(|_| RuntimeError::StateUnavailable)?;
            if control.lifecycle != RuntimeSessionLifecycle::Open {
                return Err(RuntimeError::SessionClosed);
            }
            let mut starting_retry = false;
            if let Some(execution) = control.turn_executions.get(turn_id.as_str()).cloned() {
                if execution.request_sha256() != request_sha256 {
                    return Err(RuntimeError::InvalidTurnRequest);
                }
                match execution {
                    RuntimeTurnExecution::Running { .. } => {
                        return Err(RuntimeError::TurnAlreadyRunning);
                    }
                    RuntimeTurnExecution::Abandoned { .. } => {
                        return Err(RuntimeError::RecoveryRequired);
                    }
                    RuntimeTurnExecution::TerminalPending {
                        event_id,
                        event,
                        result,
                        ..
                    } => {
                        commit_runtime_lifecycle_event(
                            &self.inner,
                            &mut control,
                            event_id,
                            *event,
                            false,
                        )?;
                        if control.hard_recovery_required {
                            return Err(RuntimeError::RecoveryRequired);
                        }
                        control.turn_executions.remove(turn_id.as_str());
                        return Ok(*result);
                    }
                    RuntimeTurnExecution::Starting { .. } => {
                        starting_retry = true;
                    }
                }
            } else {
                if control.recovery_required || control.hard_recovery_required {
                    return Err(RuntimeError::RecoveryRequired);
                }
                let state = self.inner.journal.state()?;
                if let Some(existing) = state.turns.get(&turn_id) {
                    if existing.status == TurnStatus::Running {
                        control.hard_recovery_required = true;
                        refresh_recovery_required(&mut control);
                        return Err(RuntimeError::RecoveryRequired);
                    }
                    return Err(RuntimeError::TurnAlreadyFinished);
                }
            }

            let state = self.inner.journal.state()?;
            validate_runtime_turn_session_modes(&state, &turn)?;
            validate_runtime_turn_input(&state, &turn, &source_agent_id)?;
            execution_id = control
                .next_turn_execution_id
                .checked_add(1)
                .ok_or(RuntimeError::StateUnavailable)?;
            control.next_turn_execution_id = execution_id;
            let mut probe = ArtifactProbe::default();
            let input_probe = runtime_input_event(
                &self.inner,
                &input_key,
                &turn.input_messages,
                &turn_id,
                &source_agent_id,
                &root_turn_id,
                parent_turn_id.as_ref(),
                &turn.prompt_summary,
                turn.spawned_agent.as_ref(),
                ArtifactMode::Probe,
                &mut probe,
            )?;
            if starting_retry {
                commit_runtime_lifecycle_event(
                    &self.inner,
                    &mut control,
                    start_event_id,
                    input_probe,
                    false,
                )?;
                if control.hard_recovery_required {
                    return Err(RuntimeError::RecoveryRequired);
                }
            } else {
                validate_runtime_event_candidate(&state, &start_event_id, &input_probe)?;
                let input_journal_bytes = encoded_record_len(
                    self.inner.artifacts.session_id(),
                    &start_event_id,
                    &input_probe,
                )?;
                if input_journal_bytes > self.inner.config.journal.max_event_bytes
                    || terminal_journal_bytes > self.inner.config.journal.max_event_bytes
                {
                    return Err(RuntimeError::TurnUnpersistable);
                }
                let budget = ToolRoundPersistenceBudget {
                    journal_bytes: input_journal_bytes
                        .checked_add(terminal_journal_bytes)
                        .ok_or(RuntimeError::TurnUnpersistable)?,
                    journal_records: 2,
                    unknown_artifacts: 0,
                    state_items: state_collection_event_items(&input_probe),
                };
                ensure_preflight_capacity(
                    &self.inner,
                    &control,
                    &state,
                    &budget,
                    &probe.missing_ids,
                )
                .map_err(|_| RuntimeError::TurnUnpersistable)?;
                control.turn_executions.insert(
                    turn_id.as_str().to_owned(),
                    RuntimeTurnExecution::Starting {
                        request_sha256: request_sha256.clone(),
                        terminal_journal_bytes,
                    },
                );

                let mut commit_probe = ArtifactProbe::default();
                let first_input_event = runtime_input_event(
                    &self.inner,
                    &input_key,
                    &turn.input_messages,
                    &turn_id,
                    &source_agent_id,
                    &root_turn_id,
                    parent_turn_id.as_ref(),
                    &turn.prompt_summary,
                    turn.spawned_agent.as_ref(),
                    ArtifactMode::Commit,
                    &mut commit_probe,
                );
                let input_event = match first_input_event {
                    Ok(event) => event,
                    Err(first_error) => {
                        let materialized = materialized_probe_artifacts(&self.inner, &probe);
                        match materialized {
                            Ok(materialized) if materialized.is_empty() => {
                                control.turn_executions.remove(turn_id.as_str());
                                return Err(first_error);
                            }
                            Ok(materialized) if materialized == probe.missing_ids => {
                                input_probe.clone()
                            }
                            Ok(_) => {
                                let mut retry_probe = ArtifactProbe::default();
                                match runtime_input_event(
                                    &self.inner,
                                    &input_key,
                                    &turn.input_messages,
                                    &turn_id,
                                    &source_agent_id,
                                    &root_turn_id,
                                    parent_turn_id.as_ref(),
                                    &turn.prompt_summary,
                                    turn.spawned_agent.as_ref(),
                                    ArtifactMode::Commit,
                                    &mut retry_probe,
                                ) {
                                    Ok(event) => event,
                                    Err(_) => {
                                        if materialized_probe_artifacts(&self.inner, &probe)
                                            .is_ok_and(|materialized| {
                                                materialized == probe.missing_ids
                                            })
                                        {
                                            input_probe.clone()
                                        } else {
                                            abandon_runtime_turn_after_start_failure(
                                                &self.inner,
                                                &mut control,
                                                &turn_id,
                                                &request_sha256,
                                            );
                                            return Err(RuntimeError::RecoveryRequired);
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                abandon_runtime_turn_after_start_failure(
                                    &self.inner,
                                    &mut control,
                                    &turn_id,
                                    &request_sha256,
                                );
                                return Err(RuntimeError::RecoveryRequired);
                            }
                        }
                    }
                };
                let input_mapping_matches = canonical_sha256(&input_event).and_then(|committed| {
                    canonical_sha256(&input_probe).map(|probed| committed == probed)
                });
                if !matches!(input_mapping_matches, Ok(true)) {
                    abandon_runtime_turn_after_start_failure(
                        &self.inner,
                        &mut control,
                        &turn_id,
                        &request_sha256,
                    );
                    return Err(RuntimeError::RecoveryRequired);
                }
                if let Err(error) = commit_runtime_lifecycle_event(
                    &self.inner,
                    &mut control,
                    start_event_id,
                    input_event,
                    false,
                ) {
                    if !control.recovery_required {
                        if recover_unreferenced_runtime_artifacts(&self.inner).is_ok() {
                            control.turn_executions.remove(turn_id.as_str());
                        } else {
                            abandon_runtime_turn_after_start_failure(
                                &self.inner,
                                &mut control,
                                &turn_id,
                                &request_sha256,
                            );
                            return Err(RuntimeError::RecoveryRequired);
                        }
                    }
                    return Err(error);
                }
            }
            control.turn_executions.insert(
                turn_id.as_str().to_owned(),
                RuntimeTurnExecution::Running {
                    request_sha256: request_sha256.clone(),
                    terminal_journal_bytes,
                    execution_id,
                    cancellation,
                },
            );
        }

        self.finish_running_turn(
            turn,
            turn_id,
            source_agent_id,
            terminal_event_id,
            request_sha256,
            terminal_journal_bytes,
            execution_id,
        )
        .await
    }

    /// 执行已经确认原子起点的 Agent Loop，并提交冻结的唯一终态。
    #[allow(clippy::too_many_arguments)]
    async fn finish_running_turn(
        &self,
        turn: RuntimeTurnRequest,
        turn_id: TurnId,
        source_agent_id: keencode_resources::AgentId,
        terminal_event_id: SessionEventId,
        request_sha256: String,
        terminal_journal_bytes: u64,
        execution_id: u64,
    ) -> Result<TurnResult, RuntimeError> {
        let mut guard =
            RuntimeTurnGuard::new(&self.inner, &turn_id, request_sha256.clone(), execution_id);
        let result = self.runner.run_turn(turn.request).await;
        let terminal = runtime_terminal_event(&turn_id, &source_agent_id, &result);
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let running_matches =
            control
                .turn_executions
                .get(turn_id.as_str())
                .is_some_and(|execution| {
                    execution.request_sha256() == request_sha256
                        && matches!(
                            execution,
                            RuntimeTurnExecution::Running {
                                execution_id: running_execution_id,
                                ..
                            } if *running_execution_id == execution_id
                        )
                });
        if !running_matches {
            close_runtime_publisher_if_idle(&self.inner, &mut control)?;
            guard.disarm();
            return Err(RuntimeError::RecoveryRequired);
        }
        if control.recovery_required || control.hard_recovery_required {
            control.turn_executions.insert(
                turn_id.as_str().to_owned(),
                RuntimeTurnExecution::Abandoned { request_sha256 },
            );
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            close_runtime_publisher_if_idle(&self.inner, &mut control)?;
            guard.disarm();
            return Err(RuntimeError::RecoveryRequired);
        }
        let terminal_bytes = encoded_record_len(
            self.inner.artifacts.session_id(),
            &terminal_event_id,
            &terminal,
        )?;
        if terminal_bytes > terminal_journal_bytes {
            control.turn_executions.insert(
                turn_id.as_str().to_owned(),
                RuntimeTurnExecution::Abandoned { request_sha256 },
            );
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            close_runtime_publisher_if_idle(&self.inner, &mut control)?;
            guard.disarm();
            return Err(RuntimeError::RecoveryRequired);
        }
        control.turn_executions.insert(
            turn_id.as_str().to_owned(),
            RuntimeTurnExecution::TerminalPending {
                request_sha256,
                terminal_journal_bytes,
                event_id: terminal_event_id.clone(),
                event: Box::new(terminal.clone()),
                result: Box::new(result.clone()),
            },
        );
        let terminal_commit = commit_runtime_lifecycle_event(
            &self.inner,
            &mut control,
            terminal_event_id.clone(),
            terminal,
            false,
        );
        if let Err(error) = terminal_commit {
            mark_event_indeterminate(&mut control, terminal_event_id.as_str());
            close_runtime_publisher_if_idle(&self.inner, &mut control)?;
            guard.disarm();
            return Err(error);
        }
        if control.hard_recovery_required {
            close_runtime_publisher_if_idle(&self.inner, &mut control)?;
            guard.disarm();
            return Err(RuntimeError::RecoveryRequired);
        }
        control.turn_executions.remove(turn_id.as_str());
        close_runtime_publisher_if_idle(&self.inner, &mut control)?;
        guard.disarm();
        Ok(result)
    }
}

/// 按当前健康 Journal 状态删除尚未形成任何权威引用的完整 Artifact pair。
fn recover_unreferenced_runtime_artifacts(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    let state = inner.journal.state()?;
    inner.artifacts.recover_for_state(&inner.lease, &state)?;
    Ok(())
}

/// 起点物化无法安全继续时回收孤儿并建立只能通过冷打开解除的硬栅栏。
fn abandon_runtime_turn_after_start_failure(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    turn_id: &TurnId,
    request_sha256: &str,
) {
    let _ = recover_unreferenced_runtime_artifacts(inner);
    control.turn_executions.insert(
        turn_id.as_str().to_owned(),
        RuntimeTurnExecution::Abandoned {
            request_sha256: request_sha256.to_owned(),
        },
    );
    control.hard_recovery_required = true;
    refresh_recovery_required(control);
}

/// 只暴露绑定 Session 的 AgentCommitSink 实现，避免外部替换内部账本。
struct RuntimeCommitSink {
    /// 与 RuntimeSession 共同持有 lease、Journal、ArtifactStore 和恢复栅栏。
    inner: Arc<RuntimeSessionInner>,
    /// 在工具执行前同步持久化当前项目 Goal 用量的注入出口。
    usage_sink: Arc<dyn RuntimeModelRoundUsageSink>,
}

impl AgentCommitSink for RuntimeCommitSink {
    /// 校验用量身份属于当前运行 Turn 后同步交给注入的项目级持久出口。
    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        let state = self.inner.journal.state().map_err(|_| {
            AgentCommitSinkError::indeterminate("无法读取模型 Round 用量对应的权威 Session")
        })?;
        let identity_matches = usage.session_id().as_str()
            == self.inner.artifacts.session_id().as_str()
            && usage.model_round() > 0
            && !usage.model().trim().is_empty()
            && state
                .turns
                .get(&TurnId::new(usage.turn_id().as_str()).map_err(|_| {
                    AgentCommitSinkError::rejected("模型 Round 用量携带无效 Turn 身份")
                })?)
                .is_some_and(|turn| {
                    turn.status == TurnStatus::Running
                        && turn.source_agent_id.as_str() == usage.source_agent_id().as_str()
                });
        if !identity_matches {
            return Err(AgentCommitSinkError::rejected(
                "模型 Round 用量不属于当前运行 Session Turn",
            ));
        }
        self.usage_sink.commit(usage)
    }

    /// 在任何工具生命周期或副作用前验证已知 Round 内容并签发一次性 reservation。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        preflight_round(&self.inner, round)
    }

    /// 同步、幂等地把 Agent 六类权威事件映射并追加到同一 Session Journal。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        commit_agent_event(&self.inner, event)
    }
}

/// 工具 Round Permit Drop、成功提交或不确定提交时回调的唯一 reservation。
struct RuntimeRoundReservation {
    /// Runtime 可能先于异常清理路径释放，Weak 避免 reservation 延长整个 Session 生命周期。
    inner: Weak<RuntimeSessionInner>,
    /// reservation 的不可变 Round 身份。
    key: RoundKey,
    /// 防止旧 Permit 结束后来同身份 reservation 的唯一令牌。
    token: u64,
}

impl AgentToolRoundReservation for RuntimeRoundReservation {
    /// 成功提交后一次性消费匹配 reservation。
    fn consume(self: Box<Self>) {
        (*self).finish(ReservationFinish::Consume);
    }

    /// 未提交且无持久进度时释放；已有进度则保留尾部容量并冻结 Runtime。
    fn release(self: Box<Self>) {
        (*self).finish(ReservationFinish::Release);
    }

    /// 不确定提交时保留 reservation 和完整事件，并冻结 Runtime。
    fn retain_indeterminate(self: Box<Self>, event: AgentCommitEvent) {
        (*self).finish(ReservationFinish::RetainIndeterminate(Box::new(event)));
    }
}

impl RuntimeRoundReservation {
    /// 按令牌结束当前 reservation，旧令牌永远不能修改后来条目。
    fn finish(self, finish: ReservationFinish) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut control) = inner.control.lock() else {
            return;
        };
        let matches = control
            .reservations
            .get(&self.key)
            .is_some_and(|entry| entry.token == self.token);
        if !matches {
            return;
        }
        match finish {
            ReservationFinish::Consume => {
                control.reservations.remove(&self.key);
            }
            ReservationFinish::RetainIndeterminate(event) => {
                control
                    .pending_indeterminate
                    .remove(event.event_id().as_str());
                if let Some(entry) = control.reservations.get_mut(&self.key) {
                    entry.retained_event = Some(*event);
                }
            }
            ReservationFinish::Release => {
                let progressed = control.reservations.get(&self.key).is_some_and(|entry| {
                    !entry.committed_event_ids.is_empty()
                        || !entry.materialized_artifact_ids.is_empty()
                        || entry.retained_event.is_some()
                });
                if progressed {
                    if let Some(entry) = control.reservations.get_mut(&self.key) {
                        entry.abandoned_after_progress = true;
                    }
                } else {
                    control.reservations.remove(&self.key);
                }
            }
        }
        refresh_recovery_required(&mut control);
    }
}

/// 同时获取 lease、ArtifactStore 和健康 Journal，固定跨进程锁顺序。
fn open_resources(
    config: &RuntimeConfig,
    session_id: &SessionId,
) -> Result<(SessionLease, Arc<ArtifactStore>, SessionJournal), RuntimeError> {
    let lease = acquire_lease(&config.storage_root, session_id)?;
    let artifacts = Arc::new(ArtifactStore::open(
        &config.storage_root,
        session_id.clone(),
        config.artifacts,
    )?);
    let journal = match SessionJournal::open_with_artifact_validator(
        &config.storage_root,
        session_id.clone(),
        config.journal,
        artifacts.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => return Err(RuntimeError::RecoveryRequired),
    };
    Ok((lease, artifacts, journal))
}

/// 非阻塞获取目标 Session 的独占 Runtime lease。
fn acquire_lease(root: &Path, session_id: &SessionId) -> Result<SessionLease, RuntimeError> {
    match SessionLease::try_acquire(root, session_id.clone())? {
        SessionLeaseAcquire::Acquired(lease) => Ok(lease),
        SessionLeaseAcquire::Busy { .. } => Err(RuntimeError::SessionBusy),
    }
}

/// 为资源层内部事件执行稳定 ID 与 sequence CAS 追加。
fn append_resource_event(
    journal: &SessionJournal,
    event_id: SessionEventId,
    event: SessionEvent,
) -> Result<(), RuntimeError> {
    append_resource_event_with_publisher(journal, None, event_id, event)
}

/// 为已经存在 Publisher 的 Runtime 内部事件追加并发布唯一权威回执。
fn append_runtime_resource_event(
    inner: &RuntimeSessionInner,
    event_id: SessionEventId,
    event: SessionEvent,
) -> Result<(), RuntimeError> {
    append_resource_event_with_publisher(&inner.journal, Some(&inner.publisher), event_id, event)
}

/// 执行资源事件 CAS 追加，并只为本次新追加的记录发布一次权威投递。
fn append_resource_event_with_publisher(
    journal: &SessionJournal,
    publisher: Option<&SessionEventPublisher>,
    event_id: SessionEventId,
    event: SessionEvent,
) -> Result<(), RuntimeError> {
    let mut expected_sequence = journal.state()?.last_sequence;
    for _ in 0..2 {
        match journal.append_idempotent(event_id.clone(), expected_sequence, event.clone())? {
            IdempotentAppendOutcome::Appended(receipt) => {
                if let Some(publisher) = publisher {
                    publisher.publish_authoritative(receipt.record)?;
                }
                return Ok(());
            }
            IdempotentAppendOutcome::AlreadyCommitted { .. } => return Ok(()),
            IdempotentAppendOutcome::SequenceConflict {
                actual_sequence, ..
            } => expected_sequence = actual_sequence,
            IdempotentAppendOutcome::EventIdConflict { .. } => {
                return Err(RuntimeError::RecoveryRequired);
            }
            IdempotentAppendOutcome::Indeterminate { .. } => {
                return Err(RuntimeError::RecoveryRequired);
            }
        }
    }
    Err(RuntimeError::RecoveryRequired)
}

/// 完成工具 Round 已知内容验证、序列化容量计算与一次性账本登记。
fn preflight_round(
    inner: &Arc<RuntimeSessionInner>,
    round: &AgentToolRoundPreflight,
) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
    let key = round_key_from_preflight(round).map_err(|_| preflight_unpersistable())?;
    preflight_round_candidate_with_completion(
        inner,
        key,
        round.completion(),
        round.assistant_message(),
        round.pre_tool_context(),
    )
}

/// 使用最小工具响应事实预检测试候选，保持容量回归聚焦于指定正文。
#[cfg(test)]
fn preflight_round_candidate(
    inner: &Arc<RuntimeSessionInner>,
    key: RoundKey,
    assistant_message: &Message,
    pre_tool_context: &[Message],
) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
    let completion = ModelRoundCompletion {
        metadata: keencode_model::ResponseMetadata::default(),
        usage: keencode_model::TokenUsage::unknown(),
        stop_reason: keencode_model::StopReason::ToolUse,
    };
    preflight_round_candidate_with_completion(
        inner,
        key,
        &completion,
        assistant_message,
        pre_tool_context,
    )
}

/// 对已经解包的不可变 Round 与响应事实执行容量预检。
fn preflight_round_candidate_with_completion(
    inner: &Arc<RuntimeSessionInner>,
    key: RoundKey,
    completion: &ModelRoundCompletion,
    assistant_message: &Message,
    pre_tool_context: &[Message],
) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
    let mut control = inner.control.lock().map_err(|_| preflight_unavailable())?;
    if control.lifecycle != RuntimeSessionLifecycle::Open || control.recovery_required {
        return Err(preflight_unavailable());
    }
    let state = inner.journal.state().map_err(|_| preflight_unavailable())?;
    validate_round_identity(&state, &key).map_err(|_| preflight_unpersistable())?;
    assistant_message
        .validate()
        .map_err(|_| preflight_unpersistable())?;
    for message in pre_tool_context {
        message.validate().map_err(|_| preflight_unpersistable())?;
    }
    if control.reservations.contains_key(&key) {
        return Err(preflight_unavailable());
    }

    let mut tool_calls = Vec::new();
    let mut tool_request_sha256 = BTreeMap::new();
    for block in &assistant_message.content {
        let ContentBlock::ToolCall { tool_call } = block else {
            continue;
        };
        let request_index =
            u32::try_from(tool_calls.len()).map_err(|_| preflight_unpersistable())?;
        let request_sha256 =
            canonical_sha256(&(request_index, tool_call)).map_err(|_| preflight_unpersistable())?;
        if tool_request_sha256
            .insert(tool_call.id.clone(), request_sha256)
            .is_some()
        {
            return Err(preflight_unpersistable());
        }
        tool_calls.push(tool_call);
    }
    if tool_calls.is_empty() || tool_calls.len() > TOOL_OUTPUT_LIMITS.max_round_content_blocks {
        return Err(preflight_unpersistable());
    }

    let mut probe = ArtifactProbe::default();
    let assistant = map_message(
        inner,
        &key,
        0,
        assistant_message,
        ArtifactMode::Probe,
        &mut probe,
    )
    .map_err(|_| preflight_unpersistable())?;
    let mut known_messages = vec![assistant];
    for (index, message) in pre_tool_context.iter().enumerate() {
        let position = index.checked_add(2).ok_or_else(preflight_unpersistable)?;
        known_messages.push(
            map_message(
                inner,
                &key,
                position,
                message,
                ArtifactMode::Probe,
                &mut probe,
            )
            .map_err(|_| preflight_unpersistable())?,
        );
    }
    let turn_id = TurnId::new(key.turn_id.clone()).map_err(|_| preflight_unpersistable())?;
    let source_agent_id = keencode_resources::AgentId::new(key.agent_id.clone())
        .map_err(|_| preflight_unpersistable())?;
    let event = SessionEvent::AtomicBatch {
        events: vec![
            SessionEvent::ModelRoundCompleted {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round: key.model_round,
                requested_model: key.model.clone(),
                metadata: completion.metadata.clone(),
                usage: completion.usage.clone(),
                stop_reason: completion.stop_reason.clone(),
            },
            SessionEvent::TranscriptSegmentCommitted {
                segment: TranscriptSegment {
                    turn_id,
                    source_agent_id,
                    model_round: key.model_round,
                    segment_index: key.segment_index,
                    expected_transcript_revision: state.transcript_revision,
                    messages: known_messages.clone(),
                },
            },
        ],
    };
    let placeholder_id =
        SessionEventId::new(PREFLIGHT_EVENT_ID).map_err(|_| preflight_unpersistable())?;
    let encoded = encoded_record_len(&state.session_id, &placeholder_id, &event)
        .map_err(|_| preflight_unpersistable())?;
    let reserve_cold_recovery_terminal =
        !control
            .turn_executions
            .get(&key.turn_id)
            .is_some_and(|execution| {
                matches!(execution, RuntimeTurnExecution::Running { .. })
                    && execution.reserves_terminal_record()
            });
    let budget = tool_round_persistence_budget(
        inner,
        &state,
        &key,
        &tool_calls,
        &known_messages,
        encoded,
        reserve_cold_recovery_terminal,
    )
    .map_err(|_| preflight_unpersistable())?;
    ensure_preflight_capacity(inner, &control, &state, &budget, &probe.missing_ids)?;

    let token = control
        .next_reservation_token
        .checked_add(1)
        .ok_or_else(preflight_unavailable)?;
    control.next_reservation_token = token;
    let known_content_sha256 = known_content_sha256(assistant_message, pre_tool_context)
        .map_err(|_| preflight_unpersistable())?;
    control.reservations.insert(
        key.clone(),
        ReservationEntry {
            token,
            known_content_sha256,
            pre_tool_context_count: pre_tool_context.len(),
            reserved_journal_bytes: budget.journal_bytes,
            reserved_journal_records: budget.journal_records,
            tool_request_sha256,
            missing_artifact_ids: probe.missing_ids,
            materialized_artifact_uses: BTreeMap::new(),
            reserved_unknown_artifacts: budget.unknown_artifacts,
            materialized_artifact_ids: BTreeSet::new(),
            committed_event_ids: BTreeSet::new(),
            reserved_state_items: budget.state_items,
            retained_event: None,
            abandoned_after_progress: false,
        },
    );
    drop(control);
    Ok(Box::new(RuntimeRoundReservation {
        inner: Arc::downgrade(inner),
        key,
        token,
    }))
}

/// 计算工具 Round 从请求生命周期到 Transcript 与恢复 Turn 终态的最坏持久化容量。
fn tool_round_persistence_budget(
    inner: &RuntimeSessionInner,
    state: &SessionState,
    key: &RoundKey,
    tool_calls: &[&ToolCall],
    known_messages: &[SessionMessage],
    known_round_event_bytes: u64,
    reserve_cold_recovery_terminal: bool,
) -> Result<ToolRoundPersistenceBudget, RuntimeError> {
    let maximum_unknown_artifact_bytes = TOOL_OUTPUT_LIMITS
        .max_text_bytes
        .max(TOOL_OUTPUT_LIMITS.max_image_decoded_bytes)
        as u64;
    if inner.config.artifacts.max_artifact_bytes < maximum_unknown_artifact_bytes {
        return Err(RuntimeError::RecoveryRequired);
    }
    if tool_calls.is_empty() || tool_calls.len() > TOOL_OUTPUT_LIMITS.max_round_content_blocks {
        return Err(RuntimeError::RecoveryRequired);
    }

    let placeholder_id = SessionEventId::new(PREFLIGHT_EVENT_ID)?;
    let mut journal_bytes = known_round_event_bytes;
    let mut maximum_completed_base_bytes = 0_u64;
    for (index, call) in tool_calls.iter().enumerate() {
        let request_index = u32::try_from(index).map_err(|_| RuntimeError::RecoveryRequired)?;
        let request_id = request_id_for_key(key, &call.id)?;
        let lifecycle = [
            SessionEvent::ToolRequested {
                request: keencode_resources::ToolRequest {
                    request_id: request_id.clone(),
                    turn_id: TurnId::new(key.turn_id.clone())?,
                    agent_id: keencode_resources::AgentId::new(key.agent_id.clone())?,
                    model_round: key.model_round,
                    request_index,
                    model_tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    effect: ToolEffect::ChangesState,
                },
            },
            SessionEvent::ToolExecutionStarted {
                request_id: request_id.clone(),
            },
            SessionEvent::ToolCompleted {
                request_id,
                outcome: ToolOutcome {
                    status: ToolCompletionStatus::Succeeded,
                    result: PersistedToolResult {
                        tool_call_id: call.id.clone(),
                        content: Vec::new(),
                        is_error: false,
                    },
                },
            },
        ];
        for (event_index, event) in lifecycle.iter().enumerate() {
            let encoded = encoded_record_len(&state.session_id, &placeholder_id, event)?;
            if encoded > inner.config.journal.max_event_bytes {
                return Err(RuntimeError::RecoveryRequired);
            }
            if event_index == lifecycle.len() - 1 {
                maximum_completed_base_bytes = maximum_completed_base_bytes.max(encoded);
            }
            journal_bytes = journal_bytes
                .checked_add(encoded)
                .ok_or(RuntimeError::RecoveryRequired)?;
        }
    }

    if reserve_cold_recovery_terminal {
        let recovery_turn_id = TurnId::new(key.turn_id.clone())?;
        let recovery_agent_id = keencode_resources::AgentId::new(key.agent_id.clone())?;
        let recovery_turn_event =
            recovery_turn_stopped_event(&recovery_turn_id, &recovery_agent_id);
        let recovery_turn_event_id = recovery_event_id("turn-stopped", &key.turn_id)?;
        let recovery_turn_event_bytes = encoded_record_len(
            &state.session_id,
            &recovery_turn_event_id,
            &recovery_turn_event,
        )?;
        if recovery_turn_event_bytes > inner.config.journal.max_event_bytes {
            return Err(RuntimeError::RecoveryRequired);
        }
        journal_bytes = journal_bytes
            .checked_add(recovery_turn_event_bytes)
            .ok_or(RuntimeError::RecoveryRequired)?;
    }

    let round_json_bytes = u64::try_from(TOOL_OUTPUT_LIMITS.max_round_json_bytes)
        .map_err(|_| RuntimeError::RecoveryRequired)?;
    let mapped_expansion_bytes = u64::try_from(TOOL_OUTPUT_LIMITS.max_round_content_blocks)
        .map_err(|_| RuntimeError::RecoveryRequired)?
        .checked_mul(PERSISTED_CONTENT_BLOCK_EXPANSION_BYTES)
        .ok_or(RuntimeError::RecoveryRequired)?;
    let unknown_copy_bytes = round_json_bytes
        .checked_add(mapped_expansion_bytes)
        .ok_or(RuntimeError::RecoveryRequired)?;
    let unknown_event_reserve_bytes = unknown_copy_bytes
        .checked_add(TOOL_ROUND_FIXED_WIRE_SLACK_BYTES)
        .ok_or(RuntimeError::RecoveryRequired)?;
    let worst_round_event_bytes = known_round_event_bytes
        .checked_add(unknown_event_reserve_bytes)
        .ok_or(RuntimeError::RecoveryRequired)?;
    let worst_completed_event_bytes = maximum_completed_base_bytes
        .checked_add(unknown_event_reserve_bytes)
        .ok_or(RuntimeError::RecoveryRequired)?;
    if worst_round_event_bytes > inner.config.journal.max_event_bytes
        || worst_completed_event_bytes > inner.config.journal.max_event_bytes
    {
        return Err(RuntimeError::RecoveryRequired);
    }
    journal_bytes = journal_bytes
        .checked_add(
            unknown_event_reserve_bytes
                .checked_mul(2)
                .ok_or(RuntimeError::RecoveryRequired)?,
        )
        .ok_or(RuntimeError::RecoveryRequired)?;
    let tool_count = u64::try_from(tool_calls.len()).map_err(|_| RuntimeError::RecoveryRequired)?;
    let journal_records = tool_count
        .checked_mul(3)
        .and_then(|records| records.checked_add(if reserve_cold_recovery_terminal { 2 } else { 1 }))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let known_state_items = known_messages
        .iter()
        .fold(StateCollectionItems::default(), |total, message| {
            total.saturating_add(state_collection_message_items(message))
        });
    let tool_count = tool_calls.len();
    let duplicated_tool_argument_items = tool_calls.iter().fold(0_usize, |total, call| {
        total.saturating_add(json_collection_items(&call.arguments))
    });
    let state_items = known_state_items.saturating_add(StateCollectionItems {
        transcript: 1,
        messages: 1_usize
            .saturating_add(TOOL_OUTPUT_LIMITS.max_post_hook_additions)
            .saturating_add(TOOL_ROUND_OPTIONAL_SUMMARY_MESSAGES),
        transcript_segments: 1,
        model_rounds: 1,
        tools: tool_count,
        message_parts: tool_count
            .saturating_add(TOOL_OUTPUT_LIMITS.max_post_hook_additions)
            .saturating_add(TOOL_ROUND_OPTIONAL_SUMMARY_MESSAGES),
        message_tool_result_content: TOOL_OUTPUT_LIMITS.max_round_content_blocks,
        tool_outcome_result_content: TOOL_OUTPUT_LIMITS.max_round_content_blocks,
        json_collection_items: duplicated_tool_argument_items,
        ..StateCollectionItems::default()
    });
    Ok(ToolRoundPersistenceBudget {
        journal_bytes,
        journal_records,
        unknown_artifacts: TOOL_OUTPUT_LIMITS.max_round_content_blocks,
        state_items,
    })
}

/// 把单个 Agent 事件映射、容量校验并幂等追加到资源 Journal。
fn commit_agent_event(
    inner: &Arc<RuntimeSessionInner>,
    event: &AgentCommitEvent,
) -> Result<(), AgentCommitSinkError> {
    let mut control = inner.control.lock().map_err(|_| commit_indeterminate())?;
    if event.session_id().as_str() != inner.artifacts.session_id().as_str() {
        return Err(commit_rejected());
    }
    let is_registered_running_turn = control
        .turn_executions
        .get(event.turn_id().as_str())
        .is_some_and(|execution| matches!(execution, RuntimeTurnExecution::Running { .. }));
    if control.lifecycle == RuntimeSessionLifecycle::Closed
        || (control.lifecycle == RuntimeSessionLifecycle::Closing && !is_registered_running_turn)
    {
        return Err(commit_rejected());
    }
    let event_key = event.event_id().as_str().to_owned();
    let event_id = SessionEventId::new(event.event_id().as_str()).map_err(|_| commit_rejected())?;
    let reconciling = control.pending_indeterminate.contains(&event_key);
    if !recovery_gate_allows_event(&control, &event_key) {
        return Err(commit_indeterminate());
    }
    let state = match inner.journal.state() {
        Ok(state) => state,
        Err(ResourceError::CorruptReadOnly) => {
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            return Err(commit_indeterminate());
        }
        Err(_) if reconciling => return Err(commit_indeterminate()),
        Err(_) => return Err(commit_rejected()),
    };
    let previous = control.mappings.get(&event_key);
    let mapping_known = previous.is_some();
    let hints = previous.map_or_else(MappingHints::default, |record| MappingHints {
        transcript_revision: record.transcript_revision,
        compaction_digest: record.compaction_digest.clone(),
    });
    let mut probe = ArtifactProbe::default();
    let mapped_probe = map_agent_event(
        inner,
        event,
        &state,
        &hints,
        ArtifactMode::Probe,
        &mut probe,
    )
    .map_err(|_| commit_rejected())?;
    let mapped_sha256 = canonical_sha256(&mapped_probe).map_err(|_| commit_rejected())?;
    let event_state_items = state_collection_event_items(&mapped_probe);
    if previous.is_some_and(|record| record.event_sha256 != mapped_sha256) {
        return Err(commit_rejected());
    }
    let round_key = round_key_from_commit(event).map_err(|_| commit_rejected())?;
    let reservation_key = validate_round_reservation(&control, event, round_key.as_ref())?;
    let encoded = encoded_record_len(&state.session_id, &event_id, &mapped_probe)
        .map_err(|_| commit_rejected())?;
    if !mapping_known {
        ensure_commit_capacity(
            inner,
            &control,
            &state,
            encoded,
            reservation_key.as_ref(),
            &probe.missing_ids,
            event_state_items,
        )?;
    }

    let mut commit_probe = ArtifactProbe::default();
    let mapped_result = map_agent_event(
        inner,
        event,
        &state,
        &hints,
        ArtifactMode::Commit,
        &mut commit_probe,
    );
    let materialized_artifacts = match materialized_probe_artifacts(inner, &probe) {
        Ok(materialized) => materialized,
        Err(_) => {
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            return Err(commit_indeterminate());
        }
    };
    if charge_materialized_reservation_artifacts(
        &mut control,
        reservation_key.as_ref(),
        &materialized_artifacts,
        &probe.missing_uses,
    )
    .is_err()
    {
        control.hard_recovery_required = true;
        refresh_recovery_required(&mut control);
        return Err(commit_indeterminate());
    }
    let mapped = mapped_result.map_err(|_| commit_rejected())?;
    if canonical_sha256(&mapped).map_err(|_| commit_rejected())? != mapped_sha256 {
        return Err(commit_rejected());
    }
    control
        .mappings
        .entry(event_key.clone())
        .or_insert_with(|| MappingRecord {
            event_sha256: mapped_sha256,
            transcript_revision: mapping_transcript_revision(event, &state),
            compaction_digest: mapping_compaction_digest(event, &state),
        });

    let appended_record =
        match inner
            .journal
            .append_idempotent(event_id, state.last_sequence, mapped)
        {
            Ok(IdempotentAppendOutcome::Appended(receipt)) => Some(receipt.record),
            Ok(IdempotentAppendOutcome::AlreadyCommitted { .. }) => None,
            Ok(IdempotentAppendOutcome::Indeterminate { .. }) => {
                mark_event_indeterminate(&mut control, &event_key);
                return Err(commit_indeterminate());
            }
            Ok(IdempotentAppendOutcome::EventIdConflict { .. }) if reconciling => {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
                return Err(commit_indeterminate());
            }
            Ok(IdempotentAppendOutcome::EventIdConflict { .. }) => {
                control.mappings.remove(&event_key);
                return Err(commit_rejected());
            }
            Ok(IdempotentAppendOutcome::SequenceConflict { .. }) if reconciling => {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
                return Err(commit_indeterminate());
            }
            Ok(IdempotentAppendOutcome::SequenceConflict { .. }) => {
                control.mappings.remove(&event_key);
                return Err(commit_rejected());
            }
            Err(ResourceError::CorruptReadOnly) => {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
                return Err(commit_indeterminate());
            }
            Err(_) if reconciling => return Err(commit_indeterminate()),
            Err(_) => {
                control.mappings.remove(&event_key);
                return Err(commit_rejected());
            }
        };
    if charge_confirmed_reservation_event(
        &mut control,
        reservation_key.as_ref(),
        &event_key,
        encoded,
        event_state_items,
    )
    .is_err()
    {
        control.hard_recovery_required = true;
        refresh_recovery_required(&mut control);
        return Err(commit_indeterminate());
    }
    mark_event_confirmed(&mut control, &event_key);
    if let Some(record) = appended_record {
        if inner.publisher.publish_authoritative(record).is_err() {
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            return Err(commit_indeterminate());
        }
    }
    if let AgentCommitEventKind::ToolCompleted { tool_call_id, .. } = event.kind()
        && let Some(round_key) = round_key.as_ref()
        && let Ok(request_id) = request_id_for_key(round_key, tool_call_id.as_str())
    {
        // 工具已经形成明确终态后，未应用的文件变更证据仍由 Journal 保留，
        // 但对应 Applied 备用容量不再阻塞其他正常工作。
        file_changes::release_file_change_reservation(&mut control, &request_id);
    }
    Ok(())
}

/// 为相同 Turn 的重复调用冻结全部会影响执行语义的 Provider 中立输入。
fn runtime_turn_request_sha256(turn: &RuntimeTurnRequest) -> Result<String, RuntimeError> {
    let plan_guard = match turn.request.plan_guard().state() {
        keencode_agent::PlanGuardState::Inactive => "inactive",
        keencode_agent::PlanGuardState::ReadOnly => "read_only",
    };
    canonical_sha256(&(
        "keencode/runtime-turn-request/v1",
        turn.request.session_id().as_str(),
        turn.request.turn_id().as_str(),
        turn.request.source_agent_id().as_str(),
        turn.request.model_request(),
        &turn.input_messages,
        &turn.root_turn_id,
        &turn.parent_turn_id,
        &turn.prompt_summary,
        &turn.spawned_agent,
        plan_guard,
    ))
}

/// 校验根 Agent 请求采用的计划守卫和当前 Session 权威快照一致。
fn validate_runtime_turn_session_modes(
    state: &SessionState,
    turn: &RuntimeTurnRequest,
) -> Result<(), RuntimeError> {
    let request_is_read_only = matches!(
        turn.request.plan_guard().state(),
        keencode_agent::PlanGuardState::ReadOnly
    );
    let plan_matches = !state.plan.enabled || request_is_read_only;
    if plan_matches {
        Ok(())
    } else {
        Err(RuntimeError::InvalidTurnRequest)
    }
}

/// 仅允许已有历史 Turn 的已注册子 Agent 在 followup 时省略初始消息。
fn validate_runtime_turn_input(
    state: &SessionState,
    turn: &RuntimeTurnRequest,
    source_agent_id: &keencode_resources::AgentId,
) -> Result<(), RuntimeError> {
    if !turn.input_messages.is_empty() {
        return Ok(());
    }
    let is_followup_child = source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        && turn.spawned_agent.is_none()
        && state
            .sub_agents
            .get(source_agent_id)
            .is_some_and(|agent| agent.current_turn_id.is_some());
    if is_followup_child {
        Ok(())
    } else {
        Err(RuntimeError::InvalidTurnRequest)
    }
}

/// 以 Probe 或 Commit 模式构造 Turn 起点、可选子 Agent Running 状态和全部输入消息。
#[allow(clippy::too_many_arguments)]
fn runtime_input_event(
    inner: &RuntimeSessionInner,
    input_key: &RoundKey,
    input_messages: &[Message],
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
    root_turn_id: &TurnId,
    parent_turn_id: Option<&TurnId>,
    prompt_summary: &str,
    spawned_agent: Option<&SubAgentState>,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<SessionEvent, RuntimeError> {
    let is_child = source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID;
    let mut events = Vec::with_capacity(
        input_messages
            .len()
            .saturating_add(if is_child { 2 } else { 1 })
            .saturating_add(usize::from(spawned_agent.is_some())),
    );
    if let Some(agent) = spawned_agent {
        events.push(SessionEvent::SubAgentSpawned {
            agent: agent.clone(),
        });
    }
    events.push(SessionEvent::TurnStarted {
        turn_id: turn_id.clone(),
        source_agent_id: source_agent_id.clone(),
        root_turn_id: root_turn_id.clone(),
        parent_turn_id: parent_turn_id.cloned(),
        prompt_summary: prompt_summary.to_owned(),
    });
    if is_child {
        events.push(SessionEvent::SubAgentStatusChanged {
            agent_id: source_agent_id.clone(),
            turn_id: Some(turn_id.clone()),
            status: SubAgentStatus::Running,
            result_summary: None,
        });
    }
    for (position, message) in input_messages.iter().enumerate() {
        events.push(SessionEvent::MessageAdded {
            message: map_message(inner, input_key, position, message, mode, probe)?,
        });
        #[cfg(test)]
        if matches!(mode, ArtifactMode::Commit)
            && take_runtime_input_commit_fault(turn_id, position.saturating_add(1))
        {
            return Err(RuntimeError::RecoveryRequired);
        }
    }
    Ok(SessionEvent::AtomicBatch { events })
}

/// 在任何 Artifact 写入前用资源层同一 reducer 验证生命周期候选与当前状态一致。
fn validate_runtime_event_candidate(
    state: &SessionState,
    event_id: &SessionEventId,
    event: &SessionEvent,
) -> Result<(), RuntimeError> {
    let sequence = state
        .last_sequence
        .checked_add(1)
        .ok_or(RuntimeError::TurnUnpersistable)?;
    let mut candidate = state.clone();
    reduce_record(
        &mut candidate,
        SessionEventRecord {
            schema: SESSION_EVENT_SCHEMA.to_owned(),
            version: SESSION_EVENT_VERSION,
            event_id: event_id.clone(),
            session: state.session_id.clone(),
            sequence,
            time_unix_ms: state.updated_at_unix_ms,
            event: event.clone(),
        },
    )
    .map_err(|_| RuntimeError::InvalidTurnRequest)
}

/// 返回带子 Agent 状态配对的正常 Turn 终态事件。
fn runtime_completed_event(
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
    final_message: Option<String>,
) -> SessionEvent {
    let completed = SessionEvent::TurnCompleted {
        turn_id: turn_id.clone(),
    };
    if source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
        completed
    } else {
        SessionEvent::AtomicBatch {
            events: vec![
                completed,
                SessionEvent::SubAgentStatusChanged {
                    agent_id: source_agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    status: SubAgentStatus::Completed,
                    result_summary: final_message,
                },
            ],
        }
    }
}

/// 提取正常完成响应中的普通文本，并按子 Agent 终态摘要上限截断。
fn model_response_text(response: &ModelResponse) -> Option<String> {
    let text = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| truncate_runtime_terminal_message(&text))
}

/// 返回带子 Agent 状态配对的非正常 Turn 终态事件。
fn runtime_stopped_event(
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
    reason: TurnStopReason,
    message: String,
) -> SessionEvent {
    let stopped = SessionEvent::TurnStopped {
        turn_id: turn_id.clone(),
        reason,
        message: message.clone(),
    };
    if source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
        return stopped;
    }
    let (status, result_summary) = match reason {
        TurnStopReason::Cancelled => (SubAgentStatus::Interrupted, None),
        TurnStopReason::Failed
        | TurnStopReason::LimitReached
        | TurnStopReason::ContextBlocked
        | TurnStopReason::ModelOutputLimit
        | TurnStopReason::ModelRefusal => (SubAgentStatus::Failed, Some(message)),
    };
    SessionEvent::AtomicBatch {
        events: vec![
            stopped,
            SessionEvent::SubAgentStatusChanged {
                agent_id: source_agent_id.clone(),
                turn_id: Some(turn_id.clone()),
                status,
                result_summary,
            },
        ],
    }
}

/// 在 UTF-8 字符边界截断 Runtime 终态说明，保持资源层子 Agent 摘要上限。
fn truncate_runtime_terminal_message(message: &str) -> String {
    if message.len() <= MAX_RUNTIME_TERMINAL_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_RUNTIME_TERMINAL_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    message[..end].to_owned()
}

/// 以最坏失败正文计算根或子 Agent 唯一终态所需的保守 Journal 字节数。
fn runtime_terminal_reservation_bytes(
    session_id: &SessionId,
    event_id: &SessionEventId,
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
) -> Result<u64, RuntimeError> {
    let event = runtime_stopped_event(
        turn_id,
        source_agent_id,
        TurnStopReason::Failed,
        "\0".repeat(MAX_RUNTIME_TERMINAL_MESSAGE_BYTES),
    );
    encoded_record_len(session_id, event_id, &event)
}

/// 测试中为指定 Turn 注入“若干消息已完成 Artifact 物化后失败”的一次性故障。
#[cfg(test)]
fn inject_runtime_input_commit_fault(turn_id: &TurnId, after_messages: usize) {
    inject_runtime_input_commit_faults(turn_id, after_messages, 1);
}

/// 测试中为指定 Turn 注入固定次数的输入 Artifact 物化后失败。
#[cfg(test)]
fn inject_runtime_input_commit_faults(
    turn_id: &TurnId,
    after_messages: usize,
    remaining_failures: usize,
) {
    runtime_input_commit_faults()
        .lock()
        .expect("Runtime 输入故障锁应可用")
        .insert(
            turn_id.as_str().to_owned(),
            RuntimeInputCommitFault {
                after_messages,
                remaining_failures,
            },
        );
}

/// 单个 Runtime 输入测试故障的触发位置和剩余次数。
#[cfg(test)]
struct RuntimeInputCommitFault {
    /// 已经完成映射的消息数量。
    after_messages: usize,
    /// 尚需触发的故障次数。
    remaining_failures: usize,
}

/// 返回按 Turn 隔离的 Runtime 输入 Artifact 提交测试故障集合。
#[cfg(test)]
fn runtime_input_commit_faults() -> &'static Mutex<BTreeMap<String, RuntimeInputCommitFault>> {
    static FAULTS: std::sync::OnceLock<Mutex<BTreeMap<String, RuntimeInputCommitFault>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 在指定消息数量已经映射后消费当前 Turn 的一次性输入提交故障。
#[cfg(test)]
fn take_runtime_input_commit_fault(turn_id: &TurnId, mapped_messages: usize) -> bool {
    let mut faults = runtime_input_commit_faults()
        .lock()
        .expect("Runtime 输入故障锁应可用");
    let Some(fault) = faults.get_mut(turn_id.as_str()) else {
        return false;
    };
    if fault.after_messages != mapped_messages || fault.remaining_failures == 0 {
        return false;
    }
    fault.remaining_failures -= 1;
    if fault.remaining_failures == 0 {
        faults.remove(turn_id.as_str());
    }
    true
}

/// 测试中按完整事件标识注入一次生命周期追加结果不确定，避免并行用例互相干扰。
#[cfg(test)]
fn inject_runtime_lifecycle_indeterminate(event_id: &SessionEventId) {
    runtime_lifecycle_indeterminate_faults()
        .lock()
        .expect("Runtime 生命周期故障锁应可用")
        .insert(event_id.as_str().to_owned());
}

/// 返回测试进程共享但按事件标识隔离的一次性生命周期故障集合。
#[cfg(test)]
fn runtime_lifecycle_indeterminate_faults() -> &'static Mutex<BTreeSet<String>> {
    static FAULTS: std::sync::OnceLock<Mutex<BTreeSet<String>>> = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// 仅消费与当前事件完全匹配的一次性测试故障。
#[cfg(test)]
fn take_runtime_lifecycle_indeterminate(event_id: &SessionEventId) -> bool {
    runtime_lifecycle_indeterminate_faults()
        .lock()
        .expect("Runtime 生命周期故障锁应可用")
        .remove(event_id.as_str())
}

/// 测试中按完整事件标识注入一次确定未追加的生命周期资源失败。
#[cfg(test)]
fn inject_runtime_lifecycle_failure(event_id: &SessionEventId) {
    runtime_lifecycle_failure_faults()
        .lock()
        .expect("Runtime 生命周期明确故障锁应可用")
        .insert(event_id.as_str().to_owned());
}

/// 返回按事件标识隔离的确定性生命周期资源故障集合。
#[cfg(test)]
fn runtime_lifecycle_failure_faults() -> &'static Mutex<BTreeSet<String>> {
    static FAULTS: std::sync::OnceLock<Mutex<BTreeSet<String>>> = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// 仅消费与当前事件完全匹配的一次确定性生命周期资源故障。
#[cfg(test)]
fn take_runtime_lifecycle_failure(event_id: &SessionEventId) -> bool {
    runtime_lifecycle_failure_faults()
        .lock()
        .expect("Runtime 生命周期明确故障锁应可用")
        .remove(event_id.as_str())
}

/// 测试中让指定生命周期事件在真实追加可见后返回一次结果不确定。
#[cfg(test)]
fn inject_runtime_lifecycle_visible_indeterminate(event_id: &SessionEventId) {
    runtime_lifecycle_visible_indeterminate_faults()
        .lock()
        .expect("Runtime 可见生命周期故障锁应可用")
        .insert(event_id.as_str().to_owned());
}

/// 返回按事件标识隔离的追加后可见测试故障集合。
#[cfg(test)]
fn runtime_lifecycle_visible_indeterminate_faults() -> &'static Mutex<BTreeSet<String>> {
    static FAULTS: std::sync::OnceLock<Mutex<BTreeSet<String>>> = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// 消费与当前事件完全匹配的一次性追加后可见测试故障。
#[cfg(test)]
fn take_runtime_lifecycle_visible_indeterminate(event_id: &SessionEventId) -> bool {
    runtime_lifecycle_visible_indeterminate_faults()
        .lock()
        .expect("Runtime 可见生命周期故障锁应可用")
        .remove(event_id.as_str())
}

/// 在 Runtime 控制锁内提交或对账生命周期事件，并维护同一恢复栅栏。
fn commit_runtime_lifecycle_event(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    event_id: SessionEventId,
    event: SessionEvent,
    control_operation: bool,
) -> Result<(), RuntimeError> {
    let event_key = event_id.as_str().to_owned();
    if !recovery_gate_allows_event(control, &event_key) {
        return Err(RuntimeError::RecoveryRequired);
    }
    #[cfg(test)]
    if take_runtime_lifecycle_failure(&event_id) {
        return Err(ResourceError::Json("测试注入 Runtime 生命周期明确失败".to_owned()).into());
    }
    #[cfg(test)]
    if take_runtime_lifecycle_indeterminate(&event_id) {
        mark_event_indeterminate(control, &event_key);
        return Err(RuntimeError::RecoveryRequired);
    }
    #[cfg(test)]
    let visible_indeterminate = take_runtime_lifecycle_visible_indeterminate(&event_id);
    if control_operation && !inner.journal.contains_event_id(&event_id)? {
        let state = inner.journal.state()?;
        ensure_control_event_capacity(inner, control, &state, &event_id, &event)?;
    }
    let mut expected_sequence = match inner.journal.state() {
        Ok(state) => state.last_sequence,
        Err(ResourceError::CorruptReadOnly) => {
            control.hard_recovery_required = true;
            mark_event_indeterminate(control, &event_key);
            return Err(RuntimeError::RecoveryRequired);
        }
        Err(error) => return Err(RuntimeError::Resource(error)),
    };
    for _ in 0..2 {
        let appended_record = match inner.journal.append_idempotent(
            event_id.clone(),
            expected_sequence,
            event.clone(),
        ) {
            Ok(IdempotentAppendOutcome::Appended(receipt)) => Some(receipt.record),
            Ok(IdempotentAppendOutcome::AlreadyCommitted { .. }) => None,
            Ok(IdempotentAppendOutcome::SequenceConflict {
                actual_sequence, ..
            }) => {
                expected_sequence = actual_sequence;
                continue;
            }
            Ok(IdempotentAppendOutcome::Indeterminate { .. }) => {
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Ok(IdempotentAppendOutcome::EventIdConflict { .. }) => {
                if control_operation {
                    return Err(RuntimeError::ControlOperationConflict);
                }
                control.hard_recovery_required = true;
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Err(ResourceError::CorruptReadOnly) => {
                control.hard_recovery_required = true;
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Err(error) => return Err(RuntimeError::Resource(error)),
        };
        if let Some(record) = appended_record {
            if inner.publisher.publish_authoritative(record).is_err() {
                control.hard_recovery_required = true;
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
        }
        #[cfg(test)]
        if visible_indeterminate {
            mark_event_indeterminate(control, &event_key);
            return Err(RuntimeError::RecoveryRequired);
        }
        mark_event_confirmed(control, &event_key);
        return Ok(());
    }
    control.hard_recovery_required = true;
    mark_event_indeterminate(control, &event_key);
    Err(RuntimeError::RecoveryRequired)
}

/// 为 Runtime 自己提交的 Turn 起止事件生成稳定且互不碰撞的幂等标识。
fn runtime_lifecycle_event_id(
    session_id: &SessionId,
    turn_id: &TurnId,
    phase: &'static str,
) -> Result<SessionEventId, RuntimeError> {
    let digest = canonical_sha256(&(
        "keencode/runtime-turn-lifecycle/v1",
        session_id.as_str(),
        turn_id.as_str(),
        phase,
    ))?;
    SessionEventId::new(format!("runtime-{phase}-{digest}")).map_err(RuntimeError::from)
}

/// 为尚未启动 Turn 终态生成不随请求正文变化的稳定 Journal 批次身份。
fn runtime_unstarted_turn_termination_event_id(
    session_id: &SessionId,
    agent_id: &keencode_resources::AgentId,
    turn_id: &TurnId,
) -> Result<SessionEventId, RuntimeError> {
    let digest = canonical_sha256(&(
        "keencode/runtime-unstarted-turn-termination/v1",
        session_id.as_str(),
        agent_id.as_str(),
        turn_id.as_str(),
    ))?;
    SessionEventId::new(format!("runtime-unstarted-termination-{digest}"))
        .map_err(RuntimeError::from)
}

/// 校验尚未启动 Turn 终态补偿的输入身份与单层子 Agent 约束。
fn validate_unstarted_turn_termination_request(
    session_id: &SessionId,
    request: &UnstartedTurnTerminationRequest,
) -> Result<(), RuntimeError> {
    if request.agent.agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
        || request.agent.parent_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        || request.parent_turn_id != request.root_turn_id
        || request.turn_id == request.root_turn_id
        || request.turn_id == request.parent_turn_id
        || request.agent.task.trim().is_empty()
        || request.prompt_summary.trim().is_empty()
        || !valid_unstarted_turn_agent_path(&request.agent.agent_path)
    {
        return Err(RuntimeError::InvalidTurnRequest);
    }
    if matches!(&request.termination, UnstartedTurnTermination::Failed { message } if message.trim().is_empty())
    {
        return Err(RuntimeError::InvalidTurnRequest);
    }
    if session_id.as_str().is_empty() {
        return Err(RuntimeError::InvalidTurnRequest);
    }
    Ok(())
}

/// 校验新补偿批次不会越过现有 Runtime 权威 Turn 或替换其他 Agent 身份。
fn validate_unstarted_turn_termination_state(
    state: &SessionState,
    request: &UnstartedTurnTerminationRequest,
) -> Result<(), RuntimeError> {
    if state.turns.contains_key(&request.turn_id) {
        // Journal 已有该 Turn 时，可能是正常运行或已经形成终态；两者都不能由
        // Collaboration 的外部未启动终态结果静默覆盖。
        return Err(RuntimeError::RecoveryRequired);
    }
    let root = state
        .turns
        .get(&request.root_turn_id)
        .ok_or(RuntimeError::RecoveryRequired)?;
    if root.source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        || root.turn_id != request.root_turn_id
        || root.root_turn_id != request.root_turn_id
        || root.parent_turn_id.is_some()
    {
        return Err(RuntimeError::RecoveryRequired);
    }
    let parent = state
        .turns
        .get(&request.parent_turn_id)
        .ok_or(RuntimeError::RecoveryRequired)?;
    if parent.root_turn_id != request.root_turn_id
        || parent.source_agent_id != request.agent.parent_agent_id
    {
        return Err(RuntimeError::RecoveryRequired);
    }
    if let Some(existing) = state.sub_agents.get(&request.agent.agent_id) {
        if request.initial_task {
            // InitialTask 只能为 Journal 中尚不存在的 Agent 创建身份；已有身份时
            // 不能把本次请求伪装成首次分派并借幂等事件覆盖任务定义。
            return Err(RuntimeError::RecoveryRequired);
        }
        if existing.parent_agent_id != request.agent.parent_agent_id
            || existing.agent_path != request.agent.agent_path
            || existing.task != request.agent.task
            || matches!(
                existing.status,
                SubAgentStatus::Running | SubAgentStatus::Waiting
            )
            || existing.status == SubAgentStatus::Stopped
        {
            return Err(RuntimeError::RecoveryRequired);
        }
    } else if !request.initial_task {
        // Followup/Retry 不得凭空重建已经丢失的子 Agent 定义。
        return Err(RuntimeError::RecoveryRequired);
    }
    Ok(())
}

/// 校验已经存在的尚未启动终态事件及其 Agent 不可变身份，防止稳定事件 ID
/// 把不同任务正文或不同 Turn 谱系误判为幂等重放。
fn validate_known_unstarted_turn_termination(
    journal: &SessionJournal,
    state: &SessionState,
    request: &UnstartedTurnTerminationRequest,
    event_id: &SessionEventId,
) -> Result<(), RuntimeError> {
    let existing_agent = state
        .sub_agents
        .get(&request.agent.agent_id)
        .ok_or(RuntimeError::RecoveryRequired)?;
    if existing_agent.parent_agent_id != request.agent.parent_agent_id
        || existing_agent.agent_path != request.agent.agent_path
        || existing_agent.task != request.agent.task
    {
        return Err(RuntimeError::RecoveryRequired);
    }

    let persisted = find_event_by_id(journal, event_id)?;
    let SessionEvent::AtomicBatch { events } = &persisted.event else {
        return Err(RuntimeError::RecoveryRequired);
    };
    let includes_spawn = events
        .iter()
        .any(|event| matches!(event, SessionEvent::SubAgentSpawned { .. }));
    if request.initial_task != includes_spawn {
        return Err(RuntimeError::RecoveryRequired);
    }
    let expected = unstarted_turn_termination_events(request, includes_spawn)?;
    if persisted.event != expected {
        return Err(RuntimeError::RecoveryRequired);
    }
    Ok(())
}

/// 从权威 Journal 分页查找已知幂等事件；索引与日志不一致时按恢复错误处理。
fn find_event_by_id(
    journal: &SessionJournal,
    event_id: &SessionEventId,
) -> Result<SessionEventRecord, RuntimeError> {
    let mut after_sequence = None;
    loop {
        let page = journal.read_page(after_sequence, MAX_REPLAY_PAGE_RECORDS)?;
        if let Some(record) = page
            .records
            .into_iter()
            .find(|record| record.event_id == *event_id)
        {
            return Ok(record);
        }
        if !page.has_more {
            return Err(RuntimeError::RecoveryRequired);
        }
        after_sequence = page.next_after;
        if after_sequence.is_none() {
            return Err(RuntimeError::RecoveryRequired);
        }
    }
}

/// 先通过 Journal 索引判断事件是否存在，再读取并返回完整权威记录。
fn find_committed_event(
    journal: &SessionJournal,
    event_id: &SessionEventId,
) -> Result<Option<SessionEventRecord>, RuntimeError> {
    if !journal.contains_event_id(event_id)? {
        return Ok(None);
    }
    find_event_by_id(journal, event_id).map(Some)
}

/// 依据 Journal 现有 Agent 状态构造完整尚未启动终态原子批次。
fn unstarted_turn_termination_events(
    request: &UnstartedTurnTerminationRequest,
    include_spawn: bool,
) -> Result<SessionEvent, RuntimeError> {
    let task = request.agent.task.clone();
    if task.trim().is_empty() {
        return Err(RuntimeError::RecoveryRequired);
    }
    let source_agent_id = request.agent.agent_id.clone();
    let mut events = Vec::with_capacity(if include_spawn { 5 } else { 4 });
    if include_spawn {
        events.push(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: source_agent_id.clone(),
                parent_agent_id: request.agent.parent_agent_id.clone(),
                agent_path: request.agent.agent_path.clone(),
                task,
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        });
    }
    events.push(SessionEvent::TurnStarted {
        turn_id: request.turn_id.clone(),
        source_agent_id: source_agent_id.clone(),
        root_turn_id: request.root_turn_id.clone(),
        parent_turn_id: Some(request.parent_turn_id.clone()),
        prompt_summary: request.prompt_summary.clone(),
    });
    events.push(SessionEvent::SubAgentStatusChanged {
        agent_id: source_agent_id.clone(),
        turn_id: Some(request.turn_id.clone()),
        status: SubAgentStatus::Running,
        result_summary: None,
    });
    let (reason, message, status, result_summary) = match &request.termination {
        UnstartedTurnTermination::Interrupted => (
            TurnStopReason::Cancelled,
            UNSTARTED_TURN_INTERRUPTION_MESSAGE.to_owned(),
            SubAgentStatus::Interrupted,
            None,
        ),
        UnstartedTurnTermination::Failed { message } => (
            TurnStopReason::Failed,
            message.clone(),
            SubAgentStatus::Failed,
            Some(message.clone()),
        ),
    };
    events.push(SessionEvent::TurnStopped {
        turn_id: request.turn_id.clone(),
        reason,
        message,
    });
    events.push(SessionEvent::SubAgentStatusChanged {
        agent_id: source_agent_id,
        turn_id: Some(request.turn_id.clone()),
        status,
        result_summary,
    });
    Ok(SessionEvent::AtomicBatch { events })
}

/// 尚未启动 Turn 从未进入执行端即被取消时写入的固定安全说明。
const UNSTARTED_TURN_INTERRUPTION_MESSAGE: &str = "等待容量的 Agent Turn 在启动前已取消";

/// 校验一个单层子 Agent 的持久路径形状，避免补偿事件绕过资源层路径约束。
fn valid_unstarted_turn_agent_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("/root/") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// 仅以 Session 和可信操作标识派生控制事件身份，使跨方法复用也能被正文冲突检测。
fn runtime_control_event_id(
    session_id: &SessionId,
    operation_id: &str,
) -> Result<SessionEventId, RuntimeError> {
    validate_control_operation_id(operation_id)?;
    let digest = canonical_sha256(&(
        "keencode/runtime-session-control/v1",
        session_id.as_str(),
        operation_id,
    ))?;
    SessionEventId::new(format!("runtime-control-{digest}")).map_err(RuntimeError::from)
}

/// 以显式控制操作域派生互不碰撞的稳定事件身份。
fn runtime_control_event_id_in_domain(
    session_id: &SessionId,
    operation_domain: &str,
    operation_id: &str,
) -> Result<SessionEventId, RuntimeError> {
    validate_control_operation_id(operation_id)?;
    validate_control_operation_domain(operation_domain)?;
    let digest = canonical_sha256(&(
        "keencode/runtime-session-control-domain/v1",
        operation_domain,
        session_id.as_str(),
        operation_id,
    ))?;
    SessionEventId::new(format!("runtime-control-{digest}")).map_err(RuntimeError::from)
}

/// 由 Session、邮箱动作和强类型消息 ID 派生跨重启稳定的幂等事件身份。
fn runtime_mailbox_event_id(
    session_id: &SessionId,
    action: &'static str,
    message_id: &MailboxMessageId,
) -> Result<SessionEventId, RuntimeError> {
    let digest = canonical_sha256(&(
        "keencode/runtime-mailbox/v1",
        session_id.as_str(),
        action,
        message_id.as_str(),
    ))?;
    SessionEventId::new(format!("runtime-mailbox-{action}-{digest}")).map_err(RuntimeError::from)
}

/// 把 Agent Runner 的唯一终态转换为同一 Session Journal 的类型化 Turn 终态。
fn runtime_terminal_event(
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
    result: &TurnResult,
) -> SessionEvent {
    match result.state.terminal_reason() {
        Some(TerminalReason::Completed) if result.error.is_none() => runtime_completed_event(
            turn_id,
            source_agent_id,
            result.final_response.as_ref().and_then(model_response_text),
        ),
        reason => {
            let reason = match reason {
                Some(TerminalReason::Cancelled) => TurnStopReason::Cancelled,
                Some(TerminalReason::LimitReached) => TurnStopReason::LimitReached,
                Some(TerminalReason::ContextBlocked) => TurnStopReason::ContextBlocked,
                Some(TerminalReason::ModelOutputLimit) => TurnStopReason::ModelOutputLimit,
                Some(TerminalReason::ModelRefusal) => TurnStopReason::ModelRefusal,
                Some(TerminalReason::Completed | TerminalReason::Failed) | None => {
                    TurnStopReason::Failed
                }
            };
            let message = result
                .error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Agent Turn 未返回可确认的正常终态".to_owned());
            runtime_stopped_event(
                turn_id,
                source_agent_id,
                reason,
                truncate_runtime_terminal_message(&message),
            )
        }
    }
}

/// 判断恢复栅栏是否允许目标事件继续提交或执行相同身份对账。
fn recovery_gate_allows_event(control: &ControlState, event_id: &str) -> bool {
    !control.recovery_required || control.pending_indeterminate.contains(event_id)
}

/// 记录首次结果不确定的事件并冻结其他权威工作。
fn mark_event_indeterminate(control: &mut ControlState, event_id: &str) {
    control.pending_indeterminate.insert(event_id.to_owned());
    refresh_recovery_required(control);
}

/// 在 Journal 已确认追加或幂等命中后解除目标事件的待对账状态。
fn mark_event_confirmed(control: &mut ControlState, event_id: &str) {
    control.pending_indeterminate.remove(event_id);
    refresh_recovery_required(control);
}

/// 根据硬错误、待对账事件和已保留工具 Round 统一刷新恢复栅栏。
fn refresh_recovery_required(control: &mut ControlState) {
    control.recovery_required = control.hard_recovery_required
        || !control.pending_indeterminate.is_empty()
        || control
            .reservations
            .values()
            .any(|entry| entry.retained_event.is_some() || entry.abandoned_after_progress);
}

/// 校验最终工具 Round 仍绑定同一 Sink、身份和冻结 Assistant/PreToolUse 内容。
fn validate_round_reservation(
    control: &ControlState,
    event: &AgentCommitEvent,
    key: Option<&RoundKey>,
) -> Result<Option<RoundKey>, AgentCommitSinkError> {
    let key = key.ok_or_else(commit_rejected)?;
    match event.kind() {
        AgentCommitEventKind::ContextCompactionApplied { .. } => Ok(None),
        AgentCommitEventKind::DynamicInputCommitted { .. } => Ok(None),
        AgentCommitEventKind::ModelRoundCommitted { messages, .. }
        | AgentCommitEventKind::RoundCommitted { messages, .. } => {
            let has_tool_call = messages.first().is_some_and(|message| {
                message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolCall { .. }))
            });
            if !has_tool_call {
                return Ok(None);
            }
            let entry = control.reservations.get(key).ok_or_else(commit_rejected)?;
            if entry.retained_event.is_some() || entry.abandoned_after_progress {
                return Err(commit_indeterminate());
            }
            let end = entry
                .pre_tool_context_count
                .checked_add(2)
                .ok_or_else(commit_rejected)?;
            let assistant = messages.first().ok_or_else(commit_rejected)?;
            let pre_context = messages.get(2..end).ok_or_else(commit_rejected)?;
            let actual =
                known_content_sha256(assistant, pre_context).map_err(|_| commit_rejected())?;
            if actual != entry.known_content_sha256 {
                return Err(commit_rejected());
            }
            Ok(Some(key.clone()))
        }
        AgentCommitEventKind::ToolRequested {
            request_index,
            tool_call_id,
            call,
            ..
        } => {
            let reservation_key = unique_lifecycle_reservation_key(control, key)?;
            let entry = control
                .reservations
                .get(&reservation_key)
                .ok_or_else(commit_rejected)?;
            if entry.retained_event.is_some() || entry.abandoned_after_progress {
                return Err(commit_indeterminate());
            }
            let actual =
                canonical_sha256(&(*request_index, call)).map_err(|_| commit_rejected())?;
            if tool_call_id.as_str() != call.id
                || entry.tool_request_sha256.get(tool_call_id.as_str()) != Some(&actual)
            {
                return Err(commit_rejected());
            }
            Ok(Some(reservation_key))
        }
        AgentCommitEventKind::ToolExecutionStarted { tool_call_id }
        | AgentCommitEventKind::ToolCompleted { tool_call_id, .. } => {
            let reservation_key = unique_lifecycle_reservation_key(control, key)?;
            let entry = control
                .reservations
                .get(&reservation_key)
                .ok_or_else(commit_rejected)?;
            if entry.retained_event.is_some() || entry.abandoned_after_progress {
                return Err(commit_indeterminate());
            }
            if !entry
                .tool_request_sha256
                .contains_key(tool_call_id.as_str())
            {
                return Err(commit_rejected());
            }
            Ok(Some(reservation_key))
        }
    }
}

/// 在忽略 Transcript 段序号后为工具生命周期事件找到唯一 Round reservation。
fn unique_lifecycle_reservation_key(
    control: &ControlState,
    event_key: &RoundKey,
) -> Result<RoundKey, AgentCommitSinkError> {
    let mut matching = control
        .reservations
        .keys()
        .filter(|candidate| same_round_identity(candidate, event_key));
    let key = matching.next().cloned().ok_or_else(commit_rejected)?;
    if matching.next().is_some() {
        return Err(commit_rejected());
    }
    Ok(key)
}

/// 比较工具生命周期共享且不包含 Transcript 段序号的 Round 身份。
fn same_round_identity(left: &RoundKey, right: &RoundKey) -> bool {
    left.session_id == right.session_id
        && left.turn_id == right.turn_id
        && left.agent_id == right.agent_id
        && left.model == right.model
        && left.model_round == right.model_round
}

/// 映射 Agent 六类权威事件，所有身份来自不可变事件信封而不是 payload。
fn map_agent_event(
    inner: &RuntimeSessionInner,
    event: &AgentCommitEvent,
    state: &SessionState,
    hints: &MappingHints,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<SessionEvent, RuntimeError> {
    let key = round_key_from_commit(event)?.ok_or(RuntimeError::RecoveryRequired)?;
    validate_round_identity(state, &key)?;
    match event.kind() {
        AgentCommitEventKind::ContextCompactionApplied { record } => {
            let expected = hints
                .transcript_revision
                .unwrap_or(state.transcript_revision);
            let applied = expected
                .checked_add(1)
                .ok_or(RuntimeError::RecoveryRequired)?;
            let digest =
                hints
                    .compaction_digest
                    .clone()
                    .unwrap_or(state.compaction_source_digest_sha256(
                        &TurnId::new(key.turn_id.clone())?,
                        &keencode_resources::AgentId::new(key.agent_id.clone())?,
                        key.model_round,
                        record.replaced_start_index,
                        record.replaced_end_index_exclusive,
                    )?);
            Ok(SessionEvent::CompactionApplied {
                turn_id: TurnId::new(key.turn_id)?,
                source_agent_id: keencode_resources::AgentId::new(key.agent_id)?,
                model_round: key.model_round,
                compaction: CompactionRecord {
                    trigger: match record.trigger {
                        AgentCompactionTrigger::Budget => ContextCompressionTrigger::Budget,
                        AgentCompactionTrigger::ProviderOverflow => {
                            ContextCompressionTrigger::ProviderOverflow
                        }
                    },
                    estimated_tokens_before: record.estimated_tokens_before,
                    estimated_tokens_after: record.estimated_tokens_after,
                    replaced_start_index: record.replaced_start_index,
                    replaced_end_index_exclusive: record.replaced_end_index_exclusive,
                    replaced_message_count: record.replaced_message_count,
                    retained_message_count: record.retained_message_count,
                    source_digest_sha256: digest,
                    summary: record.summary.clone(),
                    expected_transcript_revision: expected,
                    applied_transcript_revision: applied,
                },
            })
        }
        AgentCommitEventKind::ToolRequested {
            request_index,
            tool_call_id,
            call,
            effect,
        } => {
            if tool_call_id.as_str() != call.id || call.name.trim().is_empty() {
                return Err(RuntimeError::RecoveryRequired);
            }
            let session_id = SessionId::new(key.session_id)?;
            let turn_id = TurnId::new(key.turn_id.clone())?;
            let agent_id = keencode_resources::AgentId::new(key.agent_id)?;
            let request_id = RequestId::derive_model_tool_call(
                &session_id,
                &turn_id,
                &agent_id,
                key.model_round,
                tool_call_id.as_str(),
            )?;
            Ok(SessionEvent::ToolRequested {
                request: keencode_resources::ToolRequest {
                    request_id,
                    turn_id,
                    agent_id,
                    model_round: key.model_round,
                    request_index: *request_index,
                    model_tool_call_id: tool_call_id.as_str().to_owned(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    effect: match effect {
                        AgentToolEffect::ReadOnly => ToolEffect::ReadOnly,
                        AgentToolEffect::ChangesState => ToolEffect::ChangesState,
                    },
                },
            })
        }
        AgentCommitEventKind::ToolExecutionStarted { tool_call_id } => {
            Ok(SessionEvent::ToolExecutionStarted {
                request_id: request_id_for_key(&key, tool_call_id.as_str())?,
            })
        }
        AgentCommitEventKind::ToolCompleted {
            tool_call_id,
            status,
            result,
        } => {
            if tool_call_id.as_str() != result.tool_call_id {
                return Err(RuntimeError::RecoveryRequired);
            }
            let persisted = map_tool_result(inner, result, mode, probe)?;
            Ok(SessionEvent::ToolCompleted {
                request_id: request_id_for_key(&key, tool_call_id.as_str())?,
                outcome: ToolOutcome {
                    status: match status {
                        AgentToolCompletionStatus::Succeeded => ToolCompletionStatus::Succeeded,
                        AgentToolCompletionStatus::Failed => ToolCompletionStatus::Failed,
                        AgentToolCompletionStatus::Cancelled => ToolCompletionStatus::Cancelled,
                    },
                    result: persisted,
                },
            })
        }
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index,
            completion,
            messages,
        } => {
            let expected = hints
                .transcript_revision
                .unwrap_or(state.transcript_revision);
            let mut mapped = Vec::with_capacity(messages.len());
            for (index, message) in messages.iter().enumerate() {
                mapped.push(map_message(inner, &key, index, message, mode, probe)?);
            }
            let turn_id = TurnId::new(key.turn_id.clone())?;
            let source_agent_id = keencode_resources::AgentId::new(key.agent_id.clone())?;
            Ok(SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::ModelRoundCompleted {
                        turn_id: turn_id.clone(),
                        source_agent_id: source_agent_id.clone(),
                        model_round: key.model_round,
                        requested_model: key.model,
                        metadata: completion.metadata.clone(),
                        usage: completion.usage.clone(),
                        stop_reason: completion.stop_reason.clone(),
                    },
                    SessionEvent::TranscriptSegmentCommitted {
                        segment: TranscriptSegment {
                            turn_id,
                            source_agent_id,
                            model_round: key.model_round,
                            segment_index: *segment_index,
                            expected_transcript_revision: expected,
                            messages: mapped,
                        },
                    },
                ],
            })
        }
        AgentCommitEventKind::RoundCommitted {
            segment_index,
            messages,
        } => {
            let expected = hints
                .transcript_revision
                .unwrap_or(state.transcript_revision);
            let mut mapped = Vec::with_capacity(messages.len());
            for (index, message) in messages.iter().enumerate() {
                mapped.push(map_message(inner, &key, index, message, mode, probe)?);
            }
            Ok(SessionEvent::TranscriptSegmentCommitted {
                segment: TranscriptSegment {
                    turn_id: TurnId::new(key.turn_id)?,
                    source_agent_id: keencode_resources::AgentId::new(key.agent_id)?,
                    model_round: key.model_round,
                    segment_index: *segment_index,
                    expected_transcript_revision: expected,
                    messages: mapped,
                },
            })
        }
        AgentCommitEventKind::DynamicInputCommitted {
            segment_index,
            receipts,
            messages,
        } => {
            if messages.is_empty() || receipts.is_empty() {
                return Err(RuntimeError::RecoveryRequired);
            }
            let expected = hints
                .transcript_revision
                .unwrap_or(state.transcript_revision);
            let turn_id = TurnId::new(key.turn_id.clone())?;
            let source_agent_id = keencode_resources::AgentId::new(key.agent_id.clone())?;
            let mut events = Vec::with_capacity(receipts.len().saturating_add(1));
            let mut seen_kinds = BTreeSet::new();
            let mut mapped_messages = Vec::with_capacity(messages.len());
            for (index, message) in messages.iter().enumerate() {
                if !matches!(
                    message.role,
                    ModelMessageRole::User | ModelMessageRole::Developer
                ) {
                    return Err(RuntimeError::RecoveryRequired);
                }
                mapped_messages.push(map_message(inner, &key, index, message, mode, probe)?);
            }
            events.push(SessionEvent::TranscriptSegmentCommitted {
                segment: TranscriptSegment {
                    turn_id: turn_id.clone(),
                    source_agent_id: source_agent_id.clone(),
                    model_round: key.model_round,
                    segment_index: *segment_index,
                    expected_transcript_revision: expected,
                    messages: mapped_messages,
                },
            });
            for receipt in receipts {
                if receipt.through_sequence() == 0 || !seen_kinds.insert(receipt.kind().as_str()) {
                    return Err(RuntimeError::RecoveryRequired);
                }
                let kind = match receipt.kind() {
                    AgentDynamicInputKind::Mailbox => DynamicInputKind::Mailbox,
                    AgentDynamicInputKind::UserSteer => DynamicInputKind::UserSteer,
                };
                events.push(SessionEvent::DynamicInputReceiptCommitted {
                    turn_id: turn_id.clone(),
                    source_agent_id: source_agent_id.clone(),
                    model_round: key.model_round,
                    segment_index: *segment_index,
                    kind,
                    through_sequence: receipt.through_sequence(),
                });
            }
            Ok(SessionEvent::AtomicBatch { events })
        }
    }
}

/// 把 Provider 中立消息无损转换为可恢复 SessionMessage。
fn map_message(
    inner: &RuntimeSessionInner,
    key: &RoundKey,
    position: usize,
    message: &Message,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<SessionMessage, RuntimeError> {
    message
        .validate()
        .map_err(|_| RuntimeError::RecoveryRequired)?;
    let role = match message.role {
        ModelMessageRole::System => MessageRole::System,
        ModelMessageRole::Developer => MessageRole::Developer,
        ModelMessageRole::User => MessageRole::User,
        ModelMessageRole::Assistant => MessageRole::Assistant,
        ModelMessageRole::Tool => MessageRole::Tool,
    };
    let agent_id = match role {
        MessageRole::Assistant | MessageRole::Tool => {
            Some(keencode_resources::AgentId::new(key.agent_id.clone())?)
        }
        MessageRole::System | MessageRole::Developer | MessageRole::User => None,
    };
    let mut content = Vec::with_capacity(message.content.len());
    for block in &message.content {
        content.push(match block {
            ContentBlock::Text { text } => map_message_text(inner, text, mode, probe)?,
            ContentBlock::Reasoning { reasoning } => {
                if reasoning.text.len() > inner.config.max_inline_text_bytes
                    || reasoning
                        .summary
                        .as_ref()
                        .is_some_and(|summary| summary.len() > inner.config.max_inline_text_bytes)
                {
                    return Err(RuntimeError::ReasoningTooLarge);
                }
                MessagePart::Reasoning {
                    text: reasoning.text.clone(),
                    summary: reasoning.summary.clone(),
                    continuation: reasoning.continuation.as_ref().map(|continuation| {
                        keencode_resources::ReasoningContinuation {
                            kind: continuation.kind.clone(),
                            data: continuation.data.clone(),
                        }
                    }),
                }
            }
            ContentBlock::Image { image } => MessagePart::Image {
                source: map_image_source(inner, &image.source, mode, probe)?,
            },
            ContentBlock::ToolCall { tool_call } => MessagePart::ToolCall {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                arguments: tool_call.arguments.clone(),
            },
            ContentBlock::ToolResult { tool_result } => MessagePart::ToolResult {
                tool_call_id: tool_result.tool_call_id.clone(),
                content: map_tool_result(inner, tool_result, mode, probe)?.content,
                is_error: tool_result.is_error,
            },
        });
    }
    Ok(SessionMessage {
        message_id: stable_message_id(key, position, message)?,
        turn_id: Some(TurnId::new(key.turn_id.clone())?),
        agent_id,
        role,
        content,
    })
}

/// 将一条持久消息及其 Artifact 恢复为 Provider 中立模型消息。
fn materialize_model_message(
    artifacts: &ArtifactStore,
    message: &SessionMessage,
) -> Result<Message, RuntimeError> {
    let role = match message.role {
        MessageRole::System => ModelMessageRole::System,
        MessageRole::Developer => ModelMessageRole::Developer,
        MessageRole::User => ModelMessageRole::User,
        MessageRole::Assistant => ModelMessageRole::Assistant,
        MessageRole::Tool => ModelMessageRole::Tool,
    };
    let mut content = Vec::with_capacity(message.content.len());
    for part in &message.content {
        content.push(match part {
            MessagePart::Text { text } => ContentBlock::Text { text: text.clone() },
            MessagePart::Reasoning {
                text,
                summary,
                continuation,
            } => ContentBlock::Reasoning {
                reasoning: ReasoningContent {
                    text: text.clone(),
                    summary: summary.clone(),
                    continuation: continuation.as_ref().map(|continuation| {
                        OpaqueReasoningState::new(
                            continuation.kind.clone(),
                            continuation.data.clone(),
                        )
                    }),
                },
            },
            MessagePart::Image { source } => ContentBlock::Image {
                image: materialize_model_image(artifacts, source)?,
            },
            MessagePart::ToolCall {
                tool_call_id,
                tool_name,
                arguments,
            } => ContentBlock::ToolCall {
                tool_call: ToolCall::new(
                    tool_call_id.clone(),
                    tool_name.clone(),
                    arguments.clone(),
                ),
            },
            MessagePart::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_result: ToolResult::new(
                    tool_call_id.clone(),
                    materialize_tool_result_content(artifacts, content)?,
                    *is_error,
                ),
            },
            MessagePart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Utf8Text,
            } => ContentBlock::Text {
                text: materialize_utf8_artifact(artifacts, artifact)?,
            },
            MessagePart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Image,
            } => ContentBlock::Image {
                image: materialize_artifact_image(artifacts, artifact)?,
            },
            MessagePart::Artifact {
                materialization: ArtifactMaterialization::Binary,
                ..
            } => return Err(RuntimeError::RecoveryRequired),
        });
    }
    let message = Message::new(role, content);
    message
        .validate()
        .map_err(|_| RuntimeError::RecoveryRequired)?;
    Ok(message)
}

/// 恢复持久化图片 URL 或内容寻址图片字节。
fn materialize_model_image(
    artifacts: &ArtifactStore,
    source: &MessageImageSource,
) -> Result<ImageContent, RuntimeError> {
    match source {
        MessageImageSource::Url { url } => Ok(ImageContent::from_url(url.clone())),
        MessageImageSource::Artifact { artifact } => {
            materialize_artifact_image(artifacts, artifact)
        }
    }
}

/// 读取并严格校验一个 UTF-8 文本 Artifact。
fn materialize_utf8_artifact(
    artifacts: &ArtifactStore,
    artifact: &ArtifactUse,
) -> Result<String, RuntimeError> {
    let bytes = artifacts.read_use(artifact)?;
    String::from_utf8(bytes).map_err(|_| RuntimeError::RecoveryRequired)
}

/// 读取图片 Artifact 并恢复为统一模型层的 Base64 图片。
fn materialize_artifact_image(
    artifacts: &ArtifactStore,
    artifact: &ArtifactUse,
) -> Result<ImageContent, RuntimeError> {
    let media_type = artifact
        .media_type
        .as_deref()
        .filter(|media_type| is_canonical_image_media_type(media_type))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let bytes = artifacts.read_use(artifact)?;
    Ok(ImageContent::from_base64(
        media_type,
        BASE64_STANDARD.encode(bytes),
    ))
}

/// 恢复一个工具结果的有序文本、图片或 Artifact 内容。
fn materialize_tool_result_content(
    artifacts: &ArtifactStore,
    parts: &[ToolResultPart],
) -> Result<Vec<ToolResultContent>, RuntimeError> {
    let mut content = Vec::with_capacity(parts.len());
    for part in parts {
        content.push(match part {
            ToolResultPart::Text { text } => ToolResultContent::Text { text: text.clone() },
            ToolResultPart::Image { source } => ToolResultContent::Image {
                image: materialize_model_image(artifacts, source)?,
            },
            ToolResultPart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Utf8Text,
            } => ToolResultContent::Text {
                text: materialize_utf8_artifact(artifacts, artifact)?,
            },
            ToolResultPart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Image,
            } => ToolResultContent::Image {
                image: materialize_artifact_image(artifacts, artifact)?,
            },
            ToolResultPart::Artifact {
                materialization: ArtifactMaterialization::Binary,
                ..
            } => return Err(RuntimeError::RecoveryRequired),
        });
    }
    Ok(content)
}

/// 把统一工具结果转换为保持内容块顺序的可恢复结果。
fn map_tool_result(
    inner: &RuntimeSessionInner,
    result: &keencode_model::ToolResult,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<PersistedToolResult, RuntimeError> {
    result
        .validate()
        .map_err(|_| RuntimeError::RecoveryRequired)?;
    let mut content = Vec::with_capacity(result.content.len());
    for part in &result.content {
        content.push(match part {
            ToolResultContent::Text { text } => map_tool_result_text(inner, text, mode, probe)?,
            ToolResultContent::Image { image } => ToolResultPart::Image {
                source: map_image_source(inner, &image.source, mode, probe)?,
            },
        });
    }
    Ok(PersistedToolResult {
        tool_call_id: result.tool_call_id.clone(),
        content,
        is_error: result.is_error,
    })
}

/// 把普通消息文本按内联阈值保持为文本或转换为 UTF-8 Artifact。
fn map_message_text(
    inner: &RuntimeSessionInner,
    text: &str,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<MessagePart, RuntimeError> {
    if text.len() <= inner.config.max_inline_text_bytes {
        return Ok(MessagePart::Text {
            text: text.to_owned(),
        });
    }
    Ok(MessagePart::Artifact {
        artifact: map_utf8_text_artifact(inner, text, mode, probe)?,
        materialization: ArtifactMaterialization::Utf8Text,
    })
}

/// 把工具结果文本按内联阈值保持为文本或转换为 UTF-8 Artifact。
fn map_tool_result_text(
    inner: &RuntimeSessionInner,
    text: &str,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<ToolResultPart, RuntimeError> {
    if text.len() <= inner.config.max_inline_text_bytes {
        return Ok(ToolResultPart::Text {
            text: text.to_owned(),
        });
    }
    Ok(ToolResultPart::Artifact {
        artifact: map_utf8_text_artifact(inner, text, mode, probe)?,
        materialization: ArtifactMaterialization::Utf8Text,
    })
}

/// 为超限 UTF-8 文本生成 Probe 与 Commit 完全相同的内容寻址引用。
fn map_utf8_text_artifact(
    inner: &RuntimeSessionInner,
    text: &str,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<ArtifactUse, RuntimeError> {
    let bytes = text.as_bytes();
    if bytes.len() as u64 > inner.config.artifacts.max_artifact_bytes {
        return Err(RuntimeError::Resource(ResourceError::ArtifactTooLarge {
            actual: bytes.len() as u64,
            limit: inner.config.artifacts.max_artifact_bytes,
        }));
    }
    let digest = digest_hex(bytes);
    let media_type = Some(UTF8_TEXT_MEDIA_TYPE.to_owned());
    let candidate = ArtifactUse {
        artifact_id: ArtifactId::new(digest.clone())?,
        sha256: digest.clone(),
        size_bytes: bytes.len() as u64,
        media_type: media_type.clone(),
    };
    match inner.artifacts.validate_use(&candidate) {
        Ok(()) => {}
        Err(ResourceError::ArtifactNotFound) => {
            probe.missing_ids.insert(digest.clone());
            probe.missing_uses.insert(digest, candidate.clone());
        }
        Err(error) => return Err(RuntimeError::Resource(error)),
    }
    match mode {
        ArtifactMode::Probe => Ok(candidate),
        ArtifactMode::Commit => Ok(inner.artifacts.put(bytes, media_type)?.as_event_use()),
    }
}

/// 把 URL 图片保持为 URL，把 Base64 图片变为可复核 Artifact 引用。
fn map_image_source(
    inner: &RuntimeSessionInner,
    source: &ImageSource,
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<MessageImageSource, RuntimeError> {
    match source {
        ImageSource::Url { url } if is_data_url(url) => {
            let (media_type, bytes) = decode_data_image_url(url)?;
            map_inline_image(inner, &media_type, &bytes, mode, probe)
        }
        ImageSource::Url { url } => {
            validate_remote_image_url(url).map(|()| MessageImageSource::Url { url: url.clone() })
        }
        ImageSource::Base64 { media_type, data } => {
            validate_inline_image_source(media_type, data)?;
            let bytes = decode_canonical_base64(data)?;
            map_inline_image(inner, media_type, &bytes, mode, probe)
        }
    }
}

/// 校验远端图片只使用有界、无空白的绝对 HTTP(S) ASCII 地址。
fn validate_remote_image_url(url: &str) -> Result<(), RuntimeError> {
    if url.len() > MAX_PERSISTED_IMAGE_URL_BYTES || !is_canonical_remote_image_url(url) {
        return Err(RuntimeError::InvalidImageUrl);
    }
    Ok(())
}

/// 解析并严格解码唯一受支持的 `data:image/<token>;base64,...` 形式。
fn decode_data_image_url(url: &str) -> Result<(String, Vec<u8>), RuntimeError> {
    if url.len() > TOOL_OUTPUT_LIMITS.max_data_url_bytes
        || url.len() > TOOL_OUTPUT_LIMITS.max_image_source_bytes
    {
        return Err(RuntimeError::InvalidImageData);
    }
    let body = url.get(5..).ok_or(RuntimeError::InvalidImageData)?;
    let (metadata, data) = body.split_once(',').ok_or(RuntimeError::InvalidImageData)?;
    let mut metadata_parts = metadata.split(';');
    let media_type = metadata_parts
        .next()
        .ok_or(RuntimeError::InvalidImageData)?;
    let mut saw_base64 = false;
    for parameter in metadata_parts {
        if !parameter.eq_ignore_ascii_case("base64") || saw_base64 {
            return Err(RuntimeError::InvalidImageData);
        }
        saw_base64 = true;
    }
    if !saw_base64 {
        return Err(RuntimeError::InvalidImageData);
    }
    validate_inline_image_source(media_type, data)?;
    Ok((media_type.to_owned(), decode_canonical_base64(data)?))
}

/// 校验内联图片媒体类型与编码文本都处于 Agent 公共硬限制内。
fn validate_inline_image_source(media_type: &str, data: &str) -> Result<(), RuntimeError> {
    if !is_canonical_image_media_type(media_type)
        || media_type
            .len()
            .checked_add(data.len())
            .is_none_or(|bytes| bytes > TOOL_OUTPUT_LIMITS.max_image_source_bytes)
        || data.len() > TOOL_OUTPUT_LIMITS.max_base64_characters
    {
        return Err(RuntimeError::InvalidImageData);
    }
    Ok(())
}

/// 严格解码标准有填充 Base64，并拒绝非规范尾位或解码后超限。
fn decode_canonical_base64(data: &str) -> Result<Vec<u8>, RuntimeError> {
    if data.is_empty() || data.len() % 4 != 0 {
        return Err(RuntimeError::InvalidImageData);
    }
    let bytes = BASE64_STANDARD
        .decode(data)
        .map_err(|_| RuntimeError::InvalidImageData)?;
    if bytes.len() > TOOL_OUTPUT_LIMITS.max_image_decoded_bytes
        || BASE64_STANDARD.encode(&bytes) != data
    {
        return Err(RuntimeError::InvalidImageData);
    }
    Ok(bytes)
}

/// 把已经严格验证的图片字节映射为 Probe 与 Commit 一致的 Artifact 引用。
fn map_inline_image(
    inner: &RuntimeSessionInner,
    media_type: &str,
    bytes: &[u8],
    mode: ArtifactMode,
    probe: &mut ArtifactProbe,
) -> Result<MessageImageSource, RuntimeError> {
    if bytes.len() as u64 > inner.config.artifacts.max_artifact_bytes {
        return Err(RuntimeError::InvalidImageData);
    }
    let digest = digest_hex(bytes);
    let candidate = ArtifactUse {
        artifact_id: ArtifactId::new(digest.clone())?,
        sha256: digest.clone(),
        size_bytes: bytes.len() as u64,
        media_type: Some(media_type.to_owned()),
    };
    match inner.artifacts.validate_use(&candidate) {
        Ok(()) => {}
        Err(ResourceError::ArtifactNotFound) => {
            probe.missing_ids.insert(digest.clone());
            probe.missing_uses.insert(digest, candidate.clone());
        }
        Err(error) => return Err(RuntimeError::Resource(error)),
    }
    let artifact = match mode {
        ArtifactMode::Probe => candidate,
        ArtifactMode::Commit => inner
            .artifacts
            .put(bytes, Some(media_type.to_owned()))?
            .as_event_use(),
    };
    Ok(MessageImageSource::Artifact { artifact })
}

/// 判断字符串是否使用大小写不敏感的 data URL scheme。
fn is_data_url(url: &str) -> bool {
    url.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

/// 从 Agent 预检对象创建严格资源身份可接受的 RoundKey。
fn round_key_from_preflight(round: &AgentToolRoundPreflight) -> Result<RoundKey, RuntimeError> {
    let key = RoundKey {
        session_id: round.session_id().as_str().to_owned(),
        turn_id: round.turn_id().as_str().to_owned(),
        agent_id: round.source_agent_id().as_str().to_owned(),
        model: round.model().to_owned(),
        model_round: round.model_round(),
        segment_index: round.segment_index(),
    };
    validate_round_key(&key)?;
    Ok(key)
}

/// 从 Agent 权威事件创建严格资源身份可接受的 RoundKey。
fn round_key_from_commit(event: &AgentCommitEvent) -> Result<Option<RoundKey>, RuntimeError> {
    let segment_index = match event.kind() {
        AgentCommitEventKind::ModelRoundCommitted { segment_index, .. }
        | AgentCommitEventKind::RoundCommitted { segment_index, .. } => *segment_index,
        AgentCommitEventKind::DynamicInputCommitted { segment_index, .. } => *segment_index,
        AgentCommitEventKind::ContextCompactionApplied { .. }
        | AgentCommitEventKind::ToolRequested { .. }
        | AgentCommitEventKind::ToolExecutionStarted { .. }
        | AgentCommitEventKind::ToolCompleted { .. } => 0,
    };
    let key = RoundKey {
        session_id: event.session_id().as_str().to_owned(),
        turn_id: event.turn_id().as_str().to_owned(),
        agent_id: event.source_agent_id().as_str().to_owned(),
        model: event.model().to_owned(),
        model_round: event.model_round(),
        segment_index,
    };
    validate_round_key(&key)?;
    Ok(Some(key))
}

/// 校验 Agent 身份可无损进入资源层且模型 Round 非零。
fn validate_round_key(key: &RoundKey) -> Result<(), RuntimeError> {
    SessionId::new(key.session_id.clone())?;
    TurnId::new(key.turn_id.clone())?;
    keencode_resources::AgentId::new(key.agent_id.clone())?;
    if key.model.trim().is_empty() || key.model_round == 0 {
        return Err(RuntimeError::RecoveryRequired);
    }
    Ok(())
}

/// 校验目标 Turn 正在运行且由 RoundKey 中同一 Agent 执行。
fn validate_round_identity(state: &SessionState, key: &RoundKey) -> Result<(), RuntimeError> {
    if state.session_id.as_str() != key.session_id {
        return Err(RuntimeError::RecoveryRequired);
    }
    let turn_id = TurnId::new(key.turn_id.clone())?;
    let agent_id = keencode_resources::AgentId::new(key.agent_id.clone())?;
    if state
        .turns
        .get(&turn_id)
        .is_none_or(|turn| turn.status != TurnStatus::Running || turn.source_agent_id != agent_id)
    {
        return Err(RuntimeError::RecoveryRequired);
    }
    Ok(())
}

/// 使用完整 Round 身份派生资源层工具请求标识。
fn request_id_for_key(key: &RoundKey, tool_call_id: &str) -> Result<RequestId, RuntimeError> {
    Ok(RequestId::derive_model_tool_call(
        &SessionId::new(key.session_id.clone())?,
        &TurnId::new(key.turn_id.clone())?,
        &keencode_resources::AgentId::new(key.agent_id.clone())?,
        key.model_round,
        tool_call_id,
    )?)
}

/// 为映射后的消息生成内容与位置绑定的稳定 SHA-256 身份。
fn stable_message_id(
    key: &RoundKey,
    position: usize,
    message: &Message,
) -> Result<String, RuntimeError> {
    canonical_sha256(&(
        "keencode-runtime-message-v1",
        &key.session_id,
        &key.turn_id,
        &key.agent_id,
        key.model_round,
        key.segment_index,
        position,
        message,
    ))
}

/// 计算冻结 Assistant 与 PreToolUse 消息的规范 JSON 摘要。
fn known_content_sha256(
    assistant: &Message,
    pre_context: &[Message],
) -> Result<String, RuntimeError> {
    canonical_sha256(&(assistant, pre_context))
}

/// 校验桌面控制请求携带的可信幂等标识，拒绝空值、隐式空白与无界输入。
fn validate_control_operation_id(operation_id: &str) -> Result<(), RuntimeError> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || operation_id.trim() != operation_id
        || operation_id.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidControlOperation);
    }
    Ok(())
}

/// 校验显式控制操作域，保证其可安全参与稳定身份派生。
fn validate_control_operation_domain(operation_domain: &str) -> Result<(), RuntimeError> {
    if operation_domain.is_empty()
        || operation_domain.len() > 128
        || operation_domain.trim() != operation_domain
        || operation_domain.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidControlOperation);
    }
    Ok(())
}

/// 判断字符串是否为资源层可接受的固定 64 位小写十六进制 SHA-256。
fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// 计算任意可序列化值的 SHA-256，用作内容一致性而非身份授权。
fn canonical_sha256(value: &impl Serialize) -> Result<String, RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(|_| RuntimeError::RecoveryRequired)?;
    Ok(digest_hex(&bytes))
}

/// 把字节摘要编码成固定小写十六进制。
fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    output
}

/// 与资源 Journal 物理 JSONL envelope 一致的容量编码视图。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventWire<'a> {
    /// 固定 Session 事件 schema。
    schema: &'static str,
    /// 固定 Session 事件版本。
    version: u32,
    /// 当前幂等事件标识。
    event_id: &'a SessionEventId,
    /// 当前根 Session 标识。
    session: &'a SessionId,
    /// 使用最大十进制宽度的保守 sequence。
    sequence: u64,
    /// 使用最大十进制宽度的保守 Unix 毫秒时间。
    time_unix_ms: u64,
    /// 扁平写入顶层 type 与 payload 的资源事件。
    #[serde(flatten)]
    event: &'a SessionEvent,
}

/// 返回包含换行符的保守实际 Journal 编码字节数。
fn encoded_record_len(
    session_id: &SessionId,
    event_id: &SessionEventId,
    event: &SessionEvent,
) -> Result<u64, RuntimeError> {
    let wire = EventWire {
        schema: SESSION_EVENT_SCHEMA,
        version: SESSION_EVENT_VERSION,
        event_id,
        session: session_id,
        sequence: u64::MAX,
        time_unix_ms: u64::MAX,
        event,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| RuntimeError::RecoveryRequired)?;
    Ok(u64::try_from(bytes.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1))
}

/// 读取当前日志物理长度；不存在的全新日志长度为零。
fn journal_len(journal: &SessionJournal) -> Result<u64, RuntimeError> {
    match fs::metadata(journal.log_path()) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(ResourceError::Json(format!("读取 Journal 容量失败：{error}")).into()),
    }
}

/// 按资源层 `validate_state_collections` 的二十一 个独立维度计算当前状态精确占用。
fn state_collection_items(state: &SessionState) -> StateCollectionItems {
    let transcript_messages = state.raw_transcript_messages();
    let message_items = transcript_messages
        .iter()
        .fold(StateCollectionItems::default(), |total, message| {
            total.saturating_add(state_collection_message_items(message))
        });
    let tool_outcome_result_content = state.tools.values().fold(0_usize, |total, tool| {
        total.saturating_add(
            tool.outcome
                .as_ref()
                .map_or(0, |outcome| outcome.result.content.len()),
        )
    });
    let terminal_output_artifacts = state.terminals.values().fold(0_usize, |total, terminal| {
        total.saturating_add(terminal.output_artifacts.len())
    });
    let tool_argument_items = state.tools.values().fold(0_usize, |total, tool| {
        total.saturating_add(json_collection_items(&tool.request.arguments))
    });
    let file_changes = state
        .tools
        .values()
        .filter(|tool| tool.file_change.is_some())
        .count();
    let file_snapshot_chunks = state
        .tools
        .values()
        .filter_map(|tool| tool.file_change.as_ref())
        .fold(0_usize, |total, change| {
            let before = change
                .before
                .as_ref()
                .map_or(0, |snapshot| snapshot.chunks.len());
            total
                .saturating_add(before)
                .saturating_add(change.after.chunks.len())
        });
    StateCollectionItems {
        turns: state.turns.len(),
        transcript: state.transcript.len(),
        messages: message_items.messages,
        transcript_segments: state.transcript_segments().count(),
        model_rounds: state.model_rounds.len(),
        tools: state.tools.len(),
        file_changes,
        file_snapshot_chunks,
        terminals: state.terminals.len(),
        compactions: state.applied_compactions().count(),
        todos: state.todos.items.len(),
        sub_agents: state.sub_agents.len(),
        mailbox: state.mailbox.len(),
        worktrees: state.worktrees.len(),
        generated_titles: state.generated_titles.len(),
        dynamic_input_receipts: state.dynamic_input_receipts.len(),
        message_parts: message_items.message_parts,
        message_tool_result_content: message_items.message_tool_result_content,
        tool_outcome_result_content,
        terminal_output_artifacts,
        json_collection_items: message_items
            .json_collection_items
            .saturating_add(tool_argument_items),
    }
}

/// 计算一条持久化消息在消息相关集合维度中的精确占用。
fn state_collection_message_items(message: &SessionMessage) -> StateCollectionItems {
    let message_tool_result_content = message.content.iter().fold(0_usize, |total, part| {
        let count = match part {
            MessagePart::ToolResult { content, .. } => content.len(),
            MessagePart::Text { .. }
            | MessagePart::Reasoning { .. }
            | MessagePart::Image { .. }
            | MessagePart::ToolCall { .. }
            | MessagePart::Artifact { .. } => 0,
        };
        total.saturating_add(count)
    });
    let json_collection_items = message.content.iter().fold(0_usize, |total, part| {
        total.saturating_add(message_part_json_collection_items(part))
    });
    StateCollectionItems {
        messages: 1,
        message_parts: message.content.len(),
        message_tool_result_content,
        json_collection_items,
        ..StateCollectionItems::default()
    }
}

/// 计算单个资源事件一旦确认后可能增加的二十一维状态集合项数。
fn state_collection_event_items(event: &SessionEvent) -> StateCollectionItems {
    match event {
        SessionEvent::TurnStarted { .. } => StateCollectionItems {
            turns: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .fold(StateCollectionItems::default(), |total, event| {
                total.saturating_add(state_collection_event_items(event))
            }),
        SessionEvent::MessageAdded { message } => state_collection_message_items(message)
            .saturating_add(StateCollectionItems {
                transcript: 1,
                ..StateCollectionItems::default()
            }),
        SessionEvent::TranscriptSegmentCommitted { segment } => segment.messages.iter().fold(
            StateCollectionItems {
                transcript: 1,
                transcript_segments: 1,
                ..StateCollectionItems::default()
            },
            |total, message| total.saturating_add(state_collection_message_items(message)),
        ),
        SessionEvent::ModelRoundCompleted { .. } => StateCollectionItems {
            model_rounds: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::ToolRequested { request } => StateCollectionItems {
            tools: 1,
            json_collection_items: json_collection_items(&request.arguments),
            ..StateCollectionItems::default()
        },
        SessionEvent::ToolFileChangePrepared { change, .. } => StateCollectionItems {
            file_changes: 1,
            file_snapshot_chunks: change
                .before
                .as_ref()
                .map_or(0, |snapshot| snapshot.chunks.len())
                .saturating_add(change.after.chunks.len()),
            ..StateCollectionItems::default()
        },
        SessionEvent::ToolCompleted { outcome, .. } => StateCollectionItems {
            tool_outcome_result_content: outcome.result.content.len(),
            ..StateCollectionItems::default()
        },
        SessionEvent::ToolSideEffectUnknown { result, .. } => StateCollectionItems {
            tool_outcome_result_content: result.content.len(),
            ..StateCollectionItems::default()
        },
        SessionEvent::TerminalStarted { terminal } => StateCollectionItems {
            terminals: 1,
            terminal_output_artifacts: terminal.output_artifacts.len(),
            ..StateCollectionItems::default()
        },
        SessionEvent::TerminalOutputRecorded { .. } => StateCollectionItems {
            terminal_output_artifacts: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::CompactionApplied { .. } => StateCollectionItems {
            transcript: 1,
            compactions: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::TodoReplaced { items, .. } => StateCollectionItems {
            todos: items.len(),
            ..StateCollectionItems::default()
        },
        SessionEvent::SubAgentSpawned { .. } => StateCollectionItems {
            sub_agents: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::MailboxMessageQueued { .. } => StateCollectionItems {
            mailbox: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::WorktreeAssigned { .. } => StateCollectionItems {
            worktrees: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::TitleGenerated { .. } => StateCollectionItems {
            generated_titles: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::DynamicInputReceiptCommitted { .. } => StateCollectionItems {
            dynamic_input_receipts: 1,
            ..StateCollectionItems::default()
        },
        SessionEvent::SessionCreated { .. }
        | SessionEvent::SessionRenamed { .. }
        | SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TurnCompleted { .. }
        | SessionEvent::TurnStopped { .. }
        | SessionEvent::ToolExecutionStarted { .. }
        | SessionEvent::ToolFileChangeApplied { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::PlanChanged { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::SubAgentStatusChanged { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => StateCollectionItems::default(),
    }
}

/// 统计消息块内部 JSON Array 元素和 Object 成员，保持与资源层相同递归语义。
fn message_part_json_collection_items(part: &MessagePart) -> usize {
    match part {
        MessagePart::Reasoning {
            continuation: Some(continuation),
            ..
        } => json_collection_items(&continuation.data),
        MessagePart::ToolCall { arguments, .. } => json_collection_items(arguments),
        MessagePart::Text { .. }
        | MessagePart::Reasoning {
            continuation: None, ..
        }
        | MessagePart::Image { .. }
        | MessagePart::ToolResult { .. }
        | MessagePart::Artifact { .. } => 0,
    }
}

/// 递归统计 JSON Array 元素和 Object 成员，任一溢出按最大值处理。
fn json_collection_items(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(items) => items.iter().fold(items.len(), |total, nested| {
            total.saturating_add(json_collection_items(nested))
        }),
        serde_json::Value::Object(entries) => {
            entries.values().fold(entries.len(), |total, nested| {
                total.saturating_add(json_collection_items(nested))
            })
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

/// 聚合所有活跃 reservation 尚未由确认事件消费的状态集合预算。
fn protected_state_collection_items(control: &ControlState) -> StateCollectionItems {
    control
        .reservations
        .values()
        .fold(StateCollectionItems::default(), |total, entry| {
            total.saturating_add(entry.reserved_state_items)
        })
}

/// 汇总全部执行中 Runtime Turn 为唯一终态保护的 Journal 字节与记录数。
fn protected_runtime_terminal_capacity(control: &ControlState) -> Option<(u64, u64)> {
    control
        .turn_executions
        .values()
        .try_fold((0_u64, 0_u64), |(bytes, records), execution| {
            Some((
                bytes.checked_add(execution.terminal_journal_bytes())?,
                records.checked_add(u64::from(execution.reserves_terminal_record()))?,
            ))
        })
}

/// 确认预检已知内容可以在当前 Journal 与 Artifact 结构容量内保留。
fn ensure_preflight_capacity(
    inner: &RuntimeSessionInner,
    control: &ControlState,
    state: &SessionState,
    budget: &ToolRoundPersistenceBudget,
    event_missing_artifacts: &BTreeSet<String>,
) -> Result<(), AgentToolRoundPreflightError> {
    let protected_tool_bytes = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_bytes)
        })
        .ok_or_else(preflight_unavailable)?;
    let protected_tool_records = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_records)
        })
        .ok_or_else(preflight_unavailable)?;
    let (protected_runtime_bytes, protected_runtime_records) =
        protected_runtime_terminal_capacity(control).ok_or_else(preflight_unavailable)?;
    let (protected_file_change_bytes, protected_file_change_records) =
        file_changes::protected_file_change_journal_capacity(control)
            .ok_or_else(preflight_unavailable)?;
    let protected_bytes = protected_tool_bytes
        .checked_add(protected_runtime_bytes)
        .and_then(|value| value.checked_add(protected_file_change_bytes))
        .ok_or_else(preflight_unavailable)?;
    let protected_records = protected_tool_records
        .checked_add(protected_runtime_records)
        .and_then(|value| value.checked_add(protected_file_change_records))
        .ok_or_else(preflight_unavailable)?;
    let log_len = journal_len(&inner.journal).map_err(|_| preflight_unavailable())?;
    if log_len
        .checked_add(protected_bytes)
        .and_then(|value| value.checked_add(budget.journal_bytes))
        .is_none_or(|value| value > inner.config.journal.max_log_bytes)
        || state
            .last_sequence
            .checked_add(protected_records)
            .and_then(|value| value.checked_add(budget.journal_records))
            .is_none_or(|value| value > inner.config.journal.max_records)
    {
        return Err(preflight_unpersistable());
    }
    let protected_state_items = protected_state_collection_items(control)
        .saturating_add(file_changes::protected_file_change_state_items(control));
    if !state_collection_items(state)
        .saturating_add(protected_state_items)
        .saturating_add(budget.state_items)
        .fits_limit(inner.config.journal.max_state_collection_items)
    {
        return Err(preflight_unpersistable());
    }
    let mut protected_artifacts = control
        .reservations
        .values()
        .flat_map(|entry| entry.missing_artifact_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    protected_artifacts.extend(file_changes::protected_file_change_artifacts(control));
    protected_artifacts.extend(event_missing_artifacts.iter().cloned());
    let protected_unknown_artifacts = control
        .reservations
        .values()
        .try_fold(0_usize, |total, entry| {
            total.checked_add(entry.reserved_unknown_artifacts)
        })
        .and_then(|total| total.checked_add(budget.unknown_artifacts))
        .ok_or_else(preflight_unavailable)?;
    let remaining = inner
        .artifacts
        .capacity()
        .map_err(|_| preflight_unavailable())?
        .remaining();
    if protected_artifacts
        .len()
        .checked_add(protected_unknown_artifacts)
        .is_none_or(|required| required > remaining)
    {
        return Err(preflight_unpersistable());
    }
    Ok(())
}

/// 在实际 Artifact 写入和 Journal 追加前保护其他 Round 的 reservation 容量。
fn ensure_commit_capacity(
    inner: &RuntimeSessionInner,
    control: &ControlState,
    state: &SessionState,
    event_bytes: u64,
    consuming_round: Option<&RoundKey>,
    event_missing_artifacts: &BTreeSet<String>,
    event_state_items: StateCollectionItems,
) -> Result<(), AgentCommitSinkError> {
    if event_bytes > inner.config.journal.max_event_bytes {
        return Err(commit_rejected());
    }
    if let Some(key) = consuming_round {
        let entry = control.reservations.get(key).ok_or_else(commit_rejected)?;
        if event_bytes > entry.reserved_journal_bytes || entry.reserved_journal_records == 0 {
            return Err(commit_rejected());
        }
        if !entry.reserved_state_items.covers(event_state_items) {
            return Err(commit_rejected());
        }
        let unknown = event_missing_artifacts
            .iter()
            .filter(|artifact_id| !entry.missing_artifact_ids.contains(*artifact_id))
            .count();
        if unknown > entry.reserved_unknown_artifacts {
            return Err(commit_rejected());
        }
    }
    let protected_tool_bytes = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_bytes)
        })
        .ok_or_else(commit_rejected)?;
    let protected_tool_records = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_records)
        })
        .ok_or_else(commit_rejected)?;
    let (protected_runtime_bytes, protected_runtime_records) =
        protected_runtime_terminal_capacity(control).ok_or_else(commit_rejected)?;
    let (protected_file_change_bytes, protected_file_change_records) =
        file_changes::protected_file_change_journal_capacity(control)
            .ok_or_else(commit_rejected)?;
    let protected_bytes = protected_tool_bytes
        .checked_add(protected_runtime_bytes)
        .and_then(|value| value.checked_add(protected_file_change_bytes))
        .ok_or_else(commit_rejected)?;
    let protected_records = protected_tool_records
        .checked_add(protected_runtime_records)
        .and_then(|value| value.checked_add(protected_file_change_records))
        .ok_or_else(commit_rejected)?;
    let unreserved_event_bytes = if consuming_round.is_none() {
        event_bytes
    } else {
        0
    };
    let unreserved_event_records = u64::from(consuming_round.is_none());
    let log_len = journal_len(&inner.journal).map_err(|_| commit_rejected())?;
    if log_len
        .checked_add(protected_bytes)
        .and_then(|value| value.checked_add(unreserved_event_bytes))
        .is_none_or(|value| value > inner.config.journal.max_log_bytes)
        || state
            .last_sequence
            .checked_add(protected_records)
            .and_then(|value| value.checked_add(unreserved_event_records))
            .is_none_or(|value| value > inner.config.journal.max_records)
    {
        return Err(commit_rejected());
    }
    let unreserved_state_items = if consuming_round.is_none() {
        event_state_items
    } else {
        StateCollectionItems::default()
    };
    if !state_collection_items(state)
        .saturating_add(protected_state_collection_items(control))
        .saturating_add(unreserved_state_items)
        .fits_limit(inner.config.journal.max_state_collection_items)
    {
        return Err(commit_rejected());
    }
    let mut protected_artifacts = control
        .reservations
        .values()
        .flat_map(|entry| entry.missing_artifact_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    protected_artifacts.extend(file_changes::protected_file_change_artifacts(control));
    let protected_unknown_artifacts = control
        .reservations
        .values()
        .try_fold(0_usize, |total, entry| {
            total.checked_add(entry.reserved_unknown_artifacts)
        })
        .ok_or_else(commit_rejected)?;
    if consuming_round.is_none() {
        protected_artifacts.extend(event_missing_artifacts.iter().cloned());
    }
    let remaining = inner
        .artifacts
        .capacity()
        .map_err(|_| commit_rejected())?
        .remaining();
    if protected_artifacts
        .len()
        .checked_add(protected_unknown_artifacts)
        .is_none_or(|required| required > remaining)
    {
        return Err(commit_rejected());
    }
    Ok(())
}

/// 在控制事件写入前保留其自身空间，并保护尚未确认的 Round、Turn 与文件变更事件。
///
/// 文件变更的 `Prepared` 与 `Applied` 不走该通用入口，但它们登记的专用 reservation
/// 必须在所有普通控制事件和 Agent 提交的容量计算中可见。这样控制事件不会抢走已经
/// 承诺给文件应用确认的最后一条 Journal 记录。
fn ensure_control_event_capacity(
    inner: &RuntimeSessionInner,
    control: &ControlState,
    state: &SessionState,
    event_id: &SessionEventId,
    event: &SessionEvent,
) -> Result<(), RuntimeError> {
    let event_bytes = encoded_record_len(&state.session_id, event_id, event)?;
    if event_bytes > inner.config.journal.max_event_bytes {
        return Err(RuntimeError::TurnUnpersistable);
    }
    let protected_tool_bytes = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_bytes)
        })
        .ok_or(RuntimeError::RecoveryRequired)?;
    let protected_tool_records = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_records)
        })
        .ok_or(RuntimeError::RecoveryRequired)?;
    let (protected_runtime_bytes, protected_runtime_records) =
        protected_runtime_terminal_capacity(control).ok_or(RuntimeError::RecoveryRequired)?;
    let (protected_file_change_bytes, protected_file_change_records) =
        file_changes::protected_file_change_journal_capacity(control)
            .ok_or(RuntimeError::RecoveryRequired)?;
    let protected_bytes = protected_tool_bytes
        .checked_add(protected_runtime_bytes)
        .and_then(|value| value.checked_add(protected_file_change_bytes))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let protected_records = protected_tool_records
        .checked_add(protected_runtime_records)
        .and_then(|value| value.checked_add(protected_file_change_records))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let log_len = journal_len(&inner.journal)?;
    if log_len
        .checked_add(protected_bytes)
        .and_then(|value| value.checked_add(event_bytes))
        .is_none_or(|value| value > inner.config.journal.max_log_bytes)
        || state
            .last_sequence
            .checked_add(protected_records)
            .and_then(|value| value.checked_add(1))
            .is_none_or(|value| value > inner.config.journal.max_records)
    {
        return Err(RuntimeError::TurnUnpersistable);
    }
    let protected_state_items = protected_state_collection_items(control)
        .saturating_add(file_changes::protected_file_change_state_items(control));
    if !state_collection_items(state)
        .saturating_add(protected_state_items)
        .saturating_add(state_collection_event_items(event))
        .fits_limit(inner.config.journal.max_state_collection_items)
    {
        return Err(RuntimeError::TurnUnpersistable);
    }
    Ok(())
}

/// 核对预检发现的缺失 Artifact 中哪些已经形成完整且可验证的磁盘 pair。
fn materialized_probe_artifacts(
    inner: &RuntimeSessionInner,
    probe: &ArtifactProbe,
) -> Result<BTreeSet<String>, RuntimeError> {
    let mut materialized = BTreeSet::new();
    for (artifact_id, artifact) in &probe.missing_uses {
        match inner.artifacts.validate_use(artifact) {
            Ok(()) => {
                materialized.insert(artifact_id.clone());
            }
            Err(ResourceError::ArtifactNotFound) => {}
            Err(error) => return Err(RuntimeError::Resource(error)),
        }
    }
    Ok(materialized)
}

/// Artifact 完整物化后立即、幂等地扣除已知或未知槽位，不等待 Journal 结果。
fn charge_materialized_reservation_artifacts(
    control: &mut ControlState,
    reservation_key: Option<&RoundKey>,
    materialized_artifacts: &BTreeSet<String>,
    materialized_uses: &BTreeMap<String, ArtifactUse>,
) -> Result<(), ()> {
    let newly_materialized = if let Some(key) = reservation_key {
        let entry = control.reservations.get(key).ok_or(())?;
        materialized_artifacts
            .iter()
            .filter(|artifact_id| !entry.materialized_artifact_ids.contains(*artifact_id))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let unknown_artifacts = if let Some(key) = reservation_key {
        let entry = control.reservations.get(key).ok_or(())?;
        newly_materialized
            .iter()
            .filter(|artifact_id| !entry.missing_artifact_ids.contains(*artifact_id))
            .count()
    } else {
        0
    };
    if let Some(key) = reservation_key {
        let entry = control.reservations.get(key).ok_or(())?;
        entry
            .reserved_unknown_artifacts
            .checked_sub(unknown_artifacts)
            .ok_or(())?;
    }
    for entry in control.reservations.values_mut() {
        for artifact_id in materialized_artifacts {
            entry.missing_artifact_ids.remove(artifact_id);
        }
    }
    if let Some(key) = reservation_key {
        let entry = control.reservations.get_mut(key).ok_or(())?;
        entry.reserved_unknown_artifacts -= unknown_artifacts;
        for artifact_id in newly_materialized {
            if let Some(artifact) = materialized_uses.get(&artifact_id) {
                entry
                    .materialized_artifact_uses
                    .insert(artifact_id.clone(), artifact.clone());
            }
            entry.materialized_artifact_ids.insert(artifact_id);
        }
    }
    Ok(())
}

/// 在事件已被 Journal 明确确认后，从匹配 reservation 中一次性扣除记录与字节。
fn charge_confirmed_reservation_event(
    control: &mut ControlState,
    reservation_key: Option<&RoundKey>,
    event_id: &str,
    event_bytes: u64,
    event_state_items: StateCollectionItems,
) -> Result<(), ()> {
    let Some(key) = reservation_key else {
        return Ok(());
    };
    let entry = control.reservations.get_mut(key).ok_or(())?;
    if entry.committed_event_ids.contains(event_id) {
        return Ok(());
    }
    let remaining_bytes = entry
        .reserved_journal_bytes
        .checked_sub(event_bytes)
        .ok_or(())?;
    let remaining_records = entry.reserved_journal_records.checked_sub(1).ok_or(())?;
    let mut remaining_state_items = entry.reserved_state_items;
    remaining_state_items.try_consume(event_state_items)?;
    entry.reserved_journal_bytes = remaining_bytes;
    entry.reserved_journal_records = remaining_records;
    entry.reserved_state_items = remaining_state_items;
    entry.committed_event_ids.insert(event_id.to_owned());
    Ok(())
}

/// 返回首次映射时应冻结的 Transcript revision。
fn mapping_transcript_revision(event: &AgentCommitEvent, state: &SessionState) -> Option<u64> {
    matches!(
        event.kind(),
        AgentCommitEventKind::ModelRoundCommitted { .. }
            | AgentCommitEventKind::RoundCommitted { .. }
            | AgentCommitEventKind::DynamicInputCommitted { .. }
            | AgentCommitEventKind::ContextCompactionApplied { .. }
    )
    .then_some(state.transcript_revision)
}

/// 返回首次映射压缩事件时应冻结的资源层作用域摘要。
fn mapping_compaction_digest(event: &AgentCommitEvent, state: &SessionState) -> Option<String> {
    let AgentCommitEventKind::ContextCompactionApplied { record } = event.kind() else {
        return None;
    };
    let turn_id = TurnId::new(event.turn_id().as_str()).ok()?;
    let agent_id = keencode_resources::AgentId::new(event.source_agent_id().as_str()).ok()?;
    state
        .compaction_source_digest_sha256(
            &turn_id,
            &agent_id,
            event.model_round(),
            record.replaced_start_index,
            record.replaced_end_index_exclusive,
        )
        .ok()
}

/// 冷恢复第一步：确认所有遗留终端进程已经丢失并记录取消退出。
fn recover_terminals(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    let terminals = inner
        .journal
        .state()?
        .terminals
        .values()
        .filter(|terminal| !terminal.exited)
        .map(|terminal| terminal.terminal_id.clone())
        .collect::<Vec<_>>();
    for terminal_id in terminals {
        append_runtime_resource_event(
            inner,
            recovery_event_id("terminal-exited", terminal_id.as_str())?,
            SessionEvent::TerminalExited {
                terminal_id,
                exit_code: None,
                cancelled: true,
            },
        )?;
    }
    Ok(())
}

/// 冷恢复第二步：副作用工具标记未知，其余未终态工具显式取消。
fn recover_tool_outcomes(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    let tools = inner
        .journal
        .state()?
        .tools
        .values()
        .filter(|tool| tool.outcome.is_none())
        .cloned()
        .collect::<Vec<_>>();
    for tool in tools {
        let event = if tool.execution_started && tool.request.effect == ToolEffect::ChangesState {
            SessionEvent::ToolSideEffectUnknown {
                request_id: tool.request.request_id.clone(),
                result: side_effect_unknown_result(&tool.request.model_tool_call_id),
            }
        } else {
            SessionEvent::ToolCompleted {
                request_id: tool.request.request_id.clone(),
                outcome: ToolOutcome {
                    status: ToolCompletionStatus::Cancelled,
                    result: PersistedToolResult {
                        tool_call_id: tool.request.model_tool_call_id.clone(),
                        content: vec![ToolResultPart::Text {
                            text: RECOVERY_CANCELLED_RESULT.to_owned(),
                        }],
                        is_error: true,
                    },
                },
            }
        };
        append_runtime_resource_event(
            inner,
            recovery_event_id("tool-outcome", tool.request.request_id.as_str())?,
            event,
        )?;
    }
    Ok(())
}

/// 冷恢复第三步：把已终态但未进入 Transcript 的工具交换合成为唯一恢复段。
fn recover_tool_transcript(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    loop {
        let state = inner.journal.state()?;
        let mut groups = BTreeMap::<(TurnId, keencode_resources::AgentId, u32), Vec<_>>::new();
        for tool in state
            .tools
            .values()
            .filter(|tool| tool.outcome.is_some() && tool.transcript_segment.is_none())
        {
            groups
                .entry((
                    tool.request.turn_id.clone(),
                    tool.request.agent_id.clone(),
                    tool.request.model_round,
                ))
                .or_default()
                .push(tool.clone());
        }
        let Some(((turn_id, agent_id, model_round), mut tools)) = groups.into_iter().next() else {
            return Ok(());
        };
        tools.sort_by_key(|tool| tool.request.request_index);
        let segment_index = state
            .transcript_segments()
            .filter(|segment| {
                segment.turn_id == turn_id
                    && segment.source_agent_id == agent_id
                    && segment.model_round == model_round
            })
            .map(|segment| segment.segment_index)
            .max()
            .map_or(Some(0), |value| value.checked_add(1))
            .ok_or(RuntimeError::RecoveryRequired)?;
        let identity = format!("{}:{}:{}", turn_id.as_str(), agent_id.as_str(), model_round);
        let assistant = SessionMessage {
            message_id: recovery_message_id("assistant", &identity),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Assistant,
            content: tools
                .iter()
                .map(|tool| MessagePart::ToolCall {
                    tool_call_id: tool.request.model_tool_call_id.clone(),
                    tool_name: tool.request.tool_name.clone(),
                    arguments: tool.request.arguments.clone(),
                })
                .collect(),
        };
        let results = SessionMessage {
            message_id: recovery_message_id("tool", &identity),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Tool,
            content: tools
                .iter()
                .map(|tool| {
                    let outcome = tool.outcome.as_ref().expect("筛选后工具必须有终态");
                    MessagePart::ToolResult {
                        tool_call_id: outcome.result.tool_call_id.clone(),
                        content: outcome.result.content.clone(),
                        is_error: outcome.result.is_error,
                    }
                })
                .collect(),
        };
        append_runtime_resource_event(
            inner,
            recovery_event_id("tool-transcript", &identity)?,
            SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::ModelRoundCompleted {
                        turn_id: turn_id.clone(),
                        source_agent_id: agent_id.clone(),
                        model_round,
                        requested_model: state.provider.as_ref().map_or_else(
                            || RECOVERY_UNKNOWN_MODEL.to_owned(),
                            |provider| provider.model.clone(),
                        ),
                        metadata: keencode_model::ResponseMetadata::default(),
                        usage: keencode_model::TokenUsage::unknown(),
                        stop_reason: keencode_model::StopReason::ToolUse,
                    },
                    SessionEvent::TranscriptSegmentCommitted {
                        segment: TranscriptSegment {
                            turn_id,
                            source_agent_id: agent_id,
                            model_round,
                            segment_index,
                            expected_transcript_revision: state.transcript_revision,
                            messages: vec![assistant, results],
                        },
                    },
                ],
            },
        )?;
    }
}

/// 冷恢复最后一步：在全部终端和工具事实收敛后停止遗留 Running Turn。
fn recover_turns(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    let turns = inner
        .journal
        .state()?
        .turns
        .values()
        .filter(|turn| turn.status == TurnStatus::Running)
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    for turn_id in turns {
        let state = inner.journal.state()?;
        let turn = state
            .turns
            .get(&turn_id)
            .ok_or(RuntimeError::RecoveryRequired)?;
        let event = recovery_turn_stopped_event(&turn_id, &turn.source_agent_id);
        append_runtime_resource_event(
            inner,
            recovery_event_id("turn-stopped", turn_id.as_str())?,
            event,
        )?;
    }
    Ok(())
}

/// 按根 Agent 或子 Agent 身份构造与冷恢复完全一致的单条 Turn 终态事件。
fn recovery_turn_stopped_event(
    turn_id: &TurnId,
    source_agent_id: &keencode_resources::AgentId,
) -> SessionEvent {
    let stopped = SessionEvent::TurnStopped {
        turn_id: turn_id.clone(),
        reason: TurnStopReason::Failed,
        message: RECOVERY_TURN_STOP_MESSAGE.to_owned(),
    };
    if source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
        stopped
    } else {
        SessionEvent::AtomicBatch {
            events: vec![
                stopped,
                SessionEvent::SubAgentStatusChanged {
                    agent_id: source_agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    status: SubAgentStatus::Failed,
                    result_summary: Some(RECOVERY_TURN_STOP_MESSAGE.to_owned()),
                },
            ],
        }
    }
}

/// 冷恢复收尾：停止已经登记但尚未来得及创建 Turn 的 Pending 子 Agent。
fn recover_pending_sub_agents(inner: &RuntimeSessionInner) -> Result<(), RuntimeError> {
    let pending_agents = inner
        .journal
        .state()?
        .sub_agents
        .values()
        .filter(|agent| agent.status == SubAgentStatus::Pending && agent.current_turn_id.is_none())
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    for agent_id in pending_agents {
        append_runtime_resource_event(
            inner,
            recovery_event_id("pending-agent-stopped", agent_id.as_str())?,
            SessionEvent::SubAgentStatusChanged {
                agent_id,
                turn_id: None,
                status: SubAgentStatus::Stopped,
                result_summary: None,
            },
        )?;
    }
    Ok(())
}

/// 为冷恢复事件生成短小、稳定且满足路径段规则的幂等身份。
fn recovery_event_id(kind: &str, identity: &str) -> Result<SessionEventId, RuntimeError> {
    SessionEventId::new(format!(
        "recovery-{kind}-{}",
        digest_hex(identity.as_bytes())
    ))
    .map_err(RuntimeError::from)
}

/// 为恢复合成消息生成不与普通 Runtime 消息冲突的稳定身份。
fn recovery_message_id(kind: &str, identity: &str) -> String {
    format!("recovery-{kind}-{}", digest_hex(identity.as_bytes()))
}

/// 返回不包含模型正文、工具参数或资源路径的固定预检不可持久化错误。
fn preflight_unpersistable() -> AgentToolRoundPreflightError {
    AgentToolRoundPreflightError::unpersistable("工具 Round 已知内容无法无损持久化")
}

/// 返回不包含底层路径或正文的固定预检暂不可用错误。
fn preflight_unavailable() -> AgentToolRoundPreflightError {
    AgentToolRoundPreflightError::unavailable("Session 持久化入口暂不可用")
}

/// 返回确认没有提交且不包含事件正文的固定错误。
fn commit_rejected() -> AgentCommitSinkError {
    AgentCommitSinkError::rejected("权威事件未提交")
}

/// 返回无法确认提交状态且要求恢复的固定错误。
fn commit_indeterminate() -> AgentCommitSinkError {
    AgentCommitSinkError::indeterminate("权威事件提交状态不确定，需要恢复")
}

#[cfg(test)]
mod tests;
