//! KeenCode Agent 运行时的纯领域状态机。
//!
//! 本 crate 只描述 Turn、计划守卫和单层 Agent 的确定性规则，
//! 不依赖模型协议、桌面框架、存储或具体工具实现。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod agent;
mod cancellation;
mod collaboration;
mod context;
mod event;
mod hook;
mod ids;
mod plan_guard;
mod runner;
mod state;
mod structured_output;
mod tool;
mod turn;

pub use agent::{AgentDepth, AgentDepthError, AgentStatus, MailboxDelivery};
pub use cancellation::TurnCancellation;
pub use collaboration::{
    AgentCapabilities, AgentDefinition, AgentExecutionPort, AgentHandle, AgentPath, AgentPathError,
    AgentProfile, AgentTemplateSnapshot, AgentTreeQuiesceResult, AgentTurnCause, AgentTurnLaunch,
    AgentTurnOutcome, AgentTurnSignal, AgentTurnSignalKind, AgentTurnStartResult, CloseAgentTree,
    CollaborationAgentStatus, CollaborationAgentSummary, CollaborationAppendResult,
    CollaborationCapacity, CollaborationCoordinator, CollaborationError, CollaborationEvent,
    CollaborationEventBatch, CollaborationEventBatchId, CollaborationEventKind,
    CollaborationIdGenerator, CollaborationInvocationInput, CollaborationInvocationKey,
    CollaborationInvocationKind, CollaborationInvocationOutput, CollaborationInvocationReceipt,
    CollaborationLimits, CollaborationPortError, CollaborationStore, CollaborationTransitionCommit,
    ContextInheritance, MailboxActivitySummary, MailboxMessage, MailboxMessageId,
    MailboxMessageKind, QuiesceAgentTree, RecoveredAgent, RecoveredAgentCheckpoint,
    RecoveredAgentTree, RecoveredCollaborationInvocation, RecoveredCoordinator,
    RecoveredMailboxMessage, RecoveredRootLifecycle, RecoveredTurn, RootAgentRequest,
    SpawnAgentRequest, SpawnedAgent, TurnCancellationDisposition, TurnCompletionDisposition,
    UserSteer, UserSteerSummary, UuidCollaborationIdGenerator, WaitAgentOutcome, WorktreeLease,
    root_turn_prompt_digest,
};
pub use context::{
    ContextCompressionOutcome, ContextCompressionRecord, ContextCompressionTrigger,
    ContextCompressor, ContextError, ContextFuture, ContextManager, ContextPolicy,
    ContextSummaryCallResult, ContextSummaryModelUsage, ContextSummaryOutcome,
    ContextSummaryRequest, ContextTokenEstimator, JsonContextTokenEstimator,
    ProviderContextCompressor,
};
pub use event::{
    AgentCommitEvent, AgentCommitEventKind, AgentCommitSink, AgentCommitSinkError,
    AgentCommitSinkErrorKind, AgentDynamicInputBoundary, AgentDynamicInputKind,
    AgentDynamicInputReceipt, AgentEventDeliveryError, AgentEventFuture, AgentEventSink,
    AgentEventSinkError, AgentStreamEvent, AgentStreamEventKind, AgentToolRoundPreflight,
    AgentToolRoundPreflightError, AgentToolRoundPreflightErrorKind, AgentToolRoundReservation,
    ContextCompactionFailureKind, ModelCallPurpose, ModelRoundCompletion, ModelRoundUsage,
    NoopAgentCommitSink, NoopAgentEventSink, ToolCompletionStatus,
};
pub use hook::{
    AgentHook, HookCallbackError, HookContextAddition, HookError, HookFuture,
    HookInvocationContext, HookLimits, HookLimitsError, HookPhase, HookRegistrationError,
    HookRegistry, HookRuntime, PostToolUseContext, PostToolUseFailureContext, PreToolUseAction,
    PreToolUseContext, PreToolUseOutput, StopHookAction, StopHookContext, StopHookOutput,
    ToolHookFailureKind, ToolHookOutput,
};
pub(crate) use hook::{PostHookOutputBudget, ResolvedHookContext, ResolvedStopHook};
pub use ids::{
    AgentEventId, AgentId, IdentifierError, MAX_TOOL_CALL_ID_BYTES, SessionId, ToolCallId, TurnId,
};
pub use plan_guard::{PlanGuard, PlanGuardError, PlanGuardState, ToolEffect, ToolInputHash};
pub use runner::{
    AgentDynamicInputAcknowledgement, AgentDynamicInputBatch, AgentDynamicInputError,
    AgentDynamicInputSource, AgentRunError, AgentRunner, RunLimits, RunLimitsError, ToolLoopKind,
    TurnRequest, TurnResult,
};
pub use state::{
    GoalChange, GoalChangeKind, GoalController, GoalDraft, GoalPatch, GoalRecord, GoalSnapshot,
    GoalStatus, GoalTransition, GoalUsageDelta, InMemoryRuntimeState, MAX_GOAL_EVIDENCE_CHARS,
    MAX_PLAN_CONTENT_CHARS, PlanChange, PlanController, PlanSnapshot, RuntimeStateError,
    TodoChange, TodoController, TodoItem, TodoSnapshot, TodoStatus,
};
pub use structured_output::{STRUCTURED_OUTPUT_TOOL_NAME, StructuredOutputMode};
pub use tool::{
    AgentTool, TOOL_OUTPUT_LIMITS, ToolConcurrency, ToolContext, ToolError, ToolFuture, ToolOutput,
    ToolOutputErrorCode, ToolOutputLimits, ToolRegistry, ToolRegistryError,
    is_canonical_image_media_type, is_canonical_remote_image_url,
};
pub use turn::{CounterKind, TerminalReason, TurnPhase, TurnState, TurnTransitionError};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod collaboration_tests;

#[cfg(test)]
mod context_tests;

#[cfg(test)]
mod hook_tests;

#[cfg(test)]
mod stream_tests;
