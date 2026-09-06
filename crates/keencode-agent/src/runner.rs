//! Provider 中立的单 Turn Agent Loop 与工具调度。

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::{Either, select};
use futures_util::stream::FuturesUnordered;
use futures_util::{StreamExt, stream};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelProvider, ModelRequest, ModelResponse,
    ModelStream, ProviderCapabilities, ResponseMetadata, StopReason, StructuredOutputEnforcement,
    StructuredOutputFailureKind, TokenUsage, ToolCall, ToolChoice, ToolResult,
    collect_model_stream,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::context::{
    context_error_is_cancelled, context_error_model_usage, context_error_without_summary_usage,
};
use crate::event::AgentToolRoundBinding;
use crate::structured_output::{STRUCTURED_OUTPUT_TOOL_NAME, StructuredOutputMode};
use crate::tool::{
    NormalizedToolError, SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT, TOOL_OUTPUT_LIMIT_RESULT,
    ToolOutputRejection, ToolResultFootprint, ToolRoundOutputBudget, measure_tool_result,
    normalize_tool_error, validate_tool_output,
};
use crate::{
    AgentCommitEvent, AgentCommitEventKind, AgentCommitSink, AgentCommitSinkError,
    AgentCommitSinkErrorKind, AgentDynamicInputBoundary, AgentDynamicInputReceipt,
    AgentEventDeliveryError, AgentEventSink, AgentId, AgentStreamEvent, AgentStreamEventKind,
    AgentTool, AgentToolRoundPreflight, AgentToolRoundPreflightError,
    AgentToolRoundPreflightErrorKind, AgentToolRoundReservation, ContextCompactionFailureKind,
    ContextCompressionOutcome, ContextCompressionRecord, ContextCompressionTrigger, ContextError,
    ContextManager, CounterKind, HookError, HookInvocationContext, HookRuntime, ModelCallPurpose,
    ModelRoundCompletion, ModelRoundUsage, NoopAgentCommitSink, NoopAgentEventSink, PlanGuard,
    PlanGuardError, PostHookOutputBudget, PostToolUseContext, PostToolUseFailureContext,
    PreToolUseContext, ResolvedHookContext, ResolvedStopHook, SessionId, StopHookContext,
    TerminalReason, ToolCompletionStatus, ToolConcurrency, ToolContext, ToolEffect,
    ToolHookFailureKind, ToolInputHash, ToolOutputErrorCode, ToolRegistry, TurnCancellation,
    TurnId, TurnPhase, TurnState, TurnTransitionError,
};

/// Step 上限触发后交给唯一无工具总结 Round 的稳定说明。
const STEP_LIMIT_SUMMARY_INSTRUCTION: &str = "工具执行步骤已经达到本 Turn 的硬上限。不得继续调用工具；请只根据现有结果简要总结已完成工作、失败和剩余事项。";

/// PreToolUse 回调失败时写入配对结果且不回显 Hook 自有文本的固定说明。
const PRE_HOOK_FAILED_RESULT: &str = "PreToolUse Hook 失败，工具未执行";

/// Hook 输出超过硬预算时替换原始 Hook 文本的固定配对结果。
const HOOK_BUDGET_EXCEEDED_RESULT: &str = "Hook 输出超过上下文预算，工具未执行";

/// 工具返回无效 Provider 中立输出时交给模型的固定有界说明。
const INVALID_TOOL_OUTPUT_RESULT: &str = "工具返回了无效输出";

/// 重复失败观察用于区分无效工具输出的稳定错误码。
const INVALID_TOOL_OUTPUT_ERROR_CODE: &str = "invalid_output";

/// 每个权威事件同步提交时允许的总尝试次数；所有重投复用同一事件对象和稳定身份。
const AUTHORITATIVE_EVENT_MAX_COMMIT_ATTEMPTS: usize = 2;

/// 多个摘要物理请求聚合成一个逻辑用途时使用的保留调用尝试序号。
const AGGREGATED_CONTEXT_CALL_ATTEMPT: u32 = 0;

/// 单个模型安全边界最多接收的动态消息数量。
const MAX_DYNAMIC_INPUT_MESSAGES_PER_BOUNDARY: usize = 256;

/// 动态消息已提交后确认持久 claim 的最大尝试次数。
const DYNAMIC_INPUT_ACKNOWLEDGEMENT_ATTEMPTS: usize = 2;

/// 实时事件 Sink 错误进入 Turn 终态前允许使用的最大 UTF-8 字节数。
const MAX_EVENT_SINK_ERROR_MESSAGE_BYTES: usize = 1_024;

/// 权威提交 Sink 错误进入 Turn 终态前允许使用的最大 UTF-8 字节数。
const MAX_COMMIT_SINK_ERROR_MESSAGE_BYTES: usize = 1_024;

/// 一个 Turn 可使用的确定性模型 Round 与工具 Step 上限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunLimits {
    /// 单个 Turn 正常循环最多发起的模型请求次数；Step 熔断后的唯一总结请求不计入。
    pub max_rounds: u32,
    /// 单个 Turn 最多真正开始执行的工具次数。
    pub max_steps: u32,
    /// Turn 取消后等待工具完成进程树和临时资源清理的毫秒数。
    pub tool_cancel_grace_ms: u64,
    /// 单个实时事件等待 Sink 可靠接收的最大毫秒数。
    pub event_sink_timeout_ms: u64,
    /// 相同工具和规范化输入允许连续出现的最大次数。
    pub max_identical_tool_calls: u32,
    /// 相同工具、输入和错误码允许连续失败的最大次数。
    pub max_repeated_tool_failures: u32,
}

impl RunLimits {
    /// 创建两个上限都大于零的运行限制。
    pub const fn new(max_rounds: u32, max_steps: u32) -> Result<Self, RunLimitsError> {
        if max_rounds == 0 {
            return Err(RunLimitsError::ZeroRounds);
        }
        if max_steps == 0 {
            return Err(RunLimitsError::ZeroSteps);
        }
        Ok(Self {
            max_rounds,
            max_steps,
            tool_cancel_grace_ms: 5_000,
            event_sink_timeout_ms: 5_000,
            max_identical_tool_calls: 3,
            max_repeated_tool_failures: 3,
        })
    }

    /// 覆盖工具取消清理窗口；零毫秒会被拒绝。
    pub const fn with_tool_cancel_grace_ms(
        mut self,
        tool_cancel_grace_ms: u64,
    ) -> Result<Self, RunLimitsError> {
        if tool_cancel_grace_ms == 0 {
            return Err(RunLimitsError::ZeroToolCancelGrace);
        }
        self.tool_cancel_grace_ms = tool_cancel_grace_ms;
        Ok(self)
    }

    /// 覆盖单事件 Sink 接收时限；零毫秒会被拒绝。
    pub const fn with_event_sink_timeout_ms(
        mut self,
        event_sink_timeout_ms: u64,
    ) -> Result<Self, RunLimitsError> {
        if event_sink_timeout_ms == 0 {
            return Err(RunLimitsError::ZeroEventSinkTimeout);
        }
        self.event_sink_timeout_ms = event_sink_timeout_ms;
        Ok(self)
    }

    /// 覆盖连续同调用与重复真实失败上限；任一为零都会被拒绝。
    pub const fn with_loop_limits(
        mut self,
        max_identical_tool_calls: u32,
        max_repeated_tool_failures: u32,
    ) -> Result<Self, RunLimitsError> {
        if max_identical_tool_calls == 0 {
            return Err(RunLimitsError::ZeroIdenticalToolCalls);
        }
        if max_repeated_tool_failures == 0 {
            return Err(RunLimitsError::ZeroRepeatedToolFailures);
        }
        self.max_identical_tool_calls = max_identical_tool_calls;
        self.max_repeated_tool_failures = max_repeated_tool_failures;
        Ok(self)
    }
}

impl Default for RunLimits {
    /// 返回适合交互式编码任务的保守上限。
    fn default() -> Self {
        Self {
            max_rounds: 64,
            max_steps: 256,
            tool_cancel_grace_ms: 5_000,
            event_sink_timeout_ms: 5_000,
            max_identical_tool_calls: 3,
            max_repeated_tool_failures: 3,
        }
    }
}

/// Agent Loop 上限配置无效时返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunLimitsError {
    /// 模型 Round 上限不能为零。
    ZeroRounds,
    /// 工具 Step 上限不能为零。
    ZeroSteps,
    /// 工具取消清理窗口不能为零。
    ZeroToolCancelGrace,
    /// 实时事件 Sink 接收时限不能为零。
    ZeroEventSinkTimeout,
    /// 连续相同工具调用上限不能为零。
    ZeroIdenticalToolCalls,
    /// 重复真实工具失败上限不能为零。
    ZeroRepeatedToolFailures,
}

impl fmt::Display for RunLimitsError {
    /// 输出具体的零上限字段。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroRounds => formatter.write_str("模型 Round 上限必须大于零"),
            Self::ZeroSteps => formatter.write_str("工具 Step 上限必须大于零"),
            Self::ZeroToolCancelGrace => formatter.write_str("工具取消清理窗口必须大于零"),
            Self::ZeroEventSinkTimeout => formatter.write_str("实时事件 Sink 接收时限必须大于零"),
            Self::ZeroIdenticalToolCalls => formatter.write_str("连续相同工具调用上限必须大于零"),
            Self::ZeroRepeatedToolFailures => formatter.write_str("重复真实工具失败上限必须大于零"),
        }
    }
}

impl Error for RunLimitsError {}

/// Agent Loop 触发硬熔断时的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolLoopKind {
    /// 模型连续请求相同工具和规范化输入。
    IdenticalCall,
    /// 相同工具、输入和真实工具错误码重复失败。
    RepeatedFailure,
}

/// 启动一个 Agent Turn 所需的不可歧义输入。
#[derive(Clone, Debug)]
pub struct TurnRequest {
    /// 当前 Turn 所属的根 Session。
    session_id: SessionId,
    /// 当前 Turn 的稳定唯一标识。
    turn_id: TurnId,
    /// 发起当前 Turn 的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 不包含 Runtime 工具定义的 Provider 中立请求模板。
    model_request: ModelRequest,
    /// 在工具执行前强制生效的计划只读守卫。
    plan_guard: PlanGuard,
    /// 由 Session 控制面持有并可向工具传播的取消令牌。
    cancellation: TurnCancellation,
}

impl TurnRequest {
    /// 创建使用默认模型选项且尚未取消的 Turn 请求。
    pub fn new(
        session_id: SessionId,
        turn_id: TurnId,
        source_agent_id: AgentId,
        model: impl Into<String>,
        messages: Vec<Message>,
        plan_guard: PlanGuard,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            source_agent_id,
            model_request: ModelRequest::new(model, messages),
            plan_guard,
            cancellation: TurnCancellation::new(),
        }
    }

    /// 返回当前 Turn 所属的根 Session 标识。
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回当前 Turn 的稳定标识。
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// 返回执行当前 Turn 的根 Agent 或单层子 Agent 标识。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回不包含 Runtime 工具定义的 Provider 中立请求模板。
    pub const fn model_request(&self) -> &ModelRequest {
        &self.model_request
    }

    /// 返回在工具执行前强制生效的计划只读守卫。
    pub const fn plan_guard(&self) -> PlanGuard {
        self.plan_guard
    }

    /// 返回模型请求模板的可变引用；工具定义与消息会由 Agent Loop 覆盖。
    pub fn model_request_mut(&mut self) -> &mut ModelRequest {
        &mut self.model_request
    }

    /// 设置由 Session 控制面持有的取消令牌。
    pub fn set_cancellation(&mut self, cancellation: TurnCancellation) {
        self.cancellation = cancellation;
    }

    /// 返回当前 Turn 的取消令牌。
    pub const fn cancellation(&self) -> &TurnCancellation {
        &self.cancellation
    }
}

/// 动态消息 claim 或确认失败时返回的安全错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDynamicInputError {
    /// 不包含 mailbox、Steer 正文或凭据的稳定说明。
    message: String,
}

impl AgentDynamicInputError {
    /// 创建不包含动态正文的错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 返回可写入 Turn 终态的安全说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentDynamicInputError {
    /// 输出不包含动态正文的稳定说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentDynamicInputError {}

/// Runtime Journal 已确认动态消息后用于完成两阶段消费的持久回执。
pub trait AgentDynamicInputAcknowledgement: Send + Sync {
    /// 幂等确认本批消息已进入权威 Transcript。
    fn acknowledge(&self) -> Result<(), AgentDynamicInputError>;
}

/// 一个已在外部状态中持久 claim、尚未确认进入 Transcript 的消息批次。
pub struct AgentDynamicInputBatch {
    /// 按模型实际观察顺序排列的用户或开发者消息。
    messages: Vec<Message>,
    /// 与消息批次一一对应、供权威 Journal 恢复确认的消费水位。
    receipts: Vec<AgentDynamicInputReceipt>,
    /// 消息提交成功后必须调用的幂等确认回执。
    acknowledgement: Option<Arc<dyn AgentDynamicInputAcknowledgement>>,
}

impl AgentDynamicInputBatch {
    /// 创建带两阶段确认回执的非空动态消息批次。
    pub fn new(
        messages: Vec<Message>,
        acknowledgement: Arc<dyn AgentDynamicInputAcknowledgement>,
    ) -> Self {
        Self::new_with_receipts(messages, Vec::new(), acknowledgement)
    }

    /// 创建带有权威消费水位的非空动态消息批次。
    pub fn new_with_receipts(
        messages: Vec<Message>,
        receipts: Vec<AgentDynamicInputReceipt>,
        acknowledgement: Arc<dyn AgentDynamicInputAcknowledgement>,
    ) -> Self {
        Self {
            messages,
            receipts,
            acknowledgement: Some(acknowledgement),
        }
    }

    /// 创建当前安全边界没有动态输入的空批次。
    pub const fn empty() -> Self {
        Self {
            messages: Vec::new(),
            receipts: Vec::new(),
            acknowledgement: None,
        }
    }

    /// 拆分冻结消息和提交后确认回执。
    fn into_parts(
        self,
    ) -> (
        Vec<Message>,
        Vec<AgentDynamicInputReceipt>,
        Option<Arc<dyn AgentDynamicInputAcknowledgement>>,
    ) {
        (self.messages, self.receipts, self.acknowledgement)
    }
}

/// 在指定模型循环边界从 Runtime 持久状态 claim 动态输入的端口。
pub trait AgentDynamicInputSource: Send + Sync {
    /// 为可信 Session、Turn 与 Agent 在指定内存边界 claim 最多数量的动态消息。
    fn claim(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        source_agent_id: &AgentId,
        boundary: AgentDynamicInputBoundary,
        maximum: usize,
    ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError>;
}

/// 默认不注入动态消息的输入端口。
struct NoopAgentDynamicInputSource;

impl AgentDynamicInputSource for NoopAgentDynamicInputSource {
    /// 始终返回空批次。
    fn claim(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _source_agent_id: &AgentId,
        _boundary: AgentDynamicInputBoundary,
        _maximum: usize,
    ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError> {
        Ok(AgentDynamicInputBatch::empty())
    }
}

/// 一个 Turn 唯一终态及其可恢复 Transcript。
#[derive(Clone, Debug)]
pub struct TurnResult {
    /// 已进入唯一终态的状态机快照。
    pub state: TurnState,
    /// 后续模型继续使用的有效 Transcript；被替换历史可由 `compactions` 审计和恢复。
    pub messages: Vec<Message>,
    /// 正常完成时最后一个模型响应。
    pub final_response: Option<ModelResponse>,
    /// 启用结构化输出并成功时已经通过 Schema 校验的 JSON 值。
    pub structured_output: Option<Value>,
    /// 当前 Turn 新产生且可由 Session 层逐条持久化的上下文压缩记录。
    pub compactions: Vec<ContextCompressionRecord>,
    /// 失败、取消或达到上限时的运行原因。
    pub error: Option<AgentRunError>,
    /// 测试构建中暴露最终实际占用的 Hook 上下文字节数，用于验证原子预算回滚。
    #[cfg(test)]
    pub(crate) hook_context_bytes: usize,
}

impl TurnResult {
    /// 返回 Turn 是否以正常完成原因结束且没有运行错误。
    pub fn is_success(&self) -> bool {
        self.state.terminal_reason() == Some(TerminalReason::Completed) && self.error.is_none()
    }
}

/// Agent Loop 的模型、状态或硬上限错误。
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunError {
    /// 上层取消了当前 Turn。
    Cancelled,
    /// Provider 请求、流事件或响应归约失败。
    Model(ModelError),
    /// 上下文预算、压缩或唯一强制恢复失败。
    Context(ContextError),
    /// Hook 回调、输出或硬预算失败。
    Hook(HookError),
    /// 实时事件 Sink 返回失败或超过单事件接收时限。
    EventSink(AgentEventDeliveryError),
    /// 同步权威提交 Sink 明确失败。
    CommitSink(AgentCommitSinkError),
    /// 工具 Round 在任何生命周期或副作用前未通过持久化预检。
    ToolRoundPreflight(AgentToolRoundPreflightError),
    /// Turn 状态机不变量被违反。
    State(TurnTransitionError),
    /// Runtime 动态 mailbox 或用户 Steer 在提交前无法可靠进入 Transcript。
    DynamicInput {
        /// 不包含消息正文的安全说明。
        message: String,
    },
    /// 动态输入正文已进入 Transcript，但外部 claim 的确认未完成。
    ///
    /// 该错误要求上层把当前 Turn 收敛为失败并保留 claim，等待冷恢复依据
    /// Runtime Journal 的权威回执重新完成确认；不能按普通动态输入错误重试当前 Turn。
    DynamicInputAcknowledgement {
        /// 不包含消息正文的安全说明。
        message: String,
    },
    /// 同一个模型响应重复使用了工具调用 ID。
    DuplicateToolCallId {
        /// 重复的模型工具调用标识。
        id: String,
    },
    /// 模型响应达到输出 Token 上限；已确认的文本或推理仍可保留，但不能继续当前 Turn。
    ModelOutputLimit,
    /// 模型因内容安全策略拒答或截断；已确认的文本或推理仍可保留，但不能继续当前 Turn。
    ModelRefusal,
    /// 模型响应不能形成有效的 Agent Round。
    InvalidResponse {
        /// 指出缺失内容或结束原因的不变量说明。
        message: String,
    },
    /// Round 或 Step 达到配置硬上限。
    LimitReached {
        /// 达到上限的计数器。
        counter: CounterKind,
        /// 当前配置的最大值。
        maximum: u32,
    },
    /// 工具调用或真实失败达到确定性循环熔断阈值。
    ToolLoop {
        /// 相同调用还是重复真实失败。
        kind: ToolLoopKind,
        /// 触发熔断的精确工具名称。
        tool_name: String,
        /// 当前配置允许的最大连续次数。
        maximum: u32,
    },
    /// 状态变更工具已执行但输出超限，必须停止 Turn 以禁止相同调用自动重试。
    ToolOutputLimit {
        /// 可由控制面枚举且不会随说明文字变化的稳定机器错误码。
        code: ToolOutputErrorCode,
        /// ToolCompleted 未能确认提交时保留的有界分类与安全说明。
        completion_commit_error: Option<AgentCommitSinkError>,
    },
    /// 不应由用户输入触发的内部一致性错误。
    Internal {
        /// 不包含凭据或 Transcript 的安全说明。
        message: String,
    },
}

impl fmt::Display for AgentRunError {
    /// 输出适合 Session 终态事件的安全说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Agent Turn 已取消"),
            Self::Model(error) => write!(formatter, "模型调用失败：{error}"),
            Self::Context(error) => write!(formatter, "上下文处理失败：{error}"),
            Self::Hook(error) => write!(formatter, "Hook 处理失败：{error}"),
            Self::EventSink(error) => write!(formatter, "实时事件投递失败：{error}"),
            Self::CommitSink(error) => write!(formatter, "权威事件提交失败：{error}"),
            Self::ToolRoundPreflight(error) => {
                write!(formatter, "工具 Round 持久化预检失败：{error}")
            }
            Self::State(error) => write!(formatter, "Turn 状态错误：{error}"),
            Self::DynamicInput { message } => write!(formatter, "动态输入处理失败：{message}"),
            Self::DynamicInputAcknowledgement { message } => {
                write!(formatter, "动态输入确认失败：{message}")
            }
            Self::DuplicateToolCallId { id } => write!(formatter, "工具调用 ID 重复：{id}"),
            Self::ModelOutputLimit => formatter.write_str("模型输出达到 Token 上限"),
            Self::ModelRefusal => formatter.write_str("模型因内容安全策略拒答"),
            Self::InvalidResponse { message } => write!(formatter, "模型响应无效：{message}"),
            Self::LimitReached { counter, maximum } => {
                write!(formatter, "{counter:?} 达到运行上限 {maximum}")
            }
            Self::ToolLoop {
                kind,
                tool_name,
                maximum,
            } => write!(
                formatter,
                "工具 {tool_name} 触发 {kind:?} 循环熔断，上限为 {maximum}"
            ),
            Self::ToolOutputLimit {
                code,
                completion_commit_error,
            } => {
                write!(
                    formatter,
                    "工具输出超过安全上限（{code}），副作用可能已发生，禁止自动重试"
                )?;
                if let Some(error) = completion_commit_error {
                    write!(
                        formatter,
                        "；工具终态提交失败（{:?}）：{error}",
                        error.kind()
                    )?;
                }
                Ok(())
            }
            Self::Internal { message } => write!(formatter, "Agent 内部错误：{message}"),
        }
    }
}

impl Error for AgentRunError {}

impl From<ModelError> for AgentRunError {
    /// 把 Provider 中立错误包装为 Agent 运行错误。
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

impl From<ContextError> for AgentRunError {
    /// 把稳定上下文错误包装为 Agent 运行错误。
    fn from(error: ContextError) -> Self {
        if context_error_is_cancelled(&error) {
            Self::Cancelled
        } else {
            Self::Context(error)
        }
    }
}

impl From<HookError> for AgentRunError {
    /// 把 Hook 取消映射为 Turn 取消，其余保持稳定 Hook 分类。
    fn from(error: HookError) -> Self {
        match error {
            HookError::Cancelled { .. } => Self::Cancelled,
            error => Self::Hook(error),
        }
    }
}

impl From<TurnTransitionError> for AgentRunError {
    /// 把状态迁移错误包装为 Agent 运行错误。
    fn from(error: TurnTransitionError) -> Self {
        Self::State(error)
    }
}

/// 组合模型、工具、计划守卫和硬上限的单 Turn Agent Runtime。
pub struct AgentRunner {
    /// 接收 Provider 中立请求并产生严格模型事件流的模型实现。
    provider: Arc<dyn ModelProvider>,
    /// 本次 Runtime 可向模型提供的冻结工具集合。
    tools: ToolRegistry,
    /// 防止失控模型循环无限消耗资源的硬上限。
    limits: RunLimits,
    /// 执行请求前预算和 Provider 超限唯一恢复的上下文管理器。
    context: ContextManager,
    /// 按注册顺序运行并带硬预算的 Hook 运行时。
    hooks: HookRuntime,
    /// 每次模型采样前 claim mailbox 与用户 Steer 的持久输入端口。
    dynamic_input: Arc<dyn AgentDynamicInputSource>,
    /// 按 Provider 到达顺序接收可信实时事件且默认不产生副作用的出口。
    event_sink: Arc<dyn AgentEventSink>,
    /// 在返回前同步确认工具、压缩与 Transcript 权威事实的提交出口。
    commit_sink: Arc<dyn AgentCommitSink>,
}

impl AgentRunner {
    /// 创建不依赖厂商协议或桌面框架的 Agent Runner。
    pub fn new(provider: Arc<dyn ModelProvider>, tools: ToolRegistry, limits: RunLimits) -> Self {
        let context = ContextManager::for_provider(provider.clone());
        Self {
            provider,
            tools,
            limits,
            context,
            hooks: HookRuntime::empty(),
            dynamic_input: Arc::new(NoopAgentDynamicInputSource),
            event_sink: Arc::new(NoopAgentEventSink),
            commit_sink: Arc::new(NoopAgentCommitSink),
        }
    }

    /// 覆盖默认上下文策略、估算器或摘要器，主要用于 Runtime 组合根与确定性测试。
    pub fn with_context_manager(mut self, context: ContextManager) -> Self {
        self.context = context;
        self
    }

    /// 返回当前 Runner 冻结后的上下文管理器。
    pub const fn context_manager(&self) -> &ContextManager {
        &self.context
    }

    /// 覆盖默认空 Hook 运行时，并在后续 Turn 中冻结其顺序和硬预算。
    pub fn with_hook_runtime(mut self, hooks: HookRuntime) -> Self {
        self.hooks = hooks;
        self
    }

    /// 返回当前 Runner 冻结后的 Hook 运行时。
    pub const fn hook_runtime(&self) -> &HookRuntime {
        &self.hooks
    }

    /// 注入每次模型采样前使用的持久动态输入端口。
    pub fn with_dynamic_input_source(
        mut self,
        dynamic_input: Arc<dyn AgentDynamicInputSource>,
    ) -> Self {
        self.dynamic_input = dynamic_input;
        self
    }

    /// 注入实时事件 Sink；同一 Turn 的下一事件只会在前一事件确认后投递。
    pub fn with_event_sink(mut self, event_sink: Arc<dyn AgentEventSink>) -> Self {
        self.event_sink = event_sink;
        self
    }

    /// 返回当前 Runner 冻结后的实时事件 Sink。
    pub fn event_sink(&self) -> &Arc<dyn AgentEventSink> {
        &self.event_sink
    }

    /// 注入同步权威提交 Sink；提交调用不会受 Turn Future 取消或实时事件超时影响。
    pub fn with_commit_sink(mut self, commit_sink: Arc<dyn AgentCommitSink>) -> Self {
        self.commit_sink = commit_sink;
        self
    }

    /// 返回当前 Runner 使用的同步权威提交 Sink。
    pub fn commit_sink(&self) -> &Arc<dyn AgentCommitSink> {
        &self.commit_sink
    }

    /// 同步、幂等提交一次具有稳定用途的模型调用用量与实际耗时。
    fn commit_model_call_usage(
        &self,
        request: &TurnRequest,
        model_round: u32,
        call_attempt: u32,
        purpose: ModelCallPurpose,
        completion: ModelRoundCompletion,
        elapsed_millis: u64,
    ) -> Result<(), AgentRunError> {
        let usage = ModelRoundUsage::new(
            request.session_id.clone(),
            request.turn_id.clone(),
            request.source_agent_id.clone(),
            request.model_request.model.clone(),
            model_round,
            call_attempt,
            completion,
            elapsed_millis,
        )
        .with_purpose(purpose);
        let mut last_error = None;
        for _ in 0..AUTHORITATIVE_EVENT_MAX_COMMIT_ATTEMPTS {
            match self.commit_sink.commit_model_round_usage(&usage) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        Err(commit_sink_run_error(last_error.unwrap_or_else(|| {
            AgentCommitSinkError::rejected("模型 Round 用量提交没有返回结果")
        })))
    }

    /// 在任何响应工具执行前提交正常 Agent Round 的明确用量与实际耗时。
    fn commit_model_round_usage(
        &self,
        request: &TurnRequest,
        model_round: u32,
        call_attempt: u32,
        response: &ModelResponse,
        elapsed: Duration,
    ) -> Result<(), AgentRunError> {
        self.commit_model_call_usage(
            request,
            model_round,
            call_attempt,
            ModelCallPurpose::AgentRound,
            ModelRoundCompletion::from_response(response),
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// 在模型流失败且已经收到明确 Usage 时同步记账，但不提交不完整 Transcript。
    fn commit_failed_model_usage(
        &self,
        request: &TurnRequest,
        model_round: u32,
        call_attempt: u32,
        error: &ModelError,
        status: &Arc<Mutex<ModelStreamTapStatus>>,
        elapsed: Duration,
    ) -> Result<(), AgentRunError> {
        let Some(telemetry) = tap_model_usage(status) else {
            return Ok(());
        };
        self.commit_model_call_usage(
            request,
            model_round,
            call_attempt,
            ModelCallPurpose::AgentRound,
            ModelRoundCompletion {
                metadata: telemetry.metadata,
                usage: telemetry.usage,
                stop_reason: telemetry
                    .stop_reason
                    .unwrap_or_else(|| model_error_stop_reason(error)),
            },
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// 同步提交一个权威事件；调用开始后不再受 Turn 取消或实时投递超时影响。
    fn commit_event(
        &self,
        request: &TurnRequest,
        model_round: u32,
        kind: AgentCommitEventKind,
    ) -> Result<(), AgentRunError> {
        let identity = ModelEventIdentity::for_turn(request, model_round);
        let event = identity.commit_envelope(kind);
        commit_event_with_bounded_retry(
            self.commit_sink.as_ref(),
            &event,
            AUTHORITATIVE_EVENT_MAX_COMMIT_ATTEMPTS,
        )
        .map_err(commit_sink_run_error)
    }

    /// 投递一个带当前 Turn 身份的上下文压缩临时事件。
    async fn deliver_context_compaction_event(
        &self,
        request: &TurnRequest,
        model_round: u32,
        kind: AgentStreamEventKind,
        cancellable: bool,
    ) -> Result<(), AgentRunError> {
        let identity = ModelEventIdentity::for_turn(request, model_round);
        let event = identity.envelope(kind);
        let timeout = Duration::from_millis(self.limits.event_sink_timeout_ms);
        if cancellable {
            return match deliver_event_cancellable(
                &self.event_sink,
                &event,
                timeout,
                &request.cancellation,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(CancellableDeliveryError::Cancelled) => Err(AgentRunError::Cancelled),
                Err(CancellableDeliveryError::Delivery(error)) => {
                    Err(AgentRunError::EventSink(error))
                }
            };
        }
        deliver_event_bounded(&self.event_sink, &event, timeout)
            .await
            .map_err(AgentRunError::EventSink)
    }

    /// 发送 Started/Failed 边界并在权威提交成功前保持原 Transcript 不变。
    async fn compact_context(
        &self,
        request: &TurnRequest,
        model_request: &ModelRequest,
        capabilities: &ProviderCapabilities,
        model_round: u32,
        trigger: ContextCompressionTrigger,
        target_tokens: u64,
    ) -> Result<ContextCompressionOutcome, AgentRunError> {
        self.deliver_context_compaction_event(
            request,
            model_round,
            AgentStreamEventKind::ContextCompactionStarted {
                estimated_tokens: self.context.estimate_request(model_request),
            },
            true,
        )
        .await?;

        let outcome = match self
            .context
            .compact_with_capabilities(
                model_request,
                trigger,
                target_tokens,
                capabilities,
                &request.cancellation,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(usage) = context_error_model_usage(&error)
                    && let Err(commit_error) = self.commit_model_call_usage(
                        request,
                        model_round,
                        AGGREGATED_CONTEXT_CALL_ATTEMPT,
                        ModelCallPurpose::for_context_compaction(trigger),
                        ModelRoundCompletion {
                            metadata: usage.metadata.clone(),
                            usage: usage.usage.clone(),
                            stop_reason: usage.stop_reason.clone(),
                        },
                        usage.elapsed_millis,
                    )
                {
                    self.deliver_context_compaction_event(
                        request,
                        model_round,
                        AgentStreamEventKind::ContextCompactionFailed {
                            failure_kind: ContextCompactionFailureKind::Storage,
                        },
                        false,
                    )
                    .await?;
                    return Err(commit_error);
                }
                let error = context_error_without_summary_usage(error);
                if context_error_is_cancelled(&error) {
                    return Err(AgentRunError::Cancelled);
                }
                self.deliver_context_compaction_event(
                    request,
                    model_round,
                    AgentStreamEventKind::ContextCompactionFailed {
                        failure_kind: context_compaction_failure_kind(&error),
                    },
                    false,
                )
                .await?;
                return Err(AgentRunError::Context(error));
            }
        };

        if let Some(usage) = &outcome.summary_model_usage
            && let Err(error) = self.commit_model_call_usage(
                request,
                model_round,
                AGGREGATED_CONTEXT_CALL_ATTEMPT,
                ModelCallPurpose::for_context_compaction(trigger),
                ModelRoundCompletion {
                    metadata: usage.metadata.clone(),
                    usage: usage.usage.clone(),
                    stop_reason: usage.stop_reason.clone(),
                },
                usage.elapsed_millis,
            )
        {
            self.deliver_context_compaction_event(
                request,
                model_round,
                AgentStreamEventKind::ContextCompactionFailed {
                    failure_kind: ContextCompactionFailureKind::Storage,
                },
                false,
            )
            .await?;
            return Err(error);
        }

        if let Err(error) = self.commit_event(
            request,
            model_round,
            AgentCommitEventKind::ContextCompactionApplied {
                record: outcome.record.clone(),
            },
        ) {
            self.deliver_context_compaction_event(
                request,
                model_round,
                AgentStreamEventKind::ContextCompactionFailed {
                    failure_kind: ContextCompactionFailureKind::Storage,
                },
                false,
            )
            .await?;
            return Err(error);
        }
        Ok(outcome)
    }

    /// 提交工具生命周期事件，并在最终结果不确定时立即冻结当前 Round 预留供恢复。
    fn commit_tool_lifecycle_event(
        &self,
        request: &TurnRequest,
        model_round: u32,
        round_permit: &mut AgentToolRoundPermit,
        kind: AgentCommitEventKind,
    ) -> Result<(), AgentRunError> {
        if round_permit.recovery_retained() {
            return Err(AgentRunError::Internal {
                message: "工具 Round 已进入恢复保留，禁止继续提交生命周期事件".to_owned(),
            });
        }
        let identity = ModelEventIdentity::for_turn(request, model_round);
        let event = identity.commit_envelope(kind);
        match commit_event_with_bounded_retry(
            self.commit_sink.as_ref(),
            &event,
            AUTHORITATIVE_EVENT_MAX_COMMIT_ATTEMPTS,
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == AgentCommitSinkErrorKind::Indeterminate => {
                round_permit.retain_after_indeterminate(event)?;
                Err(commit_sink_run_error(error))
            }
            Err(error) => Err(commit_sink_run_error(error)),
        }
    }

    /// 在任何工具生命周期或真实执行前同步取得绑定 Round 的一次性 Permit。
    fn preflight_tool_round(
        &self,
        request: &TurnRequest,
        model_round: u32,
        segment_index: u32,
        completion: ModelRoundCompletion,
        assistant_message: Message,
        pre_tool_context: Vec<Message>,
    ) -> Result<AgentToolRoundPermit, AgentRunError> {
        ensure_not_cancelled(&request.cancellation)?;
        let binding = AgentToolRoundBinding::new(
            request.session_id.clone(),
            request.turn_id.clone(),
            request.source_agent_id.clone(),
            request.model_request.model.clone(),
            model_round,
            segment_index,
        );
        let round =
            AgentToolRoundPreflight::new(binding, completion, assistant_message, pre_tool_context);
        let reservation = self
            .commit_sink
            .preflight_tool_round(&round)
            .map_err(tool_round_preflight_run_error)?;
        let permit = AgentToolRoundPermit::new(round, self.commit_sink.clone(), reservation);
        ensure_not_cancelled(&request.cancellation)?;
        Ok(permit)
    }

    /// 先让 Session 层可靠接收一段 Round 消息，再更新内存 Transcript。
    fn commit_round_messages(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        completion: Option<ModelRoundCompletion>,
        messages: Vec<Message>,
    ) -> Result<(), AgentRunError> {
        if messages.is_empty() && completion.is_none() {
            return Err(AgentRunError::Internal {
                message: "Round 提交不能包含空消息段".to_owned(),
            });
        }
        let segment_index = active.next_segment_index;
        let next_segment_index =
            segment_index
                .checked_add(1)
                .ok_or_else(|| AgentRunError::Internal {
                    message: "Round Transcript 段序号溢出".to_owned(),
                })?;
        let kind = match completion {
            Some(completion) => AgentCommitEventKind::ModelRoundCommitted {
                segment_index,
                completion,
                messages: messages.clone(),
            },
            None => AgentCommitEventKind::RoundCommitted {
                segment_index,
                messages: messages.clone(),
            },
        };
        self.commit_event(request, active.state.round_count(), kind)?;
        active.next_segment_index = next_segment_index;
        active.messages.extend(messages);
        Ok(())
    }

    /// 把当前安全边界 claim 的动态消息先提交 Transcript，再幂等确认外部 claim。
    fn commit_dynamic_input(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        boundary: AgentDynamicInputBoundary,
    ) -> Result<bool, AgentRunError> {
        let batch = self
            .dynamic_input
            .claim(
                &request.session_id,
                &request.turn_id,
                &request.source_agent_id,
                boundary,
                MAX_DYNAMIC_INPUT_MESSAGES_PER_BOUNDARY,
            )
            .map_err(dynamic_input_run_error)?;
        let (messages, receipts, acknowledgement) = batch.into_parts();
        if messages.is_empty() {
            if acknowledgement.is_some() {
                return Err(AgentRunError::DynamicInput {
                    message: "空动态输入批次不得携带确认回执".to_owned(),
                });
            }
            return Ok(false);
        }
        if messages.len() > MAX_DYNAMIC_INPUT_MESSAGES_PER_BOUNDARY {
            return Err(AgentRunError::DynamicInput {
                message: "动态输入批次超过单次安全边界上限".to_owned(),
            });
        }
        let acknowledgement = acknowledgement.ok_or_else(|| AgentRunError::DynamicInput {
            message: "非空动态输入批次缺少确认回执".to_owned(),
        })?;
        if receipts
            .iter()
            .any(|receipt| receipt.through_sequence() == 0)
        {
            return Err(AgentRunError::DynamicInput {
                message: "动态输入确认水位必须大于零".to_owned(),
            });
        }
        let mut receipt_kinds = HashSet::new();
        if receipts
            .iter()
            .any(|receipt| !receipt_kinds.insert(receipt.kind()))
        {
            return Err(AgentRunError::DynamicInput {
                message: "动态输入确认水位的来源类别不能重复".to_owned(),
            });
        }
        for message in &messages {
            if !matches!(message.role, MessageRole::User | MessageRole::Developer) {
                return Err(AgentRunError::DynamicInput {
                    message: "动态输入只能使用用户或开发者消息角色".to_owned(),
                });
            }
            message
                .validate()
                .map_err(|_| AgentRunError::DynamicInput {
                    message: "动态输入消息不满足 Provider 中立约束".to_owned(),
                })?;
        }
        if receipts.is_empty() {
            // 未提供持久水位的纯内存测试/嵌入调用仍可使用普通消息段；生产 Runtime
            // 必须通过 `new_with_receipts` 进入带权威回执的分支。
            self.commit_round_messages(request, active, None, messages)?;
        } else {
            let segment_index = active.next_segment_index;
            let next_segment_index =
                segment_index
                    .checked_add(1)
                    .ok_or_else(|| AgentRunError::Internal {
                        message: "动态输入 Transcript 段序号溢出".to_owned(),
                    })?;
            self.commit_event(
                request,
                active.state.round_count(),
                AgentCommitEventKind::DynamicInputCommitted {
                    segment_index,
                    receipts,
                    messages: messages.clone(),
                },
            )?;
            active.next_segment_index = next_segment_index;
            active.messages.extend(messages);
        }
        let mut last_error = None;
        for _ in 0..DYNAMIC_INPUT_ACKNOWLEDGEMENT_ATTEMPTS {
            match acknowledgement.acknowledge() {
                Ok(()) => return Ok(true),
                Err(error) => last_error = Some(error),
            }
        }
        Err(dynamic_input_acknowledgement_run_error(
            last_error.unwrap_or_else(|| AgentDynamicInputError::new("动态输入确认没有返回结果")),
        ))
    }

    /// 通过预检返回的一次性 Permit 同步提交匹配工具 Round。
    fn commit_preflighted_tool_round(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        permit: AgentToolRoundPermit,
        messages: Vec<Message>,
    ) -> Result<(), AgentRunError> {
        if messages.is_empty() {
            return Err(AgentRunError::Internal {
                message: "工具 Round 提交不能包含空消息段".to_owned(),
            });
        }
        let segment_index = active.next_segment_index;
        let next_segment_index =
            segment_index
                .checked_add(1)
                .ok_or_else(|| AgentRunError::Internal {
                    message: "工具 Round Transcript 段序号溢出".to_owned(),
                })?;
        let identity = ModelEventIdentity::for_turn(request, active.state.round_count());
        let event = identity.commit_envelope(AgentCommitEventKind::ModelRoundCommitted {
            segment_index,
            completion: permit.completion().clone(),
            messages: messages.clone(),
        });
        permit.commit(event)?;
        active.next_segment_index = next_segment_index;
        active.messages.extend(messages);
        Ok(())
    }

    /// 执行一个 Turn，并在所有路径上返回恰好一个终态。
    pub async fn run_turn(&self, request: TurnRequest) -> TurnResult {
        let mut active = ActiveTurn {
            state: TurnState::new(request.turn_id.clone(), request.source_agent_id.clone()),
            messages: request.model_request.messages.clone(),
            seen_tool_call_ids: HashSet::new(),
            compactions: Vec::new(),
            forced_context_retry_used: false,
            next_model_call_attempt: 1,
            hook_context_bytes: 0,
            stop_hook_rounds: 0,
            next_segment_index: 0,
            last_tool_call: None,
            identical_tool_call_count: 0,
            last_tool_failure: None,
            repeated_tool_failure_count: 0,
            step_limit_summary: None,
        };
        let outcome = self.run_active(&request, &mut active).await;

        let (final_response, structured_output, mut error) = match outcome {
            Ok(completion) => {
                if let Err(transition_error) = active.state.finish(TerminalReason::Completed) {
                    (None, None, Some(AgentRunError::State(transition_error)))
                } else {
                    (
                        Some(completion.response),
                        completion.structured_output,
                        None,
                    )
                }
            }
            Err(run_error) => {
                let terminal_reason = match &run_error {
                    AgentRunError::Cancelled => TerminalReason::Cancelled,
                    AgentRunError::LimitReached { .. } | AgentRunError::ToolLoop { .. } => {
                        TerminalReason::LimitReached
                    }
                    AgentRunError::Context(_) => TerminalReason::ContextBlocked,
                    AgentRunError::ModelOutputLimit => TerminalReason::ModelOutputLimit,
                    AgentRunError::ModelRefusal => TerminalReason::ModelRefusal,
                    AgentRunError::ToolRoundPreflight(error)
                        if error.kind() == AgentToolRoundPreflightErrorKind::Unpersistable =>
                    {
                        TerminalReason::ContextBlocked
                    }
                    AgentRunError::Model(_)
                    | AgentRunError::Hook(_)
                    | AgentRunError::EventSink(_)
                    | AgentRunError::CommitSink(_)
                    | AgentRunError::ToolRoundPreflight(_)
                    | AgentRunError::State(_)
                    | AgentRunError::DynamicInput { .. }
                    | AgentRunError::DynamicInputAcknowledgement { .. }
                    | AgentRunError::DuplicateToolCallId { .. }
                    | AgentRunError::InvalidResponse { .. }
                    | AgentRunError::ToolOutputLimit { .. }
                    | AgentRunError::Internal { .. } => TerminalReason::Failed,
                };
                let finish_result = if terminal_reason == TerminalReason::Cancelled {
                    active
                        .state
                        .transition_to(TurnPhase::Cancelling)
                        .and_then(|_| active.state.finish(terminal_reason))
                } else {
                    active.state.finish(terminal_reason)
                };
                let final_error = finish_result
                    .err()
                    .map(AgentRunError::State)
                    .unwrap_or(run_error);
                (None, None, Some(final_error))
            }
        };

        if !active.state.is_terminal() && error.is_none() {
            error = Some(AgentRunError::Internal {
                message: "Agent Loop 返回时 Turn 尚未进入终态".to_owned(),
            });
        }
        TurnResult {
            state: active.state,
            messages: active.messages,
            final_response,
            structured_output,
            compactions: active.compactions,
            error,
            #[cfg(test)]
            hook_context_bytes: active.hook_context_bytes,
        }
    }

    /// 运行模型与工具 Round，正常完成时停在 `CommittingRound`。
    async fn run_active(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
    ) -> Result<CompletedTurn, AgentRunError> {
        if !request.model_request.tools.is_empty() {
            return Err(AgentRunError::InvalidResponse {
                message: "Turn 请求模板不能预置工具，工具必须来自 Runtime 注册表".to_owned(),
            });
        }
        request.model_request.validate()?;
        let provider_capabilities = self.provider.capabilities(&request.model_request.model);
        let structured_mode =
            self.structured_output_mode(&request.model_request, &provider_capabilities)?;
        active.state.transition_to(TurnPhase::PreparingContext)?;

        loop {
            ensure_not_cancelled(&request.cancellation)?;
            let summary_only = active.step_limit_summary.is_some();
            if !summary_only && active.state.round_count() >= self.limits.max_rounds {
                return Err(active.step_limit_summary.take().unwrap_or(
                    AgentRunError::LimitReached {
                        counter: CounterKind::Round,
                        maximum: self.limits.max_rounds,
                    },
                ));
            }

            active.state.begin_round()?;
            active.next_segment_index = 0;
            self.commit_dynamic_input(
                request,
                active,
                AgentDynamicInputBoundary::BeforeModelSampling,
            )?;

            let mut model_request = request.model_request.clone();
            model_request.messages = active.messages.clone();
            model_request.tools = if summary_only {
                Vec::new()
            } else {
                self.tools.definitions()
            };
            if summary_only {
                model_request.structured_output = None;
            } else if let Some(result_tool) = structured_mode.result_tool() {
                model_request.structured_output = None;
                model_request.tools.push(result_tool);
            }
            model_request.tool_choice = if summary_only {
                ToolChoice::None
            } else {
                ToolChoice::Auto
            };
            model_request.parallel_tool_calls = if summary_only {
                None
            } else {
                (!model_request.tools.is_empty()).then_some(
                    matches!(
                        &structured_mode,
                        StructuredOutputMode::None | StructuredOutputMode::Native(_)
                    ) && provider_capabilities.parallel_tool_calls,
                )
            };
            if let Some(target_tokens) = self
                .context
                .precompression_target(&model_request, &provider_capabilities)
            {
                active.state.transition_to(TurnPhase::Compacting)?;
                let outcome = self
                    .compact_context(
                        request,
                        &model_request,
                        &provider_capabilities,
                        active.state.round_count(),
                        ContextCompressionTrigger::Budget,
                        target_tokens,
                    )
                    .await
                    .map_err(|error| {
                        prefer_step_limit_summary_error(active.step_limit_summary.as_ref(), error)
                    })?;
                active.messages = outcome.messages;
                model_request.messages = active.messages.clone();
                active.compactions.push(outcome.record);
                active.state.transition_to(TurnPhase::RequestingModel)?;
            }
            let model_call_attempt = active.next_model_call_attempt()?;
            let completed_round = match self
                .request_model(
                    request,
                    model_request.clone(),
                    model_call_attempt,
                    &mut active.state,
                )
                .await
            {
                Err(AgentRunError::Model(ModelError::ContextLengthExceeded { .. }))
                    if !active.forced_context_retry_used =>
                {
                    active.forced_context_retry_used = true;
                    active.state.transition_to(TurnPhase::Compacting)?;
                    let target_tokens = self
                        .context
                        .forced_target(&model_request, &provider_capabilities);
                    let outcome = self
                        .compact_context(
                            request,
                            &model_request,
                            &provider_capabilities,
                            active.state.round_count(),
                            ContextCompressionTrigger::ProviderOverflow,
                            target_tokens,
                        )
                        .await
                        .map_err(|error| {
                            prefer_step_limit_summary_error(
                                active.step_limit_summary.as_ref(),
                                error,
                            )
                        })?;
                    active.messages = outcome.messages;
                    model_request.messages = active.messages.clone();
                    active.compactions.push(outcome.record);
                    active.state.transition_to(TurnPhase::RequestingModel)?;
                    let retry_call_attempt = active.next_model_call_attempt()?;
                    match self
                        .request_model(
                            request,
                            model_request.clone(),
                            retry_call_attempt,
                            &mut active.state,
                        )
                        .await
                    {
                        Err(AgentRunError::Model(ModelError::ContextLengthExceeded { .. })) => {
                            active.state.transition_to(TurnPhase::Compacting)?;
                            Err(AgentRunError::Context(ContextError::StillExceeded {
                                estimated_tokens: self.context.estimate_request(&model_request),
                            }))
                        }
                        result => result,
                    }
                }
                Err(AgentRunError::Model(ModelError::ContextLengthExceeded { .. })) => {
                    active.state.transition_to(TurnPhase::Compacting)?;
                    Err(AgentRunError::Context(ContextError::StillExceeded {
                        estimated_tokens: self.context.estimate_request(&model_request),
                    }))
                }
                result => result,
            }
            .map_err(|error| {
                prefer_step_limit_summary_error(active.step_limit_summary.as_ref(), error)
            })?;
            self.commit_model_round_usage(
                request,
                active.state.round_count(),
                completed_round.call_attempt,
                &completed_round.response,
                completed_round.elapsed,
            )?;
            let mut response = completed_round.response;
            if let Some(error) = model_terminal_error(&response.stop_reason) {
                let committed = partial_model_response_messages(&response);
                active.state.transition_to(TurnPhase::CommittingRound)?;
                self.commit_round_messages(
                    request,
                    active,
                    Some(ModelRoundCompletion::from_response(&response)),
                    committed,
                )?;
                return Err(error);
            }
            let tool_calls = extract_tool_calls(&response, &mut active.seen_tool_call_ids)
                .map_err(|error| {
                    prefer_step_limit_summary_error(active.step_limit_summary.as_ref(), error)
                })?;
            if response.content.is_empty() {
                if let Some(error) = active.step_limit_summary.take() {
                    return Err(error);
                }
                return Err(match structured_mode.enforcement() {
                    Some(enforcement) => structured_run_error(
                        enforcement,
                        StructuredOutputFailureKind::MissingOutput,
                        "模型响应没有任何内容块",
                    ),
                    None => AgentRunError::InvalidResponse {
                        message: "模型响应没有任何内容块".to_owned(),
                    },
                });
            }

            if let Some(error) = active.step_limit_summary.take() {
                let mut committed = vec![Message::new(
                    MessageRole::Assistant,
                    response.content.clone(),
                )];
                if !tool_calls.is_empty() {
                    committed.push(Message::new(
                        MessageRole::Tool,
                        tool_calls
                            .into_iter()
                            .map(|call| ContentBlock::ToolResult {
                                tool_result: ToolResult::text(
                                    call.id,
                                    "Step 上限后的最终总结 Round 禁止调用工具",
                                    true,
                                ),
                            })
                            .collect(),
                    ));
                }
                active.state.transition_to(TurnPhase::CommittingRound)?;
                self.commit_round_messages(
                    request,
                    active,
                    Some(ModelRoundCompletion::from_response(&response)),
                    committed,
                )?;
                return Err(error);
            }

            if let StructuredOutputMode::ToolEmulated(_) = &structured_mode {
                if tool_calls
                    .iter()
                    .any(|call| call.name == STRUCTURED_OUTPUT_TOOL_NAME)
                {
                    let completion = complete_emulated_output(&structured_mode, response)?;
                    let committed = vec![Message::new(
                        MessageRole::Assistant,
                        completion.response.content.clone(),
                    )];
                    active.state.transition_to(TurnPhase::CommittingRound)?;
                    self.commit_round_messages(
                        request,
                        active,
                        Some(ModelRoundCompletion::from_response(&completion.response)),
                        committed,
                    )?;
                    if self
                        .should_complete_after_stop_hooks(request, active, &completion.response)
                        .await?
                    {
                        return Ok(completion);
                    }
                    continue;
                }
            }

            if tool_calls.is_empty() {
                if response.stop_reason == StopReason::ToolUse {
                    return Err(AgentRunError::InvalidResponse {
                        message: "模型以工具调用结束但没有返回工具调用内容块".to_owned(),
                    });
                }
                if matches!(&structured_mode, StructuredOutputMode::None)
                    && response.stop_reason != StopReason::Completed
                {
                    return Err(AgentRunError::InvalidResponse {
                        message: format!(
                            "普通文本响应必须以 completed 结束，实际为 {:?}",
                            response.stop_reason
                        ),
                    });
                }
                let structured_output = match &structured_mode {
                    StructuredOutputMode::None => None,
                    StructuredOutputMode::Native(config) => Some(
                        config.parse_response(&response, StructuredOutputEnforcement::Native)?,
                    ),
                    StructuredOutputMode::ToolEmulated(_) => {
                        return Err(structured_run_error(
                            StructuredOutputEnforcement::ToolEmulated,
                            StructuredOutputFailureKind::MissingOutput,
                            "模型没有调用保留结果工具",
                        ));
                    }
                };
                let committed = vec![Message::new(
                    MessageRole::Assistant,
                    response.content.clone(),
                )];
                active.state.transition_to(TurnPhase::CommittingRound)?;
                self.commit_round_messages(
                    request,
                    active,
                    Some(ModelRoundCompletion::from_response(&response)),
                    committed,
                )?;
                let completion = CompletedTurn {
                    response,
                    structured_output,
                };
                if self
                    .should_complete_after_stop_hooks(request, active, &completion.response)
                    .await?
                {
                    return Ok(completion);
                }
                continue;
            }

            if response.stop_reason != StopReason::ToolUse {
                return Err(AgentRunError::InvalidResponse {
                    message: format!(
                        "普通工具响应必须以 tool_use 结束，实际为 {:?}",
                        response.stop_reason
                    ),
                });
            }
            active.state.transition_to(TurnPhase::SchedulingTools)?;
            let batch = self
                .execute_tools(request, active, &mut response, tool_calls)
                .await?;
            let ToolBatchResult {
                results,
                post_context,
                terminal_error,
                summary_error,
                round_permit,
                hook_context_bytes,
                lifecycle_fully_committed,
            } = batch;
            if !lifecycle_fully_committed {
                return Err(terminal_error.unwrap_or_else(|| AgentRunError::Internal {
                    message: "工具终态未全部确认但缺少投递错误".to_owned(),
                }));
            }
            active.hook_context_bytes = hook_context_bytes;
            let mut committed = vec![round_permit.assistant_message().clone()];
            committed.push(Message::new(
                MessageRole::Tool,
                results
                    .into_iter()
                    .map(|tool_result| ContentBlock::ToolResult { tool_result })
                    .collect(),
            ));
            committed.extend(round_permit.pre_tool_context().iter().cloned());
            committed.extend(
                post_context
                    .into_iter()
                    .flatten()
                    .map(ResolvedHookContext::into_message),
            );
            if terminal_error.is_none() {
                if let Some(error) = summary_error {
                    active.step_limit_summary = Some(error);
                    committed.push(Message::text(
                        MessageRole::User,
                        STEP_LIMIT_SUMMARY_INSTRUCTION,
                    ));
                }
            }
            active.state.transition_to(TurnPhase::CommittingRound)?;
            self.commit_preflighted_tool_round(request, active, round_permit, committed)?;
            if let Some(error) = terminal_error {
                return Err(error);
            }
            active.state.transition_to(TurnPhase::PreparingContext)?;
        }
    }

    /// 仅对正常候选完成运行 Stop Hook，并在 Continue 时提交有界上下文。
    async fn run_stop_hooks(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        response: &ModelResponse,
    ) -> Result<bool, AgentRunError> {
        active.state.transition_to(TurnPhase::RunningStopHooks)?;
        if self.hooks.registry().is_empty() {
            return Ok(true);
        }
        if active.stop_hook_rounds >= self.hooks.limits().max_stop_hook_rounds {
            return Err(HookError::StopRoundsExceeded {
                maximum: self.hooks.limits().max_stop_hook_rounds,
            }
            .into());
        }
        active.stop_hook_rounds = active.stop_hook_rounds.saturating_add(1);
        let outcome = self
            .hooks
            .run_stop(
                StopHookContext {
                    invocation: hook_invocation_context(request),
                    response: response.clone(),
                    model_round: active.state.round_count(),
                    stop_hook_round: active.stop_hook_rounds,
                },
                &request.cancellation,
            )
            .await
            .map_err(AgentRunError::from)?;
        match outcome {
            ResolvedStopHook::Stop => Ok(true),
            ResolvedStopHook::Continue(additions) => {
                if active.stop_hook_rounds >= self.hooks.limits().max_stop_hook_rounds {
                    return Err(HookError::StopRoundsExceeded {
                        maximum: self.hooks.limits().max_stop_hook_rounds,
                    }
                    .into());
                }
                self.append_hook_context(request, active, additions).await?;
                active.state.transition_to(TurnPhase::PreparingContext)?;
                Ok(false)
            }
        }
    }

    /// Stop Hook 同意结束后仅补读当前 Turn 的用户 Steer；未消费 mailbox 不阻止完成。
    async fn should_complete_after_stop_hooks(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        response: &ModelResponse,
    ) -> Result<bool, AgentRunError> {
        if !self.run_stop_hooks(request, active, response).await? {
            return Ok(false);
        }
        if !self.commit_dynamic_input(
            request,
            active,
            AgentDynamicInputBoundary::AfterFinalCandidate,
        )? {
            return Ok(true);
        }
        active.state.transition_to(TurnPhase::PreparingContext)?;
        Ok(false)
    }

    /// 原子占用 Hook 字节预算并把上下文按原顺序追加为统一用户消息。
    async fn append_hook_context(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        additions: Vec<ResolvedHookContext>,
    ) -> Result<(), AgentRunError> {
        let mut hook_context_bytes = active.hook_context_bytes;
        self.hooks
            .charge_context(&mut hook_context_bytes, &additions)
            .map_err(AgentRunError::from)?;
        let messages = additions
            .into_iter()
            .map(ResolvedHookContext::into_message)
            .collect();
        self.commit_round_messages(request, active, None, messages)?;
        active.hook_context_bytes = hook_context_bytes;
        Ok(())
    }

    /// 根据 Provider 能力为当前 Turn 冻结结构化输出执行方式。
    fn structured_output_mode(
        &self,
        request: &ModelRequest,
        capabilities: &keencode_model::ProviderCapabilities,
    ) -> Result<StructuredOutputMode, AgentRunError> {
        let mode = StructuredOutputMode::resolve(request.structured_output.as_ref(), capabilities)?;
        if matches!(mode, StructuredOutputMode::ToolEmulated(_))
            && self
                .tools
                .definitions()
                .iter()
                .any(|tool| tool.name == STRUCTURED_OUTPUT_TOOL_NAME)
        {
            return Err(AgentRunError::Internal {
                message: format!("工具注册表占用了保留名称 {STRUCTURED_OUTPUT_TOOL_NAME}"),
            });
        }
        Ok(mode)
    }

    /// 在请求和逐事件读取阶段响应取消，并在严格归约前逐项投递实时事件。
    async fn request_model(
        &self,
        turn_request: &TurnRequest,
        model_request: ModelRequest,
        call_attempt: u32,
        state: &mut TurnState,
    ) -> Result<CompletedModelRound, AgentRunError> {
        let started = Instant::now();
        let identity = ModelEventIdentity {
            session_id: turn_request.session_id.clone(),
            turn_id: turn_request.turn_id.clone(),
            source_agent_id: turn_request.source_agent_id.clone(),
            model: model_request.model.clone(),
            model_round: state.round_count(),
        };
        let event_timeout = Duration::from_millis(self.limits.event_sink_timeout_ms);
        let cancelled = Box::pin(turn_request.cancellation.cancelled());
        let requested = self.provider.stream(model_request);
        let model_stream = match select(cancelled, requested).await {
            Either::Left(((), _)) => {
                let error = cancelled_model_error();
                deliver_failure_boundary(&self.event_sink, &identity, &error, event_timeout)
                    .await
                    .map_err(AgentRunError::EventSink)?;
                return Err(AgentRunError::Cancelled);
            }
            Either::Right((Ok(model_stream), _)) => model_stream,
            Either::Right((Err(error), _)) => {
                deliver_failure_boundary(&self.event_sink, &identity, &error, event_timeout)
                    .await
                    .map_err(AgentRunError::EventSink)?;
                return Err(model_error_to_run_error(error));
            }
        };
        state.transition_to(TurnPhase::StreamingModel)?;

        let tap_status = Arc::new(Mutex::new(ModelStreamTapStatus::default()));
        let tapped: ModelStream = Box::pin(stream::unfold(
            TappedModelStream {
                model_stream,
                event_sink: self.event_sink.clone(),
                identity: identity.clone(),
                cancellation: turn_request.cancellation.clone(),
                event_timeout,
                status: tap_status.clone(),
                done: false,
            },
            tap_model_stream_event,
        ));
        let reduced = collect_model_stream(tapped).await;
        if let Some(error) = take_tap_delivery_error(&tap_status) {
            self.commit_failed_model_usage(
                turn_request,
                state.round_count(),
                call_attempt,
                &sink_aborted_model_error(),
                &tap_status,
                started.elapsed(),
            )?;
            return Err(AgentRunError::EventSink(error));
        }
        match reduced {
            Ok(response) => Ok(CompletedModelRound {
                response,
                elapsed: started.elapsed(),
                call_attempt,
            }),
            Err(error) => {
                self.commit_failed_model_usage(
                    turn_request,
                    state.round_count(),
                    call_attempt,
                    &error,
                    &tap_status,
                    started.elapsed(),
                )?;
                if !tap_failure_boundary_sent(&tap_status) {
                    deliver_failure_boundary(&self.event_sink, &identity, &error, event_timeout)
                        .await
                        .map_err(AgentRunError::EventSink)?;
                }
                Err(model_error_to_run_error(error))
            }
        }
    }

    /// 在任何真实执行前按模型顺序提交全部有效工具请求。
    fn emit_tool_requests(
        &self,
        request: &TurnRequest,
        model_round: u32,
        round_permit: &mut AgentToolRoundPermit,
        prepared: &mut [PreparedCall],
    ) -> Result<(), AgentRunError> {
        for call in prepared {
            let Some(lifecycle) = call.lifecycle() else {
                continue;
            };
            if lifecycle.phase != PreparedToolLifecyclePhase::Prepared {
                return Err(AgentRunError::Internal {
                    message: "工具请求生命周期被重复提交".to_owned(),
                });
            }
            ensure_not_cancelled(&request.cancellation)?;
            let request_index = u32::try_from(call.index).map_err(|_| AgentRunError::Internal {
                message: "模型工具调用位置超过 u32 上限".to_owned(),
            })?;
            let kind = AgentCommitEventKind::ToolRequested {
                request_index,
                tool_call_id: lifecycle.tool_call_id.clone(),
                call: lifecycle.call.clone(),
                effect: lifecycle.effect,
            };
            self.commit_tool_lifecycle_event(request, model_round, round_permit, kind)?;
            call.lifecycle_mut()
                .expect("生命周期元数据在事件投递期间不应消失")
                .phase = PreparedToolLifecyclePhase::Requested;
        }
        Ok(())
    }

    /// 在计入 Step 和调用真实工具实现前提交副作用执行起点。
    fn emit_tool_execution_started(
        &self,
        request: &TurnRequest,
        model_round: u32,
        round_permit: &mut AgentToolRoundPermit,
        call: &mut PreparedCall,
    ) -> Result<(), AgentRunError> {
        let lifecycle = call
            .requested_lifecycle()
            .ok_or_else(|| AgentRunError::Internal {
                message: "待执行工具缺少已提交的生命周期元数据".to_owned(),
            })?;
        if lifecycle.phase != PreparedToolLifecyclePhase::Requested {
            return Err(AgentRunError::Internal {
                message: "工具尚未完成执行前守卫、已经开始或已经结束".to_owned(),
            });
        }
        ensure_not_cancelled(&request.cancellation)?;
        let tool_call_id = lifecycle.tool_call_id.clone();
        self.commit_tool_lifecycle_event(
            request,
            model_round,
            round_permit,
            AgentCommitEventKind::ToolExecutionStarted { tool_call_id },
        )?;
        call.lifecycle_mut()
            .expect("执行起点投递期间生命周期元数据不应消失")
            .phase = PreparedToolLifecyclePhase::Started;
        Ok(())
    }

    /// 为已经提交请求的工具同步提交唯一结果。
    fn emit_tool_completed(
        &self,
        request: &TurnRequest,
        model_round: u32,
        round_permit: &mut AgentToolRoundPermit,
        call: &mut PreparedCall,
        status: ToolCompletionStatus,
        result: &ToolResult,
    ) -> Result<(), AgentRunError> {
        let Some(lifecycle) = call.requested_lifecycle() else {
            return Ok(());
        };
        if matches!(
            lifecycle.phase,
            PreparedToolLifecyclePhase::Completed
                | PreparedToolLifecyclePhase::CompletionUnconfirmed
        ) {
            return Err(AgentRunError::Internal {
                message: "工具唯一终态被重复投递".to_owned(),
            });
        }
        if matches!(
            status,
            ToolCompletionStatus::Succeeded | ToolCompletionStatus::Failed
        ) && lifecycle.phase != PreparedToolLifecyclePhase::Started
        {
            return Err(AgentRunError::Internal {
                message: "成功或失败工具结果缺少可靠执行起点".to_owned(),
            });
        }
        if lifecycle.tool_call_id.as_str() != result.tool_call_id {
            return Err(AgentRunError::Internal {
                message: "工具生命周期结果与可信调用标识不匹配".to_owned(),
            });
        }
        let tool_call_id = lifecycle.tool_call_id.clone();
        let outcome = self.commit_tool_lifecycle_event(
            request,
            model_round,
            round_permit,
            AgentCommitEventKind::ToolCompleted {
                tool_call_id,
                status,
                result: result.clone(),
            },
        );
        call.lifecycle_mut()
            .expect("工具终态投递期间生命周期元数据不应消失")
            .phase = if outcome.is_ok() {
            PreparedToolLifecyclePhase::Completed
        } else {
            PreparedToolLifecyclePhase::CompletionUnconfirmed
        };
        outcome
    }

    /// 先冻结并预检整个批次，再完成实际工具执行。
    async fn execute_tools(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        response: &mut ModelResponse,
        calls: Vec<ToolCall>,
    ) -> Result<ToolBatchResult, AgentRunError> {
        if !ToolRoundOutputBudget::can_reserve_results(calls.len()) {
            return Err(AgentRunError::InvalidResponse {
                message: "模型工具调用数量超过 Round 固定结果容量".to_owned(),
            });
        }
        for call in &calls {
            crate::ToolCallId::new(call.id.clone()).map_err(|error| {
                AgentRunError::InvalidResponse {
                    message: format!("工具调用 ID 无效：{error}"),
                }
            })?;
        }
        let mut final_tool_calls = calls.clone();
        let mut prepared = Vec::with_capacity(calls.len());
        let mut preparation_error = None;
        for (index, mut call) in calls.into_iter().enumerate() {
            if let Some(error) = &preparation_error {
                prepared.push(PreparedCall::immediate(
                    index,
                    ToolResult::text(
                        call.id,
                        format!("前序 Runtime 错误阻止了本次工具批次：{error}"),
                        true,
                    ),
                ));
                continue;
            }
            if request.cancellation.is_cancelled() {
                let error = AgentRunError::Cancelled;
                prepared.push(PreparedCall::immediate(
                    index,
                    ToolResult::text(call.id, "工具调用在执行前因 Turn 取消而中止", true),
                ));
                preparation_error = Some(error);
                continue;
            }
            let tool_call_id =
                crate::ToolCallId::new(call.id.clone()).map_err(|_| AgentRunError::Internal {
                    message: "已通过批次预检的工具调用 ID 在冻结时失效".to_owned(),
                })?;
            let Some(tool) = self.tools.get(&call.name) else {
                prepared.push(PreparedCall::immediate(
                    index,
                    ToolResult::text(call.id, format!("工具不存在：{}", call.name), true),
                ));
                continue;
            };
            let definition =
                self.tools
                    .definition(&call.name)
                    .ok_or_else(|| AgentRunError::Internal {
                        message: format!("已解析工具 {} 缺少冻结定义", call.name),
                    })?;
            if let Err(error) = definition.validate_input(&call.arguments) {
                prepared.push(PreparedCall::immediate(
                    index,
                    ToolResult::text(call.id, format!("工具输入无效：{error}"), true),
                ));
                continue;
            }
            let mut effect = match tool.effect(&call.arguments) {
                Ok(effect) => effect,
                Err(error) => {
                    prepared.push(PreparedCall::immediate(
                        index,
                        tool_error_result(&call.id, &error),
                    ));
                    continue;
                }
            };
            let pre_hook = match self
                .hooks
                .run_pre_tool_use(
                    PreToolUseContext {
                        invocation: hook_invocation_context(request),
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        input: call.arguments.clone(),
                    },
                    &request.cancellation,
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    let run_error = AgentRunError::from(error);
                    prepared.push(PreparedCall::immediate(
                        index,
                        ToolResult::text(call.id, PRE_HOOK_FAILED_RESULT, true),
                    ));
                    preparation_error = Some(run_error);
                    continue;
                }
            };
            let hook_context = pre_hook.context;
            if let Some(message) = pre_hook.blocked {
                let visible_text = format!("PreToolUse Hook 阻止了工具执行：{message}");
                let model_visible_hook_bytes = visible_text.len();
                prepared.push(
                    PreparedCall::immediate_with_context_and_model_visible_bytes(
                        index,
                        ToolResult::text(call.id, visible_text, true),
                        hook_context,
                        model_visible_hook_bytes,
                    ),
                );
                continue;
            }
            if pre_hook.modified {
                call.arguments = pre_hook.input;
                if let Err(error) = definition.validate_input(&call.arguments) {
                    prepared.push(PreparedCall::immediate_with_context(
                        index,
                        ToolResult::text(
                            call.id,
                            format!("Hook 修改后的工具输入无效：{error}"),
                            true,
                        ),
                        hook_context,
                    ));
                    continue;
                }
                final_tool_calls[index] = call.clone();
                effect = match tool.effect(&call.arguments) {
                    Ok(effect) => effect,
                    Err(error) => {
                        prepared.push(PreparedCall::immediate_with_context(
                            index,
                            tool_error_result(&call.id, &error),
                            hook_context,
                        ));
                        continue;
                    }
                };
            }
            let fingerprint = ToolCallFingerprint {
                tool_name: call.name.clone(),
                input_hash: canonical_input_hash(&call.arguments)?,
            };
            if let Err(error) =
                active.observe_tool_call(fingerprint.clone(), self.limits.max_identical_tool_calls)
            {
                prepared.push(PreparedCall::immediate_with_context(
                    index,
                    ToolResult::text(
                        call.id,
                        format!("Runtime 阻止了重复工具循环：{error}"),
                        true,
                    ),
                    hook_context,
                ));
                preparation_error = Some(error);
                continue;
            }
            match request.plan_guard.authorize(effect) {
                Ok(()) => {}
                Err(PlanGuardError::StateChangeDenied) => {
                    prepared.push(PreparedCall::immediate_with_context(
                        index,
                        ToolResult::text(call.id, "计划模式禁止执行会改变状态的工具", true),
                        hook_context,
                    ));
                    continue;
                }
            };
            prepared.push(PreparedCall::execute(
                index,
                PreparedExecution {
                    call,
                    tool_call_id,
                    tool: tool.clone(),
                    effect,
                    concurrency: tool.concurrency(),
                    fingerprint,
                },
                hook_context,
            ));
        }

        let initial_hook_context_bytes = active.hook_context_bytes;
        let mut preflight_bytes = initial_hook_context_bytes;
        let mut preflight_error = None;
        for call in &prepared {
            if let Err(error) = self.hooks.charge_context_and_model_visible_bytes(
                &mut preflight_bytes,
                call.context(),
                call.model_visible_hook_bytes(),
            ) {
                preflight_error = Some(AgentRunError::from(error));
                break;
            }
        }
        if let Some(error) = preflight_error {
            for call in &mut prepared {
                call.discard_hook_payloads_for_budget_error();
            }
            preflight_bytes = initial_hook_context_bytes;
            preparation_error.get_or_insert(error);
        }

        rewrite_response_tool_calls(response, &final_tool_calls)?;
        let frozen_pre_tool_context = prepared
            .iter()
            .flat_map(|call| call.context().iter().cloned())
            .map(ResolvedHookContext::into_message)
            .collect();
        let mut round_permit = self.preflight_tool_round(
            request,
            active.state.round_count(),
            active.next_segment_index,
            ModelRoundCompletion::from_response(response),
            Message::new(MessageRole::Assistant, response.content.clone()),
            frozen_pre_tool_context,
        )?;

        if preparation_error.is_none() {
            preparation_error = self
                .emit_tool_requests(
                    request,
                    active.state.round_count(),
                    &mut round_permit,
                    &mut prepared,
                )
                .err();
        }
        let batch = self
            .execute_prepared(
                request,
                active,
                PreparedBatch {
                    calls: prepared,
                    terminal_error: preparation_error,
                    preflight_hook_context_bytes: preflight_bytes,
                    round_permit,
                },
            )
            .await?;
        Ok(batch)
    }

    /// 执行相邻并发安全只读段，并让其他调用形成顺序屏障。
    async fn execute_prepared(
        &self,
        request: &TurnRequest,
        active: &mut ActiveTurn,
        batch: PreparedBatch,
    ) -> Result<ToolBatchResult, AgentRunError> {
        let PreparedBatch {
            calls: mut prepared,
            mut terminal_error,
            preflight_hook_context_bytes,
            mut round_permit,
        } = batch;
        let mut results = vec![None; prepared.len()];
        let mut result_budget_charged = vec![false; prepared.len()];
        let mut post_context = (0..prepared.len())
            .map(|_| None)
            .collect::<Vec<Option<Vec<ResolvedHookContext>>>>();
        let executable_count = prepared
            .iter()
            .filter(|call| matches!(&call.disposition, PreparedDisposition::Execute { .. }))
            .count();
        if terminal_error.is_none() && executable_count > 0 {
            enter_execution_phase(&mut active.state)?;
        }
        let mut summary_error = None;
        let mut completion_error = None;
        let mut prospective_hook_context_bytes = preflight_hook_context_bytes;
        let mut hook_budget_failed = false;
        let mut round_output_budget = ToolRoundOutputBudget::new(prepared.len());
        let mut post_hook_output_budget = PostHookOutputBudget::default();
        let mut cursor = 0;
        while terminal_error.is_none() && cursor < prepared.len() {
            match &prepared[cursor].disposition {
                PreparedDisposition::Immediate(result) => {
                    let index = prepared[cursor].index;
                    let result =
                        normalize_immediate_round_result(result.clone(), &mut round_output_budget)?;
                    results[index] = Some(result.clone());
                    result_budget_charged[index] = true;
                    if let Err(error) = self.emit_tool_completed(
                        request,
                        active.state.round_count(),
                        &mut round_permit,
                        &mut prepared[cursor],
                        ToolCompletionStatus::Cancelled,
                        &result,
                    ) {
                        completion_error.get_or_insert(error);
                    }
                    cursor += 1;
                    if completion_error.is_some() {
                        break;
                    }
                }
                PreparedDisposition::Execute {
                    effect: ToolEffect::ReadOnly,
                    concurrency: ToolConcurrency::ParallelReadOnly,
                    ..
                } => {
                    let start = cursor;
                    while cursor < prepared.len() && prepared[cursor].is_parallel_read_only() {
                        cursor += 1;
                    }
                    let end = cursor;
                    if let Err(error) =
                        ensure_step_capacity(&active.state, self.limits.max_steps, end - start)
                    {
                        fill_unexecuted_results(&prepared[start..], &mut results, &error);
                        summary_error = Some(error);
                        break;
                    }
                    let cancel_grace = Duration::from_millis(self.limits.tool_cancel_grace_ms);
                    let segment_cancellation = request.cancellation.child_token();
                    let mut futures = FuturesUnordered::new();
                    for (index, prepared_call) in
                        prepared.iter_mut().enumerate().take(end).skip(start)
                    {
                        if let Err(error) =
                            ensure_step_capacity(&active.state, self.limits.max_steps, 1)
                        {
                            segment_cancellation.cancel();
                            terminal_error.get_or_insert(error);
                            break;
                        }
                        if let Err(error) = self.emit_tool_execution_started(
                            request,
                            active.state.round_count(),
                            &mut round_permit,
                            prepared_call,
                        ) {
                            segment_cancellation.cancel();
                            terminal_error.get_or_insert(error);
                            break;
                        }
                        if let Err(error) =
                            record_started_steps(&mut active.state, self.limits.max_steps, 1)
                        {
                            segment_cancellation.cancel();
                            terminal_error.get_or_insert(error);
                            break;
                        }
                        let call = prepared_call.clone();
                        let call_cancellation = segment_cancellation.clone();
                        futures.push(async move {
                            (
                                index,
                                execute_one_raw(request, call, call_cancellation, cancel_grace)
                                    .await,
                            )
                        });
                    }
                    while let Some((index, outcome)) = futures.next().await {
                        match outcome {
                            Ok(mut raw) => {
                                let post = if completion_error.is_none()
                                    && terminal_error.is_none()
                                    && !round_permit.recovery_retained()
                                {
                                    finalize_tool_before_completion(
                                        request,
                                        &prepared[index],
                                        &mut raw,
                                        &self.hooks,
                                        ToolEffect::ReadOnly,
                                        ToolFinalizationBudgets {
                                            hook_context_bytes: &mut prospective_hook_context_bytes,
                                            post_hook_output: &mut post_hook_output_budget,
                                            round_output: &mut round_output_budget,
                                            hook_budget_failed: &mut hook_budget_failed,
                                        },
                                    )
                                    .await?
                                } else {
                                    raw.enforce_round_budget(
                                        ToolEffect::ReadOnly,
                                        &mut round_output_budget,
                                    )?;
                                    Vec::new()
                                };
                                results[index] = Some(raw.result.clone());
                                result_budget_charged[index] = true;
                                if !round_permit.recovery_retained() {
                                    if let Err(error) = self.emit_tool_completed(
                                        request,
                                        active.state.round_count(),
                                        &mut round_permit,
                                        &mut prepared[index],
                                        raw.status,
                                        &raw.result,
                                    ) {
                                        segment_cancellation.cancel();
                                        completion_error.get_or_insert(error);
                                    }
                                }
                                post_context[index] = Some(post);
                                if let Some(error) = &raw.terminal_error {
                                    segment_cancellation.cancel();
                                    terminal_error.get_or_insert_with(|| error.clone());
                                }
                                if let Some(observation) = raw.observation {
                                    if let Some(error) = active.observe_execution(
                                        observation,
                                        self.limits.max_repeated_tool_failures,
                                    ) {
                                        segment_cancellation.cancel();
                                        terminal_error.get_or_insert(error);
                                    }
                                }
                            }
                            Err(error) => {
                                segment_cancellation.cancel();
                                let result = normalize_immediate_round_result(
                                    interrupted_tool_result(&prepared[index], &error),
                                    &mut round_output_budget,
                                )?;
                                results[index] = Some(result.clone());
                                result_budget_charged[index] = true;
                                if !round_permit.recovery_retained() {
                                    if let Err(delivery_error) = self.emit_tool_completed(
                                        request,
                                        active.state.round_count(),
                                        &mut round_permit,
                                        &mut prepared[index],
                                        completion_status_for_run_error(&error),
                                        &result,
                                    ) {
                                        completion_error.get_or_insert(delivery_error);
                                    }
                                }
                                terminal_error.get_or_insert(error);
                            }
                        }
                    }
                    if completion_error.is_some() || terminal_error.is_some() {
                        break;
                    }
                }
                PreparedDisposition::Execute { .. } => {
                    let index = prepared[cursor].index;
                    if let Err(error) =
                        ensure_step_capacity(&active.state, self.limits.max_steps, 1)
                    {
                        fill_unexecuted_results(&prepared[cursor..], &mut results, &error);
                        summary_error = Some(error);
                        break;
                    }
                    if let Err(error) = self.emit_tool_execution_started(
                        request,
                        active.state.round_count(),
                        &mut round_permit,
                        &mut prepared[cursor],
                    ) {
                        terminal_error.get_or_insert(error);
                        break;
                    }
                    if let Err(error) =
                        record_started_steps(&mut active.state, self.limits.max_steps, 1)
                    {
                        terminal_error.get_or_insert(error);
                        break;
                    }
                    let call = prepared[cursor].clone();
                    match execute_one_raw(
                        request,
                        call,
                        request.cancellation.child_token(),
                        Duration::from_millis(self.limits.tool_cancel_grace_ms),
                    )
                    .await
                    {
                        Ok(mut raw) => {
                            let effect = prepared[cursor].execution_effect().ok_or_else(|| {
                                AgentRunError::Internal {
                                    message: "已执行工具缺少冻结副作用分类".to_owned(),
                                }
                            })?;
                            let post = finalize_tool_before_completion(
                                request,
                                &prepared[cursor],
                                &mut raw,
                                &self.hooks,
                                effect,
                                ToolFinalizationBudgets {
                                    hook_context_bytes: &mut prospective_hook_context_bytes,
                                    post_hook_output: &mut post_hook_output_budget,
                                    round_output: &mut round_output_budget,
                                    hook_budget_failed: &mut hook_budget_failed,
                                },
                            )
                            .await?;
                            results[index] = Some(raw.result.clone());
                            result_budget_charged[index] = true;
                            if let Err(error) = self.emit_tool_completed(
                                request,
                                active.state.round_count(),
                                &mut round_permit,
                                &mut prepared[cursor],
                                raw.status,
                                &raw.result,
                            ) {
                                completion_error.get_or_insert(error);
                            }
                            post_context[index] = Some(post);
                            if let Some(error) = &raw.terminal_error {
                                terminal_error.get_or_insert_with(|| error.clone());
                            }
                            if let Some(observation) = raw.observation {
                                if let Some(error) = active.observe_execution(
                                    observation,
                                    self.limits.max_repeated_tool_failures,
                                ) {
                                    terminal_error.get_or_insert(error);
                                }
                            }
                        }
                        Err(error) => {
                            let result = normalize_immediate_round_result(
                                interrupted_tool_result(&prepared[cursor], &error),
                                &mut round_output_budget,
                            )?;
                            results[index] = Some(result.clone());
                            result_budget_charged[index] = true;
                            if let Err(delivery_error) = self.emit_tool_completed(
                                request,
                                active.state.round_count(),
                                &mut round_permit,
                                &mut prepared[cursor],
                                completion_status_for_run_error(&error),
                                &result,
                            ) {
                                completion_error.get_or_insert(delivery_error);
                            }
                            terminal_error.get_or_insert(error);
                        }
                    }
                    cursor += 1;
                    if completion_error.is_some() || terminal_error.is_some() {
                        break;
                    }
                }
            }
        }

        let cleanup_error = completion_error
            .as_ref()
            .or(terminal_error.as_ref())
            .or(summary_error.as_ref())
            .cloned();
        if let Some(error) = &cleanup_error {
            fill_unexecuted_results(&prepared, &mut results, error);
        }
        for index in 0..results.len() {
            if result_budget_charged[index] {
                continue;
            }
            let result = results[index]
                .take()
                .ok_or_else(|| AgentRunError::Internal {
                    message: format!("待归一工具批次第 {index} 项缺少配对结果"),
                })?;
            results[index] = Some(normalize_immediate_round_result(
                result,
                &mut round_output_budget,
            )?);
            result_budget_charged[index] = true;
        }
        for index in 0..prepared.len() {
            if round_permit.recovery_retained() {
                break;
            }
            if prepared[index].lifecycle_completion_attempted()
                || prepared[index].requested_lifecycle().is_none()
            {
                continue;
            }
            let result = results[index]
                .as_ref()
                .ok_or_else(|| AgentRunError::Internal {
                    message: format!("待收尾工具批次第 {index} 项缺少配对结果"),
                })?
                .clone();
            let status = if prepared[index].lifecycle_started() {
                completion_status_for_run_error(cleanup_error.as_ref().ok_or_else(|| {
                    AgentRunError::Internal {
                        message: format!("已启动待收尾工具批次第 {index} 项缺少终止错误"),
                    }
                })?)
            } else {
                ToolCompletionStatus::Cancelled
            };
            if let Err(error) = self.emit_tool_completed(
                request,
                active.state.round_count(),
                &mut round_permit,
                &mut prepared[index],
                status,
                &result,
            ) {
                completion_error.get_or_insert(error);
            }
        }
        let lifecycle_fully_committed = !round_permit.recovery_retained()
            && completion_error.is_none()
            && !matches!(terminal_error, Some(AgentRunError::CommitSink(_)));
        if let Some(error) = completion_error {
            terminal_error = Some(merge_tool_completion_error(terminal_error, error));
        }
        let results = collect_tool_results(results)?;
        let post_context = if hook_budget_failed {
            (0..prepared.len()).map(|_| Vec::new()).collect()
        } else {
            post_context
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect()
        };
        Ok(ToolBatchResult {
            results,
            post_context,
            terminal_error,
            summary_error,
            round_permit,
            hook_context_bytes: if hook_budget_failed {
                preflight_hook_context_bytes
            } else {
                prospective_hook_context_bytes
            },
            lifecycle_fully_committed,
        })
    }
}

/// 为一次模型调用冻结且不接受 Provider 覆盖的事件身份。
#[derive(Clone)]
struct ModelEventIdentity {
    /// 当前模型调用所属的根 Session。
    session_id: SessionId,
    /// 当前模型调用所属的用户 Turn。
    turn_id: TurnId,
    /// 发起当前模型调用的根 Agent 或单层子 Agent。
    source_agent_id: AgentId,
    /// 当前调用实际使用的 Provider 中立模型标识。
    model: String,
    /// Turn 状态机在请求前写入的逻辑 Round。
    model_round: u32,
}

impl ModelEventIdentity {
    /// 从 Turn 控制面身份和逻辑 Round 构造非 Provider 事件信封。
    fn for_turn(request: &TurnRequest, model_round: u32) -> Self {
        Self {
            session_id: request.session_id.clone(),
            turn_id: request.turn_id.clone(),
            source_agent_id: request.source_agent_id.clone(),
            model: request.model_request.model.clone(),
            model_round,
        }
    }

    /// 使用冻结身份包装一个 Provider 事件或失败边界。
    fn envelope(&self, kind: AgentStreamEventKind) -> AgentStreamEvent {
        AgentStreamEvent::new(
            self.session_id.clone(),
            self.turn_id.clone(),
            self.source_agent_id.clone(),
            self.model.clone(),
            self.model_round,
            kind,
        )
    }

    /// 使用冻结身份包装一个工具、压缩或 Transcript 权威提交事件。
    fn commit_envelope(&self, kind: AgentCommitEventKind) -> AgentCommitEvent {
        AgentCommitEvent::new(
            self.session_id.clone(),
            self.turn_id.clone(),
            self.source_agent_id.clone(),
            self.model.clone(),
            self.model_round,
            kind,
        )
    }
}

/// tap 流在两次轮询之间保留的 Provider、Sink 与终止栅栏状态。
struct TappedModelStream {
    /// 尚未消费完成的 Provider 中立事件流。
    model_stream: ModelStream,
    /// 当前 Runner 注入且对不同 Turn 并发安全的实时出口。
    event_sink: Arc<dyn AgentEventSink>,
    /// 当前模型调用冻结后的可信事件身份。
    identity: ModelEventIdentity,
    /// 与模型读取和普通事件投递竞态的 Turn 取消令牌。
    cancellation: TurnCancellation,
    /// 单事件等待 Sink 确认接收的硬时限。
    event_timeout: Duration,
    /// 向 tap 外层回传不能编码为 ModelError 的 Sink 失败。
    status: Arc<Mutex<ModelStreamTapStatus>>,
    /// MessageEnd、错误、取消或 Sink 失败后的不可逆轮询栅栏。
    done: bool,
}

/// tap 不能通过 `ModelStream` 类型直接返回的投递结果。
#[derive(Default)]
struct ModelStreamTapStatus {
    /// 首个 Sink 失败；一旦写入就不会被后续状态覆盖。
    delivery_error: Option<AgentEventDeliveryError>,
    /// 当前 Round 是否已经成功发送唯一失败边界。
    failure_boundary_sent: bool,
    /// 已由实时 Sink 确认接收的响应元数据。
    metadata: ResponseMetadata,
    /// 已由实时 Sink 确认接收的最新用量快照。
    usage: TokenUsage,
    /// 已由实时 Sink 确认接收的结束原因。
    stop_reason: Option<StopReason>,
}

/// 失败模型调用中已经由实时 Sink 确认的可记账用量快照。
struct ModelStreamTapUsage {
    /// Provider 在响应开始事件中报告的元数据。
    metadata: ResponseMetadata,
    /// Provider 在 Usage 事件中报告的字段集合。
    usage: TokenUsage,
    /// Provider 已报告的结束原因；流中断时可能为空。
    stop_reason: Option<StopReason>,
}

/// 普通事件投递被 Turn 取消或被 Sink 失败中止。
enum CancellableDeliveryError {
    /// Turn 取消赢得了与当前 Sink Future 的竞态。
    Cancelled,
    /// Sink 主动失败或超过硬接收时限。
    Delivery(AgentEventDeliveryError),
}

/// 轮询一个 Provider 事件，先等待 Sink 确认，再把同一事件交给严格归约器。
async fn tap_model_stream_event(
    mut tapped: TappedModelStream,
) -> Option<(
    Result<keencode_model::ModelStreamEvent, ModelError>,
    TappedModelStream,
)> {
    if tapped.done {
        return None;
    }
    let cancellation = tapped.cancellation.clone();
    let cancelled = Box::pin(cancellation.cancelled());
    let next_event = Box::pin(tapped.model_stream.next());
    let polled = select(cancelled, next_event).await;
    match polled {
        Either::Left(((), pending_event)) => {
            drop(pending_event);
            Some(finish_tapped_with_model_error(tapped, cancelled_model_error()).await)
        }
        Either::Right((None, pending_cancel)) => {
            drop(pending_cancel);
            None
        }
        Either::Right((Some(Err(error)), pending_cancel)) => {
            drop(pending_cancel);
            Some(finish_tapped_with_model_error(tapped, error).await)
        }
        Either::Right((Some(Ok(event)), pending_cancel)) => {
            drop(pending_cancel);
            let envelope = tapped
                .identity
                .envelope(AgentStreamEventKind::ModelEvent { event });
            match deliver_event_cancellable(
                &tapped.event_sink,
                &envelope,
                tapped.event_timeout,
                &tapped.cancellation,
            )
            .await
            {
                Ok(()) => {
                    let AgentStreamEventKind::ModelEvent { event } = envelope.into_kind() else {
                        unreachable!("普通模型事件信封的类型在 Sink 返回后不应改变");
                    };
                    observe_tap_model_event(&tapped.status, &event);
                    tapped.done =
                        matches!(event, keencode_model::ModelStreamEvent::MessageEnd { .. });
                    Some((Ok(event), tapped))
                }
                Err(CancellableDeliveryError::Cancelled) => {
                    Some(finish_tapped_with_model_error(tapped, cancelled_model_error()).await)
                }
                Err(CancellableDeliveryError::Delivery(delivery_error)) => {
                    record_tap_delivery_error(&tapped.status, delivery_error);
                    tapped.done = true;
                    Some((Err(sink_aborted_model_error()), tapped))
                }
            }
        }
    }
}

/// 发送唯一失败边界并停止 tap；边界投递失败时保留 Sink 根因。
async fn finish_tapped_with_model_error(
    mut tapped: TappedModelStream,
    error: ModelError,
) -> (
    Result<keencode_model::ModelStreamEvent, ModelError>,
    TappedModelStream,
) {
    match deliver_failure_boundary(
        &tapped.event_sink,
        &tapped.identity,
        &error,
        tapped.event_timeout,
    )
    .await
    {
        Ok(()) => mark_tap_failure_boundary_sent(&tapped.status),
        Err(delivery_error) => record_tap_delivery_error(&tapped.status, delivery_error),
    }
    tapped.done = true;
    let item = if has_tap_delivery_error(&tapped.status) {
        Err(sink_aborted_model_error())
    } else {
        Err(error)
    };
    (item, tapped)
}

/// 记录已由实时 Sink 确认的模型事件，供失败调用安全保留部分用量。
fn observe_tap_model_event(
    status: &Arc<Mutex<ModelStreamTapStatus>>,
    event: &keencode_model::ModelStreamEvent,
) {
    let mut status = status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match event {
        keencode_model::ModelStreamEvent::MessageStart { metadata } => {
            status.metadata = metadata.clone();
        }
        keencode_model::ModelStreamEvent::Usage { usage } => {
            status.usage.update_from(usage);
        }
        keencode_model::ModelStreamEvent::MessageEnd { stop_reason } => {
            status.stop_reason = Some(stop_reason.clone());
        }
        keencode_model::ModelStreamEvent::TextDelta { .. }
        | keencode_model::ModelStreamEvent::ReasoningDelta { .. }
        | keencode_model::ModelStreamEvent::ReasoningSummaryDelta { .. }
        | keencode_model::ModelStreamEvent::ReasoningContinuation { .. }
        | keencode_model::ModelStreamEvent::ToolCallStart { .. }
        | keencode_model::ModelStreamEvent::ToolCallArgumentsDelta { .. }
        | keencode_model::ModelStreamEvent::ToolCallEnd { .. } => {}
    }
}

/// 返回失败模型调用中已由实时 Sink 确认的明确用量；未知用量不伪造成已消耗。
fn tap_model_usage(status: &Arc<Mutex<ModelStreamTapStatus>>) -> Option<ModelStreamTapUsage> {
    let status = status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    status.usage.is_reported().then(|| ModelStreamTapUsage {
        metadata: status.metadata.clone(),
        usage: status.usage.clone(),
        stop_reason: status.stop_reason.clone(),
    })
}

/// 在硬时限内等待 Sink 可靠接收，主动失败与背压超时均不重试。
async fn deliver_event_bounded(
    event_sink: &Arc<dyn AgentEventSink>,
    event: &AgentStreamEvent,
    timeout: Duration,
) -> Result<(), AgentEventDeliveryError> {
    match tokio::time::timeout(timeout, event_sink.send(event)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(AgentEventDeliveryError::SinkFailed {
            message: truncate_utf8(error.message(), MAX_EVENT_SINK_ERROR_MESSAGE_BYTES),
        }),
        Err(_) => Err(AgentEventDeliveryError::TimedOut {
            maximum_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }),
    }
}

/// 让 Turn 取消与普通事件 Sink Future 竞态，取消后丢弃未确认事件。
async fn deliver_event_cancellable(
    event_sink: &Arc<dyn AgentEventSink>,
    event: &AgentStreamEvent,
    timeout: Duration,
    cancellation: &TurnCancellation,
) -> Result<(), CancellableDeliveryError> {
    let cancelled = Box::pin(cancellation.cancelled());
    let delivered = Box::pin(deliver_event_bounded(event_sink, event, timeout));
    match select(cancelled, delivered).await {
        Either::Left(((), pending_delivery)) => {
            drop(pending_delivery);
            Err(CancellableDeliveryError::Cancelled)
        }
        Either::Right((result, pending_cancel)) => {
            drop(pending_cancel);
            result.map_err(CancellableDeliveryError::Delivery)
        }
    }
}

/// 在不受已触发取消令牌短路的有界窗口内发送唯一 Round 失败边界。
async fn deliver_failure_boundary(
    event_sink: &Arc<dyn AgentEventSink>,
    identity: &ModelEventIdentity,
    error: &ModelError,
    timeout: Duration,
) -> Result<(), AgentEventDeliveryError> {
    let event = identity.envelope(AgentStreamEventKind::ModelFailure {
        error: error.clone(),
    });
    deliver_event_bounded(event_sink, &event, timeout).await
}

/// 标记当前 Round 的唯一失败边界已经被 Sink 可靠接收。
fn mark_tap_failure_boundary_sent(status: &Arc<Mutex<ModelStreamTapStatus>>) {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .failure_boundary_sent = true;
}

/// 返回当前 Round 是否已经成功发送失败边界。
fn tap_failure_boundary_sent(status: &Arc<Mutex<ModelStreamTapStatus>>) -> bool {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .failure_boundary_sent
}

/// 原子保留 tap 遇到的首个 Sink 失败，避免后续边界覆盖根因。
fn record_tap_delivery_error(
    status: &Arc<Mutex<ModelStreamTapStatus>>,
    error: AgentEventDeliveryError,
) {
    let mut status = status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if status.delivery_error.is_none() {
        status.delivery_error = Some(error);
    }
}

/// 返回 tap 是否已经记录不可恢复的 Sink 失败。
fn has_tap_delivery_error(status: &Arc<Mutex<ModelStreamTapStatus>>) -> bool {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .delivery_error
        .is_some()
}

/// 取出 tap 的唯一 Sink 失败供 AgentRunError 保持独立分类。
fn take_tap_delivery_error(
    status: &Arc<Mutex<ModelStreamTapStatus>>,
) -> Option<AgentEventDeliveryError> {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .delivery_error
        .take()
}

/// 创建不会包含 Provider 正文的统一 Turn 取消错误边界。
fn cancelled_model_error() -> ModelError {
    ModelError::Cancelled {
        message: "Agent Turn 在模型流完成前被取消".to_owned(),
    }
}

/// 把 Provider 已确认的模型终止原因转换为不会丢失语义的 Turn 错误。
fn model_terminal_error(stop_reason: &StopReason) -> Option<AgentRunError> {
    match stop_reason {
        StopReason::MaxOutputTokens => Some(AgentRunError::ModelOutputLimit),
        StopReason::ContentFilter => Some(AgentRunError::ModelRefusal),
        StopReason::Cancelled => Some(AgentRunError::Cancelled),
        StopReason::Completed | StopReason::ToolUse | StopReason::Other { .. } => None,
    }
}

/// 提取非正常模型响应中已经确认的文本和推理，丢弃不能独立回放的工具调用。
fn partial_model_response_messages(response: &ModelResponse) -> Vec<Message> {
    let content = response
        .content
        .iter()
        .filter(|block| {
            matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::Reasoning { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if content.is_empty() {
        Vec::new()
    } else {
        vec![Message::new(MessageRole::Assistant, content)]
    }
}

/// 创建只在 tap 内部占位且会被独立 Sink 错误替换的模型错误。
fn sink_aborted_model_error() -> ModelError {
    ModelError::Cancelled {
        message: "实时事件 Sink 中止了模型流".to_owned(),
    }
}

/// 为没有收到 MessageEnd 的失败调用生成不包含远端正文的稳定结束原因。
fn model_error_stop_reason(error: &ModelError) -> StopReason {
    if matches!(error, ModelError::Cancelled { .. }) {
        StopReason::Cancelled
    } else {
        StopReason::Other {
            reason: "model_error".to_owned(),
        }
    }
}

/// 将 Provider 的统一取消映射为 Turn 取消，其余保持模型错误分类。
fn model_error_to_run_error(error: ModelError) -> AgentRunError {
    match error {
        ModelError::Cancelled { .. } => AgentRunError::Cancelled,
        ModelError::Protocol { message } => {
            if let Some(id) = duplicate_tool_call_id_from_protocol_message(&message) {
                AgentRunError::DuplicateToolCallId { id }
            } else {
                AgentRunError::Model(ModelError::Protocol { message })
            }
        }
        error => AgentRunError::Model(error),
    }
}

/// 从模型流的稳定协议诊断中提取已经通过 ID 边界校验的重复调用标识。
fn duplicate_tool_call_id_from_protocol_message(message: &str) -> Option<String> {
    let id = message
        .strip_prefix("工具调用标识 ")?
        .strip_suffix(" 在响应中重复")?;
    crate::ToolCallId::new(id.to_owned())
        .ok()
        .map(|id| id.into_inner())
}

/// 一个正在运行且尚未归档的 Turn 内部状态。
struct ActiveTurn {
    /// 当前 Turn 唯一状态机及其 Round、Step 计数。
    state: TurnState,
    /// 已提交并会用于后续模型 Round 的 Provider 中立消息。
    messages: Vec<Message>,
    /// 当前 Turn 所有历史模型 Round 已见的工具调用 ID。
    seen_tool_call_ids: HashSet<String>,
    /// 当前 Turn 已成功提交且等待 Session 层持久化的压缩记录。
    compactions: Vec<ContextCompressionRecord>,
    /// Provider 超限后的强制压缩重试是否已在当前 Turn 消耗。
    forced_context_retry_used: bool,
    /// 当前 Turn 下一次 Provider 模型调用使用的单调尝试序号。
    next_model_call_attempt: u32,
    /// 当前 Turn 已实际注入模型消息的 Hook 上下文字节数。
    hook_context_bytes: usize,
    /// 当前 Turn 已运行的 Stop Hook 候选轮数。
    stop_hook_rounds: u32,
    /// 当前模型 Round 下一段 Transcript 权威提交应使用的零基序号。
    next_segment_index: u32,
    /// 最近一个通过最终输入校验的工具与输入摘要。
    last_tool_call: Option<ToolCallFingerprint>,
    /// 最近工具与输入摘要连续出现的次数。
    identical_tool_call_count: u32,
    /// 最近一次真实工具失败的完整指纹；任何成功或不同失败都会替换或清除。
    last_tool_failure: Option<ToolFailureFingerprint>,
    /// 最近相同失败指纹真正连续出现的次数。
    repeated_tool_failure_count: u32,
    /// Step 上限触发后等待执行唯一无工具总结 Round 的原始错误。
    step_limit_summary: Option<AgentRunError>,
}

impl ActiveTurn {
    /// 分配一个跨当前 Turn 单调递增的模型调用尝试序号，供用量幂等身份使用。
    fn next_model_call_attempt(&mut self) -> Result<u32, AgentRunError> {
        let attempt = self.next_model_call_attempt;
        self.next_model_call_attempt =
            attempt
                .checked_add(1)
                .ok_or_else(|| AgentRunError::Internal {
                    message: "模型调用尝试序号溢出".to_owned(),
                })?;
        Ok(attempt)
    }

    /// 记录一个最终规范化调用，并在超过连续同调用上限前阻止整个批次。
    fn observe_tool_call(
        &mut self,
        fingerprint: ToolCallFingerprint,
        maximum: u32,
    ) -> Result<(), AgentRunError> {
        if self.last_tool_call.as_ref() == Some(&fingerprint) {
            self.identical_tool_call_count = self.identical_tool_call_count.saturating_add(1);
        } else {
            self.last_tool_call = Some(fingerprint.clone());
            self.identical_tool_call_count = 1;
        }
        if self.identical_tool_call_count > maximum {
            return Err(AgentRunError::ToolLoop {
                kind: ToolLoopKind::IdenticalCall,
                tool_name: fingerprint.tool_name,
                maximum,
            });
        }
        Ok(())
    }

    /// 按模型原始调用顺序更新真实工具失败计数并返回首个熔断错误。
    fn observe_execution(
        &mut self,
        observation: ToolExecutionObservation,
        maximum: u32,
    ) -> Option<AgentRunError> {
        match observation {
            ToolExecutionObservation::Succeeded => {
                self.last_tool_failure = None;
                self.repeated_tool_failure_count = 0;
                None
            }
            ToolExecutionObservation::Failed { call, error_code } => {
                let key = ToolFailureFingerprint {
                    call: call.clone(),
                    error_code,
                };
                if self.last_tool_failure.as_ref() == Some(&key) {
                    self.repeated_tool_failure_count =
                        self.repeated_tool_failure_count.saturating_add(1);
                } else {
                    self.last_tool_failure = Some(key);
                    self.repeated_tool_failure_count = 1;
                }
                (self.repeated_tool_failure_count >= maximum).then_some(AgentRunError::ToolLoop {
                    kind: ToolLoopKind::RepeatedFailure,
                    tool_name: call.tool_name,
                    maximum,
                })
            }
        }
    }
}

/// 一个工具名称和最终规范化输入的稳定循环判定键。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ToolCallFingerprint {
    /// 已在注册表解析成功的精确工具名称。
    tool_name: String,
    /// 递归规范化 JSON 后的固定长度摘要。
    input_hash: ToolInputHash,
}

/// 跨 Round 记录同一真实工具失败的稳定键。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ToolFailureFingerprint {
    /// 工具名称和最终输入摘要。
    call: ToolCallFingerprint,
    /// 工具实现返回的稳定错误码。
    error_code: String,
}

/// 一次实际工具执行对重复失败计数器的确定性影响。
#[derive(Clone)]
enum ToolExecutionObservation {
    /// 工具成功，清除同工具和输入的全部错误码计数。
    Succeeded,
    /// 工具实现返回真实 ToolError，增加对应错误码计数。
    Failed {
        /// 工具名称和最终输入摘要。
        call: ToolCallFingerprint,
        /// ToolError 提供的稳定错误码。
        error_code: String,
    },
}

/// 一次已完整归约的 Provider 响应及其单调时钟墙钟耗时。
struct CompletedModelRound {
    /// Provider 中立完整响应。
    response: ModelResponse,
    /// 从发起 Provider 请求到响应完整归约的实际持续时间。
    elapsed: Duration,
    /// 当前 Turn 内对应的稳定模型调用尝试序号。
    call_attempt: u32,
}

/// 一个已完成模型响应及其可选结构化 JSON 投影。
struct CompletedTurn {
    /// 最终提交到 Transcript 的 Provider 中立响应。
    response: ModelResponse,
    /// 已经通过 Schema 校验的结构化值。
    structured_output: Option<Value>,
}

/// 由 Runner 绑定原始 Sink、冻结正文和一次性预留的工具 Round Permit。
pub(crate) struct AgentToolRoundPermit {
    /// 只能由本 Permit 完成的唯一 Round 身份。
    binding: AgentToolRoundBinding,
    /// 预检时冻结且最终模型 Round 必须原样提交的 Provider 响应事实。
    completion: ModelRoundCompletion,
    /// 预检时已写回最终工具参数的完整 Assistant 消息。
    assistant_message: Message,
    /// 预检时已知且必须紧随工具结果提交的全部 PreToolUse 消息。
    pre_tool_context: Vec<Message>,
    /// 签发 Permit 时刚完成预检的同一权威 Sink。
    sink: Arc<dyn AgentCommitSink>,
    /// 未消费、未转入不确定恢复保留时必须显式释放的预留。
    reservation: Option<Box<dyn AgentToolRoundReservation>>,
    /// 首个不确定提交已经冻结 Journal 推进，后续生命周期、Hook 与 Round 均不得继续。
    recovery_retained: bool,
}

impl AgentToolRoundPermit {
    /// 使用 Runner 刚完成预检的候选、Sink 和预留签发不可伪造的 Permit。
    pub(crate) fn new(
        round: AgentToolRoundPreflight,
        sink: Arc<dyn AgentCommitSink>,
        reservation: Box<dyn AgentToolRoundReservation>,
    ) -> Self {
        let (binding, completion, assistant_message, pre_tool_context) = round.into_parts();
        Self {
            binding,
            completion,
            assistant_message,
            pre_tool_context,
            sink,
            reservation: Some(reservation),
            recovery_retained: false,
        }
    }

    /// 返回最终 Round 必须复用的完整 Assistant 消息。
    const fn assistant_message(&self) -> &Message {
        &self.assistant_message
    }

    /// 返回最终模型 Round 必须复用的 Provider 响应事实。
    const fn completion(&self) -> &ModelRoundCompletion {
        &self.completion
    }

    /// 返回最终 Round 中必须紧随工具结果的全部 PreToolUse 消息。
    fn pre_tool_context(&self) -> &[Message] {
        &self.pre_tool_context
    }

    /// 返回当前 Round 是否已经因不确定提交进入只能恢复对账的冻结状态。
    const fn recovery_retained(&self) -> bool {
        self.recovery_retained
    }

    /// 首次提交结果不确定时把预留和完整事件转入恢复状态；后续不确定事件保持已冻结状态。
    fn retain_after_indeterminate(&mut self, event: AgentCommitEvent) -> Result<(), AgentRunError> {
        if !self.binding.matches_event_identity(&event) {
            return Err(AgentRunError::Internal {
                message: "工具 Round Permit 与待恢复事件身份不匹配".to_owned(),
            });
        }
        if self.recovery_retained {
            return Ok(());
        }
        self.reservation
            .take()
            .expect("首次进入恢复保留前必须仍持有工具 Round 预留")
            .retain_indeterminate(event);
        self.recovery_retained = true;
        Ok(())
    }

    /// 校验身份和冻结正文后，以同一事件身份有界重投并结束预留生命周期。
    pub(crate) fn commit(mut self, event: AgentCommitEvent) -> Result<(), AgentRunError> {
        if self.recovery_retained {
            return Err(AgentRunError::Internal {
                message: "已进入恢复保留的工具 Round 不得继续提交".to_owned(),
            });
        }
        if !self.binding.matches_commit_event(&event) {
            return Err(AgentRunError::Internal {
                message: "工具 Round Permit 与提交事件身份不匹配".to_owned(),
            });
        }
        let messages = match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted {
                completion,
                messages,
                ..
            } if completion == &self.completion => messages,
            _ => {
                return Err(AgentRunError::Internal {
                    message: "工具 Round Permit 只能提交预检冻结的模型 Round".to_owned(),
                });
            }
        };
        let pre_context_end = 2usize
            .checked_add(self.pre_tool_context.len())
            .ok_or_else(|| AgentRunError::Internal {
                message: "工具 Round PreToolUse 消息数量溢出".to_owned(),
            })?;
        let has_matching_tool_results = messages.get(1).is_some_and(|message| {
            if message.role != MessageRole::Tool || message.content.is_empty() {
                return false;
            }
            let expected_ids = self.assistant_message.content.iter().filter_map(|block| {
                let ContentBlock::ToolCall { tool_call } = block else {
                    return None;
                };
                Some(tool_call.id.as_str())
            });
            let actual_ids = message.content.iter().filter_map(|block| {
                let ContentBlock::ToolResult { tool_result } = block else {
                    return None;
                };
                Some(tool_result.tool_call_id.as_str())
            });
            message
                .content
                .iter()
                .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
                && expected_ids.eq(actual_ids)
        });
        if messages.first() != Some(&self.assistant_message)
            || !has_matching_tool_results
            || messages.get(2..pre_context_end) != Some(self.pre_tool_context.as_slice())
        {
            return Err(AgentRunError::Internal {
                message: "工具 Round Permit 与提交事件冻结正文不匹配".to_owned(),
            });
        }
        match commit_event_with_bounded_retry(
            self.sink.as_ref(),
            &event,
            AUTHORITATIVE_EVENT_MAX_COMMIT_ATTEMPTS,
        ) {
            Ok(()) => {
                self.reservation
                    .take()
                    .expect("工具 Round Permit 成功提交前必须持有预留")
                    .consume();
                Ok(())
            }
            Err(error) if error.kind() == AgentCommitSinkErrorKind::Indeterminate => {
                self.retain_after_indeterminate(event)?;
                Err(commit_sink_run_error(error))
            }
            Err(error) => Err(commit_sink_run_error(error)),
        }
    }
}

impl Drop for AgentToolRoundPermit {
    /// 所有未成功消费或转入恢复保留的路径都显式释放预留。
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            reservation.release();
        }
    }
}

/// 已完成预检并等待按屏障规则执行的一批工具输入。
struct PreparedBatch {
    /// 按模型原始顺序冻结的工具调用处置。
    calls: Vec<PreparedCall>,
    /// PreHook、计划守卫或取消已经确定的可选终止错误。
    terminal_error: Option<AgentRunError>,
    /// 全部 PreHook 内容通过原子预检后的候选字节数。
    preflight_hook_context_bytes: usize,
    /// 持有到匹配工具 Round 成功提交或任意提前退出的一次性容量 Permit。
    round_permit: AgentToolRoundPermit,
}

/// 一批工具调用的完整配对结果及可选 Turn 终止原因。
struct ToolBatchResult {
    /// 与模型工具调用一一对应且保持原顺序的结果。
    results: Vec<ToolResult>,
    /// 按模型调用顺序保留的 PostToolUse 上下文；超预算时每项均为空。
    post_context: Vec<Vec<ResolvedHookContext>>,
    /// 工具已经开始后发生取消或内部失败时延后提交的终止错误。
    terminal_error: Option<AgentRunError>,
    /// Step 上限触发后等待唯一无工具总结 Round 的原始错误。
    summary_error: Option<AgentRunError>,
    /// 只能消费一次且绑定最终工具 Round 身份的容量 Permit。
    round_permit: AgentToolRoundPermit,
    /// 本批实际提交后当前 Turn 已占用的 Hook 上下文字节数。
    hook_context_bytes: usize,
    /// `true` 表示全部已请求工具的唯一终态均已由 Sink 确认，允许提交 Round。
    lifecycle_fully_committed: bool,
}

/// 校验保留结果工具调用并转换为统一最终文本响应。
fn complete_emulated_output(
    mode: &StructuredOutputMode,
    response: ModelResponse,
) -> Result<CompletedTurn, AgentRunError> {
    let value = mode
        .parse_response(&response)?
        .ok_or_else(|| AgentRunError::Internal {
            message: "结果工具只应在结构化输出模式下完成".to_owned(),
        })?;
    let serialized = serde_json::to_string(&value).map_err(|error| AgentRunError::Internal {
        message: format!("结构化 JSON 无法序列化：{error}"),
    })?;
    let mut content = response
        .content
        .into_iter()
        .filter(|block| matches!(block, ContentBlock::Reasoning { .. }))
        .collect::<Vec<_>>();
    content.push(ContentBlock::text(serialized));
    Ok(CompletedTurn {
        response: ModelResponse::new(
            response.metadata,
            content,
            response.usage,
            StopReason::Completed,
        ),
        structured_output: Some(value),
    })
}

/// 创建能保留结构化失败分类的 Agent 运行错误。
fn structured_run_error(
    enforcement: StructuredOutputEnforcement,
    failure: StructuredOutputFailureKind,
    message: impl Into<String>,
) -> AgentRunError {
    AgentRunError::Model(ModelError::StructuredOutput {
        enforcement,
        failure,
        message: message.into(),
    })
}

/// 一个已经冻结输入和执行策略的工具执行候选。
#[derive(Clone)]
struct PreparedExecution {
    /// 模型产生且最终输入已经重新校验的工具调用。
    call: ToolCall,
    /// 从真实模型调用冻结且经过非空、有界校验的可信身份。
    tool_call_id: crate::ToolCallId,
    /// 注册表中与冻结定义一致的工具实现。
    tool: Arc<dyn AgentTool>,
    /// 基于最终输入重新计算的副作用分类。
    effect: ToolEffect,
    /// 工具声明且受副作用屏障约束的并发方式。
    concurrency: ToolConcurrency,
    /// 工具名称和最终输入的循环保护摘要。
    fingerprint: ToolCallFingerprint,
}

/// 一个已经通过全部批次预检并需要形成持久生命周期的工具请求。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparedToolLifecyclePhase {
    /// 生命周期事件尚未可靠进入 Sink。
    Prepared,
    /// 工具请求已经可靠提交。
    Requested,
    /// 工具执行起点已经可靠提交。
    Started,
    /// 工具唯一终态已经可靠提交。
    Completed,
    /// 工具终态在有限失败重投或一次超时后仍未确认，本 Turn 不得提交 Round。
    CompletionUnconfirmed,
}

/// 一个已经通过全部批次预检并需要形成持久生命周期的工具请求。
#[derive(Clone)]
struct PreparedToolLifecycle {
    /// 已完成有界校验的可信工具调用标识。
    tool_call_id: crate::ToolCallId,
    /// Hook 修改后最终冻结的工具名称和参数。
    call: ToolCall,
    /// 最终参数对应的副作用分类。
    effect: ToolEffect,
    /// 按 Sink 确认或投递失败结果推进的显式生命周期阶段。
    phase: PreparedToolLifecyclePhase,
}

/// 预检完成后等待执行或已经形成结果的工具调用。
#[derive(Clone)]
struct PreparedCall {
    /// 当前调用在模型响应中的稳定原始位置。
    index: usize,
    /// 当前调用冻结后的立即结果或待执行信息。
    disposition: PreparedDisposition,
    /// PreToolUse 已生成且等待按模型调用顺序提交的上下文。
    context: Vec<ResolvedHookContext>,
    /// Hook 生成且通过其他消息包装进入模型的实际可见文本字节数。
    model_visible_hook_bytes: usize,
    /// 通过全部批次预检后发送给 Session 层的工具生命周期元数据。
    lifecycle: Option<PreparedToolLifecycle>,
}

impl PreparedCall {
    /// 创建不需要启动工具的立即结果。
    fn immediate(index: usize, result: ToolResult) -> Self {
        Self {
            index,
            disposition: PreparedDisposition::Immediate(result),
            context: Vec::new(),
            model_visible_hook_bytes: 0,
            lifecycle: None,
        }
    }

    /// 创建不启动工具但仍提交 PreToolUse 上下文的立即结果。
    fn immediate_with_context(
        index: usize,
        result: ToolResult,
        context: Vec<ResolvedHookContext>,
    ) -> Self {
        Self {
            index,
            disposition: PreparedDisposition::Immediate(result),
            context,
            model_visible_hook_bytes: 0,
            lifecycle: None,
        }
    }

    /// 创建带 PreToolUse 上下文及额外模型可见 Hook 文本的立即结果。
    fn immediate_with_context_and_model_visible_bytes(
        index: usize,
        result: ToolResult,
        context: Vec<ResolvedHookContext>,
        model_visible_hook_bytes: usize,
    ) -> Self {
        Self {
            index,
            disposition: PreparedDisposition::Immediate(result),
            context,
            model_visible_hook_bytes,
            lifecycle: None,
        }
    }

    /// 创建输入和执行策略均已冻结的工具调用。
    fn execute(
        index: usize,
        execution: PreparedExecution,
        context: Vec<ResolvedHookContext>,
    ) -> Self {
        let PreparedExecution {
            call,
            tool_call_id,
            tool,
            effect,
            concurrency,
            fingerprint,
        } = execution;
        let lifecycle = PreparedToolLifecycle {
            tool_call_id: tool_call_id.clone(),
            call: call.clone(),
            effect,
            phase: PreparedToolLifecyclePhase::Prepared,
        };
        Self {
            index,
            disposition: PreparedDisposition::Execute {
                call,
                tool_call_id,
                tool,
                effect,
                concurrency,
                fingerprint,
            },
            context,
            model_visible_hook_bytes: 0,
            lifecycle: Some(lifecycle),
        }
    }

    /// 判断当前调用能否加入相邻只读并发段。
    fn is_parallel_read_only(&self) -> bool {
        matches!(
            self.disposition,
            PreparedDisposition::Execute {
                effect: ToolEffect::ReadOnly,
                concurrency: ToolConcurrency::ParallelReadOnly,
                ..
            }
        )
    }

    /// 返回 PreToolUse 已生成的上下文，用于在任何副作用前预检全局字节预算。
    fn context(&self) -> &[ResolvedHookContext] {
        &self.context
    }

    /// 返回由 Hook 通过工具结果包装进入模型的额外可见文本字节数。
    const fn model_visible_hook_bytes(&self) -> usize {
        self.model_visible_hook_bytes
    }

    /// 返回当前调用已经冻结且等待可靠投递的生命周期元数据。
    const fn lifecycle(&self) -> Option<&PreparedToolLifecycle> {
        self.lifecycle.as_ref()
    }

    /// 返回已经由 Session 层可靠接收工具请求的生命周期元数据。
    fn requested_lifecycle(&self) -> Option<&PreparedToolLifecycle> {
        self.lifecycle
            .as_ref()
            .filter(|lifecycle| lifecycle.phase != PreparedToolLifecyclePhase::Prepared)
    }

    /// 返回当前调用可变的生命周期元数据。
    fn lifecycle_mut(&mut self) -> Option<&mut PreparedToolLifecycle> {
        self.lifecycle.as_mut()
    }

    /// 返回当前调用是否已经尝试过工具终态投递。
    fn lifecycle_completion_attempted(&self) -> bool {
        self.lifecycle.as_ref().is_some_and(|lifecycle| {
            matches!(
                lifecycle.phase,
                PreparedToolLifecyclePhase::Completed
                    | PreparedToolLifecyclePhase::CompletionUnconfirmed
            )
        })
    }

    /// 返回当前调用是否已经越过可靠执行起点。
    fn lifecycle_started(&self) -> bool {
        self.lifecycle
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.phase == PreparedToolLifecyclePhase::Started)
    }

    /// 返回真实工具执行和 PostHook 使用的最终调用。
    fn execution_call(&self) -> Option<&ToolCall> {
        match &self.disposition {
            PreparedDisposition::Immediate(_) => None,
            PreparedDisposition::Execute { call, .. } => Some(call),
        }
    }

    /// 返回真实执行候选已经冻结的副作用分类。
    const fn execution_effect(&self) -> Option<ToolEffect> {
        match &self.disposition {
            PreparedDisposition::Immediate(_) => None,
            PreparedDisposition::Execute { effect, .. } => Some(*effect),
        }
    }

    /// Hook 预算失败时丢弃上下文，并用固定文本替换 Hook 自有可见结果。
    fn discard_hook_payloads_for_budget_error(&mut self) {
        self.context.clear();
        if self.model_visible_hook_bytes > 0 {
            if let PreparedDisposition::Immediate(result) = &mut self.disposition {
                let tool_call_id = result.tool_call_id.clone();
                *result = ToolResult::text(tool_call_id, HOOK_BUDGET_EXCEEDED_RESULT, true);
            }
            self.model_visible_hook_bytes = 0;
        }
    }
}

/// 工具调用在冻结和预检阶段使用的唯一处置。
#[derive(Clone)]
enum PreparedDisposition {
    /// 不启动工具而直接回传的错误或拒绝结果。
    Immediate(ToolResult),
    /// 输入与策略已冻结，等待按指定屏障规则执行的调用。
    Execute {
        /// 模型产生且输入已冻结的调用。
        call: ToolCall,
        /// 从真实模型调用冻结且工具输入不能覆盖的可信身份。
        tool_call_id: crate::ToolCallId,
        /// 注册表中的工具实现。
        tool: Arc<dyn AgentTool>,
        /// 本次规范化输入的副作用分类。
        effect: ToolEffect,
        /// 工具声明的只读并发方式。
        concurrency: ToolConcurrency,
        /// 工具名称和最终输入的循环保护摘要。
        fingerprint: ToolCallFingerprint,
    },
}

/// 把执行器内部错误映射为已经开始工具的稳定终态分类。
fn completion_status_for_run_error(error: &AgentRunError) -> ToolCompletionStatus {
    if matches!(error, AgentRunError::Cancelled) {
        ToolCompletionStatus::Cancelled
    } else {
        ToolCompletionStatus::Failed
    }
}

/// 判断 Hook 终止错误是否来自 PostHook 数量、编码或全局上下文容量硬上限。
fn is_post_hook_output_limit_error(error: &AgentRunError) -> bool {
    matches!(
        error,
        AgentRunError::Hook(
            HookError::ContextBytesExceeded { .. }
                | HookError::PostOutputAdditionsExceeded { .. }
                | HookError::PostOutputModelVisibleBytesExceeded { .. }
                | HookError::PostOutputJsonBytesExceeded { .. }
                | HookError::PostOutputRoundBytesExceeded { .. }
        )
    )
}

/// 一次工具最终化期间需要原子更新的四组累计预算。
struct ToolFinalizationBudgets<'a> {
    /// 当前 Turn 已接纳的 Hook 上下文 UTF-8 字节数。
    hook_context_bytes: &'a mut usize,
    /// 当前工具 Round 已接纳的 PostHook 独立预算。
    post_hook_output: &'a mut PostHookOutputBudget,
    /// 当前工具 Round 已接纳的结果与 PostHook 聚合预算。
    round_output: &'a mut ToolRoundOutputBudget,
    /// 任一 PostHook 容量失败后用于整批清空 Post 内容的稳定标记。
    hook_budget_failed: &'a mut bool,
}

/// 在唯一工具终态提交前运行 PostHook、原子计量其输出并冻结最终结果。
async fn finalize_tool_before_completion(
    request: &TurnRequest,
    prepared: &PreparedCall,
    raw: &mut RawExecutedTool,
    hooks: &HookRuntime,
    effect: ToolEffect,
    budgets: ToolFinalizationBudgets<'_>,
) -> Result<Vec<ResolvedHookContext>, AgentRunError> {
    let base_round_output_budget = *budgets.round_output;
    let mut candidate_round_output_budget = base_round_output_budget;
    raw.enforce_round_budget(effect, &mut candidate_round_output_budget)?;
    let failure_hook_already_saw_fixed_result =
        raw.failure == Some(ToolHookFailureKind::OutputLimitExceeded);
    let outcome = run_post_tool_hook(request, prepared, raw, hooks).await;
    let mut candidate_hook_context_bytes = *budgets.hook_context_bytes;
    let mut candidate_post_hook_output_budget = *budgets.post_hook_output;
    let mut capacity_error = outcome
        .terminal_error
        .as_ref()
        .filter(|error| is_post_hook_output_limit_error(error))
        .cloned();

    if capacity_error.is_none() {
        let charged = candidate_post_hook_output_budget
            .charge(&outcome.context)
            .and_then(|footprint| {
                if candidate_round_output_budget.try_charge_post_hook(
                    footprint.content_blocks,
                    footprint.model_visible_bytes,
                    footprint.json_bytes,
                ) {
                    Ok(())
                } else {
                    Err(HookError::PostOutputRoundBytesExceeded {
                        maximum_model_visible_bytes: crate::TOOL_OUTPUT_LIMITS
                            .max_round_model_visible_bytes,
                        maximum_json_bytes: crate::TOOL_OUTPUT_LIMITS.max_round_json_bytes,
                    })
                }
            })
            .and_then(|()| {
                hooks.charge_context(&mut candidate_hook_context_bytes, &outcome.context)
            });
        if let Err(error) = charged {
            capacity_error = Some(error.into());
        }
    }

    if let Some(error) = capacity_error {
        *budgets.hook_budget_failed = true;
        raw.replace_with_output_limit(effect)?;
        raw.terminal_error.get_or_insert(error);
        let mut replacement_round_output_budget = base_round_output_budget;
        let accepted_post_hook = (*budgets.post_hook_output).footprint();
        if !replacement_round_output_budget.try_release_post_hook(
            accepted_post_hook.content_blocks,
            accepted_post_hook.model_visible_bytes,
            accepted_post_hook.json_bytes,
        ) {
            return Err(AgentRunError::Internal {
                message: "PostHook 聚合预算回滚与已接纳占用不一致".to_owned(),
            });
        }
        if !replacement_round_output_budget.try_charge_result(raw.footprint) {
            return Err(AgentRunError::Internal {
                message: "工具 Round 连 PostHook 超限固定结果也无法容纳".to_owned(),
            });
        }
        *budgets.round_output = replacement_round_output_budget;
        *budgets.post_hook_output = PostHookOutputBudget::default();
        if !failure_hook_already_saw_fixed_result {
            let _ = run_post_tool_hook(request, prepared, raw, hooks).await;
        }
        return Ok(Vec::new());
    }

    *budgets.hook_context_bytes = candidate_hook_context_bytes;
    *budgets.post_hook_output = candidate_post_hook_output_budget;
    *budgets.round_output = candidate_round_output_budget;
    if let Some(error) = outcome.terminal_error {
        raw.terminal_error.get_or_insert(error);
    }
    Ok(outcome.context)
}

/// 为已经启动但未能正常完成的调用生成可审计的配对结果。
fn interrupted_tool_result(call: &PreparedCall, error: &AgentRunError) -> ToolResult {
    let call_id = match &call.disposition {
        PreparedDisposition::Immediate(result) => result.tool_call_id.clone(),
        PreparedDisposition::Execute { call, .. } => call.id.clone(),
    };
    let message = match error {
        AgentRunError::Cancelled => "工具调用因 Turn 取消而中止；请重新检查可能的外部副作用",
        AgentRunError::Model(_)
        | AgentRunError::Context(_)
        | AgentRunError::Hook(_)
        | AgentRunError::EventSink(_)
        | AgentRunError::CommitSink(_)
        | AgentRunError::ToolRoundPreflight(_)
        | AgentRunError::State(_)
        | AgentRunError::DynamicInput { .. }
        | AgentRunError::DynamicInputAcknowledgement { .. }
        | AgentRunError::DuplicateToolCallId { .. }
        | AgentRunError::ModelOutputLimit
        | AgentRunError::ModelRefusal
        | AgentRunError::InvalidResponse { .. }
        | AgentRunError::LimitReached { .. }
        | AgentRunError::ToolLoop { .. }
        | AgentRunError::ToolOutputLimit { .. }
        | AgentRunError::Internal { .. } => {
            "工具调用因 Runtime 错误而中止；请重新检查可能的外部副作用"
        }
    };
    ToolResult::text(call_id, message, true)
}

/// 在批次提前结束后为所有尚未启动的调用补齐同 ID 错误结果。
fn fill_unexecuted_results(
    pending: &[PreparedCall],
    results: &mut [Option<ToolResult>],
    error: &AgentRunError,
) {
    for call in pending {
        if results[call.index].is_none() {
            results[call.index] = Some(match &call.disposition {
                PreparedDisposition::Immediate(result) => result.clone(),
                PreparedDisposition::Execute { .. } => interrupted_tool_result(call, error),
            });
        }
    }
}

/// 在生命周期提交前接纳 Runtime 立即结果；异常或聚合超限时整体替换为固定只读失败。
fn normalize_immediate_round_result(
    result: ToolResult,
    budget: &mut ToolRoundOutputBudget,
) -> Result<ToolResult, AgentRunError> {
    if let Ok(footprint) = measure_tool_result(&result) {
        if budget.try_charge_result(footprint) {
            return Ok(result);
        }
    }
    let result = output_limit_result(&result.tool_call_id, ToolEffect::ReadOnly);
    let footprint = measure_tool_result(&result).map_err(|_| AgentRunError::Internal {
        message: "Runtime 固定立即超限结果无法通过自身硬上限".to_owned(),
    })?;
    if !budget.try_charge_result(footprint) {
        return Err(AgentRunError::Internal {
            message: "工具 Round 连固定立即超限结果也无法容纳".to_owned(),
        });
    }
    Ok(result)
}

/// 按模型原始调用顺序收集已经补齐的唯一工具结果。
fn collect_tool_results(
    results: Vec<Option<ToolResult>>,
) -> Result<Vec<ToolResult>, AgentRunError> {
    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.ok_or_else(|| AgentRunError::Internal {
                message: format!("工具批次第 {index} 项缺少配对结果"),
            })
        })
        .collect()
}

/// 将最终通过 Hook 校验的工具参数按 ID 写回模型响应中的权威 ToolCall。
fn rewrite_response_tool_calls(
    response: &mut ModelResponse,
    final_tool_calls: &[ToolCall],
) -> Result<(), AgentRunError> {
    let mut replacements = final_tool_calls.iter();
    for block in &mut response.content {
        let ContentBlock::ToolCall { tool_call } = block else {
            continue;
        };
        let replacement = replacements.next().ok_or_else(|| AgentRunError::Internal {
            message: "权威工具调用数量少于模型响应中的调用数量".to_owned(),
        })?;
        if tool_call.id != replacement.id || tool_call.name != replacement.name {
            return Err(AgentRunError::Internal {
                message: "权威工具调用与模型响应的 ID 或名称不一致".to_owned(),
            });
        }
        *tool_call = replacement.clone();
    }
    if replacements.next().is_some() {
        return Err(AgentRunError::Internal {
            message: "权威工具调用数量多于模型响应中的调用数量".to_owned(),
        });
    }
    Ok(())
}

/// 从 Turn 请求复制 Hook 可以观察但不能修改的稳定身份。
fn hook_invocation_context(request: &TurnRequest) -> HookInvocationContext {
    HookInvocationContext {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id.clone(),
        source_agent_id: request.source_agent_id.clone(),
    }
}

/// 执行一个已可靠提交起点的冻结调用，并在 PostHook 前返回真实工具结果。
async fn execute_one_raw(
    request: &TurnRequest,
    prepared: PreparedCall,
    execution_cancellation: TurnCancellation,
    cancel_grace: Duration,
) -> Result<RawExecutedTool, AgentRunError> {
    let PreparedDisposition::Execute {
        call,
        tool_call_id,
        tool,
        effect,
        fingerprint,
        ..
    } = prepared.disposition
    else {
        return Err(AgentRunError::Internal {
            message: "立即结果被错误送入工具执行器".to_owned(),
        });
    };
    let context = ToolContext {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id.clone(),
        source_agent_id: request.source_agent_id.clone(),
        tool_call_id,
        cancellation: execution_cancellation.child_token(),
    };
    let cancelled = Box::pin(execution_cancellation.cancelled());
    let executed = tool.execute(context, call.arguments);
    let (result, status, failure, terminal_error, observation) =
        match select(cancelled, executed).await {
            Either::Left(((), pending_tool)) => {
                let _ = tokio::time::timeout(cancel_grace, pending_tool).await;
                (
                    ToolResult::text(
                        call.id.clone(),
                        "工具调用因 Turn 取消而中止；请重新检查可能的外部副作用",
                        true,
                    ),
                    ToolCompletionStatus::Cancelled,
                    Some(ToolHookFailureKind::Cancelled),
                    Some(AgentRunError::Cancelled),
                    None,
                )
            }
            Either::Right((Ok(output), _)) => match validate_tool_output(call.id.clone(), output) {
                Ok((result, _)) => (
                    result,
                    ToolCompletionStatus::Succeeded,
                    None,
                    None,
                    Some(ToolExecutionObservation::Succeeded),
                ),
                Err(ToolOutputRejection::Invalid) => (
                    ToolResult::text(call.id.clone(), INVALID_TOOL_OUTPUT_RESULT, true),
                    ToolCompletionStatus::Failed,
                    Some(ToolHookFailureKind::InvalidOutput),
                    None,
                    Some(ToolExecutionObservation::Failed {
                        call: fingerprint.clone(),
                        error_code: INVALID_TOOL_OUTPUT_ERROR_CODE.to_owned(),
                    }),
                ),
                Err(ToolOutputRejection::LimitExceeded) => {
                    let code = output_limit_error_code(effect);
                    (
                        output_limit_result(&call.id, effect),
                        ToolCompletionStatus::Failed,
                        Some(ToolHookFailureKind::OutputLimitExceeded),
                        output_limit_terminal_error(effect),
                        Some(ToolExecutionObservation::Failed {
                            call: fingerprint.clone(),
                            error_code: code.as_str().to_owned(),
                        }),
                    )
                }
            },
            Either::Right((Err(error), _)) => {
                let error = normalize_tool_error(&error);
                let error_code = error.code.clone();
                (
                    normalized_tool_error_result(&call.id, &error),
                    ToolCompletionStatus::Failed,
                    Some(ToolHookFailureKind::ToolError),
                    None,
                    Some(ToolExecutionObservation::Failed {
                        call: fingerprint.clone(),
                        error_code,
                    }),
                )
            }
        };
    let footprint = measure_tool_result(&result).map_err(|_| AgentRunError::Internal {
        message: "Runtime 生成了无效或超过内部硬上限的工具结果".to_owned(),
    })?;
    Ok(RawExecutedTool {
        result,
        footprint,
        fingerprint,
        status,
        failure,
        terminal_error,
        observation,
    })
}

/// 在工具唯一终态提交前运行对应 PostHook，使容量失败仍可冻结最终结果和分类。
async fn run_post_tool_hook(
    request: &TurnRequest,
    prepared: &PreparedCall,
    executed: &RawExecutedTool,
    hooks: &HookRuntime,
) -> PostToolHookOutcome {
    let Some(call) = prepared.execution_call() else {
        return PostToolHookOutcome {
            context: Vec::new(),
            terminal_error: Some(AgentRunError::Internal {
                message: "已完成真实执行的工具缺少冻结调用信息".to_owned(),
            }),
        };
    };
    let invocation = hook_invocation_context(request);
    let hook_context = match executed.failure {
        Some(failure) => {
            hooks
                .run_post_tool_use_failure(
                    PostToolUseFailureContext {
                        invocation,
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        input: call.arguments.clone(),
                        result: executed.result.clone(),
                        failure,
                    },
                    &request.cancellation,
                )
                .await
        }
        None => {
            hooks
                .run_post_tool_use(
                    PostToolUseContext {
                        invocation,
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        input: call.arguments.clone(),
                        result: executed.result.clone(),
                    },
                    &request.cancellation,
                )
                .await
        }
    };
    let mut terminal_error = executed.terminal_error.clone();
    let context = match hook_context {
        Ok(context) => context,
        Err(error) => {
            terminal_error.get_or_insert_with(|| AgentRunError::from(error));
            Vec::new()
        }
    };
    PostToolHookOutcome {
        context,
        terminal_error,
    }
}

/// 一个尚未运行 PostHook 的真实工具结果与运行时观察。
struct RawExecutedTool {
    /// 与模型调用 ID 严格配对的最终结果。
    result: ToolResult,
    /// 最终归一结果进入 Round 聚合预算时复用的容量快照。
    footprint: ToolResultFootprint,
    /// 工具名称与最终输入摘要，用于聚合超限后替换成功观察。
    fingerprint: ToolCallFingerprint,
    /// 工具实现真实执行后的成功、失败或取消分类。
    status: ToolCompletionStatus,
    /// `Some` 表示应调用失败 PostHook，并携带稳定失败分类。
    failure: Option<ToolHookFailureKind>,
    /// Turn 取消时在提交结果后终止 Turn 的错误。
    terminal_error: Option<AgentRunError>,
    /// 仅由真实成功或 ToolError 更新重复失败计数。
    observation: Option<ToolExecutionObservation>,
}

impl RawExecutedTool {
    /// 在任何生命周期事件和 PostHook 前原子接纳结果，超限时整体替换为固定失败。
    fn enforce_round_budget(
        &mut self,
        effect: ToolEffect,
        budget: &mut ToolRoundOutputBudget,
    ) -> Result<(), AgentRunError> {
        if budget.try_charge_result(self.footprint) {
            return Ok(());
        }
        self.replace_with_output_limit(effect)?;
        if !budget.try_charge_result(self.footprint) {
            return Err(AgentRunError::Internal {
                message: "工具 Round 连固定超限结果也无法容纳".to_owned(),
            });
        }
        Ok(())
    }

    /// 把当前结果原子替换为不含原正文的固定超限失败，并同步所有下游分类。
    fn replace_with_output_limit(&mut self, effect: ToolEffect) -> Result<(), AgentRunError> {
        let code = output_limit_error_code(effect);
        self.result = output_limit_result(&self.result.tool_call_id, effect);
        self.footprint =
            measure_tool_result(&self.result).map_err(|_| AgentRunError::Internal {
                message: "Runtime 固定工具超限结果无法通过自身硬上限".to_owned(),
            })?;
        self.status = ToolCompletionStatus::Failed;
        self.failure = Some(ToolHookFailureKind::OutputLimitExceeded);
        self.observation = Some(ToolExecutionObservation::Failed {
            call: self.fingerprint.clone(),
            error_code: code.as_str().to_owned(),
        });
        if self.terminal_error.is_none() {
            self.terminal_error = output_limit_terminal_error(effect);
        }
        Ok(())
    }
}

/// 工具唯一终态提交前运行 PostHook 得到的上下文与延迟错误。
struct PostToolHookOutcome {
    /// 成功或失败 Hook 按注册顺序生成的上下文。
    context: Vec<ResolvedHookContext>,
    /// Hook 失败或先前 Turn 取消形成的最终终止错误。
    terminal_error: Option<AgentRunError>,
}

/// 从响应提取工具调用并拒绝重复调用 ID。
fn extract_tool_calls(
    response: &ModelResponse,
    seen_identifiers: &mut HashSet<String>,
) -> Result<Vec<ToolCall>, AgentRunError> {
    let mut response_identifiers = HashSet::new();
    let mut calls = Vec::new();
    for block in &response.content {
        if let ContentBlock::ToolCall { tool_call } = block {
            if seen_identifiers.contains(&tool_call.id)
                || !response_identifiers.insert(tool_call.id.clone())
            {
                return Err(AgentRunError::DuplicateToolCallId {
                    id: tool_call.id.clone(),
                });
            }
            calls.push(tool_call.clone());
        }
    }
    seen_identifiers.extend(response_identifiers);
    Ok(calls)
}

/// 把详细上下文错误收敛为不会暴露摘要或 Provider 正文的事件分类。
fn context_compaction_failure_kind(error: &ContextError) -> ContextCompactionFailureKind {
    match error {
        ContextError::CompressionFailed { .. } => ContextCompactionFailureKind::Model,
        ContextError::SummaryCallFailed { error, .. } => context_compaction_failure_kind(error),
        ContextError::NothingCompressible
        | ContextError::CompressionRequestTooLarge { .. }
        | ContextError::SummaryRecursionLimit
        | ContextError::CompressionDidNotReduce { .. }
        | ContextError::StillExceeded { .. } => ContextCompactionFailureKind::Budget,
        ContextError::InvalidPolicy { .. }
        | ContextError::EmptySummary
        | ContextError::RecursiveToolCall
        | ContextError::RecordMismatch { .. }
        | ContextError::Cancelled => ContextCompactionFailureKind::InvalidResult,
    }
}

/// 确保工具执行阶段只进入一次且不会跳过调度阶段。
fn enter_execution_phase(state: &mut TurnState) -> Result<(), AgentRunError> {
    match state.phase() {
        TurnPhase::SchedulingTools => state.transition_to(TurnPhase::ExecutingTools)?,
        TurnPhase::ExecutingTools => {}
        phase => {
            return Err(AgentRunError::Internal {
                message: format!("工具执行器收到非法 Turn 阶段 {phase:?}"),
            });
        }
    }
    Ok(())
}

/// 确认即将同时启动的一组 Step 都有剩余额度，但不提前增加实际计数。
fn ensure_step_capacity(
    state: &TurnState,
    maximum: u32,
    count: usize,
) -> Result<(), AgentRunError> {
    let count = u32::try_from(count).map_err(|_| AgentRunError::LimitReached {
        counter: CounterKind::Step,
        maximum,
    })?;
    let Some(required) = state.step_count().checked_add(count) else {
        return Err(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum,
        });
    };
    if required > maximum {
        return Err(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum,
        });
    }
    Ok(())
}

/// 在调用真正交给执行器前原子确认容量，并记录即将启动的实际 Step。
fn record_started_steps(
    state: &mut TurnState,
    maximum: u32,
    count: usize,
) -> Result<(), AgentRunError> {
    ensure_step_capacity(state, maximum, count)?;
    let count = u32::try_from(count).map_err(|_| AgentRunError::Internal {
        message: "已通过 Step 容量检查的启动数量无法转换为 u32".to_owned(),
    })?;
    for _ in 0..count {
        state.record_step()?;
    }
    Ok(())
}

/// 最终总结请求的 Provider、上下文或响应归约失败不能覆盖已确定的 Step 上限终态。
fn prefer_step_limit_summary_error(
    summary_error: Option<&AgentRunError>,
    error: AgentRunError,
) -> AgentRunError {
    if matches!(
        error,
        AgentRunError::Model(_)
            | AgentRunError::ModelOutputLimit
            | AgentRunError::ModelRefusal
            | AgentRunError::Context(_)
            | AgentRunError::EventSink(_)
            | AgentRunError::CommitSink(_)
            | AgentRunError::ToolRoundPreflight(_)
            | AgentRunError::DuplicateToolCallId { .. }
            | AgentRunError::InvalidResponse { .. }
    ) {
        summary_error.cloned().unwrap_or(error)
    } else {
        error
    }
}

/// 返回输入顺序稳定的 SHA-256 规范参数摘要。
fn canonical_input_hash(input: &Value) -> Result<ToolInputHash, AgentRunError> {
    let canonical = canonicalize_json(input);
    let bytes = serde_json::to_vec(&canonical).map_err(|error| AgentRunError::Internal {
        message: format!("工具输入无法序列化：{error}"),
    })?;
    let digest = Sha256::digest(bytes);
    let mut fixed = [0_u8; 32];
    fixed.copy_from_slice(&digest);
    Ok(ToolInputHash::from_bytes(fixed))
}

/// 递归排序对象键，避免输入 Hash 受 JSON Map 实现或插入顺序影响。
fn canonicalize_json(input: &Value) -> Value {
    match input {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                    .collect(),
            )
        }
        value => value.clone(),
    }
}

/// 把工具错误转换为模型可消费且与调用 ID 配对的结果。
fn tool_error_result(call_id: &str, error: &crate::ToolError) -> ToolResult {
    let error = normalize_tool_error(error);
    normalized_tool_error_result(call_id, &error)
}

/// 把已通过字段硬上限的工具错误转换为固定结构的配对结果。
fn normalized_tool_error_result(call_id: &str, error: &NormalizedToolError) -> ToolResult {
    let retryability = if error.retryable {
        "可有限重试"
    } else {
        "不可自动重试"
    };
    ToolResult::text(
        call_id,
        format!(
            "工具执行失败（{}；{retryability}）：{}",
            error.code, error.message
        ),
        true,
    )
}

/// 根据已冻结的副作用分类选择工具输出超限机器码。
const fn output_limit_error_code(effect: ToolEffect) -> ToolOutputErrorCode {
    match effect {
        ToolEffect::ReadOnly => ToolOutputErrorCode::LimitExceeded,
        ToolEffect::ChangesState => ToolOutputErrorCode::SideEffectLimitExceeded,
    }
}

/// 创建不含任何原输出前缀且明确不可自动重试的固定配对结果。
fn output_limit_result(call_id: &str, effect: ToolEffect) -> ToolResult {
    let text = match effect {
        ToolEffect::ReadOnly => TOOL_OUTPUT_LIMIT_RESULT,
        ToolEffect::ChangesState => SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT,
    };
    ToolResult::text(call_id, text, true)
}

/// 状态变更工具输出超限时终止 Turn；只读工具允许模型在下一 Round 改变策略。
const fn output_limit_terminal_error(effect: ToolEffect) -> Option<AgentRunError> {
    match effect {
        ToolEffect::ReadOnly => None,
        ToolEffect::ChangesState => Some(AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: None,
        }),
    }
}

/// 合并工具终态提交错误，并为副作用输出超限同时保留机器码与提交失败事实。
fn merge_tool_completion_error(
    terminal_error: Option<AgentRunError>,
    completion_error: AgentRunError,
) -> AgentRunError {
    match (terminal_error, completion_error) {
        (
            Some(AgentRunError::ToolOutputLimit {
                code,
                completion_commit_error: None,
            }),
            AgentRunError::CommitSink(error),
        ) => AgentRunError::ToolOutputLimit {
            code,
            completion_commit_error: Some(error),
        },
        (_, completion_error) => completion_error,
    }
}

/// 把权威提交错误转换为有界 Turn 错误，避免 Sink 尾部诊断泄漏或无界增长。
fn commit_sink_run_error(error: AgentCommitSinkError) -> AgentRunError {
    let message = truncate_utf8(error.message(), MAX_COMMIT_SINK_ERROR_MESSAGE_BYTES);
    let error = match error.kind() {
        AgentCommitSinkErrorKind::Rejected => AgentCommitSinkError::rejected(message),
        AgentCommitSinkErrorKind::Indeterminate => AgentCommitSinkError::indeterminate(message),
    };
    AgentRunError::CommitSink(error)
}

/// 把动态输入端口错误裁剪为不包含正文的稳定 Turn 错误。
fn dynamic_input_run_error(error: AgentDynamicInputError) -> AgentRunError {
    AgentRunError::DynamicInput {
        message: truncate_utf8(error.message(), MAX_COMMIT_SINK_ERROR_MESSAGE_BYTES),
    }
}

/// 把已提交 Transcript 后的动态输入确认错误转换为独立终态分类。
fn dynamic_input_acknowledgement_run_error(error: AgentDynamicInputError) -> AgentRunError {
    AgentRunError::DynamicInputAcknowledgement {
        message: truncate_utf8(error.message(), MAX_COMMIT_SINK_ERROR_MESSAGE_BYTES),
    }
}

/// 使用同一事件对象和稳定身份执行有界同步重投，并保留任何不确定提交分类。
fn commit_event_with_bounded_retry(
    sink: &dyn AgentCommitSink,
    event: &AgentCommitEvent,
    maximum_attempts: usize,
) -> Result<(), AgentCommitSinkError> {
    let mut latest_rejected = None;
    let mut latest_indeterminate = None;
    for _ in 0..maximum_attempts.max(1) {
        match sink.commit(event) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == AgentCommitSinkErrorKind::Indeterminate => {
                latest_indeterminate = Some(error);
            }
            Err(error) => latest_rejected = Some(error),
        }
    }
    Err(latest_indeterminate
        .or(latest_rejected)
        .expect("权威事件提交至少应执行一次"))
}

/// 保留预检失败分类并截断安全诊断，供 Turn 选择上下文阻塞或普通失败。
fn tool_round_preflight_run_error(error: AgentToolRoundPreflightError) -> AgentRunError {
    let message = truncate_utf8(error.message(), MAX_COMMIT_SINK_ERROR_MESSAGE_BYTES);
    let error = match error.kind() {
        AgentToolRoundPreflightErrorKind::Unpersistable => {
            AgentToolRoundPreflightError::unpersistable(message)
        }
        AgentToolRoundPreflightErrorKind::Unavailable => {
            AgentToolRoundPreflightError::unavailable(message)
        }
    };
    AgentRunError::ToolRoundPreflight(error)
}

/// 在进入任何新的模型或工具工作前检查取消状态。
fn ensure_not_cancelled(cancellation: &TurnCancellation) -> Result<(), AgentRunError> {
    if cancellation.is_cancelled() {
        Err(AgentRunError::Cancelled)
    } else {
        Ok(())
    }
}

/// 在 UTF-8 字符边界上截断诊断文本，保证结果不超过指定字节数。
fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}
