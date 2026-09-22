//! 平台无关的 Host 生命周期、Prompt admission queue 与操作幂等账本。
//!
//! Desktop、Web、TUI 和 Agent CLI 都必须先经过这里的 admission，再把已接受的
//! Prompt 交给 Runtime。客户端各自维护的 UI 草稿队列不属于权威执行队列。该模块
//! 不绑定 Tauri、WebSocket 或 stdio；平台入口只负责把连接角色和 Runtime 回调接入。

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use keencode_acp::{
    ConnectionId, HostConnectionRole, HostDisconnectAction, HostLifecycleAction,
    HostLifecyclePhase, HostOwnerKind, MAX_HOST_PROMPT_BYTES, OperationId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// 默认每个 Session 允许的 active + pending 操作总量。
pub const DEFAULT_HOST_QUEUE_CAPACITY: usize = 20;
/// 默认保留的终态操作数，用于响应丢失后的重复查询与重试。
pub const DEFAULT_HOST_TERMINAL_RETENTION: usize = 256;
/// Prompt payload digest 的固定十六进制长度。
pub const HOST_PAYLOAD_DIGEST_HEX_BYTES: usize = 64;

/// Host Core 的固定配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCoreConfig {
    /// 每个 Session 的 active + pending 上限。
    pub queue_capacity: usize,
    /// 每个 Session 保留的终态操作上限。
    pub terminal_retention: usize,
}

impl Default for HostCoreConfig {
    /// 返回有界且适合桌面、移动 Web 和 CLI 共用的默认配置。
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_HOST_QUEUE_CAPACITY,
            terminal_retention: DEFAULT_HOST_TERMINAL_RETENTION,
        }
    }
}

impl HostCoreConfig {
    /// 校验队列和终态保留均为非零且不超过固定安全上限。
    pub fn validate(self) -> Result<Self, HostCoreError> {
        if self.queue_capacity == 0
            || self.queue_capacity > 1024
            || self.terminal_retention == 0
            || self.terminal_retention > 16_384
        {
            return Err(HostCoreError::InvalidConfig);
        }
        Ok(self)
    }
}

/// 一次 Prompt admission 的最小请求事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAdmissionRequest {
    /// 发起请求的连接；连接断开后不能改变操作归属或取消语义。
    pub connection_id: ConnectionId,
    /// 权威 Runtime Session 标识。
    pub session_id: String,
    /// 跨连接重试使用的幂等标识；不能复用 JSON-RPC request id。
    pub operation_id: OperationId,
    /// 完整 Prompt 正文；Host 会在 admission 边界做长度校验。
    pub prompt: String,
    /// 调用方对规范化请求正文计算的 SHA-256；用于检测同一 operationId 的冲突复用。
    pub payload_digest: String,
    /// CLI 请求断开后继续运行；该标志不改变 Host owner 生命周期。
    pub detached: bool,
}

impl PromptAdmissionRequest {
    /// 校验请求正文、Session 标识和 payload digest 的边界。
    pub fn validate(&self) -> Result<(), HostCoreError> {
        if self.session_id.is_empty() || self.session_id.len() > 256 {
            return Err(HostCoreError::InvalidRequest("session_id"));
        }
        if self.session_id.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(HostCoreError::InvalidRequest("session_id"));
        }
        if self.prompt.is_empty() || self.prompt.len() > MAX_HOST_PROMPT_BYTES {
            return Err(HostCoreError::InvalidRequest("prompt"));
        }
        validate_digest(&self.payload_digest)?;
        Ok(())
    }

    /// 计算用于 operationId 冲突判断的稳定请求指纹。
    pub fn request_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.payload_digest.as_bytes());
        hasher.update([0]);
        hasher.update(self.session_id.as_bytes());
        hasher.update([0]);
        hasher.update(self.prompt.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Runtime 真正开始执行前必须绑定的稳定身份。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionIdentity {
    /// 权威 Session 标识。
    pub session_id: String,
    /// Runtime 为根 Turn 分配的稳定 Turn 标识。
    pub turn_id: String,
    /// Runtime/Agent 任务标识；非交互 CLI detach 必须在返回前拿到它。
    pub task_id: String,
}

impl ExecutionIdentity {
    /// 创建并校验稳定 Session/Turn/Task 三元组。
    pub fn new(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Result<Self, HostCoreError> {
        let value = Self {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            task_id: task_id.into(),
        };
        for (name, value) in [
            ("session_id", &value.session_id),
            ("turn_id", &value.turn_id),
            ("task_id", &value.task_id),
        ] {
            if value.is_empty()
                || value.len() > 256
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(HostCoreError::InvalidRequest(name));
            }
        }
        Ok(value)
    }
}

/// 操作的运行阶段；状态变更只能由 Host Core 的方法完成。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// 已接受并等待 Runtime claim。
    Admitted,
    /// 已由 Runtime claim，等待绑定稳定 Turn/Task 身份。
    Claimed,
    /// 已绑定身份并运行中。
    Running,
    /// Agent 请求用户输入；连接断开不能把该状态变成取消。
    NeedsInput,
    /// 已形成权威终态。
    Completed,
    /// 已失败，错误摘要由 Host 归一化后返回。
    Failed,
    /// 已明确取消。
    Cancelled,
    /// Host 重启后发现旧操作没有可证明终态，必须先对账。
    RecoveryRequired,
}

impl OperationState {
    /// 判断该状态是否已经是不可继续执行的终态。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Prompt 进入 Host 队列后的终态摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationTerminal {
    /// 稳定的错误/终态分类；不应包含 Provider 原文或凭据。
    pub code: String,
    /// 可供客户端显示的有界摘要。
    pub message: Option<String>,
}

impl OperationTerminal {
    /// 创建有界终态摘要。
    pub fn new(
        code: impl Into<String>,
        message: impl Into<Option<String>>,
    ) -> Result<Self, HostCoreError> {
        let code = code.into();
        if code.is_empty() || code.len() > 128 || code.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(HostCoreError::InvalidRequest("terminal.code"));
        }
        let message = message.into();
        if message.as_ref().is_some_and(|value| value.len() > 8 * 1024) {
            return Err(HostCoreError::InvalidRequest("terminal.message"));
        }
        Ok(Self { code, message })
    }
}

/// 当前操作的有限、可重连查询快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationStatus {
    /// 跨连接幂等操作标识。
    pub operation_id: OperationId,
    /// 权威 Session 标识。
    pub session_id: String,
    /// 当前操作阶段。
    pub state: OperationState,
    /// 已绑定的稳定执行身份；admission 未 claim 时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionIdentity>,
    /// 当前等待用户输入的唯一请求标识。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    /// 已形成终态时的可重放摘要。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<OperationTerminal>,
    /// 最近一次 Elicitation 赢家；用于其他客户端识别重复回答结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation_answer: Option<ElicitationAnswerReceipt>,
    /// detached CLI 是否已经取得可以安全退出的稳定身份。
    pub detached: bool,
}

/// 一次 Elicitation 回答的幂等收据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElicitationAnswerReceipt {
    /// 对应的待决 Elicitation 标识。
    pub elicitation_id: String,
    /// 客户端生成的回答幂等标识；与 JSON-RPC request id 分离。
    pub answer_id: OperationId,
    /// 规范化回答正文的 SHA-256；只用于检测冲突，不回显正文。
    pub answer_digest: String,
}

/// Elicitation 回答的首次赢家或幂等重复结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElicitationAnswerDisposition {
    /// 本次回答首次赢得待决输入。
    Accepted,
    /// 相同 answerId 和 digest 已经赢得该输入，没有重复副作用。
    Duplicate,
}

/// Elicitation 回答操作返回的稳定结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElicitationAnswerResult {
    /// 本次回答是首次接受还是幂等重复。
    pub disposition: ElicitationAnswerDisposition,
    /// 回答后的 operation 状态。
    pub status: OperationStatus,
}

/// admission 返回的结果分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionDisposition {
    /// 当前 Session 无 active/pending 操作，下一次 claim 可立即执行。
    Accepted,
    /// 当前 Session 已有运行或排队操作，本次进入有界队列。
    Queued,
    /// 相同 operationId 和请求指纹已经存在，本次没有重复副作用。
    Duplicate,
}

/// admission 成功或重复时返回的稳定操作收据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionReceipt {
    /// 操作标识。
    pub operation_id: OperationId,
    /// 权威 Session 标识。
    pub session_id: String,
    /// 本次是首次接受、排队还是幂等重复。
    pub disposition: AdmissionDisposition,
    /// 当前有限状态。
    pub status: OperationStatus,
    /// 当前 pending 队列中的 1-based 位置；active/终态为空。
    pub queue_position: Option<usize>,
}

/// Runtime claim 后拿到的 Prompt；只有 Host Core 可将其交给执行器。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedPrompt {
    /// 跨连接操作标识。
    pub operation_id: OperationId,
    /// 发起连接；仅用于审计和请求回复，断开不取消操作。
    pub connection_id: ConnectionId,
    /// 权威 Session 标识。
    pub session_id: String,
    /// 原始有界 Prompt 正文。
    pub prompt: String,
    /// 是否要求客户端 detach 后继续运行。
    pub detached: bool,
}

/// Host 重启后从 Journal 还原的操作事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredOperation {
    /// 跨连接操作标识。
    pub operation_id: OperationId,
    /// 权威 Session 标识。
    pub session_id: String,
    /// Admission 时记录的请求指纹。
    pub request_fingerprint: String,
    /// 已知的稳定执行身份。
    pub execution: Option<ExecutionIdentity>,
    /// 已知终态；缺失时说明操作需要人工/Runtime 对账。
    pub terminal: Option<OperationTerminal>,
    /// 已知终态分类；恢复时不能把 failed/cancelled 强行改写成 completed。
    pub terminal_state: Option<OperationState>,
    /// Journal 是否记录了尚未完成的用户输入请求。
    pub elicitation_id: Option<String>,
    /// 是否允许重建为等待输入而非未知执行。
    pub detached: bool,
}

/// Host Core 内部返回的错误分类。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum HostCoreError {
    /// 配置上限为零或超过硬上限。
    #[error("Host Core 配置无效")]
    InvalidConfig,
    /// 请求字段未满足边界。
    #[error("Host 请求字段无效: {0}")]
    InvalidRequest(&'static str),
    /// 同一 operationId 被不同请求正文复用。
    #[error("operationId 已被不同请求占用")]
    OperationConflict,
    /// 同一 Elicitation 已经由其他回答幂等键赢得。
    #[error("elicitation 已经由其他回答赢得")]
    ElicitationAlreadyAnswered,
    /// 当前 Session 队列已达到有界上限。
    #[error("Session admission queue 已满")]
    QueueFull,
    /// 操作不存在。
    #[error("operation 不存在")]
    OperationNotFound,
    /// 当前状态不允许执行请求动作。
    #[error("operation 当前状态不允许该动作")]
    InvalidState,
    /// Host 生命周期不允许当前动作。
    #[error("Host 生命周期不允许该动作")]
    InvalidLifecycle,
    /// 连接标识已登记或不存在。
    #[error("连接状态无效")]
    InvalidConnection,
    /// Host owner 与连接角色不一致。
    #[error("Host owner 与连接角色不一致")]
    OwnerMismatch,
    /// 内部状态锁已损坏。
    #[error("Host Core 状态不可用")]
    StateUnavailable,
}

/// Host 生命周期控制器。
pub struct HostLifecycleController {
    inner: Mutex<LifecycleState>,
}

#[derive(Debug)]
struct LifecycleState {
    owner_kind: HostOwnerKind,
    phase: HostLifecyclePhase,
    connections: BTreeMap<ConnectionId, HostConnectionRole>,
}

impl HostLifecycleController {
    /// 创建尚未对外服务的 Host 生命周期状态。
    pub fn new(owner_kind: HostOwnerKind) -> Self {
        Self {
            inner: Mutex::new(LifecycleState {
                owner_kind,
                phase: HostLifecyclePhase::Starting,
                connections: BTreeMap::new(),
            }),
        }
    }

    /// 返回当前 Host phase。
    pub fn phase(&self) -> Result<HostLifecyclePhase, HostCoreError> {
        Ok(self.lock()?.phase)
    }

    /// 返回 owner 类型。
    pub fn owner_kind(&self) -> Result<HostOwnerKind, HostCoreError> {
        Ok(self.lock()?.owner_kind)
    }

    /// 标记 Runtime、Journal、Provider 和服务端点均已就绪。
    pub fn mark_ready(&self) -> Result<(), HostCoreError> {
        let mut state = self.lock()?;
        if state.phase != HostLifecyclePhase::Starting {
            return Err(HostCoreError::InvalidLifecycle);
        }
        state.phase = HostLifecyclePhase::Ready;
        Ok(())
    }

    /// 登记一个连接；只有匹配 owner 类型的 owner role 才能登记为 owner。
    pub fn attach(
        &self,
        connection_id: ConnectionId,
        role: HostConnectionRole,
    ) -> Result<(), HostCoreError> {
        let mut state = self.lock()?;
        if !matches!(
            state.phase,
            HostLifecyclePhase::Starting | HostLifecyclePhase::Ready
        ) || state.connections.contains_key(&connection_id)
        {
            return Err(HostCoreError::InvalidConnection);
        }
        if role == HostConnectionRole::DesktopOwner && state.owner_kind != HostOwnerKind::Desktop {
            return Err(HostCoreError::OwnerMismatch);
        }
        if role == HostConnectionRole::HeadlessOwner && state.owner_kind != HostOwnerKind::Headless
        {
            return Err(HostCoreError::OwnerMismatch);
        }
        if role.owns_host()
            && state
                .connections
                .values()
                .any(|existing| existing.owns_host())
        {
            return Err(HostCoreError::InvalidConnection);
        }
        state.connections.insert(connection_id, role);
        Ok(())
    }

    /// owner 明确请求关闭时进入 draining；非 owner 不得触发共享 Host shutdown。
    pub fn begin_shutdown(
        &self,
        requester: &ConnectionId,
    ) -> Result<HostLifecycleAction, HostCoreError> {
        let mut state = self.lock()?;
        let role = *state
            .connections
            .get(requester)
            .ok_or(HostCoreError::InvalidConnection)?;
        if !role.owns_host() || state.phase != HostLifecyclePhase::Ready {
            return Err(HostCoreError::InvalidLifecycle);
        }
        state.phase = HostLifecyclePhase::Draining;
        Ok(HostLifecycleAction::Shutdown)
    }

    /// 完成所有 Runtime、响应、投递和后台资源收尾。
    pub fn finish_shutdown(&self) -> Result<(), HostCoreError> {
        let mut state = self.lock()?;
        if state.phase != HostLifecyclePhase::Draining {
            return Err(HostCoreError::InvalidLifecycle);
        }
        state.phase = HostLifecyclePhase::Stopped;
        state.connections.clear();
        Ok(())
    }

    /// 记录不可恢复启动/运行故障；调用方必须丢弃该 Host 实例并重新创建。
    pub fn fail(&self) -> Result<HostLifecycleAction, HostCoreError> {
        let mut state = self.lock()?;
        if state.phase == HostLifecyclePhase::Stopped {
            return Err(HostCoreError::InvalidLifecycle);
        }
        state.phase = HostLifecyclePhase::Failed;
        state.connections.clear();
        Ok(HostLifecycleAction::Fail)
    }

    /// 断开连接；owner 断开会要求 shutdown，client/detached client 只 detach。
    pub fn disconnect(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<HostDisconnectAction, HostCoreError> {
        let mut state = self.lock()?;
        let role = state
            .connections
            .remove(connection_id)
            .ok_or(HostCoreError::InvalidConnection)?;
        // 传输连接断开不代表用户明确退出。Desktop 关闭到托盘、WebSocket 短断线和
        // CLI 重连都必须保留 Host；owner 权限只由 begin_shutdown 显式命令消费。
        let _ = role;
        Ok(HostDisconnectAction::DetachClient)
    }

    /// 返回当前连接数量。
    pub fn connection_count(&self) -> Result<usize, HostCoreError> {
        Ok(self.lock()?.connections.len())
    }

    fn lock(&self) -> Result<MutexGuard<'_, LifecycleState>, HostCoreError> {
        self.inner
            .lock()
            .map_err(|_| HostCoreError::StateUnavailable)
    }
}

/// 每个 Session 的有界 Prompt admission queue 与跨连接幂等账本。
pub struct HostPromptQueue {
    inner: Mutex<QueueState>,
    config: HostCoreConfig,
}

#[derive(Debug)]
struct QueueState {
    sessions: BTreeMap<String, SessionQueue>,
    operations: BTreeMap<OperationId, OperationEntry>,
}

#[derive(Debug, Default)]
struct SessionQueue {
    active: Option<OperationId>,
    pending: VecDeque<OperationId>,
    terminal: VecDeque<OperationId>,
    recovery_blocked: bool,
}

#[derive(Debug)]
struct OperationEntry {
    request_fingerprint: String,
    connection_id: ConnectionId,
    session_id: String,
    prompt: String,
    detached: bool,
    state: OperationState,
    execution: Option<ExecutionIdentity>,
    elicitation_id: Option<String>,
    elicitation_answer: Option<ElicitationAnswerReceipt>,
    terminal: Option<OperationTerminal>,
}

impl HostPromptQueue {
    /// 创建新的空队列；配置必须通过硬上限校验。
    pub fn new(config: HostCoreConfig) -> Result<Self, HostCoreError> {
        Ok(Self {
            inner: Mutex::new(QueueState {
                sessions: BTreeMap::new(),
                operations: BTreeMap::new(),
            }),
            config: config.validate()?,
        })
    }

    /// 按默认上限创建 Host 队列。
    pub fn with_defaults() -> Self {
        Self::new(HostCoreConfig::default()).expect("默认 Host Core 配置必须有效")
    }

    /// 接受一个 Prompt，返回首次接受、排队或幂等重复收据。
    pub fn admit(
        &self,
        request: PromptAdmissionRequest,
    ) -> Result<AdmissionReceipt, HostCoreError> {
        request.validate()?;
        let fingerprint = request.request_fingerprint();
        let mut state = self.lock()?;

        if let Some(existing) = state.operations.get(&request.operation_id) {
            if existing.session_id != request.session_id
                || existing.request_fingerprint != fingerprint
            {
                return Err(HostCoreError::OperationConflict);
            }
            let operation_id = request.operation_id.clone();
            return Ok(AdmissionReceipt {
                operation_id,
                session_id: existing.session_id.clone(),
                disposition: AdmissionDisposition::Duplicate,
                status: status_from_entry(&request.operation_id, existing),
                queue_position: queue_position(&request.operation_id, &state),
            });
        }

        let operation_id = request.operation_id.clone();
        let (disposition, position) = {
            let queue = state
                .sessions
                .entry(request.session_id.clone())
                .or_default();
            if queue.recovery_blocked {
                return Err(HostCoreError::InvalidState);
            }
            let current_size = usize::from(queue.active.is_some()) + queue.pending.len();
            if current_size >= self.config.queue_capacity {
                return Err(HostCoreError::QueueFull);
            }
            let disposition = if queue.active.is_none() && queue.pending.is_empty() {
                AdmissionDisposition::Accepted
            } else {
                AdmissionDisposition::Queued
            };
            queue.pending.push_back(operation_id.clone());
            (disposition, queue.pending.len())
        };
        let entry = OperationEntry {
            request_fingerprint: fingerprint,
            connection_id: request.connection_id,
            session_id: request.session_id.clone(),
            prompt: request.prompt,
            detached: request.detached,
            state: OperationState::Admitted,
            execution: None,
            elicitation_id: None,
            elicitation_answer: None,
            terminal: None,
        };
        state.operations.insert(operation_id.clone(), entry);
        let status = status_from_entry(
            &operation_id,
            state
                .operations
                .get(&operation_id)
                .expect("新建 operation 必须进入全局账本"),
        );
        Ok(AdmissionReceipt {
            operation_id,
            session_id: request.session_id,
            disposition,
            status,
            queue_position: Some(position),
        })
    }

    /// 为指定 Session claim 下一条 Prompt；同一 Session 同时只能有一个 active。
    pub fn claim_next(&self, session_id: &str) -> Result<Option<ClaimedPrompt>, HostCoreError> {
        let mut state = self.lock()?;
        let operation_id = {
            let queue = state
                .sessions
                .get_mut(session_id)
                .ok_or(HostCoreError::OperationNotFound)?;
            if queue.recovery_blocked || queue.active.is_some() {
                return Ok(None);
            }
            let Some(operation_id) = queue.pending.pop_front() else {
                return Ok(None);
            };
            queue.active = Some(operation_id.clone());
            operation_id
        };
        let entry = state
            .operations
            .get_mut(&operation_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if entry.state != OperationState::Admitted {
            return Err(HostCoreError::InvalidState);
        }
        entry.state = OperationState::Claimed;
        Ok(Some(ClaimedPrompt {
            operation_id,
            connection_id: entry.connection_id.clone(),
            session_id: entry.session_id.clone(),
            prompt: entry.prompt.clone(),
            detached: entry.detached,
        }))
    }

    /// 只 claim 指定 operation；只有它已经位于 Session 队首时才会改变状态。
    ///
    /// Prompt 请求处理器各自等待自己的 operation，不能调用无条件的
    /// [`Self::claim_next`] 后再判断结果，否则并发处理器可能误领其他客户端的请求。
    pub fn claim_operation(
        &self,
        session_id: &str,
        operation_id: &OperationId,
    ) -> Result<Option<ClaimedPrompt>, HostCoreError> {
        let mut state = self.lock()?;
        let should_claim = {
            let queue = state
                .sessions
                .get(session_id)
                .ok_or(HostCoreError::OperationNotFound)?;
            !queue.recovery_blocked
                && queue.active.is_none()
                && queue.pending.front() == Some(operation_id)
        };
        if !should_claim {
            return Ok(None);
        }
        let queue = state
            .sessions
            .get_mut(session_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        let claimed_id = queue
            .pending
            .pop_front()
            .ok_or(HostCoreError::OperationNotFound)?;
        if &claimed_id != operation_id {
            return Err(HostCoreError::InvalidState);
        }
        queue.active = Some(claimed_id.clone());
        let entry = state
            .operations
            .get_mut(&claimed_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if entry.state != OperationState::Admitted {
            return Err(HostCoreError::InvalidState);
        }
        entry.state = OperationState::Claimed;
        Ok(Some(ClaimedPrompt {
            operation_id: claimed_id,
            connection_id: entry.connection_id.clone(),
            session_id: entry.session_id.clone(),
            prompt: entry.prompt.clone(),
            detached: entry.detached,
        }))
    }

    /// 将 Runtime 已分配的稳定 Session/Turn/Task 身份绑定到 claimed operation。
    pub fn bind_execution(
        &self,
        operation_id: &OperationId,
        execution: ExecutionIdentity,
    ) -> Result<OperationStatus, HostCoreError> {
        let mut state = self.lock()?;
        let entry = state
            .operations
            .get_mut(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if entry.state != OperationState::Claimed || entry.execution.is_some() {
            return Err(HostCoreError::InvalidState);
        }
        if entry.session_id != execution.session_id {
            return Err(HostCoreError::InvalidRequest("execution.session_id"));
        }
        entry.execution = Some(execution);
        entry.state = OperationState::Running;
        Ok(status_from_entry(operation_id, entry))
    }

    /// 将运行中操作置为等待用户输入；不释放 active slot，也不因连接断开取消。
    pub fn mark_needs_input(
        &self,
        operation_id: &OperationId,
        elicitation_id: impl Into<String>,
    ) -> Result<OperationStatus, HostCoreError> {
        let elicitation_id = bounded_id(elicitation_id.into(), "elicitation_id")?;
        let mut state = self.lock()?;
        let entry = state
            .operations
            .get_mut(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if !matches!(
            entry.state,
            OperationState::Running | OperationState::Claimed
        ) || entry.execution.is_none()
            || entry.elicitation_id.is_some()
        {
            return Err(HostCoreError::InvalidState);
        }
        entry.elicitation_id = Some(elicitation_id);
        entry.elicitation_answer = None;
        entry.state = OperationState::NeedsInput;
        Ok(status_from_entry(operation_id, entry))
    }

    /// 由任意仍有权限的 Desktop/Web 客户端回答待决 Elicitation。
    ///
    /// 第一个 `answer_id + answer_digest` 赢得该输入；同一回答重试返回 Duplicate，
    /// 其他回答返回 `ElicitationAlreadyAnswered`，从而不会出现两个客户端同时推进
    /// 一个 Agent Turn 的竞态。非交互 CLI 断开后不能自动调用此方法。
    pub fn answer_elicitation(
        &self,
        operation_id: &OperationId,
        elicitation_id: impl Into<String>,
        answer_id: OperationId,
        answer_digest: impl Into<String>,
    ) -> Result<ElicitationAnswerResult, HostCoreError> {
        let elicitation_id = bounded_id(elicitation_id.into(), "elicitation_id")?;
        let answer_digest = answer_digest.into();
        validate_digest(&answer_digest)?;
        let mut state = self.lock()?;
        let entry = state
            .operations
            .get_mut(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if let Some(previous) = &entry.elicitation_answer {
            if previous.elicitation_id == elicitation_id
                && previous.answer_id == answer_id
                && previous.answer_digest == answer_digest
            {
                return Ok(ElicitationAnswerResult {
                    disposition: ElicitationAnswerDisposition::Duplicate,
                    status: status_from_entry(operation_id, entry),
                });
            }
            return Err(HostCoreError::ElicitationAlreadyAnswered);
        }
        if entry.state != OperationState::NeedsInput
            || entry.elicitation_id.as_deref() != Some(elicitation_id.as_str())
        {
            return Err(HostCoreError::InvalidState);
        }
        entry.elicitation_answer = Some(ElicitationAnswerReceipt {
            elicitation_id,
            answer_id,
            answer_digest,
        });
        entry.elicitation_id = None;
        entry.state = OperationState::Running;
        Ok(ElicitationAnswerResult {
            disposition: ElicitationAnswerDisposition::Accepted,
            status: status_from_entry(operation_id, entry),
        })
    }

    /// 收到任意授权客户端的 Elicitation 响应后恢复运行；客户端断开不调用此方法。
    pub fn resume_after_input(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HostCoreError> {
        let mut state = self.lock()?;
        let entry = state
            .operations
            .get_mut(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        if entry.state != OperationState::NeedsInput || entry.elicitation_id.is_none() {
            return Err(HostCoreError::InvalidState);
        }
        entry.elicitation_id = None;
        entry.state = OperationState::Running;
        Ok(status_from_entry(operation_id, entry))
    }

    /// 记录权威完成、失败或取消终态，并释放 Session active slot。
    pub fn finish(
        &self,
        operation_id: &OperationId,
        state_value: OperationState,
        terminal: OperationTerminal,
    ) -> Result<OperationStatus, HostCoreError> {
        if !state_value.is_terminal() {
            return Err(HostCoreError::InvalidState);
        }
        let mut state = self.lock()?;
        let session_id = state
            .operations
            .get(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?
            .session_id
            .clone();
        let status = {
            let entry = state
                .operations
                .get_mut(operation_id)
                .ok_or(HostCoreError::OperationNotFound)?;
            if !matches!(
                entry.state,
                OperationState::Claimed | OperationState::Running | OperationState::NeedsInput
            ) {
                return Err(HostCoreError::InvalidState);
            }
            entry.state = state_value;
            entry.terminal = Some(terminal);
            entry.elicitation_id = None;
            status_from_entry(operation_id, entry)
        };
        let evicted = {
            let queue = state
                .sessions
                .get_mut(&session_id)
                .ok_or(HostCoreError::OperationNotFound)?;
            if queue.active.as_ref() != Some(operation_id) {
                return Err(HostCoreError::InvalidState);
            }
            queue.active = None;
            queue.terminal.push_back(operation_id.clone());
            let mut evicted = Vec::new();
            while queue.terminal.len() > self.config.terminal_retention {
                if let Some(operation_id) = queue.terminal.pop_front() {
                    evicted.push(operation_id);
                }
            }
            evicted
        };
        for evicted in evicted {
            // Only evict terminal receipts; an active/pending operation can never be in
            // this deque unless an internal invariant was broken.
            if state
                .operations
                .get(&evicted)
                .is_some_and(|candidate| candidate.state.is_terminal())
            {
                state.operations.remove(&evicted);
            }
        }
        Ok(status)
    }

    /// 取消尚未 claim 的 pending 操作；运行中操作必须走 Runtime cancel 协议后再 finish。
    pub fn cancel_pending(
        &self,
        operation_id: &OperationId,
        terminal: OperationTerminal,
    ) -> Result<OperationStatus, HostCoreError> {
        let mut state = self.lock()?;
        let session_id = state
            .operations
            .get(operation_id)
            .ok_or(HostCoreError::OperationNotFound)?
            .session_id
            .clone();
        let status = {
            let entry = state
                .operations
                .get_mut(operation_id)
                .ok_or(HostCoreError::OperationNotFound)?;
            if entry.state != OperationState::Admitted {
                return Err(HostCoreError::InvalidState);
            }
            entry.state = OperationState::Cancelled;
            entry.terminal = Some(terminal);
            status_from_entry(operation_id, entry)
        };
        let queue = state
            .sessions
            .get_mut(&session_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        queue.pending.retain(|candidate| candidate != operation_id);
        queue.terminal.push_back(operation_id.clone());
        Ok(status)
    }

    /// 查询当前操作；终态 receipt 在 retention 窗口内可重放。
    pub fn status(&self, operation_id: &OperationId) -> Result<OperationStatus, HostCoreError> {
        let state = self.lock()?;
        state
            .operations
            .get(operation_id)
            .map(|entry| status_from_entry(operation_id, entry))
            .ok_or(HostCoreError::OperationNotFound)
    }

    /// 按唯一待决 Elicitation 标识查找所属 operation。
    ///
    /// 该查询只返回当前非终态操作，供 Host 在已有 ElicitationCoordinator 收到
    /// Client Response 后把回答原子记入同一 admission 账本。
    pub fn operation_for_elicitation(
        &self,
        elicitation_id: &str,
    ) -> Result<Option<OperationStatus>, HostCoreError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .iter()
            .find(|(_, entry)| {
                !entry.state.is_terminal()
                    && entry.elicitation_id.as_deref() == Some(elicitation_id)
            })
            .map(|(operation_id, entry)| status_from_entry(operation_id, entry)))
    }

    /// 按 Runtime 已绑定的 Session/Turn 身份查找当前 operation。
    ///
    /// cancel 和终态归约必须以 Runtime 身份为准，不能用连接或 JSON-RPC 请求 ID
    /// 推断归属；同一 operation 允许在重连后继续由其他客户端观察和控制。
    pub fn operation_for_execution(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<OperationStatus>, HostCoreError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .iter()
            .find(|(_, entry)| {
                !entry.state.is_terminal()
                    && entry.execution.as_ref().is_some_and(|execution| {
                        execution.session_id == session_id && execution.turn_id == turn_id
                    })
            })
            .map(|(operation_id, entry)| status_from_entry(operation_id, entry)))
    }

    /// 返回指定 Session 当前唯一的非终态 operation。
    ///
    /// Elicitation 的请求 ID 在回答前可能尚未写入队列；该查询让 Host 能先把
    /// operation 标记为 `NeedsInput`，再交给既有 ElicitationCoordinator 严格收口。
    pub fn operation_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<OperationStatus>, HostCoreError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .iter()
            .find(|(_, entry)| entry.session_id == session_id && !entry.state.is_terminal())
            .map(|(operation_id, entry)| status_from_entry(operation_id, entry)))
    }

    /// 连接断开只返回当前操作快照，不修改 active/pending/NeedsInput 状态。
    ///
    /// 非交互 CLI 看到 `NeedsInput` 后应返回 `needs-input`，由 Desktop/Web 后续接管；
    /// 这里不能因为原连接消失就调用 `cancel_pending` 或 `finish`。
    pub fn disconnect_report(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<OperationStatus>, HostCoreError> {
        let state = self.lock()?;
        let mut result = Vec::new();
        for (operation_id, entry) in &state.operations {
            if &entry.connection_id == connection_id && !entry.state.is_terminal() {
                result.push(status_from_entry(operation_id, entry));
            }
        }
        Ok(result)
    }

    /// Host 重启后还原一个有终态的操作，供响应丢失后的幂等重试查询。
    pub fn recover_terminal(
        &self,
        recovered: RecoveredOperation,
    ) -> Result<OperationStatus, HostCoreError> {
        let terminal = recovered
            .terminal
            .clone()
            .ok_or(HostCoreError::InvalidState)?;
        let terminal_state = recovered
            .terminal_state
            .filter(|state| state.is_terminal())
            .ok_or(HostCoreError::InvalidState)?;
        self.recover(recovered, terminal_state, Some(terminal))
    }

    /// Host 重启后还原待决 Elicitation，保留 active slot 直到任意授权客户端回答。
    pub fn recover_needs_input(
        &self,
        recovered: RecoveredOperation,
    ) -> Result<OperationStatus, HostCoreError> {
        let elicitation_id = recovered
            .elicitation_id
            .clone()
            .ok_or(HostCoreError::InvalidState)?;
        if recovered.execution.is_none() {
            return Err(HostCoreError::InvalidState);
        }
        let status = self.recover(recovered, OperationState::NeedsInput, None)?;
        if status.elicitation_id.as_deref() != Some(elicitation_id.as_str()) {
            return Err(HostCoreError::InvalidState);
        }
        Ok(status)
    }

    /// Host 重启后还原没有可证明终态的操作；阻塞同一 Session 的新 Prompt，防止重跑。
    pub fn recover_unknown(
        &self,
        recovered: RecoveredOperation,
    ) -> Result<OperationStatus, HostCoreError> {
        let session_id = recovered.session_id.clone();
        let operation_id = recovered.operation_id.clone();
        self.recover(recovered, OperationState::RecoveryRequired, None)?;
        let mut state = self.lock()?;
        state
            .sessions
            .entry(session_id)
            .or_default()
            .recovery_blocked = true;
        state
            .operations
            .get(&operation_id)
            .map(|entry| status_from_entry(&operation_id, entry))
            .ok_or(HostCoreError::OperationNotFound)
    }

    /// 对账完成后解除 Session recovery block；不会自动重放未知副作用。
    pub fn clear_recovery_block(&self, session_id: &str) -> Result<(), HostCoreError> {
        let mut state = self.lock()?;
        let queue = state
            .sessions
            .get_mut(session_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        queue.recovery_blocked = false;
        Ok(())
    }

    /// 返回当前 Session active/pending 数量和是否被恢复阻塞。
    pub fn queue_snapshot(&self, session_id: &str) -> Result<HostQueueSnapshot, HostCoreError> {
        let state = self.lock()?;
        let queue = state
            .sessions
            .get(session_id)
            .ok_or(HostCoreError::OperationNotFound)?;
        Ok(HostQueueSnapshot {
            session_id: session_id.to_owned(),
            active_operation_id: queue.active.clone(),
            pending_operation_ids: queue.pending.iter().cloned().collect(),
            recovery_blocked: queue.recovery_blocked,
        })
    }

    fn recover(
        &self,
        recovered: RecoveredOperation,
        state_value: OperationState,
        terminal: Option<OperationTerminal>,
    ) -> Result<OperationStatus, HostCoreError> {
        let operation_id = recovered.operation_id.clone();
        let session_id = recovered.session_id.clone();
        validate_digest(&recovered.request_fingerprint)?;
        if recovered.session_id.is_empty() || recovered.session_id.len() > 256 {
            return Err(HostCoreError::InvalidRequest("session_id"));
        }
        let mut state = self.lock()?;
        if let Some(existing) = state.operations.get(&operation_id) {
            if existing.request_fingerprint != recovered.request_fingerprint {
                return Err(HostCoreError::OperationConflict);
            }
            return Ok(status_from_entry(&operation_id, existing));
        }
        if state_value == OperationState::NeedsInput {
            let queue = state.sessions.entry(session_id.clone()).or_default();
            if queue.active.is_some() {
                return Err(HostCoreError::InvalidState);
            }
            queue.active = Some(operation_id.clone());
        } else {
            state.sessions.entry(session_id.clone()).or_default();
        }
        let entry = OperationEntry {
            request_fingerprint: recovered.request_fingerprint,
            connection_id: ConnectionId::new("recovered-host")
                .map_err(|_| HostCoreError::InvalidRequest("connection_id"))?,
            session_id,
            prompt: String::new(),
            detached: recovered.detached,
            state: state_value,
            execution: recovered.execution,
            elicitation_id: recovered.elicitation_id,
            elicitation_answer: None,
            terminal,
        };
        state.operations.insert(operation_id.clone(), entry);
        if state_value.is_terminal() {
            let recovered_session_id = state
                .operations
                .get(&operation_id)
                .expect("恢复 operation 必须进入全局账本")
                .session_id
                .clone();
            state
                .sessions
                .get_mut(&recovered_session_id)
                .expect("恢复 operation 的 Session 必须存在")
                .terminal
                .push_back(operation_id.clone());
        }
        Ok(status_from_entry(
            &operation_id,
            state
                .operations
                .get(&operation_id)
                .expect("恢复 operation 必须进入全局账本"),
        ))
    }

    fn lock(&self) -> Result<MutexGuard<'_, QueueState>, HostCoreError> {
        self.inner
            .lock()
            .map_err(|_| HostCoreError::StateUnavailable)
    }
}

/// Session 当前的 active/pending/recovery 快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostQueueSnapshot {
    /// 权威 Session 标识。
    pub session_id: String,
    /// 当前 active operation；同一 Session 最多一个。
    pub active_operation_id: Option<OperationId>,
    /// 按 admission 顺序排列的 pending operation。
    pub pending_operation_ids: Vec<OperationId>,
    /// Host 重启后未知副作用尚未完成对账。
    pub recovery_blocked: bool,
}

fn status_from_entry(operation_id: &OperationId, entry: &OperationEntry) -> OperationStatus {
    OperationStatus {
        operation_id: operation_id.clone(),
        session_id: entry.session_id.clone(),
        state: entry.state,
        execution: entry.execution.clone(),
        elicitation_id: entry.elicitation_id.clone(),
        terminal: entry.terminal.clone(),
        elicitation_answer: entry.elicitation_answer.clone(),
        detached: entry.detached,
    }
}

fn queue_position(operation_id: &OperationId, state: &QueueState) -> Option<usize> {
    state
        .sessions
        .values()
        .find_map(|queue| {
            queue
                .pending
                .iter()
                .position(|candidate| candidate == operation_id)
        })
        .map(|position| position + 1)
}

fn validate_digest(value: &str) -> Result<(), HostCoreError> {
    if value.len() != HOST_PAYLOAD_DIGEST_HEX_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(HostCoreError::InvalidRequest("payload_digest"));
    }
    Ok(())
}

fn bounded_id(value: String, field: &'static str) -> Result<String, HostCoreError> {
    if value.is_empty() || value.len() > 256 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(HostCoreError::InvalidRequest(field));
    }
    Ok(value)
}

impl fmt::Display for AdmissionDisposition {
    /// 输出稳定的 admission 结果名称，供非交互 CLI stdout 使用。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Accepted => "accepted",
            Self::Queued => "queued",
            Self::Duplicate => "duplicate",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(value: &str) -> ConnectionId {
        ConnectionId::new(value).expect("测试连接标识应合法")
    }

    fn operation(value: &str) -> OperationId {
        OperationId::new(value).expect("测试操作标识应合法")
    }

    fn request(
        connection_id: &str,
        operation_id: &str,
        session_id: &str,
    ) -> PromptAdmissionRequest {
        let prompt = format!("prompt-{operation_id}");
        let payload_digest = format!("{:x}", Sha256::digest(prompt.as_bytes()));
        PromptAdmissionRequest {
            connection_id: connection(connection_id),
            session_id: session_id.to_owned(),
            operation_id: operation(operation_id),
            prompt,
            payload_digest,
            detached: false,
        }
    }

    fn terminal(code: &str) -> OperationTerminal {
        OperationTerminal::new(code, None::<String>).expect("测试终态应合法")
    }

    fn execution(session_id: &str, operation_id: &str) -> ExecutionIdentity {
        ExecutionIdentity::new(
            session_id,
            format!("turn-{operation_id}"),
            format!("task-{operation_id}"),
        )
        .expect("测试执行身份应合法")
    }

    #[test]
    fn lifecycle_owner_transport断开不会触发shutdown但显式命令可以() {
        let lifecycle = HostLifecycleController::new(HostOwnerKind::Desktop);
        let owner = connection("desktop-owner");
        lifecycle
            .attach(owner.clone(), HostConnectionRole::DesktopOwner)
            .unwrap();
        lifecycle.mark_ready().unwrap();
        assert_eq!(
            lifecycle.disconnect(&owner).unwrap(),
            HostDisconnectAction::DetachClient
        );
        assert_eq!(lifecycle.phase().unwrap(), HostLifecyclePhase::Ready);

        lifecycle
            .attach(owner.clone(), HostConnectionRole::DesktopOwner)
            .unwrap();
        assert_eq!(
            lifecycle.begin_shutdown(&owner).unwrap(),
            HostLifecycleAction::Shutdown
        );
        assert_eq!(lifecycle.phase().unwrap(), HostLifecyclePhase::Draining);
        lifecycle.finish_shutdown().unwrap();
        assert_eq!(lifecycle.phase().unwrap(), HostLifecyclePhase::Stopped);
    }

    #[test]
    fn admission有界且同operation重试无重复副作用() {
        let queue = HostPromptQueue::new(HostCoreConfig {
            queue_capacity: 2,
            terminal_retention: 4,
        })
        .unwrap();
        let first = queue
            .admit(request("desktop", "op-1", "session-a"))
            .unwrap();
        assert_eq!(first.disposition, AdmissionDisposition::Accepted);
        let duplicate = queue.admit(request("web", "op-1", "session-a")).unwrap();
        assert_eq!(duplicate.disposition, AdmissionDisposition::Duplicate);
        let second = queue.admit(request("web", "op-2", "session-a")).unwrap();
        assert_eq!(second.disposition, AdmissionDisposition::Queued);
        assert_eq!(
            queue.admit(request("cli", "op-3", "session-a")),
            Err(HostCoreError::QueueFull)
        );

        let claimed = queue.claim_next("session-a").unwrap().unwrap();
        assert_eq!(claimed.operation_id, operation("op-1"));
        queue
            .bind_execution(&operation("op-1"), execution("session-a", "op-1"))
            .unwrap();
        queue
            .finish(
                &operation("op-1"),
                OperationState::Completed,
                terminal("ok"),
            )
            .unwrap();
        let completed_retry = queue.admit(request("cli", "op-1", "session-a")).unwrap();
        assert_eq!(completed_retry.disposition, AdmissionDisposition::Duplicate);
        assert_eq!(completed_retry.status.state, OperationState::Completed);
        assert!(queue.claim_next("session-a").unwrap().is_some());
    }

    #[test]
    fn elicitation只有一个winner且断开保持pending() {
        let queue = HostPromptQueue::with_defaults();
        queue
            .admit(request("cli", "op-input", "session-input"))
            .unwrap();
        queue.claim_next("session-input").unwrap().unwrap();
        queue
            .bind_execution(
                &operation("op-input"),
                execution("session-input", "op-input"),
            )
            .unwrap();
        queue
            .mark_needs_input(&operation("op-input"), "ask-1")
            .unwrap();
        let disconnected = queue
            .disconnect_report(&connection("cli"))
            .expect("断开报告应成功");
        assert_eq!(disconnected[0].state, OperationState::NeedsInput);
        let answer_digest = format!("{:x}", Sha256::digest(b"yes"));
        let winner = queue
            .answer_elicitation(
                &operation("op-input"),
                "ask-1",
                operation("answer-1"),
                answer_digest.clone(),
            )
            .unwrap();
        assert_eq!(winner.disposition, ElicitationAnswerDisposition::Accepted);
        let duplicate = queue
            .answer_elicitation(
                &operation("op-input"),
                "ask-1",
                operation("answer-1"),
                answer_digest,
            )
            .unwrap();
        assert_eq!(
            duplicate.disposition,
            ElicitationAnswerDisposition::Duplicate
        );
        assert_eq!(
            queue.answer_elicitation(
                &operation("op-input"),
                "ask-1",
                operation("answer-2"),
                format!("{:x}", Sha256::digest(b"no")),
            ),
            Err(HostCoreError::ElicitationAlreadyAnswered)
        );
    }

    #[test]
    fn 恢复终态保留failed且未知执行阻塞新admission() {
        let queue = HostPromptQueue::with_defaults();
        let recovered_failed = RecoveredOperation {
            operation_id: operation("op-failed"),
            session_id: "session-recovery".to_owned(),
            request_fingerprint: "a".repeat(HOST_PAYLOAD_DIGEST_HEX_BYTES),
            execution: Some(execution("session-recovery", "op-failed")),
            terminal: Some(terminal("provider_failed")),
            terminal_state: Some(OperationState::Failed),
            elicitation_id: None,
            detached: false,
        };
        assert_eq!(
            queue.recover_terminal(recovered_failed).unwrap().state,
            OperationState::Failed
        );

        let recovered_unknown = RecoveredOperation {
            operation_id: operation("op-unknown"),
            session_id: "session-recovery".to_owned(),
            request_fingerprint: "b".repeat(HOST_PAYLOAD_DIGEST_HEX_BYTES),
            execution: Some(execution("session-recovery", "op-unknown")),
            terminal: None,
            terminal_state: None,
            elicitation_id: None,
            detached: true,
        };
        assert_eq!(
            queue.recover_unknown(recovered_unknown).unwrap().state,
            OperationState::RecoveryRequired
        );
        assert_eq!(
            queue.admit(request("web", "op-new", "session-recovery")),
            Err(HostCoreError::InvalidState)
        );
        queue.clear_recovery_block("session-recovery").unwrap();
        assert!(
            queue
                .admit(request("web", "op-new", "session-recovery"))
                .is_ok()
        );
    }
}
