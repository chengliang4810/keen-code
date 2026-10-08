//! 自研 Agent Runtime 的桌面生产装配根。
mod automatic_titles;
mod file_changes;
mod interruption_context;
mod workflow_actor;
mod workflow_tool_executor;
#[cfg(test)]
pub(crate) use file_changes::RuntimeFileMutationRecorder;
pub(crate) use workflow_actor::{WorkflowActor, WorkflowActorRequest};

#[cfg(test)]
mod live_prompt_tests;

use crate::{
    analytics::{AnalyticsRecorder, ModelRetryNotice},
    app_settings::DEFAULT_BACKGROUND_AGENT_LIMIT,
    client_request::ClientRequestDisplayGate,
    elicitation::{ElicitationCoordinator, PendingElicitationView},
    memories::MemoryService,
    permissions::{PendingPermissionView, PermissionCoordinator, PermissionMode},
    storage,
    workflows::agent_tools::WorkflowToolPort,
};
use anyhow::{Context, anyhow, bail};
use chrono::{SecondsFormat, TimeZone, Utc};
use keencode_acp::{BackgroundTaskInfo, BackgroundTaskKind, ConnectionId};
use keencode_agent::{
    AgentCapabilities, AgentCommitSinkError, AgentDepth, AgentDynamicInputAcknowledgement,
    AgentDynamicInputBatch, AgentDynamicInputBoundary, AgentDynamicInputError,
    AgentDynamicInputKind, AgentDynamicInputReceipt, AgentDynamicInputSource, AgentExecutionPort,
    AgentId as RunnerAgentId, AgentProfile, AgentRunError, AgentRunner, AgentTemplateSnapshot,
    AgentTreeQuiesceResult, AgentTurnCause, AgentTurnLaunch, AgentTurnOutcome, AgentTurnSignal,
    AgentTurnStartResult, CloseAgentTree, CollaborationAgentStatus, CollaborationAgentSummary,
    CollaborationAppendResult, CollaborationCoordinator, CollaborationError, CollaborationEvent,
    CollaborationEventKind, CollaborationGlobalTurnLimiter, CollaborationGlobalTurnPermit,
    CollaborationPortError, CollaborationStore, CollaborationTransitionCommit, ContextManager,
    ContextPolicy, ContextTokenEstimator, GoalChange, GoalController, GoalDraft, GoalPatch,
    GoalStatus, GoalTransition, GoalUsageDelta, HookPhase, HookRuntime, JsonContextTokenEstimator,
    MailboxMessage as RunnerMailboxMessage, MailboxMessageKind, ModelRoundUsage, PlanGuard,
    PlanGuardState, ProviderContextCompressor, QuiesceAgentTree, RecoveredAgent,
    RecoveredAgentCheckpoint, RecoveredCoordinator, RecoveredRootLifecycle, RootAgentRequest,
    RunLimits, RuntimeStateError, SessionId as AgentSessionId, StructuredOutputMode,
    TerminalReason, ToolCallId, ToolRegistry, TurnCancellation, TurnCancellationDisposition,
    TurnId as AgentTurnId, TurnRequest, UuidCollaborationIdGenerator, root_turn_prompt_digest,
};
use keencode_model::{
    ContentBlock, ImageContent, Message, MessageRole, ModelFuture, ModelMessages, ModelProvider,
    ModelRequest, ModelStream, ProviderCapabilities, ProviderProtocol, ReasoningConfig,
    ReasoningEffort, StructuredOutputConfig, ToolChoice, last_non_empty_text,
};
use keencode_provider::{
    ProviderRegistry, REQUEST_METADATA_AGENT_ID, REQUEST_METADATA_PROMPT_CACHE_KEY,
    REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID, REQUEST_METADATA_TURN_ID,
    ResolvedProvider,
};
use keencode_resources::{
    AgentId as ResourceAgentId, COMPACTION_SUMMARY_PREFIX,
    DynamicInputKind as ResourceDynamicInputKind, FollowupMode,
    MailboxMessage as ResourceMailboxMessage, MailboxMessageId as ResourceMailboxMessageId,
    MailboxState, MessagePart as ResourceMessagePart, MessageRole as ResourceMessageRole,
    ProviderProtocolSnapshot, ProviderSnapshot, ReasoningEffortSnapshot, SessionEvent,
    SessionInputQueueItem, SessionInputQueueState, SessionMessage, SessionPermissionMode,
    SessionState, SubAgentState, SubAgentStatus, TranscriptRecord, TranscriptSegment,
    TurnId as ResourceTurnId, TurnStatus,
};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, PersistentAgentState, RuntimeConfig, RuntimeError,
    RuntimeEventPayload, RuntimeEventSubscription, RuntimeManager, RuntimeModelRetryScheduled,
    RuntimeModelRoundUsageSink, RuntimeSession, RuntimeSnapshot, RuntimeTurnRequest,
    StoredSessionMetadata, TurnCancellationOutcome, UnstartedTurnTermination,
    UnstartedTurnTerminationRequest,
};
use keencode_tools::{
    AskUserTool, BackgroundTaskCompletion, BackgroundTaskManager, BackgroundTaskStatus, BashTool,
    CompletedTurnContext, EditTool, GitWorktreeLeaseManager, ReadTool, ResolvedSpawnAgentTemplate,
    SpawnAgentContextSource, SpawnAgentTemplateContext, SpawnAgentTemplateResolver,
    ToolEnvironment, UserQuestionHandler, WebServiceConfig, WriteTool,
    finalize_child_agent_tool_snapshot, register_collaboration_tools,
    register_collaboration_tools_with_template_resolver, register_deferred_tools,
    register_local_tools_with_background, register_state_tools, register_web_tools,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;
use url::Url;

/// 独立标题请求的硬超时，避免单飞锁被异常端点永久占用。
const TITLE_GENERATION_TIMEOUT_SECS: u64 = 60;
/// 自动标题允许的最大 Unicode 标量数量；与前端展示上限保持一致。
const GENERATED_TITLE_MAX_CHARS: usize = 36;
/// Collaboration v2 原子提交文件使用的唯一 Schema。
const COLLABORATION_TRANSITION_SCHEMA: &str = "keencode/session/collaboration-transition";
/// Collaboration v2 局部 Agent checkpoint 文件使用的唯一 Schema。
const COLLABORATION_AGENT_SCHEMA: &str = "keencode/session/collaboration-agent-checkpoint";
/// Collaboration v2 原子提交文件当前唯一版本。
const COLLABORATION_TRANSITION_VERSION: u32 = 2;
/// 单个完整协调器提交文件允许读取的最大字节数。
const MAX_COLLABORATION_TRANSITION_FILE_BYTES: u64 = 1280 * 1024 * 1024;
/// 单个局部 Agent checkpoint 文件允许读取的最大字节数。
const MAX_COLLABORATION_AGENT_FILE_BYTES: u64 = 128 * 1024 * 1024;
/// 动态输入正文首行使用的可恢复水位 Schema。
const DYNAMIC_INPUT_MARKER_SCHEMA: &str = "keencode/dynamic-input/v1";
/// 每个后台命令增量读取允许返回的最大字节数。
const BACKGROUND_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
/// 全树关闭等待 Agent Turn 收敛的同步上限。
const AGENT_TREE_QUIESCE_TIMEOUT: Duration = Duration::from_secs(30);
/// 单条终态回传最多尝试八次（包含首次提交），避免 Store 故障永久占用执行状态。
const RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS: usize = 8;
/// 终态回传重试的总时间上限；超时后保留持久文件供冷恢复。
const RUNTIME_TURN_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);
/// 执行失败进入协作终态时允许保留的最大 UTF-8 字节数。
const MAX_COLLABORATION_FAILURE_BYTES: usize = 64 * 1024;
/// 终态错误投影进入 ACP/UI 事件时允许保留的最大 UTF-8 字节数。
/// 单条扩展诊断日志允许保留的最大 UTF-8 字节数。
const MAX_EXTENSION_DIAGNOSTIC_BYTES: usize = 4 * 1024;
/// 协调器提交文件允许累积保留的等待容量取消证据数量。
const MAX_UNSTARTED_TURN_TERMINATION_RECORDS: usize = 4_096;
/// 原生隔离验收默认沿用资源层的每 Session Artifact 数量上限。
const NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT: usize = 1_024;
/// 原生隔离验收允许的 Artifact 数量覆盖环境变量。
const NATIVE_TEST_ARTIFACT_CAPACITY_ENV: &str = "KEENCODE_NATIVE_TEST_MAX_ARTIFACTS_PER_SESSION";
/// 原生隔离评测覆盖值的最小合法数量。
const NATIVE_TEST_ARTIFACT_CAPACITY_MIN: usize = 1;
/// 原生隔离评测覆盖值的最大合法数量，防止测试环境意外制造无界资源压力。
const NATIVE_TEST_ARTIFACT_CAPACITY_MAX: usize = 8_192;
/// 非法覆盖值的固定错误；不能把环境变量原文写入日志或桌面错误。
const NATIVE_TEST_ARTIFACT_CAPACITY_ERROR: &str =
    "KEENCODE_NATIVE_TEST_MAX_ARTIFACTS_PER_SESSION must be an integer in 1..=8192";

/// 解析原生隔离验收的 Artifact 容量覆盖。
///
/// 生产构建或非 benchmark 进程即使继承了同名环境变量，也必须继续使用
/// ArtifactStore 的默认上限。只有两个门控同时成立时，非法值才会 fail-closed；
/// 解析错误只返回固定说明，避免把环境变量内容带入日志或用户界面。
fn native_test_artifact_capacity(
    native_acceptance_enabled: bool,
    benchmark_enabled: bool,
    override_value: Option<&std::ffi::OsStr>,
) -> Result<usize, &'static str> {
    if !native_acceptance_enabled || !benchmark_enabled {
        return Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT);
    }
    let Some(override_value) = override_value else {
        return Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT);
    };
    let value = override_value
        .to_str()
        .ok_or(NATIVE_TEST_ARTIFACT_CAPACITY_ERROR)?;
    let capacity = value
        .parse::<usize>()
        .map_err(|_| NATIVE_TEST_ARTIFACT_CAPACITY_ERROR)?;
    if !(NATIVE_TEST_ARTIFACT_CAPACITY_MIN..=NATIVE_TEST_ARTIFACT_CAPACITY_MAX).contains(&capacity)
    {
        return Err(NATIVE_TEST_ARTIFACT_CAPACITY_ERROR);
    }
    Ok(capacity)
}

#[cfg(test)]
mod native_test_artifact_capacity_tests {
    use super::*;

    #[test]
    fn default_is_used_without_override_or_when_any_gate_is_closed() {
        assert_eq!(
            native_test_artifact_capacity(true, true, None),
            Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT)
        );
        assert_eq!(
            native_test_artifact_capacity(false, true, Some(std::ffi::OsStr::new("2048")),),
            Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT)
        );
        assert_eq!(
            native_test_artifact_capacity(true, false, Some(std::ffi::OsStr::new("2048")),),
            Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT)
        );
        assert_eq!(
            native_test_artifact_capacity(false, false, Some(std::ffi::OsStr::new("not-a-number")),),
            Ok(NATIVE_TEST_ARTIFACT_CAPACITY_DEFAULT)
        );
    }

    #[test]
    fn valid_override_is_a_strict_usize_inclusive_range() {
        for (raw, expected) in [("1", 1usize), ("1024", 1024), ("8192", 8192)] {
            assert_eq!(
                native_test_artifact_capacity(true, true, Some(std::ffi::OsStr::new(raw)),),
                Ok(expected)
            );
        }
    }

    #[test]
    fn invalid_override_fails_closed_without_echoing_value() {
        for raw in [
            "",
            "0",
            "8193",
            "-1",
            "1.0",
            "not-a-number",
            "999999999999999999999999",
        ] {
            let error = native_test_artifact_capacity(true, true, Some(std::ffi::OsStr::new(raw)))
                .expect_err("非法覆盖值必须拒绝");
            assert_eq!(error, NATIVE_TEST_ARTIFACT_CAPACITY_ERROR);
            if !raw.is_empty() {
                assert!(!error.contains(raw));
            }
        }
    }
}

/// 单次后台任务取消请求在 Runtime 中观察到的真实结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundTaskCancellationOutcome {
    /// 本次请求首次发出取消信号。
    Requested,
    /// 任务已经收到过取消信号，本次没有重复发出。
    AlreadyRequested,
    /// 任务在本次请求时已经不再运行，未发出取消信号。
    NotRunning,
}

/// 自研 Runtime 生产装配或 Provider 热加载失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentRuntimeError {
    /// 无法创建本地 Runtime 或读取当前配置。
    InitializationFailed,
    /// Session 标识不满足当前资源层约束。
    InvalidSession,
    /// Runtime 已永久关闭。
    RuntimeClosed,
    /// Provider 配置不能原子替换到当前注册表。
    ProviderReloadFailed,
    /// 当前配置没有同时选择可解析的 Provider 与模型。
    ProviderNotConfigured,
    /// 请求打开的 Session 不存在或其权威日志已经损坏。
    SessionUnavailable,
    /// 请求项目与 Session 创建时持久绑定的项目根不一致。
    SessionProjectMismatch,
    /// Session Runtime 控制面操作失败。
    RuntimeOperationFailed,
    /// 请求恢复的后台子 Agent 身份或生命周期状态不符合当前恢复契约。
    InvalidResumeTarget,
    /// 全局或项目指令损坏、不可读或超过自动注入预算。
    InstructionsUnavailable,
    /// Journal 与 Collaboration 的冷恢复事实无法证明属于同一条 Turn 谱系。
    RecoveryRequired,
    /// Client Response 不属于任何已登记的请求路由。
    UnknownClientRequest,
    /// Client Response 未通过对应路由的严格校验。
    ClientResponseRejected,
    /// Runtime 内部共享状态不可用。
    StateUnavailable,
}

// 保留错误归一调用点，原生链路失败时可定位具体 Runtime 操作而非总指向日志助手。
#[track_caller]
fn runtime_operation_failed(error: impl fmt::Display) -> AgentRuntimeError {
    let source = std::panic::Location::caller();
    tracing::error!(
        error = %format_args!("{error:#}"),
        source_line = source.line(),
        source_column = source.column(),
        source = %source,
        "Runtime operation failed"
    );
    AgentRuntimeError::RuntimeOperationFailed
}

/// 将后台子 Agent 当前不可恢复的领域状态归一为稳定公开错误。
#[track_caller]
fn map_resume_collaboration_error(error: CollaborationError) -> AgentRuntimeError {
    match error {
        CollaborationError::AgentNotFound { .. }
        | CollaborationError::CrossTreeOperation
        | CollaborationError::RetryNotAllowed { .. }
        | CollaborationError::TargetNotIdle { .. }
        | CollaborationError::TargetStopped { .. }
        | CollaborationError::TreeClosed { .. } => AgentRuntimeError::InvalidResumeTarget,
        error => runtime_operation_failed(error),
    }
}

/// 保留生产装配失败原因，避免转换成稳定枚举时丢失诊断证据。
#[track_caller]
fn initialization_failed(context: &'static str, error: impl fmt::Display) -> AgentRuntimeError {
    tracing::error!(context, error = %format_args!("{error:#}"), source = %std::panic::Location::caller(), "Agent Runtime initialization failed");
    AgentRuntimeError::InitializationFailed
}

/// 保留 Provider 热加载失败原因，避免转换成稳定枚举时丢失诊断证据。
#[track_caller]
fn provider_reload_failed(error: impl fmt::Display) -> AgentRuntimeError {
    tracing::error!(error = %format_args!("{error:#}"), source = %std::panic::Location::caller(), "Provider reload failed");
    AgentRuntimeError::ProviderReloadFailed
}

impl fmt::Display for AgentRuntimeError {
    /// 输出不包含事件正文、工具输入或 Provider 凭据的稳定说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitializationFailed => formatter.write_str("Agent Runtime 初始化失败"),
            Self::InvalidSession => formatter.write_str("Session 标识无效"),
            Self::RuntimeClosed => formatter.write_str("Agent Runtime 已关闭"),
            Self::ProviderReloadFailed => formatter.write_str("Provider 热加载失败"),
            Self::ProviderNotConfigured => formatter.write_str("当前没有可用的默认模型"),
            Self::SessionUnavailable => formatter.write_str("Session 不存在或不可恢复"),
            Self::SessionProjectMismatch => formatter.write_str("Session 不属于当前项目"),
            Self::RuntimeOperationFailed => formatter.write_str("Session Runtime 操作失败"),
            Self::InvalidResumeTarget => formatter.write_str("后台子 Agent 当前不能恢复"),
            Self::InstructionsUnavailable => {
                formatter.write_str("无法加载全局或项目指令文件：请检查文件类型、UTF-8 编码和大小")
            }
            Self::RecoveryRequired => formatter.write_str("Session 需要恢复后才能继续协作运行"),
            Self::UnknownClientRequest => formatter.write_str("Client Request 不存在"),
            Self::ClientResponseRejected => formatter.write_str("Client Response 无效"),
            Self::StateUnavailable => formatter.write_str("Agent Runtime 状态不可用"),
        }
    }
}

/// 当前 Provider 注册表代次绑定的默认模型选择。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DefaultProviderBinding {
    /// Provider 注册表中的稳定标识。
    provider_id: String,
    /// Provider 明确允许的精确模型标识。
    model: String,
    /// 选择完成时对应的注册表代次。
    generation: u64,
}

/// App 级运行时偏好；只保留当前本地 Runtime 的有效快照，不启动空闲 Session。
///
/// ZCode 的 CLI 会把同一偏好同步到活动 workspace。桌面 Runtime 没有外部 CLI，
/// 因此由这份受 Runtime 所有的快照承接后续 Elicitation 策略读取。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppRuntimePreferences {
    /// AskUser 自动解析/倒计时能力是否启用。
    pub ask_user_question_auto_resolution_enabled: bool,
}

impl Default for AppRuntimePreferences {
    fn default() -> Self {
        Self {
            ask_user_question_auto_resolution_enabled: true,
        }
    }
}

/// 跨扩展候选共享的一次性生命周期启动状态。
///
/// `started` 是会话级一次性占位，覆盖已完成阶段和仍在等待确认的预留；`active`
/// 记录当前未确认回调的 Turn/token。两套状态必须共享，避免热重载创建新 Hook
/// 候选后，旧 worker 的迟到完成或回滚误触碰新候选的启动记录。
#[derive(Clone)]
pub(crate) struct LifecycleStartState {
    started: Arc<Mutex<HashSet<(String, HookPhase)>>>,
    active: Arc<Mutex<HashMap<(String, HookPhase), LifecycleStartRecord>>>,
    next_token: Arc<AtomicU64>,
}

#[derive(Clone)]
struct LifecycleStartRecord {
    turn_id: String,
    token: u64,
    claimed: bool,
    callback_completed: bool,
}

impl LifecycleStartState {
    pub(crate) fn new() -> Self {
        Self {
            started: Arc::new(Mutex::new(HashSet::new())),
            active: Arc::new(Mutex::new(HashMap::new())),
            next_token: Arc::new(AtomicU64::new(1)),
        }
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> Arc<Mutex<HashSet<(String, HookPhase)>>> {
        Arc::clone(&self.started)
    }

    pub(crate) fn reserve(&self, key: (String, HookPhase), turn_id: String) -> bool {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if started.contains(&key) || active.contains_key(&key) {
            return false;
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        started.insert(key.clone());
        active.insert(
            key,
            LifecycleStartRecord {
                turn_id,
                token,
                claimed: false,
                callback_completed: false,
            },
        );
        true
    }

    pub(crate) fn claim(
        &self,
        key: (String, HookPhase),
        turn_id: String,
    ) -> Option<LifecycleStartAttempt> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = active.get_mut(&key)?;
        if record.turn_id != turn_id || record.claimed {
            return None;
        }
        record.claimed = true;
        let token = record.token;
        Some(LifecycleStartAttempt {
            state: self.clone(),
            key,
            turn_id,
            token,
            callback_completed: false,
        })
    }

    fn callback_completed(&self, key: &(String, HookPhase), turn_id: &str, token: u64) -> bool {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = active.get_mut(key) else {
            return false;
        };
        if record.turn_id != turn_id || record.token != token {
            return false;
        }
        record.callback_completed = true;
        true
    }

    pub(crate) fn deliver(&self, key: &(String, HookPhase), turn_id: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = active.get(key) else {
            return;
        };
        if record.turn_id == turn_id && record.callback_completed {
            active.remove(key);
        }
    }

    pub(crate) fn abort(&self, key: &(String, HookPhase), turn_id: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = active.get(key) else {
            return;
        };
        if record.turn_id != turn_id {
            return;
        }
        active.remove(key);
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        started.remove(key);
    }

    /// 显式 Prompt 阻断结束当前回调时，保留已经完成的启动阶段。
    pub(crate) fn abort_preserving_completed_start(
        &self,
        key: &(String, HookPhase),
        turn_id: &str,
    ) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = active.get(key) else {
            return;
        };
        if record.turn_id != turn_id {
            return;
        }
        let callback_completed = record.callback_completed;
        active.remove(key);
        if callback_completed {
            return;
        }
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        started.remove(key);
    }

    fn rollback(&self, key: &(String, HookPhase), turn_id: &str, token: u64) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = active.get(key) else {
            return;
        };
        if record.turn_id != turn_id || record.token != token {
            return;
        }
        active.remove(key);
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        started.remove(key);
    }
}

/// 生命周期 Hook 回调对一次启动阶段的临时所有权。
pub(crate) struct LifecycleStartAttempt {
    state: LifecycleStartState,
    key: (String, HookPhase),
    turn_id: String,
    token: u64,
    callback_completed: bool,
}

impl LifecycleStartAttempt {
    pub(crate) fn callback_succeeded(&mut self) {
        self.callback_completed =
            self.state
                .callback_completed(&self.key, &self.turn_id, self.token);
    }
}

impl Drop for LifecycleStartAttempt {
    fn drop(&mut self) {
        if !self.callback_completed {
            self.state.rollback(&self.key, &self.turn_id, self.token);
        }
    }
}

/// 启动根 Turn 时由命令层显式传入、只在模型请求期装配的行为上下文。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RootTurnOptions {
    /// 用户在原输入框显式选择且经宿主校验的资源身份，与正文分开进入同一权威消息。
    pub references: Vec<keencode_model::InputReference>,
    /// 已由宿主读取并校验的图片；Runtime 会在 provider 不支持视觉时拒绝本轮。
    pub attachment_images: Vec<ImageContent>,
    /// 已由宿主读取的 UTF-8 文本附件，作为隐藏用户上下文参与本轮请求。
    pub attachment_context: Vec<String>,
    /// Memory、Plan 或 Ultra 等本轮背景；以 is_meta 用户消息放在历史之前，不写入 Session Transcript。
    pub developer_context: Option<String>,
    /// 本轮开始前必须原子写入 Session 快照的 Plan 模式状态。
    pub plan_enabled: bool,
    /// 本轮 typed 问答唯一允许响应的 Native 连接。
    pub elicitation_connection_id: Option<ConnectionId>,
}

/// 仅供运行时内部使用的续跑身份，不能由 ACP 用户输入伪造。
#[derive(Clone, Debug)]
struct GoalContinuation {
    goal_id: String,
    iteration: u32,
}

/// 完成校验只给出下一轮调度建议，不代替 Goal 工具的证据写入。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GoalVerification {
    passed: bool,
    reason: String,
    next_action: String,
}

/// 根 Turn 启动屏障完成后的精确幂等结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootTurnStartOutcome {
    /// 本次调用创建并确认了新的权威 TurnStarted。
    Started,
    /// 相同 Turn 标识与输入已经存在，本次没有重复启动。
    Deduplicated,
}

/// 扩展候选构建工具、Hook 和 Agent 模板时使用的可信 Session 上下文。
#[derive(Clone)]
pub struct RuntimeToolContext {
    /// 冻结的子代理模板名称，供启动 Hook 匹配。
    pub(crate) agent_type: String,
    /// 当前根 Session 标识。
    session_id: String,
    /// 当前 Session 创建时绑定的规范项目根。
    project_root: PathBuf,
    /// 当前 Turn 冻结的 Plan 只读守卫。
    plan_guard: PlanGuard,
    /// 会话存活期间共享各代理启动记录与未确认的生命周期租约。
    lifecycle_start_state: LifecycleStartState,
}

/// 扩展候选在装配时产生、仅写入日志的安全诊断。
///
/// 诊断只允许携带已经由具体扩展实现清理和截断的标识、分类与说明；
/// Runtime 不接受原始 Provider、MCP 或 LSP 输出，也不把诊断写入模型 Transcript。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeExtensionDiagnostic {
    /// 产生诊断的扩展类型，例如 `mcp` 或 `lsp`。
    pub source: String,
    /// 扩展 Server 的安全稳定名称。
    pub server: String,
    /// 供日志和客户端诊断定位的稳定分类码。
    pub code: String,
    /// 可直接展示的有界说明。
    pub message: String,
    /// 可选的远端工具名称；LSP 或 Server 级故障没有该值。
    pub tool: Option<String>,
}

/// 一个已经完成扩展候选构建的 MCP Server 运行态快照。
///
/// 该快照只包含传输、连接状态、可用工具数量和安全错误摘要；它不保存
/// 命令参数、HTTP Header、访问令牌或任何其他 MCP 配置正文。`enabled`
/// 由 Host 结合当前配置读取，避免候选代次中的旧配置覆盖最新启用状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMcpServerSnapshot {
    /// MCP Server 的稳定运行时名称。
    pub name: String,
    /// 当前候选使用的 MCP 传输类型。
    pub transport: keencode_acp::McpTransportKind,
    /// 当前候选观察到的连接生命周期状态。
    pub connection_status: keencode_acp::McpConnectionStatus,
    /// 当前候选发现并保留的可用工具数量。
    pub tools_count: u32,
    /// 当前候选可证明的 OAuth 生命周期状态。
    pub oauth_status: keencode_acp::McpOAuthStatus,
    /// 连接或工具发现失败时的安全错误摘要。
    pub error: Option<String>,
}

impl RuntimeToolContext {
    pub(crate) fn lifecycle_start_state(&self) -> LifecycleStartState {
        self.lifecycle_start_state.clone()
    }

    /// 返回当前根 Session 标识。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 返回当前 Session 的规范项目根。
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// 返回当前 Turn 冻结的 Plan 只读守卫。
    pub const fn plan_guard(&self) -> PlanGuard {
        self.plan_guard
    }
}

/// 扩展 Agent 模板解析时允许继承的父 Agent 上下文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeAgentTemplateContext {
    /// 当前根 Session 标识。
    pub session_id: String,
    /// 直接父 Agent 标识。
    pub parent_agent_id: String,
    /// 当前 Agent 树的根 Turn 标识。
    pub root_turn_id: String,
}

/// 扩展提供且不携带任何旧私有协议类型的 Agent 模板。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeAgentTemplate {
    /// 模板稳定名称。
    pub name: String,
    /// 是否把项目级 AGENTS/CLAUDE 指令加入子 Agent 的冻结提示词；全局指令始终保留。
    pub inject_agents_md: bool,
    /// 追加到 KeenCode 基础提示之后的系统说明。
    pub system_prompt: String,
    /// 可选的精确模型覆盖。
    pub model: Option<String>,
    /// 可选的推理强度覆盖。
    pub reasoning_effort: Option<String>,
    /// 模板允许暴露的统一工具名称；`None` 表示继承父工具快照。
    pub tool_names: Option<Vec<String>>,
    /// 从继承或显式工具集合中移除的统一工具名称。
    pub disallowed_tool_names: Vec<String>,
    /// 模板允许执行的最大模型轮数；为空时使用 Runtime 默认上限。
    pub max_turns: Option<u32>,
    /// 模板额外允许写入的可信路径；仍受 Plan 只读守卫约束。
    pub allowed_write_dirs: Vec<PathBuf>,
}

/// MCP、Skills、插件、Hook 和 Agent catalog 注入 Runtime 的 Provider 中立边界。
pub trait RuntimeExtensionContributor: Send + Sync {
    /// 返回候选构建时冻结的插件引用身份目录；只包含 UI 所需的公开标识，不携带配置正文或凭据。
    fn plugin_reference_catalog(&self) -> Vec<Value> {
        Vec::new()
    }

    /// 返回候选构建时冻结的 Skill 引用目录；只包含 Composer 所需的公开元数据和安全路径。
    fn skill_reference_catalog(&self) -> Vec<Value> {
        Vec::new()
    }

    /// 仅返回已冻结且实际可调用的扩展目录元数据，不读取正文或执行指令。
    fn prompt_catalog(&self, _can_spawn: bool, _has_skill: bool) -> String {
        String::new()
    }

    /// 将当前候选的扩展工具注册进本 Turn 冻结工具表。
    fn register_tools(
        &self,
        registry: &mut ToolRegistry,
        context: &RuntimeToolContext,
    ) -> Result<(), String>;

    /// 构建当前候选的冻结 Hook 运行时。
    fn build_hook_runtime(&self, context: &RuntimeToolContext) -> Result<HookRuntime, String>;

    /// 验证并装配插件 LSP 执行端；声明 LSP 但无执行端时必须返回错误。
    fn prepare_lsp_runtime(&self, context: &RuntimeToolContext) -> Result<(), String>;

    /// 返回当前候选已经收集的安全诊断；默认没有诊断的贡献器不产生通知。
    fn diagnostics(&self) -> &[RuntimeExtensionDiagnostic] {
        &[]
    }

    /// 返回当前候选已经完成构建的 MCP Server 只读运行态快照。
    ///
    /// 默认贡献器没有 MCP 时返回空列表；实现不得在此方法中启动连接、
    /// 重试远端请求或读取新的配置。
    fn mcp_runtime_snapshot(&self) -> Vec<RuntimeMcpServerSnapshot> {
        Vec::new()
    }

    /// 返回项目候选中已经冻结的 MCP 延迟目录。
    ///
    /// 候选持有目录和连接的完整生命周期；每个 Turn 只注册同一候选代次的
    /// 搜索/执行包装器，不重新连接 Server 或从 Session 配置猜测运行态。
    fn mcp_tool_catalog(&self) -> Option<Arc<keencode_tools::DeferredToolCatalog>> {
        None
    }

    /// 返回当前项目候选占用的 MCP Server 名称，用于拒绝 Session 同名覆盖。
    fn mcp_server_names(&self) -> Vec<String> {
        self.mcp_runtime_snapshot()
            .into_iter()
            .map(|server| server.name)
            .collect()
    }

    /// 立即撤销当前贡献器已经注册的 MCP 工具；非 MCP 扩展保持不变。
    ///
    /// 配置被禁用、删除或损坏时，新的候选可能尚未完成构建；该同步边界
    /// 先让已有 Turn 共享的延迟目录失效，避免继续使用过期 MCP 工具。
    fn revoke_mcp_tools(&self) -> Result<(), String> {
        Ok(())
    }

    /// 按稳定名称解析插件或全局 Agent 模板。
    fn resolve_agent(
        &self,
        name: &str,
        parent: &RuntimeAgentTemplateContext,
    ) -> Result<Option<RuntimeAgentTemplate>, String>;
}

/// 完整构建成功后才可原子发布的一代扩展运行时候选。
pub struct RuntimeExtensionCandidate {
    /// 严格递增且不复用的候选代次。
    generation: u64,
    /// 同时持有 MCP、Skills、插件和 Hook 快照的贡献器。
    contributor: Arc<dyn RuntimeExtensionContributor>,
    /// 认证失效后标记候选待重建；保持原代次，下一次显式请求不能命中旧缓存。
    mcp_revoked: AtomicBool,
}

/// 将单个 Turn 已冻结的扩展候选适配为 spawn_agent 的同步模板解析端口。
struct RuntimeSpawnAgentTemplateResolver {
    /// 当前项目和候选代次唯一的扩展贡献器。
    contributor: Arc<dyn RuntimeExtensionContributor>,
}

impl SpawnAgentTemplateResolver for RuntimeSpawnAgentTemplateResolver {
    /// 严格解析显式模板并拆分可持久快照与 AgentProfile 覆盖字段。
    fn resolve(
        &self,
        name: &str,
        context: &SpawnAgentTemplateContext,
    ) -> Result<Option<ResolvedSpawnAgentTemplate>, keencode_agent::ToolError> {
        let parent = RuntimeAgentTemplateContext {
            session_id: context.session_id.as_str().to_owned(),
            parent_agent_id: context.parent_agent_id.as_str().to_owned(),
            root_turn_id: context.root_turn_id.as_str().to_owned(),
        };
        let template = self.contributor.resolve_agent(name, &parent).map_err(|_| {
            keencode_agent::ToolError::permanent(
                "agent_template_resolution_failed",
                "Agent 模板解析失败",
            )
        })?;
        Ok(template.map(|template| ResolvedSpawnAgentTemplate {
            snapshot: AgentTemplateSnapshot {
                inject_agents_md: template.inject_agents_md,
                name: template.name,
                system_prompt: template.system_prompt,
                max_turns: template.max_turns,
                allowed_write_dirs: template.allowed_write_dirs,
            },
            model: template.model,
            reasoning_effort: template.reasoning_effort,
            tool_names: template.tool_names,
            disallowed_tool_names: template.disallowed_tool_names,
        }))
    }
}

impl RuntimeExtensionCandidate {
    /// 创建非零代次的完整扩展候选。
    pub fn new(
        generation: u64,
        contributor: Arc<dyn RuntimeExtensionContributor>,
    ) -> Result<Self, AgentRuntimeError> {
        if generation == 0 {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        Ok(Self {
            generation,
            contributor,
            mcp_revoked: AtomicBool::new(false),
        })
    }

    /// 返回候选代次。
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// `projects/<project-id>/<session-id>/collaboration-v2.json` 中一次完整原子提交。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CollaborationTransitionFile {
    /// 文件语义的稳定 Schema。
    schema: String,
    /// Schema 内部版本。
    version: u32,
    /// 文件唯一所属的根 Session。
    session: String,
    /// 对除本字段外完整载荷计算的 SHA-256 小写十六进制摘要。
    checksum_sha256: String,
    /// 事件批次和同水位完整 checkpoint。
    commit: CollaborationTransitionCommit,
    /// 与协调器终态原子保存、尚待补齐 Journal 的未启动子 Turn 证据。
    unstarted_turn_terminations: Vec<UnstartedTurnTerminationRecord>,
}

/// 持久保留的未启动终态证据，避免后续 Coordinator 提交覆盖原始事件批次。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UnstartedTurnTerminationRecord {
    /// 被中断的单层子 Agent。
    agent_id: RunnerAgentId,
    /// 被中断 Agent 的不可变直接父 Agent。
    parent_agent_id: RunnerAgentId,
    /// 被中断 Agent 在根树中的不可变路径。
    agent_path: String,
    /// 未进入 Runtime 生命周期即被取消或拒绝的 Turn。
    turn_id: AgentTurnId,
    /// 被中断 Turn 所属的根 Turn。
    root_turn_id: AgentTurnId,
    /// 被中断 Turn 的直接父 Turn。
    parent_turn_id: AgentTurnId,
    /// Agent 创建时冻结的任务正文。
    task: String,
    /// 该 Turn 的不可变输入摘要。
    prompt_summary: String,
    /// 是否为该 Agent 的首次 InitialTask。
    initial_task: bool,
    /// 由可信转换冻结的终态及稳定失败说明，参与文件校验和幂等正文校验。
    termination: UnstartedTurnTermination,
}

/// 从 Store 原子提交文件读取的完整 checkpoint 与等待取消证据。
#[derive(Clone, Debug)]
struct CollaborationTransitionSnapshot {
    /// 与事件批次同一原子边界提交的完整协调器 checkpoint。
    commit: CollaborationTransitionCommit,
    /// 截止当前 checkpoint 的全部等待容量取消证据。
    unstarted_turn_terminations: Vec<UnstartedTurnTerminationRecord>,
}

/// 局部驱逐 Agent checkpoint 的完整磁盘记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CollaborationAgentFile {
    /// 文件语义的稳定 Schema。
    schema: String,
    /// Schema 内部版本。
    version: u32,
    /// 文件唯一所属的根 Session。
    session: String,
    /// 对除本字段外完整载荷计算的 SHA-256 小写十六进制摘要。
    checksum_sha256: String,
    /// 单 Agent 的完整恢复 checkpoint。
    checkpoint: RecoveredAgentCheckpoint,
}

/// 计算完整协调器文件摘要时使用的不含自引用 checksum 的稳定载荷。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationTransitionChecksum<'a> {
    /// 文件语义的稳定 Schema。
    schema: &'a str,
    /// Schema 内部版本。
    version: u32,
    /// 文件唯一所属的根 Session。
    session: &'a str,
    /// 事件批次和同水位完整 checkpoint。
    commit: &'a CollaborationTransitionCommit,
    /// 已确认属于 WaitingCapacity 到 Interrupted 的未启动 Turn 证据。
    unstarted_turn_terminations: &'a [UnstartedTurnTerminationRecord],
}

/// 计算局部 Agent 文件摘要时使用的不含自引用 checksum 的稳定载荷。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationAgentChecksum<'a> {
    /// 文件语义的稳定 Schema。
    schema: &'a str,
    /// Schema 内部版本。
    version: u32,
    /// 文件唯一所属的根 Session。
    session: &'a str,
    /// 单 Agent 的完整恢复 checkpoint。
    checkpoint: &'a RecoveredAgentCheckpoint,
}

/// 每个 Runtime Session 独占的 Collaboration v2 生产磁盘 Store。
struct SessionCollaborationStore {
    /// 已通过资源层单一路径段校验的根 Session 标识。
    session_id: String,
    /// 原子保存最新事件批次和完整协调器 checkpoint 的文件。
    transition_path: PathBuf,
    /// 使用 Agent 标识摘要命名的局部 checkpoint 目录。
    agent_checkpoint_directory: PathBuf,
    /// 生产装配绑定的 RuntimeSession；纯 Store 单元测试可以暂不绑定。
    runtime_session: OnceLock<RuntimeSession>,
    /// 串行化读取、比较和原子替换，保证单进程内线性化。
    commit_gate: Mutex<()>,
    /// 测试注入：让提交泵线程内的下一次未启动失败 Journal 发布立即失败。
    ///
    /// 协作批次落盘已移入后台提交泵线程，线程局部的 Journal 故障注入无法
    /// 覆盖该线程；该开关用于在测试中模拟"发布失败、receipt 留盘"窗口。
    #[cfg(test)]
    fail_next_unstarted_publish: std::sync::atomic::AtomicBool,
}

impl SessionCollaborationStore {
    /// 按会话定位记录创建所属项目目录中的生产 Store。
    fn new(storage_root: &Path, session_id: &str) -> Result<Self, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session_directory = keencode_resources::session_storage_directory(
            storage_root,
            &keencode_resources::SessionId::new(session_id).map_err(runtime_operation_failed)?,
        )
        .map_err(runtime_operation_failed)?;
        Ok(Self {
            session_id: session_id.to_owned(),
            transition_path: session_directory.join("collaboration-v2.json"),
            agent_checkpoint_directory: session_directory.join("collaboration-v2-agents"),
            runtime_session: OnceLock::new(),
            commit_gate: Mutex::new(()),
            #[cfg(test)]
            fail_next_unstarted_publish: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 将生产 Store 绑定到同一 Session Runtime，保证 Collaboration 取消补偿不能绕过 Journal。
    fn bind_runtime_session(&self, session: &RuntimeSession) -> Result<(), AgentRuntimeError> {
        if session.session_id().as_str() != self.session_id {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        if let Some(bound) = self.runtime_session.get() {
            if bound.session_id() != session.session_id() {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            return Ok(());
        }
        self.runtime_session
            .set(session.clone())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)
    }

    /// 读取生产装配绑定的 RuntimeSession；未绑定时禁止热路径静默返回成功。
    fn bound_runtime_session(&self) -> Result<RuntimeSession, AgentRuntimeError> {
        self.runtime_session
            .get()
            .cloned()
            .ok_or(AgentRuntimeError::RecoveryRequired)
    }

    /// 确认等待容量取消已写入 Runtime Journal 后清理对应的持久 pending 证据。
    fn acknowledge_unstarted_turn_terminations(
        &self,
        acknowledged: &[UnstartedTurnTerminationRecord],
    ) -> Result<(), CollaborationPortError> {
        if acknowledged.is_empty() {
            return Ok(());
        }
        validate_unstarted_turn_termination_records(acknowledged)?;
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        let Some(current) = self.load_transition_file_unlocked()? else {
            return Ok(());
        };
        let remaining = current
            .unstarted_turn_terminations
            .iter()
            .filter(|record| !acknowledged.contains(record))
            .cloned()
            .collect::<Vec<_>>();
        if remaining.len() == current.unstarted_turn_terminations.len() {
            return Ok(());
        }
        let bytes = self.encode_transition_file(&current.commit, &remaining)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COLLABORATION_TRANSITION_FILE_BYTES
        {
            return Err(CollaborationPortError::new(
                "Collaboration pending 取消证据超过磁盘上限",
            ));
        }
        storage::atomic_write_private(&self.transition_path, &bytes)
            .map_err(|_| CollaborationPortError::new("Collaboration pending 取消证据确认失败"))
    }

    /// 返回 Store 当前完整提交文件；调用方可同时读取事件批次、checkpoint 和取消证据。
    fn load_transition_snapshot(
        &self,
    ) -> Result<Option<CollaborationTransitionSnapshot>, CollaborationPortError> {
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        Ok(self
            .load_transition_file_unlocked()?
            .map(|record| CollaborationTransitionSnapshot {
                commit: record.commit,
                unstarted_turn_terminations: record.unstarted_turn_terminations,
            }))
    }

    /// 在已持有提交门时读取并校验最新完整协调器文件。
    fn load_transition_file_unlocked(
        &self,
    ) -> Result<Option<CollaborationTransitionFile>, CollaborationPortError> {
        let Some(bytes) = read_bounded_regular_file(
            &self.transition_path,
            MAX_COLLABORATION_TRANSITION_FILE_BYTES,
            "Collaboration 提交文件",
        )?
        else {
            return Ok(None);
        };
        let record: CollaborationTransitionFile = serde_json::from_slice(&bytes)
            .map_err(|_| CollaborationPortError::new("Collaboration 提交文件 JSON 无效"))?;
        self.validate_transition_file(&record)?;
        Ok(Some(record))
    }

    /// 校验完整协调器文件的 Schema、Session、checksum 与领域约束。
    fn validate_transition_file(
        &self,
        record: &CollaborationTransitionFile,
    ) -> Result<(), CollaborationPortError> {
        if record.schema != COLLABORATION_TRANSITION_SCHEMA
            || record.version != COLLABORATION_TRANSITION_VERSION
            || record.session != self.session_id
            || !valid_sha256_hex(&record.checksum_sha256)
        {
            return Err(CollaborationPortError::new("Collaboration 提交文件头无效"));
        }
        record
            .commit
            .validate()
            .map_err(|_| CollaborationPortError::new("Collaboration 提交领域状态无效"))?;
        validate_transition_session(&record.commit, &self.session_id)?;
        validate_unstarted_turn_termination_records(&record.unstarted_turn_terminations)?;
        let checksum = collaboration_transition_checksum(
            &record.schema,
            record.version,
            &record.session,
            &record.commit,
            &record.unstarted_turn_terminations,
        )?;
        if checksum != record.checksum_sha256 {
            return Err(CollaborationPortError::new(
                "Collaboration 提交文件 checksum 不匹配",
            ));
        }
        Ok(())
    }

    /// 将完整提交编码为带自校验摘要的唯一磁盘表示。
    fn encode_transition_file(
        &self,
        commit: &CollaborationTransitionCommit,
        unstarted_turn_terminations: &[UnstartedTurnTerminationRecord],
    ) -> Result<Vec<u8>, CollaborationPortError> {
        commit
            .validate()
            .map_err(|_| CollaborationPortError::new("Collaboration 提交领域状态无效"))?;
        validate_transition_session(commit, &self.session_id)?;
        validate_unstarted_turn_termination_records(unstarted_turn_terminations)?;
        let checksum = collaboration_transition_checksum(
            COLLABORATION_TRANSITION_SCHEMA,
            COLLABORATION_TRANSITION_VERSION,
            &self.session_id,
            commit,
            unstarted_turn_terminations,
        )?;
        serde_json::to_vec(&CollaborationTransitionFile {
            schema: COLLABORATION_TRANSITION_SCHEMA.to_owned(),
            version: COLLABORATION_TRANSITION_VERSION,
            session: self.session_id.clone(),
            checksum_sha256: checksum,
            commit: commit.clone(),
            unstarted_turn_terminations: unstarted_turn_terminations.to_vec(),
        })
        .map_err(|_| CollaborationPortError::new("Collaboration 提交文件无法序列化"))
    }

    /// 返回局部 Agent checkpoint 的内容寻址文件名。
    fn agent_checkpoint_path(&self, agent_id: &RunnerAgentId) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(b"keencode.session.collaboration-agent-path.v1\0");
        digest.update(agent_id.as_str().as_bytes());
        self.agent_checkpoint_directory
            .join(format!("{:x}.json", digest.finalize()))
    }

    /// 在已持有提交门时读取并校验一个局部 Agent checkpoint。
    fn load_agent_file_unlocked(
        &self,
        agent_id: &RunnerAgentId,
    ) -> Result<Option<CollaborationAgentFile>, CollaborationPortError> {
        let path = self.agent_checkpoint_path(agent_id);
        let Some(bytes) = read_bounded_regular_file(
            &path,
            MAX_COLLABORATION_AGENT_FILE_BYTES,
            "Collaboration Agent checkpoint 文件",
        )?
        else {
            return Ok(None);
        };
        let record: CollaborationAgentFile = serde_json::from_slice(&bytes)
            .map_err(|_| CollaborationPortError::new("Collaboration Agent checkpoint JSON 无效"))?;
        self.validate_agent_file(&record, agent_id)?;
        Ok(Some(record))
    }

    /// 校验局部 Agent 文件的归属、身份和 checksum。
    fn validate_agent_file(
        &self,
        record: &CollaborationAgentFile,
        agent_id: &RunnerAgentId,
    ) -> Result<(), CollaborationPortError> {
        let definition = &record.checkpoint.agent.definition;
        if record.schema != COLLABORATION_AGENT_SCHEMA
            || record.version != COLLABORATION_TRANSITION_VERSION
            || record.session != self.session_id
            || !valid_sha256_hex(&record.checksum_sha256)
            || record.checkpoint.revision == 0
            || definition.agent_id != *agent_id
            || definition.root_agent_id != record.checkpoint.root_agent_id
            || definition.root_session_id.as_str() != self.session_id
        {
            return Err(CollaborationPortError::new(
                "Collaboration Agent checkpoint 文件头或归属无效",
            ));
        }
        let checksum = collaboration_agent_checksum(
            &record.schema,
            record.version,
            &record.session,
            &record.checkpoint,
        )?;
        if checksum != record.checksum_sha256 {
            return Err(CollaborationPortError::new(
                "Collaboration Agent checkpoint checksum 不匹配",
            ));
        }
        Ok(())
    }

    /// 将局部 Agent checkpoint 编码为带自校验摘要的唯一磁盘表示。
    fn encode_agent_file(
        &self,
        checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<Vec<u8>, CollaborationPortError> {
        let definition = &checkpoint.agent.definition;
        if checkpoint.revision == 0
            || definition.root_agent_id != checkpoint.root_agent_id
            || definition.root_session_id.as_str() != self.session_id
        {
            return Err(CollaborationPortError::new(
                "Collaboration Agent checkpoint 归属无效",
            ));
        }
        let checksum = collaboration_agent_checksum(
            COLLABORATION_AGENT_SCHEMA,
            COLLABORATION_TRANSITION_VERSION,
            &self.session_id,
            checkpoint,
        )?;
        serde_json::to_vec(&CollaborationAgentFile {
            schema: COLLABORATION_AGENT_SCHEMA.to_owned(),
            version: COLLABORATION_TRANSITION_VERSION,
            session: self.session_id.clone(),
            checksum_sha256: checksum,
            checkpoint: checkpoint.clone(),
        })
        .map_err(|_| CollaborationPortError::new("Collaboration Agent checkpoint 无法序列化"))
    }
    /// 按普通或冷恢复语义原子替换事件与 checkpoint 的共同文件。
    fn commit_transition_with_policy(
        &self,
        commit: &CollaborationTransitionCommit,
        allow_recovery_source: bool,
    ) -> CollaborationAppendResult {
        let _gate = match self.commit_gate.lock() {
            Ok(gate) => gate,
            Err(_) => {
                return CollaborationAppendResult::Indeterminate {
                    error: CollaborationPortError::new("Collaboration Store 状态不可用"),
                };
            }
        };
        let current = match self.load_transition_file_unlocked() {
            Ok(current) => current,
            Err(error) => return CollaborationAppendResult::Indeterminate { error },
        };
        let current_sequence = current
            .as_ref()
            .map_or(0, |record| record.commit.checkpoint.last_event_sequence);
        if let Some(current) = &current
            && current.commit == *commit
        {
            let records = current.unstarted_turn_terminations.clone();
            drop(_gate);
            self.publish_committed_unstarted_failures(commit, &records);
            return CollaborationAppendResult::AlreadyCommitted { current_sequence };
        }
        if commit.validate().is_err()
            || validate_transition_session(commit, &self.session_id).is_err()
            || current_sequence != commit.batch.expected_sequence
            || current.as_ref().is_some_and(|record| {
                record.commit.batch.batch_id == commit.batch.batch_id
                    && record.commit.batch != commit.batch
            })
        {
            return CollaborationAppendResult::Conflict {
                actual_sequence: current_sequence,
            };
        }
        let additions = match unstarted_turn_termination_records_with_policy(
            self,
            commit,
            current.as_ref(),
            allow_recovery_source,
        ) {
            Ok(additions) => additions,
            Err(error) => return CollaborationAppendResult::Indeterminate { error },
        };
        let mut unstarted_turn_terminations = current.as_ref().map_or_else(Vec::new, |record| {
            record.unstarted_turn_terminations.clone()
        });
        for addition in additions {
            if !unstarted_turn_terminations.contains(&addition) {
                unstarted_turn_terminations.push(addition);
            }
        }
        let bytes = match self.encode_transition_file(commit, &unstarted_turn_terminations) {
            Ok(bytes) => bytes,
            Err(error) => return CollaborationAppendResult::Indeterminate { error },
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COLLABORATION_TRANSITION_FILE_BYTES
        {
            return CollaborationAppendResult::Absent { current_sequence };
        }
        if storage::atomic_write_private(&self.transition_path, &bytes).is_ok() {
            drop(_gate);
            self.publish_committed_unstarted_failures(commit, &unstarted_turn_terminations);
            return CollaborationAppendResult::Appended;
        }
        let result = match self.load_transition_file_unlocked() {
            Ok(Some(record)) if record.commit == *commit => {
                CollaborationAppendResult::AlreadyCommitted {
                    current_sequence: record.commit.checkpoint.last_event_sequence,
                }
            }
            Ok(Some(record))
                if record.commit.checkpoint.last_event_sequence == current_sequence =>
            {
                CollaborationAppendResult::Absent { current_sequence }
            }
            Ok(Some(record)) => CollaborationAppendResult::Conflict {
                actual_sequence: record.commit.checkpoint.last_event_sequence,
            },
            Ok(None) if current_sequence == 0 => {
                CollaborationAppendResult::Absent { current_sequence }
            }
            Ok(None) => CollaborationAppendResult::Indeterminate {
                error: CollaborationPortError::new("Collaboration 原子写失败后原提交文件不可判定"),
            },
            Err(error) => CollaborationAppendResult::Indeterminate { error },
        };
        drop(_gate);
        if matches!(result, CollaborationAppendResult::AlreadyCommitted { .. }) {
            self.publish_committed_unstarted_failures(commit, &unstarted_turn_terminations);
        }
        result
    }

    /// 仅在协调器终态与 receipt 已原子提交后尝试发布失败生命周期；失败时保留 receipt。
    ///
    /// Journal 与协调器不做易失双写：磁盘 receipt 是后续启动、mailbox 和冷恢复的屏障。
    /// Journal 失败不谎报已落盘的协调器提交不存在，也不抛弃已确定的失败终态。
    fn publish_committed_unstarted_failures(
        &self,
        commit: &CollaborationTransitionCommit,
        records: &[UnstartedTurnTerminationRecord],
    ) {
        let failures = records
            .iter()
            .filter(|record| {
                matches!(record.termination, UnstartedTurnTermination::Failed { .. })
                    && commit.batch.events.iter().any(|event| {
                        event.agent_id == record.agent_id
                            && event.turn_id.as_ref() == Some(&record.turn_id)
                            && unstarted_turn_terminal_event_matches(
                                &event.kind,
                                &record.termination,
                            )
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if failures.is_empty() {
            return;
        }
        #[cfg(test)]
        if self
            .fail_next_unstarted_publish
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            // 模拟 Journal 发布失败：receipt 已随批次原子落盘，Journal 对账
            // 留待后续屏障重试，与真实 Journal 故障的持久化语义一致。
            tracing::warn!(target: "agent_runtime", "测试注入：未启动失败 Journal 发布延后");
            return;
        }
        let result = self.bound_runtime_session().and_then(|session| {
            reconcile_unstarted_turn_termination_records(
                &session,
                self,
                &commit.checkpoint,
                &failures,
            )
        });
        if let Err(error) = result {
            tracing::warn!(target: "agent_runtime", error = %error,
                "未启动失败已持久保存，Journal 对账待后续屏障重试");
        }
    }

    /// 对账磁盘中的待决终态；调用方不得持有本 Store 提交锁或只凭内存状态放行。
    fn reconcile_pending_unstarted_turns(&self) -> Result<(), AgentRuntimeError> {
        let Some(snapshot) = self
            .load_transition_snapshot()
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?
        else {
            return Ok(());
        };
        if snapshot.unstarted_turn_terminations.is_empty() {
            return Ok(());
        }
        reconcile_unstarted_turn_termination_records(
            &self.bound_runtime_session()?,
            self,
            &snapshot.commit.checkpoint,
            &snapshot.unstarted_turn_terminations,
        )
    }
}

impl CollaborationStore for SessionCollaborationStore {
    /// 返回原子提交文件中的可信 checkpoint 水位；全新 Session 返回零。
    fn current_sequence(&self) -> Result<u64, CollaborationPortError> {
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        Ok(self
            .load_transition_file_unlocked()?
            .map_or(0, |record| record.commit.checkpoint.last_event_sequence))
    }

    /// 返回与最后事件批次在同一文件原子提交的完整协调器 checkpoint。
    fn load_coordinator_checkpoint(
        &self,
    ) -> Result<Option<RecoveredCoordinator>, CollaborationPortError> {
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        Ok(self
            .load_transition_file_unlocked()?
            .map(|record| record.commit.checkpoint))
    }

    /// 比较稳定批次和水位后原子替换事件与 checkpoint 的共同文件。
    fn commit_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        self.commit_transition_with_policy(commit, false)
    }

    /// 提交仅由协调器冷恢复生成的未知 Turn 收敛批次。
    fn commit_recovery_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        self.commit_transition_with_policy(commit, true)
    }

    /// 按 Agent 标识摘要读取局部驱逐 checkpoint。
    fn load_agent_checkpoint(
        &self,
        agent_id: &RunnerAgentId,
    ) -> Result<Option<RecoveredAgentCheckpoint>, CollaborationPortError> {
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        Ok(self
            .load_agent_file_unlocked(agent_id)?
            .map(|record| record.checkpoint))
    }

    /// 只允许同内容重试或严格递增一版的局部 Agent checkpoint 原子替换。
    fn save_agent_checkpoint(
        &self,
        checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<(), CollaborationPortError> {
        let _gate = self
            .commit_gate
            .lock()
            .map_err(|_| CollaborationPortError::new("Collaboration Store 状态不可用"))?;
        let agent_id = &checkpoint.agent.definition.agent_id;
        let current = self.load_agent_file_unlocked(agent_id)?;
        if current
            .as_ref()
            .is_some_and(|record| record.checkpoint == *checkpoint)
        {
            return Ok(());
        }
        if current.as_ref().is_some_and(|record| {
            record
                .checkpoint
                .revision
                .checked_add(1)
                .is_none_or(|next| next != checkpoint.revision)
        }) {
            return Err(CollaborationPortError::new(
                "Collaboration Agent checkpoint 修订号冲突",
            ));
        }
        let bytes = self.encode_agent_file(checkpoint)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COLLABORATION_AGENT_FILE_BYTES {
            return Err(CollaborationPortError::new(
                "Collaboration Agent checkpoint 超过磁盘上限",
            ));
        }
        storage::atomic_write_private(&self.agent_checkpoint_path(agent_id), &bytes)
            .map_err(|_| CollaborationPortError::new("Collaboration Agent checkpoint 写入失败"))
    }
}

/// 读取固定上限内的普通文件，拒绝目录、符号链接和读取期间发生的长度变化。
fn read_bounded_regular_file(
    path: &Path,
    maximum_bytes: u64,
    label: &str,
) -> Result<Option<Vec<u8>>, CollaborationPortError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CollaborationPortError::new(format!("{label}无法检查"))),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        return Err(CollaborationPortError::new(format!(
            "{label}类型或大小无效"
        )));
    }
    let bytes =
        fs::read(path).map_err(|_| CollaborationPortError::new(format!("{label}无法读取")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len()
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes
    {
        return Err(CollaborationPortError::new(format!(
            "{label}读取期间发生变化"
        )));
    }
    Ok(Some(bytes))
}

/// 计算完整协调器提交文件的稳定 SHA-256 摘要。
fn collaboration_transition_checksum(
    schema: &str,
    version: u32,
    session: &str,
    commit: &CollaborationTransitionCommit,
    unstarted_turn_terminations: &[UnstartedTurnTerminationRecord],
) -> Result<String, CollaborationPortError> {
    let bytes = serde_json::to_vec(&CollaborationTransitionChecksum {
        schema,
        version,
        session,
        commit,
        unstarted_turn_terminations,
    })
    .map_err(|_| CollaborationPortError::new("Collaboration checksum 载荷无法序列化"))?;
    Ok(sha256_hex(&bytes))
}

/// 用 Store 上一已提交 checkpoint 校验 Running Turn 的不可变派发身份。
///
/// 普通中断及未启动失败使用相同的根链、Turn 与来源校验；是否属于未启动失败
/// 还须由调用方验证 start_pending、Journal 尚无起点以及精确配对的失败事件。
fn trusted_running_turn_interruption(
    commit: &CollaborationTransitionCommit,
    trusted_previous: Option<&CollaborationTransitionFile>,
    event: &CollaborationEvent,
    previous_turn_id: &AgentTurnId,
    interrupted_turn_id: &AgentTurnId,
    allow_recovery_source: bool,
) -> bool {
    let Some(previous) = trusted_previous else {
        return false;
    };
    if previous.commit.checkpoint.last_event_sequence != commit.batch.expected_sequence {
        return false;
    }
    let Some(agent) = recovered_agent_for_id(&previous.commit.checkpoint, &event.agent_id) else {
        return false;
    };
    matches!(
        agent.status,
        CollaborationAgentStatus::Running { ref turn_id }
            if turn_id == previous_turn_id && turn_id == interrupted_turn_id
    ) && event.turn_id.as_ref() == Some(interrupted_turn_id)
        && event.session_id == agent.definition.session_id
        && event.agent_path == agent.definition.path
        && event.parent_agent_id.as_ref() == agent.definition.parent_agent_id.as_ref()
        && event.parent_turn_id.as_ref() == agent.current_parent_turn_id.as_ref()
        && event.root_turn_id.as_ref() == agent.current_root_turn_id.as_ref()
        && if allow_recovery_source {
            agent.current_source_agent_id.as_ref() == Some(&event.source_agent_id)
        } else {
            event.source_agent_id == event.agent_id
        }
}

#[cfg(test)]
fn unstarted_turn_termination_records(
    store: &SessionCollaborationStore,
    commit: &CollaborationTransitionCommit,
) -> Result<Vec<UnstartedTurnTerminationRecord>, CollaborationPortError> {
    unstarted_turn_termination_records_with_policy(store, commit, None, false)
}

/// 按可信旧 checkpoint 与配对事件提取等待取消或预检失败的未启动终态证据。
fn unstarted_turn_termination_records_with_policy(
    store: &SessionCollaborationStore,
    commit: &CollaborationTransitionCommit,
    trusted_previous: Option<&CollaborationTransitionFile>,
    allow_recovery_source: bool,
) -> Result<Vec<UnstartedTurnTerminationRecord>, CollaborationPortError> {
    let events = &commit.batch.events;
    let mut records = Vec::new();
    for event in events {
        let CollaborationEventKind::AgentStatusChanged { previous, current } = &event.kind else {
            continue;
        };
        if let CollaborationEventKind::AgentStatusChanged {
            previous:
                CollaborationAgentStatus::Running {
                    turn_id: previous_turn_id,
                },
            current:
                CollaborationAgentStatus::Interrupted {
                    turn_id: interrupted_turn_id,
                },
        } = &event.kind
            && (previous_turn_id != interrupted_turn_id
                || !trusted_running_turn_interruption(
                    commit,
                    trusted_previous,
                    event,
                    previous_turn_id,
                    interrupted_turn_id,
                    allow_recovery_source,
                ))
        {
            return Err(CollaborationPortError::new(
                "Running Turn 的中断事件缺少可信旧 checkpoint 事实",
            ));
        }
        let (waiting_turn_id, interrupted_turn_id, termination) = match (previous, current) {
            (
                CollaborationAgentStatus::WaitingCapacity {
                    turn_id: previous_turn,
                },
                CollaborationAgentStatus::Interrupted { turn_id },
            ) => (
                previous_turn,
                turn_id,
                UnstartedTurnTermination::Interrupted,
            ),
            (
                CollaborationAgentStatus::Running {
                    turn_id: previous_turn,
                },
                CollaborationAgentStatus::Failed { turn_id, message },
            ) if event.agent_id.as_str() != keencode_resources::ROOT_AGENT_ID => {
                let Some(previous_agent) = trusted_previous.and_then(|record| {
                    recovered_agent_for_id(&record.commit.checkpoint, &event.agent_id)
                }) else {
                    return Err(CollaborationPortError::new(
                        "未启动失败缺少可信旧 checkpoint",
                    ));
                };
                if !previous_agent.start_pending {
                    // 已确认派发的普通执行失败由 Runner 写 Journal，不属于本补偿入口。
                    continue;
                }
                if !trusted_running_turn_interruption(
                    commit,
                    trusted_previous,
                    event,
                    previous_turn,
                    turn_id,
                    false,
                ) || message.trim().is_empty()
                {
                    return Err(CollaborationPortError::new(
                        "未启动失败的派发身份或说明不一致",
                    ));
                }
                let session = store.runtime_session.get().ok_or_else(|| {
                    CollaborationPortError::new("未启动失败 Store 尚未绑定 RuntimeSession")
                })?;
                let snapshot = session.snapshot().map_err(|_| {
                    CollaborationPortError::new("未启动失败无法读取 Runtime Journal")
                })?;
                if snapshot
                    .state
                    .turns
                    .keys()
                    .any(|known| known.as_str() == turn_id.as_str())
                {
                    // Runner 可先完成再确认派发；必须核对谱系和失败正文，不能只凭 ID 跳过。
                    validate_journal_turn_correspondence(
                        &snapshot.state,
                        Some(session),
                        previous_agent,
                        turn_id,
                        previous_agent.current_turn_cause.as_ref().ok_or_else(|| {
                            CollaborationPortError::new("已写入失败 Turn 缺少原派发原因")
                        })?,
                        previous_agent.current_turn_prompt.as_deref(),
                        previous_agent.current_parent_turn_id.as_ref(),
                        previous_agent
                            .current_root_turn_id
                            .as_ref()
                            .ok_or_else(|| {
                                CollaborationPortError::new("已写入失败 Turn 缺少原根 Turn")
                            })?,
                        None,
                        Some(&AgentTurnOutcome::Failed {
                            message: message.clone(),
                        }),
                    )
                    .map_err(|_| {
                        CollaborationPortError::new("已有 Journal 失败与派发身份或正文冲突")
                    })?
                    .ok_or_else(|| {
                        CollaborationPortError::new("已有 Journal 未形成一致失败终态")
                    })?;
                    continue;
                }
                (
                    previous_turn,
                    turn_id,
                    UnstartedTurnTermination::Failed {
                        message: message.clone(),
                    },
                )
            }
            _ => continue,
        };
        if waiting_turn_id != interrupted_turn_id
            || event.agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
            || event.turn_id.as_ref() != Some(waiting_turn_id)
            || !events.iter().any(|candidate| {
                candidate.agent_id == event.agent_id
                    && candidate.turn_id.as_ref() == Some(waiting_turn_id)
                    && unstarted_turn_terminal_event_matches(&candidate.kind, &termination)
            })
        {
            return Err(CollaborationPortError::new(
                "WaitingCapacity 到 Interrupted 事件缺少一致的中断证据",
            ));
        }
        let agent = recovered_agent_for_id(&commit.checkpoint, &event.agent_id)
            .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少 Agent checkpoint"))?;
        let definition = &agent.definition;
        let parent_agent_id = definition
            .parent_agent_id
            .clone()
            .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少父 Agent"))?;
        let source_is_known_in_tree = commit
            .checkpoint
            .roots
            .iter()
            .find(|root| root.root_agent_id == definition.root_agent_id)
            .is_some_and(|root| {
                root.known_agents
                    .iter()
                    .any(|known| known.agent_id == event.source_agent_id)
            });
        if definition.depth != AgentDepth::CHILD
            || definition.root_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
            || event.session_id != definition.session_id
            || event.parent_agent_id.as_ref() != Some(&parent_agent_id)
            || event.agent_path != definition.path
            || !source_is_known_in_tree
        {
            return Err(CollaborationPortError::new(
                "等待容量取消事件的来源、父链或路径不一致",
            ));
        }
        let last_turn = agent
            .last_turn
            .as_ref()
            .filter(|turn| turn.turn_id == *waiting_turn_id)
            .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少 Turn checkpoint"))?;
        if matches!(termination, UnstartedTurnTermination::Failed { .. }) {
            let previous_agent = trusted_previous
                .and_then(|record| {
                    recovered_agent_for_id(&record.commit.checkpoint, &event.agent_id)
                })
                .ok_or_else(|| CollaborationPortError::new("未启动失败缺少原派发事实"))?;
            if previous_agent.definition != *definition
                || previous_agent.current_turn_cause.as_ref() != Some(&last_turn.cause)
                || previous_agent.current_turn_prompt != last_turn.prompt
                || agent.start_pending
            {
                return Err(CollaborationPortError::new("未启动失败与原派发任务不一致"));
            }
        }
        let parent_turn_id = event
            .parent_turn_id
            .clone()
            .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少父 Turn"))?;
        let root_turn_id = event
            .root_turn_id
            .clone()
            .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少根 Turn"))?;
        let prompt_summary =
            collaboration_turn_prompt_summary(&last_turn.cause, last_turn.prompt.as_deref(), None)
                .map_err(|_| CollaborationPortError::new("等待容量取消证据的 Turn 摘要无效"))?
                .ok_or_else(|| CollaborationPortError::new("等待容量取消证据缺少 Turn 摘要"))?;
        let interruption_events = events
            .iter()
            .filter(|candidate| {
                candidate.agent_id == event.agent_id
                    && candidate.turn_id.as_ref() == Some(waiting_turn_id)
                    && unstarted_turn_terminal_event_matches(&candidate.kind, &termination)
            })
            .collect::<Vec<_>>();
        let Some(interruption_event) = interruption_events.first() else {
            return Err(CollaborationPortError::new(
                "WaitingCapacity 到 Interrupted 事件缺少中断事件",
            ));
        };
        if interruption_events.len() != 1
            || interruption_event.session_id != event.session_id
            || interruption_event.source_agent_id != event.source_agent_id
            || interruption_event.parent_agent_id != event.parent_agent_id
            || interruption_event.agent_path != event.agent_path
            || interruption_event.parent_turn_id != event.parent_turn_id
            || interruption_event.root_turn_id != event.root_turn_id
            || interruption_event.sequence.checked_add(1) != Some(event.sequence)
        {
            return Err(CollaborationPortError::new(
                "等待容量取消的中断事件字段或顺序不一致",
            ));
        }
        if last_turn.parent_turn_id.as_ref() != Some(&parent_turn_id)
            || last_turn.root_turn_id != root_turn_id
            || definition.parent_agent_id.as_ref() != Some(&parent_agent_id)
            || definition.path.as_str() != event.agent_path.as_str()
            || &agent.status != current
            || !unstarted_turn_outcome_matches(&last_turn.outcome, &termination)
            || matches!(last_turn.cause, AgentTurnCause::RootUser)
        {
            return Err(CollaborationPortError::new(
                "等待容量取消证据的 Agent 或 Turn 身份不一致",
            ));
        }
        let initial_task = matches!(last_turn.cause, AgentTurnCause::InitialTask);
        let task = if initial_task {
            last_turn
                .prompt
                .clone()
                .ok_or_else(|| CollaborationPortError::new("初始等待容量取消证据缺少任务正文"))?
        } else {
            let session = store.runtime_session.get().ok_or_else(|| {
                CollaborationPortError::new("等待容量取消 Store 尚未绑定 RuntimeSession")
            })?;
            let resource_agent_id = ResourceAgentId::new(event.agent_id.as_str().to_owned())
                .map_err(|_| CollaborationPortError::new("等待容量取消 Agent 标识无效"))?;
            session
                .snapshot()
                .map_err(|_| CollaborationPortError::new("等待容量取消无法读取 Runtime Journal"))?
                .state
                .sub_agents
                .get(&resource_agent_id)
                .map(|agent| agent.task.clone())
                .ok_or_else(|| CollaborationPortError::new("后续等待容量取消证据缺少任务正文"))?
        };
        let record = UnstartedTurnTerminationRecord {
            agent_id: event.agent_id.clone(),
            parent_agent_id,
            agent_path: event.agent_path.as_str().to_owned(),
            turn_id: waiting_turn_id.clone(),
            root_turn_id,
            parent_turn_id,
            task,
            prompt_summary,
            initial_task,
            termination,
        };
        if records.contains(&record) {
            return Err(CollaborationPortError::new(
                "等待容量取消证据在同一批次内重复",
            ));
        }
        records.push(record);
    }
    if records.len() > MAX_UNSTARTED_TURN_TERMINATION_RECORDS {
        return Err(CollaborationPortError::new(
            "等待容量取消证据超过持久化上限",
        ));
    }
    Ok(records)
}

/// 仅接受与持久补偿终态精确配对的领域事件，不根据文本前缀推断失败来源。
fn unstarted_turn_terminal_event_matches(
    kind: &CollaborationEventKind,
    termination: &UnstartedTurnTermination,
) -> bool {
    match (kind, termination) {
        (CollaborationEventKind::AgentTurnInterrupted, UnstartedTurnTermination::Interrupted) => {
            true
        }
        (
            CollaborationEventKind::AgentTurnFailed { message },
            UnstartedTurnTermination::Failed { message: expected },
        ) => message == expected,
        _ => false,
    }
}

/// 校验最近 Turn 的终态与 receipt 中的失败说明完全一致。
fn unstarted_turn_outcome_matches(
    outcome: &AgentTurnOutcome,
    termination: &UnstartedTurnTermination,
) -> bool {
    match (outcome, termination) {
        (AgentTurnOutcome::Interrupted, UnstartedTurnTermination::Interrupted) => true,
        (
            AgentTurnOutcome::Failed { message },
            UnstartedTurnTermination::Failed { message: expected },
        ) => message == expected,
        _ => false,
    }
}

/// 校验持久未启动终态证据的身份和数量上限。
fn validate_unstarted_turn_termination_records(
    records: &[UnstartedTurnTerminationRecord],
) -> Result<(), CollaborationPortError> {
    if records.len() > MAX_UNSTARTED_TURN_TERMINATION_RECORDS
        || records.iter().any(|record| {
            record.agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                || record.turn_id.as_str().is_empty()
                || record.root_turn_id.as_str().is_empty()
                || record.parent_turn_id.as_str().is_empty()
                || record.agent_path.trim().is_empty()
                || !valid_waiting_capacity_agent_path(&record.agent_path)
                || record.task.trim().is_empty()
                || record.prompt_summary.trim().is_empty()
                || matches!(&record.termination, UnstartedTurnTermination::Failed { message } if message.trim().is_empty())
        })
        || records
            .iter()
            .enumerate()
            .any(|(index, record)| records[..index].iter().any(|previous| {
                previous.agent_id == record.agent_id && previous.turn_id == record.turn_id
            }))
    {
        return Err(CollaborationPortError::new(
            "等待容量取消证据身份、数量或唯一性无效",
        ));
    }
    Ok(())
}

/// 校验等待容量取消证据只引用合法的单层子 Agent 路径。
fn valid_waiting_capacity_agent_path(value: &str) -> bool {
    keencode_agent::AgentPath::parse(value)
        .map(|path| path.as_str() != "/root")
        .unwrap_or(false)
}

/// 计算局部 Agent checkpoint 文件的稳定 SHA-256 摘要。
fn collaboration_agent_checksum(
    schema: &str,
    version: u32,
    session: &str,
    checkpoint: &RecoveredAgentCheckpoint,
) -> Result<String, CollaborationPortError> {
    let bytes = serde_json::to_vec(&CollaborationAgentChecksum {
        schema,
        version,
        session,
        checkpoint,
    })
    .map_err(|_| CollaborationPortError::new("Collaboration Agent checksum 无法序列化"))?;
    Ok(sha256_hex(&bytes))
}

/// 校验完整提交的全部根树和事件都归属于当前 Session Store。
fn validate_transition_session(
    commit: &CollaborationTransitionCommit,
    session_id: &str,
) -> Result<(), CollaborationPortError> {
    if commit
        .checkpoint
        .roots
        .iter()
        .any(|root| root.root_session_id.as_str() != session_id)
    {
        return Err(CollaborationPortError::new(
            "Collaboration 提交包含跨 Session 状态",
        ));
    }
    Ok(())
}

/// 返回任意字节内容的 SHA-256 小写十六进制摘要。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

/// 判断 checksum 是否是规范的 64 位小写十六进制文本。
fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// 一批动态输入在 Transcript 首行保存的可恢复确认水位。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DynamicInputMarker {
    /// 固定协议 Schema，恢复时拒绝误把普通用户文本当作水位。
    schema: String,
    /// 水位所属根 Runtime Session。
    session_id: String,
    /// 实际消费动态输入的根或单层子 Agent。
    agent_id: String,
    /// 水位所属的唯一 Turn。
    turn_id: String,
    /// 本条聚合消息携带的输入类型。
    kind: DynamicInputMarkerKind,
    /// 本次已原子写入 Transcript 的最大单调序号。
    through_sequence: u64,
}

/// 动态输入水位只允许 mailbox 与用户 Steer 两种来源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DynamicInputMarkerKind {
    /// Agent 间持久 mailbox 前缀。
    Mailbox,
    /// 当前 Turn 的用户追加引导。
    UserSteer,
}

/// 根命令层在调度 Coordinator 前冻结、由执行端按 TurnId 一次消费的请求。
struct PreparedRootTurn {
    /// 当前 Provider 注册表代次解析出的不可变模型端点。
    provider: ResolvedProvider,
    /// Session 模型快照中的可选推理强度。
    reasoning_effort: Option<ReasoningEffortSnapshot>,
    /// 本根 Turn 新增且必须进入权威 Transcript 的消息，例如用户输入。
    input_messages: Vec<Message>,
    /// Memory、Plan 或 Ultra 等只在 Provider 历史之前装配的 is_meta 消息；不得进入权威 Transcript。
    request_context: Vec<Message>,
    /// Runtime TurnStarted 使用的稳定用户输入摘要。
    summary: String,
    /// 供命令层同时观察“尚未形成起点即失败”的一次性完成通知；错误必须保留
    /// 稳定的 Runtime 枚举，避免命令层只能看到无上下文的“根回合准备失败”。
    completion: oneshot::Sender<Result<(), AgentRuntimeError>>,
}

/// 首次注册固定根 Agent 时从当前 Session Turn 冻结的基础配置。
struct RootAgentSeed {
    /// 当前实际解析的模型标识。
    model: String,
    /// 当前推理强度的稳定文本快照。
    reasoning_effort: Option<String>,
    /// 本 Turn 已生效且子 Agent 不得放宽的 Plan 守卫。
    plan_guard: PlanGuard,
}

/// 从已校验的持久协调器中恢复首次根注册时冻结的配置。
fn recovered_root_agent_seed(checkpoint: &RecoveredCoordinator) -> Option<RootAgentSeed> {
    checkpoint
        .roots
        .first()?
        .known_agents
        .iter()
        .find(|agent| agent.depth == AgentDepth::ROOT)
        .map(|root| RootAgentSeed {
            model: root.profile.model.clone(),
            reasoning_effort: root.profile.reasoning_effort.clone(),
            plan_guard: root.profile.plan_guard,
        })
}

/// 执行端当前托管的一条运行中 Turn。
#[derive(Clone)]
struct ManagedRuntimeTurn {
    /// 执行当前 Turn 的根或单层子 Agent。
    agent_id: RunnerAgentId,
    /// 当前 Turn 所属的 Agent 深度；后台任务列表只投影单层子 Agent。
    agent_depth: AgentDepth,
    /// 当前 Agent Turn 启动时记录的单行摘要。
    summary: String,
    /// 当前 Agent Turn 启动时记录的 Unix 毫秒时间戳。
    started_at_unix_ms: u64,
    /// 用于计算持续时间的进程内单调时钟。
    started: Instant,
    /// 与 Coordinator 和 Runner 共用的唯一取消令牌。
    cancellation: TurnCancellation,
    /// Runner 已经形成、但 Coordinator 尚未确认接收的稳定终态。
    terminal_outcome: Option<AgentTurnOutcome>,
}

/// 从 Coordinator checkpoint 读取、用于崩溃恢复确认的当前动态输入 claim。
#[derive(Clone, Debug, Eq, PartialEq)]
struct RecoveredDynamicInputClaim {
    /// claim 所属 Agent。
    agent_id: RunnerAgentId,
    /// claim 当前绑定的 Turn；可能不同于 Transcript marker 中的旧 Turn。
    turn_id: AgentTurnId,
    /// mailbox 或用户 Steer 输入类别。
    kind: DynamicInputMarkerKind,
    /// claim 覆盖的最大单调序号。
    through_sequence: u64,
    /// mailbox claim 对应且必须先在 Runtime Journal 标记 Delivered 的消息标识。
    mailbox_message_ids: Vec<ResourceMailboxMessageId>,
    /// checkpoint 中与消息标识、序号、路由和正文绑定的完整 mailbox 前缀。
    mailbox_messages: Vec<RunnerMailboxMessage>,
    /// 用户 steer claim 的完整前缀；恢复 ack 必须核对正文和资源身份而非仅水位。
    user_steers: Vec<keencode_agent::UserSteer>,
}

/// 把 Runner 已完成模型 Round 的明确 Token 用量同步累计到项目级 Goal。
///
/// 时间不走模型 Round：Round 之间的工具执行、子 Agent 等待与排队间隙同样是
/// 目标的真实执行时长，统一由根回合终态的 `commit_goal_turn_elapsed` 按墙钟
/// 一次性累计，避免按 Round 累计造成漏记或重复计数。
struct RuntimeGoalUsageSink {
    /// 当前唯一根 Runtime Session 标识。
    session_id: String,
    /// 项目级 Goal 的持久事务出口。
    persistent_state: Arc<PersistentAgentState>,
    /// Goal 首次变化后向同项目打开 Session 广播的装配根弱引用。
    owner: Weak<AgentRuntime>,
}

/// 会话首次 Turn 前按 Agent 冻结的稳定提示词事实。
///
/// 这是上下文冻结（#13）的核心取舍：指令正文、日期、时区、cwd 派生值和
/// capability/catalog 段在冻结时点计算一次，会话内复用快照，使模型请求的
/// System 段跨 Turn 字节稳定，为 prompt cache 断点（#12）打基础。
///
/// 敏感面盘点：下列变化会破坏稳定前缀，均按低频事件接受并显式记录。
/// - AGENTS.md / CLAUDE.md / CLAUDE.local.md 外部修改：本会话不生效（日期
///   跨午夜同理，快照不随时钟漂移），只有新会话取新值。
/// - `spawn_agent` / `Skill` 能力启停或扩展目录内容变化：触发本结构体的
///   低频重建（`refreshed`），只重算能力段与目录，环境与指令仍保持冻结值。
/// - MCP 工具表变化：工具定义数组随请求变化属于合法缓存失效；能力指纹
///   不变时稳定前缀本身不受影响。
/// - 进程重启（执行端 `RuntimeAgentExecution` 重建）后，同一逻辑 Session 的
///   各 Agent 会在自身首次 Turn 前重新冻结（新时钟、新指令快照），与跨午夜
///   快照语义一致：冻结值属于当前进程内的执行端实例，不跨进程迁移。
struct FrozenAgentPrompt {
    /// 冻结的环境事实快照；mode 按本轮 Plan 守卫逐轮渲染。
    environment: crate::agent_prompt::EnvironmentSnapshot,
    /// 冻结的全局与项目自定义指令正文。
    custom_instructions: String,
    /// 冻结时点渲染的能力说明段。
    capabilities: String,
    /// 冻结时点的能力指纹；变化时触发低频重建。
    capability_fingerprint: (bool, bool),
    /// 冻结的扩展目录文本；空表示无目录。
    catalog: String,
    /// 小上下文只发送独立核心提示词，不拼接能力、目录或自定义指令。
    small_context: bool,
}

/// 构建冻结提示词时随工具快照变化的能力输入，避免调用方传递一组易错位的标量。
struct FrozenPromptContext<'a> {
    cwd: &'a Path,
    can_spawn: bool,
    has_skill: bool,
    catalog: &'a str,
    small_context: bool,
    /// 只控制项目级指令；全局 AGENTS.md 仍由 Runtime 强制保留。
    inject_agents_md: bool,
}

/// 一次隔离模型生成的完整请求参数；Provider 由调用路径单独绑定。
struct IsolatedGenerationRequest<'a> {
    session_id: &'a str,
    system_prompt: &'a str,
    input: &'a str,
    timeout_secs: u64,
    purpose: &'static str,
    structured_output: Option<StructuredOutputConfig>,
}

impl FrozenAgentPrompt {
    /// 能力指纹或目录变化时重建能力说明与目录，环境与指令保持首次冻结值。
    ///
    /// 会话内安装 MCP、启用技能或扩展热重载属于低频事件，被接受为合法的
    /// prompt cache 失效；重建时记录诊断而不静默漂移。
    fn refreshed(
        frozen: &Self,
        can_spawn: bool,
        has_skill: bool,
        catalog: &str,
        small_context: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            environment: frozen.environment.clone(),
            custom_instructions: frozen.custom_instructions.clone(),
            capabilities: crate::agent_prompt::capabilities(can_spawn, has_skill),
            capability_fingerprint: (can_spawn, has_skill),
            catalog: if small_context {
                String::new()
            } else {
                catalog.to_owned()
            },
            small_context,
        })
    }

    /// 组装跨 Turn 字节稳定的请求前缀：System 规则、能力说明加冻结指令、目录殿后。
    fn stable_prefix(&self) -> Vec<Message> {
        if self.small_context {
            return vec![Message::text(
                MessageRole::System,
                self.environment.render_small_context_core(),
            )];
        }
        let mut capabilities = self.capabilities.clone();
        if !self.custom_instructions.is_empty() {
            capabilities.push_str("\n\n");
            capabilities.push_str(&self.custom_instructions);
        }
        let mut prefix = vec![
            Message::text(MessageRole::System, crate::agent_prompt::core()),
            Message::text(MessageRole::System, capabilities),
        ];
        if !self.catalog.is_empty() {
            prefix.push(Message::text(MessageRole::Developer, self.catalog.clone()));
        }
        prefix
    }
}

/// Session 首次完整 context 冻结的 Composer 扩展引用目录。
///
/// 该投影只保存已过滤的公开元数据和已校验来源路径，不持有 Skill 正文、
/// 插件配置或凭据。使用 `Value` 是为了复用前端现有 zcode catalog 合同，
/// 但数据只能由不可变扩展候选生成。
#[derive(Clone, Debug)]
pub struct SessionExtensionReferenceCatalog {
    /// Session 首次 context 时的插件身份目录。
    pub plugins: Vec<Value>,
    /// Session 首次 context 时的 Skill 引用目录。
    pub skills: Vec<Value>,
}

/// 判断 Session 是否已经产生用户可见的持久事实。
///
/// deferred draft 只允许在没有这些事实时刷新扩展目录；一旦首发或其他命令
/// 写入 Journal，目录必须继续由该 Session 的首次完整 context 固定，避免热替换
/// 把已开始对话的 Skill 能力边界改成另一份事实。
fn session_has_persisted_user_facts(state: &SessionState) -> bool {
    !state.turns.is_empty()
        || !state.transcript.is_empty()
        || !state.input_queue.items.is_empty()
        || !state.input_queue.completions.is_empty()
        || !state.dynamic_input_receipts.is_empty()
        || !state.model_rounds.is_empty()
        || !state.tools.is_empty()
        || !state.terminals.is_empty()
        || !state.todos.items.is_empty()
        || state.plan.plan_artifact.is_some()
        || !state.workflow_events.is_empty()
        || !state.sub_agents.is_empty()
        || !state.mailbox.is_empty()
        || !state.worktrees.is_empty()
}

/// `RuntimeAgentExecution` 内全部需要同步线性化的易失状态。
#[derive(Default)]
struct RuntimeAgentExecutionState {
    /// 已经越过执行端副作用起点且尚未被 Coordinator 接收终态的 Turn。
    accepted_turns: HashSet<AgentTurnId>,
    /// 尚未完成执行与 Coordinator 终态回传的托管 Turn。
    running_turns: HashMap<AgentTurnId, ManagedRuntimeTurn>,
    /// 根命令层已准备但尚未由 Coordinator 交付的 Turn。
    prepared_root_turns: HashMap<AgentTurnId, PreparedRootTurn>,
    /// 已经完成全树静止确认的根 Agent。
    quiesced_roots: HashSet<RunnerAgentId>,
    /// 最近一次已经向当前 Session 发送过诊断的扩展候选代次。
    extension_diagnostics_generation: Option<u64>,
    /// 各 Agent 在自身首次 Turn 前冻结的稳定提示词事实。
    frozen_prompts: HashMap<RunnerAgentId, Arc<FrozenAgentPrompt>>,
    /// 首次完整 Agent context 装配时冻结的 UI 扩展引用目录；不会随工作区热替换漂移。
    frozen_extension_reference_catalog: Option<SessionExtensionReferenceCatalog>,
    /// 按历史 Turn 记录其启动时使用的 Provider 快照，供 opaque reasoning 续传兼容性判断。
    historical_provider_by_turn: Option<HashMap<String, ProviderSnapshot>>,
}

/// `frozen_prompts` 的软上限；超过后在下次冻结插入前惰性收缩一次。
///
/// 执行端口没有子 Agent 终态注销回调：`quiesce_tree`/`close_tree` 只在 Session
/// 关闭时触发（此时整个执行端状态随之回收），Coordinator 的空闲 Agent 驱逐
/// 不回调执行端口，而逐 Turn 释放会让 root 的跨 Turn 稳定前缀失效。因此选择
/// 软上限收缩作为最小替代：单会话内冻结条目数始终有界；被收缩后再次活跃的
/// 子 Agent 会按当前事实重新冻结，属于与能力段重建同级的合法一次性缓存失效。
const FROZEN_PROMPT_SOFT_LIMIT: usize = 256;

/// 超过软上限时只保留 root、当前托管 Turn 涉及的 Agent 与即将插入的 Agent。
fn retain_live_frozen_prompts(
    frozen_prompts: &mut HashMap<RunnerAgentId, Arc<FrozenAgentPrompt>>,
    running_agent_ids: impl Iterator<Item = RunnerAgentId>,
    incoming_agent_id: &RunnerAgentId,
) {
    if frozen_prompts.len() < FROZEN_PROMPT_SOFT_LIMIT {
        return;
    }
    let running: HashSet<RunnerAgentId> = running_agent_ids.collect();
    frozen_prompts.retain(|agent_id, _| {
        agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
            || agent_id == incoming_agent_id
            || running.contains(agent_id)
    });
}

/// `RuntimeAgentExecution` 的 Session 级装配依赖；集中资源接线，避免构造函数参数顺序
/// 成为资源接线的隐含契约。
struct RuntimeAgentExecutionContext {
    /// 回到桌面装配根以读取热替换 Provider、扩展、Web 和宿主边界。
    owner: Weak<AgentRuntime>,
    /// 全部根与子 Turn 共用的权威 Runtime Session。
    session: RuntimeSession,
    /// Session 创建时绑定的规范项目根。
    project_root: PathBuf,
    /// Todo、Goal 与 Plan 的唯一生产持久控制器。
    persistent_state: Arc<PersistentAgentState>,
    /// 跨 Turn 共享且关闭时必须完整回收的后台进程管理器。
    background_tasks: Arc<BackgroundTaskManager>,
    /// Native Host 装配的共享 Tokio 执行器；同步清理线程只借用此 Handle 驱动关闭。
    executor_handle: tokio::runtime::Handle,
    /// 只接受不透明 lease 的 Session 独占 Git Worktree 管理器。
    worktrees: Arc<GitWorktreeLeaseManager>,
    /// 与协调器原子提交共享的持久补偿账本，启动前必须完成其 Journal 对账。
    store: Arc<SessionCollaborationStore>,
}

/// Session 级 V2 执行端：真正创建 Runner 任务并管理取消、静止与系统清理。
struct RuntimeAgentExecution {
    /// 与协调器原子提交共享的持久补偿账本，启动前必须完成其 Journal 对账。
    store: Arc<SessionCollaborationStore>,
    /// 回到桌面装配根以读取热替换 Provider、扩展、Web 和宿主边界。
    owner: Weak<AgentRuntime>,
    /// 全部根与子 Turn 共用的权威 Runtime Session。
    session: RuntimeSession,
    /// 与 `session` 一致的稳定文本标识。
    session_id: String,
    /// Session 创建时绑定的规范项目根。
    project_root: PathBuf,
    /// Todo、Goal 与 Plan 的唯一生产持久控制器。
    persistent_state: Arc<PersistentAgentState>,
    /// 跨 Turn 共享且关闭时必须完整回收的后台进程管理器。
    background_tasks: Arc<BackgroundTaskManager>,
    /// Native Host 装配的共享 Tokio 执行器；同步清理线程只借用此 Handle 驱动关闭。
    executor_handle: tokio::runtime::Handle,
    /// 只接受不透明 lease 的 Session 独占 Git Worktree 管理器。
    worktrees: Arc<GitWorktreeLeaseManager>,
    /// 反向弱绑定避免 Coordinator 与执行端形成强引用环。
    coordinator: OnceLock<Weak<CollaborationCoordinator>>,
    /// 托管 Turn、准备请求和幂等记录的同步状态。
    state: Arc<Mutex<RuntimeAgentExecutionState>>,
    /// 退出或 Session 拆除开始后禁止新的 Runner 进入执行副作用边界。
    accepting_work: AtomicBool,
    lifecycle_start_state: LifecycleStartState,
    /// 全树静止等待托管 Turn 数量归零的条件变量。
    idle: Arc<Condvar>,
}

/// Runner 内的 Goal 工具仍复用同一个持久控制器；该委托只负责把实际变化转成
/// Runtime 通知，设置页随后从 GoalFileStore 读取完整快照，不缓存工具返回正文。
struct RuntimeGoalController {
    state: Arc<PersistentAgentState>,
    owner: Weak<AgentRuntime>,
    session_id: String,
}

impl RuntimeGoalController {
    fn publish(&self, change: &GoalChange) {
        if !change.changed {
            return;
        }
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        owner.publish_goal_changed(
            &self.session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
    }
}

impl GoalController for RuntimeGoalController {
    fn goal_snapshot(&self) -> Result<keencode_agent::GoalSnapshot, RuntimeStateError> {
        self.state.goal_snapshot()
    }

    fn create_goal(
        &self,
        operation_id: &str,
        draft: GoalDraft,
    ) -> Result<GoalChange, RuntimeStateError> {
        let change = self.state.create_goal(operation_id, draft)?;
        self.publish(&change);
        Ok(change)
    }

    fn update_goal(
        &self,
        operation_id: &str,
        patch: GoalPatch,
    ) -> Result<GoalChange, RuntimeStateError> {
        let change = self.state.update_goal(operation_id, patch)?;
        self.publish(&change);
        Ok(change)
    }

    fn transition_goal(
        &self,
        operation_id: &str,
        transition: GoalTransition,
    ) -> Result<GoalChange, RuntimeStateError> {
        let change = self.state.transition_goal(operation_id, transition)?;
        self.publish(&change);
        Ok(change)
    }

    fn clear_goal(&self, operation_id: &str) -> Result<GoalChange, RuntimeStateError> {
        let change = self.state.clear_goal(operation_id)?;
        self.publish(&change);
        Ok(change)
    }

    fn record_goal_usage(
        &self,
        operation_id: &str,
        delta: GoalUsageDelta,
    ) -> Result<GoalChange, RuntimeStateError> {
        let change = self.state.record_goal_usage(operation_id, delta)?;
        self.publish(&change);
        Ok(change)
    }
}

/// 一个根 Session 的完整 Collaboration v2 生产装配。
struct SessionCollaborationRuntime {
    /// 唯一持久协调器。
    coordinator: Arc<CollaborationCoordinator>,
    /// 与协调器共享、用于后台取消和冷恢复对账的生产 Store。
    store: Arc<SessionCollaborationStore>,
    /// 唯一执行端，强引用由 Session 装配持有。
    execution: Arc<RuntimeAgentExecution>,
    /// 固定为 `root` 的应用层根 Agent 标识。
    root_agent_id: RunnerAgentId,
    /// 停止当前 Session 唯一后台 Shell 完成事件泵的单次信号。
    background_completion_cancel: Mutex<Option<oneshot::Sender<()>>>,
}

impl SessionCollaborationRuntime {
    /// 幂等停止后台 Shell 完成事件泵，避免 Session 关闭后继续发布任务终态。
    fn stop_background_completion_pump(&self) -> Result<(), AgentRuntimeError> {
        if let Some(cancel) = self
            .background_completion_cancel
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .take()
        {
            let _ = cancel.send(());
        }
        Ok(())
    }
}

impl Drop for SessionCollaborationRuntime {
    /// 非正常拆除路径也必须唤醒并结束后台完成事件泵。
    fn drop(&mut self) {
        if let Ok(cancel) = self.background_completion_cancel.get_mut()
            && let Some(cancel) = cancel.take()
        {
            let _ = cancel.send(());
        }
    }
}

/// `spawn_agent` 冻结父历史时读取同一权威 Runtime Transcript 的适配器。
struct RuntimeSpawnAgentContextSource {
    /// 当前根 Runtime Session。
    session: RuntimeSession,
}

/// 每次模型采样前从 Coordinator 两阶段 claim 动态输入的适配器。
struct RuntimeDynamicInputSource {
    /// claim 之后、镜像 mailbox 之前补齐其来源身份的持久化屏障。
    store: Arc<SessionCollaborationStore>,
    /// 当前根 Runtime Session 标识，防止跨 Session 误接线。
    session_id: String,
    /// 当前 Session 的唯一 Coordinator。
    coordinator: Arc<CollaborationCoordinator>,
    /// mailbox 镜像写入的唯一权威 Runtime Session。
    session: RuntimeSession,
}

/// Transcript 提交成功后按 mailbox、Steer 固定顺序完成两阶段确认。
struct RuntimeDynamicInputAcknowledgement {
    /// 当前 Session 的唯一 Coordinator。
    coordinator: Arc<CollaborationCoordinator>,
    /// mailbox Delivered 状态必须先提交到的权威 Runtime Session。
    session: RuntimeSession,
    /// 消费输入的 Agent。
    agent_id: RunnerAgentId,
    /// 消费输入的 Turn。
    turn_id: AgentTurnId,
    /// 非空 mailbox 批次的最大序号。
    mailbox_through_sequence: Option<u64>,
    /// 本批 mailbox 在资源层使用的稳定消息标识。
    mailbox_message_ids: Vec<ResourceMailboxMessageId>,
    /// 非空用户 Steer 批次的最大序号。
    steer_through_sequence: Option<u64>,
}

impl RuntimeAgentExecution {
    /// 创建尚未反向绑定 Coordinator 的 Session 执行端。
    fn new(context: RuntimeAgentExecutionContext) -> Self {
        let session_id = context.session.session_id().as_str().to_owned();
        Self {
            store: context.store,
            owner: context.owner,
            session: context.session,
            session_id,
            project_root: context.project_root,
            persistent_state: context.persistent_state,
            background_tasks: context.background_tasks,
            executor_handle: context.executor_handle,
            worktrees: context.worktrees,
            coordinator: OnceLock::new(),
            state: Arc::new(Mutex::new(RuntimeAgentExecutionState::default())),
            accepting_work: AtomicBool::new(true),
            lifecycle_start_state: LifecycleStartState::new(),
            idle: Arc::new(Condvar::new()),
        }
    }

    /// 在公开任何 Session 装配前完成唯一 Coordinator 反向绑定。
    fn bind_coordinator(
        &self,
        coordinator: &Arc<CollaborationCoordinator>,
    ) -> Result<(), AgentRuntimeError> {
        self.coordinator
            .set(Arc::downgrade(coordinator))
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 返回仍存活的 Coordinator，关闭后的悬空弱引用不得继续启动 Turn。
    fn coordinator(&self) -> Result<Arc<CollaborationCoordinator>, CollaborationPortError> {
        self.coordinator
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| CollaborationPortError::new("Collaboration 执行端尚未绑定协调器"))
    }

    /// 惰性读取并缓存历史 Turn 起点的 Provider 快照，避免每次新 Turn 重扫 Journal。
    fn historical_provider_snapshots(
        &self,
    ) -> Result<HashMap<String, ProviderSnapshot>, AgentRuntimeError> {
        if let Some(cached) = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .historical_provider_by_turn
            .clone()
        {
            return Ok(cached);
        }
        let loaded = historical_provider_snapshots_by_turn(&self.session)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        Ok(state
            .historical_provider_by_turn
            .get_or_insert(loaded)
            .clone())
    }

    /// 记录已经完成装配的 Turn Provider，供同一执行端后续 Turn 复用。
    fn remember_turn_provider(
        &self,
        turn_id: &AgentTurnId,
        provider: ProviderSnapshot,
    ) -> Result<(), AgentRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        state
            .historical_provider_by_turn
            .get_or_insert_with(HashMap::new)
            .insert(turn_id.as_str().to_owned(), provider);
        Ok(())
    }

    /// 将根请求发布到按 TurnId 去重的准备表，并返回命令层完成通知。
    fn prepare_root_turn(
        &self,
        turn_id: AgentTurnId,
        provider: ResolvedProvider,
        reasoning_effort: Option<ReasoningEffortSnapshot>,
        input_messages: Vec<Message>,
        request_context: Vec<Message>,
        summary: String,
    ) -> Result<oneshot::Receiver<Result<(), AgentRuntimeError>>, AgentRuntimeError> {
        let (completion, receiver) = oneshot::channel();
        if !self.accepting_work.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if !self.accepting_work.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        if state.accepted_turns.contains(&turn_id)
            || state.prepared_root_turns.contains_key(&turn_id)
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        state.prepared_root_turns.insert(
            turn_id,
            PreparedRootTurn {
                provider,
                reasoning_effort,
                input_messages,
                request_context,
                summary,
                completion,
            },
        );
        Ok(receiver)
    }

    /// 撤销尚未越过执行端副作用起点的根准备请求。
    fn discard_prepared_root_turn(&self, turn_id: &AgentTurnId) {
        if let Ok(mut state) = self.state.lock() {
            state.prepared_root_turns.remove(turn_id);
        }
    }

    /// 先封锁 Runner、准备表和后台进程入口，并取消当前已越过副作用边界的 Turn。
    ///
    /// 该方法只改变当前进程的易失执行状态；Coordinator 负责随后把对应领域 Turn
    /// 写成 Interrupted，不能在这里提前清空可恢复账本。
    fn begin_shutdown(&self) -> Result<(), AgentRuntimeError> {
        self.accepting_work.store(false, Ordering::Release);
        self.background_tasks.stop_accepting_tasks();
        let prepared = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            for turn in state.running_turns.values() {
                turn.cancellation.cancel();
            }
            let prepared = state
                .prepared_root_turns
                .drain()
                .map(|(_, prepared)| prepared)
                .collect::<Vec<_>>();
            self.idle.notify_all();
            prepared
        };
        for prepared in prepared {
            let _ = prepared
                .completion
                .send(Err(AgentRuntimeError::RuntimeClosed));
        }
        Ok(())
    }

    /// 等待 Runner 真实退出，再关闭后台进程管理器；保留 accepted 账本供诊断。
    fn finish_shutdown(&self) -> Result<(), AgentRuntimeError> {
        let deadline = Instant::now() + AGENT_TREE_QUIESCE_TIMEOUT;
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        while !state.running_turns.is_empty() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            };
            let (next, timeout) = self
                .idle
                .wait_timeout(state, remaining)
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            state = next;
            if timeout.timed_out() && !state.running_turns.is_empty() {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
        }
        drop(state);
        shutdown_background_tasks_blocking(
            Arc::clone(&self.background_tasks),
            self.executor_handle.clone(),
        )
        .map_err(runtime_operation_failed)
    }

    /// 判断命令层准备、Runner 执行或后台 Shell 是否仍持有活动工作。
    fn has_active_work(&self) -> Result<bool, AgentRuntimeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if !state.prepared_root_turns.is_empty() || !state.running_turns.is_empty() {
            return Ok(true);
        }
        drop(state);
        self.background_tasks
            .list_running()
            .map(|tasks| !tasks.is_empty())
            .map_err(runtime_operation_failed)
    }

    /// 关闭 Session 时取消并清空本地执行账本，后台进程由同一边界统一回收。
    fn stop_local_work_for_close(&self) -> Result<(), AgentRuntimeError> {
        let prepared = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            for turn in state.running_turns.values() {
                turn.cancellation.cancel();
            }
            let prepared = state
                .prepared_root_turns
                .drain()
                .map(|(_, prepared)| prepared)
                .collect::<Vec<_>>();
            state.running_turns.clear();
            state.accepted_turns.clear();
            self.idle.notify_all();
            prepared
        };
        for prepared in prepared {
            let _ = prepared
                .completion
                .send(Err(AgentRuntimeError::RuntimeClosed));
        }
        shutdown_background_tasks_blocking(
            Arc::clone(&self.background_tasks),
            self.executor_handle.clone(),
        )
        .map_err(runtime_operation_failed)
    }

    /// 为指定扩展候选登记一次性日志，避免每个子 Agent 重复记录。
    fn claim_extension_diagnostics(&self, generation: u64) -> Result<bool, AgentRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if state.extension_diagnostics_generation == Some(generation) {
            return Ok(false);
        }
        state.extension_diagnostics_generation = Some(generation);
        Ok(true)
    }
}

/// 统一释放 Runner 本地终态状态；提交失败时保留 accepted 标记供恢复护栏使用。
fn release_runtime_turn_state(
    state: &Mutex<RuntimeAgentExecutionState>,
    idle: &Condvar,
    turn_id: &AgentTurnId,
    completion_succeeded: bool,
) {
    if let Ok(mut state) = state.lock() {
        state.running_turns.remove(turn_id);
        if completion_succeeded {
            state.accepted_turns.remove(turn_id);
        }
        idle.notify_all();
    }
}

impl SpawnAgentContextSource for RuntimeSpawnAgentContextSource {
    /// 按权威 Turn 起点顺序返回父 Agent 已进入唯一终态的有效 Transcript 分组。
    fn completed_turns(
        &self,
        context: &SpawnAgentTemplateContext,
    ) -> Result<Vec<CompletedTurnContext>, keencode_agent::ToolError> {
        if context.session_id.as_str() != self.session.session_id().as_str() {
            return Err(keencode_agent::ToolError::permanent(
                "agent_context_session_mismatch",
                "父 Transcript 请求不属于当前根 Session",
            ));
        }
        let source_agent_id = keencode_resources::AgentId::new(
            context.parent_agent_id.as_str().to_owned(),
        )
        .map_err(|_| {
            keencode_agent::ToolError::permanent(
                "agent_context_invalid",
                "父 Agent 标识无法映射到权威 Transcript",
            )
        })?;
        let snapshot = self.session.snapshot().map_err(|_| {
            keencode_agent::ToolError::retryable(
                "agent_context_unavailable",
                "当前无法读取父 Transcript 快照",
            )
        })?;
        let effective = snapshot
            .state
            .effective_transcript(&source_agent_id)
            .map_err(|_| {
                keencode_agent::ToolError::permanent(
                    "agent_context_invalid",
                    "父 Transcript 历史无法通过一致性校验",
                )
            })?;
        let completed_turn_ids = snapshot
            .state
            .turns
            .values()
            .filter(|turn| {
                turn.source_agent_id == source_agent_id
                    && turn.status != TurnStatus::Running
                    && turn.completed_at_unix_ms.is_some()
            })
            .map(|turn| turn.turn_id.clone())
            .collect::<HashSet<_>>();
        let mut grouped = Vec::<(String, Vec<Message>)>::new();
        for message in effective {
            let materialized = self.session.materialize_message(&message).map_err(|_| {
                keencode_agent::ToolError::permanent(
                    "agent_context_invalid",
                    "父 Transcript 消息 Artifact 无法安全物化",
                )
            })?;
            let is_compaction_summary = message.agent_id.as_ref() == Some(&source_agent_id)
                && message.turn_id.as_ref().is_some_and(|turn_id| {
                    snapshot.state.applied_compactions().any(|compaction| {
                        compaction.source_agent_id == source_agent_id
                            && &compaction.turn_id == turn_id
                            && materialized.role == MessageRole::User
                            && materialized.content.first().is_some_and(|content| {
                                matches!(
                                    content,
                                    ContentBlock::Text { text }
                                        if text == &format!(
                                            "{COMPACTION_SUMMARY_PREFIX}{}",
                                            compaction.record.summary
                                        )
                                )
                            })
                    })
                });
            let is_completed = message
                .turn_id
                .as_ref()
                .is_some_and(|turn_id| completed_turn_ids.contains(turn_id));
            if !is_completed && !is_compaction_summary {
                break;
            }
            let group_key = if is_compaction_summary {
                format!("compaction:{}", message.message_id)
            } else {
                message
                    .turn_id
                    .as_ref()
                    .expect("已完成 Transcript 消息必须绑定 Turn")
                    .as_str()
                    .to_owned()
            };
            if let Some((previous_key, messages)) = grouped.last_mut()
                && previous_key == &group_key
            {
                messages.push(materialized);
            } else {
                grouped.push((group_key, vec![materialized]));
            }
        }
        Ok(grouped
            .into_iter()
            .map(|(_, messages)| CompletedTurnContext { messages })
            .collect())
    }
}

impl AgentDynamicInputSource for RuntimeDynamicInputSource {
    /// 在采样前 claim mailbox 与当前 Turn 的 Steer；最终候选边界只 claim Steer。
    fn claim(
        &self,
        session_id: &AgentSessionId,
        turn_id: &AgentTurnId,
        source_agent_id: &RunnerAgentId,
        boundary: AgentDynamicInputBoundary,
        maximum: usize,
    ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError> {
        if session_id.as_str() != self.session_id {
            return Err(AgentDynamicInputError::new(
                "动态输入请求不属于当前根 Session",
            ));
        }
        let mailbox = if matches!(boundary, AgentDynamicInputBoundary::BeforeModelSampling) {
            self.coordinator
                .consume_mailbox(source_agent_id, turn_id, maximum)
                .map_err(|error| {
                    AgentDynamicInputError::new(format!("无法 claim Agent mailbox：{error}"))
                })?
        } else {
            Vec::new()
        };
        let steers = self
            .coordinator
            .consume_user_steers(source_agent_id, turn_id)
            .map_err(|error| {
                AgentDynamicInputError::new(format!("无法 claim 用户 Steer：{error}"))
            })?;
        // 必须位于 claim 之后，覆盖与本次 claim 并发提交的子 Turn 失败通知。
        if !mailbox.is_empty() {
            self.store
                .reconcile_pending_unstarted_turns()
                .map_err(|error| {
                    AgentDynamicInputError::new(format!("无法对账未启动 Agent 终态：{error}"))
                })?;
        }
        let mut mailbox_message_ids = Vec::with_capacity(mailbox.len());
        for message in &mailbox {
            let message_id = ResourceMailboxMessageId::new(message.message_id.as_str().to_owned())
                .map_err(|_| AgentDynamicInputError::new("Agent mailbox 消息标识无效"))?;
            let related_turn_id = message
                .related_turn_id
                .as_ref()
                .ok_or_else(|| AgentDynamicInputError::new("Agent mailbox 缺少来源 Turn"))?;
            self.session
                .queue_mailbox_message(ResourceMailboxMessage {
                    message_id: message_id.clone(),
                    from: ResourceAgentId::new(message.source_agent_id.as_str().to_owned())
                        .map_err(|_| AgentDynamicInputError::new("Agent mailbox 来源无效"))?,
                    to: ResourceAgentId::new(message.target_agent_id.as_str().to_owned())
                        .map_err(|_| AgentDynamicInputError::new("Agent mailbox 目标无效"))?,
                    related_turn_id: ResourceTurnId::new(related_turn_id.as_str().to_owned())
                        .map_err(|_| AgentDynamicInputError::new("Agent mailbox 来源 Turn 无效"))?,
                    body: message.content.clone(),
                    artifact: None,
                    state: MailboxState::Queued,
                })
                .map_err(|error| {
                    AgentDynamicInputError::new(format!("无法镜像 Agent mailbox：{error}"))
                })?;
            mailbox_message_ids.push(message_id);
        }
        if mailbox.is_empty() && steers.is_empty() {
            return Ok(AgentDynamicInputBatch::empty());
        }

        let mailbox_through_sequence = mailbox.last().map(|message| message.sequence);
        let steer_through_sequence = steers.last().map(|steer| steer.sequence);
        let mut messages = Vec::with_capacity(2);
        if let Some(through_sequence) = mailbox_through_sequence {
            let marker = DynamicInputMarker {
                schema: DYNAMIC_INPUT_MARKER_SCHEMA.to_owned(),
                session_id: self.session_id.clone(),
                agent_id: source_agent_id.as_str().to_owned(),
                turn_id: turn_id.as_str().to_owned(),
                kind: DynamicInputMarkerKind::Mailbox,
                through_sequence,
            };
            let mut body = dynamic_input_marker_line(&marker)?;
            body.push_str("\n以下是本轮安全边界前已持久排队的 Agent mailbox 消息：");
            for message in &mailbox {
                let kind = match &message.kind {
                    MailboxMessageKind::AgentMessage => "agent_message",
                    MailboxMessageKind::ChildTurnFinished { .. } => "child_turn_finished",
                };
                body.push_str(&format!(
                    "\n\n[sequence={} from_path={} kind={kind}]\n{}",
                    message.sequence,
                    message.source_agent_path.as_str(),
                    message.content
                ));
            }
            messages.push(Message::text(MessageRole::Developer, body));
        }
        if let Some(through_sequence) = steer_through_sequence {
            let marker = DynamicInputMarker {
                schema: DYNAMIC_INPUT_MARKER_SCHEMA.to_owned(),
                session_id: self.session_id.clone(),
                agent_id: source_agent_id.as_str().to_owned(),
                turn_id: turn_id.as_str().to_owned(),
                kind: DynamicInputMarkerKind::UserSteer,
                through_sequence,
            };
            let mut body = dynamic_input_marker_line(&marker)?;
            append_user_steer_body(&mut body, &steers);
            // marker 与正文必须留在模型可见的 User 消息里，但整条消息是内部消费
            // 协议，不能作为用户发言展示。mailbox 分支靠 Developer 角色天然隐藏，
            // steer 受恢复校验约束必须是 User，只能靠 is_meta 保持投影一致。
            let mut message = Message::text(MessageRole::User, body);
            message.is_meta = true;
            messages.push(message);
        }
        let mut receipts = Vec::with_capacity(2);
        if let Some(through_sequence) = mailbox_through_sequence {
            receipts.push(AgentDynamicInputReceipt::new(
                AgentDynamicInputKind::Mailbox,
                through_sequence,
            ));
        }
        if let Some(through_sequence) = steer_through_sequence {
            receipts.push(
                AgentDynamicInputReceipt::new(AgentDynamicInputKind::UserSteer, through_sequence)
                    .with_user_messages(
                        steers
                            .iter()
                            .map(|steer| {
                                let mut message = Message::text(MessageRole::User, &steer.content);
                                message.references = steer.references.clone();
                                (steer.sequence, message)
                            })
                            .collect(),
                    ),
            );
        }
        Ok(AgentDynamicInputBatch::new_with_receipts(
            messages,
            receipts,
            Arc::new(RuntimeDynamicInputAcknowledgement {
                coordinator: Arc::clone(&self.coordinator),
                session: self.session.clone(),
                agent_id: source_agent_id.clone(),
                turn_id: turn_id.clone(),
                mailbox_through_sequence,
                mailbox_message_ids,
                steer_through_sequence,
            }),
        ))
    }
}

impl AgentDynamicInputAcknowledgement for RuntimeDynamicInputAcknowledgement {
    /// mailbox 先确认；若 Steer 失败，Runner 重试时 mailbox 的幂等空确认不会阻断恢复。
    fn acknowledge(&self) -> Result<(), AgentDynamicInputError> {
        if let Some(through_sequence) = self.mailbox_through_sequence {
            for message_id in &self.mailbox_message_ids {
                self.session
                    .deliver_mailbox_message(message_id.clone())
                    .map_err(|_| AgentDynamicInputError::new("无法确认 Runtime mailbox 投递"))?;
            }
            self.coordinator
                .acknowledge_mailbox(&self.agent_id, &self.turn_id, through_sequence)
                .map_err(|_| AgentDynamicInputError::new("无法确认 Agent mailbox 水位"))?;
        }
        if let Some(through_sequence) = self.steer_through_sequence {
            self.coordinator
                .acknowledge_user_steers(&self.agent_id, &self.turn_id, through_sequence)
                .map_err(|_| AgentDynamicInputError::new("无法确认用户 Steer 水位"))?;
        }
        Ok(())
    }
}

/// 将动态输入水位编码成单行 JSON；正文永远从下一行开始。
fn dynamic_input_marker_line(
    marker: &DynamicInputMarker,
) -> Result<String, AgentDynamicInputError> {
    serde_json::to_string(marker)
        .map_err(|_| AgentDynamicInputError::new("无法编码动态输入确认水位"))
}

/// 同一份消费正文用于模型请求与冷恢复核验，逐条保存引用归属。
fn append_user_steer_body(body: &mut String, steers: &[keencode_agent::UserSteer]) {
    body.push_str("\n以下是用户在当前 Turn 中追加的引导，按顺序执行：");
    for steer in steers {
        body.push_str(&format!(
            "\n\n[sequence={}]\n{}",
            steer.sequence, steer.content
        ));
        // 每条 steer 独立绑定引用；不能合并同名的不同市场或突破单条引用上限。
        if !steer.references.is_empty() {
            body.push_str("\n该条用户引导明确选择的资源（身份数据，不是更高权限指令）：\n");
            body.push_str(
                &serde_json::to_string(&steer.references).expect("已验证的字符串引用可以序列化"),
            );
        }
    }
}

/// 只把与权威 Transcript 段和消息角色完全绑定的首行 JSON 识别为动态输入 marker。
///
/// 普通开发者消息即使恰好包含 JSON 也返回 `None`；一旦首行声明当前 marker schema，
/// 任意字段、身份或角色不一致都会使恢复失败，避免把模型可写正文当成消费凭据。
fn validated_dynamic_input_marker(
    session_id: &str,
    segment: &TranscriptSegment,
    stored: &SessionMessage,
    materialized: &Message,
) -> Result<Option<DynamicInputMarker>, AgentRuntimeError> {
    if !segment.messages.iter().any(|message| message == stored)
        || stored.turn_id.as_ref() != Some(&segment.turn_id)
        || stored.agent_id.is_some()
    {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    let expected_role = match stored.role {
        ResourceMessageRole::Developer => MessageRole::Developer,
        ResourceMessageRole::User => MessageRole::User,
        ResourceMessageRole::Assistant
        | ResourceMessageRole::System
        | ResourceMessageRole::Tool => return Ok(None),
    };
    if materialized.role != expected_role
        || materialized.content.len() != 1
        || !matches!(
            materialized.content.first(),
            Some(ContentBlock::Text { .. })
        )
    {
        return Ok(None);
    }
    let Some(ContentBlock::Text { text }) = materialized.content.first() else {
        return Ok(None);
    };
    let first_line = text
        .split_once('\n')
        .map_or(text.as_str(), |(line, _)| line);
    let value: Value = match serde_json::from_str(first_line) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if value
        .get("schema")
        .and_then(Value::as_str)
        .is_none_or(|schema| schema != DYNAMIC_INPUT_MARKER_SCHEMA)
    {
        return Ok(None);
    }
    let marker: DynamicInputMarker =
        serde_json::from_value(value).map_err(runtime_operation_failed)?;
    if marker.session_id != session_id
        || marker.agent_id != segment.source_agent_id.as_str()
        || marker.turn_id != segment.turn_id.as_str()
        || marker.through_sequence == 0
    {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    let role_matches_kind = matches!(
        (marker.kind, expected_role),
        (DynamicInputMarkerKind::Mailbox, MessageRole::Developer)
            | (DynamicInputMarkerKind::UserSteer, MessageRole::User)
    );
    if !role_matches_kind {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    Ok(Some(marker))
}

/// 从恢复 checkpoint 提取当前仍未确认的 mailbox 与用户 Steer claim。
fn recovered_dynamic_input_claims(
    checkpoint: Option<&RecoveredCoordinator>,
) -> Result<Vec<RecoveredDynamicInputClaim>, AgentRuntimeError> {
    let mut claims = Vec::new();
    for agent in checkpoint
        .into_iter()
        .flat_map(|checkpoint| &checkpoint.roots)
        .flat_map(|root| &root.agents)
    {
        if let Some((turn_id, through_sequence)) = agent
            .mailbox_claim_turn_id
            .clone()
            .zip(agent.mailbox_claim_through_sequence)
        {
            let mailbox_messages = agent
                .mailbox
                .iter()
                .take_while(|entry| entry.message.sequence <= through_sequence)
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>();
            if mailbox_messages.is_empty()
                || mailbox_messages.last().map(|message| message.sequence) != Some(through_sequence)
                || mailbox_messages
                    .windows(2)
                    .any(|messages| messages[0].sequence >= messages[1].sequence)
            {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            let mailbox_message_ids = mailbox_messages
                .iter()
                .map(|message| {
                    ResourceMailboxMessageId::new(message.message_id.as_str().to_owned())
                        .map_err(runtime_operation_failed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            claims.push(RecoveredDynamicInputClaim {
                agent_id: agent.definition.agent_id.clone(),
                turn_id,
                kind: DynamicInputMarkerKind::Mailbox,
                through_sequence,
                mailbox_message_ids,
                mailbox_messages,
                user_steers: Vec::new(),
            });
        }
        if let Some((turn_id, through_sequence)) = agent
            .steer_claim_turn_id
            .clone()
            .zip(agent.steer_claim_through_sequence)
        {
            let user_steers = agent
                .pending_steers
                .iter()
                .take_while(|steer| steer.sequence <= through_sequence)
                .cloned()
                .collect::<Vec<_>>();
            if user_steers.is_empty()
                || user_steers.last().map(|steer| steer.sequence) != Some(through_sequence)
                || user_steers.iter().any(|steer| {
                    steer.turn_id != turn_id
                        || keencode_model::InputReference::validate_all(&steer.references).is_err()
                })
            {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            claims.push(RecoveredDynamicInputClaim {
                agent_id: agent.definition.agent_id.clone(),
                turn_id,
                kind: DynamicInputMarkerKind::UserSteer,
                through_sequence,
                mailbox_message_ids: Vec::new(),
                mailbox_messages: Vec::new(),
                user_steers,
            });
        }
    }
    claims.sort_by(|left, right| {
        let kind_order = |kind: DynamicInputMarkerKind| match kind {
            DynamicInputMarkerKind::Mailbox => 0_u8,
            DynamicInputMarkerKind::UserSteer => 1_u8,
        };
        (
            kind_order(left.kind),
            left.agent_id.as_str(),
            left.through_sequence,
        )
            .cmp(&(
                kind_order(right.kind),
                right.agent_id.as_str(),
                right.through_sequence,
            ))
    });
    Ok(claims)
}

/// 从权威 Transcript 中提取指定 Agent/Turn 最后一条非空 Assistant 普通文本。
///
/// 结果摘要没有独立的 Runtime Journal 字段，必须从同一 Turn 的持久 Transcript
/// 重建。只接受身份完全匹配的 Assistant 消息；Artifact 文本必须通过同一 Runtime
/// Session 物化，不能静默丢弃。
fn recovered_agent_final_message(
    state: &SessionState,
    session: Option<&RuntimeSession>,
    agent_id: &ResourceAgentId,
    turn_id: &ResourceTurnId,
) -> Result<Option<String>, AgentRuntimeError> {
    for record in &state.transcript {
        let messages: &[SessionMessage] = match record {
            TranscriptRecord::MessageAdded(message) => std::slice::from_ref(message),
            TranscriptRecord::SegmentCommitted(segment) => &segment.messages,
            TranscriptRecord::CompactionApplied(_) => continue,
        };
        for message in messages {
            if message.turn_id.as_ref() != Some(turn_id) {
                continue;
            }
            if matches!(
                message.role,
                ResourceMessageRole::Assistant | ResourceMessageRole::Tool
            ) && message.agent_id.as_ref() != Some(agent_id)
            {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            if message.role != ResourceMessageRole::Assistant {
                continue;
            }
            if session.is_none()
                && message
                    .content
                    .iter()
                    .any(|part| matches!(part, ResourceMessagePart::Artifact { .. }))
            {
                // Artifact 内容必须由同一 Runtime Session 读取；没有
                // Session 时不能静默跳过任意一段持久结果。
                return Err(AgentRuntimeError::RecoveryRequired);
            }
        }
    }

    // 先定位最终的非空正文再读取 Artifact，避免恢复一次结果时重新物化全部
    // 历史模型轮次；末尾只有工具/推理块时继续向前寻找最后一条非空正文。
    for record in state.transcript.iter().rev() {
        let messages: &[SessionMessage] = match record {
            TranscriptRecord::MessageAdded(message) => std::slice::from_ref(message),
            TranscriptRecord::SegmentCommitted(segment) => &segment.messages,
            TranscriptRecord::CompactionApplied(_) => continue,
        };
        for message in messages.iter().rev() {
            if message.turn_id.as_ref() != Some(turn_id)
                || message.role != ResourceMessageRole::Assistant
            {
                continue;
            }
            let text = if let Some(session) = session {
                let materialized = session
                    .materialize_message(message)
                    .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
                if materialized.role != MessageRole::Assistant {
                    return Err(AgentRuntimeError::RecoveryRequired);
                }
                last_non_empty_text(&materialized.content).map(str::to_owned)
            } else {
                if message
                    .content
                    .iter()
                    .any(|part| matches!(part, ResourceMessagePart::Artifact { .. }))
                {
                    // Artifact 内容必须由同一 Runtime Session 读取；没有
                    // Session 时静默跳过会把不完整结果伪装成完整摘要。
                    return Err(AgentRuntimeError::RecoveryRequired);
                }
                message.content.iter().rev().find_map(|part| match part {
                    ResourceMessagePart::Text { text } => {
                        (!text.trim().is_empty()).then(|| text.to_owned())
                    }
                    ResourceMessagePart::Artifact { .. } => None,
                    ResourceMessagePart::Reasoning { .. }
                    | ResourceMessagePart::Image { .. }
                    | ResourceMessagePart::ToolCall { .. }
                    | ResourceMessagePart::ToolResult { .. } => None,
                })
            };
            if let Some(text) = text {
                return Ok(Some(bounded_collaboration_failure(&text)));
            }
        }
    }
    Ok(None)
}

/// 判断指定 Agent/Turn 是否至少有一条权威 Assistant 消息。
fn transcript_has_agent_assistant(
    state: &SessionState,
    agent_id: &ResourceAgentId,
    turn_id: &ResourceTurnId,
) -> bool {
    state.transcript.iter().any(|record| {
        let messages: &[SessionMessage] = match record {
            TranscriptRecord::MessageAdded(message) => std::slice::from_ref(message),
            TranscriptRecord::SegmentCommitted(segment) => &segment.messages,
            TranscriptRecord::CompactionApplied(_) => return false,
        };
        messages.iter().any(|message| {
            message.turn_id.as_ref() == Some(turn_id)
                && message.agent_id.as_ref() == Some(agent_id)
                && message.role == ResourceMessageRole::Assistant
        })
    })
}

/// 将 Runtime Journal 中同一 Agent Turn 的已落盘终态转换为 Collaboration 恢复结果。
fn authoritative_recovered_turn_outcome(
    state: &SessionState,
    agent_id: &RunnerAgentId,
    turn_id: &AgentTurnId,
    session: Option<&RuntimeSession>,
) -> Result<Option<AgentTurnOutcome>, AgentRuntimeError> {
    let resource_agent_id =
        ResourceAgentId::new(agent_id.as_str().to_owned()).map_err(runtime_operation_failed)?;
    let resource_turn_id =
        ResourceTurnId::new(turn_id.as_str().to_owned()).map_err(runtime_operation_failed)?;
    let Some(turn) = state.turns.get(&resource_turn_id) else {
        return Ok(None);
    };
    if turn.turn_id != resource_turn_id || turn.source_agent_id != resource_agent_id {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    let outcome = match turn.status {
        TurnStatus::Running => return Ok(None),
        TurnStatus::Completed => {
            let final_message = recovered_agent_final_message(
                state,
                session,
                &resource_agent_id,
                &resource_turn_id,
            )?;
            if resource_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                && !transcript_has_agent_assistant(state, &resource_agent_id, &resource_turn_id)
            {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            AgentTurnOutcome::Completed {
                final_message: if resource_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
                    final_message
                } else {
                    final_message.or_else(|| {
                        state
                            .sub_agents
                            .get(&resource_agent_id)
                            .filter(|agent| {
                                agent.current_turn_id.as_ref() == Some(&resource_turn_id)
                            })
                            .and_then(|agent| agent.result_summary.clone())
                    })
                },
            }
        }
        TurnStatus::Cancelled => AgentTurnOutcome::Interrupted,
        TurnStatus::Failed => {
            let message = turn
                .outcome_message
                .as_ref()
                .filter(|message| !message.trim().is_empty())
                .cloned()
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            AgentTurnOutcome::Failed {
                message: redacted_collaboration_failure(&message),
            }
        }
    };
    Ok(Some(outcome))
}

/// 将 Collaboration 的 Turn 原因转换为 Runtime 起点使用的稳定摘要。
fn collaboration_turn_prompt_summary(
    cause: &AgentTurnCause,
    prompt: Option<&str>,
    root_plan_guard: Option<PlanGuard>,
) -> Result<Option<String>, AgentRuntimeError> {
    if prompt.is_some_and(|value| value.trim().is_empty()) {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    match cause {
        AgentTurnCause::RootUser => {
            let prompt = prompt.ok_or(AgentRuntimeError::RecoveryRequired)?;
            Ok(root_plan_guard.map(|guard| {
                root_turn_summary(
                    prompt,
                    None,
                    matches!(guard.state(), PlanGuardState::ReadOnly),
                )
            }))
        }
        AgentTurnCause::InitialTask => {
            let prompt = prompt.ok_or(AgentRuntimeError::RecoveryRequired)?;
            Ok(Some(prompt.trim().chars().take(256).collect::<String>()))
        }
        AgentTurnCause::Followup { .. } => {
            if prompt.is_some() {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            Ok(Some("Agent mailbox followup".to_owned()))
        }
        AgentTurnCause::Retry { .. } => Ok(Some(
            prompt
                .map(|value| value.trim().chars().take(256).collect::<String>())
                .unwrap_or_else(|| "Agent mailbox followup".to_owned()),
        )),
    }
}

/// 找出一个完整 Collaboration checkpoint 中的 Agent。
fn recovered_agent_for_id<'a>(
    checkpoint: &'a RecoveredCoordinator,
    agent_id: &RunnerAgentId,
) -> Option<&'a RecoveredAgent> {
    checkpoint
        .roots
        .iter()
        .flat_map(|root| &root.agents)
        .find(|agent| agent.definition.agent_id == *agent_id)
}

/// 将一个持久等待容量取消证据还原为 Runtime 对账请求，并校验完整不可变字段。
fn unstarted_turn_termination_request_from_record(
    session: &RuntimeSession,
    checkpoint: &RecoveredCoordinator,
    record: &UnstartedTurnTerminationRecord,
) -> Result<UnstartedTurnTerminationRequest, AgentRuntimeError> {
    let definition = checkpoint
        .roots
        .iter()
        .filter(|root| root.root_agent_id == record.parent_agent_id)
        .flat_map(|root| {
            root.agents
                .iter()
                .map(|agent| &agent.definition)
                .chain(root.known_agents.iter())
        })
        .find(|definition| definition.agent_id == record.agent_id)
        .ok_or(AgentRuntimeError::RecoveryRequired)?;
    if definition.depth != AgentDepth::CHILD
        || definition.root_session_id.as_str() != session.session_id().as_str()
        || definition.root_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        || definition.parent_agent_id.as_ref() != Some(&record.parent_agent_id)
        || definition.path.as_str() != record.agent_path
    {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    let resource_agent_id = ResourceAgentId::new(record.agent_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    let (status, result_summary) = match &record.termination {
        UnstartedTurnTermination::Interrupted => (SubAgentStatus::Interrupted, None),
        UnstartedTurnTermination::Failed { message } => {
            (SubAgentStatus::Failed, Some(message.clone()))
        }
    };
    Ok(UnstartedTurnTerminationRequest {
        agent: SubAgentState {
            agent_id: resource_agent_id,
            parent_agent_id: ResourceAgentId::new(record.parent_agent_id.as_str().to_owned())
                .map_err(|_| AgentRuntimeError::RecoveryRequired)?,
            agent_path: record.agent_path.clone(),
            task: record.task.clone(),
            status,
            current_turn_id: Some(
                ResourceTurnId::new(record.turn_id.as_str().to_owned())
                    .map_err(|_| AgentRuntimeError::RecoveryRequired)?,
            ),
            result_summary,
        },
        turn_id: ResourceTurnId::new(record.turn_id.as_str().to_owned())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?,
        root_turn_id: ResourceTurnId::new(record.root_turn_id.as_str().to_owned())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?,
        parent_turn_id: ResourceTurnId::new(record.parent_turn_id.as_str().to_owned())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?,
        prompt_summary: record.prompt_summary.clone(),
        initial_task: record.initial_task,
        termination: record.termination.clone(),
    })
}

/// 对账已由 Store 原子持久化的未启动终态，先确认 Journal 再逐项删除 receipt。
fn reconcile_unstarted_turn_termination_records(
    session: &RuntimeSession,
    store: &SessionCollaborationStore,
    checkpoint: &RecoveredCoordinator,
    records: &[UnstartedTurnTerminationRecord],
) -> Result<(), AgentRuntimeError> {
    for record in records {
        let request = unstarted_turn_termination_request_from_record(session, checkpoint, record)?;
        match session.record_unstarted_turn_termination(request) {
            Ok(_) => {}
            Err(RuntimeError::RecoveryRequired | RuntimeError::InvalidTurnRequest) => {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            Err(error) => return Err(runtime_operation_failed(error)),
        }
        store
            .acknowledge_unstarted_turn_terminations(std::slice::from_ref(record))
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    }
    Ok(())
}

/// 冷恢复仅依据 Store 持久的双事件证据补齐尚未启动的 Runtime Turn，缺证据即拒绝恢复。
fn reconcile_cold_unstarted_turn_terminations(
    session: &RuntimeSession,
    store: &SessionCollaborationStore,
    state: &SessionState,
    checkpoint: Option<&RecoveredCoordinator>,
    records: &[UnstartedTurnTerminationRecord],
) -> Result<(), AgentRuntimeError> {
    let Some(checkpoint) = checkpoint else {
        return if records.is_empty() {
            Ok(())
        } else {
            Err(AgentRuntimeError::RecoveryRequired)
        };
    };
    for agent in checkpoint.roots.iter().flat_map(|root| &root.agents) {
        if matches!(
            agent.status,
            CollaborationAgentStatus::Interrupted { .. } | CollaborationAgentStatus::Failed { .. }
        ) && let Some(last_turn) = agent.last_turn.as_ref()
            && matches!(
                last_turn.outcome,
                AgentTurnOutcome::Interrupted | AgentTurnOutcome::Failed { .. }
            )
            && !state
                .turns
                .keys()
                .any(|turn_id| turn_id.as_str() == last_turn.turn_id.as_str())
            && !records.iter().any(|record| {
                record.agent_id == agent.definition.agent_id && record.turn_id == last_turn.turn_id
            })
        {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
    }
    reconcile_unstarted_turn_termination_records(session, store, checkpoint, records)
}

/// 为 Collaboration 根 Turn 解析 Journal 摘要所需的外部 Plan 守卫，并校验输入摘要绑定。
fn collaboration_root_plan_guard(
    checkpoint: &RecoveredCoordinator,
    agent: &RecoveredAgent,
    turn_id: &AgentTurnId,
    cause: &AgentTurnCause,
    prompt: Option<&str>,
    current_plan_guard: Option<PlanGuard>,
) -> Result<Option<PlanGuard>, AgentRuntimeError> {
    let binding = checkpoint
        .root_turn_bindings
        .iter()
        .find(|binding| binding.turn_id.as_str() == turn_id.as_str());
    if !matches!(cause, AgentTurnCause::RootUser) {
        if binding.is_some() {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        return Ok(None);
    }
    if agent.definition.depth != AgentDepth::ROOT {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    if let Some(binding) = binding {
        if binding.root_agent_id.as_str() != agent.definition.root_agent_id.as_str()
            || prompt.is_none_or(|value| root_turn_prompt_digest(value) != binding.prompt_digest)
            || current_plan_guard.is_some_and(|guard| guard != binding.plan_guard)
        {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        return Ok(Some(binding.plan_guard));
    }
    // 仅允许协调器内部自动分配的根 Turn 没有外部绑定；生产命令层使用的外部 Turn 必须有绑定。
    let internal_prefix = format!("turn/{}/", agent.definition.root_agent_id.as_str());
    if !turn_id.as_str().starts_with(&internal_prefix) {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    Ok(current_plan_guard)
}

/// 严格核验 Journal 与 Collaboration 对同一 Turn 的身份、谱系、摘要和终态是否一致。
// Journal 与 Collaboration 的字段必须保持显式对应，合并参数结构会掩盖各自的权威边界。
#[allow(clippy::too_many_arguments)]
fn validate_journal_turn_correspondence(
    state: &SessionState,
    session: Option<&RuntimeSession>,
    agent: &RecoveredAgent,
    turn_id: &AgentTurnId,
    cause: &AgentTurnCause,
    prompt: Option<&str>,
    parent_turn_id: Option<&AgentTurnId>,
    root_turn_id: &AgentTurnId,
    root_plan_guard: Option<PlanGuard>,
    expected_outcome: Option<&AgentTurnOutcome>,
) -> Result<Option<AgentTurnOutcome>, AgentRuntimeError> {
    let resource_agent_id = ResourceAgentId::new(agent.definition.agent_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    let resource_turn_id = ResourceTurnId::new(turn_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    let resource_root_turn_id = ResourceTurnId::new(root_turn_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    let resource_parent_turn_id = parent_turn_id
        .map(|parent| ResourceTurnId::new(parent.as_str().to_owned()))
        .transpose()
        .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
    let Some(turn) = state.turns.get(&resource_turn_id) else {
        return if expected_outcome.is_some() {
            Err(AgentRuntimeError::RecoveryRequired)
        } else {
            Ok(None)
        };
    };
    if turn.turn_id != resource_turn_id
        || turn.source_agent_id != resource_agent_id
        || turn.root_turn_id != resource_root_turn_id
        || turn.parent_turn_id != resource_parent_turn_id
    {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    if let Some(expected_summary) =
        collaboration_turn_prompt_summary(cause, prompt, root_plan_guard)?
    {
        // 根输入引用来自权威用户消息；恢复不能只重算正文摘要或从当前插件目录猜选择。
        let expected_summary = if matches!(cause, AgentTurnCause::RootUser) {
            let inputs = state
                .transcript
                .iter()
                .flat_map(|record| match record {
                    TranscriptRecord::MessageAdded(message) => std::slice::from_ref(message),
                    TranscriptRecord::SegmentCommitted(segment) => segment.messages.as_slice(),
                    TranscriptRecord::CompactionApplied(_) => &[],
                })
                .filter(|message| {
                    message.role == ResourceMessageRole::User
                        && !message.is_meta
                        && message.agent_id.is_none()
                        && message.turn_id.as_ref() == Some(&resource_turn_id)
                })
                .collect::<Vec<_>>();
            if inputs.len() > 1 {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            let references = inputs
                .first()
                .map_or(&[][..], |message| message.references.as_slice());
            keencode_model::InputReference::validate_all(references)
                .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
            append_input_reference_digest(expected_summary, references)?
        } else {
            expected_summary
        };
        if turn.prompt_summary != expected_summary {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
    }
    if turn.status == TurnStatus::Running {
        return if expected_outcome.is_some() {
            Err(AgentRuntimeError::RecoveryRequired)
        } else {
            Ok(None)
        };
    }
    let actual_outcome =
        authoritative_recovered_turn_outcome(state, &agent.definition.agent_id, turn_id, session)?
            .ok_or(AgentRuntimeError::RecoveryRequired)?;
    if expected_outcome.is_some_and(|expected| expected != &actual_outcome) {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    if matches!(
        &agent.status,
        CollaborationAgentStatus::Cancelling {
            turn_id: cancelling_turn_id
        } if cancelling_turn_id == turn_id
    ) {
        // Journal 终态可能先于取消回调落盘；在线路径会把同一竞态归一为 Interrupted。
        return Ok(Some(AgentTurnOutcome::Interrupted));
    }
    Ok(Some(actual_outcome))
}

/// 只收集 Open 根树未决 Turn 在 Runtime Journal 中已经形成的唯一权威终态；
/// Closing 根树仅校验 Journal 对应关系，避免越过全树静止边界写回完成通知。
/// 同时拒绝已终态 checkpoint 缺少 Journal 终态或因果字段不一致的冷启动。
#[cfg(test)]
fn recovered_authoritative_turn_outcomes(
    checkpoint: Option<&RecoveredCoordinator>,
    state: &SessionState,
) -> Result<HashMap<AgentTurnId, AgentTurnOutcome>, AgentRuntimeError> {
    recovered_authoritative_turn_outcomes_with_waiting_capacity(
        None,
        checkpoint,
        state,
        &HashSet::new(),
    )
}

/// 冷恢复时允许仅对有持久 WaitingCapacity 证据的未启动中断 Turn 暂缓 Runtime 对账。
fn recovered_authoritative_turn_outcomes_with_waiting_capacity(
    session: Option<&RuntimeSession>,
    checkpoint: Option<&RecoveredCoordinator>,
    state: &SessionState,
    waiting_capacity_turns: &HashSet<AgentTurnId>,
) -> Result<HashMap<AgentTurnId, AgentTurnOutcome>, AgentRuntimeError> {
    let mut outcomes = HashMap::new();
    let Some(checkpoint) = checkpoint else {
        return Ok(outcomes);
    };
    for (lifecycle, agent) in checkpoint
        .roots
        .iter()
        .flat_map(|root| root.agents.iter().map(move |agent| (root.lifecycle, agent)))
    {
        if let Some(last_turn) = agent.last_turn.as_ref() {
            let root_plan_guard = collaboration_root_plan_guard(
                checkpoint,
                agent,
                &last_turn.turn_id,
                &last_turn.cause,
                last_turn.prompt.as_deref(),
                None,
            )?;
            let journal_contains_turn = state
                .turns
                .keys()
                .any(|known| known.as_str() == last_turn.turn_id.as_str());
            let is_waiting_capacity_turn = waiting_capacity_turns.contains(&last_turn.turn_id)
                && matches!(last_turn.outcome, AgentTurnOutcome::Interrupted);
            if journal_contains_turn || !is_waiting_capacity_turn {
                validate_journal_turn_correspondence(
                    state,
                    session,
                    agent,
                    &last_turn.turn_id,
                    &last_turn.cause,
                    last_turn.prompt.as_deref(),
                    last_turn.parent_turn_id.as_ref(),
                    &last_turn.root_turn_id,
                    root_plan_guard,
                    Some(&last_turn.outcome),
                )?
                .ok_or(AgentRuntimeError::RecoveryRequired)?;
            }
        } else if matches!(
            agent.status,
            CollaborationAgentStatus::Completed { .. }
                | CollaborationAgentStatus::Interrupted { .. }
                | CollaborationAgentStatus::Failed { .. }
        ) {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        let Some(turn_id) = agent.status.active_turn_id() else {
            continue;
        };
        let cause = agent
            .current_turn_cause
            .as_ref()
            .ok_or(AgentRuntimeError::RecoveryRequired)?;
        let root_turn_id = agent
            .current_root_turn_id
            .as_ref()
            .ok_or(AgentRuntimeError::RecoveryRequired)?;
        let root_plan_guard = collaboration_root_plan_guard(
            checkpoint,
            agent,
            turn_id,
            cause,
            agent.current_turn_prompt.as_deref(),
            agent.current_plan_guard,
        )?;
        let outcome = validate_journal_turn_correspondence(
            state,
            session,
            agent,
            turn_id,
            cause,
            agent.current_turn_prompt.as_deref(),
            agent.current_parent_turn_id.as_ref(),
            root_turn_id,
            root_plan_guard,
            None,
        )?;
        if lifecycle == RecoveredRootLifecycle::Open
            && outcome.is_some_and(|outcome| outcomes.insert(turn_id.clone(), outcome).is_some())
        {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
    }
    Ok(outcomes)
}

/// 只读取权威资源层回执，并确认已写入但 Coordinator 尚未确认的 claim。
fn recover_dynamic_input_acknowledgements(
    session: &RuntimeSession,
    coordinator: &CollaborationCoordinator,
    claims: &[RecoveredDynamicInputClaim],
) -> Result<(), AgentRuntimeError> {
    if claims.is_empty() {
        return Ok(());
    }
    let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
    for claim in claims {
        let was_committed = validate_dynamic_input_claim(session, &snapshot.state, claim)?;
        if !was_committed {
            continue;
        }
        match claim.kind {
            DynamicInputMarkerKind::Mailbox => {
                for message_id in &claim.mailbox_message_ids {
                    session
                        .deliver_mailbox_message(message_id.clone())
                        .map_err(runtime_operation_failed)?;
                }
                coordinator.acknowledge_mailbox(
                    &claim.agent_id,
                    &claim.turn_id,
                    claim.through_sequence,
                )
            }
            DynamicInputMarkerKind::UserSteer => coordinator.acknowledge_user_steers(
                &claim.agent_id,
                &claim.turn_id,
                claim.through_sequence,
            ),
        }
        .map_err(runtime_operation_failed)?;
    }
    Ok(())
}

/// 在同一进程的后续根 Turn 启动前重新对账未确认动态输入 claim。
///
/// 冷启动时 `ensure_collaboration_runtime` 已经执行过一次恢复，但 live
/// Coordinator 会跨 Turn 保留在内存中；若上一个 Turn 在 Transcript 提交后
/// ack 失败，下一次根 Turn 仍必须依据同一组三重证据完成确认，不能把旧 claim
/// 重绑定到新 Turn 并再次写入相同动态正文。
fn reconcile_live_dynamic_input_acknowledgements(
    session: &RuntimeSession,
    coordinator: &CollaborationCoordinator,
) -> Result<(), AgentRuntimeError> {
    let checkpoint = coordinator
        .checkpoint_coordinator()
        .map_err(runtime_operation_failed)?;
    let claims = recovered_dynamic_input_claims(Some(&checkpoint))?;
    if let Err(error) = recover_dynamic_input_acknowledgements(session, coordinator, &claims) {
        return Err(if error == AgentRuntimeError::RuntimeOperationFailed {
            AgentRuntimeError::RecoveryRequired
        } else {
            error
        });
    }
    let checkpoint_after = coordinator
        .checkpoint_coordinator()
        .map_err(runtime_operation_failed)?;
    let remaining = recovered_dynamic_input_claims(Some(&checkpoint_after))?;
    if remaining.is_empty() {
        Ok(())
    } else {
        // 没有完整三重证据的 claim 不能重绑定到下一 Turn，否则会重复消费正文。
        Err(AgentRuntimeError::RecoveryRequired)
    }
}

/// 以资源层权威回执的 Agent、Turn、类别和水位四元组匹配一个未确认 claim。
///
/// 回执由 Runtime 与动态 Transcript 段原子写入；模型可见正文中的 marker 只用于诊断，
/// 不能作为恢复确认依据，也不能让同一 Agent 的不同 Turn 互相确认。
#[cfg(test)]
fn dynamic_input_receipt_matches_claim(
    state: &SessionState,
    claim: &RecoveredDynamicInputClaim,
) -> bool {
    let kind = match claim.kind {
        DynamicInputMarkerKind::Mailbox => ResourceDynamicInputKind::Mailbox,
        DynamicInputMarkerKind::UserSteer => ResourceDynamicInputKind::UserSteer,
    };
    state.dynamic_input_receipts.iter().any(|receipt| {
        receipt.source_agent_id.as_str() == claim.agent_id.as_str()
            && receipt.turn_id.as_str() == claim.turn_id.as_str()
            && receipt.kind == kind
            && receipt.through_sequence == claim.through_sequence
    })
}

/// 将 Coordinator checkpoint 的 mailbox 前缀与同一 Session 的权威邮箱记录逐项对账。
///
/// checkpoint 只保存协作层消息；若恢复时直接按其中的 ID 调用投递接口，篡改后的
/// checkpoint 可能把另一封消息误标为 Delivered。因此必须同时核对序号、路由、来源
/// Turn 和正文。已是 Delivered 的消息允许幂等重试，它可能来自上一次部分确认。
fn validate_recovered_mailbox_claim(
    state: &SessionState,
    claim: &RecoveredDynamicInputClaim,
) -> Result<(), AgentRuntimeError> {
    if !matches!(claim.kind, DynamicInputMarkerKind::Mailbox) {
        return Ok(());
    }
    if claim.mailbox_messages.is_empty()
        || claim.mailbox_message_ids.len() != claim.mailbox_messages.len()
    {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    let mut previous_sequence = None;
    let mut message_ids = HashSet::new();
    for (message_id, expected) in claim
        .mailbox_message_ids
        .iter()
        .zip(&claim.mailbox_messages)
    {
        if expected.sequence == 0
            || previous_sequence.is_some_and(|previous| previous >= expected.sequence)
            || !message_ids.insert(message_id.clone())
            || message_id.as_str() != expected.message_id.as_str()
            || expected.target_agent_id.as_str() != claim.agent_id.as_str()
        {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        previous_sequence = Some(expected.sequence);
        let source = ResourceAgentId::new(expected.source_agent_id.as_str().to_owned())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
        let target = ResourceAgentId::new(expected.target_agent_id.as_str().to_owned())
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
        let related_turn = expected
            .related_turn_id
            .as_ref()
            .ok_or(AgentRuntimeError::RecoveryRequired)
            .and_then(|turn_id| {
                ResourceTurnId::new(turn_id.as_str().to_owned())
                    .map_err(|_| AgentRuntimeError::RecoveryRequired)
            })?;
        let Some(actual) = state.mailbox.get(message_id) else {
            return Err(AgentRuntimeError::RecoveryRequired);
        };
        if actual.message_id != *message_id
            || actual.from != source
            || actual.to != target
            || actual.related_turn_id != related_turn
            || actual.body != expected.content
            || actual.artifact.is_some()
            || !matches!(actual.state, MailboxState::Queued | MailboxState::Delivered)
        {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
    }
    if previous_sequence != Some(claim.through_sequence) {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    Ok(())
}

/// 重建 mailbox 动态消息的完整模型可见正文，防止只校验 marker 水位而忽略正文绑定。
fn expected_mailbox_dynamic_input_text(
    session: &RuntimeSession,
    claim: &RecoveredDynamicInputClaim,
) -> Result<String, AgentRuntimeError> {
    let marker = DynamicInputMarker {
        schema: DYNAMIC_INPUT_MARKER_SCHEMA.to_owned(),
        session_id: session.session_id().as_str().to_owned(),
        agent_id: claim.agent_id.as_str().to_owned(),
        turn_id: claim.turn_id.as_str().to_owned(),
        kind: DynamicInputMarkerKind::Mailbox,
        through_sequence: claim.through_sequence,
    };
    let mut body = dynamic_input_marker_line(&marker).map_err(runtime_operation_failed)?;
    body.push_str("\n以下是本轮安全边界前已持久排队的 Agent mailbox 消息：");
    for message in &claim.mailbox_messages {
        let kind = match &message.kind {
            MailboxMessageKind::AgentMessage => "agent_message",
            MailboxMessageKind::ChildTurnFinished { .. } => "child_turn_finished",
        };
        body.push_str(&format!(
            "\n\n[sequence={} from_path={} kind={kind}]\n{}",
            message.sequence,
            message.source_agent_path.as_str(),
            message.content
        ));
    }
    Ok(body)
}

/// 在恢复确认前把动态 claim 与 Journal 中的 receipt、Transcript 段和 marker 三重对账。
///
/// receipt 是唯一的消费权威；Transcript/marker 仅作为同一原子批次的结构证据，任何
/// 缺失、重复或身份不一致都停止恢复，不能把 checkpoint 中的可伪造水位直接当成已消费。
fn validate_dynamic_input_claim(
    session: &RuntimeSession,
    state: &SessionState,
    claim: &RecoveredDynamicInputClaim,
) -> Result<bool, AgentRuntimeError> {
    validate_recovered_mailbox_claim(state, claim)?;
    let kind = match claim.kind {
        DynamicInputMarkerKind::Mailbox => ResourceDynamicInputKind::Mailbox,
        DynamicInputMarkerKind::UserSteer => ResourceDynamicInputKind::UserSteer,
    };
    let receipts = state
        .dynamic_input_receipts
        .iter()
        .filter(|receipt| {
            receipt.source_agent_id.as_str() == claim.agent_id.as_str()
                && receipt.turn_id.as_str() == claim.turn_id.as_str()
                && receipt.kind == kind
                && receipt.through_sequence == claim.through_sequence
        })
        .collect::<Vec<_>>();
    let Some(receipt) = receipts.first() else {
        return Ok(false);
    };
    if receipts.len() != 1 {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    let segments = state
        .transcript
        .iter()
        .filter_map(|record| match record {
            TranscriptRecord::SegmentCommitted(segment)
                if segment.turn_id == receipt.turn_id
                    && segment.source_agent_id == receipt.source_agent_id
                    && segment.model_round == receipt.model_round
                    && segment.segment_index == receipt.segment_index =>
            {
                Some(segment)
            }
            TranscriptRecord::MessageAdded(_) | TranscriptRecord::CompactionApplied(_) => None,
            TranscriptRecord::SegmentCommitted(_) => None,
        })
        .collect::<Vec<_>>();
    let Some(segment) = segments.first() else {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    };
    if segments.len() != 1 {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    let mut matching_markers = 0_usize;
    for stored in &segment.messages {
        let materialized = session
            .materialize_message(stored)
            .map_err(runtime_operation_failed)?;
        if let Some(marker) = validated_dynamic_input_marker(
            session.session_id().as_str(),
            segment,
            stored,
            &materialized,
        )? {
            let marker_kind = match marker.kind {
                DynamicInputMarkerKind::Mailbox => ResourceDynamicInputKind::Mailbox,
                DynamicInputMarkerKind::UserSteer => ResourceDynamicInputKind::UserSteer,
            };
            if marker_kind == receipt.kind && marker.through_sequence == receipt.through_sequence {
                if matches!(claim.kind, DynamicInputMarkerKind::Mailbox) {
                    let expected = expected_mailbox_dynamic_input_text(session, claim)?;
                    let exact = materialized.role == MessageRole::Developer
                        && materialized.content.len() == 1
                        && matches!(
                            materialized.content.first(),
                            Some(ContentBlock::Text { text }) if text == &expected
                        );
                    if !exact {
                        return Err(AgentRuntimeError::RecoveryRequired);
                    }
                }
                if matches!(claim.kind, DynamicInputMarkerKind::UserSteer) {
                    let expected_inputs = claim
                        .user_steers
                        .iter()
                        .map(|steer| keencode_resources::DynamicUserInput {
                            sequence: steer.sequence,
                            text: steer.content.clone(),
                            references: steer.references.clone(),
                        })
                        .collect::<Vec<_>>();
                    // 可选展示回执存在时必须与同一个 claim 完全相等，不能确认被替换的正文或市场。
                    if !receipt.user_inputs.is_empty() && receipt.user_inputs != expected_inputs {
                        return Err(AgentRuntimeError::RecoveryRequired);
                    }
                    let mut expected =
                        dynamic_input_marker_line(&marker).map_err(runtime_operation_failed)?;
                    append_user_steer_body(&mut expected, &claim.user_steers);
                    if !stored.is_meta
                        || materialized.content.len() != 1
                        || !matches!(materialized.content.first(), Some(ContentBlock::Text { text }) if text == &expected)
                    {
                        return Err(AgentRuntimeError::RecoveryRequired);
                    }
                }
                matching_markers = matching_markers.saturating_add(1);
            }
        }
    }
    if matching_markers != 1 {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    Ok(true)
}

impl RuntimeModelRoundUsageSink for RuntimeGoalUsageSink {
    /// 仅累计 Goal 所属 Session（含其子 Agent）的 Token 用量，其他项目对话不消耗该预算。
    fn commit(&self, usage: &ModelRoundUsage) -> Result<(), AgentCommitSinkError> {
        if usage.session_id().as_str() != self.session_id {
            return Err(AgentCommitSinkError::rejected(
                "模型 Round 用量不属于当前 Goal Session",
            ));
        }
        let snapshot = self.persistent_state.goal_snapshot().map_err(|_| {
            AgentCommitSinkError::indeterminate("无法读取模型 Round 对应的项目 Goal")
        })?;
        if snapshot.goal.as_ref().is_none_or(|goal| {
            goal.status.is_terminal() || goal.owner_session_id != self.session_id
        }) {
            return Ok(());
        }
        let reported = &usage.completion().usage;
        let tokens = reported.total_tokens.or_else(|| {
            reported
                .input_tokens
                .zip(reported.output_tokens)
                .and_then(|(input, output)| input.checked_add(output))
        });
        let operation_id = goal_usage_operation_id(&[
            usage.session_id().as_str(),
            usage.turn_id().as_str(),
            usage.source_agent_id().as_str(),
            usage.purpose().as_str(),
            &usage.model_round().to_string(),
            &usage.call_attempt().to_string(),
        ]);
        match self.persistent_state.record_goal_usage(
            &operation_id,
            GoalUsageDelta {
                tokens: tokens.unwrap_or(0),
                // 执行时长由根回合终态按墙钟统一累计，Round 只贡献 Token。
                elapsed_seconds: 0,
            },
        ) {
            Ok(change) => {
                if change.changed
                    && let Some(owner) = self.owner.upgrade()
                {
                    owner.publish_goal_changed(
                        &self.session_id,
                        change.current.goal.as_ref().map(|goal| goal.id.clone()),
                        change.current.revision,
                        change
                            .current
                            .goal
                            .as_ref()
                            .map(|goal| goal_status_name(goal.status).to_owned()),
                    );
                }
                Ok(())
            }
            Err(RuntimeStateError::NotFound { .. } | RuntimeStateError::Terminal { .. }) => Ok(()),
            Err(
                RuntimeStateError::Invalid { .. }
                | RuntimeStateError::Conflict { .. }
                | RuntimeStateError::CounterOverflow { .. },
            ) => Err(AgentCommitSinkError::rejected(
                "项目 Goal 拒绝模型 Round 用量",
            )),
            Err(RuntimeStateError::LockPoisoned | RuntimeStateError::Storage { .. }) => Err(
                AgentCommitSinkError::indeterminate("项目 Goal 用量提交结果不确定"),
            ),
        }
    }
}

/// 根回合终态时把整段墙钟执行时长累计进项目 Goal。
///
/// 回合墙钟覆盖模型 Round 之间的工具执行、子 Agent 等待与排队间隙，是目标
/// 的真实执行时长；模型 Round 用量因此只提交 Token。相同 Turn 的重复提交
/// 按幂等操作标识去重，失败只向调用方返回错误用于诊断，不影响回合终态。
fn commit_goal_turn_elapsed(
    session_id: &str,
    persistent_state: &PersistentAgentState,
    owner: &Weak<AgentRuntime>,
    turn_id: &str,
    elapsed_seconds: u64,
) -> Result<(), RuntimeStateError> {
    if elapsed_seconds == 0 {
        return Ok(());
    }
    let snapshot = persistent_state.goal_snapshot()?;
    if snapshot
        .goal
        .as_ref()
        .is_none_or(|goal| goal.status.is_terminal() || goal.owner_session_id != session_id)
    {
        return Ok(());
    }
    let operation_id =
        goal_usage_operation_id(&[session_id, turn_id, "root", "turn_wall", "0", "0"]);
    match persistent_state.record_goal_usage(
        &operation_id,
        GoalUsageDelta {
            tokens: 0,
            elapsed_seconds,
        },
    ) {
        Ok(change) => {
            if change.changed
                && let Some(owner) = owner.upgrade()
            {
                owner.publish_goal_changed(
                    session_id,
                    change.current.goal.as_ref().map(|goal| goal.id.clone()),
                    change.current.revision,
                    change
                        .current
                        .goal
                        .as_ref()
                        .map(|goal| goal_status_name(goal.status).to_owned()),
                );
            }
            Ok(())
        }
        Err(RuntimeStateError::NotFound { .. } | RuntimeStateError::Terminal { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

impl AgentExecutionPort for RuntimeAgentExecution {
    /// 完整预检 Provider、工具和 Runtime 请求后按 TurnId 幂等创建异步 Runner。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
        if self.store.reconcile_pending_unstarted_turns().is_err() {
            // 这是先前终态的持久对账屏障，不是当前 Turn 的永久拒绝。
            return AgentTurnStartResult::RetryableUnknown {
                error: CollaborationPortError::new("未启动 Agent 终态尚未完成 Journal 对账"),
            };
        }
        let mut prepared_root = match self.state.lock() {
            Ok(mut state) => {
                if !self.accepting_work.load(Ordering::Acquire) {
                    return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                        error: CollaborationPortError::new("Agent 执行端已进入关闭阶段"),
                    };
                }
                if state.accepted_turns.contains(&launch.turn_id) {
                    return AgentTurnStartResult::AlreadyAccepted;
                }
                state.prepared_root_turns.remove(&launch.turn_id)
            }
            Err(_) => {
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                    error: CollaborationPortError::new("Agent 执行端状态不可用"),
                };
            }
        };
        let Some(owner) = self.owner.upgrade() else {
            if let Some(prepared) = prepared_root.take() {
                let _ = prepared
                    .completion
                    .send(Err(AgentRuntimeError::RuntimeOperationFailed));
            }
            return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                error: CollaborationPortError::new("Agent Runtime 已关闭"),
            };
        };
        let built = owner.build_runtime_launch(self, &launch, prepared_root.as_ref());
        let (runner, request, summary) = match built {
            Ok(built) => built,
            Err(error) => {
                if let Some(prepared) = prepared_root.take() {
                    let _ = prepared.completion.send(Err(error));
                }
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                    // Runtime 错误只有稳定说明，不包含文件正文、路径或 Provider 凭据。
                    error: CollaborationPortError::new(error.to_string()),
                };
            }
        };
        let turn_started_unix_ms;
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => {
                    if let Some(prepared) = prepared_root.take() {
                        let _ = prepared
                            .completion
                            .send(Err(AgentRuntimeError::StateUnavailable));
                    }
                    return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                        error: CollaborationPortError::new("Agent 执行端状态不可用"),
                    };
                }
            };
            if !self.accepting_work.load(Ordering::Acquire) {
                if let Some(prepared) = prepared_root.take() {
                    let _ = prepared
                        .completion
                        .send(Err(AgentRuntimeError::RuntimeClosed));
                }
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                    error: CollaborationPortError::new("Agent 执行端已进入关闭阶段"),
                };
            }
            if state.accepted_turns.contains(&launch.turn_id) {
                if let Some(prepared) = prepared_root.take() {
                    let _ = prepared.completion.send(Ok(()));
                }
                return AgentTurnStartResult::AlreadyAccepted;
            }
            state.accepted_turns.insert(launch.turn_id.clone());
            turn_started_unix_ms = unix_time_ms();
            state.running_turns.insert(
                launch.turn_id.clone(),
                ManagedRuntimeTurn {
                    agent_id: launch.agent.agent_id.clone(),
                    agent_depth: launch.agent.depth,
                    summary,
                    started_at_unix_ms: turn_started_unix_ms,
                    started: Instant::now(),
                    cancellation: launch.cancellation.clone(),
                    terminal_outcome: None,
                },
            );
        }
        let goal_elapsed_session = self.session_id.clone();
        let goal_elapsed_state = Arc::clone(&self.persistent_state);
        let goal_elapsed_owner = Weak::clone(&self.owner);
        let goal_elapsed_root_turn = launch.parent_turn_id.is_none();
        let completion = prepared_root.map(|prepared| prepared.completion);
        let coordinator = match self.coordinator() {
            Ok(coordinator) => coordinator,
            Err(error) => {
                if let Ok(mut state) = self.state.lock() {
                    state.running_turns.remove(&launch.turn_id);
                    state.accepted_turns.remove(&launch.turn_id);
                    self.idle.notify_all();
                }
                if let Some(completion) = completion {
                    let _ = completion.send(Err(AgentRuntimeError::RuntimeOperationFailed));
                }
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect { error };
            }
        };
        let execution_state = Arc::clone(&self.state);
        let execution_idle = Arc::clone(&self.idle);
        let agent_id = launch.agent.agent_id.clone();
        let turn_id = launch.turn_id.clone();
        let cancellation = launch.cancellation.clone();
        self.executor_handle.spawn(async move {
            let result = runner.run_turn(request).await;
            let pending_dynamic_input_acknowledgement = match result.as_ref() {
                Ok(result) => match result.error.as_ref() {
                    Some(AgentRunError::DynamicInputAcknowledgement { .. }) => true,
                    Some(AgentRunError::DynamicInput { .. }) => {
                        // `RuntimeDynamicInputSource::claim` 可能已经在 Coordinator 中建立
                        // claim，随后才在 Journal mailbox 镜像处失败；这时普通 complete_turn
                        // 会被 PendingInputClaim 拒绝，必须只对当前 Agent/Turn 保留 claim。
                        match coordinator_has_pending_dynamic_input_claim(
                            &coordinator,
                            &agent_id,
                            &turn_id,
                        ) {
                            Ok(pending) => pending,
                            Err(error) => {
                                tracing::warn!(
                                    target: "agent_runtime",
                                    turn_id = %turn_id,
                                    error = %error,
                                    "无法核对动态输入 claim，保留普通终态路径等待恢复"
                                );
                                false
                            }
                        }
                    }
                    _ => false,
                },
                Err(_) => {
                    // RuntimeSession 发现 Journal 镜像进入不确定状态时，会把内层
                    // DynamicInput 错误提升为外层 RecoveryRequired；此时仍须按当前
                    // Coordinator claim 收敛，否则普通 complete_turn 会永久占用槽位。
                    match coordinator_has_pending_dynamic_input_claim(
                        &coordinator,
                        &agent_id,
                        &turn_id,
                    ) {
                        Ok(pending) => pending,
                        Err(error) => {
                            tracing::warn!(
                                target: "agent_runtime",
                                turn_id = %turn_id,
                                error = %error,
                                "无法核对外层 Runtime 错误对应的动态输入 claim，保留普通终态路径等待恢复"
                            );
                            false
                        }
                    }
                }
            };
            let command_result = result
                .as_ref()
                .map(|_| ())
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed);
            let outcome = runtime_turn_outcome(result);
            if let Ok(mut state) = execution_state.lock()
                && let Some(turn) = state.running_turns.get_mut(&turn_id)
            {
                turn.terminal_outcome = Some(outcome.clone());
            }
            if let Some(completion) = completion {
                let _ = completion.send(command_result);
            }
            let mut retry_delay = Duration::from_millis(25);
            let completion_deadline = Instant::now() + RUNTIME_TURN_COMPLETION_TIMEOUT;
            let mut attempts = 0;
            let completion_error = loop {
                attempts += 1;
                match complete_runtime_turn(
                    &coordinator,
                    &agent_id,
                    &turn_id,
                    outcome.clone(),
                    pending_dynamic_input_acknowledgement,
                ) {
                    Ok(_) => break None,
                    Err(error)
                        if should_retry_runtime_turn_completion(
                            &error,
                            attempts,
                            Instant::now(),
                            completion_deadline,
                            cancellation.is_cancelled(),
                        ) =>
                    {
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(1));
                    }
                    Err(error) => break Some(error),
                }
            };
            if completion_error.is_some() {
                tracing::warn!(
                    target: "agent_runtime",
                    turn_id = %turn_id,
                    attempts,
                    "Agent Turn 终态回传未收敛，已保留持久恢复事实"
                );
            }
            if goal_elapsed_root_turn {
                let elapsed_seconds = unix_time_ms()
                    .saturating_sub(turn_started_unix_ms)
                    .div_ceil(1_000)
                    .max(1);
                if let Err(error) = commit_goal_turn_elapsed(
                    &goal_elapsed_session,
                    &goal_elapsed_state,
                    &goal_elapsed_owner,
                    turn_id.as_str(),
                    elapsed_seconds,
                ) {
                    tracing::warn!(
                        target: "agent_runtime",
                        turn_id = %turn_id,
                        error = %error,
                        "Goal 回合墙钟时长累计失败"
                    );
                }
            }
            // 只有 Coordinator 已确认终态时才释放 accepted 标记；失败路径保留它，
            // 防止同一 Turn 在当前进程内因持久终态尚未确认而再次执行。
            release_runtime_turn_state(
                execution_state.as_ref(),
                execution_idle.as_ref(),
                &turn_id,
                completion_error.is_none(),
            );
            if goal_elapsed_root_turn
                && completion_error.is_none()
                && !cancellation.is_cancelled()
                && matches!(outcome, AgentTurnOutcome::Completed { .. })
                && let Some(owner) = goal_elapsed_owner.upgrade()
            {
                // 终态已经写入 Journal；独立续跑重新进入根 Turn 起点与 Session 锁。
                if let Err(error) = owner.continue_active_goal(&goal_elapsed_session).await {
                    tracing::warn!(target: "agent_runtime", session_id = %goal_elapsed_session,
                        error = %error, "Goal 独立续跑未启动");
                }
            }
        });
        AgentTurnStartResult::Accepted
    }

    /// Runner 会在每次模型采样前主动读取持久动态输入，因此信号只需保持幂等可达。
    fn signal_turn(&self, _signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
        Ok(())
    }

    /// 取消请求中的全部托管 Turn，并在硬上限内等待异步 Runner 回传终态。
    fn quiesce_tree(&self, request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return AgentTreeQuiesceResult::RetryableUnknown {
                    error: CollaborationPortError::new("Agent 执行端状态不可用"),
                };
            }
        };
        if state.quiesced_roots.contains(&request.root_agent_id) {
            return AgentTreeQuiesceResult::AlreadyQuiesced;
        }
        let requested = request.agent_ids.iter().collect::<HashSet<_>>();
        for turn in state.running_turns.values() {
            if requested.contains(&turn.agent_id) {
                turn.cancellation.cancel();
            }
        }
        let deadline = Instant::now() + AGENT_TREE_QUIESCE_TIMEOUT;
        while state
            .running_turns
            .values()
            .any(|turn| requested.contains(&turn.agent_id))
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AgentTreeQuiesceResult::RetryableUnknown {
                    error: CollaborationPortError::new("等待 Agent 树静止超时"),
                };
            };
            let (next, timeout) = match self.idle.wait_timeout(state, remaining) {
                Ok(result) => result,
                Err(_) => {
                    return AgentTreeQuiesceResult::RetryableUnknown {
                        error: CollaborationPortError::new("Agent 执行端等待状态不可用"),
                    };
                }
            };
            state = next;
            if timeout.timed_out()
                && state
                    .running_turns
                    .values()
                    .any(|turn| requested.contains(&turn.agent_id))
            {
                return AgentTreeQuiesceResult::RetryableUnknown {
                    error: CollaborationPortError::new("等待 Agent 树静止超时"),
                };
            }
        }
        state.quiesced_roots.insert(request.root_agent_id);
        AgentTreeQuiesceResult::Quiesced
    }

    /// 在专用系统线程中等待异步后台进程回收，再按受管 lease 清理 Worktree。
    fn close_tree(&self, request: CloseAgentTree) -> Result<(), CollaborationPortError> {
        shutdown_background_tasks_blocking(
            Arc::clone(&self.background_tasks),
            self.executor_handle.clone(),
        )?;
        self.worktrees
            .release_many(&request.worktree_leases)
            .map(|_| ())
            .map_err(|_| CollaborationPortError::new("清理 Agent Worktree 失败"))
    }
}

/// 只判断当前 Agent/Turn 是否确实持有未确认的动态输入 claim。
///
/// `AgentRunError::DynamicInput` 既可能来自 claim 后的 Journal 镜像失败，也可能只是
/// 非法批次或来源适配器错误；只有 checkpoint 明确绑定当前两项身份时，才允许使用
/// 保留 claim 的终态收敛路径，避免把普通动态输入错误误当成 ack 失败。
fn coordinator_has_pending_dynamic_input_claim(
    coordinator: &CollaborationCoordinator,
    agent_id: &RunnerAgentId,
    turn_id: &AgentTurnId,
) -> Result<bool, keencode_agent::CollaborationError> {
    let checkpoint = coordinator.checkpoint_coordinator()?;
    Ok(checkpoint
        .roots
        .iter()
        .flat_map(|root| root.agents.iter())
        .filter(|agent| &agent.definition.agent_id == agent_id)
        .any(|agent| {
            agent.mailbox_claim_turn_id.as_ref() == Some(turn_id)
                || agent.steer_claim_turn_id.as_ref() == Some(turn_id)
        }))
}

/// 按 Runner 终态错误选择 Coordinator 收敛路径；ack 未完成时必须保留动态 claim。
fn complete_runtime_turn(
    coordinator: &CollaborationCoordinator,
    agent_id: &RunnerAgentId,
    turn_id: &AgentTurnId,
    outcome: AgentTurnOutcome,
    pending_dynamic_input_acknowledgement: bool,
) -> Result<(), keencode_agent::CollaborationError> {
    if pending_dynamic_input_acknowledgement {
        coordinator
            .complete_turn_with_pending_dynamic_input(agent_id, turn_id, outcome)
            .map(|_| ())
    } else {
        coordinator
            .complete_turn(agent_id, turn_id, outcome)
            .map(|_| ())
    }
}

/// 只有明确可恢复的 Store 或后置动作故障允许重试终态回传。
fn is_retryable_runtime_turn_completion_error(error: &keencode_agent::CollaborationError) -> bool {
    matches!(
        error,
        keencode_agent::CollaborationError::Store { .. }
            | keencode_agent::CollaborationError::CommittedExecutionPending { .. }
    )
}

/// 判断终态回传是否仍可在当前次数、时间和取消状态内重试。
fn should_retry_runtime_turn_completion(
    error: &keencode_agent::CollaborationError,
    attempts: usize,
    now: Instant,
    deadline: Instant,
    cancelled: bool,
) -> bool {
    is_retryable_runtime_turn_completion_error(error)
        && attempts < RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS
        && !cancelled
        && now < deadline
}

/// 将 Runner 结果映射为 Coordinator 唯一终态，并限制错误正文进入持久领域状态。
fn runtime_turn_outcome(
    result: Result<keencode_agent::TurnResult, RuntimeError>,
) -> AgentTurnOutcome {
    match result {
        Ok(result) => match result.state.terminal_reason() {
            Some(TerminalReason::Completed) => AgentTurnOutcome::Completed {
                final_message: result.final_response.as_ref().and_then(model_response_text),
            },
            Some(TerminalReason::Cancelled) => AgentTurnOutcome::Interrupted,
            Some(
                TerminalReason::Failed
                | TerminalReason::LimitReached
                | TerminalReason::ContextBlocked
                | TerminalReason::ModelOutputLimit
                | TerminalReason::ModelRefusal,
            )
            | None => AgentTurnOutcome::Failed {
                message: redacted_collaboration_failure(
                    result
                        .error
                        .as_ref()
                        .map(ToString::to_string)
                        .as_deref()
                        .unwrap_or("Agent Turn 未返回明确终态"),
                ),
            },
        },
        Err(error) => AgentTurnOutcome::Failed {
            message: redacted_collaboration_failure(&error.to_string()),
        },
    }
}

/// 提取最后一次模型响应的普通文本；纯工具或推理响应保持 `None`。
fn model_response_text(response: &keencode_model::ModelResponse) -> Option<String> {
    last_non_empty_text(&response.content).map(bounded_collaboration_failure)
}

/// 在 UTF-8 字符边界内限制进入 Collaboration 持久状态的失败或结果文本。
fn bounded_collaboration_failure(value: &str) -> String {
    if value.len() <= MAX_COLLABORATION_FAILURE_BYTES {
        return value.to_owned();
    }
    let suffix = "\n...[已截断]";
    let maximum = MAX_COLLABORATION_FAILURE_BYTES.saturating_sub(suffix.len());
    let mut boundary = maximum.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{suffix}", &value[..boundary])
}

/// 失败正文先限制输入，再统一脱敏并再次约束输出；正常完成文本不经过该入口。
fn redacted_collaboration_failure(value: &str) -> String {
    let suffix = "\n...[已截断]";
    let truncated = value.len() > MAX_COLLABORATION_FAILURE_BYTES;
    let maximum = if truncated {
        MAX_COLLABORATION_FAILURE_BYTES.saturating_sub(suffix.len())
    } else {
        MAX_COLLABORATION_FAILURE_BYTES
    };
    let redacted = keencode_model::redact_error_secrets_bounded(value, maximum);
    if truncated {
        format!("{redacted}{suffix}")
    } else {
        redacted
    }
}

/// 将扩展诊断格式化为有界日志正文。
fn extension_diagnostic_message(diagnostic: &RuntimeExtensionDiagnostic) -> String {
    let target = diagnostic
        .tool
        .as_deref()
        .map(|tool| format!(" Server={} Tool={tool}", diagnostic.server))
        .unwrap_or_else(|| format!(" Server={}", diagnostic.server));
    let message = format!(
        "扩展诊断：{}{} Code={} {}",
        diagnostic.source, target, diagnostic.code, diagnostic.message
    );
    bounded_extension_diagnostic(&message)
}

/// 在 UTF-8 字符边界内限制扩展诊断日志正文。
fn bounded_extension_diagnostic(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_EXTENSION_DIAGNOSTIC_BYTES {
        return sanitized;
    }
    let suffix = "...[已截断]";
    let maximum = MAX_EXTENSION_DIAGNOSTIC_BYTES.saturating_sub(suffix.len());
    let mut boundary = maximum.min(sanitized.len());
    while boundary > 0 && !sanitized.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{suffix}", &sanitized[..boundary])
}

/// 在专用同步线程借用共享 Tokio 执行器关闭后台任务，避免同步端口嵌套 block_on。
///
/// `BackgroundTaskManager::shutdown` 会等待每个子进程树进入终态；调用方仍需
/// 保留这条阻塞边界，不能在同步 Collaboration 端口中直接 poll 或丢弃 future。
fn shutdown_background_tasks_blocking(
    manager: Arc<BackgroundTaskManager>,
    executor_handle: tokio::runtime::Handle,
) -> Result<(), CollaborationPortError> {
    let join_handle = std::thread::Builder::new()
        .name("keencode-agent-background-shutdown".to_owned())
        .spawn(move || {
            executor_handle
                .block_on(manager.shutdown())
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .map_err(|_| CollaborationPortError::new("无法创建后台任务关闭线程"))?;

    let result = join_handle
        .join()
        .map_err(|_| CollaborationPortError::new("后台任务关闭线程异常退出"))?;
    result.map_err(CollaborationPortError::new)
}

impl Error for AgentRuntimeError {}

/// 自研 Runtime 的进程内唯一桌面装配根。
/// 跨层转发的任务终态通知：后台 Shell 与子代理共用同一注入通道。
#[derive(Clone, Debug)]
pub struct TaskTerminalNotice {
    /// 任务所属的根 Session。
    pub session_id: String,
    /// 稳定任务标识（Shell 任务 id 或子代理 Turn id）。
    pub task_id: String,
    /// 任务类别。
    pub kind: TaskNoticeKind,
    /// 已归一的状态文本（succeeded/failed/cancelled）。
    pub status_text: &'static str,
    /// 从启动到终态的持续毫秒数。
    pub duration_ms: u64,
    /// 有界结果摘要；可能为空。
    pub summary: Option<String>,
    /// 子代理任务的 Agent 标识。
    pub agent_id: Option<String>,
}

/// 任务终态通知的类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskNoticeKind {
    /// 后台 Shell 命令。
    Shell,
    /// 子代理回合。
    Agent,
}

/// 将后台任务的 Unix 毫秒启动时间格式化为 UTC RFC 3339 毫秒文本。
fn background_task_started_at(unix_ms: u64) -> Result<String, AgentRuntimeError> {
    let unix_ms = i64::try_from(unix_ms).map_err(runtime_operation_failed)?;
    Utc.timestamp_millis_opt(unix_ms)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 将单调时钟持续时间转换为不会溢出的前端毫秒数。
fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 将 Agent Goal 状态转换为稳定的小写名称。
fn goal_status_name(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Completed => "completed",
        GoalStatus::Blocked => "blocked",
    }
}

/// 将后台 Shell 的终态送入独立任务通知中继；它不再依赖 ACP UI 投递世代。
async fn run_background_task_completion_pump(
    runtime: Weak<AgentRuntime>,
    session_id: String,
    mut completions: tokio::sync::broadcast::Receiver<BackgroundTaskCompletion>,
    mut cancelled: oneshot::Receiver<()>,
) {
    loop {
        let completion = tokio::select! {
            _ = &mut cancelled => break,
            result = completions.recv() => match result {
                Ok(completion) => completion,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        };
        if completion.session_id != session_id {
            continue;
        }
        if completion.status == BackgroundTaskStatus::Failed {
            tracing::error!(session_id, task_id = %completion.task_id, "background shell failed");
        }
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        // 上层 Host 通知泵把终态格式化为 task-notification 并注入主对话；
        // 没有订阅者时广播通道会按既有语义静默丢弃。
        let _ = runtime.task_notification_tx.send(TaskTerminalNotice {
            session_id: session_id.clone(),
            task_id: completion.task_id.clone(),
            kind: TaskNoticeKind::Shell,
            status_text: completion.status.as_str(),
            duration_ms: completion.duration_ms,
            summary: Some(completion.summary.clone()).filter(|summary| !summary.is_empty()),
            agent_id: None,
        });
    }
}

pub struct AgentRuntime {
    /// 三种厂商协议共享的原子热替换 Provider 注册表。
    provider_registry: ProviderRegistry,
    /// 串行化 Provider 注册表替换与默认模型代次发布。
    provider_reload: Mutex<()>,
    /// 与注册表代次绑定且不包含凭据的默认模型选择。
    default_provider: RwLock<Option<DefaultProviderBinding>>,
    /// 最近一次由 App 同步的运行时偏好；跨窗口同步只更新这一份权威快照。
    app_runtime_preferences: Arc<RwLock<AppRuntimePreferences>>,
    /// 按 Session 隔离本地资源、租约和 Turn 生命周期的运行时管理器。
    runtime_manager: RuntimeManager,
    /// 本地 Runtime 与工具 Artifact 共同使用的应用数据根。
    storage_root: PathBuf,
    /// 原生宿主装配的本地记忆服务；记忆只作为 root request-only 上下文进入模型。
    memory_service: RwLock<Option<Arc<MemoryService>>>,
    /// 应用设置快照中的本地记忆开关，不因请求参数或 Session 状态自行改变。
    local_memories_enabled: AtomicBool,
    /// 全部 Session 共享且不恢复旧问答的 Native typed Elicitation 协调器。
    elicitations: Arc<ElicitationCoordinator>,
    /// 全部真实工具共享的权限审批门；冷恢复未重放 mode 时默认为 build。
    permissions: Arc<PermissionCoordinator>,
    /// Workflow AgentTool 的真实 Host 端口；只在普通 root 的工具冻结阶段读取。
    workflow_tool_port: RwLock<Option<Arc<dyn WorkflowToolPort>>>,
    /// actor 到父会话的问答投递映射；值只保存 Session 身份，不保存问题正文。
    workflow_elicitation_routes: RwLock<HashMap<String, String>>,
    /// 当前桌面窗口焦点及其变化通知；设置页直接从同一份 watch 状态读取和订阅。
    focus_change_tx: tokio::sync::watch::Sender<Option<String>>,
    /// Goal 文件不属于 Session Journal；用独立的进程内通知让设置页及时重读其事实源。
    goal_change_tx: tokio::sync::broadcast::Sender<GoalChangeNotice>,
    /// WebFetch/WebSearch 的检索网关配置；这是 Agent 工具能力，不属于 UI Web 宿主。
    web_service: RwLock<Option<WebServiceConfig>>,
    /// 当前后台 Agent 设置；同时作为设备级和每根树的子 Agent 上限。
    background_agent_limit: AtomicUsize,
    /// 所有 Session Coordinator 共享的设备级子 Agent 容量。
    collaboration_global_turn_limiter: Arc<CollaborationGlobalTurnLimiter>,
    /// 已实际启动过 Agent 树的 Session 级 Collaboration v2 生产装配。
    collaboration_sessions: Mutex<HashMap<String, Arc<SessionCollaborationRuntime>>>,
    /// 每个 Session 串行化 Turn 启动屏障，避免 Accepted 先于权威 TurnStarted。
    turn_start_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 在同步 cleanup 认领与异步根 Turn 起点之间提供同一 admission 边界；
    /// 计数支持同一 Session 的并发请求在 Turn gate 中排队，而不是互相误拒绝。
    session_start_admissions: Mutex<HashMap<String, usize>>,
    /// 只协调 deferred draft 的短生命周期；Session 正文和状态仍只在 Journal 中。
    deferred_session_lifecycle: Mutex<HashMap<String, DeferredSessionLifecycle>>,
    /// 每个 Session 串行化标题付费请求；关闭 Session 时移除，容量只随打开 Session 增长。
    title_generation_gates: Mutex<HashMap<String, Arc<TitleGeneration>>>,
    /// 按规范项目根隔离、仅在完整构建成功后原子发布的扩展候选。
    extension_candidates: RwLock<HashMap<PathBuf, Arc<RuntimeExtensionCandidate>>>,
    /// 设置域写入扩展资源后递增的项目级刷新 epoch；epoch 只影响下一次候选构建，
    /// 不会撤换当前仍可能被活动 Turn 使用的候选。
    extension_candidate_refresh_epochs: Mutex<HashMap<PathBuf, u64>>,
    /// 每个项目最近一次成功发布候选所确认的刷新 epoch。
    extension_candidate_published_epochs: Mutex<HashMap<PathBuf, u64>>,
    /// 已打开 Session 的 Composer 扩展引用快照；只保存公开投影，不持有正文或凭据。
    session_extension_reference_catalogs: RwLock<HashMap<String, SessionExtensionReferenceCatalog>>,
    /// 串行化项目候选发布与 MCP 撤销，避免旧传播覆盖新候选状态。
    extension_candidate_change_gate: Mutex<()>,
    /// 关闭后禁止建立新 Session 或热加载配置。
    closed: AtomicBool,
    /// 串行化进程退出，并保留首次失败结果，避免失败后重复调用伪造成功。
    shutdown_error: Mutex<Option<AgentRuntimeError>>,
    /// 防止并发 shutdown 在首次调用尚未记录失败结果时提前返回成功。
    shutdown_gate: AsyncMutex<()>,
    /// 全局后台任务完成中继：把每个 Session 泵捕获的终态（后台 Shell 与
    /// 子代理）转发给上层（AcpHost 通知泵），用于把任务完成注入主对话。
    task_notification_tx: tokio::sync::broadcast::Sender<TaskTerminalNotice>,
    /// Native Host 装配的共享 Tokio 执行器；所有 Session completion pump 均提交到这里。
    executor_handle: tokio::runtime::Handle,
}

/// Goal 持久化变化的最小运行时通知；正文和状态仍由 GoalFileStore 提供。
#[derive(Clone, Debug)]
pub(crate) struct GoalChangeNotice {
    pub(crate) session_id: String,
}

#[derive(Default)]
struct TitleGeneration {
    /// 自动命名只允许一个后台 worker；失败后由下一次发送/打开重试。
    automatic_inflight: AtomicBool,
    gate: tokio::sync::Mutex<()>,
    cancellation: TurnCancellation,
}

/// Deferred draft 的进程内生命周期认领。
///
/// 这只是关闭与首次发送之间的互斥租约；Session 正文、模型配置和 Turn
/// 仍由 Journal 持有，进程重启后继续按 Journal 事实恢复。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeferredSessionLifecycle {
    /// 已被 promote 或首次发送认领，旧 cleanup 不得再关闭。
    Promoted,
    /// cleanup 已取得关闭认领，新的 promote/send 必须等待下一次打开。
    Closing,
}

/// 已取得 Session Turn gate 的显式 mutation admission。
///
/// 持有该 guard 期间，fork/edit/workspace mutation 可以对 active work 做检查并
/// 保证关闭步骤与检查处于同一临界区。根 Turn 即使先登记 admission 也只能等待
/// 该 guard；`close` 会再次复查，拒绝取消一个刚刚进入起点屏障的发送。
pub(crate) struct SessionMutationAdmission {
    runtime: Arc<AgentRuntime>,
    session_id: String,
    _gate: tokio::sync::OwnedMutexGuard<()>,
}

/// Workspace 事务跨越 Git 与资源层期间持有的 Turn 起点屏障。
///
/// 该 guard 不复制 Session 正文，也不替代 Journal；它只保证冷 Session 在
/// 事务恢复前不会被新的 root Turn 重新登记。已注册 Session 由
/// [`SessionMutationAdmission`] 在关闭成功后转换为此 guard。
pub(crate) struct SessionMutationGuard {
    /// 共享 Runtime 用于在冷 Session 的 workspace 事务结束时释放生命周期认领。
    runtime: Arc<AgentRuntime>,
    /// 需要屏蔽读取侧 reopen 的稳定 Session 标识。
    session_id: String,
    _gate: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for SessionMutationGuard {
    fn drop(&mut self) {
        self.runtime
            .clear_deferred_session_lifecycle(&self.session_id);
        drop(self._gate.take());
    }
}

/// Workspace mutation 的两种合法入口：仍登记的 Session 需要先关闭，冷 Session
/// 直接持有同一 Turn gate。枚举由 Runtime 在线性化点生成，避免调用方先检查后抢锁。
pub(crate) enum SessionWorkspaceMutationAdmission {
    Registered(SessionMutationAdmission),
    Closed(SessionMutationGuard),
}

impl SessionMutationAdmission {
    /// 在仍持有 Turn gate 时完成最终 admission 检查并关闭 Session。
    ///
    /// 返回 `false` 表示新的 root start 在 guard 等待期间取得了 admission，或
    /// Session 已出现活动工作；此时不触碰权限、Collaboration 或 Runtime 关闭状态。
    pub(crate) async fn close_and_hold(
        self,
    ) -> Result<Option<SessionMutationGuard>, AgentRuntimeError> {
        if self.runtime.session_has_active_work(&self.session_id)? {
            return Ok(None);
        }
        // admission 计数可能在 guard 等待期间新增；只有这里的 lifecycle 冻结成功，
        // root start 才会在 gate 释放后看到 Closing 并被拒绝。
        if !self
            .runtime
            .claim_session_mutation_close(&self.session_id)?
        {
            return Ok(None);
        }
        if let Err(error) = self
            .runtime
            .close_session_locked(&self.session_id, false)
            .await
        {
            // 关闭失败时不能留下永久 Closing；否则恢复路径只能看到假占用，后续
            // workspace mutation 和用户发送都会被错误拒绝。
            self.runtime
                .clear_deferred_session_lifecycle(&self.session_id);
            return Err(error);
        }
        Ok(Some(SessionMutationGuard {
            runtime: Arc::clone(&self.runtime),
            session_id: self.session_id,
            _gate: Some(self._gate),
        }))
    }
}

/// Agent Runtime 的平台无关构造输入。
///
/// Native Host 只需要提供持久化根和已装配的 Provider 注册表即可复用同一 Runtime；
/// 路径解析、事件边界和配置读取均由宿主装配层负责。
pub(crate) struct AgentRuntimeBuildConfig {
    /// Runtime 使用的本地数据根目录。
    pub(crate) storage_root: PathBuf,
    /// 当前 Host 代次使用的 Provider 注册表。
    pub(crate) provider_registry: ProviderRegistry,
    /// 可选的请求观测记录器；headless 没有桌面 Analytics 时可以省略。
    pub(crate) analytics: Option<Arc<AnalyticsRecorder>>,
    /// 启动时使用的默认 Provider/模型；None 表示尚未配置可调用模型。
    pub(crate) default_provider: Option<(String, String)>,
    /// Native Host 创建的唯一 MemoryService；headless/test 装配可以省略。
    pub(crate) memory_service: Option<Arc<MemoryService>>,
    /// 启动时读取的应用级本地记忆开关。
    pub(crate) local_memories_enabled: bool,
    /// Native Host 的共享 Tokio 执行器句柄；Runtime 不为单个 Session 创建执行器。
    pub(crate) executor_handle: tokio::runtime::Handle,
}

#[cfg(test)]
fn test_executor_handle() -> tokio::runtime::Handle {
    // 所有测试构造器统一复用进程级多线程执行器。同步 Collaboration 端口可能
    // 在专用线程中借用 Handle::block_on；不能把 current-thread 测试执行器的
    // 唯一 worker 同时阻塞在 close_tree 的 join 上。
    static TEST_EXECUTOR: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    TEST_EXECUTOR
        .get_or_init(|| {
            Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                    .expect("测试共享 Tokio 执行器应创建"),
            )
        })
        .handle()
        .clone()
}

impl AgentRuntime {
    /// 使用 Native Host 已完成的依赖装配创建 Runtime；此入口不读取进程全局状态，
    /// 因而同样适用于桌面窗口、独立 ACP Host 和离线测试。
    pub(crate) fn build_native(
        config: AgentRuntimeBuildConfig,
    ) -> Result<Arc<Self>, AgentRuntimeError> {
        let runtime = Arc::new(Self::new_with_registry(
            config.storage_root,
            config.provider_registry,
            config.executor_handle,
        )?);
        if let Some(memory_service) = config.memory_service {
            *runtime
                .memory_service
                .write()
                .map_err(|_| AgentRuntimeError::StateUnavailable)? = Some(memory_service);
        }
        runtime
            .local_memories_enabled
            .store(config.local_memories_enabled, Ordering::Release);
        if let Some(analytics) = config.analytics {
            let weak_runtime = Arc::downgrade(&runtime);
            analytics
                .set_retry_notifier(Arc::new(move |notice| {
                    if let Some(runtime) = weak_runtime.upgrade() {
                        runtime.publish_model_retry_notice(notice);
                    }
                }))
                .map_err(|error| initialization_failed("model_retry_notifier", error))?;
        }
        if let Some((provider_id, model)) = config.default_provider {
            runtime.set_default_provider_selection(&provider_id, &model)?;
        }
        Ok(runtime)
    }

    /// 将 Provider 已确认安排的下一次尝试发布到 Runtime Publisher。
    fn publish_model_retry_notice(&self, notice: ModelRetryNotice) {
        let session = match self.runtime_manager.get(notice.session_id.clone()) {
            Ok(session) => session,
            Err(error) => {
                tracing::error!(
                    session_id = %notice.session_id,
                    %error,
                    "模型重试状态未能进入 Runtime 实时世代"
                );
                return;
            }
        };
        let occurred_at_ms = unix_time_ms();
        let message = keencode_model::redact_error_secrets_bounded(&notice.message, 2048);
        if let Err(error) = session.publish_model_retry(RuntimeModelRetryScheduled {
            turn_id: notice.turn_id.clone(),
            source_agent_id: notice.agent_id.clone(),
            attempt: notice.attempt,
            max_attempts: notice.max_attempts,
            delay_ms: notice.delay_ms,
            occurred_at_ms,
            message: message.clone(),
        }) {
            tracing::error!(
                session_id = %notice.session_id,
                %error,
                "模型重试状态未能进入 Runtime 实时世代"
            );
        }
        // Native Host 已订阅同一个 Publisher；这里仅追加瞬态 Runtime 事实。
    }

    /// 创建不向桌面发送事件的控制面测试 Runtime。
    #[cfg(test)]
    pub(crate) fn new_for_control_test(
        storage_root: impl Into<std::path::PathBuf>,
    ) -> Result<Arc<Self>, AgentRuntimeError> {
        Self::new_with_registry(
            storage_root,
            ProviderRegistry::new(),
            test_executor_handle(),
        )
        .map(Arc::new)
    }

    /// 创建不绑定桌面传输的 Runtime 单元测试实例。
    #[cfg(test)]
    fn new(storage_root: impl Into<std::path::PathBuf>) -> Result<Self, AgentRuntimeError> {
        Self::new_with_registry(
            storage_root,
            ProviderRegistry::new(),
            test_executor_handle(),
        )
    }

    /// 使用明确 Provider 注册表创建无桌面传输的 Runtime 装配根。
    fn new_with_registry(
        storage_root: impl Into<std::path::PathBuf>,
        provider_registry: ProviderRegistry,
        executor_handle: tokio::runtime::Handle,
    ) -> Result<Self, AgentRuntimeError> {
        let storage_root = storage_root.into();
        let native_acceptance_enabled =
            std::env::var("KEENCODE_NATIVE_ACCEPTANCE").as_deref() == Ok("1");
        let benchmark_enabled =
            std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1"));
        let native_test_artifact_override = if native_acceptance_enabled && benchmark_enabled {
            std::env::var_os(NATIVE_TEST_ARTIFACT_CAPACITY_ENV)
        } else {
            None
        };
        let resolved_native_test_artifact_capacity = native_test_artifact_capacity(
            native_acceptance_enabled,
            benchmark_enabled,
            native_test_artifact_override.as_deref(),
        )
        .map_err(|error| initialization_failed("native_test_artifact_capacity", error))?;
        let mut runtime_config = RuntimeConfig::new(storage_root.clone());
        if native_acceptance_enabled && benchmark_enabled {
            runtime_config.artifacts.max_artifacts_per_session =
                resolved_native_test_artifact_capacity;
        }
        let runtime_manager = RuntimeManager::new(runtime_config)
            .map_err(|error| initialization_failed("runtime_manager", error))?;
        let client_request_gate = Arc::new(ClientRequestDisplayGate::new());
        let app_runtime_preferences = Arc::new(RwLock::new(AppRuntimePreferences::default()));
        let elicitations = Arc::new(ElicitationCoordinator::with_gate_and_preferences(
            Arc::clone(&client_request_gate),
            Arc::clone(&app_runtime_preferences),
        ));
        let permissions = Arc::new(PermissionCoordinator::new());
        let background_agent_limit = DEFAULT_BACKGROUND_AGENT_LIMIT as usize;
        let collaboration_global_turn_limiter = Arc::new(
            CollaborationGlobalTurnLimiter::new(background_agent_limit)
                .map_err(runtime_operation_failed)?,
        );
        let (focus_change_tx, _) = tokio::sync::watch::channel::<Option<String>>(None);
        Ok(Self {
            provider_registry,
            provider_reload: Mutex::new(()),
            default_provider: RwLock::new(None),
            app_runtime_preferences,
            runtime_manager,
            storage_root,
            memory_service: RwLock::new(None),
            local_memories_enabled: AtomicBool::new(false),
            elicitations,
            permissions,
            workflow_tool_port: RwLock::new(None),
            workflow_elicitation_routes: RwLock::new(HashMap::new()),
            focus_change_tx,
            goal_change_tx: tokio::sync::broadcast::channel(64).0,
            web_service: RwLock::new(None),
            background_agent_limit: AtomicUsize::new(background_agent_limit),
            collaboration_global_turn_limiter,
            collaboration_sessions: Mutex::new(HashMap::new()),
            turn_start_gates: Mutex::new(HashMap::new()),
            session_start_admissions: Mutex::new(HashMap::new()),
            deferred_session_lifecycle: Mutex::new(HashMap::new()),
            title_generation_gates: Mutex::new(HashMap::new()),
            extension_candidates: RwLock::new(HashMap::new()),
            extension_candidate_refresh_epochs: Mutex::new(HashMap::new()),
            extension_candidate_published_epochs: Mutex::new(HashMap::new()),
            session_extension_reference_catalogs: RwLock::new(HashMap::new()),
            extension_candidate_change_gate: Mutex::new(()),
            closed: AtomicBool::new(false),
            shutdown_error: Mutex::new(None),
            shutdown_gate: AsyncMutex::new(()),
            task_notification_tx: tokio::sync::broadcast::channel(256).0,
            executor_handle,
        })
    }

    /// 订阅全局后台任务完成中继；上层（AcpHost 通知泵）把终态注入主对话。
    pub fn subscribe_task_completions(
        &self,
    ) -> tokio::sync::broadcast::Receiver<TaskTerminalNotice> {
        self.task_notification_tx.subscribe()
    }

    /// 订阅 Goal 文件变化；设置页等投影只据此触发重读，不缓存 Goal 正文。
    pub(crate) fn subscribe_goal_changes(
        &self,
    ) -> tokio::sync::broadcast::Receiver<GoalChangeNotice> {
        self.goal_change_tx.subscribe()
    }

    /// 订阅桌面窗口焦点变化；设置页用它切换当前 Session 的实时事件流。
    pub(crate) fn subscribe_focus_changes(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.focus_change_tx.subscribe()
    }

    /// Runtime 正在收口时让后台投影观察者退出，避免保留执行器任务。
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 返回三种厂商协议共享的 Provider 注册表。
    pub fn provider_registry(&self) -> &ProviderRegistry {
        &self.provider_registry
    }

    /// 写入 App 级运行时偏好；它只更新 Runtime 自己的受保护快照，不能因此
    /// 启动 dormant Session。具体 Session 的问答与记录策略在消费时读取这份快照。
    pub fn sync_app_runtime_preferences(
        &self,
        preferences: AppRuntimePreferences,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        *self
            .app_runtime_preferences
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = preferences;
        self.elicitations
            .sync_auto_resolution_preference(preferences.ask_user_question_auto_resolution_enabled);
        Ok(())
    }

    /// 读取当前 App 偏好快照，供需要作出运行时决策的受信桌面调用面使用。
    pub fn app_runtime_preferences(&self) -> Result<AppRuntimePreferences, AgentRuntimeError> {
        self.app_runtime_preferences
            .read()
            .map(|preferences| *preferences)
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 返回进程内唯一的 Session Runtime 管理器。
    pub fn runtime_manager(&self) -> &RuntimeManager {
        &self.runtime_manager
    }

    /// 认领一个 deferred Session，阻止旧窗口 cleanup 在配置或首次发送期间关闭它。
    ///
    /// 同一 Session 的重复认领是幂等的；只有已经进入关闭认领的草稿拒绝新操作。
    pub fn claim_deferred_session(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        let mut lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        match lifecycle.get(session_id).copied() {
            Some(DeferredSessionLifecycle::Closing) => Err(AgentRuntimeError::SessionUnavailable),
            Some(DeferredSessionLifecycle::Promoted) => Ok(()),
            None => {
                lifecycle.insert(session_id.to_owned(), DeferredSessionLifecycle::Promoted);
                Ok(())
            }
        }
    }

    /// 标记 deferred 草稿已提升；它与首次发送使用同一认领账本。
    pub fn promote_deferred_session(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        self.claim_deferred_session(session_id)
    }

    /// 原子取得 deferred cleanup 的关闭认领。
    ///
    /// 返回 `false` 表示 Session 已被 promote/发送认领，调用方必须保留它；返回
    /// `true` 后调用方再检查 Journal 是否为空，最后调用 `close_session` 完成关闭。
    pub fn begin_deferred_session_close(
        &self,
        session_id: &str,
    ) -> Result<bool, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let mut lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if lifecycle.contains_key(session_id) {
            return Ok(false);
        }
        if self
            .session_start_admissions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .is_some_and(|count| *count > 0)
        {
            return Ok(false);
        }
        lifecycle.insert(session_id.to_owned(), DeferredSessionLifecycle::Closing);
        Ok(true)
    }

    /// 关闭前发现草稿已有事实时释放 deferred 关闭认领，允许后续发送继续。
    pub fn cancel_deferred_session_close(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        let mut lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if lifecycle.get(session_id) == Some(&DeferredSessionLifecycle::Closing) {
            lifecycle.remove(session_id);
        }
        Ok(())
    }

    /// 完成显式关闭后释放本次进程内生命周期认领。
    fn clear_deferred_session_lifecycle(&self, session_id: &str) {
        if let Ok(mut lifecycle) = self.deferred_session_lifecycle.lock() {
            lifecycle.remove(session_id);
        }
    }

    /// 在根 Turn 真正取得异步 gate 前登记一次启动；deferred cleanup 读取同一份
    /// admission 账本，不能在启动已进入临界区后把空草稿认领关闭。
    fn admit_session_start(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        let mut lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if lifecycle.get(session_id) == Some(&DeferredSessionLifecycle::Closing) {
            return Err(AgentRuntimeError::SessionUnavailable);
        }
        // 一旦根 Turn 被接受，Session 已有明确用户事实；即使 Turn 后续结束，
        // workspace deferred cleanup 也不能把它当成尚未提升的空草稿关闭。
        lifecycle
            .entry(session_id.to_owned())
            .or_insert(DeferredSessionLifecycle::Promoted);
        let mut admissions = self
            .session_start_admissions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let count = admissions.entry(session_id.to_owned()).or_default();
        *count = count.saturating_add(1);
        Ok(())
    }

    fn release_session_start(&self, session_id: &str) {
        if let Ok(mut admissions) = self.session_start_admissions.lock()
            && let Some(count) = admissions.get_mut(session_id)
        {
            if *count <= 1 {
                admissions.remove(session_id);
            } else {
                *count -= 1;
            }
        }
    }

    /// 返回当前 Session 的唯一 Turn 起点屏障；显式 mutation 与 root start 必须共用它。
    fn session_turn_start_gate(
        &self,
        session_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let mut gates = self
            .turn_start_gates
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        Ok(Arc::clone(
            gates
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        ))
    }

    fn has_session_start_admission(&self, session_id: &str) -> Result<bool, AgentRuntimeError> {
        Ok(self
            .session_start_admissions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .is_some_and(|count| *count > 0))
    }

    /// 在同一 lifecycle/admission 锁顺序下冻结新的 root start，并返回关闭认领。
    /// 调用方必须已经持有对应 Turn gate，保证 active 检查与此冻结之间没有新 Turn。
    fn claim_session_mutation_close(&self, session_id: &str) -> Result<bool, AgentRuntimeError> {
        let mut lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if lifecycle.get(session_id) == Some(&DeferredSessionLifecycle::Closing) {
            return Ok(false);
        }
        let admissions = self
            .session_start_admissions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if admissions.get(session_id).is_some_and(|count| *count > 0) {
            return Ok(false);
        }
        lifecycle.insert(session_id.to_owned(), DeferredSessionLifecycle::Closing);
        Ok(true)
    }

    /// 在线性化点取得 workspace mutation admission。
    ///
    /// 与普通 mutation 不同，冷 Session 没有 Runtime manager 槽位，不能先
    /// `open_or_create_session` 再检查；否则 handoff 会在 Git 事务前重新登记
    /// Journal。这里在同一 Turn gate 下返回 `Closed`，并由调用方持有 guard
    /// 直到 workspace 恢复完成。
    pub(crate) async fn begin_workspace_mutation(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<Option<SessionWorkspaceMutationAdmission>, AgentRuntimeError> {
        let gate = self.session_turn_start_gate(session_id)?;
        let guard = Arc::clone(&gate).lock_owned().await;
        if self.has_session_start_admission(session_id)? {
            return Ok(None);
        }
        // 冷 Session 没有 Runtime manager 槽位时，旧窗口的 deferred cleanup
        // 仍可能在另一个任务中取得关闭认领。先用同一 lifecycle 账本认领
        // Promoted，保证恢复完成前 cleanup 不能把刚重新打开的 Session 关闭。
        if let Err(error) = self.claim_deferred_session(session_id) {
            if error == AgentRuntimeError::SessionUnavailable {
                return Ok(None);
            }
            return Err(error);
        }
        match self.runtime_manager.get(session_id.to_owned()) {
            Ok(session) => {
                drop(session);
                if self.session_has_active_work(session_id)? {
                    return Ok(None);
                }
                Ok(Some(SessionWorkspaceMutationAdmission::Registered(
                    SessionMutationAdmission {
                        runtime: Arc::clone(self),
                        session_id: session_id.to_owned(),
                        _gate: guard,
                    },
                )))
            }
            Err(RuntimeError::SessionNotRegistered) => {
                if !self.claim_session_mutation_close(session_id)? {
                    return Ok(None);
                }
                Ok(Some(SessionWorkspaceMutationAdmission::Closed(
                    SessionMutationGuard {
                        runtime: Arc::clone(self),
                        session_id: session_id.to_owned(),
                        _gate: Some(guard),
                    },
                )))
            }
            Err(error) => Err(runtime_operation_failed(error)),
        }
    }

    /// 返回项目与 Session 定位索引所在的数据根目录。
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// 注册 Native Host 的真实 Workflow AgentTool 端口；工具只会在后续普通
    /// root Turn 冻结时加入，已冻结的工具表不会被运行中途修改。
    pub(crate) fn set_workflow_tool_port(
        &self,
        port: Arc<dyn WorkflowToolPort>,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        *self
            .workflow_tool_port
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = Some(port);
        Ok(())
    }

    /// 返回进程内唯一 Elicitation 协调器，AskUser 不得建立旁路待决账本。
    pub fn elicitation_coordinator(&self) -> &Arc<ElicitationCoordinator> {
        &self.elicitations
    }

    /// 返回真实工具权限协调器；AskUser 问答不得复用这条审批账本。
    pub(crate) fn permission_coordinator(&self) -> &Arc<PermissionCoordinator> {
        &self.permissions
    }

    /// 返回 Native Host 使用的权限协调器；其状态仍由 Runtime 唯一持有。
    pub(crate) fn permissions(&self) -> &Arc<PermissionCoordinator> {
        &self.permissions
    }

    /// 按当前父连接读取权限 pending；其他连接只能得到拒绝，不能枚举正文。
    pub(crate) fn pending_permission_views(
        &self,
        parent_session_id: &str,
        connection_id: &ConnectionId,
    ) -> Result<Vec<PendingPermissionView>, AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        Ok(self
            .permissions
            .pending_views_for_connection(parent_session_id, connection_id))
    }

    /// 按稳定 operationId 将权限模式写入 Session Journal，并同步内存审批门。
    pub(crate) fn set_permission_mode_with_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        mode: PermissionMode,
    ) -> Result<SessionState, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let state = session
            .set_permission_mode(operation_id, resource_permission_mode(mode))
            .map_err(runtime_operation_failed)?;
        self.permissions.set_mode(session_id, mode);
        Ok(state)
    }

    /// 按稳定 operationId 将视觉输入开关写入 Session Journal。
    pub(crate) fn set_vision_enabled_with_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        enabled: bool,
    ) -> Result<SessionState, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        session
            .set_vision_enabled(operation_id, enabled)
            .map_err(runtime_operation_failed)
    }

    /// 绑定 root Session 当前唯一可回答权限的连接。
    pub(crate) fn bind_permission_session_connection(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.permissions
            .bind_session_connection(session_id, connection_id.clone())
            .map_err(|error| {
                // 权限桥只返回稳定枚举；记录阶段和错误类别，避免首发失败被
                // 归一成无上下文的 RuntimeOperationFailed，同时不写入连接或正文。
                tracing::error!(
                    target: "keencode_diagnostics",
                    stage = "start_root_turn.bind_permission_session_connection",
                    error = ?error,
                    "首发前权限连接绑定失败"
                );
                AgentRuntimeError::RuntimeOperationFailed
            })
    }

    /// 原生连接关闭时收口其全部权限等待，避免审批 Future 脱离 Host 后悬挂。
    pub(crate) fn disconnect_permission_connection(&self, connection_id: &ConnectionId) {
        self.permissions.disconnect(connection_id);
    }

    /// 让 Workflow actor 复用父会话已绑定的 Native typed 连接，并把问答显示在父 conversation。
    ///
    /// 没有已绑定连接时保持未暴露状态；actor 下一次冷恢复会再次尝试绑定当前父连接。
    pub(crate) fn bind_workflow_actor_elicitation(
        &self,
        actor_session_id: &str,
        parent_session_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(actor_session_id)?;
        validate_session_id(parent_session_id)?;
        let connection = self.elicitations.session_connection(parent_session_id);
        if connection.is_some() {
            self.permissions
                .bind_actor_parent(actor_session_id, parent_session_id);
        } else {
            self.permissions.unbind_actor_parent(actor_session_id);
        }
        let can_route = connection
            .as_ref()
            .is_some_and(|connection| self.elicitations.supports_form_for_connection(connection));
        let mut routes = self
            .workflow_elicitation_routes
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if let Some(connection) = connection.filter(|_| can_route) {
            self.elicitations
                .bind_native_session(actor_session_id, &connection)
                .map_err(runtime_operation_failed)?;
            routes.insert(actor_session_id.to_owned(), parent_session_id.to_owned());
        } else {
            routes.remove(actor_session_id);
        }
        Ok(())
    }

    /// 通过父会话已绑定的连接回答 Workflow actor 的待决问答。
    ///
    /// Elicitation pending 账本保留 actor Session，回答显示仍走父连接；这里先核对
    /// actor→parent 路由，再让协调器按原始问题 Schema 解析答案，拒绝跨会话或猜字段。
    pub(crate) fn resolve_workflow_question(
        &self,
        parent_session_id: &str,
        request_id: &str,
        answer: &str,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        if request_id.trim().is_empty() {
            return Err(AgentRuntimeError::UnknownClientRequest);
        }
        let actor_session_id = self
            .elicitations
            .pending_session_id_for_request(request_id)
            .ok_or(AgentRuntimeError::UnknownClientRequest)?;
        let routed_parent = self
            .workflow_elicitation_routes
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&actor_session_id)
            .cloned()
            .ok_or(AgentRuntimeError::ClientResponseRejected)?;
        if routed_parent != parent_session_id {
            return Err(AgentRuntimeError::ClientResponseRejected);
        }
        let pending_connection = self
            .elicitations
            .pending_connection_for_request(request_id)
            .ok_or(AgentRuntimeError::UnknownClientRequest)?;
        let parent_connection = self
            .elicitations
            .session_connection(parent_session_id)
            .ok_or(AgentRuntimeError::ClientResponseRejected)?;
        if pending_connection != parent_connection {
            return Err(AgentRuntimeError::ClientResponseRejected);
        }
        self.elicitations
            .respond_workflow_answer_from_connection(&pending_connection, request_id, answer)
            .map_err(|error| match error {
                crate::elicitation::ElicitationBridgeError::UnknownRequest => {
                    AgentRuntimeError::UnknownClientRequest
                }
                crate::elicitation::ElicitationBridgeError::ResponseConnectionMismatch => {
                    AgentRuntimeError::ClientResponseRejected
                }
                _ => AgentRuntimeError::ClientResponseRejected,
            })
    }

    /// 读取当前连接可见的 pending 问答；正文仍来自 Coordinator 的原始 Schema。
    ///
    /// 连接必须是父 Session 当前绑定的连接。actor 问答只有在已登记的
    /// actor→parent 路由和 Coordinator display session 同时匹配时才会投影，
    /// 因此查询本身不能被前端用来枚举其他 Session 的问题。
    pub(crate) fn pending_elicitation_views(
        &self,
        parent_session_id: &str,
        connection_id: &ConnectionId,
    ) -> Result<Vec<PendingElicitationView>, AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        let Some(bound_connection) = self.elicitations.session_connection(parent_session_id) else {
            return Ok(Vec::new());
        };
        if bound_connection != *connection_id {
            return Err(AgentRuntimeError::ClientResponseRejected);
        }
        let routes = self
            .workflow_elicitation_routes
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        Ok(self
            .elicitations
            .pending_views_for_connection(connection_id)
            .into_iter()
            .filter(|view| {
                view.session_id == parent_session_id
                    || (routes
                        .get(&view.session_id)
                        .is_some_and(|parent| parent == parent_session_id)
                        && view.display_session_id.as_deref() == Some(parent_session_id))
            })
            .collect())
    }

    fn workflow_elicitation_target(&self, session_id: &str) -> Result<String, AgentRuntimeError> {
        self.workflow_elicitation_routes
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)
            .map(|routes| {
                routes
                    .get(session_id)
                    .cloned()
                    .unwrap_or_else(|| session_id.to_owned())
            })
    }

    /// 原子更新后续与现存 Session 的设备级及每根树子 Agent 并发上限。
    pub fn set_background_agent_limit(&self, limit: usize) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        if limit == 0 {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let runtimes = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let sessions_by_coordinator = runtimes
            .iter()
            .map(|(session_id, runtime)| {
                (runtime.coordinator.coordinator_id(), session_id.as_str())
            })
            .collect::<HashMap<_, _>>();
        let root_refs = runtimes
            .values()
            .map(|runtime| (runtime.coordinator.as_ref(), &runtime.root_agent_id))
            .collect::<Vec<_>>();
        let report = self
            .collaboration_global_turn_limiter
            .update_limits_atomically(&root_refs, limit, limit)
            .map_err(runtime_operation_failed)?;
        self.background_agent_limit.store(limit, Ordering::Release);
        for failure in report.dispatch_errors() {
            let session_id = failure
                .coordinator_id()
                .and_then(|coordinator_id| sessions_by_coordinator.get(&coordinator_id).copied());
            tracing::warn!(
                coordinator_id = ?failure.coordinator_id(),
                session_id = ?session_id,
                error = %redacted_collaboration_failure(&failure.error().to_string()),
                "后台 Agent 限额已提交；等待 Turn 调度尚未收敛"
            );
        }
        if report.dispatch_in_progress() {
            tracing::debug!("后台 Agent 限额已提交；已有全局派发轮次继续处理等待队列");
        }
        Ok(())
    }

    /// 原子切换后续请求是否把本地记忆上下文加入模型输入。
    ///
    /// 设置页的开关必须更新 Runtime，而不能只启动记忆提取调度器；否则新的
    /// Prompt 仍会读取启动时冻结的旧值。
    pub(crate) fn set_local_memories_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        self.local_memories_enabled
            .store(enabled, Ordering::Release);
        Ok(())
    }

    /// 打开既有 Session，或在未指定标识时按项目根与创建 operationId 确定性创建 Session。
    ///
    /// 指定标识只允许打开既有 Session；拼写错误不得静默创建新的授权边界。
    pub fn open_or_create_session(
        &self,
        project_root: &Path,
        requested_session_id: Option<&str>,
        create_operation_id: &str,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        self.open_or_create_session_internal(
            project_root,
            requested_session_id,
            create_operation_id,
            false,
        )
    }

    /// 仅供 workspace 事务完成后恢复已冻结 Session；调用方必须仍持有 mutation guard。
    pub(crate) fn open_or_create_session_for_workspace_mutation(
        &self,
        project_root: &Path,
        requested_session_id: Option<&str>,
        create_operation_id: &str,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        self.open_or_create_session_internal(
            project_root,
            requested_session_id,
            create_operation_id,
            true,
        )
    }

    fn open_or_create_session_internal(
        &self,
        project_root: &Path,
        requested_session_id: Option<&str>,
        create_operation_id: &str,
        workspace_mutation_restore: bool,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        // 普通同步 open 必须与 mutation lifecycle 共用同一把锁，并覆盖到
        // Manager 注册、项目校验和扩展目录冻结；否则 check 后到 open 前仍可
        // 被 workspace close 插入，重新登记一个待变更 Session。
        let lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if !workspace_mutation_restore
            && let Some(session_id) = requested_session_id
            && lifecycle.get(session_id) == Some(&DeferredSessionLifecycle::Closing)
        {
            return Err(AgentRuntimeError::SessionUnavailable);
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let project_root = canonical_project_root(project_root)?;
        let session = if let Some(session_id) = requested_session_id {
            validate_session_id(session_id)?;
            match self.runtime_manager.get(session_id.to_owned()) {
                Ok(session) => {
                    if session.is_open().map_err(runtime_operation_failed)? {
                        session
                    } else {
                        // 先显式释放 get 返回的 lease；否则旧句柄可能活到整个
                        // match 结束，紧接着的 close/open 会误报 SessionBusy。
                        drop(session);
                        // 关闭流程可能已经冻结句柄、但在最后清理失败前仍留在
                        // Manager。这里把已移除视为幂等成功，再从 Journal 恢复。
                        match self.runtime_manager.close(session_id.to_owned()) {
                            Ok(()) | Err(RuntimeError::SessionNotRegistered) => {}
                            Err(error) => return Err(runtime_operation_failed(error)),
                        }
                        match self.runtime_manager.open(session_id.to_owned()) {
                            Ok(OpenSessionResult::Ready(session)) => session,
                            Ok(OpenSessionResult::Corrupt(_)) => {
                                return Err(AgentRuntimeError::SessionUnavailable);
                            }
                            Err(error) => return Err(runtime_operation_failed(error)),
                        }
                    }
                }
                Err(RuntimeError::SessionNotRegistered) => {
                    match self.runtime_manager.open(session_id.to_owned()) {
                        Ok(OpenSessionResult::Ready(session)) => session,
                        Ok(OpenSessionResult::Corrupt(_))
                        | Err(RuntimeError::SessionNotCreated)
                        | Err(RuntimeError::SessionNotRegistered) => {
                            return Err(AgentRuntimeError::SessionUnavailable);
                        }
                        Err(error) => return Err(runtime_operation_failed(error)),
                    }
                }
                Err(error) => return Err(runtime_operation_failed(error)),
            }
        } else {
            let generated = deterministic_session_id(&project_root, create_operation_id)?;
            // 新会话在首条用户消息前保持占位标题；项目名不充当会话标题，
            // 前端的自动命名流程只替换占位标题。
            let title = "新对话".to_owned();
            match self.runtime_manager.get(generated.clone()) {
                Ok(session) => session,
                Err(RuntimeError::SessionNotRegistered) => {
                    match self.runtime_manager.open(generated.clone()) {
                        Ok(OpenSessionResult::Ready(session)) => session,
                        Ok(OpenSessionResult::Corrupt(_)) => {
                            return Err(AgentRuntimeError::SessionUnavailable);
                        }
                        Err(RuntimeError::SessionNotCreated) => self
                            .runtime_manager
                            .create(CreateSessionRequest {
                                session_id: generated,
                                title,
                                project_root: project_root.to_string_lossy().into_owned(),
                            })
                            .map_err(runtime_operation_failed)?,
                        Err(error) => return Err(runtime_operation_failed(error)),
                    }
                }
                Err(error) => return Err(runtime_operation_failed(error)),
            }
        };
        ensure_session_project(&session, &project_root)?;
        // 权限门只保存当前进程的等待者和连接；每次打开都从 Journal 恢复 mode，
        // 避免冷启动把持久的 Yolo/Edit/Plan 意外降级为默认 Build。
        let persisted_permission_mode = session
            .read_state(|state| state.permission_mode)
            .map_err(runtime_operation_failed)?;
        self.permissions.set_mode(
            session.session_id().as_str(),
            desktop_permission_mode(persisted_permission_mode),
        );
        self.freeze_session_reference_catalog_if_available(
            session.session_id().as_str(),
            &project_root,
        )?;
        Ok(session)
    }

    /// 在 Session 起点屏障内打开或创建 Session。
    ///
    /// 首次发送、workspace release 和 deferred cleanup 都必须使用这个入口；否则
    /// `RuntimeManager::open` 可能在另一个连接刚完成 `close`、但旧 lease 尚未释放
    /// 时返回 SessionBusy，造成前端看到无依据的 SessionNotRegistered。
    pub async fn open_or_create_session_serialized(
        &self,
        project_root: &Path,
        requested_session_id: Option<&str>,
        create_operation_id: &str,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        let gate_session_id = match requested_session_id {
            Some(session_id) => session_id.to_owned(),
            None => {
                let canonical = canonical_project_root(project_root)?;
                deterministic_session_id(&canonical, create_operation_id)?
            }
        };
        let gate = self.session_turn_start_gate(&gate_session_id)?;
        let _guard = gate.lock().await;
        self.open_or_create_session(project_root, requested_session_id, create_operation_id)
    }

    /// 返回一个已打开 Session 的权威一致快照。
    pub fn session_snapshot(&self, session_id: &str) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.snapshot())
            .map_err(runtime_operation_failed)
    }

    /// 订阅指定 Session 的 Runtime 事实，用于队列自动排放等边界触发器。
    ///
    /// 订阅只读取 Runtime publisher，不在 AgentRuntime 侧复制会话事实；调用方
    /// 必须在任务结束或连接关闭时丢弃订阅，慢消费者由 Runtime 的有界 lag 规则处理。
    pub fn subscribe_session_events(
        &self,
        session_id: &str,
    ) -> Result<RuntimeEventSubscription, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.subscribe())
            .map_err(runtime_operation_failed)
    }

    /// 读取 Session 的后续输入事实；队列正文只来自权威 Journal。
    pub fn session_input_queue(
        &self,
        session_id: &str,
    ) -> Result<(FollowupMode, SessionInputQueueState), AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.input_queue_state())
            .map_err(runtime_operation_failed)
    }

    /// 使用当前 Session Provider 执行一次真实手动上下文压缩。
    ///
    /// 压缩使用一个不追加用户消息的维护 Turn；Runtime 只有在
    /// `ContextCompactionApplied` 与维护 Turn 终态均写入 Journal 后才返回成功。
    /// `target_turn_id` 必须由队列消费屏障预先持久化，避免崩溃恢复时猜测输入归属。
    pub async fn compact_session_context(
        &self,
        session_id: &str,
        operation_id: &str,
        target_turn_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        if operation_id.trim().is_empty() || target_turn_id.trim().is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(runtime_operation_failed)?;
        if !session
            .active_turn_ids()
            .map_err(runtime_operation_failed)?
            .is_empty()
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let resolved = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
        let root_agent = ResourceAgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
            .map_err(runtime_operation_failed)?;
        let messages = session
            .model_transcript_for_agent(&root_agent)
            .map_err(runtime_operation_failed)?;
        let provider_snapshot = provider_snapshot(&resolved);
        let provider: Arc<dyn ModelProvider> = Arc::new(TurnBoundProvider::new(
            Arc::new(resolved.clone()),
            session_id,
            target_turn_id,
            keencode_resources::ROOT_AGENT_ID,
        ));
        let mut request = TurnRequest::new(
            AgentSessionId::new(session_id.to_owned())
                .map_err(|_| AgentRuntimeError::InvalidSession)?,
            AgentTurnId::new(target_turn_id.to_owned())
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
            RunnerAgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
            resolved.model(),
            messages,
            if snapshot.state.plan.enabled {
                PlanGuard::read_only()
            } else {
                PlanGuard::inactive()
            },
        );
        request.model_request_mut().max_output_tokens = resolved
            .capabilities(resolved.model())
            .max_output_tokens
            .map(u32::try_from)
            .transpose()
            .map_err(runtime_operation_failed)?;
        let runner = session.bind_agent_runner(
            AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
                .with_tool_approval_gate(self.permissions.clone()),
        );
        runner
            .compact_turn(
                request,
                provider_snapshot,
                format!("manual_compact:{operation_id}"),
                1,
            )
            .await
            .map(|_| ())
            .map_err(runtime_operation_failed)
    }

    /// 持久暂停当前 Session Goal，并保持与 ACP GoalFileStore 相同的 owner/CAS 语义。
    pub fn pause_session_goal(
        &self,
        session_id: &str,
        operation_id: &str,
        expected_revision: u64,
    ) -> Result<GoalChange, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(runtime_operation_failed)?;
        let state = PersistentAgentState::open_with_goal_root(session, &self.storage_root)
            .map_err(runtime_operation_failed)?;
        let change = state
            .pause_goal_if_revision(operation_id, expected_revision)
            .map_err(runtime_operation_failed)?
            .ok_or_else(|| {
                runtime_operation_failed(RuntimeStateError::Conflict {
                    message: "Goal 修订已变化，请重新加载".to_owned(),
                })
            })?;
        self.publish_goal_changed(
            session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
        // 先持久化 Paused，再从同一 Journal 快照取得当前根 Turn 并请求取消；
        // 取消令牌会让 Turn 收口路径跳过 continue_active_goal。
        let active_root_turn_id = self
            .runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.snapshot())
            .map_err(runtime_operation_failed)?
            .state
            .turns
            .values()
            .find(|turn| {
                turn.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                    && turn.root_turn_id == turn.turn_id
                    && turn.status == TurnStatus::Running
            })
            .map(|turn| turn.turn_id.as_str().to_owned());
        if let Some(turn_id) = active_root_turn_id {
            self.cancel_turn(session_id, &turn_id)?;
        }
        Ok(change)
    }

    /// 持久恢复当前 Session Goal；只有真实 active Goal 才会交给续跑调度器。
    pub async fn resume_session_goal(
        self: &Arc<Self>,
        session_id: &str,
        operation_id: &str,
        expected_revision: u64,
    ) -> Result<GoalChange, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(runtime_operation_failed)?;
        let state = PersistentAgentState::open_with_goal_root(session, &self.storage_root)
            .map_err(runtime_operation_failed)?;
        let change = state
            .resume_goal_if_revision(operation_id, expected_revision)
            .map_err(runtime_operation_failed)?
            .ok_or_else(|| {
                runtime_operation_failed(RuntimeStateError::Conflict {
                    message: "Goal 修订已变化，请重新加载".to_owned(),
                })
            })?;
        self.publish_goal_changed(
            session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
        if change
            .current
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            self.continue_active_goal(session_id).await?;
        }
        Ok(change)
    }

    /// 持久切换后续输入模式。
    pub fn set_session_followup_mode(
        &self,
        session_id: &str,
        operation_id: &str,
        mode: FollowupMode,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(runtime_operation_failed)?;
        session
            .set_followup_mode(operation_id, mode)
            .map_err(runtime_operation_failed)?;
        self.session_snapshot(session_id)
    }

    /// 持久加入一条后续输入。
    pub fn enqueue_session_input(
        self: &Arc<Self>,
        session_id: &str,
        operation_id: &str,
        item: SessionInputQueueItem,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let snapshot = self
            .runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.enqueue_input(operation_id, item))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?;
        self.schedule_automatic_title(session_id);
        snapshot
    }

    /// 修改持久队列中的一条输入。
    pub fn edit_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
        new_text: String,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.edit_queued_input(operation_id, queue_item_id, new_text))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 重排持久队列中的一条输入。
    pub fn reorder_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
        before_queue_item_id: Option<&str>,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| {
                session.reorder_queued_input(operation_id, queue_item_id, before_queue_item_id)
            })
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 删除持久队列中的一条输入。
    pub fn delete_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.delete_queued_input(operation_id, queue_item_id))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 持久切换队列空闲自动消费开关。
    pub fn set_session_input_auto_drain(
        &self,
        session_id: &str,
        operation_id: &str,
        auto_drain: bool,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.set_input_queue_auto_drain(operation_id, auto_drain))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 为显式发送保留一条队列输入，并返回冻结的正文与模型选择。
    pub fn reserve_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
        target_turn_id: &str,
    ) -> Result<SessionInputQueueItem, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| {
                session.reserve_queued_input(operation_id, queue_item_id, target_turn_id)
            })
            .map(|(_, item)| item)
            .map_err(runtime_operation_failed)
    }

    /// 队列项对应 Turn 启动成功后确认消费。
    pub fn complete_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.complete_queued_input(operation_id, queue_item_id))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 队列项对应 Turn 启动失败时恢复为可消费状态。
    pub fn release_session_input(
        &self,
        session_id: &str,
        operation_id: &str,
        queue_item_id: &str,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.release_queued_input(operation_id, queue_item_id))
            .map(|_| self.session_snapshot(session_id))
            .map_err(runtime_operation_failed)?
    }

    /// 返回工作流启动时可冻结的无凭据 Provider 快照，并验证已绑定配置仍然在当前注册表中。
    ///
    /// 父 Session 已有快照时不能静默改用新的默认 Provider；只有未绑定 Provider 的旧会话
    /// 才允许读取当前默认选择。工作流 Host 将返回值直接写进 `run-started.models`，后续
    /// actor 只能按该值启动，避免热加载后把同一运行切换到另一套凭据或模型。
    pub(crate) fn workflow_provider_snapshot(
        &self,
        parent_session_id: &str,
    ) -> Result<ProviderSnapshot, AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        let session = self
            .runtime_manager
            .get(parent_session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        self.workflow_provider_snapshot_for_bound(
            parent_session_id,
            snapshot.state.provider.as_ref(),
        )
    }

    /// 按已取得的 Session Provider 绑定解析并校验当前注册表，不重新读取 Session。
    ///
    /// 设置页工作流启动会把此方法与同一份 Session 快照中的 cwd、Plan 状态一起使用，
    /// 确保 `run-started.models` 的 Provider 与其它启动事实来自同一个状态边界。
    pub(crate) fn workflow_provider_snapshot_for_bound(
        &self,
        parent_session_id: &str,
        bound: Option<&ProviderSnapshot>,
    ) -> Result<ProviderSnapshot, AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        let Some(bound) = bound else {
            return Ok(provider_snapshot(&self.resolve_default_provider()?));
        };
        let resolved = self.resolve_session_provider(Some(bound))?;
        let current = provider_snapshot(&resolved);
        if current.provider_id != bound.provider_id
            || current.model != bound.model
            || current.protocol != bound.protocol
            || current.config_fingerprint != bound.config_fingerprint
        {
            return Err(AgentRuntimeError::ProviderReloadFailed);
        }
        Ok(bound.clone())
    }

    /// 按工作流设置的规范模型串解析真实 Provider 快照；注册表是唯一模型能力和
    /// 配置身份来源，返回值只含无凭据字段，供 successor 的 `run-started.models` 冻结。
    pub(crate) fn workflow_provider_snapshot_for_selection(
        &self,
        parent_session_id: &str,
        provider_id: &str,
        model: &str,
        reasoning: Option<&str>,
    ) -> Result<ProviderSnapshot, AgentRuntimeError> {
        validate_session_id(parent_session_id)?;
        self.runtime_manager
            .get(parent_session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let provider = self
            .provider_registry
            .resolve(provider_id, model)
            .map_err(|_| AgentRuntimeError::ProviderNotConfigured)?;
        let mut snapshot = provider_snapshot(&provider);
        snapshot.reasoning_effort = reasoning
            .map(parse_reasoning_effort)
            .transpose()?
            .flatten()
            .map(reasoning_effort_snapshot);
        Ok(snapshot)
    }

    /// 为 Workflow actor root Turn 获取共享全局容量；等待期间响应工作流取消，
    /// 并由同一个 limiter 的 permit 释放/限额热更新唤醒，不维护旁路并发状态。
    pub(crate) async fn acquire_workflow_actor_permit(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<CollaborationGlobalTurnPermit, AgentRuntimeError> {
        let limiter = Arc::clone(&self.collaboration_global_turn_limiter);
        loop {
            if cancellation.is_cancelled() {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
            let changed = limiter.capacity_change();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(permit) = limiter
                .try_acquire_external()
                .map_err(runtime_operation_failed)?
            {
                if cancellation.is_cancelled() {
                    drop(permit);
                    return Err(AgentRuntimeError::RuntimeOperationFailed);
                }
                return Ok(permit);
            }
            tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(AgentRuntimeError::RuntimeOperationFailed);
                }
                _ = changed => {}
            }
        }
    }

    /// 读取已打开 Session 的工具图片；引用和字节完整性由 Runtime 校验。
    pub fn read_tool_image(
        &self,
        session_id: &str,
        artifact_id: &str,
    ) -> Result<Vec<u8>, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let artifact_id =
            keencode_resources::ArtifactId::new(artifact_id).map_err(runtime_operation_failed)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.read_tool_image(&artifact_id))
            .map_err(runtime_operation_failed)
    }

    /// 将桌面通知焦点切换到一个已经打开的 Session。
    pub fn focus_session(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        self.focus_change_tx.send_if_modified(|focused| {
            if focused.as_deref() == Some(session_id) {
                return false;
            }
            *focused = Some(session_id.to_owned());
            true
        });
        Ok(())
    }

    /// 清除桌面通知焦点，不关闭 Session 或取消 Turn。
    pub fn clear_focus(&self) {
        self.focus_change_tx.send_if_modified(|focused| {
            if focused.is_none() {
                return false;
            }
            *focused = None;
            true
        });
    }

    /// 返回当前桌面通知焦点的 Session 标识。
    pub fn focused_session_id(&self) -> Result<Option<String>, AgentRuntimeError> {
        Ok(self.focus_change_tx.borrow().clone())
    }

    /// 从磁盘当前唯一配置原子热替换全部 Provider。
    /// 用宿主已经解析好的 Provider 注册表代次更新默认选择。
    ///
    /// Provider 文件读取与凭据解密属于 `NativeHost` 配置边界，Runtime 只接受
    /// 已验证的 registry 和选择，避免把任何桌面句柄带入执行层。
    pub fn set_default_provider_selection(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let _reload = self
            .provider_reload
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let resolved = self
            .provider_registry
            .resolve(provider_id, model)
            .map_err(provider_reload_failed)?;
        let binding = Some(DefaultProviderBinding {
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
            generation: resolved.generation(),
        });
        *self
            .default_provider
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = binding;
        Ok(())
    }

    /// 判断当前默认 Provider 与模型能否在同一注册表代次中解析。
    pub fn provider_is_configured(&self) -> bool {
        self.resolve_default_provider().is_ok()
    }

    fn session_storage_directory(&self, session_id: &str) -> Result<PathBuf, AgentRuntimeError> {
        keencode_resources::session_storage_directory(
            &self.storage_root,
            &keencode_resources::SessionId::new(session_id).map_err(runtime_operation_failed)?,
        )
        .map_err(runtime_operation_failed)
    }

    /// 只列出指定项目的会话元数据。
    pub fn stored_sessions_for_project(
        &self,
        project_root: Option<&str>,
    ) -> anyhow::Result<Vec<StoredSessionMetadata>> {
        self.runtime_manager
            .list_stored_sessions_for_project(project_root)
            .context("列出项目会话失败")
    }

    /// 返回磁盘中全部新格式 Session 的无正文元数据。
    pub fn stored_sessions(&self) -> anyhow::Result<Vec<StoredSessionMetadata>> {
        self.runtime_manager
            .list_stored_sessions()
            .context("列出 Runtime Session 失败")
    }

    /// 读取一个健康 Session 的完整原始 Transcript。
    pub fn session_transcript(&self, session_id: &str) -> anyhow::Result<Vec<SessionMessage>> {
        let lifecycle = self
            .deferred_session_lifecycle
            .lock()
            .map_err(|_| anyhow!("Session lifecycle 状态不可用"))?;
        if lifecycle.get(session_id) == Some(&DeferredSessionLifecycle::Closing) {
            bail!("Session 正在切换工作目录");
        }
        self.runtime_manager
            .session_transcript(session_id)
            .with_context(|| format!("读取 Session {session_id} Transcript 失败"))
    }

    /// 返回仍有 Turn、子 Agent、工具、终端或工作树需要收尾的 Session 标识。
    pub fn active_session_ids(&self) -> Result<Vec<String>, AgentRuntimeError> {
        let session_ids = self
            .runtime_manager
            .registered_session_ids()
            .map_err(runtime_operation_failed)?;
        let mut active = Vec::new();
        for session_id in session_ids {
            if self.session_has_active_work(session_id.as_str())? {
                active.push(session_id.as_str().to_owned());
            }
        }
        active.sort();
        Ok(active)
    }

    /// 汇总资源层、Coordinator、Runner 准备表与后台 Shell 的真实活动状态。
    pub fn session_has_active_work(&self, session_id: &str) -> Result<bool, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        if session
            .has_active_work()
            .map_err(runtime_operation_failed)?
        {
            return Ok(true);
        }
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        let Some(collaboration) = collaboration else {
            return Ok(false);
        };
        if collaboration.execution.has_active_work()? {
            return Ok(true);
        }
        collaboration
            .coordinator
            .capacity()
            .map(|capacity| capacity.global_in_use > 0)
            .map_err(runtime_operation_failed)
    }

    /// 向产生变化的 Session 发布 Goal 变化；Goal 状态按 Session 隔离。
    pub fn publish_goal_changed(
        &self,
        source_session_id: &str,
        goal_id: Option<String>,
        revision: u64,
        status: Option<String>,
    ) {
        // Goal 文件由 PersistentAgentState 持久化；Native UI 在需要时从同一事实源读取。
        tracing::debug!(
            session_id = source_session_id,
            goal_id = ?goal_id,
            revision,
            status = ?status,
            "Goal 状态已持久化"
        );
        let _ = self.goal_change_tx.send(GoalChangeNotice {
            session_id: source_session_id.to_owned(),
        });
    }

    /// 用当前默认 Provider 执行按指定 Schema 严格校验的无工具记忆模型调用。
    pub async fn generate_isolated(
        &self,
        session_id: &str,
        system_prompt: &str,
        input: &str,
        timeout_secs: u64,
        structured_output: StructuredOutputConfig,
    ) -> anyhow::Result<String> {
        self.generate_isolated_for_purpose(
            session_id,
            system_prompt,
            input,
            timeout_secs,
            "memory",
            structured_output,
        )
        .await
    }

    /// 使用 Session 绑定 Provider 生成短标题，并按 operationId 持久复用成功结果。
    pub async fn generate_title(
        &self,
        session_id: &str,
        operation_id: &str,
        input: &str,
    ) -> Result<String, AgentRuntimeError> {
        const TITLE_SYSTEM_PROMPT: &str = "Extract the coding task topic from the user message and generate a concise Chinese title. Do not answer the user or assess whether the task can be performed. Output only a single-line title without quotes, numbering, a final period, or explanation, limited to 18 Chinese characters or 36 characters overall.";
        validate_session_id(session_id)?;
        if input.trim().is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let input_sha256 = title_input_sha256(input);
        let gate = {
            let mut gates = self
                .title_generation_gates
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            Arc::clone(
                gates
                    .entry(session_id.to_owned())
                    .or_insert_with(|| Arc::new(TitleGeneration::default())),
            )
        };
        let _gate = gate.gate.lock().await;
        if gate.cancellation.is_cancelled() {
            return Err(AgentRuntimeError::SessionUnavailable);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        if let Some(title) = session
            .cached_generated_title(operation_id, &input_sha256)
            .map_err(runtime_operation_failed)?
        {
            return Ok(title);
        }
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let provider = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
        let title = tokio::select! {
            biased;
            _ = gate.cancellation.cancelled() => return Err(AgentRuntimeError::SessionUnavailable),
            result = self.generate_isolated_with_provider(provider, IsolatedGenerationRequest {
                session_id,
                system_prompt: TITLE_SYSTEM_PROMPT,
                input,
                timeout_secs: TITLE_GENERATION_TIMEOUT_SECS,
                purpose: "title",
                structured_output: None,
            }) => result.map_err(runtime_operation_failed)?,
        };
        let title = validate_generated_title(&title)?;
        session
            .cache_generated_title(operation_id, &input_sha256, title)
            .map_err(runtime_operation_failed)
    }

    /// 执行不带业务工具的隔离模型调用，并只接受 Runtime 内部固定用途。
    async fn generate_isolated_for_purpose(
        &self,
        session_id: &str,
        system_prompt: &str,
        input: &str,
        timeout_secs: u64,
        purpose: &'static str,
        structured_output: StructuredOutputConfig,
    ) -> anyhow::Result<String> {
        if system_prompt.trim().is_empty() || input.trim().is_empty() {
            bail!("隔离模型调用的系统提示词和输入不能为空");
        }
        if !matches!(purpose, "memory" | "title") {
            bail!("隔离模型调用用途无效");
        }
        if timeout_secs == 0 {
            bail!("隔离模型调用超时必须大于零");
        }
        let provider = self
            .resolve_default_provider()
            .map_err(|error| anyhow!(error))?;
        self.generate_isolated_with_provider(
            provider,
            IsolatedGenerationRequest {
                session_id,
                system_prompt,
                input,
                timeout_secs,
                purpose,
                structured_output: Some(structured_output),
            },
        )
        .await
    }

    /// 使用绑定注册表代次的 Provider 隔离生成；结果工具仅承载数据，不执行业务操作。
    async fn generate_isolated_with_provider(
        &self,
        provider: ResolvedProvider,
        request: IsolatedGenerationRequest<'_>,
    ) -> anyhow::Result<String> {
        let IsolatedGenerationRequest {
            session_id,
            system_prompt,
            input,
            timeout_secs,
            purpose,
            structured_output,
        } = request;
        if system_prompt.trim().is_empty() || input.trim().is_empty() {
            bail!("隔离模型调用的系统提示词和输入不能为空");
        }
        if !matches!(purpose, "memory" | "title") {
            bail!("隔离模型调用用途无效");
        }
        if timeout_secs == 0 {
            bail!("隔离模型调用超时必须大于零");
        }
        let mut request = ModelRequest::new(
            provider.model(),
            vec![
                Message::text(MessageRole::System, system_prompt),
                Message::text(MessageRole::User, input),
            ],
        );
        request.tool_choice = ToolChoice::None;
        // 标题保留纯文本；记忆与普通 Turn 共用中立能力选择、结果工具和严格解析。
        let structured_mode = StructuredOutputMode::resolve(
            structured_output.as_ref(),
            &provider.capabilities(&request.model),
        )?;
        request.structured_output = structured_output.clone();
        if let Some(result_tool) = structured_mode.result_tool() {
            request.structured_output = None;
            request.tools.push(result_tool);
            request.tool_choice = ToolChoice::Required;
            request.parallel_tool_calls = Some(false);
            // 一个系统角色消息同时表达业务约束和结果通道，避免指令被拆散。
            request.messages_mut()[0] = Message::text(
                MessageRole::System,
                format!(
                    "{system_prompt}\n\nSubmit the JSON object through the value field of the sole result tool; do not emit visible prose. This tool only submits data and does not perform file, command, or network operations."
                ),
            );
        }
        request
            .metadata
            .insert(REQUEST_METADATA_PURPOSE.to_owned(), purpose.to_owned());
        // 隔离推理同样按会话注入路由标识，否则要求该 Header 的端点会拒绝请求。
        request.metadata.insert(
            REQUEST_METADATA_SESSION_ID.to_owned(),
            session_id.to_owned(),
        );
        let (response, structured_output) = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            structured_mode.complete_isolated(&provider, request),
        )
        .await
        .map_err(|_| anyhow!("隔离模型调用超时"))?
        .context("隔离模型响应不符合结构化输出约定")?;
        // HTTP 200 不代表内容符合契约；结果工具终态由共用解析器校验，绝不实际执行。
        if let Some(value) = structured_output {
            return Ok(value.to_string());
        }
        // 标题即使已产生非空文本，截断、拒答、取消或未知终态也不能写入持久缓存。
        if response.stop_reason != keencode_model::StopReason::Completed {
            bail!("隔离模型调用未完整完成");
        }
        let mut text = String::new();
        for block in response.content {
            match block {
                ContentBlock::Text { text: part } => text.push_str(&part),
                ContentBlock::Reasoning { .. } => {}
                ContentBlock::Image { .. }
                | ContentBlock::ToolCall { .. }
                | ContentBlock::ToolResult { .. } => {
                    bail!("隔离模型调用返回了不允许的非文本内容")
                }
            }
        }
        if text.trim().is_empty() {
            bail!("隔离模型调用没有返回文本");
        }
        Ok(text)
    }

    /// 在同一代次中解析当前默认 Provider，禁止热加载竞态使用旧客户端。
    fn resolve_default_provider(&self) -> Result<ResolvedProvider, AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let binding = self
            .default_provider
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .clone()
            .ok_or(AgentRuntimeError::ProviderNotConfigured)?;
        let provider = self
            .provider_registry
            .resolve(&binding.provider_id, &binding.model)
            .map_err(|_| AgentRuntimeError::ProviderNotConfigured)?;
        if provider.generation() != binding.generation {
            return Err(AgentRuntimeError::ProviderReloadFailed);
        }
        Ok(provider)
    }

    /// 解析 Session 已持久绑定的 Provider；尚未绑定时只使用当前默认选择。
    fn resolve_session_provider(
        &self,
        snapshot: Option<&ProviderSnapshot>,
    ) -> Result<ResolvedProvider, AgentRuntimeError> {
        let Some(snapshot) = snapshot else {
            return self.resolve_default_provider();
        };
        let provider = self
            .provider_registry
            .resolve(&snapshot.provider_id, &snapshot.model)
            .map_err(|_| AgentRuntimeError::ProviderNotConfigured)?;
        Ok(provider)
    }

    /// 解析子 Agent 模型；用户指定的模型优先，仅在模型不可用时回退主对话模型。
    /// 视觉等能力差异不自动回退，由主对话在子 Agent 失败后改用 `model: "inherit"` 处理。
    fn resolve_child_agent_provider(
        &self,
        session_provider: Option<&ProviderSnapshot>,
        model_reference: &str,
    ) -> Result<ResolvedProvider, AgentRuntimeError> {
        let parent = self.resolve_session_provider(session_provider)?;
        let Some(requested) =
            self.try_resolve_child_model_reference(session_provider, model_reference)
        else {
            return Ok(parent);
        };
        let requested = requested.unwrap_or_else(|_| parent.clone());
        if requested.provider_id() == parent.provider_id() && requested.model() == parent.model() {
            return Ok(parent);
        }
        Ok(requested)
    }

    /// 尝试按子 Agent 模型引用解析绑定；没有可用解析来源时返回 `None`。
    fn try_resolve_child_model_reference(
        &self,
        session_provider: Option<&ProviderSnapshot>,
        model_reference: &str,
    ) -> Option<Result<ResolvedProvider, AgentRuntimeError>> {
        if let Some((provider_id, model)) =
            split_child_agent_model_override(model_reference).ok()?
        {
            return Some(
                self.provider_registry
                    .resolve(provider_id, model)
                    .map_err(|_| AgentRuntimeError::ProviderNotConfigured),
            );
        }
        if let Some(provider) = session_provider {
            return Some(
                self.provider_registry
                    .resolve(&provider.provider_id, model_reference)
                    .map_err(|_| AgentRuntimeError::ProviderNotConfigured),
            );
        }
        let default = self.resolve_default_provider().ok()?;
        Some(
            self.provider_registry
                .resolve(default.provider_id(), model_reference)
                .map_err(|_| AgentRuntimeError::ProviderNotConfigured),
        )
    }

    /// 原子更新 WebFetch/WebSearch 的检索网关配置；空值会让下一轮不注册网络工具。
    ///
    /// 该配置服务于 Agent 的编程检索工具，不能与已移除的 HTML/WebView UI 宿主混同。
    pub(crate) fn set_web_service_config(
        &self,
        config: Option<WebServiceConfig>,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        *self
            .web_service
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = config;
        Ok(())
    }

    /// 原子发布一代完整扩展候选，失败或过时代次不会替换当前运行快照。
    pub fn publish_extension_candidate(
        &self,
        project_root: &Path,
        candidate: RuntimeExtensionCandidate,
    ) -> Result<u64, AgentRuntimeError> {
        let refresh_epoch = self.extension_candidate_refresh_epoch(project_root)?;
        self.publish_extension_candidate_at_epoch(project_root, candidate, refresh_epoch)
    }

    /// 返回项目当前的资源刷新 epoch，供候选构建在读取输入前建立一致性边界。
    pub(crate) fn extension_candidate_refresh_epoch(
        &self,
        project_root: &Path,
    ) -> Result<u64, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let epoch = self
            .extension_candidate_refresh_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .copied()
            .unwrap_or(0);
        Ok(epoch)
    }

    /// 仅在构建输入期间没有发生资源写入时发布候选。
    pub(crate) fn publish_extension_candidate_at_epoch(
        &self,
        project_root: &Path,
        candidate: RuntimeExtensionCandidate,
        refresh_epoch: u64,
    ) -> Result<u64, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let current_refresh_epoch = self
            .extension_candidate_refresh_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .copied()
            .unwrap_or(0);
        if current_refresh_epoch != refresh_epoch {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let generation = candidate.generation();
        let mut current = self
            .extension_candidates
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if current
            .get(&project_root)
            .is_some_and(|current| current.generation() >= candidate.generation())
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let candidate = Arc::new(candidate);
        current.insert(project_root.clone(), Arc::clone(&candidate));
        drop(current);
        self.extension_candidate_published_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .insert(project_root, refresh_epoch);
        Ok(generation)
    }

    /// 标记一个项目的扩展候选需要重建；当前候选继续服务已开始的 Turn。
    ///
    /// 设置域写入插件、MCP、Skill 或 Agent 模板后调用此入口。候选构建仍由
    /// NativeHost 串行完成，只有完整构建成功并发布后才会清除标记。
    pub(crate) fn invalidate_extension_candidate(
        &self,
        project_root: &Path,
    ) -> Result<(), AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let mut epochs = self
            .extension_candidate_refresh_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let epoch = epochs.entry(project_root).or_default();
        *epoch = epoch
            .checked_add(1)
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        Ok(())
    }

    /// 同步撤销全部项目候选中的 MCP 工具，供配置失效或变更时 fail-closed 使用。
    pub fn revoke_mcp_extension_tools(&self) -> Result<(), AgentRuntimeError> {
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let candidates = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .values()
            .map(Arc::clone)
            .collect::<Vec<_>>();
        for candidate in candidates {
            candidate
                .contributor
                .revoke_mcp_tools()
                .map_err(runtime_operation_failed)?;
            candidate.mcp_revoked.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// 撤销一个项目当前候选的 MCP 工具，不影响其他项目已授权的 Server。
    pub fn revoke_project_mcp_extension_tools(
        &self,
        project_root: &Path,
    ) -> Result<(), AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let candidate = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned();
        if let Some(candidate) = candidate {
            candidate
                .contributor
                .revoke_mcp_tools()
                .map_err(runtime_operation_failed)?;
            candidate.mcp_revoked.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// 认证失败仅撤销当前工具，后续用户请求才重建，防止失败通知触发无限联网重试。
    pub(crate) fn extension_candidate_needs_refresh(
        &self,
        project_root: &Path,
    ) -> Result<bool, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let _change_gate = self
            .extension_candidate_change_gate
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let latest_refresh_epoch = self
            .extension_candidate_refresh_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .copied()
            .unwrap_or(0);
        let published_epoch = self
            .extension_candidate_published_epochs
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .copied()
            .unwrap_or(0);
        if latest_refresh_epoch != published_epoch {
            return Ok(true);
        }
        self.extension_candidates
            .read()
            .map(|candidates| {
                candidates
                    .get(&project_root)
                    .is_none_or(|candidate| candidate.mcp_revoked.load(Ordering::Acquire))
            })
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 返回指定规范项目根当前扩展候选代次；尚未发布时为空。
    pub fn extension_generation(
        &self,
        project_root: &Path,
    ) -> Result<Option<u64>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        self.extension_candidates
            .read()
            .map(|candidates| {
                candidates
                    .get(&project_root)
                    .map(|candidate| candidate.generation())
            })
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 返回指定项目当前已发布候选的 MCP 运行态；没有候选时返回 `None`。
    ///
    /// 该查询只读取已发布的不可变候选，不会因查看状态而初始化 MCP Server。
    pub fn mcp_runtime_snapshot(
        &self,
        project_root: &Path,
    ) -> Result<Option<Vec<RuntimeMcpServerSnapshot>>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        self.extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)
            .map(|candidates| {
                candidates
                    .get(&project_root)
                    .map(|candidate| candidate.contributor.mcp_runtime_snapshot())
            })
    }

    /// 返回指定项目当前候选冻结的插件引用目录；没有候选时拒绝回退到实时工作区扫描。
    pub fn plugin_reference_catalog(
        &self,
        project_root: &Path,
    ) -> Result<Option<Vec<Value>>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        self.extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)
            .map(|candidates| {
                candidates
                    .get(&project_root)
                    .map(|candidate| candidate.contributor.plugin_reference_catalog())
            })
    }

    /// 返回指定项目当前已发布候选的 Skill Composer 引用目录；没有候选时返回 `None`。
    ///
    /// 调用方应先完成扩展候选初始化；本方法只读取不可变候选，不扫描工作区或用户目录。
    pub fn skill_reference_catalog(
        &self,
        project_root: &Path,
    ) -> Result<Option<Vec<Value>>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        self.extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)
            .map(|candidates| {
                candidates
                    .get(&project_root)
                    .map(|candidate| candidate.contributor.skill_reference_catalog())
            })
    }

    /// 在不启动模型或 Hook 的情况下，为已打开 Session 预热 Composer 扩展引用目录。
    ///
    /// 调用方必须先完成当前项目扩展候选初始化。尚未产生用户事实的 deferred
    /// draft 可以在首发前重新读取当前候选，覆盖预热时的目录；一旦 Journal 已有
    /// 用户事实，或执行端已经完成完整 context 冻结，目录继续保持权威快照。未知
    /// Session 或未发布候选均明确失败。
    pub fn initialize_session_extension_reference_catalog(
        &self,
        session_id: &str,
        project_root: &Path,
    ) -> Result<SessionExtensionReferenceCatalog, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let project_root = canonical_project_root(project_root)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        ensure_session_project(&session, &project_root)?;
        // Collaboration Runtime 可能已经在 Journal 首发前完成完整 context 装配（例如
        // 恢复或测试路径）。此时执行端的冻结目录才是 SkillTool 的权威，不能让
        // 后续 Composer 查询用工作区候选覆盖它；同时把同一快照回写到 UI 投影，
        // 避免界面目录与实际工具目录分裂。
        let collaboration = {
            let runtimes = self
                .collaboration_sessions
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            runtimes.get(session_id).cloned()
        };
        let frozen_execution_catalog = collaboration
            .map(|collaboration| {
                collaboration
                    .execution
                    .state
                    .lock()
                    .map(|state| state.frozen_extension_reference_catalog.clone())
                    .map_err(|_| AgentRuntimeError::StateUnavailable)
            })
            .transpose()?
            .flatten();
        if let Some(catalog) = frozen_execution_catalog {
            self.session_extension_reference_catalogs
                .write()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .insert(session_id.to_owned(), catalog.clone());
            return Ok(catalog);
        }
        let candidate = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        let snapshot = SessionExtensionReferenceCatalog {
            plugins: candidate.contributor.plugin_reference_catalog(),
            skills: candidate.contributor.skill_reference_catalog(),
        };
        let refresh_deferred = session
            .read_state(session_has_persisted_user_facts)
            .map(|has_facts| !has_facts)
            .map_err(runtime_operation_failed)?;
        let mut catalogs = self
            .session_extension_reference_catalogs
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if refresh_deferred {
            // 预热 draft 的候选可能在 createSession ACK 后才完成扫描；首发前刷新
            // 同一 Session 的引用目录，保证 Mention 选中的 Skill 能进入首轮工具表。
            catalogs.insert(session_id.to_owned(), snapshot.clone());
            Ok(snapshot)
        } else {
            Ok(catalogs
                .entry(session_id.to_owned())
                .or_insert(snapshot)
                .clone())
        }
    }

    /// 返回指定已打开 Session 首次完整 context 冻结的扩展引用目录。
    ///
    /// 尚未完成首次完整 context 的 Session 返回 `None`，调用方必须明确报告
    /// 目录尚未可用；未知 Session 同样失败，不能回退到当前工作区候选。
    pub fn session_extension_reference_catalog(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionExtensionReferenceCatalog>, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        if let Some(catalog) = self
            .session_extension_reference_catalogs
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
        {
            return Ok(Some(catalog));
        }
        let execution = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .map(|collaboration| Arc::clone(&collaboration.execution));
        let Some(execution) = execution else {
            return Ok(None);
        };
        execution
            .state
            .lock()
            .map(|state| state.frozen_extension_reference_catalog.clone())
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 从当前项目已经原子发布的候选中解析一个显式 Agent 模板。
    pub fn resolve_extension_agent(
        &self,
        project_root: &Path,
        name: &str,
        parent: &RuntimeAgentTemplateContext,
    ) -> Result<Option<RuntimeAgentTemplate>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let candidate = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        candidate
            .contributor
            .resolve_agent(name, parent)
            .map_err(runtime_operation_failed)
    }

    /// 冻结当前项目候选代次并返回供本 Turn spawn_agent 使用的模板解析器。
    pub fn spawn_agent_template_resolver(
        &self,
        project_root: &Path,
    ) -> Result<Arc<dyn SpawnAgentTemplateResolver>, AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let candidate = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        Ok(Arc::new(RuntimeSpawnAgentTemplateResolver {
            contributor: Arc::clone(&candidate.contributor),
        }))
    }

    /// 延迟创建或返回一个 Session 唯一的 Collaboration v2 生产装配。
    fn ensure_collaboration_runtime(
        self: &Arc<Self>,
        session: &RuntimeSession,
        seed: RootAgentSeed,
    ) -> Result<Arc<SessionCollaborationRuntime>, AgentRuntimeError> {
        let session_id = session.session_id().as_str().to_owned();
        let mut runtimes = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if let Some(runtime) = runtimes.get(&session_id) {
            return Ok(Arc::clone(runtime));
        }
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let project_root = canonical_project_root(Path::new(&snapshot.state.project_root))?;
        let persistent_state = Arc::new(
            PersistentAgentState::open_with_goal_root(session.clone(), &self.storage_root)
                .map_err(runtime_operation_failed)?,
        );
        let background_tasks = Arc::new(
            BackgroundTaskManager::new(
                self.session_storage_directory(&session_id)?
                    .join("background-tasks"),
                BACKGROUND_OUTPUT_CHUNK_BYTES,
            )
            .map_err(runtime_operation_failed)?,
        );
        let worktrees = Arc::new(
            GitWorktreeLeaseManager::open(
                self.session_storage_directory(&session_id)?
                    .join("worktrees"),
            )
            .map_err(runtime_operation_failed)?,
        );
        worktrees
            .recover_stale()
            .map_err(runtime_operation_failed)?;
        let store = Arc::new(SessionCollaborationStore::new(
            &self.storage_root,
            &session_id,
        )?);
        store.bind_runtime_session(session)?;
        let execution = Arc::new(RuntimeAgentExecution::new(RuntimeAgentExecutionContext {
            owner: Arc::downgrade(self),
            session: session.clone(),
            project_root: project_root.clone(),
            persistent_state: Arc::clone(&persistent_state),
            background_tasks: Arc::clone(&background_tasks),
            executor_handle: self.executor_handle.clone(),
            worktrees: Arc::clone(&worktrees),
            store: Arc::clone(&store),
        }));
        let turn_limit = self.background_agent_limit.load(Ordering::Acquire);
        let coordinator = Arc::new(CollaborationCoordinator::new_with_global_turn_limiter(
            Arc::clone(&self.collaboration_global_turn_limiter),
            store.clone(),
            execution.clone(),
            Arc::new(UuidCollaborationIdGenerator),
        ));
        execution.bind_coordinator(&coordinator)?;
        let root_agent_id = RunnerAgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
            .map_err(|_| AgentRuntimeError::InvalidSession)?;
        let recovered_transition = store
            .load_transition_snapshot()
            .map_err(runtime_operation_failed)?;
        let recovered = recovered_transition
            .as_ref()
            .map(|transition| transition.commit.checkpoint.clone());
        let waiting_capacity_records = recovered_transition
            .as_ref()
            .map(|transition| transition.unstarted_turn_terminations.clone())
            .unwrap_or_default();
        reconcile_cold_unstarted_turn_terminations(
            session,
            &store,
            &snapshot.state,
            recovered.as_ref(),
            &waiting_capacity_records,
        )?;
        let refreshed_snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let waiting_capacity_turns = waiting_capacity_records
            .iter()
            .map(|record| record.turn_id.clone())
            .collect::<HashSet<_>>();
        let recovered_claims = recovered_dynamic_input_claims(recovered.as_ref())?;
        let authoritative_outcomes = recovered_authoritative_turn_outcomes_with_waiting_capacity(
            Some(session),
            recovered.as_ref(),
            &refreshed_snapshot.state,
            &waiting_capacity_turns,
        )?;
        let handles = if let Some(checkpoint) = recovered.as_ref() {
            coordinator
                .restore_coordinator_with_authoritative_outcomes(
                    checkpoint.clone(),
                    &authoritative_outcomes,
                )
                .map_err(runtime_operation_failed)?
        } else {
            Vec::new()
        };
        if handles
            .iter()
            .any(|handle| handle.agent_id != root_agent_id)
            || handles.len() > 1
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        if !handles.is_empty() {
            coordinator
                .update_root_turn_limit(&root_agent_id, turn_limit)
                .map_err(runtime_operation_failed)?;
        }
        if handles.is_empty() {
            let provisional_profile = AgentProfile {
                model: seed.model,
                reasoning_effort: seed.reasoning_effort,
                plan_guard: seed.plan_guard,
                cwd: project_root,
                worktree_lease: None,
                tool_snapshot: Vec::new(),
            };
            let (registry, _, _) = self.assemble_agent_tools(
                &execution,
                Arc::clone(&coordinator),
                &provisional_profile,
                "general-purpose",
                provisional_profile.plan_guard,
                AgentCapabilities {
                    can_spawn_agent: true,
                },
            )?;
            let mut profile = provisional_profile;
            profile.tool_snapshot = registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
            coordinator
                .register_root_with_id(
                    root_agent_id.clone(),
                    RootAgentRequest {
                        session_id: AgentSessionId::new(session_id.clone())
                            .map_err(|_| AgentRuntimeError::InvalidSession)?,
                        profile,
                        per_root_turn_limit: turn_limit,
                    },
                )
                .map_err(runtime_operation_failed)?;
        }
        recover_dynamic_input_acknowledgements(session, &coordinator, &recovered_claims)?;
        coordinator
            .reconcile_outbox()
            .map_err(runtime_operation_failed)?;
        let completion_events = background_tasks.subscribe_completions();
        let (background_completion_cancel, background_completion_cancelled) = oneshot::channel();
        let runtime = Arc::new(SessionCollaborationRuntime {
            coordinator,
            store,
            execution,
            root_agent_id,
            background_completion_cancel: Mutex::new(Some(background_completion_cancel)),
        });
        runtimes.insert(session_id.clone(), Arc::clone(&runtime));
        self.executor_handle
            .spawn(run_background_task_completion_pump(
                Arc::downgrade(self),
                session_id,
                completion_events,
                background_completion_cancelled,
            ));
        Ok(runtime)
    }

    /// 为 Coordinator 已预约的一条根或子 Turn 构建完整 Runtime 请求与 Runner。
    fn build_runtime_launch(
        &self,
        execution: &RuntimeAgentExecution,
        launch: &AgentTurnLaunch,
        prepared_root: Option<&PreparedRootTurn>,
    ) -> Result<
        (
            keencode_runtime::RuntimeAgentRunner,
            RuntimeTurnRequest,
            String,
        ),
        AgentRuntimeError,
    > {
        if launch.agent.root_session_id.as_str() != execution.session_id
            || launch.agent.root_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let is_root = launch.agent.depth == AgentDepth::ROOT;
        let (resolved, reasoning_effort, input_messages, turn_context, summary) = if is_root {
            let prepared = prepared_root.ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            (
                prepared.provider.clone(),
                prepared
                    .reasoning_effort
                    .map(reasoning_effort_from_snapshot),
                prepared.input_messages.clone(),
                prepared.request_context.clone(),
                prepared.summary.clone(),
            )
        } else {
            let snapshot = execution
                .session
                .snapshot()
                .map_err(runtime_operation_failed)?;
            let resolved = self.resolve_child_agent_provider(
                snapshot.state.provider.as_ref(),
                &launch.agent.profile.model,
            )?;
            let reasoning = launch
                .agent
                .profile
                .reasoning_effort
                .as_deref()
                .map(parse_reasoning_effort)
                .transpose()?
                .flatten();
            let mut input_messages = Vec::new();
            if matches!(launch.cause, AgentTurnCause::InitialTask) {
                let assignment = launch
                    .agent
                    .assignment
                    .as_deref()
                    .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
                let mut system = format!(
                    "You are a single-level child agent. Your canonical path is {self_path}; your parent path is /root. Your stable lifecycle assignment, visible to every agent in this root tree, is: {assignment}. Complete work within that assignment and report verifiable results with send_message using target=/root; never use followup_task for /root. Use list_agents to discover siblings and their assignments, and address siblings only by absolute /root/<child> paths.",
                    self_path = launch.agent.path.as_str(),
                );
                if let Some(template) = launch.agent.agent_template.as_ref()
                    && !template.system_prompt.trim().is_empty()
                {
                    system.push_str("\n\n");
                    system.push_str(&template.system_prompt);
                }
                input_messages.push(Message::text(MessageRole::System, system));
            }
            if let Some(prompt) = launch.prompt.as_ref() {
                input_messages.push(Message::text(MessageRole::User, prompt));
            }
            let summary = child_agent_turn_summary(launch.prompt.as_deref());
            (resolved, reasoning, input_messages, Vec::new(), summary)
        };
        let mut turn_provider_snapshot = provider_snapshot(&resolved);
        turn_provider_snapshot.reasoning_effort = reasoning_effort.map(reasoning_effort_snapshot);
        let small_context = crate::agent_prompt::is_small_context(
            resolved.capabilities(resolved.model()).max_context_tokens,
        );

        let source_resource_id =
            keencode_resources::AgentId::new(launch.agent.agent_id.as_str().to_owned())
                .map_err(runtime_operation_failed)?;
        let transcript_with_turn_ids = if is_root {
            execution
                .session
                .model_transcript_for_agent_with_turn_ids(&source_resource_id)
                .map_err(runtime_operation_failed)?
        } else {
            let mut inherited = launch
                .agent
                .context_snapshot
                .iter()
                .map(|message| {
                    serde_json::from_str::<Message>(message)
                        .map(|message| (None, message))
                        .map_err(runtime_operation_failed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !matches!(launch.cause, AgentTurnCause::InitialTask) {
                inherited.extend(
                    execution
                        .session
                        .model_transcript_for_agent_with_turn_ids(&source_resource_id)
                        .map_err(runtime_operation_failed)?,
                );
            }
            inherited
        };
        let mut transcript = Vec::with_capacity(transcript_with_turn_ids.len());
        let mut transcript_turn_ids = Vec::with_capacity(transcript_with_turn_ids.len());
        for (turn_id, message) in transcript_with_turn_ids {
            transcript_turn_ids.push(turn_id);
            transcript.push(message);
        }
        // 仅保留由同一 Provider、模型、协议和完整配置身份生成的 opaque reasoning 续传；
        // 不修改 Journal 中的原始推理，也不移除本轮工具循环新生成的续传状态。
        let historical_providers = execution.historical_provider_snapshots()?;
        clear_historical_reasoning_state(
            &mut transcript,
            &transcript_turn_ids,
            &resolved,
            &historical_providers,
        );
        // 动态上下文只在 Provider 边界装配，持久输入仍按原顺序进入 Runtime Journal。
        transcript.extend(input_messages.clone());

        let coordinator = execution.coordinator().map_err(runtime_operation_failed)?;
        let (registry, hooks, catalog) = if small_context {
            self.assemble_small_context_tools(execution, &launch.agent.profile)?
        } else {
            self.assemble_agent_tools(
                execution,
                Arc::clone(&coordinator),
                &launch.agent.profile,
                launch
                    .agent
                    .agent_template
                    .as_ref()
                    .map(|template| template.name.as_str())
                    .unwrap_or("general-purpose"),
                launch.plan_guard,
                launch.capabilities,
            )?
        };
        if is_root && !small_context {
            self.log_extension_diagnostics(execution, &launch.agent.agent_id);
        }
        let tool_snapshot = request_tool_snapshot(&launch.agent.profile, is_root, small_context);
        let tools = registry
            .select_exact(&tool_snapshot)
            .map_err(runtime_operation_failed)?;
        let can_spawn = tools
            .definitions()
            .iter()
            .any(|tool| tool.name == "spawn_agent");
        let has_skill = tools.definitions().iter().any(|tool| tool.name == "Skill");
        // 会话首次 Turn 前按 Agent 冻结稳定前缀事实；后续 Turn 一律复用冻结值。
        let frozen = self.frozen_agent_prompt(
            execution,
            &launch.agent.agent_id,
            FrozenPromptContext {
                cwd: &launch.agent.profile.cwd,
                can_spawn,
                has_skill,
                catalog: &catalog,
                small_context,
                inject_agents_md: launch
                    .agent
                    .agent_template
                    .as_ref()
                    .is_none_or(|template| template.inject_agents_md),
            },
        )?;
        // 完整模式把 Memory/Plan 等动态上下文与环境放在历史之前；小上下文
        // 只保留独立核心提示词、普通对话输入及必要恢复说明。稳定前缀跨 Turn 字节稳定。
        let mut request_context = Vec::new();
        if !small_context {
            let mut environment_message = Message::text(
                MessageRole::Developer,
                frozen
                    .environment
                    .render(launch.plan_guard == PlanGuard::read_only()),
            );
            environment_message.is_meta = true;
            request_context.push(environment_message);
            request_context.extend(turn_context);
        }
        // 上一条非正常终态只作为本轮请求期 Developer 上下文重建；权威工具结果、
        // 外部副作用和终态事实继续由 Journal 历史提供，当前用户输入仍排在 marker 后。
        let previous_stop_notice = execution
            .session
            .read_state(|state| {
                interruption_context::previous_turn_stop_notice(state, &source_resource_id)
            })
            .map_err(runtime_operation_failed)?;
        if let Some(notice) = previous_stop_notice {
            request_context.push(notice);
        }
        // 会话稳定缓存路由键只在端点 allowlist 内装配；键值随 Session 而非 Turn 漂移。
        let prompt_cache_key =
            prompt_cache_key_for_endpoint(resolved.base_url(), &execution.session_id);
        let provider = Arc::new(
            TurnBoundProvider::new(
                Arc::new(resolved.clone()),
                &execution.session_id,
                launch.turn_id.as_str(),
                launch.agent.agent_id.as_str(),
            )
            .with_prompt_cache_key(prompt_cache_key.clone())
            .with_stable_prefix(frozen.stable_prefix())
            .with_request_context(request_context),
        );
        // 压缩摘要不能看到只服务于当前模型请求的动态上下文，避免把它间接写入摘要 Transcript。
        let compressor_provider: Arc<dyn ModelProvider> = Arc::new(
            TurnBoundProvider::new(
                Arc::new(resolved.clone()),
                &execution.session_id,
                launch.turn_id.as_str(),
                launch.agent.agent_id.as_str(),
            )
            .with_prompt_cache_key(prompt_cache_key),
        );
        let context = ContextManager::new(
            ContextPolicy::default(),
            provider.clone(),
            Arc::new(ProviderContextCompressor::new(compressor_provider)),
        )
        .map_err(runtime_operation_failed)?;
        let mut request = TurnRequest::new(
            AgentSessionId::new(execution.session_id.clone())
                .map_err(|_| AgentRuntimeError::InvalidSession)?,
            launch.turn_id.clone(),
            launch.agent.agent_id.clone(),
            resolved.model(),
            transcript,
            launch.plan_guard,
        );
        request.set_cancellation(launch.cancellation.clone());
        request.model_request_mut().max_output_tokens = resolved
            .capabilities(resolved.model())
            .max_output_tokens
            .map(u32::try_from)
            .transpose()
            .map_err(runtime_operation_failed)?;
        request.model_request_mut().reasoning = reasoning_effort.map(|effort| ReasoningConfig {
            effort: Some(effort),
            max_tokens: None,
            include_summary: true,
        });
        let mut limits = RunLimits::default();
        if let Some(max_turns) = launch
            .agent
            .agent_template
            .as_ref()
            .and_then(|template| template.max_turns)
        {
            limits.max_rounds = Some(max_turns);
        }
        let mut runner = AgentRunner::new(provider, tools, limits)
            .with_context_manager(context)
            .with_hook_runtime(hooks)
            .with_tool_approval_gate(self.permissions.clone())
            .with_dynamic_input_source(Arc::new(RuntimeDynamicInputSource {
                store: Arc::clone(&execution.store),
                session_id: execution.session_id.clone(),
                coordinator,
                session: execution.session.clone(),
            }));
        if is_root {
            runner = runner
                .with_goal_controller(Arc::new(RuntimeGoalController {
                    state: execution.persistent_state.clone(),
                    owner: execution.owner.clone(),
                    session_id: execution.session_id.clone(),
                }))
                .with_external_goal_continuation()
                .with_todo_controller(execution.persistent_state.clone());
        }
        let runner = execution.session.bind_agent_runner_with_usage_sink(
            runner,
            Arc::new(RuntimeGoalUsageSink {
                session_id: execution.session_id.clone(),
                persistent_state: Arc::clone(&execution.persistent_state),
                owner: execution.owner.clone(),
            }),
        );
        let runtime_request = if is_root {
            RuntimeTurnRequest::root(request, input_messages, summary.clone())
                .with_provider_snapshot(turn_provider_snapshot.clone())
        } else {
            let parent_turn_id = launch
                .parent_turn_id
                .as_ref()
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            if matches!(launch.cause, AgentTurnCause::InitialTask) {
                RuntimeTurnRequest::initial_child(
                    request,
                    input_messages,
                    launch.root_turn_id.as_str(),
                    parent_turn_id.as_str(),
                    summary.clone(),
                    SubAgentState {
                        agent_id: source_resource_id,
                        parent_agent_id: keencode_resources::AgentId::new(
                            launch
                                .agent
                                .parent_agent_id
                                .as_ref()
                                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?
                                .as_str()
                                .to_owned(),
                        )
                        .map_err(runtime_operation_failed)?,
                        agent_path: launch.agent.path.as_str().to_owned(),
                        task: launch
                            .prompt
                            .clone()
                            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?,
                        status: SubAgentStatus::Pending,
                        current_turn_id: None,
                        result_summary: None,
                    },
                )
                .with_provider_snapshot(turn_provider_snapshot.clone())
            } else {
                RuntimeTurnRequest::child(
                    request,
                    input_messages,
                    launch.root_turn_id.as_str(),
                    parent_turn_id.as_str(),
                    summary.clone(),
                )
                .with_provider_snapshot(turn_provider_snapshot.clone())
            }
        };
        execution.remember_turn_provider(&launch.turn_id, turn_provider_snapshot)?;
        Ok((runner, runtime_request, summary))
    }

    /// 返回该 Agent 在本会话内冻结的稳定提示词事实；首次 Turn 前计算一次。
    ///
    /// 冻结语义是显式的产品取舍：AGENTS.md、CLAUDE.local.md、日期、时区与
    /// cwd 派生值只在冻结时点读取，会话中途的外部修改不再即时生效，只有新
    /// 会话（或子 Agent 的首次 Turn）取新值，以此换取模型请求 System 段跨
    /// Turn 的字节稳定性。can_spawn/has_skill 或扩展目录变化属于工具表变化
    /// 时的合法缓存失效（低频）：此时只重建能力说明与目录并记录诊断，环境
    /// 快照与指令正文仍保持首次冻结值。
    fn frozen_agent_prompt(
        &self,
        execution: &RuntimeAgentExecution,
        agent_id: &RunnerAgentId,
        context: FrozenPromptContext<'_>,
    ) -> Result<Arc<FrozenAgentPrompt>, AgentRuntimeError> {
        let FrozenPromptContext {
            cwd,
            can_spawn,
            has_skill,
            catalog,
            small_context,
            inject_agents_md,
        } = context;
        let mut state = execution
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if let Some(frozen) = state.frozen_prompts.get(agent_id) {
            let effective_catalog = if small_context { "" } else { catalog };
            if frozen.capability_fingerprint == (can_spawn, has_skill)
                && frozen.catalog == effective_catalog
                && frozen.small_context == small_context
            {
                return Ok(Arc::clone(frozen));
            }
            tracing::info!(
                target: "keencode_diagnostics",
                session_id = %execution.session_id,
                agent_id = %agent_id,
                "agent capability context changed; rebuilding frozen prompt capability section"
            );
            let rebuilt =
                FrozenAgentPrompt::refreshed(frozen, can_spawn, has_skill, catalog, small_context);
            state
                .frozen_prompts
                .insert(agent_id.clone(), Arc::clone(&rebuilt));
            return Ok(rebuilt);
        }
        let (mut custom_instructions, project_instructions) =
            crate::personalization::prompt_context_for_agent(
                &self.storage_root,
                cwd,
                inject_agents_md,
            )
            .map_err(|_| AgentRuntimeError::InstructionsUnavailable)?;
        if let Some(instructions) = project_instructions {
            if !custom_instructions.is_empty() {
                custom_instructions.push_str("\n\n");
            }
            custom_instructions.push_str(&instructions);
        }
        let frozen = Arc::new(FrozenAgentPrompt {
            environment: crate::agent_prompt::EnvironmentSnapshot::freeze(
                cwd,
                &chrono::Local::now().fixed_offset(),
            ),
            custom_instructions,
            capabilities: crate::agent_prompt::capabilities(can_spawn, has_skill),
            capability_fingerprint: (can_spawn, has_skill),
            catalog: if small_context {
                String::new()
            } else {
                catalog.to_owned()
            },
            small_context,
        });
        // root 条目跨整个 Session 保留；越过软上限时先按 root 与活跃 Agent 收缩，
        // 保证单会话内冻结条目数有界（详见 FROZEN_PROMPT_SOFT_LIMIT 的取舍说明）。
        let running_agent_ids = state
            .running_turns
            .values()
            .map(|turn| turn.agent_id.clone())
            .collect::<Vec<_>>();
        retain_live_frozen_prompts(
            &mut state.frozen_prompts,
            running_agent_ids.into_iter(),
            agent_id,
        );
        state
            .frozen_prompts
            .insert(agent_id.clone(), Arc::clone(&frozen));
        Ok(frozen)
    }

    /// 每个 Session/候选代次只记录一次非致命扩展诊断，不投递界面消息。
    fn log_extension_diagnostics(
        &self,
        execution: &RuntimeAgentExecution,
        agent_id: &RunnerAgentId,
    ) {
        if agent_id.as_str() != keencode_resources::ROOT_AGENT_ID {
            return;
        }
        let project_root = &execution.project_root;
        let candidate = match self.extension_candidates.read() {
            Ok(candidates) => candidates.get(project_root).cloned(),
            Err(_) => {
                tracing::warn!(target: "extensions", "读取扩展候选诊断失败");
                return;
            }
        };
        let Some(candidate) = candidate else {
            return;
        };
        if candidate.contributor.diagnostics().is_empty() {
            return;
        }
        match execution.claim_extension_diagnostics(candidate.generation()) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(target: "extensions", %error, "登记扩展诊断日志状态失败");
                return;
            }
        }
        for diagnostic in candidate.contributor.diagnostics() {
            tracing::warn!(target: "extensions", session_id = %execution.session_id,
                diagnostic = %extension_diagnostic_message(diagnostic), "扩展非致命诊断");
        }
    }

    /// 为单个 Agent Turn 装配完整候选工具表；调用方必须最后按 Profile 精确筛选。
    /// 已发布候选存在时，为新打开 Session 复制 UI 引用目录；不启动任何扩展运行时。
    fn freeze_session_reference_catalog_if_available(
        &self,
        session_id: &str,
        project_root: &Path,
    ) -> Result<(), AgentRuntimeError> {
        if self
            .session_extension_reference_catalogs
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .contains_key(session_id)
        {
            return Ok(());
        }
        let project_root = canonical_project_root(project_root)?;
        let Some(candidate) = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned()
        else {
            return Ok(());
        };
        let snapshot = SessionExtensionReferenceCatalog {
            plugins: candidate.contributor.plugin_reference_catalog(),
            skills: candidate.contributor.skill_reference_catalog(),
        };
        self.session_extension_reference_catalogs
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .entry(session_id.to_owned())
            .or_insert(snapshot);
        Ok(())
    }

    /// 首次完整工具装配时复制候选的 UI 引用目录；之后 Session 查询只读该副本。
    fn freeze_extension_reference_catalog(
        &self,
        execution: &RuntimeAgentExecution,
        extension: Option<&Arc<RuntimeExtensionCandidate>>,
    ) -> Result<(), AgentRuntimeError> {
        let catalog = {
            let mut state = execution
                .state
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            if state.frozen_extension_reference_catalog.is_some() {
                return Ok(());
            }
            let catalog = SessionExtensionReferenceCatalog {
                plugins: extension
                    .map(|candidate| candidate.contributor.plugin_reference_catalog())
                    .unwrap_or_default(),
                skills: extension
                    .map(|candidate| candidate.contributor.skill_reference_catalog())
                    .unwrap_or_default(),
            };
            state.frozen_extension_reference_catalog = Some(catalog.clone());
            catalog
        };
        self.session_extension_reference_catalogs
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .entry(execution.session_id.clone())
            .or_insert(catalog);
        Ok(())
    }

    fn assemble_agent_tools(
        &self,
        execution: &RuntimeAgentExecution,
        coordinator: Arc<CollaborationCoordinator>,
        profile: &AgentProfile,
        agent_type: &str,
        plan_guard: PlanGuard,
        capabilities: AgentCapabilities,
    ) -> Result<(ToolRegistry, HookRuntime, String), AgentRuntimeError> {
        let workflow_actor = workflow_actor_environment_required(execution, &profile.cwd)?;
        // 工作流执行者虽然有独立会话，但能力属于单层子会话；不可借再次打开或更换模型扩大权限。
        let capabilities = AgentCapabilities {
            can_spawn_agent: capabilities.can_spawn_agent
                && !execution
                    .session
                    .is_workflow_actor()
                    .map_err(runtime_operation_failed)?,
        };
        let project_root = execution.project_root.clone();
        let output_directory = self
            .session_storage_directory(&execution.session_id)?
            .join("tool-output");
        // 首个会话构建环境前有界等待后台 PATH 捕获收尾；超时不阻塞会话创建。
        crate::shell_env::wait_for_capture_applied(Duration::from_secs(4));
        let environment = Arc::new(
            ToolEnvironment::new(&profile.cwd)
                .and_then(|environment| {
                    environment.with_artifact_directory(output_directory.clone())
                })
                .map(|environment| {
                    environment.with_file_mutation_recorder(Arc::new(
                        file_changes::RuntimeFileMutationRecorder::new(execution.session.clone()),
                    ))
                })
                .map(|environment| {
                    if workflow_actor {
                        environment.with_workspace_guard()
                    } else {
                        environment
                    }
                })
                .map_err(runtime_operation_failed)?,
        );
        let mut tools = ToolRegistry::new();
        register_local_tools_with_background(
            &mut tools,
            environment.clone(),
            Arc::clone(&execution.background_tasks),
        )
        .map_err(runtime_operation_failed)?;
        register_state_tools(
            &mut tools,
            execution.persistent_state.clone(),
            execution.persistent_state.clone(),
            execution.persistent_state.clone(),
        )
        .map_err(runtime_operation_failed)?;
        if let Some(web_service) = self
            .web_service
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .clone()
        {
            register_web_tools(&mut tools, environment.clone(), web_service)
                .map_err(runtime_operation_failed)?;
        }
        if capabilities.can_spawn_agent
            && let Some(port) = self
                .workflow_tool_port
                .read()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .clone()
        {
            // 只有可创建子 Agent 的普通 root 才冻结 Workflow 控制面；child 和
            // workflow actor 在上面的能力收缩后都不会发现这些递归入口。
            crate::workflows::agent_tools::register_workflow_tools(&mut tools, port)
                .map_err(runtime_operation_failed)?;
        }
        // 只有 Native Host typed 连接完成绑定时才暴露问答工具；问题正文和答案均由
        // ElicitationCoordinator 的 pending 账本承载，装配阶段不创建桌面 transport。
        let elicitation_target = self.workflow_elicitation_target(&execution.session_id)?;
        if self.elicitations.session_supports_form(&elicitation_target) {
            let connection_id = self
                .elicitations
                .session_connection(&elicitation_target)
                .ok_or(AgentRuntimeError::StateUnavailable)?;
            let agent_session_id = AgentSessionId::new(execution.session_id.clone())
                .map_err(|_| AgentRuntimeError::InvalidSession)?;
            let question_handler: Arc<dyn UserQuestionHandler> =
                if elicitation_target == execution.session_id {
                    Arc::new(
                        self.elicitations
                            .handler_native(agent_session_id, connection_id),
                    )
                } else {
                    Arc::new(self.elicitations.handler_native_projected(
                        agent_session_id,
                        Some(elicitation_target.clone()),
                        connection_id,
                    ))
                };
            tools
                .register(Arc::new(AskUserTool::new(question_handler)))
                .map_err(runtime_operation_failed)?;
        }
        let tool_context = RuntimeToolContext {
            agent_type: agent_type.to_owned(),
            lifecycle_start_state: execution.lifecycle_start_state.clone(),
            session_id: execution.session_id.clone(),
            project_root: project_root.clone(),
            plan_guard,
        };
        let extension = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned();
        self.freeze_extension_reference_catalog(execution, extension.as_ref())?;
        let catalog = extension
            .as_ref()
            .map(|candidate| {
                candidate.contributor.prompt_catalog(
                    capabilities.can_spawn_agent
                        && profile
                            .tool_snapshot
                            .iter()
                            .any(|name| name == "spawn_agent"),
                    profile.tool_snapshot.iter().any(|name| name == "Skill"),
                )
            })
            .unwrap_or_default();
        let hooks = if let Some(candidate) = extension.as_ref() {
            candidate
                .contributor
                .prepare_lsp_runtime(&tool_context)
                .map_err(runtime_operation_failed)?;
            candidate
                .contributor
                .register_tools(&mut tools, &tool_context)
                .map_err(runtime_operation_failed)?;
            candidate
                .contributor
                .build_hook_runtime(&tool_context)
                .map_err(runtime_operation_failed)?
        } else {
            HookRuntime::empty()
        };
        if let Some(deferred_catalog) = extension
            .as_ref()
            .and_then(|candidate| candidate.contributor.mcp_tool_catalog())
        {
            register_deferred_tools(&mut tools, deferred_catalog)
                .map_err(runtime_operation_failed)?;
        }
        let context_source: Arc<dyn SpawnAgentContextSource> =
            Arc::new(RuntimeSpawnAgentContextSource {
                session: execution.session.clone(),
            });
        if let Some(candidate) = extension {
            register_collaboration_tools_with_template_resolver(
                &mut tools,
                coordinator,
                profile.clone(),
                capabilities,
                context_source,
                Arc::new(RuntimeSpawnAgentTemplateResolver {
                    contributor: Arc::clone(&candidate.contributor),
                }),
            )
            .map_err(runtime_operation_failed)?;
        } else {
            register_collaboration_tools(
                &mut tools,
                coordinator,
                profile.clone(),
                capabilities,
                context_source,
            )
            .map_err(runtime_operation_failed)?;
        }
        Ok((tools, hooks, catalog))
    }

    /// 小上下文只构造四个核心工具，不发现或初始化扩展、MCP、Skills 与协作工具。
    fn assemble_small_context_tools(
        &self,
        execution: &RuntimeAgentExecution,
        profile: &AgentProfile,
    ) -> Result<(ToolRegistry, HookRuntime, String), AgentRuntimeError> {
        let workflow_actor = workflow_actor_environment_required(execution, &profile.cwd)?;
        let output_directory = self
            .session_storage_directory(&execution.session_id)?
            .join("tool-output");
        crate::shell_env::wait_for_capture_applied(Duration::from_secs(4));
        let environment = Arc::new(
            ToolEnvironment::new(&profile.cwd)
                .and_then(|environment| environment.with_artifact_directory(output_directory))
                .map(|environment| {
                    environment.with_file_mutation_recorder(Arc::new(
                        file_changes::RuntimeFileMutationRecorder::new(execution.session.clone()),
                    ))
                })
                .map(|environment| {
                    if workflow_actor {
                        environment.with_workspace_guard()
                    } else {
                        environment
                    }
                })
                .map_err(runtime_operation_failed)?,
        );
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(ReadTool::new(Arc::clone(&environment))))
            .and_then(|_| tools.register(Arc::new(EditTool::new(Arc::clone(&environment)))))
            .and_then(|_| tools.register(Arc::new(WriteTool::new(Arc::clone(&environment)))))
            .and_then(|_| tools.register(Arc::new(BashTool::new(environment))))
            .map_err(runtime_operation_failed)?;
        Ok((tools, HookRuntime::empty(), String::new()))
    }

    /// 只在前一根 Turn 的终态收敛后继续同一 Session 的活跃 Goal。
    pub(crate) async fn continue_active_goal(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        if snapshot.state.plan.enabled
            || snapshot.state.turns.values().any(|turn| {
                turn.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                    && turn.status == TurnStatus::Running
            })
        {
            return Ok(());
        }
        let resolved = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
        let collaboration = self.ensure_collaboration_runtime(
            &session,
            RootAgentSeed {
                model: resolved.model().to_owned(),
                reasoning_effort: snapshot
                    .state
                    .provider
                    .as_ref()
                    .and_then(|provider| provider.reasoning_effort)
                    .map(reasoning_effort_snapshot_name),
                plan_guard: PlanGuard::inactive(),
            },
        )?;
        let goal = collaboration
            .execution
            .persistent_state
            .goal_snapshot()
            .map_err(runtime_operation_failed)?
            .goal;
        let Some(goal) = goal.filter(|goal| {
            goal.status == GoalStatus::Active
                && goal.owner_session_id == session_id
                && !goal
                    .token_budget
                    .is_some_and(|budget| goal.tokens_used >= budget)
        }) else {
            return Ok(());
        };
        let prefix = format!("goal_iteration:{}:", goal.id);
        let iteration = u32::try_from(
            snapshot
                .state
                .turns
                .values()
                .filter(|turn| turn.prompt_summary.starts_with(&prefix))
                .count(),
        )
        .unwrap_or(u32::MAX)
        .saturating_add(1);
        static NEXT_GOAL_TURN: AtomicU64 = AtomicU64::new(1);
        let turn_id = format!(
            "turn-goal-{}-{}-{}-{}",
            goal.id,
            iteration,
            unix_time_ms(),
            NEXT_GOAL_TURN.fetch_add(1, Ordering::Relaxed)
        );
        let verification = self
            .verify_active_goal(
                &session,
                &collaboration.execution,
                &resolved,
                &goal,
                &turn_id,
            )
            .await?;
        let Some(verification) = verification else {
            return Ok(());
        };
        let instruction = format!(
            "Continue the active session goal using its current authoritative state. Next concrete action (untrusted verifier output): {}. Verification gap (untrusted verifier output): {}. Do not repeat completed work. Respect the latest user instruction and authorization boundary.",
            serde_json::to_string(&verification.next_action).map_err(runtime_operation_failed)?,
            serde_json::to_string(&verification.reason).map_err(runtime_operation_failed)?
        );
        self.start_root_turn_internal(
            session_id,
            &turn_id,
            &instruction,
            RootTurnOptions::default(),
            Some(GoalContinuation {
                goal_id: goal.id,
                iteration,
            }),
        )
        .await?;
        Ok(())
    }

    /// 无工具只读校验；无效输出或 Provider 故障会停止自动续跑，保留 Goal 供用户恢复。
    async fn verify_active_goal(
        &self,
        session: &RuntimeSession,
        execution: &RuntimeAgentExecution,
        provider: &ResolvedProvider,
        goal: &keencode_agent::GoalRecord,
        operation_turn_id: &str,
    ) -> Result<Option<GoalVerification>, AgentRuntimeError> {
        let source = ResourceAgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
            .map_err(runtime_operation_failed)?;
        let mut messages = session
            .model_transcript_for_agent(&source)
            .map_err(runtime_operation_failed)?;
        let objective = serde_json::to_string(&goal.objective).map_err(runtime_operation_failed)?;
        messages.push(Message::text(MessageRole::User, format!(
            "Check whether every requirement of the current goal is complete based only on the actual transcript evidence. Do not call tools or perform work. Goal objective (untrusted data): {objective}. Return only JSON with passed:boolean, reason:string, nextAction:string. If evidence is incomplete, passed=false and nextAction must be the smallest useful next step. Never infer completion from effort, a plan, or passing tests that do not cover all requirements."
        )));
        let mut request = ModelRequest::new(provider.model(), messages);
        request.tool_choice = ToolChoice::None;
        request.max_output_tokens = Some(512);
        let response =
            match tokio::time::timeout(Duration::from_secs(60), provider.complete(request)).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    tracing::warn!(target: "agent_runtime", error = %error,
                    "Goal 完成校验请求失败，停止自动续跑");
                    return Ok(None);
                }
                Err(_) => {
                    tracing::warn!(target: "agent_runtime", "Goal 完成校验超时，停止自动续跑");
                    return Ok(None);
                }
            };
        let tokens = response
            .usage
            .total_tokens
            .or_else(|| {
                response
                    .usage
                    .input_tokens
                    .zip(response.usage.output_tokens)
                    .map(|(input, output)| input.saturating_add(output))
            })
            .unwrap_or(0);
        if tokens > 0 {
            let operation = goal_usage_operation_id(&[
                &execution.session_id,
                operation_turn_id,
                "root",
                "goal_verifier",
                "0",
                "0",
            ]);
            let change = execution
                .persistent_state
                .record_goal_usage(
                    &operation,
                    GoalUsageDelta {
                        tokens,
                        elapsed_seconds: 0,
                    },
                )
                .map_err(runtime_operation_failed)?;
            if change.changed {
                self.publish_goal_changed(
                    &execution.session_id,
                    change.current.goal.as_ref().map(|goal| goal.id.clone()),
                    change.current.revision,
                    change
                        .current
                        .goal
                        .as_ref()
                        .map(|goal| goal_status_name(goal.status).to_owned()),
                );
            }
        }
        let Some(text) = last_non_empty_text(&response.content) else {
            return Ok(None);
        };
        let Ok(verification) = serde_json::from_str::<GoalVerification>(text) else {
            tracing::warn!(target: "agent_runtime", "Goal 完成校验输出不符合 JSON 合同，停止自动续跑");
            return Ok(None);
        };
        if verification.reason.trim().is_empty()
            || (!verification.passed && verification.next_action.trim().is_empty())
        {
            return Ok(None);
        }
        let current = execution
            .persistent_state
            .goal_snapshot()
            .map_err(runtime_operation_failed)?;
        if current.goal.as_ref().is_none_or(|current| {
            current.id != goal.id
                || current.status != GoalStatus::Active
                || current.objective != goal.objective
                || current
                    .token_budget
                    .is_some_and(|budget| current.tokens_used >= budget)
        }) {
            return Ok(None);
        }
        if verification.passed {
            let evidence = verification
                .reason
                .chars()
                .take(keencode_agent::MAX_GOAL_EVIDENCE_CHARS)
                .collect::<String>();
            let operation_id = format!("goal-verifier-complete:{operation_turn_id}");
            if let Some(change) = execution
                .persistent_state
                .complete_goal_if_revision(
                    &operation_id,
                    current.revision,
                    &goal.id,
                    &goal.objective,
                    evidence,
                )
                .map_err(runtime_operation_failed)?
            {
                self.publish_goal_changed(
                    &execution.session_id,
                    change.current.goal.as_ref().map(|goal| goal.id.clone()),
                    change.current.revision,
                    Some("completed".to_owned()),
                );
            }
            return Ok(None);
        }
        Ok(Some(verification))
    }

    /// 启动根 Turn，并等待权威 TurnStarted 已登记后才向命令层返回 Accepted。
    pub async fn start_root_turn(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
        text: &str,
        options: RootTurnOptions,
    ) -> Result<RootTurnStartOutcome, AgentRuntimeError> {
        let outcome = self
            .start_root_turn_internal(session_id, turn_id, text, options, None)
            .await?;
        if outcome == RootTurnStartOutcome::Started {
            self.schedule_automatic_title(session_id);
        }
        Ok(outcome)
    }

    /// 在根 Turn 起点屏障内取得已登记的 Session。
    ///
    /// 桌面连接重建或草稿清理可能先释放 Manager 中的句柄，但只要权威 Journal
    /// 仍然存在，发送仍应恢复同一个 Session；这里按 Journal 元数据重新登记，不能
    /// 将一次可恢复的冷恢复窗口暴露成 `SessionNotRegistered`。调用方必须已经持有
    /// `turn_start_gates`，因此 close_session 不会在恢复后、真正开始 Turn 前插入。
    fn registered_session_for_root_turn(
        &self,
        session_id: &str,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        match self.runtime_manager.get(session_id.to_owned()) {
            Ok(session) => {
                if session.is_open().map_err(runtime_operation_failed)? {
                    Ok(session)
                } else {
                    // Manager 中可能暂存已完成 close_runtime 的句柄；先释放旧 lease，
                    // 再按 Journal 重新登记，避免把冷恢复窗口当作可运行 Session。
                    drop(session);
                    let metadata = self
                        .runtime_manager
                        .stored_session_metadata(session_id)
                        .map_err(runtime_operation_failed)?;
                    self.open_or_create_session(
                        Path::new(&metadata.project_root),
                        Some(session_id),
                        "root-turn-open",
                    )
                }
            }
            Err(RuntimeError::SessionNotRegistered) => {
                let metadata = self
                    .runtime_manager
                    .stored_session_metadata(session_id)
                    .map_err(runtime_operation_failed)?;
                self.open_or_create_session(
                    Path::new(&metadata.project_root),
                    Some(session_id),
                    "root-turn-open",
                )
            }
            Err(error) => Err(runtime_operation_failed(error)),
        }
    }

    /// 复用根 Turn 的起点屏障，内部续跑输入不产生伪造的用户气泡。
    async fn start_root_turn_internal(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
        text: &str,
        options: RootTurnOptions,
        continuation: Option<GoalContinuation>,
    ) -> Result<RootTurnStartOutcome, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        if text.trim().is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        self.admit_session_start(session_id)?;
        let _start_admission = SessionStartAdmissionGuard {
            runtime: Arc::clone(self),
            session_id: session_id.to_owned(),
        };
        let gate = self.session_turn_start_gate(session_id)?;
        let _gate = gate.lock().await;
        let session = self.registered_session_for_root_turn(session_id)?;
        let normalized_developer_context = options
            .developer_context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty());
        let summary = continuation.as_ref().map_or_else(
            || root_turn_summary(text, normalized_developer_context, options.plan_enabled),
            |continuation| {
                format!(
                    "goal_iteration:{}:{}",
                    continuation.goal_id, continuation.iteration
                )
            },
        );
        let summary = append_input_reference_digest(summary, &options.references)?;
        let summary = append_attachment_digest(
            summary,
            &options.attachment_images,
            &options.attachment_context,
        )?;
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        if let Some(continuation) = continuation.as_ref() {
            let state = self.ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: self
                        .resolve_session_provider(snapshot.state.provider.as_ref())?
                        .model()
                        .to_owned(),
                    reasoning_effort: snapshot
                        .state
                        .provider
                        .as_ref()
                        .and_then(|provider| provider.reasoning_effort)
                        .map(reasoning_effort_snapshot_name),
                    plan_guard: PlanGuard::inactive(),
                },
            )?;
            let goal = state
                .execution
                .persistent_state
                .goal_snapshot()
                .map_err(runtime_operation_failed)?;
            if options.plan_enabled
                || goal.goal.as_ref().is_none_or(|goal| {
                    goal.id != continuation.goal_id
                        || goal.status != GoalStatus::Active
                        || goal.owner_session_id != session_id
                        || goal
                            .token_budget
                            .is_some_and(|budget| goal.tokens_used >= budget)
                })
            {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
        }
        if let Some(existing) = snapshot
            .state
            .turns
            .iter()
            .find(|(known_turn_id, _)| known_turn_id.as_str() == turn_id)
            .map(|(_, turn)| turn)
        {
            return if existing.prompt_summary == summary {
                Ok(RootTurnStartOutcome::Deduplicated)
            } else {
                Err(AgentRuntimeError::RuntimeOperationFailed)
            };
        }
        let resolved = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
        if !options.attachment_images.is_empty()
            && !resolved.capabilities(resolved.model()).image_input
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let reasoning_effort = snapshot
            .state
            .provider
            .as_ref()
            .and_then(|provider| provider.reasoning_effort);
        let mut input_messages = Vec::new();
        let mut request_context = Vec::new();
        let memory_service = self
            .memory_service
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .clone();
        if let Some(memory_service) = memory_service
            && let Some(context) = memory_service
                .prompt_context(self.local_memories_enabled.load(Ordering::Acquire))
                .map_err(runtime_operation_failed)?
        {
            // 本地记忆只属于当前请求；不能写入权威 Transcript，也不能改变
            // Session 的用户输入摘要或重放事实。
            let mut message = Message::text(MessageRole::User, context);
            message.is_meta = true;
            request_context.push(message);
        }
        if let Some(context) = normalized_developer_context {
            // Memory/Plan/Ultra 保留原有用户级权限，放在真实对话历史之前；
            // is_meta 用户消息不进入权威 Transcript，与既有 request-only 语义一致。
            let mut message = Message::text(MessageRole::User, context);
            message.is_meta = true;
            request_context.push(message);
        }
        for context in &options.attachment_context {
            // 文本附件内容已在宿主边界完成读取；作为 meta 输入参与模型请求，避免把
            // 可能很大的文件正文重复写入用户 Transcript，同时保留附件引用事实。
            let mut message = Message::text(MessageRole::User, context.clone());
            message.is_meta = true;
            request_context.push(message);
        }
        let mut content = Vec::with_capacity(1 + options.attachment_images.len());
        content.push(ContentBlock::text(text));
        content.extend(
            options
                .attachment_images
                .iter()
                .cloned()
                .map(|image| ContentBlock::Image { image }),
        );
        let mut input = Message::new(MessageRole::User, content);
        input.references = options.references.clone();
        input
            .validate()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        input.is_meta = continuation.is_some();
        input_messages.push(input);
        let plan = if options.plan_enabled {
            PlanGuard::read_only()
        } else {
            PlanGuard::inactive()
        };
        if let Some(connection_id) = options.elicitation_connection_id.as_ref() {
            self.elicitations
                .bind_native_session(session_id, connection_id)
                .map_err(|error| {
                    // 原生 typed 连接由 bind_native_session 登记；问答不再建立独立
                    // 的 ACP initialize 或 Client Request 投递路径。
                    tracing::error!(
                        target: "keencode_diagnostics",
                        stage = "start_root_turn.bind_elicitation_session_connection",
                        error = ?error,
                        "首发前问答连接绑定失败"
                    );
                    AgentRuntimeError::RuntimeOperationFailed
                })?;
            self.bind_permission_session_connection(session_id, connection_id)?;
        }
        let journal_turn_present = snapshot
            .state
            .turns
            .keys()
            .any(|known_turn_id| known_turn_id.as_str() == turn_id);
        let collaboration = self.ensure_collaboration_runtime(
            &session,
            RootAgentSeed {
                model: resolved.model().to_owned(),
                reasoning_effort: reasoning_effort.map(reasoning_effort_snapshot_name),
                plan_guard: plan,
            },
        )?;
        reconcile_live_dynamic_input_acknowledgements(&session, &collaboration.coordinator)?;
        let mut root_profile = AgentProfile {
            model: resolved.model().to_owned(),
            reasoning_effort: reasoning_effort.map(reasoning_effort_snapshot_name),
            plan_guard: plan,
            cwd: collaboration.execution.project_root.clone(),
            worktree_lease: None,
            tool_snapshot: Vec::new(),
        };
        let (registry, _, _) = self.assemble_agent_tools(
            &collaboration.execution,
            Arc::clone(&collaboration.coordinator),
            &root_profile,
            "general-purpose",
            plan,
            AgentCapabilities {
                can_spawn_agent: true,
            },
        )?;
        root_profile.tool_snapshot = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        collaboration
            .coordinator
            .update_root_profile(&collaboration.root_agent_id, root_profile)
            .map_err(runtime_operation_failed)?;
        let agent_turn_id =
            AgentTurnId::new(turn_id.to_owned()).map_err(|_| AgentRuntimeError::InvalidSession)?;
        let completed_receiver = collaboration.execution.prepare_root_turn(
            agent_turn_id.clone(),
            resolved,
            reasoning_effort,
            input_messages,
            request_context,
            summary,
        )?;
        let mut barrier_subscription = session.subscribe().map_err(runtime_operation_failed)?;
        if snapshot.state.plan.enabled != options.plan_enabled
            && let Err(error) = session.set_plan(
                &control_operation_id("plan", session_id, turn_id),
                keencode_resources::PlanState {
                    enabled: options.plan_enabled,
                    // 切换只读模式不等于清除计划；Plan 工具的 clear 动作负责移除
                    // 正文与 Artifact，模式事件必须保留当前最终计划引用。
                    plan_artifact: snapshot.state.plan.plan_artifact.clone(),
                },
            )
        {
            collaboration
                .execution
                .discard_prepared_root_turn(&agent_turn_id);
            return Err(runtime_operation_failed(error));
        }
        let begin_result = if journal_turn_present {
            collaboration.coordinator.begin_root_turn_with_id(
                &collaboration.root_agent_id,
                agent_turn_id.clone(),
                text,
                plan,
            )
        } else {
            collaboration.coordinator.retry_unstarted_root_turn_with_id(
                &collaboration.root_agent_id,
                agent_turn_id.clone(),
                text,
                plan,
            )
        };
        if let Err(error) = begin_result {
            collaboration
                .execution
                .discard_prepared_root_turn(&agent_turn_id);
            return Err(runtime_operation_failed(error));
        }
        let wait_for_started = wait_for_turn_started(&mut barrier_subscription, turn_id);
        tokio::pin!(wait_for_started);
        tokio::select! {
            biased;
            started = &mut wait_for_started => started?,
            completed = completed_receiver => {
                match completed {
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(runtime_operation_failed(error)),
                    Ok(Ok(())) => {
                        tokio::time::timeout(Duration::from_secs(1), &mut wait_for_started)
                            .await
                            .map_err(runtime_operation_failed)??;
                    }
                }
            }
        }
        Ok(RootTurnStartOutcome::Started)
    }

    /// 原子保存 Session 实际解析出的 Provider、模型、协议和无凭据配置摘要。
    pub fn set_session_model(
        &self,
        session_id: &str,
        operation_id: &str,
        provider_id: &str,
        model: &str,
    ) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        const OPERATION_DOMAIN: &str = "keencode/session/model";

        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;

        // 先对账同一模型操作的真实 Journal 收据；模型重试只核对模型目标，
        // 不把后来由 effort 操作更新的 Provider 其他字段视为正文冲突。
        if let Some(record) = session
            .committed_control_event_in_domain(OPERATION_DOMAIN, operation_id)
            .map_err(runtime_operation_failed)?
        {
            let same_target = matches!(
                &record.event,
                SessionEvent::ProviderSnapshotUpdated { provider }
                    if provider.provider_id == provider_id && provider.model == model
            );
            if same_target {
                return session.snapshot().map_err(runtime_operation_failed);
            }
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }

        let provider = self
            .provider_registry
            .resolve(provider_id, model)
            .map_err(|_| AgentRuntimeError::ProviderNotConfigured)?;
        let protocol = match provider.protocol() {
            ProviderProtocol::Messages => ProviderProtocolSnapshot::AnthropicMessages,
            ProviderProtocol::ChatCompletions => ProviderProtocolSnapshot::OpenAiChatCompletions,
            ProviderProtocol::Responses => ProviderProtocolSnapshot::OpenAiResponses,
        };
        let reasoning_effort = session
            .snapshot()
            .map_err(runtime_operation_failed)?
            .state
            .provider
            .and_then(|snapshot| snapshot.reasoning_effort);
        session
            .set_provider_snapshot_in_domain(
                OPERATION_DOMAIN,
                operation_id,
                ProviderSnapshot {
                    provider_id: provider.provider_id().to_owned(),
                    model: provider.model().to_owned(),
                    context_window: provider.capabilities(provider.model()).max_context_tokens,
                    protocol,
                    config_fingerprint: provider.config_identity().to_owned(),
                    reasoning_effort,
                },
            )
            .map_err(runtime_operation_failed)?;
        session.snapshot().map_err(runtime_operation_failed)
    }

    /// 原子修改 Session 推理强度；尚未绑定 Provider 时同时冻结当前默认 Provider。
    pub fn set_session_effort(
        &self,
        session_id: &str,
        operation_id: &str,
        effort: &str,
    ) -> Result<(), AgentRuntimeError> {
        const OPERATION_DOMAIN: &str = "keencode/session/effort";

        validate_session_id(session_id)?;
        let reasoning_effort = parse_reasoning_effort(effort)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;

        let requested_reasoning_effort = reasoning_effort.map(reasoning_effort_snapshot);
        // 先对账同一 effort 操作的真实 Journal 收据；只核对 effort 目标，
        // 保留后来模型切换已更新的 Provider、模型和其他快照字段。
        if let Some(record) = session
            .committed_control_event_in_domain(OPERATION_DOMAIN, operation_id)
            .map_err(runtime_operation_failed)?
        {
            let same_target = matches!(
                &record.event,
                SessionEvent::ProviderSnapshotUpdated { provider }
                    if provider.reasoning_effort == requested_reasoning_effort
            );
            if same_target {
                return Ok(());
            }
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }

        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let mut provider = match snapshot.state.provider {
            Some(provider) => provider_snapshot(&self.resolve_session_provider(Some(&provider))?),
            None => provider_snapshot(&self.resolve_default_provider()?),
        };
        provider.reasoning_effort = requested_reasoning_effort;
        session
            .set_provider_snapshot_in_domain(OPERATION_DOMAIN, operation_id, provider)
            .map_err(runtime_operation_failed)?;
        Ok(())
    }

    /// 关闭 Session 级标题请求并清理焦点；这些状态属于 Runtime 生命周期。
    async fn close_session_runtime_state(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        let title_generation = self
            .title_generation_gates
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .remove(session_id);
        if let Some(title_generation) = title_generation {
            title_generation.cancellation.cancel();
            // 等待模型 future 退出及 Session 句柄释放，不能靠调度重试等待网络超时。
            let _title_guard = title_generation.gate.lock().await;
        }
        if self
            .focused_session_id()?
            .as_deref()
            .is_some_and(|focused| focused == session_id)
        {
            self.clear_focus();
        }
        Ok(())
    }

    /// 按 Collaboration、后台资源和 Runtime Session 的所有权顺序关闭一个 Session。
    pub async fn close_session(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        let gate = self.session_turn_start_gate(session_id)?;
        let _gate = gate.lock().await;
        self.close_session_locked(session_id, true).await
    }

    /// 在不取消任何活动工作的前提下释放空闲 Session 的进程内资源。
    ///
    /// 释放与 root Turn 共用同一 admission 和起点屏障：先冻结新的发送，再确认
    /// Runtime、后续输入队列和权限等待均为空，最后复用统一清理路径关闭 MCP、
    /// Agent Runner、实时订阅和 Session lease。返回 `false` 表示 Session 仍在
    /// 使用中，调用方应保留当前 UI 观测；此路径绝不替活动树发出取消。
    pub(crate) async fn release_idle_session(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<bool, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let gate = self.session_turn_start_gate(session_id)?;
        let _gate = gate.lock().await;

        // 认领 Closing 与 start_root_turn 使用同一 lifecycle/admission 锁顺序；
        // 已登记或随后到达的 root start 都不能越过本次 idle 判定。
        if !self.claim_session_mutation_close(session_id)? {
            return Ok(false);
        }

        let idle = (|| -> Result<bool, AgentRuntimeError> {
            let session = self
                .runtime_manager
                .get(session_id.to_owned())
                .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
            if self.session_has_active_work(session_id)? {
                return Ok(false);
            }
            let (_, input_queue) = session
                .input_queue_state()
                .map_err(runtime_operation_failed)?;
            if !input_queue.items.is_empty() {
                return Ok(false);
            }
            if self.permissions.pending_count_for_session(session_id) != 0 {
                return Ok(false);
            }
            if self
                .elicitations
                .pending_request_id_for_session(session_id)
                .is_some()
            {
                return Ok(false);
            }
            Ok(true)
        })();

        match idle {
            Ok(true) => match self.close_session_locked(session_id, true).await {
                Ok(()) => Ok(true),
                Err(error) => {
                    self.clear_deferred_session_lifecycle(session_id);
                    Err(error)
                }
            },
            Ok(false) => {
                self.clear_deferred_session_lifecycle(session_id);
                Ok(false)
            }
            Err(error) => {
                self.clear_deferred_session_lifecycle(session_id);
                Err(error)
            }
        }
    }

    /// Native Host 的明确入口；空闲回收与普通 Session 释放共用同一 admission 边界。
    pub(crate) async fn release_idle_native_session(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<bool, AgentRuntimeError> {
        self.release_idle_session(session_id).await
    }

    /// 调用方必须持有对应 Session 的 Turn gate；这是显式 mutation admission 和
    /// 普通 close 共用的唯一关闭实现，避免重复取得 gate 或提前取消权限状态。
    async fn close_session_locked(
        &self,
        session_id: &str,
        release_lifecycle: bool,
    ) -> Result<(), AgentRuntimeError> {
        self.elicitations.close_session(session_id);
        self.permissions.close_session(session_id);
        let mut close_error = self.close_session_runtime_state(session_id).await.err();
        let collaboration = {
            let mut runtimes = self
                .collaboration_sessions
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            let collaboration = runtimes.get(session_id).cloned();
            if let Some(collaboration) = collaboration.as_ref() {
                // 根关闭与移除和设置热更新共用这把 map 锁：setter 只能完整看到
                // 关闭前的根，或在线性化移除后完全忽略它。
                if let Err(error) = collaboration
                    .coordinator
                    .close_root_session(&collaboration.root_agent_id)
                    && !matches!(
                        error,
                        keencode_agent::CollaborationError::AgentNotFound { .. }
                    )
                {
                    // 协调器已冻结或终态尚未收敛时不能伪造关闭成功；但仍须继续
                    // 拆除本地后台资源，保留磁盘事实交给下一次冷恢复处理。
                    close_error = Some(AgentRuntimeError::RecoveryRequired);
                }
                if let Err(error) = collaboration.stop_background_completion_pump() {
                    close_error.get_or_insert(error);
                }
                if let Err(error) = collaboration.execution.stop_local_work_for_close() {
                    close_error.get_or_insert(error);
                }
                if runtimes
                    .get(session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, collaboration))
                {
                    runtimes.remove(session_id);
                }
            }
            collaboration
        };
        drop(collaboration);
        let runtime_closed = match self.runtime_manager.close(session_id.to_owned()) {
            Ok(()) | Err(RuntimeError::SessionNotRegistered) => true,
            Err(_) => {
                close_error.get_or_insert(AgentRuntimeError::RuntimeOperationFailed);
                false
            }
        };
        if runtime_closed {
            self.session_extension_reference_catalogs
                .write()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .remove(session_id);
            // Workspace mutation 的 guard 必须在 Git、资源事务和 Session 恢复
            // 完成后才释放 Closing；普通 close 没有跨事务 guard，仍在这里收口。
            if release_lifecycle {
                self.clear_deferred_session_lifecycle(session_id);
            }
        }
        match close_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 应用退出时暂停一个 Session，保留 Collaboration 身份和待消费事实，不清理 Worktree。
    async fn shutdown_session(self: &Arc<Self>, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.elicitations.close_session(session_id);
        let gate = {
            let mut gates = self
                .turn_start_gates
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            Arc::clone(
                gates
                    .entry(session_id.to_owned())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _gate = gate.lock().await;
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        let mut shutdown_error = None;
        if let Some(collaboration) = collaboration {
            // 先切断执行端和后台 Shell 的新副作用，再由 Coordinator 持久化所有未决
            // Turn 的 Interrupted 终态；两边都失败时保持旧状态不可继续使用。
            if let Err(error) = collaboration.execution.begin_shutdown() {
                shutdown_error = Some(error);
            }
            if collaboration
                .coordinator
                .suspend_root_session(&collaboration.root_agent_id)
                .is_err()
            {
                shutdown_error.get_or_insert(AgentRuntimeError::RecoveryRequired);
            }
            // Condvar 等待和后台进程回收都必须离开异步 worker；被取消的 Runner
            // 仍需被调度才能释放执行槽，不能在当前 poll 中同步等待其回传。
            let execution = Arc::clone(&collaboration.execution);
            let quiesced = tokio::task::spawn_blocking(move || execution.finish_shutdown())
                .await
                .unwrap_or(Err(AgentRuntimeError::RuntimeOperationFailed));
            if let Err(error) = quiesced {
                shutdown_error.get_or_insert(error);
            }
            if let Err(error) = collaboration.stop_background_completion_pump() {
                shutdown_error.get_or_insert(error);
            }
            match self.collaboration_sessions.lock() {
                Ok(mut runtimes) => {
                    if runtimes
                        .get(session_id)
                        .is_some_and(|current| Arc::ptr_eq(current, &collaboration))
                    {
                        runtimes.remove(session_id);
                    }
                }
                Err(_) => {
                    shutdown_error.get_or_insert(AgentRuntimeError::StateUnavailable);
                }
            }
            drop(collaboration);
        }
        if let Err(error) = self.close_session_runtime_state(session_id).await {
            shutdown_error.get_or_insert(error);
        }
        let runtime_closed = match self.runtime_manager.close(session_id.to_owned()) {
            Ok(()) | Err(RuntimeError::SessionNotRegistered) => true,
            Err(_) => {
                shutdown_error.get_or_insert(AgentRuntimeError::RuntimeOperationFailed);
                false
            }
        };
        if runtime_closed {
            self.session_extension_reference_catalogs
                .write()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .remove(session_id);
        }
        match shutdown_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 通过 Collaboration 权威状态对当前 Session 的精确根 Turn 树发出幂等级联取消。
    pub fn cancel_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<TurnCancellationOutcome, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let requested_turn_id =
            AgentTurnId::new(turn_id.to_owned()).map_err(runtime_operation_failed)?;
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        let Some(collaboration) = collaboration else {
            let exists = session
                .snapshot()
                .map_err(runtime_operation_failed)?
                .state
                .turns
                .keys()
                .any(|known| known.as_str() == turn_id);
            return if exists {
                Ok(TurnCancellationOutcome::NotRunning)
            } else {
                Err(AgentRuntimeError::RuntimeOperationFailed)
            };
        };
        match collaboration
            .coordinator
            .cancel_turn(&collaboration.root_agent_id, &requested_turn_id)
        {
            Ok(TurnCancellationDisposition::Requested) => Ok(TurnCancellationOutcome::Requested),
            Ok(TurnCancellationDisposition::AlreadyRequested) => {
                Ok(TurnCancellationOutcome::AlreadyRequested)
            }
            Ok(TurnCancellationDisposition::NotRunning) => Ok(TurnCancellationOutcome::NotRunning),
            Err(keencode_agent::CollaborationError::TurnMismatch { .. }) => {
                let exists = session
                    .snapshot()
                    .map_err(runtime_operation_failed)?
                    .state
                    .turns
                    .keys()
                    .any(|known| known.as_str() == turn_id);
                if exists {
                    Ok(TurnCancellationOutcome::NotRunning)
                } else {
                    Err(AgentRuntimeError::RuntimeOperationFailed)
                }
            }
            Err(error) => Err(runtime_operation_failed(error)),
        }
    }

    /// 列出指定已装配 Session 中仍在运行的后台 Shell 与单层子 Agent。
    ///
    /// 列表只读取当前进程中的实时执行账本：根 Agent Turn 不属于后台任务，
    /// 已经提交终态的 Shell 或 Agent 也不会重新投影到桌面面板。
    pub fn background_tasks_list(
        &self,
        session_id: &str,
    ) -> Result<Vec<BackgroundTaskInfo>, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let runtimes = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        let mut tasks = Vec::new();
        for runtime in runtimes {
            let session_id = runtime.execution.session_id.clone();
            let current_agent_turns =
                current_child_agent_turns_for_root(&runtime.coordinator, &runtime.root_agent_id)?;
            for task in runtime
                .execution
                .background_tasks
                .list_running()
                .map_err(runtime_operation_failed)?
            {
                let started_at_unix_ms = task.started_at_unix_ms;
                tasks.push((
                    started_at_unix_ms,
                    BackgroundTaskInfo {
                        session_id: session_id.clone(),
                        task_id: task.task_id,
                        kind: BackgroundTaskKind::Shell,
                        child_thread_id: None,
                        summary: task.summary,
                        started_at: background_task_started_at(started_at_unix_ms)?,
                        duration_ms: task.duration_ms,
                        pid: task.pid,
                    },
                ));
            }
            let state = runtime
                .execution
                .state
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            for agent in current_agent_turns {
                let Some(turn_id) = current_collaboration_turn_id(&agent.status).cloned() else {
                    continue;
                };
                if let Some(turn) = state.running_turns.get(&turn_id).filter(|turn| {
                    turn.agent_id == agent.agent.agent_id && turn.agent_depth == AgentDepth::CHILD
                }) {
                    let started_at_unix_ms = turn.started_at_unix_ms;
                    tasks.push((
                        started_at_unix_ms,
                        BackgroundTaskInfo {
                            session_id: session_id.clone(),
                            task_id: turn_id.as_str().to_owned(),
                            kind: BackgroundTaskKind::Agent,
                            child_thread_id: Some(turn.agent_id.as_str().to_owned()),
                            summary: turn.summary.clone(),
                            started_at: background_task_started_at(started_at_unix_ms)?,
                            duration_ms: duration_milliseconds(turn.started.elapsed()),
                            pid: None,
                        },
                    ));
                    continue;
                }
                if !matches!(
                    agent.status,
                    CollaborationAgentStatus::WaitingCapacity { .. }
                ) {
                    continue;
                }
                // 后台任务列表被前端高频轮询，只投影排队 Agent 的开始时间，
                // 不为整页列表克隆完整权威状态。
                let started_at_unix_ms = runtime
                    .execution
                    .session
                    .read_state(|state| queued_agent_started_at(state, &agent).ok())
                    .map_err(runtime_operation_failed)?
                    .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
                tasks.push((
                    started_at_unix_ms,
                    BackgroundTaskInfo {
                        session_id: session_id.clone(),
                        task_id: turn_id.as_str().to_owned(),
                        kind: BackgroundTaskKind::Agent,
                        child_thread_id: Some(agent.agent.agent_id.as_str().to_owned()),
                        summary: child_agent_turn_summary(agent.current_turn_summary.as_deref()),
                        started_at: background_task_started_at(started_at_unix_ms)?,
                        duration_ms: unix_time_ms().saturating_sub(started_at_unix_ms),
                        pid: None,
                    },
                ));
            }
        }
        tasks.sort_by(|(left_started, left), (right_started, right)| {
            left_started
                .cmp(right_started)
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        Ok(tasks.into_iter().map(|(_, task)| task).collect::<Vec<_>>())
    }

    /// 由根 Session 授权恢复一个失败或中断的单层子 Agent，并返回新 Turn 标识。
    ///
    /// `child_thread_id` 始终按 Agent 身份解析；后台列表中的 `task_id` 是 Turn
    /// 身份，二者不能互换。恢复不伪造活跃根 Turn，Coordinator 会以目标旧 Turn
    /// 的因果链、输入和动态 claim 创建一个新的 Turn，并由 operationId 去重。
    pub fn resume_background_agent(
        self: &Arc<Self>,
        session_id: &str,
        operation_id: &str,
        child_thread_id: &str,
    ) -> Result<AgentTurnId, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let target_agent_id = RunnerAgentId::new(child_thread_id.to_owned())
            .map_err(|_| AgentRuntimeError::InvalidResumeTarget)?;
        let operation_id = ToolCallId::new(operation_id.to_owned())
            .map_err(|_| AgentRuntimeError::InvalidResumeTarget)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
        let existing_collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        let collaboration = if let Some(collaboration) = existing_collaboration {
            collaboration
        } else {
            // 已有持久 checkpoint 时恢复操作可以先按 checkpoint 中冻结的根 Profile
            // 装配；这样同一 operationId 的重放不会因为当前 Provider 暂不可用而
            // 丢失已经提交的新 Turn。全新 Session 仍必须要求当前 Provider。
            let persisted = SessionCollaborationStore::new(&self.storage_root, session_id)?
                .load_transition_snapshot()
                .map_err(runtime_operation_failed)?;
            let seed = if let Some(transition) = persisted.as_ref() {
                recovered_root_agent_seed(&transition.commit.checkpoint)
                    .ok_or(AgentRuntimeError::InvalidResumeTarget)?
            } else {
                let resolved = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
                let reasoning_effort = snapshot
                    .state
                    .provider
                    .as_ref()
                    .and_then(|provider| provider.reasoning_effort);
                RootAgentSeed {
                    model: resolved.model().to_owned(),
                    reasoning_effort: reasoning_effort.map(reasoning_effort_snapshot_name),
                    plan_guard: if snapshot.state.plan.enabled {
                        PlanGuard::read_only()
                    } else {
                        PlanGuard::inactive()
                    },
                }
            };
            self.ensure_collaboration_runtime(&session, seed)?
        };
        reconcile_live_dynamic_input_acknowledgements(&session, &collaboration.coordinator)?;
        collaboration
            .coordinator
            .resume_agent_for_root_with_operation(
                &collaboration.root_agent_id,
                &operation_id,
                &target_agent_id,
            )
            .map_err(map_resume_collaboration_error)
    }

    /// 精确取消一个后台 Shell 或单层子 Agent，并返回真实取消结果。
    pub fn background_task_cancel_outcome(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<BackgroundTaskCancellationOutcome, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let runtime = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
            .ok_or(AgentRuntimeError::SessionUnavailable)?;
        let before_snapshot = runtime
            .store
            .load_transition_snapshot()
            .map_err(runtime_operation_failed)?;
        let agent_target =
            current_child_agent_turns_for_root(&runtime.coordinator, &runtime.root_agent_id)?
                .into_iter()
                .find_map(|agent| {
                    let turn_id = current_collaboration_turn_id(&agent.status)?;
                    (turn_id.as_str() == task_id).then(|| {
                        (
                            agent.agent.agent_id,
                            turn_id.clone(),
                            matches!(
                                &agent.status,
                                CollaborationAgentStatus::WaitingCapacity {
                                    turn_id: waiting_turn_id
                                } if waiting_turn_id == turn_id
                            ),
                        )
                    })
                });
        if let Some((agent_id, turn_id, was_waiting)) = agent_target {
            if was_waiting {
                let snapshot = before_snapshot
                    .as_ref()
                    .ok_or(AgentRuntimeError::RecoveryRequired)?;
                let before_agent = recovered_agent_for_id(&snapshot.commit.checkpoint, &agent_id)
                    .ok_or(AgentRuntimeError::RecoveryRequired)?;
                if !matches!(
                    &before_agent.status,
                    CollaborationAgentStatus::WaitingCapacity { turn_id: waiting_turn_id }
                        if waiting_turn_id == &turn_id
                ) {
                    return Err(AgentRuntimeError::RecoveryRequired);
                }
            }
            let cancellation = runtime.coordinator.cancel_turn(&agent_id, &turn_id);
            if was_waiting && !reconcile_waiting_capacity_cancel(&runtime, &agent_id, &turn_id)? {
                return Err(AgentRuntimeError::RecoveryRequired);
            }
            return match cancellation {
                Ok(keencode_agent::TurnCancellationDisposition::Requested) => {
                    Ok(BackgroundTaskCancellationOutcome::Requested)
                }
                Ok(keencode_agent::TurnCancellationDisposition::AlreadyRequested) => {
                    Ok(BackgroundTaskCancellationOutcome::AlreadyRequested)
                }
                Ok(keencode_agent::TurnCancellationDisposition::NotRunning) => {
                    if was_waiting {
                        Ok(BackgroundTaskCancellationOutcome::Requested)
                    } else {
                        Ok(BackgroundTaskCancellationOutcome::NotRunning)
                    }
                }
                Err(
                    keencode_agent::CollaborationError::TargetNotRunning { .. }
                    | keencode_agent::CollaborationError::TurnMismatch { .. },
                ) => Ok(BackgroundTaskCancellationOutcome::NotRunning),
                Err(error) => Err(runtime_operation_failed(error)),
            };
        }
        if reconcile_waiting_capacity_cancel_by_turn(&runtime, task_id)? {
            // Store 中遗留的 pending 只证明取消信号此前已经发出；本次调用仅完成对账。
            return Ok(BackgroundTaskCancellationOutcome::AlreadyRequested);
        }
        runtime
            .execution
            .background_tasks
            .cancel(session_id, task_id)
            .map(|_| BackgroundTaskCancellationOutcome::Requested)
            .or_else(|error| match error.code.as_str() {
                "background_task_stop_already_requested" => {
                    Ok(BackgroundTaskCancellationOutcome::AlreadyRequested)
                }
                "background_task_not_running" | "background_task_not_found" => {
                    Ok(BackgroundTaskCancellationOutcome::NotRunning)
                }
                _ => Err(AgentRuntimeError::RuntimeOperationFailed),
            })
    }

    /// 以客户端稳定 operationId 向当前根 Turn 注入一次可恢复且跨重试去重的用户 steer。
    pub fn steer_root_turn(
        &self,
        session_id: &str,
        operation_id: &str,
        text: &str,
        references: Vec<keencode_model::InputReference>,
    ) -> Result<keencode_agent::UserSteer, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let operation_id =
            ToolCallId::new(operation_id.to_owned()).map_err(runtime_operation_failed)?;
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        let steer = collaboration
            .coordinator
            .steer_active_agent_with_operation(
                &collaboration.root_agent_id,
                &operation_id,
                text,
                references,
            )
            .map_err(runtime_operation_failed)?;
        Ok(steer)
    }

    /// 幂等按 Collaboration、后台资源与 Runtime 的所有权顺序关闭全部 Session。
    pub async fn shutdown(self: &Arc<Self>) -> Result<(), AgentRuntimeError> {
        let _shutdown_guard = self.shutdown_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return self
                .shutdown_error
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)
                .and_then(|error| error.map_or(Ok(()), Err));
        }
        // 先记录“尚未完成”的结果，再公开 Runtime 已关闭。这样即使调用方在任意
        // await 点取消本次 shutdown，后续重入也只能得到失败，而不能把未完成清理
        // 伪装成成功。只有下面完整流程成功后才清除此占位结果。
        {
            let mut shutdown_error = self
                .shutdown_error
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            *shutdown_error = Some(AgentRuntimeError::RuntimeOperationFailed);
        }
        self.closed.store(true, Ordering::Release);
        // 唤醒仅等待 Runtime 通知的设置桥接任务，使其在 Runtime 关闭后及时退出。
        self.focus_change_tx.send_replace(None);
        // 先收口权限等待，再等待各 Session 的 Runner 清理，避免 shutdown 被
        // 一个失去前端连接的审批请求阻塞。
        self.permissions.close();

        let result = async {
            let registered = self
                .runtime_manager
                .registered_session_ids()
                .map_err(runtime_operation_failed)?;
            let mut session_ids = registered
                .into_iter()
                .map(|session_id| session_id.as_str().to_owned())
                .collect::<HashSet<_>>();
            session_ids.extend(
                self.collaboration_sessions
                    .lock()
                    .map_err(|_| AgentRuntimeError::StateUnavailable)?
                    .keys()
                    .cloned(),
            );
            let mut session_ids = session_ids.into_iter().collect::<Vec<_>>();
            session_ids.sort();
            let mut first_error = None;
            for session_id in session_ids {
                if let Err(error) = self.shutdown_session(&session_id).await
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            self.elicitations.shutdown();
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                let mut shutdown_error = self
                    .shutdown_error
                    .lock()
                    .map_err(|_| AgentRuntimeError::StateUnavailable)?;
                *shutdown_error = None;
                Ok(())
            }
            Err(error) => {
                if let Ok(mut shutdown_error) = self.shutdown_error.lock() {
                    *shutdown_error = Some(error);
                }
                Err(error)
            }
        }
    }
}

/// 根 Turn 起点的 RAII admission；任何参数校验或模型失败都必须释放启动认领，
/// 否则 deferred cleanup 会被永久阻塞。
struct SessionStartAdmissionGuard {
    runtime: Arc<AgentRuntime>,
    session_id: String,
}

impl Drop for SessionStartAdmissionGuard {
    fn drop(&mut self) {
        self.runtime.release_session_start(&self.session_id);
    }
}

/// 从 Journal 顺序恢复每个已启动 Turn 使用的 Provider 快照。
///
/// Provider 快照是 Session 级当前状态，但模型消息只保留所属 Turn；按 Turn
/// 重建历史快照后，模型切换或传输配置热更新可以清除不兼容的 opaque reasoning
/// continuation，同时让相同 Provider 链路跨 Turn 保持完整请求前缀。
fn historical_provider_snapshots_by_turn(
    session: &RuntimeSession,
) -> Result<HashMap<String, ProviderSnapshot>, AgentRuntimeError> {
    Ok(session
        .history_index()
        .map_err(runtime_operation_failed)?
        .turn_providers
        .into_iter()
        .map(|(turn_id, provider)| (turn_id.as_str().to_owned(), provider))
        .collect())
}

/// 判断历史 Provider 是否仍可安全回放其中的 opaque reasoning 续传。
fn provider_supports_reasoning_continuation(
    historical: &ProviderSnapshot,
    current: &ResolvedProvider,
) -> bool {
    historical.provider_id == current.provider_id()
        && historical.model == current.model()
        && historical.protocol == provider_protocol_snapshot(current.protocol())
        && historical.config_fingerprint == current.config_identity()
}

/// 保留同一 Provider 链路的历史可读推理和协议续传状态，清除不兼容状态。
fn clear_historical_reasoning_state(
    messages: &mut Vec<Message>,
    turn_ids: &[Option<ResourceTurnId>],
    current_provider: &ResolvedProvider,
    historical_providers: &HashMap<String, ProviderSnapshot>,
) {
    assert_eq!(
        messages.len(),
        turn_ids.len(),
        "历史模型消息与 Turn 身份数量必须一致"
    );
    for (message, turn_id) in messages.iter_mut().zip(turn_ids) {
        let continuation_compatible = turn_id
            .as_ref()
            .and_then(|turn_id| historical_providers.get(turn_id.as_str()))
            .is_some_and(|historical| {
                provider_supports_reasoning_continuation(historical, current_provider)
            });
        message.content.retain_mut(|block| {
            if let ContentBlock::Reasoning { reasoning } = block {
                if !continuation_compatible {
                    reasoning.continuation = None;
                }
                return !reasoning.text.is_empty()
                    || reasoning.summary.is_some()
                    || reasoning.continuation.is_some();
            }
            true
        });
    }
    messages.retain(|message| !message.content.is_empty());
}

/// 将每次 Agent 与压缩请求的观测身份强制绑定到可信 Turn，覆盖调用方伪造字段。
struct TurnBoundProvider {
    /// 当前注册表代次解析出的不可变 Provider。
    inner: Arc<dyn ModelProvider>,
    /// 请求所属根 Session。
    session_id: String,
    /// 请求所属当前 Turn。
    turn_id: String,
    /// 发起请求的根 Agent 或单层子 Agent。
    agent_id: String,
    /// 端点在 allowlist 内时的会话稳定缓存路由键；其余端点为 `None` 不发送。
    prompt_cache_key: Option<String>,
    /// 会话冻结的稳定前缀（System 规则、能力说明、指令与目录）；不参与 Runtime Journal。
    stable_prefix: Arc<Vec<Message>>,
    /// 仅本轮动态上下文（环境与 Memory/Plan/Ultra），位于历史之前；不参与 Runtime Journal。
    request_context: Arc<Vec<Message>>,
}

impl TurnBoundProvider {
    /// 创建只允许 `purpose=agent` 的可信 Provider 包装器。
    fn new(inner: Arc<dyn ModelProvider>, session_id: &str, turn_id: &str, agent_id: &str) -> Self {
        Self {
            inner,
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            agent_id: agent_id.to_owned(),
            prompt_cache_key: None,
            stable_prefix: Arc::new(Vec::new()),
            request_context: Arc::new(Vec::new()),
        }
    }

    /// 设置会话稳定缓存路由键；仅端点在 allowlist 内时由装配方提供。
    fn with_prompt_cache_key(mut self, prompt_cache_key: Option<String>) -> Self {
        self.prompt_cache_key = prompt_cache_key;
        self
    }

    /// 设置会话冻结的稳定前缀；前缀跨 Turn 字节稳定，压缩与独立生成不启用。
    fn with_stable_prefix(mut self, prefix: Vec<Message>) -> Self {
        self.stable_prefix = Arc::new(prefix);
        self
    }

    /// 设置本轮历史之前的动态上下文；调用方输入和 Runtime Transcript 保持不变。
    fn with_request_context(mut self, request_context: Vec<Message>) -> Self {
        self.request_context = Arc::new(request_context);
        self
    }

    /// 发送和预算共用同一装配规则；预算只需构造新增消息，不复制完整历史。
    ///
    /// 环境与背景不能排在工具结果后面充当新的用户输入。
    /// 只复制有界前缀，历史分段仍共享；背景消息保持原有角色。
    fn inject_context(&self, request: &mut ModelRequest) {
        let prefix = if self.request_context.is_empty() {
            Arc::clone(&self.stable_prefix)
        } else {
            Arc::new(
                self.stable_prefix
                    .iter()
                    .chain(self.request_context.iter())
                    .cloned()
                    .collect(),
            )
        };
        request.set_request_message_context(prefix, Arc::new(Vec::new()));
    }
}

impl ContextTokenEstimator for TurnBoundProvider {
    /// 将未持久化的规则、环境和目录计入主请求；额外消息开销采用保守近似。
    fn estimate_request(&self, request: &ModelRequest) -> u64 {
        let overhead = JsonContextTokenEstimator
            .estimate_messages(self.stable_prefix.as_slice())
            .saturating_add(
                JsonContextTokenEstimator.estimate_messages(self.request_context.as_slice()),
            );
        JsonContextTokenEstimator
            .estimate_request(request)
            .saturating_add(overhead)
    }

    /// 压缩区间只估算历史消息，不能将请求期规则送去摘要或重复计入每个区间。
    fn estimate_messages(&self, messages: &[Message]) -> u64 {
        JsonContextTokenEstimator.estimate_messages(messages)
    }

    fn estimate_messages_from(&self, messages: &ModelMessages, start: usize) -> u64 {
        JsonContextTokenEstimator.estimate_messages_from(messages, start)
    }
}

impl ModelProvider for TurnBoundProvider {
    /// Provider 能力不改变，只覆盖请求观测身份。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.inner.capabilities(model)
    }

    /// 在唯一 Provider 边界覆盖四个保留键 + 条件性 prompt_cache_key，普通重试和压缩都不能绕过。
    fn stream(
        &self,
        mut request: ModelRequest,
    ) -> ModelFuture<'_, Result<ModelStream, keencode_model::ModelError>> {
        self.inject_context(&mut request);
        request.metadata.insert(
            REQUEST_METADATA_SESSION_ID.to_owned(),
            self.session_id.clone(),
        );
        request
            .metadata
            .insert(REQUEST_METADATA_TURN_ID.to_owned(), self.turn_id.clone());
        request
            .metadata
            .insert(REQUEST_METADATA_AGENT_ID.to_owned(), self.agent_id.clone());
        request
            .metadata
            .insert(REQUEST_METADATA_PURPOSE.to_owned(), "agent".to_owned());
        // 会话稳定缓存路由键只随 Session 漂移；压缩请求共用同一 Session 键。
        if let Some(prompt_cache_key) = self.prompt_cache_key.as_deref() {
            request.metadata.insert(
                REQUEST_METADATA_PROMPT_CACHE_KEY.to_owned(),
                prompt_cache_key.to_owned(),
            );
        }
        self.inner.stream(request)
    }
}

/// 已知接受 Chat Completions `prompt_cache_key` 的端点主机 allowlist。
///
/// OpenAI 官方支持 `prompt_cache_key` 作为缓存路由提示；DeepSeek 官方文档
/// 明确其上下文缓存为自动前缀匹配、chat completions 不支持该参数（会被
/// 忽略或报错），故不入 allowlist。第三方 OpenAI 兼容网关可能严格拒绝未知
/// 字段导致 400，因此默认只对以下官方主机写入，其余端点保持原线格式；
/// 新增条目须以实测确认为前提。
const PROMPT_CACHE_KEY_HOSTS: &[&str] = &["api.openai.com"];

/// 端点主机在 allowlist 内时返回会话稳定的缓存路由键，否则不发送该字段。
fn prompt_cache_key_for_endpoint(base_url: &Url, session_id: &str) -> Option<String> {
    let host = base_url.host_str()?;
    PROMPT_CACHE_KEY_HOSTS
        .contains(&host)
        .then(|| format!("keencode:{session_id}"))
}

/// 等待同一 Session 的权威 TurnStarted；Lag 或通道关闭都要求调用方恢复。
async fn wait_for_turn_started(
    subscription: &mut RuntimeEventSubscription,
    expected_turn_id: &str,
) -> Result<(), AgentRuntimeError> {
    let waited = async {
        loop {
            let delivery = subscription
                .recv()
                .await
                .map_err(runtime_operation_failed)?;
            let RuntimeEventPayload::Authoritative(record) = delivery.payload else {
                continue;
            };
            if session_event_contains_turn_started(&record.event, expected_turn_id) {
                return Ok(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waited)
        .await
        .map_err(runtime_operation_failed)?
}

/// 判断普通事件或原子批次是否包含目标 Turn 的权威起点。
fn session_event_contains_turn_started(event: &SessionEvent, expected_turn_id: &str) -> bool {
    match event {
        SessionEvent::TurnStarted { turn_id, .. } => turn_id.as_str() == expected_turn_id,
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .any(|event| session_event_contains_turn_started(event, expected_turn_id)),
        _ => false,
    }
}

/// 构造只绑定稳定客户端输入的 Turn 摘要，动态 Memory 变化不得破坏请求重试。
fn root_turn_summary(text: &str, _developer_context: Option<&str>, plan_enabled: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode-root-turn-v2\0");
    digest.update(text.as_bytes());
    digest.update(b"\0");
    digest.update([u8::from(plan_enabled)]);
    let preview = text.trim().chars().take(256).collect::<String>();
    format!("{preview} [sha256:{:x}]", digest.finalize())
}

/// 启动与冷恢复共用引用摘要，确保同正文的不同资源选择保持不同请求身份。
fn append_input_reference_digest(
    summary: String,
    references: &[keencode_model::InputReference],
) -> Result<String, AgentRuntimeError> {
    if references.is_empty() {
        return Ok(summary);
    }
    Ok(format!(
        "{summary}:refs:{:x}",
        Sha256::digest(serde_json::to_vec(references).map_err(runtime_operation_failed)?)
    ))
}

/// 把已物化附件纳入幂等摘要，避免相同 operationId 重试时静默丢掉新附件。
fn append_attachment_digest(
    summary: String,
    images: &[ImageContent],
    text_context: &[String],
) -> Result<String, AgentRuntimeError> {
    if images.is_empty() && text_context.is_empty() {
        return Ok(summary);
    }
    let payload = serde_json::to_vec(&(images, text_context)).map_err(runtime_operation_failed)?;
    Ok(format!(
        "{summary}:attachments:{:x}",
        Sha256::digest(payload)
    ))
}

/// 将界面支持的七档推理强度映射为 Provider 中立请求配置。
fn parse_reasoning_effort(value: &str) -> Result<Option<ReasoningEffort>, AgentRuntimeError> {
    match value {
        "none" => Ok(None),
        "minimal" => Ok(Some(ReasoningEffort::Minimal)),
        "low" => Ok(Some(ReasoningEffort::Low)),
        "medium" => Ok(Some(ReasoningEffort::Medium)),
        "high" => Ok(Some(ReasoningEffort::High)),
        "xhigh" => Ok(Some(ReasoningEffort::ExtraHigh)),
        "max" => Ok(Some(ReasoningEffort::Maximum)),
        _ => Err(AgentRuntimeError::RuntimeOperationFailed),
    }
}

/// 将 Provider 中立模型枚举显式转换为稳定 Session 持久枚举。
fn reasoning_effort_snapshot(effort: ReasoningEffort) -> ReasoningEffortSnapshot {
    match effort {
        ReasoningEffort::Minimal => ReasoningEffortSnapshot::Minimal,
        ReasoningEffort::Low => ReasoningEffortSnapshot::Low,
        ReasoningEffort::Medium => ReasoningEffortSnapshot::Medium,
        ReasoningEffort::High => ReasoningEffortSnapshot::High,
        ReasoningEffort::ExtraHigh => ReasoningEffortSnapshot::ExtraHigh,
        ReasoningEffort::Maximum => ReasoningEffortSnapshot::Maximum,
    }
}

/// 将持久推理强度映射为 AgentProfile 使用的稳定设置名称。
fn reasoning_effort_snapshot_name(effort: ReasoningEffortSnapshot) -> String {
    match effort {
        ReasoningEffortSnapshot::Minimal => "minimal",
        ReasoningEffortSnapshot::Low => "low",
        ReasoningEffortSnapshot::Medium => "medium",
        ReasoningEffortSnapshot::High => "high",
        ReasoningEffortSnapshot::ExtraHigh => "xhigh",
        ReasoningEffortSnapshot::Maximum => "max",
    }
    .to_owned()
}

/// 将稳定 Session 持久枚举显式恢复为 Provider 中立模型枚举。
fn reasoning_effort_from_snapshot(effort: ReasoningEffortSnapshot) -> ReasoningEffort {
    match effort {
        ReasoningEffortSnapshot::Minimal => ReasoningEffort::Minimal,
        ReasoningEffortSnapshot::Low => ReasoningEffort::Low,
        ReasoningEffortSnapshot::Medium => ReasoningEffort::Medium,
        ReasoningEffortSnapshot::High => ReasoningEffort::High,
        ReasoningEffortSnapshot::ExtraHigh => ReasoningEffort::ExtraHigh,
        ReasoningEffortSnapshot::Maximum => ReasoningEffort::Maximum,
    }
}

/// 对标题输入做域分离摘要，持久缓存不保存用户正文。
fn title_input_sha256(input: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode-title-input-v1\0");
    digest.update(input.as_bytes());
    format!("{:x}", digest.finalize())
}

/// 校验独立模型返回的是短标题而不是回答、拒绝说明或多行正文。
fn validate_generated_title(candidate: &str) -> Result<String, AgentRuntimeError> {
    const REFUSAL_PREFIXES: &[&str] = &[
        "我无法",
        "抱歉",
        "对不起",
        "i cannot",
        "i can't",
        "i am unable",
        "i'm unable",
        "sorry",
        "as an ai",
    ];

    let title = candidate.trim();
    let normalized = title
        .trim_matches(['\"', '\'', '`', '“', '”', '‘', '’'])
        .trim()
        .to_lowercase();
    let has_internal_sentence_boundary = title.char_indices().any(|(index, value)| {
        matches!(value, '。' | '！' | '？' | '!' | '?') && index + value.len_utf8() < title.len()
    });
    if title.is_empty()
        || title.chars().count() > GENERATED_TITLE_MAX_CHARS
        || title.chars().any(char::is_control)
        || has_internal_sentence_boundary
        || REFUSAL_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
    {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    Ok(title.to_owned())
}

/// 为 Turn 前置控制写入派生稳定、可跨响应丢失重试的 operationId。
fn control_operation_id(kind: &str, session_id: &str, turn_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode-turn-control-v1\0");
    digest.update(kind.as_bytes());
    digest.update(b"\0");
    digest.update(session_id.as_bytes());
    digest.update(b"\0");
    digest.update(turn_id.as_bytes());
    format!("operation-{:x}", digest.finalize())
}

/// 用量身份保留全部维度并固定为 75 字节；长度前缀避免字段分隔符歧义。
fn goal_usage_operation_id(fields: &[&str; 6]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode-goal-usage-v1\0");
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("goal-usage:{:x}", digest.finalize())
}

/// 将 Provider 协议映射为无凭据 Session 快照协议。
fn provider_protocol_snapshot(protocol: ProviderProtocol) -> ProviderProtocolSnapshot {
    match protocol {
        ProviderProtocol::Messages => ProviderProtocolSnapshot::AnthropicMessages,
        ProviderProtocol::ChatCompletions => ProviderProtocolSnapshot::OpenAiChatCompletions,
        ProviderProtocol::Responses => ProviderProtocolSnapshot::OpenAiResponses,
    }
}

/// 从已解析 Provider 构造当前唯一无凭据 Session 快照。
fn provider_snapshot(provider: &ResolvedProvider) -> ProviderSnapshot {
    ProviderSnapshot {
        provider_id: provider.provider_id().to_owned(),
        model: provider.model().to_owned(),
        context_window: provider.capabilities(provider.model()).max_context_tokens,
        protocol: provider_protocol_snapshot(provider.protocol()),
        config_fingerprint: provider.config_identity().to_owned(),
        reasoning_effort: None,
    }
}

/// 返回非零 UTC Unix 毫秒时间，系统时钟异常时使用一作为稳定下界。
pub(crate) fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

/// 从根级 Collaboration 只读快照提取仍可精确取消的单层子 Agent。
fn current_child_agent_turns_for_root(
    coordinator: &CollaborationCoordinator,
    root_agent_id: &RunnerAgentId,
) -> Result<Vec<CollaborationAgentSummary>, AgentRuntimeError> {
    coordinator
        .list_agents_for_root(root_agent_id)
        .map_err(runtime_operation_failed)
        .map(|agents| {
            agents
                .into_iter()
                .filter(|summary| {
                    summary.agent.path.depth() == AgentDepth::CHILD
                        && current_collaboration_turn_id(&summary.status).is_some()
                })
                .collect()
        })
}

/// 对账一次取消后仍保存在 Store 中的指定 WaitingCapacity pending 证据。
fn reconcile_waiting_capacity_cancel(
    runtime: &SessionCollaborationRuntime,
    agent_id: &RunnerAgentId,
    turn_id: &AgentTurnId,
) -> Result<bool, AgentRuntimeError> {
    let snapshot = runtime
        .store
        .load_transition_snapshot()
        .map_err(runtime_operation_failed)?;
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    let records = snapshot
        .unstarted_turn_terminations
        .into_iter()
        .filter(|record| &record.agent_id == agent_id && &record.turn_id == turn_id)
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(false);
    }
    let session = runtime.store.bound_runtime_session()?;
    reconcile_unstarted_turn_termination_records(
        &session,
        &runtime.store,
        &snapshot.commit.checkpoint,
        &records,
    )?;
    Ok(true)
}

/// 按稳定 Turn 标识查找并对账遗留的 WaitingCapacity pending 证据。
fn reconcile_waiting_capacity_cancel_by_turn(
    runtime: &SessionCollaborationRuntime,
    turn_id: &str,
) -> Result<bool, AgentRuntimeError> {
    let snapshot = runtime
        .store
        .load_transition_snapshot()
        .map_err(runtime_operation_failed)?;
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    let records = snapshot
        .unstarted_turn_terminations
        .into_iter()
        .filter(|record| record.turn_id.as_str() == turn_id)
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(false);
    }
    let session = runtime.store.bound_runtime_session()?;
    reconcile_unstarted_turn_termination_records(
        &session,
        &runtime.store,
        &snapshot.commit.checkpoint,
        &records,
    )?;
    Ok(true)
}

/// 返回排队、运行或取消中的当前 Turn 标识，终态与空闲状态返回空。
fn current_collaboration_turn_id(status: &CollaborationAgentStatus) -> Option<&AgentTurnId> {
    match status {
        CollaborationAgentStatus::WaitingCapacity { turn_id }
        | CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => Some(turn_id),
        _ => None,
    }
}

/// 生成与真实子 Agent 启动账本一致的短任务摘要。
fn child_agent_turn_summary(prompt: Option<&str>) -> String {
    prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| prompt.chars().take(256).collect::<String>())
        .unwrap_or_else(|| "Agent mailbox followup".to_owned())
}

/// 从权威根 Turn 读取排队任务的稳定起点；异常恢复状态退回 Session 创建时间。
fn queued_agent_started_at(
    state: &SessionState,
    agent: &CollaborationAgentSummary,
) -> Result<u64, AgentRuntimeError> {
    let root_turn_id = agent
        .current_root_turn_id
        .as_ref()
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    state
        .turns
        .iter()
        .find(|(turn_id, _)| turn_id.as_str() == root_turn_id.as_str())
        .map(|(_, turn)| turn.started_at_unix_ms)
        .or((state.created_at_unix_ms > 0).then_some(state.created_at_unix_ms))
        .filter(|started_at| *started_at > 0)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 拆分子 Agent 的 `providerId::modelId` 覆盖；普通模型标识返回空覆盖。
fn split_child_agent_model_override(
    model_reference: &str,
) -> Result<Option<(&str, &str)>, AgentRuntimeError> {
    let Some((provider_id, model)) = model_reference.split_once("::") else {
        return Ok(None);
    };
    if provider_id.is_empty()
        || model.is_empty()
        || provider_id.trim() != provider_id
        || model.trim() != model
        || provider_id.chars().any(char::is_control)
        || model.chars().any(char::is_control)
        || model.contains("::")
    {
        return Err(AgentRuntimeError::RuntimeOperationFailed);
    }
    Ok(Some((provider_id, model)))
}

/// 返回执行前的工具快照；冷恢复子 Agent 必须再次套用根专用工具边界。
fn runtime_tool_snapshot(profile: &AgentProfile, is_root: bool) -> Vec<String> {
    let mut tool_snapshot = profile.tool_snapshot.clone();
    if !is_root {
        finalize_child_agent_tool_snapshot(&mut tool_snapshot);
    }
    tool_snapshot
}

/// 小上下文请求严格暴露四个核心工具，不继承 Profile、扩展或子 Agent 通信工具。
fn request_tool_snapshot(
    profile: &AgentProfile,
    is_root: bool,
    small_context: bool,
) -> Vec<String> {
    if small_context {
        return ["Read", "Edit", "Write", "Bash"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    runtime_tool_snapshot(profile, is_root)
}

/// 将桌面权限协调器模式映射为资源层 Journal 值，保持 Core 不依赖桌面模块。
fn resource_permission_mode(mode: PermissionMode) -> SessionPermissionMode {
    match mode {
        PermissionMode::Build => SessionPermissionMode::Build,
        PermissionMode::Edit => SessionPermissionMode::Edit,
        PermissionMode::Plan => SessionPermissionMode::Plan,
        PermissionMode::Yolo => SessionPermissionMode::Yolo,
    }
}

/// 将资源层冷恢复的 Journal 值映射回桌面权限门。
fn desktop_permission_mode(mode: SessionPermissionMode) -> PermissionMode {
    match mode {
        SessionPermissionMode::Build => PermissionMode::Build,
        SessionPermissionMode::Edit => PermissionMode::Edit,
        SessionPermissionMode::Plan => PermissionMode::Plan,
        SessionPermissionMode::Yolo => PermissionMode::Yolo,
    }
}

/// 校验宿主操作只接受资源层合法 Session 标识。
fn validate_session_id(session_id: &str) -> Result<(), AgentRuntimeError> {
    keencode_resources::SessionId::new(session_id.to_owned())
        .map(|_| ())
        .map_err(|_| AgentRuntimeError::InvalidSession)
}

/// Workflow actor 的 `AgentProfile.cwd` 来自父 run-started 的冻结值。装配时必须
/// 再次与 actor Session 的持久 project_root 对账，避免恢复路径或相对路径把工具
/// 环境切换到另一个父工作区；普通 root/child 保持原有可选 workspace guard 语义。
fn workflow_actor_environment_required(
    execution: &RuntimeAgentExecution,
    profile_cwd: &Path,
) -> Result<bool, AgentRuntimeError> {
    let is_workflow_actor = execution
        .session
        .is_workflow_actor()
        .map_err(runtime_operation_failed)?;
    if !is_workflow_actor {
        return Ok(false);
    }
    let frozen_cwd = canonical_project_root(profile_cwd)?;
    if frozen_cwd != execution.project_root {
        return Err(AgentRuntimeError::SessionProjectMismatch);
    }
    Ok(true)
}

/// 将用户授权项目解析为存在的规范目录，拒绝文件和不可解析路径。
fn canonical_project_root(project_root: &Path) -> Result<PathBuf, AgentRuntimeError> {
    let canonical = std::fs::canonicalize(project_root)
        .map_err(|_| AgentRuntimeError::SessionProjectMismatch)?;
    if !canonical.is_dir() {
        return Err(AgentRuntimeError::SessionProjectMismatch);
    }
    Ok(canonical)
}

/// 从规范项目根与前端稳定 operationId 派生可跨响应丢失重试的 Session 标识。
fn deterministic_session_id(
    project_root: &Path,
    operation_id: &str,
) -> Result<String, AgentRuntimeError> {
    let operation_id = operation_id.trim();
    if operation_id.is_empty() || operation_id.len() > 512 {
        return Err(AgentRuntimeError::InvalidSession);
    }
    let mut digest = Sha256::new();
    digest.update(b"keencode-session-v1\0");
    digest.update(project_root.to_string_lossy().as_bytes());
    digest.update(b"\0");
    digest.update(operation_id.as_bytes());
    Ok(format!("session-{:x}", digest.finalize()))
}

/// 验证持久 Session 创建时绑定的项目与本次调用授权目录完全一致。
fn ensure_session_project(
    session: &RuntimeSession,
    expected_project_root: &Path,
) -> Result<(), AgentRuntimeError> {
    let snapshot = session.snapshot().map_err(runtime_operation_failed)?;
    let stored = canonical_project_root(Path::new(&snapshot.state.project_root))?;
    if stored != expected_project_root {
        return Err(AgentRuntimeError::SessionProjectMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRuntime, AgentRuntimeError, ContextManager, DeferredSessionLifecycle,
        GENERATED_TITLE_MAX_CHARS, LifecycleStartState, RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS,
        RecoveredRootLifecycle, RootAgentSeed, RootTurnOptions, RootTurnStartOutcome,
        RunnerAgentId, RuntimeAgentTemplate, RuntimeAgentTemplateContext,
        RuntimeExtensionCandidate, RuntimeExtensionContributor, RuntimeExtensionDiagnostic,
        RuntimeGoalUsageSink, RuntimeToolContext, SessionCollaborationStore,
        SessionWorkspaceMutationAdmission, TurnBoundProvider, authoritative_recovered_turn_outcome,
        clear_historical_reasoning_state, commit_goal_turn_elapsed, complete_runtime_turn,
        coordinator_has_pending_dynamic_input_claim, dynamic_input_receipt_matches_claim,
        extension_diagnostic_message, is_retryable_runtime_turn_completion_error,
        parse_reasoning_effort, prompt_cache_key_for_endpoint, provider_snapshot,
        provider_supports_reasoning_continuation, recovered_authoritative_turn_outcomes,
        release_runtime_turn_state, request_tool_snapshot, root_turn_summary,
        runtime_tool_snapshot, session_has_persisted_user_facts,
        should_retry_runtime_turn_completion, split_child_agent_model_override,
        test_executor_handle, validate_generated_title, validate_recovered_mailbox_claim,
        wait_for_turn_started,
    };
    use keencode_acp::{BackgroundTaskKind, ConnectionId};
    use keencode_agent::{
        AgentDynamicInputAcknowledgement, AgentDynamicInputBatch, AgentDynamicInputBoundary,
        AgentDynamicInputError, AgentDynamicInputSource, AgentExecutionPort, AgentPath,
        AgentProfile, AgentRunner, AgentTemplateSnapshot, AgentTreeQuiesceResult, AgentTurnLaunch,
        AgentTurnOutcome, AgentTurnSignal, AgentTurnStartResult, CloseAgentTree,
        CollaborationAgentStatus, CollaborationAppendResult, CollaborationCoordinator,
        CollaborationError, CollaborationEvent, CollaborationEventKind,
        CollaborationGlobalTurnLimiter, CollaborationLimits, CollaborationPortError,
        CollaborationStore, CollaborationTransitionCommit, ContextCompressor, ContextInheritance,
        ContextPolicy, ContextSummaryRequest, ContextTokenEstimator, GoalController, GoalDraft,
        HookPhase, HookRuntime, JsonContextTokenEstimator, PlanGuard, ProviderContextCompressor,
        QuiesceAgentTree, RecoveredCoordinator, RootAgentRequest, RunLimits, SpawnAgentRequest,
        ToolCallId, ToolRegistry, TurnCancellation, TurnId as AgentTurnId, TurnRequest,
        UuidCollaborationIdGenerator,
    };
    /// 取消后旧 worker 的迟到成功不能提交，也不能清掉重载候选的新租约。
    #[test]
    fn lifecycle_start_state_rejects_late_completion_after_reload() {
        let state = LifecycleStartState::new();
        let key = ("root".to_owned(), HookPhase::SessionStart);
        assert!(state.reserve(key.clone(), "old-turn".to_owned()));
        let mut old_attempt = state
            .claim(key.clone(), "old-turn".to_owned())
            .expect("旧候选应取得启动租约");

        state.abort(&key, "old-turn");
        assert!(
            !state
                .started()
                .lock()
                .expect("启动状态锁应可用")
                .contains(&key)
        );

        assert!(state.reserve(key.clone(), "reloaded-turn".to_owned()));
        let mut reloaded_attempt = state
            .claim(key.clone(), "reloaded-turn".to_owned())
            .expect("重载候选应取得新启动租约");
        reloaded_attempt.callback_succeeded();
        state.deliver(&key, "reloaded-turn");

        old_attempt.callback_succeeded();
        drop(old_attempt);
        assert!(
            state
                .started()
                .lock()
                .expect("启动状态锁应可用")
                .contains(&key)
        );
    }
    use keencode_model::{
        ImageContent, Message as ModelMessage, MessageRole, ModelError, ModelProvider,
        ModelRequest, ModelStreamEvent, ProviderCapabilities, ResponseMetadata, ScriptedProvider,
        ScriptedReply, StopReason, TokenUsage,
    };
    use keencode_provider::{
        ProviderConfig, ProviderModelPolicy, ProviderRegistration, REQUEST_METADATA_AGENT_ID,
        REQUEST_METADATA_PROMPT_CACHE_KEY, REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID,
        REQUEST_METADATA_TURN_ID, WireResponseMode,
    };
    use keencode_resources::{
        AgentId as ResourceAgentId, DynamicInputKind, DynamicInputReceipt,
        MailboxMessage as ResourceMailboxMessage, MailboxMessageId as ResourceMailboxMessageId,
        MailboxState, PlanState, ProviderProtocolSnapshot, ProviderSnapshot, SessionEvent,
        SessionId as ResourceSessionId, SessionInputQueueItem, SessionState, SubAgentState,
        SubAgentStatus, TitleSource, TranscriptRecord, TurnId as ResourceTurnId, TurnState,
        TurnStatus, TurnStopReason,
    };
    use keencode_runtime::{
        CreateSessionRequest, PersistentAgentState, RuntimeConfig, RuntimeSession,
        RuntimeTurnRequest,
    };
    use keencode_tools::{
        GitWorktreeLeaseManager, UserQuestion, UserQuestionHandler, UserQuestionOption,
        UserQuestionRequest,
    };
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};
    use url::Url;

    /// 扩展诊断送达 ACP 前必须保留稳定分类、清理控制字符并限制正文大小。
    #[test]
    fn extension_diagnostic_message_is_safe_and_bounded() {
        let diagnostic = RuntimeExtensionDiagnostic {
            source: "mcp".to_owned(),
            server: "docs".to_owned(),
            code: "mcp_tool_discovery_failed".to_owned(),
            message: format!("bad\n{}", "界".repeat(2_000)),
            tool: Some("lookup".to_owned()),
        };
        let message = extension_diagnostic_message(&diagnostic);
        assert!(message.starts_with(
            "扩展诊断：mcp Server=docs Tool=lookup Code=mcp_tool_discovery_failed bad "
        ));
        assert!(!message.contains('\n'));
        assert!(message.ends_with("...[已截断]"));
        assert!(message.len() <= 4 * 1024);
    }

    /// 创建一个只返回固定文本并正常结束的模型脚本。
    fn completed_reply(text: &str) -> ScriptedReply {
        ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: text.to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])
    }

    /// 仅验证普通根 Turn 的测试不应让后台自动标题额外消耗一次 Provider 响应。
    fn mark_session_title_manual(session: &RuntimeSession) {
        session
            .rename("test-manual-title", "测试会话", Some(TitleSource::Manual))
            .expect("测试 Session 标题应可显式固定");
    }

    /// 按生产装配规则构造会话冻结的稳定前缀：System 规则、能力说明与指令拼接。
    fn stable_agent_prefix(
        can_spawn: bool,
        has_skill: bool,
        instructions: &str,
    ) -> Vec<ModelMessage> {
        let mut capabilities = crate::agent_prompt::capabilities(can_spawn, has_skill);
        if !instructions.is_empty() {
            capabilities.push_str("\n\n");
            capabilities.push_str(instructions);
        }
        vec![
            ModelMessage::text(MessageRole::System, crate::agent_prompt::core()),
            ModelMessage::text(MessageRole::System, capabilities),
        ]
    }

    /// 创建包含明确 Token 用量的固定模型响应，供 replay Provider 快照测试使用。
    fn completed_reply_with_usage(text: &str, usage: TokenUsage) -> ScriptedReply {
        ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: text.to_owned(),
            },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])
    }

    /// 通过 RuntimeSession 写入一个包含模型 Round 用量的完成根 Turn。
    async fn persist_usage_root_turn(
        session: &RuntimeSession,
        turn_id: &str,
        model: &str,
        prompt: &str,
        used: u64,
    ) {
        let capabilities = ProviderCapabilities::default();
        let provider_snapshot = ProviderSnapshot {
            provider_id: "scripted-provider".to_owned(),
            model: model.to_owned(),
            context_window: capabilities.max_context_tokens,
            protocol: ProviderProtocolSnapshot::OpenAiResponses,
            config_fingerprint: "scripted-provider-config".to_owned(),
            reasoning_effort: None,
        };
        let provider = Arc::new(ScriptedProvider::new(
            capabilities,
            [completed_reply_with_usage(
                "完成",
                TokenUsage {
                    input_tokens: Some(used.saturating_sub(1)),
                    output_tokens: Some(1),
                    reasoning_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    total_tokens: Some(used),
                },
            )],
        ));
        let input = ModelMessage::text(MessageRole::User, prompt);
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("测试 Session 标识应有效"),
            keencode_agent::TurnId::new(turn_id).expect("测试 Turn 标识应有效"),
            keencode_agent::AgentId::new("root").expect("测试根 Agent 标识应有效"),
            model,
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        session
            .bind_agent_runner(AgentRunner::new(
                provider,
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(
                RuntimeTurnRequest::root(
                    request,
                    vec![input],
                    root_turn_summary(prompt, None, false),
                )
                .with_provider_snapshot(provider_snapshot),
            )
            .await
            .expect("测试模型 Round 应完成");
    }

    /// 启动只接受一次请求的本地 Responses 服务，并返回捕获的 JSON 请求正文。
    fn spawn_buffered_responses_server(
        response_text: &str,
    ) -> (String, JoinHandle<Result<Value, String>>) {
        spawn_buffered_responses_server_with_status(response_text, "completed", None)
    }

    /// 返回指定终态的本地 Responses 响应，验证有文本但未完整完成的调用边界。
    fn spawn_buffered_responses_server_with_status(
        response_text: &str,
        status: &str,
        incomplete_reason: Option<&str>,
    ) -> (String, JoinHandle<Result<Value, String>>) {
        let mut body = json!({
            "id": "response-runtime-test",
            "object": "response",
            "model": "test-model",
            "status": status,
            "output": [{
                "id": "message-runtime-test",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": response_text}]
            }],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
        });
        if let Some(reason) = incomplete_reason {
            body["incomplete_details"] = json!({"reason": reason});
        }
        spawn_buffered_responses_body(body)
    }

    /// 返回调用方提供的合成 Responses 正文，复用单次请求捕获与超时约束。
    fn spawn_buffered_responses_body(body: Value) -> (String, JoinHandle<Result<Value, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地模型端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地模型监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地模型地址应读取");
        let server = thread::spawn(move || {
            // 完整测试集并行执行时本地线程可能短暂饥饿，测试服务必须给连接与正文留出同一稳定窗口。
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err("等待本地模型请求超时".to_owned());
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(format!("接受本地模型请求失败：{error}")),
                }
            };
            // Windows 可能让 accept 得到的连接继承监听器的非阻塞状态；正文读取必须恢复为阻塞并由超时约束。
            stream
                .set_nonblocking(false)
                .map_err(|error| format!("恢复本地模型连接阻塞模式失败：{error}"))?;
            let request = read_json_request(&mut stream)?;
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .and_then(|_| stream.flush())
                .map_err(|error| format!("写入本地模型响应失败：{error}"))?;
            Ok(request)
        });
        (format!("http://{address}/v1"), server)
    }

    /// 启动按顺序处理指定数量请求的本地 Responses 服务，并返回全部请求正文。
    fn spawn_buffered_responses_server_for_requests(
        response_text: &str,
        request_count: usize,
    ) -> (String, JoinHandle<Result<Vec<Value>, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地模型端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地模型监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地模型地址应读取");
        let response_text = response_text.to_owned();
        let server = thread::spawn(move || {
            // 完整测试集并行执行时本地线程可能短暂饥饿，测试服务必须给每轮连接留出稳定窗口。
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::with_capacity(request_count);
            for _ in 0..request_count {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return Err("等待本地模型请求超时".to_owned());
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(format!("接受本地模型请求失败：{error}")),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("恢复本地模型连接阻塞模式失败：{error}"))?;
                let request = read_json_request(&mut stream)?;
                let body = json!({
                    "id": "response-runtime-test",
                    "object": "response",
                    "model": "test-model",
                    "status": "completed",
                    "output": [{
                        "id": "message-runtime-test",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": response_text}]
                    }],
                    "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .and_then(|_| stream.flush())
                    .map_err(|error| format!("写入本地模型响应失败：{error}"))?;
                requests.push(request);
            }
            Ok(requests)
        });
        (format!("http://{address}/v1"), server)
    }

    /// 依序返回预置响应体的本地 Responses 服务；第 n 个请求返回第 n 个响应体。
    fn spawn_buffered_responses_sequence(
        bodies: Vec<Value>,
    ) -> (String, JoinHandle<Result<Vec<Value>, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地模型端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地模型监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地模型地址应读取");
        let server = thread::spawn(move || {
            // 完整测试集并行执行时本地线程可能短暂饥饿，测试服务必须给每轮连接留出稳定窗口。
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::with_capacity(bodies.len());
            for body in bodies {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return Err("等待本地模型请求超时".to_owned());
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(format!("接受本地模型请求失败：{error}")),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("恢复本地模型连接阻塞模式失败：{error}"))?;
                let request = read_json_request(&mut stream)?;
                let body_text = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_text.len(),
                    body_text
                );
                stream
                    .write_all(response.as_bytes())
                    .and_then(|_| stream.flush())
                    .map_err(|error| format!("写入本地模型响应失败：{error}"))?;
                requests.push(request);
            }
            Ok(requests)
        });
        (format!("http://{address}/v1"), server)
    }

    /// 控制首个本地模型请求何时返回，用于稳定保持根 Agent Turn 活跃。
    #[derive(Clone)]
    struct ResponseGate {
        /// 由测试线程释放、由模型服务线程等待的共享状态。
        state: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        /// 已经读取完整请求正文的请求数量及其通知信号。
        observed: Arc<(std::sync::Mutex<usize>, std::sync::Condvar)>,
    }

    impl ResponseGate {
        /// 创建处于阻塞状态的响应闸门。
        fn new() -> Self {
            Self {
                state: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
                observed: Arc::new((std::sync::Mutex::new(0), std::sync::Condvar::new())),
            }
        }

        /// 记录本地模型服务已读取一条完整请求，供测试线程等待真实 Provider 到达。
        fn mark_request(&self) {
            let (observed, signal) = &*self.observed;
            if let Ok(mut observed) = observed.lock() {
                *observed = observed.saturating_add(1);
                signal.notify_all();
            }
        }

        /// 等待本地模型服务读取指定数量的请求，并以有限时限避免测试永久阻塞。
        fn wait_for_requests(&self, expected: usize) -> Result<(), String> {
            let (observed, signal) = &*self.observed;
            let mut observed = observed
                .lock()
                .map_err(|_| "本地模型请求观察状态不可用".to_owned())?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while *observed < expected {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| "等待本地模型请求超时".to_owned())?;
                let (next, timeout) = signal
                    .wait_timeout(observed, remaining)
                    .map_err(|_| "本地模型请求观察状态不可用".to_owned())?;
                observed = next;
                if timeout.timed_out() && *observed < expected {
                    return Err("等待本地模型请求超时".to_owned());
                }
            }
            Ok(())
        }

        /// 释放首个响应并唤醒等待的模型服务线程。
        fn release(&self) {
            let (released, signal) = &*self.state;
            if let Ok(mut released) = released.lock() {
                *released = true;
                signal.notify_all();
            }
        }

        /// 等待测试释放首个响应，并以有限时限避免测试线程永久阻塞。
        fn wait(&self) -> Result<(), String> {
            let (released, signal) = &*self.state;
            let mut released = released
                .lock()
                .map_err(|_| "本地模型响应闸门状态不可用".to_owned())?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !*released {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| "等待本地模型响应闸门超时".to_owned())?;
                let (next, timeout) = signal
                    .wait_timeout(released, remaining)
                    .map_err(|_| "本地模型响应闸门状态不可用".to_owned())?;
                released = next;
                if timeout.timed_out() && !*released {
                    return Err("等待本地模型响应闸门超时".to_owned());
                }
            }
            Ok(())
        }
    }

    /// 多闸门本地 Responses 测试服务的返回值，集中保留复杂类型的语义名称。
    type GatedResponsesServer = (
        String,
        Vec<ResponseGate>,
        JoinHandle<Result<Vec<Value>, String>>,
    );

    /// 启动并发处理本地 Responses 请求的服务，仅延迟首个请求的响应。
    fn spawn_gated_buffered_responses_server(
        response_text: &str,
        request_count: usize,
        gate_user_text: &str,
    ) -> (String, ResponseGate, JoinHandle<Result<Vec<Value>, String>>) {
        let (base_url, mut gates, server) = spawn_gated_buffered_responses_server_with_texts(
            response_text,
            request_count,
            &[gate_user_text],
        );
        (
            base_url,
            gates.pop().expect("单闸门本地模型服务应返回闸门"),
            server,
        )
    }

    /// 启动可分别控制多类请求响应的本地 Responses 服务，返回每类请求对应的闸门。
    fn spawn_gated_buffered_responses_server_with_texts(
        response_text: &str,
        request_count: usize,
        gate_user_texts: &[&str],
    ) -> GatedResponsesServer {
        assert!(request_count > 0, "闸门服务至少需要一个请求");
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地模型端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地模型监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地模型地址应读取");
        let gates = gate_user_texts
            .iter()
            .map(|_| ResponseGate::new())
            .collect::<Vec<_>>();
        let server_gates = gates.clone();
        let response_text = response_text.to_owned();
        let gate_user_texts = gate_user_texts
            .iter()
            .map(|text| (*text).to_owned())
            .collect::<Vec<_>>();
        let server = thread::spawn(move || {
            let captured = Arc::new(std::sync::Mutex::new(
                Vec::<(usize, Result<Value, String>)>::new(),
            ));
            let mut handlers = Vec::with_capacity(request_count);
            let deadline = Instant::now() + Duration::from_secs(10);
            for index in 0..request_count {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return Err("等待本地模型请求超时".to_owned());
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(format!("接受本地模型请求失败：{error}")),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("恢复本地模型连接阻塞模式失败：{error}"))?;
                let captured = Arc::clone(&captured);
                let gates = server_gates.clone();
                let response_text = response_text.clone();
                let gate_user_texts = gate_user_texts.clone();
                handlers.push(thread::spawn(move || {
                    let result = (|| {
                        let request = read_json_request(&mut stream)?;
                        if let Some(gate) = gate_user_texts
                            .iter()
                            .zip(gates.iter())
                            .find_map(|(text, gate)| {
                                request_contains_user_text(&request, text).then_some(gate)
                            })
                        {
                            gate.mark_request();
                            gate.wait()?;
                        }
                        let body = json!({
                            "id": "response-runtime-test",
                            "object": "response",
                            "model": "test-model",
                            "status": "completed",
                            "output": [{
                                "id": "message-runtime-test",
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": response_text}]
                            }],
                            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream
                            .write_all(response.as_bytes())
                            .and_then(|_| stream.flush())
                            .map_err(|error| format!("写入本地模型响应失败：{error}"))?;
                        Ok(request)
                    })();
                    if let Ok(mut captured) = captured.lock() {
                        captured.push((index, result));
                    }
                }));
            }
            for handler in handlers {
                handler
                    .join()
                    .map_err(|_| "本地模型请求线程不应 panic".to_owned())?;
            }
            let mut captured = Arc::try_unwrap(captured)
                .map_err(|_| "本地模型请求捕获状态仍被引用".to_owned())?
                .into_inner()
                .map_err(|_| "本地模型请求捕获状态不可用".to_owned())?;
            captured.sort_by_key(|(index, _)| *index);
            captured
                .into_iter()
                .map(|(_, result)| result)
                .collect::<Result<Vec<_>, _>>()
        });
        (format!("http://{address}/v1"), gates, server)
    }

    /// 启动只观察连接、不向模型返回响应的本地服务，用于断言 Provider 未被调用。
    fn spawn_responses_request_probe() -> (String, JoinHandle<Result<Option<Value>, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地模型端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地模型监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地模型地址应读取");
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .map_err(|error| format!("恢复本地模型连接阻塞模式失败：{error}"))?;
                        return read_json_request(&mut stream).map(Some);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Ok(None);
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(format!("接受本地模型请求失败：{error}")),
                }
            }
        });
        (format!("http://{address}/v1"), server)
    }

    /// 等待真实 Runtime 的 Runner、Coordinator 槽位和 Session 状态全部收敛。
    async fn wait_for_session_idle(runtime: &Arc<AgentRuntime>, session_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while runtime
            .session_has_active_work(session_id)
            .expect("测试 Session 活动状态应读取")
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !runtime
                .session_has_active_work(session_id)
                .expect("测试 Session 最终活动状态应读取"),
            "测试 Runtime 应在有限时限内收敛；{}",
            session_active_work_diagnostics(runtime, session_id)
        );
    }

    /// 失败诊断只读取各层活动账本，帮助区分 Runner、Journal、后台任务和容量残留。
    fn session_active_work_diagnostics(runtime: &Arc<AgentRuntime>, session_id: &str) -> String {
        let Ok(session) = runtime.runtime_manager.get(session_id.to_owned()) else {
            return "session=unavailable".to_owned();
        };
        let active_turn_ids = session
            .active_turn_ids()
            .map(|turns| format!("runtime_active_turns={turns:?}"))
            .unwrap_or_else(|error| format!("runtime_active_turns=error:{error}"));
        let journal = match session.snapshot() {
            Ok(snapshot) => format!(
                "journal_running_turns={} journal_pending_tools={} journal_open_terminals={} journal_active_sub_agents={} journal_unreleased_worktrees={}",
                snapshot
                    .state
                    .turns
                    .values()
                    .filter(|turn| turn.status == TurnStatus::Running)
                    .count(),
                snapshot
                    .state
                    .tools
                    .values()
                    .filter(|tool| tool.outcome.is_none())
                    .count(),
                snapshot
                    .state
                    .terminals
                    .values()
                    .filter(|terminal| !terminal.exited)
                    .count(),
                snapshot
                    .state
                    .sub_agents
                    .values()
                    .filter(|agent| {
                        matches!(
                            agent.status,
                            SubAgentStatus::Pending
                                | SubAgentStatus::Running
                                | SubAgentStatus::Waiting
                        )
                    })
                    .count(),
                snapshot
                    .state
                    .worktrees
                    .values()
                    .filter(|worktree| !worktree.released)
                    .count(),
            ),
            Err(error) => format!("journal=error:{error}"),
        };
        let collaboration = runtime
            .collaboration_sessions
            .lock()
            .ok()
            .and_then(|runtimes| runtimes.get(session_id).cloned());
        let collaboration = match collaboration {
            Some(collaboration) => {
                let execution = collaboration
                    .execution
                    .state
                    .lock()
                    .map(|state| {
                        format!(
                            "execution_running_turns={} execution_prepared_root_turns={} execution_running_ids={:?}",
                            state.running_turns.len(),
                            state.prepared_root_turns.len(),
                            state.running_turns.keys().collect::<Vec<_>>(),
                        )
                    })
                    .unwrap_or_else(|_| "execution_state=poisoned".to_owned());
                let background = collaboration
                    .execution
                    .background_tasks
                    .list_running()
                    .map(|tasks| {
                        format!(
                            "background_running_tasks={} background_task_ids={:?}",
                            tasks.len(),
                            tasks
                                .iter()
                                .map(|task| task.task_id.as_str())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .unwrap_or_else(|error| format!("background=error:{error}"));
                let capacity = collaboration
                    .coordinator
                    .capacity()
                    .map(|capacity| {
                        format!(
                            "coordinator_global_in_use={}/{} coordinator_roots={:?}",
                            capacity.global_in_use, capacity.global_limit, capacity.roots,
                        )
                    })
                    .unwrap_or_else(|error| format!("coordinator_capacity=error:{error}"));
                format!("{execution} {background} {capacity}")
            }
            None => "collaboration=absent".to_owned(),
        };
        format!("{active_turn_ids} {journal} {collaboration}")
    }

    /// 判断本地 Responses 请求是否包含指定的用户文本，用于区分根与子 Agent 请求。
    fn request_contains_user_text(request: &Value, expected: &str) -> bool {
        request["input"].as_array().is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"] == "user"
                    && message["content"].as_array().is_some_and(|content| {
                        content
                            .iter()
                            .any(|part| part["text"].as_str() == Some(expected))
                    })
            })
        })
    }

    /// 读取一次带 Content-Length 的本地 JSON 请求正文。
    fn read_json_request(stream: &mut TcpStream) -> Result<Value, String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("设置本地模型读取超时失败：{error}"))?;
        let mut wire = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            if let Some(position) = wire.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let count = stream
                .read(&mut buffer)
                .map_err(|error| format!("读取本地模型请求头失败：{error}"))?;
            if count == 0 {
                return Err("本地模型请求头提前结束".to_owned());
            }
            wire.extend_from_slice(&buffer[..count]);
        };
        let head = std::str::from_utf8(&wire[..header_end])
            .map_err(|error| format!("本地模型请求头不是 UTF-8：{error}"))?;
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>())
            })
            .ok_or_else(|| "本地模型请求缺少 Content-Length".to_owned())?
            .map_err(|error| format!("Content-Length 无效：{error}"))?;
        while wire.len().saturating_sub(header_end) < content_length {
            let count = stream
                .read(&mut buffer)
                .map_err(|error| format!("读取本地模型请求正文失败：{error}"))?;
            if count == 0 {
                return Err("本地模型请求正文提前结束".to_owned());
            }
            wire.extend_from_slice(&buffer[..count]);
        }
        serde_json::from_slice(&wire[header_end..header_end + content_length])
            .map_err(|error| format!("本地模型请求正文不是 JSON：{error}"))
    }

    /// 创建绑定本地 Responses Provider 和默认模型的桌面 Runtime。
    fn runtime_with_responses_provider(
        storage_root: &Path,
        base_url: &str,
        models: &[&str],
    ) -> Arc<AgentRuntime> {
        runtime_with_responses_capabilities(storage_root, base_url, models, None)
    }

    /// 记录 Provider HTTP 边界会话标识与用途的测试观测器。
    #[derive(Default)]
    struct SessionRecordingObserver {
        /// 按同步回调到达顺序保存的 (session_id, purpose)。
        seen: StdMutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl SessionRecordingObserver {
        /// 返回当前全部观测的副本。
        fn observations(&self) -> Vec<(Option<String>, Option<String>)> {
            self.seen.lock().expect("观测锁不应损坏").clone()
        }
    }

    impl keencode_provider::RequestObserver for SessionRecordingObserver {
        fn on_request(&self, observation: keencode_provider::RequestObservation) {
            self.seen
                .lock()
                .expect("观测锁不应损坏")
                .push((observation.session_id, observation.purpose));
        }
    }

    /// 创建具有明确中立能力快照的测试 Runtime，避免把默认模型能力冒充原生 Schema 支持。
    fn runtime_with_responses_capabilities(
        storage_root: &Path,
        base_url: &str,
        models: &[&str],
        capabilities: Option<ProviderCapabilities>,
    ) -> Arc<AgentRuntime> {
        runtime_with_responses_registry(
            keencode_provider::ProviderRegistry::new(),
            storage_root,
            base_url,
            models,
            capabilities,
        )
    }

    /// 用调用方提供的注册表创建测试 Runtime，以便安装请求观测器断言出站请求事实。
    fn runtime_with_responses_registry(
        registry: keencode_provider::ProviderRegistry,
        storage_root: &Path,
        base_url: &str,
        models: &[&str],
        capabilities: Option<ProviderCapabilities>,
    ) -> Arc<AgentRuntime> {
        let mut config = ProviderConfig::new_unauthenticated(
            "provider-runtime-test",
            keencode_model::ProviderProtocol::Responses,
            base_url,
        )
        .expect("测试 Provider 配置应有效");
        config.response_mode = WireResponseMode::Buffered;
        if let Some(capabilities) = capabilities {
            // 生产装配会为已登记模型写入精确能力覆盖；测试夹具必须保持同一优先级，
            // 否则 Runtime 的模型能力读取会错误地退回默认快照。
            config.default_capabilities = capabilities.clone();
            config.model_capabilities.extend(
                models
                    .iter()
                    .map(|model| ((*model).to_owned(), capabilities.clone())),
            );
        }
        let snapshot = registry
            .replace_all([ProviderRegistration::new(
                config,
                "Runtime 测试 Provider",
                "test-revision",
                ProviderModelPolicy::Enumerated {
                    models: models.iter().map(|model| (*model).to_owned()).collect(),
                },
            )
            .expect("测试 Provider 注册项应有效")])
            .expect("测试 Provider 注册表应替换");
        let runtime = Arc::new(
            AgentRuntime::new_with_registry(storage_root, registry, test_executor_handle())
                .expect("测试 Runtime 应创建"),
        );
        *runtime
            .default_provider
            .write()
            .expect("默认 Provider 锁应读取") = Some(super::DefaultProviderBinding {
            provider_id: "provider-runtime-test".to_owned(),
            model: models[0].to_string(),
            generation: snapshot.generation,
        });
        runtime
    }

    /// 子 Agent 复合模型引用必须覆盖 Session Provider，并拆出精确模型标识。
    #[test]
    fn child_agent_model_override_resolves_explicit_provider_and_model() {
        let registry = keencode_provider::ProviderRegistry::new();
        let registration = |provider_id: &str, model: &str| {
            ProviderRegistration::new(
                ProviderConfig::new_unauthenticated(
                    provider_id,
                    keencode_model::ProviderProtocol::Responses,
                    "https://example.com/v1",
                )
                .expect("测试 Provider 配置应有效"),
                format!("{provider_id} 测试 Provider"),
                "test-revision",
                ProviderModelPolicy::Enumerated {
                    models: vec![model.to_owned()],
                },
            )
            .expect("测试 Provider 注册项应有效")
        };
        registry
            .replace_all([
                registration("provider-a", "model-a"),
                registration("provider-b", "model-b"),
            ])
            .expect("双 Provider 注册表应替换");
        let session_provider = provider_snapshot(
            &registry
                .resolve("provider-a", "model-a")
                .expect("Session Provider 应解析"),
        );
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let runtime =
            AgentRuntime::new_with_registry(storage.path(), registry, test_executor_handle())
                .expect("测试 Runtime 应创建");

        let inherited = runtime
            .resolve_child_agent_provider(Some(&session_provider), "model-a")
            .expect("普通子 Agent 模型应沿用 Session Provider");
        assert_eq!(inherited.provider_id(), "provider-a");
        assert_eq!(inherited.model(), "model-a");

        let overridden = runtime
            .resolve_child_agent_provider(Some(&session_provider), "provider-b::model-b")
            .expect("复合子 Agent 模型应解析覆盖 Provider");
        assert_eq!(overridden.provider_id(), "provider-b");
        assert_eq!(overridden.model(), "model-b");
    }

    /// 子 Agent 指定模型被删除时回退主对话模型；可解析的指定模型即使能力不同也保持用户指定。
    #[test]
    fn child_agent_model_falls_back_to_session_provider() {
        let registry = keencode_provider::ProviderRegistry::new();
        let registration = |provider_id: &str, model: &str, image_input: bool| {
            let mut config = ProviderConfig::new_unauthenticated(
                provider_id,
                keencode_model::ProviderProtocol::Responses,
                "https://example.com/v1",
            )
            .expect("测试 Provider 配置应有效");
            config.default_capabilities = ProviderCapabilities {
                image_input,
                ..ProviderCapabilities::default()
            };
            ProviderRegistration::new(
                config,
                format!("{provider_id} 测试 Provider"),
                "test-revision",
                ProviderModelPolicy::Enumerated {
                    models: vec![model.to_owned()],
                },
            )
            .expect("测试 Provider 注册项应有效")
        };
        registry
            .replace_all([
                registration("provider-a", "model-a", true),
                registration("provider-b", "model-b", true),
                registration("provider-c", "model-c", false),
            ])
            .expect("三 Provider 注册表应替换");
        let session_provider = provider_snapshot(
            &registry
                .resolve("provider-a", "model-a")
                .expect("Session Provider 应解析"),
        );
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let runtime =
            AgentRuntime::new_with_registry(storage.path(), registry, test_executor_handle())
                .expect("测试 Runtime 应创建");

        let missing = runtime
            .resolve_child_agent_provider(Some(&session_provider), "provider-b::model-missing")
            .expect("模型被删除时应回退主对话模型");
        assert_eq!(missing.provider_id(), "provider-a");
        assert_eq!(missing.model(), "model-a");

        let no_vision = runtime
            .resolve_child_agent_provider(Some(&session_provider), "provider-c::model-c")
            .expect("可解析的用户指定模型应保持");
        assert_eq!(no_vision.provider_id(), "provider-c");
        assert_eq!(no_vision.model(), "model-c");

        let vision_override = runtime
            .resolve_child_agent_provider(Some(&session_provider), "provider-b::model-b")
            .expect("满足视觉要求的用户指定模型应保留");
        assert_eq!(vision_override.provider_id(), "provider-b");
        assert_eq!(vision_override.model(), "model-b");
    }

    /// 子 Agent 复合模型引用必须拒绝空段、边界空白和多余分隔符。
    #[test]
    fn child_agent_model_override_rejects_malformed_references() {
        for reference in [
            "::model-b",
            "provider-b::",
            " provider-b::model-b",
            "provider-b::model-b ",
            "provider-b::model-b::extra",
        ] {
            assert_eq!(
                split_child_agent_model_override(reference),
                Err(AgentRuntimeError::RuntimeOperationFailed),
                "应拒绝无效模型引用：{reference}"
            );
        }
        assert_eq!(split_child_agent_model_override("model-a").unwrap(), None);
        assert_eq!(
            split_child_agent_model_override("provider-b::model-b").unwrap(),
            Some(("provider-b", "model-b"))
        );
    }

    /// 桌面通知只提取根用户 Turn 的实时终态，并保留结构化失败原因。
    #[test]
    fn authoritative_runtime_turn_outcomes_preserve_terminal_semantics() {
        let session_id = ResourceSessionId::new("session-authoritative-outcome").unwrap();
        let mut state = SessionState::empty(session_id);
        let root_agent_id = ResourceAgentId::new("root").unwrap();
        let root_turn_id = ResourceTurnId::new("turn-root-failed").unwrap();
        state.turns.insert(
            root_turn_id.clone(),
            TurnState {
                turn_id: root_turn_id.clone(),
                source_agent_id: root_agent_id,
                root_turn_id: root_turn_id.clone(),
                parent_turn_id: None,
                prompt_summary: "失败恢复".to_owned(),
                started_at_unix_ms: 1,
                completed_at_unix_ms: Some(2),
                status: TurnStatus::Failed,
                stop_reason: Some(TurnStopReason::Failed),
                outcome_message: Some("权威失败说明".to_owned()),
            },
        );
        assert_eq!(
            authoritative_recovered_turn_outcome(
                &state,
                &keencode_agent::AgentId::new("root").unwrap(),
                &keencode_agent::TurnId::new("turn-root-failed").unwrap(),
                None,
            )
            .unwrap(),
            Some(keencode_agent::AgentTurnOutcome::Failed {
                message: "权威失败说明".to_owned(),
            })
        );

        let child_agent_id = ResourceAgentId::new("agent-child").unwrap();
        let child_turn_id = ResourceTurnId::new("turn-child-completed").unwrap();
        state.turns.insert(
            child_turn_id.clone(),
            TurnState {
                turn_id: child_turn_id.clone(),
                source_agent_id: child_agent_id.clone(),
                root_turn_id,
                parent_turn_id: Some(ResourceTurnId::new("turn-parent").unwrap()),
                prompt_summary: "完成恢复".to_owned(),
                started_at_unix_ms: 3,
                completed_at_unix_ms: Some(4),
                status: TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        state.sub_agents.insert(
            child_agent_id.clone(),
            SubAgentState {
                agent_id: child_agent_id,
                parent_agent_id: ResourceAgentId::new("root").unwrap(),
                agent_path: "/root/child".to_owned(),
                task: "完成子任务".to_owned(),
                status: SubAgentStatus::Completed,
                current_turn_id: Some(child_turn_id),
                result_summary: Some("子任务完成摘要".to_owned()),
            },
        );
        assert_eq!(
            authoritative_recovered_turn_outcome(
                &state,
                &keencode_agent::AgentId::new("agent-child").unwrap(),
                &keencode_agent::TurnId::new("turn-child-completed").unwrap(),
                None,
            )
            .unwrap(),
            Some(keencode_agent::AgentTurnOutcome::Completed {
                final_message: Some("子任务完成摘要".to_owned()),
            })
        );
    }

    /// 取消已提交而 Runner 终态先落 Journal 时，冷恢复必须沿用在线取消语义。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_checkpoint_normalizes_completed_journal_turn_to_interrupted() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "cancelling-journal-race")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let root_turn =
            AgentTurnId::new("turn-cancelling-journal-root").expect("根 Turn 标识应有效");
        let root_prompt = "保持父 Runtime Turn 运行";
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                root_prompt,
                PlanGuard::inactive(),
            )
            .expect("根 Collaboration Turn 应启动");
        let child_request = test_spawn_request("cancelling_journal_child", project.path());
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-cancelling-journal-child").expect("spawn 调用标识应有效"),
                child_request.clone(),
            )
            .expect("子 Agent 应创建");

        let root_provider_gate = Arc::new(tokio::sync::Notify::new());
        let root_provider_entered = Arc::new(AtomicBool::new(false));
        let root_provider = Arc::new(GateProvider {
            inner: Arc::new(ScriptedProvider::new(
                ProviderCapabilities::default(),
                [completed_reply("父 Runtime Turn 完成")],
            )),
            entered: Arc::clone(&root_provider_entered),
            gate: Arc::clone(&root_provider_gate),
        });
        let root_input = ModelMessage::text(MessageRole::User, root_prompt);
        let root_request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Session 标识应有效"),
            root_turn.clone(),
            collaboration.root_agent_id.clone(),
            "test-model",
            vec![root_input.clone()],
            PlanGuard::inactive(),
        );
        let root_session = session.clone();
        let root_task = tokio::spawn(async move {
            root_session
                .bind_agent_runner(AgentRunner::new(
                    root_provider,
                    ToolRegistry::new(),
                    RunLimits::default(),
                ))
                .run_turn(RuntimeTurnRequest::root(
                    root_request,
                    vec![root_input],
                    root_turn_summary(root_prompt, None, false),
                ))
                .await
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !root_provider_entered.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "根 Runtime Turn 应进入模型采样");
            tokio::task::yield_now().await;
        }

        let child_input = ModelMessage::text(MessageRole::User, &child_request.initial_task);
        let child_runtime_request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Session 标识应有效"),
            child.initial_turn_id.clone(),
            child.agent.agent_id.clone(),
            "test-model",
            vec![child_input.clone()],
            PlanGuard::inactive(),
        );
        let child_result = session
            .bind_agent_runner(AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities::default(),
                    [completed_reply("子 Runtime Turn 已完成")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(RuntimeTurnRequest::initial_child(
                child_runtime_request,
                vec![child_input],
                root_turn.as_str(),
                root_turn.as_str(),
                child_request.initial_task.clone(),
                SubAgentState {
                    agent_id: ResourceAgentId::new(child.agent.agent_id.as_str().to_owned())
                        .expect("资源子 Agent 标识应有效"),
                    parent_agent_id: ResourceAgentId::new(
                        collaboration.root_agent_id.as_str().to_owned(),
                    )
                    .expect("资源父 Agent 标识应有效"),
                    agent_path: child.agent.path.as_str().to_owned(),
                    task: child_request.initial_task,
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            ))
            .await
            .expect("子 Runtime Turn 应提交 Journal 终态");
        assert!(child_result.is_success());
        collaboration
            .coordinator
            .cancel_turn(&child.agent.agent_id, &child.initial_turn_id)
            .expect("子 Collaboration Turn 应进入 Cancelling");
        let checkpoint = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("真实 Cancelling checkpoint 应读取");
        let journal = session.snapshot().expect("真实 Journal 快照应读取");
        let resource_child_turn =
            ResourceTurnId::new(child.initial_turn_id.as_str().to_owned()).unwrap();
        assert_eq!(
            journal.state.turns[&resource_child_turn].status,
            TurnStatus::Completed
        );
        assert!(matches!(
            super::recovered_agent_for_id(&checkpoint, &child.agent.agent_id)
                .expect("checkpoint 应包含子 Agent")
                .status,
            CollaborationAgentStatus::Cancelling { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));

        let outcomes = recovered_authoritative_turn_outcomes(Some(&checkpoint), &journal.state)
            .expect("Journal 与 checkpoint 应完成严格对账");
        assert_eq!(
            outcomes.get(&child.initial_turn_id),
            Some(&AgentTurnOutcome::Interrupted)
        );
        let mut closing_checkpoint = checkpoint.clone();
        closing_checkpoint.roots[0].lifecycle = RecoveredRootLifecycle::Closing;
        for recovered_agent in &mut closing_checkpoint.roots[0].agents {
            let turn_id = recovered_agent
                .status
                .active_turn_id()
                .cloned()
                .expect("Closing 测试中的 Agent 应保持未决 Turn");
            recovered_agent.status = CollaborationAgentStatus::Cancelling { turn_id };
        }
        assert!(
            recovered_authoritative_turn_outcomes(Some(&closing_checkpoint), &journal.state)
                .expect("Closing checkpoint 仍应严格校验已有 Journal 记录")
                .is_empty(),
            "Closing 根树不得向 Coordinator 提供权威终态"
        );

        runtime
            .collaboration_sessions
            .lock()
            .expect("测试 Collaboration 表应可写")
            .remove(&session_id);
        let root_agent_id = collaboration.root_agent_id.clone();
        drop(collaboration);
        let recovered_store = Arc::new(
            SessionCollaborationStore::new(storage.path(), &session_id)
                .expect("冷恢复 Store 应创建"),
        );
        recovered_store
            .bind_runtime_session(&session)
            .expect("冷恢复 Store 应绑定 Session");
        let restored = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("测试容量应有效"),
            recovered_store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        restored
            .restore_coordinator_with_authoritative_outcomes(checkpoint, &outcomes)
            .expect("取消竞态应按 Interrupted 冷恢复");
        assert!(matches!(
            restored.agent_status(&child.agent.agent_id).unwrap(),
            CollaborationAgentStatus::Interrupted { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));
        let child_completion = restored
            .mailbox(&root_agent_id)
            .expect("父 mailbox 应读取")
            .into_iter()
            .find(|message| message.source_agent_id == child.agent.agent_id)
            .expect("恢复应生成唯一子 Turn 终态通知");
        assert!(matches!(
            child_completion.kind,
            keencode_agent::MailboxMessageKind::ChildTurnFinished {
                outcome: AgentTurnOutcome::Interrupted
            }
        ));

        root_provider_gate.notify_one();
        root_task
            .await
            .expect("根 Runtime 任务不应 panic")
            .expect("根 Runtime Turn 应完成");
        drop(restored);
        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }

    /// Collaboration 已保存终态但 Journal 缺少对应 Turn 时，冷启动必须要求恢复而不能静默接受。
    #[test]
    fn terminal_collaboration_checkpoint_without_journal_is_recovery_required() {
        let storage = tempfile::tempdir().expect("应创建 Collaboration 存储目录");
        let directory =
            keencode_resources::ensure_project_storage(storage.path(), "/test-project").unwrap();
        keencode_resources::register_session_location(
            storage.path(),
            &ResourceSessionId::new("session-terminal-missing-journal").unwrap(),
            &directory,
        )
        .unwrap();
        let store = Arc::new(
            SessionCollaborationStore::new(storage.path(), "session-terminal-missing-journal")
                .expect("测试 Store 应创建"),
        );
        let coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("测试容量应有效"),
            store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        coordinator
            .register_root_with_id(
                root_agent_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new("session-terminal-missing-journal")
                        .expect("Agent Session 标识应有效"),
                    profile: AgentProfile {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                        cwd: storage.path().to_path_buf(),
                        worktree_lease: None,
                        tool_snapshot: vec!["Read".to_owned()],
                    },
                    per_root_turn_limit: 2,
                },
            )
            .expect("根 Agent 应注册");
        let turn_id =
            keencode_agent::TurnId::new("terminal-missing-journal-turn").expect("Turn 标识应有效");
        coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                turn_id.clone(),
                "Journal 起点丢失",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应入队");
        coordinator
            .complete_turn(
                &root_agent_id,
                &turn_id,
                keencode_agent::AgentTurnOutcome::Completed {
                    final_message: Some("已完成但 Journal 丢失".to_owned()),
                },
            )
            .expect("协作终态应提交");
        let checkpoint = coordinator
            .checkpoint_coordinator()
            .expect("终态 checkpoint 应读取");
        let state = SessionState::empty(
            ResourceSessionId::new("session-terminal-missing-journal")
                .expect("资源 Session 标识应有效"),
        );

        assert_eq!(
            recovered_authoritative_turn_outcomes(Some(&checkpoint), &state),
            Err(AgentRuntimeError::RecoveryRequired)
        );
    }

    /// Runtime 选择动态输入 ack 终态时释放活跃 Turn，并保留两类 claim 供冷恢复确认。
    #[test]
    fn pending_dynamic_input_runtime_completion_releases_turn_and_preserves_claims() {
        let storage = tempfile::tempdir().expect("应创建 Collaboration 存储目录");
        let directory =
            keencode_resources::ensure_project_storage(storage.path(), "/test-project").unwrap();
        keencode_resources::register_session_location(
            storage.path(),
            &ResourceSessionId::new("session-pending-dynamic-input").unwrap(),
            &directory,
        )
        .unwrap();
        let session_id = "session-pending-dynamic-input";
        let store = Arc::new(
            SessionCollaborationStore::new(storage.path(), session_id).expect("测试 Store 应创建"),
        );
        let coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("测试容量应有效"),
            store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        coordinator
            .register_root_with_id(
                root_agent_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new(session_id)
                        .expect("Agent Session 标识应有效"),
                    profile: AgentProfile {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                        cwd: storage.path().to_path_buf(),
                        worktree_lease: None,
                        tool_snapshot: vec!["Read".to_owned()],
                    },
                    per_root_turn_limit: 2,
                },
            )
            .expect("根 Agent 应注册");
        let turn_id =
            keencode_agent::TurnId::new("pending-dynamic-input-turn").expect("Turn 标识应有效");
        coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                turn_id.clone(),
                "动态输入 ack 失败",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        coordinator
            .steer_active_agent_with_operation(
                &root_agent_id,
                &keencode_agent::ToolCallId::new("mirror-resource-steer").unwrap(),
                "待恢复 steer",
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@local".into(),
                }],
            )
            .expect("用户 steer 应排队");
        let steers = coordinator
            .consume_user_steers(&root_agent_id, &turn_id)
            .expect("用户 steer claim 应建立");
        coordinator
            .send_message(
                &root_agent_id,
                &turn_id,
                &keencode_agent::ToolCallId::new("mailbox-claim")
                    .expect("mailbox ToolCall 标识应有效"),
                &root_agent_id,
                "待恢复 mailbox",
            )
            .expect("mailbox 消息应排队");
        let mailbox = coordinator
            .consume_mailbox(&root_agent_id, &turn_id, 1)
            .expect("mailbox claim 应建立");
        assert!(
            coordinator_has_pending_dynamic_input_claim(&coordinator, &root_agent_id, &turn_id,)
                .expect("当前 Turn 的动态 claim 应可查询")
        );
        assert!(
            !coordinator_has_pending_dynamic_input_claim(
                &coordinator,
                &root_agent_id,
                &keencode_agent::TurnId::new("other-turn").expect("其他 Turn 标识应有效"),
            )
            .expect("其他 Turn 的动态 claim 应可查询")
        );

        complete_runtime_turn(
            &coordinator,
            &root_agent_id,
            &turn_id,
            AgentTurnOutcome::Failed {
                message: "动态输入确认未完成".to_owned(),
            },
            true,
        )
        .expect("Runtime 专用终态应提交");
        assert_eq!(coordinator.capacity().unwrap().global_in_use, 0);
        assert!(matches!(
            coordinator.agent_status(&root_agent_id).unwrap(),
            CollaborationAgentStatus::Failed { turn_id: ref failed_turn, .. }
                if failed_turn == &turn_id
        ));

        let checkpoint = coordinator
            .checkpoint_coordinator()
            .expect("失败 Agent checkpoint 应读取");
        let agent = checkpoint
            .roots
            .iter()
            .flat_map(|root| root.agents.iter())
            .find(|agent| agent.definition.agent_id == root_agent_id)
            .expect("根 Agent checkpoint 应存在");
        assert_eq!(agent.mailbox_claim_turn_id, Some(turn_id.clone()));
        assert_eq!(
            agent.mailbox_claim_through_sequence,
            Some(mailbox[0].sequence)
        );
        assert_eq!(agent.steer_claim_turn_id, Some(turn_id.clone()));
        assert_eq!(agent.steer_claim_through_sequence, Some(steers[0].sequence));

        let restored = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("恢复容量应有效"),
            Arc::new(
                SessionCollaborationStore::new(storage.path(), session_id)
                    .expect("恢复 Store 应创建"),
            ),
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        restored
            .restore_coordinator(checkpoint)
            .expect("失败 Turn 和未确认 claim 应可冷恢复");
        restored
            .acknowledge_mailbox(&root_agent_id, &turn_id, mailbox[0].sequence)
            .expect("恢复后 mailbox claim 应可确认");
        restored
            .acknowledge_user_steers(&root_agent_id, &turn_id, steers[0].sequence)
            .expect("恢复后 steer claim 应可确认");
    }

    /// 终态提交只允许重试明确可恢复错误，并严格受次数、时限和取消状态约束。
    #[test]
    fn runtime_turn_completion_retry_policy_is_bounded_and_fail_closed() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let store_error = CollaborationError::Store {
            message: "测试 Store 暂时不可用".to_owned(),
        };
        let pending_error = CollaborationError::CommittedExecutionPending {
            message: "测试后置动作待收敛".to_owned(),
        };
        let recovery_error = CollaborationError::StoreRecoveryRequired {
            message: "测试 Store 水位冲突".to_owned(),
        };

        assert!(is_retryable_runtime_turn_completion_error(&store_error));
        assert!(is_retryable_runtime_turn_completion_error(&pending_error));
        assert!(!is_retryable_runtime_turn_completion_error(&recovery_error));
        assert!(should_retry_runtime_turn_completion(
            &store_error,
            1,
            now,
            deadline,
            false,
        ));
        assert!(should_retry_runtime_turn_completion(
            &store_error,
            RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS - 1,
            now,
            deadline,
            false,
        ));
        assert!(!should_retry_runtime_turn_completion(
            &store_error,
            RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS,
            now,
            deadline,
            false,
        ));
        assert!(!should_retry_runtime_turn_completion(
            &store_error,
            1,
            deadline,
            deadline,
            false,
        ));
        assert!(!should_retry_runtime_turn_completion(
            &store_error,
            1,
            now,
            deadline,
            true,
        ));
        assert!(!should_retry_runtime_turn_completion(
            &recovery_error,
            1,
            now,
            deadline,
            false,
        ));
    }

    /// 终态回传失败时清除运行态并保留 accepted 护栏，成功时才完全释放 Turn 标识。
    #[test]
    fn failed_runtime_turn_release_keeps_accepted_guard() {
        let turn_id = keencode_agent::TurnId::new("turn-runtime-completion-failed")
            .expect("测试 Turn 标识应有效");
        let state = std::sync::Mutex::new(super::RuntimeAgentExecutionState::default());
        let idle = std::sync::Condvar::new();
        {
            let mut state = state.lock().expect("执行状态锁应可用");
            state.accepted_turns.insert(turn_id.clone());
            state.running_turns.insert(
                turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效"),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "失败执行".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: TurnCancellation::new(),
                    terminal_outcome: Some(AgentTurnOutcome::Interrupted),
                },
            );
        }

        release_runtime_turn_state(&state, &idle, &turn_id, false);
        let state_after_failure = state.lock().expect("失败后的执行状态应可读取");
        assert!(!state_after_failure.running_turns.contains_key(&turn_id));
        assert!(state_after_failure.accepted_turns.contains(&turn_id));
        drop(state_after_failure);

        release_runtime_turn_state(&state, &idle, &turn_id, true);
        assert!(
            !state
                .lock()
                .expect("成功后的执行状态应可读取")
                .accepted_turns
                .contains(&turn_id)
        );
    }

    /// 动态输入确认故障测试使用的始终失败确认器。
    struct FailingDynamicInputAcknowledgement;

    impl AgentDynamicInputAcknowledgement for FailingDynamicInputAcknowledgement {
        /// 固定返回确认失败，模拟正文已经写入后外部 claim ack 不可用。
        fn acknowledge(&self) -> Result<(), AgentDynamicInputError> {
            Err(AgentDynamicInputError::new("测试动态输入 ack 失败"))
        }
    }

    /// 向 Runtime Runner 注入一条带真实 marker 与 receipt 的固定动态输入。
    struct FixedDynamicInputSource {
        /// 只接受预期的 Session 身份。
        session_id: String,
        /// 只接受预期的 Turn 身份。
        turn_id: String,
        /// 只接受预期的 Agent 身份。
        agent_id: String,
        /// 每次 claim 返回的模型可见动态消息。
        message: ModelMessage,
        /// 与消息对应的资源层消费水位。
        receipt: keencode_agent::AgentDynamicInputReceipt,
    }

    impl keencode_agent::AgentDynamicInputSource for FixedDynamicInputSource {
        /// 在身份匹配时返回固定动态输入批次，并让确认阶段稳定失败。
        fn claim(
            &self,
            session_id: &keencode_agent::SessionId,
            turn_id: &keencode_agent::TurnId,
            source_agent_id: &keencode_agent::AgentId,
            _boundary: keencode_agent::AgentDynamicInputBoundary,
            _maximum: usize,
        ) -> Result<keencode_agent::AgentDynamicInputBatch, keencode_agent::AgentDynamicInputError>
        {
            if session_id.as_str() != self.session_id
                || turn_id.as_str() != self.turn_id
                || source_agent_id.as_str() != self.agent_id
            {
                return Err(keencode_agent::AgentDynamicInputError::new(
                    "测试动态输入身份不匹配",
                ));
            }
            Ok(keencode_agent::AgentDynamicInputBatch::new_with_receipts(
                vec![self.message.clone()],
                vec![self.receipt.clone()],
                Arc::new(FailingDynamicInputAcknowledgement),
            ))
        }
    }

    /// 暂停根模型响应，给测试机会先建立合法的资源层 child 路由。
    struct GateProvider {
        /// 最终响应由测试显式释放，确保可以先建立合法的资源层 mailbox 路由。
        inner: Arc<ScriptedProvider>,
        /// 根 Provider 已进入采样后才允许测试继续推进。
        entered: Arc<AtomicBool>,
        /// 控制根 Provider 返回脚本响应的闸门。
        gate: Arc<tokio::sync::Notify>,
    }

    impl ModelProvider for GateProvider {
        /// 委托脚本 Provider 的能力快照。
        fn capabilities(&self, model: &str) -> ProviderCapabilities {
            self.inner.capabilities(model)
        }

        /// 先等待测试建立 child 资源状态，再返回一次固定模型响应。
        fn stream(
            &self,
            request: ModelRequest,
        ) -> keencode_model::ModelFuture<'_, Result<keencode_model::ModelStream, ModelError>>
        {
            let entered = Arc::clone(&self.entered);
            let gate = Arc::clone(&self.gate);
            let inner = Arc::clone(&self.inner);
            Box::pin(async move {
                entered.store(true, Ordering::SeqCst);
                gate.notified().await;
                inner.stream(request).await
            })
        }
    }

    /// 在最终候选边界注入一封 mailbox，用于验证其必须留给后续用户 Turn。
    struct MailboxArrivingAtFinalCandidateSource {
        /// 复用真实 Runtime mailbox 与两阶段确认实现。
        inner: super::RuntimeDynamicInputSource,
        /// 首次最终候选边界是否已经排队过测试 mailbox。
        mailbox_queued: AtomicBool,
        /// 合法 mailbox 的来源 child Agent。
        mailbox_source_agent_id: keencode_agent::AgentId,
        /// 合法 mailbox 的来源 child Turn。
        mailbox_source_turn_id: keencode_agent::TurnId,
    }

    impl AgentDynamicInputSource for MailboxArrivingAtFinalCandidateSource {
        /// 首次模型采样前保持空邮箱，最终候选期间再排队 mailbox 并委托真实来源。
        fn claim(
            &self,
            session_id: &keencode_agent::SessionId,
            turn_id: &keencode_agent::TurnId,
            source_agent_id: &keencode_agent::AgentId,
            boundary: AgentDynamicInputBoundary,
            maximum: usize,
        ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError> {
            if matches!(boundary, AgentDynamicInputBoundary::BeforeModelSampling)
                && !self.mailbox_queued.load(Ordering::SeqCst)
            {
                return Ok(AgentDynamicInputBatch::empty());
            }
            if matches!(boundary, AgentDynamicInputBoundary::AfterFinalCandidate)
                && !self.mailbox_queued.swap(true, Ordering::SeqCst)
            {
                self.inner
                    .coordinator
                    .send_message(
                        &self.mailbox_source_agent_id,
                        &self.mailbox_source_turn_id,
                        &ToolCallId::new("mailbox-after-final-candidate")
                            .map_err(|_| AgentDynamicInputError::new("测试 mailbox 标识无效"))?,
                        source_agent_id,
                        "最终候选期间到达的 mailbox",
                    )
                    .map_err(|_| AgentDynamicInputError::new("测试 mailbox 无法排队"))?;
            }
            self.inner
                .claim(session_id, turn_id, source_agent_id, boundary, maximum)
        }
    }

    /// 最终候选期间到达的 mailbox 不得重开当前 Turn，而应留给下一用户 Turn 恰好消费一次。
    #[tokio::test(flavor = "current_thread")]
    async fn mailbox_arriving_after_final_candidate_waits_for_next_user_turn() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "mailbox-final-candidate")
            .expect("测试 Session 应创建");
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        let first_turn =
            keencode_agent::TurnId::new("mailbox-final-first").expect("首个 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &agent_id,
                first_turn.clone(),
                "验证最终候选 mailbox",
                PlanGuard::inactive(),
            )
            .expect("首个根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &agent_id,
                &first_turn,
                &ToolCallId::new("spawn-mailbox-final-candidate")
                    .expect("子 Agent 工具调用标识应有效"),
                test_spawn_request("mailbox_final_candidate_source", project.path()),
            )
            .expect("mailbox 来源 child 应创建");
        let source = Arc::new(MailboxArrivingAtFinalCandidateSource {
            inner: super::RuntimeDynamicInputSource {
                store: Arc::clone(&collaboration.store),
                session_id: session.session_id().as_str().to_owned(),
                coordinator: Arc::clone(&collaboration.coordinator),
                session: session.clone(),
            },
            mailbox_queued: AtomicBool::new(false),
            mailbox_source_agent_id: child.agent.agent_id.clone(),
            mailbox_source_turn_id: child.initial_turn_id.clone(),
        });
        let first_provider_script = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("父 Turn 应完成")],
        ));
        let first_provider_gate = Arc::new(tokio::sync::Notify::new());
        let first_provider_entered = Arc::new(AtomicBool::new(false));
        let first_provider = Arc::new(GateProvider {
            inner: Arc::clone(&first_provider_script),
            entered: Arc::clone(&first_provider_entered),
            gate: Arc::clone(&first_provider_gate),
        });
        let first_input = ModelMessage::text(MessageRole::User, "首个请求");
        let first_request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            first_turn.clone(),
            agent_id.clone(),
            "test-model",
            vec![first_input.clone()],
            PlanGuard::inactive(),
        );
        let first_session = session.clone();
        let first_source = Arc::clone(&source);
        let first_task = tokio::spawn(async move {
            first_session
                .bind_agent_runner(
                    AgentRunner::new(first_provider, ToolRegistry::new(), RunLimits::default())
                        .with_dynamic_input_source(
                            first_source as Arc<dyn AgentDynamicInputSource>,
                        ),
                )
                .run_turn(RuntimeTurnRequest::root(
                    first_request,
                    vec![first_input],
                    root_turn_summary("首个请求", None, false),
                ))
                .await
        });
        let first_resource_turn =
            ResourceTurnId::new(first_turn.as_str().to_owned()).expect("资源层根 Turn 标识应有效");
        let setup_deadline = Instant::now() + Duration::from_secs(2);
        while !session
            .snapshot()
            .expect("根 Turn 资源快照应读取")
            .state
            .turns
            .contains_key(&first_resource_turn)
        {
            assert!(
                Instant::now() < setup_deadline,
                "根 Turn 应在建立 child 资源前进入 Runtime Journal"
            );
            tokio::task::yield_now().await;
        }
        let child_resource_agent = SubAgentState {
            agent_id: ResourceAgentId::new(child.agent.agent_id.as_str().to_owned())
                .expect("资源层 child Agent 标识应有效"),
            parent_agent_id: ResourceAgentId::new(agent_id.as_str().to_owned())
                .expect("资源层根 Agent 标识应有效"),
            agent_path: child.agent.path.as_str().to_owned(),
            task: "最终候选 mailbox 来源 child".to_owned(),
            status: SubAgentStatus::Pending,
            current_turn_id: None,
            result_summary: None,
        };
        let child_input = ModelMessage::text(MessageRole::User, "建立 mailbox 来源路由");
        let child_request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            child.initial_turn_id.clone(),
            child.agent.agent_id.clone(),
            "test-model",
            vec![child_input.clone()],
            PlanGuard::inactive(),
        );
        let child_result = session
            .bind_agent_runner(AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities::default(),
                    [completed_reply("来源 child 已建立")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(RuntimeTurnRequest::initial_child(
                child_request,
                vec![child_input],
                first_turn.as_str(),
                first_turn.as_str(),
                "建立 mailbox 来源路由",
                child_resource_agent,
            ))
            .await
            .expect("资源层 child Turn 应建立");
        assert!(
            child_result.is_success(),
            "资源层 child Turn 失败：{child_result:?}"
        );
        let setup_deadline = Instant::now() + Duration::from_secs(2);
        while !first_provider_entered.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < setup_deadline,
                "根 Provider 应进入等待闸门"
            );
            tokio::task::yield_now().await;
        }
        first_provider_gate.notify_one();
        let first_result = first_task
            .await
            .expect("首个 Runtime Turn 任务不应 panic")
            .expect("首个 Runtime Turn 应完成");
        assert!(
            first_result.is_success(),
            "首个 Turn 失败：{:?}",
            first_result.error
        );
        assert_eq!(
            first_provider_script
                .requests()
                .expect("首个 Provider 请求应读取")
                .len(),
            1,
            "最终候选后的 mailbox 不得触发额外模型请求"
        );
        assert_eq!(
            collaboration
                .coordinator
                .mailbox(&agent_id)
                .expect("首个 Turn 后 mailbox 应读取")
                .len(),
            1,
            "最终候选期间到达的 mailbox 必须留在 Coordinator"
        );
        complete_runtime_turn(
            &collaboration.coordinator,
            &agent_id,
            &first_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("父 Turn 应完成".to_owned()),
            },
            false,
        )
        .expect("首个 Coordinator Turn 应完成");

        let second_turn =
            keencode_agent::TurnId::new("mailbox-final-next").expect("后续 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &agent_id,
                second_turn.clone(),
                "消费保留 mailbox",
                PlanGuard::inactive(),
            )
            .expect("后续根 Turn 应启动");
        let second_provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("mailbox 已处理")],
        ));
        let second_input = ModelMessage::text(MessageRole::User, "后续用户请求");
        let second_request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            second_turn.clone(),
            agent_id.clone(),
            "test-model",
            vec![second_input.clone()],
            PlanGuard::inactive(),
        );
        let second_result = session
            .bind_agent_runner(
                AgentRunner::new(
                    second_provider.clone(),
                    ToolRegistry::new(),
                    RunLimits::default(),
                )
                .with_dynamic_input_source(Arc::clone(&source) as Arc<dyn AgentDynamicInputSource>),
            )
            .run_turn(RuntimeTurnRequest::root(
                second_request,
                vec![second_input],
                root_turn_summary("后续用户请求", None, false),
            ))
            .await
            .expect("后续 Runtime Turn 应完成");
        assert!(
            second_result.is_success(),
            "后续 Turn 失败：{:?}",
            second_result.error
        );
        let second_requests = second_provider
            .requests()
            .expect("后续 Provider 请求应读取");
        assert_eq!(second_requests.len(), 1, "后续 Turn 只能发起一次模型请求");
        assert_eq!(
            second_requests[0]
                .messages
                .iter()
                .filter(|message| {
                    message.content.iter().any(|content| {
                        matches!(content, keencode_model::ContentBlock::Text { text }
                            if text.contains("最终候选期间到达的 mailbox"))
                    })
                })
                .count(),
            1,
            "后续用户 Turn 必须只注入一份 mailbox"
        );
        let mailbox_input = second_requests[0]
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match content {
                keencode_model::ContentBlock::Text { text }
                    if text.contains("最终候选期间到达的 mailbox") =>
                {
                    Some(text)
                }
                _ => None,
            })
            .expect("后续模型请求应包含 mailbox 动态输入");
        assert!(mailbox_input.contains("from_path=/root/mailbox_final_candidate_source"));
        assert!(
            !mailbox_input.contains(child.agent.agent_id.as_str()),
            "模型可见 mailbox 不能暴露内部 AgentId"
        );
        assert!(
            collaboration
                .coordinator
                .mailbox(&agent_id)
                .expect("后续 Turn 后 mailbox 应读取")
                .is_empty(),
            "mailbox 必须恰好消费一次"
        );
        let snapshot = session.snapshot().expect("动态输入状态应读取");
        assert_eq!(snapshot.state.dynamic_input_receipts.len(), 1);
        assert_eq!(
            snapshot
                .state
                .mailbox
                .values()
                .filter(|message| message.state == MailboxState::Delivered)
                .count(),
            1,
            "Runtime Journal 必须保留一条已投递 mailbox 证据"
        );
        complete_runtime_turn(
            &collaboration.coordinator,
            &agent_id,
            &second_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("mailbox 已处理".to_owned()),
            },
            false,
        )
        .expect("后续 Coordinator Turn 应完成");
    }

    /// 在真实 Runtime 动态输入 claim 建立后注入一次 Journal mailbox 镜像故障。
    struct JournalMirrorFaultDynamicInputSource {
        /// 复用生产动态输入来源，测试只负责在其副作用前设置一次故障。
        inner: super::RuntimeDynamicInputSource,
    }

    impl keencode_agent::AgentDynamicInputSource for JournalMirrorFaultDynamicInputSource {
        /// 让真实 claim 消费路径遇到一次性 Journal 追加故障。
        fn claim(
            &self,
            session_id: &keencode_agent::SessionId,
            turn_id: &keencode_agent::TurnId,
            source_agent_id: &keencode_agent::AgentId,
            boundary: keencode_agent::AgentDynamicInputBoundary,
            maximum: usize,
        ) -> Result<keencode_agent::AgentDynamicInputBatch, keencode_agent::AgentDynamicInputError>
        {
            keencode_resources::test_support::set_append_fault(
                keencode_resources::test_support::AppendFault::ZeroWrite,
            );
            self.inner
                .claim(session_id, turn_id, source_agent_id, boundary, maximum)
        }
    }

    /// Journal mailbox 镜像失败仍须以动态输入错误结束 Runner，并释放 Turn 容量而保留 claim。
    #[tokio::test(flavor = "current_thread")]
    async fn runtime_dynamic_input_mirror_failure_releases_turn_and_preserves_claim() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "dynamic-input-mirror-failure")
            .expect("测试 Session 应创建");
        // 先建立资源层同名 Turn；本测试随后直接运行 AgentRunner，避免外层 Runtime
        // 在动态输入之前把一次性 Journal 故障当成 Session 级 RecoveryRequired。
        persist_usage_root_turn(
            &session,
            "turn-dynamic-input-mirror-failure",
            "test-model",
            "动态输入镜像故障",
            1,
        )
        .await;
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let session_id = session.session_id().as_str().to_owned();
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        let turn_id = keencode_agent::TurnId::new("turn-dynamic-input-mirror-failure")
            .expect("测试 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                turn_id.clone(),
                "动态输入镜像故障",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let steer = collaboration
            .coordinator
            .steer_active_agent_with_operation(
                &root_agent_id,
                &ToolCallId::new("mirror-resource-steer").unwrap(),
                "待恢复 steer @proof",
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@local".into(),
                }],
            )
            .expect("用户 steer 应排队");
        collaboration
            .coordinator
            .send_message(
                &root_agent_id,
                &turn_id,
                &ToolCallId::new("dynamic-input-mirror-failure-message")
                    .expect("mailbox ToolCall 标识应有效"),
                &root_agent_id,
                "待镜像 mailbox",
            )
            .expect("mailbox 消息应排队");

        let input = ModelMessage::text(MessageRole::User, "动态输入镜像故障");
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session 标识应有效"),
            turn_id.clone(),
            root_agent_id.clone(),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let runner = AgentRunner::new(
            Arc::new(ScriptedProvider::new(
                ProviderCapabilities::default(),
                [completed_reply("模型不应被调用")],
            )),
            ToolRegistry::new(),
            RunLimits::default(),
        )
        .with_dynamic_input_source(Arc::new(JournalMirrorFaultDynamicInputSource {
            inner: super::RuntimeDynamicInputSource {
                session_id: session_id.clone(),
                store: Arc::clone(&collaboration.store),
                coordinator: Arc::clone(&collaboration.coordinator),
                session: session.clone(),
            },
        }));
        let result = runner.run_turn(request).await;
        keencode_resources::test_support::clear_append_fault();
        assert!(matches!(
            result.error,
            Some(keencode_agent::AgentRunError::DynamicInput { .. })
        ));

        let pending = coordinator_has_pending_dynamic_input_claim(
            &collaboration.coordinator,
            &root_agent_id,
            &turn_id,
        )
        .expect("动态 claim 应可查询");
        assert!(pending);
        complete_runtime_turn(
            &collaboration.coordinator,
            &root_agent_id,
            &turn_id,
            super::runtime_turn_outcome(Ok(result)),
            pending,
        )
        .expect("动态输入错误应走保留 claim 的终态路径");
        assert_eq!(
            collaboration
                .coordinator
                .capacity()
                .expect("Coordinator 容量应可读取")
                .global_in_use,
            0
        );
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&root_agent_id)
                .expect("根 Agent 状态应可读取"),
            CollaborationAgentStatus::Failed { turn_id: ref failed_turn, .. }
                if failed_turn == &turn_id
        ));

        let checkpoint = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("失败后的 checkpoint 应读取");
        let claims = super::recovered_dynamic_input_claims(Some(&checkpoint))
            .expect("失败后的 claim 应可提取");
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().any(|claim| {
            claim.kind == super::DynamicInputMarkerKind::Mailbox && claim.turn_id == turn_id
        }));
        assert!(claims.iter().any(|claim| {
            claim.kind == super::DynamicInputMarkerKind::UserSteer
                && claim.through_sequence == steer.sequence
                && claim.user_steers == vec![steer.clone()]
        }));
        assert!(
            session
                .snapshot()
                .expect("镜像失败后的 Session 快照应读取")
                .state
                .mailbox
                .is_empty()
        );

        let restored = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("恢复容量应有效"),
            Arc::new(
                SessionCollaborationStore::new(storage.path(), &session_id)
                    .expect("恢复 Store 应创建"),
            ),
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        restored
            .restore_coordinator(checkpoint)
            .expect("镜像故障后的 claim 应可冷恢复");
        assert_eq!(
            super::recovered_dynamic_input_claims(Some(
                &restored
                    .checkpoint_coordinator()
                    .expect("恢复后的 checkpoint 应读取"),
            ))
            .expect("恢复后的 claim 应可提取")
            .len(),
            2
        );
    }

    /// 用户 Steer 信封是内部消费协议：模型必须看到 marker 水位与引导正文，但它
    /// 不得作为用户发言出现在对话投影里（回归：首行 JSON 曾原样渲染成用户气泡）。
    #[tokio::test(flavor = "current_thread")]
    async fn user_steer_envelope_stays_meta_and_out_of_conversation_projection() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "steer-envelope-meta")
            .expect("测试 Session 应创建");
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let session_id = session.session_id().as_str().to_owned();
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        let turn_id =
            keencode_agent::TurnId::new("turn-steer-envelope-meta").expect("测试 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                turn_id.clone(),
                "steer 信封投影",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let steer = collaboration
            .coordinator
            .steer_active_agent_with_operation(
                &root_agent_id,
                &keencode_agent::ToolCallId::new("envelope-resource-steer").unwrap(),
                "追加引导正文 @proof",
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@local".into(),
                }],
            )
            .expect("用户 steer 应排队");
        let second = collaboration
            .coordinator
            .steer_active_agent_with_operation(
                &root_agent_id,
                &keencode_agent::ToolCallId::new("envelope-other-market").unwrap(),
                "第二条引导 @proof",
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@other".into(),
                }],
            )
            .unwrap();
        collaboration
            .coordinator
            .consume_user_steers(&root_agent_id, &turn_id)
            .unwrap();
        let checkpoint = collaboration.coordinator.checkpoint_coordinator().unwrap();
        let claim = super::recovered_dynamic_input_claims(Some(&checkpoint))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(claim.user_steers, vec![steer.clone(), second]);

        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("引导已收到")],
        ));
        let input = ModelMessage::text(MessageRole::User, "steer 信封投影");
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session 标识应有效"),
            turn_id.clone(),
            root_agent_id.clone(),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let result = session
            .bind_agent_runner(
                AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
                    .with_dynamic_input_source(Arc::new(super::RuntimeDynamicInputSource {
                        session_id: session_id.clone(),
                        store: Arc::clone(&collaboration.store),
                        coordinator: Arc::clone(&collaboration.coordinator),
                        session: session.clone(),
                    })),
            )
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                root_turn_summary("steer 信封投影", None, false),
            ))
            .await
            .expect("带 steer 的 Turn 应完成");
        assert!(
            result.is_success(),
            "带 steer 的 Turn 失败：{:?}",
            result.error
        );

        let requests = provider.requests().expect("Provider 请求应读取");
        assert_eq!(requests.len(), 1, "带 steer 的 Turn 只应发起一次模型请求");
        assert!(
            requests[0].messages.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(
                        block,
                        keencode_model::ContentBlock::Text { text }
                            if text.contains("追加引导正文")
                                && text.contains(&format!("[sequence={}]", steer.sequence))
                    )
                })
            }),
            "模型请求必须保留 steer 信封的 marker 水位与引导正文"
        );

        let stored = session
            .transcript()
            .expect("权威 Transcript 应读取")
            .into_iter()
            .find(|message| {
                message.content.iter().any(|part| {
                    matches!(
                        part,
                        keencode_resources::MessagePart::Text { text }
                            if text.contains("追加引导正文")
                    )
                })
            })
            .expect("steer 信封应进入权威 Transcript");
        assert!(
            stored.is_meta,
            "steer 信封必须带 is_meta，否则会被投影成用户发言"
        );

        let state = session.snapshot().expect("Session 快照应读取").state;
        assert!(super::validate_dynamic_input_claim(&session, &state, &claim).unwrap());
        let receipt = state.dynamic_input_receipts.last().unwrap();
        assert_eq!(receipt.user_inputs.len(), 2);
        assert_eq!(receipt.user_inputs[0].text, "追加引导正文 @proof");
        assert_eq!(
            receipt.user_inputs[0].references[0].path,
            "plugin://proof@local"
        );
        assert_eq!(
            receipt.user_inputs[1].references[0].path,
            "plugin://proof@other"
        );
        let mut tampered_receipt = state.clone();
        tampered_receipt
            .dynamic_input_receipts
            .last_mut()
            .unwrap()
            .user_inputs[0]
            .references[0]
            .path = "plugin://proof@changed".into();
        assert_eq!(
            super::validate_dynamic_input_claim(&session, &tampered_receipt, &claim),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let mut tampered = claim.clone();
        tampered.user_steers[0].references[0].path = "plugin://proof@wrong".into();
        assert_eq!(
            super::validate_dynamic_input_claim(&session, &state, &tampered),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let mut changed_body = claim.clone();
        changed_body.user_steers[0].content.push_str(" 被改写");
        assert_eq!(
            super::validate_dynamic_input_claim(&session, &state, &changed_body),
            Err(AgentRuntimeError::RecoveryRequired)
        );
    }

    /// 后续根 Turn 启动前必须在 live Coordinator 中完成动态 claim 对账，避免重复消费。
    #[tokio::test(flavor = "multi_thread")]
    async fn live_root_turn_reconciles_pending_dynamic_input_before_rebinding() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let (base_url, server) = spawn_buffered_responses_server("新 Turn 完成");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "live-dynamic-input-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
        let old_turn_id =
            keencode_agent::TurnId::new("turn-live-dynamic-input").expect("旧 Turn 标识应有效");

        // 先写入一个带未确认 steer claim 的 live checkpoint；其动态 Journal 证据稍后
        // 在同一进程的已建立 Coordinator 上补齐，确保测试覆盖 live 而非冷启动恢复。
        let seed_store = Arc::new(
            SessionCollaborationStore::new(storage.path(), &session_id)
                .expect("测试 Collaboration Store 应创建"),
        );
        let seed_coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("测试容量应有效"),
            seed_store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        seed_coordinator
            .register_root_with_id(
                root_agent_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new(session_id.clone())
                        .expect("Agent Session 标识应有效"),
                    profile: AgentProfile {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                        cwd: project.path().to_path_buf(),
                        worktree_lease: None,
                        tool_snapshot: vec!["Read".to_owned()],
                    },
                    per_root_turn_limit: 2,
                },
            )
            .expect("根 Agent 应注册");
        seed_coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                old_turn_id.clone(),
                "旧动态输入 Turn",
                PlanGuard::inactive(),
            )
            .expect("旧根 Turn 应启动");
        let steer = seed_coordinator
            .steer_active_agent_with_operation(
                &root_agent_id,
                &ToolCallId::new("live-resource-steer").unwrap(),
                "旧动态 steer @proof",
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@local".into(),
                }],
            )
            .expect("旧 steer 应排队");
        seed_coordinator
            .consume_user_steers(&root_agent_id, &old_turn_id)
            .expect("旧 steer claim 应建立");
        drop(seed_coordinator);

        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("同一进程 Collaboration 应建立");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&root_agent_id)
                .expect("恢复后的根 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted { ref turn_id } if turn_id == &old_turn_id
        ));
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-live-dynamic-input-blocked",
                    "证据缺失时不应启动",
                    RootTurnOptions::default(),
                )
                .await,
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let checkpoint_without_evidence = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("缺少证据时 live checkpoint 应读取");
        assert_eq!(
            checkpoint_without_evidence.roots[0].agents[0].steer_claim_turn_id,
            Some(old_turn_id.clone())
        );

        let marker = super::DynamicInputMarker {
            schema: super::DYNAMIC_INPUT_MARKER_SCHEMA.to_owned(),
            session_id: session_id.clone(),
            agent_id: root_agent_id.as_str().to_owned(),
            turn_id: old_turn_id.as_str().to_owned(),
            kind: super::DynamicInputMarkerKind::UserSteer,
            through_sequence: steer.sequence,
        };
        let mut dynamic_body = super::dynamic_input_marker_line(&marker).unwrap();
        super::append_user_steer_body(&mut dynamic_body, std::slice::from_ref(&steer));
        let mut dynamic_message = ModelMessage::text(MessageRole::User, dynamic_body);
        dynamic_message.is_meta = true;
        let old_input = ModelMessage::text(MessageRole::User, "旧动态输入 Turn");
        let old_request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session 标识应有效"),
            old_turn_id.clone(),
            root_agent_id.clone(),
            "test-model",
            vec![old_input.clone()],
            PlanGuard::inactive(),
        );
        let old_runner = AgentRunner::new(
            Arc::new(ScriptedProvider::new(
                ProviderCapabilities::default(),
                [completed_reply("模型不应被调用")],
            )),
            ToolRegistry::new(),
            RunLimits::default(),
        )
        .with_dynamic_input_source(Arc::new(FixedDynamicInputSource {
            session_id: session_id.clone(),
            turn_id: old_turn_id.as_str().to_owned(),
            agent_id: root_agent_id.as_str().to_owned(),
            message: dynamic_message,
            receipt: keencode_agent::AgentDynamicInputReceipt::new(
                keencode_agent::AgentDynamicInputKind::UserSteer,
                steer.sequence,
            ),
        }));
        let old_result = session
            .bind_agent_runner(old_runner)
            .run_turn(RuntimeTurnRequest::root(
                old_request,
                vec![old_input],
                root_turn_summary("旧动态输入 Turn", None, false),
            ))
            .await
            .expect("旧 Turn 应在 ack 故障后形成可恢复终态");
        assert!(matches!(
            old_result.error,
            Some(keencode_agent::AgentRunError::DynamicInputAcknowledgement { .. })
        ));

        let before = session.snapshot().expect("旧 Turn Journal 快照应读取");
        assert_eq!(before.state.dynamic_input_receipts.len(), 1);
        assert!(before.state.transcript.iter().any(|record| {
            matches!(
                record,
                TranscriptRecord::SegmentCommitted(segment)
                    if segment.turn_id.as_str() == old_turn_id.as_str()
                        && segment.messages.iter().any(|message| {
                            session
                                .materialize_message(message)
                                .ok()
                                .is_some_and(|materialized| {
                                    materialized.content.iter().any(|content| {
                                        matches!(content, keencode_model::ContentBlock::Text { text } if text.contains("旧动态 steer @proof") && text.contains("plugin://proof@local"))
                                    })
                                })
                        })
            )
        }));
        let checkpoint_before = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("live checkpoint 应读取");
        assert_eq!(
            checkpoint_before.roots[0].agents[0].steer_claim_turn_id,
            Some(old_turn_id.clone())
        );

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-live-dynamic-input-next",
                    "启动下一根 Turn",
                    RootTurnOptions::default(),
                )
                .await
                .expect("下一根 Turn 应在 live 对账后启动"),
            RootTurnStartOutcome::Started
        );
        let _ = finish_responses_server(server);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = session.snapshot().expect("下一根 Turn 快照应读取");
            if snapshot.state.turns.values().any(|turn| {
                turn.turn_id.as_str() == "turn-live-dynamic-input-next"
                    && turn.status == TurnStatus::Completed
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "下一根 Turn 应在测试窗口内完成");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let after = session.snapshot().expect("最终 Session 快照应读取");
        assert_eq!(after.state.dynamic_input_receipts.len(), 1);
        assert_eq!(
            after
                .state
                .transcript
                .iter()
                .filter(|record| {
                    matches!(
                        record,
                        TranscriptRecord::SegmentCommitted(segment)
                            if segment.turn_id.as_str() == old_turn_id.as_str()
                                && segment.messages.iter().any(|message| {
                                    session
                                        .materialize_message(message)
                                        .ok()
                                        .is_some_and(|materialized| {
                                            materialized.content.iter().any(|content| {
                                                matches!(content, keencode_model::ContentBlock::Text { text } if text.contains("旧动态 steer @proof") && text.contains("plugin://proof@local"))
                                            })
                                        })
                                })
                    )
                })
                .count(),
            1
        );
        let checkpoint_after = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("live 对账后的 checkpoint 应读取");
        let root_after = checkpoint_after
            .roots
            .iter()
            .flat_map(|root| root.agents.iter())
            .find(|agent| agent.definition.agent_id == root_agent_id)
            .expect("根 Agent checkpoint 应存在");
        assert!(root_after.steer_claim_turn_id.is_none());
        assert!(root_after.pending_steers.is_empty());

        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }

    /// 生产 Session Store 必须接受执行器报告的普通 Running 中断终态（此处模拟执行器回调）。
    #[test]
    fn production_collaboration_store_accepts_normal_running_interruption() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "normal-interruption-operation")
            .expect("测试 Session 应创建");
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let turn_id =
            keencode_agent::TurnId::new("turn-normal-interruption").expect("测试 Turn 标识应有效");

        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                turn_id.clone(),
                "执行器报告取消",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let completion = collaboration.coordinator.complete_turn(
            &collaboration.root_agent_id,
            &turn_id,
            AgentTurnOutcome::Interrupted,
        );
        assert!(matches!(
            completion,
            Ok(keencode_agent::TurnCompletionDisposition::Committed)
        ));
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("根 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted {
                turn_id: ref status_turn_id,
            }
                if status_turn_id == &turn_id
        ));
        assert!(
            !session
                .snapshot()
                .expect("Runtime Session 快照应读取")
                .recovery_required
        );
        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("生产 Store 快照应读取")
            .expect("生产 Store 快照应存在");
        assert_eq!(
            persisted.commit.checkpoint.last_event_sequence,
            persisted
                .commit
                .batch
                .events
                .last()
                .expect("终态批次应有事件")
                .sequence
        );
    }

    /// 生产 Session Store 必须接受单层子 Agent 执行器报告的 Running 中断终态（此处模拟回调）。
    #[test]
    fn production_collaboration_store_accepts_normal_child_running_interruption() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "normal-child-interruption-operation")
            .expect("测试 Session 应创建");
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let root_turn =
            keencode_agent::TurnId::new("turn-normal-child-root").expect("根 Turn 标识应有效");

        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                "子 Agent 取消测试",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &keencode_agent::ToolCallId::new("spawn-normal-child-interruption")
                    .expect("工具调用标识应有效"),
                test_spawn_request("normal_child_interruption", project.path()),
            )
            .expect("子 Agent 应创建");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 状态应读取"),
            CollaborationAgentStatus::Running { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));

        let completion = collaboration.coordinator.complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Interrupted,
        );
        assert!(matches!(
            completion,
            Ok(keencode_agent::TurnCompletionDisposition::Committed)
        ));
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 终态应读取"),
            CollaborationAgentStatus::Interrupted { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));
        assert!(
            !session
                .snapshot()
                .expect("Runtime Session 快照应读取")
                .recovery_required
        );
        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("生产 Store 快照应读取")
            .expect("生产 Store 快照应存在");
        assert!(persisted.commit.batch.events.iter().any(|event| {
            matches!(
                &event.kind,
                CollaborationEventKind::AgentStatusChanged {
                    previous: CollaborationAgentStatus::Running { turn_id: previous_turn_id },
                    current: CollaborationAgentStatus::Interrupted { turn_id: interrupted_turn_id },
                } if event.agent_id == child.agent.agent_id
                    && previous_turn_id == &child.initial_turn_id
                    && interrupted_turn_id == &child.initial_turn_id
                    && event.source_agent_id == child.agent.agent_id
            )
        }));
    }

    /// Runner 失败正文在进入协作领域前必须脱敏，且事件、checkpoint、mailbox 与冷恢复都不得还原秘密。
    #[tokio::test]
    async fn collaboration_failure_redaction_survives_persistence_and_cold_recovery() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "failure-redaction-persistence")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let root_turn =
            keencode_agent::TurnId::new("turn-failure-redaction-root").expect("根 Turn 标识应有效");

        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                "验证失败正文持久脱敏",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-failure-redaction").expect("工具调用标识应有效"),
                test_spawn_request("failure_redaction", project.path()),
            )
            .expect("子 Agent 应创建");

        let secrets = [
            "kc-collaboration-path-secret",
            "kc-collaboration-nested-secret",
            "kc-collaboration-fragment-secret",
        ];
        let request_id = "req-collaboration-redaction";
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [ScriptedReply::new(vec![Err(
                ModelError::ProviderUnavailable {
                    message: format!(
                        concat!(
                            "上游故障 ",
                            "path=https://path.invalid/%61pi_key={} ",
                            "nested=https://outer.invalid/?redirect=https%253A%252F%252Fuser%253A{}%2540inner.invalid%252Fv1 ",
                            "fragment=https://fragment.invalid/#%61pi_key%3D{} ",
                            "request_id={}"
                        ),
                        secrets[0], secrets[1], secrets[2], request_id
                    ),
                    status_code: Some(503),
                    retryable: false,
                },
            )])],
        ));
        let input = ModelMessage::text(MessageRole::User, "触发子 Agent Provider 失败");
        let result = AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
            .run_turn(TurnRequest::new(
                child.agent.session_id.clone(),
                child.initial_turn_id.clone(),
                child.agent.agent_id.clone(),
                "test-model",
                vec![input],
                PlanGuard::inactive(),
            ))
            .await;
        assert!(matches!(
            &result.error,
            Some(keencode_agent::AgentRunError::Model(
                ModelError::ProviderUnavailable { .. }
            ))
        ));
        let outcome = super::runtime_turn_outcome(Ok(result));
        let assert_redacted = |value: &str| {
            for secret in secrets {
                assert!(
                    !value.contains(secret),
                    "失败持久状态仍包含原始秘密 {secret}: {value}"
                );
            }
            assert!(
                value.contains(keencode_model::REDACTED_SECRET),
                "失败持久状态缺少脱敏占位符: {value}"
            );
            assert!(value.contains(request_id), "失败诊断上下文丢失: {value}");
        };
        let AgentTurnOutcome::Failed { message } = &outcome else {
            panic!("Provider 失败必须映射为协作失败终态");
        };
        assert_redacted(message);

        collaboration
            .coordinator
            .complete_turn(&child.agent.agent_id, &child.initial_turn_id, outcome)
            .expect("子 Agent 失败终态应提交");
        let CollaborationAgentStatus::Failed { message, .. } = collaboration
            .coordinator
            .agent_status(&child.agent.agent_id)
            .expect("子 Agent 失败状态应读取")
        else {
            panic!("子 Agent 必须处于失败状态");
        };
        assert_redacted(&message);

        let live_checkpoint = collaboration
            .coordinator
            .checkpoint_coordinator()
            .expect("live checkpoint 应读取");
        let checkpoint_child =
            super::recovered_agent_for_id(&live_checkpoint, &child.agent.agent_id)
                .expect("checkpoint 应包含子 Agent");
        let CollaborationAgentStatus::Failed { message, .. } = &checkpoint_child.status else {
            panic!("checkpoint 子 Agent 必须处于失败状态");
        };
        assert_redacted(message);
        let AgentTurnOutcome::Failed { message } = &checkpoint_child
            .last_turn
            .as_ref()
            .expect("checkpoint 应保留子 Turn")
            .outcome
        else {
            panic!("checkpoint 子 Turn 必须保留失败终态");
        };
        assert_redacted(message);
        let checkpoint_root =
            super::recovered_agent_for_id(&live_checkpoint, &collaboration.root_agent_id)
                .expect("checkpoint 应包含根 Agent");
        let live_mailbox = checkpoint_root
            .mailbox
            .iter()
            .find(|entry| entry.message.related_turn_id.as_ref() == Some(&child.initial_turn_id))
            .expect("根 Agent mailbox 应包含子 Turn 完成通知");
        assert_redacted(&live_mailbox.message.content);
        let keencode_agent::MailboxMessageKind::ChildTurnFinished {
            outcome: AgentTurnOutcome::Failed { message },
        } = &live_mailbox.message.kind
        else {
            panic!("根 Agent mailbox 必须保存子 Turn 失败终态");
        };
        assert_redacted(message);

        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("生产 Store 快照应读取")
            .expect("生产 Store 快照应存在");
        let event_message = persisted
            .commit
            .batch
            .events
            .iter()
            .find_map(|event| match &event.kind {
                CollaborationEventKind::AgentTurnFailed { message }
                    if event.agent_id == child.agent.agent_id =>
                {
                    Some(message)
                }
                _ => None,
            })
            .expect("终态批次应包含 AgentTurnFailed 事件");
        assert_redacted(event_message);
        let persisted_json = std::fs::read_to_string(&collaboration.store.transition_path)
            .expect("collaboration-v2.json 应读取");
        for secret in secrets {
            assert!(!persisted_json.contains(secret));
        }
        assert!(persisted_json.contains(keencode_model::REDACTED_SECRET));

        runtime
            .collaboration_sessions
            .lock()
            .expect("测试 Collaboration 表应可写")
            .remove(&session_id);
        drop(collaboration);

        let recovered_store = Arc::new(
            SessionCollaborationStore::new(storage.path(), &session_id)
                .expect("冷恢复 Store 应创建"),
        );
        recovered_store
            .bind_runtime_session(&session)
            .expect("冷恢复 Store 应绑定 Session");
        let persisted_checkpoint = recovered_store
            .load_coordinator_checkpoint()
            .expect("冷恢复 checkpoint 应读取")
            .expect("冷恢复 checkpoint 应存在");
        let recovered_coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(2).expect("测试容量应有效"),
            recovered_store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        recovered_coordinator
            .restore_coordinator(persisted_checkpoint)
            .expect("新 Coordinator 应从磁盘 checkpoint 恢复");
        let CollaborationAgentStatus::Failed { message, .. } = recovered_coordinator
            .agent_status(&child.agent.agent_id)
            .expect("冷恢复子 Agent 状态应读取")
        else {
            panic!("冷恢复子 Agent 必须处于失败状态");
        };
        assert_redacted(&message);
        let recovered_checkpoint = recovered_coordinator
            .checkpoint_coordinator()
            .expect("冷恢复后的 checkpoint 应读取");
        let recovered_root = super::recovered_agent_for_id(
            &recovered_checkpoint,
            &keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效"),
        )
        .expect("冷恢复 checkpoint 应包含根 Agent");
        let recovered_mailbox = recovered_root
            .mailbox
            .iter()
            .find(|entry| entry.message.related_turn_id.as_ref() == Some(&child.initial_turn_id))
            .expect("冷恢复根 Agent mailbox 应包含子 Turn 完成通知");
        assert_redacted(&recovered_mailbox.message.content);
        let keencode_agent::MailboxMessageKind::ChildTurnFinished {
            outcome: AgentTurnOutcome::Failed { message },
        } = &recovered_mailbox.message.kind
        else {
            panic!("冷恢复 mailbox 必须保留子 Turn 失败终态");
        };
        assert_redacted(message);

        drop(recovered_coordinator);
        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }

    /// 成功根 Turn 的最终文本必须从权威 Transcript 恢复，不能因 Runtime 重启而触发恢复栅栏。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_completed_non_empty_final_message_cold_recovery_preserves_outcome() {
        assert_completed_root_survives_cold_recovery("根 Turn 冷恢复必须保留的最终文本").await;
    }

    /// 原页面编辑重发必须经真实回退事务重建协作状态，保留前缀且允许再次发送。
    #[tokio::test(flavor = "multi_thread")]
    async fn rewound_session_rebuilds_collaboration_and_resends() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server_for_requests("回退验收回复", 3);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "rewind-resend")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        for (turn_id, prompt) in [
            ("turn-before-edit", "保留的第一轮"),
            ("turn-edit-target", "编辑前请求"),
        ] {
            runtime
                .start_root_turn(&session_id, turn_id, prompt, RootTurnOptions::default())
                .await
                .unwrap();
            wait_for_session_idle(&runtime, &session_id).await;
        }
        let target = session
            .snapshot()
            .unwrap()
            .state
            .raw_transcript_messages()
            .into_iter()
            .rev()
            .find(|message| message.role == keencode_resources::MessageRole::User)
            .unwrap()
            .message_id
            .clone();
        runtime.close_session(&session_id).await.unwrap();
        drop(session);
        let result = runtime
            .runtime_manager()
            .prepare_edit_user_closed_session(keencode_resources::SessionEditUserRequest {
                source_session_id: keencode_resources::SessionId::new(&session_id).unwrap(),
                target_message_id: target,
                expected_text: "编辑前请求".to_owned(),
                operation_id: "native-edit-resend".to_owned(),
            })
            .unwrap();
        let reopened = runtime
            .open_or_create_session(project.path(), Some(&session_id), "rewind-reopen")
            .unwrap();
        runtime
            .start_root_turn(
                &session_id,
                "turn-edited-resend",
                "编辑后请求",
                RootTurnOptions::default(),
            )
            .await
            .expect("回退后的协作恢复不能再引用已移除的 Turn");
        wait_for_session_idle(&runtime, &session_id).await;
        let state = reopened.snapshot().unwrap().state;
        assert!(
            state
                .turns
                .keys()
                .any(|turn| turn.as_str() == "turn-before-edit")
        );
        assert!(
            !state
                .turns
                .keys()
                .any(|turn| turn.as_str() == "turn-edit-target")
        );
        assert!(
            state
                .turns
                .values()
                .any(|turn| turn.turn_id.as_str() == "turn-edited-resend"
                    && turn.status == TurnStatus::Completed)
        );
        let archive = runtime
            .open_or_create_session(
                project.path(),
                Some(result.archived_session_id.as_str()),
                "rewind-archive",
            )
            .unwrap();
        assert!(
            archive
                .snapshot()
                .unwrap()
                .state
                .turns
                .keys()
                .any(|turn| turn.as_str() == "turn-edit-target")
        );
        let requests = server.join().unwrap().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(request_contains_user_text(&requests[2], "保留的第一轮"));
        assert!(request_contains_user_text(&requests[2], "编辑后请求"));
        assert!(!request_contains_user_text(&requests[2], "编辑前请求"));
        runtime.close_session(&session_id).await.unwrap();
        runtime
            .close_session(result.archived_session_id.as_str())
            .await
            .unwrap();
    }

    /// 带资源引用的完成回合冷启动后仍可继续；更换权威历史中的市场身份必须拒绝恢复。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_input_references_cold_resume_and_reject_changed_market() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server_for_requests("引用冷恢复回复", 2);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "reference-cold-resume")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        let references = vec![keencode_model::InputReference {
            name: "proof".into(),
            path: "plugin://proof@local".into(),
        }];
        runtime
            .start_root_turn(
                &session_id,
                "reference-cold-first",
                "@proof 原请求",
                RootTurnOptions {
                    references: references.clone(),
                    ..RootTurnOptions::default()
                },
            )
            .await
            .unwrap();
        wait_for_session_idle(&runtime, &session_id).await;
        let checkpoint = runtime
            .collaboration_sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .unwrap()
            .coordinator
            .checkpoint_coordinator()
            .unwrap();
        let mut tampered = session.snapshot().unwrap().state;
        for record in &mut tampered.transcript {
            if let TranscriptRecord::MessageAdded(message) = record
                && !message.references.is_empty()
            {
                message.references[0].path = "plugin://proof@changed".into();
            }
        }
        assert_eq!(
            super::recovered_authoritative_turn_outcomes_with_waiting_capacity(
                Some(&session),
                Some(&checkpoint),
                &tampered,
                &std::collections::HashSet::new()
            ),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        runtime.close_session(&session_id).await.unwrap();
        drop(session);
        drop(runtime);
        let recovered = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = recovered
            .open_or_create_session(project.path(), Some(&session_id), "reference-cold-reopen")
            .unwrap();
        assert_eq!(
            recovered
                .start_root_turn(
                    &session_id,
                    "reference-cold-next",
                    "继续原会话",
                    RootTurnOptions::default()
                )
                .await
                .unwrap(),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&recovered, &session_id).await;
        let stored = session
            .model_transcript()
            .unwrap()
            .into_iter()
            .find(|message| !message.references.is_empty())
            .unwrap();
        assert_eq!(stored.references, references);
        assert!(
            session
                .snapshot()
                .unwrap()
                .state
                .turns
                .contains_key(&ResourceTurnId::new("reference-cold-next").unwrap())
        );
        recovered.close_session(&session_id).await.unwrap();
        let requests = server.join().unwrap().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "user"
                    && message["content"].as_array().is_some_and(|parts| {
                        // 原正文与资源身份各占一个内容块，必须出现在同一条用户消息中。
                        parts
                            .iter()
                            .any(|part| part["text"].as_str() == Some("@proof 原请求"))
                            && parts.iter().any(|part| {
                                part["text"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("plugin://proof@local"))
                            })
                    }))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn root_attachment_images_and_text_context_reach_provider_and_transcript() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server_for_requests("附件已处理", 1);
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                image_input: true,
                ..ProviderCapabilities::default()
            }),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "attachment-runtime-input")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        runtime
            .start_root_turn(
                &session_id,
                "attachment-input-turn",
                "请分析附件",
                RootTurnOptions {
                    references: vec![keencode_model::InputReference {
                        name: "notes.md".to_owned(),
                        path: "C:/authorized/notes.md".to_owned(),
                    }],
                    attachment_images: vec![ImageContent::from_base64("image/png", "aGVsbG8=")],
                    attachment_context: vec!["授权文本附件内容".to_owned()],
                    ..RootTurnOptions::default()
                },
            )
            .await
            .unwrap();
        wait_for_session_idle(&runtime, &session_id).await;
        let requests = server.join().unwrap().unwrap();
        let request_text = requests[0].to_string();
        assert!(request_text.contains("aGVsbG8="));
        assert!(request_text.contains("授权文本附件内容"));
        assert!(request_text.contains("请分析附件"));
        let transcript = session.model_transcript().unwrap();
        let transcript_text = serde_json::to_string(&transcript).unwrap();
        assert!(transcript_text.contains("notes.md"));
        assert!(transcript_text.contains("image"));
        assert!(!transcript_text.contains("授权文本附件内容"));
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "attachment-input-turn",
                    "请分析附件",
                    RootTurnOptions {
                        attachment_context: vec!["另一份授权文本附件".to_owned()],
                        ..RootTurnOptions::default()
                    },
                )
                .await,
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        runtime.close_session(&session_id).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn root_attachment_image_is_rejected_before_provider_without_vision() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_responses_request_probe();
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities::default()),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "attachment-capability-reject")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        let before = session.snapshot().unwrap().state;
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "attachment-capability-turn",
                    "请分析图片",
                    RootTurnOptions {
                        attachment_images: vec![
                            ImageContent::from_base64("image/png", "aGVsbG8=",)
                        ],
                        ..RootTurnOptions::default()
                    },
                )
                .await,
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        let after = session.snapshot().unwrap().state;
        assert_eq!(before.transcript_revision, after.transcript_revision);
        assert_eq!(before.turns.len(), after.turns.len());
        assert!(server.join().unwrap().unwrap().is_none());
        runtime.close_session(&session_id).await.unwrap();
    }

    /// 原页面指定回复分叉必须保留真实前缀，并从新 Session 继续模型调用。
    #[tokio::test(flavor = "multi_thread")]
    async fn forked_prefix_session_continues_without_later_source_messages() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server_for_requests("分叉验收回复", 3);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let source = runtime
            .open_or_create_session(project.path(), None, "fork-prefix")
            .unwrap();
        let source_id = source.session_id().as_str().to_owned();
        mark_session_title_manual(&source);
        for (turn, text) in [
            ("fork-first", "需要继承的第一轮"),
            ("fork-second", "不应继承的第二轮"),
        ] {
            let references = if turn == "fork-first" {
                vec![keencode_model::InputReference {
                    name: "proof".into(),
                    path: "plugin://proof@local".into(),
                }]
            } else {
                Vec::new()
            };
            runtime
                .start_root_turn(
                    &source_id,
                    turn,
                    text,
                    RootTurnOptions {
                        references,
                        ..RootTurnOptions::default()
                    },
                )
                .await
                .unwrap();
            wait_for_session_idle(&runtime, &source_id).await;
        }
        runtime.close_session(&source_id).await.unwrap();
        drop(source);
        let result = runtime
            .runtime_manager()
            .fork_closed_session(keencode_resources::SessionForkRequest {
                source_session_id: keencode_resources::SessionId::new(&source_id).unwrap(),
                operation_id: "native-prefix-fork".into(),
                title: Some("分叉验收".into()),
                through_turn_id: Some(keencode_resources::TurnId::new("fork-first").unwrap()),
            })
            .unwrap();
        let fork_id = result.session_id.as_str();
        let target = runtime
            .open_or_create_session(project.path(), Some(fork_id), "fork-prefix-reopen")
            .unwrap();
        runtime
            .start_root_turn(
                fork_id,
                "fork-next",
                "分叉后的新问题",
                RootTurnOptions::default(),
            )
            .await
            .unwrap();
        wait_for_session_idle(&runtime, fork_id).await;
        let state = target.snapshot().unwrap().state;
        let inherited = target
            .model_transcript()
            .unwrap()
            .into_iter()
            .find(|message| !message.references.is_empty())
            .unwrap();
        assert_eq!(inherited.references[0].path, "plugin://proof@local");
        assert_eq!(
            inherited.content,
            vec![keencode_model::ContentBlock::text("需要继承的第一轮")]
        );
        assert!(
            state
                .turns
                .contains_key(&keencode_resources::TurnId::new("fork-first").unwrap())
        );
        assert!(
            !state
                .turns
                .contains_key(&keencode_resources::TurnId::new("fork-second").unwrap())
        );
        assert_eq!(
            state.turns[&keencode_resources::TurnId::new("fork-next").unwrap()].status,
            TurnStatus::Completed
        );
        let requests = server.join().unwrap().unwrap();
        assert!(requests[2].to_string().contains("plugin://proof@local"));
        assert!(request_contains_user_text(&requests[2], "需要继承的第一轮"));
        assert!(request_contains_user_text(&requests[2], "分叉后的新问题"));
        assert!(!request_contains_user_text(
            &requests[2],
            "不应继承的第二轮"
        ));
        runtime.close_session(fork_id).await.unwrap();
    }

    /// Artifact 化的长回复必须按同样的 UTF-8 截断规则恢复，不能丢弃或变成另一条摘要。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_completed_artifact_final_message_cold_recovery_preserves_outcome() {
        assert_completed_root_survives_cold_recovery(&"多字节最终结果\r\n".repeat(10_000)).await;
    }

    /// 经真实本地 Provider 完成两次根 Turn，在中间冷重启并核验协作摘要与权威正文。
    async fn assert_completed_root_survives_cold_recovery(final_message: &str) {
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let project = tempfile::tempdir().expect("应创建测试项目目录");
        let expected_summary = super::bounded_collaboration_failure(final_message);
        let (base_url, server) = spawn_buffered_responses_server_for_requests(final_message, 2);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "root-completed-cold-recovery")
            .expect("首个 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        let first_turn_id = "turn-root-completed-cold-recovery";

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    first_turn_id,
                    "执行一次成功根 Turn",
                    RootTurnOptions::default(),
                )
                .await
                .expect("首个根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;
        let first_snapshot = session.snapshot().expect("首个 Turn 快照应读取");
        assert!(first_snapshot.state.turns.values().any(|turn| {
            turn.turn_id.as_str() == first_turn_id && turn.status == TurnStatus::Completed
        }));
        if final_message.len() > RuntimeConfig::new(storage.path()).max_inline_text_bytes {
            assert!(
                first_snapshot
                    .state
                    .raw_transcript_messages()
                    .iter()
                    .any(|message| {
                        message.role == keencode_resources::MessageRole::Assistant
                            && message.content.iter().any(|part| {
                                matches!(part, keencode_resources::MessagePart::Artifact { .. })
                            })
                    }),
                "长回复必须确实进入 Artifact 路径"
            );
        }
        let collaboration = runtime
            .collaboration_sessions
            .lock()
            .expect("Collaboration 表应读取")
            .get(&session_id)
            .cloned()
            .expect("成功根 Turn 后 Collaboration Runtime 应存在");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("根 Agent 终态应读取"),
            CollaborationAgentStatus::Completed {
                final_message: Some(ref message),
                ..
            } if message == &expected_summary
        ));

        runtime
            .close_session(&session_id)
            .await
            .expect("首个 Runtime 应关闭");
        drop(collaboration);
        drop(session);
        drop(runtime);

        let recovered_runtime =
            runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let recovered_session = recovered_runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "root-completed-cold-recovery-reopen",
            )
            .expect("新 Runtime 应冷重开原 Session");
        let second_turn_id = "turn-root-completed-cold-recovery-followup";
        assert_eq!(
            recovered_runtime
                .start_root_turn(
                    &session_id,
                    second_turn_id,
                    "冷恢复后继续执行一次根 Turn",
                    RootTurnOptions::default(),
                )
                .await
                .expect("冷恢复后的根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&recovered_runtime, &session_id).await;
        let recovered_snapshot = recovered_session
            .snapshot()
            .expect("冷恢复后的 Session 快照应读取");
        assert!(recovered_snapshot.state.turns.values().any(|turn| {
            turn.turn_id.as_str() == first_turn_id && turn.status == TurnStatus::Completed
        }));
        assert!(recovered_snapshot.state.turns.values().any(|turn| {
            turn.turn_id.as_str() == second_turn_id && turn.status == TurnStatus::Completed
        }));

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应收到两次请求");
        assert_eq!(requests.len(), 2);
        assert!(request_contains_user_text(
            &requests[0],
            "执行一次成功根 Turn"
        ));
        assert!(request_contains_user_text(
            &requests[1],
            "冷恢复后继续执行一次根 Turn"
        ));

        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("冷恢复后的 Runtime 应关闭");
        drop(recovered_session);
        drop(recovered_runtime);
    }

    /// 真实根 Turn 经本地 Responses Provider 进入请求后取消，必须提交 Interrupted 终态。
    #[tokio::test(flavor = "multi_thread")]
    async fn runtime_root_turn_cancellation_via_local_http_persists_interrupted() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let prompt = "取消正在等待本地 HTTP 响应的根 Turn";
        let (base_url, gate, server) =
            spawn_gated_buffered_responses_server("取消后不应提交完成文本", 1, prompt);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "runtime-root-http-cancel")
            .expect("根取消测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let turn_id = "turn-runtime-root-http-cancel";
        let turn = AgentTurnId::new(turn_id).expect("根取消测试 Turn 标识应有效");

        assert_eq!(
            runtime
                .start_root_turn(&session_id, turn_id, prompt, RootTurnOptions::default(),)
                .await
                .expect("根 Turn 应经生产 Runner 启动"),
            RootTurnStartOutcome::Started
        );
        gate.wait_for_requests(1)
            .expect("本地 Provider 应先收到根 Turn 请求");
        let collaboration = runtime
            .collaboration_sessions
            .lock()
            .expect("Collaboration 表应读取")
            .get(&session_id)
            .cloned()
            .expect("根 Turn 启动后应存在生产 Collaboration Runtime");
        assert_eq!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("根 Agent 运行状态应读取"),
            CollaborationAgentStatus::Running {
                turn_id: turn.clone()
            }
        );

        let cancellation = runtime.cancel_turn(&session_id, turn_id);
        // 取消会关闭 Provider 请求；无论本地服务端写响应是否遇到连接断开，都必须释放闸门，
        // 让测试线程可以回收本地 HTTP 服务。
        gate.release();
        assert!(matches!(
            cancellation,
            Ok(keencode_runtime::TurnCancellationOutcome::Requested)
        ));
        wait_for_session_idle(&runtime, &session_id).await;

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("根取消测试本地模型服务应成功");
        assert_eq!(requests.len(), 1);
        assert!(request_contains_user_text(&requests[0], prompt));
        assert_eq!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("取消后的根 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted {
                turn_id: turn.clone()
            }
        );
        let session_snapshot = session.snapshot().expect("取消后的 Session 快照应读取");
        assert!(!session_snapshot.recovery_required);
        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("根取消测试 Store 快照应读取")
            .expect("根取消测试 Store 快照应存在");
        let root_checkpoint = persisted
            .commit
            .checkpoint
            .roots
            .iter()
            .flat_map(|root| root.agents.iter())
            .find(|agent| agent.definition.agent_id == collaboration.root_agent_id)
            .expect("根 Agent checkpoint 应存在");
        assert_eq!(
            root_checkpoint.status,
            CollaborationAgentStatus::Interrupted {
                turn_id: turn.clone()
            }
        );
        assert!(persisted.commit.batch.events.iter().any(|event| {
            event.agent_id == collaboration.root_agent_id
                && matches!(event.kind, CollaborationEventKind::AgentTurnInterrupted)
        }));
        assert_eq!(
            persisted.commit.checkpoint.last_event_sequence,
            persisted
                .commit
                .batch
                .events
                .last()
                .expect("根取消终态批次应有事件")
                .sequence
        );

        runtime
            .close_session(&session_id)
            .await
            .expect("根取消测试 Session 应关闭");
    }

    /// 真实取消经冷恢复后只在下一次请求注入一次性上下文；正常完成后不重放旧原因。
    #[tokio::test(flavor = "multi_thread")]
    async fn runtime_previous_interruption_notice_is_request_only_and_expires_after_success() {
        let storage = tempfile::tempdir().expect("应创建中断恢复测试存储目录");
        let project = tempfile::tempdir().expect("应创建中断恢复测试项目目录");
        let first_prompt = "冷恢复前取消这条根任务";
        let second_prompt = "冷恢复后继续上一条根任务";
        let third_prompt = "正常完成后开始下一条根任务";
        let (base_url, mut gates, server) = spawn_gated_buffered_responses_server_with_texts(
            "本地模型完成响应",
            3,
            &[first_prompt],
        );
        let first_gate = gates.pop().expect("取消测试应有首轮闸门");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "runtime-interruption-notice")
            .expect("中断恢复测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        let first_turn_id = "turn-interruption-notice-first";
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    first_turn_id,
                    first_prompt,
                    RootTurnOptions::default(),
                )
                .await
                .expect("首轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        first_gate
            .wait_for_requests(1)
            .expect("首轮请求应到达本地 Provider");
        assert!(matches!(
            runtime.cancel_turn(&session_id, first_turn_id),
            Ok(keencode_runtime::TurnCancellationOutcome::Requested)
        ));
        first_gate.release();
        wait_for_session_idle(&runtime, &session_id).await;
        let cancelled_snapshot = session.snapshot().expect("取消后的快照应读取");
        let cancelled_turn = cancelled_snapshot
            .state
            .turns
            .get(&ResourceTurnId::new(first_turn_id).expect("首轮 Turn 标识应有效"))
            .expect("首轮 Turn 应存在");
        assert_eq!(cancelled_turn.status, TurnStatus::Cancelled);
        assert_eq!(cancelled_turn.stop_reason, Some(TurnStopReason::Cancelled));

        // 关闭后重新打开同一 Session，验证模型可见状态来自 Journal 而非进程内缓存。
        runtime
            .close_session(&session_id)
            .await
            .expect("中断恢复测试首个 Runtime 应关闭");
        drop(session);
        drop(runtime);
        let recovered_runtime =
            runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let recovered_session = recovered_runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "runtime-interruption-notice-reopen",
            )
            .expect("中断恢复测试 Session 应冷重开");
        let second_turn_id = "turn-interruption-notice-second";
        assert_eq!(
            recovered_runtime
                .start_root_turn(
                    &session_id,
                    second_turn_id,
                    second_prompt,
                    RootTurnOptions::default(),
                )
                .await
                .expect("冷恢复后的根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&recovered_runtime, &session_id).await;
        let second_snapshot = recovered_session.snapshot().expect("第二轮完成快照应读取");
        let second_turn = second_snapshot
            .state
            .turns
            .get(&ResourceTurnId::new(second_turn_id).expect("第二轮 Turn 标识应有效"))
            .expect("第二轮 Turn 应存在");
        assert_eq!(second_turn.status, TurnStatus::Completed);

        let third_turn_id = "turn-interruption-notice-third";
        assert_eq!(
            recovered_runtime
                .start_root_turn(
                    &session_id,
                    third_turn_id,
                    third_prompt,
                    RootTurnOptions::default(),
                )
                .await
                .expect("第三轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&recovered_runtime, &session_id).await;
        let requests = server
            .join()
            .expect("中断恢复测试本地服务线程不应 panic")
            .expect("中断恢复测试本地服务应成功");
        assert_eq!(requests.len(), 3);
        let has_previous_stop_marker = |request: &Value| {
            request["input"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["content"].as_array().is_some_and(|content| {
                        content.iter().any(|part| {
                            part["text"]
                                .as_str()
                                .is_some_and(|text| text.contains("keencode/previous-turn-stop/v1"))
                        })
                    })
                })
            })
        };
        let first_request = requests
            .iter()
            .find(|request| request_contains_user_text(request, first_prompt))
            .expect("应捕获首轮请求");
        let second_request = requests
            .iter()
            .find(|request| request_contains_user_text(request, second_prompt))
            .expect("应捕获第二轮请求");
        let third_request = requests
            .iter()
            .find(|request| request_contains_user_text(request, third_prompt))
            .expect("应捕获第三轮请求");
        assert!(!has_previous_stop_marker(first_request));
        assert!(has_previous_stop_marker(second_request));
        assert!(!has_previous_stop_marker(third_request));

        let persisted = serde_json::to_string(
            &recovered_session
                .transcript()
                .expect("冷恢复后的权威 Transcript 应读取"),
        )
        .expect("权威 Transcript 应可序列化");
        assert!(persisted.contains(first_prompt));
        assert!(persisted.contains(second_prompt));
        assert!(persisted.contains(third_prompt));
        assert!(!persisted.contains("keencode/previous-turn-stop/v1"));
        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("中断恢复测试最终 Runtime 应关闭");
    }

    /// 真实子 Agent 经本地 Responses Provider 进入请求后取消，必须持久化 Interrupted 且不污染根 Turn。
    #[tokio::test(flavor = "multi_thread")]
    async fn runtime_child_turn_cancellation_via_local_http_persists_interrupted() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let root_prompt = "保持根 Turn 活跃以取消子 Agent";
        let child_name = "runtime_http_cancel_child";
        let child_prompt = format!("执行 {child_name} 测试任务");
        let (base_url, gates, server) = spawn_gated_buffered_responses_server_with_texts(
            "子 Agent 取消后不应提交完成文本",
            2,
            &[root_prompt, child_prompt.as_str()],
        );
        let mut gates = gates.into_iter();
        let root_gate = gates.next().expect("子取消测试应有根请求闸门");
        let child_gate = gates.next().expect("子取消测试应有子请求闸门");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "runtime-child-http-cancel")
            .expect("子 Agent 取消测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let root_turn_id = "turn-runtime-child-http-root";
        let root_turn = AgentTurnId::new(root_turn_id).expect("子取消测试根 Turn 标识应有效");

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    root_turn_id,
                    root_prompt,
                    RootTurnOptions::default(),
                )
                .await
                .expect("根 Turn 应经生产 Runner 启动"),
            RootTurnStartOutcome::Started
        );
        root_gate
            .wait_for_requests(1)
            .expect("本地 Provider 应先收到根 Turn 请求");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("生产 Collaboration Runtime 应可复用");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-runtime-http-cancel-child")
                    .expect("子 Agent 工具调用标识应有效"),
                test_spawn_request(child_name, project.path()),
            )
            .expect("子 Agent 应经生产 execution port 启动");
        child_gate
            .wait_for_requests(1)
            .expect("本地 Provider 应收到子 Agent 请求");
        assert_eq!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 运行状态应读取"),
            CollaborationAgentStatus::Running {
                turn_id: child.initial_turn_id.clone()
            }
        );

        // 先释放根响应并等待根 Turn 正常完成，避免子 Agent 取消关闭共享 HTTP 客户端连接时
        // 影响仍在等待同一 Provider 的根请求；子请求继续由独立闸门保持挂起。
        root_gate.release();
        let root_deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("根 Agent 状态应读取"),
            CollaborationAgentStatus::Completed { .. }
        ) {
            assert!(Instant::now() < root_deadline, "根 Turn 应在测试窗口内完成");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let cancellation =
            runtime.background_task_cancel_outcome(&session_id, child.initial_turn_id.as_str());
        // 子 Agent 请求已经到达闸门后才取消，确保覆盖真实 Provider 等待路径。
        child_gate.release();
        assert!(
            cancellation.is_ok(),
            "子 Agent 取消请求应成功：{cancellation:?}"
        );
        wait_for_session_idle(&runtime, &session_id).await;

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("子取消测试本地模型服务应成功");
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .any(|request| request_contains_user_text(request, root_prompt))
        );
        assert!(
            requests
                .iter()
                .any(|request| request_contains_user_text(request, child_prompt.as_str()))
        );
        assert_eq!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("取消后的子 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted {
                turn_id: child.initial_turn_id.clone()
            }
        );
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&collaboration.root_agent_id)
                .expect("子取消后的根 Agent 状态应读取"),
            CollaborationAgentStatus::Completed {
                ref turn_id,
                ..
            } if turn_id == &root_turn
        ));
        let session_snapshot = session.snapshot().expect("子取消后的 Session 快照应读取");
        assert!(!session_snapshot.recovery_required);
        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("子取消测试 Store 快照应读取")
            .expect("子取消测试 Store 快照应存在");
        let child_checkpoint = persisted
            .commit
            .checkpoint
            .roots
            .iter()
            .flat_map(|root| root.agents.iter())
            .find(|agent| agent.definition.agent_id == child.agent.agent_id)
            .expect("子 Agent checkpoint 应存在");
        assert_eq!(
            child_checkpoint.status,
            CollaborationAgentStatus::Interrupted {
                turn_id: child.initial_turn_id.clone()
            }
        );
        assert!(persisted.commit.batch.events.iter().any(|event| {
            event.agent_id == child.agent.agent_id
                && matches!(event.kind, CollaborationEventKind::AgentTurnInterrupted)
        }));
        assert_eq!(
            persisted.commit.checkpoint.last_event_sequence,
            persisted
                .commit
                .batch
                .events
                .last()
                .expect("子取消终态批次应有事件")
                .sequence
        );

        runtime
            .close_session(&session_id)
            .await
            .expect("子取消测试 Session 应关闭");
    }

    /// 动态输入恢复只信任资源层持久回执，不接受正文 marker 作为确认依据。
    #[test]
    fn dynamic_input_recovery_requires_exact_authoritative_receipt_scope() {
        let session_id = ResourceSessionId::new("session-dynamic-input").unwrap();
        let agent_id = ResourceAgentId::new("agent-dynamic-input").unwrap();
        let turn_id = ResourceTurnId::new("turn-dynamic-input").unwrap();
        let mut state = SessionState::empty(session_id);
        state.dynamic_input_receipts.push(DynamicInputReceipt {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            model_round: 1,
            segment_index: 0,
            kind: DynamicInputKind::Mailbox,
            through_sequence: 4,
            user_inputs: Vec::new(),
            transcript_revision: 2,
        });
        let claim = super::RecoveredDynamicInputClaim {
            agent_id: keencode_agent::AgentId::new(agent_id.as_str()).unwrap(),
            turn_id: keencode_agent::TurnId::new(turn_id.as_str()).unwrap(),
            kind: super::DynamicInputMarkerKind::Mailbox,
            through_sequence: 4,
            mailbox_message_ids: Vec::new(),
            mailbox_messages: Vec::new(),
            user_steers: Vec::new(),
        };
        assert!(dynamic_input_receipt_matches_claim(&state, &claim));

        // 只有正文 marker、没有资源层 receipt 时，恢复不能确认 claim。
        let forged_marker_claim = super::RecoveredDynamicInputClaim {
            through_sequence: 5,
            ..claim.clone()
        };
        assert!(!dynamic_input_receipt_matches_claim(
            &state,
            &forged_marker_claim
        ));

        // 同一 Agent 的不同 Turn 不得交叉确认。
        let other_turn_claim = super::RecoveredDynamicInputClaim {
            turn_id: keencode_agent::TurnId::new("turn-other").unwrap(),
            ..claim
        };
        assert!(!dynamic_input_receipt_matches_claim(
            &state,
            &other_turn_claim
        ));
    }

    /// mailbox 恢复必须逐项绑定权威消息；允许首条已 Delivered、后续仍 Queued 的部分确认。
    #[test]
    fn recovered_mailbox_claim_requires_exact_message_identity_and_allows_partial_delivery() {
        let session_id = ResourceSessionId::new("session-mailbox-recovery").unwrap();
        let source_agent_id = ResourceAgentId::new("agent-source").unwrap();
        let target_agent_id = ResourceAgentId::new("agent-target").unwrap();
        let source_turn_id = ResourceTurnId::new("turn-source").unwrap();
        let first_message_id = ResourceMailboxMessageId::new("mailbox-first").unwrap();
        let second_message_id = ResourceMailboxMessageId::new("mailbox-second").unwrap();

        let first_runner_message = keencode_agent::MailboxMessage {
            message_id: keencode_agent::MailboxMessageId::new("mailbox-first").unwrap(),
            sequence: 7,
            source_agent_id: keencode_agent::AgentId::new("agent-source").unwrap(),
            source_agent_path: keencode_agent::AgentPath::parse("/root/source").unwrap(),
            source_plan_guard: PlanGuard::inactive(),
            target_agent_id: keencode_agent::AgentId::new("agent-target").unwrap(),
            delivery: keencode_agent::MailboxDelivery::QueueOnly,
            kind: keencode_agent::MailboxMessageKind::AgentMessage,
            content: "第一条 mailbox".to_owned(),
            related_turn_id: Some(keencode_agent::TurnId::new("turn-source").unwrap()),
            parent_turn_id: None,
            root_turn_id: None,
        };
        let second_runner_message = keencode_agent::MailboxMessage {
            message_id: keencode_agent::MailboxMessageId::new("mailbox-second").unwrap(),
            sequence: 8,
            source_agent_id: keencode_agent::AgentId::new("agent-source").unwrap(),
            source_agent_path: keencode_agent::AgentPath::parse("/root/source").unwrap(),
            source_plan_guard: PlanGuard::inactive(),
            target_agent_id: keencode_agent::AgentId::new("agent-target").unwrap(),
            delivery: keencode_agent::MailboxDelivery::QueueOnly,
            kind: keencode_agent::MailboxMessageKind::AgentMessage,
            content: "第二条 mailbox".to_owned(),
            related_turn_id: Some(keencode_agent::TurnId::new("turn-source").unwrap()),
            parent_turn_id: None,
            root_turn_id: None,
        };
        let first_resource_message = ResourceMailboxMessage {
            message_id: first_message_id.clone(),
            from: source_agent_id.clone(),
            to: target_agent_id.clone(),
            related_turn_id: source_turn_id.clone(),
            body: first_runner_message.content.clone(),
            artifact: None,
            state: MailboxState::Delivered,
        };
        let second_resource_message = ResourceMailboxMessage {
            message_id: second_message_id.clone(),
            from: source_agent_id,
            to: target_agent_id,
            related_turn_id: source_turn_id,
            body: second_runner_message.content.clone(),
            artifact: None,
            state: MailboxState::Queued,
        };
        let mut state = SessionState::empty(session_id);
        state
            .mailbox
            .insert(first_message_id.clone(), first_resource_message);
        state
            .mailbox
            .insert(second_message_id.clone(), second_resource_message);

        let claim = super::RecoveredDynamicInputClaim {
            agent_id: keencode_agent::AgentId::new("agent-target").unwrap(),
            turn_id: keencode_agent::TurnId::new("turn-target").unwrap(),
            kind: super::DynamicInputMarkerKind::Mailbox,
            through_sequence: 8,
            mailbox_message_ids: vec![first_message_id, second_message_id],
            mailbox_messages: vec![first_runner_message.clone(), second_runner_message.clone()],
            user_steers: Vec::new(),
        };
        assert_eq!(validate_recovered_mailbox_claim(&state, &claim), Ok(()));

        // checkpoint 只能引用同一封权威消息，伪造 ID 不得把其他记录标为 Delivered。
        let mut wrong_message_id = claim.clone();
        wrong_message_id.mailbox_message_ids[0] =
            ResourceMailboxMessageId::new("other-message").unwrap();
        assert_eq!(
            validate_recovered_mailbox_claim(&state, &wrong_message_id),
            Err(AgentRuntimeError::RecoveryRequired)
        );

        // 目标路由、来源 Turn 和正文任一不一致都必须停止恢复。
        let mut wrong_route = claim.clone();
        wrong_route.mailbox_messages[0].target_agent_id =
            keencode_agent::AgentId::new("other-target").unwrap();
        assert_eq!(
            validate_recovered_mailbox_claim(&state, &wrong_route),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let mut wrong_source_turn = claim.clone();
        wrong_source_turn.mailbox_messages[0].related_turn_id =
            Some(keencode_agent::TurnId::new("other-turn").unwrap());
        assert_eq!(
            validate_recovered_mailbox_claim(&state, &wrong_source_turn),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let mut wrong_body = claim;
        wrong_body.mailbox_messages[0].content = "篡改正文".to_owned();
        assert_eq!(
            validate_recovered_mailbox_claim(&state, &wrong_body),
            Err(AgentRuntimeError::RecoveryRequired)
        );
    }

    /// 冷恢复的子 Agent Profile 在执行选择前必须移除根专用工具并补齐通信控制面。
    #[test]
    fn recovered_child_profile_is_filtered_before_runtime_tool_selection() {
        let profile = AgentProfile {
            model: "model-a".to_owned(),
            reasoning_effort: None,
            plan_guard: PlanGuard::inactive(),
            cwd: PathBuf::from("D:/workspace/recovered-child"),
            worktree_lease: None,
            tool_snapshot: [
                "Read",
                "spawn_agent",
                "AskUser",
                "TodoWrite",
                "Goal",
                "Plan",
                "CreateWorkflow",
                "SaveWorkflow",
                "GetWorkflowRun",
                "GetWorkflowRunSituation",
                "GetWorkflowRunRoster",
                "SendMessage",
            ]
            .map(str::to_owned)
            .to_vec(),
        };
        assert_eq!(
            runtime_tool_snapshot(&profile, false),
            [
                "Read",
                "SendMessage",
                "list_agents",
                "send_message",
                "followup_task",
                "wait_agent",
            ]
        );
        assert_eq!(runtime_tool_snapshot(&profile, true), profile.tool_snapshot);
        assert_eq!(
            request_tool_snapshot(&profile, true, true),
            ["Read", "Edit", "Write", "Bash"]
        );
        assert_eq!(
            request_tool_snapshot(&profile, false, true),
            ["Read", "Edit", "Write", "Bash"]
        );
    }

    /// 回收本地模型服务并返回唯一捕获的请求正文。
    fn finish_responses_server(server: JoinHandle<Result<Value, String>>) -> Value {
        server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功")
    }

    /// 只用于验证磁盘 Store 的无副作用协作执行端。
    struct NoopCollaborationExecution;

    impl AgentExecutionPort for NoopCollaborationExecution {
        /// Store 测试不会创建 Turn；若意外触发则仍以明确已接受结果保持领域推进。
        fn start_turn(&self, _launch: AgentTurnLaunch) -> AgentTurnStartResult {
            AgentTurnStartResult::Accepted
        }

        /// Store 测试没有运行 Turn，信号无需额外副作用。
        fn signal_turn(&self, _signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
            Ok(())
        }

        /// Store 测试没有运行任务，因此根树已经静止。
        fn quiesce_tree(&self, _request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
            AgentTreeQuiesceResult::Quiesced
        }

        /// Store 测试没有受管 Worktree，因此清理可幂等完成。
        fn close_tree(&self, _request: CloseAgentTree) -> Result<(), CollaborationPortError> {
            Ok(())
        }
    }

    /// 在不启动真实模型 Runner 的前提下装配可查询的测试 Collaboration Runtime。
    fn install_test_collaboration_runtime(
        runtime: &Arc<AgentRuntime>,
        session: &RuntimeSession,
        project_root: &Path,
    ) -> Arc<super::SessionCollaborationRuntime> {
        install_test_collaboration_runtime_with_limit(runtime, session, project_root, 2)
    }

    /// 在不启动真实模型 Runner 的前提下装配指定容量的测试 Collaboration Runtime。
    fn install_test_collaboration_runtime_with_limit(
        runtime: &Arc<AgentRuntime>,
        session: &RuntimeSession,
        project_root: &Path,
        turn_limit: usize,
    ) -> Arc<super::SessionCollaborationRuntime> {
        let limiter =
            Arc::new(CollaborationGlobalTurnLimiter::new(turn_limit).expect("测试容量应有效"));
        install_test_collaboration_runtime_with_shared_limiter(
            runtime,
            session,
            project_root,
            limiter,
            turn_limit,
        )
    }

    /// 使用显式共享 limiter 装配测试 Collaboration Runtime。
    fn install_test_collaboration_runtime_with_shared_limiter(
        runtime: &Arc<AgentRuntime>,
        session: &RuntimeSession,
        project_root: &Path,
        limiter: Arc<CollaborationGlobalTurnLimiter>,
        turn_limit: usize,
    ) -> Arc<super::SessionCollaborationRuntime> {
        let session_id = session.session_id().as_str().to_owned();
        let store = Arc::new(
            SessionCollaborationStore::new(&runtime.storage_root, &session_id)
                .expect("测试 Collaboration Store 应创建"),
        );
        store
            .bind_runtime_session(session)
            .expect("测试 Store 应绑定 Runtime Session");
        let background_tasks = Arc::new(
            keencode_tools::BackgroundTaskManager::new(
                runtime
                    .storage_root
                    .join("background-tasks-test")
                    .join(&session_id),
                1_024,
            )
            .expect("测试后台任务 Manager 应创建"),
        );
        let worktrees = Arc::new(
            GitWorktreeLeaseManager::open(
                runtime
                    .storage_root
                    .join("agent-worktrees-test")
                    .join(&session_id),
            )
            .expect("测试 Worktree Manager 应创建"),
        );
        let persistent_state =
            Arc::new(PersistentAgentState::open(session.clone()).expect("测试持久状态应创建"));
        let execution = Arc::new(super::RuntimeAgentExecution::new(
            super::RuntimeAgentExecutionContext {
                owner: Arc::downgrade(runtime),
                session: session.clone(),
                project_root: project_root.to_path_buf(),
                persistent_state,
                background_tasks: background_tasks.clone(),
                executor_handle: runtime.executor_handle.clone(),
                worktrees,
                store: Arc::clone(&store),
            },
        ));
        let coordinator = Arc::new(CollaborationCoordinator::new_with_global_turn_limiter(
            limiter,
            store.clone(),
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        ));
        execution
            .bind_coordinator(&coordinator)
            .expect("测试执行端应绑定协调器");
        let root_agent_id = keencode_agent::AgentId::new("root").expect("根 Agent ID 应有效");
        coordinator
            .register_root_with_id(
                root_agent_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new(session_id.clone())
                        .expect("Agent Session ID 应有效"),
                    profile: AgentProfile {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                        cwd: project_root.to_path_buf(),
                        worktree_lease: None,
                        tool_snapshot: Vec::new(),
                    },
                    per_root_turn_limit: turn_limit,
                },
            )
            .expect("测试根 Agent 应注册");
        let collaboration = Arc::new(super::SessionCollaborationRuntime {
            coordinator,
            store,
            execution,
            root_agent_id,
            background_completion_cancel: std::sync::Mutex::new(None),
        });
        runtime
            .collaboration_sessions
            .lock()
            .expect("测试 Collaboration 表应可写")
            .insert(session_id, collaboration.clone());
        collaboration
    }

    /// 独立 Coordinator 持有一个共享全局槽位，不污染被测 Session 的恢复快照。
    struct TestGlobalCapacityOccupier {
        coordinator: Arc<CollaborationCoordinator>,
        agent_id: keencode_agent::AgentId,
        turn_id: AgentTurnId,
    }

    impl TestGlobalCapacityOccupier {
        /// 收敛占位 Turn 并释放共享全局槽位。
        fn finish(self) {
            self.coordinator
                .complete_turn(
                    &self.agent_id,
                    &self.turn_id,
                    AgentTurnOutcome::Completed {
                        final_message: None,
                    },
                )
                .expect("测试占位子 Turn 应收敛");
        }
    }

    /// 在独立 Store 中启动一个子 Turn，占用指定共享 limiter 的一个槽位。
    fn occupy_test_global_turn(
        runtime: &AgentRuntime,
        project_root: &Path,
        limiter: Arc<CollaborationGlobalTurnLimiter>,
        suffix: &str,
    ) -> TestGlobalCapacityOccupier {
        let session = runtime
            .open_or_create_session(project_root, None, &format!("capacity-occupier-{suffix}"))
            .expect("占位 Runtime Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let store = Arc::new(
            SessionCollaborationStore::new(&runtime.storage_root, &session_id)
                .expect("占位 Collaboration Store 应创建"),
        );
        store
            .bind_runtime_session(&session)
            .expect("占位 Collaboration Store 应绑定 Session");
        let coordinator = Arc::new(CollaborationCoordinator::new_with_global_turn_limiter(
            limiter,
            store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        ));
        let root_agent_id = keencode_agent::AgentId::new(keencode_resources::ROOT_AGENT_ID)
            .expect("占位根 Agent 标识应有效");
        coordinator
            .register_root_with_id(
                root_agent_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new(session_id)
                        .expect("占位 Session 标识应有效"),
                    profile: AgentProfile {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                        cwd: project_root.to_path_buf(),
                        worktree_lease: None,
                        tool_snapshot: Vec::new(),
                    },
                    per_root_turn_limit: 1,
                },
            )
            .expect("占位根 Agent 应注册");
        let root_turn_id = AgentTurnId::new(format!("turn-capacity-occupier-{suffix}-root"))
            .expect("占位根 Turn 标识应有效");
        coordinator
            .begin_root_turn_with_id(
                &root_agent_id,
                root_turn_id.clone(),
                "占用共享全局子 Agent 槽位",
                PlanGuard::inactive(),
            )
            .expect("占位根 Turn 应启动");
        let child = coordinator
            .spawn_agent(
                &root_agent_id,
                &root_turn_id,
                &ToolCallId::new(format!("spawn-capacity-occupier-{suffix}"))
                    .expect("占位工具调用标识应有效"),
                test_spawn_request("capacity_occupier", project_root),
            )
            .expect("占位子 Agent 应启动");
        assert!(matches!(
            coordinator
                .agent_status(&child.agent.agent_id)
                .expect("占位子 Agent 状态应读取"),
            CollaborationAgentStatus::Running { .. }
        ));
        TestGlobalCapacityOccupier {
            coordinator,
            agent_id: child.agent.agent_id,
            turn_id: child.initial_turn_id,
        }
    }

    /// 创建测试用的单层子 Agent 请求，并使用项目目录作为绝对工作目录。
    fn test_spawn_request(name: &str, project_root: &Path) -> SpawnAgentRequest {
        SpawnAgentRequest {
            task_name: name.to_owned(),
            initial_task: format!("执行 {name} 测试任务"),
            assignment: format!("负责 {name} 测试范围"),
            context_inheritance: ContextInheritance::None,
            context_snapshot: Vec::new(),
            agent_template: None,
            profile: AgentProfile {
                model: "test-model".to_owned(),
                reasoning_effort: None,
                plan_guard: PlanGuard::inactive(),
                cwd: project_root.to_path_buf(),
                worktree_lease: None,
                tool_snapshot: Vec::new(),
            },
        }
    }

    /// 创建带项目指令注入策略的显式模板快照，供真实 Runtime 请求测试使用。
    fn test_agent_template(name: &str, inject_agents_md: bool) -> AgentTemplateSnapshot {
        AgentTemplateSnapshot {
            inject_agents_md,
            name: name.to_owned(),
            system_prompt: format!("模板 {name} 的测试系统说明"),
            max_turns: None,
            allowed_write_dirs: Vec::new(),
        }
    }

    /// 生产装配创建的不同 Session 必须共享设备级 limiter，设置同时覆盖每根树。
    #[tokio::test(flavor = "multi_thread")]
    async fn collaboration_sessions_share_device_limit_and_hot_update_roots() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let first_project = tempfile::tempdir().expect("应创建第一项目目录");
        let second_project = tempfile::tempdir().expect("应创建第二项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let first_session = runtime
            .open_or_create_session(first_project.path(), None, "shared-limit-first")
            .expect("第一 Session 应创建");
        let second_session = runtime
            .open_or_create_session(second_project.path(), None, "shared-limit-second")
            .expect("第二 Session 应创建");
        let seed = || RootAgentSeed {
            model: "test-model".to_owned(),
            reasoning_effort: None,
            plan_guard: PlanGuard::inactive(),
        };
        let first = runtime
            .ensure_collaboration_runtime(&first_session, seed())
            .expect("第一 Collaboration Runtime 应建立");
        let second = runtime
            .ensure_collaboration_runtime(&second_session, seed())
            .expect("第二 Collaboration Runtime 应建立");

        runtime
            .set_background_agent_limit(3)
            .expect("热更新后台 Agent 上限应成功");
        for collaboration in [&first, &second] {
            let capacity = collaboration.coordinator.capacity().unwrap();
            assert_eq!(capacity.global_limit, 3);
            assert_eq!(capacity.roots.len(), 1);
            assert_eq!(capacity.roots[0].2, 3);
        }
        first
            .coordinator
            .update_global_turn_limit(4)
            .expect("任一 Coordinator 应能更新共享 limiter");
        assert_eq!(second.coordinator.capacity().unwrap().global_limit, 4);
        runtime
            .collaboration_global_turn_limiter
            .update_limit(3)
            .expect("测试结束前应恢复 Runtime 设置一致性");

        runtime
            .close_session(first_session.session_id().as_str())
            .await
            .expect("第一 Session 应关闭");
        runtime
            .close_session(second_session.session_id().as_str())
            .await
            .expect("第二 Session 应关闭");
    }

    /// 任一后续 Session 已冻结时，设置预检失败不得先修改较早遍历到的健康根树。
    #[tokio::test(flavor = "multi_thread")]
    async fn background_agent_limit_preflight_failure_keeps_all_limits_unchanged() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let first_project = tempfile::tempdir().expect("应创建第一项目目录");
        let second_project = tempfile::tempdir().expect("应创建第二项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let first_session = runtime
            .open_or_create_session(first_project.path(), None, "atomic-limit-first")
            .expect("第一 Session 应创建");
        let second_session = runtime
            .open_or_create_session(second_project.path(), None, "atomic-limit-second")
            .expect("第二 Session 应创建");
        let seed = || RootAgentSeed {
            model: "test-model".to_owned(),
            reasoning_effort: None,
            plan_guard: PlanGuard::inactive(),
        };
        let first = runtime
            .ensure_collaboration_runtime(&first_session, seed())
            .expect("第一 Collaboration Runtime 应建立");
        let second = runtime
            .ensure_collaboration_runtime(&second_session, seed())
            .expect("第二 Collaboration Runtime 应建立");
        let ordered = runtime
            .collaboration_sessions
            .lock()
            .expect("Collaboration 表应读取")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(ordered.len(), 2);
        let healthy = Arc::clone(&ordered[0]);
        let frozen = Arc::clone(&ordered[1]);
        std::fs::write(&frozen.store.transition_path, b"invalid Collaboration JSON")
            .expect("应破坏后遍历 Session 的 Collaboration 提交文件");
        assert!(matches!(
            frozen.coordinator.close_root_session(&frozen.root_agent_id),
            Err(CollaborationError::StoreRecoveryRequired { .. })
        ));

        let previous_root_limit = healthy.coordinator.capacity().unwrap().roots[0].2;
        let previous_global_limit = runtime
            .collaboration_global_turn_limiter
            .capacity()
            .unwrap()
            .1;
        let previous_setting = runtime.background_agent_limit.load(Ordering::Acquire);
        assert_eq!(
            runtime.set_background_agent_limit(previous_setting + 1),
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        assert_eq!(
            healthy.coordinator.capacity().unwrap().roots[0].2,
            previous_root_limit
        );
        assert_eq!(
            runtime
                .collaboration_global_turn_limiter
                .capacity()
                .unwrap()
                .1,
            previous_global_limit
        );
        assert_eq!(
            runtime.background_agent_limit.load(Ordering::Acquire),
            previous_setting
        );

        for session_id in [
            first_session.session_id().as_str(),
            second_session.session_id().as_str(),
        ] {
            let _ = runtime.close_session(session_id).await;
        }
        drop((first, second));
    }

    /// 设置更新与新 Session 装配并发时，map 锁必须保证新根不会永久保留旧阈值。
    #[tokio::test(flavor = "multi_thread")]
    async fn background_agent_limit_update_races_new_session_without_stale_limit() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "limit-race-session")
            .expect("测试 Session 应创建");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let setter_runtime = Arc::clone(&runtime);
        let setter_barrier = Arc::clone(&barrier);
        let ensure_runtime = Arc::clone(&runtime);
        let ensure_barrier = Arc::clone(&barrier);
        let ensure_session = session.clone();
        let (set_result, collaboration) = std::thread::scope(|scope| {
            let setter = scope.spawn(move || {
                setter_barrier.wait();
                setter_runtime.set_background_agent_limit(2)
            });
            let ensure = scope.spawn(move || {
                ensure_barrier.wait();
                ensure_runtime.ensure_collaboration_runtime(
                    &ensure_session,
                    RootAgentSeed {
                        model: "test-model".to_owned(),
                        reasoning_effort: None,
                        plan_guard: PlanGuard::inactive(),
                    },
                )
            });
            barrier.wait();
            (
                setter.join().expect("设置线程不应 panic"),
                ensure.join().expect("装配线程不应 panic"),
            )
        });
        set_result.expect("并发设置应成功");
        let collaboration = collaboration.expect("并发装配应成功");
        let capacity = collaboration.coordinator.capacity().unwrap();
        assert_eq!(capacity.global_limit, 2);
        assert_eq!(capacity.roots[0].2, 2);
        assert_eq!(runtime.background_agent_limit.load(Ordering::Acquire), 2);

        runtime
            .close_session(session.session_id().as_str())
            .await
            .expect("测试 Session 应关闭");
    }

    /// 冷恢复必须忽略 checkpoint 的旧根阈值，立即采用当前后台 Agent 设置。
    #[tokio::test(flavor = "multi_thread")]
    async fn recovered_root_uses_current_background_agent_limit() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "recovered-limit-session")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let store = Arc::new(
            SessionCollaborationStore::new(storage.path(), &session_id)
                .expect("恢复测试 Store 应创建"),
        );
        store
            .bind_runtime_session(&session)
            .expect("恢复测试 Store 应绑定 Session");
        let seed = CollaborationCoordinator::new(
            CollaborationLimits::new(7).expect("旧全局容量应有效"),
            store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let root_agent_id = keencode_agent::AgentId::new("root").unwrap();
        seed.register_root_with_id(
            root_agent_id.clone(),
            RootAgentRequest {
                session_id: keencode_agent::SessionId::new(session_id.clone()).unwrap(),
                profile: test_spawn_request("recovered_limit_root", project.path()).profile,
                per_root_turn_limit: 7,
            },
        )
        .expect("旧阈值根树应写入 checkpoint");
        drop(seed);

        runtime
            .set_background_agent_limit(2)
            .expect("当前后台 Agent 设置应更新");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "unused".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("旧 checkpoint 应恢复");
        let capacity = collaboration.coordinator.capacity().unwrap();
        assert_eq!(capacity.global_limit, 2);
        assert_eq!(capacity.roots, vec![(root_agent_id, 0, 2)]);
        assert_eq!(
            collaboration
                .coordinator
                .checkpoint_coordinator()
                .unwrap()
                .roots[0]
                .per_root_turn_limit,
            2
        );

        runtime
            .close_session(&session_id)
            .await
            .expect("恢复测试 Session 应关闭");
    }

    /// 为一个已由 Coordinator 生成的等待容量取消记录构造严格配对的双事件。
    fn waiting_capacity_event_pair(
        checkpoint: &RecoveredCoordinator,
        record: &super::UnstartedTurnTerminationRecord,
        first_sequence: u64,
        source_agent_id: Option<keencode_agent::AgentId>,
    ) -> Vec<CollaborationEvent> {
        let agent = super::recovered_agent_for_id(checkpoint, &record.agent_id)
            .expect("等待容量测试记录应能找到 Agent checkpoint");
        let definition = &agent.definition;
        let source_agent_id = source_agent_id.unwrap_or_else(|| record.agent_id.clone());
        let common = |sequence: u64, kind: CollaborationEventKind| CollaborationEvent {
            session_id: definition.session_id.clone(),
            turn_id: Some(record.turn_id.clone()),
            source_agent_id: source_agent_id.clone(),
            agent_id: record.agent_id.clone(),
            parent_agent_id: Some(record.parent_agent_id.clone()),
            agent_path: AgentPath::parse(record.agent_path.clone()).expect("Agent 路径应有效"),
            parent_turn_id: Some(record.parent_turn_id.clone()),
            root_turn_id: Some(record.root_turn_id.clone()),
            sequence,
            kind,
        };
        vec![
            common(first_sequence, CollaborationEventKind::AgentTurnInterrupted),
            common(
                first_sequence + 1,
                CollaborationEventKind::AgentStatusChanged {
                    previous: CollaborationAgentStatus::WaitingCapacity {
                        turn_id: record.turn_id.clone(),
                    },
                    current: CollaborationAgentStatus::Interrupted {
                        turn_id: record.turn_id.clone(),
                    },
                },
            ),
        ]
    }

    /// 复制基础提交并替换测试事件；提取器测试故意只验证事件与 checkpoint 的领域绑定。
    fn commit_with_events(
        base: &CollaborationTransitionCommit,
        events: Vec<CollaborationEvent>,
    ) -> CollaborationTransitionCommit {
        let mut commit = base.clone();
        commit.batch.events = events.clone();
        commit.checkpoint.last_event_sequence = events
            .last()
            .map_or(commit.batch.expected_sequence, |event| event.sequence);
        commit
    }

    /// 创建一个运行中子 Turn 占满容量、并连续留下两个 WaitingCapacity pending 的测试现场。
    async fn two_waiting_capacity_fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<AgentRuntime>,
        RuntimeSession,
        Arc<super::SessionCollaborationRuntime>,
        super::CollaborationTransitionSnapshot,
    ) {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "waiting-capacity-batch")
            .expect("测试 Session 应创建");
        persist_completed_root_turn(
            &session,
            "turn-waiting-capacity-batch-root",
            "等待容量批次根 Turn",
            &root_turn_summary("等待容量批次根 Turn", None, false),
        )
        .await;
        let limiter = Arc::new(CollaborationGlobalTurnLimiter::new(1).expect("测试容量应有效"));
        let occupier = occupy_test_global_turn(
            &runtime,
            project.path(),
            Arc::clone(&limiter),
            "waiting-capacity-batch",
        );
        let collaboration = install_test_collaboration_runtime_with_shared_limiter(
            &runtime,
            &session,
            project.path(),
            limiter,
            1,
        );
        let root_turn = collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                keencode_agent::TurnId::new("turn-waiting-capacity-batch-root")
                    .expect("根 Turn 标识应有效"),
                "等待容量批次根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应入队");
        let first_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-waiting-capacity-batch-first").expect("工具调用标识应有效"),
                test_spawn_request("waiting_capacity_batch_first", project.path()),
            )
            .expect("首个等待容量子 Agent 应创建");
        let second_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-waiting-capacity-batch-second")
                    .expect("工具调用标识应有效"),
                test_spawn_request("waiting_capacity_batch_second", project.path()),
            )
            .expect("第二个等待容量子 Agent 应创建");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&first_child.agent.agent_id)
                .expect("首个子 Agent 状态应读取"),
            CollaborationAgentStatus::WaitingCapacity { .. }
        ));
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&second_child.agent.agent_id)
                .expect("第二个子 Agent 状态应读取"),
            CollaborationAgentStatus::WaitingCapacity { .. }
        ));
        collaboration
            .coordinator
            .cancel_turn(&first_child.agent.agent_id, &first_child.initial_turn_id)
            .expect("首个等待容量 Turn 应取消");
        collaboration
            .coordinator
            .cancel_turn(&second_child.agent.agent_id, &second_child.initial_turn_id)
            .expect("第二个等待容量 Turn 应取消");
        occupier.finish();
        assert_eq!(
            collaboration
                .coordinator
                .capacity()
                .expect("收敛后容量应读取")
                .global_in_use,
            0
        );
        let snapshot = collaboration
            .store
            .load_transition_snapshot()
            .expect("批次 pending 快照应读取")
            .expect("批次取消后 Store 快照应存在");
        assert_eq!(snapshot.unstarted_turn_terminations.len(), 2);
        (storage, project, runtime, session, collaboration, snapshot)
    }

    /// 记录每次按项目冻结扩展候选时实际收到的项目根和阶段。
    struct RecordingExtensionContributor {
        /// 按调用顺序保存阶段与规范项目根。
        calls: Mutex<Vec<(&'static str, PathBuf)>>,
        /// 测试用的候选级诊断快照。
        diagnostics: Vec<RuntimeExtensionDiagnostic>,
        /// 测试用的插件引用快照，模拟候选构建时的公开目录。
        plugin_reference_catalog: Vec<Value>,
        /// 测试用的 Skill 引用快照，模拟候选构建时的公开目录。
        skill_reference_catalog: Vec<Value>,
    }

    impl RecordingExtensionContributor {
        /// 创建尚未收到任何冻结调用的记录贡献器。
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                diagnostics: Vec::new(),
                plugin_reference_catalog: Vec::new(),
                skill_reference_catalog: Vec::new(),
            })
        }

        /// 创建携带固定插件与 Skill 引用目录的测试候选。
        fn with_reference_catalogs(
            plugin_reference_catalog: Vec<Value>,
            skill_reference_catalog: Vec<Value>,
        ) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                diagnostics: Vec::new(),
                plugin_reference_catalog,
                skill_reference_catalog,
            })
        }

        /// 记录一个扩展冻结阶段和可信项目根。
        fn record(&self, phase: &'static str, context: &RuntimeToolContext) {
            self.calls
                .lock()
                .push((phase, context.project_root().to_path_buf()));
        }
    }

    impl RuntimeExtensionContributor for RecordingExtensionContributor {
        /// 返回候选构建时记录的插件引用目录。
        fn plugin_reference_catalog(&self) -> Vec<Value> {
            self.plugin_reference_catalog.clone()
        }

        /// 返回候选构建时记录的 Skill 引用目录。
        fn skill_reference_catalog(&self) -> Vec<Value> {
            self.skill_reference_catalog.clone()
        }

        /// 记录工具注册阶段；测试贡献器不增加额外工具。
        fn register_tools(
            &self,
            _registry: &mut ToolRegistry,
            context: &RuntimeToolContext,
        ) -> Result<(), String> {
            self.record("tools", context);
            Ok(())
        }

        /// 记录 Hook 构建阶段并返回空 Hook 集合。
        fn build_hook_runtime(&self, context: &RuntimeToolContext) -> Result<HookRuntime, String> {
            self.record("hooks", context);
            Ok(HookRuntime::empty())
        }

        /// 记录 LSP 准备阶段。
        fn prepare_lsp_runtime(&self, context: &RuntimeToolContext) -> Result<(), String> {
            self.record("lsp", context);
            Ok(())
        }

        /// 返回测试候选冻结时携带的诊断。
        fn diagnostics(&self) -> &[RuntimeExtensionDiagnostic] {
            &self.diagnostics
        }

        /// 记录 Runtime 是否把撤销请求路由到该项目的唯一贡献器。
        fn revoke_mcp_tools(&self) -> Result<(), String> {
            self.calls.lock().push(("revoke", PathBuf::new()));
            Ok(())
        }

        /// 记录型候选不提供 Agent 模板。
        fn resolve_agent(
            &self,
            name: &str,
            _parent: &RuntimeAgentTemplateContext,
        ) -> Result<Option<RuntimeAgentTemplate>, String> {
            if name != "reviewer" {
                return Ok(None);
            }
            Ok(Some(RuntimeAgentTemplate {
                name: name.to_owned(),
                inject_agents_md: true,
                system_prompt: "审查实际变更".to_owned(),
                model: Some("provider-a::model-a".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                tool_names: Some(vec!["Read".to_owned()]),
                disallowed_tool_names: Vec::new(),
                max_turns: Some(4),
                allowed_write_dirs: Vec::new(),
            }))
        }
    }

    /// 通过真实 Runtime 提交一个完成的根 Turn，供屏障和幂等测试复用。
    async fn persist_completed_root_turn(
        session: &RuntimeSession,
        turn_id: &str,
        prompt: &str,
        prompt_summary: &str,
    ) {
        let capabilities = ProviderCapabilities::default();
        let provider = Arc::new(ScriptedProvider::new(
            capabilities.clone(),
            [completed_reply("完成")],
        ));
        let provider_snapshot = ProviderSnapshot {
            provider_id: "scripted-provider".to_owned(),
            model: "test-model".to_owned(),
            context_window: capabilities.max_context_tokens,
            protocol: ProviderProtocolSnapshot::OpenAiResponses,
            config_fingerprint: "scripted-provider-config".to_owned(),
            reasoning_effort: None,
        };
        let input = ModelMessage::text(MessageRole::User, prompt);
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("测试 Session 标识应有效"),
            keencode_agent::TurnId::new(turn_id).expect("测试 Turn 标识应有效"),
            keencode_agent::AgentId::new("root").expect("测试根 Agent 标识应有效"),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        session
            .bind_agent_runner(AgentRunner::new(
                provider,
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(
                RuntimeTurnRequest::root(request, vec![input], prompt_summary)
                    .with_provider_snapshot(provider_snapshot),
            )
            .await
            .expect("测试根 Turn 应完成");
    }

    /// pending read-side 只能返回父连接可见的 actor 问题，并沿真实问题 Schema 回答。
    #[tokio::test]
    async fn workflow_pending_views_enforce_parent_connection_and_route() {
        let storage = tempfile::tempdir().expect("应创建问答测试存储目录");
        let runtime =
            AgentRuntime::new_for_control_test(storage.path()).expect("测试 Runtime 应创建");
        let parent_connection =
            ConnectionId::new("workflow-pending-parent").expect("父连接标识应合法");
        let other_connection =
            ConnectionId::new("workflow-pending-other").expect("其他连接标识应合法");
        runtime
            .elicitations
            .bind_native_session("workflow-parent", &parent_connection)
            .expect("父 Session 应绑定连接");
        runtime
            .bind_workflow_actor_elicitation("workflow-actor", "workflow-parent")
            .expect("actor 应绑定父连接和问答路由");

        let handler = runtime.elicitations.handler_native_projected(
            keencode_agent::SessionId::new("workflow-actor").expect("actor Session 标识应合法"),
            Some("workflow-parent".to_owned()),
            parent_connection.clone(),
        );
        let question = UserQuestionRequest {
            session_id: keencode_agent::SessionId::new("workflow-actor")
                .expect("actor Session 标识应合法"),
            turn_id: keencode_agent::TurnId::new("workflow-pending-turn").expect("Turn 标识应合法"),
            source_agent_id: keencode_agent::AgentId::new("workflow-actor")
                .expect("Agent 标识应合法"),
            tool_call_id: ToolCallId::new("workflow-pending-tool").expect("Tool 标识应合法"),
            questions: vec![UserQuestion {
                id: "strategy".to_owned(),
                prompt: "选择实现策略".to_owned(),
                options: vec![UserQuestionOption {
                    label: "直接实现".to_owned(),
                    description: Some("执行真实实现".to_owned()),
                }],
                multi_select: false,
                allow_custom: true,
            }],
        };
        let answer = tokio::spawn(async move { handler.ask(question).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !runtime
                    .pending_elicitation_views("workflow-parent", &parent_connection)
                    .expect("父连接读取 pending 应成功")
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Native typed pending 应及时登记");

        let views = runtime
            .pending_elicitation_views("workflow-parent", &parent_connection)
            .expect("父连接读取 pending 应成功");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].session_id, "workflow-actor");
        assert_eq!(
            views[0].display_session_id.as_deref(),
            Some("workflow-parent")
        );
        assert_eq!(
            runtime
                .elicitations
                .pending_connection_for_request(&views[0].request_id),
            Some(parent_connection.clone())
        );
        assert!(matches!(
            runtime.pending_elicitation_views("workflow-parent", &other_connection),
            Err(AgentRuntimeError::ClientResponseRejected)
        ));

        runtime
            .resolve_workflow_question(
                "workflow-parent",
                &views[0].request_id,
                r#"{"optionId":"直接实现"}"#,
            )
            .expect("父连接应按原问题 Schema 完成 actor 回答");
        let response = answer
            .await
            .expect("actor AskUser 任务应完成")
            .expect("actor 应收到已解析答案");
        assert_eq!(response.answers[0].values, ["直接实现"]);
    }

    /// 严格外层联合不得增加旧事件名或扁平字段。
    #[tokio::test]
    async fn turn_bound_provider_overrides_identity_for_agent_and_compression_requests() {
        let scripted = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("普通响应"), completed_reply("压缩摘要")],
        ));
        let inner: Arc<dyn ModelProvider> = scripted.clone();
        let bound = Arc::new(TurnBoundProvider::new(
            inner,
            "session-trusted",
            "turn-trusted",
            "agent-trusted",
        ));
        let mut request = ModelRequest::new(
            "test-model",
            vec![ModelMessage::text(MessageRole::User, "普通请求")],
        );
        request.metadata.insert(
            REQUEST_METADATA_SESSION_ID.to_owned(),
            "session-forged".to_owned(),
        );
        request.metadata.insert(
            REQUEST_METADATA_TURN_ID.to_owned(),
            "turn-forged".to_owned(),
        );
        request.metadata.insert(
            REQUEST_METADATA_AGENT_ID.to_owned(),
            "agent-forged".to_owned(),
        );
        request
            .metadata
            .insert(REQUEST_METADATA_PURPOSE.to_owned(), "title".to_owned());
        let stream = bound
            .stream(request)
            .await
            .expect("普通请求应进入脚本 Provider");
        drop(stream);

        let compressor_provider: Arc<dyn ModelProvider> = bound;
        ProviderContextCompressor::new(compressor_provider)
            .summarize(
                ContextSummaryRequest {
                    model: "test-model".to_owned(),
                    messages: vec![ModelMessage::text(MessageRole::User, "待压缩历史")],
                    max_output_tokens: 128,
                },
                TurnCancellation::new(),
            )
            .await
            .expect("压缩请求应完成");

        let requests = scripted.requests().expect("应读取脚本请求");
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(
                request.metadata.get(REQUEST_METADATA_SESSION_ID),
                Some(&"session-trusted".to_owned())
            );
            assert_eq!(
                request.metadata.get(REQUEST_METADATA_TURN_ID),
                Some(&"turn-trusted".to_owned())
            );
            assert_eq!(
                request.metadata.get(REQUEST_METADATA_AGENT_ID),
                Some(&"agent-trusted".to_owned())
            );
            assert_eq!(
                request.metadata.get(REQUEST_METADATA_PURPOSE),
                Some(&"agent".to_owned())
            );
        }
    }

    /// 缓存路由键只对 allowlist 内端点装配，且同一会话跨 Turn 值不漂移。
    #[test]
    fn prompt_cache_key_follows_endpoint_allowlist_and_stays_session_stable() {
        let openai = Url::parse("https://api.openai.com/v1").expect("官方端点应可解析");
        let deepseek = Url::parse("https://api.deepseek.com").expect("官方端点应可解析");
        assert_eq!(
            prompt_cache_key_for_endpoint(&openai, "session-a").as_deref(),
            Some("keencode:session-a")
        );
        // DeepSeek 官方不支持 chat completions 的 prompt_cache_key 参数，不入 allowlist。
        assert_eq!(
            prompt_cache_key_for_endpoint(&deepseek, "session-a").as_deref(),
            None
        );
        // 其他主机（第三方网关、本地服务）不发送该字段。
        assert_eq!(
            prompt_cache_key_for_endpoint(
                &Url::parse("https://api.example.com/v1").expect("第三方端点应可解析"),
                "session-a"
            ),
            None
        );
        assert_eq!(
            prompt_cache_key_for_endpoint(
                &Url::parse("http://127.0.0.1:1234/v1").expect("本地端点应可解析"),
                "session-a"
            ),
            None
        );
        // 键值只随 Session 漂移：同会话重复装配得到相同值。
        assert_eq!(
            prompt_cache_key_for_endpoint(&openai, "session-a"),
            prompt_cache_key_for_endpoint(&openai, "session-a")
        );
        assert_eq!(
            prompt_cache_key_for_endpoint(&openai, "session-b").as_deref(),
            Some("keencode:session-b")
        );
    }

    /// 装配点写入的缓存键随请求到达内层 Provider；未装配端点的请求不含该键。
    #[tokio::test]
    async fn turn_bound_provider_propagates_session_prompt_cache_key_only_when_assembled() {
        let scripted = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                completed_reply("第一轮"),
                completed_reply("第二轮"),
                completed_reply("未装配请求"),
            ],
        ));
        let openai = Url::parse("https://api.openai.com/v1").expect("官方端点应可解析");
        // 模拟同一 Session 的两个 Turn：turn_id 不同，缓存键保持会话稳定。
        for turn_id in ["turn-1", "turn-2"] {
            let inner: Arc<dyn ModelProvider> = scripted.clone();
            let bound = TurnBoundProvider::new(inner, "session-cache", turn_id, "agent-trusted")
                .with_prompt_cache_key(prompt_cache_key_for_endpoint(&openai, "session-cache"));
            let stream = bound
                .stream(ModelRequest::new(
                    "test-model",
                    vec![ModelMessage::text(MessageRole::User, "会话请求")],
                ))
                .await
                .expect("会话请求应进入脚本 Provider");
            drop(stream);
        }
        let inner: Arc<dyn ModelProvider> = scripted.clone();
        let unbound = TurnBoundProvider::new(inner, "session-plain", "turn-3", "agent-trusted");
        let stream = unbound
            .stream(ModelRequest::new(
                "test-model",
                vec![ModelMessage::text(MessageRole::User, "未装配请求")],
            ))
            .await
            .expect("未装配请求应进入脚本 Provider");
        drop(stream);

        let requests = scripted.requests().expect("应读取脚本请求");
        assert_eq!(requests.len(), 3);
        for request in &requests[..2] {
            assert_eq!(
                request.metadata.get(REQUEST_METADATA_PROMPT_CACHE_KEY),
                Some(&"keencode:session-cache".to_owned()),
                "allowlist 内端点的每个 Turn 都携带同一会话缓存键"
            );
        }
        assert!(
            !requests[2]
                .metadata
                .contains_key(REQUEST_METADATA_PROMPT_CACHE_KEY),
            "allowlist 外端点的请求不得携带缓存键"
        );
    }

    /// 重复发送只注入一份规则；动态背景在用户任务之前，压缩保持独立。
    #[tokio::test]
    async fn agent_prompt_provider_boundary_is_complete_and_request_only() {
        let scripted = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                completed_reply("first"),
                completed_reply("second"),
                completed_reply("child"),
                completed_reply("summary"),
            ],
        ));
        let mut dynamic = ModelMessage::text(MessageRole::User, "dynamic");
        dynamic.is_meta = true;
        let bound = TurnBoundProvider::new(scripted.clone(), "session", "turn", "root")
            .with_stable_prefix(stable_agent_prefix(true, true, ""))
            .with_request_context(vec![dynamic.clone()]);
        let mut request = ModelRequest::new(
            "test-model",
            vec![ModelMessage::text(MessageRole::User, "task")],
        );
        request.tools = ["spawn_agent", "Skill"]
            .into_iter()
            .map(|name| {
                keencode_model::ToolDefinition::new(
                    name,
                    "test",
                    serde_json::json!({"type":"object","properties":{}}),
                )
            })
            .collect();
        for _ in 0..2 {
            drop(bound.stream(request.clone()).await.unwrap());
        }
        assert_eq!(request.messages.len(), 1);
        let child = TurnBoundProvider::new(scripted.clone(), "session", "child-turn", "child")
            .with_stable_prefix(stable_agent_prefix(false, false, ""));
        request.tools.clear();
        drop(child.stream(request.clone()).await.unwrap());
        let compressor = TurnBoundProvider::new(scripted.clone(), "session", "turn", "root");
        drop(compressor.stream(request).await.unwrap());
        let requests = scripted.requests().unwrap();
        for model_request in &requests[..3] {
            assert_eq!(
                model_request.messages[0],
                ModelMessage::text(MessageRole::System, crate::agent_prompt::core())
            );
            assert_eq!(
                model_request
                    .messages
                    .iter()
                    .filter(|message| **message == model_request.messages[0])
                    .count(),
                1
            );
        }
        assert_eq!(requests[0].messages, requests[1].messages);
        assert_eq!(
            requests[0].messages[1],
            ModelMessage::text(
                MessageRole::System,
                crate::agent_prompt::capabilities(true, true)
            )
        );
        // 背景不伪装为最新输入，真实任务保持末尾。
        let expected_root_prefix = stable_agent_prefix(true, true, "");
        assert_eq!(
            &requests[0].messages[..expected_root_prefix.len() + 1],
            &expected_root_prefix
                .into_iter()
                .chain([dynamic.clone()])
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(
            requests[0].messages.last(),
            Some(&ModelMessage::text(MessageRole::User, "task"))
        );
        assert_eq!(
            requests[2].messages[1],
            ModelMessage::text(
                MessageRole::System,
                crate::agent_prompt::capabilities(false, false)
            )
        );
        assert_eq!(
            requests[3].messages.as_slice(),
            &[ModelMessage::text(MessageRole::User, "task")]
        );
    }

    #[test]
    fn agent_prompt_background_never_becomes_a_new_user_turn_after_tools() {
        let bound = TurnBoundProvider::new(
            Arc::new(ScriptedProvider::new(ProviderCapabilities::default(), [])),
            "session",
            "turn",
            "root",
        )
        .with_stable_prefix(vec![ModelMessage::text(MessageRole::System, "rules")])
        .with_request_context(vec![
            ModelMessage::text(MessageRole::Developer, "environment"),
            ModelMessage::text(MessageRole::User, "retrieved background"),
        ]);
        let mut request = ModelRequest::new(
            "model",
            vec![ModelMessage::text(MessageRole::User, "investigate")],
        );
        for round in 0..3 {
            let id = format!("read-{round}");
            request.append_messages(vec![
                ModelMessage::new(
                    MessageRole::Assistant,
                    vec![keencode_model::ContentBlock::ToolCall {
                        tool_call: keencode_model::ToolCall::new(
                            &id,
                            "Read",
                            serde_json::json!({"path":"evidence.txt"}),
                        ),
                    }],
                ),
                ModelMessage::new(
                    MessageRole::Tool,
                    vec![keencode_model::ContentBlock::ToolResult {
                        tool_result: keencode_model::ToolResult::text(&id, "same evidence", false),
                    }],
                ),
            ]);
            let original = request.messages.clone();
            let mut outgoing = request.clone();
            bound.inject_context(&mut outgoing);
            bound.inject_context(&mut outgoing);
            assert_eq!(outgoing.messages.len(), original.len() + 3);
            assert_eq!(outgoing.messages[1].role, MessageRole::Developer);
            assert_eq!(outgoing.messages[2].role, MessageRole::User);
            assert_eq!(&outgoing.messages[3..], original.as_slice());
            assert_eq!(outgoing.messages.last().unwrap().role, MessageRole::Tool);
            assert_eq!(request.messages, original);
        }
    }

    /// #24 桌面真实装配边界：前后缀注入与跨 Round 追加都不得深拷贝历史正文。
    #[tokio::test]
    async fn turn_bound_provider_preserves_history_allocations_across_rounds() {
        let history = (0..100)
            .map(|index| {
                ModelMessage::text(
                    MessageRole::User,
                    format!("桌面历史-{index}-{}", "历".repeat(1024)),
                )
            })
            .collect::<Vec<_>>();
        let history_pointers = history
            .iter()
            .map(|message| {
                let keencode_model::ContentBlock::Text { text } = &message.content[0] else {
                    panic!("历史测试消息必须为文本");
                };
                text.as_ptr()
            })
            .collect::<Vec<_>>();
        let scripted = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("first"), completed_reply("second")],
        ));
        let stable_prefix = stable_agent_prefix(true, true, "");
        let prefix_len = stable_prefix.len() + 1;
        let mut dynamic = ModelMessage::text(MessageRole::User, "dynamic");
        dynamic.is_meta = true;
        let bound = TurnBoundProvider::new(scripted.clone(), "session", "turn", "root")
            .with_stable_prefix(stable_prefix)
            .with_request_context(vec![dynamic]);
        let mut request = ModelRequest::new("test-model", history);

        drop(bound.stream(request.clone()).await.unwrap());
        request.append_messages(vec![
            ModelMessage::text(MessageRole::Assistant, "round-one"),
            ModelMessage::text(MessageRole::User, "continue"),
        ]);
        drop(bound.stream(request).await.unwrap());

        let requests = scripted.requests().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), requests[0].messages.len() + 2);
        for (index, expected) in history_pointers.into_iter().enumerate() {
            for request in &requests {
                let keencode_model::ContentBlock::Text { text } =
                    &request.messages[prefix_len + index].content[0]
                else {
                    panic!("Provider 历史消息必须为文本");
                };
                assert_eq!(text.as_ptr(), expected, "桌面第 {index} 条历史发生了深拷贝");
            }
        }
    }

    /// 请求期的大目录必须触发预压缩，但不能进入摘要正文或持久化替换范围。
    #[tokio::test]
    async fn agent_prompt_budget_includes_request_context_without_summarizing_it() {
        let scripted = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                completed_reply("Retained historical fact."),
                completed_reply("done"),
            ],
        ));
        let instructions = "  RAW_GLOBAL_AND_PROJECT_INSTRUCTIONS\n".repeat(100);
        let bound = Arc::new(
            TurnBoundProvider::new(scripted.clone(), "session", "turn", "root")
                .with_stable_prefix(stable_agent_prefix(false, false, &instructions))
                .with_request_context(vec![ModelMessage::text(
                    MessageRole::User,
                    "REQUEST_ONLY_DYNAMIC_CONTEXT ".repeat(600),
                )]),
        );
        let mut request = ModelRequest::new("test-model", Vec::new());
        for _ in 0..12 {
            request.messages_mut().push(ModelMessage::text(
                MessageRole::User,
                "historical fact ".repeat(80),
            ));
            request.messages_mut().push(ModelMessage::text(
                MessageRole::Assistant,
                "historical result ".repeat(80),
            ));
        }
        request
            .messages_mut()
            .push(ModelMessage::text(MessageRole::User, "Continue the task."));
        request.max_output_tokens = Some(128);
        let original = request.messages.clone();
        let context = ContextManager::new(
            ContextPolicy {
                // 本用例验证请求期上下文的预算与隔离，不验证多块递归摘要；
                // 收紧摘要输出预算，确保历史在一次摘要调用中完整容纳。
                summary_max_output_tokens: 256,
                ..ContextPolicy::default()
            },
            bound.clone(),
            Arc::new(ProviderContextCompressor::new(Arc::new(
                TurnBoundProvider::new(scripted.clone(), "session", "turn", "root"),
            ))),
        )
        .unwrap();
        let capabilities = ProviderCapabilities {
            max_context_tokens: Some(context.estimate_request(&request) + 128),
            ..ProviderCapabilities::default()
        };
        assert!(
            ContextManager::for_provider(scripted.clone())
                .precompression_target(&request, &capabilities)
                .is_none()
        );
        let target = context
            .precompression_target(&request, &capabilities)
            .expect("完整请求必须触发预算压缩");
        let outcome = context
            .compact_with_capabilities(
                &request,
                keencode_agent::ContextCompressionTrigger::Budget,
                target,
                &capabilities,
                &TurnCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.record.apply(&original).unwrap(), outcome.messages);
        assert!(
            !outcome
                .record
                .summary
                .contains("RAW_GLOBAL_AND_PROJECT_INSTRUCTIONS")
        );
        assert!(
            !outcome
                .record
                .summary
                .contains("REQUEST_ONLY_DYNAMIC_CONTEXT")
        );
        request.messages = outcome.messages.into();
        assert_eq!(
            outcome.record.estimated_tokens_after,
            context.estimate_request(&request)
        );
        drop(bound.stream(request).await.unwrap());
        let requests = scripted.requests().unwrap();
        assert!(
            !serde_json::to_string(&requests[0])
                .unwrap()
                .contains("RAW_GLOBAL_AND_PROJECT_INSTRUCTIONS")
        );
        assert!(
            !serde_json::to_string(&requests[0])
                .unwrap()
                .contains("REQUEST_ONLY_DYNAMIC_CONTEXT")
        );
        let mut sent = requests[1].clone();
        // 观测 metadata 不属于模型输入；比较相同 JSON 估算口径下的完整消息和工具。
        sent.metadata.clear();
        assert!(
            outcome.record.estimated_tokens_after
                >= JsonContextTokenEstimator.estimate_request(&sent)
        );
        let sent_json = serde_json::to_string(&sent).unwrap();
        assert!(sent_json.contains("RAW_GLOBAL_AND_PROJECT_INSTRUCTIONS"));
        assert!(sent_json.contains("REQUEST_ONLY_DYNAMIC_CONTEXT"));
    }

    /// 能力指纹与稳定前缀一致时，预算口径和实际装配保持同一规则，不修改原消息。
    #[test]
    fn agent_prompt_budget_follows_tools_and_preserves_raw_request() {
        let scripted = Arc::new(ScriptedProvider::new(ProviderCapabilities::default(), []));
        let empty_bound = TurnBoundProvider::new(
            scripted.clone() as Arc<dyn keencode_model::ModelProvider>,
            "session",
            "turn",
            "summary",
        );
        for names in [vec![], vec!["Skill"], vec!["spawn_agent", "Skill"]] {
            let can_spawn = names.contains(&"spawn_agent");
            let has_skill = names.contains(&"Skill");
            let bound = TurnBoundProvider::new(scripted.clone(), "session", "turn", "root")
                .with_stable_prefix(stable_agent_prefix(can_spawn, has_skill, ""));
            let mut request = ModelRequest::new(
                "test-model",
                vec![ModelMessage::text(MessageRole::User, "task")],
            );
            request.tools = names
                .into_iter()
                .map(|name| {
                    keencode_model::ToolDefinition::new(
                        name,
                        "test",
                        serde_json::json!({"type":"object","properties":{}}),
                    )
                })
                .collect();
            let mut injected = request.clone();
            bound.inject_context(&mut injected);
            let estimate = bound.estimate_request(&request);
            let assembled = JsonContextTokenEstimator.estimate_request(&injected);
            assert!(estimate >= assembled && estimate < assembled + 32);
            assert_eq!(request.messages.len(), 1);
            assert_eq!(
                empty_bound.estimate_request(&request),
                JsonContextTokenEstimator.estimate_request(&request)
            );
            assert_eq!(
                bound.estimate_messages(&request.messages),
                JsonContextTokenEstimator.estimate_messages(&request.messages)
            );
        }
    }

    /// 全局/项目指令和 Memory、Plan、Ultra 只进入真实模型请求，不污染 Runtime Transcript。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_turn_dynamic_context_is_request_only_and_not_persisted() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let (base_url, server) = spawn_buffered_responses_server("动态上下文测试完成");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "dynamic-context-operation")
            .expect("动态上下文测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let dynamic_context = "本轮 Memory、Plan、Ultra 动态约束";
        std::fs::write(storage.path().join("AGENTS.md"), "全局指令测试标记").unwrap();
        std::fs::write(project.path().join("AGENTS.md"), "项目指令测试标记").unwrap();

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-dynamic-context",
                    "检查动态上下文持久化边界",
                    RootTurnOptions {
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: Some(dynamic_context.to_owned()),
                        plan_enabled: false,
                        elicitation_connection_id: None,
                    },
                )
                .await
                .expect("带动态上下文的根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        let request = finish_responses_server(server);
        let input = request["input"]
            .as_array()
            .expect("Responses 请求应包含 input 数组");
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(input[0]["content"][0]["text"], crate::agent_prompt::core());
        // Memory/Plan/Ultra 保留用户角色，但位于真实对话历史之前。
        assert!(input.iter().any(|message| {
            message["role"] == "user" && message["content"][0]["text"] == dynamic_context
        }));
        assert_eq!(
            input
                .iter()
                .filter(|message| {
                    message["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("项目指令测试标记"))
                })
                .count(),
            1
        );
        let static_tail = input[1]["content"][0]["text"].as_str().unwrap();
        let has_skill = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "Skill");
        assert_eq!(
            static_tail,
            format!(
                "{}\n\n全局指令测试标记\n\n项目指令测试标记",
                crate::agent_prompt::capabilities(true, has_skill)
            )
        );
        assert!(input.iter().any(|message| {
            message["role"] == "user" && message["content"][0]["text"] == "检查动态上下文持久化边界"
        }));
        let position = |needle: &str| {
            input
                .iter()
                .position(|message| {
                    message["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(needle))
                })
                .unwrap()
        };
        assert!(position("项目指令测试标记") < position("<env>"));
        assert!(position("<env>") < position(dynamic_context));
        assert_eq!(
            input
                .last()
                .and_then(|message| message["content"][0]["text"].as_str()),
            Some("检查动态上下文持久化边界"),
            "真实用户任务必须位于背景之后"
        );
        let environment = input[position("<env>")]["content"][0]["text"]
            .as_str()
            .unwrap();
        let date = environment
            .lines()
            .find_map(|line| line.strip_prefix("Current date: "))
            .unwrap();
        assert_eq!(date.len(), 10);
        assert!(chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok());
        assert!(environment.contains(&format!(
            "Time zone: {}",
            iana_time_zone::get_timezone().unwrap()
        )));

        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime
            .session_has_active_work(&session_id)
            .expect("动态上下文测试活动状态应读取")
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !runtime
                .session_has_active_work(&session_id)
                .expect("动态上下文测试 Turn 应收敛")
        );

        let transcript = runtime
            .session_transcript(&session_id)
            .expect("动态上下文测试 Transcript 应读取");
        let transcript_json = serde_json::to_string(&transcript).expect("Transcript 应可编码");
        assert!(!transcript_json.contains(dynamic_context));
        assert!(!transcript_json.contains("全局指令测试标记"));
        assert!(!transcript_json.contains("项目指令测试标记"));
        assert!(!transcript_json.contains("You are an interactive software engineering agent."));
        assert!(!transcript_json.contains("Current mode:"));
        assert!(transcript_json.contains("检查动态上下文持久化边界"));
    }

    /// 指令在会话首次 Turn 前冻结：中途修改 AGENTS.md 不影响本会话，新会话才取新值。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_turn_freezes_instructions_until_new_session() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        std::fs::write(storage.path().join("AGENTS.md"), "旧全局指令标记")
            .expect("旧全局指令应写入");
        std::fs::write(project.path().join("AGENTS.md"), "旧项目指令标记")
            .expect("旧项目指令应写入");
        let (base_url, server) =
            spawn_buffered_responses_server_for_requests("两轮上下文测试完成", 3);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "frozen-instructions-first")
            .expect("指令冻结测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-instructions-old",
                    "第一轮读取旧指令",
                    RootTurnOptions::default(),
                )
                .await
                .expect("第一轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;

        std::fs::write(storage.path().join("AGENTS.md"), "新全局指令标记")
            .expect("新全局指令应覆盖保存");
        std::fs::remove_file(project.path().join("AGENTS.md")).expect("旧项目指令应从测试项目移除");
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-instructions-frozen",
                    "第二轮沿用冻结指令",
                    RootTurnOptions::default(),
                )
                .await
                .expect("第二轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;

        // 新会话在首次 Turn 前重新冻结，取得修改后的全局指令与已删除的项目指令。
        let next_session = runtime
            .open_or_create_session(project.path(), None, "frozen-instructions-second")
            .expect("指令冻结测试新 Session 应创建");
        let next_session_id = next_session.session_id().as_str().to_owned();
        mark_session_title_manual(&next_session);
        assert_ne!(session_id, next_session_id);
        assert_eq!(
            runtime
                .start_root_turn(
                    &next_session_id,
                    "turn-instructions-refreshed",
                    "新会话读取新指令",
                    RootTurnOptions::default(),
                )
                .await
                .expect("新会话根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &next_session_id).await;

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 3);
        let developer_context = |request: &Value| {
            request["input"]
                .as_array()
                .expect("Responses 请求应包含 input 数组")
                .iter()
                .filter_map(|message| {
                    (message["role"] == "developer")
                        .then(|| message["content"][0]["text"].as_str())
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let first_context = developer_context(&requests[0]);
        assert!(first_context.contains("旧全局指令标记"));
        assert!(first_context.contains("旧项目指令标记"));
        assert!(!first_context.contains("新全局指令标记"));
        // 冻结语义：同一会话的第二轮仍使用首轮冻结的指令正文。
        let second_context = developer_context(&requests[1]);
        assert_eq!(first_context, second_context);
        let third_context = developer_context(&requests[2]);
        assert!(third_context.contains("新全局指令标记"));
        assert!(!third_context.contains("旧全局指令标记"));
        assert!(!third_context.contains("旧项目指令标记"));

        let transcript_json = serde_json::to_string(
            &runtime
                .session_transcript(&session_id)
                .expect("两轮指令测试 Transcript 应读取"),
        )
        .expect("两轮指令测试 Transcript 应可编码");
        for marker in ["旧全局指令标记", "旧项目指令标记", "新全局指令标记"] {
            assert!(!transcript_json.contains(marker));
        }
        assert!(transcript_json.contains("第一轮读取旧指令"));
        assert!(transcript_json.contains("第二轮沿用冻结指令"));
    }

    /// 同一 Session 连续三轮：冻结规则与历史不改写，动态背景放在真实输入之前。
    #[tokio::test(flavor = "multi_thread")]
    async fn session_prefix_is_byte_stable_across_turns() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let (base_url, server) =
            spawn_buffered_responses_server_for_requests("前缀稳定测试完成", 3);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "prefix-stable-operation")
            .expect("前缀稳定测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-prefix-stable-first",
                    "前缀稳定第一轮",
                    RootTurnOptions::default(),
                )
                .await
                .expect("第一轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-prefix-stable-second",
                    "前缀稳定第二轮",
                    RootTurnOptions {
                        // 模拟 Memory/Plan 在两轮之间变化，不应改写既有历史。
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: Some("本轮动态记忆标记".to_owned()),
                        plan_enabled: false,
                        elicitation_connection_id: None,
                    },
                )
                .await
                .expect("第二轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-prefix-stable-third",
                    "前缀稳定第三轮",
                    RootTurnOptions::default(),
                )
                .await
                .expect("第三轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 3);
        fn input(request: &Value) -> &Vec<Value> {
            request["input"]
                .as_array()
                .expect("Responses 请求应包含 input 数组")
        }
        let items_json = |items: &[Value]| {
            items
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .expect("输入项应可序列化")
        };
        let env_text = |items: &[Value]| {
            items
                .iter()
                .filter_map(|message| message["content"][0]["text"].as_str())
                .find(|text| text.contains("<env>"))
                .expect("请求应包含 <env> 环境消息")
                .to_owned()
        };
        let first = input(&requests[0]);
        let second = input(&requests[1]);
        let third = input(&requests[2]);
        // 环境始终存在，第二轮多一条 Memory/Plan/Ultra，但最新输入仍是任务。
        assert!(env_text(first).contains("Current mode: Normal"));
        let background_index = second
            .iter()
            .position(|message| message["content"][0]["text"] == "本轮动态记忆标记")
            .expect("本轮背景必须存在");
        let first_task_index = second
            .iter()
            .position(|message| message["content"][0]["text"] == "前缀稳定第一轮")
            .unwrap();
        assert!(background_index < first_task_index);
        assert_eq!(
            second.last().unwrap()["content"][0]["text"],
            "前缀稳定第二轮"
        );
        let second_without_background = second
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != background_index)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        // 剔除本轮新增背景后，已有规则和历史逐字节保留。
        assert_eq!(
            items_json(&second_without_background[..first.len()]),
            items_json(first)
        );
        assert_eq!(
            items_json(&third[..second_without_background.len()]),
            items_json(&second_without_background)
        );
        // 环境消息来自会话冻结快照：三轮正文逐字节相同，跨轮不重取时钟。
        assert_eq!(env_text(first), env_text(second));
        assert_eq!(env_text(second), env_text(third));
        // 动态上下文不入 Transcript：第三轮请求历史中不得再出现第二轮记忆正文。
        assert!(!items_json(third).join("\n").contains("本轮动态记忆标记"));
    }

    /// 工具轮与跨 Turn 的历史前缀逐字节稳定：第 1 轮带 reasoning 与工具调用、
    /// 第 2 轮纯文本。轮内工具 Round 与跨 Turn 历史请求必须复用同一 Provider
    /// 链路的完整 reasoning item，保证工具 Round 的缓存前缀可以跨 Turn 复用。
    #[tokio::test(flavor = "multi_thread")]
    async fn session_prefix_is_byte_stable_across_tool_turns() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let note_path = project.path().join("note.txt");
        std::fs::write(&note_path, "工具轮读取正文").expect("应写入测试文件");
        let tool_arguments = json!({"file_path": note_path.to_string_lossy()}).to_string();
        let (base_url, server) = spawn_buffered_responses_sequence(vec![
            // 第 1 轮 Round 1：reasoning item 加 Read 工具调用，模型以 tool_use 收束。
            json!({
                "id": "response-tool-prefix-round", "object": "response", "model": "test-model",
                "status": "completed",
                "output": [
                    {"type": "reasoning", "id": "rs-tool-prefix",
                     "summary": [{"type": "summary_text", "text": "工具轮思考"}]},
                    {"type": "function_call", "id": "fc-tool-prefix", "call_id": "call-tool-prefix",
                     "name": "Read", "arguments": tool_arguments, "status": "completed"}
                ],
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            }),
            // 第 1 轮 Round 2：工具结果回流后输出最终文本，Turn 正常完成。
            json!({
                "id": "response-tool-prefix-final", "object": "response", "model": "test-model",
                "status": "completed",
                "output": [{"id": "message-tool-prefix", "type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "第一轮工具轮完成"}]}],
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            }),
            // 第 2 轮：纯文本 Turn。
            json!({
                "id": "response-tool-prefix-second", "object": "response", "model": "test-model",
                "status": "completed",
                "output": [{"id": "message-tool-prefix-second", "type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "第二轮纯文本完成"}]}],
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            }),
        ]);
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                tool_calling: true,
                ..ProviderCapabilities::default()
            }),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "prefix-stable-tool-operation")
            .expect("前缀稳定工具测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-prefix-tool-first",
                    "前缀稳定工具第一轮",
                    RootTurnOptions::default(),
                )
                .await
                .expect("第一轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;
        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-prefix-tool-second",
                    "前缀稳定工具第二轮",
                    RootTurnOptions {
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: Some("本轮工具轮动态记忆标记".to_owned()),
                        plan_enabled: false,
                        elicitation_connection_id: None,
                    },
                )
                .await
                .expect("第二轮根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        wait_for_session_idle(&runtime, &session_id).await;

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 3);
        fn input(request: &Value) -> &Vec<Value> {
            request
                .get("input")
                .and_then(Value::as_array)
                .expect("Responses 请求应包含 input 数组")
        }
        let items_json = |items: &[Value]| {
            items
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .expect("输入项应可序列化")
        };
        // 前置事实：第 1 轮 Round 2（工具结果回流请求）线上携带完整 reasoning item。
        let tool_round_input = input(&requests[1]);
        assert!(
            items_json(tool_round_input)
                .join("\n")
                .contains("rs-tool-prefix"),
            "轮内工具 Round 请求应回放 reasoning item"
        );
        // 本轮背景改变缓存前缀，但不能改写已有推理、工具调用及工具结果。
        let second_input = input(&requests[2]);
        let second_without_background = second_input
            .iter()
            .filter(|message| message["content"][0]["text"] != "本轮工具轮动态记忆标记")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            items_json(&second_without_background[..tool_round_input.len()]),
            items_json(tool_round_input),
        );
        assert!(
            items_json(second_input)
                .join("\n")
                .contains("第一轮工具轮完成"),
            "第 2 轮请求历史应包含第 1 轮最终 assistant 消息"
        );
    }

    /// 子 Agent 在自身首次 Turn 前用自己的 cwd 冻结环境；根与子的环境互不串扰。
    #[tokio::test(flavor = "multi_thread")]
    async fn child_agent_freezes_environment_with_own_cwd() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let child_cwd = tempfile::tempdir().expect("应创建子 Agent 工作目录");
        let (base_url, gate, server) = spawn_gated_buffered_responses_server(
            "子 Agent 环境测试完成",
            2,
            "保持根 Turn 活跃以启动子 Agent",
        );
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "child-env-operation")
            .expect("子 Agent 环境测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .start_root_turn(
                &session_id,
                "turn-child-env-root",
                "保持根 Turn 活跃以启动子 Agent",
                RootTurnOptions::default(),
            )
            .await
            .expect("根 Turn 应启动");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("生产 Collaboration Runtime 应可复用");
        let root_turn =
            keencode_agent::TurnId::new("turn-child-env-root").expect("根 Turn 标识应有效");
        let child_result = collaboration.coordinator.spawn_agent(
            &collaboration.root_agent_id,
            &root_turn,
            &ToolCallId::new("spawn-child-env").expect("子 Agent 工具调用标识应有效"),
            test_spawn_request("child_env", child_cwd.path()),
        );
        gate.release();
        child_result.expect("子 Agent 应经真实 execution port 启动");
        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 2);
        let env_text = |request: &Value| {
            request["input"]
                .as_array()
                .expect("Responses 请求应包含 input 数组")
                .iter()
                .filter_map(|message| message["content"][0]["text"].as_str())
                .find(|text| text.contains("<env>"))
                .expect("请求应包含 <env> 环境消息")
                .to_owned()
        };
        let root_request = requests
            .iter()
            .find(|request| request_contains_user_text(request, "保持根 Turn 活跃以启动子 Agent"))
            .expect("应捕获根 Agent 请求");
        let child_request = requests
            .iter()
            .find(|request| request_contains_user_text(request, "执行 child_env 测试任务"))
            .expect("应捕获子 Agent 请求");
        let root_environment = env_text(root_request);
        let child_environment = env_text(child_request);
        // 根 cwd 经 canonical_project_root 规范化（macOS 带 /private 前缀）；
        // 子 Agent cwd 保持 spawn 传入的原样路径，二者都必须出现在各自环境中。
        let project_canonical = project.path().canonicalize().expect("项目路径应可规范化");
        assert!(root_environment.contains(&format!(
            "Primary working directory: {:?}",
            project_canonical
        )));
        assert!(!root_environment.contains(child_cwd.path().to_string_lossy().as_ref()));
        assert!(child_environment.contains(&format!(
            "Primary working directory: {:?}",
            child_cwd.path()
        )));
        assert!(!child_environment.contains(project_canonical.to_string_lossy().as_ref()));
        wait_for_session_idle(&runtime, &session_id).await;
        runtime
            .close_session(&session_id)
            .await
            .expect("子 Agent 环境测试 Session 应关闭");
    }

    /// 能力指纹变化重建能力段与目录，环境快照与冻结指令保持不变。
    #[test]
    fn frozen_prompt_refresh_rebuilds_capabilities_only() {
        let now =
            chrono::DateTime::parse_from_rfc3339("2026-09-12T08:00:00+08:00").expect("时间应有效");
        let frozen = super::FrozenAgentPrompt {
            environment: crate::agent_prompt::EnvironmentSnapshot::freeze(Path::new("/tmp"), &now),
            custom_instructions: "冻结指令".to_owned(),
            capabilities: crate::agent_prompt::capabilities(true, true),
            capability_fingerprint: (true, true),
            catalog: "旧目录".to_owned(),
            small_context: false,
        };
        let prefix = frozen.stable_prefix();
        assert_eq!(prefix.len(), 3);
        assert_eq!(
            prefix[0],
            ModelMessage::text(MessageRole::System, crate::agent_prompt::core())
        );
        assert_eq!(
            prefix[1],
            ModelMessage::text(
                MessageRole::System,
                format!(
                    "{}\n\n冻结指令",
                    crate::agent_prompt::capabilities(true, true)
                )
            )
        );
        assert_eq!(
            prefix[2],
            ModelMessage::text(MessageRole::Developer, "旧目录")
        );

        let rebuilt = super::FrozenAgentPrompt::refreshed(&frozen, false, true, "新目录", false);
        assert_eq!(rebuilt.capability_fingerprint, (false, true));
        assert_eq!(
            rebuilt.capabilities,
            crate::agent_prompt::capabilities(false, true)
        );
        assert_eq!(rebuilt.catalog, "新目录");
        assert_eq!(rebuilt.custom_instructions, "冻结指令");
        assert_eq!(rebuilt.environment, frozen.environment);
        // 目录为空时稳定前缀不含目录消息。
        assert_eq!(
            super::FrozenAgentPrompt::refreshed(&frozen, false, true, "", false)
                .stable_prefix()
                .len(),
            2
        );

        let small = super::FrozenAgentPrompt::refreshed(&frozen, false, false, "忽略目录", true);
        let small_prefix = small.stable_prefix();
        assert_eq!(small_prefix.len(), 1);
        let keencode_model::ContentBlock::Text { text } = &small_prefix[0].content[0] else {
            panic!("小上下文提示词应为文本");
        };
        assert!(text.contains("expert coding assistant in KeenCode"));
        assert!(!text.contains("冻结指令"));
        assert!(small.catalog.is_empty());
    }

    /// frozen_prompts 软上限收缩：root、活跃 Agent 与即将插入的 Agent 保留，
    /// 其余子 Agent 条目清除；未越过上限时不做任何收缩。
    #[test]
    fn frozen_prompt_retain_keeps_root_and_live_agents_only() {
        let make_frozen = || {
            Arc::new(super::FrozenAgentPrompt {
                environment: crate::agent_prompt::EnvironmentSnapshot::freeze(
                    Path::new("/tmp"),
                    &chrono::DateTime::parse_from_rfc3339("2026-09-12T08:00:00+08:00")
                        .expect("时间应有效"),
                ),
                custom_instructions: String::new(),
                capabilities: String::new(),
                capability_fingerprint: (false, false),
                catalog: String::new(),
                small_context: false,
            })
        };
        let agent_id =
            |value: &str| RunnerAgentId::new(value.to_owned()).expect("测试 Agent 标识应有效");
        let root_id = agent_id(keencode_resources::ROOT_AGENT_ID);
        let live_child = agent_id("child-live");
        let stale_child = agent_id("child-stale");
        let incoming_child = agent_id("child-incoming");
        let mut frozen_prompts: HashMap<RunnerAgentId, Arc<super::FrozenAgentPrompt>> = [
            (root_id.clone(), make_frozen()),
            (live_child.clone(), make_frozen()),
            (stale_child.clone(), make_frozen()),
        ]
        .into_iter()
        .collect();
        // 未超过软上限：即使存在陈旧子 Agent 条目也不收缩。
        super::retain_live_frozen_prompts(
            &mut frozen_prompts,
            [live_child.clone()].into_iter(),
            &incoming_child,
        );
        assert_eq!(frozen_prompts.len(), 3);
        assert!(frozen_prompts.contains_key(&stale_child));

        // 人为越过软上限：收缩后只剩 root、活跃 Agent 与即将插入的 Agent。
        for index in 0..super::FROZEN_PROMPT_SOFT_LIMIT {
            frozen_prompts.insert(agent_id(&format!("child-fill-{index}")), make_frozen());
        }
        // 以已存在条目形式验证"即将插入的 Agent"保留子句（真实调用点随后插入）。
        frozen_prompts.insert(incoming_child.clone(), make_frozen());
        super::retain_live_frozen_prompts(
            &mut frozen_prompts,
            [live_child.clone()].into_iter(),
            &incoming_child,
        );
        assert_eq!(frozen_prompts.len(), 3);
        assert!(frozen_prompts.contains_key(&root_id));
        assert!(frozen_prompts.contains_key(&live_child));
        assert!(frozen_prompts.contains_key(&incoming_child));
        assert!(!frozen_prompts.contains_key(&stale_child));
        assert!(!frozen_prompts.contains_key(&agent_id("child-fill-0")));
    }

    /// 执行前永久拒绝必须建立失败的子 Agent 身份，不能留下根 mailbox 的悬空引用。
    #[tokio::test(flavor = "multi_thread")]
    async fn unstarted_child_rejection_records_failed_journal_identity() {
        let storage = tempfile::tempdir().expect("应创建隔离存储");
        let project = tempfile::tempdir().expect("应创建隔离项目");
        let (base_url, gate, server) = spawn_gated_buffered_responses_server(
            "根任务正常完成",
            1,
            "保持根任务以验证未启动失败",
        );
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "unstarted-rejection-operation")
            .expect("隔离 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .start_root_turn(
                &session_id,
                "turn-unstarted-rejection-root",
                "保持根任务以验证未启动失败",
                RootTurnOptions::default(),
            )
            .await
            .expect("根任务应启动");
        gate.wait_for_requests(1).expect("根请求应抵达回环服务");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("应复用真实协作运行时");
        let mut request = test_spawn_request("rejected_child", project.path());
        request.profile.tool_snapshot = vec!["UnavailableFixtureTool".to_owned()];
        let result = collaboration.coordinator.spawn_agent(
            &collaboration.root_agent_id,
            &keencode_agent::TurnId::new("turn-unstarted-rejection-root").unwrap(),
            &ToolCallId::new("spawn-unstarted-rejection").unwrap(),
            request,
        );
        // 先释放并回收回环服务，失败断言不能遗留挂起请求。
        gate.release();
        let requests = server
            .join()
            .expect("服务线程应退出")
            .expect("回环请求应成功");
        wait_for_session_idle(&runtime, &session_id).await;
        assert!(result.is_err(), "工具快照失效应永久拒绝子 Turn");
        assert_eq!(requests.len(), 1, "拒绝的子任务不得请求 Provider");
        let snapshot = session.snapshot().expect("权威 Session 快照应可读");
        let child = snapshot
            .state
            .sub_agents
            .values()
            .find(|agent| agent.agent_path == "/root/rejected_child");
        assert!(
            child.is_some(),
            "永久拒绝必须补齐 Journal 中的子 Agent 身份"
        );
        assert_eq!(child.unwrap().status, SubAgentStatus::Failed);
        runtime
            .close_session(&session_id)
            .await
            .expect("隔离 Session 应关闭");
    }

    /// 仅接受根任务、确定拒绝子任务的测试端口；不会运行 Provider 或写入 Journal。
    struct RejectUnstartedChildExecution;

    impl AgentExecutionPort for RejectUnstartedChildExecution {
        /// 根用于对齐已持久的测试历史，子任务模拟真实执行端的无副作用预检拒绝。
        fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
            if launch.agent.depth == keencode_agent::AgentDepth::ROOT {
                AgentTurnStartResult::Accepted
            } else {
                AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                    error: CollaborationPortError::new("测试工具快照已失效"),
                }
            }
        }
        /// 该测试端口没有真实 Runner 可以唤醒。
        fn signal_turn(&self, _signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
            Ok(())
        }
        /// 该测试端口没有活跃执行资源。
        fn quiesce_tree(&self, _request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
            AgentTreeQuiesceResult::Quiesced
        }
        /// 该测试端口不创建 Worktree 或外部进程。
        fn close_tree(&self, _request: CloseAgentTree) -> Result<(), CollaborationPortError> {
            Ok(())
        }
    }

    /// 模拟 Journal flush 后尚未 ack 的崩溃窗口，冷重开必须用磁盘 receipt 补齐失败身份。
    #[tokio::test]
    async fn unstarted_failed_pending_receipt_survives_cold_reopen() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = Arc::new(AgentRuntime::new(storage.path()).unwrap());
        let session = runtime
            .open_or_create_session(project.path(), None, "unstarted-pending-cold")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        let root_prompt = "未启动失败冷恢复根任务";
        let root_turn = keencode_agent::TurnId::new("turn-unstarted-pending-root").unwrap();
        persist_completed_root_turn(
            &session,
            root_turn.as_str(),
            root_prompt,
            &root_turn_summary(root_prompt, None, false),
        )
        .await;
        let store =
            Arc::new(super::SessionCollaborationStore::new(storage.path(), &session_id).unwrap());
        store.bind_runtime_session(&session).unwrap();
        let coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(2).unwrap(),
            store.clone(),
            Arc::new(RejectUnstartedChildExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let root_id = keencode_agent::AgentId::new("root").unwrap();
        coordinator
            .register_root_with_id(
                root_id.clone(),
                RootAgentRequest {
                    session_id: keencode_agent::SessionId::new(session_id.clone()).unwrap(),
                    profile: test_spawn_request("unused", project.path()).profile,
                    per_root_turn_limit: 2,
                },
            )
            .unwrap();
        coordinator
            .begin_root_turn_with_id(
                &root_id,
                root_turn.clone(),
                root_prompt,
                PlanGuard::inactive(),
            )
            .unwrap();
        // 协作批次由提交泵线程落盘，Journal 发布失败改用 Store 实例开关注入，
        // 模拟"flush 后尚未 ack 的崩溃窗口"：receipt 留盘、Journal 对账延后。
        store
            .fail_next_unstarted_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let rejected = coordinator.spawn_agent(
            &root_id,
            &root_turn,
            &ToolCallId::new("spawn-cold-failed").unwrap(),
            test_spawn_request("cold_rejected", project.path()),
        );
        assert!(rejected.is_err());
        let pending = store.load_transition_snapshot().unwrap().unwrap();
        assert_eq!(pending.unstarted_turn_terminations.len(), 1);
        let receipt = pending.unstarted_turn_terminations[0].clone();
        coordinator
            .complete_turn(
                &root_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("完成".to_owned()),
                },
            )
            .unwrap();
        assert_eq!(
            store
                .load_transition_snapshot()
                .unwrap()
                .unwrap()
                .unstarted_turn_terminations
                .len(),
            1,
            "不相关提交不能抹掉尚未确认的失败证据"
        );
        drop(coordinator);
        drop(store);
        drop(session);
        runtime.runtime_manager.close(session_id.clone()).unwrap();
        drop(runtime);
        let recovered = Arc::new(AgentRuntime::new(storage.path()).unwrap());
        let reopened = recovered
            .open_or_create_session(project.path(), Some(&session_id), "unused")
            .unwrap();
        let collaboration = recovered
            .ensure_collaboration_runtime(
                &reopened,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("冷恢复必须先对账失败 receipt，再校验权威终态");
        assert!(
            collaboration
                .store
                .load_transition_snapshot()
                .unwrap()
                .unwrap()
                .unstarted_turn_terminations
                .is_empty()
        );
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(
            snapshot
                .state
                .sub_agents
                .get(&ResourceAgentId::new(receipt.agent_id.as_str()).unwrap())
                .unwrap()
                .status,
            SubAgentStatus::Failed
        );
        assert_eq!(
            snapshot
                .state
                .turns
                .get(&ResourceTurnId::new(receipt.turn_id.as_str()).unwrap())
                .unwrap()
                .status,
            TurnStatus::Failed
        );
        assert!(!snapshot.recovery_required);
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&receipt.agent_id)
                .unwrap(),
            CollaborationAgentStatus::Failed { .. }
        ));
        recovered.close_session(&session_id).await.unwrap();
    }

    /// 即时拒绝后，同一个 Session 的下一根 Turn 必须消费失败通知并真实采样。
    #[tokio::test(flavor = "multi_thread")]
    async fn unstarted_rejection_same_session_continues() {
        assert_unstarted_rejection_continuation(false, false, false).await;
    }

    /// 根与活跃子任务占满容量时，排队任务的预检拒绝不得阻断后续根 Turn。
    #[tokio::test(flavor = "multi_thread")]
    async fn unstarted_queued_rejection_same_session_continues() {
        assert_unstarted_rejection_continuation(true, false, false).await;
    }

    /// 未启动失败必须经真实磁盘重开保留，冷恢复不得重新派发失效的子任务。
    #[tokio::test(flavor = "multi_thread")]
    async fn unstarted_queued_rejection_cold_session_continues() {
        assert_unstarted_rejection_continuation(true, true, false).await;
    }

    /// Journal flush 不确定时先保留磁盘失败证据，清除故障后必须幂等补齐且能续作。
    #[tokio::test(flavor = "multi_thread")]
    async fn unstarted_rejection_journal_failure_retries_receipt() {
        assert_unstarted_rejection_continuation(false, false, true).await;
    }

    /// 用同一回环服务验证即时/排队失败及可选冷恢复；只有正常根和活跃子任务可采样。
    async fn assert_unstarted_rejection_continuation(
        queued: bool,
        cold: bool,
        journal_fault: bool,
    ) {
        let storage = tempfile::tempdir().expect("应创建隔离存储");
        let project = tempfile::tempdir().expect("应创建隔离项目");
        let expected_requests = if queued { 3 } else { 2 };
        let (base_url, gates, server) = spawn_gated_buffered_responses_server_with_texts(
            "KC_UNSTARTED_CONTINUED_OK",
            expected_requests,
            &["保持根请求占位", "执行 active_child 测试任务"],
        );
        let mut runtime =
            runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        runtime
            .set_background_agent_limit(1)
            .expect("后台并发应设为1");
        let mut session = runtime
            .open_or_create_session(project.path(), None, "unstarted-continue")
            .expect("Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        runtime
            .start_root_turn(
                &session_id,
                "turn-unstarted-held",
                "保持根请求占位",
                RootTurnOptions::default(),
            )
            .await
            .expect("根 Turn 应启动");
        gates[0].wait_for_requests(1).expect("根请求应占位");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("应取得真实协调器");
        let root_turn = keencode_agent::TurnId::new("turn-unstarted-held").unwrap();
        if queued {
            collaboration
                .coordinator
                .spawn_agent(
                    &collaboration.root_agent_id,
                    &root_turn,
                    &ToolCallId::new("spawn-active-child").unwrap(),
                    test_spawn_request("active_child", project.path()),
                )
                .expect("活跃子任务应启动");
            gates[1].wait_for_requests(1).expect("活跃子任务应占位");
        }
        let mut request = test_spawn_request("rejected_child", project.path());
        request.profile.tool_snapshot = vec!["UnavailableFixtureTool".to_owned()];
        if journal_fault {
            // 协作批次由提交泵线程落盘，Journal 发布失败改用 Store 实例开关注入。
            collaboration
                .store
                .fail_next_unstarted_publish
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let result = collaboration.coordinator.spawn_agent(
            &collaboration.root_agent_id,
            &root_turn,
            &ToolCallId::new("spawn-rejected-child").unwrap(),
            request,
        );
        if journal_fault {
            let pending = collaboration
                .store
                .load_transition_snapshot()
                .unwrap()
                .unwrap();
            assert_eq!(
                pending.unstarted_turn_terminations.len(),
                1,
                "Journal 不确定时失败 receipt 必须仍在磁盘"
            );
            // Journal 真实故障改在测试线程的显式对账路径注入；注入面与原先
            // spawn 内联发布完全相同（同一条未启动失败 record 的追加）。
            keencode_resources::test_support::set_append_fault(
                keencode_resources::test_support::AppendFault::Flush,
            );
            assert!(
                collaboration
                    .store
                    .reconcile_pending_unstarted_turns()
                    .is_err(),
                "Journal flush 故障应让对账失败并保留 Store pending"
            );
            assert!(session.snapshot().unwrap().recovery_required);
            keencode_resources::test_support::clear_append_fault();
            collaboration
                .store
                .reconcile_pending_unstarted_turns()
                .expect("清除故障后同一证据应可重试");
            let sequence = session.snapshot().unwrap().state.last_sequence;
            super::reconcile_unstarted_turn_termination_records(
                &session,
                &collaboration.store,
                &pending.commit.checkpoint,
                &pending.unstarted_turn_terminations,
            )
            .expect("已处理证据重放必须幂等");
            assert_eq!(
                session.snapshot().unwrap().state.last_sequence,
                sequence,
                "不得重复追加失败生命周期"
            );
            assert!(!session.snapshot().unwrap().recovery_required);
            assert!(
                collaboration
                    .store
                    .load_transition_snapshot()
                    .unwrap()
                    .unwrap()
                    .unstarted_turn_terminations
                    .is_empty()
            );
        }
        let waiting = result.as_ref().ok().map(|spawned| {
            collaboration
                .coordinator
                .agent_status(&spawned.agent.agent_id)
                .unwrap()
        });
        // 固定在所有断言之前释放请求，后续失败也不会让 HTTP handler 永久等待。
        for gate in &gates {
            gate.release();
        }
        if queued {
            assert!(
                matches!(
                    waiting,
                    Some(CollaborationAgentStatus::WaitingCapacity { .. })
                ),
                "第二子任务必须确实经过等待容量状态"
            );
        } else {
            assert!(result.is_err(), "即时预检拒绝应返回失败");
        }
        wait_for_session_idle(&runtime, &session_id).await;
        let failed_snapshot = session.snapshot().expect("失败后 Journal 应可读");
        let failed_child = failed_snapshot
            .state
            .sub_agents
            .values()
            .find(|agent| agent.agent_path == "/root/rejected_child")
            .expect("失败身份必须存在");
        assert_eq!(failed_child.status, SubAgentStatus::Failed);
        let failed_agent_id = failed_child.agent_id.clone();
        let failed_turn_id = failed_child
            .current_turn_id
            .clone()
            .expect("失败必须绑定子 Turn");
        assert!(matches!(
            failed_snapshot
                .state
                .turns
                .get(&failed_turn_id)
                .map(|turn| &turn.status),
            Some(TurnStatus::Failed)
        ));
        assert!(
            collaboration
                .store
                .load_transition_snapshot()
                .unwrap()
                .unwrap()
                .unstarted_turn_terminations
                .is_empty()
        );
        drop(failed_snapshot);
        if cold {
            runtime
                .shutdown_session(&session_id)
                .await
                .expect("应暂停并释放原 Session");
            drop(collaboration);
            drop(session);
            drop(runtime);
            runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
            session = runtime
                .open_or_create_session(project.path(), Some(&session_id), "unused")
                .expect("同一 Session 应经磁盘冷重开");
            assert_eq!(
                session
                    .snapshot()
                    .unwrap()
                    .state
                    .sub_agents
                    .get(&failed_agent_id)
                    .unwrap()
                    .status,
                SubAgentStatus::Failed
            );
        } else {
            drop(collaboration);
        }
        runtime
            .start_root_turn(
                &session_id,
                "turn-unstarted-continued",
                "只续作根任务，不重跑旧子任务",
                RootTurnOptions::default(),
            )
            .await
            .expect("原 Session 新根 Turn 应启动");
        wait_for_session_idle(&runtime, &session_id).await;
        let requests = server
            .join()
            .expect("服务线程应退出")
            .expect("预期请求必须全部实际抵达");
        assert_eq!(requests.len(), expected_requests);
        assert!(
            !requests
                .iter()
                .any(|request| request_contains_user_text(request, "执行 rejected_child 测试任务")),
            "被拒绝子 Turn 不得采样"
        );
        let continued = requests
            .iter()
            .find(|request| request_contains_user_text(request, "只续作根任务，不重跑旧子任务"))
            .expect("新根请求必须真实抵达服务");
        assert!(
            continued["input"]
                .to_string()
                .contains("Agent Turn 派发被永久拒绝"),
            "新根必须收到失败通知，不能过滤 mailbox"
        );
        let final_snapshot = session.snapshot().unwrap();
        assert!(matches!(
            final_snapshot
                .state
                .turns
                .get(&ResourceTurnId::new("turn-unstarted-continued").unwrap())
                .map(|turn| &turn.status),
            Some(TurnStatus::Completed)
        ));
        assert_eq!(
            final_snapshot
                .state
                .sub_agents
                .get(&failed_agent_id)
                .unwrap()
                .status,
            SubAgentStatus::Failed
        );
        runtime
            .close_session(&session_id)
            .await
            .expect("隔离 Session 应正常关闭");
    }

    /// 真实 Runtime execution/coordinator 启动的子 Agent 必须读取全局指令和自身 cwd 规则。
    #[tokio::test(flavor = "multi_thread")]
    async fn child_agent_execution_loads_global_and_own_cwd_instructions() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let child_cwd = tempfile::tempdir().expect("应创建子 Agent 工作目录");
        std::fs::write(storage.path().join("AGENTS.md"), "子 Agent 测试全局指令")
            .expect("全局子 Agent 指令应写入");
        std::fs::write(child_cwd.path().join("AGENTS.md"), "子 Agent 自身 cwd 指令")
            .expect("子 Agent cwd 指令应写入");
        let (base_url, gate, server) = spawn_gated_buffered_responses_server(
            "子 Agent 上下文测试完成",
            2,
            "保持根 Turn 活跃以启动子 Agent",
        );
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                max_output_tokens: Some(96_000),
                ..ProviderCapabilities::default()
            }),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "child-instructions-operation")
            .expect("子 Agent 指令测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-child-instructions-root",
                    "保持根 Turn 活跃以启动子 Agent",
                    RootTurnOptions::default(),
                )
                .await
                .expect("根 Turn 应经生产装配启动"),
            RootTurnStartOutcome::Started
        );
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("生产 Collaboration Runtime 应可复用");
        let root_turn = keencode_agent::TurnId::new("turn-child-instructions-root")
            .expect("根 Turn 标识应有效");
        let child_result = collaboration.coordinator.spawn_agent(
            &collaboration.root_agent_id,
            &root_turn,
            &ToolCallId::new("spawn-child-instructions").expect("子 Agent 工具调用标识应有效"),
            test_spawn_request("child_instructions", child_cwd.path()),
        );
        // 无论 spawn 是否返回预期结果，都先释放根 HTTP 响应，避免失败断言遗留阻塞线程。
        gate.release();
        child_result.expect("子 Agent 应经真实 execution port 启动");

        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(
                request["max_output_tokens"], 96_000,
                "根和子代理都应使用配置的输出预算"
            );
        }
        let root_request_index = requests
            .iter()
            .position(|request| {
                request_contains_user_text(request, "保持根 Turn 活跃以启动子 Agent")
            })
            .expect("应捕获根 Agent 请求");
        let child_request_index = requests
            .iter()
            .position(|request| {
                request_contains_user_text(request, "执行 child_instructions 测试任务")
            })
            .expect("应捕获子 Agent 请求");
        let root_input = requests[root_request_index]["input"]
            .as_array()
            .expect("根 Agent Responses 请求应包含 input 数组");
        let child_input = requests[child_request_index]["input"]
            .as_array()
            .expect("子 Agent Responses 请求应包含 input 数组");
        let child_visible_text = child_input
            .iter()
            .filter_map(|message| message["content"][0]["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(child_visible_text.contains("canonical path is /root/child_instructions"));
        assert!(child_visible_text.contains("负责 child_instructions 测试范围"));
        assert!(child_visible_text.contains("send_message using target=/root"));
        assert!(child_visible_text.contains("absolute /root/<child> paths"));
        let root_developer = root_input
            .iter()
            .filter_map(|message| {
                (message["role"] == "developer")
                    .then(|| message["content"][0]["text"].as_str())
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let child_developer = child_input
            .iter()
            .filter_map(|message| {
                (message["role"] == "developer")
                    .then(|| message["content"][0]["text"].as_str())
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(root_developer.contains("子 Agent 测试全局指令"));
        assert!(!root_developer.contains("子 Agent 自身 cwd 指令"));
        assert!(child_developer.contains("子 Agent 测试全局指令"));
        assert!(child_developer.contains("子 Agent 自身 cwd 指令"));
        assert!(child_input.iter().any(|message| {
            message["role"] == "user"
                && message["content"][0]["text"] == "执行 child_instructions 测试任务"
        }));

        wait_for_session_idle(&runtime, &session_id).await;
        runtime
            .close_session(&session_id)
            .await
            .expect("子 Agent 指令测试 Session 应关闭");
    }

    /// 显式模板的项目指令开关必须进入真实 Provider 请求，并在冷重开后继续生效。
    #[tokio::test(flavor = "multi_thread")]
    async fn child_agent_template_injection_reaches_provider_and_survives_cold_reopen() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let disabled_cwd = tempfile::tempdir().expect("应创建关闭注入的子 Agent 目录");
        let enabled_cwd = tempfile::tempdir().expect("应创建开启注入的子 Agent 目录");
        std::fs::write(storage.path().join("AGENTS.md"), "模板测试全局指令标记")
            .expect("全局模板测试指令应写入");
        std::fs::write(
            disabled_cwd.path().join("AGENTS.md"),
            "模板测试关闭项目指令标记",
        )
        .expect("关闭注入的项目指令应写入");
        std::fs::write(
            enabled_cwd.path().join("AGENTS.md"),
            "模板测试开启项目指令标记",
        )
        .expect("开启注入的项目指令应写入");

        let root_prompt = "保持模板测试根 Turn 活跃";
        let disabled_prompt = "执行 template_injection_disabled 测试任务";
        let (base_url, gates, server) = spawn_gated_buffered_responses_server_with_texts(
            "模板测试 Provider 响应",
            4,
            &[root_prompt, disabled_prompt],
        );
        let mut gates = gates.into_iter();
        let root_gate = gates.next().expect("模板测试应有根请求闸门");
        let disabled_gate = gates.next().expect("模板测试应有关闭注入请求闸门");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "template-injection-cold")
            .expect("模板测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        mark_session_title_manual(&session);
        let root_turn_id = "turn-template-injection-root";
        let root_turn = AgentTurnId::new(root_turn_id).expect("模板测试根 Turn 标识应有效");

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    root_turn_id,
                    root_prompt,
                    RootTurnOptions::default(),
                )
                .await
                .expect("模板测试根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        root_gate
            .wait_for_requests(1)
            .expect("模板测试根请求应到达 Provider");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("模板测试 Collaboration Runtime 应建立");

        let mut disabled_request =
            test_spawn_request("template_injection_disabled", disabled_cwd.path());
        disabled_request.agent_template = Some(test_agent_template("disabled", false));
        let disabled_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-template-injection-disabled")
                    .expect("关闭注入工具调用标识应有效"),
                disabled_request,
            )
            .expect("关闭注入子 Agent 应启动");
        disabled_gate
            .wait_for_requests(1)
            .expect("关闭注入子 Agent 请求应到达 Provider");

        let mut enabled_request =
            test_spawn_request("template_injection_enabled", enabled_cwd.path());
        enabled_request.agent_template = Some(test_agent_template("enabled", true));
        let enabled_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-template-injection-enabled")
                    .expect("开启注入工具调用标识应有效"),
                enabled_request,
            )
            .expect("开启注入子 Agent 应启动");

        // 根请求和关闭注入请求都已进入真实 Provider；先只释放根请求，
        // 保留关闭注入请求的闸门，以便随后真实中断并冷恢复同一个子 Agent。
        root_gate.release();
        let settle_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let root_completed = matches!(
                collaboration
                    .coordinator
                    .agent_status(&collaboration.root_agent_id)
                    .expect("模板测试根 Agent 状态应读取"),
                CollaborationAgentStatus::Completed { .. }
            );
            let enabled_completed = matches!(
                collaboration
                    .coordinator
                    .agent_status(&enabled_child.agent.agent_id)
                    .expect("开启注入子 Agent 状态应读取"),
                CollaborationAgentStatus::Completed { .. }
            );
            if root_completed && enabled_completed {
                break;
            }
            assert!(
                Instant::now() < settle_deadline,
                "根与开启注入子 Agent 应在关闭注入子 Agent 仍挂起时收敛"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // 关闭注入子 Agent 必须有可恢复的中断事实，冷重开后才能验证同一模板快照。
        let persisted = collaboration
            .store
            .load_transition_snapshot()
            .expect("模板测试 checkpoint 应读取")
            .expect("模板测试 checkpoint 应存在");
        let disabled_checkpoint = persisted
            .commit
            .checkpoint
            .roots
            .iter()
            .flat_map(|root| root.agents.iter())
            .find(|agent| agent.definition.agent_id == disabled_child.agent.agent_id)
            .expect("关闭注入子 Agent checkpoint 应存在");
        assert_eq!(
            disabled_checkpoint
                .definition
                .agent_template
                .as_ref()
                .map(|template| template.inject_agents_md),
            Some(false),
            "关闭策略必须进入可持久化 Agent 定义"
        );
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&disabled_child.agent.agent_id)
                .expect("关闭注入子 Agent 状态应读取"),
            CollaborationAgentStatus::Running { .. }
        ));
        let cancellation = runtime
            .background_task_cancel_outcome(&session_id, disabled_child.initial_turn_id.as_str());
        assert!(
            cancellation.is_ok(),
            "关闭注入子 Agent 应可中断以验证冷恢复：{cancellation:?}"
        );
        disabled_gate.release();
        wait_for_session_idle(&runtime, &session_id).await;
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&disabled_child.agent.agent_id)
                .expect("中断后的关闭注入子 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted { .. }
        ));

        let disabled_agent_id = disabled_child.agent.agent_id.as_str().to_owned();
        // close_session 是用户主动关闭语义，会调用 close_root_session 并永久关闭根树；
        // 进程退出后的冷恢复必须走 shutdown_session，保留 Open checkpoint 和 Interrupted 子 Agent。
        runtime
            .shutdown_session(&session_id)
            .await
            .expect("模板测试首个 Runtime 应暂停并释放");
        drop(collaboration);
        drop(session);
        drop(runtime);
        std::fs::write(
            disabled_cwd.path().join("AGENTS.md"),
            "模板测试冷恢复项目指令标记",
        )
        .expect("冷恢复项目指令应写入");

        let recovered_runtime =
            runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let recovered_session = recovered_runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "template-injection-cold-reopen",
            )
            .expect("模板测试 Session 应冷重开");
        let resumed_turn = recovered_runtime
            .resume_background_agent(
                &session_id,
                "resume-template-injection-disabled",
                &disabled_agent_id,
            )
            .expect("关闭注入子 Agent 应从 checkpoint 恢复");
        wait_for_session_idle(&recovered_runtime, &session_id).await;
        let recovered_snapshot = recovered_session
            .snapshot()
            .expect("模板测试冷恢复快照应读取");
        assert!(recovered_snapshot.state.turns.values().any(|turn| {
            turn.turn_id.as_str() == resumed_turn.as_str() && turn.status == TurnStatus::Completed
        }));

        let requests = server
            .join()
            .expect("模板测试 Provider 线程不应 panic")
            .expect("模板测试 Provider 应成功收到所有请求");
        assert_eq!(requests.len(), 4);
        let request_text = |request: &Value| {
            request["input"]
                .as_array()
                .expect("模板测试 Responses 请求应包含 input")
                .iter()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let disabled_requests = requests
            .iter()
            .filter(|request| request_contains_user_text(request, disabled_prompt))
            .collect::<Vec<_>>();
        assert_eq!(
            disabled_requests.len(),
            2,
            "初始和冷恢复应各有一次关闭注入请求"
        );
        for request in disabled_requests {
            let text = request_text(request);
            assert!(text.contains("模板测试全局指令标记"));
            assert!(!text.contains("模板测试关闭项目指令标记"));
            assert!(!text.contains("模板测试冷恢复项目指令标记"));
        }
        let enabled_request = requests
            .iter()
            .find(|request| {
                request_contains_user_text(request, "执行 template_injection_enabled 测试任务")
            })
            .expect("开启注入子 Agent 请求应存在");
        let enabled_text = request_text(enabled_request);
        assert!(enabled_text.contains("模板测试全局指令标记"));
        assert!(enabled_text.contains("模板测试开启项目指令标记"));
        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("模板测试冷恢复 Runtime 应关闭");
    }

    /// 损坏或超限 AGENTS.md 必须在 Provider 请求前失败，且本地 Provider 不得收到请求。
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_instructions_fail_before_provider_request() {
        let cases = [
            ("全局非 UTF-8", Some(vec![0xff, 0xfe]), None),
            ("全局超限", Some(b"a".repeat(12_001)), None),
            ("项目非 UTF-8", None, Some(vec![0xff, 0xfe])),
            ("项目超限", None, Some(b"a".repeat(128 * 1024 + 1))),
        ];
        for (index, (label, global, project_instructions)) in cases.into_iter().enumerate() {
            let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
            let project = tempfile::tempdir().expect("应创建项目目录");
            if let Some(global) = global {
                std::fs::write(storage.path().join("AGENTS.md"), global)
                    .expect("损坏全局指令应写入");
            }
            if let Some(project_instructions) = project_instructions {
                std::fs::write(project.path().join("AGENTS.md"), project_instructions)
                    .expect("损坏项目指令应写入");
            }
            let (base_url, server) = spawn_responses_request_probe();
            let runtime =
                runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
            let session = runtime
                .open_or_create_session(project.path(), None, "invalid-instructions-operation")
                .expect("损坏指令测试 Session 应创建");
            let result = runtime
                .start_root_turn(
                    session.session_id().as_str(),
                    &format!("turn-invalid-instructions-{index}"),
                    "验证损坏指令在模型请求前失败",
                    RootTurnOptions::default(),
                )
                .await;
            assert!(result.is_err(), "{label} 应拒绝启动根 Turn");
            assert!(
                server
                    .join()
                    .expect("Provider 探针线程不应 panic")
                    .expect("Provider 探针应正常结束")
                    .is_none(),
                "{label} 不得收到模型请求"
            );
        }
    }

    /// 创建 operationId 必须稳定去重，焦点切换不得改变 Session 生命周期。
    #[test]
    fn session_creation_is_idempotent_and_focus_can_be_cleared() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let first = runtime
            .open_or_create_session(project.path(), None, "create-operation-a")
            .expect("首次创建应成功");
        let retried = runtime
            .open_or_create_session(project.path(), None, "create-operation-a")
            .expect("相同创建操作应返回原 Session");
        assert_eq!(first.session_id(), retried.session_id());

        runtime
            .focus_session(first.session_id().as_str())
            .expect("已打开 Session 应可聚焦");
        assert_eq!(
            runtime.focused_session_id().expect("焦点应读取"),
            Some(first.session_id().as_str().to_owned())
        );
        runtime.clear_focus();
        assert_eq!(runtime.focused_session_id().expect("焦点应读取"), None);
    }

    /// 新 Store 实例必须从单个原子文件恢复与事件水位完全一致的协调器状态。
    #[test]
    fn collaboration_store_reopens_atomic_transition_checkpoint() {
        let storage = tempfile::tempdir().expect("应创建 Collaboration 存储目录");
        let directory =
            keencode_resources::ensure_project_storage(storage.path(), "/test-project").unwrap();
        keencode_resources::register_session_location(
            storage.path(),
            &ResourceSessionId::new("session-collaboration-restart").unwrap(),
            &directory,
        )
        .unwrap();
        let project = tempfile::tempdir().expect("应创建 Agent 项目目录");
        let session_id = "session-collaboration-restart";
        let first_store = Arc::new(
            SessionCollaborationStore::new(storage.path(), session_id).expect("首次 Store 应创建"),
        );
        let first_coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(4).expect("测试容量应有效"),
            first_store.clone(),
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let registered = first_coordinator
            .register_root(RootAgentRequest {
                session_id: keencode_agent::SessionId::new(session_id)
                    .expect("测试 Session 标识应有效"),
                profile: AgentProfile {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                    cwd: project.path().to_path_buf(),
                    worktree_lease: None,
                    tool_snapshot: vec!["Read".to_owned()],
                },
                per_root_turn_limit: 4,
            })
            .expect("根 Agent 应原子注册");
        assert_eq!(first_store.current_sequence().expect("首次水位应读取"), 1);
        let committed = first_store
            .load_transition_file_unlocked()
            .expect("原子提交文件应读取")
            .expect("根注册后应存在提交文件");
        assert!(matches!(
            first_store.commit_transition(&committed.commit),
            CollaborationAppendResult::AlreadyCommitted {
                current_sequence: 1
            }
        ));
        drop(first_coordinator);
        drop(first_store);

        let reopened_store = Arc::new(
            SessionCollaborationStore::new(storage.path(), session_id).expect("重启 Store 应创建"),
        );
        let recovered = reopened_store
            .load_coordinator_checkpoint()
            .expect("重启 checkpoint 应读取")
            .expect("重启 checkpoint 应存在");
        assert_eq!(recovered.last_event_sequence, 1);
        let reopened_coordinator = CollaborationCoordinator::new(
            CollaborationLimits::new(4).expect("测试容量应有效"),
            reopened_store,
            Arc::new(NoopCollaborationExecution),
            Arc::new(UuidCollaborationIdGenerator),
        );
        let restored = reopened_coordinator
            .restore_coordinator(recovered)
            .expect("重启协调器应恢复");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0], registered);
        assert_eq!(
            reopened_coordinator
                .agent_status(&registered.agent_id)
                .expect("恢复根状态应读取"),
            keencode_agent::CollaborationAgentStatus::Idle
        );
    }

    /// 项目撤销必须路由到唯一候选并使缓存失效，其他项目和无候选路径互不影响。
    #[test]
    fn project_mcp_revocation_invalidates_only_the_selected_runtime_candidate() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project_a = tempfile::tempdir().expect("应创建项目 A");
        let project_b = tempfile::tempdir().expect("应创建项目 B");
        let project_c = tempfile::tempdir().expect("应创建未发布候选的项目 C");
        let runtime = AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建");
        let first = RecordingExtensionContributor::new();
        let second = RecordingExtensionContributor::new();
        for (project, contributor) in [
            (project_a.path(), first.clone()),
            (project_b.path(), second.clone()),
        ] {
            runtime
                .publish_extension_candidate(
                    project,
                    RuntimeExtensionCandidate::new(1, contributor).expect("候选代次应有效"),
                )
                .expect("项目候选应发布");
            assert!(!runtime.extension_candidate_needs_refresh(project).unwrap());
        }
        assert!(
            runtime
                .extension_candidate_needs_refresh(project_c.path())
                .unwrap()
        );
        runtime
            .revoke_project_mcp_extension_tools(project_a.path())
            .expect("A 的工具应撤销");
        assert!(
            runtime
                .extension_candidate_needs_refresh(project_a.path())
                .unwrap()
        );
        assert!(
            !runtime
                .extension_candidate_needs_refresh(project_b.path())
                .unwrap()
        );
        assert_eq!(
            runtime.extension_generation(project_a.path()).unwrap(),
            Some(1)
        );
        assert_eq!(first.calls.lock().len(), 1);
        assert!(second.calls.lock().is_empty());

        runtime
            .revoke_project_mcp_extension_tools(project_c.path())
            .unwrap();
        runtime
            .revoke_project_mcp_extension_tools(project_a.path())
            .unwrap();
        assert_eq!(first.calls.lock().len(), 2);
        assert!(second.calls.lock().is_empty());
        assert_eq!(
            runtime.extension_generation(project_c.path()).unwrap(),
            None
        );

        let replacement = RecordingExtensionContributor::new();
        runtime
            .publish_extension_candidate(
                project_a.path(),
                RuntimeExtensionCandidate::new(2, replacement.clone()).unwrap(),
            )
            .unwrap();
        assert!(
            !runtime
                .extension_candidate_needs_refresh(project_a.path())
                .unwrap()
        );
        assert!(replacement.calls.lock().is_empty());
        assert_eq!(first.calls.lock().len(), 2);
        runtime.revoke_mcp_extension_tools().unwrap();
        assert!(
            runtime
                .extension_candidate_needs_refresh(project_a.path())
                .unwrap()
        );
        assert!(
            runtime
                .extension_candidate_needs_refresh(project_b.path())
                .unwrap()
        );
        assert_eq!(replacement.calls.lock().len(), 1);
        assert_eq!(second.calls.lock().len(), 1);
        assert_eq!(
            first.calls.lock().len(),
            2,
            "已被替换的旧候选不得再收到撤销"
        );
    }

    /// 资源写入只登记项目级刷新；活动候选保持原代次，下一次发布成功后才清除标记。
    #[test]
    fn extension_candidate_invalidation_preserves_active_generation_until_republish() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建候选项目目录");
        let runtime = AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建");
        let current = RecordingExtensionContributor::new();
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(1, current).expect("候选代次应有效"),
            )
            .expect("初始候选应发布");

        let build_epoch = runtime
            .extension_candidate_refresh_epoch(project.path())
            .expect("初始刷新 epoch 应可读取");
        runtime
            .invalidate_extension_candidate(project.path())
            .expect("项目候选应登记刷新");
        assert_eq!(
            runtime.extension_generation(project.path()).unwrap(),
            Some(1),
            "失效登记不能立即替换活动候选"
        );
        assert!(
            runtime
                .extension_candidate_needs_refresh(project.path())
                .unwrap()
        );
        assert_eq!(
            runtime.publish_extension_candidate_at_epoch(
                project.path(),
                RuntimeExtensionCandidate::new(2, RecordingExtensionContributor::new())
                    .expect("过时候选代次应有效"),
                build_epoch,
            ),
            Err(AgentRuntimeError::RuntimeOperationFailed),
            "构建期间发生资源写入时不得发布旧候选"
        );
        assert_eq!(
            runtime.extension_generation(project.path()).unwrap(),
            Some(1),
            "拒绝过时候选后仍应保留活动候选"
        );

        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(2, RecordingExtensionContributor::new())
                    .expect("替代候选代次应有效"),
            )
            .expect("替代候选应发布");
        assert_eq!(
            runtime.extension_generation(project.path()).unwrap(),
            Some(2)
        );
        assert!(
            !runtime
                .extension_candidate_needs_refresh(project.path())
                .unwrap()
        );
    }

    /// Agent 模板只允许从同一规范项目已经发布的候选中解析。
    #[test]
    fn extension_agent_resolution_is_project_scoped_and_strict() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建候选项目目录");
        let other_project = tempfile::tempdir().expect("应创建隔离项目目录");
        let runtime = AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建");
        let contributor = RecordingExtensionContributor::new();
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(1, contributor).expect("候选代次应有效"),
            )
            .expect("项目候选应发布");
        let parent = RuntimeAgentTemplateContext {
            session_id: "session-agent-template".to_owned(),
            parent_agent_id: "root".to_owned(),
            root_turn_id: "turn-agent-template".to_owned(),
        };

        let template = runtime
            .resolve_extension_agent(project.path(), "reviewer", &parent)
            .expect("已发布项目应解析模板")
            .expect("reviewer 模板应存在");
        assert_eq!(template.name, "reviewer");
        assert!(
            runtime
                .resolve_extension_agent(project.path(), "missing", &parent)
                .expect("未知模板应返回空而不是回退")
                .is_none()
        );
        assert_eq!(
            runtime
                .resolve_extension_agent(other_project.path(), "reviewer", &parent)
                .expect_err("其他项目不得复用候选"),
            AgentRuntimeError::RuntimeOperationFailed
        );
    }

    /// 已开始 Session 的扩展引用必须固定在首次完整 context 候选；只有尚未产生
    /// 用户事实的 deferred draft 允许显式刷新，未知 ID 和冷驻留 Session 都不能
    /// 通过当前工作区候选制造隐式回退。
    #[test]
    fn session_extension_reference_catalog_is_frozen_and_fail_closed() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let cold_project = tempfile::tempdir().expect("应创建冷驻留项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let old_plugins = vec![json!({"pluginId": "local:old-plugin"})];
        let old_skills = vec![json!({"id": "workspace:old-skill"})];
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(
                    1,
                    RecordingExtensionContributor::with_reference_catalogs(
                        old_plugins.clone(),
                        old_skills.clone(),
                    ),
                )
                .expect("旧候选代次应有效"),
            )
            .expect("旧候选应发布");
        let session = runtime
            .open_or_create_session(project.path(), None, "extension-catalog-freeze")
            .expect("已发布候选后 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        assert_eq!(
            runtime
                .session_extension_reference_catalog(&session_id)
                .expect("旧 Session 目录应可读取")
                .expect("旧 Session 应已冻结")
                .plugins,
            old_plugins
        );
        assert_eq!(
            runtime
                .session_extension_reference_catalog(&session_id)
                .expect("旧 Session Skill 目录应可读取")
                .expect("旧 Session 应已冻结")
                .skills,
            old_skills
        );

        let new_plugins = vec![json!({"pluginId": "local:new-plugin"})];
        let new_skills = vec![json!({"id": "workspace:new-skill"})];
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(
                    2,
                    RecordingExtensionContributor::with_reference_catalogs(
                        new_plugins.clone(),
                        new_skills.clone(),
                    ),
                )
                .expect("新候选代次应有效"),
            )
            .expect("新候选应发布");
        let frozen = runtime
            .session_extension_reference_catalog(&session_id)
            .expect("热替换后旧 Session 目录仍应可读取")
            .expect("旧 Session 目录不得丢失");
        assert_eq!(frozen.plugins, old_plugins);
        assert_eq!(frozen.skills, old_skills);
        assert_eq!(
            runtime
                .plugin_reference_catalog(project.path())
                .expect("当前项目候选应可读取")
                .expect("当前项目应有候选"),
            new_plugins
        );
        let refreshed = runtime
            .initialize_session_extension_reference_catalog(&session_id, project.path())
            .expect("尚未首发的 draft 应允许刷新当前候选");
        assert_eq!(refreshed.plugins, new_plugins);
        assert_eq!(refreshed.skills, new_skills);
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("首发前的执行上下文应能冻结当前候选");
        let execution_catalog = collaboration
            .execution
            .state
            .lock()
            .expect("执行状态应可读取")
            .frozen_extension_reference_catalog
            .clone()
            .expect("工具装配应冻结扩展目录");
        let ui_catalog = runtime
            .session_extension_reference_catalog(&session_id)
            .expect("UI 引用目录应可读取")
            .expect("UI 引用目录应存在");
        assert_eq!(ui_catalog.skills, execution_catalog.skills);
        assert_eq!(ui_catalog.plugins, execution_catalog.plugins);

        let latest_plugins = vec![json!({"pluginId": "local:latest-plugin"})];
        let latest_skills = vec![json!({"id": "workspace:latest-skill"})];
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(
                    3,
                    RecordingExtensionContributor::with_reference_catalogs(
                        latest_plugins.clone(),
                        latest_skills.clone(),
                    ),
                )
                .expect("最新候选代次应有效"),
            )
            .expect("最新候选应发布");
        let frozen_after_replacement = runtime
            .initialize_session_extension_reference_catalog(&session_id, project.path())
            .expect("已有执行冻结的 Session 应继续返回权威目录");
        assert_eq!(frozen_after_replacement.plugins, new_plugins);
        assert_eq!(frozen_after_replacement.skills, new_skills);

        assert!(matches!(
            runtime.session_extension_reference_catalog("missing-session"),
            Err(AgentRuntimeError::SessionUnavailable)
        ));

        let cold_session = runtime
            .open_or_create_session(cold_project.path(), None, "extension-catalog-cold")
            .expect("候选发布前的冷驻留 Session 应创建");
        let cold_session_id = cold_session.session_id().as_str().to_owned();
        runtime
            .publish_extension_candidate(
                cold_project.path(),
                RuntimeExtensionCandidate::new(
                    1,
                    RecordingExtensionContributor::with_reference_catalogs(
                        vec![json!({"pluginId": "local:cold-plugin"})],
                        vec![json!({"id": "workspace:cold-skill"})],
                    ),
                )
                .expect("冷项目候选代次应有效"),
            )
            .expect("冷项目候选应发布");
        assert!(
            runtime
                .session_extension_reference_catalog(&cold_session_id)
                .expect("冷驻留 Session 查询应成功")
                .is_none()
        );
    }

    #[test]
    fn deferred_catalog_refresh_uses_journal_facts_boundary() {
        let draft_id = keencode_resources::SessionId::new("deferred-catalog-draft")
            .expect("测试 draft Session 标识应有效");
        let draft = SessionState::empty(draft_id.clone());
        assert!(!session_has_persisted_user_facts(&draft));

        let mut started = draft;
        started.input_queue.items.push(SessionInputQueueItem {
            queue_item_id: "queue:deferred-catalog".to_owned(),
            source_command_id: "command:deferred-catalog".to_owned(),
            client_id: None,
            kind: keencode_resources::SessionInputKind::SendText,
            text: "首发事实".to_owned(),
            attachments: Vec::new(),
            model_selection: None,
            mode: None,
            plan_enabled: false,
            requested_delivery: keencode_resources::SessionInputDelivery::Queue,
            admitted_delivery: keencode_resources::SessionInputDelivery::Queue,
            admission_seq: 1,
            reserve_attempt: 0,
            dispatch: keencode_resources::SessionInputDispatch::Queued,
            promoted_turn_id: None,
            admitted_at_unix_ms: 1,
        });
        assert!(session_has_persisted_user_facts(&started));
    }

    /// 恢复期间 live 事件必须缓存，末页后只释放冻结水位之后的事件。
    #[tokio::test]
    async fn turn_started_barrier_and_root_turn_retry_are_deterministic() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "turn-operation")
            .expect("测试 Session 应创建");
        let mut subscription = session.subscribe().expect("应订阅 Runtime 事件");
        let summary = root_turn_summary("检查项目", None, false);
        persist_completed_root_turn(&session, "turn-stable", "检查项目", &summary).await;
        wait_for_turn_started(&mut subscription, "turn-stable")
            .await
            .expect("屏障应观察到权威 TurnStarted");

        assert_eq!(
            runtime
                .start_root_turn(
                    session.session_id().as_str(),
                    "turn-stable",
                    "检查项目",
                    RootTurnOptions::default(),
                )
                .await
                .expect("相同输入重试应去重"),
            RootTurnStartOutcome::Deduplicated
        );
        assert_eq!(
            runtime
                .start_root_turn(
                    session.session_id().as_str(),
                    "turn-stable",
                    "检查项目",
                    RootTurnOptions {
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: None,
                        plan_enabled: true,
                        elicitation_connection_id: None,
                    },
                )
                .await,
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        assert_eq!(
            runtime
                .start_root_turn(
                    session.session_id().as_str(),
                    "turn-stable",
                    "检查项目",
                    RootTurnOptions {
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: Some("重新抽取的动态记忆".to_owned()),
                        plan_enabled: false,
                        elicitation_connection_id: None,
                    },
                )
                .await
                .expect("动态开发者上下文变化不得破坏相同客户端请求去重"),
            RootTurnStartOutcome::Deduplicated
        );
    }

    /// 根 Turn 切换 Plan 模式时不得误清除已保存的最终计划 Artifact 引用。
    #[tokio::test]
    async fn root_plan_mode_toggle_preserves_final_plan_artifact() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "plan-mode-artifact-operation")
            .expect("测试 Session 应创建");
        let artifact = session
            .put_artifact("# 已保存计划".as_bytes(), Some("text/markdown".to_owned()))
            .expect("测试计划 Artifact 应写入")
            .as_event_use();
        session
            .set_plan(
                "plan-mode-artifact-seed",
                PlanState {
                    enabled: false,
                    plan_artifact: Some(artifact.clone()),
                },
            )
            .expect("测试计划状态应写入");

        assert_eq!(
            runtime
                .start_root_turn(
                    session.session_id().as_str(),
                    "turn-plan-mode-artifact",
                    "验证 Plan 模式切换",
                    RootTurnOptions {
                        references: Vec::new(),
                        attachment_images: Vec::new(),
                        attachment_context: Vec::new(),
                        developer_context: None,
                        plan_enabled: true,
                        elicitation_connection_id: None,
                    },
                )
                .await,
            Err(AgentRuntimeError::ProviderNotConfigured)
        );
        assert_eq!(
            session
                .snapshot()
                .expect("Plan 模式切换后 Session 快照应读取")
                .state
                .plan
                .plan_artifact,
            Some(artifact)
        );
    }

    /// 界面七档推理强度必须无歧义映射到 Provider 中立枚举。
    #[test]
    fn reasoning_effort_parser_accepts_only_current_seven_levels() {
        assert_eq!(parse_reasoning_effort("none"), Ok(None));
        assert_eq!(
            parse_reasoning_effort("minimal"),
            Ok(Some(keencode_model::ReasoningEffort::Minimal))
        );
        assert_eq!(
            parse_reasoning_effort("low"),
            Ok(Some(keencode_model::ReasoningEffort::Low))
        );
        assert_eq!(
            parse_reasoning_effort("medium"),
            Ok(Some(keencode_model::ReasoningEffort::Medium))
        );
        assert_eq!(
            parse_reasoning_effort("high"),
            Ok(Some(keencode_model::ReasoningEffort::High))
        );
        assert_eq!(
            parse_reasoning_effort("xhigh"),
            Ok(Some(keencode_model::ReasoningEffort::ExtraHigh))
        );
        assert_eq!(
            parse_reasoning_effort("max"),
            Ok(Some(keencode_model::ReasoningEffort::Maximum))
        );
        assert_eq!(
            parse_reasoning_effort("maximum"),
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
    }

    /// 配置更新后旧会话自动解析最新客户端，推理设置也不要求重选模型。
    #[tokio::test]
    async fn session_provider_connection_change_resolves_latest_configuration() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime =
            runtime_with_responses_provider(storage.path(), "http://127.0.0.1:9/v1", &["model-a"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "provider-change-session")
            .expect("应创建测试 Session");
        let session_id = session.session_id().as_str();
        let original = runtime
            .set_session_model(
                session_id,
                "original-selection",
                "provider-runtime-test",
                "model-a",
            )
            .expect("应持久化原模型绑定")
            .state
            .provider
            .expect("应存在原绑定");
        let changed = ProviderConfig::new_unauthenticated(
            "provider-runtime-test",
            keencode_model::ProviderProtocol::Responses,
            "http://127.0.0.1:10/v1",
        )
        .expect("应构造新的本地测试地址");
        runtime
            .provider_registry
            .replace_all([ProviderRegistration::new(
                changed,
                "Runtime 测试 Provider",
                "changed-revision",
                ProviderModelPolicy::Enumerated {
                    models: vec!["model-a".to_owned()],
                },
            )
            .expect("应构造新注册项")])
            .expect("应热替换模型配置");

        let resolved = runtime.resolve_session_provider(Some(&original)).unwrap();
        assert_ne!(resolved.config_identity(), original.config_fingerprint);
        runtime
            .set_session_effort(session_id, "refresh-effort", "high")
            .unwrap();
        let refreshed = session.snapshot().unwrap().state.provider.unwrap();
        assert_eq!(refreshed.config_fingerprint, resolved.config_identity());
        assert_eq!(refreshed.model, original.model);
    }

    /// 执行中选择另一模型并更新供应商，本轮仍完成，下一轮自动使用最新端点与选择。
    #[tokio::test(flavor = "multi_thread")]
    async fn session_provider_switch_during_turn_uses_latest_configuration_next_turn() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (old_url, gate, old_server) =
            spawn_gated_buffered_responses_server("old complete", 1, "old request");
        let (new_url, new_server) = spawn_buffered_responses_server("new complete");
        let runtime =
            runtime_with_responses_provider(storage.path(), &old_url, &["model-a", "model-b"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "switch-session")
            .unwrap();
        mark_session_title_manual(&session);
        let id = session.session_id().as_str();
        runtime
            .start_root_turn(id, "old-turn", "old request", RootTurnOptions::default())
            .await
            .unwrap();
        gate.wait_for_requests(1).unwrap();
        let selection =
            runtime.set_session_model(id, "select-b", "provider-runtime-test", "model-b");
        gate.release();
        let old_requests = old_server.join().unwrap().unwrap();
        selection.unwrap();
        wait_for_session_idle(&runtime, id).await;
        let mut config = ProviderConfig::new_unauthenticated(
            "provider-runtime-test",
            keencode_model::ProviderProtocol::Responses,
            &new_url,
        )
        .unwrap();
        config.default_capabilities.max_context_tokens = Some(64000);
        config.default_capabilities.max_output_tokens = Some(4096);
        runtime
            .provider_registry
            .replace_all([ProviderRegistration::new(
                config,
                "updated",
                "revision-2",
                ProviderModelPolicy::Enumerated {
                    models: vec!["model-a".into(), "model-b".into()],
                },
            )
            .unwrap()])
            .unwrap();
        runtime
            .start_root_turn(id, "new-turn", "new request", RootTurnOptions::default())
            .await
            .unwrap();
        let new_request = finish_responses_server(new_server);
        wait_for_session_idle(&runtime, id).await;
        assert_eq!(old_requests[0]["model"], "model-a");
        assert_eq!(new_request["model"], "model-b");
        assert_eq!(new_request["max_output_tokens"], 4096);
        assert!(new_request.to_string().contains("old complete"));
        let new_input = new_request["input"].as_array().unwrap();
        assert_eq!(
            new_input[0]["content"][0]["text"],
            crate::agent_prompt::EnvironmentSnapshot::freeze(
                &project.path().canonicalize().unwrap(),
                &chrono::Local::now().fixed_offset(),
            )
            .render_small_context_core()
        );
        assert!(
            !new_request
                .to_string()
                .contains(crate::agent_prompt::core())
        );
        let tool_names = new_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tool_names, ["Bash", "Edit", "Read", "Write"]);
        assert_eq!(
            session
                .snapshot()
                .unwrap()
                .state
                .provider
                .unwrap()
                .context_window,
            None
        );
        runtime.close_session(id).await.unwrap();
    }

    #[test]
    fn historical_reasoning_preserves_compatible_continuation_without_changing_text() {
        use keencode_model::{ContentBlock, Message};
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let runtime =
            runtime_with_responses_provider(storage.path(), "http://127.0.0.1:9/v1", &["model-a"]);
        let resolved = runtime
            .provider_registry
            .resolve("provider-runtime-test", "model-a")
            .expect("测试 Provider 应解析");
        let turn_id = ResourceTurnId::new("historical-reasoning-turn").unwrap();
        let mut messages = vec![Message::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::Reasoning {
                    reasoning: keencode_model::ReasoningContent {
                        text: String::new(),
                        summary: Some("visible reasoning".into()),
                        continuation: Some(keencode_model::OpaqueReasoningState::new(
                            "responses-reasoning-item-v1",
                            serde_json::json!({"id":"old-response"}),
                        )),
                    },
                },
                ContentBlock::text("answer"),
            ],
        )];
        let historical =
            HashMap::from([(turn_id.as_str().to_owned(), provider_snapshot(&resolved))]);
        super::clear_historical_reasoning_state(
            &mut messages,
            &[Some(turn_id)],
            &resolved,
            &historical,
        );
        let ContentBlock::Reasoning { reasoning } = &messages[0].content[0] else {
            panic!("reasoning retained")
        };
        assert_eq!(reasoning.summary.as_deref(), Some("visible reasoning"));
        assert!(reasoning.continuation.is_some());
        assert_eq!(messages[0].content[1], ContentBlock::text("answer"));
    }

    #[test]
    fn historical_reasoning_clears_incompatible_continuation_but_keeps_text() {
        use keencode_model::{ContentBlock, Message};
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let runtime =
            runtime_with_responses_provider(storage.path(), "http://127.0.0.1:9/v1", &["model-a"]);
        let resolved = runtime
            .provider_registry
            .resolve("provider-runtime-test", "model-a")
            .expect("测试 Provider 应解析");
        let turn_id = ResourceTurnId::new("incompatible-reasoning-turn").unwrap();
        let mut messages = vec![Message::new(
            MessageRole::Assistant,
            vec![ContentBlock::Reasoning {
                reasoning: keencode_model::ReasoningContent {
                    text: "visible reasoning".into(),
                    summary: None,
                    continuation: Some(keencode_model::OpaqueReasoningState::new(
                        "responses-reasoning-item-v1",
                        serde_json::json!({"id":"old-response"}),
                    )),
                },
            }],
        )];
        let historical = HashMap::from([(
            turn_id.as_str().to_owned(),
            ProviderSnapshot {
                provider_id: "other-provider".to_owned(),
                ..provider_snapshot(&resolved)
            },
        )]);
        super::clear_historical_reasoning_state(
            &mut messages,
            &[Some(turn_id)],
            &resolved,
            &historical,
        );
        let ContentBlock::Reasoning { reasoning } = &messages[0].content[0] else {
            panic!("reasoning retained")
        };
        assert_eq!(reasoning.text, "visible reasoning");
        assert!(reasoning.continuation.is_none());
    }

    /// 凭据修订变化即使不改变传输端点，也必须清除 opaque reasoning 续传。
    #[test]
    fn credential_revision_change_clears_reasoning_continuation() {
        use keencode_model::{ContentBlock, Message};
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let runtime =
            runtime_with_responses_provider(storage.path(), "http://127.0.0.1:9/v1", &["model-a"]);
        let original = runtime
            .provider_registry
            .resolve("provider-runtime-test", "model-a")
            .expect("原始 Provider 应解析");
        let mut rotated_config = ProviderConfig::new_unauthenticated(
            "provider-runtime-test",
            keencode_model::ProviderProtocol::Responses,
            "http://127.0.0.1:9/v1",
        )
        .expect("轮换后的 Provider 配置应有效");
        rotated_config.response_mode = WireResponseMode::Buffered;
        runtime
            .provider_registry
            .replace_all([ProviderRegistration::new(
                rotated_config,
                "Runtime 测试 Provider",
                "rotated-revision",
                ProviderModelPolicy::Enumerated {
                    models: vec!["model-a".to_owned()],
                },
            )
            .expect("轮换后的 Provider 注册项应有效")])
            .expect("凭据修订轮换应成功");
        let rotated = runtime
            .provider_registry
            .resolve("provider-runtime-test", "model-a")
            .expect("轮换后的 Provider 应解析");
        assert_eq!(
            original.transport_fingerprint(),
            rotated.transport_fingerprint(),
            "凭据轮换不应改变传输指纹"
        );
        assert_ne!(original.config_identity(), rotated.config_identity());

        let turn_id = ResourceTurnId::new("credential-rotation-turn").unwrap();
        let mut messages = vec![Message::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::Reasoning {
                    reasoning: keencode_model::ReasoningContent {
                        text: "可见推理".to_owned(),
                        summary: None,
                        continuation: Some(keencode_model::OpaqueReasoningState::new(
                            "responses-reasoning-item-v1",
                            serde_json::json!({"id":"old-response"}),
                        )),
                    },
                },
                ContentBlock::text("answer"),
            ],
        )];
        let historical =
            HashMap::from([(turn_id.as_str().to_owned(), provider_snapshot(&original))]);
        assert!(!provider_supports_reasoning_continuation(
            historical.get(turn_id.as_str()).unwrap(),
            &rotated,
        ));
        clear_historical_reasoning_state(&mut messages, &[Some(turn_id)], &rotated, &historical);
        let ContentBlock::Reasoning { reasoning } = &messages[0].content[0] else {
            panic!("可见 reasoning 应保留")
        };
        assert_eq!(reasoning.text, "可见推理");
        assert!(reasoning.continuation.is_none());
        assert_eq!(messages[0].content[1], ContentBlock::text("answer"));
    }

    /// 子 Agent 的显式 Provider/model 覆盖必须随 Turn 快照持久化，并在冷恢复后按 Turn 还原。
    #[tokio::test]
    async fn child_provider_override_survives_cold_recovery() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let registry = keencode_provider::ProviderRegistry::new();
        let registration = |provider_id: &str, model: &str, revision: &str| {
            ProviderRegistration::new(
                ProviderConfig::new_unauthenticated(
                    provider_id,
                    keencode_model::ProviderProtocol::Responses,
                    "http://127.0.0.1:9/v1",
                )
                .expect("Provider 配置应有效"),
                format!("{provider_id} 测试 Provider"),
                revision,
                ProviderModelPolicy::Enumerated {
                    models: vec![model.to_owned()],
                },
            )
            .expect("Provider 注册项应有效")
        };
        let generation = registry
            .replace_all([
                registration("provider-a", "model-a", "revision-a"),
                registration("provider-b", "model-b", "revision-b"),
            ])
            .expect("Provider 注册表应替换")
            .generation;
        let runtime = Arc::new(
            AgentRuntime::new_with_registry(storage.path(), registry, test_executor_handle())
                .expect("Runtime 应创建"),
        );
        *runtime
            .default_provider
            .write()
            .expect("默认 Provider 锁应读取") = Some(super::DefaultProviderBinding {
            provider_id: "provider-a".to_owned(),
            model: "model-a".to_owned(),
            generation,
        });
        let session = runtime
            .open_or_create_session(project.path(), None, "child-provider-cold-recovery")
            .expect("Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let root_turn_id = AgentTurnId::new("root-provider-cold").expect("根 Turn ID 应有效");
        let child_turn_id = AgentTurnId::new("child-provider-cold").expect("子 Turn ID 应有效");
        let root_provider = provider_snapshot(
            &runtime
                .provider_registry
                .resolve("provider-a", "model-a")
                .expect("根 Provider 应解析"),
        );
        let child_provider = provider_snapshot(
            &runtime
                .provider_registry
                .resolve("provider-b", "model-b")
                .expect("覆盖 Provider 应解析"),
        );
        let root_input = ModelMessage::text(MessageRole::User, "root");
        let root_request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session ID 应有效"),
            root_turn_id.clone(),
            RunnerAgentId::new("root").expect("根 Agent ID 应有效"),
            "model-a",
            vec![root_input.clone()],
            PlanGuard::inactive(),
        );
        let root_result = session
            .bind_agent_runner(AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities::default(),
                    [completed_reply("root done")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(
                RuntimeTurnRequest::root(root_request, vec![root_input], "root")
                    .with_provider_snapshot(root_provider),
            )
            .await
            .expect("根 Turn 应完成");
        assert!(root_result.is_success(), "根 Turn 失败：{root_result:?}");

        let child_agent = ResourceAgentId::new("override_child").expect("子 Agent ID 应有效");
        let child_input = ModelMessage::text(MessageRole::User, "child");
        let child_request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session ID 应有效"),
            child_turn_id.clone(),
            RunnerAgentId::new("override_child").expect("子 Agent ID 应有效"),
            "model-b",
            vec![child_input.clone()],
            PlanGuard::inactive(),
        );
        let child_result = session
            .bind_agent_runner(AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities::default(),
                    [completed_reply("child done")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            ))
            .run_turn(
                RuntimeTurnRequest::initial_child(
                    child_request,
                    vec![child_input],
                    root_turn_id.as_str(),
                    root_turn_id.as_str(),
                    "child",
                    SubAgentState {
                        agent_id: child_agent,
                        parent_agent_id: ResourceAgentId::new("root").expect("根 Agent ID 应有效"),
                        agent_path: "/root/override_child".to_owned(),
                        task: "child".to_owned(),
                        status: SubAgentStatus::Pending,
                        current_turn_id: None,
                        result_summary: None,
                    },
                )
                .with_provider_snapshot(child_provider.clone()),
            )
            .await
            .expect("子 Turn 应完成");
        assert!(child_result.is_success(), "子 Turn 失败：{child_result:?}");
        assert!(
            session
                .snapshot()
                .expect("Session 快照应读取")
                .state
                .provider
                .is_none(),
            "Turn Provider 快照不得污染 Session 当前 Provider"
        );

        runtime
            .close_session(&session_id)
            .await
            .expect("Session 应关闭");
        drop(session);
        drop(runtime);

        let recovered_runtime = Arc::new(
            AgentRuntime::new_with_registry(
                storage.path(),
                {
                    let registry = keencode_provider::ProviderRegistry::new();
                    registry
                        .replace_all([
                            registration("provider-a", "model-a", "revision-a"),
                            registration("provider-b", "model-b", "revision-b"),
                        ])
                        .expect("冷恢复 Provider 注册表应替换");
                    registry
                },
                test_executor_handle(),
            )
            .expect("冷恢复 Runtime 应创建"),
        );
        let recovered = recovered_runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "child-provider-cold-recovery-reopen",
            )
            .expect("Session 应冷恢复");
        let historical = super::historical_provider_snapshots_by_turn(&recovered)
            .expect("历史 Provider 快照应可重建");
        assert_eq!(
            historical.get(child_turn_id.as_str()),
            Some(&child_provider),
            "冷恢复必须保留子 Agent 的显式 Provider/model 覆盖"
        );
        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("冷恢复 Session 应关闭");
    }

    /// 首次设置推理强度必须冻结默认 Provider，切换模型时继续保留该强度。
    #[test]
    fn session_effort_freezes_default_provider_and_survives_model_switch() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = runtime_with_responses_provider(
            storage.path(),
            "http://127.0.0.1:9/v1",
            &["model-a", "model-b"],
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "effort-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str();

        runtime
            .set_session_effort(session_id, "set-effort-max", "max")
            .expect("未绑定 Session 应冻结默认 Provider");
        let frozen = runtime
            .session_snapshot(session_id)
            .expect("冻结后快照应读取")
            .state
            .provider
            .expect("Provider 应持久绑定");
        assert_eq!(frozen.provider_id, "provider-runtime-test");
        assert_eq!(frozen.model, "model-a");
        assert_eq!(
            frozen.reasoning_effort,
            Some(keencode_resources::ReasoningEffortSnapshot::Maximum)
        );

        runtime
            .set_session_model(
                session_id,
                "switch-effort-model",
                "provider-runtime-test",
                "model-b",
            )
            .expect("模型切换应成功");
        let switched = runtime
            .session_snapshot(session_id)
            .expect("切换后快照应读取")
            .state
            .provider
            .expect("切换后 Provider 应存在");
        assert_eq!(switched.model, "model-b");
        assert_eq!(
            switched.reasoning_effort,
            Some(keencode_resources::ReasoningEffortSnapshot::Maximum)
        );
    }

    /// 模型与 effort 必须按独立操作域对账；重试只核对自身目标且冷恢复后不回退其他字段。
    #[tokio::test(flavor = "multi_thread")]
    async fn session_config_retries_are_domain_scoped_and_cold_recovery_stable() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = runtime_with_responses_provider(
            storage.path(),
            "http://127.0.0.1:9/v1",
            &["model-a", "model-b"],
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "config-retry-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();

        // 同一个 JSON-RPC nonce 可以分别代表 model 与 effort；effort 重写的字段
        // 不得让 model 的原始收据在重试时变成冲突，也不得让模型回退。
        runtime
            .set_session_model(
                &session_id,
                "shared-json-rpc-nonce",
                "provider-runtime-test",
                "model-a",
            )
            .expect("模型首次设置应成功");
        runtime
            .set_session_effort(&session_id, "shared-json-rpc-nonce", "high")
            .expect("相同 nonce 的 effort 设置不应跨域冲突");
        let after_effort = runtime
            .session_snapshot(&session_id)
            .expect("effort 设置后快照应读取");
        assert_eq!(
            after_effort
                .state
                .provider
                .as_ref()
                .map(|provider| provider.model.as_str()),
            Some("model-a")
        );
        assert_eq!(
            after_effort.state.provider.as_ref().and_then(|provider| {
                provider
                    .reasoning_effort
                    .map(super::reasoning_effort_snapshot_name)
            }),
            Some("high".to_owned())
        );
        let sequence_after_first_pair = after_effort.state.last_sequence;
        runtime
            .set_session_model(
                &session_id,
                "shared-json-rpc-nonce",
                "provider-runtime-test",
                "model-a",
            )
            .expect("模型重试应复用原始收据");
        let after_model_retry = runtime
            .session_snapshot(&session_id)
            .expect("模型重试后快照应读取");
        assert_eq!(
            after_model_retry.state.last_sequence, sequence_after_first_pair,
            "同目标重试不得重写 Provider 快照"
        );
        assert_eq!(
            after_model_retry
                .state
                .provider
                .as_ref()
                .and_then(|provider| provider.reasoning_effort),
            Some(keencode_resources::ReasoningEffortSnapshot::High)
        );
        assert_eq!(
            runtime.set_session_model(
                &session_id,
                "shared-json-rpc-nonce",
                "provider-runtime-test",
                "model-b",
            ),
            Err(AgentRuntimeError::RuntimeOperationFailed),
            "同域同 nonce 的不同模型必须明确冲突"
        );

        // 反向顺序覆盖 effortX -> modelY -> retry effortX，确认模型切换不回退。
        runtime
            .set_session_effort(&session_id, "reverse-json-rpc-nonce", "low")
            .expect("反向 effort 首次设置应成功");
        runtime
            .set_session_model(
                &session_id,
                "reverse-json-rpc-nonce",
                "provider-runtime-test",
                "model-b",
            )
            .expect("反向模型设置应成功");
        runtime
            .set_session_effort(&session_id, "reverse-json-rpc-nonce", "low")
            .expect("effort 重试应复用原始收据");
        let after_reverse_retry = runtime
            .session_snapshot(&session_id)
            .expect("反向重试后快照应读取");
        assert_eq!(
            after_reverse_retry
                .state
                .provider
                .as_ref()
                .map(|provider| provider.model.as_str()),
            Some("model-b")
        );
        assert_eq!(
            after_reverse_retry
                .state
                .provider
                .as_ref()
                .and_then(|provider| provider.reasoning_effort),
            Some(keencode_resources::ReasoningEffortSnapshot::Low)
        );
        assert_eq!(
            runtime.set_session_effort(&session_id, "reverse-json-rpc-nonce", "medium"),
            Err(AgentRuntimeError::RuntimeOperationFailed),
            "同域同 nonce 的不同 effort 必须明确冲突"
        );

        let sequence_before_cold_recovery = after_reverse_retry.state.last_sequence;
        drop(session);
        runtime
            .close_session(&session_id)
            .await
            .expect("配置测试 Session 应关闭以模拟冷恢复");
        let reopened = runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "config-retry-cold-recovery",
            )
            .expect("冷恢复后 Session 应重开");
        let model_record = reopened
            .committed_control_event_in_domain("keencode/session/model", "shared-json-rpc-nonce")
            .expect("冷恢复后模型收据应可查询")
            .expect("模型收据应返回真实 Journal 记录");
        assert!(model_record.sequence > 0);
        assert!(matches!(
            model_record.event,
            SessionEvent::ProviderSnapshotUpdated { ref provider }
                if provider.provider_id == "provider-runtime-test"
                    && provider.model == "model-a"
        ));
        runtime
            .set_session_model(
                &session_id,
                "shared-json-rpc-nonce",
                "provider-runtime-test",
                "model-a",
            )
            .expect("冷恢复后模型重试应幂等");
        runtime
            .set_session_effort(&session_id, "shared-json-rpc-nonce", "high")
            .expect("冷恢复后 effort 重试应幂等");
        let after_cold_retry = runtime
            .session_snapshot(&session_id)
            .expect("冷恢复重试后快照应读取");
        assert_eq!(
            after_cold_retry.state.last_sequence, sequence_before_cold_recovery,
            "冷恢复后的幂等重试不得追加或回放配置事件"
        );
        assert_eq!(
            after_cold_retry
                .state
                .provider
                .as_ref()
                .map(|provider| provider.model.as_str()),
            Some("model-b")
        );
        assert_eq!(
            after_cold_retry
                .state
                .provider
                .as_ref()
                .and_then(|provider| provider.reasoning_effort),
            Some(keencode_resources::ReasoningEffortSnapshot::Low)
        );
    }

    /// 标题校验必须拒绝模型回答、拒绝说明、多行正文和超长候选。
    #[test]
    fn title_validation_rejects_non_title_model_output() {
        assert_eq!(
            validate_generated_title("  修复 Agent Runtime  "),
            Ok("修复 Agent Runtime".to_owned())
        );
        assert_eq!(
            validate_generated_title(
                "我无法直接访问或读取你本地的文件系统。如果你能提供 sample.txt"
            ),
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        assert_eq!(
            validate_generated_title("第一行\n第二行"),
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        assert_eq!(
            validate_generated_title(&"标".repeat(GENERATED_TITLE_MAX_CHARS + 1)),
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn title_generation_close_releases_lease_without_caching_stale_result() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, gate, server) =
            spawn_gated_buffered_responses_server("旧标题", 1, "blocked title");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "title-close-session")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        let title_task = {
            let runtime = Arc::clone(&runtime);
            let session_id = session_id.clone();
            tokio::spawn(async move {
                runtime
                    .generate_title(&session_id, "blocked-title", "blocked title")
                    .await
            })
        };
        gate.wait_for_requests(1).unwrap();
        let closed =
            tokio::time::timeout(Duration::from_secs(2), runtime.close_session(&session_id)).await;
        // 在放行 HTTP 响应之前重开，确保关闭释放锁不依赖供应商返回。
        let reopened = runtime.open_or_create_session(
            project.path(),
            Some(&session_id),
            "title-reopen-session",
        );
        let cancelled_before_response = title_task.is_finished();
        gate.release();
        let title_result = tokio::time::timeout(Duration::from_secs(5), title_task)
            .await
            .unwrap()
            .unwrap();
        // 取消 HTTP 请求后服务端写响应可能收到 BrokenPipe，但线程必须回收。
        let _response_result = server.join().unwrap();
        closed
            .expect("关闭不能等待标题网络超时")
            .expect("关闭应成功");
        let reopened = reopened.expect("标题未返回时也必须释放 Runtime lease");
        assert!(
            cancelled_before_response,
            "关闭应等待标题任务取消并释放句柄"
        );
        assert_eq!(title_result, Err(AgentRuntimeError::SessionUnavailable));
        assert_eq!(
            reopened
                .cached_generated_title(
                    "blocked-title",
                    &super::title_input_sha256("blocked title")
                )
                .unwrap(),
            None,
            "旧标题不能写入重开后的会话"
        );
    }

    /// 首轮 ACK 后等待已提交输入；重复调度只请求一次隔离标题，且不污染聊天正文。
    #[tokio::test(flavor = "multi_thread")]
    async fn automatic_title_after_root_start_is_singleflight_and_persistent() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server_for_requests("自动命名验收", 2);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "automatic-title-session")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .start_root_turn(
                &session_id,
                "automatic-title-turn",
                "修复文件读取缓存",
                RootTurnOptions::default(),
            )
            .await
            .unwrap();
        for _ in 0..8 {
            runtime.schedule_automatic_title(&session_id);
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = session.snapshot().unwrap().state;
                if state.title_source == keencode_resources::TitleSource::Automatic
                    && state
                        .turns
                        .values()
                        .all(|turn| turn.status != TurnStatus::Running)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("已确认用户输入必须触发自动标题");
        let requests = server.join().unwrap().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["tool_choice"] == "none")
                .count(),
            1,
            "只有隔离标题请求禁用工具"
        );
        let state = session.snapshot().unwrap().state;
        assert_eq!(state.title, "自动命名验收");
        assert_eq!(state.generated_titles.len(), 1);
        assert_eq!(
            session
                .transcript()
                .unwrap()
                .iter()
                .filter(
                    |message| message.role == keencode_resources::MessageRole::User
                        && !message.is_meta
                )
                .count(),
            1,
            "命名不得插入额外用户消息"
        );
        runtime.schedule_automatic_title(&session_id);
        assert_eq!(
            session.snapshot().unwrap().state.last_sequence,
            state.last_sequence
        );
    }

    /// 同一标题 operationId 的并发与顺序重试只能发起一次真实 Provider 请求。
    #[tokio::test(flavor = "multi_thread")]
    async fn title_generation_is_singleflight_and_uses_persistent_result() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let (base_url, server) = spawn_buffered_responses_server("  并发标题  ");
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                tool_calling: true,
                structured_output: keencode_model::StructuredOutputCapability::Native,
                ..ProviderCapabilities::default()
            }),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "title-session-operation")
            .expect("标题测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();

        let (first, second) = tokio::join!(
            runtime.generate_title(&session_id, "title-operation", "实现 Agent Runtime"),
            runtime.generate_title(&session_id, "title-operation", "实现 Agent Runtime")
        );
        let request = finish_responses_server(server);
        assert_eq!(first, Ok("并发标题".to_owned()));
        assert_eq!(second, Ok("并发标题".to_owned()));
        assert_eq!(request["model"], "test-model");
        assert_eq!(request["tool_choice"], "none");
        assert!(
            request.get("text").is_none(),
            "模型支持原生 Schema 时标题也保持纯文本"
        );
        assert!(request.get("tools").is_none(), "标题不得携带记忆结果工具");

        assert_eq!(
            runtime
                .generate_title(&session_id, "title-operation", "实现 Agent Runtime")
                .await,
            Ok("并发标题".to_owned())
        );
        assert_eq!(
            runtime
                .generate_title(&session_id, "title-operation", "不同输入")
                .await,
            Err(AgentRuntimeError::RuntimeOperationFailed)
        );
        assert_eq!(
            runtime
                .title_generation_gates
                .lock()
                .expect("标题锁表应读取")
                .len(),
            1
        );
        runtime
            .close_session(&session_id)
            .await
            .expect("关闭 Session 应释放标题锁");
        assert!(
            runtime
                .title_generation_gates
                .lock()
                .expect("标题锁表应读取")
                .is_empty()
        );
    }

    /// 真实 HTTP 返回非空但不完整的文本时，标题不得写缓存，记忆不得接收该正文。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_generation_rejects_incomplete_text_without_caching_titles() {
        for reason in ["max_output_tokens", "content_filter", "synthetic_unknown"] {
            for purpose in ["title", "memory"] {
                let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
                let project = tempfile::tempdir().expect("应创建合成项目目录");
                let (base_url, server) = spawn_buffered_responses_server_with_status(
                    "尚未完整结束",
                    "incomplete",
                    Some(reason),
                );
                let runtime = runtime_with_responses_capabilities(
                    storage.path(),
                    &base_url,
                    &["test-model"],
                    Some(ProviderCapabilities {
                        tool_calling: true,
                        ..ProviderCapabilities::default()
                    }),
                );
                let session = runtime
                    .open_or_create_session(project.path(), None, "incomplete-session")
                    .expect("合成 Session 应创建");
                let session_id = session.session_id().as_str().to_owned();
                let input = "为合成任务生成短文本";
                if purpose == "title" {
                    assert!(
                        runtime
                            .generate_title(&session_id, "incomplete-title", input)
                            .await
                            .is_err(),
                        "{reason} 的文本不得成为成功标题"
                    );
                    assert!(
                        session
                            .cached_generated_title(
                                "incomplete-title",
                                &super::title_input_sha256(input),
                            )
                            .expect("标题缓存应可读取")
                            .is_none(),
                        "失败的标题调用不得产生持久成功缓存"
                    );
                } else {
                    assert!(
                        runtime
                            .generate_isolated(
                                "session-isolated-test",
                                "整合合成事实",
                                input,
                                10,
                                keencode_model::StructuredOutputConfig::new(
                                    "test_memory",
                                    json!({"type": "object"}),
                                ),
                            )
                            .await
                            .is_err(),
                        "{reason} 的文本不得作为成功记忆返回"
                    );
                }
                let request = finish_responses_server(server);
                assert_eq!(request["model"], "test-model");
                if purpose == "memory" {
                    assert_eq!(request["tool_choice"], "required");
                    assert_eq!(
                        request["tools"][0]["name"],
                        keencode_agent::STRUCTURED_OUTPUT_TOOL_NAME
                    );
                    assert!(request.get("text").is_none());
                } else {
                    assert_eq!(request["tool_choice"], "none");
                    assert!(request.get("text").is_none(), "标题不应被强制为 JSON");
                }
            }
        }
    }

    /// 记忆必须实际发送原生 Schema，且即使 HTTP 成功也拒绝尾随文本和错误结构。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_memory_generation_requests_and_validates_native_schema() {
        let schema = json!({
            "type": "object",
            "properties": {"memoryMd": {"type": "string"}},
            "required": ["memoryMd"],
            "additionalProperties": false
        });
        for (text, accepted) in [
            (r##"{"memoryMd":"# 已验证合成记忆"}"##, true),
            (r##"{"memoryMd":"# 记忆"} trailing"##, false),
            (r##"{"memoryMd":42}"##, false),
            (r##"{}"##, false),
            (r##"{"memoryMd":"# 记忆","extra":true}"##, false),
        ] {
            let storage = tempfile::tempdir().expect("应创建记忆测试存储目录");
            let body = json!({
                "id": "response-runtime-test", "object": "response", "model": "test-model",
                "status": "completed",
                "output": [{
                    "id": "message-runtime-test", "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": text}]
                }],
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            });
            let expected_requests = if accepted { 1 } else { 6 };
            let (base_url, server) =
                spawn_buffered_responses_sequence(vec![body; expected_requests]);
            let runtime = runtime_with_responses_capabilities(
                storage.path(),
                &base_url,
                &["test-model"],
                Some(ProviderCapabilities {
                    structured_output: keencode_model::StructuredOutputCapability::Native,
                    ..ProviderCapabilities::default()
                }),
            );
            let result = runtime
                .generate_isolated(
                    "session-isolated-test",
                    "只返回约定的合成记忆 JSON",
                    "合成数据",
                    10,
                    keencode_model::StructuredOutputConfig::new("test_memory", schema.clone()),
                )
                .await;
            assert_eq!(result.is_ok(), accepted, "响应必须严格验证：{text}");
            if accepted {
                assert_eq!(
                    serde_json::from_str::<Value>(&result.unwrap()).unwrap(),
                    json!({"memoryMd": "# 已验证合成记忆"}),
                );
            }
            let requests = server
                .join()
                .expect("本地模型服务线程不应 panic")
                .expect("本地模型服务应成功");
            assert_eq!(requests.len(), expected_requests);
            let request = &requests[0];
            assert_eq!(request["tool_choice"], "none");
            assert_eq!(request["text"]["format"]["type"], "json_schema");
            assert_eq!(request["text"]["format"]["strict"], true);
            assert_eq!(request["text"]["format"]["schema"], schema);
        }
    }

    /// 隔离记忆必须在同一总超时内复用共用纠正通道，并把坏候选留在私有请求中。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_memory_generation_corrects_invalid_native_response() {
        let schema = json!({
            "type": "object",
            "properties": {"memoryMd": {"type": "string"}},
            "required": ["memoryMd"],
            "additionalProperties": false
        });
        let response_body = |id: &str, text: &str| {
            json!({
                "id": id, "object": "response", "model": "test-model",
                "status": "completed",
                "output": [{
                    "id": format!("message-{id}"), "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": text}]
                }],
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            })
        };
        let (base_url, server) = spawn_buffered_responses_sequence(vec![
            response_body("invalid-memory", r#"{"memoryMd":42}"#),
            response_body("valid-memory", r##"{"memoryMd":"# 已纠正记忆"}"##),
        ]);
        let storage = tempfile::tempdir().expect("应创建记忆测试存储目录");
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                structured_output: keencode_model::StructuredOutputCapability::Native,
                ..ProviderCapabilities::default()
            }),
        );

        let result = runtime
            .generate_isolated(
                "session-isolated-test",
                "只返回约定的合成记忆 JSON",
                "合成数据",
                10,
                keencode_model::StructuredOutputConfig::new("test_memory", schema.clone()),
            )
            .await
            .expect("第一次纠正应成功");

        assert_eq!(
            serde_json::from_str::<Value>(&result).unwrap(),
            json!({"memoryMd": "# 已纠正记忆"})
        );
        let requests = server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["tool_choice"], "none");
        assert_eq!(requests[1]["text"]["format"]["schema"], schema);
        let input = requests[1]["input"].as_array().expect("应编码纠正上下文");
        assert_eq!(input.len(), 4);
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(input[2]["content"][0]["text"], r#"{"memoryMd":42}"#);
        assert_eq!(input[3]["role"], "user");
        let correction = input[3]["content"][0]["text"]
            .as_str()
            .expect("纠正说明应编码为文本");
        assert!(correction.contains("\"failure\":\"schema_violation\""));
        assert!(correction.contains(&schema.to_string()));
    }

    /// 非原生记忆复用结果工具；唯一结果必须严格验证，不把模型工具调用交给执行器。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_memory_generation_uses_reserved_result_tool() {
        let schema = json!({
            "type": "object",
            "properties": {"memoryMd": {"type": "string"}},
            "required": ["memoryMd"],
            "additionalProperties": false
        });
        for (case, arguments, extra, accepted) in [
            (
                "valid",
                json!({"value": {"memoryMd": "合成记忆"}}),
                None,
                true,
            ),
            (
                "empty_string",
                json!({"value": {"memoryMd": ""}}),
                None,
                true,
            ),
            ("missing", json!({}), None, false),
            (
                "wrong_type",
                json!({"value": {"memoryMd": 42}}),
                None,
                false,
            ),
            (
                "extra_value",
                json!({"value": {"memoryMd": "记忆", "extra": true}}),
                None,
                false,
            ),
            (
                "extra_wrapper",
                json!({"value": {"memoryMd": "记忆"}, "extra": true}),
                None,
                false,
            ),
            ("non_object", json!([{"memoryMd": "记忆"}]), None, false),
            (
                "visible_text",
                json!({"value": {"memoryMd": "记忆"}}),
                Some(json!({
                    "type": "message", "id": "extra-message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "额外正文"}]
                })),
                false,
            ),
            (
                "ordinary_tool",
                json!({"value": {"memoryMd": "记忆"}}),
                Some(json!({
                    "type": "function_call", "id": "extra-call", "call_id": "ordinary-call",
                    "name": "write_file", "arguments": "{}", "status": "completed"
                })),
                false,
            ),
            (
                "duplicate_result",
                json!({"value": {"memoryMd": "记忆"}}),
                Some(json!({
                    "type": "function_call", "id": "extra-call", "call_id": "duplicate-call",
                    "name": keencode_agent::STRUCTURED_OUTPUT_TOOL_NAME,
                    "arguments": r#"{"value":{"memoryMd":"另一份"}} "#, "status": "completed"
                })),
                false,
            ),
        ] {
            let storage = tempfile::tempdir().expect("应创建合成记忆目录");
            let mut output = vec![json!({
                "type": "function_call", "id": "result-item", "call_id": "result-call",
                "name": keencode_agent::STRUCTURED_OUTPUT_TOOL_NAME,
                "arguments": arguments.to_string(), "status": "completed"
            })];
            if let Some(extra) = extra {
                output.push(extra);
            }
            let body = json!({
                "id": "response-runtime-test", "object": "response", "model": "test-model",
                "status": "completed", "output": output,
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            });
            // 非对象工具参数在模型归约层直接形成 Protocol 错误；其余完整结构错误
            // 会真实消耗五次私有纠正后返回最后一次原始诊断。
            let expected_requests = if accepted || case == "non_object" {
                1
            } else {
                6
            };
            let (base_url, server) =
                spawn_buffered_responses_sequence(vec![body; expected_requests]);
            let runtime = runtime_with_responses_capabilities(
                storage.path(),
                &base_url,
                &["test-model"],
                Some(ProviderCapabilities {
                    tool_calling: true,
                    ..ProviderCapabilities::default()
                }),
            );
            let result = runtime
                .generate_isolated(
                    "session-isolated-test",
                    "通过结果通道提交合成事实",
                    "合成数据",
                    10,
                    keencode_model::StructuredOutputConfig::new("test_memory", schema.clone()),
                )
                .await;
            assert_eq!(result.is_ok(), accepted, "{case}: {result:?}");
            if accepted {
                assert_eq!(
                    serde_json::from_str::<Value>(&result.unwrap()).unwrap(),
                    arguments["value"]
                );
            }
            let requests = server
                .join()
                .expect("本地模型服务线程不应 panic")
                .expect("本地模型服务应成功");
            assert_eq!(requests.len(), expected_requests, "{case}");
            let request = &requests[0];
            assert_eq!(request["tool_choice"], "required");
            assert_eq!(request["parallel_tool_calls"], false);
            let input = request["input"].as_array().expect("应编码输入消息");
            assert_eq!(input.len(), 2, "隔离请求只有一条系统消息和一条用户消息");
            assert_eq!(input[0]["role"], "developer");
            let instructions = input[0]["content"][0]["text"].as_str().unwrap();
            assert!(instructions.starts_with("通过结果通道提交合成事实"));
            assert!(instructions.contains("value field of the sole result tool"));
            assert_eq!(input[1]["role"], "user");
            assert!(request.get("text").is_none());
            assert_eq!(request["tools"].as_array().unwrap().len(), 1);
            assert_eq!(
                request["tools"][0]["name"],
                keencode_agent::STRUCTURED_OUTPUT_TOOL_NAME
            );
            assert_eq!(
                request["tools"][0]["parameters"]["properties"]["value"],
                schema
            );
            assert_eq!(
                request["tools"][0]["parameters"]["required"],
                json!(["value"])
            );
            assert_eq!(
                request["tools"][0]["parameters"]["additionalProperties"],
                false
            );
        }
    }

    /// 两种结构化能力都缺失时必须在联网前失败，而不是盲发 Schema 或降级为文本。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_memory_generation_rejects_missing_capabilities_before_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let storage = tempfile::tempdir().unwrap();
        let runtime = runtime_with_responses_capabilities(
            storage.path(),
            &format!("http://{}/v1", listener.local_addr().unwrap()),
            &["test-model"],
            Some(ProviderCapabilities::default()),
        );
        let error = runtime
            .generate_isolated(
                "session-isolated-test",
                "合成系统指令",
                "合成输入",
                2,
                keencode_model::StructuredOutputConfig::new(
                    "test_memory",
                    json!({"type": "object"}),
                ),
            )
            .await
            .expect_err("能力缺失必须失败");
        assert!(matches!(error.downcast_ref::<keencode_model::ModelError>(),
            Some(keencode_model::ModelError::UnsupportedCapability { capability, .. }) if capability == "structured_output"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    /// 隔离推理（记忆与标题）必须把会话标识带到 Provider HTTP 边界。
    #[tokio::test(flavor = "multi_thread")]
    async fn isolated_generation_carries_session_identity_to_provider_boundary() {
        let storage = tempfile::tempdir().expect("应创建记忆测试存储目录");
        let body = json!({
            "id": "response-runtime-test", "object": "response", "model": "test-model",
            "status": "completed",
            "output": [{
                "id": "message-runtime-test", "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": r##"{"memoryMd":"# 会话路由记忆"}"##}]
            }],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
        });
        let (base_url, server) = spawn_buffered_responses_sequence(vec![body]);
        let observer = Arc::new(SessionRecordingObserver::default());
        let runtime = runtime_with_responses_registry(
            keencode_provider::ProviderRegistry::with_request_observer(observer.clone()),
            storage.path(),
            &base_url,
            &["test-model"],
            Some(ProviderCapabilities {
                structured_output: keencode_model::StructuredOutputCapability::Native,
                ..ProviderCapabilities::default()
            }),
        );
        runtime
            .generate_isolated(
                "session-memory-route",
                "只返回约定的合成记忆 JSON",
                "合成数据",
                10,
                keencode_model::StructuredOutputConfig::new(
                    "test_memory",
                    json!({
                        "type": "object",
                        "properties": {"memoryMd": {"type": "string"}},
                        "required": ["memoryMd"],
                        "additionalProperties": false
                    }),
                ),
            )
            .await
            .expect("隔离记忆调用应成功");
        server
            .join()
            .expect("本地模型服务线程不应 panic")
            .expect("本地模型服务应成功");

        // 端点在缺少该标识时直接拒绝请求，因此每条观测都必须带上本会话标识。
        let observations = observer.observations();
        assert!(!observations.is_empty(), "隔离调用必须产生请求观测");
        for (session_id, purpose) in observations {
            assert_eq!(session_id.as_deref(), Some("session-memory-route"));
            assert_eq!(purpose.as_deref(), Some("memory"));
        }
    }

    /// 根 Turn 必须把 Session 持久推理强度写入每次 Provider 请求。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_turn_applies_persisted_reasoning_effort_to_provider_request() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let (base_url, server) = spawn_buffered_responses_server("完成");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "reasoning-turn-operation")
            .expect("推理测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .set_session_effort(&session_id, "reasoning-high", "high")
            .expect("推理强度应持久化");

        assert_eq!(
            runtime
                .start_root_turn(
                    &session_id,
                    "turn-reasoning",
                    "检查推理字段",
                    RootTurnOptions::default(),
                )
                .await
                .expect("根 Turn 应启动"),
            RootTurnStartOutcome::Started
        );
        let request = finish_responses_server(server);
        assert_eq!(request["reasoning"]["effort"], "high");
        assert_eq!(request["reasoning"]["summary"], "auto");
    }

    /// rewind 后仅为控制面引用保留的根 Turn 骨架不得在冷回放中生成空对话。
    #[tokio::test]
    async fn close_session_releases_collaboration_runtime() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "close-session-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("Collaboration 装配应建立");
        assert!(!runtime.session_has_active_work(&session_id).unwrap());
        let pending_turn_id = keencode_agent::TurnId::new("turn-terminal-pending").unwrap();
        collaboration
            .execution
            .state
            .lock()
            .unwrap()
            .running_turns
            .insert(
                pending_turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: keencode_agent::AgentId::new("root").unwrap(),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "待关闭任务".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: TurnCancellation::new(),
                    terminal_outcome: Some(keencode_agent::AgentTurnOutcome::Interrupted),
                },
            );
        assert!(runtime.session_has_active_work(&session_id).unwrap());
        collaboration
            .execution
            .state
            .lock()
            .unwrap()
            .running_turns
            .remove(&pending_turn_id);

        runtime
            .close_session(&session_id)
            .await
            .expect("完整 Session 应关闭");
        assert!(
            runtime
                .collaboration_sessions
                .lock()
                .expect("Collaboration 表应读取")
                .get(&session_id)
                .is_none()
        );
        assert!(runtime.runtime_manager().get(session_id.clone()).is_err());
        assert!(
            collaboration
                .background_completion_cancel
                .lock()
                .expect("完成泵停止状态应读取")
                .is_none()
        );

        drop(collaboration);
        drop(session);
        let reopened = runtime
            .open_or_create_session(project.path(), Some(&session_id), "close-session-reopen")
            .expect("全部旧 lease 释放后应重开同一 Session");
        drop(reopened);
        runtime
            .close_session(&session_id)
            .await
            .expect("重开的 Session 应关闭");
    }

    /// 根关闭尚在静止等待时，设置更新必须等到 map 中的旧 Runtime 已完整移除。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_agent_limit_update_serializes_with_session_close() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "close-limit-race-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("Collaboration 装配应建立");
        let pending_turn_id =
            AgentTurnId::new("turn-close-limit-race").expect("测试 Turn 标识应有效");
        let cancellation = TurnCancellation::new();
        collaboration
            .execution
            .state
            .lock()
            .expect("执行状态锁应可用")
            .running_turns
            .insert(
                pending_turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: collaboration.root_agent_id.clone(),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "关闭竞态占位任务".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: cancellation.clone(),
                    terminal_outcome: Some(AgentTurnOutcome::Interrupted),
                },
            );

        let close_runtime = Arc::clone(&runtime);
        let close_session_id = session_id.clone();
        let close =
            tokio::spawn(async move { close_runtime.close_session(&close_session_id).await });
        tokio::time::timeout(Duration::from_secs(2), cancellation.cancelled())
            .await
            .expect("关闭应进入根树静止等待");
        let map_locked_during_close = runtime.collaboration_sessions.try_lock().is_err();

        let setter_runtime = Arc::clone(&runtime);
        let setter =
            tokio::task::spawn_blocking(move || setter_runtime.set_background_agent_limit(2));
        {
            let mut state = collaboration
                .execution
                .state
                .lock()
                .expect("执行状态锁应可用");
            state.running_turns.remove(&pending_turn_id);
            collaboration.execution.idle.notify_all();
        }

        assert_eq!(close.await.expect("关闭任务不应 panic"), Ok(()));
        assert_eq!(setter.await.expect("设置任务不应 panic"), Ok(()));
        assert!(
            map_locked_during_close,
            "close_session 必须持有映射锁直到根关闭并移除完成"
        );
        assert!(
            runtime
                .collaboration_sessions
                .lock()
                .expect("Collaboration 表应读取")
                .get(&session_id)
                .is_none()
        );
        assert_eq!(runtime.background_agent_limit.load(Ordering::Acquire), 2);
        assert_eq!(
            runtime
                .collaboration_global_turn_limiter
                .capacity()
                .unwrap()
                .1,
            2
        );
    }

    /// 协调器关闭因 Store 冻结失败时，仍必须清理完成泵和 Runtime 注册。
    #[tokio::test]
    async fn close_session_cleans_resources_after_collaboration_recovery_failure() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "close-recovery-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("Collaboration 装配应建立");
        let pending_turn_id = keencode_agent::TurnId::new("turn-close-recovery-pending")
            .expect("测试 Turn 标识应有效");
        {
            let mut state = collaboration
                .execution
                .state
                .lock()
                .expect("执行状态锁应可用");
            state.accepted_turns.insert(pending_turn_id.clone());
            state.running_turns.insert(
                pending_turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效"),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "恢复失败任务".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: TurnCancellation::new(),
                    terminal_outcome: Some(keencode_agent::AgentTurnOutcome::Interrupted),
                },
            );
        }
        let transition_path = runtime
            .session_storage_directory(&session_id)
            .unwrap()
            .join("collaboration-v2.json");
        std::fs::write(&transition_path, b"invalid Collaboration JSON")
            .expect("测试应能破坏 Collaboration 提交文件");

        assert_eq!(
            runtime.close_session(&session_id).await,
            Err(AgentRuntimeError::RecoveryRequired)
        );
        assert!(
            runtime
                .collaboration_sessions
                .lock()
                .expect("Collaboration 表应读取")
                .get(&session_id)
                .is_none()
        );
        assert!(runtime.runtime_manager().get(session_id).is_err());
        let state = collaboration
            .execution
            .state
            .lock()
            .expect("关闭后的执行状态应读取");
        assert!(!state.running_turns.contains_key(&pending_turn_id));
        assert!(!state.accepted_turns.contains(&pending_turn_id));
        assert!(
            !collaboration
                .execution
                .background_tasks
                .is_accepting_tasks()
        );
        assert!(
            collaboration
                .background_completion_cancel
                .lock()
                .expect("完成泵停止状态应读取")
                .is_none()
        );
    }

    /// 停机取消后的执行回收必须仍能在调用 shutdown 的同一个 Tokio worker 上推进。
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_yields_to_cancelled_runner_cleanup_on_same_worker() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "shutdown-same-worker")
            .expect("测试 Session 应创建");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("Collaboration 装配应建立");
        let turn_id =
            keencode_agent::TurnId::new("turn-shutdown-same-worker").expect("执行槽标识应有效");
        let cancellation = TurnCancellation::new();
        {
            let mut state = collaboration.execution.state.lock().unwrap();
            state.accepted_turns.insert(turn_id.clone());
            state.running_turns.insert(
                turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: keencode_agent::AgentId::new("root").unwrap(),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "验证执行槽异步回收，不调用模型".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: cancellation.clone(),
                    terminal_outcome: None,
                },
            );
        }
        let (ready, registered) = tokio::sync::oneshot::channel();
        let cleanup = tokio::spawn({
            let execution = collaboration.execution.clone();
            async move {
                ready.send(()).expect("应通知取消等待即将注册");
                cancellation.cancelled().await;
                super::release_runtime_turn_state(
                    execution.state.as_ref(),
                    execution.idle.as_ref(),
                    &turn_id,
                    true,
                );
            }
        });
        registered.await.expect("同 worker 的取消等待应已挂起");

        let started = Instant::now();
        let result = runtime.shutdown().await;
        cleanup.await.expect("取消后的执行槽回收不应 panic");
        assert_eq!(result, Ok(()), "停机不能阻塞自己依赖的异步回收");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            collaboration
                .execution
                .state
                .lock()
                .unwrap()
                .running_turns
                .is_empty()
        );
        assert!(
            !collaboration
                .execution
                .accepting_work
                .load(Ordering::Acquire)
        );
        assert!(
            runtime
                .runtime_manager()
                .get(session.session_id().as_str().to_owned())
                .is_err()
        );
    }

    /// 标准信封自身仍必须保留严格 Session/Turn/Agent 身份。
    #[test]
    fn goal_usage_identity_is_bounded_and_unambiguous() {
        use super::goal_usage_operation_id;
        let long = "x".repeat(512);
        let base = [
            &long[..],
            "turn",
            "root",
            "agent_round",
            "4294967295",
            "4294967295",
        ];
        let original = goal_usage_operation_id(&base);
        assert_eq!(original.len(), 75);
        assert_eq!(original, goal_usage_operation_id(&base));
        for index in 0..base.len() {
            let mut changed = base;
            changed[index] = "other";
            assert_ne!(original, goal_usage_operation_id(&changed));
        }
        assert_ne!(
            goal_usage_operation_id(&["a:b", "c", "root", "agent_round", "1", "1"]),
            goal_usage_operation_id(&["a", "b:c", "root", "agent_round", "1", "1"])
        );
    }

    /// 真实 Goal Sink 必须区分失败调用与同 Round 重试，并只把成功响应写入 Transcript。
    #[tokio::test]
    async fn goal_usage_sink_distinguishes_failed_round_retry_attempts() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: format!("session-{}", "a".repeat(64)),
                title: "Goal 用量重试测试".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("Session 应创建");
        let persistent_state =
            Arc::new(PersistentAgentState::open(session.clone()).expect("Goal 持久控制器应创建"));
        persistent_state
            .create_goal(
                "goal-create-retry-attempts",
                GoalDraft {
                    title: "验证模型用量".to_owned(),
                    objective: "验证失败调用和同 Round 重试均正确累计一次".to_owned(),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
            .expect("活跃 Goal 应创建");

        let first_usage = TokenUsage {
            input_tokens: Some(31),
            output_tokens: Some(4),
            reasoning_tokens: Some(1),
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(35),
        };
        let retry_usage = TokenUsage {
            input_tokens: Some(41),
            output_tokens: Some(6),
            reasoning_tokens: Some(2),
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(47),
        };
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                ScriptedReply::new(vec![
                    Ok(ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    }),
                    Ok(ModelStreamEvent::Usage {
                        usage: first_usage.clone(),
                    }),
                    Err(ModelError::ContextLengthExceeded {
                        message: "首次 Agent Round 上下文超限".to_owned(),
                    }),
                ]),
                completed_reply("强制压缩摘要"),
                ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::TextDelta {
                        index: 0,
                        delta: "重试后的成功响应".to_owned(),
                    },
                    ModelStreamEvent::Usage {
                        usage: retry_usage.clone(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: StopReason::Completed,
                    },
                ]),
            ],
        ));
        let context_provider = provider.clone();
        let runner = session.bind_agent_runner_with_usage_sink(
            AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
                .with_context_manager(ContextManager::for_provider(context_provider)),
            Arc::new(RuntimeGoalUsageSink {
                session_id: session.session_id().as_str().to_owned(),
                persistent_state: persistent_state.clone(),
                owner: std::sync::Weak::new(),
            }),
        );
        let input_messages = vec![
            ModelMessage::text(MessageRole::User, format!("历史一{}", "旧".repeat(700))),
            ModelMessage::text(MessageRole::User, format!("历史二{}", "旧".repeat(700))),
            ModelMessage::text(MessageRole::User, "当前问题"),
        ];
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new("27b70d1b-3ef9-4252-9dbd-9026a2d32e3b")
                .expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new("root").expect("根 Agent ID 应有效"),
            "test-model",
            input_messages.clone(),
            PlanGuard::inactive(),
        );
        let result = runner
            .run_turn(RuntimeTurnRequest::root(
                request,
                input_messages,
                "验证失败调用与强制重试用量",
            ))
            .await
            .expect("失败调用已记账且重试成功时 Turn 应完成");

        assert!(result.is_success());
        let goal = persistent_state
            .goal_snapshot()
            .expect("Goal 快照应读取")
            .goal
            .expect("活跃 Goal 应保留");
        assert_eq!(goal.tokens_used, 35 + 47);
        // 执行时长改由根回合终态按墙钟统一累计，模型 Round 只贡献 Token。
        assert_eq!(goal.time_used_seconds, 0);

        let state = session.snapshot().expect("Session 快照应读取").state;
        assert_eq!(state.model_rounds.len(), 1);
        assert_eq!(state.model_rounds[0].model_round, 1);
        assert_eq!(state.model_rounds[0].usage, retry_usage);
        assert_eq!(
            state
                .transcript
                .iter()
                .filter(|record| matches!(
                    record,
                    keencode_resources::TranscriptRecord::SegmentCommitted(_)
                ))
                .count(),
            1,
            "失败调用不得写入 Transcript 段"
        );
    }

    /// 根回合墙钟只在归属 Session 的活跃 Goal 上累计，相同 Turn 幂等，跨 Session 跳过。
    #[tokio::test]
    async fn goal_turn_elapsed_commits_wall_time_for_owner_active_goal() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: format!("session-{}", "c".repeat(64)),
                title: "Goal 回合墙钟测试".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("Session 应创建");
        let persistent_state =
            Arc::new(PersistentAgentState::open(session.clone()).expect("Goal 持久控制器应创建"));
        persistent_state
            .create_goal(
                "goal-turn-wall",
                GoalDraft {
                    title: "验证回合墙钟".to_owned(),
                    objective: "回合结束后按墙钟累计执行时长".to_owned(),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
            .expect("活跃 Goal 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let no_owner = std::sync::Weak::new();

        commit_goal_turn_elapsed(
            &session_id,
            persistent_state.as_ref(),
            &no_owner,
            "turn-wall-1",
            17 * 60 + 33,
        )
        .expect("根回合墙钟应累计");
        // 相同 Turn 的重复提交按幂等操作标识去重，不得重复累计。
        commit_goal_turn_elapsed(
            &session_id,
            persistent_state.as_ref(),
            &no_owner,
            "turn-wall-1",
            17 * 60 + 33,
        )
        .expect("相同 Turn 的重复提交应幂等");
        let goal = persistent_state
            .goal_snapshot()
            .expect("Goal 快照应读取")
            .goal
            .expect("活跃 Goal 应保留");
        assert_eq!(goal.time_used_seconds, 17 * 60 + 33);

        // 非归属 Session 的时长不消耗当前 Goal。
        commit_goal_turn_elapsed(
            "other-session",
            persistent_state.as_ref(),
            &no_owner,
            "turn-wall-2",
            60,
        )
        .expect("非归属 Session 的提交应直接跳过");
        let goal = persistent_state
            .goal_snapshot()
            .expect("Goal 快照应读取")
            .goal
            .expect("活跃 Goal 应保留");
        assert_eq!(goal.time_used_seconds, 17 * 60 + 33);
    }

    /// Journal 追加结果不确定时，Store pending 必须保留，随后同一 Turn 重试才可清理。
    #[tokio::test]
    async fn waiting_capacity_journal_indeterminate_retains_pending_until_retry() {
        let (_storage, _project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let first = initial.unstarted_turn_terminations[0].clone();
        let second = initial.unstarted_turn_terminations[1].clone();
        let session_id = session.session_id().as_str().to_owned();

        keencode_resources::test_support::set_append_fault(
            keencode_resources::test_support::AppendFault::Flush,
        );
        assert_eq!(
            runtime.background_task_cancel_outcome(&session_id, first.turn_id.as_str()),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        let after_failure = collaboration
            .store
            .load_transition_snapshot()
            .expect("Journal 不确定后 Store 快照应读取")
            .expect("Journal 不确定后 pending 快照应存在");
        assert_eq!(after_failure.unstarted_turn_terminations.len(), 2);
        assert!(matches!(
            session
                .snapshot()
                .expect("Journal 不确定后的 Runtime 快照应读取")
                .state
                .sub_agents
                .get(&ResourceAgentId::new(first.agent_id.as_str().to_owned()).unwrap())
                .map(|agent| &agent.status),
            Some(SubAgentStatus::Interrupted)
        ));
        let runtime_snapshot = session
            .snapshot()
            .expect("Journal 不确定后的 Runtime 控制状态应读取");
        assert!(runtime_snapshot.recovery_required);
        assert_eq!(runtime_snapshot.pending_indeterminate_events, 1);

        keencode_resources::test_support::clear_append_fault();
        runtime
            .background_task_cancel_outcome(&session_id, first.turn_id.as_str())
            .expect("相同 WaitingCapacity 证据应可重试对账");
        let after_retry = collaboration
            .store
            .load_transition_snapshot()
            .expect("重试后的 Store 快照应读取")
            .expect("重试后的 Store 快照应存在");
        assert_eq!(after_retry.unstarted_turn_terminations.len(), 1);
        let first_resource_turn = ResourceTurnId::new(first.turn_id.as_str().to_owned()).unwrap();
        let first_resource_agent =
            ResourceAgentId::new(first.agent_id.as_str().to_owned()).unwrap();
        let state = session
            .snapshot()
            .expect("重试后的 Journal 快照应读取")
            .state;
        assert_eq!(
            state
                .turns
                .get(&first_resource_turn)
                .map(|turn| turn.status.clone()),
            Some(TurnStatus::Cancelled)
        );
        assert_eq!(
            state
                .sub_agents
                .get(&first_resource_agent)
                .map(|agent| agent.status.clone()),
            Some(SubAgentStatus::Interrupted)
        );

        runtime
            .background_task_cancel_outcome(&session_id, second.turn_id.as_str())
            .expect("剩余 pending 证据也应可对账");
        assert!(
            collaboration
                .store
                .load_transition_snapshot()
                .expect("最终 Store 快照应读取")
                .expect("最终 Store 快照应存在")
                .unstarted_turn_terminations
                .is_empty()
        );
        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }

    /// Journal 已可见事件但正文冲突时，Runtime 必须硬冻结并保留 Store pending。
    #[tokio::test]
    async fn waiting_capacity_journal_conflict_fails_closed_and_keeps_pending() {
        let (_storage, _project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let first = initial.unstarted_turn_terminations[0].clone();
        let session_id = session.session_id().as_str().to_owned();
        keencode_resources::test_support::set_append_fault(
            keencode_resources::test_support::AppendFault::Flush,
        );
        assert_eq!(
            runtime.background_task_cancel_outcome(&session_id, first.turn_id.as_str()),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        keencode_resources::test_support::clear_append_fault();

        let pending = collaboration
            .store
            .load_transition_snapshot()
            .expect("冲突测试 pending 快照应读取")
            .expect("冲突测试 pending 快照应存在");
        let record = pending
            .unstarted_turn_terminations
            .iter()
            .find(|record| record.turn_id == first.turn_id)
            .expect("冲突测试应找到目标 pending")
            .clone();
        let mut conflicting = super::unstarted_turn_termination_request_from_record(
            &session,
            &pending.commit.checkpoint,
            &record,
        )
        .expect("原始 pending 应能还原为 Runtime 请求");
        conflicting.agent.task.push_str("-tampered");
        assert!(matches!(
            session.record_unstarted_turn_termination(conflicting),
            Err(keencode_runtime::RuntimeError::RecoveryRequired)
        ));
        let runtime_snapshot = session.snapshot().expect("冲突后的 Runtime 快照应读取");
        assert!(runtime_snapshot.recovery_required);
        assert_eq!(runtime_snapshot.pending_indeterminate_events, 1);
        assert_eq!(
            collaboration
                .store
                .load_transition_snapshot()
                .expect("冲突后的 Store 快照应读取")
                .expect("冲突后的 Store 快照应存在")
                .unstarted_turn_terminations
                .len(),
            2
        );
    }

    /// Journal 追加结果不确定后进程退出，冷恢复必须逐条对账并清理全部 pending。
    #[tokio::test]
    async fn waiting_capacity_journal_indeterminate_cold_recovery_reconciles_pending() {
        let (storage, project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let first = initial.unstarted_turn_terminations[0].clone();
        let session_id = session.session_id().as_str().to_owned();
        keencode_resources::test_support::set_append_fault(
            keencode_resources::test_support::AppendFault::Flush,
        );
        assert_eq!(
            runtime.background_task_cancel_outcome(&session_id, first.turn_id.as_str()),
            Err(AgentRuntimeError::RecoveryRequired)
        );
        keencode_resources::test_support::clear_append_fault();

        let root_turn = keencode_agent::TurnId::new("turn-waiting-capacity-batch-root")
            .expect("根 Turn 标识应有效");
        collaboration
            .coordinator
            .complete_turn(
                &collaboration.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    // 与 persist_completed_root_turn 真正保存的模型最终文本保持一致。
                    final_message: Some("完成".to_owned()),
                },
            )
            .expect("退出前根 Turn 协作终态应提交");
        runtime
            .collaboration_sessions
            .lock()
            .expect("测试 Collaboration 表应可写")
            .remove(&session_id);
        drop(collaboration);
        drop(session);
        runtime
            .runtime_manager
            .close(session_id.clone())
            .expect("旧 Runtime Session 应关闭");
        drop(runtime);

        let recovered_runtime =
            Arc::new(AgentRuntime::new(storage.path()).expect("冷恢复 Runtime 应创建"));
        let reopened = recovered_runtime
            .open_or_create_session(project.path(), Some(&session_id), "unused")
            .expect("冷恢复 Session 应打开");
        let recovered = match recovered_runtime.ensure_collaboration_runtime(
            &reopened,
            RootAgentSeed {
                model: "test-model".to_owned(),
                reasoning_effort: None,
                plan_guard: PlanGuard::inactive(),
            },
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                let debug_store =
                    SessionCollaborationStore::new(&recovered_runtime.storage_root, &session_id)
                        .expect("诊断 Store 应创建");
                debug_store
                    .bind_runtime_session(&reopened)
                    .expect("诊断 Store 应绑定");
                let debug_snapshot = debug_store
                    .load_transition_snapshot()
                    .expect("诊断 Store 快照应读取")
                    .expect("诊断 Store 快照应存在");
                let debug_records = debug_snapshot.unstarted_turn_terminations.clone();
                let mut details = Vec::new();
                for debug_record in &debug_records {
                    let request_result = super::unstarted_turn_termination_request_from_record(
                        &reopened,
                        &debug_snapshot.commit.checkpoint,
                        debug_record,
                    );
                    let result = match request_result {
                        Ok(request) => reopened
                            .record_unstarted_turn_termination(request)
                            .map(|outcome| format!("{outcome:?}"))
                            .map_err(|error| format!("{error:?}")),
                        Err(error) => Err(format!("{error:?}")),
                    };
                    details.push(format!("{} => {:?}", debug_record.turn_id.as_str(), result));
                }
                panic!("冷恢复应逐条对账 pending 取消证据: {error:?}; {details:?}");
            }
        };
        let pending = recovered
            .store
            .load_transition_snapshot()
            .expect("冷恢复后的 Store 快照应读取")
            .expect("冷恢复后的 Store 快照应存在");
        assert!(pending.unstarted_turn_terminations.is_empty());
        let state = reopened
            .snapshot()
            .expect("冷恢复后的 Journal 快照应读取")
            .state;
        for record in initial.unstarted_turn_terminations {
            let resource_agent = ResourceAgentId::new(record.agent_id.as_str().to_owned()).unwrap();
            let resource_turn = ResourceTurnId::new(record.turn_id.as_str().to_owned()).unwrap();
            assert_eq!(
                state
                    .turns
                    .get(&resource_turn)
                    .map(|turn| turn.status.clone()),
                Some(TurnStatus::Cancelled)
            );
            assert_eq!(
                state
                    .sub_agents
                    .get(&resource_agent)
                    .map(|agent| agent.status.clone()),
                Some(SubAgentStatus::Interrupted)
            );
        }
        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("冷恢复 Session 应关闭");
    }

    /// 单个 Collaboration 批次中的多个等待容量取消证据必须逐条提取并对账。
    #[tokio::test]
    async fn waiting_capacity_batch_extracts_and_reconciles_multiple_records() {
        let (_storage, _project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let first = &initial.unstarted_turn_terminations[0];
        let second = &initial.unstarted_turn_terminations[1];
        let first_sequence = initial.commit.batch.expected_sequence + 1;
        let mut events =
            waiting_capacity_event_pair(&initial.commit.checkpoint, first, first_sequence, None);
        events.extend(waiting_capacity_event_pair(
            &initial.commit.checkpoint,
            second,
            first_sequence + 2,
            None,
        ));
        let commit = commit_with_events(&initial.commit, events);
        let records = super::unstarted_turn_termination_records(&collaboration.store, &commit)
            .expect("同一批次的多个等待容量证据应全部提取");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].agent_id, first.agent_id);
        assert_eq!(records[1].agent_id, second.agent_id);
        assert_eq!(records[0].turn_id, first.turn_id);
        assert_eq!(records[1].turn_id, second.turn_id);

        super::reconcile_unstarted_turn_termination_records(
            &session,
            &collaboration.store,
            &commit.checkpoint,
            &records,
        )
        .expect("多个等待容量证据应逐条写入 Runtime Journal 并确认");
        assert!(
            collaboration
                .store
                .load_transition_snapshot()
                .expect("多记录对账后的 Store 快照应读取")
                .expect("多记录对账后的 Store 快照应存在")
                .unstarted_turn_terminations
                .is_empty()
        );
        let state = session
            .snapshot()
            .expect("多记录对账后的 Journal 快照应读取")
            .state;
        for record in records {
            let resource_agent = ResourceAgentId::new(record.agent_id.as_str().to_owned()).unwrap();
            let resource_turn = ResourceTurnId::new(record.turn_id.as_str().to_owned()).unwrap();
            assert_eq!(
                state
                    .turns
                    .get(&resource_turn)
                    .map(|turn| turn.status.clone()),
                Some(TurnStatus::Cancelled)
            );
            assert_eq!(
                state
                    .sub_agents
                    .get(&resource_agent)
                    .map(|agent| agent.status.clone()),
                Some(SubAgentStatus::Interrupted)
            );
        }
        runtime
            .close_session(session.session_id().as_str())
            .await
            .expect("多记录测试 Session 应关闭");
    }

    /// WaitingCapacity 取消批次的来源、父链、路径和事件字段均必须按证据严格拒绝篡改。
    #[tokio::test]
    async fn waiting_capacity_batch_rejects_tampered_identity_and_running_or_root() {
        let (_storage, _project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let record = &initial.unstarted_turn_terminations[0];
        let first_sequence = initial.commit.batch.expected_sequence + 1;
        let valid_events =
            waiting_capacity_event_pair(&initial.commit.checkpoint, record, first_sequence, None);
        let valid = commit_with_events(&initial.commit, valid_events);
        assert_eq!(
            super::unstarted_turn_termination_records(&collaboration.store, &valid)
                .expect("基准等待容量批次应合法")
                .len(),
            1
        );

        let mut cases = Vec::new();
        let mut source = valid.clone();
        for event in &mut source.batch.events {
            event.source_agent_id =
                keencode_agent::AgentId::new("forged-source").expect("篡改来源 Agent 标识应有效");
        }
        cases.push(("source_agent_id", source));

        let mut parent = valid.clone();
        for event in &mut parent.batch.events {
            event.parent_agent_id = None;
        }
        cases.push(("parent_agent_id", parent));

        let mut path = valid.clone();
        for event in &mut path.batch.events {
            event.agent_path = AgentPath::root()
                .child("forged_path")
                .expect("篡改路径应有效");
        }
        cases.push(("agent_path", path));

        let mut session_identity = valid.clone();
        for event in &mut session_identity.batch.events {
            event.session_id =
                keencode_agent::SessionId::new("forged-session").expect("篡改 Session 标识应有效");
        }
        cases.push(("session_id", session_identity));

        let mut event_root = valid.clone();
        event_root.batch.events[0].root_turn_id =
            Some(keencode_agent::TurnId::new("forged-root-turn").expect("篡改根 Turn 标识应有效"));
        cases.push(("interrupted_event_root_turn_id", event_root));

        let mut event_turn = valid.clone();
        event_turn.batch.events[1].turn_id = Some(
            keencode_agent::TurnId::new("forged-status-turn").expect("篡改状态 Turn 标识应有效"),
        );
        cases.push(("status_event_turn_id", event_turn));

        let mut missing_interruption = valid.clone();
        missing_interruption.batch.events.remove(0);
        cases.push(("missing_interruption_event", missing_interruption));

        let mut running = valid.clone();
        let CollaborationEventKind::AgentStatusChanged { previous, .. } =
            &mut running.batch.events[1].kind
        else {
            panic!("基准第二事件应为状态变化");
        };
        *previous = CollaborationAgentStatus::Running {
            turn_id: record.turn_id.clone(),
        };
        cases.push(("running_to_interrupted", running));

        let mut root = valid.clone();
        for event in &mut root.batch.events {
            event.agent_id = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");
            event.parent_agent_id = None;
            event.agent_path = AgentPath::root();
        }
        cases.push(("root_agent", root));

        for (name, malicious) in cases {
            assert!(
                super::unstarted_turn_termination_records(&collaboration.store, &malicious)
                    .is_err(),
                "恶意 WaitingCapacity 批次 {name} 必须拒绝"
            );
        }
        runtime
            .close_session(session.session_id().as_str())
            .await
            .expect("恶意批次测试 Session 应关闭");
    }

    /// Store 已确认取消但 Journal 尚未写入时，冷恢复必须补齐 Turn 并清理 pending 证据。
    #[tokio::test]
    async fn waiting_capacity_cancel_pending_record_survives_cold_recovery() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "waiting-capacity-cold-recovery")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        persist_completed_root_turn(
            &session,
            "turn-waiting-capacity-cold-root",
            "等待容量冷恢复根 Turn",
            &root_turn_summary("等待容量冷恢复根 Turn", None, false),
        )
        .await;

        let limiter = Arc::new(CollaborationGlobalTurnLimiter::new(1).expect("测试容量应有效"));
        let occupier = occupy_test_global_turn(
            &runtime,
            project.path(),
            Arc::clone(&limiter),
            "waiting-capacity-cold-recovery",
        );
        let collaboration = install_test_collaboration_runtime_with_shared_limiter(
            &runtime,
            &session,
            project.path(),
            limiter,
            1,
        );
        let root_turn = collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                keencode_agent::TurnId::new("turn-waiting-capacity-cold-root")
                    .expect("根 Turn 标识应有效"),
                "等待容量冷恢复根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应入队");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &keencode_agent::ToolCallId::new("spawn-waiting-capacity-cold")
                    .expect("工具调用标识应有效"),
                test_spawn_request("waiting_capacity_cold", project.path()),
            )
            .expect("等待容量子 Agent 应创建");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 状态应读取"),
            CollaborationAgentStatus::WaitingCapacity { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));

        assert_eq!(
            collaboration
                .coordinator
                .cancel_turn(&child.agent.agent_id, &child.initial_turn_id)
                .expect("等待容量取消应提交"),
            keencode_agent::TurnCancellationDisposition::Requested
        );
        let pending = collaboration
            .store
            .load_transition_snapshot()
            .expect("pending 快照应读取")
            .expect("Store 提交后应存在快照");
        assert_eq!(pending.unstarted_turn_terminations.len(), 1);
        assert_eq!(
            pending.unstarted_turn_terminations[0].turn_id,
            child.initial_turn_id
        );
        assert!(
            !session
                .snapshot()
                .expect("Journal 快照应读取")
                .state
                .turns
                .keys()
                .any(|turn_id| turn_id.as_str() == child.initial_turn_id.as_str())
        );
        occupier.finish();

        // 根 Turn 的 Journal 已经完成，退出前同步提交相同的 Collaboration 终态；否则
        // 冷恢复会正确拒绝“Journal 已完成、Coordinator 仍 Running”的不一致快照。
        collaboration
            .coordinator
            .complete_turn(
                &collaboration.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    // 不能伪造缺失摘要，否则会遮蔽根最终结果的冷恢复一致性校验。
                    final_message: Some("完成".to_owned()),
                },
            )
            .expect("根 Turn 协作终态应提交");

        // 模拟 Store 已提交、Journal 对账前进程退出；释放旧 Session lease 后执行真正冷恢复。
        runtime
            .collaboration_sessions
            .lock()
            .expect("测试 Collaboration 表应可写")
            .remove(&session_id);
        drop(collaboration);
        drop(session);
        runtime
            .runtime_manager
            .close(session_id.clone())
            .expect("旧 Runtime Session 应关闭");
        drop(runtime);

        let recovered_runtime =
            Arc::new(AgentRuntime::new(storage.path()).expect("冷恢复 Runtime 应创建"));
        let reopened = recovered_runtime
            .open_or_create_session(project.path(), Some(&session_id), "unused")
            .expect("冷恢复 Session 应打开");
        let recovered = recovered_runtime
            .ensure_collaboration_runtime(
                &reopened,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("pending 取消证据应可冷恢复");
        let child_resource_id = ResourceAgentId::new(child.agent.agent_id.as_str().to_owned())
            .expect("子 Agent 资源标识应有效");
        let child_resource_turn = ResourceTurnId::new(child.initial_turn_id.as_str().to_owned())
            .expect("子 Turn 资源标识应有效");
        let snapshot = reopened.snapshot().expect("冷恢复 Snapshot 应读取");
        assert_eq!(
            snapshot
                .state
                .turns
                .get(&child_resource_turn)
                .map(|turn| turn.status.clone()),
            Some(TurnStatus::Cancelled)
        );
        assert_eq!(
            snapshot
                .state
                .sub_agents
                .get(&child_resource_id)
                .map(|agent| agent.status.clone()),
            Some(SubAgentStatus::Interrupted)
        );
        assert!(
            recovered
                .store
                .load_transition_snapshot()
                .expect("冷恢复 pending 快照应读取")
                .expect("冷恢复 Store 快照应存在")
                .unstarted_turn_terminations
                .is_empty(),
            "Journal 对账成功后必须确认并清理 pending 证据"
        );
        assert!(matches!(
            recovered
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("冷恢复子 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted { ref turn_id }
                if turn_id == &child.initial_turn_id
        ));
        recovered_runtime
            .close_session(&session_id)
            .await
            .expect("冷恢复 Session 应关闭");
    }

    /// 后台任务列表只投影仍运行的单层子 Agent，并按 Session/Turn 稳定取消。
    #[tokio::test]
    async fn background_agent_tasks_project_and_cancel_by_exact_turn() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "background-agent-operation")
            .expect("测试 Session 应创建");
        persist_completed_root_turn(
            &session,
            "turn-background-root",
            "后台任务根 Turn",
            "后台任务根 Turn",
        )
        .await;
        // 本用例需要同时覆盖一个运行中子 Agent 和一个等待容量的子 Agent；
        // 后台并发上限只计算子 Agent，不包含根 Turn，因此显式限制为 1。
        let collaboration =
            install_test_collaboration_runtime_with_limit(&runtime, &session, project.path(), 1);
        let root_turn = collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                keencode_agent::TurnId::new("turn-background-root").expect("根 Turn 标识应有效"),
                "后台任务根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let first_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &keencode_agent::ToolCallId::new("spawn-background-first")
                    .expect("工具调用标识应有效"),
                test_spawn_request("background_first", project.path()),
            )
            .expect("首个后台子 Agent 应创建");
        let second_child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &keencode_agent::ToolCallId::new("spawn-background-second")
                    .expect("工具调用标识应有效"),
                test_spawn_request("background_second", project.path()),
            )
            .expect("第二个后台子 Agent 应创建");
        {
            let mut state = collaboration
                .execution
                .state
                .lock()
                .expect("执行状态锁应可用");
            state.running_turns.insert(
                root_turn.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: collaboration.root_agent_id.clone(),
                    agent_depth: super::AgentDepth::ROOT,
                    summary: "根任务不应投影".to_owned(),
                    started_at_unix_ms: 1,
                    started: Instant::now(),
                    cancellation: TurnCancellation::new(),
                    terminal_outcome: None,
                },
            );
            state.running_turns.insert(
                first_child.initial_turn_id.clone(),
                super::ManagedRuntimeTurn {
                    agent_id: first_child.agent.agent_id.clone(),
                    agent_depth: super::AgentDepth::CHILD,
                    summary: "首个子任务".to_owned(),
                    started_at_unix_ms: 3,
                    started: Instant::now(),
                    cancellation: TurnCancellation::new(),
                    terminal_outcome: None,
                },
            );
        }

        let tasks = runtime
            .background_tasks_list(session.session_id().as_str())
            .expect("后台 Agent 列表应成功");
        assert_eq!(tasks.len(), 2);
        let running_task = tasks
            .iter()
            .find(|task| task.task_id == first_child.initial_turn_id.as_str())
            .expect("运行中子 Agent 应进入后台列表");
        let waiting_task = tasks
            .iter()
            .find(|task| task.task_id == second_child.initial_turn_id.as_str())
            .expect("等待容量的子 Agent 应进入后台列表");
        assert_eq!(running_task.kind, BackgroundTaskKind::Agent);
        assert_eq!(waiting_task.kind, BackgroundTaskKind::Agent);
        assert_eq!(
            waiting_task.child_thread_id,
            Some(second_child.agent.agent_id.as_str().to_owned())
        );
        assert_eq!(waiting_task.summary, "执行 background_second 测试任务");
        assert!(waiting_task.pid.is_none());
        let encoded = serde_json::to_value(waiting_task).expect("后台任务 DTO 应可序列化");
        assert_eq!(
            encoded["childThreadId"],
            second_child.agent.agent_id.as_str()
        );
        assert_eq!(
            encoded["durationMs"].as_u64().expect("持续时间应为数字"),
            waiting_task.duration_ms
        );

        runtime
            .background_task_cancel_outcome(
                session.session_id().as_str(),
                second_child.initial_turn_id.as_str(),
            )
            .expect("等待容量的指定子 Agent 应按精确 Turn 取消");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&second_child.agent.agent_id)
                .expect("子 Agent 状态应读取"),
            CollaborationAgentStatus::Interrupted { ref turn_id }
                if turn_id == &second_child.initial_turn_id
        ));
        runtime
            .background_task_cancel_outcome(
                session.session_id().as_str(),
                first_child.initial_turn_id.as_str(),
            )
            .expect("剩余运行中的子 Agent 应按精确 Turn 取消");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&first_child.agent.agent_id)
                .expect("首个子 Agent 状态应读取"),
            CollaborationAgentStatus::Cancelling { ref turn_id }
                if turn_id == &first_child.initial_turn_id
        ));
    }

    /// 同一个 Running 子 Agent 的并发取消必须由 Coordinator 原子地区分首次请求和重复请求。
    #[test]
    fn background_task_cancel_outcome_concurrent_running_child_is_idempotent() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "cancel-outcome-concurrent")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let root_turn_id = "turn-cancel-outcome-concurrent-root";
        let root_turn = AgentTurnId::new(root_turn_id).expect("根 Turn 标识应有效");
        let collaboration =
            install_test_collaboration_runtime_with_limit(&runtime, &session, project.path(), 3);
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn,
                "并发取消测试根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &AgentTurnId::new(root_turn_id).expect("根 Turn 标识应有效"),
                &ToolCallId::new("spawn-cancel-outcome-concurrent").expect("工具调用标识应有效"),
                test_spawn_request("cancel_outcome_concurrent", project.path()),
            )
            .expect("Running 子 Agent 应创建");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 状态应读取"),
            CollaborationAgentStatus::Running { .. }
        ));

        let task_id = child.initial_turn_id.as_str().to_owned();
        let first_runtime = runtime.clone();
        let second_runtime = runtime.clone();
        let first_session_id = session_id.clone();
        let second_session_id = session_id.clone();
        let first_task_id = task_id.clone();
        let second_task_id = task_id.clone();
        let first = std::thread::spawn(move || {
            first_runtime.background_task_cancel_outcome(&first_session_id, &first_task_id)
        });
        let second = std::thread::spawn(move || {
            second_runtime.background_task_cancel_outcome(&second_session_id, &second_task_id)
        });
        let outcomes = [
            first.join().expect("第一次取消线程不应 panic"),
            second.join().expect("第二次取消线程不应 panic"),
        ];
        assert!(outcomes.iter().all(|outcome| outcome.is_ok()));
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        Ok(super::BackgroundTaskCancellationOutcome::Requested)
                    )
                })
                .count(),
            1,
            "并发取消只能有一个首次请求"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        Ok(super::BackgroundTaskCancellationOutcome::AlreadyRequested)
                    )
                })
                .count(),
            1,
            "并发取消的另一个请求必须报告重复请求"
        );
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("取消后的子 Agent 状态应读取"),
            CollaborationAgentStatus::Cancelling { .. }
        ));
    }

    /// 已经提交完成终态的子 Agent 不应被精确取消接口伪装成成功。
    #[test]
    fn background_task_cancel_outcome_completed_child_is_not_running() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "cancel-outcome-completed")
            .expect("测试 Session 应创建");
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let root_turn =
            AgentTurnId::new("turn-cancel-outcome-completed-root").expect("根 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                "完成后取消测试根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-cancel-outcome-completed").expect("工具调用标识应有效"),
                test_spawn_request("cancel_outcome_completed", project.path()),
            )
            .expect("子 Agent 应创建");
        collaboration
            .coordinator
            .complete_turn(
                &child.agent.agent_id,
                &child.initial_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: Some("子 Agent 已完成".to_owned()),
                },
            )
            .expect("子 Agent 完成终态应提交");

        assert_eq!(
            runtime
                .background_task_cancel_outcome(
                    session.session_id().as_str(),
                    child.initial_turn_id.as_str(),
                )
                .expect("终态子 Agent 的取消查询应成功返回结果"),
            super::BackgroundTaskCancellationOutcome::NotRunning
        );
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("终态子 Agent 状态应读取"),
            CollaborationAgentStatus::Completed { .. }
        ));
    }

    /// WaitingCapacity 首次取消应报告 Requested，完成对账后的重复调用只能报告非首次结果。
    #[tokio::test]
    async fn background_task_cancel_outcome_waiting_capacity_duplicate_is_not_requested() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "cancel-outcome-waiting")
            .expect("测试 Session 应创建");
        persist_completed_root_turn(
            &session,
            "turn-cancel-outcome-waiting-root",
            "等待容量取消测试根 Turn",
            "等待容量取消测试根 Turn",
        )
        .await;
        let limiter = Arc::new(CollaborationGlobalTurnLimiter::new(1).expect("测试容量应有效"));
        let occupier = occupy_test_global_turn(
            &runtime,
            project.path(),
            Arc::clone(&limiter),
            "cancel-outcome-waiting",
        );
        let collaboration = install_test_collaboration_runtime_with_shared_limiter(
            &runtime,
            &session,
            project.path(),
            limiter,
            1,
        );
        let root_turn =
            AgentTurnId::new("turn-cancel-outcome-waiting-root").expect("根 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                "等待容量取消测试根 Turn",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("spawn-cancel-outcome-waiting").expect("工具调用标识应有效"),
                test_spawn_request("cancel_outcome_waiting", project.path()),
            )
            .expect("WaitingCapacity 子 Agent 应创建");
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("等待容量子 Agent 状态应读取"),
            CollaborationAgentStatus::WaitingCapacity { .. }
        ));

        let first = runtime
            .background_task_cancel_outcome(
                session.session_id().as_str(),
                child.initial_turn_id.as_str(),
            )
            .expect("首次 WaitingCapacity 取消应成功");
        assert_eq!(first, super::BackgroundTaskCancellationOutcome::Requested);
        let duplicate = runtime
            .background_task_cancel_outcome(
                session.session_id().as_str(),
                child.initial_turn_id.as_str(),
            )
            .expect("重复 WaitingCapacity 取消应返回明确结果");
        assert_ne!(
            duplicate,
            super::BackgroundTaskCancellationOutcome::Requested,
            "重复取消不得再次报告首次请求"
        );
        assert_eq!(
            duplicate,
            super::BackgroundTaskCancellationOutcome::NotRunning
        );
        occupier.finish();
    }

    /// 仅剩 Store pending 的 WaitingCapacity 取消证据是此前已发出的请求，不应再次报告 Requested。
    #[tokio::test]
    async fn background_task_cancel_outcome_pending_waiting_capacity_is_already_requested() {
        let (_storage, _project, runtime, session, collaboration, initial) =
            two_waiting_capacity_fixture().await;
        let record = initial.unstarted_turn_terminations[0].clone();
        let outcome = runtime
            .background_task_cancel_outcome(session.session_id().as_str(), record.turn_id.as_str())
            .expect("遗留 WaitingCapacity 取消证据应可对账");
        assert_eq!(
            outcome,
            super::BackgroundTaskCancellationOutcome::AlreadyRequested
        );
        assert!(
            collaboration
                .store
                .load_transition_snapshot()
                .expect("pending 对账后的 Store 快照应读取")
                .expect("pending 对账后的 Store 快照应存在")
                .unstarted_turn_terminations
                .iter()
                .all(|pending| pending.turn_id != record.turn_id)
        );
        runtime
            .close_session(session.session_id().as_str())
            .await
            .expect("测试 Session 应关闭");
    }

    /// 冷恢复必须按 Transcript 的 reasoning、工具、reasoning、正文顺序投影，且工具生命周期只能出现一次。
    #[tokio::test]
    async fn deferred_close_and_first_send_linearize_without_losing_journal() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server("并发清理后的首次回复");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "deferred-close-first-send")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        // 模拟 deferred UI 在 cleanup 前释放的短暂 Session view；真实 Journal
        // 仍由 RuntimeManager 持有，旧 view 不应阻止关闭后的冷恢复打开。
        drop(session);
        runtime.promote_deferred_session(&session_id).unwrap();
        // 发送/提升先取得 admission 时，旧 deferred cleanup 必须放弃关闭。
        assert!(!runtime.begin_deferred_session_close(&session_id).unwrap());
        let start_result = runtime
            .start_root_turn(
                &session_id,
                "deferred-first-turn",
                "首次发送必须保留",
                RootTurnOptions::default(),
            )
            .await;
        assert!(
            matches!(
                start_result,
                Ok(RootTurnStartOutcome::Started | RootTurnStartOutcome::Deduplicated)
            ),
            "首次发送必须在 close 竞态中恢复同一 Session：{start_result:?}"
        );

        let _ = server.join().unwrap();
        let reopened = runtime
            .open_or_create_session(project.path(), Some(&session_id), "deferred-close-reopen")
            .unwrap();
        let has_prompt = reopened
            .snapshot()
            .unwrap()
            .state
            .raw_transcript_messages()
            .into_iter()
            .any(|message| {
                message.role == keencode_resources::MessageRole::User
                    && message.content.iter().any(|part| {
                        matches!(part, keencode_resources::MessagePart::Text { text } if text == "首次发送必须保留")
                    })
            });
        assert!(
            has_prompt,
            "promote/start 与 cleanup 竞态不能丢失 Journal 用户输入"
        );
        runtime.close_session(&session_id).await.unwrap();
    }

    /// cleanup 先取得 admission 后，后台根 Turn 必须在关闭事实完成前被拒绝。
    #[tokio::test(flavor = "multi_thread")]
    async fn deferred_cleanup_claim_rejects_root_start() {
        // 使用失效地址确保测试验证的是 lifecycle admission，而不是 Provider 请求。
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = runtime_with_responses_provider(
            storage.path(),
            "http://127.0.0.1:9/v1",
            &["test-model"],
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "deferred-cleanup-first")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);

        assert!(runtime.begin_deferred_session_close(&session_id).unwrap());
        let rejected = runtime
            .start_root_turn(
                &session_id,
                "cleanup-raced-turn",
                "不应启动",
                RootTurnOptions::default(),
            )
            .await;
        assert!(
            matches!(rejected, Err(AgentRuntimeError::SessionUnavailable)),
            "cleanup admission 后必须拒绝后台启动：{rejected:?}"
        );
        runtime.close_session(&session_id).await.unwrap();
    }

    /// mutation 先持有 Turn gate、root start 后取得 admission 时，mutation 必须放弃
    /// 关闭；释放 gate 后发送继续进入同一 Journal，不能先取消权限或删除 Session。
    #[tokio::test(flavor = "multi_thread")]
    async fn session_mutation_admission_rejects_inflight_root_start_without_closing_journal() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server("mutation 竞态后仍保留");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "mutation-start-race")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);

        let admission = runtime
            .begin_workspace_mutation(&session_id)
            .await
            .unwrap()
            .expect("空闲 Session 应取得 workspace mutation gate");
        let SessionWorkspaceMutationAdmission::Registered(mutation) = admission else {
            panic!("已登记 Session 应取得 Registered workspace admission");
        };
        let start_runtime = Arc::clone(&runtime);
        let start_session_id = session_id.clone();
        let start = tokio::spawn(async move {
            start_runtime
                .start_root_turn(
                    &start_session_id,
                    "mutation-raced-turn",
                    "mutation 竞态必须保留这条输入",
                    RootTurnOptions::default(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if runtime
                    .session_start_admissions
                    .lock()
                    .unwrap()
                    .contains_key(&session_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("root start 应在 mutation gate 外先取得 admission");

        assert!(mutation.close_and_hold().await.unwrap().is_none());
        let started = start.await.unwrap();
        assert!(
            matches!(
                started,
                Ok(RootTurnStartOutcome::Started | RootTurnStartOutcome::Deduplicated)
            ),
            "mutation 被拒绝后 root start 必须继续：{started:?}"
        );

        let _ = server.join().unwrap();
        let snapshot = runtime.session_snapshot(&session_id).unwrap();
        assert!(snapshot
            .state
            .raw_transcript_messages()
            .into_iter()
            .any(|message| {
                message.role == keencode_resources::MessageRole::User
                    && message.content.iter().any(|part| {
                        matches!(part, keencode_resources::MessagePart::Text { text } if text == "mutation 竞态必须保留这条输入")
                    })
            }));
        runtime.close_session(&session_id).await.unwrap();
    }

    /// 已登记 Session 关闭后，workspace guard 必须继续持有 Closing；否则读取侧会在
    /// Git/资源事务完成前重新登记 Manager Session，导致后续目录变更被判定为 busy。
    #[tokio::test(flavor = "multi_thread")]
    async fn registered_workspace_admission_keeps_session_closed_until_guard_drops() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = AgentRuntime::new_for_control_test(storage.path()).unwrap();
        let session = runtime
            .open_or_create_session(project.path(), None, "registered-workspace-admission")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);

        let admission = runtime
            .begin_workspace_mutation(&session_id)
            .await
            .unwrap()
            .expect("已登记 Session 应取得 workspace admission");
        let SessionWorkspaceMutationAdmission::Registered(mutation) = admission else {
            panic!("RuntimeManager 中仍登记的 Session 不应走 Closed admission");
        };
        let guard = mutation
            .close_and_hold()
            .await
            .unwrap()
            .expect("空闲已登记 Session 应完成关闭并保留 mutation guard");

        assert_eq!(
            runtime
                .deferred_session_lifecycle
                .lock()
                .unwrap()
                .get(&session_id),
            Some(&DeferredSessionLifecycle::Closing)
        );
        assert!(matches!(
            runtime.runtime_manager().get(session_id.clone()),
            Err(keencode_runtime::RuntimeError::SessionNotRegistered)
        ));
        assert!(matches!(
            runtime.open_or_create_session(
                project.path(),
                Some(&session_id),
                "registered-workspace-read-reopen",
            ),
            Err(AgentRuntimeError::SessionUnavailable)
        ));

        drop(guard);
        assert!(
            runtime
                .deferred_session_lifecycle
                .lock()
                .unwrap()
                .get(&session_id)
                .is_none()
        );
        let reopened = runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "registered-workspace-after-guard",
            )
            .unwrap();
        drop(reopened);
        runtime.close_session(&session_id).await.unwrap();
    }

    /// 冷 Session 的 workspace admission 也必须持有同一 Turn gate；Closing lifecycle
    /// 期间 root start 立即拒绝，guard 释放后才允许重新打开 Journal。
    #[tokio::test(flavor = "multi_thread")]
    async fn cold_workspace_admission_rejects_root_start_until_guard_drops() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = AgentRuntime::new_for_control_test(storage.path()).unwrap();
        let session = runtime
            .open_or_create_session(project.path(), None, "cold-workspace-admission")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        runtime.close_session(&session_id).await.unwrap();

        let admission = runtime
            .begin_workspace_mutation(&session_id)
            .await
            .unwrap()
            .expect("冷 Session 应取得 workspace admission");
        let SessionWorkspaceMutationAdmission::Closed(guard) = admission else {
            panic!("已关闭 Session 不应重新登记为 Runtime mutation");
        };
        assert_eq!(
            runtime
                .deferred_session_lifecycle
                .lock()
                .unwrap()
                .get(&session_id),
            Some(&DeferredSessionLifecycle::Closing)
        );
        assert!(matches!(
            runtime.open_or_create_session(
                project.path(),
                Some(&session_id),
                "workspace-read-reopen"
            ),
            Err(AgentRuntimeError::SessionUnavailable)
        ));

        assert!(
            matches!(
                runtime
                    .start_root_turn(
                        &session_id,
                        "cold-workspace-raced-turn",
                        "交接 gate 释放后才允许恢复的输入",
                        RootTurnOptions::default(),
                    )
                    .await,
                Err(AgentRuntimeError::SessionUnavailable)
            ),
            "Closing lifecycle 期间 root start 必须立即拒绝"
        );
        assert!(
            !runtime.begin_deferred_session_close(&session_id).unwrap(),
            "workspace admission 持有期间 deferred cleanup 不得认领同一 cold Session"
        );
        drop(guard);
        assert!(
            runtime
                .deferred_session_lifecycle
                .lock()
                .unwrap()
                .get(&session_id)
                .is_none()
        );
        let started = runtime
            .start_root_turn(
                &session_id,
                "cold-workspace-after-guard",
                "释放 gate 后才允许恢复的输入",
                RootTurnOptions::default(),
            )
            .await;
        assert!(
            matches!(started, Err(AgentRuntimeError::ProviderNotConfigured)),
            "释放 gate 后才应进入真实启动路径：{started:?}"
        );
        runtime.close_session(&session_id).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deferred_cleanup_claim_is_exclusive() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (base_url, server) = spawn_buffered_responses_server("cleanup 只能在显式关闭时收口");
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "deferred-claim")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);

        let started = runtime
            .start_root_turn(
                &session_id,
                "deferred-cleanup-after-start",
                "已经产生事实的输入",
                RootTurnOptions::default(),
            )
            .await;
        assert!(
            matches!(
                started,
                Ok(RootTurnStartOutcome::Started | RootTurnStartOutcome::Deduplicated)
            ),
            "真实 root Turn 应先完成 admission：{started:?}"
        );
        assert!(!runtime.begin_deferred_session_close(&session_id).unwrap());

        let _ = server.join().unwrap();
        let snapshot = runtime.session_snapshot(&session_id).unwrap();
        assert!(!snapshot.state.raw_transcript_messages().is_empty());
        runtime.close_session(&session_id).await.unwrap();
    }

    /// 活动子 Agent 占用协作容量时，idle release 必须保留整棵 Agent 树并且不发取消。
    #[tokio::test(flavor = "multi_thread")]
    async fn release_idle_session_keeps_active_collaboration_runtime() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "release-idle-active")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
        let root_turn = AgentTurnId::new("release-idle-active-root").expect("根 Turn 标识应有效");
        collaboration
            .coordinator
            .begin_root_turn_with_id(
                &collaboration.root_agent_id,
                root_turn.clone(),
                "保留活动协作树",
                PlanGuard::inactive(),
            )
            .expect("根 Turn 应启动");
        let child = collaboration
            .coordinator
            .spawn_agent(
                &collaboration.root_agent_id,
                &root_turn,
                &ToolCallId::new("release-idle-active-child").expect("spawn 标识应有效"),
                test_spawn_request("release_idle_active", project.path()),
            )
            .expect("子 Agent 应启动");

        assert!(
            !runtime
                .release_idle_session(&session_id)
                .await
                .expect("活动 Session 的 idle release 应返回")
        );
        assert!(runtime.runtime_manager().get(session_id.clone()).is_ok());
        assert!(matches!(
            collaboration
                .coordinator
                .agent_status(&child.agent.agent_id)
                .expect("子 Agent 状态应读取"),
            CollaborationAgentStatus::Running { .. }
        ));

        collaboration
            .coordinator
            .complete_turn(
                &child.agent.agent_id,
                &child.initial_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .expect("子 Agent 应完成");
        collaboration
            .coordinator
            .complete_turn(
                &collaboration.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .expect("根 Turn 应完成");
        drop(session);
        runtime.close_session(&session_id).await.unwrap();
    }

    /// 空闲 Session 释放后必须撤销进程内 lease，并能从同一 Journal 冷恢复。
    #[tokio::test(flavor = "multi_thread")]
    async fn release_idle_session_releases_and_cold_reopens_session() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = Arc::new(AgentRuntime::new(storage.path()).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "release-idle-cold-reopen")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        persist_completed_root_turn(
            &session,
            "release-idle-persisted-turn",
            "释放后仍可恢复的输入",
            "释放后仍可恢复的输入",
        )
        .await;
        drop(session);

        assert!(
            runtime
                .release_idle_session(&session_id)
                .await
                .expect("空闲 Session 应释放成功")
        );
        assert!(matches!(
            runtime.runtime_manager().get(session_id.clone()),
            Err(keencode_runtime::RuntimeError::SessionNotRegistered)
        ));

        let reopened = runtime
            .open_or_create_session(
                project.path(),
                Some(&session_id),
                "release-idle-cold-reopen-after-release",
            )
            .expect("释放后的 Session 应可冷恢复");
        assert!(reopened
            .snapshot()
            .expect("冷恢复快照应读取")
            .state
            .raw_transcript_messages()
            .into_iter()
            .any(|message| {
                message.content.iter().any(|part| {
                    matches!(part, keencode_resources::MessagePart::Text { text } if text == "释放后仍可恢复的输入")
                })
            }));
        drop(reopened);
        runtime.close_session(&session_id).await.unwrap();
    }
}
