//! Collaboration v2 的确定性并发与竞态测试。

use crate::collaboration::{
    AgentTreeQuiesceResult, AgentTurnStartResult, CollaborationAppendResult,
    CollaborationEventBatchId, MAX_AGENTS_PER_COORDINATOR, MAX_AGENTS_PER_ROOT,
    MAX_PORT_ERROR_BYTES, QuiesceAgentTree, RecoveredAgentCheckpoint, RecoveredCoordinator,
    RecoveredRootLifecycle, collaboration_event_batch,
};
use crate::{
    AgentDepth, AgentExecutionPort, AgentId, AgentPath, AgentProfile, AgentTurnCause,
    AgentTurnLaunch, AgentTurnOutcome, AgentTurnSignal, AgentTurnSignalKind, CloseAgentTree,
    CollaborationAgentStatus, CollaborationCoordinator, CollaborationError, CollaborationEvent,
    CollaborationEventKind, CollaborationIdGenerator, CollaborationInvocationKind,
    CollaborationLimits, CollaborationPortError, CollaborationStore, CollaborationTransitionCommit,
    ContextInheritance, MailboxDelivery, MailboxMessage, MailboxMessageId, PlanGuard,
    RecoveredAgentTree, RootAgentRequest, SessionId, SpawnAgentRequest, ToolCallId,
    TurnCompletionDisposition, TurnId, UserSteer, WaitAgentOutcome, WorktreeLease,
};
use keencode_model::{Message, MessageRole};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

/// 普通执行 Turn 使用的无 Plan 限制测试守卫。
const NO_PLAN: PlanGuard = PlanGuard::inactive();

/// 测试 Store 为稳定幂等键保存的完整事件批次。
#[derive(Clone)]
struct RecordedBatch {
    /// 批次要求的上一事件序号。
    expected_sequence: u64,
    /// 批次原子保存的完整事件。
    events: Vec<CollaborationEvent>,
    /// 与事件批次在同一提交边界保存的完整协调器 checkpoint。
    checkpoint: RecoveredCoordinator,
}

/// 严格校验事件序号并可注入冷恢复快照的测试 Store。
#[derive(Default)]
struct RecordingStore {
    /// 串行化水位、批次、事件和完整 checkpoint 的单一提交边界。
    commit_gate: Mutex<()>,
    /// 最近已原子追加的事件序号。
    sequence: Mutex<u64>,
    /// 按追加顺序保存的全部事件。
    events: Mutex<Vec<CollaborationEvent>>,
    /// 按稳定批次标识保存的幂等追加记录。
    batches: Mutex<HashMap<CollaborationEventBatchId, RecordedBatch>>,
    /// 最近一次事件提交原子携带的完整协调器 checkpoint。
    coordinator_checkpoint: Mutex<Option<RecoveredCoordinator>>,
    /// 按 Agent 标识索引的局部驱逐 checkpoint。
    recovered: Mutex<HashMap<AgentId, RecoveredAgentCheckpoint>>,
    /// 是否让下一批派发确认事件追加失败。
    fail_next_dispatch_ack: AtomicBool,
    /// 是否让下一批启动失败补偿事件追加失败。
    fail_next_failure_compensation: AtomicBool,
    /// 是否让下一批全树清理确认事件追加失败。
    fail_next_cleanup_ack: AtomicBool,
    /// 是否让下一批协作幂等提交在无副作用前明确失败。
    fail_next_collaboration_commit: AtomicBool,
    /// 是否让下一批事件在完整提交后返回一次不确定结果。
    commit_then_indeterminate: AtomicBool,
    /// 剩余需要连续返回且不产生副作用的不确定次数。
    indeterminate_without_commit: AtomicU64,
    /// 是否让下一批直接报告已提交但返回高于批末的历史水位。
    already_committed_ahead: AtomicBool,
}

impl RecordingStore {
    /// 返回已追加事件的快照。
    fn events(&self) -> Vec<CollaborationEvent> {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        self.events.lock().expect("事件锁不应中毒").clone()
    }

    /// 返回已按稳定键提交的原子事件批次快照。
    fn batches(&self) -> Vec<RecordedBatch> {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        self.batches
            .lock()
            .expect("批次锁不应中毒")
            .values()
            .cloned()
            .collect()
    }

    /// 将一棵根树快照同时挂到树内每个 Agent 标识上。
    fn install_recovery(&self, tree: RecoveredAgentTree) {
        let mut recovered = self.recovered.lock().expect("恢复锁不应中毒");
        for agent in &tree.agents {
            recovered.insert(
                agent.definition.agent_id.clone(),
                RecoveredAgentCheckpoint {
                    root_agent_id: tree.root_agent_id.clone(),
                    revision: 1,
                    agent: agent.clone(),
                },
            );
        }
    }

    /// 注入一次 StartTurn 派发确认持久化故障。
    fn fail_next_dispatch_ack(&self) {
        self.fail_next_dispatch_ack.store(true, Ordering::SeqCst);
    }

    /// 注入一次 StartTurn 失败补偿持久化故障。
    fn fail_next_failure_compensation(&self) {
        self.fail_next_failure_compensation
            .store(true, Ordering::SeqCst);
    }

    /// 注入一次 CloseTree 清理确认持久化故障。
    fn fail_next_cleanup_ack(&self) {
        self.fail_next_cleanup_ack.store(true, Ordering::SeqCst);
    }

    /// 注入一次协作幂等事件批次明确未提交故障。
    fn fail_next_collaboration_commit(&self) {
        self.fail_next_collaboration_commit
            .store(true, Ordering::SeqCst);
    }

    /// 注入一次先完整提交、再模拟连接中断的不确定结果。
    fn commit_then_indeterminate(&self) {
        self.commit_then_indeterminate.store(true, Ordering::SeqCst);
    }

    /// 注入指定次数不产生副作用且无法判定提交状态的结果。
    fn indeterminate_without_commit(&self, attempts: u64) {
        self.indeterminate_without_commit
            .store(attempts, Ordering::SeqCst);
    }

    /// 模拟其他协调器在 Store 中提交了一个后续事件。
    fn advance_sequence(&self) {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        let mut sequence = self.sequence.lock().expect("序号锁不应中毒");
        *sequence = sequence.checked_add(1).expect("测试 Store 水位不会耗尽");
    }

    /// 注入一次携带过高实时水位的历史批次提交结果。
    fn already_committed_ahead(&self) {
        self.already_committed_ahead.store(true, Ordering::SeqCst);
    }
}

impl CollaborationStore for RecordingStore {
    /// 返回测试 Store 当前事件水位。
    fn current_sequence(&self) -> Result<u64, CollaborationPortError> {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        Ok(*self.sequence.lock().expect("序号锁不应中毒"))
    }

    /// 返回最近一次与事件批次原子提交的协调器 checkpoint。
    fn load_coordinator_checkpoint(
        &self,
    ) -> Result<Option<RecoveredCoordinator>, CollaborationPortError> {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        Ok(self
            .coordinator_checkpoint
            .lock()
            .expect("协调器 checkpoint 锁不应中毒")
            .clone())
    }

    /// 校验稳定批次、期望序号与完整 checkpoint 后原子提交。
    fn commit_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        let _gate = self.commit_gate.lock().expect("提交门锁不应中毒");
        let batch = &commit.batch;
        if self.already_committed_ahead.swap(false, Ordering::SeqCst) {
            let current_sequence = batch
                .events
                .last()
                .map_or(batch.expected_sequence, |event| event.sequence)
                .checked_add(1)
                .expect("测试 Store 水位不会耗尽");
            return CollaborationAppendResult::AlreadyCommitted { current_sequence };
        }
        let mut batches = self.batches.lock().expect("批次锁不应中毒");
        if let Some(recorded) = batches.get(&batch.batch_id) {
            return if recorded.expected_sequence == batch.expected_sequence
                && recorded.events == batch.events
                && recorded.checkpoint == commit.checkpoint
            {
                CollaborationAppendResult::AlreadyCommitted {
                    current_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
                }
            } else {
                CollaborationAppendResult::Conflict {
                    actual_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
                }
            };
        }
        let mut remaining = self.indeterminate_without_commit.load(Ordering::SeqCst);
        while remaining > 0 {
            match self.indeterminate_without_commit.compare_exchange(
                remaining,
                remaining - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return CollaborationAppendResult::Indeterminate {
                        error: CollaborationPortError::new("测试未提交不确定故障"),
                    };
                }
                Err(actual) => remaining = actual,
            }
        }
        if batch.events.iter().any(|event| {
            matches!(
                event.kind,
                CollaborationEventKind::AgentTurnDispatchAcknowledged
            )
        }) && self.fail_next_dispatch_ack.swap(false, Ordering::SeqCst)
        {
            return CollaborationAppendResult::Absent {
                current_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
            };
        }
        if batch.events.iter().any(|event| {
            matches!(
                event.kind,
                CollaborationEventKind::CollaborationInvocationCommitted { .. }
            )
        }) && self
            .fail_next_collaboration_commit
            .swap(false, Ordering::SeqCst)
        {
            return CollaborationAppendResult::Absent {
                current_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
            };
        }
        if batch
            .events
            .iter()
            .any(|event| matches!(event.kind, CollaborationEventKind::AgentTurnFailed { .. }))
            && self
                .fail_next_failure_compensation
                .swap(false, Ordering::SeqCst)
        {
            return CollaborationAppendResult::Absent {
                current_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
            };
        }
        if batch.events.iter().any(|event| {
            matches!(
                event.kind,
                CollaborationEventKind::AgentTreeCleanupCompleted
            )
        }) && self.fail_next_cleanup_ack.swap(false, Ordering::SeqCst)
        {
            return CollaborationAppendResult::Absent {
                current_sequence: *self.sequence.lock().expect("序号锁不应中毒"),
            };
        }
        let mut sequence = self.sequence.lock().expect("序号锁不应中毒");
        if *sequence != batch.expected_sequence {
            return CollaborationAppendResult::Conflict {
                actual_sequence: *sequence,
            };
        }
        let mut next_sequence = *sequence;
        for event in &batch.events {
            let expected = next_sequence.checked_add(1).expect("测试事件序号不会耗尽");
            if event.sequence != expected {
                return CollaborationAppendResult::Absent {
                    current_sequence: *sequence,
                };
            }
            next_sequence = expected;
        }
        if commit.checkpoint.last_event_sequence != next_sequence {
            return CollaborationAppendResult::Absent {
                current_sequence: *sequence,
            };
        }
        *sequence = next_sequence;
        self.events
            .lock()
            .expect("事件锁不应中毒")
            .extend_from_slice(&batch.events);
        batches.insert(
            batch.batch_id.clone(),
            RecordedBatch {
                expected_sequence: batch.expected_sequence,
                events: batch.events.clone(),
                checkpoint: commit.checkpoint.clone(),
            },
        );
        *self
            .coordinator_checkpoint
            .lock()
            .expect("协调器 checkpoint 锁不应中毒") = Some(commit.checkpoint.clone());
        if self.commit_then_indeterminate.swap(false, Ordering::SeqCst) {
            CollaborationAppendResult::Indeterminate {
                error: CollaborationPortError::new("测试提交后连接中断"),
            }
        } else {
            CollaborationAppendResult::Appended
        }
    }

    /// 返回预先注入的冷恢复树。
    fn load_agent_checkpoint(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<RecoveredAgentCheckpoint>, CollaborationPortError> {
        Ok(self
            .recovered
            .lock()
            .expect("恢复锁不应中毒")
            .get(agent_id)
            .cloned())
    }

    /// 原子保存驱逐前的单 Agent 局部 checkpoint。
    fn save_agent_checkpoint(
        &self,
        checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<(), CollaborationPortError> {
        let mut recovered = self.recovered.lock().expect("恢复锁不应中毒");
        let agent_id = checkpoint.agent.definition.agent_id.clone();
        if let Some(previous) = recovered.get(&agent_id) {
            if previous.revision > checkpoint.revision
                || (previous.revision == checkpoint.revision && previous != checkpoint)
            {
                return Err(CollaborationPortError::new("checkpoint 局部修订号冲突"));
            }
        }
        recovered.insert(agent_id, checkpoint.clone());
        Ok(())
    }
}

/// 只记录非阻塞调度、安全边界信号与全树清理的测试执行端口。
#[derive(Default)]
struct RecordingExecution {
    /// 已接收的 Turn 启动请求。
    launches: Mutex<Vec<AgentTurnLaunch>>,
    /// 已按 TurnId 接受且不会重复创建副作用的启动集合。
    accepted_turns: Mutex<HashSet<TurnId>>,
    /// 已接收的安全边界信号。
    signals: Mutex<Vec<AgentTurnSignal>>,
    /// 已接收并确认的全树静止请求。
    quiesces: Mutex<Vec<QuiesceAgentTree>>,
    /// 全树静止端口被调用的总次数，包含被拒绝的请求。
    quiesce_attempts: AtomicU64,
    /// 已接收的全树清理请求。
    closes: Mutex<Vec<CloseAgentTree>>,
    /// 是否让下一次 Turn 启动返回故障。
    fail_next_start: AtomicBool,
    /// 下一次 Turn 启动需要返回的指定永久错误文本。
    next_start_error: Mutex<Option<String>>,
    /// 是否让全部 Turn 启动持续返回故障。
    fail_all_starts: AtomicBool,
    /// 是否让下一次 Turn 启动返回可重试的不确定结果且不创建任务。
    unknown_next_start: AtomicBool,
    /// 是否让下一次 Turn 启动先创建任务再返回可重试的不确定结果。
    accept_then_unknown: AtomicBool,
    /// 是否让下一次安全边界信号返回故障。
    fail_next_signal: AtomicBool,
    /// 是否让下一次全树清理返回故障。
    fail_next_close: AtomicBool,
    /// 是否让下一次全树静止返回可重试不确定结果。
    unknown_next_quiesce: AtomicBool,
    /// 是否让全部全树静止请求永久失败。
    reject_all_quiesces: AtomicBool,
}

impl RecordingExecution {
    /// 返回已接收启动请求的快照。
    fn launches(&self) -> Vec<AgentTurnLaunch> {
        self.launches.lock().expect("启动锁不应中毒").clone()
    }

    /// 返回已接收安全边界信号的快照。
    fn signals(&self) -> Vec<AgentTurnSignal> {
        self.signals.lock().expect("信号锁不应中毒").clone()
    }

    /// 返回已接收全树清理请求的快照。
    fn closes(&self) -> Vec<CloseAgentTree> {
        self.closes.lock().expect("关闭锁不应中毒").clone()
    }

    /// 返回已确认静止请求的快照。
    fn quiesces(&self) -> Vec<QuiesceAgentTree> {
        self.quiesces.lock().expect("静止锁不应中毒").clone()
    }

    /// 返回全树静止端口的累计调用次数。
    fn quiesce_attempts(&self) -> u64 {
        self.quiesce_attempts.load(Ordering::SeqCst)
    }

    /// 按 Turn 标识查找已发送的启动请求。
    fn launch(&self, turn_id: &TurnId) -> AgentTurnLaunch {
        self.launches()
            .into_iter()
            .find(|launch| &launch.turn_id == turn_id)
            .expect("指定 Turn 应已启动")
    }

    /// 注入一次 Turn 启动故障。
    fn fail_next_start(&self) {
        self.fail_next_start.store(true, Ordering::SeqCst);
    }

    /// 注入一次携带指定文本的 Turn 启动永久拒绝。
    fn fail_next_start_with(&self, message: String) {
        *self.next_start_error.lock().expect("启动错误锁不应中毒") = Some(message);
    }

    /// 注入持续 Turn 启动故障，验证补偿不会形成无界链。
    fn fail_all_starts(&self) {
        self.fail_all_starts.store(true, Ordering::SeqCst);
    }

    /// 注入一次没有创建任务的可重试不确定启动结果。
    fn unknown_next_start(&self) {
        self.unknown_next_start.store(true, Ordering::SeqCst);
    }

    /// 注入一次任务已创建但响应边界不确定的启动结果。
    fn accept_then_unknown(&self) {
        self.accept_then_unknown.store(true, Ordering::SeqCst);
    }

    /// 注入一次安全边界信号故障。
    fn fail_next_signal(&self) {
        self.fail_next_signal.store(true, Ordering::SeqCst);
    }

    /// 注入一次全树清理故障。
    fn fail_next_close(&self) {
        self.fail_next_close.store(true, Ordering::SeqCst);
    }

    /// 注入一次无法判断是否已经静止的结果。
    fn unknown_next_quiesce(&self) {
        self.unknown_next_quiesce.store(true, Ordering::SeqCst);
    }

    /// 注入持续静止拒绝，模拟无法停止的 runner 或受管子进程。
    fn reject_all_quiesces(&self) {
        self.reject_all_quiesces.store(true, Ordering::SeqCst);
    }
}

impl AgentExecutionPort for RecordingExecution {
    /// 按 TurnId 幂等记录启动请求并返回明确的副作用边界。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
        if let Some(message) = self
            .next_start_error
            .lock()
            .expect("启动错误锁不应中毒")
            .take()
        {
            return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                error: CollaborationPortError::new(message),
            };
        }
        if self.fail_all_starts.load(Ordering::SeqCst)
            || self.fail_next_start.swap(false, Ordering::SeqCst)
        {
            return AgentTurnStartResult::PermanentRejectedBeforeSideEffect {
                error: CollaborationPortError::new("测试 Turn 启动故障"),
            };
        }
        if self.unknown_next_start.swap(false, Ordering::SeqCst) {
            return AgentTurnStartResult::RetryableUnknown {
                error: CollaborationPortError::new("测试 Turn 启动结果不确定"),
            };
        }
        let mut accepted = self.accepted_turns.lock().expect("启动去重锁不应中毒");
        if !accepted.insert(launch.turn_id.clone()) {
            return AgentTurnStartResult::AlreadyAccepted;
        }
        self.launches.lock().expect("启动锁不应中毒").push(launch);
        if self.accept_then_unknown.swap(false, Ordering::SeqCst) {
            AgentTurnStartResult::RetryableUnknown {
                error: CollaborationPortError::new("测试 Turn 已接受但响应中断"),
            }
        } else {
            AgentTurnStartResult::Accepted
        }
    }

    /// 记录安全边界信号。
    fn signal_turn(&self, signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
        if self.fail_next_signal.swap(false, Ordering::SeqCst) {
            return Err(CollaborationPortError::new("测试 Turn 信号故障"));
        }
        self.signals.lock().expect("信号锁不应中毒").push(signal);
        Ok(())
    }

    /// 按根身份幂等确认全树静止，并支持故障注入。
    fn quiesce_tree(&self, request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
        self.quiesce_attempts.fetch_add(1, Ordering::SeqCst);
        if self.reject_all_quiesces.load(Ordering::SeqCst) {
            return AgentTreeQuiesceResult::PermanentRejectedBeforeQuiesce {
                error: CollaborationPortError::new("测试 stubborn runner 拒绝静止"),
            };
        }
        if self.unknown_next_quiesce.swap(false, Ordering::SeqCst) {
            return AgentTreeQuiesceResult::RetryableUnknown {
                error: CollaborationPortError::new("测试全树静止结果不确定"),
            };
        }
        let mut quiesces = self.quiesces.lock().expect("静止锁不应中毒");
        if quiesces
            .iter()
            .any(|existing| existing.root_agent_id == request.root_agent_id)
        {
            AgentTreeQuiesceResult::AlreadyQuiesced
        } else {
            quiesces.push(request);
            AgentTreeQuiesceResult::Quiesced
        }
    }

    /// 记录全树清理请求。
    fn close_tree(&self, request: CloseAgentTree) -> Result<(), CollaborationPortError> {
        if self.fail_next_close.swap(false, Ordering::SeqCst) {
            return Err(CollaborationPortError::new("测试全树清理故障"));
        }
        self.closes.lock().expect("关闭锁不应中毒").push(request);
        Ok(())
    }
}

/// 在真正产生 StartTurn 副作用前阻塞，用于确定性复现关闭竞态的执行端口。
struct BlockingStartExecution {
    /// 测试线程等待 StartTurn 已通过协调器检查的位置。
    start_entered: Arc<Barrier>,
    /// 测试线程允许 StartTurn 继续产生副作用的栅栏。
    release_start: Arc<Barrier>,
    /// 按实际副作用发生顺序记录 start 与 close。
    events: Mutex<Vec<&'static str>>,
    /// 已按 TurnId 接受的启动集合。
    accepted_turns: Mutex<HashSet<TurnId>>,
    /// 是否让下一次 StartTurn 在执行端口边界返回不确定结果，以留下可重试 outbox。
    unknown_next_start: AtomicBool,
    /// 是否让下一次 SignalTurn 返回故障，以留下可重试 outbox。
    fail_next_signal: AtomicBool,
    /// 最近进入阻塞 StartTurn 的根 Agent 身份。
    blocked_root: Mutex<Option<AgentId>>,
    /// 已真正到达执行端口的安全边界信号。
    signals: Mutex<Vec<AgentTurnSignal>>,
}

impl BlockingStartExecution {
    /// 创建一对由测试线程参与的双向栅栏。
    fn new() -> Self {
        Self {
            start_entered: Arc::new(Barrier::new(2)),
            release_start: Arc::new(Barrier::new(2)),
            events: Mutex::new(Vec::new()),
            accepted_turns: Mutex::new(HashSet::new()),
            unknown_next_start: AtomicBool::new(false),
            fail_next_signal: AtomicBool::new(false),
            blocked_root: Mutex::new(None),
            signals: Mutex::new(Vec::new()),
        }
    }

    /// 返回执行端口观察到的副作用顺序。
    fn events(&self) -> Vec<&'static str> {
        self.events.lock().expect("竞态事件锁不应中毒").clone()
    }

    /// 注入一次没有产生执行副作用的可重试 StartTurn 结果。
    fn unknown_next_start(&self) {
        self.unknown_next_start.store(true, Ordering::SeqCst);
    }

    /// 注入一次不产生执行副作用的 SignalTurn 故障。
    fn fail_next_signal(&self) {
        self.fail_next_signal.store(true, Ordering::SeqCst);
    }

    /// 返回当前被阻塞 StartTurn 的根 Agent 身份。
    fn blocked_root(&self) -> AgentId {
        self.blocked_root
            .lock()
            .expect("阻塞根身份锁不应中毒")
            .clone()
            .expect("应已有一个被阻塞的 StartTurn")
    }

    /// 返回已真正接受 StartTurn 的 Turn 身份集合。
    fn accepted_turns(&self) -> HashSet<TurnId> {
        self.accepted_turns
            .lock()
            .expect("竞态去重锁不应中毒")
            .clone()
    }

    /// 返回已真正送达执行端的 SignalTurn 快照。
    fn signals(&self) -> Vec<AgentTurnSignal> {
        self.signals.lock().expect("竞态信号锁不应中毒").clone()
    }
}

impl AgentExecutionPort for BlockingStartExecution {
    /// 阻塞到测试显式放行后才登记启动副作用。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
        if self
            .accepted_turns
            .lock()
            .expect("竞态去重锁不应中毒")
            .contains(&launch.turn_id)
        {
            return AgentTurnStartResult::AlreadyAccepted;
        }
        if self.unknown_next_start.swap(false, Ordering::SeqCst) {
            return AgentTurnStartResult::RetryableUnknown {
                error: CollaborationPortError::new("测试 Turn 启动结果不确定"),
            };
        }
        *self.blocked_root.lock().expect("阻塞根身份锁不应中毒") =
            Some(launch.agent.root_agent_id.clone());
        self.start_entered.wait();
        self.release_start.wait();
        self.accepted_turns
            .lock()
            .expect("竞态去重锁不应中毒")
            .insert(launch.turn_id);
        self.events
            .lock()
            .expect("竞态事件锁不应中毒")
            .push("start");
        AgentTurnStartResult::Accepted
    }

    /// 竞态测试不需要阻塞安全边界信号，但需要记录真正送达的信号。
    fn signal_turn(&self, signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
        if self.fail_next_signal.swap(false, Ordering::SeqCst) {
            return Err(CollaborationPortError::new("测试 Turn 信号故障"));
        }
        self.signals
            .lock()
            .expect("竞态信号锁不应中毒")
            .push(signal);
        Ok(())
    }

    /// 阻塞启动完成后立即确认根树已经静止。
    fn quiesce_tree(&self, _request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
        AgentTreeQuiesceResult::Quiesced
    }

    /// 记录全树清理副作用，供测试断言其严格发生在 start 之后。
    fn close_tree(&self, _request: CloseAgentTree) -> Result<(), CollaborationPortError> {
        self.events
            .lock()
            .expect("竞态事件锁不应中毒")
            .push("close");
        Ok(())
    }
}

/// 使并发测试也只生成唯一可预测前缀的标识生成器。
#[derive(Default)]
struct SequentialIds {
    /// 为所有标识类型共享的单调计数器。
    next: AtomicU64,
}

impl SequentialIds {
    /// 原子分配下一个正整数。
    fn number(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl CollaborationIdGenerator for SequentialIds {
    /// 生成可预测 Agent 标识。
    fn next_agent_id(&self) -> AgentId {
        AgentId::new(format!("agent-{}", self.number())).expect("Agent 标识非空")
    }

    /// 生成可预测 Session 标识。
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("session-{}", self.number())).expect("Session 标识非空")
    }

    /// 生成可预测 mailbox 消息标识。
    fn next_message_id(&self) -> MailboxMessageId {
        MailboxMessageId::new(format!("message-{}", self.number())).expect("消息标识非空")
    }
}

/// 始终返回同一 AgentId、其余身份仍唯一的关闭墓碑测试生成器。
struct RepeatingAgentIds {
    /// 每次 Agent 分配都返回的固定标识。
    agent_id: AgentId,
    /// Session 与消息标识共享的单调计数器。
    next: AtomicU64,
}

impl CollaborationIdGenerator for RepeatingAgentIds {
    /// 返回固定 Agent 标识以验证关闭根身份不能复用。
    fn next_agent_id(&self) -> AgentId {
        self.agent_id.clone()
    }

    /// 生成唯一 Session 标识。
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!(
            "tombstone-session-{}",
            self.next.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .expect("墓碑测试 Session 标识非空")
    }

    /// 生成唯一 mailbox 消息标识。
    fn next_message_id(&self) -> MailboxMessageId {
        MailboxMessageId::new(format!(
            "tombstone-message-{}",
            self.next.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .expect("墓碑测试消息标识非空")
    }
}

/// 测试使用的领域端口和根 Agent 组合。
struct Fixture {
    /// 被测协调器。
    coordinator: Arc<CollaborationCoordinator>,
    /// 可检查事件和注入恢复快照的 Store。
    store: Arc<RecordingStore>,
    /// 可检查启动、取消和清理请求的执行端口。
    execution: Arc<RecordingExecution>,
    /// 已注册的根 Agent 标识。
    root_agent_id: AgentId,
}

/// 创建指定双层容量的测试协调器和根 Agent。
fn fixture(global_limit: usize, root_limit: usize) -> Fixture {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(global_limit).expect("测试容量有效"),
        store.clone(),
        execution.clone(),
        Arc::new(SequentialIds::default()),
    ));
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("root-session").expect("根 Session 标识非空"),
            profile: profile("root"),
            per_root_turn_limit: root_limit,
        })
        .expect("根 Agent 应注册成功");
    Fixture {
        coordinator,
        store,
        execution,
        root_agent_id: root.agent_id,
    }
}

/// 创建无外部依赖的独立 Agent 运行配置。
fn profile(name: &str) -> AgentProfile {
    AgentProfile {
        model: format!("model-{name}"),
        reasoning_effort: Some("medium".to_owned()),
        plan_guard: NO_PLAN,
        cwd: PathBuf::from(format!("D:/workspace/{name}")),
        worktree_lease: None,
        tool_snapshot: vec!["read_file".to_owned(), "run_command".to_owned()],
    }
}

/// 创建一个继承最近两个 Turn 的子 Agent 请求。
fn spawn_request(name: &str) -> SpawnAgentRequest {
    SpawnAgentRequest {
        task_name: name.to_owned(),
        initial_task: format!("执行 {name} 任务"),
        context_inheritance: ContextInheritance::RecentTurns { count: 2 },
        context_snapshot: Vec::new(),
        agent_template: None,
        profile: profile(name),
    }
}

/// 将 Provider 中立消息编码为核心要求的规范 JSON 快照。
fn context_message(role: MessageRole, text: &str) -> String {
    serde_json::to_string(&Message::text(role, text)).expect("测试上下文消息应可规范编码")
}

/// 模拟 Runtime 在 Transcript 提交成功后确认 mailbox claim。
fn acknowledge_mailbox_batch(
    coordinator: &CollaborationCoordinator,
    agent_id: &AgentId,
    turn_id: &TurnId,
    messages: &[MailboxMessage],
) {
    if let Some(last) = messages.last() {
        coordinator
            .acknowledge_mailbox(agent_id, turn_id, last.sequence)
            .expect("mailbox claim 应确认成功");
    }
}

/// 模拟 Runtime 在 Transcript 提交成功后确认用户 steer claim。
fn acknowledge_steer_batch(
    coordinator: &CollaborationCoordinator,
    agent_id: &AgentId,
    turn_id: &TurnId,
    steers: &[UserSteer],
) {
    if let Some(last) = steers.last() {
        coordinator
            .acknowledge_user_steers(agent_id, turn_id, last.sequence)
            .expect("用户 steer claim 应确认成功");
    }
}

/// 外部根 Turn 标识必须可直接启动、严格重放，并在冷恢复后保留因果绑定。
#[test]
fn external_root_turn_is_strictly_idempotent_and_restorable() {
    let fixture = fixture(2, 2);
    let external_turn = TurnId::new("desktop-turn-42").unwrap();
    let first = fixture
        .coordinator
        .begin_root_turn_with_id(
            &fixture.root_agent_id,
            external_turn.clone(),
            "外部根任务",
            NO_PLAN,
        )
        .unwrap();
    assert_eq!(first, external_turn);
    let root_launch = fixture.execution.launch(&external_turn);
    assert_eq!(root_launch.root_turn_id, external_turn);
    assert_eq!(root_launch.parent_turn_id, None);
    let event_count = fixture.store.events().len();
    let launch_count = fixture.execution.launches().len();

    assert_eq!(
        fixture
            .coordinator
            .begin_root_turn_with_id(
                &fixture.root_agent_id,
                external_turn.clone(),
                "外部根任务",
                NO_PLAN,
            )
            .unwrap(),
        external_turn
    );
    assert_eq!(fixture.store.events().len(), event_count);
    assert_eq!(fixture.execution.launches().len(), launch_count);
    assert!(matches!(
        fixture.coordinator.begin_root_turn_with_id(
            &fixture.root_agent_id,
            external_turn.clone(),
            "被篡改的外部根任务",
            NO_PLAN,
        ),
        Err(CollaborationError::IdentifierCollision { kind: "Root Turn" })
    ));

    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &external_turn,
            &fixed_tool_call_id("external-root-spawn"),
            spawn_request("external_root_child"),
        )
        .unwrap();
    let child_launch = fixture.execution.launch(&child.initial_turn_id);
    assert_eq!(child_launch.parent_turn_id, Some(external_turn.clone()));
    assert_eq!(child_launch.root_turn_id, external_turn.clone());

    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = restore_coordinator(fixture.store.clone(), 2, 42_000);
    restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(
        restored
            .begin_root_turn_with_id(
                &fixture.root_agent_id,
                external_turn.clone(),
                "外部根任务",
                NO_PLAN,
            )
            .unwrap(),
        external_turn
    );
}

/// Journal 尚未形成 TurnStarted 时，冷恢复留下的 Interrupted 外部根 Turn 可以同 ID 重试。
#[test]
fn unstarted_external_root_turn_can_retry_without_duplicate_dispatch() {
    let fixture = fixture(2, 2);
    let external_turn = TurnId::new("desktop-turn-retry-42").unwrap();
    fixture
        .coordinator
        .begin_root_turn_with_id(
            &fixture.root_agent_id,
            external_turn.clone(),
            "崩溃前尚未写入 Journal",
            NO_PLAN,
        )
        .unwrap();
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored_execution = Arc::new(RecordingExecution::default());
    let restored = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        fixture.store.clone(),
        restored_execution.clone(),
        Arc::new(SequentialIds {
            next: AtomicU64::new(42_100),
        }),
    ));
    restored.restore_coordinator(checkpoint).unwrap();
    assert!(matches!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id } if turn_id == external_turn
    ));
    let launch_count = restored_execution.launches().len();

    assert_eq!(
        restored
            .retry_unstarted_root_turn_with_id(
                &fixture.root_agent_id,
                external_turn.clone(),
                "崩溃前尚未写入 Journal",
                NO_PLAN,
            )
            .unwrap(),
        external_turn
    );
    assert_eq!(restored_execution.launches().len(), launch_count + 1);
    assert!(matches!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == external_turn
    ));
}

/// 外部根 Turn 不得占用协调器保留的内部单调 Turn 命名空间。
#[test]
fn external_root_turn_rejects_internal_turn_namespace() {
    let fixture = fixture(1, 1);
    let reserved = TurnId::new(format!("turn/{}/999", fixture.root_agent_id)).unwrap();
    assert!(matches!(
        fixture.coordinator.begin_root_turn_with_id(
            &fixture.root_agent_id,
            reserved,
            "不得占用内部身份",
            NO_PLAN,
        ),
        Err(CollaborationError::IdentifierCollision { kind: "Root Turn" })
    ));
}

/// 上下文策略与规范消息必须在 spawn 时冻结，并参与严格幂等摘要。
#[test]
fn context_snapshot_is_frozen_validated_and_idempotent() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "冻结上下文", NO_PLAN)
        .unwrap();
    let snapshot = vec![
        context_message(MessageRole::User, "父问题"),
        context_message(MessageRole::Assistant, "父回答"),
    ];
    let mut request = spawn_request("context_child");
    request.context_inheritance = ContextInheritance::All;
    request.context_snapshot = snapshot.clone();
    let tool_call_id = fixed_tool_call_id("context-snapshot-spawn");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            request.clone(),
        )
        .unwrap();
    assert_eq!(
        fixture
            .execution
            .launch(&child.initial_turn_id)
            .agent
            .context_snapshot,
        snapshot
    );

    let mut changed = request;
    changed.context_snapshot[1] = context_message(MessageRole::Assistant, "被篡改的父回答");
    assert!(matches!(
        fixture
            .coordinator
            .spawn_agent(&fixture.root_agent_id, &root_turn, &tool_call_id, changed,),
        Err(CollaborationError::IdempotencyConflict { .. })
    ));

    let mut forbidden_none = spawn_request("none_context_child");
    forbidden_none.context_inheritance = ContextInheritance::None;
    forbidden_none.context_snapshot = vec![context_message(MessageRole::User, "不得继承")];
    assert!(matches!(
        fixture.coordinator.spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &fixed_tool_call_id("none-context-spawn"),
            forbidden_none,
        ),
        Err(CollaborationError::InvalidContextInheritance)
    ));
}

/// Plan 只读守卫必须从父 Agent 向子 Agent 单调收紧，并完整进入恢复快照。
#[test]
fn child_profile_inherits_read_only_plan_across_restore_and_replay() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        store.clone(),
        execution.clone(),
        Arc::new(SequentialIds::default()),
    );
    let root_profile = profile("plan-root");
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("plan-root-session").unwrap(),
            profile: root_profile,
            per_root_turn_limit: 2,
        })
        .unwrap();
    let root_turn = coordinator
        .begin_root_turn(&root.agent_id, "只读父任务", PlanGuard::read_only())
        .unwrap();
    assert_eq!(
        execution.launch(&root_turn).plan_guard,
        PlanGuard::read_only()
    );
    let tool_call_id = fixed_tool_call_id("plan-profile-spawn");
    let mut request = spawn_request("plan_child");
    request.profile.plan_guard = NO_PLAN;
    let spawned = coordinator
        .spawn_agent(&root.agent_id, &root_turn, &tool_call_id, request.clone())
        .unwrap();
    let launch = execution.launch(&spawned.initial_turn_id);
    assert_eq!(launch.plan_guard, PlanGuard::read_only());
    assert_eq!(launch.agent.profile.plan_guard, PlanGuard::read_only());

    let mut replay_with_changed_modes = request;
    replay_with_changed_modes.profile.plan_guard = PlanGuard::read_only();
    assert!(matches!(
        coordinator
            .spawn_agent(
                &root.agent_id,
                &root_turn,
                &tool_call_id,
                replay_with_changed_modes,
            )
            .unwrap_err(),
        CollaborationError::IdempotencyConflict { .. }
    ));

    let checkpoint = coordinator.checkpoint_coordinator().unwrap();
    let mut missing_turn_plan = checkpoint.clone();
    missing_turn_plan
        .roots
        .iter_mut()
        .flat_map(|tree| &mut tree.agents)
        .find(|agent| agent.definition.agent_id == spawned.agent.agent_id)
        .expect("伪造快照应包含子 Agent")
        .current_plan_guard = None;
    assert!(matches!(
        restore_coordinator(store.clone(), 2, 43_000)
            .restore_coordinator(missing_turn_plan)
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
    let mut weakened_turn_plan = checkpoint.clone();
    weakened_turn_plan
        .roots
        .iter_mut()
        .flat_map(|tree| &mut tree.agents)
        .find(|agent| agent.definition.agent_id == spawned.agent.agent_id)
        .expect("伪造快照应包含子 Agent")
        .current_plan_guard = Some(NO_PLAN);
    assert!(matches!(
        restore_coordinator(store.clone(), 2, 43_500)
            .restore_coordinator(weakened_turn_plan)
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let restored = restore_coordinator(store, 2, 44_000);
    restored.restore_coordinator(checkpoint).unwrap();
    let restored_child = restored
        .checkpoint_root(&root.agent_id)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == spawned.agent.agent_id)
        .expect("恢复快照应保留子 Agent 定义");
    assert_eq!(
        restored_child.definition.profile.plan_guard,
        PlanGuard::read_only()
    );
}

/// 普通父 Agent 必须允许子 Agent 主动选择更严格的只读计划守卫。
#[test]
fn child_profile_preserves_requested_read_only_plan() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        store,
        execution.clone(),
        Arc::new(SequentialIds::default()),
    );
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("full-root-session").unwrap(),
            profile: profile("full-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let root_turn = coordinator
        .begin_root_turn(&root.agent_id, "普通父任务", NO_PLAN)
        .unwrap();
    let mut request = spawn_request("strict_child");
    request.profile.plan_guard = PlanGuard::read_only();
    let spawned = coordinator
        .spawn_agent(&root.agent_id, &root_turn, &next_tool_call_id(), request)
        .unwrap();
    let launch = execution.launch(&spawned.initial_turn_id);
    assert_eq!(launch.agent.profile.plan_guard, PlanGuard::read_only());
}

/// 已有子 Agent 在根 Plan Turn 中收到 Followup 时必须继续继承只读守卫。
#[test]
fn followup_inherits_current_source_turn_plan_guard_for_existing_child() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "普通根任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("followup_plan_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();

    let plan_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "只读根任务", PlanGuard::read_only())
        .unwrap();
    let (_, followup_turn) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &plan_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "只读 Followup",
        )
        .unwrap();
    let followup_turn = followup_turn.expect("空闲子 Agent 应创建 Followup Turn");
    assert_eq!(
        fixture.execution.launch(&followup_turn).plan_guard,
        PlanGuard::read_only()
    );
}

/// 已有失败子 Agent 在根 Plan Turn 中重试时必须继续继承只读守卫。
#[test]
fn retry_inherits_current_source_turn_plan_guard_for_existing_child() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "普通根任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("retry_plan_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Failed {
                message: "测试失败".to_owned(),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();

    let plan_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "只读根任务", PlanGuard::read_only())
        .unwrap();
    let retry_turn = fixture
        .coordinator
        .retry_agent(&fixture.root_agent_id, &plan_turn, &child.agent.agent_id)
        .unwrap();
    assert_eq!(
        fixture.execution.launch(&retry_turn).plan_guard,
        PlanGuard::read_only()
    );
}

/// 根 Session 的 Plan 状态按 Turn 冻结，关闭 Plan 后的新 Turn 不得沿用旧只读状态。
#[test]
fn root_turn_plan_guard_can_change_between_turns() {
    let fixture = fixture(1, 1);
    let plan_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "只读调研", PlanGuard::read_only())
        .unwrap();
    assert_eq!(
        fixture.execution.launch(&plan_turn).plan_guard,
        PlanGuard::read_only()
    );
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &plan_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();

    let implementation_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "开始实施", NO_PLAN)
        .unwrap();
    assert_eq!(
        fixture.execution.launch(&implementation_turn).plan_guard,
        NO_PLAN
    );
}

/// 全局并发上限提升后应立即调度既有等待 Turn，降低时不得取消已占用槽位。
#[test]
fn global_turn_limit_hot_update_schedules_and_throttles_without_cancelling() {
    let fixture = fixture(1, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "父任务", NO_PLAN)
        .expect("根 Turn 应占用首个全局槽位");
    let first_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("first_hot_limit_child"),
        )
        .expect("首个子 Agent 应持久排队");
    let second_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("second_hot_limit_child"),
        )
        .expect("第二个子 Agent 应持久排队");

    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&first_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));
    let raised = fixture
        .coordinator
        .update_global_turn_limit(2)
        .expect("提高上限应立即调度等待 Turn");
    assert_eq!(raised.global_limit, 2);
    assert_eq!(raised.global_in_use, 2);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&first_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));

    let lowered = fixture
        .coordinator
        .update_global_turn_limit(1)
        .expect("降低上限不得取消既有 Turn");
    assert_eq!(lowered.global_limit, 1);
    assert_eq!(lowered.global_in_use, 2);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&second_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&first_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert_eq!(
        fixture.coordinator.update_global_turn_limit(0).unwrap_err(),
        CollaborationError::InvalidTurnLimit
    );
}

/// 并发提高和降低全局上限必须串行化，且每个已排队 Turn 最多启动一次。
#[test]
fn concurrent_global_turn_limit_updates_keep_single_start_and_valid_capacity() {
    let fixture = fixture(1, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "并发父任务", NO_PLAN)
        .expect("根 Turn 应启动");
    let first_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("first_concurrent_limit_child"),
        )
        .expect("首个子 Agent 应排队");
    let second_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("second_concurrent_limit_child"),
        )
        .expect("第二个子 Agent 应排队");
    let barrier = Arc::new(Barrier::new(3));
    let raise_coordinator = fixture.coordinator.clone();
    let raise_barrier = barrier.clone();
    let raised = std::thread::spawn(move || {
        raise_barrier.wait();
        raise_coordinator.update_global_turn_limit(3)
    });
    let lower_coordinator = fixture.coordinator.clone();
    let lower_barrier = barrier.clone();
    let lowered = std::thread::spawn(move || {
        lower_barrier.wait();
        lower_coordinator.update_global_turn_limit(2)
    });
    barrier.wait();
    raised.join().expect("提高线程不应 panic").unwrap();
    lowered.join().expect("降低线程不应 panic").unwrap();

    let capacity = fixture.coordinator.capacity().unwrap();
    assert!(matches!(capacity.global_limit, 2 | 3));
    assert_eq!(capacity.global_in_use, 3);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&first_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&second_child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    let launches = fixture.execution.launches();
    assert_eq!(
        launches
            .iter()
            .filter(|launch| launch.turn_id == first_child.initial_turn_id)
            .count(),
        1
    );
    assert_eq!(
        launches
            .iter()
            .filter(|launch| launch.turn_id == second_child.initial_turn_id)
            .count(),
        1
    );
}

/// 为互不相关的既有测试调用分配唯一可信 ToolCall 身份。
fn next_tool_call_id() -> ToolCallId {
    static NEXT_TOOL_CALL_ID: AtomicU64 = AtomicU64::new(1);
    ToolCallId::new(format!(
        "collaboration-test-call-{}",
        NEXT_TOOL_CALL_ID.fetch_add(1, Ordering::SeqCst)
    ))
    .expect("测试 ToolCall 标识应当非空且有界")
}

/// 创建可以跨重放复用的固定可信 ToolCall 身份。
fn fixed_tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("固定测试 ToolCall 标识应当非空且有界")
}

/// 验证 SpawnAgent 立即返回、单层守卫，以及父 Turn 可独立先于子 Agent 完成。
#[test]
fn spawn_is_immediate_single_layer_and_parent_can_finish_before_child() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "父任务", NO_PLAN)
        .expect("根 Turn 应启动");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("review"),
        )
        .expect("SpawnAgent 应立即返回");

    assert_eq!(child.agent.path, AgentPath::parse("/root/review").unwrap());
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: child.initial_turn_id.clone()
        }
    );
    let child_launch = fixture.execution.launch(&child.initial_turn_id);
    assert!(!child_launch.capabilities.can_spawn_agent);
    assert_eq!(
        fixture
            .coordinator
            .spawn_agent(
                &child.agent.agent_id,
                &child.initial_turn_id,
                &next_tool_call_id(),
                spawn_request("nested"),
            )
            .unwrap_err(),
        CollaborationError::RecursiveSpawnForbidden {
            source_agent_id: child.agent.agent_id.clone()
        }
    );

    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("父先完成".to_owned())
                }
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Completed {
            turn_id: root_turn.clone(),
            final_message: Some("父先完成".to_owned()),
        }
    );
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert!(
        fixture
            .coordinator
            .mailbox(&fixture.root_agent_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("审查完成".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Completed {
            turn_id: root_turn.clone(),
            final_message: Some("父先完成".to_owned()),
        }
    );
    let mailbox = fixture.coordinator.mailbox(&fixture.root_agent_id).unwrap();
    assert_eq!(mailbox.len(), 1);
    assert_eq!(mailbox[0].delivery, MailboxDelivery::QueueOnly);
    assert_eq!(fixture.execution.launches().len(), 2);

    let events = fixture.store.events();
    let parent_completed_position = events
        .iter()
        .position(|event| {
            event.agent_id == fixture.root_agent_id
                && matches!(
                    event.kind,
                    CollaborationEventKind::AgentTurnCompleted { .. }
                )
        })
        .expect("父 Turn 应在子 Turn 结束前独立完成");
    let child_completed_position = events
        .iter()
        .position(|event| {
            event.agent_id == child.agent.agent_id
                && matches!(
                    event.kind,
                    CollaborationEventKind::AgentTurnCompleted { .. }
                )
        })
        .expect("子 Turn 完成事件应存在");
    let mailbox_position = events
        .iter()
        .position(|event| {
            event.agent_id == fixture.root_agent_id
                && matches!(
                    event.kind,
                    CollaborationEventKind::AgentMessageQueued { .. }
                )
        })
        .expect("父 mailbox 入队事件应存在");
    assert!(parent_completed_position < child_completed_position);
    assert!(child_completed_position < mailbox_position);
    assert!(
        events
            .windows(2)
            .all(|window| window[1].sequence == window[0].sequence + 1)
    );
}

/// 父 Turn 完成后立即释放槽位并调度等待中的子 Agent，不等待任一子 Agent 收敛。
#[test]
fn parent_completion_releases_capacity_without_waiting_for_children() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "并行父任务", NO_PLAN)
        .unwrap();
    let first = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("join_first"),
        )
        .unwrap();
    let second = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("join_second"),
        )
        .unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&second.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));

    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Failed {
                    message: "父候选失败".to_owned(),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&second.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 2);
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Failed {
            turn_id: root_turn.clone(),
            message: "父候选失败".to_owned(),
        }
    );
    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("重复回调不能覆盖失败候选".to_owned()),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::IgnoredStale
    );

    fixture
        .coordinator
        .complete_turn(
            &first.agent.agent_id,
            &first.initial_turn_id,
            AgentTurnOutcome::Failed {
                message: "第一个子任务失败".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Failed {
            turn_id: root_turn.clone(),
            message: "父候选失败".to_owned(),
        }
    );
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);

    fixture
        .coordinator
        .cancel_current_turn(&second.agent.agent_id)
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &second.agent.agent_id,
            &second.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("取消后到达的迟到结果".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Failed {
            turn_id: root_turn,
            message: "父候选失败".to_owned(),
        }
    );
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
}

/// 验证根级 Agent 列表不依赖已结束的父 Turn，仍能观察运行中的子 Agent。
#[test]
fn list_agents_for_root_includes_child_after_parent_completion() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "列出已结束父任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("listed_running_child"),
        )
        .unwrap();

    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("父任务已完成".to_owned()),
            },
        )
        .unwrap();

    let agents = fixture
        .coordinator
        .list_agents_for_root(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(
        agents
            .iter()
            .map(|summary| summary.agent.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/root", "/root/listed_running_child"]
    );
    assert!(matches!(
        agents[0].status,
        CollaborationAgentStatus::Completed { ref turn_id, .. } if turn_id == &root_turn
    ));
    assert!(matches!(
        agents[1].status,
        CollaborationAgentStatus::Running { ref turn_id } if turn_id == &child.initial_turn_id
    ));
    assert_eq!(
        agents[1].current_turn_summary.as_deref(),
        Some("执行 listed_running_child 任务")
    );
    assert_eq!(agents[1].current_root_turn_id.as_ref(), Some(&root_turn));
    assert!(agents[0].current_turn_summary.is_none());
    assert!(agents[0].current_root_turn_id.is_none());
}

/// 父子终态并发到达时，无论线性化顺序如何都只能产生一个父 Turn 终态。
#[test]
fn parent_and_last_child_completion_race_is_linearizable() {
    for _attempt in 0..32 {
        let fixture = fixture(2, 2);
        let root_turn = fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "父子完成竞态", NO_PLAN)
            .unwrap();
        let child = fixture
            .coordinator
            .spawn_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                spawn_request("join_race_child"),
            )
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));

        let parent_coordinator = fixture.coordinator.clone();
        let parent_barrier = barrier.clone();
        let parent_agent_id = fixture.root_agent_id.clone();
        let parent_turn_id = root_turn.clone();
        let parent = thread::spawn(move || {
            parent_barrier.wait();
            parent_coordinator.complete_turn(
                &parent_agent_id,
                &parent_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: Some("竞态父结果".to_owned()),
                },
            )
        });

        let child_coordinator = fixture.coordinator.clone();
        let child_barrier = barrier.clone();
        let child_agent_id = child.agent.agent_id.clone();
        let child_turn_id = child.initial_turn_id;
        let child_completion = thread::spawn(move || {
            child_barrier.wait();
            child_coordinator.complete_turn(
                &child_agent_id,
                &child_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
        });

        barrier.wait();
        assert_eq!(
            parent.join().unwrap().unwrap(),
            TurnCompletionDisposition::Committed
        );
        assert_eq!(
            child_completion.join().unwrap().unwrap(),
            TurnCompletionDisposition::Committed
        );
        assert_eq!(
            fixture
                .coordinator
                .agent_status(&fixture.root_agent_id)
                .unwrap(),
            CollaborationAgentStatus::Completed {
                turn_id: root_turn,
                final_message: Some("竞态父结果".to_owned()),
            }
        );
        assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
        assert_eq!(
            fixture
                .store
                .events()
                .iter()
                .filter(|event| {
                    event.agent_id == fixture.root_agent_id
                        && matches!(
                            event.kind,
                            CollaborationEventKind::AgentTurnCompleted { .. }
                        )
                })
                .count(),
            1
        );
    }
}

/// 取消父 Turn 只中断父执行，已经启动的子 Agent 必须继续独立运行。
#[test]
fn parent_turn_cancellation_does_not_cascade_to_child() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "取消父任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("independent_cancel_child"),
        )
        .unwrap();
    let root_launch = fixture.execution.launch(&root_turn);
    let child_launch = fixture.execution.launch(&child.initial_turn_id);

    assert_eq!(
        fixture
            .coordinator
            .cancel_current_turn(&fixture.root_agent_id)
            .unwrap(),
        root_turn
    );
    assert!(root_launch.cancellation.is_cancelled());
    assert!(!child_launch.cancellation.is_cancelled());
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: child.initial_turn_id.clone(),
        }
    );
    let events_after_first_cancel = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .cancel_current_turn(&fixture.root_agent_id)
            .unwrap(),
        root_turn
    );
    assert_eq!(fixture.store.events().len(), events_after_first_cancel);

    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("取消后的迟到父结果".to_owned()),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted {
            turn_id: root_turn.clone(),
        }
    );
    assert!(!child_launch.cancellation.is_cancelled());

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("子任务独立完成".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Completed {
            turn_id: child.initial_turn_id,
            final_message: Some("子任务独立完成".to_owned()),
        }
    );
    let mailbox = fixture.coordinator.mailbox(&fixture.root_agent_id).unwrap();
    assert_eq!(mailbox.len(), 1);
    assert_eq!(mailbox[0].delivery, MailboxDelivery::QueueOnly);
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
}

/// 验证根树取消期间列表区分正在运行与仍在容量队列中的子 Agent。
#[test]
fn list_agents_for_root_preserves_cancelling_and_waiting_states() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "取消期间列出 Agent", NO_PLAN)
        .unwrap();
    let running_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("listed_running_child"),
        )
        .unwrap();
    let waiting_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("listed_waiting_child"),
        )
        .unwrap();

    fixture
        .coordinator
        .cancel_current_turn(&fixture.root_agent_id)
        .unwrap();

    let agents = fixture
        .coordinator
        .list_agents_for_root(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(
        agents
            .iter()
            .map(|summary| summary.agent.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/root",
            "/root/listed_running_child",
            "/root/listed_waiting_child",
        ]
    );
    assert!(matches!(
        agents[0].status,
        CollaborationAgentStatus::Cancelling { ref turn_id } if turn_id == &root_turn
    ));
    assert!(matches!(
        agents[1].status,
        CollaborationAgentStatus::Running { ref turn_id } if turn_id == &running_child.initial_turn_id
    ));
    assert!(matches!(
        agents[2].status,
        CollaborationAgentStatus::WaitingCapacity { ref turn_id } if turn_id == &waiting_child.initial_turn_id
    ));
    assert_eq!(
        agents[2].current_turn_summary.as_deref(),
        Some("执行 listed_waiting_child 任务")
    );
    assert_eq!(agents[2].current_root_turn_id.as_ref(), Some(&root_turn));
}

/// live checkpoint 中已完成父 Turn 保持终态，只有崩溃时仍活跃的子 Turn 被恢复为中断。
#[test]
fn completed_parent_and_running_child_restore_independently() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "恢复父任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("join_recovery_child"),
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("恢复后的父结果".to_owned()),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let root_snapshot = checkpoint
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap();
    let parent_snapshot = root_snapshot
        .agents
        .iter()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .unwrap();
    assert_eq!(
        parent_snapshot.status,
        CollaborationAgentStatus::Completed {
            turn_id: root_turn.clone(),
            final_message: Some("恢复后的父结果".to_owned()),
        }
    );
    assert_eq!(parent_snapshot.current_root_turn_id, None);
    assert!(!parent_snapshot.start_pending);

    let restored = restore_coordinator(fixture.store.clone(), 2, 45_000);
    restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Completed {
            turn_id: root_turn,
            final_message: Some("恢复后的父结果".to_owned()),
        }
    );
    assert_eq!(
        restored.agent_status(&child.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted {
            turn_id: child.initial_turn_id,
        }
    );
    assert_eq!(restored.capacity().unwrap().global_in_use, 0);
    let mailbox = restored.mailbox(&fixture.root_agent_id).unwrap();
    assert_eq!(mailbox.len(), 1);
    assert_eq!(mailbox[0].delivery, MailboxDelivery::QueueOnly);
    restored
        .checkpoint_quiescent_root(&fixture.root_agent_id)
        .unwrap();
}

/// 关闭根 Session 是唯一会统一停止父子 Agent 树并释放全部真实容量的边界。
#[test]
fn closing_root_tree_stops_surviving_child_without_capacity_underflow() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "关闭父任务", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("join_close_child"),
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);

    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
    assert!(fixture.coordinator.capacity().unwrap().roots.is_empty());
    assert_eq!(fixture.execution.quiesces().len(), 1);
    assert_eq!(fixture.execution.closes().len(), 1);
    assert!(matches!(
        fixture.coordinator.agent_status(&fixture.root_agent_id),
        Err(CollaborationError::AgentNotFound { .. })
    ));
}

/// 相同可信键和规范输入必须原样重放首次结果，来源 Turn 结束后也不能重复副作用。
#[test]
fn collaboration_invocations_replay_after_source_turn_ends_without_side_effects() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "幂等父任务", NO_PLAN)
        .unwrap();
    let spawn_call_id = fixed_tool_call_id("spawn-replay");
    let request = spawn_request("replay_child");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &spawn_call_id,
            request.clone(),
        )
        .unwrap();
    let events_after_spawn = fixture.store.events().len();
    let launches_after_spawn = fixture.execution.launches().len();
    assert_eq!(
        fixture
            .coordinator
            .spawn_agent(&fixture.root_agent_id, &root_turn, &spawn_call_id, request,)
            .unwrap(),
        child
    );
    assert_eq!(fixture.store.events().len(), events_after_spawn);
    assert_eq!(fixture.execution.launches().len(), launches_after_spawn);

    let message_call_id = fixed_tool_call_id("message-replay");
    let message_id = fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &message_call_id,
            &child.agent.agent_id,
            "只投递一次",
        )
        .unwrap();
    let events_after_message = fixture.store.events().len();
    let signals_after_message = fixture.execution.signals().len();
    assert_eq!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &message_call_id,
                &child.agent.agent_id,
                "只投递一次",
            )
            .unwrap(),
        message_id
    );
    assert_eq!(fixture.store.events().len(), events_after_message);
    assert_eq!(fixture.execution.signals().len(), signals_after_message);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child.agent.agent_id)
            .unwrap()
            .len(),
        1
    );

    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let events_after_source_end = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &message_call_id,
                &child.agent.agent_id,
                "只投递一次",
            )
            .unwrap(),
        message_id
    );
    assert_eq!(fixture.store.events().len(), events_after_source_end);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child.agent.agent_id)
            .unwrap()
            .len(),
        1
    );
}

/// StopAgent 同一可信键必须在取消完成和来源 Turn 结束后仍重放首次停止结果。
#[test]
fn stop_invocation_replays_after_terminal_state_and_rejects_changed_target() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "StopAgent 幂等父任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("stop_replay_child"),
        )
        .unwrap();
    let other_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("stop_replay_other_child"),
        )
        .unwrap();
    let child_launch = fixture.execution.launch(&child.initial_turn_id);
    let stop_call_id = fixed_tool_call_id("stop-replay");
    let stopped_turn_id = fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &stop_call_id,
            &child.agent.agent_id,
        )
        .unwrap();
    assert_eq!(stopped_turn_id, child.initial_turn_id);
    assert!(child_launch.cancellation.is_cancelled());
    let events_after_stop = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        stopped_turn_id
    );
    assert_eq!(fixture.store.events().len(), events_after_stop);

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("取消后的过期成功".to_owned()),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let events_after_terminal = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        stopped_turn_id
    );
    assert_eq!(fixture.store.events().len(), events_after_terminal);
    assert!(matches!(
        fixture.coordinator.stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &stop_call_id,
            &other_child.agent.agent_id,
        ),
        Err(CollaborationError::IdempotencyConflict { .. })
    ));
    assert_eq!(
        fixture
            .store
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                    if receipt.key.tool_call_id == stop_call_id
                        && receipt.kind == CollaborationInvocationKind::StopAgent
            ))
            .count(),
        1
    );
}

/// TriggerTurn 重放必须复用首次消息和 Turn，不能再次预约容量或启动 Agent。
#[test]
fn followup_invocation_replays_original_triggered_turn() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建空闲子 Agent", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("followup_replay_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let tool_call_id = fixed_tool_call_id("followup-replay");
    let first = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            &child.agent.agent_id,
            "只触发一次",
        )
        .unwrap();
    let events_after_first = fixture.store.events().len();
    let launches_after_first = fixture.execution.launches().len();
    let second = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            &child.agent.agent_id,
            "只触发一次",
        )
        .unwrap();
    assert_eq!(first, second);
    assert!(first.1.is_some());
    assert_eq!(fixture.store.events().len(), events_after_first);
    assert_eq!(fixture.execution.launches().len(), launches_after_first);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child.agent.agent_id)
            .unwrap()
            .len(),
        1
    );
}

/// 同一键改变输入或操作类型必须稳定冲突，不能退回普通业务校验。
#[test]
fn collaboration_invocation_conflict_is_stable_for_changed_input_and_operation() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "冲突父任务", NO_PLAN)
        .unwrap();
    let tool_call_id = fixed_tool_call_id("conflict-call");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            spawn_request("conflict_child"),
        )
        .unwrap();
    let expected = CollaborationError::IdempotencyConflict {
        source_agent_id: fixture.root_agent_id.clone(),
        source_turn_id: root_turn.clone(),
        tool_call_id: tool_call_id.clone(),
    };
    let mut changed = spawn_request("conflict_child");
    changed.initial_task.push_str(" changed");
    assert_eq!(
        fixture
            .coordinator
            .spawn_agent(&fixture.root_agent_id, &root_turn, &tool_call_id, changed,)
            .unwrap_err(),
        expected
    );
    assert_eq!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &tool_call_id,
                &child.agent.agent_id,
                "改成另一种操作",
            )
            .unwrap_err(),
        expected
    );
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &tool_call_id,
                &child.agent.agent_id,
            )
            .unwrap_err(),
        expected
    );
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &tool_call_id,
                &fixture.root_agent_id,
            )
            .unwrap_err(),
        expected,
        "既有可信键的跨操作冲突必须优先于 StopAgent 自停业务校验"
    );
}

/// 同一 ToolCall 文本在不同 Agent 或 Turn 下必须形成彼此隔离的新调用。
#[test]
fn collaboration_invocation_keys_are_isolated_across_agents_and_turns() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "第一主 Turn", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("isolation_child"),
        )
        .unwrap();
    let shared_tool_call_id = fixed_tool_call_id("provider-reused-id");
    let root_message = fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &first_root_turn,
            &shared_tool_call_id,
            &child.agent.agent_id,
            "root to child",
        )
        .unwrap();
    let child_message = fixture
        .coordinator
        .send_message(
            &child.agent.agent_id,
            &child.initial_turn_id,
            &shared_tool_call_id,
            &fixture.root_agent_id,
            "child to root",
        )
        .unwrap();
    assert_ne!(root_message, child_message);

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let second_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "第二主 Turn", NO_PLAN)
        .unwrap();
    let second_turn_message = fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &second_root_turn,
            &shared_tool_call_id,
            &child.agent.agent_id,
            "second turn",
        )
        .unwrap();
    assert_ne!(root_message, second_turn_message);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child.agent.agent_id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&fixture.root_agent_id)
            .unwrap()
            .len(),
        2
    );
}

/// Steer 的外部 operationId 在同一 Agent 的不同 Turn 中必须重新执行，不能复用旧结果。
#[test]
fn steer_operation_ids_are_isolated_across_turns() {
    let fixture = fixture(4, 4);
    let operation_id = fixed_tool_call_id("provider-reused-steer-id");
    let first_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "第一主 Turn", NO_PLAN)
        .unwrap();
    let first = fixture
        .coordinator
        .steer_active_agent_with_operation(&fixture.root_agent_id, &operation_id, "第一条 steer")
        .unwrap();
    assert_eq!(first.turn_id, first_turn);
    let claimed = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &first_turn)
        .unwrap();
    fixture
        .coordinator
        .acknowledge_user_steers(
            &fixture.root_agent_id,
            &first_turn,
            claimed.last().expect("Steer claim 应包含首条消息").sequence,
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();

    let second_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "第二主 Turn", NO_PLAN)
        .unwrap();
    let second = fixture
        .coordinator
        .steer_active_agent_with_operation(&fixture.root_agent_id, &operation_id, "第二条 steer")
        .unwrap();
    assert_eq!(second.turn_id, second_turn);
    assert_ne!(first, second);

    let replayed = fixture
        .coordinator
        .steer_active_agent_with_operation(&fixture.root_agent_id, &operation_id, "第二条 steer")
        .unwrap();
    assert_eq!(replayed, second);
}

/// 两个 Runner 并发提交同一 SpawnAgent 调用时只能产生一个 Agent、Turn 和端口动作。
#[test]
fn concurrent_duplicate_spawn_returns_one_committed_result() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "并发幂等", NO_PLAN)
        .unwrap();
    let tool_call_id = fixed_tool_call_id("concurrent-spawn");
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let coordinator = fixture.coordinator.clone();
        let root_agent_id = fixture.root_agent_id.clone();
        let root_turn = root_turn.clone();
        let tool_call_id = tool_call_id.clone();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            coordinator.spawn_agent(
                &root_agent_id,
                &root_turn,
                &tool_call_id,
                spawn_request("concurrent_child"),
            )
        }));
    }
    let first = threads.remove(0).join().unwrap().unwrap();
    let second = threads.remove(0).join().unwrap().unwrap();
    assert_eq!(first, second);
    assert_eq!(
        fixture
            .execution
            .launches()
            .iter()
            .filter(|launch| launch.turn_id == first.initial_turn_id)
            .count(),
        1
    );
    assert_eq!(
        fixture
            .store
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                    if receipt.key.tool_call_id == tool_call_id
            ))
            .count(),
        1
    );
}

/// Store 明确未提交时不保留幂等记录，使用同一键重试只产生一次最终副作用。
#[test]
fn failed_collaboration_commit_can_retry_same_key_once() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "失败后重试", NO_PLAN)
        .unwrap();
    let tool_call_id = fixed_tool_call_id("retry-after-store-failure");
    let request = spawn_request("retry_child");
    fixture.store.fail_next_collaboration_commit();
    assert!(matches!(
        fixture.coordinator.spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            request.clone(),
        ),
        Err(CollaborationError::Store { .. })
    ));
    assert!(
        fixture
            .coordinator
            .resolve_path(
                &fixture.root_agent_id,
                &AgentPath::parse("/root/retry_child").unwrap(),
            )
            .unwrap()
            .is_none()
    );
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            request.clone(),
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .spawn_agent(&fixture.root_agent_id, &root_turn, &tool_call_id, request,)
            .unwrap(),
        child
    );
    assert_eq!(
        fixture
            .execution
            .launches()
            .iter()
            .filter(|launch| launch.turn_id == child.initial_turn_id)
            .count(),
        1
    );
}

/// Store 先提交再断线时同批 receipt 完成对账，后续重放不能再次写 Store 或调用端口。
#[test]
fn uncertain_committed_invocation_replays_without_duplicate_effects() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "未知提交", NO_PLAN)
        .unwrap();
    let tool_call_id = fixed_tool_call_id("commit-then-unknown");
    let request = spawn_request("unknown_child");
    fixture.store.commit_then_indeterminate();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &tool_call_id,
            request.clone(),
        )
        .unwrap();
    let events_after_commit = fixture.store.events().len();
    let launches_after_commit = fixture.execution.launches().len();
    assert_eq!(
        fixture
            .coordinator
            .spawn_agent(&fixture.root_agent_id, &root_turn, &tool_call_id, request,)
            .unwrap(),
        child
    );
    assert_eq!(fixture.store.events().len(), events_after_commit);
    assert_eq!(fixture.execution.launches().len(), launches_after_commit);
    assert_eq!(
        fixture
            .store
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                    if receipt.key.tool_call_id == tool_call_id
            ))
            .count(),
        1
    );
    let committed_batch = fixture
        .store
        .batches()
        .into_iter()
        .find(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    &event.kind,
                    CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                        if receipt.key.tool_call_id == tool_call_id
                )
            })
        })
        .expect("协作幂等 receipt 应存在于一个原子批次");
    assert!(committed_batch.events.iter().any(|event| matches!(
        &event.kind,
        CollaborationEventKind::AgentSpawned { definition, .. }
            if definition.agent_id == child.agent.agent_id
    )));
    assert!(committed_batch.events.iter().any(|event| {
        event.turn_id.as_ref() == Some(&child.initial_turn_id)
            && matches!(
                event.kind,
                CollaborationEventKind::AgentTurnQueued { .. }
                    | CollaborationEventKind::AgentTurnStarted { .. }
            )
    }));
}

/// StopAgent 提交后断线必须由同批 receipt 对账，重放不能重复取消或状态事件。
#[test]
fn uncertain_committed_stop_replays_without_duplicate_stop_effects() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "StopAgent 未知提交", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("unknown_stop_child"),
        )
        .unwrap();
    let child_launch = fixture.execution.launch(&child.initial_turn_id);
    let stop_call_id = fixed_tool_call_id("commit-stop-then-unknown");
    fixture.store.commit_then_indeterminate();
    let stopped_turn_id = fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &stop_call_id,
            &child.agent.agent_id,
        )
        .unwrap();
    assert_eq!(stopped_turn_id, child.initial_turn_id);
    assert!(child_launch.cancellation.is_cancelled());
    let events_after_commit = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        stopped_turn_id
    );
    assert_eq!(fixture.store.events().len(), events_after_commit);
    let events = fixture.store.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.agent_id == child.agent.agent_id
                    && matches!(
                        &event.kind,
                        CollaborationEventKind::AgentStatusChanged {
                            current: CollaborationAgentStatus::Cancelling { turn_id },
                            ..
                        } if turn_id == &child.initial_turn_id
                    )
            })
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.kind,
                CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                    if receipt.key.tool_call_id == stop_call_id
                        && receipt.kind == CollaborationInvocationKind::StopAgent
            ))
            .count(),
        1
    );
    let committed_batch = fixture
        .store
        .batches()
        .into_iter()
        .find(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    &event.kind,
                    CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                        if receipt.key.tool_call_id == stop_call_id
                )
            })
        })
        .expect("StopAgent receipt 应存在于一个原子批次");
    assert!(committed_batch.events.iter().any(|event| {
        event.agent_id == child.agent.agent_id
            && matches!(
                &event.kind,
                CollaborationEventKind::AgentStatusChanged {
                    current: CollaborationAgentStatus::Cancelling { turn_id },
                    ..
                } if turn_id == &child.initial_turn_id
            )
    }));
}

/// checkpoint 冷恢复后即使来源 Turn 已被崩溃收敛，已消费消息和 Spawn 仍原样重放。
#[test]
fn collaboration_invocations_survive_checkpoint_restore_and_consumed_mailbox() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "冷恢复幂等", NO_PLAN)
        .unwrap();
    let spawn_call_id = fixed_tool_call_id("restore-spawn");
    let spawn_request = spawn_request("restore_idempotency_child");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &spawn_call_id,
            spawn_request.clone(),
        )
        .unwrap();
    let message_call_id = fixed_tool_call_id("restore-message");
    let message_id = fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &message_call_id,
            &child.agent.agent_id,
            "恢复前已经消费",
        )
        .unwrap();
    let consumed = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &child.initial_turn_id, 1)
        .unwrap();
    assert_eq!(consumed[0].message_id, message_id);
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &child.initial_turn_id,
        &consumed,
    );
    let stop_call_id = fixed_tool_call_id("restore-stop");
    let stopped_turn_id = fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &stop_call_id,
            &child.agent.agent_id,
        )
        .unwrap();
    let snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(snapshot.invocations.len(), 3);
    assert!(
        snapshot
            .invocations
            .iter()
            .all(|invocation| invocation.input_digest != [0; 32])
    );
    let restored = restore_coordinator(fixture.store.clone(), 4, 90_000);
    restored.restore_coordinator(snapshot).unwrap();
    let events_before_replay = fixture.store.events().len();
    assert_eq!(
        restored
            .spawn_agent(
                &fixture.root_agent_id,
                &root_turn,
                &spawn_call_id,
                spawn_request,
            )
            .unwrap(),
        child
    );
    assert_eq!(
        restored
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &message_call_id,
                &child.agent.agent_id,
                "恢复前已经消费",
            )
            .unwrap(),
        message_id
    );
    assert_eq!(
        restored
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        stopped_turn_id
    );
    assert_eq!(fixture.store.events().len(), events_before_replay);
    assert!(restored.mailbox(&child.agent.agent_id).unwrap().is_empty());
    assert!(matches!(
        restored.send_message(
            &fixture.root_agent_id,
            &root_turn,
            &message_call_id,
            &child.agent.agent_id,
            "恢复后改变正文",
        ),
        Err(CollaborationError::IdempotencyConflict { .. })
    ));
}

/// 验证投递语义及并发 claim 重放同一 FIFO 批次，ack 后才 exactly-once 删除。
#[test]
fn mailbox_is_fifo_exactly_once_and_delivery_modes_are_distinct() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建子 Agent", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("mailbox"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let command_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "发送消息", NO_PLAN)
        .unwrap();
    let launches_before = fixture.execution.launches().len();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &command_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "one",
        )
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &command_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "two",
        )
        .unwrap();
    assert_eq!(fixture.execution.launches().len(), launches_before);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Completed { .. }
    ));

    let (_message_id, followup_turn) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &command_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "three",
        )
        .unwrap();
    let followup_turn = followup_turn.expect("空闲 Agent 应启动 Followup Turn");
    assert_eq!(fixture.execution.launches().len(), launches_before + 1);

    let barrier = Arc::new(Barrier::new(3));
    let mut consumers = Vec::new();
    for _index in 0..2 {
        let coordinator = fixture.coordinator.clone();
        let barrier = barrier.clone();
        let child_id = child.agent.agent_id.clone();
        let turn_id = followup_turn.clone();
        consumers.push(thread::spawn(move || {
            barrier.wait();
            coordinator
                .consume_mailbox(&child_id, &turn_id, usize::MAX)
                .expect("mailbox 消费应成功")
        }));
    }
    barrier.wait();
    let batches = consumers
        .into_iter()
        .map(|consumer| consumer.join().expect("消费线程不应 panic"))
        .collect::<Vec<_>>();
    assert_eq!(batches.len(), 2);
    assert!(batches.iter().all(|batch| batch == &batches[0]));
    assert_eq!(
        batches[0]
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &followup_turn,
        &batches[0],
    );
    fixture
        .coordinator
        .acknowledge_mailbox(&child.agent.agent_id, &followup_turn, 3)
        .expect("重复 ack 应幂等成功");
    assert!(
        fixture
            .coordinator
            .consume_mailbox(&child.agent.agent_id, &followup_turn, usize::MAX)
            .unwrap()
            .is_empty()
    );
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &command_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// 验证 mailbox claim 跨崩溃恢复保留原批次，重试后才可确认并读取后续输入。
#[test]
fn mailbox_claim_survives_restore_and_rebinds_to_retry_turn() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建可恢复 mailbox claim", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("mailbox_claim_restore"),
        )
        .unwrap();
    for content in ["第一条", "第二条"] {
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
                content,
            )
            .unwrap();
    }

    let claimed = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &child.initial_turn_id, 1)
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].content, "第一条");
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "claim 后新增",
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .consume_mailbox(&child.agent.agent_id, &child.initial_turn_id, usize::MAX)
            .unwrap(),
        claimed
    );

    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = restore_coordinator(fixture.store.clone(), 4, 94_000);
    restored.restore_coordinator(checkpoint).unwrap();
    assert!(matches!(
        restored.agent_status(&child.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id }
            if turn_id == child.initial_turn_id
    ));
    let source_turn = restored
        .begin_root_turn(&fixture.root_agent_id, "恢复后重试子 Agent", NO_PLAN)
        .unwrap();
    let retry_turn = restored
        .retry_agent(&fixture.root_agent_id, &source_turn, &child.agent.agent_id)
        .unwrap();
    assert_eq!(
        restored
            .consume_mailbox(&child.agent.agent_id, &retry_turn, usize::MAX)
            .unwrap(),
        claimed
    );
    assert!(matches!(
        restored
            .acknowledge_mailbox(&child.agent.agent_id, &retry_turn, 2)
            .unwrap_err(),
        CollaborationError::InputClaimMismatch { .. }
    ));
    acknowledge_mailbox_batch(&restored, &child.agent.agent_id, &retry_turn, &claimed);

    let remaining = restored
        .consume_mailbox(&child.agent.agent_id, &retry_turn, usize::MAX)
        .unwrap();
    assert_eq!(
        remaining
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["第二条", "claim 后新增"]
    );
    acknowledge_mailbox_batch(&restored, &child.agent.agent_id, &retry_turn, &remaining);
    assert!(restored.mailbox(&child.agent.agent_id).unwrap().is_empty());
    restored
        .complete_turn(
            &child.agent.agent_id,
            &retry_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .events()
            .iter()
            .filter(|event| matches!(
                event.kind,
                CollaborationEventKind::AgentMessagesClaimed { .. }
            ))
            .count(),
        2
    );
}

/// 验证用户 steer claim 阻止正常完成，并可在崩溃收敛后直接 ack 再处理新增批次。
#[test]
fn steer_claim_survives_restore_and_can_be_acknowledged_after_interruption() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建可恢复 steer claim", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "第一条 steer")
        .unwrap();
    let claimed = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "claim 后新增 steer")
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .consume_user_steers(&fixture.root_agent_id, &root_turn)
            .unwrap(),
        claimed
    );
    assert!(matches!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap_err(),
        CollaborationError::PendingInputClaim {
            input_kind: "用户 steer",
            ..
        }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .acknowledge_user_steers(&fixture.root_agent_id, &root_turn, claimed[0].sequence + 1)
            .unwrap_err(),
        CollaborationError::InputClaimMismatch { .. }
    ));
    let wrong_turn = TurnId::new("wrong-steer-claim-turn").unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .acknowledge_user_steers(&fixture.root_agent_id, &wrong_turn, claimed[0].sequence,)
            .unwrap_err(),
        CollaborationError::InputClaimMismatch { .. }
    ));

    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = restore_coordinator(fixture.store.clone(), 2, 95_000);
    restored.restore_coordinator(checkpoint).unwrap();
    restored
        .acknowledge_user_steers(&fixture.root_agent_id, &root_turn, claimed[0].sequence)
        .unwrap();
    restored
        .acknowledge_user_steers(&fixture.root_agent_id, &root_turn, claimed[0].sequence)
        .expect("重复 ack 不得重新删除正文");
    let recovered_agent = restored
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .unwrap();
    assert!(recovered_agent.steer_claim_turn_id.is_none());
    assert_eq!(recovered_agent.pending_steers.len(), 1);
    assert_eq!(
        recovered_agent.pending_steers[0].content,
        "claim 后新增 steer"
    );

    let next_turn = restored
        .begin_root_turn(&fixture.root_agent_id, "处理剩余 steer", NO_PLAN)
        .unwrap();
    let remaining = restored
        .consume_user_steers(&fixture.root_agent_id, &next_turn)
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].turn_id, next_turn);
    assert_eq!(remaining[0].content, "claim 后新增 steer");
    acknowledge_steer_batch(&restored, &fixture.root_agent_id, &next_turn, &remaining);
    restored
        .complete_turn(
            &fixture.root_agent_id,
            &next_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// 验证动态输入已写入 Transcript 但 ack 失败时，终态会释放 Turn 并保留 claim。
#[test]
fn pending_dynamic_input_completion_preserves_claim_without_followup() {
    let fixture = fixture(2, 2);
    let turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "保留未确认动态输入", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &turn_id, "待恢复的 steer")
        .unwrap();
    let claimed = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &turn_id)
        .unwrap();

    assert_eq!(
        fixture
            .coordinator
            .complete_turn_with_pending_dynamic_input(
                &fixture.root_agent_id,
                &turn_id,
                AgentTurnOutcome::Failed {
                    message: "动态输入 ack 未确认".to_owned(),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
        .unwrap(),
        CollaborationAgentStatus::Failed {
            turn_id: ref failed_turn,
            ..
        } if failed_turn == &turn_id
    ));
    assert_eq!(fixture.execution.launches().len(), 1);

    let checkpoint = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap();
    let agent = checkpoint
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .expect("根 Agent checkpoint 应存在");
    assert_eq!(agent.steer_claim_turn_id, Some(turn_id.clone()));
    assert_eq!(
        agent.steer_claim_through_sequence,
        Some(claimed[0].sequence)
    );
    assert_eq!(agent.pending_steers.len(), 1);
    assert_eq!(agent.pending_steers[0].turn_id, turn_id);
}

/// 验证冷恢复拒绝缺字段、不存在水位和错误 Turn 绑定的输入 claim。
#[test]
fn recovery_rejects_malformed_input_claims() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "生成待篡改输入 claim", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &fixture.root_agent_id,
            "mailbox claim",
        )
        .unwrap();
    fixture
        .coordinator
        .consume_mailbox(&fixture.root_agent_id, &root_turn, 1)
        .unwrap();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "steer claim")
        .unwrap();
    fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    let snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();

    let mut missing_mailbox_turn = snapshot.clone();
    missing_mailbox_turn.roots[0].agents[0].mailbox_claim_turn_id = None;
    let mut missing_steer_sequence = snapshot.clone();
    missing_steer_sequence.roots[0].agents[0].steer_claim_through_sequence = None;
    let mut nonexistent_sequence = snapshot.clone();
    nonexistent_sequence.roots[0].agents[0].steer_claim_through_sequence = Some(99);
    let mut mismatched_turn = snapshot;
    mismatched_turn.roots[0].agents[0].mailbox_claim_turn_id =
        Some(TurnId::new("mismatched-claim-turn").unwrap());

    for (index, corrupted) in [
        missing_mailbox_turn,
        missing_steer_sequence,
        nonexistent_sequence,
        mismatched_turn,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            restore_coordinator(fixture.store.clone(), 2, 96_000 + index as u64)
                .restore_coordinator(corrupted)
                .unwrap_err(),
            CollaborationError::InvalidRecovery { .. }
        ));
    }
}

/// 验证取消时自动创建的 Followup Turn 会接管尚未 ack 的 mailbox 与 steer claim。
#[test]
fn cancellation_rebinds_input_claims_to_automatic_followup() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建取消恢复场景", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("cancelled_claim_followup"),
        )
        .unwrap();
    let (message_id, triggered_turn) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "取消前追加任务",
        )
        .unwrap();
    assert!(triggered_turn.is_none());
    let mailbox = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &child.initial_turn_id, 1)
        .unwrap();
    assert_eq!(mailbox[0].message_id, message_id);
    fixture
        .coordinator
        .steer_agent(
            &child.agent.agent_id,
            &child.initial_turn_id,
            "取消前已读取 steer",
        )
        .unwrap();
    let steers = fixture
        .coordinator
        .consume_user_steers(&child.agent.agent_id, &child.initial_turn_id)
        .unwrap();
    fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("取消后的过期结果".to_owned()),
            },
        )
        .unwrap();

    let followup = fixture
        .execution
        .launches()
        .into_iter()
        .rev()
        .find(|launch| {
            matches!(
                &launch.cause,
                AgentTurnCause::Followup {
                    message_id: candidate
                } if candidate == &message_id
            )
        })
        .expect("取消收敛后应自动启动未确认 Followup");
    assert_ne!(followup.turn_id, child.initial_turn_id);
    let replayed_mailbox = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &followup.turn_id, usize::MAX)
        .unwrap();
    assert_eq!(replayed_mailbox, mailbox);
    let replayed_steers = fixture
        .coordinator
        .consume_user_steers(&child.agent.agent_id, &followup.turn_id)
        .unwrap();
    assert_eq!(replayed_steers.len(), steers.len());
    assert_eq!(replayed_steers[0].sequence, steers[0].sequence);
    assert_eq!(replayed_steers[0].turn_id, followup.turn_id);
    assert_eq!(replayed_steers[0].content, steers[0].content);
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &followup.turn_id,
        &replayed_mailbox,
    );
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &followup.turn_id,
        &replayed_steers,
    );
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &followup.turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// 验证运行中 Followup 只发安全边界信号，当前 Turn 收敛后才启动下一 Turn。
#[test]
fn followup_to_running_agent_never_reenters_current_turn() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "运行中 Followup", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("running_followup"),
        )
        .unwrap();
    let launches_before = fixture.execution.launches().len();
    let (_message_id, triggered_turn) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "在当前 Turn 后继续",
        )
        .unwrap();
    assert!(triggered_turn.is_none());
    assert_eq!(fixture.execution.launches().len(), launches_before);
    let signals = fixture
        .execution
        .signals
        .lock()
        .expect("信号锁不应中毒")
        .clone();
    assert!(signals.iter().any(|signal| {
        signal.agent_id == child.agent.agent_id && signal.turn_id == child.initial_turn_id
    }));

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(fixture.execution.launches().len(), launches_before + 1);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// 验证 WaitAgent 不返回正文，可被 mailbox、steer 或暂停时钟确定性唤醒。
#[tokio::test(start_paused = true)]
async fn wait_agent_reports_mailbox_steer_and_timeout() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "等待测试", NO_PLAN)
        .unwrap();

    let timeout_wait =
        fixture
            .coordinator
            .wait_agent(&fixture.root_agent_id, &root_turn, Duration::from_secs(10));
    tokio::pin!(timeout_wait);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(timeout_wait.await.unwrap(), WaitAgentOutcome::TimedOut);

    let coordinator = fixture.coordinator.clone();
    let root_id = fixture.root_agent_id.clone();
    let turn_for_wait = root_turn.clone();
    let steer_wait = tokio::spawn(async move {
        coordinator
            .wait_agent(&root_id, &turn_for_wait, Duration::from_secs(60))
            .await
    });
    tokio::task::yield_now().await;
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "用户追加")
        .unwrap();
    assert!(matches!(
        steer_wait.await.unwrap().unwrap(),
        WaitAgentOutcome::UserSteer(summary) if summary.pending_count == 1
    ));
    let consumed_steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &consumed_steers,
    );

    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("waiter"),
        )
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "mailbox body",
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .wait_agent(
                &child.agent.agent_id,
                &child.initial_turn_id,
                Duration::from_secs(60)
            )
            .await
            .unwrap(),
        WaitAgentOutcome::MailboxActivity(crate::MailboxActivitySummary {
            pending_count: 1,
            latest_sequence: 1,
        })
    );
}

/// 验证 WaitAgent 不把尚未 ack 的稳定 claim 重复报告为新活动，只报告后续序号。
#[tokio::test(start_paused = true)]
async fn wait_agent_ignores_claimed_batches_and_reports_new_suffixes() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "等待 claim 后续活动", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &fixture.root_agent_id,
            "已 claim mailbox",
        )
        .unwrap();
    let first_mailbox = fixture
        .coordinator
        .consume_mailbox(&fixture.root_agent_id, &root_turn, 1)
        .unwrap();
    let mailbox_timeout =
        fixture
            .coordinator
            .wait_agent(&fixture.root_agent_id, &root_turn, Duration::from_secs(10));
    tokio::pin!(mailbox_timeout);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(mailbox_timeout.await.unwrap(), WaitAgentOutcome::TimedOut);
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &fixture.root_agent_id,
            "新增 mailbox",
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .wait_agent(&fixture.root_agent_id, &root_turn, Duration::from_secs(10),)
            .await
            .unwrap(),
        WaitAgentOutcome::MailboxActivity(crate::MailboxActivitySummary {
            pending_count: 1,
            latest_sequence: 2,
        })
    );
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &first_mailbox,
    );
    let second_mailbox = fixture
        .coordinator
        .consume_mailbox(&fixture.root_agent_id, &root_turn, 1)
        .unwrap();
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &second_mailbox,
    );

    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "已 claim steer")
        .unwrap();
    let first_steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    let steer_timeout =
        fixture
            .coordinator
            .wait_agent(&fixture.root_agent_id, &root_turn, Duration::from_secs(10));
    tokio::pin!(steer_timeout);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(steer_timeout.await.unwrap(), WaitAgentOutcome::TimedOut);
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "新增 steer")
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .wait_agent(&fixture.root_agent_id, &root_turn, Duration::from_secs(10),)
            .await
            .unwrap(),
        WaitAgentOutcome::UserSteer(crate::UserSteerSummary {
            pending_count: 1,
            latest_sequence: 2,
        })
    );
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &first_steers,
    );
    let second_steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &second_steers,
    );
}

/// 父 Turn 取消不级联；只有显式中断目标子 Agent 后，该子 Agent 才进入可重试终态。
#[test]
fn parent_cancellation_keeps_child_running_until_explicit_interrupt() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "取消测试", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("cancel"),
        )
        .unwrap();
    let root_launch = fixture.execution.launch(&root_turn);
    let child_launch = fixture.execution.launch(&child.initial_turn_id);

    fixture
        .coordinator
        .cancel_current_turn(&fixture.root_agent_id)
        .unwrap();
    assert!(root_launch.cancellation.is_cancelled());
    assert!(!child_launch.cancellation.is_cancelled());
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("过期成功".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted {
            turn_id: root_turn.clone(),
        }
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: child.initial_turn_id.clone(),
        }
    );
    let control_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "显式中断并重试子 Agent", NO_PLAN)
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &control_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
            )
            .unwrap(),
        child.initial_turn_id
    );
    assert!(child_launch.cancellation.is_cancelled());
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("不应覆盖中断".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted {
            turn_id: child.initial_turn_id.clone()
        }
    );
    let retry_turn = fixture
        .coordinator
        .retry_agent(&fixture.root_agent_id, &control_turn, &child.agent.agent_id)
        .unwrap();
    assert_ne!(retry_turn, child.initial_turn_id);
    assert!(matches!(
        fixture.coordinator.agent_status(&child.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == retry_turn
    ));
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &retry_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &control_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// 父 Turn 取消不得移除等待容量的子 Agent；父槽位释放后仍按原顺序调度。
#[test]
fn parent_cancellation_preserves_waiting_children_and_queue_order() {
    let fixture = fixture(1, 1);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(
            &fixture.root_agent_id,
            "取消含排队子 Agent 的父任务",
            NO_PLAN,
        )
        .unwrap();
    let root_launch = fixture.execution.launch(&root_turn);
    let first = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("queued_cancel_first"),
        )
        .unwrap();
    let second = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("queued_cancel_second"),
        )
        .unwrap();

    fixture
        .coordinator
        .cancel_current_turn(&fixture.root_agent_id)
        .unwrap();
    assert!(root_launch.cancellation.is_cancelled());
    for child in [&first, &second] {
        assert_eq!(
            fixture
                .coordinator
                .agent_status(&child.agent.agent_id)
                .unwrap(),
            CollaborationAgentStatus::WaitingCapacity {
                turn_id: child.initial_turn_id.clone(),
            }
        );
    }
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Cancelling {
            turn_id: root_turn.clone(),
        }
    );
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);
    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: Some("取消后迟到的父结果".to_owned()),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id: root_turn }
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&first.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: first.initial_turn_id.clone(),
        }
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&second.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity {
            turn_id: second.initial_turn_id.clone(),
        }
    );
    fixture
        .coordinator
        .complete_turn(
            &first.agent.agent_id,
            &first.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&second.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: second.initial_turn_id.clone(),
        }
    );
    fixture
        .coordinator
        .complete_turn(
            &second.agent.agent_id,
            &second.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
}

/// 父取消与子终态并发到达时必须各自线性化，父取消不得改变子终态。
#[test]
fn parent_cancellation_and_last_child_completion_race_is_linearizable() {
    for _attempt in 0..32 {
        let fixture = fixture(2, 2);
        let root_turn = fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "父取消与子完成竞态", NO_PLAN)
            .unwrap();
        let child = fixture
            .coordinator
            .spawn_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                spawn_request("cancel_race_child"),
            )
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));

        let cancel_coordinator = fixture.coordinator.clone();
        let cancel_barrier = barrier.clone();
        let cancel_agent_id = fixture.root_agent_id.clone();
        let cancel = thread::spawn(move || {
            cancel_barrier.wait();
            cancel_coordinator.cancel_current_turn(&cancel_agent_id)
        });

        let child_coordinator = fixture.coordinator.clone();
        let child_barrier = barrier.clone();
        let child_agent_id = child.agent.agent_id.clone();
        let child_turn_id = child.initial_turn_id.clone();
        let child_completion = thread::spawn(move || {
            child_barrier.wait();
            child_coordinator.complete_turn(
                &child_agent_id,
                &child_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
        });

        barrier.wait();
        assert_eq!(cancel.join().unwrap().unwrap(), root_turn);
        assert_eq!(
            child_completion.join().unwrap().unwrap(),
            TurnCompletionDisposition::Committed
        );
        assert_eq!(
            fixture
                .coordinator
                .complete_turn(
                    &fixture.root_agent_id,
                    &root_turn,
                    AgentTurnOutcome::Completed {
                        final_message: None,
                    },
                )
                .unwrap(),
            TurnCompletionDisposition::Committed
        );
        assert_eq!(
            fixture
                .coordinator
                .agent_status(&fixture.root_agent_id)
                .unwrap(),
            CollaborationAgentStatus::Interrupted {
                turn_id: root_turn.clone(),
            }
        );
        assert_eq!(
            fixture
                .coordinator
                .agent_status(&child.agent.agent_id)
                .unwrap(),
            CollaborationAgentStatus::Completed {
                turn_id: child.initial_turn_id,
                final_message: None,
            }
        );
        assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
        assert_eq!(
            fixture
                .store
                .events()
                .iter()
                .filter(|event| {
                    event.agent_id == fixture.root_agent_id
                        && matches!(event.kind, CollaborationEventKind::AgentTurnInterrupted)
                })
                .count(),
            1
        );
    }
}

/// 父取消不得抑制子 Agent 已认领的 TriggerTurn，子后续 Turn 仍应独立启动。
#[test]
fn parent_cancellation_keeps_child_followup_restart() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(
            &fixture.root_agent_id,
            "取消带 Followup 的子 Agent",
            NO_PLAN,
        )
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("cancel_followup_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "父取消后仍需执行",
        )
        .unwrap();
    let launches_before_cancel = fixture.execution.launches().len();

    fixture
        .coordinator
        .cancel_current_turn(&fixture.root_agent_id)
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let launches = fixture.execution.launches();
    assert_eq!(launches.len(), launches_before_cancel + 1);
    let followup_turn = launches
        .last()
        .expect("子 Agent 后续 Turn 应已启动")
        .turn_id
        .clone();
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running {
            turn_id: followup_turn.clone(),
        }
    );
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &followup_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
}

/// live checkpoint 恢复时分别中断取消中的父 Turn 与仍运行的子 Turn。
#[test]
fn cancelling_parent_and_running_child_restore_as_independent_interruptions() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "恢复取消中的父任务", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("cancel_join_restore_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .cancel_current_turn(&fixture.root_agent_id)
        .unwrap();
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let root_snapshot = checkpoint
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap();
    let parent_snapshot = root_snapshot
        .agents
        .iter()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .unwrap();
    assert_eq!(
        parent_snapshot.status,
        CollaborationAgentStatus::Cancelling {
            turn_id: root_turn.clone(),
        }
    );

    let restored = restore_coordinator(fixture.store.clone(), 2, 46_000);
    restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id: root_turn }
    );
    assert_eq!(
        restored.agent_status(&child.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted {
            turn_id: child.initial_turn_id,
        }
    );
    assert_eq!(restored.capacity().unwrap().global_in_use, 0);
}

/// 验证并发 spawn 和终态回调下的全局/每根槽位不会超卖或泄漏。
#[test]
fn global_and_per_root_slots_are_reserved_atomically_under_race() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        store,
        execution,
        Arc::new(SequentialIds::default()),
    ));
    let root_one = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("root-one-session").unwrap(),
            profile: profile("root_one"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let root_two = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("root-two-session").unwrap(),
            profile: profile("root_two"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let root_one_turn = coordinator
        .begin_root_turn(&root_one.agent_id, "root one", NO_PLAN)
        .unwrap();
    let root_two_turn = coordinator
        .begin_root_turn(&root_two.agent_id, "root two", NO_PLAN)
        .unwrap();

    let workers = 16usize;
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut threads = Vec::new();
    for index in 0..workers {
        let coordinator = coordinator.clone();
        let barrier = barrier.clone();
        let source = if index % 2 == 0 {
            root_one.agent_id.clone()
        } else {
            root_two.agent_id.clone()
        };
        let source_turn = if index % 2 == 0 {
            root_one_turn.clone()
        } else {
            root_two_turn.clone()
        };
        threads.push(thread::spawn(move || {
            barrier.wait();
            coordinator
                .spawn_agent(
                    &source,
                    &source_turn,
                    &next_tool_call_id(),
                    spawn_request(&format!("child_{index}")),
                )
                .expect("并发 SpawnAgent 应成功")
        }));
    }
    barrier.wait();
    let children = threads
        .into_iter()
        .map(|worker| worker.join().expect("spawn 线程不应 panic"))
        .collect::<Vec<_>>();
    let capacity = coordinator.capacity().unwrap();
    assert_eq!(capacity.global_in_use, 2);
    assert!(
        capacity
            .roots
            .iter()
            .all(|(_root, in_use, limit)| in_use <= limit)
    );

    coordinator
        .complete_turn(
            &root_one.agent_id,
            &root_one_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    coordinator
        .complete_turn(
            &root_two.agent_id,
            &root_two_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    loop {
        let running = children
            .iter()
            .filter_map(|child| {
                let status = coordinator.agent_status(&child.agent.agent_id).unwrap();
                match status {
                    CollaborationAgentStatus::Running { turn_id } => {
                        Some((child.agent.agent_id.clone(), turn_id))
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        if running.is_empty() {
            break;
        }
        let barrier = Arc::new(Barrier::new(running.len() + 1));
        let mut finishers = Vec::new();
        for (agent_id, turn_id) in running {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            finishers.push(thread::spawn(move || {
                barrier.wait();
                coordinator
                    .complete_turn(
                        &agent_id,
                        &turn_id,
                        AgentTurnOutcome::Completed {
                            final_message: None,
                        },
                    )
                    .expect("并发终态提交应成功")
            }));
        }
        barrier.wait();
        for finisher in finishers {
            assert_eq!(
                finisher.join().expect("终态线程不应 panic"),
                TurnCompletionDisposition::Committed
            );
        }
        let capacity = coordinator.capacity().unwrap();
        assert!(capacity.global_in_use <= capacity.global_limit);
        assert!(
            capacity
                .roots
                .iter()
                .all(|(_root, in_use, limit)| in_use <= limit)
        );
    }
    assert_eq!(coordinator.capacity().unwrap().global_in_use, 0);
}

/// 验证关闭根 Session 会停止整树、释放槽位、取消令牌并幂等忽略迟到终态。
#[test]
fn closing_root_stops_whole_tree_and_ignores_late_completion() {
    let fixture = fixture(3, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "关闭测试", NO_PLAN)
        .unwrap();
    let first = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("first"),
        )
        .unwrap();
    let second = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("second"),
        )
        .unwrap();
    let launches = fixture.execution.launches();
    assert_eq!(launches.len(), 3);

    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
    assert!(fixture.coordinator.capacity().unwrap().roots.is_empty());
    for agent_id in [
        &fixture.root_agent_id,
        &first.agent.agent_id,
        &second.agent.agent_id,
    ] {
        assert!(matches!(
            fixture.coordinator.agent_status(agent_id).unwrap_err(),
            CollaborationError::AgentNotFound { .. }
        ));
    }
    assert!(
        launches
            .iter()
            .all(|launch| launch.cancellation.is_cancelled())
    );
    assert_eq!(
        fixture
            .execution
            .closes
            .lock()
            .expect("关闭锁不应中毒")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &fixture.root_agent_id,
                &root_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap(),
        TurnCompletionDisposition::IgnoredStale
    );
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "不应启动", NO_PLAN)
            .unwrap_err(),
        CollaborationError::AgentNotFound { .. }
    ));
}

/// 验证 stubborn runner 未确认静止时不会释放槽位、调度后续 Turn 或开始 Worktree 清理。
#[test]
fn rejected_quiesce_keeps_capacity_reserved_and_prevents_oversubscription() {
    let fixture = fixture(1, 1);
    let first_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "无法静止的根任务", NO_PLAN)
        .unwrap();
    let waiting_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("quiesce-waiting-root-session").unwrap(),
            profile: profile("quiesce-waiting-root"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let waiting_turn = fixture
        .coordinator
        .begin_root_turn(&waiting_root.agent_id, "必须等待静止确认", NO_PLAN)
        .unwrap();
    fixture.execution.reject_all_quiesces();

    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let capacity = fixture.coordinator.capacity().unwrap();
    assert_eq!(capacity.global_in_use, 1);
    assert!(
        capacity
            .roots
            .iter()
            .any(|(root_id, in_use, _limit)| { root_id == &fixture.root_agent_id && *in_use == 1 })
    );
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Cancelling { turn_id } if turn_id == first_turn
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&waiting_root.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { turn_id } if turn_id == waiting_turn
    ));
    assert!(
        !fixture
            .execution
            .launches()
            .iter()
            .any(|launch| launch.turn_id == waiting_turn)
    );
    assert!(fixture.execution.quiesces().is_empty());
    assert!(fixture.execution.closes().is_empty());
    assert!(matches!(
        fixture.coordinator.reconcile_outbox().unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);
}

/// 验证静止结果不确定时保留原请求，幂等对账成功后才进入清理并卸载根树。
#[test]
fn unknown_quiesce_is_reconciled_before_cleanup() {
    let fixture = fixture(1, 1);
    fixture.execution.unknown_next_quiesce();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(
        checkpoint.roots[0].lifecycle,
        RecoveredRootLifecycle::Closing
    );
    assert!(fixture.execution.closes().is_empty());
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(fixture.execution.quiesces().len(), 1);
    assert_eq!(fixture.execution.closes().len(), 1);
    assert!(fixture.coordinator.capacity().unwrap().roots.is_empty());
}

/// 验证 QueueOnly 冷加载后仍不启动 Turn，而 TriggerTurn 冷加载后才启动。
#[test]
fn cold_recovery_preserves_identity_mailbox_and_wake_semantics() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建恢复目标", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("cold"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let second_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "QueueOnly 冷加载", NO_PLAN)
        .unwrap();
    fixture.store.install_recovery(
        fixture
            .coordinator
            .checkpoint_root(&fixture.root_agent_id)
            .unwrap(),
    );
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    let launches_before = fixture.execution.launches().len();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &second_root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "cold queue",
        )
        .unwrap();
    assert_eq!(fixture.execution.launches().len(), launches_before);
    assert_eq!(
        fixture
            .coordinator
            .resolve_path(
                &fixture.root_agent_id,
                &AgentPath::parse("/root/cold").unwrap()
            )
            .unwrap()
            .expect("冷恢复 Agent 应驻留")
            .agent_id,
        child.agent.agent_id
    );
    assert_eq!(
        fixture.coordinator.mailbox(&child.agent.agent_id).unwrap()[0].content,
        "cold queue"
    );

    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &second_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let third_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "TriggerTurn 冷加载", NO_PLAN)
        .unwrap();
    fixture.store.install_recovery(
        fixture
            .coordinator
            .checkpoint_root(&fixture.root_agent_id)
            .unwrap(),
    );
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    let launches_before_followup = fixture.execution.launches().len();
    let (_message_id, turn_id) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &third_root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "cold followup",
        )
        .unwrap();
    assert!(turn_id.is_some());
    assert_eq!(
        fixture.execution.launches().len(),
        launches_before_followup + 1
    );
}

/// 验证冷恢复不会接受被篡改的直接父链或 Agent 定义。
#[test]
fn cold_recovery_rejects_tampered_parent_chain() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建校验目标", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("tampered"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let recovery_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "尝试冷恢复", NO_PLAN)
        .unwrap();
    let mut recovery = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap();
    recovery
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .expect("恢复快照应包含子 Agent")
        .definition
        .parent_agent_id = Some(child.agent.agent_id.clone());
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    fixture.store.install_recovery(recovery);
    assert!(matches!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &recovery_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
                "不应接受",
            )
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证安全边界信号失败不会撤销 steer，且后续活动按最新版本合并重试。
#[test]
fn signal_failure_keeps_committed_steer_available() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "信号故障测试", NO_PLAN)
        .unwrap();
    fixture.execution.fail_next_signal();

    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "必须保留的 steer")
        .unwrap();
    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "合并后的 steer")
        .unwrap();
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
    assert_eq!(
        fixture
            .execution
            .signals
            .lock()
            .expect("信号锁不应中毒")
            .len(),
        1
    );
    assert_eq!(
        fixture.execution.signals.lock().expect("信号锁不应中毒")[0].activity_version,
        2
    );
    let steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    assert_eq!(steers.len(), 2);
    assert_eq!(steers[0].content, "必须保留的 steer");
    assert_eq!(steers[1].content, "合并后的 steer");
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &steers,
    );
}

/// 验证全树清理失败不会阻断同批次中属于其他根树的 Turn 启动。
#[test]
fn close_failure_does_not_block_later_start_actions() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "占用第一个槽位", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("occupier"),
        )
        .unwrap();
    let second_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("second-root-session").unwrap(),
            profile: profile("second-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let third_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("third-root-session").unwrap(),
            profile: profile("third-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let second_turn = fixture
        .coordinator
        .begin_root_turn(&second_root.agent_id, "等待第一个释放槽位", NO_PLAN)
        .unwrap();
    let third_turn = fixture
        .coordinator
        .begin_root_turn(&third_root.agent_id, "等待第二个释放槽位", NO_PLAN)
        .unwrap();
    fixture.execution.fail_next_close();

    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let launches = fixture.execution.launches();
    assert!(launches.iter().any(|launch| launch.turn_id == second_turn));
    assert!(launches.iter().any(|launch| launch.turn_id == third_turn));
    assert!(matches!(
        fixture.coordinator.agent_status(&second_root.agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == second_turn
    ));
    assert!(matches!(
        fixture.coordinator.agent_status(&third_root.agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == third_turn
    ));
}

/// 验证同批第一个 Turn 派发失败会独立收敛，且不阻断后续 Turn 派发。
#[test]
fn start_failure_is_compensated_without_blocking_later_start() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "占用第一个槽位", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("start_occupier"),
        )
        .unwrap();
    let failed_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("failed-root-session").unwrap(),
            profile: profile("failed-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let running_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("running-root-session").unwrap(),
            profile: profile("running-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let failed_turn = fixture
        .coordinator
        .begin_root_turn(&failed_root.agent_id, "首个派发应失败", NO_PLAN)
        .unwrap();
    let running_turn = fixture
        .coordinator
        .begin_root_turn(&running_root.agent_id, "后续派发必须成功", NO_PLAN)
        .unwrap();
    fixture.execution.fail_next_start();

    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert!(matches!(
        fixture.coordinator.agent_status(&failed_root.agent_id).unwrap(),
        CollaborationAgentStatus::Failed { turn_id, .. } if turn_id == failed_turn
    ));
    assert!(matches!(
        fixture.coordinator.agent_status(&running_root.agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == running_turn
    ));
    let launches = fixture.execution.launches();
    assert!(!launches.iter().any(|launch| launch.turn_id == failed_turn));
    assert!(launches.iter().any(|launch| launch.turn_id == running_turn));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);
}

/// 验证等待容量的 Turn 可在进入执行端口前被取消或 StopAgent 中断。
#[test]
fn waiting_capacity_turns_can_be_cancelled_and_stopped() {
    let fixture = fixture(1, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "占满全局容量", NO_PLAN)
        .unwrap();
    let cancelled = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("queued_cancel"),
        )
        .unwrap();
    let stopped = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("queued_stop"),
        )
        .unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&cancelled.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&stopped.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));

    assert_eq!(
        fixture
            .coordinator
            .cancel_current_turn(&cancelled.agent.agent_id)
            .unwrap(),
        cancelled.initial_turn_id
    );
    let stop_call_id = fixed_tool_call_id("waiting-capacity-stop");
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &stopped.agent.agent_id,
            )
            .unwrap(),
        stopped.initial_turn_id
    );
    let events_after_stop = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &stop_call_id,
                &stopped.agent.agent_id,
            )
            .unwrap(),
        stopped.initial_turn_id,
        "WaitingCapacity StopAgent 重放必须返回首次 Turn"
    );
    assert_eq!(
        fixture.store.events().len(),
        events_after_stop,
        "WaitingCapacity StopAgent 重放不得追加事件"
    );
    let committed_batch = fixture
        .store
        .batches()
        .into_iter()
        .find(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    &event.kind,
                    CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                        if receipt.key.tool_call_id == stop_call_id
                            && receipt.kind == CollaborationInvocationKind::StopAgent
                )
            })
        })
        .expect("WaitingCapacity StopAgent receipt 应存在于原子批次");
    assert!(committed_batch.events.iter().any(|event| {
        event.agent_id == stopped.agent.agent_id
            && matches!(event.kind, CollaborationEventKind::AgentTurnInterrupted)
    }));
    assert!(matches!(
        fixture.coordinator.agent_status(&cancelled.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id }
            if turn_id == cancelled.initial_turn_id
    ));
    assert!(matches!(
        fixture.coordinator.agent_status(&stopped.agent.agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id }
            if turn_id == stopped.initial_turn_id
    ));
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(fixture.execution.launches().len(), 1);
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
}

/// 验证进程重启后可整体恢复稳定身份、mailbox 与全局事件序号。
#[test]
fn full_root_restore_preserves_identity_mailbox_and_event_sequence() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建完整恢复快照", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("restart"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("子任务完成".to_owned()),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "重启后仍需保留",
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("根任务完成".to_owned()),
            },
        )
        .unwrap();
    let recovered = fixture.coordinator.checkpoint_coordinator().unwrap();
    let recovered_root = recovered
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap();
    let previous_event_count = fixture.store.events().len();
    let previous_sequence = recovered.last_event_sequence;
    assert_eq!(
        fixture.store.events().last().unwrap().sequence,
        previous_sequence
    );

    let restored_execution = Arc::new(RecordingExecution::default());
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(4).unwrap(),
        fixture.store.clone(),
        restored_execution,
        Arc::new(SequentialIds {
            next: AtomicU64::new(10_000),
        }),
    );
    let root_handle = restored
        .restore_coordinator(recovered.clone())
        .unwrap()
        .into_iter()
        .find(|handle| handle.agent_id == fixture.root_agent_id)
        .unwrap();
    assert_eq!(root_handle.agent_id, fixture.root_agent_id);
    assert_eq!(root_handle.session_id, recovered_root.root_session_id);
    assert_eq!(
        restored
            .resolve_path(
                &fixture.root_agent_id,
                &AgentPath::parse("/root/restart").unwrap()
            )
            .unwrap()
            .unwrap()
            .agent_id,
        child.agent.agent_id
    );
    assert_eq!(
        restored.mailbox(&child.agent.agent_id).unwrap()[0].content,
        "重启后仍需保留"
    );

    restored
        .begin_root_turn(&fixture.root_agent_id, "恢复后的新 Turn", NO_PLAN)
        .unwrap();
    let events = fixture.store.events();
    assert_eq!(events[previous_event_count].sequence, previous_sequence + 1);
}

/// 验证完整冷启动把执行结果未知的 Running 状态确定性收敛为 Interrupted。
#[test]
fn full_root_restore_interrupts_unknown_active_turns() {
    let fixture = fixture(2, 2);
    let turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "保持运行状态", NO_PLAN)
        .unwrap();
    let recovered = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        fixture.store.clone(),
        Arc::new(RecordingExecution::default()),
        Arc::new(SequentialIds {
            next: AtomicU64::new(20_000),
        }),
    );

    restored.restore_coordinator(recovered).unwrap();
    assert!(matches!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id: restored_turn_id }
            if restored_turn_id == turn_id
    ));
    assert_eq!(restored.capacity().unwrap().global_in_use, 0);
    assert!(fixture.store.events().iter().any(|event| {
        event.turn_id.as_ref() == Some(&turn_id)
            && matches!(event.kind, CollaborationEventKind::AgentTurnInterrupted)
    }));
}

/// 验证 Runtime Journal 已确认失败时，协调器恢复不得再把同一 Turn 改写成中断。
#[test]
fn full_root_restore_prefers_authoritative_runtime_failure() {
    let fixture = fixture(2, 2);
    let turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "等待 Runtime 终态", NO_PLAN)
        .unwrap();
    let recovered = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        fixture.store.clone(),
        Arc::new(RecordingExecution::default()),
        Arc::new(SequentialIds {
            next: AtomicU64::new(21_000),
        }),
    );
    let failure_message = "Runtime Journal 已确认原 Turn 失败".to_owned();
    let authoritative_outcomes = HashMap::from([(
        turn_id.clone(),
        AgentTurnOutcome::Failed {
            message: failure_message.clone(),
        },
    )]);

    restored
        .restore_coordinator_with_authoritative_outcomes(recovered, &authoritative_outcomes)
        .unwrap();

    assert!(matches!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Failed {
            turn_id: restored_turn_id,
            message
        } if restored_turn_id == turn_id && message == failure_message
    ));
    assert_eq!(restored.capacity().unwrap().global_in_use, 0);
    assert!(fixture.store.events().iter().any(|event| {
        event.turn_id.as_ref() == Some(&turn_id)
            && matches!(
                &event.kind,
                CollaborationEventKind::AgentTurnFailed { message }
                    if message == &failure_message
            )
    }));
}

/// 创建使用既有持久事件 Store 的空恢复协调器。
fn restore_coordinator(
    store: Arc<RecordingStore>,
    global_limit: usize,
    id_seed: u64,
) -> CollaborationCoordinator {
    CollaborationCoordinator::new(
        CollaborationLimits::new(global_limit).expect("恢复容量应有效"),
        store,
        Arc::new(RecordingExecution::default()),
        Arc::new(SequentialIds {
            next: AtomicU64::new(id_seed),
        }),
    )
}

/// 复用全局水位与根身份命名空间，仅替换需要篡改验证的根树集合。
fn snapshot_with_roots(
    base: &RecoveredCoordinator,
    roots: Vec<RecoveredAgentTree>,
) -> RecoveredCoordinator {
    let mut snapshot = base.clone();
    snapshot.roots = roots;
    snapshot
}

/// 验证多根快照共享全局水位和命名空间，失败恢复不留下半棵树，随后仍可整体恢复。
#[test]
fn multi_root_checkpoint_and_restore_are_atomic() {
    let fixture = fixture(4, 2);
    let second_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("second-root-session").unwrap(),
            profile: profile("second-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(snapshot.roots.len(), 2);
    assert!(
        snapshot
            .roots
            .windows(2)
            .all(|pair| pair[0].root_agent_id < pair[1].root_agent_id)
    );

    let restored = restore_coordinator(fixture.store.clone(), 4, 30_000);
    let mut inconsistent = snapshot.clone();
    inconsistent.next_root_sequence = 1;
    assert!(matches!(
        restored.restore_coordinator(inconsistent).unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let handles = restored.restore_coordinator(snapshot.clone()).unwrap();
    assert_eq!(handles.len(), 2);
    assert!(
        handles
            .iter()
            .any(|handle| handle.agent_id == second_root.agent_id)
    );
    assert_eq!(restored.checkpoint_coordinator().unwrap(), snapshot);

    let previous_sequence = snapshot.last_event_sequence;
    restored
        .begin_root_turn(&handles[0].agent_id, "多根恢复后的新 Turn", NO_PLAN)
        .unwrap();
    assert_eq!(
        fixture.store.events()[previous_sequence as usize].sequence,
        previous_sequence + 1
    );
}

/// 验证驱逐只释放驻留状态，稳定路径、身份和 Worktree lease 清理责任仍由根树保留。
#[test]
fn evicted_agent_keeps_identity_and_worktree_cleanup() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建可驱逐子 Agent", NO_PLAN)
        .unwrap();
    let mut request = spawn_request("evicted");
    let worktree_lease = WorktreeLease::new("worktree-keencode-evicted").unwrap();
    request.profile.worktree_lease = Some(worktree_lease.clone());
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            request,
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    assert!(
        fixture
            .coordinator
            .checkpoint_root(&fixture.root_agent_id)
            .unwrap()
            .agents
            .iter()
            .any(|agent| agent.definition.agent_id == child.agent.agent_id)
    );
    let mut duplicate_lease = spawn_request("duplicate_lease");
    duplicate_lease.profile.worktree_lease = Some(worktree_lease.clone());
    assert!(matches!(
        fixture
            .coordinator
            .spawn_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                duplicate_lease
            )
            .unwrap_err(),
        CollaborationError::InvalidAgentProfile { .. }
    ));

    assert!(matches!(
        fixture
            .coordinator
            .register_root(RootAgentRequest {
                session_id: child.agent.session_id.clone(),
                profile: profile("duplicate-evicted-session"),
                per_root_turn_limit: 1,
            })
            .unwrap_err(),
        CollaborationError::IdentifierCollision {
            kind: "Agent Session"
        }
    ));

    assert_eq!(
        fixture
            .coordinator
            .resolve_path(&fixture.root_agent_id, &child.agent.path)
            .unwrap()
            .unwrap(),
        child.agent
    );
    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    let close = fixture.execution.closes().pop().unwrap();
    assert!(close.agent_ids.contains(&child.agent.agent_id));
    assert_eq!(close.worktree_leases, vec![worktree_lease]);
}

/// 验证完整恢复拒绝伪造定义、重复 Session 和重复 Worktree lease。
#[test]
fn full_restore_rejects_forged_definitions_and_duplicate_identities() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建身份恢复快照", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("identity"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let original_snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();
    let original = original_snapshot
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap()
        .clone();

    let mut forged = original.clone();
    forged
        .known_agents
        .iter_mut()
        .find(|definition| definition.agent_id == child.agent.agent_id)
        .unwrap()
        .profile
        .model = "forged-model".to_owned();
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 31_000)
            .restore_coordinator(snapshot_with_roots(&original_snapshot, vec![forged]))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut forged_completion = original.clone();
    forged_completion
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .unwrap()
        .mailbox[0]
        .message
        .content
        .push_str("-tampered");
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 31_500)
            .restore_coordinator(snapshot_with_roots(
                &original_snapshot,
                vec![forged_completion],
            ))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut duplicate_session = original.clone();
    for definition in duplicate_session
        .known_agents
        .iter_mut()
        .chain(
            duplicate_session
                .agents
                .iter_mut()
                .map(|agent| &mut agent.definition),
        )
        .filter(|definition| definition.agent_id == child.agent.agent_id)
    {
        definition.session_id = duplicate_session.root_session_id.clone();
    }
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 32_000)
            .restore_coordinator(snapshot_with_roots(
                &original_snapshot,
                vec![duplicate_session],
            ))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut duplicate_worktree_lease = original;
    let shared_worktree_lease = WorktreeLease::new("shared-worktree-lease").unwrap();
    for definition in duplicate_worktree_lease.known_agents.iter_mut().chain(
        duplicate_worktree_lease
            .agents
            .iter_mut()
            .map(|agent| &mut agent.definition),
    ) {
        definition.profile.worktree_lease = Some(shared_worktree_lease.clone());
    }
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 33_000)
            .restore_coordinator(snapshot_with_roots(
                &original_snapshot,
                vec![duplicate_worktree_lease],
            ))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证恢复 mailbox 不能伪造跨树来源，也不能保留指向已消失 Turn 的归属。
#[test]
fn recovery_rejects_cross_tree_mailbox_and_stale_claims() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建 mailbox 恢复快照", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("mailbox_recovery"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "持久 QueueOnly 消息",
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let second_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("mailbox-second-root").unwrap(),
            profile: profile("mailbox-second-root"),
            per_root_turn_limit: 2,
        })
        .unwrap();
    let second_turn = fixture
        .coordinator
        .begin_root_turn(&second_root.agent_id, "创建第二棵树 mailbox", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &second_root.agent_id,
            &second_turn,
            &next_tool_call_id(),
            &second_root.agent_id,
            "第二棵树消息",
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &second_root.agent_id,
            &second_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();

    let mut cross_tree = snapshot.clone();
    let child_snapshot = cross_tree
        .roots
        .iter_mut()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap()
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap();
    child_snapshot.mailbox[0].message.source_agent_id = second_root.agent_id.clone();
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 34_000)
            .restore_coordinator(cross_tree)
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut stale_claim = snapshot.clone();
    let child_snapshot = stale_claim
        .roots
        .iter_mut()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap()
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap();
    child_snapshot.mailbox[0].claimed_turn_id = Some(TurnId::new("stale-turn").unwrap());
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 35_000)
            .restore_coordinator(stale_claim)
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut duplicate_message_id = snapshot;
    let first_message_id = duplicate_message_id
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap()
        .mailbox[0]
        .message
        .message_id
        .clone();
    duplicate_message_id
        .roots
        .iter_mut()
        .find(|tree| tree.root_agent_id == second_root.agent_id)
        .unwrap()
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == second_root.agent_id)
        .unwrap()
        .mailbox[0]
        .message
        .message_id = first_message_id;
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 35_500)
            .restore_coordinator(duplicate_message_id)
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证局部驱逐 checkpoint 不绑定全局事件水位，其他事件推进后仍可安全冷加载。
#[test]
fn local_agent_checkpoint_survives_unrelated_global_event_progress() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建旧恢复快照", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("stale_recovery"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let second_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "推进事件水位", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    fixture
        .coordinator
        .steer_agent(
            &fixture.root_agent_id,
            &second_root_turn,
            "生成快照之后的新事件",
        )
        .unwrap();
    let consumed_steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &second_root_turn)
        .unwrap();
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &second_root_turn,
        &consumed_steers,
    );

    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &second_root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "全局水位推进后仍可局部恢复",
        )
        .unwrap();
}

/// 验证恢复入口在分配驻留状态前拒绝 mailbox 数量和累计字节超限。
#[test]
fn recovery_enforces_mailbox_count_and_byte_limits() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建 mailbox 边界快照", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("mailbox_limits"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "边界消息",
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let original_snapshot = fixture.coordinator.checkpoint_coordinator().unwrap();
    let original = original_snapshot
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap()
        .clone();

    let mut too_many = original.clone();
    let child_snapshot = too_many
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap();
    let template = child_snapshot.mailbox[0].clone();
    child_snapshot.mailbox = (1_u64..=4_097)
        .map(|sequence| {
            let mut entry = template.clone();
            entry.message.sequence = sequence;
            entry.message.message_id =
                MailboxMessageId::new(format!("limit-message-{sequence}")).unwrap();
            entry
        })
        .collect();
    child_snapshot.next_mailbox_sequence = 4_098;
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 36_000)
            .restore_coordinator(snapshot_with_roots(&original_snapshot, vec![too_many]))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut too_large = original;
    let child_snapshot = too_large
        .agents
        .iter_mut()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap();
    let template = child_snapshot.mailbox[0].clone();
    let maximum_message = "x".repeat(4 * 1024 * 1024);
    child_snapshot.mailbox = (1_u64..=9)
        .map(|sequence| {
            let mut entry = template.clone();
            entry.message.sequence = sequence;
            entry.message.message_id =
                MailboxMessageId::new(format!("byte-message-{sequence}")).unwrap();
            entry.message.content = maximum_message.clone();
            entry
        })
        .collect();
    child_snapshot.next_mailbox_sequence = 10;
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 4, 37_000)
            .restore_coordinator(snapshot_with_roots(&original_snapshot, vec![too_large]))
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证未消费 steer 阻止静默完成，数量和累计字节边界均在持久化前生效。
#[test]
fn pending_steers_block_completion_and_obey_resource_limits() {
    let completion = fixture(2, 2);
    let completion_turn = completion
        .coordinator
        .begin_root_turn(&completion.root_agent_id, "验证未消费 steer", NO_PLAN)
        .unwrap();
    completion
        .coordinator
        .steer_agent(&completion.root_agent_id, &completion_turn, "继续处理")
        .unwrap();
    assert!(matches!(
        completion
            .coordinator
            .complete_turn(
                &completion.root_agent_id,
                &completion_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap_err(),
        CollaborationError::PendingUserSteers { .. }
    ));
    let consumed_steers = completion
        .coordinator
        .consume_user_steers(&completion.root_agent_id, &completion_turn)
        .unwrap();
    assert_eq!(consumed_steers.len(), 1);
    acknowledge_steer_batch(
        completion.coordinator.as_ref(),
        &completion.root_agent_id,
        &completion_turn,
        &consumed_steers,
    );
    completion
        .coordinator
        .complete_turn(
            &completion.root_agent_id,
            &completion_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();

    let count = fixture(2, 2);
    let count_turn = count
        .coordinator
        .begin_root_turn(&count.root_agent_id, "验证 steer 数量边界", NO_PLAN)
        .unwrap();
    for _ in 0..1_024 {
        count
            .coordinator
            .steer_agent(&count.root_agent_id, &count_turn, "s")
            .unwrap();
    }
    assert!(matches!(
        count
            .coordinator
            .steer_agent(&count.root_agent_id, &count_turn, "overflow")
            .unwrap_err(),
        CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 数量",
            maximum: 1_024
        }
    ));

    let bytes = fixture(2, 2);
    let bytes_turn = bytes
        .coordinator
        .begin_root_turn(&bytes.root_agent_id, "验证 steer 字节边界", NO_PLAN)
        .unwrap();
    let maximum_message = "x".repeat(4 * 1024 * 1024);
    bytes
        .coordinator
        .steer_agent(&bytes.root_agent_id, &bytes_turn, maximum_message.clone())
        .unwrap();
    bytes
        .coordinator
        .steer_agent(&bytes.root_agent_id, &bytes_turn, maximum_message)
        .unwrap();
    assert!(matches!(
        bytes
            .coordinator
            .steer_agent(&bytes.root_agent_id, &bytes_turn, "overflow")
            .unwrap_err(),
        CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 总字节数",
            maximum: 8_388_608
        }
    ));
}

/// 验证运行中收到但未消费的 TriggerTurn 会逐 Turn 重归属，消费后不再重复触发。
#[test]
fn unconsumed_trigger_turn_is_reclaimed_until_consumed() {
    let fixture = fixture(4, 4);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "驱动 Followup 重归属", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("trigger_reclaim"),
        )
        .unwrap();
    let (message_id, triggered) = fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "运行中追加任务",
        )
        .unwrap();
    assert!(triggered.is_none());

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let first_followup = fixture
        .execution
        .launches()
        .into_iter()
        .find(|launch| {
            matches!(
                &launch.cause,
                AgentTurnCause::Followup {
                    message_id: candidate
                } if candidate == &message_id
            )
        })
        .unwrap();
    let first_claim = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap()
        .mailbox[0]
        .claimed_turn_id
        .clone();
    assert_eq!(first_claim, Some(first_followup.turn_id.clone()));

    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &first_followup.turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let second_followup = fixture
        .execution
        .launches()
        .into_iter()
        .rev()
        .find(|launch| {
            matches!(
                &launch.cause,
                AgentTurnCause::Followup {
                    message_id: candidate
                } if candidate == &message_id
            )
        })
        .unwrap();
    assert_ne!(second_followup.turn_id, first_followup.turn_id);
    let claimed = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap()
        .mailbox[0]
        .claimed_turn_id
        .clone();
    assert_eq!(claimed, Some(second_followup.turn_id.clone()));

    let launches_before_consumption = fixture.execution.launches().len();
    let consumed = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &second_followup.turn_id, usize::MAX)
        .unwrap();
    assert!(
        consumed
            .iter()
            .any(|message| message.message_id == message_id)
    );
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &second_followup.turn_id,
        &consumed,
    );
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &second_followup.turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(
        fixture.execution.launches().len(),
        launches_before_consumption
    );
}

/// 验证普通 mailbox 达到单 Agent 上限时，子终态仍可用有界通知原子提交。
#[test]
fn child_completion_survives_full_parent_mailbox_and_large_output() {
    let fixture = fixture(3, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "填满父 mailbox", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("large_completion"),
        )
        .unwrap();
    for index in 0..4_096 {
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &fixture.root_agent_id,
                format!("普通消息-{index}"),
            )
            .unwrap();
    }
    let large_final_message = "结果".repeat((4 * 1024 * 1024) / "结果".len());

    assert_eq!(
        fixture
            .coordinator
            .complete_turn(
                &child.agent.agent_id,
                &child.initial_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: Some(large_final_message),
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    let mailbox = fixture.coordinator.mailbox(&fixture.root_agent_id).unwrap();
    assert_eq!(mailbox.len(), 4_097);
    let completion = mailbox.last().expect("父 mailbox 应追加完成通知");
    assert!(completion.content.len() <= 64 * 1024);
    assert!(matches!(
        completion.kind,
        crate::MailboxMessageKind::ChildTurnFinished { .. }
    ));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 1);
}

/// 验证同一子 Agent 的新终态会替代尚未消费的旧完成通知。
#[test]
fn newer_child_completion_supersedes_unconsumed_notification() {
    let fixture = fixture(3, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "驱动子 Agent 重试", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("superseded"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Failed {
                message: "第一次失败".to_owned(),
            },
        )
        .unwrap();
    let retry_turn = fixture
        .coordinator
        .retry_agent(&fixture.root_agent_id, &root_turn, &child.agent.agent_id)
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &retry_turn,
            AgentTurnOutcome::Failed {
                message: "第二次失败".to_owned(),
            },
        )
        .unwrap();

    let child_notifications = fixture
        .coordinator
        .mailbox(&fixture.root_agent_id)
        .unwrap()
        .into_iter()
        .filter(|message| message.source_agent_id == child.agent.agent_id)
        .collect::<Vec<_>>();
    assert_eq!(child_notifications.len(), 1);
    assert_eq!(child_notifications[0].related_turn_id, Some(retry_turn));
    assert!(child_notifications[0].content.contains("第二次失败"));
    assert!(fixture.store.events().iter().any(|event| matches!(
        event.kind,
        CollaborationEventKind::AgentCompletionNotificationSuperseded { .. }
    )));
}

/// 验证不同根树用根身份命名空间生成互不覆盖的单调 TurnId。
#[test]
fn turn_ids_are_namespaced_across_roots() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        store,
        execution,
        Arc::new(SequentialIds::default()),
    );
    let first = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("collision-root-first").unwrap(),
            profile: profile("collision-first"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let second = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("collision-root-second").unwrap(),
            profile: profile("collision-second"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let first_turn = coordinator
        .begin_root_turn(&first.agent_id, "第一个 Turn", NO_PLAN)
        .unwrap();
    let second_turn = coordinator
        .begin_root_turn(&second.agent_id, "第二个 Turn", NO_PLAN)
        .unwrap();
    assert_ne!(first_turn, second_turn);
    assert!(
        first_turn
            .as_str()
            .starts_with(&format!("turn/{}/", first.agent_id))
    );
    assert!(
        second_turn
            .as_str()
            .starts_with(&format!("turn/{}/", second.agent_id))
    );
    assert_eq!(
        coordinator
            .complete_turn(
                &first.agent_id,
                &first_turn,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap(),
        TurnCompletionDisposition::Committed
    );
    coordinator
        .complete_turn(
            &second.agent_id,
            &second_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    assert_eq!(coordinator.capacity().unwrap().global_in_use, 0);
}

/// 验证 checkpoint 只保存单调水位且恢复后继续分配新 TurnId。
#[test]
fn turn_sequence_remains_bounded_and_monotonic_after_restore() {
    let fixture = fixture(2, 2);
    let historical_turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "生成历史 Turn", NO_PLAN)
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &historical_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        fixture.store.clone(),
        Arc::new(RecordingExecution::default()),
        Arc::new(SequentialIds {
            next: AtomicU64::new(50_000),
        }),
    );
    restored.restore_coordinator(checkpoint).unwrap();
    let next_turn_id = restored
        .begin_root_turn(&fixture.root_agent_id, "恢复后继续单调分配", NO_PLAN)
        .unwrap();
    assert_ne!(next_turn_id, historical_turn_id);
    assert!(next_turn_id.as_str().ends_with("/2"));
    restored
        .complete_turn(
            &fixture.root_agent_id,
            &next_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    for index in 0..256 {
        let turn_id = restored
            .begin_root_turn(&fixture.root_agent_id, format!("有界历史 {index}"), NO_PLAN)
            .unwrap();
        restored
            .complete_turn(
                &fixture.root_agent_id,
                &turn_id,
                AgentTurnOutcome::Completed {
                    final_message: None,
                },
            )
            .unwrap();
    }
    assert_eq!(
        restored
            .checkpoint_root(&fixture.root_agent_id)
            .unwrap()
            .next_turn_sequence,
        259
    );
}

/// 验证 StartTurn 确认或失败补偿持久化故障都会保留可显式收敛的 outbox。
#[test]
fn start_turn_outbox_reconciles_after_ack_and_compensation_store_failures() {
    let dispatch_ack = fixture(2, 2);
    dispatch_ack.store.fail_next_dispatch_ack();
    assert!(matches!(
        dispatch_ack
            .coordinator
            .begin_root_turn(&dispatch_ack.root_agent_id, "派发确认故障", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let dispatched_turn = dispatch_ack.execution.launches()[0].turn_id.clone();
    assert_eq!(dispatch_ack.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(dispatch_ack.coordinator.reconcile_outbox().unwrap(), 0);
    assert_eq!(
        dispatch_ack
            .execution
            .launches()
            .iter()
            .filter(|launch| launch.turn_id == dispatched_turn)
            .count(),
        1
    );

    let compensation = fixture(2, 2);
    compensation.execution.fail_next_start();
    compensation.store.fail_next_failure_compensation();
    assert!(matches!(
        compensation
            .coordinator
            .begin_root_turn(&compensation.root_agent_id, "补偿持久化故障", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert!(matches!(
        compensation
            .coordinator
            .agent_status(&compensation.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert_eq!(compensation.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(compensation.coordinator.reconcile_outbox().unwrap(), 0);
}

/// 验证持续派发失败不会对同一未消费 Followup 形成递归或无界迭代补偿。
#[test]
fn persistent_start_failure_releases_followup_claim_without_rescheduling_loop() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建可失败 Followup", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("persistent_start"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Failed {
                message: "允许 Followup".to_owned(),
            },
        )
        .unwrap();
    fixture.execution.fail_all_starts();

    assert!(matches!(
        fixture
            .coordinator
            .followup_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
                "派发持续失败",
            )
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Failed { .. }
    ));
    let child_checkpoint = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.definition.agent_id == child.agent.agent_id)
        .unwrap();
    assert!(
        child_checkpoint
            .mailbox
            .iter()
            .all(|mailbox| mailbox.claimed_turn_id.is_none())
    );
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
}

/// 验证 CloseTree 确认故障与进程重启都不会丢失清理命令。
#[test]
fn close_tree_outbox_reconciles_and_survives_closed_tree_restore() {
    let ack_failure = fixture(1, 1);
    ack_failure.store.fail_next_cleanup_ack();
    assert!(matches!(
        ack_failure
            .coordinator
            .close_root_session(&ack_failure.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert_eq!(ack_failure.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(ack_failure.coordinator.reconcile_outbox().unwrap(), 0);
    assert!(matches!(
        ack_failure
            .coordinator
            .checkpoint_root(&ack_failure.root_agent_id)
            .unwrap_err(),
        CollaborationError::AgentNotFound { .. }
    ));

    let restart = fixture(1, 1);
    restart.execution.fail_next_close();
    assert!(matches!(
        restart
            .coordinator
            .close_root_session(&restart.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let closed_checkpoint = restart.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(
        closed_checkpoint.roots[0].lifecycle,
        RecoveredRootLifecycle::CleanupPending
    );
    let restored_execution = Arc::new(RecordingExecution::default());
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        restart.store.clone(),
        restored_execution.clone(),
        Arc::new(SequentialIds {
            next: AtomicU64::new(50_000),
        }),
    );
    restored.restore_coordinator(closed_checkpoint).unwrap();
    assert_eq!(restored.reconcile_outbox().unwrap(), 1);
    assert_eq!(restored_execution.closes().len(), 1);
    assert!(matches!(
        restored
            .checkpoint_root(&restart.root_agent_id)
            .unwrap_err(),
        CollaborationError::AgentNotFound { .. }
    ));
}

/// 验证 live checkpoint 的三种未决状态、steer 与 TriggerTurn 归属可确定性恢复。
#[test]
fn live_checkpoint_interrupts_pending_states_and_retry_preserves_steer() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "保持根 Running", NO_PLAN)
        .unwrap();
    let cancelling = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("crash_cancelling"),
        )
        .unwrap();
    fixture
        .coordinator
        .steer_agent(
            &cancelling.agent.agent_id,
            &cancelling.initial_turn_id,
            "崩溃前 steer",
        )
        .unwrap();
    fixture
        .coordinator
        .followup_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &cancelling.agent.agent_id,
            "崩溃前 followup",
        )
        .unwrap();
    let waiting = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("crash_waiting"),
        )
        .unwrap();
    fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &cancelling.agent.agent_id,
        )
        .unwrap();
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let checkpoint_root = checkpoint
        .roots
        .iter()
        .find(|tree| tree.root_agent_id == fixture.root_agent_id)
        .unwrap();
    assert!(checkpoint_root.live);
    assert!(checkpoint_root.agents.iter().any(|agent| matches!(
        agent.status,
        CollaborationAgentStatus::Running { ref turn_id } if turn_id == &root_turn
    )));
    assert!(checkpoint_root.agents.iter().any(|agent| matches!(
        agent.status,
        CollaborationAgentStatus::Cancelling { ref turn_id }
            if turn_id == &cancelling.initial_turn_id
    )));
    assert!(checkpoint_root.agents.iter().any(|agent| matches!(
        agent.status,
        CollaborationAgentStatus::WaitingCapacity { ref turn_id }
            if turn_id == &waiting.initial_turn_id
    )));

    let restored = restore_coordinator(fixture.store.clone(), 2, 60_000);
    restored.restore_coordinator(checkpoint).unwrap();
    for (agent_id, turn_id) in [
        (&fixture.root_agent_id, &root_turn),
        (&cancelling.agent.agent_id, &cancelling.initial_turn_id),
        (&waiting.agent.agent_id, &waiting.initial_turn_id),
    ] {
        assert!(matches!(
            restored.agent_status(agent_id).unwrap(),
            CollaborationAgentStatus::Interrupted { turn_id: restored_turn }
                if &restored_turn == turn_id
        ));
    }
    let recovered_checkpoint = restored.checkpoint_root(&fixture.root_agent_id).unwrap();
    let recovered_cancelling = recovered_checkpoint
        .agents
        .iter()
        .find(|agent| agent.definition.agent_id == cancelling.agent.agent_id)
        .unwrap();
    assert_eq!(recovered_cancelling.pending_steers.len(), 1);
    assert!(
        recovered_cancelling
            .mailbox
            .iter()
            .all(|mailbox| mailbox.claimed_turn_id.is_none())
    );

    let restored_root_turn = restored
        .begin_root_turn(&fixture.root_agent_id, "恢复后的来源 Turn", NO_PLAN)
        .unwrap();
    assert!(matches!(
        restored
            .followup_agent(
                &fixture.root_agent_id,
                &restored_root_turn,
                &next_tool_call_id(),
                &cancelling.agent.agent_id,
                "不能绕过旧 steer 创建新 Followup",
            )
            .unwrap_err(),
        CollaborationError::PendingUserSteers { .. }
    ));
    let retry_turn = restored
        .retry_agent(
            &fixture.root_agent_id,
            &restored_root_turn,
            &cancelling.agent.agent_id,
        )
        .unwrap();
    let steers = restored
        .consume_user_steers(&cancelling.agent.agent_id, &retry_turn)
        .unwrap();
    assert_eq!(steers.len(), 1);
    assert_eq!(steers[0].turn_id, retry_turn);
    assert_eq!(steers[0].content, "崩溃前 steer");
    acknowledge_steer_batch(&restored, &cancelling.agent.agent_id, &retry_turn, &steers);
}

/// 验证运行时树级普通消息字节边界与同一状态生成的可恢复 checkpoint 一致。
#[test]
fn runtime_tree_mailbox_byte_limit_produces_restorable_checkpoint() {
    let fixture = fixture(3, 3);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "填充树级 mailbox", NO_PLAN)
        .unwrap();
    let first = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("tree_limit_first"),
        )
        .unwrap();
    let second = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("tree_limit_second"),
        )
        .unwrap();
    let maximum_message = "x".repeat(4 * 1024 * 1024);
    for target in [&first.agent.agent_id, &second.agent.agent_id] {
        for _ in 0..8 {
            fixture
                .coordinator
                .send_message(
                    &fixture.root_agent_id,
                    &root_turn,
                    &next_tool_call_id(),
                    target,
                    maximum_message.clone(),
                )
                .unwrap();
        }
    }
    assert!(matches!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &fixture.root_agent_id,
                maximum_message,
            )
            .unwrap_err(),
        CollaborationError::ResourceLimitExceeded {
            resource: "单棵根树未消费普通 mailbox 正文字节数",
            maximum: 68_747_264
        }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let root_agent_id = fixture.root_agent_id.clone();
    let store = fixture.store.clone();
    drop(fixture);

    let restored = restore_coordinator(store, 3, 70_000);
    restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(restored.capacity().unwrap().global_in_use, 0);
    assert!(matches!(
        restored.agent_status(&root_agent_id).unwrap(),
        CollaborationAgentStatus::Interrupted { .. }
    ));
    restored.checkpoint_quiescent_root(&root_agent_id).unwrap();
}

/// 验证旧 Turn 延迟到新 Turn 期间执行时不能冒充当前协作调用者。
#[test]
fn stale_source_turn_is_rejected_for_every_collaboration_operation() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "旧来源 Turn", NO_PLAN)
        .unwrap();
    let running_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("stale_running"),
        )
        .unwrap();
    let failed_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("stale_failed"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &failed_child.agent.agent_id,
            &failed_child.initial_turn_id,
            AgentTurnOutcome::Failed {
                message: "准备重试目标".to_owned(),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &running_child.agent.agent_id,
            &running_child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &first_root_turn,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let _second_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "当前来源 Turn", NO_PLAN)
        .unwrap();
    let events_before = fixture.store.events().len();
    let launches_before = fixture.execution.launches().len();

    let errors = vec![
        fixture
            .coordinator
            .spawn_agent(
                &fixture.root_agent_id,
                &first_root_turn,
                &next_tool_call_id(),
                spawn_request("stale_spawn"),
            )
            .unwrap_err(),
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &first_root_turn,
                &next_tool_call_id(),
                &running_child.agent.agent_id,
                "过期 send",
            )
            .unwrap_err(),
        fixture
            .coordinator
            .followup_agent(
                &fixture.root_agent_id,
                &first_root_turn,
                &next_tool_call_id(),
                &running_child.agent.agent_id,
                "过期 followup",
            )
            .unwrap_err(),
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &first_root_turn,
                &next_tool_call_id(),
                &running_child.agent.agent_id,
            )
            .unwrap_err(),
        fixture
            .coordinator
            .retry_agent(
                &fixture.root_agent_id,
                &first_root_turn,
                &failed_child.agent.agent_id,
            )
            .unwrap_err(),
    ];
    assert!(
        errors
            .into_iter()
            .all(|error| matches!(error, CollaborationError::TurnMismatch { .. }))
    );
    assert_eq!(fixture.store.events().len(), events_before);
    assert_eq!(fixture.execution.launches().len(), launches_before);
}

/// 验证运行中目标延迟消费 Followup 时沿用消息来源 Turn 的原始因果链。
#[test]
fn delayed_followup_uses_message_source_turn_causality() {
    let fixture = fixture(4, 4);
    let first_root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "第一根 Turn", NO_PLAN)
        .unwrap();
    let source_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("causal_source"),
        )
        .unwrap();
    let target_child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &first_root_turn,
            &next_tool_call_id(),
            spawn_request("causal_target"),
        )
        .unwrap();
    let (_message_id, triggered_turn) = fixture
        .coordinator
        .followup_agent(
            &source_child.agent.agent_id,
            &source_child.initial_turn_id,
            &next_tool_call_id(),
            &target_child.agent.agent_id,
            "跨子 Turn 的后续任务",
        )
        .unwrap();
    assert!(triggered_turn.is_none());
    fixture
        .coordinator
        .complete_turn(
            &target_child.agent.agent_id,
            &target_child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .unwrap();
    let followup_launch = fixture
        .execution
        .launches()
        .into_iter()
        .find(|launch| {
            launch.agent.agent_id == target_child.agent.agent_id
                && launch.turn_id != target_child.initial_turn_id
        })
        .expect("子 Agent 应启动延迟 Followup Turn");
    assert_eq!(
        followup_launch.parent_turn_id,
        Some(source_child.initial_turn_id)
    );
    assert_eq!(followup_launch.root_turn_id, first_root_turn);
}

/// 验证 Store 在提交后断线时用同一稳定批次完成对账且不重复事件。
#[test]
fn commit_then_indeterminate_store_result_is_reconciled_without_duplicate_events() {
    let fixture = fixture(2, 2);
    fixture.store.commit_then_indeterminate();
    let turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "提交后断线", NO_PLAN)
        .unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { turn_id: running } if running == turn_id
    ));
    let events = fixture.store.events();
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1)
    );
    let unique_sequences = events
        .iter()
        .map(|event| event.sequence)
        .collect::<HashSet<_>>();
    assert_eq!(unique_sequences.len(), events.len());
}

/// 验证连续不确定的 Store 结果会冻结协调器，防止旧内存水位继续写入。
#[test]
fn repeated_indeterminate_store_result_requires_recovery() {
    let fixture = fixture(2, 2);
    fixture.store.indeterminate_without_commit(2);
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "连续不确定", NO_PLAN)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(matches!(
        fixture.coordinator.capacity().unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
}

/// 验证未创建任务的可重试不确定启动保留 outbox，重试后只创建一次任务。
#[test]
fn retryable_unknown_start_keeps_turn_running_until_reconciled() {
    let fixture = fixture(2, 2);
    fixture.execution.unknown_next_start();
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "启动结果不确定", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Running { .. }
    ));
    assert!(fixture.execution.launches().is_empty());
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(fixture.execution.launches().len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
}

/// 验证执行端已创建任务但响应中断时，幂等重试不会创建孤儿或重复任务。
#[test]
fn accepted_then_unknown_start_is_acknowledged_idempotently() {
    let fixture = fixture(2, 2);
    fixture.execution.accept_then_unknown();
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "已接受后断线", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert_eq!(fixture.execution.launches().len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    assert_eq!(fixture.execution.launches().len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
}

/// 验证阻塞 StartTurn 与 CloseTree 按根线性化，成功关闭后不会出现更晚的启动副作用。
#[test]
fn start_turn_and_close_tree_are_linearized_per_root() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(BlockingStartExecution::new());
    let coordinator = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        store,
        execution.clone(),
        Arc::new(SequentialIds::default()),
    ));
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("linearized-root-session").unwrap(),
            profile: profile("linearized-root"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let start_coordinator = coordinator.clone();
    let start_root_id = root.agent_id.clone();
    let starter = thread::spawn(move || {
        start_coordinator.begin_root_turn(&start_root_id, "阻塞启动", NO_PLAN)
    });
    execution.start_entered.wait();

    let close_returned = Arc::new(AtomicBool::new(false));
    let close_coordinator = coordinator.clone();
    let close_root_id = root.agent_id.clone();
    let close_flag = close_returned.clone();
    let closer = thread::spawn(move || {
        let result = close_coordinator.close_root_session(&close_root_id);
        close_flag.store(true, Ordering::SeqCst);
        result
    });
    thread::sleep(Duration::from_millis(25));
    assert!(!close_returned.load(Ordering::SeqCst));
    execution.release_start.wait();

    starter.join().expect("启动线程不应 panic").unwrap();
    closer.join().expect("关闭线程不应 panic").unwrap();
    assert_eq!(execution.events(), vec!["start", "close"]);
    assert!(close_returned.load(Ordering::SeqCst));
    assert!(coordinator.capacity().unwrap().roots.is_empty());
}

/// 应用暂停只保留可恢复账本；已完成子 Agent、父 mailbox 与协作回执在冷恢复后仍使用原身份。
#[test]
fn suspend_preserves_completed_child_identity_mailbox_and_receipt_after_restore() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "完成后暂停", NO_PLAN)
        .unwrap();
    let spawn_call_id = fixed_tool_call_id("suspend-completed-child");
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &spawn_call_id,
            spawn_request("suspend_completed_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("子 Agent 已完成".to_owned()),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &root_turn,
            AgentTurnOutcome::Completed {
                final_message: Some("根 Agent 已完成".to_owned()),
            },
        )
        .unwrap();

    let before = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(before.roots.len(), 1);
    assert_eq!(before.roots[0].agents.len(), 2);
    assert_eq!(before.roots[0].agents[0].definition.path, AgentPath::root());
    assert_eq!(
        before.roots[0].agents[1].definition.agent_id,
        child.agent.agent_id
    );
    assert_eq!(before.invocations.len(), 1);

    fixture
        .coordinator
        .suspend_root_session(&fixture.root_agent_id)
        .unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::TreeClosed { .. }
    ));
    assert!(fixture.execution.quiesces().is_empty());
    assert!(fixture.execution.closes().is_empty());
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(checkpoint.roots[0].lifecycle, RecoveredRootLifecycle::Open);
    assert_eq!(checkpoint.roots[0].agents.len(), 2);
    assert_eq!(checkpoint.invocations.len(), 1);

    let restored = restore_coordinator(fixture.store.clone(), 2, 93_000);
    let handles = restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].agent_id, fixture.root_agent_id);
    let agents = restored
        .list_agents_for_root(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].agent.path, AgentPath::root());
    assert_eq!(agents[0].agent.agent_id, fixture.root_agent_id);
    assert_eq!(agents[1].agent.agent_id, child.agent.agent_id);
    assert!(matches!(
        agents[0].status,
        CollaborationAgentStatus::Completed { .. }
    ));
    assert!(matches!(
        agents[1].status,
        CollaborationAgentStatus::Completed { .. }
    ));
    assert_eq!(restored.mailbox(&fixture.root_agent_id).unwrap().len(), 1);
    assert_eq!(
        restored.checkpoint_coordinator().unwrap().invocations.len(),
        1
    );
}

/// 暂停必须先取消运行中的 Turn、收敛容量等待队列，并且不再向执行端派发新的 Turn。
#[test]
fn suspend_interrupts_active_and_waiting_turns_without_new_start() {
    let fixture = fixture(1, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "暂停根 Turn", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &fixed_tool_call_id("suspend-waiting-child"),
            spawn_request("suspend_waiting_child"),
        )
        .unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::WaitingCapacity { .. }
    ));
    assert_eq!(fixture.execution.launches().len(), 1);

    fixture
        .coordinator
        .suspend_root_session(&fixture.root_agent_id)
        .unwrap();

    assert!(
        fixture
            .execution
            .launch(&root_turn)
            .cancellation
            .is_cancelled()
    );
    assert_eq!(fixture.execution.launches().len(), 1);
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&fixture.root_agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id } if turn_id == root_turn
    ));
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child.agent.agent_id)
            .unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id } if turn_id == child.initial_turn_id
    ));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
    assert_eq!(
        fixture
            .coordinator
            .capacity()
            .unwrap()
            .roots
            .first()
            .map(|(_, in_use, _)| *in_use),
        Some(0)
    );
    assert_eq!(fixture.store.events().len(), {
        let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
        checkpoint.last_event_sequence as usize
    });
    assert!(
        fixture.coordinator.reconcile_outbox().unwrap() == 0,
        "暂停后不得遗留可派发 StartTurn/SignalTurn outbox"
    );
}

/// Store checkpoint 连续提交失败后，同一协调器实例必须保持 fail-closed。
#[test]
fn suspend_checkpoint_failure_freezes_same_instance_before_new_start_or_dispatch() {
    let fixture = fixture(1, 1);
    fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "暂停 checkpoint 故障", NO_PLAN)
        .unwrap();
    let launches_before_failure = fixture.execution.launches().len();
    fixture.store.indeterminate_without_commit(2);

    assert!(matches!(
        fixture
            .coordinator
            .suspend_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "故障后不得新启动", NO_PLAN)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(matches!(
        fixture.coordinator.reconcile_outbox().unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert_eq!(
        fixture.execution.launches().len(),
        launches_before_failure,
        "checkpoint 提交失败后不得产生新的 StartTurn 副作用"
    );
}

/// 暂停前已进入 outbox 的迟到 StartTurn/SignalTurn 不得越过根级 execution fence。
#[test]
fn suspend_execution_fence_discards_late_start_and_signal_actions() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(BlockingStartExecution::new());
    let coordinator = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(2).unwrap(),
        store,
        execution.clone(),
        Arc::new(SequentialIds::default()),
    ));
    let first_root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("late-fence-first-session").unwrap(),
            profile: profile("late-fence-first"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let second_root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("late-fence-second-session").unwrap(),
            profile: profile("late-fence-second"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let first_turn = TurnId::new("late-fence-first-turn").unwrap();
    let second_turn = TurnId::new("late-fence-second-turn").unwrap();

    execution.unknown_next_start();
    assert!(matches!(
        coordinator
            .begin_root_turn_with_id(
                &first_root.agent_id,
                first_turn.clone(),
                "留下第一个 StartTurn outbox",
                NO_PLAN,
            )
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    execution.unknown_next_start();
    assert!(matches!(
        coordinator
            .begin_root_turn_with_id(
                &second_root.agent_id,
                second_turn.clone(),
                "留下第二个 StartTurn outbox",
                NO_PLAN,
            )
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    execution.fail_next_signal();
    coordinator
        .steer_agent(
            &first_root.agent_id,
            &first_turn,
            "留下第一个 SignalTurn outbox",
        )
        .unwrap();
    execution.fail_next_signal();
    coordinator
        .steer_agent(
            &second_root.agent_id,
            &second_turn,
            "留下第二个 SignalTurn outbox",
        )
        .unwrap();
    assert!(
        execution.signals().is_empty(),
        "故障注入应留下未送达 SignalTurn"
    );

    let reconcile_coordinator = coordinator.clone();
    let reconciler = thread::spawn(move || reconcile_coordinator.reconcile_outbox());
    execution.start_entered.wait();
    let blocked_root = execution.blocked_root();
    let paused_root = if blocked_root == first_root.agent_id {
        second_root.agent_id.clone()
    } else {
        first_root.agent_id.clone()
    };
    let paused_turn = if paused_root == first_root.agent_id {
        first_turn.clone()
    } else {
        second_turn.clone()
    };

    coordinator
        .suspend_root_session(&paused_root)
        .expect("暂停未被阻塞根的另一个根应成功");
    execution.release_start.wait();
    assert_eq!(
        reconciler
            .join()
            .expect("outbox 对账线程不应 panic")
            .unwrap(),
        4,
        "对账快照应包含两个 StartTurn 与两个 SignalTurn"
    );

    let accepted_turns = execution.accepted_turns();
    assert_eq!(accepted_turns.len(), 1, "只有未暂停根的 StartTurn 可以送达");
    assert!(
        !accepted_turns.contains(&paused_turn),
        "暂停根的迟到 StartTurn 不得产生执行副作用"
    );
    let signals = execution.signals();
    assert_eq!(signals.len(), 1, "只有未暂停根的 SignalTurn 可以送达");
    assert!(
        signals.iter().all(|signal| signal.agent_id != paused_root),
        "暂停根的迟到 SignalTurn 不得产生执行副作用"
    );
    assert!(matches!(
        coordinator.agent_status(&paused_root).unwrap(),
        CollaborationAgentStatus::Interrupted { turn_id } if turn_id == paused_turn
    ));
}

/// 验证关闭返回后旧 runner 的协作工具调用全部被根墓碑与卸载状态隔离。
#[test]
fn runner_collaboration_calls_are_isolated_after_close() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "关闭前 runner", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("closed_runner_child"),
        )
        .unwrap();
    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    let events_after_close = fixture.store.events().len();
    let launches_after_close = fixture.execution.launches().len();

    assert!(
        fixture
            .coordinator
            .spawn_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                spawn_request("closed_runner_spawn"),
            )
            .is_err()
    );
    assert!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
                "关闭后的 send",
            )
            .is_err()
    );
    assert!(
        fixture
            .coordinator
            .followup_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
                "关闭后的 followup",
            )
            .is_err()
    );
    assert!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child.agent.agent_id,
            )
            .is_err()
    );
    assert!(
        fixture
            .coordinator
            .retry_agent(&fixture.root_agent_id, &root_turn, &child.agent.agent_id,)
            .is_err()
    );
    assert_eq!(fixture.store.events().len(), events_after_close);
    assert_eq!(fixture.execution.launches().len(), launches_after_close);
}

/// 验证已经清理的根树会立即卸载，不会耗尽协调器的活跃根树上限。
#[test]
fn cleaned_roots_do_not_consume_active_root_limit() {
    let store = Arc::new(RecordingStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let coordinator = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        store,
        execution,
        Arc::new(SequentialIds::default()),
    );
    for index in 0..1_025 {
        let root = coordinator
            .register_root(RootAgentRequest {
                session_id: SessionId::new(format!("recycled-root-session-{index}")).unwrap(),
                profile: profile("recycled-root"),
                per_root_turn_limit: 1,
            })
            .unwrap();
        if index == 0 {
            let root_turn = coordinator
                .begin_root_turn(&root.agent_id, "创建后随树清理的幂等记录", NO_PLAN)
                .unwrap();
            coordinator
                .spawn_agent(
                    &root.agent_id,
                    &root_turn,
                    &fixed_tool_call_id("cleanup-invocation"),
                    spawn_request("cleanup_child"),
                )
                .unwrap();
        }
        coordinator.close_root_session(&root.agent_id).unwrap();
    }
    assert!(coordinator.capacity().unwrap().roots.is_empty());
    assert!(
        coordinator
            .checkpoint_coordinator()
            .unwrap()
            .invocations
            .is_empty()
    );
}

/// 验证 Turn 自行消费全部输入后会作废未送达的冗余信号 outbox。
#[test]
fn consumed_activity_invalidates_pending_signal_outbox() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "作废信号 outbox", NO_PLAN)
        .unwrap();
    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "自行消费 steer")
        .unwrap();
    let consumed_steers = fixture
        .coordinator
        .consume_user_steers(&fixture.root_agent_id, &root_turn)
        .unwrap();
    assert_eq!(consumed_steers.len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
    acknowledge_steer_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &consumed_steers,
    );

    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &fixture.root_agent_id,
            "自行消费 mailbox",
        )
        .unwrap();
    let consumed_mailbox = fixture
        .coordinator
        .consume_mailbox(&fixture.root_agent_id, &root_turn, 1)
        .unwrap();
    assert_eq!(consumed_mailbox.len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &fixture.root_agent_id,
        &root_turn,
        &consumed_mailbox,
    );
}

/// 验证关闭根树会同时作废不确定 StartTurn 与失败 SignalTurn 的全部 outbox。
#[test]
fn close_invalidates_start_and_signal_outboxes() {
    let fixture = fixture(2, 2);
    fixture.execution.unknown_next_start();
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "关闭前不确定启动", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let root_turn = match fixture
        .coordinator
        .agent_status(&fixture.root_agent_id)
        .unwrap()
    {
        CollaborationAgentStatus::Running { turn_id } => turn_id,
        status => panic!("根 Agent 应保持 Running，实际为 {status:?}"),
    };
    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .steer_agent(&fixture.root_agent_id, &root_turn, "关闭前待唤醒输入")
        .unwrap();
    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 0);
    assert!(fixture.execution.launches().is_empty());
    assert_eq!(fixture.execution.closes().len(), 1);
}

/// 验证单调根 counter 在释放执行 fence 后仍不会复用已清理根身份。
#[test]
fn monotonic_root_counter_prevents_reuse_without_permanent_tombstones() {
    let repeated_root_id = AgentId::new("repeated-closed-root").unwrap();
    let coordinator = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        Arc::new(RecordingStore::default()),
        Arc::new(RecordingExecution::default()),
        Arc::new(RepeatingAgentIds {
            agent_id: repeated_root_id.clone(),
            next: AtomicU64::new(0),
        }),
    );
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("first-tombstone-session").unwrap(),
            profile: profile("first-tombstone"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    coordinator.close_root_session(&root.agent_id).unwrap();
    assert_eq!(coordinator.execution_fence_count(), 0);
    let second = coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("second-tombstone-session").unwrap(),
            profile: profile("second-tombstone"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    assert_ne!(second.agent_id, root.agent_id);
    assert_eq!(second.agent_id.as_str(), "root/repeated-closed-root/2");
}

/// 验证同一 Turn 的 mailbox 与用户 steer 信号各自保留、消费和确认。
#[test]
fn mailbox_and_user_steer_signal_outboxes_are_independent() {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建双信号目标", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("dual_signal"),
        )
        .unwrap();

    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child.agent.agent_id,
            "mailbox 信号",
        )
        .unwrap();
    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .steer_agent(&child.agent.agent_id, &child.initial_turn_id, "steer 信号")
        .unwrap();

    let consumed_mailbox = fixture
        .coordinator
        .consume_mailbox(&child.agent.agent_id, &child.initial_turn_id, 1)
        .unwrap();
    assert_eq!(consumed_mailbox.len(), 1);
    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    acknowledge_mailbox_batch(
        fixture.coordinator.as_ref(),
        &child.agent.agent_id,
        &child.initial_turn_id,
        &consumed_mailbox,
    );
    let signals = fixture.execution.signals.lock().expect("信号锁不应中毒");
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].kind, AgentTurnSignalKind::UserSteer);
}

/// 验证 Store 明确返回 Conflict 时协调器立即冻结，不再接受领域操作。
#[test]
fn store_conflict_freezes_coordinator_immediately() {
    let fixture = fixture(1, 1);
    *fixture.store.sequence.lock().expect("序号锁不应中毒") += 1;
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "制造 Store 冲突", NO_PLAN)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "冻结后不得继续", NO_PLAN)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
}

/// 验证空根集快照仍恢复原命名空间与下一根序号。
#[test]
fn empty_coordinator_restore_preserves_root_namespace_and_counter() {
    let fixture = fixture(1, 1);
    fixture
        .coordinator
        .close_root_session(&fixture.root_agent_id)
        .unwrap();
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert!(checkpoint.roots.is_empty());
    let expected_root_id = format!(
        "root/{}/{}",
        checkpoint.root_identity_namespace, checkpoint.next_root_sequence
    );

    let restored = restore_coordinator(fixture.store.clone(), 1, 80_000);
    restored.restore_coordinator(checkpoint).unwrap();
    let root = restored
        .register_root(RootAgentRequest {
            session_id: SessionId::new("empty-restore-next-root-session").unwrap(),
            profile: profile("empty-restore-next-root"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    assert_eq!(root.agent_id.as_str(), expected_root_id);
}

/// 创建已达到指定 Agent 数量且单树仍合法的恢复树，用于协调器总配额验证。
fn recovered_tree_with_agent_count(
    template: &RecoveredAgentTree,
    namespace: &AgentId,
    root_sequence: usize,
    agent_count: usize,
) -> RecoveredAgentTree {
    let root_agent_id = AgentId::new(format!("root/{namespace}/{root_sequence}")).unwrap();
    let root_session_id = SessionId::new(format!("quota-root-session-{root_sequence}")).unwrap();
    let root_template = template
        .known_agents
        .iter()
        .find(|definition| definition.depth == AgentDepth::ROOT)
        .expect("模板应包含根定义");
    let recovered_template = template
        .agents
        .iter()
        .find(|agent| agent.definition.depth == AgentDepth::ROOT)
        .expect("模板应包含根恢复状态");
    let mut root_definition = root_template.clone();
    root_definition.agent_id = root_agent_id.clone();
    root_definition.session_id = root_session_id.clone();
    root_definition.root_agent_id = root_agent_id.clone();
    root_definition.root_session_id = root_session_id.clone();
    root_definition.parent_agent_id = None;
    root_definition.path = AgentPath::root();
    root_definition.depth = AgentDepth::ROOT;
    root_definition.context_inheritance = ContextInheritance::None;
    let mut root_agent = recovered_template.clone();
    root_agent.definition = root_definition.clone();
    root_agent.status = CollaborationAgentStatus::Idle;
    root_agent.mailbox.clear();
    root_agent.next_mailbox_sequence = 1;
    root_agent.next_steer_sequence = 1;
    root_agent.last_turn = None;
    root_agent.current_source_agent_id = None;
    root_agent.current_turn_cause = None;
    root_agent.current_turn_prompt = None;
    root_agent.current_parent_turn_id = None;
    root_agent.current_root_turn_id = None;
    root_agent.pending_steers.clear();
    root_agent.start_pending = false;

    let mut known_agents = Vec::with_capacity(agent_count);
    let mut agents = Vec::with_capacity(agent_count);
    known_agents.push(root_definition.clone());
    agents.push(root_agent.clone());
    for child_index in 1..agent_count {
        let mut definition = root_definition.clone();
        definition.agent_id =
            AgentId::new(format!("quota-agent-{root_sequence}-{child_index}")).unwrap();
        definition.session_id =
            SessionId::new(format!("quota-session-{root_sequence}-{child_index}")).unwrap();
        definition.parent_agent_id = Some(root_agent_id.clone());
        definition.path = AgentPath::root()
            .child(format!("child_{child_index}"))
            .unwrap();
        definition.depth = AgentDepth::CHILD;
        let mut agent = root_agent.clone();
        agent.definition = definition.clone();
        known_agents.push(definition);
        agents.push(agent);
    }
    RecoveredAgentTree {
        root_agent_id,
        root_session_id,
        per_root_turn_limit: 1,
        lifecycle: RecoveredRootLifecycle::Open,
        live: true,
        next_turn_sequence: 1,
        next_checkpoint_revision: 1,
        known_agents,
        agents,
    }
}

/// 验证每棵树都合法时，完整恢复仍拒绝跨根树累计 Agent 总量超限。
#[test]
fn restore_rejects_coordinator_total_agent_quota() {
    let fixture = fixture(1, 1);
    let base = fixture.coordinator.checkpoint_coordinator().unwrap();
    let template = &base.roots[0];
    let root_count = MAX_AGENTS_PER_COORDINATOR / MAX_AGENTS_PER_ROOT + 1;
    let roots = (1..=root_count)
        .map(|root_sequence| {
            recovered_tree_with_agent_count(
                template,
                &base.root_identity_namespace,
                root_sequence,
                MAX_AGENTS_PER_ROOT,
            )
        })
        .collect::<Vec<_>>();
    let recovered = RecoveredCoordinator {
        last_event_sequence: base.last_event_sequence,
        root_identity_namespace: base.root_identity_namespace,
        next_root_sequence: (root_count + 1) as u64,
        roots,
        invocations: Vec::new(),
        root_turn_bindings: Vec::new(),
    };
    assert!(matches!(
        restore_coordinator(fixture.store.clone(), 1, 81_000)
            .restore_coordinator(recovered)
            .unwrap_err(),
        CollaborationError::ResourceLimitExceeded {
            resource: "协调器 Agent 身份数量",
            maximum: MAX_AGENTS_PER_COORDINATOR
        }
    ));
}

/// 创建一个已完成并被驱逐的子 Agent，返回其身份和原始局部 checkpoint。
fn evicted_child_checkpoint_fixture() -> (Fixture, AgentId, RecoveredAgentCheckpoint, TurnId) {
    let fixture = fixture(2, 2);
    let root_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建局部 checkpoint", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            spawn_request("checkpoint_tamper"),
        )
        .unwrap();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("原始终态".to_owned()),
            },
        )
        .unwrap();
    fixture
        .coordinator
        .evict_idle_agent(&child.agent.agent_id)
        .unwrap();
    let checkpoint = fixture
        .store
        .recovered
        .lock()
        .expect("恢复锁不应中毒")
        .get(&child.agent.agent_id)
        .expect("驱逐应保存局部 checkpoint")
        .clone();
    (fixture, child.agent.agent_id, checkpoint, root_turn)
}

/// 验证局部 checkpoint 的 revision 或内容摘要被篡改后都拒绝冷加载。
#[test]
fn cold_load_rejects_tampered_checkpoint_revision_and_digest() {
    let (fixture, child_agent_id, original, root_turn) = evicted_child_checkpoint_fixture();
    let mut wrong_revision = original.clone();
    wrong_revision.revision += 1;
    fixture
        .store
        .recovered
        .lock()
        .expect("恢复锁不应中毒")
        .insert(child_agent_id.clone(), wrong_revision);
    assert!(matches!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child_agent_id,
                "revision 篡改后冷加载",
            )
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));

    let mut wrong_digest = original;
    wrong_digest
        .agent
        .last_turn
        .as_mut()
        .expect("已完成子 Agent 应包含最近 Turn")
        .prompt = Some("被篡改的正文".to_owned());
    fixture
        .store
        .recovered
        .lock()
        .expect("恢复锁不应中毒")
        .insert(child_agent_id.clone(), wrong_digest);
    assert!(matches!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &child_agent_id,
                "digest 篡改后冷加载",
            )
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证未知目标返回不存在错误，且已知身份缺少局部 checkpoint 仍要求恢复。
#[test]
fn cold_load_classifies_unknown_target_without_downgrading_recovery_failures() {
    let (fixture, child_agent_id, _checkpoint, root_turn) = evicted_child_checkpoint_fixture();
    let unknown_target = AgentId::new("/root/native_reader_a").unwrap();
    assert!(matches!(
        fixture
            .coordinator
            .send_message(
                &fixture.root_agent_id,
                &root_turn,
                &next_tool_call_id(),
                &unknown_target,
                "未知目标不应被视为存储损坏",
            )
            .unwrap_err(),
        CollaborationError::AgentNotFound { agent_id } if agent_id == unknown_target
    ));

    fixture
        .coordinator
        .send_message(
            &fixture.root_agent_id,
            &root_turn,
            &next_tool_call_id(),
            &child_agent_id,
            "未知目标失败后仍可使用已知目标",
        )
        .expect("未知目标失败不得污染后续的有效冷加载");

    let (missing_fixture, missing_child_agent_id, _checkpoint, missing_root_turn) =
        evicted_child_checkpoint_fixture();
    missing_fixture
        .store
        .recovered
        .lock()
        .expect("恢复锁不应中毒")
        .remove(&missing_child_agent_id);
    assert!(matches!(
        missing_fixture
            .coordinator
            .send_message(
                &missing_fixture.root_agent_id,
                &missing_root_turn,
                &next_tool_call_id(),
                &missing_child_agent_id,
                "已知身份缺少 checkpoint",
            )
            .unwrap_err(),
        CollaborationError::InvalidRecovery { .. }
    ));
}

/// 验证 Closing checkpoint 重启后继续占用容量，并先静止再清理和调度。
#[test]
fn closing_checkpoint_restore_reserves_capacity_until_quiesce_retry() {
    let fixture = fixture(1, 1);
    let closing_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "关闭中的活跃 Turn", NO_PLAN)
        .unwrap();
    fixture.execution.reject_all_quiesces();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(
        checkpoint.roots[0].lifecycle,
        RecoveredRootLifecycle::Closing
    );

    let execution = Arc::new(RecordingExecution::default());
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        fixture.store.clone(),
        execution.clone(),
        Arc::new(SequentialIds {
            next: AtomicU64::new(82_000),
        }),
    );
    restored.restore_coordinator(checkpoint).unwrap();
    assert_eq!(restored.capacity().unwrap().global_in_use, 1);
    assert!(matches!(
        restored.agent_status(&fixture.root_agent_id).unwrap(),
        CollaborationAgentStatus::Cancelling { turn_id } if turn_id == closing_turn
    ));
    let second_root = restored
        .register_root(RootAgentRequest {
            session_id: SessionId::new("closing-restore-second-root-session").unwrap(),
            profile: profile("closing-restore-second-root"),
            per_root_turn_limit: 1,
        })
        .unwrap();
    let second_turn = restored
        .begin_root_turn(&second_root.agent_id, "等待 Closing 根释放容量", NO_PLAN)
        .unwrap();
    assert!(matches!(
        restored.agent_status(&second_root.agent_id).unwrap(),
        CollaborationAgentStatus::WaitingCapacity { turn_id } if turn_id == second_turn
    ));

    assert_eq!(restored.reconcile_outbox().unwrap(), 1);
    assert_eq!(execution.quiesces().len(), 1);
    assert_eq!(execution.closes().len(), 1);
    assert!(matches!(
        restored.agent_status(&second_root.agent_id).unwrap(),
        CollaborationAgentStatus::Running { turn_id } if turn_id == second_turn
    ));
}

/// 验证规范批次编码对相同内容稳定，并覆盖水位、事件字段和负载字段。
#[test]
fn canonical_event_batch_id_is_stable_and_field_sensitive() {
    let event = CollaborationEvent {
        session_id: SessionId::new("batch-session-a").unwrap(),
        turn_id: Some(TurnId::new("batch-turn-a").unwrap()),
        source_agent_id: AgentId::new("batch-source-a").unwrap(),
        agent_id: AgentId::new("batch-agent-a").unwrap(),
        parent_agent_id: Some(AgentId::new("batch-parent-a").unwrap()),
        agent_path: AgentPath::root().child("batch_child_a").unwrap(),
        parent_turn_id: Some(TurnId::new("batch-parent-turn-a").unwrap()),
        root_turn_id: Some(TurnId::new("batch-root-turn-a").unwrap()),
        sequence: 7,
        kind: CollaborationEventKind::AgentTurnFailed {
            message: "batch-failure-a".to_owned(),
        },
    };
    let stable = collaboration_event_batch(6, std::slice::from_ref(&event));
    assert_eq!(
        stable.batch_id,
        collaboration_event_batch(6, std::slice::from_ref(&event)).batch_id
    );
    assert_ne!(
        stable.batch_id,
        collaboration_event_batch(5, std::slice::from_ref(&event)).batch_id
    );

    let mut variants = Vec::new();
    let mut changed = event.clone();
    changed.session_id = SessionId::new("batch-session-b").unwrap();
    variants.push(changed);
    let mut changed = event.clone();
    changed.turn_id = Some(TurnId::new("batch-turn-b").unwrap());
    variants.push(changed);
    let mut changed = event.clone();
    changed.source_agent_id = AgentId::new("batch-source-b").unwrap();
    variants.push(changed);
    let mut changed = event.clone();
    changed.agent_id = AgentId::new("batch-agent-b").unwrap();
    variants.push(changed);
    let mut changed = event.clone();
    changed.parent_agent_id = None;
    variants.push(changed);
    let mut changed = event.clone();
    changed.agent_path = AgentPath::root().child("batch_child_b").unwrap();
    variants.push(changed);
    let mut changed = event.clone();
    changed.parent_turn_id = Some(TurnId::new("batch-parent-turn-b").unwrap());
    variants.push(changed);
    let mut changed = event.clone();
    changed.root_turn_id = Some(TurnId::new("batch-root-turn-b").unwrap());
    variants.push(changed);
    let mut changed = event.clone();
    changed.sequence = 8;
    variants.push(changed);
    let mut changed = event.clone();
    changed.kind = CollaborationEventKind::AgentTurnFailed {
        message: "batch-failure-b".to_owned(),
    };
    variants.push(changed);
    for changed in variants {
        assert_ne!(
            stable.batch_id,
            collaboration_event_batch(6, std::slice::from_ref(&changed)).batch_id
        );
    }
    assert_ne!(
        stable.batch_id,
        collaboration_event_batch(6, &[event.clone(), event]).batch_id
    );
}

/// 验证旧 Closing 快照不能越过已前进的 Store 水位执行静止副作用。
#[test]
fn stale_closing_restore_freezes_before_quiesce_side_effect() {
    let fixture = fixture(1, 1);
    fixture.execution.reject_all_quiesces();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(
        checkpoint.roots[0].lifecycle,
        RecoveredRootLifecycle::Closing
    );
    fixture.store.advance_sequence();

    let execution = Arc::new(RecordingExecution::default());
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        fixture.store,
        execution.clone(),
        Arc::new(SequentialIds {
            next: AtomicU64::new(90_000),
        }),
    );
    assert!(matches!(
        restored.restore_coordinator(checkpoint).unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(execution.quiesces().is_empty());
    assert!(execution.closes().is_empty());
}

/// 验证旧 CleanupPending 快照不能越过已前进的 Store 水位执行清理副作用。
#[test]
fn stale_cleanup_restore_freezes_before_close_side_effect() {
    let fixture = fixture(1, 1);
    fixture.execution.fail_next_close();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    assert_eq!(
        checkpoint.roots[0].lifecycle,
        RecoveredRootLifecycle::CleanupPending
    );
    fixture.store.advance_sequence();

    let execution = Arc::new(RecordingExecution::default());
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(1).unwrap(),
        fixture.store,
        execution.clone(),
        Arc::new(SequentialIds {
            next: AtomicU64::new(91_000),
        }),
    );
    assert!(matches!(
        restored.restore_coordinator(checkpoint).unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(execution.quiesces().is_empty());
    assert!(execution.closes().is_empty());
}

/// 验证历史批次返回高于批末的实时水位时协调器立即冻结。
#[test]
fn already_committed_with_advanced_watermark_freezes_coordinator() {
    let fixture = fixture(1, 1);
    fixture.store.already_committed_ahead();
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "触发历史批次水位冲突", NO_PLAN)
            .unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert!(matches!(
        fixture.coordinator.capacity().unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
}

/// 验证关闭中的 Cancelling Turn 拒绝 steer，且无未决 steer 的快照仍可恢复。
#[test]
fn closing_tree_rejects_steer_and_remains_restorable() {
    let fixture = fixture(1, 1);
    let turn_id = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "即将关闭的根 Turn", NO_PLAN)
        .unwrap();
    fixture.execution.reject_all_quiesces();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert!(matches!(
        fixture
            .coordinator
            .steer_agent(&fixture.root_agent_id, &turn_id, "关闭后不应接受")
            .unwrap_err(),
        CollaborationError::TreeClosed { .. }
    ));
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();

    let restored = restore_coordinator(fixture.store, 1, 92_000);
    restored.restore_coordinator(checkpoint).unwrap();
}

/// 验证开放根树中已经 Cancelling 的子 Turn 不再接受用户 steer。
#[test]
fn cancelling_agent_in_open_tree_rejects_steer() {
    let fixture = fixture(2, 2);
    let parent_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "创建后取消子 Agent", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &parent_turn,
            &next_tool_call_id(),
            spawn_request("cancelled_steer"),
        )
        .unwrap();
    let first_stop_call_id = fixed_tool_call_id("enter-cancelling");
    fixture
        .coordinator
        .stop_agent(
            &fixture.root_agent_id,
            &parent_turn,
            &first_stop_call_id,
            &child.agent.agent_id,
        )
        .unwrap();
    let already_cancelling_call_id = fixed_tool_call_id("already-cancelling-stop");
    let events_before_already_cancelling = fixture.store.events().len();
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &parent_turn,
                &already_cancelling_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        child.initial_turn_id
    );
    let events_after_already_cancelling = fixture.store.events().len();
    assert_eq!(
        events_after_already_cancelling,
        events_before_already_cancelling + 1,
        "已 Cancelling 目标的新 StopAgent 只应追加首次 receipt"
    );
    assert_eq!(
        fixture
            .coordinator
            .stop_agent(
                &fixture.root_agent_id,
                &parent_turn,
                &already_cancelling_call_id,
                &child.agent.agent_id,
            )
            .unwrap(),
        child.initial_turn_id,
        "已 Cancelling 目标的 StopAgent 重放必须返回首次 Turn"
    );
    assert_eq!(
        fixture.store.events().len(),
        events_after_already_cancelling,
        "已 Cancelling 目标的 StopAgent 重放不得追加事件"
    );
    let receipt_batches = fixture
        .store
        .batches()
        .into_iter()
        .filter(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    &event.kind,
                    CollaborationEventKind::CollaborationInvocationCommitted { receipt }
                        if receipt.key.tool_call_id == already_cancelling_call_id
                            && receipt.kind == CollaborationInvocationKind::StopAgent
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(receipt_batches.len(), 1);
    assert_eq!(
        receipt_batches[0].events.len(),
        1,
        "已 Cancelling 目标不应重复写状态事件，只需原子保存首次 receipt"
    );
    assert!(matches!(
        fixture
            .coordinator
            .steer_agent(
                &child.agent.agent_id,
                &child.initial_turn_id,
                "取消中不应接受",
            )
            .unwrap_err(),
        CollaborationError::TargetNotRunning { .. }
    ));
}

/// 验证 outbox 重试会在任何外部副作用前复核 Store 水位。
#[test]
fn reconcile_outbox_freezes_before_side_effect_when_store_advanced() {
    let fixture = fixture(1, 1);
    fixture.execution.reject_all_quiesces();
    assert!(matches!(
        fixture
            .coordinator
            .close_root_session(&fixture.root_agent_id)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert_eq!(fixture.execution.quiesce_attempts(), 1);
    fixture.store.advance_sequence();
    assert!(matches!(
        fixture.coordinator.reconcile_outbox().unwrap_err(),
        CollaborationError::StoreRecoveryRequired { .. }
    ));
    assert_eq!(fixture.execution.quiesce_attempts(), 1);
}

/// 验证子 Agent 完成会为仍在运行的父 Turn 保留并重试 mailbox 信号。
#[test]
fn child_completion_retries_parent_mailbox_signal() {
    let fixture = fixture(2, 2);
    let parent_turn = fixture
        .coordinator
        .begin_root_turn(&fixture.root_agent_id, "等待子 Agent 报告", NO_PLAN)
        .unwrap();
    let child = fixture
        .coordinator
        .spawn_agent(
            &fixture.root_agent_id,
            &parent_turn,
            &next_tool_call_id(),
            spawn_request("completion_signal"),
        )
        .unwrap();
    fixture.execution.fail_next_signal();
    fixture
        .coordinator
        .complete_turn(
            &child.agent.agent_id,
            &child.initial_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("子任务已完成".to_owned()),
            },
        )
        .unwrap();
    let checkpoint = fixture
        .coordinator
        .checkpoint_root(&fixture.root_agent_id)
        .unwrap();
    let parent = checkpoint
        .agents
        .iter()
        .find(|agent| agent.definition.agent_id == fixture.root_agent_id)
        .unwrap();
    assert!(parent.mailbox.iter().any(|entry| matches!(
        entry.message.kind,
        crate::MailboxMessageKind::ChildTurnFinished { .. }
    )));
    assert!(fixture.execution.signals().is_empty());

    assert_eq!(fixture.coordinator.reconcile_outbox().unwrap(), 1);
    let signals = fixture.execution.signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].agent_id, fixture.root_agent_id);
    assert_eq!(signals[0].turn_id, parent_turn);
    assert_eq!(signals[0].kind, AgentTurnSignalKind::MailboxAvailable);
    assert!(signals[0].activity_version > 0);
}

/// 验证多字节端口错误按稳定 UTF-8 边界限长并附带截断标记。
#[test]
fn port_error_is_utf8_bounded() {
    let error = CollaborationPortError::new("厂商错误".repeat(MAX_PORT_ERROR_BYTES));
    assert!(error.message().len() <= MAX_PORT_ERROR_BYTES);
    assert!(error.message().ends_with("[端口错误已截断]"));
    assert!(error.message().is_char_boundary(error.message().len()));
}

/// 验证超长 StartTurn 永久拒绝仍会释放容量并生成可恢复 checkpoint。
#[test]
fn oversized_start_error_is_compensated_and_restorable() {
    let fixture = fixture(1, 1);
    fixture
        .execution
        .fail_next_start_with("错".repeat(1_500_000));
    assert!(matches!(
        fixture
            .coordinator
            .begin_root_turn(&fixture.root_agent_id, "触发超长厂商错误", NO_PLAN)
            .unwrap_err(),
        CollaborationError::CommittedExecutionPending { .. }
    ));
    assert_eq!(fixture.coordinator.capacity().unwrap().global_in_use, 0);
    let checkpoint = fixture.coordinator.checkpoint_coordinator().unwrap();
    let failed_message = match &checkpoint.roots[0].agents[0].status {
        CollaborationAgentStatus::Failed { message, .. } => message,
        status => panic!("预期失败终态，实际为 {status:?}"),
    };
    assert!(failed_message.len() < MAX_PORT_ERROR_BYTES + 1_024);

    let restored = restore_coordinator(fixture.store, 1, 93_000);
    restored.restore_coordinator(checkpoint).unwrap();
}

/// 验证 Worktree lease 拒绝路径、非法字符和超长标识。
#[test]
fn worktree_lease_rejects_invalid_or_oversized_ids() {
    assert!(WorktreeLease::new("").is_err());
    assert!(WorktreeLease::new("D:/worktrees/unsafe").is_err());
    assert!(WorktreeLease::new("含中文").is_err());
    assert!(WorktreeLease::new("a".repeat(129)).is_err());
    let lease = WorktreeLease::new("managed-worktree_01").unwrap();
    assert_eq!(lease.as_str(), "managed-worktree_01");
    assert_eq!(lease.to_string(), "managed-worktree_01");
}
