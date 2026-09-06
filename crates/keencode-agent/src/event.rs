//! Agent Runtime 的 Provider 中立实时事件与权威提交出口。

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use keencode_model::{
    Message, ModelError, ModelStreamEvent, ResponseMetadata, StopReason, TokenUsage, ToolCall,
    ToolResult,
};

use crate::{
    AgentEventId, AgentId, ContextCompressionRecord, ContextCompressionTrigger, SessionId,
    ToolCallId, ToolEffect, TurnId, ids::allocate_agent_event_id,
};

/// 事件 Sink 返回的对象安全异步结果。
pub type AgentEventFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), AgentEventSinkError>> + Send + 'a>>;

/// 接收单个 Turn 实时模型事件的 Provider 中立出口。
///
/// Runner 对同一 Turn 串行调用此接口。实现不得把投递工作脱离当前 Future：当
/// Future 因取消或超时被丢弃时，事件之后不得再被发布。不同 Turn 可以并发调用
/// 同一个 Sink，因此实现需要自行保证并发安全。工具、压缩和 Transcript 等权威
/// 事实必须使用同步的 [`AgentCommitSink`]，不能通过本接口延迟提交。
pub trait AgentEventSink: Send + Sync {
    /// 接收一个已经绑定可信 Runtime 身份的事件，并在可靠接收后返回。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a>;
}

/// 默认丢弃全部实时事件且不会产生背压的 Sink。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopAgentEventSink;

impl AgentEventSink for NoopAgentEventSink {
    /// 立即确认事件，保持未注入 Sink 时的简单调用方式。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

/// Sink 未能确认可靠接收事件时返回的稳定错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEventSinkError {
    /// 不包含事件正文或凭据的安全错误说明。
    message: String,
}

impl AgentEventSinkError {
    /// 创建一个 Sink 错误；Runner 会在进入终态前对说明执行有界截断。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 返回 Sink 提供的安全错误说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentEventSinkError {
    /// 输出不包含实时事件正文的错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentEventSinkError {}

/// 同步权威提交失败后决定是否可以释放关联资源的稳定分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentCommitSinkErrorKind {
    /// Sink 确认事件没有提交，调用方可以安全释放关联预留。
    Rejected,
    /// Sink 无法确认事件是否已提交，调用方必须保留恢复所需状态。
    Indeterminate,
}

/// 同步权威提交失败时返回的稳定错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCommitSinkError {
    /// 决定调用方释放资源还是冻结等待恢复的稳定分类。
    kind: AgentCommitSinkErrorKind,
    /// 不包含事件正文、工具结果或凭据的安全错误说明。
    message: String,
}

impl AgentCommitSinkError {
    /// 创建一个确认未提交、允许安全重试或释放的错误。
    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: AgentCommitSinkErrorKind::Rejected,
            message: message.into(),
        }
    }

    /// 创建一个无法确认是否已经提交、必须保留恢复状态的错误。
    pub fn indeterminate(message: impl Into<String>) -> Self {
        Self {
            kind: AgentCommitSinkErrorKind::Indeterminate,
            message: message.into(),
        }
    }

    /// 返回决定关联资源处置方式的稳定分类。
    pub const fn kind(&self) -> AgentCommitSinkErrorKind {
        self.kind
    }

    /// 返回 Sink 提供的安全错误说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentCommitSinkError {
    /// 输出不包含权威事件正文的错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentCommitSinkError {}

/// 工具 Round 预检失败后决定 Turn 终态语义的稳定分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentToolRoundPreflightErrorKind {
    /// 已冻结模型内容无法在当前持久化约束下无损保存，应按上下文阻塞结束。
    Unpersistable,
    /// 持久层锁、IO 或只读状态使本次预检无法完成，应按普通失败结束。
    Unavailable,
}

/// 工具 Round 在副作用前不能完成持久化预检时返回的安全错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentToolRoundPreflightError {
    /// 决定 Turn 进入上下文阻塞还是普通失败的稳定分类。
    kind: AgentToolRoundPreflightErrorKind,
    /// 不包含模型正文、工具参数或凭据的安全说明。
    message: String,
}

impl AgentToolRoundPreflightError {
    /// 创建一个确定无法无损持久化的预检错误。
    pub fn unpersistable(message: impl Into<String>) -> Self {
        Self {
            kind: AgentToolRoundPreflightErrorKind::Unpersistable,
            message: message.into(),
        }
    }

    /// 创建一个因持久层暂不可用而无法完成检查的预检错误。
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: AgentToolRoundPreflightErrorKind::Unavailable,
            message: message.into(),
        }
    }

    /// 返回决定 Turn 终态的稳定错误分类。
    pub const fn kind(&self) -> AgentToolRoundPreflightErrorKind {
        self.kind
    }

    /// 返回不包含模型内容或凭据的安全说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentToolRoundPreflightError {
    /// 输出不包含预检候选正文的错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentToolRoundPreflightError {}

/// 工具 Round 预检与最终提交共用的不可变身份绑定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentToolRoundBinding {
    /// 当前候选所属的根 Session。
    session_id: SessionId,
    /// 当前候选所属的用户 Turn。
    turn_id: TurnId,
    /// 产生当前候选的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 当前模型调用使用的 Provider 中立模型标识。
    model: String,
    /// 当前 Turn 状态机记录的逻辑模型 Round。
    model_round: u32,
    /// 当前 Round 即将提交的 Transcript 段序号。
    segment_index: u32,
}

impl AgentToolRoundBinding {
    /// 由 Runner 使用可信身份创建不能被 Provider 改写的 Round 绑定。
    pub(crate) fn new(
        session_id: SessionId,
        turn_id: TurnId,
        source_agent_id: AgentId,
        model: String,
        model_round: u32,
        segment_index: u32,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            source_agent_id,
            model,
            model_round,
            segment_index,
        }
    }

    /// 返回当前绑定所属的根 Session。
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回当前绑定所属的用户 Turn。
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// 返回产生当前绑定的 Agent。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回当前绑定使用的模型标识。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回当前绑定所属的逻辑模型 Round。
    pub const fn model_round(&self) -> u32 {
        self.model_round
    }

    /// 返回当前绑定占用的 Transcript 段序号。
    pub const fn segment_index(&self) -> u32 {
        self.segment_index
    }

    /// 判断权威事件是否正是本绑定允许的一次 Round 提交。
    pub(crate) fn matches_commit_event(&self, event: &AgentCommitEvent) -> bool {
        let segment_index = match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted { segment_index, .. }
            | AgentCommitEventKind::RoundCommitted { segment_index, .. } => segment_index,
            _ => return false,
        };
        self.matches_event_identity(event) && segment_index == &self.segment_index
    }

    /// 判断工具生命周期事件是否属于本 Permit 冻结的同一模型 Round。
    pub(crate) fn matches_event_identity(&self, event: &AgentCommitEvent) -> bool {
        event.session_id() == &self.session_id
            && event.turn_id() == &self.turn_id
            && event.source_agent_id() == &self.source_agent_id
            && event.model() == self.model
            && event.model_round() == self.model_round
    }
}

/// 工具副作用开始前交给 Session 层验证的不可变模型 Round 内容。
///
/// 该对象不是权威事件，不分配 [`AgentEventId`]，也不能推进 Journal sequence。
/// `assistant_message` 已包含全部 PreToolUse 修改后的最终工具参数；Runner 在预检
/// 成功后必须把同一份冻结消息用于最终 [`AgentCommitEventKind::ModelRoundCommitted`]。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolRoundPreflight {
    /// 预检和最终提交必须完全一致的可信 Round 身份。
    binding: AgentToolRoundBinding,
    /// 预检与最终提交必须完全一致的 Provider 响应元数据、用量和停止原因。
    completion: ModelRoundCompletion,
    /// 已写回最终工具参数且之后不得改写的完整 Assistant 消息。
    assistant_message: Message,
    /// 预检前已经确定且最终 Round 必须包含的 PreToolUse 上下文消息。
    pre_tool_context: Vec<Message>,
}

impl AgentToolRoundPreflight {
    /// 由 Runner 使用可信 Turn 身份和冻结消息创建只读预检候选。
    pub(crate) fn new(
        binding: AgentToolRoundBinding,
        completion: ModelRoundCompletion,
        assistant_message: Message,
        pre_tool_context: Vec<Message>,
    ) -> Self {
        Self {
            binding,
            completion,
            assistant_message,
            pre_tool_context,
        }
    }

    /// 返回当前候选所属的根 Session。
    pub const fn session_id(&self) -> &SessionId {
        self.binding.session_id()
    }

    /// 返回当前候选所属的用户 Turn。
    pub const fn turn_id(&self) -> &TurnId {
        self.binding.turn_id()
    }

    /// 返回产生当前候选的 Agent。
    pub const fn source_agent_id(&self) -> &AgentId {
        self.binding.source_agent_id()
    }

    /// 返回当前模型调用使用的模型标识。
    pub fn model(&self) -> &str {
        self.binding.model()
    }

    /// 返回当前候选所属的逻辑模型 Round。
    pub const fn model_round(&self) -> u32 {
        self.binding.model_round()
    }

    /// 返回当前候选即将占用的 Transcript 段序号。
    pub const fn segment_index(&self) -> u32 {
        self.binding.segment_index()
    }

    /// 返回预检时冻结的 Provider 响应元数据、用量和停止原因。
    pub const fn completion(&self) -> &ModelRoundCompletion {
        &self.completion
    }

    /// 返回已经冻结且最终提交必须复用的完整 Assistant 消息。
    pub const fn assistant_message(&self) -> &Message {
        &self.assistant_message
    }

    /// 返回预检时已经确定的全部 PreToolUse 上下文消息。
    pub fn pre_tool_context(&self) -> &[Message] {
        &self.pre_tool_context
    }

    /// 把预检候选拆为 Runner 签发 Permit 所需的全部冻结内容。
    pub(crate) fn into_parts(
        self,
    ) -> (
        AgentToolRoundBinding,
        ModelRoundCompletion,
        Message,
        Vec<Message>,
    ) {
        (
            self.binding,
            self.completion,
            self.assistant_message,
            self.pre_tool_context,
        )
    }
}

/// Sink 为已预留持久化容量提供的一次性 RAII 状态。
///
/// 包装层会在提前退出时调用 `release`，成功提交后调用 `consume`。两个接口均使用
/// `Box<Self>` 接收者，保持对象安全并禁止重复结束同一预留。
pub trait AgentToolRoundReservation: Send {
    /// 在匹配 Round 已同步提交后一次性消费预留。
    fn consume(self: Box<Self>);

    /// 在匹配 Round 未提交时一次性释放预留。
    fn release(self: Box<Self>);

    /// 工具生命周期或 Round 提交结果不确定时保留预留与完整事件，供 Session 恢复对账。
    fn retain_indeterminate(self: Box<Self>, event: AgentCommitEvent);
}

/// 同步接收工具、压缩和 Transcript 权威事实的提交出口。
///
/// `preflight_tool_round` 不得追加事件、创建 Artifact、推进 sequence 或改变权威投影。
/// 它用于在任何工具生命周期和真实副作用前拒绝无法无损持久化的完整模型响应与已知
/// PreToolUse 上下文。成功时返回的 Permit 必须持有这些已知内容所需的保守容量预算，
/// 直到匹配 Round 提交成功而被消费，任意明确失败、取消和返回路径 Drop 后释放，或某个
/// 工具生命周期及 Round 提交结果不确定时连同完整事件转入恢复保留。
/// 预检不承诺尚未知晓的 ToolResult、图片和 PostToolUse 内容必然可提交；最终完整事件仍
/// 必须由 Permit 调用同一 Sink 的 `commit` 同步确认。
///
/// `commit` 在返回前必须已经完成提交或明确失败，不得把工作交给后台任务。调用一旦
/// 开始便不受 Turn Future 被丢弃、取消或 Tokio timeout 影响。相同事件可能在明确
/// 失败后以不变的 [`AgentEventId`] 重投；实现必须把相同身份与相同内容视为幂等成功，
/// 并拒绝相同身份对应不同内容。不同 Turn 可以并发调用同一个 Sink。
pub trait AgentCommitSink: Send + Sync {
    /// 同步提交一次 Provider Round 的明确用量和墙钟耗时。
    ///
    /// Runner 会在解析或执行该响应中的任何工具调用前调用此接口；响应流失败时，若已
    /// 收到明确 Usage，也会在提交不完整 Transcript 前调用此接口。实现必须使用
    /// [`ModelRoundUsage`] 携带的稳定 Session、Turn、Agent、Round、调用尝试与调用用途执行
    /// 幂等提交；相同身份、用途和正文的重试不得重复累计，不同正文必须拒绝。
    fn commit_model_round_usage(
        &self,
        _usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        Ok(())
    }

    /// 同步验证一个工具 Round 的不可变 Assistant 内容可以无损进入持久层。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError>;

    /// 同步提交一个已经绑定可信 Runtime 身份的权威事件。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError>;
}

/// 默认确认全部权威事件且不产生副作用的提交 Sink。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopAgentCommitSink;

/// Noop Sink 显式返回且无需释放外部容量的一次性预留。
struct NoopAgentToolRoundReservation;

impl AgentToolRoundReservation for NoopAgentToolRoundReservation {
    /// 消费纯内存预留，不执行额外动作。
    fn consume(self: Box<Self>) {}

    /// 释放纯内存预留，不执行额外动作。
    fn release(self: Box<Self>) {}

    /// Noop Sink 不会产生不确定提交；保留接口显式丢弃测试事件。
    fn retain_indeterminate(self: Box<Self>, _event: AgentCommitEvent) {}
}

impl AgentCommitSink for NoopAgentCommitSink {
    /// 立即确认工具 Round 预检，保持纯内存运行方式不依赖持久层。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(Box::new(NoopAgentToolRoundReservation))
    }

    /// 立即确认权威事件，保持未注入持久层时的纯内存运行方式。
    fn commit(&self, _event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        Ok(())
    }
}

/// Runner 等待 Sink 可靠接收事件失败时的稳定分类。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentEventDeliveryError {
    /// Sink 在硬时限内返回失败；事件是否已落地需由稳定事件身份幂等对账。
    SinkFailed {
        /// 已经执行有界截断且不包含事件正文的说明。
        message: String,
    },
    /// Sink 没有在配置的硬时限内确认接收。
    TimedOut {
        /// 当前单事件允许等待的最大毫秒数。
        maximum_ms: u64,
    },
}

impl fmt::Display for AgentEventDeliveryError {
    /// 输出适合 Turn 终态诊断的稳定中文说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SinkFailed { message } => write!(formatter, "实时事件 Sink 失败：{message}"),
            Self::TimedOut { maximum_ms } => {
                write!(formatter, "实时事件 Sink 超过接收时限 {maximum_ms} 毫秒")
            }
        }
    }
}

impl Error for AgentEventDeliveryError {}

/// 一个由 Runner 绑定可信身份并按 Provider 到达顺序发送的实时事件。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentStreamEvent {
    /// Runtime 在首次投递前分配且所有重投保持不变的幂等身份。
    event_id: AgentEventId,
    /// 当前事件所属的根 Session，由 Turn 请求控制面提供。
    session_id: SessionId,
    /// 当前事件所属的用户 Turn，由 Turn 请求控制面提供。
    turn_id: TurnId,
    /// 产生当前事件的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 当前模型调用使用的 Provider 中立模型标识。
    model: String,
    /// 当前 Turn 状态机记录的逻辑模型 Round，从一开始递增。
    model_round: u32,
    /// Provider 事件或由 Runner 生成的唯一失败边界。
    kind: AgentStreamEventKind,
}

impl AgentStreamEvent {
    /// 由 Runner 使用可信 Turn 身份创建事件信封。
    pub(crate) fn new(
        session_id: SessionId,
        turn_id: TurnId,
        source_agent_id: AgentId,
        model: String,
        model_round: u32,
        kind: AgentStreamEventKind,
    ) -> Self {
        Self {
            event_id: allocate_agent_event_id(),
            session_id,
            turn_id,
            source_agent_id,
            model,
            model_round,
            kind,
        }
    }

    /// 返回 Runtime 冻结且重投时不变的事件投递身份。
    pub const fn event_id(&self) -> &AgentEventId {
        &self.event_id
    }

    /// 返回当前事件所属的根 Session。
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回当前事件所属的用户 Turn。
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// 返回产生当前事件的 Agent。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回当前模型调用使用的模型标识。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回当前事件所属的逻辑模型 Round。
    pub const fn model_round(&self) -> u32 {
        self.model_round
    }

    /// 返回 Provider 事件或失败边界的只读视图。
    pub const fn kind(&self) -> &AgentStreamEventKind {
        &self.kind
    }

    /// 消费信封并返回 Provider 事件或失败边界。
    pub fn into_kind(self) -> AgentStreamEventKind {
        self.kind
    }
}

/// 一个由 Runner 绑定可信身份并同步提交的权威事件。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCommitEvent {
    /// Runtime 在首次提交前分配且所有重投保持不变的幂等身份。
    event_id: AgentEventId,
    /// 当前事件所属的根 Session，由 Turn 请求控制面提供。
    session_id: SessionId,
    /// 当前事件所属的用户 Turn，由 Turn 请求控制面提供。
    turn_id: TurnId,
    /// 产生当前事件的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 当前模型调用使用的 Provider 中立模型标识。
    model: String,
    /// 当前 Turn 状态机记录的逻辑模型 Round，从一开始递增。
    model_round: u32,
    /// 工具、压缩或 Transcript 的权威事实。
    kind: AgentCommitEventKind,
}

impl AgentCommitEvent {
    /// 由 Runner 使用可信 Turn 身份创建权威事件信封。
    pub(crate) fn new(
        session_id: SessionId,
        turn_id: TurnId,
        source_agent_id: AgentId,
        model: String,
        model_round: u32,
        kind: AgentCommitEventKind,
    ) -> Self {
        Self {
            event_id: allocate_agent_event_id(),
            session_id,
            turn_id,
            source_agent_id,
            model,
            model_round,
            kind,
        }
    }

    /// 返回 Runtime 冻结且重投时不变的事件提交身份。
    pub const fn event_id(&self) -> &AgentEventId {
        &self.event_id
    }

    /// 返回当前事件所属的根 Session。
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回当前事件所属的用户 Turn。
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// 返回产生当前事件的 Agent。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回当前模型调用使用的模型标识。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回当前事件所属的逻辑模型 Round。
    pub const fn model_round(&self) -> u32 {
        self.model_round
    }

    /// 返回权威事件载荷的只读视图。
    pub const fn kind(&self) -> &AgentCommitEventKind {
        &self.kind
    }

    /// 消费信封并返回权威事件载荷。
    pub fn into_kind(self) -> AgentCommitEventKind {
        self.kind
    }
}

/// 工具生命周期结束时的 Provider 中立分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCompletionStatus {
    /// 工具实现成功返回了可提交结果。
    Succeeded,
    /// 工具实现或 Runtime 在执行后返回失败结果。
    Failed,
    /// 工具没有执行或在 Turn 取消后停止。
    Cancelled,
}

/// 上下文压缩失败且原 Transcript 未改变时使用的稳定分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextCompactionFailureKind {
    /// 摘要模型请求或响应归约失败。
    Model,
    /// 当前历史无法满足替换范围或 Token 缩减预算。
    Budget,
    /// 已生成压缩结果，但权威 Session 提交失败。
    Storage,
    /// 摘要或持久化替换记录违反上下文不变量。
    InvalidResult,
}

/// Agent 实时出口能够观察的模型流和上下文压缩临时事件。
#[derive(Clone, Debug, PartialEq)]
pub enum AgentStreamEventKind {
    /// Provider Adapter 产生的原始统一事件；Sink 确认后同一事件才会交给归约器。
    ModelEvent {
        /// 文本、推理、工具参数、Usage 或消息起止事件。
        event: ModelStreamEvent,
    },
    /// Provider、取消或严格归约失败形成的模型 Round 失败边界。
    ModelFailure {
        /// Provider 中立且不包含凭据的稳定错误。
        error: ModelError,
    },
    /// Runtime 在修改有效 Transcript 前开始一次上下文压缩尝试。
    ContextCompactionStarted {
        /// 压缩前完整 Provider 中立请求的估算 Token 数。
        estimated_tokens: u64,
    },
    /// 上下文压缩失败且原 Transcript 保持不变。
    ContextCompactionFailed {
        /// 不包含模型正文、工具结果或凭据的稳定失败分类。
        failure_kind: ContextCompactionFailureKind,
    },
}

/// 一次 Provider 模型调用可持久化的响应元数据与可空用量。
///
/// 成功调用携带完整归约响应；流失败时只携带实时 Sink 已确认的元数据和用量，
/// `stop_reason` 由 Provider 结束事件提供，或由 Runtime 为未收到结束事件的失败补充。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRoundCompletion {
    /// Provider 返回的响应标识和实际模型；未报告字段保持为空。
    pub metadata: ResponseMetadata,
    /// Provider 明确报告的 Token 用量；未知字段保持 `None`。
    pub usage: TokenUsage,
    /// Provider 中立的响应结束原因；失败且未收到结束事件时由 Runtime 补充。
    pub stop_reason: StopReason,
}

/// 动态输入被 Runner 请求的内存边界。
///
/// 该枚举只描述当前模型循环的采样边界，不会写入持久事件或恢复状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentDynamicInputBoundary {
    /// 开始一次模型采样前；邮箱和用户 Steer 都可以进入本轮上下文。
    BeforeModelSampling,
    /// 最终候选响应经过 Stop Hook 后；仅允许读取当前 Turn 的用户 Steer。
    AfterFinalCandidate,
}

/// 一次 Provider 模型调用可观察的动态输入来源类别。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AgentDynamicInputKind {
    /// 来自 Agent 间权威 mailbox 的输入。
    Mailbox,
    /// 来自用户在当前 Turn 中追加的 Steer 输入。
    UserSteer,
}

impl AgentDynamicInputKind {
    /// 返回用于跨 Runtime 映射的稳定英文标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mailbox => "mailbox",
            Self::UserSteer => "user_steer",
        }
    }
}

/// 动态输入批次中可由持久层核验的单条消费水位。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentDynamicInputReceipt {
    /// 动态输入的权威来源类别。
    kind: AgentDynamicInputKind,
    /// 本批实际写入模型上下文的最大单调序号。
    through_sequence: u64,
}

impl AgentDynamicInputReceipt {
    /// 创建一条动态输入水位；零水位会在 Runner 的安全边界中拒绝。
    pub const fn new(kind: AgentDynamicInputKind, through_sequence: u64) -> Self {
        Self {
            kind,
            through_sequence,
        }
    }

    /// 返回动态输入来源类别。
    pub const fn kind(self) -> AgentDynamicInputKind {
        self.kind
    }

    /// 返回本批实际写入的最大单调序号。
    pub const fn through_sequence(self) -> u64 {
        self.through_sequence
    }
}

/// 一次 Provider 调用在当前逻辑 Round 中承担的稳定用途。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCallPurpose {
    /// 正常 Agent 推理、文本生成或工具选择请求。
    AgentRound,
    /// 请求前预算阈值触发的上下文摘要调用。
    ContextCompactionBudget,
    /// Provider 返回上下文超限后触发的唯一摘要调用。
    ContextCompactionProviderOverflow,
}

impl ModelCallPurpose {
    /// 返回可用于持久 operation ID 且跨版本稳定的英文标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentRound => "agent_round",
            Self::ContextCompactionBudget => "context_compaction_budget",
            Self::ContextCompactionProviderOverflow => "context_compaction_provider_overflow",
        }
    }

    /// 从压缩触发原因构造互不冲突的摘要调用用途。
    pub const fn for_context_compaction(trigger: ContextCompressionTrigger) -> Self {
        match trigger {
            ContextCompressionTrigger::Budget => Self::ContextCompactionBudget,
            ContextCompressionTrigger::ProviderOverflow => Self::ContextCompactionProviderOverflow,
        }
    }
}

/// 一次 Provider 模型调用的稳定身份、明确用量与实际墙钟耗时。
///
/// 该对象在完整响应形成后，或流失败但已经收到明确用量时，交给
/// [`AgentCommitSink`]。它不是 Session Journal 事件，不分配 [`AgentEventId`]；持久实现
/// 必须把 Session、Turn、Agent、Round、调用尝试与调用用途共同纳入可跨重启复用的
/// operation ID。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRoundUsage {
    /// 模型调用所属根 Session。
    session_id: SessionId,
    /// 模型调用所属 Turn。
    turn_id: TurnId,
    /// 发起调用的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 当前调用实际使用的 Provider 中立模型标识。
    model: String,
    /// 当前 Turn 内从一开始严格递增的逻辑模型 Round。
    model_round: u32,
    /// 当前 Turn 内从一开始严格递增的模型调用尝试；同一 Round 的重试使用不同值。
    call_attempt: u32,
    /// 区分正常 Agent Round 与同一 Round 内的上下文摘要调用。
    purpose: ModelCallPurpose,
    /// 已由实时 Sink 确认的响应元数据与 Token 用量，以及 Provider 或 Runtime 的停止原因。
    completion: ModelRoundCompletion,
    /// 从发起 Provider 请求到完整响应归约结束的单调时钟毫秒数。
    elapsed_millis: u64,
}

impl ModelRoundUsage {
    /// 由 Runner 使用受信 Turn 身份和单调时钟结果创建完整用量事实。
    /// 参数保持逐字段显式传入，避免把跨 Session、Turn、Agent 与调用尝试的身份误配。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: SessionId,
        turn_id: TurnId,
        source_agent_id: AgentId,
        model: String,
        model_round: u32,
        call_attempt: u32,
        completion: ModelRoundCompletion,
        elapsed_millis: u64,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            source_agent_id,
            model,
            model_round,
            call_attempt,
            purpose: ModelCallPurpose::AgentRound,
            completion,
            elapsed_millis,
        }
    }

    /// 将新建用量事实绑定到同一 Round 内的明确调用用途。
    pub(crate) fn with_purpose(mut self, purpose: ModelCallPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    /// 返回模型调用所属根 Session。
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回模型调用所属 Turn。
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// 返回发起模型调用的根 Agent 或单层子 Agent。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回当前模型调用使用的 Provider 中立模型标识。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回当前 Turn 内的逻辑模型 Round。
    pub const fn model_round(&self) -> u32 {
        self.model_round
    }

    /// 返回当前 Turn 内稳定且单调的模型调用尝试序号。
    pub const fn call_attempt(&self) -> u32 {
        self.call_attempt
    }

    /// 返回本次 Provider 调用在逻辑 Round 中承担的稳定用途。
    pub const fn purpose(&self) -> ModelCallPurpose {
        self.purpose
    }

    /// 返回模型调用可持久化的响应事实；失败调用可能只包含部分已确认字段。
    pub const fn completion(&self) -> &ModelRoundCompletion {
        &self.completion
    }

    /// 返回单调时钟测得的实际调用毫秒数。
    pub const fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis
    }
}

impl ModelRoundCompletion {
    /// 从已经通过流归约校验的模型响应复制持久化元数据。
    pub fn from_response(response: &keencode_model::ModelResponse) -> Self {
        Self {
            metadata: response.metadata.clone(),
            usage: response.usage.clone(),
            stop_reason: response.stop_reason.clone(),
        }
    }
}

/// Agent 同步提交出口能够观察的工具、压缩与 Transcript 权威事件。
#[derive(Clone, Debug, PartialEq)]
pub enum AgentCommitEventKind {
    /// 一次成功上下文压缩已经形成可恢复记录。
    ContextCompactionApplied {
        /// 可由 Session 层持久化并在恢复时重新应用的完整记录。
        record: ContextCompressionRecord,
    },
    /// 一个通过工具定义、Hook、计划守卫和批次预检的请求已经冻结。
    ToolRequested {
        /// 当前调用在原始模型响应中的零基稳定位置；被预检淘汰的调用允许形成间隙。
        request_index: u32,
        /// 已完成有界校验的可信工具调用标识。
        tool_call_id: ToolCallId,
        /// 最终提交给工具实现的名称与参数。
        call: ToolCall,
        /// 最终输入对应的只读或状态变更分类。
        effect: ToolEffect,
    },
    /// 工具已经越过全部执行前守卫，即将调用真实实现。
    ToolExecutionStarted {
        /// 即将执行的可信工具调用标识。
        tool_call_id: ToolCallId,
    },
    /// 一个已请求工具已经形成唯一结果。
    ToolCompleted {
        /// 与请求和执行起点一致的可信工具调用标识。
        tool_call_id: ToolCallId,
        /// 区分成功、失败和未执行或取消的终态。
        status: ToolCompletionStatus,
        /// 按模型协议中立格式保留的完整结果。
        result: ToolResult,
    },
    /// 一个完整 Provider 模型响应与其首个 Transcript 段需要原子提交。
    ModelRoundCommitted {
        /// 同一 Turn、Agent 和模型 Round 内从零开始的提交段序号。
        segment_index: u32,
        /// 不得把 Provider 未报告字段改写为零的响应元数据。
        completion: ModelRoundCompletion,
        /// 按 Transcript 顺序追加且不会在后续 Round 改写的消息。
        messages: Vec<Message>,
    },
    /// 当前逻辑 Round 的一段消息已经可靠提交。
    RoundCommitted {
        /// 同一 Turn、Agent 和模型 Round 内从零开始的提交段序号。
        segment_index: u32,
        /// 按 Transcript 顺序追加且不会在后续 Round 改写的消息。
        messages: Vec<Message>,
    },
    /// 一批采样前动态输入与其权威消费水位原子提交。
    DynamicInputCommitted {
        /// 同一 Turn、Agent 和模型 Round 内从零开始的提交段序号。
        segment_index: u32,
        /// 由 Runtime Journal 用于恢复两阶段消费的稳定水位集合。
        receipts: Vec<AgentDynamicInputReceipt>,
        /// 当前采样前实际加入模型上下文的消息。
        messages: Vec<Message>,
    },
}
