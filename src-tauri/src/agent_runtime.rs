//! 自研 Agent Runtime 的桌面生产装配根与唯一 ACP 投递泵。

mod file_changes;
mod tool_projection;

use crate::{
    analytics::AnalyticsRecorder, app_settings::DEFAULT_BACKGROUND_AGENT_LIMIT,
    client_request::ClientRequestDisplayGate, elicitation::ElicitationCoordinator, providers,
    storage,
};
use anyhow::{Context, anyhow, bail};
use chrono::{SecondsFormat, TimeZone, Utc};
use keencode_acp::{
    AcpClientRequestFrame, AgentLifecycleStatus, BackgroundTaskInfo, BackgroundTaskKind,
    BackgroundTaskTerminalStatus, CompactionFailureKind, KeenCodeEvent, KeenCodeEventEnvelope,
    KeenCodeEventEnvelopeParams, MAX_REPLAY_EVENTS, ReplaySessionResponse, SessionSequence,
    SessionUpdateDeliveryEnvelope, SystemNotificationLevel, TurnFailureKind,
};
use keencode_agent::{
    AgentCapabilities, AgentCommitSinkError, AgentDepth, AgentDynamicInputAcknowledgement,
    AgentDynamicInputBatch, AgentDynamicInputBoundary, AgentDynamicInputError,
    AgentDynamicInputKind, AgentDynamicInputReceipt, AgentDynamicInputSource, AgentExecutionPort,
    AgentId as RunnerAgentId, AgentProfile, AgentRunError, AgentRunner, AgentStreamEvent,
    AgentStreamEventKind, AgentTemplateSnapshot, AgentTreeQuiesceResult, AgentTurnCause,
    AgentTurnLaunch, AgentTurnOutcome, AgentTurnSignal, AgentTurnStartResult, CloseAgentTree,
    CollaborationAgentStatus, CollaborationAgentSummary, CollaborationAppendResult,
    CollaborationCoordinator, CollaborationEvent, CollaborationEventKind, CollaborationLimits,
    CollaborationPortError, CollaborationStore, CollaborationTransitionCommit,
    ContextCompactionFailureKind, ContextManager, GoalController, GoalStatus, GoalUsageDelta,
    HookRuntime, MailboxMessage as RunnerMailboxMessage, MailboxMessageKind, ModelRoundUsage,
    PlanGuard, PlanGuardState, QuiesceAgentTree, RecoveredAgent, RecoveredAgentCheckpoint,
    RecoveredCoordinator, RootAgentRequest, RunLimits, RuntimeStateError,
    SessionId as AgentSessionId, StructuredOutputMode, TerminalReason, ToolCallId, ToolRegistry,
    TurnCancellation, TurnCancellationDisposition, TurnId as AgentTurnId, TurnRequest,
    UuidCollaborationIdGenerator, root_turn_prompt_digest,
};
use keencode_model::{
    ContentBlock, ImageSource, Message, MessageRole, ModelFuture, ModelProvider, ModelRequest,
    ModelStream, ModelStreamEvent, ProviderCapabilities, ProviderProtocol, ReasoningConfig,
    ReasoningEffort, StructuredOutputConfig, ToolChoice,
};
use keencode_provider::{
    ProviderRegistry, ProviderRegistrySnapshot, REQUEST_METADATA_AGENT_ID,
    REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID, REQUEST_METADATA_TURN_ID,
    ResolvedProvider,
};
use keencode_resources::{
    AgentId as ResourceAgentId, COMPACTION_SUMMARY_PREFIX,
    DynamicInputKind as ResourceDynamicInputKind, MailboxMessage as ResourceMailboxMessage,
    MailboxMessageId as ResourceMailboxMessageId, MailboxState, MessagePart as ResourceMessagePart,
    MessageRole as ResourceMessageRole, ProviderProtocolSnapshot, ProviderSnapshot,
    ReasoningEffortSnapshot, SessionEvent, SessionEventRecord, SessionMessage, SessionState,
    SubAgentState, SubAgentStatus, TodoStatus, ToolCompletionStatus, TranscriptRecord,
    TranscriptSegment, TurnId as ResourceTurnId, TurnStatus, TurnStopReason,
};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, PersistentAgentState, RuntimeConfig,
    RuntimeControlEvent, RuntimeError, RuntimeEventPayload, RuntimeEventReceiveError,
    RuntimeEventSubscription, RuntimeManager, RuntimeModelRoundUsageSink, RuntimeSession,
    RuntimeSnapshot, RuntimeTurnRequest, StoredSessionMetadata, TurnCancellationOutcome,
    UnstartedTurnTermination, UnstartedTurnTerminationRequest,
};
use keencode_tools::{
    AskUserTool, BackgroundTaskCompletion, BackgroundTaskManager, BackgroundTaskStatus,
    CompletedTurnContext, GitWorktreeLeaseManager, ResolvedSpawnAgentTemplate,
    SpawnAgentContextSource, SpawnAgentTemplateContext, SpawnAgentTemplateResolver,
    ToolEnvironment, WebServiceConfig, register_collaboration_tools,
    register_collaboration_tools_with_template_resolver, register_local_tools_with_background,
    register_state_tools, register_web_tools, retain_child_agent_tool_snapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

/// 桌面端接收全部 ACP 投递的唯一 Tauri 事件名称。
pub const ACP_DELIVERY_EVENT: &str = "acp://delivery";
/// 每个 Session 投递泵允许排队的最大命令数量。
const DELIVERY_QUEUE_CAPACITY: usize = 256;
/// 投递队列满时等待一个可用槽位的生产时限。
const DELIVERY_QUEUE_RESERVE_TIMEOUT: Duration = Duration::from_secs(2);
/// 命令入队后等待同步投递边界确认的最大时限。
const DELIVERY_ACK_TIMEOUT: Duration = Duration::from_secs(30);
/// 等待投递泵处理关闭命令的最大时限。
const DELIVERY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 独立标题请求的硬超时，避免单飞锁被异常端点永久占用。
const TITLE_GENERATION_TIMEOUT_SECS: u64 = 60;
/// 自动标题允许的最大 Unicode 标量数量；与前端展示上限保持一致。
const GENERATED_TITLE_MAX_CHARS: usize = 36;
/// Collaboration v2 原子提交文件使用的唯一 Schema。
const COLLABORATION_TRANSITION_SCHEMA: &str = "keencode/session/collaboration-transition";
/// Collaboration v2 局部 Agent checkpoint 文件使用的唯一 Schema。
const COLLABORATION_AGENT_SCHEMA: &str = "keencode/session/collaboration-agent-checkpoint";
/// Collaboration v2 原子提交文件当前唯一版本。
const COLLABORATION_TRANSITION_VERSION: u32 = 1;
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
/// 单条扩展诊断进入 ACP 系统通知时允许保留的最大 UTF-8 字节数。
const MAX_EXTENSION_DIAGNOSTIC_BYTES: usize = 4 * 1024;
/// 协调器提交文件允许累积保留的等待容量取消证据数量。
const MAX_UNSTARTED_TURN_TERMINATION_RECORDS: usize = 4_096;

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

/// 自研 Runtime 生产装配、Provider 热加载或桌面投递失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentRuntimeError {
    /// 无法创建本地 Runtime 或读取当前配置。
    InitializationFailed,
    /// Session 标识不满足当前资源层约束。
    InvalidSession,
    /// Session 尚未建立桌面投递世代。
    SessionDeliveryMissing,
    /// Runtime 已永久关闭。
    RuntimeClosed,
    /// Session 投递泵已经关闭。
    DeliveryClosed,
    /// 当前投递世代先前发生发送错误，禁止继续伪造连续序号。
    DeliveryPoisoned,
    /// 投递队列未能在有界时限内取得槽位，命令没有入队。
    DeliveryQueueTimeout,
    /// 命令已经入队但回执超时或通道提前关闭，实际投递结果未知。
    DeliveryOutcomeUnknown,
    /// 关闭命令未能在有界时限内确认，旧投递世代仍可能在运行。
    DeliveryShutdownUnknown,
    /// Session 投递序号已经耗尽。
    DeliverySequenceExhausted,
    /// Tauri 无法向桌面窗口发送当前投递。
    DesktopEmitFailed,
    /// Provider 配置不能原子替换到当前注册表。
    ProviderReloadFailed,
    /// Session 绑定的连接配置已改变，必须由用户显式重新选择模型。
    ProviderConfigurationChanged,
    /// 当前配置没有同时选择可解析的 Provider 与模型。
    ProviderNotConfigured,
    /// 请求打开的 Session 不存在或其权威日志已经损坏。
    SessionUnavailable,
    /// 请求项目与 Session 创建时持久绑定的项目根不一致。
    SessionProjectMismatch,
    /// Session Runtime 控制面操作失败。
    RuntimeOperationFailed,
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

impl fmt::Display for AgentRuntimeError {
    /// 输出不包含事件正文、工具输入或 Provider 凭据的稳定说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitializationFailed => formatter.write_str("Agent Runtime 初始化失败"),
            Self::InvalidSession => formatter.write_str("Session 标识无效"),
            Self::SessionDeliveryMissing => formatter.write_str("Session 投递尚未建立"),
            Self::RuntimeClosed => formatter.write_str("Agent Runtime 已关闭"),
            Self::DeliveryClosed => formatter.write_str("Session 投递泵已关闭"),
            Self::DeliveryPoisoned => formatter.write_str("Session 投递世代已停止"),
            Self::DeliveryQueueTimeout => {
                formatter.write_str("Session 投递队列已满，未能在时限内入队")
            }
            Self::DeliveryOutcomeUnknown => {
                formatter.write_str("Session 投递结果未知，当前世代已冻结")
            }
            Self::DeliveryShutdownUnknown => {
                formatter.write_str("Session 投递泵关闭结果未知，禁止建立新世代")
            }
            Self::DeliverySequenceExhausted => formatter.write_str("Session 投递序号已耗尽"),
            Self::DesktopEmitFailed => formatter.write_str("桌面事件投递失败"),
            Self::ProviderReloadFailed => formatter.write_str("Provider 热加载失败"),
            Self::ProviderConfigurationChanged => {
                formatter.write_str("Session 模型连接配置已改变，请重新选择模型")
            }
            Self::ProviderNotConfigured => formatter.write_str("当前没有可用的默认模型"),
            Self::SessionUnavailable => formatter.write_str("Session 不存在或不可恢复"),
            Self::SessionProjectMismatch => formatter.write_str("Session 不属于当前项目"),
            Self::RuntimeOperationFailed => formatter.write_str("Session Runtime 操作失败"),
            Self::InstructionsUnavailable => formatter
                .write_str("无法加载全局或项目 AGENTS.md：请检查文件类型、UTF-8 编码和大小"),
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

/// 启动根 Turn 时由命令层显式传入、只在模型请求期装配的行为上下文。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RootTurnOptions {
    /// Memory、Plan 或 Ultra 等本轮动态开发者上下文；不会写入 Session Transcript。
    pub developer_context: Option<String>,
    /// 本轮开始前必须原子写入 Session 快照的 Plan 模式状态。
    pub plan_enabled: bool,
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
    /// 当前根 Session 标识。
    session_id: String,
    /// 当前 Session 创建时绑定的规范项目根。
    project_root: PathBuf,
    /// 当前 Turn 冻结的 Plan 只读守卫。
    plan_guard: PlanGuard,
}

/// 扩展候选在装配时产生、需要通过当前 Turn 告知客户端的安全诊断。
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
    /// 仅供 crate 内测试安全构造隔离上下文，不开放生产装配入口或字段。
    #[cfg(test)]
    pub(crate) fn for_extension_test(project_root: PathBuf, plan_guard: PlanGuard) -> Self {
        Self {
            session_id: "extension-chain-session".to_owned(),
            project_root,
            plan_guard,
        }
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
    /// 追加到 KeenCode 基础提示之后的系统说明。
    pub system_prompt: String,
    /// 可选的精确模型覆盖。
    pub model: Option<String>,
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
                name: template.name,
                system_prompt: template.system_prompt,
                max_turns: template.max_turns,
                allowed_write_dirs: template.allowed_write_dirs,
            },
            model: template.model,
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

/// `sessions/<session-id>/collaboration-v2.json` 中一次完整原子提交。
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
}

impl SessionCollaborationStore {
    /// 为指定 Session 创建尚不触碰磁盘的生产 Store。
    fn new(storage_root: &Path, session_id: &str) -> Result<Self, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session_directory = storage_root.join("sessions").join(session_id);
        Ok(Self {
            session_id: session_id.to_owned(),
            transition_path: session_directory.join("collaboration-v2.json"),
            agent_checkpoint_directory: session_directory.join("collaboration-v2-agents"),
            runtime_session: OnceLock::new(),
            commit_gate: Mutex::new(()),
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
    /// Memory、Plan 或 Ultra 等只在 Provider 请求期装配的消息；不得进入权威 Transcript。
    request_context: Vec<Message>,
    /// Runtime TurnStarted 使用的稳定用户输入摘要。
    summary: String,
    /// 供命令层同时观察“尚未形成起点即失败”的一次性完成通知。
    completion: oneshot::Sender<Result<(), ()>>,
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
}

/// 把 Runner 已完成模型 Round 的明确用量同步累计到项目级 Goal。
struct RuntimeGoalUsageSink {
    /// 当前唯一根 Runtime Session 标识。
    session_id: String,
    /// 项目级 Goal 的持久事务出口。
    persistent_state: Arc<PersistentAgentState>,
    /// Goal 首次变化后向同项目打开 Session 广播的装配根弱引用。
    owner: Weak<AgentRuntime>,
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
}

/// Session 级 V2 执行端：真正创建 Runner 任务并管理取消、静止与系统清理。
struct RuntimeAgentExecution {
    /// 与协调器原子提交共享的持久补偿账本，启动前必须完成其 Journal 对账。
    store: Arc<SessionCollaborationStore>,
    /// 回到桌面装配根以读取热替换 Provider、扩展、Web 和投递边界。
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
    /// 只接受不透明 lease 的 Session 独占 Git Worktree 管理器。
    worktrees: Arc<GitWorktreeLeaseManager>,
    /// 反向弱绑定避免 Coordinator 与执行端形成强引用环。
    coordinator: OnceLock<Weak<CollaborationCoordinator>>,
    /// 托管 Turn、准备请求和幂等记录的同步状态。
    state: Arc<Mutex<RuntimeAgentExecutionState>>,
    /// 退出或 Session 拆除开始后禁止新的 Runner 进入执行副作用边界。
    accepting_work: AtomicBool,
    /// 全树静止等待托管 Turn 数量归零的条件变量。
    idle: Arc<Condvar>,
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
    /// 幂等停止后台 Shell 完成事件泵，避免 Session 关闭后继续触碰桌面投递。
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
    fn new(
        owner: Weak<AgentRuntime>,
        session: RuntimeSession,
        project_root: PathBuf,
        persistent_state: Arc<PersistentAgentState>,
        background_tasks: Arc<BackgroundTaskManager>,
        worktrees: Arc<GitWorktreeLeaseManager>,
        store: Arc<SessionCollaborationStore>,
    ) -> Self {
        let session_id = session.session_id().as_str().to_owned();
        Self {
            store,
            owner,
            session,
            session_id,
            project_root,
            persistent_state,
            background_tasks,
            worktrees,
            coordinator: OnceLock::new(),
            state: Arc::new(Mutex::new(RuntimeAgentExecutionState::default())),
            accepting_work: AtomicBool::new(true),
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

    /// 将根请求发布到按 TurnId 去重的准备表，并返回命令层完成通知。
    fn prepare_root_turn(
        &self,
        turn_id: AgentTurnId,
        provider: ResolvedProvider,
        reasoning_effort: Option<ReasoningEffortSnapshot>,
        input_messages: Vec<Message>,
        request_context: Vec<Message>,
        summary: String,
    ) -> Result<oneshot::Receiver<Result<(), ()>>, AgentRuntimeError> {
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
            let _ = prepared.completion.send(Err(()));
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
        shutdown_background_tasks_blocking(Arc::clone(&self.background_tasks))
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
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
            let _ = prepared.completion.send(Err(()));
        }
        shutdown_background_tasks_blocking(Arc::clone(&self.background_tasks))
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
    }

    /// 为指定扩展候选取得一次性诊断通知发送权，避免每个子 Agent 重复提示。
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

    /// 诊断批次无法进入投递队列时释放发送权，允许后续根 Turn 重试。
    fn release_extension_diagnostics_claim(
        &self,
        generation: u64,
    ) -> Result<(), AgentRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if state.extension_diagnostics_generation == Some(generation) {
            state.extension_diagnostics_generation = None;
        }
        Ok(())
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
                .map_err(|_| AgentDynamicInputError::new("无法 claim Agent mailbox"))?
        } else {
            Vec::new()
        };
        let steers = self
            .coordinator
            .consume_user_steers(source_agent_id, turn_id)
            .map_err(|_| AgentDynamicInputError::new("无法 claim 用户 Steer"))?;
        // 必须位于 claim 之后，覆盖与本次 claim 并发提交的子 Turn 失败通知。
        if !mailbox.is_empty() {
            self.store
                .reconcile_pending_unstarted_turns()
                .map_err(|_| AgentDynamicInputError::new("无法对账未启动 Agent 终态"))?;
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
                .map_err(|_| AgentDynamicInputError::new("无法镜像 Agent mailbox"))?;
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
                    "\n\n[sequence={} source={} kind={kind}]\n{}",
                    message.sequence,
                    message.source_agent_id.as_str(),
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
            body.push_str("\n以下是用户在当前 Turn 中追加的引导，按顺序执行：");
            for steer in &steers {
                body.push_str(&format!(
                    "\n\n[sequence={}]\n{}",
                    steer.sequence, steer.content
                ));
            }
            messages.push(Message::text(MessageRole::User, body));
        }
        let mut receipts = Vec::with_capacity(2);
        if let Some(through_sequence) = mailbox_through_sequence {
            receipts.push(AgentDynamicInputReceipt::new(
                AgentDynamicInputKind::Mailbox,
                through_sequence,
            ));
        }
        if let Some(through_sequence) = steer_through_sequence {
            receipts.push(AgentDynamicInputReceipt::new(
                AgentDynamicInputKind::UserSteer,
                through_sequence,
            ));
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
        serde_json::from_value(value).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            claims.push(RecoveredDynamicInputClaim {
                agent_id: agent.definition.agent_id.clone(),
                turn_id,
                kind: DynamicInputMarkerKind::Mailbox,
                through_sequence,
                mailbox_message_ids,
                mailbox_messages,
            });
        }
        if let Some((turn_id, through_sequence)) = agent
            .steer_claim_turn_id
            .clone()
            .zip(agent.steer_claim_through_sequence)
        {
            claims.push(RecoveredDynamicInputClaim {
                agent_id: agent.definition.agent_id.clone(),
                turn_id,
                kind: DynamicInputMarkerKind::UserSteer,
                through_sequence,
                mailbox_message_ids: Vec::new(),
                mailbox_messages: Vec::new(),
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

/// 从权威 Transcript 中提取指定 Agent/Turn 最后一条 Assistant 消息的普通文本。
///
/// 根 Agent 的结果摘要没有单独的 Runtime Journal 字段，必须从同一 Turn 的持久
/// Transcript 重建。只接受身份完全匹配的 Assistant 消息，并保留“最后一条消息没有
/// 普通文本”这一语义；Artifact 文本必须通过同一 Runtime Session 物化，不能静默丢弃。
fn recovered_root_final_message(
    state: &SessionState,
    session: Option<&RuntimeSession>,
    agent_id: &ResourceAgentId,
    turn_id: &ResourceTurnId,
) -> Result<Option<String>, AgentRuntimeError> {
    let mut last_assistant = None;
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
            last_assistant = Some(message);
        }
    }
    let message = last_assistant.ok_or(AgentRuntimeError::RecoveryRequired)?;
    // 先定位最终响应再读取 Artifact，避免恢复一次根结果时重新物化全部历史模型轮次。
    let text = if let Some(session) = session {
        let materialized = session
            .materialize_message(message)
            .map_err(|_| AgentRuntimeError::RecoveryRequired)?;
        if materialized.role != MessageRole::Assistant {
            return Err(AgentRuntimeError::RecoveryRequired);
        }
        materialized
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let mut text_blocks = Vec::new();
        for part in &message.content {
            match part {
                ResourceMessagePart::Text { text } => text_blocks.push(text.as_str()),
                ResourceMessagePart::Artifact { .. } => {
                    return Err(AgentRuntimeError::RecoveryRequired);
                }
                ResourceMessagePart::Reasoning { .. }
                | ResourceMessagePart::Image { .. }
                | ResourceMessagePart::ToolCall { .. }
                | ResourceMessagePart::ToolResult { .. } => {}
            }
        }
        text_blocks.join("\n")
    };
    Ok((!text.is_empty()).then(|| bounded_collaboration_failure(&text)))
}

/// 将 Runtime Journal 中同一 Agent Turn 的已落盘终态转换为 Collaboration 恢复结果。
fn authoritative_recovered_turn_outcome(
    state: &SessionState,
    agent_id: &RunnerAgentId,
    turn_id: &AgentTurnId,
    session: Option<&RuntimeSession>,
) -> Result<Option<AgentTurnOutcome>, AgentRuntimeError> {
    let resource_agent_id = ResourceAgentId::new(agent_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let resource_turn_id = ResourceTurnId::new(turn_id.as_str().to_owned())
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let Some(turn) = state.turns.get(&resource_turn_id) else {
        return Ok(None);
    };
    if turn.turn_id != resource_turn_id || turn.source_agent_id != resource_agent_id {
        return Err(AgentRuntimeError::RecoveryRequired);
    }
    let outcome = match turn.status {
        TurnStatus::Running => return Ok(None),
        TurnStatus::Completed => AgentTurnOutcome::Completed {
            final_message: if resource_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
                recovered_root_final_message(state, session, &resource_agent_id, &resource_turn_id)?
            } else {
                state
                    .sub_agents
                    .get(&resource_agent_id)
                    .filter(|agent| agent.current_turn_id.as_ref() == Some(&resource_turn_id))
                    .and_then(|agent| agent.result_summary.clone())
            },
        },
        TurnStatus::Cancelled => AgentTurnOutcome::Interrupted,
        TurnStatus::Failed => {
            let message = turn
                .outcome_message
                .as_ref()
                .filter(|message| !message.trim().is_empty())
                .cloned()
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            AgentTurnOutcome::Failed { message }
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
            Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
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
        && turn.prompt_summary != expected_summary
    {
        return Err(AgentRuntimeError::RecoveryRequired);
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
    Ok(Some(actual_outcome))
}

/// 只收集 checkpoint 当前未决 Turn 在 Runtime Journal 中已经形成的唯一权威终态，
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
    for agent in checkpoint.roots.iter().flat_map(|root| &root.agents) {
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
        if validate_journal_turn_correspondence(
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
        )?
        .is_some_and(|outcome| outcomes.insert(turn_id.clone(), outcome).is_some())
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
    let snapshot = session
        .snapshot()
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
    let mut body = dynamic_input_marker_line(&marker)
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    body.push_str("\n以下是本轮安全边界前已持久排队的 Agent mailbox 消息：");
    for message in &claim.mailbox_messages {
        let kind = match &message.kind {
            MailboxMessageKind::AgentMessage => "agent_message",
            MailboxMessageKind::ChildTurnFinished { .. } => "child_turn_finished",
        };
        body.push_str(&format!(
            "\n\n[sequence={} source={} kind={kind}]\n{}",
            message.sequence,
            message.source_agent_id.as_str(),
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
    /// 仅在项目存在活跃 Goal 时，以模型 Round 和调用尝试稳定身份同步幂等累计明确用量。
    fn commit(&self, usage: &ModelRoundUsage) -> Result<(), AgentCommitSinkError> {
        if usage.session_id().as_str() != self.session_id {
            return Err(AgentCommitSinkError::rejected(
                "模型 Round 用量不属于当前 Goal Session",
            ));
        }
        let snapshot = self.persistent_state.goal_snapshot().map_err(|_| {
            AgentCommitSinkError::indeterminate("无法读取模型 Round 对应的项目 Goal")
        })?;
        if snapshot
            .goal
            .as_ref()
            .is_none_or(|goal| goal.status != GoalStatus::Active)
        {
            return Ok(());
        }
        let reported = &usage.completion().usage;
        let tokens = reported.total_tokens.or_else(|| {
            reported
                .input_tokens
                .zip(reported.output_tokens)
                .and_then(|(input, output)| input.checked_add(output))
        });
        let elapsed_seconds = usage.elapsed_millis().div_ceil(1_000).max(1);
        let operation_id = format!(
            "goal-usage:{}:{}:{}:{}:{}:{}",
            usage.session_id().as_str(),
            usage.turn_id().as_str(),
            usage.source_agent_id().as_str(),
            usage.purpose().as_str(),
            usage.model_round(),
            usage.call_attempt(),
        );
        match self.persistent_state.record_goal_usage(
            &operation_id,
            GoalUsageDelta {
                tokens: tokens.unwrap_or(0),
                elapsed_seconds,
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
                let _ = prepared.completion.send(Err(()));
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
                    let _ = prepared.completion.send(Err(()));
                }
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                    // Runtime 错误只有稳定说明，不包含文件正文、路径或 Provider 凭据。
                    error: CollaborationPortError::new(error.to_string()),
                };
            }
        };
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => {
                    if let Some(prepared) = prepared_root.take() {
                        let _ = prepared.completion.send(Err(()));
                    }
                    return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                        error: CollaborationPortError::new("Agent 执行端状态不可用"),
                    };
                }
            };
            if !self.accepting_work.load(Ordering::Acquire) {
                if let Some(prepared) = prepared_root.take() {
                    let _ = prepared.completion.send(Err(()));
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
            state.running_turns.insert(
                launch.turn_id.clone(),
                ManagedRuntimeTurn {
                    agent_id: launch.agent.agent_id.clone(),
                    agent_depth: launch.agent.depth,
                    summary,
                    started_at_unix_ms: unix_time_ms(),
                    started: Instant::now(),
                    cancellation: launch.cancellation.clone(),
                    terminal_outcome: None,
                },
            );
        }
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
                    let _ = completion.send(Err(()));
                }
                return AgentTurnStartResult::PermanentRejectedBeforeSideEffect { error };
            }
        };
        let execution_state = Arc::clone(&self.state);
        let execution_idle = Arc::clone(&self.idle);
        let agent_id = launch.agent.agent_id.clone();
        let turn_id = launch.turn_id.clone();
        let cancellation = launch.cancellation.clone();
        tauri::async_runtime::spawn(async move {
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
            let command_result = result.as_ref().map(|_| ()).map_err(|_| ());
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
            // 只有 Coordinator 已确认终态时才释放 accepted 标记；失败路径保留它，
            // 防止同一 Turn 在当前进程内因持久终态尚未确认而再次执行。
            release_runtime_turn_state(
                execution_state.as_ref(),
                execution_idle.as_ref(),
                &turn_id,
                completion_error.is_none(),
            );
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
        shutdown_background_tasks_blocking(Arc::clone(&self.background_tasks))?;
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
                message: bounded_collaboration_failure(
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
            message: bounded_collaboration_failure(&error.to_string()),
        },
    }
}

/// 提取最后一次模型响应的普通文本；纯工具或推理响应保持 `None`。
fn model_response_text(response: &keencode_model::ModelResponse) -> Option<String> {
    let text = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| bounded_collaboration_failure(&text))
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

/// 将一条扩展诊断格式化为不包含凭据且满足 ACP 上限的系统通知正文。
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

/// 在 UTF-8 字符边界内限制进入 ACP 通知的扩展诊断正文。
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

/// 在 Tokio worker 之外执行异步后台任务关闭，避免同步端口嵌套 block_on。
fn shutdown_background_tasks_blocking(
    manager: Arc<BackgroundTaskManager>,
) -> Result<(), CollaborationPortError> {
    std::thread::Builder::new()
        .name("keencode-agent-background-shutdown".to_owned())
        .spawn(move || tauri::async_runtime::block_on(manager.shutdown()))
        .map_err(|_| CollaborationPortError::new("无法创建后台任务关闭线程"))?
        .join()
        .map_err(|_| CollaborationPortError::new("后台任务关闭线程异常退出"))?
        .map(|_| ())
        .map_err(|_| CollaborationPortError::new("后台任务关闭失败"))
}

impl Error for AgentRuntimeError {}

/// 一个能声明待决请求并严格处理完整 JSON-RPC 响应的路由。
pub trait ClientRequestRouter: Send + Sync {
    /// 判断请求标识是否属于当前路由的待决账本。
    fn contains_pending(&self, request_id: &str) -> bool;

    /// 严格处理完整 JSON-RPC 响应，错误说明不得包含敏感载荷。
    fn respond(&self, response_json: &str) -> Result<(), String>;
}

/// 自研 Runtime 的进程内唯一桌面装配根。
pub struct AgentRuntime {
    /// 三种厂商协议共享的原子热替换 Provider 注册表。
    provider_registry: ProviderRegistry,
    /// 串行化 Provider 注册表替换与默认模型代次发布。
    provider_reload: Mutex<()>,
    /// 与注册表代次绑定且不包含凭据的默认模型选择。
    default_provider: RwLock<Option<DefaultProviderBinding>>,
    /// 按 Session 隔离本地资源、租约和 Turn 生命周期的运行时管理器。
    runtime_manager: RuntimeManager,
    /// 本地 Runtime 与工具 Artifact 共同使用的应用数据根。
    storage_root: PathBuf,
    /// 每个已连接 Session 当前唯一的桌面投递世代。
    deliveries: Mutex<HashMap<String, SessionDeliverySender>>,
    /// 每个 Session 串行化投递世代替换和关闭，避免旧泵仍在运行时发布新泵。
    delivery_reset_gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 每个 Session 当前唯一 Runtime 订阅泵及其显式停止通道。
    live_pumps: Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>,
    /// 为订阅泵分配不复用的进程内世代，防止旧泵清理新泵状态。
    next_live_pump_generation: AtomicU64,
    /// Elicitation 等 Agent 到 Client 请求的可注入路由。
    client_request_routers: RwLock<Vec<Arc<dyn ClientRequestRouter>>>,
    /// 全部 Session 共享且不恢复旧问答的标准 ACP Elicitation 协调器。
    elicitations: Arc<ElicitationCoordinator>,
    /// 当前桌面窗口聚焦的唯一 Session；仅用于通知路由，不参与授权。
    focused_session: RwLock<Option<String>>,
    /// WebFetch 与 WebSearch 当前原子配置；为空时工具表不暴露网络工具。
    web_service: RwLock<Option<WebServiceConfig>>,
    /// 新建 Collaboration 协调器使用的后台 Agent 全局 Turn 上限。
    background_agent_limit: AtomicUsize,
    /// 已实际启动过 Agent 树的 Session 级 Collaboration v2 生产装配。
    collaboration_sessions: Mutex<HashMap<String, Arc<SessionCollaborationRuntime>>>,
    /// 每个 Session 串行化 Turn 启动屏障，避免 Accepted 先于权威 TurnStarted。
    turn_start_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 每个 Session 串行化标题付费请求；关闭投递时移除，容量只随打开 Session 增长。
    title_generation_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 按规范项目根隔离、仅在完整构建成功后原子发布的扩展候选。
    extension_candidates: RwLock<HashMap<PathBuf, Arc<RuntimeExtensionCandidate>>>,
    /// Tauri 或测试环境提供的同步可靠投递边界。
    emitter: Arc<dyn DeliveryEmitter>,
    /// 当前 Runtime 创建的投递世代使用的时间边界；生产装配固定使用有界生产配置。
    delivery_timeouts: DeliveryTimeouts,
    /// 关闭后禁止建立新投递世代或热加载配置。
    closed: AtomicBool,
    /// 串行化进程退出，并保留首次失败结果，避免失败后重复调用伪造成功。
    shutdown_error: Mutex<Option<AgentRuntimeError>>,
    /// 防止并发 shutdown 在首次调用尚未记录失败结果时提前返回成功。
    shutdown_gate: AsyncMutex<()>,
}

impl AgentRuntime {
    /// 从当前 KeenCode 数据根和 Provider 配置创建生产装配根。
    pub fn build(app: &AppHandle) -> Result<Arc<Self>, AgentRuntimeError> {
        let storage_root =
            storage::root_dir(app).map_err(|_| AgentRuntimeError::InitializationFailed)?;
        let emitter: Arc<dyn DeliveryEmitter> = Arc::new(TauriDeliveryEmitter { app: app.clone() });
        let analytics = Arc::new(
            AnalyticsRecorder::new(app).map_err(|_| AgentRuntimeError::InitializationFailed)?,
        );
        app.manage(Arc::clone(&analytics));
        let registry = ProviderRegistry::with_request_observer(analytics);
        let runtime = Arc::new(Self::new_with_registry(storage_root, emitter, registry)?);
        runtime
            .reload_providers(app)
            .map_err(|_| AgentRuntimeError::InitializationFailed)?;
        Ok(runtime)
    }

    /// 使用明确存储根和投递器创建尚未连接 Session 的装配根。
    #[cfg(test)]
    fn new(
        storage_root: impl Into<std::path::PathBuf>,
        emitter: Arc<dyn DeliveryEmitter>,
    ) -> Result<Self, AgentRuntimeError> {
        Self::new_with_registry(storage_root, emitter, ProviderRegistry::new())
    }

    /// 使用明确 Provider 注册表创建测试或生产装配根。
    fn new_with_registry(
        storage_root: impl Into<std::path::PathBuf>,
        emitter: Arc<dyn DeliveryEmitter>,
        provider_registry: ProviderRegistry,
    ) -> Result<Self, AgentRuntimeError> {
        Self::new_with_registry_and_delivery_timeouts(
            storage_root,
            emitter,
            provider_registry,
            DeliveryTimeouts::production(),
        )
    }

    /// 使用指定投递时间边界创建 Runtime；仅测试需要缩短等待时使用。
    #[cfg(test)]
    fn new_with_delivery_timeouts(
        storage_root: impl Into<std::path::PathBuf>,
        emitter: Arc<dyn DeliveryEmitter>,
        delivery_timeouts: DeliveryTimeouts,
    ) -> Result<Self, AgentRuntimeError> {
        Self::new_with_registry_and_delivery_timeouts(
            storage_root,
            emitter,
            ProviderRegistry::new(),
            delivery_timeouts,
        )
    }

    /// 使用明确 Provider 注册表和投递时间边界创建 Runtime 的内部装配函数。
    fn new_with_registry_and_delivery_timeouts(
        storage_root: impl Into<std::path::PathBuf>,
        emitter: Arc<dyn DeliveryEmitter>,
        provider_registry: ProviderRegistry,
        delivery_timeouts: DeliveryTimeouts,
    ) -> Result<Self, AgentRuntimeError> {
        let storage_root = storage_root.into();
        let runtime_manager = RuntimeManager::new(RuntimeConfig::new(storage_root.clone()))
            .map_err(|_| AgentRuntimeError::InitializationFailed)?;
        let client_request_gate = Arc::new(ClientRequestDisplayGate::new());
        let elicitations = Arc::new(ElicitationCoordinator::with_gate(Arc::clone(
            &client_request_gate,
        )));
        let elicitation_router: Arc<dyn ClientRequestRouter> = elicitations.clone();
        Ok(Self {
            provider_registry,
            provider_reload: Mutex::new(()),
            default_provider: RwLock::new(None),
            runtime_manager,
            storage_root,
            deliveries: Mutex::new(HashMap::new()),
            delivery_reset_gates: Mutex::new(HashMap::new()),
            live_pumps: Mutex::new(HashMap::new()),
            next_live_pump_generation: AtomicU64::new(0),
            client_request_routers: RwLock::new(vec![elicitation_router]),
            elicitations,
            focused_session: RwLock::new(None),
            web_service: RwLock::new(None),
            background_agent_limit: AtomicUsize::new(DEFAULT_BACKGROUND_AGENT_LIMIT as usize),
            collaboration_sessions: Mutex::new(HashMap::new()),
            turn_start_gates: Mutex::new(HashMap::new()),
            title_generation_gates: Mutex::new(HashMap::new()),
            extension_candidates: RwLock::new(HashMap::new()),
            emitter,
            delivery_timeouts,
            closed: AtomicBool::new(false),
            shutdown_error: Mutex::new(None),
            shutdown_gate: AsyncMutex::new(()),
        })
    }

    /// 返回三种厂商协议共享的 Provider 注册表。
    pub fn provider_registry(&self) -> &ProviderRegistry {
        &self.provider_registry
    }

    /// 返回进程内唯一的 Session Runtime 管理器。
    pub fn runtime_manager(&self) -> &RuntimeManager {
        &self.runtime_manager
    }

    /// 返回进程内唯一 Elicitation 协调器，AskUser 不得建立旁路待决账本。
    pub fn elicitation_coordinator(&self) -> &Arc<ElicitationCoordinator> {
        &self.elicitations
    }

    /// 更新后续与现存 Session 的后台 Agent 并发上限，Coordinator 总槽位始终包含根 Turn。
    pub fn set_background_agent_limit(&self, limit: usize) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        if limit == 0 {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let next_turn_limit = collaboration_turn_limit(limit)?;
        let previous = self.background_agent_limit.load(Ordering::Acquire);
        let previous_turn_limit = collaboration_turn_limit(previous)?;
        let runtimes = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut updated: Vec<Arc<SessionCollaborationRuntime>> = Vec::new();
        for runtime in &runtimes {
            if runtime
                .coordinator
                .update_turn_limits(&runtime.root_agent_id, next_turn_limit, next_turn_limit)
                .is_err()
            {
                for applied in updated {
                    let _ = applied.coordinator.update_turn_limits(
                        &applied.root_agent_id,
                        previous_turn_limit,
                        previous_turn_limit,
                    );
                }
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
            updated.push(Arc::clone(runtime));
        }
        self.background_agent_limit.store(limit, Ordering::Release);
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
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let project_root = canonical_project_root(project_root)?;
        let session = if let Some(session_id) = requested_session_id {
            validate_session_id(session_id)?;
            match self.runtime_manager.get(session_id.to_owned()) {
                Ok(session) => session,
                Err(RuntimeError::SessionNotRegistered) => {
                    match self.runtime_manager.open(session_id.to_owned()) {
                        Ok(OpenSessionResult::Ready(session)) => session,
                        Ok(OpenSessionResult::Corrupt(_))
                        | Err(RuntimeError::SessionNotCreated)
                        | Err(RuntimeError::SessionNotRegistered) => {
                            return Err(AgentRuntimeError::SessionUnavailable);
                        }
                        Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
                    }
                }
                Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
            }
        } else {
            let generated = deterministic_session_id(&project_root, create_operation_id)?;
            let title = project_root
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("新对话")
                .to_owned();
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
                            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
                        Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
                    }
                }
                Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
            }
        };
        ensure_session_project(&session, &project_root)?;
        Ok(session)
    }

    /// 返回一个已打开 Session 的权威一致快照。
    pub fn session_snapshot(&self, session_id: &str) -> Result<RuntimeSnapshot, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .and_then(|session| session.snapshot())
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
    }

    /// 将桌面通知焦点切换到一个已经打开的 Session。
    pub fn focus_session(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        *self
            .focused_session
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = Some(session_id.to_owned());
        Ok(())
    }

    /// 清除桌面通知焦点，不关闭 Session 或取消 Turn。
    pub fn clear_focus(&self) {
        if let Ok(mut focused) = self.focused_session.write() {
            *focused = None;
        }
    }

    /// 返回当前桌面通知焦点的 Session 标识。
    pub fn focused_session_id(&self) -> Result<Option<String>, AgentRuntimeError> {
        self.focused_session
            .read()
            .map(|focused| focused.clone())
            .map_err(|_| AgentRuntimeError::StateUnavailable)
    }

    /// 从磁盘当前唯一配置原子热替换全部 Provider。
    pub fn reload_providers(
        &self,
        app: &AppHandle,
    ) -> Result<ProviderRegistrySnapshot, AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let _reload = self
            .provider_reload
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let current = providers::list(app).map_err(|_| AgentRuntimeError::ProviderReloadFailed)?;
        let snapshot = providers::replace_runtime_registry(&self.provider_registry, &current)
            .map_err(|_| AgentRuntimeError::ProviderReloadFailed)?;
        let binding = match (&current.active_provider_id, &current.default_model) {
            (Some(provider_id), Some(model)) => {
                let resolved = self
                    .provider_registry
                    .resolve(provider_id, model)
                    .map_err(|_| AgentRuntimeError::ProviderReloadFailed)?;
                if resolved.generation() != snapshot.generation {
                    return Err(AgentRuntimeError::ProviderReloadFailed);
                }
                Some(DefaultProviderBinding {
                    provider_id: provider_id.clone(),
                    model: model.clone(),
                    generation: snapshot.generation,
                })
            }
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => {
                return Err(AgentRuntimeError::ProviderReloadFailed);
            }
        };
        *self
            .default_provider
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)? = binding;
        Ok(snapshot)
    }

    /// 判断当前默认 Provider 与模型能否在同一注册表代次中解析。
    pub fn provider_is_configured(&self) -> bool {
        self.resolve_default_provider().is_ok()
    }

    /// 返回磁盘中全部新格式 Session 的无正文元数据。
    pub fn stored_sessions(&self) -> anyhow::Result<Vec<StoredSessionMetadata>> {
        self.runtime_manager
            .list_stored_sessions()
            .context("列出 Runtime Session 失败")
    }

    /// 读取一个健康 Session 的完整原始 Transcript。
    pub fn session_transcript(&self, session_id: &str) -> anyhow::Result<Vec<SessionMessage>> {
        self.runtime_manager
            .session_transcript(session_id)
            .with_context(|| format!("读取 Session {session_id} Transcript 失败"))
    }

    /// 返回仍有 Turn、子 Agent、工具、终端或工作树需要收尾的 Session 标识。
    pub fn active_session_ids(&self) -> Result<Vec<String>, AgentRuntimeError> {
        let session_ids = self
            .runtime_manager
            .registered_session_ids()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
    }

    /// 向与来源 Session 绑定同一项目且已连接桌面投递的全部 Session 发布 Goal 变化。
    pub fn publish_goal_changed(
        &self,
        source_session_id: &str,
        goal_id: Option<String>,
        revision: u64,
        status: Option<String>,
    ) {
        let Ok(source) = self.runtime_manager.get(source_session_id.to_owned()) else {
            return;
        };
        let Ok(source_snapshot) = source.snapshot() else {
            return;
        };
        let Ok(session_ids) = self.runtime_manager.registered_session_ids() else {
            return;
        };
        for session_id in session_ids {
            let Ok(session) = self.runtime_manager.get(session_id.as_str().to_owned()) else {
                continue;
            };
            let Ok(snapshot) = session.snapshot() else {
                continue;
            };
            if snapshot.state.project_root != source_snapshot.state.project_root {
                continue;
            }
            let Ok(delivery) = self.session_delivery(session_id.as_str()) else {
                continue;
            };
            let _ = delivery.send_batch_detached(vec![DeliveryDraft::KeenCodeEvent {
                turn_id: None,
                source_agent_id: None,
                journal_sequence: None,
                occurred_at_ms: unix_time_ms(),
                event: KeenCodeEvent::GoalChanged {
                    goal_id: goal_id.clone(),
                    revision,
                    status: status.clone(),
                },
            }]);
        }
    }

    /// 用当前默认 Provider 执行按指定 Schema 严格校验的无工具记忆模型调用。
    pub async fn generate_isolated(
        &self,
        system_prompt: &str,
        input: &str,
        timeout_secs: u64,
        structured_output: StructuredOutputConfig,
    ) -> anyhow::Result<String> {
        self.generate_isolated_for_purpose(
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
        const TITLE_SYSTEM_PROMPT: &str = "从用户消息中提取编码任务主题，并生成简洁中文标题。你不是在回答用户，也不要判断任务能否执行。只输出单行标题，不加引号、序号、句号或解释，最多 18 个汉字或 36 个字符。";
        validate_session_id(session_id)?;
        if input.trim().is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let input_sha256 = title_input_sha256(input);
        let gate = {
            let mut gates = self
                .title_generation_gates
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?;
            Arc::clone(
                gates
                    .entry(session_id.to_owned())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _gate = gate.lock().await;
        if let Some(title) = session
            .cached_generated_title(operation_id, &input_sha256)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        {
            return Ok(title);
        }
        let snapshot = session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let provider = self.resolve_session_provider(snapshot.state.provider.as_ref())?;
        let title = self
            .generate_isolated_with_provider(
                provider,
                TITLE_SYSTEM_PROMPT,
                input,
                TITLE_GENERATION_TIMEOUT_SECS,
                "title",
                None,
            )
            .await
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let title = validate_generated_title(&title)?;
        session
            .cache_generated_title(operation_id, &input_sha256, title)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
    }

    /// 执行不带业务工具的隔离模型调用，并只接受 Runtime 内部固定用途。
    async fn generate_isolated_for_purpose(
        &self,
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
            system_prompt,
            input,
            timeout_secs,
            purpose,
            Some(structured_output),
        )
        .await
    }

    /// 使用绑定注册表代次的 Provider 隔离生成；结果工具仅承载数据，不执行业务操作。
    async fn generate_isolated_with_provider(
        &self,
        provider: ResolvedProvider,
        system_prompt: &str,
        input: &str,
        timeout_secs: u64,
        purpose: &'static str,
        structured_output: Option<StructuredOutputConfig>,
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
            request.messages[0] = Message::text(
                MessageRole::System,
                format!(
                    "{system_prompt}\n\n本次 JSON 对象必须通过唯一结果工具的 value 字段提交；不得再输出可见正文。该工具仅提交数据，不执行文件、命令或网络操作。"
                ),
            );
        }
        request
            .metadata
            .insert(REQUEST_METADATA_PURPOSE.to_owned(), purpose.to_owned());
        let response = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            provider.complete(request),
        )
        .await
        .map_err(|_| anyhow!("隔离模型调用超时"))??;
        // HTTP 200 不代表内容符合契约；结果工具终态由共用解析器校验，绝不实际执行。
        if let Some(value) = structured_mode
            .parse_response(&response)
            .context("隔离记忆模型响应不符合结构化输出约定")?
        {
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
        if provider.transport_fingerprint() != snapshot.config_fingerprint
            || provider_protocol_snapshot(provider.protocol()) != snapshot.protocol
        {
            return Err(AgentRuntimeError::ProviderConfigurationChanged);
        }
        Ok(provider)
    }

    /// 解析子 Agent 模型；普通模型沿用 Session Provider，复合引用显式选择 Provider。
    fn resolve_child_agent_provider(
        &self,
        session_provider: Option<&ProviderSnapshot>,
        model_reference: &str,
    ) -> Result<ResolvedProvider, AgentRuntimeError> {
        if let Some((provider_id, model)) = split_child_agent_model_override(model_reference)? {
            return self
                .provider_registry
                .resolve(provider_id, model)
                .map_err(|_| AgentRuntimeError::ProviderNotConfigured);
        }
        if let Some(provider) = session_provider {
            return self
                .provider_registry
                .resolve(&provider.provider_id, model_reference)
                .map_err(|_| AgentRuntimeError::ProviderNotConfigured);
        }
        let default = self.resolve_default_provider()?;
        self.provider_registry
            .resolve(default.provider_id(), model_reference)
            .map_err(|_| AgentRuntimeError::ProviderNotConfigured)
    }

    /// 原子替换 WebFetch 与 WebSearch 的服务配置；为空时后续 Turn 不注册网络工具。
    pub fn set_web_service_config(
        &self,
        config: Option<WebServiceConfig>,
    ) -> Result<(), AgentRuntimeError> {
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
        let project_root = canonical_project_root(project_root)?;
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
        current.insert(project_root, Arc::new(candidate));
        Ok(generation)
    }

    /// 同步撤销全部项目候选中的 MCP 工具，供配置失效或变更时 fail-closed 使用。
    pub fn revoke_mcp_extension_tools(&self) -> Result<(), AgentRuntimeError> {
        let candidates = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for candidate in candidates {
            candidate
                .contributor
                .revoke_mcp_tools()
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
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
        let snapshot = session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let project_root = canonical_project_root(Path::new(&snapshot.state.project_root))?;
        let persistent_state = Arc::new(
            PersistentAgentState::open(session.clone())
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
        );
        let background_tasks = Arc::new(
            BackgroundTaskManager::new(
                self.storage_root.join("background-tasks").join(&session_id),
                BACKGROUND_OUTPUT_CHUNK_BYTES,
            )
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
        );
        let worktrees = Arc::new(
            GitWorktreeLeaseManager::open(
                self.storage_root.join("agent-worktrees").join(&session_id),
            )
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
        );
        worktrees
            .recover_stale()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let store = Arc::new(SessionCollaborationStore::new(
            &self.storage_root,
            &session_id,
        )?);
        store.bind_runtime_session(session)?;
        let execution = Arc::new(RuntimeAgentExecution::new(
            Arc::downgrade(self),
            session.clone(),
            project_root.clone(),
            Arc::clone(&persistent_state),
            Arc::clone(&background_tasks),
            Arc::clone(&worktrees),
            Arc::clone(&store),
        ));
        let turn_limit =
            collaboration_turn_limit(self.background_agent_limit.load(Ordering::Acquire))?;
        let coordinator = Arc::new(CollaborationCoordinator::new(
            CollaborationLimits::new(turn_limit)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
            store.clone(),
            execution.clone(),
            Arc::new(UuidCollaborationIdGenerator),
        ));
        execution.bind_coordinator(&coordinator)?;
        let root_agent_id = RunnerAgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
            .map_err(|_| AgentRuntimeError::InvalidSession)?;
        let recovered_transition = store
            .load_transition_snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        let refreshed_snapshot = session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
        if handles.is_empty() {
            let provisional_profile = AgentProfile {
                model: seed.model,
                reasoning_effort: seed.reasoning_effort,
                plan_guard: seed.plan_guard,
                cwd: project_root,
                worktree_lease: None,
                tool_snapshot: Vec::new(),
            };
            let delivery = self.session_delivery(&session_id)?;
            let (registry, _) = self.assemble_agent_tools(
                &execution,
                Arc::clone(&coordinator),
                &provisional_profile,
                provisional_profile.plan_guard,
                AgentCapabilities {
                    can_spawn_agent: true,
                },
                &delivery,
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        }
        recover_dynamic_input_acknowledgements(session, &coordinator, &recovered_claims)?;
        coordinator
            .reconcile_outbox()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        tauri::async_runtime::spawn(run_background_task_completion_pump(
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
        let (resolved, reasoning_effort, input_messages, mut request_context, summary) = if is_root
        {
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                let mut system = "你是 KeenCode 单层子 Agent。只完成分派任务，并通过协作工具向根 Agent 汇报可验证结果。".to_owned();
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

        // 每次主/子 Agent Turn 都从当前隔离数据根和自身工作目录加载指令。
        // 它们只进入 Provider 请求，因此冷恢复和后续 Turn 不会叠加旧指令正文。
        if let Some(instructions) =
            crate::personalization::prompt_context(&self.storage_root, &launch.agent.profile.cwd)
                .map_err(|_| AgentRuntimeError::InstructionsUnavailable)?
        {
            request_context.insert(0, Message::text(MessageRole::Developer, instructions));
        }

        let source_resource_id =
            keencode_resources::AgentId::new(launch.agent.agent_id.as_str().to_owned())
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let mut transcript = if is_root {
            execution
                .session
                .model_transcript_for_agent(&source_resource_id)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        } else {
            let mut inherited = launch
                .agent
                .context_snapshot
                .iter()
                .map(|message| {
                    serde_json::from_str::<Message>(message)
                        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !matches!(launch.cause, AgentTurnCause::InitialTask) {
                inherited.extend(
                    execution
                        .session
                        .model_transcript_for_agent(&source_resource_id)
                        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
                );
            }
            inherited
        };
        if is_root
            && !transcript
                .iter()
                .any(|message| message.role == MessageRole::System)
            && !input_messages
                .iter()
                .any(|message| message.role == MessageRole::System)
        {
            transcript.push(Message::text(
                MessageRole::System,
                "你是 KeenCode 编码 Agent。遵守用户要求和当前项目约束，先理解代码再执行，工具结果必须如实处理。",
            ));
        }
        // 动态上下文只在 Provider 边界装配，持久输入仍按原顺序进入 Runtime Journal。
        transcript.extend(input_messages.clone());

        let delivery = self.session_delivery(&execution.session_id)?;
        let coordinator = execution
            .coordinator()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let (registry, hooks) = self.assemble_agent_tools(
            execution,
            Arc::clone(&coordinator),
            &launch.agent.profile,
            launch.plan_guard,
            launch.capabilities,
            &delivery,
        )?;
        if is_root {
            self.send_extension_diagnostics(
                execution,
                &delivery,
                &launch.turn_id,
                &launch.agent.agent_id,
            );
        }
        let tool_snapshot = runtime_tool_snapshot(&launch.agent.profile, is_root);
        let tools = registry
            .select_exact(&tool_snapshot)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let provider: Arc<dyn ModelProvider> = Arc::new(
            TurnBoundProvider::new(
                Arc::new(resolved.clone()),
                &execution.session_id,
                launch.turn_id.as_str(),
                launch.agent.agent_id.as_str(),
            )
            .with_request_context(request_context),
        );
        // 压缩摘要不能看到只服务于当前模型请求的动态上下文，避免把它间接写入摘要 Transcript。
        let compressor_provider: Arc<dyn ModelProvider> = Arc::new(TurnBoundProvider::new(
            Arc::new(resolved.clone()),
            &execution.session_id,
            launch.turn_id.as_str(),
            launch.agent.agent_id.as_str(),
        ));
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
            limits.max_rounds = max_turns;
        }
        let runner = execution.session.bind_agent_runner_with_usage_sink(
            AgentRunner::new(provider, tools, limits)
                .with_context_manager(ContextManager::for_provider(compressor_provider))
                .with_hook_runtime(hooks)
                .with_dynamic_input_source(Arc::new(RuntimeDynamicInputSource {
                    store: Arc::clone(&execution.store),
                    session_id: execution.session_id.clone(),
                    coordinator,
                    session: execution.session.clone(),
                })),
            Arc::new(RuntimeGoalUsageSink {
                session_id: execution.session_id.clone(),
                persistent_state: Arc::clone(&execution.persistent_state),
                owner: execution.owner.clone(),
            }),
        );
        let runtime_request = if is_root {
            RuntimeTurnRequest::root(request, input_messages, summary.clone())
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
                        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
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
            } else {
                RuntimeTurnRequest::child(
                    request,
                    input_messages,
                    launch.root_turn_id.as_str(),
                    parent_turn_id.as_str(),
                    summary.clone(),
                )
            }
        };
        Ok((runner, runtime_request, summary))
    }

    /// 将当前扩展候选的降级诊断以一次性 Turn 级 ACP 通知送达客户端。
    ///
    /// 扩展故障属于可选能力，通知投递失败不能阻断核心 Agent；诊断已经在工具层
    /// 完成脱敏和截断，且每个 Session/候选代次只发送一次，避免子 Agent 或重试刷屏。
    fn send_extension_diagnostics(
        &self,
        execution: &RuntimeAgentExecution,
        delivery: &SessionDeliverySender,
        turn_id: &AgentTurnId,
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
        let Ok(true) = execution.claim_extension_diagnostics(candidate.generation()) else {
            tracing::warn!(target: "extensions", "登记扩展候选诊断发送状态失败");
            return;
        };
        let drafts = candidate
            .contributor
            .diagnostics()
            .iter()
            .map(|diagnostic| DeliveryDraft::KeenCodeEvent {
                turn_id: Some(turn_id.as_str().to_owned()),
                source_agent_id: Some(agent_id.as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms: unix_time_ms(),
                event: KeenCodeEvent::SystemNotification {
                    level: SystemNotificationLevel::Warning,
                    message: extension_diagnostic_message(diagnostic),
                },
            })
            .collect::<Vec<_>>();
        if let Err(error) = delivery.send_batch_detached(drafts) {
            let _ = execution.release_extension_diagnostics_claim(candidate.generation());
            tracing::warn!(target: "extensions", %error, "扩展诊断 ACP 通知未能排队");
        }
    }

    /// 为单个 Agent Turn 装配完整候选工具表；调用方必须最后按 Profile 精确筛选。
    fn assemble_agent_tools(
        &self,
        execution: &RuntimeAgentExecution,
        coordinator: Arc<CollaborationCoordinator>,
        profile: &AgentProfile,
        plan_guard: PlanGuard,
        capabilities: AgentCapabilities,
        delivery: &SessionDeliverySender,
    ) -> Result<(ToolRegistry, HookRuntime), AgentRuntimeError> {
        let project_root = execution.project_root.clone();
        let environment = Arc::new(
            ToolEnvironment::new(&profile.cwd)
                .and_then(|environment| {
                    environment.with_artifact_directory(
                        self.storage_root
                            .join("tool-output")
                            .join(&execution.session_id),
                    )
                })
                .map(|environment| {
                    environment.with_file_mutation_recorder(Arc::new(
                        file_changes::RuntimeFileMutationRecorder::new(execution.session.clone()),
                    ))
                })
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
        );
        let mut tools = ToolRegistry::new();
        register_local_tools_with_background(
            &mut tools,
            environment.clone(),
            Arc::clone(&execution.background_tasks),
        )
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        register_state_tools(
            &mut tools,
            execution.persistent_state.clone(),
            execution.persistent_state.clone(),
            execution.persistent_state.clone(),
        )
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        // 只有 Client 在 initialize 中声明 form 能力，运行时才暴露交互问答工具。
        if self.elicitations.supports_form() {
            let question_handler = Arc::new(
                self.elicitations.handler(
                    AgentSessionId::new(execution.session_id.clone())
                        .map_err(|_| AgentRuntimeError::InvalidSession)?,
                    Arc::new(delivery.clone()),
                ),
            );
            tools
                .register(Arc::new(AskUserTool::new(question_handler)))
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        }
        if let Some(web_service) = self
            .web_service
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .clone()
        {
            register_web_tools(&mut tools, environment.clone(), web_service)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        }
        let tool_context = RuntimeToolContext {
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
        let hooks = if let Some(candidate) = extension.as_ref() {
            candidate
                .contributor
                .prepare_lsp_runtime(&tool_context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
            candidate
                .contributor
                .register_tools(&mut tools, &tool_context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
            candidate
                .contributor
                .build_hook_runtime(&tool_context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        } else {
            HookRuntime::empty()
        };
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        } else {
            register_collaboration_tools(
                &mut tools,
                coordinator,
                profile.clone(),
                capabilities,
                context_source,
            )
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        }
        Ok((tools, hooks))
    }

    /// 测试专用：不创建协作 Session，只冻结本地、Web 与扩展候选工具。
    #[cfg(test)]
    fn freeze_turn_tools(
        &self,
        session_id: &str,
        project_root: &Path,
        plan_guard: PlanGuard,
        _delivery: &SessionDeliverySender,
    ) -> Result<(ToolRegistry, HookRuntime), AgentRuntimeError> {
        let project_root = canonical_project_root(project_root)?;
        let environment = Arc::new(
            ToolEnvironment::new(&project_root)
                .and_then(|environment| {
                    environment.with_artifact_directory(
                        self.storage_root.join("tool-output").join(session_id),
                    )
                })
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
        );
        let mut tools = ToolRegistry::new();
        keencode_tools::register_local_tools(&mut tools, Arc::clone(&environment))
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        if let Some(web_service) = self
            .web_service
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .clone()
        {
            register_web_tools(&mut tools, environment, web_service)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        }
        let context = RuntimeToolContext {
            session_id: session_id.to_owned(),
            project_root: project_root.clone(),
            plan_guard,
        };
        let extension = self
            .extension_candidates
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(&project_root)
            .cloned();
        let hooks = if let Some(candidate) = extension {
            candidate
                .contributor
                .prepare_lsp_runtime(&context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
            candidate
                .contributor
                .register_tools(&mut tools, &context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
            candidate
                .contributor
                .build_hook_runtime(&context)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        } else {
            HookRuntime::empty()
        };
        Ok((tools, hooks))
    }

    /// 启动根 Turn，并等待权威 TurnStarted 已登记后才向命令层返回 Accepted。
    pub async fn start_root_turn(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
        text: &str,
        options: RootTurnOptions,
    ) -> Result<RootTurnStartOutcome, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        if text.trim().is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
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
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let normalized_developer_context = options
            .developer_context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty());
        let summary = root_turn_summary(text, normalized_developer_context, options.plan_enabled);
        let snapshot = session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        let (resolved, reasoning_effort) = match &snapshot.state.provider {
            Some(provider) => (
                self.resolve_session_provider(Some(provider))?,
                provider.reasoning_effort,
            ),
            None => {
                let resolved = self.resolve_default_provider()?;
                session
                    .set_provider_snapshot(
                        &control_operation_id("provider", session_id, turn_id),
                        provider_snapshot(&resolved),
                    )
                    .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
                (resolved, None)
            }
        };
        let mut input_messages = Vec::new();
        let mut request_context = Vec::new();
        let root_resource_id =
            keencode_resources::AgentId::new(keencode_resources::ROOT_AGENT_ID.to_owned())
                .map_err(|_| AgentRuntimeError::InvalidSession)?;
        let root_transcript = session
            .model_transcript_for_agent(&root_resource_id)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        if !root_transcript
            .iter()
            .any(|message| message.role == MessageRole::System)
        {
            input_messages.push(Message::text(
                MessageRole::System,
                "你是 KeenCode 编码 Agent。遵守用户要求和当前项目约束，先理解代码再执行，工具结果必须如实处理。",
            ));
        }
        if let Some(context) = normalized_developer_context {
            request_context.push(Message::text(MessageRole::Developer, context));
        }
        input_messages.push(Message::text(MessageRole::User, text));
        let delivery = self.ensure_session_delivery(session_id)?;
        let plan = if options.plan_enabled {
            PlanGuard::read_only()
        } else {
            PlanGuard::inactive()
        };
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
        let (registry, _) = self.assemble_agent_tools(
            &collaboration.execution,
            Arc::clone(&collaboration.coordinator),
            &root_profile,
            plan,
            AgentCapabilities {
                can_spawn_agent: true,
            },
            &delivery,
        )?;
        root_profile.tool_snapshot = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        collaboration
            .coordinator
            .update_root_profile(&collaboration.root_agent_id, root_profile)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        let mut barrier_subscription = session
            .subscribe()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        if snapshot.state.plan.enabled != options.plan_enabled
            && session
                .set_plan(
                    &control_operation_id("plan", session_id, turn_id),
                    keencode_resources::PlanState {
                        enabled: options.plan_enabled,
                        // 切换只读模式不等于清除计划；Plan 工具的 clear 动作负责移除
                        // 正文与 Artifact，模式事件必须保留当前最终计划引用。
                        plan_artifact: snapshot.state.plan.plan_artifact.clone(),
                    },
                )
                .is_err()
        {
            collaboration
                .execution
                .discard_prepared_root_turn(&agent_turn_id);
            return Err(AgentRuntimeError::RuntimeOperationFailed);
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
        if begin_result.is_err() {
            collaboration
                .execution
                .discard_prepared_root_turn(&agent_turn_id);
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let wait_for_started = wait_for_turn_started(&mut barrier_subscription, turn_id);
        tokio::pin!(wait_for_started);
        tokio::select! {
            biased;
            started = &mut wait_for_started => started?,
            completed = completed_receiver => {
                match completed {
                    Ok(Err(())) | Err(_) => return Err(AgentRuntimeError::RuntimeOperationFailed),
                    Ok(Ok(())) => {
                        tokio::time::timeout(Duration::from_secs(1), &mut wait_for_started)
                            .await
                            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)??;
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        {
            let same_target = matches!(
                &record.event,
                SessionEvent::ProviderSnapshotUpdated { provider }
                    if provider.provider_id == provider_id && provider.model == model
            );
            if same_target {
                return session
                    .snapshot()
                    .map_err(|_| AgentRuntimeError::RuntimeOperationFailed);
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
                    config_fingerprint: provider.transport_fingerprint().to_owned(),
                    reasoning_effort,
                },
            )
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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

        let snapshot = session
            .snapshot()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let mut provider = match snapshot.state.provider {
            Some(provider) => {
                self.resolve_session_provider(Some(&provider))?;
                provider
            }
            None => provider_snapshot(&self.resolve_default_provider()?),
        };
        provider.reasoning_effort = requested_reasoning_effort;
        session
            .set_provider_snapshot_in_domain(OPERATION_DOMAIN, operation_id, provider)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        Ok(())
    }

    /// 为一个已由 Runtime 打开或创建的 Session 建立首个桌面投递世代。
    pub fn attach_session_delivery(
        &self,
        session_id: &str,
    ) -> Result<SessionDeliverySender, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if let Some(delivery) = deliveries.get(session_id) {
            return Ok(delivery.clone());
        }
        let delivery = SessionDeliverySender::spawn_with_config(
            session_id,
            Arc::clone(&self.emitter),
            false,
            DELIVERY_QUEUE_CAPACITY,
            self.delivery_timeouts,
        );
        deliveries.insert(session_id.to_owned(), delivery.clone());
        Ok(delivery)
    }

    /// 幂等确保一个已打开 Session 具有唯一桌面投递 FIFO。
    pub fn ensure_session_delivery(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<SessionDeliverySender, AgentRuntimeError> {
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let delivery = self.attach_session_delivery(session_id)?;
        let mut pumps = self
            .live_pumps
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        if pumps.contains_key(session_id) {
            return Ok(delivery);
        }
        let subscription = session
            .subscribe()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let generation = self
            .next_live_pump_generation
            .fetch_add(1, Ordering::AcqRel)
            .checked_add(1)
            .ok_or(AgentRuntimeError::StateUnavailable)?;
        let (cancel, cancelled) = oneshot::channel();
        pumps.insert(session_id.to_owned(), (generation, cancel));
        tauri::async_runtime::spawn(run_runtime_event_pump(
            Arc::downgrade(self),
            session_id.to_owned(),
            generation,
            subscription,
            cancelled,
        ));
        Ok(delivery)
    }

    /// 返回一个已建立 Session 的当前桌面投递世代。
    pub fn session_delivery(
        &self,
        session_id: &str,
    ) -> Result<SessionDeliverySender, AgentRuntimeError> {
        validate_session_id(session_id)?;
        self.deliveries
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
            .ok_or(AgentRuntimeError::SessionDeliveryMissing)
    }

    /// 返回当前 Session 的投递世代门；所有替换和关闭都必须在同一门内完成。
    fn delivery_reset_gate(
        &self,
        session_id: &str,
    ) -> Result<Arc<AsyncMutex<()>>, AgentRuntimeError> {
        let mut gates = self
            .delivery_reset_gates
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        Ok(Arc::clone(
            gates
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        ))
    }

    /// 替换 Session 当前投递世代，使下一条标准更新从序号一重新开始。
    pub async fn reset_session_delivery(
        &self,
        session_id: &str,
    ) -> Result<SessionDeliverySender, AgentRuntimeError> {
        validate_session_id(session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        let reset_gate = self.delivery_reset_gate(session_id)?;
        let _reset_guard = reset_gate.lock().await;
        let previous = self
            .deliveries
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        if let Some(previous) = previous {
            // 旧 pump 关闭超时或结果未知时，必须保留旧世代并阻止新世代发布。
            previous.shutdown().await?;
        }
        let delivery = SessionDeliverySender::spawn_with_config(
            session_id,
            Arc::clone(&self.emitter),
            true,
            DELIVERY_QUEUE_CAPACITY,
            self.delivery_timeouts,
        );
        self.deliveries
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .insert(session_id.to_owned(), delivery.clone());
        Ok(delivery)
    }

    /// 关闭并移除一个 Session 的桌面投递世代；Session Runtime 仍可继续存在。
    pub async fn close_session_delivery(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        let reset_gate = self.delivery_reset_gate(session_id)?;
        let _reset_guard = reset_gate.lock().await;
        self.turn_start_gates
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .remove(session_id);
        self.title_generation_gates
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .remove(session_id);
        if let Some((_, cancel)) = self
            .live_pumps
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .remove(session_id)
        {
            let _ = cancel.send(());
        }
        let delivery = self
            .deliveries
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .remove(session_id);
        if self
            .focused_session_id()?
            .as_deref()
            .is_some_and(|focused| focused == session_id)
        {
            self.clear_focus();
        }
        if let Some(delivery) = delivery {
            delivery.shutdown().await?;
        }
        Ok(())
    }

    /// 按 Collaboration、后台资源、实时投递、Runtime Session 的所有权顺序关闭一个 Session。
    pub async fn close_session(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
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
        let mut close_error = None;
        if let Some(collaboration) = collaboration {
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
                    close_error.get_or_insert(AgentRuntimeError::StateUnavailable);
                }
            }
            drop(collaboration);
        }
        if let Err(error) = self.close_session_delivery(session_id).await {
            close_error.get_or_insert(error);
        }
        match self.runtime_manager.close(session_id.to_owned()) {
            Ok(()) | Err(RuntimeError::SessionNotRegistered) => {}
            Err(_) => {
                close_error.get_or_insert(AgentRuntimeError::RuntimeOperationFailed);
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
            let quiesced =
                tauri::async_runtime::spawn_blocking(move || execution.finish_shutdown())
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
        if let Err(error) = self.close_session_delivery(session_id).await {
            shutdown_error.get_or_insert(error);
        }
        match self.runtime_manager.close(session_id.to_owned()) {
            Ok(()) | Err(RuntimeError::SessionNotRegistered) => {}
            Err(_) => {
                shutdown_error.get_or_insert(AgentRuntimeError::RuntimeOperationFailed);
            }
        }
        match shutdown_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 通过 Collaboration 权威状态对当前 Session 的精确根 Turn 发出幂等取消信号。
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
        let requested_turn_id = AgentTurnId::new(turn_id.to_owned())
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned();
        let Some(collaboration) = collaboration else {
            let exists = session
                .snapshot()
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
                    .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
            Err(_) => Err(AgentRuntimeError::RuntimeOperationFailed),
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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
            let session_state = runtime
                .execution
                .session
                .snapshot()
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
                .state;
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
                let started_at_unix_ms = queued_agent_started_at(&session_state, &agent)?;
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
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
                Err(_) => Err(AgentRuntimeError::RuntimeOperationFailed),
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

    /// 精确取消一个后台 Shell 或单层子 Agent，保持旧 Tauri 命令的无返回值契约。
    pub fn background_task_cancel(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(), AgentRuntimeError> {
        self.background_task_cancel_outcome(session_id, task_id)
            .map(|_| ())
    }

    /// 以客户端稳定 operationId 向当前根 Turn 注入一次可恢复且跨重试去重的用户 steer。
    pub fn steer_root_turn(
        &self,
        session_id: &str,
        operation_id: &str,
        text: &str,
    ) -> Result<(), AgentRuntimeError> {
        validate_session_id(session_id)?;
        let operation_id = ToolCallId::new(operation_id.to_owned())
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let collaboration = self
            .collaboration_sessions
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .get(session_id)
            .cloned()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        collaboration
            .coordinator
            .steer_active_agent_with_operation(&collaboration.root_agent_id, &operation_id, text)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        Ok(())
    }

    /// 分页读取权威 Journal，通过 live 与 replay 共用映射器投递后返回精确水位。
    pub async fn replay_session(
        self: &Arc<Self>,
        session_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> Result<ReplaySessionResponse, AgentRuntimeError> {
        validate_session_id(session_id)?;
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        if limit == 0 || limit > MAX_REPLAY_EVENTS as usize {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let start_after = after.unwrap_or(0);
        let delivery = if after.is_none() {
            // 新的完整恢复必须先让旧 pump 完整停止，再发布从序号一开始的新世代。
            self.reset_session_delivery(session_id).await?
        } else {
            self.session_delivery(session_id)?
        };
        // 同一 delivery 世代的连续分页复用 Provider 游标；断点跳转才重新扫描历史前缀。
        let mut replay_cursor = delivery.replay_cursor.lock().await;
        let mut next_cursor = replay_cursor.clone();
        let continuous = next_cursor.frozen_state.is_some()
            && next_cursor.through_sequence.is_some()
            && next_cursor.next_after == start_after;
        if !continuous {
            let snapshot = session
                .snapshot()
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
            next_cursor.next_after = start_after;
            next_cursor.provider = provider_snapshot_before_sequence(&session, start_after)?;
            next_cursor.through_sequence = Some(snapshot.state.last_sequence);
            next_cursor.frozen_state = Some(Arc::new(snapshot.state));
        }
        // 工具是否已被 Transcript 消费必须与固定水位使用同一快照，不能读取下一页
        // 期间的新终态，否则会隐藏本页孤立工具或提前泄露水位之后的结果。
        let frozen_state = next_cursor
            .frozen_state
            .as_ref()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        let page = session
            .replay(
                (start_after != 0).then_some(start_after),
                MAX_REPLAY_EVENTS as usize,
            )
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let through_journal_sequence = next_cursor
            .through_sequence
            .unwrap_or(page.through_sequence);
        if through_journal_sequence < start_after {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        next_cursor.through_sequence = Some(through_journal_sequence);
        let mut drafts = Vec::new();
        let mut next_after = start_after;
        let mut historical_provider = next_cursor.provider.clone();
        for record in page.records {
            if record.sequence > through_journal_sequence {
                break;
            }
            // 先在临时 Provider 上映射；只有物理记录适合当前页时才提交游标。
            let (record_drafts, next_historical_provider) = map_authoritative_record_with_provider(
                &session,
                frozen_state,
                &record,
                AuthoritativeProjectionMode::Replay,
                historical_provider.clone(),
            )?;
            if record_drafts.len() > MAX_REPLAY_EVENTS as usize {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
            if !record_drafts.is_empty() && drafts.len().saturating_add(record_drafts.len()) > limit
            {
                if next_after == start_after {
                    return Err(AgentRuntimeError::RuntimeOperationFailed);
                }
                break;
            }
            drafts.extend(record_drafts);
            historical_provider = next_historical_provider;
            next_after = record.sequence;
            // 达到投影上限后仍继续消费零投影物理记录，使 Provider 快照、事件水位等
            // 不可见事实不会被卡在上一页；下一条会产生投影的记录会在上面的容量检查处
            // 保留给下一页，并且不会提交该记录携带的临时 Provider 状态。
        }
        next_cursor.next_after = next_after;
        next_cursor.provider = historical_provider;
        let replayed_events =
            u32::try_from(drafts.len()).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let has_more = next_after < through_journal_sequence;
        let through_delivery_sequence = delivery
            .send_replay_batch(drafts, through_journal_sequence, !has_more)
            .await?;
        let response = ReplaySessionResponse {
            session_id: session_id.to_owned(),
            start_after,
            next_after,
            through_journal_sequence,
            through_delivery_sequence,
            replayed_events,
            has_more,
        };
        response
            .validate()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        if !response.has_more {
            // 完整恢复结束后释放整份 Transcript 快照，不给空闲 Session 留第二份正文。
            next_cursor.frozen_state = None;
        }
        *replay_cursor = next_cursor;
        Ok(response)
    }

    /// 登记一个 Elicitation 或未来标准 Client Request 路由。
    pub fn register_client_request_router(
        &self,
        router: Arc<dyn ClientRequestRouter>,
    ) -> Result<(), AgentRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentRuntimeError::RuntimeClosed);
        }
        self.client_request_routers
            .write()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .push(router);
        Ok(())
    }

    /// 按完整字符串 JSON-RPC ID 把响应交给唯一匹配的严格路由。
    pub fn route_client_response(
        &self,
        request_id: &str,
        response_json: &str,
    ) -> Result<(), AgentRuntimeError> {
        let routers = self
            .client_request_routers
            .read()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?;
        let mut matches = routers
            .iter()
            .filter(|router| router.contains_pending(request_id));
        let router = matches
            .next()
            .ok_or(AgentRuntimeError::UnknownClientRequest)?;
        if matches.next().is_some() {
            return Err(AgentRuntimeError::StateUnavailable);
        }
        router
            .respond(response_json)
            .map_err(|_| AgentRuntimeError::ClientResponseRejected)
    }

    /// 幂等按 Collaboration、后台资源、投递与 Runtime 的所有权顺序关闭全部 Session。
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

        let result = async {
            let registered = self
                .runtime_manager
                .registered_session_ids()
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
            session_ids.extend(
                self.deliveries
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
            let live_pumps = self
                .live_pumps
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .drain()
                .map(|(_, (_, cancel))| cancel)
                .collect::<Vec<_>>();
            for cancel in live_pumps {
                let _ = cancel.send(());
            }
            let deliveries = self
                .deliveries
                .lock()
                .map_err(|_| AgentRuntimeError::StateUnavailable)?
                .drain()
                .map(|(_, delivery)| delivery)
                .collect::<Vec<_>>();
            for delivery in deliveries {
                delivery.shutdown().await?;
            }
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
    /// 仅在真正发给模型前插入的 Memory、Plan 或 Ultra 消息；不参与 Runtime Journal。
    request_context: Vec<Message>,
}

impl TurnBoundProvider {
    /// 创建只允许 `purpose=agent` 的可信 Provider 包装器。
    fn new(inner: Arc<dyn ModelProvider>, session_id: &str, turn_id: &str, agent_id: &str) -> Self {
        Self {
            inner,
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            agent_id: agent_id.to_owned(),
            request_context: Vec::new(),
        }
    }

    /// 设置当前 Agent 请求期动态上下文；调用方输入和 Runtime Transcript 保持不变。
    fn with_request_context(mut self, request_context: Vec<Message>) -> Self {
        self.request_context = request_context;
        self
    }
}

impl ModelProvider for TurnBoundProvider {
    /// Provider 能力不改变，只覆盖请求观测身份。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.inner.capabilities(model)
    }

    /// 在唯一 Provider 边界覆盖四个保留 metadata，普通重试和压缩都不能绕过。
    fn stream(
        &self,
        mut request: ModelRequest,
    ) -> ModelFuture<'_, Result<ModelStream, keencode_model::ModelError>> {
        if !self.request_context.is_empty() {
            let system_count = request
                .messages
                .iter()
                .take_while(|message| message.role == MessageRole::System)
                .count();
            let mut messages = Vec::with_capacity(
                request
                    .messages
                    .len()
                    .saturating_add(self.request_context.len()),
            );
            messages.extend(request.messages[..system_count].iter().cloned());
            messages.extend(self.request_context.iter().cloned());
            messages.extend(request.messages[system_count..].iter().cloned());
            request.messages = messages;
        }
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
        self.inner.stream(request)
    }
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
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
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

/// 将“后台 Agent 数”设置转换为包含根 Turn 的 Coordinator 总槽位数。
fn collaboration_turn_limit(background_agent_limit: usize) -> Result<usize, AgentRuntimeError> {
    background_agent_limit
        .checked_add(1)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
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
        config_fingerprint: provider.transport_fingerprint().to_owned(),
        reasoning_effort: None,
    }
}

/// 一次连续 replay 分页共享的历史 Provider 游标。
///
/// `next_after` 只在物理 Journal 记录真正被当前页接受后推进；`provider` 始终
/// 表示该游标之后一条记录的历史 Provider。`through_sequence` 在一次完整恢复中
/// 固定，避免实时追加的事件改变已经发给 Client 的分页边界。
#[derive(Clone, Debug, Default)]
struct ReplayProviderCursor {
    /// 已被当前恢复页接受的最后一个物理 Journal sequence。
    next_after: u64,
    /// 扫描到 `next_after` 后的历史 Provider 快照。
    provider: Option<ProviderSnapshot>,
    /// 当前完整 replay 固定的 Journal 水位；尚未开始时为空。
    through_sequence: Option<u64>,
    /// 与固定水位一致的状态；连续分页共享，尾页确认投递后立即释放。
    frozen_state: Option<Arc<SessionState>>,
}

/// 当前投递世代的健康和停止状态；发送端与 pump 共享，避免回执超时后继续盲发。
struct DeliveryLifecycle {
    /// 0 表示健康，1 表示已知失败，2/3 表示某个已入队命令或关闭结果未知。
    health: AtomicU8,
    /// pump 已经退出的可观察状态。
    stopped: AtomicBool,
    /// 供需要观察停止状态的边界复用的通知器。
    stopped_notify: tokio::sync::Notify,
}

/// 投递世代健康状态常量：没有失败。
const DELIVERY_HEALTHY: u8 = 0;
/// 投递世代健康状态常量：同步 emit 已明确失败。
const DELIVERY_FAILED: u8 = 1;
/// 投递世代健康状态常量：已入队命令的实际结果未知。
const DELIVERY_OUTCOME_UNKNOWN: u8 = 2;
/// 投递世代健康状态常量：关闭命令的实际结果未知。
const DELIVERY_SHUTDOWN_UNKNOWN: u8 = 3;

impl DeliveryLifecycle {
    /// 创建一个尚未发生失败且 pump 尚未停止的投递状态。
    fn new() -> Self {
        Self {
            health: AtomicU8::new(DELIVERY_HEALTHY),
            stopped: AtomicBool::new(false),
            stopped_notify: tokio::sync::Notify::new(),
        }
    }

    /// 返回当前已知的拒绝原因；未知结果会阻止任何后续数据命令。
    fn rejection(&self) -> Option<AgentRuntimeError> {
        match self.health.load(Ordering::Acquire) {
            DELIVERY_FAILED => Some(AgentRuntimeError::DeliveryPoisoned),
            DELIVERY_OUTCOME_UNKNOWN => Some(AgentRuntimeError::DeliveryOutcomeUnknown),
            DELIVERY_SHUTDOWN_UNKNOWN => Some(AgentRuntimeError::DeliveryShutdownUnknown),
            _ => None,
        }
    }

    /// 记录一个已知的 emit/映射失败，但不覆盖更严重的未知结果。
    fn mark_failed(&self) {
        let _ = self.health.compare_exchange(
            DELIVERY_HEALTHY,
            DELIVERY_FAILED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// 记录入队命令回执未知，并永久冻结当前世代。
    fn mark_outcome_unknown(&self) {
        let _ = self.health.compare_exchange(
            DELIVERY_HEALTHY,
            DELIVERY_OUTCOME_UNKNOWN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// 记录关闭结果未知，并永久冻结当前世代。
    fn mark_shutdown_unknown(&self) {
        let _ = self.health.compare_exchange(
            DELIVERY_HEALTHY,
            DELIVERY_SHUTDOWN_UNKNOWN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// 关闭已知失败的 pump 时仍可安全确认；未知结果则必须阻止新世代。
    fn shutdown_error(&self) -> Option<AgentRuntimeError> {
        match self.health.load(Ordering::Acquire) {
            DELIVERY_OUTCOME_UNKNOWN => Some(AgentRuntimeError::DeliveryOutcomeUnknown),
            DELIVERY_SHUTDOWN_UNKNOWN => Some(AgentRuntimeError::DeliveryShutdownUnknown),
            _ => None,
        }
    }

    /// 标记 pump 已经退出，并唤醒等待旧世代收敛的边界。
    fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stopped_notify.notify_waiters();
    }
}

/// Session 投递命令的可替换时间边界；生产和确定性测试共用同一实现。
#[derive(Clone, Copy, Debug)]
struct DeliveryTimeouts {
    /// 等待 mpsc 槽位的最大时间。
    queue_reserve: Duration,
    /// 入队后等待 emit 回执的最大时间。
    acknowledgement: Duration,
    /// 关闭命令等待 pump 收敛的最大时间。
    shutdown: Duration,
}

impl DeliveryTimeouts {
    /// 返回生产环境采用的明确有界时间。
    const fn production() -> Self {
        Self {
            queue_reserve: DELIVERY_QUEUE_RESERVE_TIMEOUT,
            acknowledgement: DELIVERY_ACK_TIMEOUT,
            shutdown: DELIVERY_SHUTDOWN_TIMEOUT,
        }
    }

    /// 返回测试使用的短时间边界，避免依赖脆弱的 sleep 竞态。
    #[cfg(test)]
    const fn test() -> Self {
        Self {
            queue_reserve: Duration::from_millis(20),
            acknowledgement: Duration::from_millis(40),
            shutdown: Duration::from_millis(40),
        }
    }
}

/// 一个绑定单一 Session 当前投递世代的可克隆发送句柄。
#[derive(Clone)]
pub struct SessionDeliverySender {
    /// 当前世代接收命令的有界 FIFO。
    commands: mpsc::Sender<DeliveryCommand>,
    /// 共享的失败状态；回执超时后所有生产者立即停止入队。
    lifecycle: Arc<DeliveryLifecycle>,
    /// 当前世代用于连续 replay 分页的 Provider 游标门。
    replay_cursor: Arc<AsyncMutex<ReplayProviderCursor>>,
    /// 当前发送端使用的队列、回执和关闭时间边界。
    timeouts: DeliveryTimeouts,
}

impl SessionDeliverySender {
    /// 建立持有独立 SessionSequence 的单消费者投递任务。
    #[cfg(test)]
    fn spawn(session_id: &str, emitter: Arc<dyn DeliveryEmitter>, recovering: bool) -> Self {
        Self::spawn_with_config(
            session_id,
            emitter,
            recovering,
            DELIVERY_QUEUE_CAPACITY,
            DeliveryTimeouts::production(),
        )
    }

    /// 使用指定队列容量和时间边界建立投递任务；测试通过该边界验证真实超时语义。
    fn spawn_with_config(
        session_id: &str,
        emitter: Arc<dyn DeliveryEmitter>,
        recovering: bool,
        queue_capacity: usize,
        timeouts: DeliveryTimeouts,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(queue_capacity.max(1));
        let session_id = session_id.to_owned();
        let lifecycle = Arc::new(DeliveryLifecycle::new());
        let replay_cursor = Arc::new(AsyncMutex::new(ReplayProviderCursor::default()));
        tauri::async_runtime::spawn(run_delivery_pump(
            session_id,
            emitter,
            receiver,
            recovering,
            Arc::clone(&lifecycle),
        ));
        Self {
            commands,
            lifecycle,
            replay_cursor,
            timeouts,
        }
    }

    /// 将一个完整标准 ACP Client Request 放入 Session 共享 FIFO。
    pub async fn send_client_request(
        &self,
        request: AcpClientRequestFrame,
    ) -> Result<(), AgentRuntimeError> {
        self.send_command(|acknowledged| DeliveryCommand::EmitClientRequest {
            request: Box::new(request),
            acknowledged,
        })
        .await
    }

    /// 将一个不可被其他生产者插入的标准更新或生命周期事件批次放入 FIFO。
    pub async fn send_batch(&self, drafts: Vec<DeliveryDraft>) -> Result<(), AgentRuntimeError> {
        if drafts.is_empty() {
            return Ok(());
        }
        self.send_command(|acknowledged| DeliveryCommand::EmitBatch {
            drafts,
            terminal_notice: None,
            acknowledged,
        })
        .await
    }

    /// 将一个实时权威批次放入 FIFO，并携带仅可由实时根 Turn 产生的终态通知元数据。
    async fn send_live_batch(
        &self,
        drafts: Vec<DeliveryDraft>,
        terminal_notice: Option<RootTaskTerminalNotice>,
    ) -> Result<(), AgentRuntimeError> {
        if drafts.is_empty() {
            return Ok(());
        }
        self.send_command(|acknowledged| DeliveryCommand::EmitBatch {
            drafts,
            terminal_notice,
            acknowledged,
        })
        .await
    }

    /// 从同步持久回调把非空批次无等待放入 FIFO；队列拥塞时调用方可退化为后续刷新。
    fn send_batch_detached(&self, drafts: Vec<DeliveryDraft>) -> Result<(), AgentRuntimeError> {
        if drafts.is_empty() {
            return Ok(());
        }
        if let Some(error) = self.lifecycle.rejection() {
            return Err(error);
        }
        let (acknowledged, _completed) = oneshot::channel();
        self.commands
            .try_send(DeliveryCommand::EmitBatch {
                drafts,
                terminal_notice: None,
                acknowledged,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => AgentRuntimeError::DeliveryClosed,
                mpsc::error::TrySendError::Full(_) => AgentRuntimeError::DeliveryQueueTimeout,
            })
    }

    /// 在恢复门内按 Journal 顺序投递一页，并在末页原子释放门后缓存的 live 事件。
    async fn send_replay_batch(
        &self,
        drafts: Vec<DeliveryDraft>,
        through_sequence: u64,
        final_page: bool,
    ) -> Result<u64, AgentRuntimeError> {
        self.send_command(|acknowledged| DeliveryCommand::EmitReplayBatch {
            drafts,
            through_sequence,
            final_page,
            acknowledged,
        })
        .await
    }

    /// 幂等停止当前投递世代并等待泵确认已经处理完先前命令。
    pub async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
        if let Some(error) = self.lifecycle.shutdown_error() {
            return Err(error);
        }
        if self.commands.is_closed() {
            return Ok(());
        }
        let (acknowledged, completed) = oneshot::channel();
        match tokio::time::timeout(
            self.timeouts.shutdown,
            self.commands
                .send(DeliveryCommand::Shutdown { acknowledged }),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) if self.lifecycle.health.load(Ordering::Acquire) == DELIVERY_FAILED => {
                return Ok(());
            }
            Ok(Err(_)) => {
                self.lifecycle.mark_shutdown_unknown();
                return Err(AgentRuntimeError::DeliveryShutdownUnknown);
            }
            Err(_) => {
                self.lifecycle.mark_shutdown_unknown();
                return Err(AgentRuntimeError::DeliveryShutdownUnknown);
            }
        }
        match tokio::time::timeout(self.timeouts.shutdown, completed).await {
            Ok(Ok(result)) => {
                if let Some(error) = self.lifecycle.shutdown_error() {
                    Err(error)
                } else {
                    result
                }
            }
            Ok(Err(_)) | Err(_) => {
                self.lifecycle.mark_shutdown_unknown();
                Err(AgentRuntimeError::DeliveryShutdownUnknown)
            }
        }
    }

    /// 发送一条带 oneshot 回执的泵命令并等待实际投递完成。
    async fn send_command<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, AgentRuntimeError>>) -> DeliveryCommand,
    ) -> Result<T, AgentRuntimeError> {
        if let Some(error) = self.lifecycle.rejection() {
            return Err(error);
        }
        let (acknowledged, completed) = oneshot::channel();
        let permit = match tokio::time::timeout(
            self.timeouts.queue_reserve,
            self.commands.reserve(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(AgentRuntimeError::DeliveryClosed),
            Err(_) => return Err(AgentRuntimeError::DeliveryQueueTimeout),
        };
        if let Some(error) = self.lifecycle.rejection() {
            return Err(error);
        }
        permit.send(command(acknowledged));
        match tokio::time::timeout(self.timeouts.acknowledgement, completed).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                self.lifecycle.mark_outcome_unknown();
                Err(AgentRuntimeError::DeliveryOutcomeUnknown)
            }
        }
    }
}

/// 一个尚未分配当前桌面世代序号的投递草稿。
pub enum DeliveryDraft {
    /// 标准 ACP Session 更新草稿。
    SessionUpdate {
        /// 产生更新的可选 Turn。
        turn_id: Option<String>,
        /// 产生更新的可选 Agent。
        source_agent_id: Option<String>,
        /// 更新发生时的 UTC Unix 毫秒时间。
        occurred_at_ms: u64,
        /// 权威重放事件的 Journal 序号；实时增量为空。
        journal_sequence: Option<u64>,
        /// 原样保留的标准 ACP 更新。
        update: Box<keencode_acp::schema::SessionUpdate>,
    },
    /// KeenCode 生命周期扩展事件草稿。
    KeenCodeEvent {
        /// 产生事件的可选 Turn。
        turn_id: Option<String>,
        /// 产生事件的可选 Agent。
        source_agent_id: Option<String>,
        /// 权威事件的 Journal 序号；临时事件为空。
        journal_sequence: Option<u64>,
        /// 事件发生时的 UTC Unix 毫秒时间。
        occurred_at_ms: u64,
        /// 生命周期事件正文。
        event: KeenCodeEvent,
    },
}

/// 恢复门内缓存的实时批次及其终态通知元数据；历史重放永远不携带通知。
struct BufferedLiveBatch {
    /// 尚未越过恢复门的实时投影草稿。
    drafts: Vec<DeliveryDraft>,
    /// 仅根 Turn 终态批次可能携带的桌面通知信息。
    terminal_notice: Option<RootTaskTerminalNotice>,
}

/// 每个 Session 唯一消费者接受的三种串行命令。
enum DeliveryCommand {
    /// 原子物理事件映射出的一个或多个桌面投递。
    EmitBatch {
        /// 不允许被其他生产者插入的有序草稿。
        drafts: Vec<DeliveryDraft>,
        /// 仅实时根 Turn 终态批次携带的通知信息；普通和历史投影为空。
        terminal_notice: Option<RootTaskTerminalNotice>,
        /// 最后一条实际 emit 完成后的回执。
        acknowledged: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
    /// 与普通事件共享 FIFO 的完整 Client Request。
    EmitClientRequest {
        /// 标准 ACP JSON-RPC 2.0 请求。
        request: Box<AcpClientRequestFrame>,
        /// 请求实际 emit 完成后的回执。
        acknowledged: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
    /// 恢复门内直接发送的一页权威历史。
    EmitReplayBatch {
        /// 本页按 Journal 顺序映射出的投递草稿。
        drafts: Vec<DeliveryDraft>,
        /// 本轮分页开始时冻结的权威 Journal 水位。
        through_sequence: u64,
        /// 是否为本轮恢复的最后一页。
        final_page: bool,
        /// 本页历史投递完成后的实际桌面投递水位回执。
        acknowledged: oneshot::Sender<Result<u64, AgentRuntimeError>>,
    },
    /// 处理完此前命令后关闭当前投递世代。
    Shutdown {
        /// 泵停止前发送的幂等回执。
        acknowledged: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
}

/// Tauri 唯一事件载荷的严格外层联合。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AcpDelivery {
    /// 标准 ACP Session 更新。
    SessionUpdate {
        /// 带 Session 身份和当前投递序号的标准信封。
        envelope: SessionUpdateDeliveryEnvelope,
    },
    /// KeenCode 生命周期扩展事件。
    #[serde(rename = "keencode_event")]
    KeenCodeEvent {
        /// 带独立 Journal/投递序号的扩展信封。
        envelope: KeenCodeEventEnvelope,
    },
    /// Agent 向 Client 发起的标准 JSON-RPC 请求。
    ClientRequest {
        /// 完整且未拆散的标准请求帧。
        request: AcpClientRequestFrame,
    },
}

/// 对 Tauri 和测试记录器隐藏具体发送机制的同步投递边界。
trait DeliveryEmitter: Send + Sync {
    /// 只有事件被目标边界接受后才能返回成功。
    fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError>;

    /// 在实时根 Turn 终态已经成功投递后触发桌面通知；测试投递器默认保持静默。
    fn notify_task_terminal(
        &self,
        _task_title: Option<&str>,
        _stop_reason: Option<TurnStopReason>,
    ) {
    }
}

/// 把严格联合发送到当前 Tauri 应用的生产投递器。
struct TauriDeliveryEmitter {
    /// 绑定当前桌面进程的 Tauri 句柄。
    app: AppHandle,
}

impl DeliveryEmitter for TauriDeliveryEmitter {
    /// 同步调用 Tauri emit，失败时让当前 Session 世代永久停止。
    fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
        self.app
            .emit(ACP_DELIVERY_EVENT, delivery)
            .map_err(|_| AgentRuntimeError::DesktopEmitFailed)
    }

    /// 使用当前应用设置发送一次原生根任务终态通知。
    fn notify_task_terminal(&self, task_title: Option<&str>, stop_reason: Option<TurnStopReason>) {
        self.app
            .state::<Arc<crate::task_notifications::TaskNotifications>>()
            .notify_terminal(&self.app, task_title, stop_reason);
    }
}

/// 串行处理一个 Session 当前世代的全部桌面消息。
async fn run_delivery_pump(
    session_id: String,
    emitter: Arc<dyn DeliveryEmitter>,
    mut receiver: mpsc::Receiver<DeliveryCommand>,
    mut recovering: bool,
    lifecycle: Arc<DeliveryLifecycle>,
) {
    let mut sequence = SessionSequence::new();
    let mut buffered_live = Vec::<BufferedLiveBatch>::new();
    let mut shutdown_acknowledged = None;
    while let Some(command) = receiver.recv().await {
        match command {
            DeliveryCommand::EmitBatch {
                drafts,
                terminal_notice,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else if recovering {
                    buffered_live.push(BufferedLiveBatch {
                        drafts,
                        terminal_notice,
                    });
                    Ok(())
                } else {
                    let result =
                        emit_batch(&session_id, &emitter, &mut sequence, drafts).map(|_| ());
                    if result.is_ok() {
                        notify_root_task_terminal(&emitter, terminal_notice);
                    }
                    result
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::EmitReplayBatch {
                drafts,
                through_sequence,
                final_page,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else if !recovering {
                    Err(AgentRuntimeError::RuntimeOperationFailed)
                } else {
                    let replay_result = emit_batch(&session_id, &emitter, &mut sequence, drafts);
                    if final_page {
                        match replay_result {
                            Ok(history_delivery_sequence) => {
                                let mut retained = Vec::new();
                                let mut terminal_notices = Vec::new();
                                for buffered in buffered_live.drain(..) {
                                    let mut retained_batch = false;
                                    for draft in buffered.drafts {
                                        if draft_is_after_recovery_waterline(
                                            &draft,
                                            through_sequence,
                                        ) {
                                            retained_batch = true;
                                            retained.push(draft);
                                        }
                                    }
                                    if retained_batch && let Some(notice) = buffered.terminal_notice
                                    {
                                        terminal_notices.push(notice);
                                    }
                                }
                                let release_result =
                                    emit_batch(&session_id, &emitter, &mut sequence, retained)
                                        .map(|_| ());
                                if release_result.is_ok() {
                                    recovering = false;
                                    for notice in terminal_notices {
                                        notify_root_task_terminal(&emitter, Some(notice));
                                    }
                                }
                                release_result.map(|_| history_delivery_sequence)
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        replay_result
                    }
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::EmitClientRequest {
                request,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else {
                    emitter.emit(&AcpDelivery::ClientRequest { request: *request })
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::Shutdown { acknowledged } => {
                shutdown_acknowledged = Some(acknowledged);
                break;
            }
        }
    }
    lifecycle.mark_stopped();
    if let Some(acknowledged) = shutdown_acknowledged {
        let _ = acknowledged.send(Ok(()));
    }
}

/// 只有非取消的实时根 Turn 投影成功后才进入桌面通知边界。
fn notify_root_task_terminal(
    emitter: &Arc<dyn DeliveryEmitter>,
    notice: Option<RootTaskTerminalNotice>,
) {
    let Some(notice) = notice else {
        return;
    };
    if notice.stop_reason == Some(TurnStopReason::Cancelled) {
        return;
    }
    emitter.notify_task_terminal(Some(&notice.task_title), notice.stop_reason);
}

/// 把 Session 唯一后台任务 Manager 的终态广播映射为无 Journal 游标的桌面事件。
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
        let Some(event) = background_task_completion_event(&completion) else {
            continue;
        };
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let delivery = match runtime.session_delivery(&session_id) {
            Ok(delivery) => delivery,
            Err(_) => break,
        };
        if delivery
            .send_batch(vec![DeliveryDraft::KeenCodeEvent {
                turn_id: None,
                source_agent_id: None,
                journal_sequence: None,
                occurred_at_ms: unix_time_ms(),
                event,
            }])
            .await
            .is_err()
        {
            break;
        }
    }
}

/// 构造一个通过 ACP 严格校验的后台 Shell 完成事件；可疑摘要会被安全省略。
fn background_task_completion_event(
    completion: &BackgroundTaskCompletion,
) -> Option<KeenCodeEvent> {
    let status = match completion.status {
        BackgroundTaskStatus::Running => return None,
        BackgroundTaskStatus::Succeeded => BackgroundTaskTerminalStatus::Succeeded,
        BackgroundTaskStatus::Failed => BackgroundTaskTerminalStatus::Failed,
        BackgroundTaskStatus::Cancelled => BackgroundTaskTerminalStatus::Cancelled,
    };
    validated_background_task_completion_event(
        &completion.task_id,
        BackgroundTaskKind::Shell,
        None,
        status,
        completion.duration_ms,
        Some(&completion.summary),
    )
}

/// 构造一个通过 ACP 严格校验的后台完成事件；不安全摘要会被单独省略。
fn validated_background_task_completion_event(
    task_id: &str,
    task_kind: BackgroundTaskKind,
    agent_id: Option<&str>,
    status: BackgroundTaskTerminalStatus,
    duration_ms: u64,
    summary: Option<&str>,
) -> Option<KeenCodeEvent> {
    let summary = summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned);
    let event = KeenCodeEvent::BackgroundTaskCompleted {
        task_id: task_id.to_owned(),
        task_kind,
        agent_id: agent_id.map(str::to_owned),
        status,
        duration_ms,
        summary,
    };
    if event.validate().is_ok() {
        return Some(event);
    }
    let fallback = KeenCodeEvent::BackgroundTaskCompleted {
        task_id: task_id.to_owned(),
        task_kind,
        agent_id: agent_id.map(str::to_owned),
        status,
        duration_ms,
        summary: None,
    };
    fallback.validate().is_ok().then_some(fallback)
}

/// 从子 Agent 权威终态与 Turn 时间生成 Session 级后台完成草稿。
fn agent_background_task_completion_draft(
    state: &SessionState,
    record: &SessionEventRecord,
    agent_id: &ResourceAgentId,
    turn_id: &ResourceTurnId,
    status: &SubAgentStatus,
    result_summary: Option<&str>,
) -> Result<Option<DeliveryDraft>, AgentRuntimeError> {
    let (terminal_status, expected_turn_status) = match status {
        SubAgentStatus::Completed => (
            BackgroundTaskTerminalStatus::Succeeded,
            TurnStatus::Completed,
        ),
        SubAgentStatus::Failed => (BackgroundTaskTerminalStatus::Failed, TurnStatus::Failed),
        SubAgentStatus::Interrupted | SubAgentStatus::Stopped => (
            BackgroundTaskTerminalStatus::Cancelled,
            TurnStatus::Cancelled,
        ),
        SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting => {
            return Ok(None);
        }
    };
    let turn = state
        .turns
        .get(turn_id)
        .filter(|turn| turn.source_agent_id == *agent_id && turn.status == expected_turn_status)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    let completed_at_unix_ms = turn
        .completed_at_unix_ms
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    let event = validated_background_task_completion_event(
        turn_id.as_str(),
        BackgroundTaskKind::Agent,
        Some(agent_id.as_str()),
        terminal_status,
        completed_at_unix_ms.saturating_sub(turn.started_at_unix_ms),
        result_summary,
    )
    .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    Ok(Some(DeliveryDraft::KeenCodeEvent {
        turn_id: None,
        source_agent_id: None,
        journal_sequence: None,
        occurred_at_ms: record.time_unix_ms,
        event,
    }))
}

/// 将 Runtime 有界广播订阅映射到当前 Session 投递世代，Lag 时显式要求重放。
async fn run_runtime_event_pump(
    runtime: Weak<AgentRuntime>,
    session_id: String,
    generation: u64,
    mut subscription: RuntimeEventSubscription,
    mut cancelled: oneshot::Receiver<()>,
) {
    loop {
        let received = tokio::select! {
            _ = &mut cancelled => break,
            received = subscription.recv() => received,
        };
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let lagged = matches!(&received, Err(RuntimeEventReceiveError::Lagged(_)));
        let mut terminal_notice = None;
        let drafts = match received {
            Ok(delivery) => match delivery.payload {
                RuntimeEventPayload::Transient(event) => map_transient_event(&event),
                RuntimeEventPayload::Authoritative(record) => {
                    let session = match runtime.runtime_manager.get(session_id.clone()) {
                        Ok(session) => session,
                        Err(_) => break,
                    };
                    let snapshot = match session.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(_) => break,
                    };
                    terminal_notice = root_task_terminal_notice(&snapshot.state, &record.event);
                    match map_authoritative_record(
                        &session,
                        &snapshot.state,
                        &record,
                        AuthoritativeProjectionMode::Live,
                    ) {
                        Ok(drafts) => drafts,
                        Err(_) => break,
                    }
                }
                RuntimeEventPayload::Control(RuntimeControlEvent::SessionClosed) => break,
            },
            Err(RuntimeEventReceiveError::Lagged(_)) => vec![DeliveryDraft::KeenCodeEvent {
                turn_id: None,
                source_agent_id: None,
                journal_sequence: None,
                occurred_at_ms: unix_time_ms(),
                event: KeenCodeEvent::RecoveryStateChanged {
                    state: keencode_acp::RecoveryState::Replaying,
                },
            }],
            Err(RuntimeEventReceiveError::Closed) => break,
        };
        if drafts.is_empty() {
            continue;
        }
        let sender = match runtime.session_delivery(&session_id) {
            Ok(sender) => sender,
            Err(_) => break,
        };
        if sender
            .send_live_batch(drafts, terminal_notice)
            .await
            .is_err()
        {
            break;
        }
        if lagged {
            let session = match runtime.runtime_manager.get(session_id.clone()) {
                Ok(session) => session,
                Err(_) => break,
            };
            subscription = match session.subscribe() {
                Ok(subscription) => subscription,
                Err(_) => break,
            };
        }
    }
    if let Some(runtime) = runtime.upgrade()
        && let Ok(mut pumps) = runtime.live_pumps.lock()
        && pumps
            .get(&session_id)
            .is_some_and(|(current, _)| *current == generation)
    {
        pumps.remove(&session_id);
    }
}

/// 一次实时根任务终态通知所需的最小非敏感数据。
#[derive(Clone, Debug, Eq, PartialEq)]
struct RootTaskTerminalNotice {
    /// Session 当前用户可见标题；空标题由通知模块替换为稳定回退文案。
    task_title: String,
    /// 正常完成为 `None`，非正常终态保留 Runtime 的结构化停止原因。
    stop_reason: Option<TurnStopReason>,
}

/// 从单条或原子批次权威事件中提取根 Turn 唯一终态，忽略子 Agent 终态。
fn root_task_terminal_notice(
    state: &SessionState,
    event: &SessionEvent,
) -> Option<RootTaskTerminalNotice> {
    match event {
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .find_map(|event| root_task_terminal_notice(state, event)),
        SessionEvent::TurnCompleted { turn_id } => root_turn_terminal_notice(state, turn_id, None),
        SessionEvent::TurnStopped {
            turn_id, reason, ..
        } => root_turn_terminal_notice(state, turn_id, Some(*reason)),
        _ => None,
    }
}

/// 只为根 Agent 的顶层用户 Turn 构造通知，防止后台子 Agent 完成时打扰用户。
fn root_turn_terminal_notice(
    state: &SessionState,
    turn_id: &ResourceTurnId,
    stop_reason: Option<TurnStopReason>,
) -> Option<RootTaskTerminalNotice> {
    let turn = state.turns.get(turn_id)?;
    if turn.source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        || turn.parent_turn_id.is_some()
        || turn.root_turn_id.as_str() != turn_id.as_str()
    {
        return None;
    }
    Some(RootTaskTerminalNotice {
        task_title: state.title.clone(),
        stop_reason,
    })
}

/// 将可信 Agent 实时事件映射为标准 ACP 增量或 KeenCode 临时生命周期事件。
fn map_transient_event(event: &AgentStreamEvent) -> Vec<DeliveryDraft> {
    let occurred_at_ms = unix_time_ms();
    let update = match event.kind() {
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::TextDelta { delta, .. },
        } => Some(keencode_acp::schema::SessionUpdate::AgentMessageChunk(
            keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                delta.clone(),
            )),
        )),
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::ReasoningDelta { delta, .. }
                | ModelStreamEvent::ReasoningSummaryDelta { delta, .. },
        } => Some(keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
            keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                delta.clone(),
            )),
        )),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallStart { .. },
        } => None,
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageStart { .. },
        } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ModelFirstStreamObserved,
            }];
        }
        AgentStreamEventKind::ModelFailure { .. } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::SystemNotification {
                    level: SystemNotificationLevel::Error,
                    message: "模型请求失败，Turn 将提交结构化终态".to_owned(),
                },
            }];
        }
        AgentStreamEventKind::ContextCompactionStarted { estimated_tokens } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ContextCompactionStarted {
                    estimated_tokens: *estimated_tokens,
                },
            }];
        }
        AgentStreamEventKind::ContextCompactionFailed { failure_kind } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ContextCompactionFailed {
                    failure_kind: match failure_kind {
                        ContextCompactionFailureKind::Model => CompactionFailureKind::Model,
                        ContextCompactionFailureKind::Budget => CompactionFailureKind::Budget,
                        ContextCompactionFailureKind::Storage => CompactionFailureKind::Storage,
                        ContextCompactionFailureKind::InvalidResult => {
                            CompactionFailureKind::InvalidResult
                        }
                    },
                },
            }];
        }
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::MessageEnd { .. }
                | ModelStreamEvent::Usage { .. }
                | ModelStreamEvent::ToolCallArgumentsDelta { .. }
                | ModelStreamEvent::ToolCallEnd { .. }
                | ModelStreamEvent::ReasoningContinuation { .. },
        } => None,
    };
    update
        .map(|update| {
            vec![DeliveryDraft::SessionUpdate {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                occurred_at_ms,
                journal_sequence: None,
                update: Box::new(update),
            }]
        })
        .unwrap_or_default()
}

/// 返回非零 UTC Unix 毫秒时间，系统时钟异常时使用一作为稳定下界。
fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

/// 将后台任务的 Unix 毫秒启动时间格式化为 UTC RFC 3339 毫秒文本。
fn background_task_started_at(unix_ms: u64) -> Result<String, AgentRuntimeError> {
    let unix_ms = i64::try_from(unix_ms).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    Utc.timestamp_millis_opt(unix_ms)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 将单调时钟持续时间转换为不会溢出的前端毫秒数。
fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 从根级 Collaboration 只读快照提取仍可精确取消的单层子 Agent。
fn current_child_agent_turns_for_root(
    coordinator: &CollaborationCoordinator,
    root_agent_id: &RunnerAgentId,
) -> Result<Vec<CollaborationAgentSummary>, AgentRuntimeError> {
    coordinator
        .list_agents_for_root(root_agent_id)
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
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

/// 恢复完成时只释放水位之后的权威事件；门内临时增量由后续权威提交恢复。
fn draft_is_after_recovery_waterline(draft: &DeliveryDraft, through_sequence: u64) -> bool {
    match draft {
        DeliveryDraft::SessionUpdate {
            journal_sequence: Some(sequence),
            ..
        }
        | DeliveryDraft::KeenCodeEvent {
            journal_sequence: Some(sequence),
            ..
        } => *sequence > through_sequence,
        DeliveryDraft::SessionUpdate {
            journal_sequence: None,
            ..
        }
        | DeliveryDraft::KeenCodeEvent {
            journal_sequence: None,
            ..
        } => false,
    }
}

/// 连续分配序号并投递一个不可交错批次，返回最后成功的桌面投递序号。
fn emit_batch(
    session_id: &str,
    emitter: &Arc<dyn DeliveryEmitter>,
    sequence: &mut SessionSequence,
    drafts: Vec<DeliveryDraft>,
) -> Result<u64, AgentRuntimeError> {
    for draft in drafts {
        let delivery_sequence = sequence
            .allocate()
            .map_err(|_| AgentRuntimeError::DeliverySequenceExhausted)?;
        let delivery = materialize_delivery(session_id, delivery_sequence, draft)?;
        emitter.emit(&delivery)?;
    }
    Ok(sequence.last_allocated())
}

/// 把一个草稿绑定到当前 Session 与新分配的投递序号。
fn materialize_delivery(
    session_id: &str,
    delivery_sequence: u64,
    draft: DeliveryDraft,
) -> Result<AcpDelivery, AgentRuntimeError> {
    match draft {
        DeliveryDraft::SessionUpdate {
            turn_id,
            source_agent_id,
            occurred_at_ms,
            journal_sequence: _,
            update,
        } => SessionUpdateDeliveryEnvelope::new(
            session_id,
            turn_id,
            source_agent_id,
            delivery_sequence,
            occurred_at_ms,
            *update,
        )
        .map(|envelope| AcpDelivery::SessionUpdate { envelope })
        .map_err(|_| AgentRuntimeError::DeliveryPoisoned),
        DeliveryDraft::KeenCodeEvent {
            turn_id,
            source_agent_id,
            journal_sequence,
            occurred_at_ms,
            event,
        } => {
            let params = match (turn_id, source_agent_id) {
                (Some(turn_id), Some(source_agent_id)) => KeenCodeEventEnvelopeParams::for_turn(
                    session_id,
                    turn_id,
                    source_agent_id,
                    delivery_sequence,
                    occurred_at_ms,
                    event,
                ),
                (None, None) => KeenCodeEventEnvelopeParams::for_session(
                    session_id,
                    delivery_sequence,
                    occurred_at_ms,
                    event,
                ),
                _ => return Err(AgentRuntimeError::DeliveryPoisoned),
            };
            match journal_sequence {
                Some(journal_sequence) => {
                    KeenCodeEventEnvelope::new_authoritative(journal_sequence, params)
                }
                None => KeenCodeEventEnvelope::new_transient(params),
            }
            .map(|envelope| AcpDelivery::KeenCodeEvent { envelope })
            .map_err(|_| AgentRuntimeError::DeliveryPoisoned)
        }
    }
}

/// 权威消息在实时流与历史重放中的投影方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthoritativeProjectionMode {
    /// 实时投影跳过已由模型增量发送的 Assistant 文本与推理。
    Live,
    /// 历史重放从持久 Transcript 重建完整 Assistant 内容。
    Replay,
}

/// 将一条权威 Journal 记录映射为 live 与 replay 共用语义的 ACP 草稿集合。
fn map_authoritative_record(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    mode: AuthoritativeProjectionMode,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    let (drafts, _) = map_authoritative_record_with_provider(
        session,
        state,
        record,
        mode,
        state.provider.clone(),
    )?;
    Ok(drafts)
}

/// 使用指定历史 Provider 映射一条权威 Journal 记录，并返回记录后的 Provider 状态。
fn map_authoritative_record_with_provider(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    mode: AuthoritativeProjectionMode,
    provider: Option<ProviderSnapshot>,
) -> Result<(Vec<DeliveryDraft>, Option<ProviderSnapshot>), AgentRuntimeError> {
    let mut provider = provider;
    let drafts = map_authoritative_event(
        session,
        state,
        record,
        &record.event,
        mode,
        &mut provider,
        None,
    )?;
    Ok((drafts, provider))
}

/// 递归映射普通事件或原子批次，并让批次内全部投递共享同一 Journal sequence。
fn map_authoritative_event(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    event: &SessionEvent,
    mode: AuthoritativeProjectionMode,
    provider: &mut Option<ProviderSnapshot>,
    atomic_siblings: Option<&[SessionEvent]>,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    if mode == AuthoritativeProjectionMode::Replay {
        let request_id = match event {
            SessionEvent::ToolRequested { request } => Some(&request.request_id),
            SessionEvent::ToolExecutionStarted { request_id }
            | SessionEvent::ToolFileChangePrepared { request_id, .. }
            | SessionEvent::ToolFileChangeApplied { request_id }
            | SessionEvent::ToolCompleted { request_id, .. }
            | SessionEvent::ToolSideEffectUnknown { request_id, .. } => Some(request_id),
            _ => None,
        };
        if request_id
            .and_then(|request_id| state.tools.get(request_id))
            .is_some_and(|lifecycle| lifecycle.transcript_segment.is_some())
        {
            // 完整轮次按 Transcript 中推理、文本、工具的语义顺序恢复；尚未提交
            // Transcript 的崩溃/在途工具仍由各自权威生命周期事件正常投影。
            return Ok(Vec::new());
        }
    }
    let drafts = match event {
        SessionEvent::AtomicBatch { events } => {
            let mut drafts = Vec::new();
            for nested in events {
                drafts.extend(map_authoritative_event(
                    session,
                    state,
                    record,
                    nested,
                    mode,
                    provider,
                    Some(events),
                )?);
            }
            drafts
        }
        SessionEvent::SessionCreated { title, .. } | SessionEvent::SessionRenamed { title } => {
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::SessionInfoUpdate(
                    keencode_acp::schema::SessionInfoUpdate::new().title(title.clone()),
                ),
            )]
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            ..
        } => vec![keencode_event_draft(
            record,
            Some(turn_id.as_str()),
            Some(source_agent_id.as_str()),
            KeenCodeEvent::TurnStarted {
                root_turn_id: root_turn_id.as_str().to_owned(),
                parent_turn_id: parent_turn_id
                    .as_ref()
                    .map(|turn_id| turn_id.as_str().to_owned()),
            },
        )],
        SessionEvent::TurnCompleted { turn_id } => {
            let agent_id = turn_agent_id(state, turn_id.as_str())?;
            vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id),
                KeenCodeEvent::TurnCompleted,
            )]
        }
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            let agent_id = turn_agent_id(state, turn_id.as_str())?;
            let event = match reason {
                TurnStopReason::Cancelled => KeenCodeEvent::TurnCancelled,
                TurnStopReason::Failed => KeenCodeEvent::TurnFailed {
                    failure_kind: TurnFailureKind::Internal,
                    message: message.clone(),
                },
                TurnStopReason::LimitReached => KeenCodeEvent::TurnFailed {
                    failure_kind: TurnFailureKind::Internal,
                    message: message.clone(),
                },
                TurnStopReason::ContextBlocked => KeenCodeEvent::TurnFailed {
                    failure_kind: TurnFailureKind::Context,
                    message: message.clone(),
                },
                TurnStopReason::ModelOutputLimit | TurnStopReason::ModelRefusal => {
                    KeenCodeEvent::TurnFailed {
                        failure_kind: TurnFailureKind::Model,
                        message: message.clone(),
                    }
                }
            };
            vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id),
                event,
            )]
        }
        SessionEvent::MessageAdded { message } => {
            map_persisted_message(session, state, record, message, mode, None)?
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            let mut drafts = Vec::new();
            for message in &segment.messages {
                drafts.extend(map_persisted_message(
                    session,
                    state,
                    record,
                    message,
                    mode,
                    Some(segment),
                )?);
            }
            drafts
        }
        SessionEvent::DynamicInputReceiptCommitted { .. } => Vec::new(),
        SessionEvent::ToolRequested { request } => vec![session_update_draft(
            record,
            Some(request.turn_id.as_str()),
            Some(request.agent_id.as_str()),
            keencode_acp::schema::SessionUpdate::ToolCall(
                keencode_acp::schema::ToolCall::new(
                    request.model_tool_call_id.clone(),
                    request.tool_name.clone(),
                )
                .raw_input(request.arguments.clone()),
            ),
        )],
        SessionEvent::ToolExecutionStarted { request_id } => {
            let request = tool_request(state, request_id.as_str())?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        keencode_acp::schema::ToolCallUpdateFields::new()
                            .status(keencode_acp::schema::ToolCallStatus::InProgress),
                    ),
                ),
            )]
        }
        SessionEvent::ToolFileChangePrepared { request_id, change } => {
            file_changes::change_update_drafts(session, state, record, request_id, change)?
        }
        SessionEvent::ToolFileChangeApplied { request_id } => {
            let change = state
                .tools
                .get(request_id)
                .and_then(|tool| tool.file_change.as_ref())
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            file_changes::change_update_drafts(session, state, record, request_id, change)?
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => {
            let request = tool_request(state, request_id.as_str())?;
            let fields = file_changes::with_change_content(
                session,
                state,
                request_id,
                tool_projection::completed_fields(outcome.status, &outcome.result)?,
            )?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        fields,
                    )
                    .meta(Some(tool_projection::outcome_meta(outcome.status)?)),
                ),
            )]
        }
        SessionEvent::ToolSideEffectUnknown { request_id, result } => {
            let request = tool_request(state, request_id.as_str())?;
            let fields = file_changes::with_change_content(
                session,
                state,
                request_id,
                tool_projection::completed_fields(ToolCompletionStatus::SideEffectUnknown, result)?,
            )?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        fields,
                    )
                    .meta(Some(tool_projection::outcome_meta(
                        ToolCompletionStatus::SideEffectUnknown,
                    )?)),
                ),
            )]
        }
        SessionEvent::CompactionApplied {
            turn_id,
            source_agent_id,
            compaction,
            ..
        } => vec![keencode_event_draft(
            record,
            Some(turn_id.as_str()),
            Some(source_agent_id.as_str()),
            KeenCodeEvent::ContextCompactionCompleted {
                replaced_through_sequence: record.sequence.saturating_sub(1),
                estimated_tokens: compaction.estimated_tokens_after,
            },
        )],
        SessionEvent::SubAgentSpawned { agent } => {
            let siblings = atomic_siblings.ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            let mut matching_turns = siblings.iter().filter_map(|sibling| match sibling {
                SessionEvent::TurnStarted {
                    turn_id,
                    source_agent_id,
                    root_turn_id,
                    parent_turn_id: Some(parent_turn_id),
                    ..
                } if source_agent_id == &agent.agent_id => {
                    Some((turn_id, root_turn_id, parent_turn_id))
                }
                _ => None,
            });
            let (_child_turn_id, root_turn_id, parent_turn_id) = matching_turns
                .next()
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            if matching_turns.next().is_some() {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
            vec![keencode_event_draft(
                record,
                Some(parent_turn_id.as_str()),
                Some(agent.parent_agent_id.as_str()),
                KeenCodeEvent::AgentSpawned {
                    agent_id: agent.agent_id.as_str().to_owned(),
                    parent_agent_id: agent.parent_agent_id.as_str().to_owned(),
                    agent_path: agent.agent_path.clone(),
                    task: agent.task.clone(),
                    parent_turn_id: parent_turn_id.as_str().to_owned(),
                    root_turn_id: root_turn_id.as_str().to_owned(),
                },
            )]
        }
        SessionEvent::SubAgentStatusChanged {
            agent_id,
            turn_id,
            status,
            result_summary,
        } => {
            let Some(turn_id) = turn_id.as_ref() else {
                return Ok(Vec::new());
            };
            let mut drafts = vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id.as_str()),
                KeenCodeEvent::AgentStatusChanged {
                    agent_id: agent_id.as_str().to_owned(),
                    status: map_agent_status(status),
                },
            )];
            if let Some(completion) = agent_background_task_completion_draft(
                state,
                record,
                agent_id,
                turn_id,
                status,
                result_summary.as_deref(),
            )? {
                drafts.push(completion);
            }
            drafts
        }
        SessionEvent::MailboxMessageQueued { message } => vec![keencode_event_draft(
            record,
            Some(message.related_turn_id.as_str()),
            Some(message.from.as_str()),
            KeenCodeEvent::AgentMessageQueued {
                message_id: message.message_id.as_str().to_owned(),
                from_agent_id: message.from.as_str().to_owned(),
                to_agent_id: message.to.as_str().to_owned(),
            },
        )],
        SessionEvent::ModelRoundCompleted {
            turn_id,
            source_agent_id,
            requested_model,
            usage,
            ..
        } => model_round_usage_draft(
            record,
            provider.as_ref(),
            turn_id,
            source_agent_id,
            requested_model,
            usage,
        ),
        SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TerminalStarted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. } => Vec::new(),
        SessionEvent::TodoReplaced {
            items, revision, ..
        } => {
            let mut meta = keencode_acp::schema::Meta::new();
            meta.insert("_keencode".to_owned(), json!({ "todoRevision": revision }));
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::Plan(
                    keencode_acp::schema::Plan::new(
                        items
                            .iter()
                            .map(|item| {
                                keencode_acp::schema::PlanEntry::new(
                                    item.content.clone(),
                                    keencode_acp::schema::PlanEntryPriority::Medium,
                                    match item.status {
                                        TodoStatus::Pending => {
                                            keencode_acp::schema::PlanEntryStatus::Pending
                                        }
                                        TodoStatus::InProgress => {
                                            keencode_acp::schema::PlanEntryStatus::InProgress
                                        }
                                        TodoStatus::Completed => {
                                            keencode_acp::schema::PlanEntryStatus::Completed
                                        }
                                    },
                                )
                            })
                            .collect(),
                    )
                    .meta(meta),
                ),
            )]
        }
        SessionEvent::PlanChanged { plan } => vec![session_update_draft(
            record,
            None,
            None,
            keencode_acp::schema::SessionUpdate::CurrentModeUpdate(
                keencode_acp::schema::CurrentModeUpdate::new(if plan.enabled {
                    "plan"
                } else {
                    "default"
                }),
            ),
        )],
        SessionEvent::ProviderSnapshotUpdated { provider: next } => {
            *provider = Some(next.clone());
            Vec::new()
        }
        SessionEvent::TitleGenerated { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => Vec::new(),
    };
    Ok(drafts)
}

/// 通过标准 ACP 扩展槽携带资源锚点，不冒充要求 UUID 的未启用 messageId 字段。
fn persisted_message_meta(message_id: &str) -> keencode_acp::schema::Meta {
    serde_json::Map::from_iter([(
        "keencode/messageId".to_owned(),
        serde_json::Value::String(message_id.to_owned()),
    )])
}

/// 将一条已物化的持久消息拆为标准用户、Agent 或推理内容更新。
fn map_persisted_message(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    message: &SessionMessage,
    mode: AuthoritativeProjectionMode,
    segment: Option<&TranscriptSegment>,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    if matches!(
        message.role,
        ResourceMessageRole::System | ResourceMessageRole::Developer
    ) {
        return Ok(Vec::new());
    }
    let materialized = session
        .materialize_message(message)
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let turn_id = message.turn_id.as_ref().map(|turn_id| turn_id.as_str());
    let agent_id = match (message.agent_id.as_ref(), turn_id) {
        (Some(agent_id), _) => Some(agent_id.as_str()),
        (None, Some(turn_id)) => Some(turn_agent_id(state, turn_id)?),
        (None, None) => None,
    };
    let mut drafts = Vec::new();
    for block in materialized.content {
        if mode == AuthoritativeProjectionMode::Replay
            && let Some(segment) = segment
            && let Some(tool_drafts) =
                replay_segment_tool_drafts(session, state, record, segment, &block)?
        {
            drafts.extend(tool_drafts);
            continue;
        }
        let update = match block {
            ContentBlock::Text { text } => {
                if message.role == ResourceMessageRole::Tool
                    || (message.role == ResourceMessageRole::Assistant
                        && mode == AuthoritativeProjectionMode::Live)
                {
                    continue;
                }
                let chunk = keencode_acp::schema::ContentChunk::new(
                    keencode_acp::schema::ContentBlock::from(text),
                )
                .meta(Some(persisted_message_meta(&message.message_id)));
                if message.role == ResourceMessageRole::User {
                    keencode_acp::schema::SessionUpdate::UserMessageChunk(chunk)
                } else {
                    keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk)
                }
            }
            ContentBlock::Reasoning { reasoning } => {
                if message.role != ResourceMessageRole::Assistant
                    || mode == AuthoritativeProjectionMode::Live
                {
                    continue;
                }
                keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
                    keencode_acp::schema::ContentChunk::new(
                        keencode_acp::schema::ContentBlock::from(reasoning.text),
                    )
                    .meta(Some(persisted_message_meta(&message.message_id))),
                )
            }
            ContentBlock::Image { image } => {
                if !matches!(
                    message.role,
                    ResourceMessageRole::User | ResourceMessageRole::Assistant
                ) {
                    continue;
                }
                let content = match image.source {
                    ImageSource::Base64 { media_type, data } => {
                        keencode_acp::schema::ContentBlock::Image(
                            keencode_acp::schema::ImageContent::new(data, media_type),
                        )
                    }
                    ImageSource::Url { url } => keencode_acp::schema::ContentBlock::ResourceLink(
                        keencode_acp::schema::ResourceLink::new("image", url),
                    ),
                };
                let chunk = keencode_acp::schema::ContentChunk::new(content)
                    .meta(Some(persisted_message_meta(&message.message_id)));
                if message.role == ResourceMessageRole::User {
                    keencode_acp::schema::SessionUpdate::UserMessageChunk(chunk)
                } else {
                    keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk)
                }
            }
            ContentBlock::ToolCall { tool_call } => {
                if message.role != ResourceMessageRole::Assistant
                    || (mode == AuthoritativeProjectionMode::Live
                        && persisted_tool_lifecycle_exists(state, turn_id, agent_id, &tool_call.id))
                {
                    continue;
                }
                keencode_acp::schema::SessionUpdate::ToolCall(
                    keencode_acp::schema::ToolCall::new(tool_call.id, tool_call.name)
                        .raw_input(tool_call.arguments),
                )
            }
            ContentBlock::ToolResult { tool_result } => {
                if message.role != ResourceMessageRole::Tool
                    || (mode == AuthoritativeProjectionMode::Live
                        && persisted_tool_lifecycle_exists(
                            state,
                            turn_id,
                            agent_id,
                            &tool_result.tool_call_id,
                        ))
                {
                    continue;
                }
                let status = if tool_result.is_error {
                    keencode_acp::schema::ToolCallStatus::Failed
                } else {
                    keencode_acp::schema::ToolCallStatus::Completed
                };
                let raw_output = serde_json::to_value(&tool_result.content)
                    .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        tool_result.tool_call_id,
                        keencode_acp::schema::ToolCallUpdateFields::new()
                            .status(status)
                            .raw_output(raw_output),
                    ),
                )
            }
        };
        // Session 级用户消息按资源层契约可以省略 Turn 与 Agent 身份；Assistant/Tool
        // 更新仍必须具备可审查的来源边界。
        if !matches!(message.role, ResourceMessageRole::User)
            && (turn_id.is_none() || agent_id.is_none())
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        drafts.push(session_update_draft(record, turn_id, agent_id, update));
    }
    Ok(drafts)
}

/// 在已提交段的语义位置重建工具生命周期，精确绑定 Round/段而非仅凭可能复用的模型 ID。
fn replay_segment_tool_drafts(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    segment: &TranscriptSegment,
    block: &ContentBlock,
) -> Result<Option<Vec<DeliveryDraft>>, AgentRuntimeError> {
    let (tool_call_id, is_request) = match block {
        ContentBlock::ToolCall { tool_call } => (tool_call.id.as_str(), true),
        ContentBlock::ToolResult { tool_result } => (tool_result.tool_call_id.as_str(), false),
        _ => return Ok(None),
    };
    let lifecycle = state.tools.values().find(|lifecycle| {
        lifecycle.request.model_tool_call_id == tool_call_id
            && lifecycle
                .transcript_segment
                .as_ref()
                .is_some_and(|reference| {
                    reference.turn_id == segment.turn_id
                        && reference.source_agent_id == segment.source_agent_id
                        && reference.model_round == segment.model_round
                        && reference.segment_index == segment.segment_index
                        && Some(reference.transcript_revision)
                            == segment.expected_transcript_revision.checked_add(1)
                })
    });
    let Some(lifecycle) = lifecycle else {
        // 未进入执行生命周期的模型可见错误仍由 Transcript 工具块自身恢复。
        return Ok(None);
    };
    let mut events = Vec::with_capacity(2);
    if is_request {
        events.push((
            lifecycle.requested_at_unix_ms,
            SessionEvent::ToolRequested {
                request: lifecycle.request.clone(),
            },
        ));
        if let Some(started_at) = lifecycle.execution_started_at_unix_ms {
            events.push((
                started_at,
                SessionEvent::ToolExecutionStarted {
                    request_id: lifecycle.request.request_id.clone(),
                },
            ));
        }
    } else {
        events.push((
            lifecycle
                .completed_at_unix_ms
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?,
            SessionEvent::ToolCompleted {
                request_id: lifecycle.request.request_id.clone(),
                outcome: lifecycle
                    .outcome
                    .clone()
                    .ok_or(AgentRuntimeError::RuntimeOperationFailed)?,
            },
        ));
    }
    let mut drafts = Vec::new();
    for (time_unix_ms, event) in events {
        // 保留生命周期真实时间和与 live 相同的 raw output；Journal 游标则归属于
        // 当前原子段，不能倒退到已被前页消费的物理请求记录。
        let projected = SessionEventRecord {
            schema: record.schema.clone(),
            version: record.version,
            event_id: record.event_id.clone(),
            session: record.session.clone(),
            sequence: record.sequence,
            time_unix_ms,
            event,
        };
        drafts.extend(map_authoritative_event(
            session,
            state,
            &projected,
            &projected.event,
            AuthoritativeProjectionMode::Live,
            &mut None,
            None,
        )?);
    }
    Ok(Some(drafts))
}

/// 判断一个 Transcript 工具块是否已有完整资源层 lifecycle，存在时由专用事件投影。
fn persisted_tool_lifecycle_exists(
    state: &SessionState,
    turn_id: Option<&str>,
    agent_id: Option<&str>,
    model_tool_call_id: &str,
) -> bool {
    let (Some(turn_id), Some(agent_id)) = (turn_id, agent_id) else {
        return false;
    };
    state.tools.values().any(|lifecycle| {
        lifecycle.request.turn_id.as_str() == turn_id
            && lifecycle.request.agent_id.as_str() == agent_id
            && lifecycle.request.model_tool_call_id == model_tool_call_id
    })
}

/// 构造带权威 Journal sequence 的标准 Session 更新草稿。
fn session_update_draft(
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    update: keencode_acp::schema::SessionUpdate,
) -> DeliveryDraft {
    DeliveryDraft::SessionUpdate {
        turn_id: turn_id.map(str::to_owned),
        source_agent_id: source_agent_id.map(str::to_owned),
        occurred_at_ms: record.time_unix_ms,
        journal_sequence: Some(record.sequence),
        update: Box::new(update),
    }
}

/// 将权威模型 Round 的明确用量投影为标准 ACP 上下文用量更新。
///
/// `total_tokens` 优先使用 Provider 明确报告的总数；缺少总数时才在输入和
/// 输出都明确报告时相加。上下文窗口或用量任一未知都不生成更新，避免把
/// 未知值伪造成零或把不完整的 Token 统计展示为事实。
fn model_round_usage_draft(
    record: &SessionEventRecord,
    provider: Option<&ProviderSnapshot>,
    turn_id: &ResourceTurnId,
    source_agent_id: &ResourceAgentId,
    requested_model: &str,
    usage: &keencode_model::TokenUsage,
) -> Vec<DeliveryDraft> {
    let Some(context_window) = provider
        .filter(|provider| provider.model == requested_model)
        .and_then(|provider| provider.context_window)
        .filter(|context_window| *context_window > 0)
    else {
        return Vec::new();
    };
    let used = usage.total_tokens.or_else(|| {
        usage
            .input_tokens
            .zip(usage.output_tokens)
            .and_then(|(input, output)| input.checked_add(output))
    });
    let Some(used) = used else {
        return Vec::new();
    };
    vec![session_update_draft(
        record,
        Some(turn_id.as_str()),
        Some(source_agent_id.as_str()),
        keencode_acp::schema::SessionUpdate::UsageUpdate(keencode_acp::schema::UsageUpdate::new(
            used,
            context_window,
        )),
    )]
}

/// 从 Journal 顺序恢复指定 sequence 之前最近一次 Provider 快照。
///
/// `after` 分页不能只读取当前页，否则从中间水位开始的 replay 会把最终快照误用到
/// 历史 Round；这里按有界页扫描前缀，既覆盖普通记录也覆盖 AtomicBatch 内嵌更新。
fn provider_snapshot_before_sequence(
    session: &RuntimeSession,
    sequence: u64,
) -> Result<Option<ProviderSnapshot>, AgentRuntimeError> {
    if sequence == 0 {
        return Ok(None);
    }
    let mut after = None;
    let mut provider = None;
    loop {
        let page = session
            .replay(after, MAX_REPLAY_EVENTS as usize)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        for record in &page.records {
            if record.sequence > sequence {
                return Ok(provider);
            }
            update_provider_snapshot_from_event(&mut provider, &record.event);
            if record.sequence == sequence {
                return Ok(provider);
            }
        }
        let Some(next_after) = page.next_after else {
            return Ok(provider);
        };
        if next_after <= after.unwrap_or(0) {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        after = Some(next_after);
        if !page.has_more {
            return Ok(provider);
        }
    }
}

/// 按物理 Journal 记录内的事件顺序推进 Provider 历史状态。
fn update_provider_snapshot_from_event(
    provider: &mut Option<ProviderSnapshot>,
    event: &SessionEvent,
) {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for nested in events {
                update_provider_snapshot_from_event(provider, nested);
            }
        }
        SessionEvent::ProviderSnapshotUpdated { provider: next } => {
            *provider = Some(next.clone());
        }
        _ => {}
    }
}

/// 构造带权威 Journal sequence 的 KeenCode 生命周期草稿。
fn keencode_event_draft(
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    event: KeenCodeEvent,
) -> DeliveryDraft {
    DeliveryDraft::KeenCodeEvent {
        turn_id: turn_id.map(str::to_owned),
        source_agent_id: source_agent_id.map(str::to_owned),
        journal_sequence: Some(record.sequence),
        occurred_at_ms: record.time_unix_ms,
        event,
    }
}

/// 从当前一致快照解析 Turn 的 Agent 身份。
fn turn_agent_id<'a>(state: &'a SessionState, turn_id: &str) -> Result<&'a str, AgentRuntimeError> {
    state
        .turns
        .iter()
        .find(|(known_turn_id, _)| known_turn_id.as_str() == turn_id)
        .map(|(_, turn)| turn.source_agent_id.as_str())
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 从当前一致快照解析工具生命周期的不可变请求。
fn tool_request<'a>(
    state: &'a SessionState,
    request_id: &str,
) -> Result<&'a keencode_resources::ToolRequest, AgentRuntimeError> {
    state
        .tools
        .iter()
        .find(|(known_request_id, _)| known_request_id.as_str() == request_id)
        .map(|(_, lifecycle)| &lifecycle.request)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 将资源层单层 Agent 状态映射为桌面生命周期状态。
fn map_agent_status(status: &SubAgentStatus) -> AgentLifecycleStatus {
    match status {
        SubAgentStatus::Pending => AgentLifecycleStatus::Pending,
        SubAgentStatus::Running => AgentLifecycleStatus::Running,
        SubAgentStatus::Waiting => AgentLifecycleStatus::Waiting,
        SubAgentStatus::Completed => AgentLifecycleStatus::Completed,
        SubAgentStatus::Failed => AgentLifecycleStatus::Failed,
        SubAgentStatus::Interrupted => AgentLifecycleStatus::Interrupted,
        SubAgentStatus::Stopped => AgentLifecycleStatus::Stopped,
    }
}

/// 将 Agent Goal 状态转换为 ACP GoalChanged 使用的稳定小写名称。
fn goal_status_name(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Completed => "completed",
        GoalStatus::Blocked => "blocked",
    }
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
        retain_child_agent_tool_snapshot(&mut tool_snapshot);
    }
    tool_snapshot
}

/// 校验桌面投递只接受资源层合法 Session 标识。
fn validate_session_id(session_id: &str) -> Result<(), AgentRuntimeError> {
    keencode_resources::SessionId::new(session_id.to_owned())
        .map(|_| ())
        .map_err(|_| AgentRuntimeError::InvalidSession)
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
    let snapshot = session
        .snapshot()
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let stored = canonical_project_root(Path::new(&snapshot.state.project_root))?;
    if stored != expected_project_root {
        return Err(AgentRuntimeError::SessionProjectMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AcpDelivery, AgentRuntime, AgentRuntimeError, AuthoritativeProjectionMode, ContextManager,
        DeliveryDraft, DeliveryEmitter, DeliveryTimeouts, GENERATED_TITLE_MAX_CHARS,
        RUNTIME_TURN_COMPLETION_MAX_ATTEMPTS, RootAgentSeed, RootTaskTerminalNotice,
        RootTurnOptions, RootTurnStartOutcome, RuntimeAgentTemplate, RuntimeAgentTemplateContext,
        RuntimeExtensionCandidate, RuntimeExtensionContributor, RuntimeExtensionDiagnostic,
        RuntimeGoalUsageSink, RuntimeToolContext, SessionCollaborationStore, SessionDeliverySender,
        TurnBoundProvider, authoritative_recovered_turn_outcome, background_task_completion_event,
        complete_runtime_turn, coordinator_has_pending_dynamic_input_claim,
        dynamic_input_receipt_matches_claim, extension_diagnostic_message,
        is_retryable_runtime_turn_completion_error, map_authoritative_record, materialize_delivery,
        parse_reasoning_effort, provider_snapshot, recovered_authoritative_turn_outcomes,
        release_runtime_turn_state, root_task_terminal_notice, root_turn_summary,
        runtime_tool_snapshot, should_retry_runtime_turn_completion,
        split_child_agent_model_override, validate_generated_title,
        validate_recovered_mailbox_claim, wait_for_turn_started,
    };
    use keencode_acp::schema::{
        ClientCapabilities, ContentBlock, ContentChunk, CreateElicitationRequest,
        ElicitationCapabilities, ElicitationFormCapabilities, ElicitationFormMode,
        ElicitationSchema, ElicitationSessionScope, RequestId, SessionUpdate,
    };
    use keencode_acp::{
        AcpClientRequestEncoder, BackgroundTaskKind, BackgroundTaskTerminalStatus,
        ElicitationRouter, KeenCodeEvent, SessionUpdateDeliveryEnvelope,
    };
    use keencode_agent::{
        AgentDynamicInputAcknowledgement, AgentDynamicInputBatch, AgentDynamicInputBoundary,
        AgentDynamicInputError, AgentDynamicInputSource, AgentExecutionPort, AgentPath,
        AgentProfile, AgentRunner, AgentTreeQuiesceResult, AgentTurnLaunch, AgentTurnOutcome,
        AgentTurnSignal, AgentTurnStartResult, CloseAgentTree, CollaborationAgentStatus,
        CollaborationAppendResult, CollaborationCoordinator, CollaborationError,
        CollaborationEvent, CollaborationEventKind, CollaborationLimits, CollaborationPortError,
        CollaborationStore, CollaborationTransitionCommit, ContextCompressor, ContextInheritance,
        ContextSummaryRequest, GoalController, GoalDraft, HookRuntime, PlanGuard,
        ProviderContextCompressor, QuiesceAgentTree, RecoveredCoordinator, RootAgentRequest,
        RunLimits, SpawnAgentRequest, ToolCallId, ToolRegistry, TurnCancellation,
        TurnId as AgentTurnId, TurnRequest, UuidCollaborationIdGenerator,
    };
    use keencode_model::{
        Message as ModelMessage, MessageRole, ModelError, ModelProvider, ModelRequest,
        ModelStreamEvent, ProviderCapabilities, ResponseMetadata, ScriptedProvider, ScriptedReply,
        StopReason, TokenUsage,
    };
    use keencode_provider::{
        ProviderConfig, ProviderModelPolicy, ProviderRegistration, REQUEST_METADATA_AGENT_ID,
        REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID, REQUEST_METADATA_TURN_ID,
        WireResponseMode,
    };
    use keencode_resources::{
        AgentId as ResourceAgentId, DynamicInputKind, DynamicInputReceipt,
        MailboxMessage as ResourceMailboxMessage, MailboxMessageId as ResourceMailboxMessageId,
        MailboxState, PlanState, ProviderProtocolSnapshot, ProviderSnapshot, SESSION_EVENT_SCHEMA,
        SESSION_EVENT_VERSION, SessionEvent, SessionEventId, SessionEventRecord,
        SessionId as ResourceSessionId, SessionState, SubAgentState, SubAgentStatus, TodoItem,
        TodoStatus, TranscriptRecord, TurnId as ResourceTurnId, TurnState, TurnStatus,
        TurnStopReason,
    };
    use keencode_runtime::{
        CreateSessionRequest, PersistentAgentState, RuntimeConfig, RuntimeSession,
        RuntimeTurnRequest,
    };
    use keencode_tools::{
        BackgroundTaskCompletion, BackgroundTaskStatus, GitWorktreeLeaseManager, WebServiceConfig,
    };
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Condvar, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

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

    /// 记录每次 emit/notify，并可在指定调用序号模拟同步失败。
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum EmitterCall {
        /// 一次桌面投影调用及其完整载荷。
        Emit(Value),
        /// 一次根任务终态通知调用。
        Notify {
            /// 通知使用的任务标题。
            task_title: Option<String>,
            /// 通知使用的结构化停止原因。
            stop_reason: Option<TurnStopReason>,
        },
    }

    struct RecordingEmitter {
        /// 已按实际 emit 顺序接受的 JSON 值。
        deliveries: Mutex<Vec<Value>>,
        /// 按实际调用顺序记录 emit 与终态通知。
        calls: Mutex<Vec<EmitterCall>>,
        /// 从一开始计数的可选失败调用序号。
        fail_at: Option<usize>,
    }

    impl RecordingEmitter {
        /// 创建永不失败的记录器。
        fn successful() -> Arc<Self> {
            Arc::new(Self {
                deliveries: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                fail_at: None,
            })
        }

        /// 创建在指定调用处失败的记录器。
        fn failing_at(fail_at: usize) -> Arc<Self> {
            Arc::new(Self {
                deliveries: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                fail_at: Some(fail_at),
            })
        }

        /// 返回当前完整发送顺序的副本。
        fn snapshot(&self) -> Vec<Value> {
            self.deliveries.lock().clone()
        }

        /// 返回 emit 与终态通知的完整调用顺序。
        fn calls_snapshot(&self) -> Vec<EmitterCall> {
            self.calls.lock().clone()
        }
    }

    impl DeliveryEmitter for RecordingEmitter {
        /// 先记录尝试，再在配置序号上返回发送失败。
        fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
            let value =
                serde_json::to_value(delivery).map_err(|_| AgentRuntimeError::DesktopEmitFailed)?;
            let mut deliveries = self.deliveries.lock();
            deliveries.push(value.clone());
            self.calls.lock().push(EmitterCall::Emit(value));
            if self.fail_at == Some(deliveries.len()) {
                return Err(AgentRuntimeError::DesktopEmitFailed);
            }
            Ok(())
        }

        /// 记录终态通知调用，不模拟原生通知系统本身。
        fn notify_task_terminal(
            &self,
            task_title: Option<&str>,
            stop_reason: Option<TurnStopReason>,
        ) {
            self.calls.lock().push(EmitterCall::Notify {
                task_title: task_title.map(str::to_owned),
                stop_reason,
            });
        }
    }

    /// 可由测试线程释放的同步投递闸门，用于覆盖 emit 长时间阻塞的边界。
    #[derive(Clone)]
    struct BlockingEmitter {
        /// 已进入 emit 的调用次数及其等待通知。
        calls: Arc<(StdMutex<usize>, Condvar)>,
        /// 已经从 emit 返回的调用次数及其等待通知。
        completed: Arc<(StdMutex<usize>, Condvar)>,
        /// 控制被阻塞调用继续返回的共享闸门。
        release: Arc<(StdMutex<bool>, Condvar)>,
        /// 被同步阻塞的最大调用序号；测试只阻塞前 N 次调用。
        block_through: usize,
        /// 已由 emit 接收的桌面载荷，用于验证回执未知时事件仍可能完成。
        deliveries: Arc<Mutex<Vec<Value>>>,
    }

    impl BlockingEmitter {
        /// 创建阻塞所有调用的测试投递器。
        fn blocking_all() -> Arc<Self> {
            Arc::new(Self {
                calls: Arc::new((StdMutex::new(0), Condvar::new())),
                completed: Arc::new((StdMutex::new(0), Condvar::new())),
                release: Arc::new((StdMutex::new(false), Condvar::new())),
                block_through: usize::MAX,
                deliveries: Arc::new(Mutex::new(Vec::new())),
            })
        }

        /// 等待指定数量的 emit 调用真实进入同步投递边界。
        fn wait_for_calls(&self, expected: usize) {
            let (calls, signal) = &*self.calls;
            let mut calls = calls.lock().expect("阻塞投递调用状态应可用");
            let deadline = Instant::now() + Duration::from_secs(1);
            while *calls < expected {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("等待阻塞投递调用不应超时");
                let (next, timeout) = signal
                    .wait_timeout(calls, remaining)
                    .expect("阻塞投递调用状态应可用");
                calls = next;
                assert!(!timeout.timed_out() || *calls >= expected);
            }
        }

        /// 等待指定数量的 emit 调用已经返回。
        fn wait_for_completed(&self, expected: usize) {
            let (completed, signal) = &*self.completed;
            let mut completed = completed.lock().expect("阻塞投递完成状态应可用");
            let deadline = Instant::now() + Duration::from_secs(1);
            while *completed < expected {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("等待阻塞投递完成不应超时");
                let (next, timeout) = signal
                    .wait_timeout(completed, remaining)
                    .expect("阻塞投递完成状态应可用");
                completed = next;
                assert!(!timeout.timed_out() || *completed >= expected);
            }
        }

        /// 释放全部仍在 emit 中等待的调用。
        fn release(&self) {
            let (released, signal) = &*self.release;
            let mut released = released.lock().expect("阻塞投递闸门状态应可用");
            *released = true;
            signal.notify_all();
        }

        /// 返回已经进入 emit 的桌面载荷副本。
        fn deliveries(&self) -> Vec<Value> {
            self.deliveries.lock().clone()
        }
    }

    impl DeliveryEmitter for BlockingEmitter {
        /// 记录调用后按测试闸门同步阻塞，模拟不可被异步取消的桌面边界。
        fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
            let value =
                serde_json::to_value(delivery).map_err(|_| AgentRuntimeError::DesktopEmitFailed)?;
            let call_number = {
                let (calls, signal) = &*self.calls;
                let mut calls = calls.lock().expect("阻塞投递调用状态应可用");
                *calls = calls.saturating_add(1);
                signal.notify_all();
                *calls
            };
            self.deliveries.lock().push(value);
            if call_number <= self.block_through {
                let (released, signal) = &*self.release;
                let mut released = released.lock().expect("阻塞投递闸门状态应可用");
                while !*released {
                    released = signal.wait(released).expect("阻塞投递闸门状态应可用");
                }
            }
            let (completed, signal) = &*self.completed;
            let mut completed = completed.lock().expect("阻塞投递完成状态应可用");
            *completed = completed.saturating_add(1);
            signal.notify_all();
            Ok(())
        }
    }

    /// 构造一个绑定 Turn 的标准文本增量草稿。
    fn text_draft(text: &str) -> DeliveryDraft {
        DeliveryDraft::SessionUpdate {
            turn_id: Some("turn-a".to_owned()),
            source_agent_id: Some("agent-root".to_owned()),
            occurred_at_ms: 1,
            journal_sequence: None,
            update: Box::new(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::from(text),
            ))),
        }
    }

    /// 构造带权威 Journal 序号的文本草稿，用于恢复门排序验证。
    fn journal_text_draft(text: &str, journal_sequence: u64) -> DeliveryDraft {
        DeliveryDraft::SessionUpdate {
            turn_id: Some("turn-a".to_owned()),
            source_agent_id: Some("agent-root".to_owned()),
            occurred_at_ms: journal_sequence,
            journal_sequence: Some(journal_sequence),
            update: Box::new(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::from(text),
            ))),
        }
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
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
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
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                root_turn_summary(prompt, None, false),
            ))
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
            "测试 Runtime 应在有限时限内收敛"
        );
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

    /// 创建具有明确中立能力快照的测试 Runtime，避免把默认模型能力冒充原生 Schema 支持。
    fn runtime_with_responses_capabilities(
        storage_root: &Path,
        base_url: &str,
        models: &[&str],
        capabilities: Option<ProviderCapabilities>,
    ) -> Arc<AgentRuntime> {
        let registry = keencode_provider::ProviderRegistry::new();
        let mut config = ProviderConfig::new_unauthenticated(
            "provider-runtime-test",
            keencode_model::ProviderProtocol::Responses,
            base_url,
        )
        .expect("测试 Provider 配置应有效");
        config.response_mode = WireResponseMode::Buffered;
        if let Some(capabilities) = capabilities {
            config.default_capabilities = capabilities;
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
            AgentRuntime::new_with_registry(storage_root, RecordingEmitter::successful(), registry)
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
        let runtime = AgentRuntime::new_with_registry(
            storage.path(),
            RecordingEmitter::successful(),
            registry,
        )
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
    fn task_terminal_notice_ignores_child_turns_and_handles_atomic_batches() {
        let session_id = ResourceSessionId::new("session-task-notification").unwrap();
        let mut state = SessionState::empty(session_id);
        state.title = "通知测试任务".to_owned();
        let root_turn_id = ResourceTurnId::new("turn-root").unwrap();
        state.turns.insert(
            root_turn_id.clone(),
            TurnState {
                turn_id: root_turn_id.clone(),
                source_agent_id: ResourceAgentId::new(keencode_resources::ROOT_AGENT_ID).unwrap(),
                root_turn_id: root_turn_id.clone(),
                parent_turn_id: None,
                prompt_summary: "根任务".to_owned(),
                started_at_unix_ms: 1,
                completed_at_unix_ms: Some(2),
                status: TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        assert_eq!(
            root_task_terminal_notice(
                &state,
                &SessionEvent::TurnCompleted {
                    turn_id: root_turn_id.clone(),
                },
            ),
            Some(RootTaskTerminalNotice {
                task_title: "通知测试任务".to_owned(),
                stop_reason: None,
            })
        );

        let child_turn_id = ResourceTurnId::new("turn-child").unwrap();
        state.turns.insert(
            child_turn_id.clone(),
            TurnState {
                turn_id: child_turn_id.clone(),
                source_agent_id: ResourceAgentId::new("agent-child").unwrap(),
                root_turn_id: root_turn_id.clone(),
                parent_turn_id: Some(root_turn_id.clone()),
                prompt_summary: "子任务".to_owned(),
                started_at_unix_ms: 1,
                completed_at_unix_ms: Some(2),
                status: TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        assert_eq!(
            root_task_terminal_notice(
                &state,
                &SessionEvent::TurnCompleted {
                    turn_id: child_turn_id.clone(),
                },
            ),
            None
        );

        let notice = root_task_terminal_notice(
            &state,
            &SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnCompleted {
                        turn_id: child_turn_id,
                    },
                    SessionEvent::TurnStopped {
                        turn_id: root_turn_id,
                        reason: TurnStopReason::ContextBlocked,
                        message: "上下文无法继续".to_owned(),
                    },
                ],
            },
        )
        .expect("原子批次中的根 Turn 终态应产生通知");
        assert_eq!(notice.stop_reason, Some(TurnStopReason::ContextBlocked));
    }

    /// 实时根 Turn 必须先完成桌面 emit 才通知；取消、空投影和失败投递均保持静默。
    #[tokio::test]
    async fn live_terminal_notification_follows_projection_and_filters_non_notifiable_cases() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        sender
            .send_live_batch(
                vec![text_draft("completed")],
                Some(RootTaskTerminalNotice {
                    task_title: "完成任务".to_owned(),
                    stop_reason: None,
                }),
            )
            .await
            .expect("成功的实时根 Turn 应完成投递");
        let calls = emitter.calls_snapshot();
        assert_eq!(calls.len(), 2);
        assert!(matches!(
            &calls[0],
            EmitterCall::Emit(value) if value["envelope"]["update"]["content"]["text"] == "completed"
        ));
        assert_eq!(
            calls[1],
            EmitterCall::Notify {
                task_title: Some("完成任务".to_owned()),
                stop_reason: None,
            }
        );

        sender
            .send_live_batch(
                vec![text_draft("cancelled")],
                Some(RootTaskTerminalNotice {
                    task_title: "取消任务".to_owned(),
                    stop_reason: Some(TurnStopReason::Cancelled),
                }),
            )
            .await
            .expect("取消的实时根 Turn 仍应完成桌面投递");
        sender
            .send_live_batch(
                Vec::new(),
                Some(RootTaskTerminalNotice {
                    task_title: "空投影".to_owned(),
                    stop_reason: None,
                }),
            )
            .await
            .expect("空投影应作为无操作成功");
        let calls = emitter.calls_snapshot();
        assert_eq!(calls.len(), 3);
        assert!(
            matches!(&calls[2], EmitterCall::Emit(value) if value["envelope"]["update"]["content"]["text"] == "cancelled")
        );
        sender.shutdown().await.expect("成功投递器应关闭");

        let failing = RecordingEmitter::failing_at(1);
        let failing_sender = SessionDeliverySender::spawn("session-b", failing.clone(), false);
        assert_eq!(
            failing_sender
                .send_live_batch(
                    vec![text_draft("failed emit")],
                    Some(RootTaskTerminalNotice {
                        task_title: "失败任务".to_owned(),
                        stop_reason: Some(TurnStopReason::Failed),
                    }),
                )
                .await,
            Err(AgentRuntimeError::DesktopEmitFailed)
        );
        assert_eq!(failing.calls_snapshot().len(), 1);
        assert!(matches!(&failing.calls_snapshot()[0], EmitterCall::Emit(_)));
        failing_sender
            .shutdown()
            .await
            .expect("失败投递器仍应可关闭");

        let replay_failing = RecordingEmitter::failing_at(1);
        let replay_failing_sender = SessionDeliverySender::spawn("session-c", replay_failing, true);
        assert_eq!(
            replay_failing_sender
                .send_replay_batch(vec![journal_text_draft("failed replay", 1)], 1, true)
                .await,
            Err(AgentRuntimeError::DesktopEmitFailed)
        );
        replay_failing_sender
            .shutdown()
            .await
            .expect("历史投递失败后仍应可关闭投递器");
    }

    /// 历史重放不通知，恢复门只在较新的实时根投影真正释放后通知一次。
    #[tokio::test]
    async fn recovery_delays_live_terminal_notification_until_release() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), true);
        sender
            .send_live_batch(
                vec![journal_text_draft("stale-live", 1)],
                Some(RootTaskTerminalNotice {
                    task_title: "过期任务".to_owned(),
                    stop_reason: None,
                }),
            )
            .await
            .expect("恢复期间的旧实时事件应缓存");
        sender
            .send_live_batch(
                vec![journal_text_draft("new-live", 2)],
                Some(RootTaskTerminalNotice {
                    task_title: "新任务".to_owned(),
                    stop_reason: Some(TurnStopReason::Failed),
                }),
            )
            .await
            .expect("恢复期间的新实时事件应缓存");
        assert!(emitter.calls_snapshot().is_empty());

        let history_delivery_sequence = sender
            .send_replay_batch(vec![journal_text_draft("historical", 1)], 1, true)
            .await
            .expect("历史末页应先投递再释放实时缓存");
        assert_eq!(history_delivery_sequence, 1);
        let calls = emitter.calls_snapshot();
        assert_eq!(calls.len(), 3);
        assert!(matches!(
            &calls[0],
            EmitterCall::Emit(value) if value["envelope"]["update"]["content"]["text"] == "historical"
        ));
        assert!(matches!(
            &calls[1],
            EmitterCall::Emit(value) if value["envelope"]["update"]["content"]["text"] == "new-live"
        ));
        assert_eq!(
            calls[2],
            EmitterCall::Notify {
                task_title: Some("新任务".to_owned()),
                stop_reason: Some(TurnStopReason::Failed),
            }
        );
        sender.shutdown().await.expect("恢复投递器应关闭");
    }

    /// Runtime Journal 的失败与子 Agent 完成终态必须无损转换为 Collaboration 恢复结果。
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

    /// Collaboration 已保存终态但 Journal 缺少对应 Turn 时，冷启动必须要求恢复而不能静默接受。
    #[test]
    fn terminal_collaboration_checkpoint_without_journal_is_recovery_required() {
        let storage = tempfile::tempdir().expect("应创建 Collaboration 存储目录");
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
            .steer_agent(&root_agent_id, &turn_id, "待恢复 steer")
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
                vec![self.receipt],
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
            .steer_agent(&root_agent_id, &turn_id, "待恢复 steer")
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
            .steer_agent(&root_agent_id, &old_turn_id, "旧动态 steer")
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
        let dynamic_message = format!(
            "{}\n以下是旧 Turn 的动态 steer：\n\n{}",
            serde_json::to_string(&marker).expect("动态 marker 应可编码"),
            steer.content
        );
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
            message: ModelMessage::text(MessageRole::User, dynamic_message),
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
                                        matches!(content, keencode_model::ContentBlock::Text { text } if text.contains("旧 Turn 的动态 steer"))
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
                                                matches!(content, keencode_model::ContentBlock::Text { text } if text.contains("旧 Turn 的动态 steer"))
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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

    /// 成功根 Turn 的最终文本必须从权威 Transcript 恢复，不能因 Runtime 重启而触发恢复栅栏。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_completed_non_empty_final_message_cold_recovery_preserves_outcome() {
        assert_completed_root_survives_cold_recovery("根 Turn 冷恢复必须保留的最终文本").await;
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
            runtime.background_task_cancel(&session_id, child.initial_turn_id.as_str());
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
            transcript_revision: 2,
        });
        let claim = super::RecoveredDynamicInputClaim {
            agent_id: keencode_agent::AgentId::new(agent_id.as_str()).unwrap(),
            turn_id: keencode_agent::TurnId::new(turn_id.as_str()).unwrap(),
            kind: super::DynamicInputMarkerKind::Mailbox,
            through_sequence: 4,
            mailbox_message_ids: Vec::new(),
            mailbox_messages: Vec::new(),
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

    /// 冷恢复的旧子 Agent Profile 在执行选择前必须再次移除根专用工具。
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
                "SendMessage",
            ]
            .map(str::to_owned)
            .to_vec(),
        };
        assert_eq!(
            runtime_tool_snapshot(&profile, false),
            ["Read", "SendMessage"]
        );
        assert_eq!(runtime_tool_snapshot(&profile, true), profile.tool_snapshot);
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
            Arc::downgrade(runtime),
            session.clone(),
            project_root.to_path_buf(),
            persistent_state,
            background_tasks.clone(),
            worktrees,
            Arc::clone(&store),
        ));
        let coordinator = Arc::new(CollaborationCoordinator::new(
            CollaborationLimits::new(turn_limit).expect("测试容量应有效"),
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
                    per_root_turn_limit: 2,
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

    /// 创建测试用的单层子 Agent 请求，并使用项目目录作为绝对工作目录。
    fn test_spawn_request(name: &str, project_root: &Path) -> SpawnAgentRequest {
        SpawnAgentRequest {
            task_name: name.to_owned(),
            initial_task: format!("执行 {name} 测试任务"),
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

    /// 创建一个根 Turn 占满容量、并连续留下两个 WaitingCapacity pending 的测试现场。
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let collaboration =
            install_test_collaboration_runtime_with_limit(&runtime, &session, project.path(), 1);
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
    }

    impl RecordingExtensionContributor {
        /// 创建尚未收到任何冻结调用的记录贡献器。
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                diagnostics: Vec::new(),
            })
        }

        /// 创建带候选级诊断的记录贡献器。
        fn with_diagnostics(diagnostics: Vec<RuntimeExtensionDiagnostic>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                diagnostics,
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
                system_prompt: "审查实际变更".to_owned(),
                model: Some("provider-a::model-a".to_owned()),
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
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_reply("完成")],
        ));
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
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                prompt_summary,
            ))
            .await
            .expect("测试根 Turn 应完成");
    }

    /// 构造一个完整标准 Elicitation Client Request。
    fn client_request() -> keencode_acp::AcpClientRequestFrame {
        let capabilities = ClientCapabilities::new().elicitation(Some(
            ElicitationCapabilities::new().form(Some(ElicitationFormCapabilities::new())),
        ));
        let router = ElicitationRouter::from_client_capabilities(&capabilities);
        AcpClientRequestEncoder::new()
            .elicitation_request_frame(
                RequestId::Str("elicitation-runtime-test".to_owned()),
                &router,
                CreateElicitationRequest::new(
                    ElicitationFormMode::new(
                        ElicitationSessionScope::new("session-a"),
                        ElicitationSchema::new(),
                    ),
                    "请选择执行方式",
                ),
            )
            .expect("测试请求应编码")
    }

    /// 严格外层联合不得增加旧事件名或扁平字段。
    #[tokio::test]
    async fn outer_union_uses_only_current_delivery_shapes() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        sender
            .send_batch(vec![text_draft("hello")])
            .await
            .expect("标准更新应发送");
        sender
            .send_batch(vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some("turn-a".to_owned()),
                source_agent_id: Some("agent-root".to_owned()),
                journal_sequence: None,
                occurred_at_ms: 2,
                event: KeenCodeEvent::ModelFirstStreamObserved,
            }])
            .await
            .expect("临时事件应发送");
        sender
            .send_client_request(client_request())
            .await
            .expect("Client Request 应发送");
        let values = emitter.snapshot();
        assert_eq!(values[0]["type"], "session_update");
        assert!(values[0].get("envelope").is_some());
        assert_eq!(values[1]["type"], "keencode_event");
        assert!(values[1].get("envelope").is_some());
        assert_eq!(values[2]["type"], "client_request");
        assert_eq!(values[2]["request"]["jsonrpc"], "2.0");
    }

    /// 不同 Session 必须各自从一开始分配投递序号。
    #[tokio::test]
    async fn sessions_allocate_isolated_sequences() {
        let emitter = RecordingEmitter::successful();
        let first = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        let second = SessionDeliverySender::spawn("session-b", emitter.clone(), false);
        first
            .send_batch(vec![text_draft("a")])
            .await
            .expect("首个 Session 应发送");
        second
            .send_batch(vec![text_draft("b")])
            .await
            .expect("第二个 Session 应发送");
        let values = emitter.snapshot();
        assert_eq!(values[0]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[1]["envelope"]["deliverySequence"], 1);
    }

    /// 单 Session 批次内序号严格递增且回执晚于最后一次 emit。
    #[tokio::test]
    async fn batch_is_monotonic_atomic_and_acknowledged_after_last_emit() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        sender
            .send_batch(vec![text_draft("one"), text_draft("two")])
            .await
            .expect("完整批次应发送");
        let values = emitter.snapshot();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[1]["envelope"]["deliverySequence"], 2);
        assert_eq!(values[0]["envelope"]["update"]["content"]["text"], "one");
        assert_eq!(values[1]["envelope"]["update"]["content"]["text"], "two");
    }

    /// 两个并发生产者也不能把单条更新插入另一个生产者的原子批次。
    #[tokio::test]
    async fn concurrent_producer_cannot_interleave_atomic_batch() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        let batch_sender = sender.clone();
        let batch = tokio::spawn(async move {
            batch_sender
                .send_batch(vec![text_draft("batch-one"), text_draft("batch-two")])
                .await
        });
        let single =
            tokio::spawn(async move { sender.send_batch(vec![text_draft("single")]).await });
        batch
            .await
            .expect("批次任务不应 panic")
            .expect("批次应发送");
        single
            .await
            .expect("单条任务不应 panic")
            .expect("单条应发送");

        let texts = emitter
            .snapshot()
            .into_iter()
            .map(|value| {
                value["envelope"]["update"]["content"]["text"]
                    .as_str()
                    .expect("测试文本应存在")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert!(
            texts == ["batch-one", "batch-two", "single"]
                || texts == ["single", "batch-one", "batch-two"]
        );
    }

    /// Client Request 与普通更新必须共享同一个 Session FIFO。
    #[tokio::test]
    async fn client_request_and_updates_share_fifo() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        sender
            .send_batch(vec![text_draft("before")])
            .await
            .expect("前置更新应发送");
        sender
            .send_client_request(client_request())
            .await
            .expect("请求应发送");
        sender
            .send_batch(vec![text_draft("after")])
            .await
            .expect("后置更新应发送");
        let values = emitter.snapshot();
        assert_eq!(values[0]["type"], "session_update");
        assert_eq!(values[1]["type"], "client_request");
        assert_eq!(values[2]["type"], "session_update");
        assert_eq!(values[2]["envelope"]["deliverySequence"], 2);
    }

    /// emit 失败后当前世代必须拒绝全部后续投递。
    #[tokio::test]
    async fn emit_failure_poison_stops_later_delivery() {
        let emitter = RecordingEmitter::failing_at(2);
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), false);
        assert_eq!(
            sender
                .send_batch(vec![text_draft("one"), text_draft("two")])
                .await,
            Err(AgentRuntimeError::DesktopEmitFailed)
        );
        assert_eq!(
            sender.send_batch(vec![text_draft("three")]).await,
            Err(AgentRuntimeError::DeliveryPoisoned)
        );
        assert_eq!(emitter.snapshot().len(), 2);
    }

    /// 普通请求和压缩请求都必须覆盖调用方伪造的四个保留观测字段。
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
                        developer_context: Some(dynamic_context.to_owned()),
                        plan_enabled: false,
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
        assert!(input.iter().any(|message| {
            message["role"] == "developer" && message["content"][0]["text"] == dynamic_context
        }));
        assert!(input.iter().any(|message| {
            message["role"] == "developer"
                && message["content"][0]["text"].as_str().is_some_and(|text| {
                    text.contains("全局指令测试标记") && text.contains("项目指令测试标记")
                })
        }));
        assert!(input.iter().any(|message| {
            message["role"] == "user" && message["content"][0]["text"] == "检查动态上下文持久化边界"
        }));

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
        assert!(transcript_json.contains("检查动态上下文持久化边界"));
        runtime
            .close_session_delivery(&session_id)
            .await
            .expect("动态上下文测试投递应关闭");
    }

    /// 同一 Session 的连续根 Turn 必须重新读取最新 AGENTS.md，且新旧正文均不得进入 Transcript。
    #[tokio::test(flavor = "multi_thread")]
    async fn root_turn_reloads_current_instructions_between_turns() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        std::fs::write(storage.path().join("AGENTS.md"), "旧全局指令标记")
            .expect("旧全局指令应写入");
        std::fs::write(project.path().join("AGENTS.md"), "旧项目指令标记")
            .expect("旧项目指令应写入");
        let (base_url, server) =
            spawn_buffered_responses_server_for_requests("两轮上下文测试完成", 2);
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
        let session = runtime
            .open_or_create_session(project.path(), None, "reload-instructions-operation")
            .expect("指令刷新测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();

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
                    "turn-instructions-new",
                    "第二轮读取新指令",
                    RootTurnOptions::default(),
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
        assert_eq!(requests.len(), 2);
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
        let second_context = developer_context(&requests[1]);
        assert!(second_context.contains("新全局指令标记"));
        assert!(!second_context.contains("旧全局指令标记"));
        assert!(!second_context.contains("旧项目指令标记"));

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
        assert!(transcript_json.contains("第二轮读取新指令"));
        runtime
            .close_session_delivery(&session_id)
            .await
            .expect("两轮指令测试投递应关闭");
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
        let runtime =
            Arc::new(AgentRuntime::new(storage.path(), RecordingEmitter::successful()).unwrap());
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
        keencode_resources::test_support::set_append_fault(
            keencode_resources::test_support::AppendFault::Flush,
        );
        let rejected = coordinator.spawn_agent(
            &root_id,
            &root_turn,
            &ToolCallId::new("spawn-cold-failed").unwrap(),
            test_spawn_request("cold_rejected", project.path()),
        );
        keencode_resources::test_support::clear_append_fault();
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
        let recovered =
            Arc::new(AgentRuntime::new(storage.path(), RecordingEmitter::successful()).unwrap());
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
            keencode_resources::test_support::set_append_fault(
                keencode_resources::test_support::AppendFault::Flush,
            );
        }
        let result = collaboration.coordinator.spawn_agent(
            &collaboration.root_agent_id,
            &root_turn,
            &ToolCallId::new("spawn-rejected-child").unwrap(),
            request,
        );
        keencode_resources::test_support::clear_append_fault();
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
            assert!(session.snapshot().unwrap().recovery_required);
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
        let runtime = runtime_with_responses_provider(storage.path(), &base_url, &["test-model"]);
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
            runtime
                .close_session_delivery(session.session_id().as_str())
                .await
                .expect("损坏指令测试投递应关闭");
        }
    }

    /// 创建 operationId 必须稳定去重，焦点切换不得改变 Session 生命周期。
    #[test]
    fn session_creation_is_idempotent_and_focus_can_be_cleared() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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

    /// Web 服务配置只影响后续冻结工具表，关闭后不得继续暴露网络工具。
    #[test]
    fn web_service_hot_update_changes_new_turn_tool_snapshot() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = AgentRuntime::new(storage.path(), RecordingEmitter::successful())
            .expect("测试 Runtime 应创建");
        let delivery =
            SessionDeliverySender::spawn("session-web", RecordingEmitter::successful(), true);
        let before = runtime
            .freeze_turn_tools(
                "session-web",
                project.path(),
                PlanGuard::inactive(),
                &delivery,
            )
            .expect("本地工具应冻结")
            .0
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(!before.iter().any(|name| name == "WebFetch"));
        assert!(!before.iter().any(|name| name == "WebSearch"));

        runtime
            .set_web_service_config(Some(
                WebServiceConfig::new("https://example.com/").expect("测试网址应有效"),
            ))
            .expect("Web 配置应发布");
        let enabled = runtime
            .freeze_turn_tools(
                "session-web",
                project.path(),
                PlanGuard::inactive(),
                &delivery,
            )
            .expect("网络工具应冻结")
            .0
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(enabled.iter().any(|name| name == "WebFetch"));
        assert!(enabled.iter().any(|name| name == "WebSearch"));

        runtime
            .set_web_service_config(None)
            .expect("Web 配置应关闭");
        let disabled = runtime
            .freeze_turn_tools(
                "session-web",
                project.path(),
                PlanGuard::inactive(),
                &delivery,
            )
            .expect("关闭后本地工具应冻结")
            .0
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(!disabled.iter().any(|name| name == "WebFetch"));
        assert!(!disabled.iter().any(|name| name == "WebSearch"));
    }

    /// 项目撤销必须路由到唯一候选并使缓存失效，其他项目和无候选路径互不影响。
    #[test]
    fn project_mcp_revocation_invalidates_only_the_selected_runtime_candidate() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project_a = tempfile::tempdir().expect("应创建项目 A");
        let project_b = tempfile::tempdir().expect("应创建项目 B");
        let project_c = tempfile::tempdir().expect("应创建未发布候选的项目 C");
        let runtime = AgentRuntime::new(storage.path(), RecordingEmitter::successful())
            .expect("测试 Runtime 应创建");
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

    /// Agent 模板只允许从同一规范项目已经发布的候选中解析。
    #[test]
    fn extension_agent_resolution_is_project_scoped_and_strict() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建候选项目目录");
        let other_project = tempfile::tempdir().expect("应创建隔离项目目录");
        let runtime = AgentRuntime::new(storage.path(), RecordingEmitter::successful())
            .expect("测试 Runtime 应创建");
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

    /// 扩展诊断必须真实进入 ACP，并在同一候选代次中对重复根调用保持 exactly-once。
    #[tokio::test]
    async fn extension_diagnostics_are_delivered_once_to_root_acp() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let emitter = RecordingEmitter::successful();
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), emitter.clone()).expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "extension-diagnostic-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .attach_session_delivery(&session_id)
            .expect("Session 投递应建立");
        let diagnostic = RuntimeExtensionDiagnostic {
            source: "mcp".to_owned(),
            server: "plugin:local:demo:docs".to_owned(),
            code: "mcp_config_invalid".to_owned(),
            message: "配置无效，已跳过该 Server".to_owned(),
            tool: None,
        };
        runtime
            .publish_extension_candidate(
                project.path(),
                RuntimeExtensionCandidate::new(
                    1,
                    RecordingExtensionContributor::with_diagnostics(vec![diagnostic.clone()]),
                )
                .expect("候选代次应有效"),
            )
            .expect("扩展候选应发布");
        let collaboration = runtime
            .ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".to_owned(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            )
            .expect("Collaboration Runtime 应建立");
        let delivery = runtime
            .session_delivery(&session_id)
            .expect("Session 投递应可读取");
        let turn_id = keencode_agent::TurnId::new("turn-extension-diagnostics")
            .expect("测试 Turn 标识应有效");
        let root_agent = keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效");

        runtime.send_extension_diagnostics(
            &collaboration.execution,
            &delivery,
            &turn_id,
            &root_agent,
        );
        runtime.send_extension_diagnostics(
            &collaboration.execution,
            &delivery,
            &turn_id,
            &root_agent,
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        while emitter.snapshot().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let values = emitter.snapshot();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["type"], "keencode_event");
        assert_eq!(values[0]["envelope"]["sessionId"], session_id);
        assert_eq!(
            values[0]["envelope"]["turnId"],
            "turn-extension-diagnostics"
        );
        assert_eq!(values[0]["envelope"]["sourceAgentId"], "root");
        assert!(values[0]["envelope"].get("journalSequence").is_none());
        assert_eq!(
            values[0]["envelope"]["event"]["type"],
            "system_notification"
        );
        assert_eq!(values[0]["envelope"]["event"]["level"], "warning");
        assert_eq!(
            values[0]["envelope"]["event"]["message"],
            "扩展诊断：mcp Server=plugin:local:demo:docs Code=mcp_config_invalid 配置无效，已跳过该 Server"
        );

        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }

    /// 恢复期间 live 事件必须缓存，末页后只释放冻结水位之后的事件。
    #[tokio::test]
    async fn recovery_gate_emits_replay_before_newer_buffered_live_events() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter.clone(), true);
        sender
            .send_batch(vec![
                journal_text_draft("stale-live", 1),
                journal_text_draft("new-live", 2),
            ])
            .await
            .expect("恢复期间 live 事件应缓存");
        assert!(emitter.snapshot().is_empty());

        let history_delivery_sequence = sender
            .send_replay_batch(vec![journal_text_draft("replayed", 1)], 1, true)
            .await
            .expect("末页应释放恢复门");
        assert_eq!(history_delivery_sequence, 1);
        let values = emitter.snapshot();
        assert_eq!(values.len(), 2);
        assert_eq!(
            values[0]["envelope"]["update"]["content"]["text"],
            "replayed"
        );
        assert_eq!(
            values[1]["envelope"]["update"]["content"]["text"],
            "new-live"
        );
        assert_eq!(values[0]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[1]["envelope"]["deliverySequence"], 2);
    }

    /// 同步 emit 阻塞时队列满必须在有界时间内返回明确的入队超时。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delivery_queue_reserve_timeout_does_not_enqueue_or_poison_sender() {
        let emitter = BlockingEmitter::blocking_all();
        let sender = SessionDeliverySender::spawn_with_config(
            "session-delivery-queue-timeout",
            emitter.clone(),
            false,
            1,
            DeliveryTimeouts {
                queue_reserve: Duration::from_millis(20),
                acknowledgement: Duration::from_secs(1),
                shutdown: Duration::from_millis(200),
            },
        );
        let first = tokio::spawn({
            let sender = sender.clone();
            async move { sender.send_batch(vec![text_draft("first")]).await }
        });
        emitter.wait_for_calls(1);

        let second = tokio::spawn({
            let sender = sender.clone();
            async move { sender.send_batch(vec![text_draft("second")]).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while sender.commands.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("第二条命令应在泵阻塞期间占满队列");

        assert_eq!(
            sender.send_batch(vec![text_draft("third")]).await,
            Err(AgentRuntimeError::DeliveryQueueTimeout)
        );
        assert_eq!(sender.lifecycle.rejection(), None);

        emitter.release();
        assert_eq!(first.await.expect("首个发送任务不应 panic"), Ok(()));
        assert_eq!(second.await.expect("第二个发送任务不应 panic"), Ok(()));
        assert_eq!(emitter.deliveries().len(), 2);
        sender.shutdown().await.expect("队列恢复后应可关闭投递泵");
    }

    /// emit 已接受事件但回执超时后，当前世代必须冻结并保留实际结果未知语义。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delivery_ack_timeout_poisoned_sender_keeps_late_emit_unknown() {
        let emitter = BlockingEmitter::blocking_all();
        let mut timeouts = DeliveryTimeouts::test();
        timeouts.acknowledgement = Duration::from_millis(20);
        timeouts.shutdown = Duration::from_millis(40);
        let sender = SessionDeliverySender::spawn_with_config(
            "session-delivery-ack-timeout",
            emitter.clone(),
            true,
            2,
            timeouts,
        );
        let first = tokio::spawn({
            let sender = sender.clone();
            async move {
                sender
                    .send_replay_batch(vec![journal_text_draft("late", 1)], 1, true)
                    .await
            }
        });
        emitter.wait_for_calls(1);
        assert_eq!(
            first.await.expect("超时发送任务不应 panic"),
            Err(AgentRuntimeError::DeliveryOutcomeUnknown)
        );
        assert_eq!(
            sender
                .send_replay_batch(vec![journal_text_draft("after-timeout", 1)], 1, true)
                .await,
            Err(AgentRuntimeError::DeliveryOutcomeUnknown)
        );

        emitter.release();
        emitter.wait_for_completed(1);
        assert_eq!(emitter.deliveries().len(), 1);
        drop(sender);
    }

    /// 旧世代关闭结果未知时 reset 不得发布可重新编号的新投递世代。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reset_delivery_does_not_publish_generation_after_shutdown_timeout() {
        let directory = tempfile::tempdir().expect("应创建测试目录");
        let emitter = BlockingEmitter::blocking_all();
        let runtime = Arc::new(
            AgentRuntime::new_with_delivery_timeouts(
                directory.path(),
                emitter.clone(),
                DeliveryTimeouts {
                    queue_reserve: Duration::from_millis(20),
                    acknowledgement: Duration::from_secs(1),
                    shutdown: Duration::from_millis(20),
                },
            )
            .expect("测试 Runtime 应创建"),
        );
        let old = runtime
            .attach_session_delivery("session-reset-timeout")
            .expect("旧投递世代应建立");
        let sending = tokio::spawn({
            let old = old.clone();
            async move { old.send_batch(vec![text_draft("blocked")]).await }
        });
        emitter.wait_for_calls(1);

        assert!(matches!(
            runtime
                .reset_session_delivery("session-reset-timeout")
                .await,
            Err(AgentRuntimeError::DeliveryShutdownUnknown)
        ));
        let current = runtime
            .session_delivery("session-reset-timeout")
            .expect("关闭未知时旧投递世代应保留");
        assert!(current.commands.same_channel(&old.commands));
        assert_eq!(
            current.send_batch(vec![text_draft("must-not-send")]).await,
            Err(AgentRuntimeError::DeliveryShutdownUnknown)
        );

        emitter.release();
        assert_eq!(sending.await.expect("阻塞发送任务不应 panic"), Ok(()));
        drop(current);
        drop(old);
        drop(runtime);
    }

    /// 连续 replay 分页必须提交零投影物理记录的游标和 Provider 状态。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replay_advances_physical_cursor_through_zero_projection_records() {
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let project = tempfile::tempdir().expect("应创建测试项目目录");
        let emitter = RecordingEmitter::successful();
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), emitter.clone()).expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "replay-zero-projection")
            .expect("测试 Session 应创建");
        let provider = ProviderSnapshot {
            provider_id: "provider-zero-projection".to_owned(),
            model: "model-zero-projection".to_owned(),
            context_window: Some(64_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint-zero-projection".to_owned(),
            reasoning_effort: None,
        };
        session
            .set_provider_snapshot("replay-zero-provider", provider.clone())
            .expect("Provider 快照应提交");
        let provider_sequence = session
            .snapshot()
            .expect("Provider 序号应读取")
            .state
            .last_sequence;
        session
            .rename("replay-zero-rename", "重放后的标题")
            .expect("尾部投影事件应提交");
        let tail_sequence = session
            .snapshot()
            .expect("尾部序号应读取")
            .state
            .last_sequence;

        let first = runtime
            .replay_session(session.session_id().as_str(), None, 1)
            .await
            .expect("首个 replay 分页应成功");
        assert_eq!(first.replayed_events, 1);
        assert_eq!(first.through_delivery_sequence, 1);
        assert_eq!(first.next_after, provider_sequence);
        assert_eq!(first.through_journal_sequence, tail_sequence);
        assert!(first.has_more);
        let delivery = runtime
            .session_delivery(session.session_id().as_str())
            .expect("当前 replay 投递应存在");
        {
            let cursor = delivery.replay_cursor.lock().await;
            assert_eq!(cursor.next_after, provider_sequence);
            assert_eq!(cursor.provider, Some(provider.clone()));
            assert_eq!(cursor.through_sequence, Some(tail_sequence));
            assert_eq!(
                cursor.frozen_state.as_ref().unwrap().last_sequence,
                tail_sequence,
            );
        }

        // 第一页发出后追加的状态不能改变第二页的历史快照和固定水位。
        session
            .rename("replay-zero-after-waterline", "水位之后的新标题")
            .expect("分页之间的实时记录应能提交");
        let last = runtime
            .replay_session(session.session_id().as_str(), Some(first.next_after), 1)
            .await
            .expect("连续 replay 尾页应成功");
        assert_eq!(last.next_after, tail_sequence);
        assert_eq!(last.through_journal_sequence, tail_sequence);
        assert_eq!(last.through_delivery_sequence, 2);
        assert!(!last.has_more);
        assert!(delivery.replay_cursor.lock().await.frozen_state.is_none());
        runtime
            .close_session_delivery(session.session_id().as_str())
            .await
            .expect("测试 replay 投递应关闭");
    }

    /// 非连续 after 必须从请求水位前重建 Provider，而不是沿用当前分页缓存。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replay_non_contiguous_after_rebuilds_provider_cursor() {
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let project = tempfile::tempdir().expect("应创建测试项目目录");
        let emitter = RecordingEmitter::successful();
        let runtime =
            Arc::new(AgentRuntime::new(storage.path(), emitter).expect("测试 Runtime 应创建"));
        let session = runtime
            .open_or_create_session(project.path(), None, "replay-non-contiguous")
            .expect("测试 Session 应创建");
        let provider_a = ProviderSnapshot {
            provider_id: "provider-replay-a".to_owned(),
            model: "model-replay-a".to_owned(),
            context_window: Some(128_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint-replay-a".to_owned(),
            reasoning_effort: None,
        };
        let provider_b = ProviderSnapshot {
            provider_id: "provider-replay-b".to_owned(),
            model: "model-replay-b".to_owned(),
            context_window: Some(32_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint-replay-b".to_owned(),
            reasoning_effort: None,
        };
        session
            .set_provider_snapshot("replay-non-contiguous-a", provider_a.clone())
            .expect("Provider A 快照应提交");
        let provider_a_sequence = session
            .snapshot()
            .expect("Provider A 序号应读取")
            .state
            .last_sequence;
        session
            .rename("replay-non-contiguous-rename-a", "历史标题 A")
            .expect("中间投影事件 A 应提交");
        session
            .rename("replay-non-contiguous-rename-a-2", "历史标题 A-2")
            .expect("第二个中间投影事件 A 应提交");
        session
            .set_provider_snapshot("replay-non-contiguous-b", provider_b.clone())
            .expect("Provider B 快照应提交");
        let provider_b_sequence = session
            .snapshot()
            .expect("Provider B 序号应读取")
            .state
            .last_sequence;
        session
            .rename("replay-non-contiguous-rename-b", "历史标题 B")
            .expect("尾部投影事件 B 应提交");

        let first = runtime
            .replay_session(session.session_id().as_str(), None, 3)
            .await
            .expect("首个 replay 分页应成功");
        assert_eq!(first.next_after, provider_b_sequence);
        assert!(first.has_more);
        let jumped = runtime
            .replay_session(session.session_id().as_str(), Some(provider_a_sequence), 1)
            .await
            .expect("非连续 replay 分页应成功");
        assert_eq!(jumped.next_after, provider_a_sequence + 1);
        let delivery = runtime
            .session_delivery(session.session_id().as_str())
            .expect("当前 replay 投递应存在");
        assert_eq!(
            delivery.replay_cursor.lock().await.provider,
            Some(provider_a)
        );
        runtime
            .close_session_delivery(session.session_id().as_str())
            .await
            .expect("测试 replay 投递应关闭");
    }

    /// 重复 ensure 必须复用唯一投递 FIFO 和唯一 Runtime 订阅泵。
    #[tokio::test]
    async fn ensure_session_delivery_is_idempotent() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "delivery-operation")
            .expect("测试 Session 应创建");
        let first = runtime
            .ensure_session_delivery(session.session_id().as_str())
            .expect("首次 ensure 应建立投递");
        let second = runtime
            .ensure_session_delivery(session.session_id().as_str())
            .expect("重复 ensure 应复用投递");
        assert!(first.commands.same_channel(&second.commands));
        assert_eq!(
            runtime.live_pumps.lock().expect("订阅泵状态应读取").len(),
            1
        );
        runtime
            .close_session_delivery(session.session_id().as_str())
            .await
            .expect("投递应关闭");
    }

    /// 权威 TurnStarted 屏障必须可观察，且相同 Turn 输入只能返回去重结果。
    #[tokio::test]
    async fn turn_started_barrier_and_root_turn_retry_are_deterministic() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
                        developer_context: None,
                        plan_enabled: true,
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
                        developer_context: Some("重新抽取的动态记忆".to_owned()),
                        plan_enabled: false,
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
                        developer_context: None,
                        plan_enabled: true,
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

    /// 配置变更必须在模型请求前拒绝；用户显式重选才更新快照，旧操作重试不能替代重选。
    #[tokio::test]
    async fn session_provider_connection_change_requires_explicit_reselection() {
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

        let error = runtime
            .start_root_turn(
                session_id,
                "blocked-provider-change-turn",
                "不应发送模型请求",
                RootTurnOptions::default(),
            )
            .await
            .expect_err("旧绑定不得用于新的连接配置");
        let rejected = session.snapshot().expect("应读取拒绝后的状态");
        assert!(
            rejected.state.turns.is_empty(),
            "拒绝前不能产生 TurnStarted"
        );
        assert_eq!(rejected.state.provider.as_ref(), Some(&original));
        runtime
            .set_session_model(
                session_id,
                "original-selection",
                "provider-runtime-test",
                "model-a",
            )
            .expect("旧操作重试应返回原收据");
        assert_eq!(
            session.snapshot().unwrap().state.provider.as_ref(),
            Some(&original)
        );

        let reselected = runtime
            .set_session_model(
                session_id,
                "explicit-reselection",
                "provider-runtime-test",
                "model-a",
            )
            .expect("用户显式重选应绑定当前连接配置")
            .state
            .provider
            .expect("应存在新绑定");
        assert_ne!(reselected.config_fingerprint, original.config_fingerprint);
        assert!(runtime.resolve_session_provider(Some(&reselected)).is_ok());
        assert_eq!(error, AgentRuntimeError::ProviderConfigurationChanged);
        assert_eq!(
            error.to_string(),
            "Session 模型连接配置已改变，请重新选择模型"
        );
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
            .close_session_delivery(&session_id)
            .await
            .expect("关闭 Session 投递应同时释放标题锁");
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
            let (base_url, server) = spawn_buffered_responses_server(text);
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
            let request = finish_responses_server(server);
            assert_eq!(request["tool_choice"], "none");
            assert_eq!(request["text"]["format"]["type"], "json_schema");
            assert_eq!(request["text"]["format"]["strict"], true);
            assert_eq!(request["text"]["format"]["schema"], schema);
        }
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
            let (base_url, server) = spawn_buffered_responses_body(json!({
                "id": "response-runtime-test", "object": "response", "model": "test-model",
                "status": "completed", "output": output,
                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
            }));
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
            let request = finish_responses_server(server);
            assert_eq!(request["tool_choice"], "required");
            assert_eq!(request["parallel_tool_calls"], false);
            let input = request["input"].as_array().expect("应编码输入消息");
            assert_eq!(input.len(), 2, "隔离请求只有一条系统消息和一条用户消息");
            assert_eq!(input[0]["role"], "system");
            let instructions = input[0]["content"][0]["text"].as_str().unwrap();
            assert!(instructions.starts_with("通过结果通道提交合成事实"));
            assert!(instructions.contains("唯一结果工具的 value 字段"));
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
        runtime
            .close_session_delivery(&session_id)
            .await
            .expect("推理测试投递应关闭");
    }

    /// 后台 Shell 终态必须成为 Session 级瞬态事件，且可疑摘要不得进入桌面边界。
    #[test]
    fn background_completion_is_session_scoped_and_redacts_suspicious_summary() {
        let completed = background_task_completion_event(&BackgroundTaskCompletion {
            session_id: "session-a".to_owned(),
            task_id: "background-1".to_owned(),
            status: BackgroundTaskStatus::Succeeded,
            duration_ms: 25,
            summary: "运行检查".to_owned(),
        })
        .expect("合法后台终态应映射");
        assert_eq!(
            completed,
            KeenCodeEvent::BackgroundTaskCompleted {
                task_id: "background-1".to_owned(),
                task_kind: BackgroundTaskKind::Shell,
                agent_id: None,
                status: BackgroundTaskTerminalStatus::Succeeded,
                duration_ms: 25,
                summary: Some("运行检查".to_owned()),
            }
        );

        let redacted = background_task_completion_event(&BackgroundTaskCompletion {
            session_id: "session-a".to_owned(),
            task_id: "background-2".to_owned(),
            status: BackgroundTaskStatus::Failed,
            duration_ms: 30,
            summary: "authorization: secret".to_owned(),
        })
        .expect("可疑摘要应被省略而不是毒化投递");
        assert!(matches!(
            redacted,
            KeenCodeEvent::BackgroundTaskCompleted {
                task_kind: BackgroundTaskKind::Shell,
                agent_id: None,
                status: BackgroundTaskTerminalStatus::Failed,
                summary: None,
                ..
            }
        ));
        assert!(
            background_task_completion_event(&BackgroundTaskCompletion {
                session_id: "session-a".to_owned(),
                task_id: "background-3".to_owned(),
                status: BackgroundTaskStatus::Running,
                duration_ms: 1,
                summary: String::new(),
            })
            .is_none()
        );
    }

    /// 编辑锚点必须通过官方 ContentChunk 的命名空间 meta 投递，而不是伪造顶级字段。
    #[test]
    fn persisted_message_identity_uses_standard_metadata_slot() {
        let storage = tempfile::tempdir().expect("测试目录应创建");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "message-anchor-projection".to_owned(),
                title: "消息锚点".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("测试 Session 应创建");
        let state = session.snapshot().unwrap().state;
        let message = keencode_resources::SessionMessage {
            message_id: "resource-message-anchor".to_owned(),
            turn_id: None,
            agent_id: None,
            role: keencode_resources::MessageRole::User,
            content: vec![keencode_resources::MessagePart::Text {
                text: "同一条用户消息".to_owned(),
            }],
        };
        let record = SessionEventRecord {
            schema: SESSION_EVENT_SCHEMA.to_owned(),
            version: SESSION_EVENT_VERSION,
            event_id: SessionEventId::new("message-anchor-event").unwrap(),
            session: state.session_id.clone(),
            sequence: 2,
            time_unix_ms: 2,
            event: SessionEvent::MessageAdded { message },
        };
        for mode in [
            AuthoritativeProjectionMode::Live,
            AuthoritativeProjectionMode::Replay,
        ] {
            let drafts = map_authoritative_record(&session, &state, &record, mode).unwrap();
            assert_eq!(drafts.len(), 1);
            let value = serde_json::to_value(
                materialize_delivery(
                    session.session_id().as_str(),
                    1,
                    drafts.into_iter().next().unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            let update = &value["envelope"]["update"];
            assert_eq!(
                update["_meta"]["keencode/messageId"],
                "resource-message-anchor"
            );
            assert!(update.get("messageId").is_none());
        }
    }

    /// 模型拒答和输出上限必须在实时和历史投影中保持模型失败，不伪装成内部错误。
    #[test]
    fn model_stop_projection_preserves_failure_category() {
        let storage = tempfile::tempdir().expect("测试目录应创建");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "model-stop-projection".to_owned(),
                title: "模型停止投影".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("测试 Session 应创建");
        for reason in [
            TurnStopReason::ModelOutputLimit,
            TurnStopReason::ModelRefusal,
        ] {
            let turn_id = ResourceTurnId::new("model-stop-turn").unwrap();
            let mut state = session.snapshot().unwrap().state;
            state.turns.insert(
                turn_id.clone(),
                TurnState {
                    turn_id: turn_id.clone(),
                    source_agent_id: ResourceAgentId::new("root").unwrap(),
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: None,
                    prompt_summary: "验证模型停止".to_owned(),
                    started_at_unix_ms: 1,
                    completed_at_unix_ms: Some(2),
                    status: TurnStatus::Failed,
                    stop_reason: Some(reason),
                    outcome_message: Some("模型没有完整完成".to_owned()),
                },
            );
            let record = SessionEventRecord {
                schema: SESSION_EVENT_SCHEMA.to_owned(),
                version: SESSION_EVENT_VERSION,
                event_id: SessionEventId::new("model-stop-event").unwrap(),
                session: state.session_id.clone(),
                sequence: 2,
                time_unix_ms: 2,
                event: SessionEvent::TurnStopped {
                    turn_id,
                    reason,
                    message: "模型没有完整完成".to_owned(),
                },
            };
            for mode in [
                AuthoritativeProjectionMode::Live,
                AuthoritativeProjectionMode::Replay,
            ] {
                let drafts = map_authoritative_record(&session, &state, &record, mode)
                    .expect("模型终态应投影");
                assert_eq!(drafts.len(), 1);
                let delivery = materialize_delivery(
                    session.session_id().as_str(),
                    1,
                    drafts.into_iter().next().unwrap(),
                )
                .expect("投递应编码");
                let json = serde_json::to_value(delivery).expect("投递应序列化");
                let event = &json["envelope"]["event"];
                assert_eq!(event["type"], "turn_failed");
                assert_eq!(event["failureKind"], "model");
                assert_eq!(event["message"], "模型没有完整完成");
            }
        }
    }

    /// 子 Agent 各种终态必须投影为无 Journal 游标的 Session 级完成通知。
    #[test]
    fn background_agent_completion_is_safe_and_live_replay_stable() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "agent-completion-projection".to_owned(),
                title: "子 Agent 完成投影".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("测试 Session 应创建");
        let session_id = ResourceSessionId::new(session.session_id().as_str().to_owned())
            .expect("资源层 Session 标识应有效");
        let root_turn_id =
            ResourceTurnId::new("turn-agent-completion-root").expect("根 Turn 标识应有效");
        let cases = [
            (
                SubAgentStatus::Completed,
                TurnStatus::Completed,
                None,
                BackgroundTaskTerminalStatus::Succeeded,
                Some("完成摘要"),
            ),
            (
                SubAgentStatus::Failed,
                TurnStatus::Failed,
                Some(TurnStopReason::Failed),
                BackgroundTaskTerminalStatus::Failed,
                Some("authorization: secret"),
            ),
            (
                SubAgentStatus::Interrupted,
                TurnStatus::Cancelled,
                Some(TurnStopReason::Cancelled),
                BackgroundTaskTerminalStatus::Cancelled,
                None,
            ),
            (
                SubAgentStatus::Stopped,
                TurnStatus::Cancelled,
                Some(TurnStopReason::Cancelled),
                BackgroundTaskTerminalStatus::Cancelled,
                Some("已停止"),
            ),
        ];

        for (index, (agent_status, turn_status, stop_reason, expected, summary)) in
            cases.into_iter().enumerate()
        {
            let agent_id = ResourceAgentId::new(format!("agent-completion-{index}"))
                .expect("子 Agent 标识应有效");
            let turn_id = ResourceTurnId::new(format!("turn-agent-completion-{index}"))
                .expect("子 Turn 标识应有效");
            let mut state = SessionState::empty(session_id.clone());
            state.turns.insert(
                turn_id.clone(),
                TurnState {
                    turn_id: turn_id.clone(),
                    source_agent_id: agent_id.clone(),
                    root_turn_id: root_turn_id.clone(),
                    parent_turn_id: Some(root_turn_id.clone()),
                    prompt_summary: "执行子任务".to_owned(),
                    started_at_unix_ms: 1_000,
                    completed_at_unix_ms: Some(1_250),
                    status: turn_status,
                    stop_reason,
                    outcome_message: None,
                },
            );
            let record = SessionEventRecord {
                schema: SESSION_EVENT_SCHEMA.to_owned(),
                version: SESSION_EVENT_VERSION,
                event_id: SessionEventId::new(format!("agent-completion-event-{index}"))
                    .expect("事件标识应有效"),
                session: session_id.clone(),
                sequence: u64::try_from(index).expect("索引应转换").saturating_add(2),
                time_unix_ms: 1_300,
                event: SessionEvent::SubAgentStatusChanged {
                    agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    status: agent_status,
                    result_summary: summary.map(str::to_owned),
                },
            };
            let live = map_authoritative_record(
                &session,
                &state,
                &record,
                AuthoritativeProjectionMode::Live,
            )
            .expect("live 子 Agent 终态应投影");
            let replay = map_authoritative_record(
                &session,
                &state,
                &record,
                AuthoritativeProjectionMode::Replay,
            )
            .expect("replay 子 Agent 终态应投影");
            assert_eq!(live.len(), 2);
            assert_eq!(replay.len(), 2);
            let live = live
                .into_iter()
                .enumerate()
                .map(|(offset, draft)| {
                    serde_json::to_value(
                        materialize_delivery(
                            session.session_id().as_str(),
                            u64::try_from(offset).expect("偏移应转换").saturating_add(1),
                            draft,
                        )
                        .expect("live 完成信封应构造"),
                    )
                    .expect("live 完成信封应序列化")
                })
                .collect::<Vec<_>>();
            let replay = replay
                .into_iter()
                .enumerate()
                .map(|(offset, draft)| {
                    serde_json::to_value(
                        materialize_delivery(
                            session.session_id().as_str(),
                            u64::try_from(offset).expect("偏移应转换").saturating_add(1),
                            draft,
                        )
                        .expect("replay 完成信封应构造"),
                    )
                    .expect("replay 完成信封应序列化")
                })
                .collect::<Vec<_>>();
            assert_eq!(live, replay);
            let completion = &live[1]["envelope"];
            assert!(completion.get("journalSequence").is_none());
            assert!(completion.get("turnId").is_none());
            assert!(completion.get("sourceAgentId").is_none());
            assert_eq!(completion["event"]["taskId"], turn_id.as_str());
            assert_eq!(completion["event"]["agentId"], agent_id.as_str());
            assert_eq!(completion["event"]["durationMs"], 250);
            assert_eq!(
                completion["event"]["status"],
                serde_json::to_value(expected).expect("终态应序列化")
            );
            if index == 1 {
                assert!(completion["event"].get("summary").is_none());
            }
        }
    }

    /// Todo 权威替换必须生成无伪造 Turn/Agent 身份的 Session 级 ACP Plan。
    #[test]
    fn todo_replacement_projects_as_session_scoped_plan() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = AgentRuntime::new(storage.path(), RecordingEmitter::successful())
            .expect("测试 Runtime 应创建");
        let session = runtime
            .open_or_create_session(project.path(), None, "todo-projection-operation")
            .expect("测试 Session 应创建");
        let session_id = ResourceSessionId::new(session.session_id().as_str().to_owned())
            .expect("Session 标识应有效");
        let record = SessionEventRecord {
            schema: SESSION_EVENT_SCHEMA.to_owned(),
            version: SESSION_EVENT_VERSION,
            event_id: SessionEventId::new("todo-projection-event").expect("事件标识应有效"),
            session: session_id.clone(),
            sequence: 2,
            time_unix_ms: 2,
            event: SessionEvent::TodoReplaced {
                items: vec![
                    TodoItem {
                        content: "检查实现".to_owned(),
                        status: TodoStatus::InProgress,
                        active_form: "正在检查实现".to_owned(),
                    },
                    TodoItem {
                        content: "运行验证".to_owned(),
                        status: TodoStatus::Pending,
                        active_form: "正在运行验证".to_owned(),
                    },
                ],
                operation_payload_sha256: "0".repeat(64),
                revision: 1,
            },
        };
        let drafts = map_authoritative_record(
            &session,
            &SessionState::empty(session_id),
            &record,
            AuthoritativeProjectionMode::Live,
        )
        .expect("Todo 应投影");
        assert_eq!(drafts.len(), 1);
        let delivery = materialize_delivery(
            session.session_id().as_str(),
            1,
            drafts.into_iter().next().expect("应存在 Plan 草稿"),
        )
        .expect("Session 级 Plan 信封应通过校验");
        let value = serde_json::to_value(delivery).expect("Plan 投递应序列化");
        let envelope = &value["envelope"];
        assert!(envelope.get("turnId").is_none());
        assert!(envelope.get("sourceAgentId").is_none());
        assert_eq!(envelope["update"]["sessionUpdate"], "plan");
        assert_eq!(envelope["update"]["entries"][0]["status"], "in_progress");
        assert_eq!(envelope["update"]["entries"][1]["status"], "pending");
        assert_eq!(envelope["update"]["_meta"]["_keencode"]["todoRevision"], 1);
    }

    /// 完整关闭必须先拆除 Collaboration 与完成泵，并在句柄释放后允许重开同一 Session。
    #[tokio::test]
    async fn close_session_releases_collaboration_runtime_and_delivery() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "close-session-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .ensure_session_delivery(&session_id)
            .expect("Session 投递应建立");
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
        assert!(runtime.session_delivery(&session_id).is_err());
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

    /// 协调器关闭因 Store 冻结失败时，仍必须清理完成泵、投递和 Runtime 注册。
    #[tokio::test]
    async fn close_session_cleans_resources_after_collaboration_recovery_failure() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "close-recovery-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        runtime
            .ensure_session_delivery(&session_id)
            .expect("Session 投递应建立");
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
        let transition_path = storage
            .path()
            .join("sessions")
            .join(&session_id)
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
        assert!(runtime.session_delivery(&session_id).is_err());
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

    /// Session load 替换世代后首条更新必须重新从一开始。
    #[tokio::test]
    async fn reset_delivery_starts_new_generation_at_one() {
        let directory = tempfile::tempdir().expect("应创建测试目录");
        let emitter = RecordingEmitter::successful();
        let runtime =
            AgentRuntime::new(directory.path(), emitter.clone()).expect("测试 Runtime 应创建");
        let first = runtime
            .attach_session_delivery("session-a")
            .expect("首个世代应建立");
        first
            .send_batch(vec![text_draft("one"), text_draft("two")])
            .await
            .expect("首个世代应发送");
        let second = runtime
            .reset_session_delivery("session-a")
            .await
            .expect("新世代应建立");
        second
            .send_replay_batch(vec![text_draft("replayed")], 0, true)
            .await
            .expect("新世代应发送");
        let values = emitter.snapshot();
        assert_eq!(values[0]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[1]["envelope"]["deliverySequence"], 2);
        assert_eq!(values[2]["envelope"]["deliverySequence"], 1);
    }

    /// Lag 信号后的旧世代在途事件不得污染新世代 replay，恢复门还必须保留较新的 live。
    #[tokio::test]
    async fn lag_recovery_resets_generation_and_releases_buffered_live_in_order() {
        let directory = tempfile::tempdir().expect("应创建测试目录");
        let emitter = RecordingEmitter::successful();
        let runtime =
            AgentRuntime::new(directory.path(), emitter.clone()).expect("测试 Runtime 应创建");
        let old_generation = runtime
            .attach_session_delivery("session-a")
            .expect("旧投递世代应建立");
        old_generation
            .send_batch(vec![DeliveryDraft::KeenCodeEvent {
                turn_id: None,
                source_agent_id: None,
                journal_sequence: None,
                occurred_at_ms: 1,
                event: KeenCodeEvent::RecoveryStateChanged {
                    state: keencode_acp::RecoveryState::Replaying,
                },
            }])
            .await
            .expect("Lag 恢复信号应发送");
        old_generation
            .send_batch(vec![journal_text_draft("old-in-flight", 1)])
            .await
            .expect("首次 replay 前的旧世代在途事件应可完成");

        let recovery_generation = runtime
            .reset_session_delivery("session-a")
            .await
            .expect("首次 replay 应建立恢复世代");
        recovery_generation
            .send_batch(vec![journal_text_draft("new-live", 2)])
            .await
            .expect("新世代 live 应进入恢复缓存");
        recovery_generation
            .send_replay_batch(vec![journal_text_draft("replayed", 1)], 1, true)
            .await
            .expect("末页应先 replay 再释放较新的 live");

        let values = emitter.snapshot();
        assert_eq!(values.len(), 4);
        assert_eq!(values[0]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[1]["envelope"]["deliverySequence"], 2);
        assert_eq!(values[2]["envelope"]["deliverySequence"], 1);
        assert_eq!(values[3]["envelope"]["deliverySequence"], 2);
        assert_eq!(
            values[2]["envelope"]["update"]["content"]["text"],
            "replayed"
        );
        assert_eq!(
            values[3]["envelope"]["update"]["content"]["text"],
            "new-live"
        );
    }

    /// 重复 shutdown 不得失败或恢复接受新投递。
    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let emitter = RecordingEmitter::successful();
        let sender = SessionDeliverySender::spawn("session-a", emitter, false);
        sender.shutdown().await.expect("首次关闭应成功");
        sender.shutdown().await.expect("重复关闭应成功");
        assert_eq!(
            sender.send_batch(vec![text_draft("late")]).await,
            Err(AgentRuntimeError::DeliveryClosed)
        );
    }

    /// 停机取消后的执行回收必须仍能在调用 shutdown 的同一个 Tokio worker 上推进。
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_yields_to_cancelled_runner_cleanup_on_same_worker() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "shutdown-same-worker")
            .expect("测试 Session 应创建");
        runtime
            .attach_session_delivery(session.session_id().as_str())
            .expect("测试 Session 投递应建立");
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

    /// shutdown Future 在清理中途被取消后，后续调用不得把未完成清理报告为成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_remains_failed_closed_on_retry() {
        let directory = tempfile::tempdir().expect("应创建测试存储目录");
        let emitter = BlockingEmitter::blocking_all();
        let runtime = Arc::new(
            AgentRuntime::new_with_delivery_timeouts(
                directory.path(),
                emitter.clone(),
                DeliveryTimeouts {
                    queue_reserve: Duration::from_millis(20),
                    acknowledgement: Duration::from_secs(1),
                    shutdown: Duration::from_secs(1),
                },
            )
            .expect("测试 Runtime 应创建"),
        );
        let delivery = runtime
            .attach_session_delivery("shutdown-cancelled")
            .expect("投递世代应建立");
        let sending = tokio::spawn({
            let delivery = delivery.clone();
            async move { delivery.send_batch(vec![text_draft("阻塞关闭")]).await }
        });
        emitter.wait_for_calls(1);

        let shutdown = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.shutdown().await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !runtime.closed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown 应先公开关闭状态");
        shutdown.abort();
        assert!(
            shutdown
                .await
                .expect_err("取消的 shutdown 任务应返回取消")
                .is_cancelled()
        );

        assert_eq!(
            runtime.shutdown().await,
            Err(AgentRuntimeError::RuntimeOperationFailed),
            "取消的清理不能在重入时伪装成功"
        );

        emitter.release();
        assert_eq!(sending.await.expect("阻塞投递任务不应 panic"), Ok(()));
        drop(delivery);
        drop(runtime);
    }

    /// 标准信封自身仍必须保留严格 Session/Turn/Agent 身份。
    #[test]
    fn generated_standard_envelope_remains_strict() {
        let envelope = SessionUpdateDeliveryEnvelope::new(
            "session-a",
            Some("turn-a".to_owned()),
            Some("agent-root".to_owned()),
            1,
            1,
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from("hello"))),
        )
        .expect("测试信封应有效");
        assert_eq!(envelope.delivery_sequence(), 1);
        assert_eq!(envelope.turn_id(), Some("turn-a"));
    }

    /// 模型 Round 用量只有在上下文窗口和完整 Token 统计都明确时才投影。
    #[test]
    fn model_round_usage_projection_is_known_only_and_replay_stable() {
        let storage = tempfile::tempdir().expect("应创建测试存储目录");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "runtime-usage-projection".to_owned(),
                title: "用量投影测试".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("测试 Session 应创建");
        let session_id =
            ResourceSessionId::new(session.session_id().as_str()).expect("资源 Session 标识应有效");
        let turn_id = ResourceTurnId::new("turn-usage-projection").expect("Turn 标识应有效");
        let source_agent_id = ResourceAgentId::new("root").expect("来源 Agent 标识应有效");
        let mut state = SessionState::empty(session_id.clone());
        state.provider = Some(ProviderSnapshot {
            provider_id: "provider-a".to_owned(),
            model: "test-model".to_owned(),
            context_window: Some(128_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint".to_owned(),
            reasoning_effort: None,
        });
        let make_record = |usage: TokenUsage| SessionEventRecord {
            schema: SESSION_EVENT_SCHEMA.to_owned(),
            version: SESSION_EVENT_VERSION,
            event_id: SessionEventId::new("usage-projection-event").expect("事件标识应有效"),
            session: session_id.clone(),
            sequence: 7,
            time_unix_ms: 7,
            event: SessionEvent::ModelRoundCompleted {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round: 1,
                requested_model: "test-model".to_owned(),
                metadata: ResponseMetadata::default(),
                usage,
                stop_reason: StopReason::Completed,
            },
        };

        let total_record = make_record(TokenUsage {
            input_tokens: Some(90),
            output_tokens: Some(20),
            reasoning_tokens: Some(3),
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(100),
        });
        let live = map_authoritative_record(
            &session,
            &state,
            &total_record,
            AuthoritativeProjectionMode::Live,
        )
        .expect("已知总用量应投影");
        let replay = map_authoritative_record(
            &session,
            &state,
            &total_record,
            AuthoritativeProjectionMode::Replay,
        )
        .expect("已知总用量应重放");
        assert_eq!(live.len(), 1);
        assert_eq!(replay.len(), 1);
        let live_value = serde_json::to_value(
            materialize_delivery(
                session.session_id().as_str(),
                1,
                live.into_iter().next().expect("live 应有用量更新"),
            )
            .expect("live 用量信封应构造"),
        )
        .expect("live 用量信封应序列化");
        let replay_value = serde_json::to_value(
            materialize_delivery(
                session.session_id().as_str(),
                1,
                replay.into_iter().next().expect("replay 应有用量更新"),
            )
            .expect("replay 用量信封应构造"),
        )
        .expect("replay 用量信封应序列化");
        assert_eq!(live_value, replay_value);
        assert_eq!(
            live_value["envelope"]["update"]["sessionUpdate"],
            "usage_update"
        );
        assert_eq!(live_value["envelope"]["update"]["used"], 100);
        assert_eq!(live_value["envelope"]["update"]["size"], 128_000);

        let summed = map_authoritative_record(
            &session,
            &state,
            &make_record(TokenUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                ..TokenUsage::unknown()
            }),
            AuthoritativeProjectionMode::Live,
        )
        .expect("输入和输出均明确时应投影");
        assert_eq!(summed.len(), 1);
        let summed_value = serde_json::to_value(
            materialize_delivery(
                session.session_id().as_str(),
                1,
                summed.into_iter().next().expect("求和用量应存在"),
            )
            .expect("求和用量信封应构造"),
        )
        .expect("求和用量信封应序列化");
        assert_eq!(summed_value["envelope"]["update"]["used"], 18);

        assert!(
            map_authoritative_record(
                &session,
                &state,
                &make_record(TokenUsage::unknown()),
                AuthoritativeProjectionMode::Live,
            )
            .expect("未知用量不应造成投影错误")
            .is_empty()
        );
        let mut no_context_state = state.clone();
        no_context_state
            .provider
            .as_mut()
            .expect("测试 Provider 快照应存在")
            .context_window = None;
        assert!(
            map_authoritative_record(
                &session,
                &no_context_state,
                &total_record,
                AuthoritativeProjectionMode::Replay,
            )
            .expect("未知上下文窗口不应造成投影错误")
            .is_empty()
        );
    }

    /// 分页 replay 必须按每个历史模型 Round 当时的 Provider 快照投影上下文窗口。
    #[tokio::test(flavor = "multi_thread")]
    async fn replay_uses_provider_snapshot_at_each_historical_round() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let emitter = RecordingEmitter::successful();
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), emitter.clone()).expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "replay-provider-history-operation")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let provider_a = ProviderSnapshot {
            provider_id: "provider-a".to_owned(),
            model: "model-a".to_owned(),
            context_window: Some(128_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint-a".to_owned(),
            reasoning_effort: None,
        };
        let provider_b = ProviderSnapshot {
            provider_id: "provider-b".to_owned(),
            model: "model-b".to_owned(),
            context_window: Some(32_000),
            protocol: ProviderProtocolSnapshot::OpenAiChatCompletions,
            config_fingerprint: "fingerprint-b".to_owned(),
            reasoning_effort: None,
        };

        let provider_a_sequence = session
            .set_provider_snapshot("replay-provider-a", provider_a)
            .expect("历史 Provider A 应写入")
            .last_sequence;
        persist_usage_root_turn(&session, "replay-turn-a", "model-a", "历史模型 A", 11).await;
        session
            .set_provider_snapshot("replay-provider-b", provider_b)
            .expect("历史 Provider B 应写入");
        persist_usage_root_turn(&session, "replay-turn-b", "model-b", "历史模型 B", 13).await;

        // 第一页消费首条 SessionCreated 及其后的零投影 Provider 快照，第二页从该物理水位开始。
        let first_page = runtime
            .replay_session(&session_id, None, 1)
            .await
            .expect("首个 replay 分页应成功");
        assert_eq!(first_page.start_after, 0);
        assert_eq!(first_page.next_after, provider_a_sequence);
        assert!(first_page.has_more);
        runtime
            .replay_session(&session_id, Some(first_page.next_after), 1_000)
            .await
            .expect("后续 replay 分页应成功");

        let usage = emitter
            .snapshot()
            .into_iter()
            .filter_map(|value| {
                (value["type"] == "session_update"
                    && value["envelope"]["update"]["sessionUpdate"] == "usage_update")
                    .then(|| {
                        (
                            value["envelope"]["update"]["used"].as_u64(),
                            value["envelope"]["update"]["size"].as_u64(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            usage,
            vec![(Some(11), Some(128_000)), (Some(13), Some(32_000))]
        );
        runtime
            .close_session_delivery(&session_id)
            .await
            .expect("测试 replay 投递应关闭");
    }

    /// 真实 Goal Sink 必须区分失败调用与同 Round 重试，并只把成功响应写入 Transcript。
    #[tokio::test]
    async fn goal_usage_sink_distinguishes_failed_round_retry_attempts() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "runtime-goal-usage-retry".to_owned(),
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
            keencode_agent::TurnId::new("turn-goal-usage-retry").expect("Agent Turn ID 应有效"),
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
        assert_eq!(goal.time_used_seconds, 3);

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
            runtime.background_task_cancel(&session_id, first.turn_id.as_str()),
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
            .background_task_cancel(&session_id, first.turn_id.as_str())
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
            .background_task_cancel(&session_id, second.turn_id.as_str())
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
            runtime.background_task_cancel(&session_id, first.turn_id.as_str()),
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
            runtime.background_task_cancel(&session_id, first.turn_id.as_str()),
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

        let recovered_runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("冷恢复 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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

        let collaboration =
            install_test_collaboration_runtime_with_limit(&runtime, &session, project.path(), 1);
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

        let recovered_runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("冷恢复 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let collaboration = install_test_collaboration_runtime(&runtime, &session, project.path());
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
            .background_task_cancel(
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
            .background_task_cancel(
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
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
        let collaboration =
            install_test_collaboration_runtime_with_limit(&runtime, &session, project.path(), 1);
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
    async fn cold_replay_preserves_reasoning_tool_reasoning_text_order_without_duplicates() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        std::fs::write(project.path().join("replay-input.txt"), "冷恢复测试文件")
            .expect("测试输入文件应写入");

        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), RecordingEmitter::successful())
                .expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "cold-replay-order")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let turn_id = "turn-cold-replay-order";
        let input = ModelMessage::text(MessageRole::User, "读取文件并总结");
        let first_reply = ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "thought1".to_owned(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: "call-replay".to_owned(),
                name: "Read".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: "call-replay".to_owned(),
                delta: json!({"file_path": "replay-input.txt"}).to_string(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: "call-replay".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ]);
        let second_reply = ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "thought2".to_owned(),
            },
            ModelStreamEvent::TextDelta {
                index: 1,
                delta: "text".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ]);
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                ..ProviderCapabilities::default()
            },
            [first_reply, second_reply],
        ));
        let provider_for_assertions = provider.clone();
        let environment = Arc::new(
            keencode_tools::ToolEnvironment::new(project.path()).expect("测试工具环境应创建"),
        );
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(keencode_tools::ReadTool::new(environment)))
            .expect("Read 工具应注册");
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session 标识应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn 标识应有效"),
            keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效"),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let runner =
            session.bind_agent_runner(AgentRunner::new(provider, tools, RunLimits::default()));
        let result = runner
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                "冷恢复顺序测试",
            ))
            .await
            .expect("测试 Turn 应完成");
        assert!(result.is_success(), "合成工具轮次应成功：{result:?}");
        assert_eq!(result.state.round_count(), 2);
        assert_eq!(result.state.step_count(), 1);
        assert_eq!(
            result.state.terminal_reason(),
            Some(keencode_agent::TerminalReason::Completed)
        );
        assert_eq!(
            provider_for_assertions
                .requests()
                .expect("Provider 请求应读取")
                .len(),
            2
        );
        drop(runner);

        // 关闭旧 Manager 持有的 Session，释放 lease 后再打开同一 Session，确保下面是真实冷恢复。
        runtime
            .runtime_manager
            .close(session_id.clone())
            .expect("旧 Runtime Session 应关闭");
        drop(session);
        drop(runtime);

        let replay_emitter = RecordingEmitter::successful();
        let recovered_runtime = Arc::new(
            AgentRuntime::new(storage.path(), replay_emitter.clone())
                .expect("冷恢复 Runtime 应创建"),
        );
        let reopened = recovered_runtime
            .open_or_create_session(project.path(), Some(&session_id), "unused")
            .expect("冷恢复 Session 应打开");
        let cold_snapshot = reopened.snapshot().expect("冷恢复快照应读取");
        let resource_turn_id = ResourceTurnId::new(turn_id).expect("资源 Turn 标识应有效");
        let cold_turn = cold_snapshot
            .state
            .turns
            .get(&resource_turn_id)
            .expect("冷恢复 Turn 应存在");
        assert_eq!(cold_turn.status, TurnStatus::Completed);
        assert_eq!(cold_turn.source_agent_id.as_str(), "root");

        recovered_runtime
            .replay_session(&session_id, None, keencode_acp::MAX_REPLAY_EVENTS as usize)
            .await
            .expect("冷恢复 replay 应完成");
        let replayed = replay_emitter.snapshot();
        let turn_updates = replayed
            .iter()
            .filter(|value| {
                value["type"] == "session_update"
                    && value["envelope"]["turnId"] == turn_id
                    && value["envelope"]["sourceAgentId"] == "root"
            })
            .collect::<Vec<_>>();
        let semantic_order = turn_updates
            .iter()
            .map(|value| {
                let update = &value["envelope"]["update"];
                match update["sessionUpdate"].as_str() {
                    Some("user_message_chunk") => format!(
                        "user:{}",
                        update["content"]["text"]
                            .as_str()
                            .expect("用户更新必须携带原始输入"),
                    ),
                    Some("agent_thought_chunk") => {
                        format!(
                            "thought:{}",
                            update["content"]["text"]
                                .as_str()
                                .expect("推理更新必须携带文本")
                        )
                    }
                    Some("agent_message_chunk") => {
                        format!(
                            "text:{}",
                            update["content"]["text"]
                                .as_str()
                                .expect("正文更新必须携带文本")
                        )
                    }
                    Some("tool_call") => {
                        format!(
                            "tool:{}",
                            update["toolCallId"]
                                .as_str()
                                .expect("工具调用更新必须携带调用标识")
                        )
                    }
                    Some("tool_call_update") => format!(
                        "tool_update:{}:{}",
                        update["toolCallId"]
                            .as_str()
                            .expect("工具结果更新必须携带调用标识"),
                        update["status"].as_str().expect("工具结果更新必须携带状态")
                    ),
                    other => panic!("不应出现其他 Turn ACP 更新：{other:?}; value={value}"),
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            semantic_order,
            vec![
                "user:读取文件并总结",
                "thought:thought1",
                "tool:call-replay",
                "tool_update:call-replay:in_progress",
                "tool_update:call-replay:completed",
                "thought:thought2",
                "text:text",
            ]
        );
        let tool_result_update = turn_updates
            .iter()
            .find(|value| value["envelope"]["update"]["status"] == "completed")
            .expect("重放必须有工具终态");
        assert_eq!(
            tool_result_update["envelope"]["update"]["_meta"]["keencode/toolOutcome"],
            "succeeded"
        );
        assert_eq!(
            tool_result_update["envelope"]["update"]["rawOutput"]["content"][0]["type"],
            "text"
        );
        assert_eq!(
            turn_updates
                .iter()
                .filter(|value| value["envelope"]["update"]["sessionUpdate"] == "tool_call")
                .count(),
            1
        );
        assert_eq!(
            turn_updates
                .iter()
                .filter(|value| {
                    value["envelope"]["update"]["sessionUpdate"] == "tool_call_update"
                        && value["envelope"]["update"]["status"] == "in_progress"
                })
                .count(),
            1
        );
        assert_eq!(
            turn_updates
                .iter()
                .filter(|value| {
                    value["envelope"]["update"]["sessionUpdate"] == "tool_call_update"
                        && value["envelope"]["update"]["status"] == "completed"
                })
                .count(),
            1
        );

        let completed_events = replayed
            .iter()
            .filter(|value| {
                value["type"] == "keencode_event"
                    && value["envelope"]["event"]["type"] == "turn_completed"
            })
            .collect::<Vec<_>>();
        assert_eq!(completed_events.len(), 1);
        assert_eq!(completed_events[0]["envelope"]["turnId"], turn_id);
        assert_eq!(completed_events[0]["envelope"]["sourceAgentId"], "root");
    }

    /// 在工具执行起点后暂停只读工具，验证 replay 水位固定且实时完成事件只在恢复门释放后跟随。
    struct BlockingReplayReadTool {
        /// 工具已进入执行体的通知；执行起点事件在此之前已经持久化。
        started: Arc<tokio::sync::Notify>,
        /// 允许工具返回成功结果的通知。
        release: Arc<tokio::sync::Notify>,
    }

    impl BlockingReplayReadTool {
        /// 创建由外部测试控制完成时机的只读工具。
        fn new(started: Arc<tokio::sync::Notify>, release: Arc<tokio::sync::Notify>) -> Self {
            Self { started, release }
        }
    }

    impl keencode_agent::AgentTool for BlockingReplayReadTool {
        /// 暴露与 Read 相同输入形状的测试工具定义。
        fn definition(&self) -> keencode_model::ToolDefinition {
            keencode_model::ToolDefinition::new(
                "Read",
                "暂停后返回固定文本的测试只读工具",
                json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "minLength": 1 }
                    },
                    "required": ["file_path"],
                    "additionalProperties": false
                }),
            )
        }

        /// 测试工具始终声明只读影响，允许 Runtime 持久化执行起点。
        fn effect(
            &self,
            _input: &Value,
        ) -> Result<keencode_agent::ToolEffect, keencode_agent::ToolError> {
            Ok(keencode_agent::ToolEffect::ReadOnly)
        }

        /// 使用独占模式让测试只保留一个明确的在途执行。
        fn concurrency(&self) -> keencode_agent::ToolConcurrency {
            keencode_agent::ToolConcurrency::Exclusive
        }

        /// 通知执行起点后等待释放或 Turn 取消，再返回固定只读结果。
        fn execute(
            &self,
            context: keencode_agent::ToolContext,
            _input: Value,
        ) -> keencode_agent::ToolFuture<'_> {
            let started = self.started.clone();
            let release = self.release.clone();
            Box::pin(async move {
                started.notify_one();
                tokio::select! {
                    _ = release.notified() => Ok(keencode_agent::ToolOutput::text("blocked-read")),
                    _ = context.cancellation.cancelled() => Err(keencode_agent::ToolError::permanent(
                        "turn_cancelled",
                        "阻塞读取测试被取消",
                    )),
                }
            })
        }
    }

    /// 提取指定 Turn 的标准 Session 更新语义，忽略 Session 级更新和生命周期扩展事件。
    fn collect_replay_turn_update_semantics(values: &[Value], turn_id: &str) -> Vec<String> {
        values
            .iter()
            .filter_map(|value| {
                let envelope = &value["envelope"];
                if value["type"] != "session_update"
                    || envelope["turnId"] != turn_id
                    || envelope["sourceAgentId"] != "root"
                {
                    return None;
                }
                let update = &envelope["update"];
                match update["sessionUpdate"].as_str() {
                    Some("user_message_chunk") => Some(format!(
                        "user:{}",
                        update["content"]["text"]
                            .as_str()
                            .expect("用户更新必须携带原始输入"),
                    )),
                    Some("agent_thought_chunk") => Some(format!(
                        "thought:{}",
                        update["content"]["text"]
                            .as_str()
                            .expect("推理更新必须携带文本"),
                    )),
                    Some("agent_message_chunk") => Some(format!(
                        "text:{}",
                        update["content"]["text"]
                            .as_str()
                            .expect("正文更新必须携带文本"),
                    )),
                    Some("tool_call") => Some(format!(
                        "tool:{}",
                        update["toolCallId"]
                            .as_str()
                            .expect("工具调用更新必须携带调用标识"),
                    )),
                    Some("tool_call_update") => Some(format!(
                        "tool_update:{}:{}",
                        update["toolCallId"]
                            .as_str()
                            .expect("工具结果更新必须携带调用标识"),
                        update["status"].as_str().expect("工具结果更新必须携带状态"),
                    )),
                    Some(other) => panic!("不应出现其他 Turn ACP 更新：{other}; value={value}"),
                    None => panic!("Turn ACP 更新缺少 sessionUpdate：{value}"),
                }
            })
            .collect()
    }

    /// 在真实在途工具期间分页 replay，随后验证旧水位、实时缓冲和新的完整恢复各自边界。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replay_paging_freezes_inflight_tool_waterline_before_completion() {
        let storage = tempfile::tempdir().expect("应创建 Runtime 存储目录");
        let project = tempfile::tempdir().expect("应创建项目目录");
        let replay_emitter = RecordingEmitter::successful();
        let runtime = Arc::new(
            AgentRuntime::new(storage.path(), replay_emitter.clone()).expect("测试 Runtime 应创建"),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "replay-inflight-page")
            .expect("测试 Session 应创建");
        let session_id = session.session_id().as_str().to_owned();
        let turn_id = "turn-replay-inflight-page";
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(BlockingReplayReadTool::new(
                started.clone(),
                release.clone(),
            )))
            .expect("阻塞 Read 工具应注册");

        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                ..ProviderCapabilities::default()
            },
            [
                ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::ReasoningDelta {
                        index: 0,
                        delta: "thought1".to_owned(),
                    },
                    ModelStreamEvent::ToolCallStart {
                        index: 1,
                        id: "call-replay-inflight".to_owned(),
                        name: "Read".to_owned(),
                    },
                    ModelStreamEvent::ToolCallArgumentsDelta {
                        index: 1,
                        id: "call-replay-inflight".to_owned(),
                        delta: json!({"file_path": "replay-input.txt"}).to_string(),
                    },
                    ModelStreamEvent::ToolCallEnd {
                        index: 1,
                        id: "call-replay-inflight".to_owned(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: StopReason::ToolUse,
                    },
                ]),
                ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::ReasoningDelta {
                        index: 0,
                        delta: "thought2".to_owned(),
                    },
                    ModelStreamEvent::TextDelta {
                        index: 1,
                        delta: "text".to_owned(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: StopReason::Completed,
                    },
                ]),
            ],
        ));
        let provider_for_assertions = provider.clone();
        let input = ModelMessage::text(MessageRole::User, "读取并总结");
        let request = TurnRequest::new(
            keencode_agent::SessionId::new(session_id.clone()).expect("Agent Session 标识应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn 标识应有效"),
            keencode_agent::AgentId::new("root").expect("根 Agent 标识应有效"),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        );
        let runner =
            session.bind_agent_runner(AgentRunner::new(provider, tools, RunLimits::default()));
        let turn_task = tokio::spawn(async move {
            runner
                .run_turn(RuntimeTurnRequest::root(
                    request,
                    vec![input],
                    "在途 replay 分页",
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .expect("阻塞工具应在测试窗口内进入执行体");

        let blocked = session.snapshot().expect("在途工具快照应读取");
        let through = blocked.state.last_sequence;
        let blocked_lifecycle = blocked
            .state
            .tools
            .values()
            .find(|lifecycle| lifecycle.request.model_tool_call_id == "call-replay-inflight")
            .expect("在途工具生命周期应存在");
        assert!(blocked_lifecycle.execution_started);
        assert!(blocked_lifecycle.outcome.is_none());
        assert!(blocked_lifecycle.transcript_segment.is_none());

        let first_page = runtime
            .replay_session(&session_id, None, 2)
            .await
            .expect("在途工具首个 replay 分页应成功");
        assert_eq!(first_page.through_journal_sequence, through);
        assert_eq!(first_page.next_after, 1);
        assert_eq!(first_page.replayed_events, 1);
        assert!(first_page.has_more);
        let delivery = runtime
            .session_delivery(&session_id)
            .expect("首个 replay 后投递世代应存在");
        {
            let cursor = delivery.replay_cursor.lock().await;
            assert_eq!(cursor.through_sequence, Some(through));
            let frozen = cursor
                .frozen_state
                .as_ref()
                .expect("首个 replay 应保存冻结状态");
            let frozen_lifecycle = frozen
                .tools
                .values()
                .find(|lifecycle| lifecycle.request.model_tool_call_id == "call-replay-inflight")
                .expect("冻结状态应保留在途工具");
            assert!(frozen_lifecycle.execution_started);
            assert!(frozen_lifecycle.outcome.is_none());
            assert!(frozen_lifecycle.transcript_segment.is_none());
        }

        // 首页建立恢复门后再启动实时泵，释放工具产生的权威更新必须先进入门内缓存。
        runtime
            .ensure_session_delivery(&session_id)
            .expect("恢复中的 Session 应建立实时泵");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if runtime
                    .live_pumps
                    .lock()
                    .expect("实时泵状态应读取")
                    .contains_key(&session_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("实时泵应在测试窗口内启动");

        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(5), turn_task)
            .await
            .expect("完整 Turn 应在测试窗口内结束")
            .expect("Turn 任务不应 panic")
            .expect("Runtime Turn 应完成");
        assert!(result.is_success(), "在途分页 Turn 应成功：{result:?}");
        assert_eq!(result.state.round_count(), 2);
        assert_eq!(result.state.step_count(), 1);
        assert_eq!(
            provider_for_assertions
                .requests()
                .expect("Provider 请求应读取")
                .len(),
            2
        );
        let completed = session.snapshot().expect("完成后的 Session 快照应读取");
        assert!(completed.state.last_sequence > through);
        let completed_lifecycle = completed
            .state
            .tools
            .values()
            .find(|lifecycle| lifecycle.request.model_tool_call_id == "call-replay-inflight")
            .expect("完成后的工具生命周期应存在");
        assert!(completed_lifecycle.outcome.is_some());
        assert!(completed_lifecycle.transcript_segment.is_some());

        // 旧恢复世代逐页继续消费固定水位；末页前实时事件仍不得越过恢复门投影。
        let mut after = first_page.next_after;
        let (before_final, final_page) = loop {
            let before = replay_emitter.snapshot();
            let page = runtime
                .replay_session(&session_id, Some(after), 2)
                .await
                .expect("连续 replay 分页应成功");
            assert_eq!(page.through_journal_sequence, through);
            if page.has_more {
                assert!(page.next_after > after);
                let visible = replay_emitter.snapshot();
                let semantics = collect_replay_turn_update_semantics(&visible, turn_id);
                assert!(!semantics.iter().any(|item| {
                    item == "thought:thought2"
                        || item == "text:text"
                        || item == "tool_update:call-replay-inflight:completed"
                }));
                after = page.next_after;
            } else {
                break (before, page);
            }
        };
        assert_eq!(
            collect_replay_turn_update_semantics(&before_final, turn_id),
            vec!["user:读取并总结"]
        );
        assert_eq!(final_page.next_after, through);
        assert_eq!(final_page.replayed_events, 2);
        assert!(!final_page.has_more);

        // 末页回执只代表恢复门已释放；此前已完成 Turn 的 live pump 仍可能尚未把
        // 门内权威缓存交给投递泵，因此等待同一世代的最终语义收敛后再断言顺序。
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let semantics =
                    collect_replay_turn_update_semantics(&replay_emitter.snapshot(), turn_id);
                if semantics
                    == vec![
                        "user:读取并总结",
                        "tool:call-replay-inflight",
                        "tool_update:call-replay-inflight:in_progress",
                        "tool_update:call-replay-inflight:completed",
                    ]
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("旧 replay 世代应在测试窗口内释放全部实时事件");
        let old_generation = replay_emitter.snapshot();
        assert_eq!(
            collect_replay_turn_update_semantics(&old_generation, turn_id),
            vec![
                "user:读取并总结",
                "tool:call-replay-inflight",
                "tool_update:call-replay-inflight:in_progress",
                "tool_update:call-replay-inflight:completed",
            ]
        );
        assert_eq!(
            old_generation
                .iter()
                .filter(|value| {
                    value["type"] == "keencode_event"
                        && value["envelope"]["event"]["type"] == "turn_completed"
                })
                .count(),
            1
        );

        // 新的完整 replay 重新从最终 Session 状态建立恢复门，必须恢复完整 Transcript 语义顺序。
        runtime
            .close_session_delivery(&session_id)
            .await
            .expect("旧 replay 投递世代应关闭");
        let full_start = replay_emitter.snapshot().len();
        let full_page = runtime
            .replay_session(&session_id, None, keencode_acp::MAX_REPLAY_EVENTS as usize)
            .await
            .expect("新的完整 replay 应成功");
        assert!(!full_page.has_more);
        let full_values = replay_emitter.snapshot();
        let full_values = &full_values[full_start..];
        assert_eq!(
            collect_replay_turn_update_semantics(full_values, turn_id),
            vec![
                "user:读取并总结",
                "thought:thought1",
                "tool:call-replay-inflight",
                "tool_update:call-replay-inflight:in_progress",
                "tool_update:call-replay-inflight:completed",
                "thought:thought2",
                "text:text",
            ]
        );
        assert_eq!(
            full_values
                .iter()
                .filter(|value| {
                    value["type"] == "session_update"
                        && value["envelope"]["turnId"] == turn_id
                        && value["envelope"]["update"]["sessionUpdate"] == "tool_call"
                })
                .count(),
            1
        );
        assert_eq!(
            full_values
                .iter()
                .filter(|value| {
                    value["type"] == "keencode_event"
                        && value["envelope"]["event"]["type"] == "turn_completed"
                })
                .count(),
            1
        );
        runtime
            .close_session(&session_id)
            .await
            .expect("测试 Session 应关闭");
    }
}
