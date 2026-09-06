//! 验证 Runtime Journal 与 Collaboration v2 冷启动恢复之间的权威边界。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keencode_agent::{
    AgentExecutionPort, AgentId, AgentProfile, AgentRunner, AgentTreeQuiesceResult,
    AgentTurnLaunch, AgentTurnOutcome, AgentTurnSignal, AgentTurnStartResult, CloseAgentTree,
    CollaborationAgentStatus, CollaborationAppendResult, CollaborationCoordinator,
    CollaborationEvent, CollaborationEventBatchId, CollaborationIdGenerator, CollaborationLimits,
    CollaborationPortError, CollaborationStore, CollaborationTransitionCommit, ContextInheritance,
    PlanGuard, QuiesceAgentTree, RecoveredAgentCheckpoint, RecoveredCoordinator, RootAgentRequest,
    SpawnAgentRequest, ToolCallId, TurnId as AgentTurnId,
};
use keencode_model::{
    Message, MessageRole, ModelError, ModelFuture, ModelProvider, ModelRequest, ModelStream,
    ModelStreamEvent, ProviderCapabilities, ResponseMetadata, ScriptedProvider, ScriptedReply,
    StopReason,
};
use keencode_resources::{
    AgentId as ResourceAgentId, SubAgentState, SubAgentStatus, TurnId as ResourceTurnId, TurnStatus,
};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeSession, RuntimeTurnRequest,
};
use tempfile::TempDir;
use tokio::sync::Notify;

/// 内存协作 Store 保存的稳定批次记录。
#[derive(Clone)]
struct RecordedCollaborationBatch {
    /// 与批次绑定的期望事件水位。
    expected_sequence: u64,
    /// 批次中按序追加的完整事件。
    events: Vec<CollaborationEvent>,
    /// 与批次原子保存的协调器 checkpoint。
    checkpoint: RecoveredCoordinator,
}

/// 内存协作 Store 的共享可变状态。
#[derive(Default)]
struct CollaborationMemoryState {
    /// 当前已提交的最后事件序号。
    sequence: u64,
    /// 最近一次与事件批次原子保存的完整协调器 checkpoint。
    checkpoint: Option<RecoveredCoordinator>,
    /// 按幂等批次标识保存的已提交批次。
    batches: HashMap<CollaborationEventBatchId, RecordedCollaborationBatch>,
    /// 按提交顺序保留的协作事件，供 exactly-once 断言使用。
    events: Vec<CollaborationEvent>,
    /// 局部 Agent checkpoint，当前测试不主动驱逐 Agent。
    agent_checkpoints: HashMap<AgentId, RecoveredAgentCheckpoint>,
}

/// 只实现本测试所需边界的持久协作 Store。
#[derive(Default)]
struct MemoryCollaborationStore {
    /// 将协作水位、批次和 checkpoint 置于同一内存提交临界区。
    state: Mutex<CollaborationMemoryState>,
}

impl MemoryCollaborationStore {
    /// 返回按提交顺序保存的协作事件快照。
    fn events(&self) -> Vec<CollaborationEvent> {
        self.state
            .lock()
            .expect("内存协作 Store 锁不应中毒")
            .events
            .clone()
    }
}

impl CollaborationStore for MemoryCollaborationStore {
    /// 返回当前协作事件水位。
    fn current_sequence(&self) -> Result<u64, CollaborationPortError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CollaborationPortError::new("内存协作 Store 锁已中毒"))?
            .sequence)
    }

    /// 返回最近一次原子保存的协调器 checkpoint。
    fn load_coordinator_checkpoint(
        &self,
    ) -> Result<Option<RecoveredCoordinator>, CollaborationPortError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CollaborationPortError::new("内存协作 Store 锁已中毒"))?
            .checkpoint
            .clone())
    }

    /// 校验并幂等提交一个协作事件批次及其 checkpoint。
    fn commit_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        if commit.validate().is_err() {
            return CollaborationAppendResult::Indeterminate {
                error: CollaborationPortError::new("测试协作批次校验失败"),
            };
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return CollaborationAppendResult::Indeterminate {
                    error: CollaborationPortError::new("内存协作 Store 锁已中毒"),
                };
            }
        };
        let batch = &commit.batch;
        if let Some(previous) = state.batches.get(&batch.batch_id) {
            return if previous.expected_sequence == batch.expected_sequence
                && previous.events == batch.events
                && previous.checkpoint == commit.checkpoint
            {
                CollaborationAppendResult::AlreadyCommitted {
                    current_sequence: state.sequence,
                }
            } else {
                CollaborationAppendResult::Conflict {
                    actual_sequence: state.sequence,
                }
            };
        }
        if state.sequence != batch.expected_sequence {
            return CollaborationAppendResult::Conflict {
                actual_sequence: state.sequence,
            };
        }
        let Some(last_event) = batch.events.last() else {
            return CollaborationAppendResult::Indeterminate {
                error: CollaborationPortError::new("测试协作批次不能为空"),
            };
        };
        state.sequence = last_event.sequence;
        state.events.extend(batch.events.iter().cloned());
        state.batches.insert(
            batch.batch_id.clone(),
            RecordedCollaborationBatch {
                expected_sequence: batch.expected_sequence,
                events: batch.events.clone(),
                checkpoint: commit.checkpoint.clone(),
            },
        );
        state.checkpoint = Some(commit.checkpoint.clone());
        CollaborationAppendResult::Appended
    }

    /// 当前测试没有局部驱逐 checkpoint，因此始终返回空。
    fn load_agent_checkpoint(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<RecoveredAgentCheckpoint>, CollaborationPortError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CollaborationPortError::new("内存协作 Store 锁已中毒"))?
            .agent_checkpoints
            .get(agent_id)
            .cloned())
    }

    /// 保存局部 Agent checkpoint，满足协调器端口的持久化契约。
    fn save_agent_checkpoint(
        &self,
        checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<(), CollaborationPortError> {
        self.state
            .lock()
            .map_err(|_| CollaborationPortError::new("内存协作 Store 锁已中毒"))?
            .agent_checkpoints
            .insert(
                checkpoint.agent.definition.agent_id.clone(),
                checkpoint.clone(),
            );
        Ok(())
    }
}

/// 立即接受 Turn 并记录启动次数的协作执行端口。
#[derive(Default)]
struct AcceptingExecution {
    /// 已经交给执行端口的 Turn 启动请求。
    launches: Mutex<Vec<AgentTurnLaunch>>,
    /// 已经按 TurnId 接受的启动集合。
    accepted: Mutex<HashSet<AgentTurnId>>,
}

impl AgentExecutionPort for AcceptingExecution {
    /// 幂等接受一个已经取得容量的 Turn。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
        let turn_id = launch.turn_id.clone();
        let mut accepted = self.accepted.lock().expect("执行端口锁不应中毒");
        if !accepted.insert(turn_id) {
            return AgentTurnStartResult::AlreadyAccepted;
        }
        self.launches
            .lock()
            .expect("执行端口锁不应中毒")
            .push(launch);
        AgentTurnStartResult::Accepted
    }

    /// 测试执行端口不需要额外处理安全边界信号。
    fn signal_turn(&self, _signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
        Ok(())
    }

    /// 测试执行端口立即确认整树静止。
    fn quiesce_tree(&self, _request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
        AgentTreeQuiesceResult::Quiesced
    }

    /// 测试执行端口立即确认整树清理。
    fn close_tree(&self, _request: CloseAgentTree) -> Result<(), CollaborationPortError> {
        Ok(())
    }
}

/// 为协作 ID 提供稳定且不重复的测试实现。
#[derive(Default)]
struct SequentialIds {
    /// 下一个测试 ID 使用的单调序号。
    next: AtomicU64,
}

impl CollaborationIdGenerator for SequentialIds {
    /// 分配一个新的 Agent 标识。
    fn next_agent_id(&self) -> AgentId {
        AgentId::new(format!(
            "agent-{}",
            self.next.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .expect("测试 Agent 标识应有效")
    }

    /// 分配一个新的 Agent Session 标识。
    fn next_session_id(&self) -> keencode_agent::SessionId {
        keencode_agent::SessionId::new(format!(
            "session-{}",
            self.next.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .expect("测试 Agent Session 标识应有效")
    }

    /// 分配一个新的 mailbox 消息标识。
    fn next_message_id(&self) -> keencode_agent::MailboxMessageId {
        keencode_agent::MailboxMessageId::new(format!(
            "message-{}",
            self.next.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .expect("测试 mailbox 标识应有效")
    }
}

/// 在放行前阻塞模型请求的 Provider，用来让根 Turn 保持真实 Running 状态。
struct GatedProvider {
    /// 释放模型请求的异步闸门。
    gate: Arc<Notify>,
    /// 闸门打开后返回固定响应的脚本 Provider。
    inner: ScriptedProvider,
}

impl GatedProvider {
    /// 创建一个等待闸门后返回指定脚本的 Provider。
    fn new(gate: Arc<Notify>, reply: ScriptedReply) -> Self {
        Self {
            gate,
            inner: ScriptedProvider::new(ProviderCapabilities::default(), [reply]),
        }
    }
}

impl ModelProvider for GatedProvider {
    /// 返回脚本 Provider 的固定能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.inner.capabilities(model)
    }

    /// 等待测试闸门打开后再交给脚本 Provider 产生模型流。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            gate.notified().await;
            self.inner.stream(request).await
        })
    }
}

/// 创建使用默认容量限制的隔离 Runtime Session。
fn create_session(root: &TempDir, session_id: &str) -> RuntimeSession {
    RuntimeSession::create_session(
        RuntimeConfig::new(root.path()),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "Runtime 协作恢复测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("Runtime Session 应创建成功")
}

/// 创建一个同时满足 Runtime 与 Collaboration 校验的 Agent Profile。
fn profile(cwd: &Path) -> AgentProfile {
    AgentProfile {
        model: "test-model".to_owned(),
        reasoning_effort: None,
        plan_guard: PlanGuard::inactive(),
        cwd: cwd.to_path_buf(),
        worktree_lease: None,
        tool_snapshot: Vec::new(),
    }
}

/// 创建与当前 Runtime Session 绑定的根 Agent Turn 请求。
fn root_runtime_turn(session: &RuntimeSession, turn_id: &str, prompt: &str) -> RuntimeTurnRequest {
    let input = Message::text(MessageRole::User, prompt);
    RuntimeTurnRequest::root(
        keencode_agent::TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            AgentTurnId::new(turn_id).expect("Agent Turn 标识应有效"),
            AgentId::new("root").expect("根 Agent 标识应有效"),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        ),
        vec![input],
        prompt,
    )
}

/// 创建首次子 Agent Turn 的 Runtime 请求，并把 Pending 身份绑定到起点批次。
fn initial_child_runtime_turn(
    session: &RuntimeSession,
    turn_id: &AgentTurnId,
    child_agent_id: &AgentId,
    root_turn_id: &AgentTurnId,
    prompt: &str,
    spawned_agent: SubAgentState,
) -> RuntimeTurnRequest {
    let input = Message::text(MessageRole::User, prompt);
    RuntimeTurnRequest::initial_child(
        keencode_agent::TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            turn_id.clone(),
            child_agent_id.clone(),
            "test-model",
            vec![input.clone()],
            PlanGuard::inactive(),
        ),
        vec![input],
        root_turn_id.as_str(),
        root_turn_id.as_str(),
        prompt,
        spawned_agent,
    )
}

/// 创建一个会让 Agent Loop 失败的脚本响应。
fn failure_reply(message: &str) -> ScriptedReply {
    ScriptedReply::new(vec![Err(ModelError::Protocol {
        message: message.to_owned(),
    })])
}

/// 创建一个正常完成的脚本响应。
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

/// 等待 Runtime Journal 观察到指定 Turn 已进入 Running。
async fn wait_until_runtime_running(session: &RuntimeSession, turn_id: &ResourceTurnId) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let running = session
                .snapshot()
                .expect("Runtime 快照应可读取")
                .state
                .turns
                .get(turn_id)
                .is_some_and(|turn| turn.status == TurnStatus::Running);
            if running {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Runtime Turn 应在测试超时前进入 Running");
}

/// 将 Runtime Journal 的终态转换为 Collaboration 可消费的权威终态。
fn authoritative_outcomes(session: &RuntimeSession) -> HashMap<AgentTurnId, AgentTurnOutcome> {
    session
        .snapshot()
        .expect("Runtime 快照应可读取")
        .state
        .turns
        .values()
        .filter_map(|turn| {
            let outcome = match turn.status {
                TurnStatus::Completed => AgentTurnOutcome::Completed {
                    final_message: None,
                },
                TurnStatus::Failed => AgentTurnOutcome::Failed {
                    message: turn
                        .outcome_message
                        .clone()
                        .expect("失败 Turn 应包含结果说明"),
                },
                TurnStatus::Cancelled => AgentTurnOutcome::Interrupted,
                TurnStatus::Running => return None,
            };
            Some((
                AgentTurnId::new(turn.turn_id.as_str()).expect("Runtime Turn 标识应有效"),
                outcome,
            ))
        })
        .collect()
}

/// 从 Runtime Session 中读取健康重启后的句柄。
fn reopen_session(root: &TempDir, session_id: &str) -> RuntimeSession {
    match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("Runtime Session 应可重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("测试 Session 不应损坏：{report:?}")
        }
    }
}

/// 用一份旧 checkpoint 创建恢复协调器。
fn new_coordinator(
    store: Arc<MemoryCollaborationStore>,
    execution: Arc<AcceptingExecution>,
) -> CollaborationCoordinator {
    CollaborationCoordinator::new(
        CollaborationLimits::new(4).expect("测试协作容量应有效"),
        store,
        execution,
        Arc::new(SequentialIds::default()),
    )
}

/// Runtime Journal 的真实失败终态必须覆盖旧 Collaboration checkpoint 的 Running 状态。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_failure_wins_over_running_collaboration_checkpoint() {
    let root = TempDir::new().expect("应创建测试目录");
    let session_id = "runtime-collab-root-failure";
    let session = create_session(&root, session_id);
    let store = Arc::new(MemoryCollaborationStore::default());
    let execution = Arc::new(AcceptingExecution::default());
    let coordinator = new_coordinator(store.clone(), execution);
    let root_agent_id = AgentId::new("root").expect("根 Agent 标识应有效");
    coordinator
        .register_root_with_id(
            root_agent_id.clone(),
            RootAgentRequest {
                session_id: keencode_agent::SessionId::new(session_id)
                    .expect("协作 Session 标识应有效"),
                profile: profile(root.path()),
                per_root_turn_limit: 2,
            },
        )
        .expect("根 Agent 应注册");
    let turn_id = AgentTurnId::new("runtime-root-failure-turn").expect("Turn 标识应有效");
    coordinator
        .begin_root_turn_with_id(
            &root_agent_id,
            turn_id.clone(),
            "让 Runtime 形成失败终态",
            PlanGuard::inactive(),
        )
        .expect("协作根 Turn 应启动");
    let checkpoint = coordinator
        .checkpoint_coordinator()
        .expect("旧协作 checkpoint 应可读取");
    assert!(matches!(
        coordinator.agent_status(&root_agent_id).expect("状态应可读取"),
        CollaborationAgentStatus::Running { turn_id: running } if running == turn_id
    ));
    assert_eq!(
        coordinator.capacity().expect("容量应可读取").global_in_use,
        1
    );

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [failure_reply("真实模型响应协议错误")],
    ));
    let runner = session.bind_agent_runner(AgentRunner::new(
        provider,
        keencode_agent::ToolRegistry::new(),
        keencode_agent::RunLimits::new(4, 4).expect("运行限制应有效"),
    ));
    let result = runner
        .run_turn(root_runtime_turn(
            &session,
            turn_id.as_str(),
            "让 Runtime 形成失败终态",
        ))
        .await
        .expect("Runtime 终态提交应成功");
    assert!(!result.is_success());
    assert_eq!(
        result.state.terminal_reason(),
        Some(keencode_agent::TerminalReason::Failed)
    );
    let failed_message = session
        .snapshot()
        .expect("Runtime 快照应可读取")
        .state
        .turns
        .get(&ResourceTurnId::new(turn_id.as_str()).expect("资源 Turn 标识应有效"))
        .expect("Runtime Turn 应存在")
        .outcome_message
        .clone()
        .expect("失败 Turn 应保存安全说明");
    assert!(!failed_message.trim().is_empty());
    drop(runner);
    drop(session);

    let reopened = reopen_session(&root, session_id);
    let reopened_snapshot = reopened.snapshot().expect("重启后的 Runtime 快照应可读取");
    let reopened_turn = reopened_snapshot
        .state
        .turns
        .get(&ResourceTurnId::new(turn_id.as_str()).expect("资源 Turn 标识应有效"))
        .expect("重启后的 Runtime Turn 应存在");
    assert_eq!(reopened_turn.status, TurnStatus::Failed);
    let outcomes = authoritative_outcomes(&reopened);
    assert!(matches!(
        outcomes.get(&turn_id),
        Some(AgentTurnOutcome::Failed { message }) if message == &failed_message
    ));
    drop(reopened);

    let restored = new_coordinator(store.clone(), Arc::new(AcceptingExecution::default()));
    restored
        .restore_coordinator_with_authoritative_outcomes(checkpoint, &outcomes)
        .expect("Runtime 权威失败应覆盖旧 Running checkpoint");
    assert!(matches!(
        restored.agent_status(&root_agent_id).expect("恢复状态应可读取"),
        CollaborationAgentStatus::Failed { turn_id: restored_turn, message }
            if restored_turn == turn_id && message == failed_message
    ));
    assert_eq!(
        restored.capacity().expect("恢复容量应可读取").global_in_use,
        0
    );

    let events_after_first_restore = store.events().len();
    let restored_checkpoint = restored
        .checkpoint_coordinator()
        .expect("恢复后的 checkpoint 应可读取");
    let second = new_coordinator(store.clone(), Arc::new(AcceptingExecution::default()));
    second
        .restore_coordinator_with_authoritative_outcomes(restored_checkpoint, &HashMap::new())
        .expect("已收敛 checkpoint 应可再次冷恢复");
    assert_eq!(store.events().len(), events_after_first_restore);
    assert_eq!(
        second
            .capacity()
            .expect("二次恢复容量应可读取")
            .global_in_use,
        0
    );
}

/// 子 Agent 真实失败后，冷恢复应一次性释放容量并给父 Agent 生成完成通知。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_runtime_failure_reaches_parent_mailbox_exactly_once_after_restore() {
    let root = TempDir::new().expect("应创建测试目录");
    let session_id = "runtime-collab-child-failure";
    let session = create_session(&root, session_id);
    let store = Arc::new(MemoryCollaborationStore::default());
    let execution = Arc::new(AcceptingExecution::default());
    let coordinator = new_coordinator(store.clone(), execution);
    let root_agent_id = AgentId::new("root").expect("根 Agent 标识应有效");
    coordinator
        .register_root_with_id(
            root_agent_id.clone(),
            RootAgentRequest {
                session_id: keencode_agent::SessionId::new(session_id)
                    .expect("协作 Session 标识应有效"),
                profile: profile(root.path()),
                per_root_turn_limit: 2,
            },
        )
        .expect("根 Agent 应注册");
    let root_turn_id = AgentTurnId::new("runtime-child-parent-turn").expect("Turn 标识应有效");
    coordinator
        .begin_root_turn_with_id(
            &root_agent_id,
            root_turn_id.clone(),
            "保持父 Turn 运行",
            PlanGuard::inactive(),
        )
        .expect("父协作 Turn 应启动");

    let gate = Arc::new(Notify::new());
    let parent_provider = Arc::new(GatedProvider::new(
        gate.clone(),
        completed_reply("父任务完成"),
    ));
    let parent_runner = session.bind_agent_runner(AgentRunner::new(
        parent_provider,
        keencode_agent::ToolRegistry::new(),
        keencode_agent::RunLimits::new(4, 4).expect("运行限制应有效"),
    ));
    let parent_session = session.clone();
    let parent_turn_for_task = root_turn_id.clone();
    let parent_task = tokio::spawn(async move {
        parent_runner
            .run_turn(root_runtime_turn(
                &parent_session,
                parent_turn_for_task.as_str(),
                "保持父 Turn 运行",
            ))
            .await
    });
    wait_until_runtime_running(
        &session,
        &ResourceTurnId::new(root_turn_id.as_str()).expect("资源 Turn 标识应有效"),
    )
    .await;

    let child_request = SpawnAgentRequest {
        task_name: "worker".to_owned(),
        initial_task: "执行一个会失败的子任务".to_owned(),
        context_inheritance: ContextInheritance::None,
        context_snapshot: Vec::new(),
        agent_template: None,
        profile: profile(root.path()),
    };
    let child_tool_call_id = ToolCallId::new("spawn-worker").expect("工具调用标识应有效");
    let spawned = coordinator
        .spawn_agent(
            &root_agent_id,
            &root_turn_id,
            &child_tool_call_id,
            child_request.clone(),
        )
        .expect("子 Agent 应创建");
    let child_agent_id = spawned.agent.agent_id.clone();
    let child_turn_id = spawned.initial_turn_id.clone();
    let pending_child = SubAgentState {
        agent_id: ResourceAgentId::new(child_agent_id.as_str()).expect("资源 Agent 标识应有效"),
        parent_agent_id: ResourceAgentId::new(root_agent_id.as_str())
            .expect("资源 Agent 标识应有效"),
        agent_path: spawned.agent.path.as_str().to_owned(),
        task: child_request.initial_task.clone(),
        status: SubAgentStatus::Pending,
        current_turn_id: None,
        result_summary: None,
    };
    let child_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [failure_reply("子 Agent 的模型响应失败")],
    ));
    let child_runner = session.bind_agent_runner(AgentRunner::new(
        child_provider,
        keencode_agent::ToolRegistry::new(),
        keencode_agent::RunLimits::new(4, 4).expect("运行限制应有效"),
    ));
    let child_result = child_runner
        .run_turn(initial_child_runtime_turn(
            &session,
            &child_turn_id,
            &child_agent_id,
            &root_turn_id,
            &child_request.initial_task,
            pending_child,
        ))
        .await
        .expect("子 Agent Runtime 终态提交应成功");
    assert!(!child_result.is_success());
    drop(child_runner);

    gate.notify_one();
    let parent_result = parent_task
        .await
        .expect("父 Agent 任务不应 panic")
        .expect("父 Agent Runtime 终态提交应成功");
    assert!(parent_result.is_success());
    drop(session);

    let reopened = reopen_session(&root, session_id);
    let runtime_snapshot = reopened.snapshot().expect("重启后的 Runtime 快照应可读取");
    assert_eq!(
        runtime_snapshot
            .state
            .sub_agents
            .get(&ResourceAgentId::new(child_agent_id.as_str()).expect("资源 Agent 标识应有效"))
            .expect("子 Agent 状态应存在")
            .status,
        SubAgentStatus::Failed
    );
    let outcomes = authoritative_outcomes(&reopened);
    assert!(matches!(
        outcomes.get(&child_turn_id),
        Some(AgentTurnOutcome::Failed { .. })
    ));
    drop(reopened);

    let checkpoint = coordinator
        .checkpoint_coordinator()
        .expect("旧协作 checkpoint 应可读取");
    let restored = new_coordinator(store.clone(), Arc::new(AcceptingExecution::default()));
    restored
        .restore_coordinator_with_authoritative_outcomes(checkpoint, &outcomes)
        .expect("子 Agent Runtime 失败应可覆盖旧 Running checkpoint");
    assert_eq!(
        restored.capacity().expect("恢复容量应可读取").global_in_use,
        0
    );
    assert!(matches!(
        restored.agent_status(&child_agent_id).expect("子 Agent 状态应可读取"),
        CollaborationAgentStatus::Failed { turn_id, .. } if turn_id == child_turn_id
    ));
    let completion_messages = restored
        .mailbox(&root_agent_id)
        .expect("父 Agent mailbox 应可读取")
        .into_iter()
        .filter(|message| message.source_agent_id == child_agent_id)
        .collect::<Vec<_>>();
    assert_eq!(completion_messages.len(), 1);
    assert!(
        completion_messages[0]
            .content
            .contains("子 Agent /root/worker 已失败")
    );

    let events_after_first_restore = store.events().len();
    let restored_checkpoint = restored
        .checkpoint_coordinator()
        .expect("恢复后的 checkpoint 应可读取");
    let second = new_coordinator(store.clone(), Arc::new(AcceptingExecution::default()));
    second
        .restore_coordinator_with_authoritative_outcomes(restored_checkpoint, &HashMap::new())
        .expect("带完成通知的 checkpoint 应可再次恢复");
    assert_eq!(store.events().len(), events_after_first_restore);
    assert_eq!(
        second
            .mailbox(&root_agent_id)
            .expect("二次恢复父 mailbox 应可读取")
            .into_iter()
            .filter(|message| message.source_agent_id == child_agent_id)
            .count(),
        1
    );
}
