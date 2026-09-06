//! 单层异步 Agent 协作工具回归测试。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keencode_agent::{
    AgentCapabilities, AgentExecutionPort, AgentId, AgentProfile, AgentTemplateSnapshot,
    AgentTool as RuntimeAgentTool, AgentTreeQuiesceResult, AgentTurnLaunch, AgentTurnOutcome,
    AgentTurnSignal, AgentTurnStartResult, CloseAgentTree, CollaborationAgentStatus,
    CollaborationAppendResult, CollaborationCoordinator, CollaborationEventBatchId,
    CollaborationIdGenerator, CollaborationLimits, CollaborationPortError, CollaborationStore,
    CollaborationTransitionCommit, MailboxMessageId, PlanGuard, QuiesceAgentTree,
    RecoveredAgentCheckpoint, RecoveredCoordinator, RootAgentRequest, SessionId, ToolCallId,
    ToolContext, ToolError, ToolRegistry, TurnCancellation, TurnId,
};
use keencode_model::{Message, MessageRole, ToolResultContent};
use serde_json::{Value, json};

use super::collaboration_tools::{
    MAX_AGENT_ID_BYTES, MAX_INITIAL_TASK_BYTES, MAX_MESSAGE_BYTES, MAX_WAIT_TIMEOUT_MILLISECONDS,
};
use super::{
    CompletedTurnContext, FollowupTaskTool, InterruptAgentTool, ListAgentsTool,
    ResolvedSpawnAgentTemplate, RetryAgentTool, SendMessageTool, SpawnAgentContextSource,
    SpawnAgentTemplateContext, SpawnAgentTemplateResolver, SpawnAgentTool, WaitAgentTool,
    register_collaboration_tools,
};

/// 测试中按固定结果解析显式 Agent 模板。
struct StaticTemplateResolver {
    /// 成功时返回的模板；为空时模拟未知名称。
    template: Option<ResolvedSpawnAgentTemplate>,
    /// 是否模拟候选解析失败。
    fail: bool,
}

impl SpawnAgentTemplateResolver for StaticTemplateResolver {
    /// 返回固定模板、未知结果或稳定解析错误。
    fn resolve(
        &self,
        _name: &str,
        _context: &SpawnAgentTemplateContext,
    ) -> Result<Option<ResolvedSpawnAgentTemplate>, ToolError> {
        if self.fail {
            return Err(ToolError::permanent(
                "agent_template_resolution_failed",
                "测试模板解析失败",
            ));
        }
        Ok(self.template.clone())
    }
}

/// 测试中返回固定已完成 Turn，并记录 Runtime 传入的可信来源上下文。
#[derive(Default)]
struct StaticContextSource {
    /// 按完成顺序返回的父 Turn 消息组。
    turns: Vec<CompletedTurnContext>,
    /// 每次读取收到的可信来源上下文。
    contexts: Mutex<Vec<SpawnAgentTemplateContext>>,
    /// 是否模拟 Transcript 读取失败。
    fail: bool,
}

impl StaticContextSource {
    /// 创建包含固定已完成 Turn 的测试来源。
    fn with_turns(turns: Vec<CompletedTurnContext>) -> Self {
        Self {
            turns,
            ..Self::default()
        }
    }

    /// 创建一旦被读取就失败的测试来源。
    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    /// 返回 Transcript 来源实际被读取的次数。
    fn call_count(&self) -> usize {
        self.contexts
            .lock()
            .expect("测试上下文记录锁不应中毒")
            .len()
    }

    /// 返回最后一次读取收到的可信来源上下文。
    fn last_context(&self) -> SpawnAgentTemplateContext {
        self.contexts
            .lock()
            .expect("测试上下文记录锁不应中毒")
            .last()
            .cloned()
            .expect("测试上下文来源应至少被读取一次")
    }
}

impl SpawnAgentContextSource for StaticContextSource {
    /// 返回固定的已完成 Turn，或模拟一次稳定的 Transcript 读取错误。
    fn completed_turns(
        &self,
        context: &SpawnAgentTemplateContext,
    ) -> Result<Vec<CompletedTurnContext>, ToolError> {
        self.contexts
            .lock()
            .expect("测试上下文记录锁不应中毒")
            .push(context.clone());
        if self.fail {
            return Err(ToolError::permanent(
                "agent_context_unavailable",
                "测试 Transcript 读取失败",
            ));
        }
        Ok(self.turns.clone())
    }
}

/// 创建不包含历史消息的默认测试上下文来源。
fn empty_context_source() -> Arc<dyn SpawnAgentContextSource> {
    Arc::new(StaticContextSource::default())
}

/// 创建绑定空历史来源的默认 spawn_agent 测试工具。
fn spawn_tool(
    coordinator: Arc<CollaborationCoordinator>,
    child_profile: AgentProfile,
) -> SpawnAgentTool {
    SpawnAgentTool::new(coordinator, child_profile, empty_context_source())
}

/// 只维护严格连续事件水位的内存测试 Store。
#[derive(Default)]
struct TestStore {
    /// 串行化水位、批次和 checkpoint 的单一提交边界。
    commit_gate: Mutex<()>,
    /// 最近一次原子追加后的事件序号。
    sequence: Mutex<u64>,
    /// 已按稳定批次标识提交的事件批次，用于模拟连接中断后的精确重放。
    committed_batches: Mutex<HashSet<CollaborationEventBatchId>>,
    /// 最近一次事件提交原子携带的协调器 checkpoint。
    coordinator_checkpoint: Mutex<Option<RecoveredCoordinator>>,
    /// 是否让下一批在完整提交后返回一次不确定结果。
    commit_then_indeterminate: AtomicBool,
}

impl TestStore {
    /// 让下一批事件先持久提交，再向调用方报告一次不确定结果。
    fn commit_then_indeterminate(&self) {
        self.commit_then_indeterminate.store(true, Ordering::SeqCst);
    }
}

impl CollaborationStore for TestStore {
    /// 返回当前测试事件水位。
    fn current_sequence(&self) -> Result<u64, CollaborationPortError> {
        let _gate = self.commit_gate.lock().expect("测试提交门锁不应中毒");
        Ok(*self.sequence.lock().expect("测试 Store 锁不应中毒"))
    }

    /// 返回最近一次与事件批次原子提交的协调器 checkpoint。
    fn load_coordinator_checkpoint(
        &self,
    ) -> Result<Option<RecoveredCoordinator>, CollaborationPortError> {
        let _gate = self.commit_gate.lock().expect("测试提交门锁不应中毒");
        Ok(self
            .coordinator_checkpoint
            .lock()
            .expect("测试 checkpoint 锁不应中毒")
            .clone())
    }

    /// 只接受期望水位、连续事件序号和 checkpoint 完全匹配的提交。
    fn commit_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        let _gate = self.commit_gate.lock().expect("测试提交门锁不应中毒");
        let batch = &commit.batch;
        let mut sequence = self.sequence.lock().expect("测试 Store 锁不应中毒");
        let mut committed_batches = self
            .committed_batches
            .lock()
            .expect("测试已提交批次锁不应中毒");
        if committed_batches.contains(&batch.batch_id) {
            return CollaborationAppendResult::AlreadyCommitted {
                current_sequence: *sequence,
            };
        }
        if *sequence != batch.expected_sequence {
            return CollaborationAppendResult::Conflict {
                actual_sequence: *sequence,
            };
        }
        let mut next = *sequence;
        for event in &batch.events {
            let expected = next.checked_add(1).expect("测试事件序号不会耗尽");
            if event.sequence != expected {
                return CollaborationAppendResult::Absent {
                    current_sequence: *sequence,
                };
            }
            next = expected;
        }
        if commit.checkpoint.last_event_sequence != next {
            return CollaborationAppendResult::Absent {
                current_sequence: *sequence,
            };
        }
        *sequence = next;
        committed_batches.insert(batch.batch_id.clone());
        *self
            .coordinator_checkpoint
            .lock()
            .expect("测试 checkpoint 锁不应中毒") = Some(commit.checkpoint.clone());
        if self.commit_then_indeterminate.swap(false, Ordering::SeqCst) {
            CollaborationAppendResult::Indeterminate {
                error: CollaborationPortError::new("测试提交后连接中断"),
            }
        } else {
            CollaborationAppendResult::Appended
        }
    }

    /// 工具测试不驱逐 Agent，因此始终没有局部 checkpoint。
    fn load_agent_checkpoint(
        &self,
        _agent_id: &AgentId,
    ) -> Result<Option<RecoveredAgentCheckpoint>, CollaborationPortError> {
        Ok(None)
    }

    /// 工具测试允许协调器保存局部 checkpoint，但不需要保留其正文。
    fn save_agent_checkpoint(
        &self,
        _checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<(), CollaborationPortError> {
        Ok(())
    }
}

/// 记录已按 TurnId 幂等接受的启动请求。
#[derive(Default)]
struct TestExecution {
    /// 已接受的 Turn 标识集合。
    accepted: Mutex<HashSet<TurnId>>,
    /// 首次接受时保存的完整启动请求。
    launches: Mutex<Vec<AgentTurnLaunch>>,
}

impl TestExecution {
    /// 返回指定 Turn 的已接收启动请求。
    fn launch(&self, turn_id: &TurnId) -> AgentTurnLaunch {
        self.launches
            .lock()
            .expect("测试启动锁不应中毒")
            .iter()
            .find(|launch| &launch.turn_id == turn_id)
            .cloned()
            .expect("指定 Turn 应已发送给执行端")
    }

    /// 返回已首次接受的启动请求数量。
    fn launch_count(&self) -> usize {
        self.launches.lock().expect("测试启动锁不应中毒").len()
    }
}

impl AgentExecutionPort for TestExecution {
    /// 按 TurnId 幂等接受启动请求并保存取消令牌。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult {
        let mut accepted = self.accepted.lock().expect("测试启动集合锁不应中毒");
        if !accepted.insert(launch.turn_id.clone()) {
            return AgentTurnStartResult::AlreadyAccepted;
        }
        self.launches
            .lock()
            .expect("测试启动锁不应中毒")
            .push(launch);
        AgentTurnStartResult::Accepted
    }

    /// 工具测试只需要信号成功，不需要第二套活动状态。
    fn signal_turn(&self, _signal: AgentTurnSignal) -> Result<(), CollaborationPortError> {
        Ok(())
    }

    /// 工具测试立即确认根树已经静止。
    fn quiesce_tree(&self, _request: QuiesceAgentTree) -> AgentTreeQuiesceResult {
        AgentTreeQuiesceResult::Quiesced
    }

    /// 工具测试立即确认根树清理完成。
    fn close_tree(&self, _request: CloseAgentTree) -> Result<(), CollaborationPortError> {
        Ok(())
    }
}

/// 为 Agent、Session 和 mailbox 消息生成可预测唯一标识。
#[derive(Default)]
struct TestIds {
    /// 所有标识类型共享的单调计数器。
    next: AtomicU64,
}

impl TestIds {
    /// 原子分配下一个正整数。
    fn number(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl CollaborationIdGenerator for TestIds {
    /// 生成唯一 Agent 标识。
    fn next_agent_id(&self) -> AgentId {
        AgentId::new(format!("agent-{}", self.number())).expect("测试 Agent 标识非空")
    }

    /// 生成唯一 Agent Session 标识。
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("session-{}", self.number())).expect("测试 Session 标识非空")
    }

    /// 生成唯一 mailbox 消息标识。
    fn next_message_id(&self) -> MailboxMessageId {
        MailboxMessageId::new(format!("message-{}", self.number())).expect("测试消息标识非空")
    }
}

/// 工具测试共享的协调器、根 Agent 和活跃根 Turn。
struct Fixture {
    /// 被测协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
    /// 可注入原子提交边界故障并供冷恢复复用的 Store。
    store: Arc<TestStore>,
    /// 可检查启动数量和取消令牌的执行端口。
    execution: Arc<TestExecution>,
    /// 根 Agent 的可信标识。
    root_agent_id: AgentId,
    /// 根 Session 的可信标识。
    root_session_id: SessionId,
    /// 当前运行中的根 Turn 标识。
    root_turn_id: TurnId,
}

impl Fixture {
    /// 创建当前根 Turn 的可信工具上下文。
    fn root_context(&self) -> ToolContext {
        tool_context(
            &self.root_session_id,
            &self.root_turn_id,
            &self.root_agent_id,
        )
    }
}

/// 创建指定全局和单树 Turn 容量的运行中根 Agent。
fn fixture(global_limit: usize, root_limit: usize) -> Fixture {
    fixture_with_root_turn_plan(global_limit, root_limit, PlanGuard::inactive())
}

/// 创建使用指定根 Turn Plan 守卫的测试协调器。
fn fixture_with_root_turn_plan(
    global_limit: usize,
    root_limit: usize,
    plan_guard: PlanGuard,
) -> Fixture {
    let execution = Arc::new(TestExecution::default());
    let store = Arc::new(TestStore::default());
    let coordinator = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(global_limit).expect("测试全局容量有效"),
        store.clone(),
        execution.clone(),
        Arc::new(TestIds::default()),
    ));
    let root_session_id = SessionId::new("root-session").expect("根 Session 标识非空");
    let root = coordinator
        .register_root(RootAgentRequest {
            session_id: root_session_id.clone(),
            profile: profile("root"),
            per_root_turn_limit: root_limit,
        })
        .expect("根 Agent 应注册成功");
    let root_turn_id = coordinator
        .begin_root_turn(&root.agent_id, "执行根任务", plan_guard)
        .expect("根 Turn 应启动");
    Fixture {
        coordinator,
        store,
        execution,
        root_agent_id: root.agent_id,
        root_session_id,
        root_turn_id,
    }
}

/// 创建无外部 lease 的可信测试 Agent 配置。
fn profile(name: &str) -> AgentProfile {
    AgentProfile {
        model: format!("model-{name}"),
        reasoning_effort: Some("medium".to_owned()),
        plan_guard: PlanGuard::inactive(),
        cwd: PathBuf::from(format!("D:/workspace/{name}")),
        worktree_lease: None,
        tool_snapshot: vec!["Read".to_owned(), "SendMessage".to_owned()],
    }
}

/// 为互不相关的工具调用分配唯一可信 ToolCall 身份。
fn next_tool_call_id() -> ToolCallId {
    static NEXT_TOOL_CALL_ID: AtomicU64 = AtomicU64::new(1);
    ToolCallId::new(format!(
        "collaboration-tool-test-call-{}",
        NEXT_TOOL_CALL_ID.fetch_add(1, Ordering::SeqCst)
    ))
    .expect("测试 ToolCall 标识应有效")
}

/// 创建只包含可信运行时身份且自动分配 ToolCall 身份的工具上下文。
fn tool_context(session_id: &SessionId, turn_id: &TurnId, agent_id: &AgentId) -> ToolContext {
    tool_context_with_call_id(session_id, turn_id, agent_id, next_tool_call_id())
}

/// 创建使用指定可信 ToolCall 身份的工具上下文，用于重放和冲突测试。
fn tool_context_with_call_id(
    session_id: &SessionId,
    turn_id: &TurnId,
    agent_id: &AgentId,
    tool_call_id: ToolCallId,
) -> ToolContext {
    ToolContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        tool_call_id,
        cancellation: TurnCancellation::new(),
    }
}

/// 返回合法的 spawn_agent 工具输入。
fn agent_input(task_name: &str) -> Value {
    json!({
        "task_name": task_name,
        "message": format!("执行 {task_name} 子任务"),
        "fork_turns": "2"
    })
}

/// 从工具成功结果提取唯一文本并解析 JSON。
fn output_json(output: keencode_agent::ToolOutput) -> Value {
    assert_eq!(output.content.len(), 1, "协作工具结果应只有一个文本块");
    let ToolResultContent::Text { text } = &output.content[0] else {
        panic!("协作工具不应返回图片")
    };
    serde_json::from_str(text).expect("协作工具文本应为合法 JSON")
}

/// 使用 spawn_agent 工具创建子 Agent 并返回输出 JSON。
async fn spawn_with_tool(fixture: &Fixture, task_name: &str) -> Value {
    output_json(
        spawn_tool(fixture.coordinator.clone(), profile(task_name))
            .execute(fixture.root_context(), agent_input(task_name))
            .await
            .expect("spawn_agent 工具应创建子 Agent"),
    )
}

/// 从 spawn_agent 工具输出解析可信子 Agent 标识。
fn spawned_agent_id(output: &Value) -> AgentId {
    AgentId::new(
        output["agent_id"]
            .as_str()
            .expect("spawn_agent 输出应包含 agent_id"),
    )
    .expect("spawn_agent 输出标识应有效")
}

/// 从 spawn_agent 工具输出解析初始 Turn 标识。
fn spawned_turn_id(output: &Value) -> TurnId {
    TurnId::new(
        output["initial_turn_id"]
            .as_str()
            .expect("spawn_agent 输出应包含 initial_turn_id"),
    )
    .expect("Agent 初始 Turn 标识应有效")
}

/// 断言未知目标只返回固定不存在错误，并且不回显目标文本。
fn assert_agent_not_found(error: ToolError, unknown_target: &str) {
    assert_eq!(error.code, "agent_not_found");
    assert!(!error.message.contains(unknown_target));
}

/// 注册函数只加入七个严格协作工具，且 Schema 不接受任一运行时身份伪造。
#[test]
fn registration_and_schemas_reject_runtime_identity_fields() {
    let fixture = fixture(2, 2);
    let mut registry = ToolRegistry::new();
    register_collaboration_tools(
        &mut registry,
        fixture.coordinator,
        profile("registered-child"),
        AgentCapabilities {
            can_spawn_agent: true,
        },
        empty_context_source(),
    )
    .expect("七个协作工具应注册成功");
    let definitions = registry.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "followup_task",
            "interrupt_agent",
            "list_agents",
            "retry_agent",
            "send_message",
            "spawn_agent",
            "wait_agent",
        ]
    );
    for definition in &definitions {
        definition.validate().expect("协作工具 Schema 应有效");
        let properties = definition.input_schema["properties"]
            .as_object()
            .expect("协作工具顶层 properties 应为对象");
        for forbidden in ["session_id", "turn_id", "source_agent_id", "tool_call_id"] {
            assert!(
                !properties.contains_key(forbidden),
                "{} 不得暴露来源字段 {forbidden}",
                definition.name
            );
        }
    }
    for tool_name in ["send_message", "followup_task"] {
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .expect("消息工具定义应存在");
        assert_eq!(
            definition.input_schema["properties"]["target_agent_id"]["description"],
            "使用 spawn_agent/list_agents 返回的 agent_id，不接受任务名或路径"
        );
    }
    let agent = definitions
        .iter()
        .find(|definition| definition.name == "spawn_agent")
        .expect("应存在 spawn_agent 定义");
    for forbidden in ["session_id", "turn_id", "source_agent_id", "tool_call_id"] {
        let mut forged = agent_input("forged");
        forged
            .as_object_mut()
            .expect("测试 spawn_agent 输入应为对象")
            .insert(forbidden.to_owned(), json!("attacker"));
        assert!(
            agent.validate_input(&forged).is_err(),
            "模型输入不得覆盖运行时字段 {forbidden}"
        );
    }
}

/// 单层子 Agent 只能使用通信、等待、查询和中断工具，不能递归创建 Agent。
#[test]
fn child_registration_omits_recursive_spawn_tool() {
    let fixture = fixture(2, 2);
    let mut registry = ToolRegistry::new();
    register_collaboration_tools(
        &mut registry,
        fixture.coordinator,
        profile("nested-child"),
        AgentCapabilities {
            can_spawn_agent: false,
        },
        empty_context_source(),
    )
    .expect("子 Agent 协作工具应注册成功");
    assert_eq!(
        registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        vec![
            "followup_task",
            "interrupt_agent",
            "list_agents",
            "retry_agent",
            "send_message",
            "wait_agent",
        ]
    );
}

/// spawn_agent 即使因容量排队也立即返回身份，且子 Agent 无法递归创建下一层。
#[tokio::test]
async fn agent_returns_identity_under_capacity_and_rejects_recursive_spawn() {
    let saturated = fixture(1, 1);
    let queued = spawn_with_tool(&saturated, "queued").await;
    let queued_agent_id = spawned_agent_id(&queued);
    let queued_turn_id = spawned_turn_id(&queued);
    assert_eq!(queued["outcome"], "created");
    assert_eq!(queued["path"], "/root/queued");
    assert_eq!(
        saturated
            .coordinator
            .agent_status(&queued_agent_id)
            .expect("排队子 Agent 状态应可读取"),
        CollaborationAgentStatus::WaitingCapacity {
            turn_id: queued_turn_id
        }
    );
    assert_eq!(saturated.execution.launch_count(), 1);

    let running = fixture(2, 2);
    let child = spawn_with_tool(&running, "child").await;
    let child_agent_id = spawned_agent_id(&child);
    let child_turn_id = spawned_turn_id(&child);
    let child_context = tool_context(&running.root_session_id, &child_turn_id, &child_agent_id);
    let error = spawn_tool(running.coordinator, profile("nested"))
        .execute(child_context, agent_input("nested"))
        .await
        .expect_err("子 Agent 不得递归创建 Agent");
    assert_eq!(error.code, "recursive_agent_forbidden");
}

/// 默认子 Agent 的持久工具快照必须移除全部根专用工具。
#[tokio::test]
async fn spawned_child_profile_removes_root_only_tools_from_snapshot() {
    let fixture = fixture(2, 2);
    let mut child_profile = profile("snapshot-child");
    child_profile
        .tool_snapshot
        .extend(["spawn_agent", "AskUser", "TodoWrite", "Goal", "Plan"].map(str::to_owned));
    let output = output_json(
        spawn_tool(fixture.coordinator.clone(), child_profile)
            .execute(fixture.root_context(), agent_input("snapshot_child"))
            .await
            .expect("子 Agent 应以收紧后的工具快照创建"),
    );
    let launch = fixture.execution.launch(&spawned_turn_id(&output));
    assert_eq!(launch.agent.profile.tool_snapshot, ["Read", "SendMessage"]);
}

/// Plan 根 Turn 通过 Agent 工具创建的子 Agent 必须继承唯一的只读守卫。
#[tokio::test]
async fn agent_tool_inherits_parent_turn_plan_guard() {
    let fixture = fixture_with_root_turn_plan(2, 2, PlanGuard::read_only());
    let mut requested_profile = profile("plan-tool-child");
    requested_profile.plan_guard = PlanGuard::inactive();
    let output = output_json(
        spawn_tool(fixture.coordinator.clone(), requested_profile)
            .execute(fixture.root_context(), agent_input("plan_tool_child"))
            .await
            .unwrap(),
    );
    let child_turn_id = spawned_turn_id(&output);
    let launch = fixture.execution.launch(&child_turn_id);
    assert_eq!(launch.plan_guard, PlanGuard::read_only());
    assert_eq!(launch.agent.profile.plan_guard, PlanGuard::read_only());
}

/// 显式 Agent 必须在工具入口解析并把模板、模型和工具限制冻结进可恢复定义。
#[tokio::test]
async fn explicit_agent_template_is_frozen_before_spawn_and_survives_restore() {
    let fixture = fixture(2, 2);
    let template_snapshot = AgentTemplateSnapshot {
        name: "reviewer".to_owned(),
        system_prompt: "审查实际变更".to_owned(),
        max_turns: Some(4),
        allowed_write_dirs: vec![PathBuf::from("reports/generated")],
    };
    let resolver: Arc<dyn SpawnAgentTemplateResolver> = Arc::new(StaticTemplateResolver {
        template: Some(ResolvedSpawnAgentTemplate {
            snapshot: template_snapshot.clone(),
            model: Some("provider-a::review-model".to_owned()),
            tool_names: None,
            disallowed_tool_names: vec!["SendMessage".to_owned()],
        }),
        fail: false,
    });
    let mut inherited_profile = profile("template-child");
    inherited_profile
        .tool_snapshot
        .extend(["spawn_agent", "AskUser", "TodoWrite", "Goal", "Plan"].map(str::to_owned));
    let output = output_json(
        spawn_tool(fixture.coordinator.clone(), inherited_profile)
            .with_template_resolver(resolver)
            .execute(
                fixture.root_context(),
                json!({
                    "task_name": "review_task",
                    "message": "审查当前改动",
                    "fork_turns": "none",
                    "agent": "reviewer"
                }),
            )
            .await
            .expect("显式模板应在创建前解析"),
    );
    let child_turn_id = spawned_turn_id(&output);
    let launch = fixture.execution.launch(&child_turn_id);
    assert_eq!(launch.agent.agent_template, Some(template_snapshot.clone()));
    assert_eq!(launch.agent.profile.model, "provider-a::review-model");
    assert_eq!(launch.agent.profile.tool_snapshot, vec!["Read"]);

    let checkpoint = fixture
        .coordinator
        .checkpoint_coordinator()
        .expect("模板定义应进入 checkpoint");
    let encoded = serde_json::to_vec(&checkpoint).expect("模板 checkpoint 应可序列化");
    let recovered: RecoveredCoordinator =
        serde_json::from_slice(&encoded).expect("模板 checkpoint 应可反序列化");
    let recovered_definition = recovered.roots[0]
        .known_agents
        .iter()
        .find(|definition| definition.agent_template.is_some())
        .expect("恢复快照应包含模板子 Agent");
    assert_eq!(
        recovered_definition.agent_template.as_ref(),
        Some(&template_snapshot)
    );
    let restored = CollaborationCoordinator::new(
        CollaborationLimits::new(2).expect("恢复容量应有效"),
        fixture.store,
        Arc::new(TestExecution::default()),
        Arc::new(TestIds::default()),
    );
    restored
        .restore_coordinator(recovered)
        .expect("带模板的完整 checkpoint 应可冷恢复");
}

/// 显式模板即使重选根专用工具，最终冻结的子 Agent Profile 也必须统一移除。
#[tokio::test]
async fn explicit_agent_template_cannot_restore_root_only_tools() {
    let fixture = fixture(2, 2);
    let mut child_profile = profile("explicit-template-child");
    child_profile
        .tool_snapshot
        .extend(["spawn_agent", "AskUser", "TodoWrite", "Goal", "Plan"].map(str::to_owned));
    let resolver: Arc<dyn SpawnAgentTemplateResolver> = Arc::new(StaticTemplateResolver {
        template: Some(ResolvedSpawnAgentTemplate {
            snapshot: AgentTemplateSnapshot {
                name: "reviewer".to_owned(),
                system_prompt: "审查实际变更".to_owned(),
                max_turns: None,
                allowed_write_dirs: Vec::new(),
            },
            model: None,
            tool_names: Some(vec![
                "Read".to_owned(),
                "AskUser".to_owned(),
                "TodoWrite".to_owned(),
                "Goal".to_owned(),
                "Plan".to_owned(),
                "spawn_agent".to_owned(),
            ]),
            disallowed_tool_names: Vec::new(),
        }),
        fail: false,
    });
    let output = output_json(
        spawn_tool(fixture.coordinator.clone(), child_profile)
            .with_template_resolver(resolver)
            .execute(
                fixture.root_context(),
                json!({
                    "task_name": "explicit_template_tools",
                    "message": "审查当前改动",
                    "fork_turns": "none",
                    "agent": "reviewer"
                }),
            )
            .await
            .expect("显式模板应创建收紧后的子 Agent"),
    );
    let launch = fixture.execution.launch(&spawned_turn_id(&output));
    assert_eq!(launch.agent.profile.tool_snapshot, ["Read"]);
}

/// 显式 Agent 未知或解析失败时必须在创建身份前 fail-closed。
#[tokio::test]
async fn explicit_agent_template_unknown_and_error_do_not_spawn() {
    for (resolver, expected_code) in [
        (
            StaticTemplateResolver {
                template: None,
                fail: false,
            },
            "agent_template_not_found",
        ),
        (
            StaticTemplateResolver {
                template: None,
                fail: true,
            },
            "agent_template_resolution_failed",
        ),
    ] {
        let fixture = fixture(2, 2);
        let error = spawn_tool(fixture.coordinator.clone(), profile("strict-template"))
            .with_template_resolver(Arc::new(resolver))
            .execute(
                fixture.root_context(),
                json!({
                    "task_name": "strict_template",
                    "message": "不得回退到通用 Agent",
                    "fork_turns": "none",
                    "agent": "missing"
                }),
            )
            .await
            .expect_err("未知或失败模板必须拒绝");
        assert_eq!(error.code, expected_code);
        assert_eq!(fixture.execution.launch_count(), 1);
    }
}

/// 完整历史继承不能通过显式模板绕过父模型冻结规则。
#[tokio::test]
async fn explicit_agent_template_model_override_is_rejected_for_all_history() {
    let fixture = fixture(2, 2);
    let resolver: Arc<dyn SpawnAgentTemplateResolver> = Arc::new(StaticTemplateResolver {
        template: Some(ResolvedSpawnAgentTemplate {
            snapshot: AgentTemplateSnapshot {
                name: "other-model".to_owned(),
                system_prompt: "使用专用模型".to_owned(),
                max_turns: None,
                allowed_write_dirs: Vec::new(),
            },
            model: Some("provider-b::other-model".to_owned()),
            tool_names: None,
            disallowed_tool_names: Vec::new(),
        }),
        fail: false,
    });
    let source = Arc::new(StaticContextSource::default());
    let error = SpawnAgentTool::new(
        fixture.coordinator.clone(),
        profile("all-template-child"),
        source.clone(),
    )
    .with_template_resolver(resolver)
    .execute(
        fixture.root_context(),
        json!({
            "task_name": "all_template_child",
            "message": "不得切换模型后继承完整历史",
            "fork_turns": "all",
            "agent": "other-model"
        }),
    )
    .await
    .expect_err("完整历史继承不得接受模板模型覆盖");
    assert_eq!(error.code, "invalid_input");
    assert_eq!(source.call_count(), 0);
    assert_eq!(fixture.execution.launch_count(), 1);
}

/// `fork_turns=none` 必须完全跳过 Transcript 来源，即使来源当前不可用也能创建空快照。
#[tokio::test]
async fn spawn_agent_none_context_does_not_read_transcript_source() {
    let fixture = fixture(2, 2);
    let source = Arc::new(StaticContextSource::failing());
    let output = output_json(
        SpawnAgentTool::new(
            fixture.coordinator.clone(),
            profile("none-context-child"),
            source.clone(),
        )
        .execute(
            fixture.root_context(),
            json!({
                "task_name": "none_context_child",
                "message": "不要继承父 Transcript",
                "fork_turns": "none"
            }),
        )
        .await
        .expect("none 继承不应读取 Transcript 来源"),
    );
    let launch = fixture.execution.launch(&spawned_turn_id(&output));
    assert!(launch.agent.context_snapshot.is_empty());
    assert_eq!(source.call_count(), 0);
}

/// `fork_turns=all` 必须按已完成 Turn 顺序冻结每条 Provider 中立消息的规范 JSON。
#[tokio::test]
async fn spawn_agent_all_context_freezes_completed_turn_messages_in_order() {
    let fixture = fixture(2, 2);
    let messages = [
        Message::text(MessageRole::User, "第一个已完成 Turn 的问题"),
        Message::text(MessageRole::Assistant, "第一个已完成 Turn 的回答"),
        Message::text(MessageRole::User, "第二个已完成 Turn 的问题"),
        Message::text(MessageRole::Assistant, "第二个已完成 Turn 的回答"),
    ];
    let source = Arc::new(StaticContextSource::with_turns(vec![
        CompletedTurnContext {
            messages: messages[..2].to_vec(),
        },
        CompletedTurnContext {
            messages: messages[2..].to_vec(),
        },
    ]));
    let output = output_json(
        SpawnAgentTool::new(
            fixture.coordinator.clone(),
            profile("all-context-child"),
            source.clone(),
        )
        .execute(
            fixture.root_context(),
            json!({
                "task_name": "all_context_child",
                "message": "继承全部已完成 Turn",
                "fork_turns": "all"
            }),
        )
        .await
        .expect("all 继承应冻结全部已完成 Turn"),
    );
    let launch = fixture.execution.launch(&spawned_turn_id(&output));
    assert_eq!(
        launch.agent.context_snapshot,
        messages
            .iter()
            .map(|message| serde_json::to_string::<Message>(message).expect("测试消息应可规范编码"))
            .collect::<Vec<_>>()
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(
        source.last_context(),
        SpawnAgentTemplateContext {
            session_id: fixture.root_session_id.clone(),
            parent_agent_id: fixture.root_agent_id.clone(),
            root_turn_id: fixture.root_turn_id.clone(),
        }
    );
}

/// 最近 N 继承按 Turn 分组截取，必须保留选中 Turn 内的全部消息与原始顺序。
#[tokio::test]
async fn spawn_agent_recent_context_selects_completed_turn_groups_not_messages() {
    let fixture = fixture(2, 2);
    let old = Message::text(MessageRole::User, "不应继承的旧 Turn");
    let recent = [
        Message::text(MessageRole::User, "最近第二个 Turn 的问题"),
        Message::text(MessageRole::Assistant, "最近第二个 Turn 的回答"),
        Message::text(MessageRole::User, "最近第一个 Turn 的问题"),
        Message::text(MessageRole::Assistant, "最近第一个 Turn 的回答"),
    ];
    let source = Arc::new(StaticContextSource::with_turns(vec![
        CompletedTurnContext {
            messages: vec![old],
        },
        CompletedTurnContext {
            messages: recent[..2].to_vec(),
        },
        CompletedTurnContext {
            messages: recent[2..].to_vec(),
        },
    ]));
    let output = output_json(
        SpawnAgentTool::new(
            fixture.coordinator.clone(),
            profile("recent-context-child"),
            source.clone(),
        )
        .execute(
            fixture.root_context(),
            json!({
                "task_name": "recent_context_child",
                "message": "只继承最近两个已完成 Turn",
                "fork_turns": "2"
            }),
        )
        .await
        .expect("最近 N 继承应按 Turn 截取"),
    );
    let launch = fixture.execution.launch(&spawned_turn_id(&output));
    assert_eq!(
        launch.agent.context_snapshot,
        recent
            .iter()
            .map(|message| serde_json::to_string::<Message>(message).expect("测试消息应可规范编码"))
            .collect::<Vec<_>>()
    );
    assert_eq!(source.call_count(), 1);
}

/// 需要读取历史时 Transcript 来源失败必须在创建子 Agent 身份前 fail-closed。
#[tokio::test]
async fn spawn_agent_context_source_failure_does_not_create_child() {
    let fixture = fixture(2, 2);
    let source = Arc::new(StaticContextSource::failing());
    let error = SpawnAgentTool::new(
        fixture.coordinator.clone(),
        profile("failed-context-child"),
        source.clone(),
    )
    .execute(
        fixture.root_context(),
        json!({
            "task_name": "failed_context_child",
            "message": "来源失败时不得创建",
            "fork_turns": "all"
        }),
    )
    .await
    .expect_err("Transcript 来源失败必须拒绝 spawn");
    assert_eq!(error.code, "agent_context_unavailable");
    assert_eq!(source.call_count(), 1);
    assert_eq!(fixture.execution.launch_count(), 1);
}

/// 完整历史继承必须固定沿用父 Agent 模型配置，不能由模型输入覆盖。
#[tokio::test]
async fn spawn_agent_rejects_model_overrides_when_forking_all_turns() {
    let fixture = fixture(2, 2);
    let tool = spawn_tool(fixture.coordinator.clone(), profile("all_history_child"));
    for input in [
        json!({
            "task_name": "model_override",
            "message": "尝试覆盖模型",
            "fork_turns": "all",
            "model": "other-model"
        }),
        json!({
            "task_name": "effort_override",
            "message": "尝试覆盖推理强度",
            "reasoning_effort": "high"
        }),
    ] {
        let error = tool
            .execute(fixture.root_context(), input)
            .await
            .expect_err("fork_turns=all 不得覆盖模型配置");
        assert_eq!(error.code, "invalid_input");
    }
    assert_eq!(fixture.execution.launch_count(), 1);
}

/// Agent 与 SendMessage 使用同一可信 ToolCall 身份重放时返回首次结果且不重复副作用。
#[tokio::test]
async fn agent_and_send_message_replay_same_trusted_call_id_without_duplicates() {
    let fixture = fixture(3, 3);
    let agent_call_id = ToolCallId::new("agent-replay-call").expect("重放 ToolCall 标识应有效");
    let agent_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        agent_call_id,
    );
    let agent_tool = spawn_tool(fixture.coordinator.clone(), profile("replay_child"));
    let input = agent_input("replay_child");
    let first_agent = output_json(
        agent_tool
            .execute(agent_context.clone(), input.clone())
            .await
            .expect("首次 Agent 调用应成功"),
    );
    let launches_after_first = fixture.execution.launch_count();
    let replayed_agent = output_json(
        agent_tool
            .execute(agent_context, input)
            .await
            .expect("相同可信 ToolCall 的 Agent 重放应成功"),
    );
    assert_eq!(replayed_agent, first_agent);
    assert_eq!(fixture.execution.launch_count(), launches_after_first);

    let child_agent_id = spawned_agent_id(&first_agent);
    let message_call_id =
        ToolCallId::new("message-replay-call").expect("消息重放 ToolCall 标识应有效");
    let message_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        message_call_id,
    );
    let message_input = json!({
        "target_agent_id": child_agent_id.as_str(),
        "message": "只允许持久化一次的消息"
    });
    let message_tool = SendMessageTool::new(fixture.coordinator.clone());
    let first_message = output_json(
        message_tool
            .execute(message_context.clone(), message_input.clone())
            .await
            .expect("首次 SendMessage 调用应成功"),
    );
    let replayed_message = output_json(
        message_tool
            .execute(message_context, message_input)
            .await
            .expect("相同可信 ToolCall 的 SendMessage 重放应成功"),
    );
    assert_eq!(replayed_message, first_message);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child_agent_id)
            .expect("重放后目标 mailbox 应可读取")
            .len(),
        1,
        "SendMessage 重放不得重复入队"
    );
}

/// 同一可信 ToolCall 身份改写业务输入时必须返回不泄露输入的稳定冲突。
#[tokio::test]
async fn same_trusted_call_id_with_changed_input_is_safe_conflict() {
    let fixture = fixture(3, 3);
    let call_id_text = "agent-conflict-call";
    let context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        ToolCallId::new(call_id_text).expect("冲突 ToolCall 标识应有效"),
    );
    let tool = spawn_tool(fixture.coordinator.clone(), profile("conflict_child"));
    let first_input = agent_input("conflict_child");
    let first_agent = output_json(
        tool.execute(context.clone(), first_input.clone())
            .await
            .expect("首次 Agent 调用应成功"),
    );
    let child_agent_id = spawned_agent_id(&first_agent);
    let child_turn_id = spawned_turn_id(&first_agent);
    let launches_after_first = fixture.execution.launch_count();

    let secret_changed_input = "changed-secret-不应出现在错误结果";
    let mut changed_input = first_input;
    changed_input["message"] = json!(secret_changed_input);
    let error = tool
        .execute(context.clone(), changed_input)
        .await
        .expect_err("同一 ToolCall 改写输入必须冲突");
    assert_eq!(error.code, "collaboration_idempotency_conflict");
    assert!(!error.message.contains(secret_changed_input));
    assert!(!error.message.contains(call_id_text));
    assert_eq!(fixture.execution.launch_count(), launches_after_first);

    let cross_operation_error = InterruptAgentTool::new(fixture.coordinator.clone())
        .execute(
            context,
            json!({ "target_agent_id": child_agent_id.as_str() }),
        )
        .await
        .expect_err("同一 ToolCall 改为 interrupt_agent 必须稳定冲突");
    assert_eq!(
        cross_operation_error.code,
        "collaboration_idempotency_conflict"
    );
    assert!(!cross_operation_error.message.contains(call_id_text));
    assert_eq!(fixture.execution.launch_count(), launches_after_first);
    assert!(
        !fixture
            .execution
            .launch(&child_turn_id)
            .cancellation
            .is_cancelled(),
        "跨操作冲突不得误停止首次创建的子 Agent"
    );
}

/// 完整协调器冷恢复后，三类可变更协作调用仍返回首次结果且不重复状态。
#[tokio::test]
async fn collaboration_replay_survives_cold_coordinator_restore() {
    let fixture = fixture(3, 3);
    let agent_call_id = ToolCallId::new("cold-agent-call").expect("冷恢复 ToolCall 标识应有效");
    let agent_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        agent_call_id.clone(),
    );
    let agent_profile = profile("cold_child");
    let agent_input = agent_input("cold_child");
    let first_agent = output_json(
        spawn_tool(fixture.coordinator.clone(), agent_profile.clone())
            .execute(agent_context, agent_input.clone())
            .await
            .expect("冷恢复前 Agent 调用应成功"),
    );
    let child_agent_id = spawned_agent_id(&first_agent);
    let child_turn_id = spawned_turn_id(&first_agent);

    let message_call_id =
        ToolCallId::new("cold-message-call").expect("冷恢复消息 ToolCall 标识应有效");
    let message_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        message_call_id.clone(),
    );
    let message_input = json!({
        "target_agent_id": child_agent_id.as_str(),
        "message": "冷恢复后不能重复的消息"
    });
    let first_message = output_json(
        SendMessageTool::new(fixture.coordinator.clone())
            .execute(message_context, message_input.clone())
            .await
            .expect("冷恢复前 SendMessage 调用应成功"),
    );
    let stop_call_id =
        ToolCallId::new("cold-stop-call").expect("冷恢复 StopAgent ToolCall 标识应有效");
    let stop_input = json!({ "target_agent_id": child_agent_id.as_str() });
    let first_stop = output_json(
        InterruptAgentTool::new(fixture.coordinator.clone())
            .execute(
                tool_context_with_call_id(
                    &fixture.root_session_id,
                    &fixture.root_turn_id,
                    &fixture.root_agent_id,
                    stop_call_id.clone(),
                ),
                stop_input.clone(),
            )
            .await
            .expect("冷恢复前 StopAgent 调用应成功"),
    );
    assert_eq!(first_stop["turn_id"], child_turn_id.as_str());
    let checkpoint = fixture
        .coordinator
        .checkpoint_coordinator()
        .expect("协调器 checkpoint 应包含协作幂等记录");

    let restored_execution = Arc::new(TestExecution::default());
    let restored = Arc::new(CollaborationCoordinator::new(
        CollaborationLimits::new(3).expect("恢复全局容量有效"),
        fixture.store.clone(),
        restored_execution.clone(),
        Arc::new(TestIds::default()),
    ));
    restored
        .restore_coordinator(checkpoint)
        .expect("协调器应从完整 checkpoint 冷恢复");
    let sequence_before_replays = fixture
        .store
        .current_sequence()
        .expect("冷恢复后应可读取事件水位");
    let checkpoint_before_replays = restored
        .checkpoint_coordinator()
        .expect("冷恢复重放前应可读取协调器 checkpoint");

    let replayed_agent = output_json(
        spawn_tool(restored.clone(), agent_profile)
            .execute(
                tool_context_with_call_id(
                    &fixture.root_session_id,
                    &fixture.root_turn_id,
                    &fixture.root_agent_id,
                    agent_call_id,
                ),
                agent_input,
            )
            .await
            .expect("冷恢复后 Agent 调用应从幂等记录重放"),
    );
    let replayed_message = output_json(
        SendMessageTool::new(restored.clone())
            .execute(
                tool_context_with_call_id(
                    &fixture.root_session_id,
                    &fixture.root_turn_id,
                    &fixture.root_agent_id,
                    message_call_id,
                ),
                message_input,
            )
            .await
            .expect("冷恢复后 SendMessage 调用应从幂等记录重放"),
    );
    let replayed_stop = output_json(
        InterruptAgentTool::new(restored.clone())
            .execute(
                tool_context_with_call_id(
                    &fixture.root_session_id,
                    &fixture.root_turn_id,
                    &fixture.root_agent_id,
                    stop_call_id,
                ),
                stop_input,
            )
            .await
            .expect("冷恢复后 StopAgent 调用应从幂等记录重放"),
    );
    assert_eq!(replayed_agent, first_agent);
    assert_eq!(replayed_message, first_message);
    assert_eq!(replayed_stop, first_stop);
    assert_eq!(restored_execution.launch_count(), 0);
    assert_eq!(
        fixture
            .store
            .current_sequence()
            .expect("冷恢复重放后应可读取事件水位"),
        sequence_before_replays,
        "冷恢复重放不得追加状态或幂等提交事件"
    );
    assert_eq!(
        restored
            .checkpoint_coordinator()
            .expect("冷恢复重放后应可读取协调器 checkpoint"),
        checkpoint_before_replays,
        "冷恢复重放不得改变协调器持久投影"
    );
    assert_eq!(
        restored
            .mailbox(&child_agent_id)
            .expect("冷恢复后目标 mailbox 应可读取")
            .len(),
        1,
        "冷恢复重放不得重复消息"
    );
}

/// Store 提交后返回不确定结果时，精确批次对账和随后重放都不得重复副作用。
#[tokio::test]
async fn commit_then_indeterminate_reconciles_without_duplicate_collaboration_effects() {
    let fixture = fixture(3, 3);
    fixture.store.commit_then_indeterminate();
    let agent_call_id =
        ToolCallId::new("indeterminate-agent-call").expect("不确定 Agent ToolCall 标识应有效");
    let agent_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        agent_call_id,
    );
    let agent_input = agent_input("indeterminate_child");
    let agent_tool = spawn_tool(fixture.coordinator.clone(), profile("indeterminate_child"));
    let first_agent = output_json(
        agent_tool
            .execute(agent_context.clone(), agent_input.clone())
            .await
            .expect("提交后不确定的 Agent 调用应通过同批次对账成功"),
    );
    let launches_after_first = fixture.execution.launch_count();
    let replayed_agent = output_json(
        agent_tool
            .execute(agent_context, agent_input)
            .await
            .expect("对账后的 Agent 调用应可安全重放"),
    );
    assert_eq!(replayed_agent, first_agent);
    assert_eq!(fixture.execution.launch_count(), launches_after_first);

    let child_agent_id = spawned_agent_id(&first_agent);
    fixture.store.commit_then_indeterminate();
    let message_call_id =
        ToolCallId::new("indeterminate-message-call").expect("不确定消息 ToolCall 标识应有效");
    let message_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        message_call_id,
    );
    let message_input = json!({
        "target_agent_id": child_agent_id.as_str(),
        "message": "提交后不确定但只能入队一次"
    });
    let message_tool = SendMessageTool::new(fixture.coordinator.clone());
    let first_message = output_json(
        message_tool
            .execute(message_context.clone(), message_input.clone())
            .await
            .expect("提交后不确定的 SendMessage 应通过同批次对账成功"),
    );
    let replayed_message = output_json(
        message_tool
            .execute(message_context, message_input)
            .await
            .expect("对账后的 SendMessage 应可安全重放"),
    );
    assert_eq!(replayed_message, first_message);
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child_agent_id)
            .expect("不确定提交对账后 mailbox 应可读取")
            .len(),
        1,
        "提交后不确定不得导致重复 mailbox 条目"
    );

    fixture.store.commit_then_indeterminate();
    let stop_call_id =
        ToolCallId::new("indeterminate-stop-call").expect("不确定 StopAgent ToolCall 标识应有效");
    let stop_context = tool_context_with_call_id(
        &fixture.root_session_id,
        &fixture.root_turn_id,
        &fixture.root_agent_id,
        stop_call_id,
    );
    let stop_input = json!({ "target_agent_id": child_agent_id.as_str() });
    let stop_tool = InterruptAgentTool::new(fixture.coordinator.clone());
    let first_stop = output_json(
        stop_tool
            .execute(stop_context.clone(), stop_input.clone())
            .await
            .expect("提交后不确定的 StopAgent 应通过同批次对账成功"),
    );
    let sequence_after_stop = fixture
        .store
        .current_sequence()
        .expect("StopAgent 对账后应可读取事件水位");
    let checkpoint_after_stop = fixture
        .coordinator
        .checkpoint_coordinator()
        .expect("StopAgent 对账后应可读取协调器 checkpoint");
    let status_after_stop = fixture
        .coordinator
        .agent_status(&child_agent_id)
        .expect("StopAgent 对账后应可读取目标状态");
    assert!(
        fixture
            .execution
            .launch(&spawned_turn_id(&first_agent))
            .cancellation
            .is_cancelled(),
        "提交后不确定的 StopAgent 对账成功后必须执行取消动作"
    );
    let replayed_stop = output_json(
        stop_tool
            .execute(stop_context, stop_input)
            .await
            .expect("对账后的 StopAgent 应可安全重放"),
    );
    assert_eq!(replayed_stop, first_stop);
    assert_eq!(
        fixture
            .store
            .current_sequence()
            .expect("StopAgent 重放后应可读取事件水位"),
        sequence_after_stop,
        "StopAgent 重放不得追加重复状态或幂等提交事件"
    );
    assert_eq!(
        fixture
            .coordinator
            .checkpoint_coordinator()
            .expect("StopAgent 重放后应可读取协调器 checkpoint"),
        checkpoint_after_stop,
        "StopAgent 重放不得改变协调器持久投影"
    );
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child_agent_id)
            .expect("StopAgent 重放后应可读取目标状态"),
        status_after_stop,
        "StopAgent 重放不得重复改变目标状态"
    );
}

/// WaitAgent 只返回 mailbox 活动摘要，调用前后完整正文和字节序列完全不变。
#[tokio::test]
async fn wait_reports_activity_without_consuming_or_leaking_mailbox_content() {
    let fixture = fixture(2, 2);
    let child = spawn_with_tool(&fixture, "worker").await;
    let child_agent_id = spawned_agent_id(&child);
    let child_turn_id = spawned_turn_id(&child);
    let secret = "mailbox-secret-正文-不得出现在等待结果";
    SendMessageTool::new(fixture.coordinator.clone())
        .execute(
            tool_context(&fixture.root_session_id, &child_turn_id, &child_agent_id),
            json!({
                "target_agent_id": fixture.root_agent_id.as_str(),
                "message": secret
            }),
        )
        .await
        .expect("子 Agent 应能向父 Agent 发送消息");
    let before = fixture
        .coordinator
        .mailbox(&fixture.root_agent_id)
        .expect("等待前 mailbox 应可读取");
    let output = WaitAgentTool::new(fixture.coordinator.clone())
        .execute(fixture.root_context(), json!({ "timeout_ms": 0 }))
        .await
        .expect("已有活动时 WaitAgent 应立即返回");
    let output_text = match &output.content[0] {
        ToolResultContent::Text { text } => text,
        ToolResultContent::Image { .. } => panic!("WaitAgent 不应返回图片"),
    };
    let output_json: Value = serde_json::from_str(output_text).expect("等待结果应为 JSON");
    let after = fixture
        .coordinator
        .mailbox(&fixture.root_agent_id)
        .expect("等待后 mailbox 应可读取");
    assert_eq!(output_json["outcome"], "mailbox_activity");
    assert_eq!(output_json["pending_count"], 1);
    assert!(!output_text.contains(secret), "等待摘要不得泄露正文");
    assert_eq!(before, after, "WaitAgent 不得改变 mailbox 的任一字节");
}

/// WaitAgent 分别稳定报告硬超时、等待期间 TurnEnded 和工具取消。
#[tokio::test]
async fn wait_handles_timeout_turn_end_and_cancellation() {
    let timeout_fixture = fixture(1, 1);
    let timed_out = output_json(
        WaitAgentTool::new(timeout_fixture.coordinator.clone())
            .execute(timeout_fixture.root_context(), json!({ "timeout_ms": 1 }))
            .await
            .expect("空 mailbox 应在硬超时后返回"),
    );
    assert_eq!(timed_out["outcome"], "timed_out");

    let ended_fixture = fixture(1, 1);
    let ended_coordinator = ended_fixture.coordinator.clone();
    let ended_agent_id = ended_fixture.root_agent_id.clone();
    let ended_turn_id = ended_fixture.root_turn_id.clone();
    let completion = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        ended_coordinator
            .complete_turn(
                &ended_agent_id,
                &ended_turn_id,
                AgentTurnOutcome::Completed {
                    final_message: Some("done".to_owned()),
                },
            )
            .expect("根 Turn 应在等待期间完成");
    });
    let ended = output_json(
        WaitAgentTool::new(ended_fixture.coordinator.clone())
            .execute(ended_fixture.root_context(), json!({ "timeout_ms": 1_000 }))
            .await
            .expect("等待期间 Turn 完成应返回 TurnEnded"),
    );
    completion.await.expect("完成任务不应 panic");
    assert_eq!(ended["outcome"], "turn_ended");

    let cancelled_fixture = fixture(1, 1);
    let context = cancelled_fixture.root_context();
    let cancellation = context.cancellation.clone();
    let cancellation_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancellation.cancel();
    });
    let error = WaitAgentTool::new(cancelled_fixture.coordinator)
        .execute(context, json!({ "timeout_ms": 1_000 }))
        .await
        .expect_err("取消应终止 WaitAgent");
    cancellation_task.await.expect("取消任务不应 panic");
    assert_eq!(error.code, "turn_cancelled");
}

/// 未知 ID 或路径只能返回 agent_not_found；随后使用有效身份仍可完成四类操作。
#[tokio::test]
async fn unknown_target_errors_are_directed_and_valid_targets_remain_operable() {
    let send_fixture = fixture(2, 2);
    let send_child = spawn_with_tool(&send_fixture, "unknown_send_target").await;
    let send_child_agent_id = spawned_agent_id(&send_child);
    let unknown_path = "/root/native_reader_a";
    let send_error = SendMessageTool::new(send_fixture.coordinator.clone())
        .execute(
            send_fixture.root_context(),
            json!({
                "target_agent_id": unknown_path,
                "message": "未知路径"
            }),
        )
        .await
        .expect_err("未知路径不能被 SendMessage 当作恢复故障");
    assert_agent_not_found(send_error, unknown_path);
    let send_output = output_json(
        SendMessageTool::new(send_fixture.coordinator.clone())
            .execute(
                send_fixture.root_context(),
                json!({
                    "target_agent_id": send_child_agent_id.as_str(),
                    "message": "有效 SendMessage"
                }),
            )
            .await
            .expect("未知路径失败后有效 SendMessage 仍应执行"),
    );
    assert_eq!(send_output["outcome"], "queued");

    let followup_fixture = fixture(2, 2);
    let followup_child = spawn_with_tool(&followup_fixture, "unknown_followup_target").await;
    let followup_child_agent_id = spawned_agent_id(&followup_child);
    let unknown_id = "unknown-followup-agent";
    let followup_error = FollowupTaskTool::new(followup_fixture.coordinator.clone())
        .execute(
            followup_fixture.root_context(),
            json!({
                "target_agent_id": unknown_id,
                "message": "未知 ID"
            }),
        )
        .await
        .expect_err("未知 ID 不能被 FollowupTask 当作恢复故障");
    assert_agent_not_found(followup_error, unknown_id);
    let followup_output = output_json(
        FollowupTaskTool::new(followup_fixture.coordinator.clone())
            .execute(
                followup_fixture.root_context(),
                json!({
                    "target_agent_id": followup_child_agent_id.as_str(),
                    "message": "有效 FollowupTask"
                }),
            )
            .await
            .expect("未知 ID 失败后有效 FollowupTask 仍应执行"),
    );
    assert_eq!(followup_output["outcome"], "queued");
    assert!(followup_output["triggered_turn_id"].is_null());

    let interrupt_fixture = fixture(2, 2);
    let interrupt_child = spawn_with_tool(&interrupt_fixture, "unknown_interrupt_target").await;
    let interrupt_child_agent_id = spawned_agent_id(&interrupt_child);
    let interrupt_error = InterruptAgentTool::new(interrupt_fixture.coordinator.clone())
        .execute(
            interrupt_fixture.root_context(),
            json!({ "target_agent_id": unknown_path }),
        )
        .await
        .expect_err("未知路径不能被 InterruptAgent 当作恢复故障");
    assert_agent_not_found(interrupt_error, unknown_path);
    let interrupt_output = output_json(
        InterruptAgentTool::new(interrupt_fixture.coordinator.clone())
            .execute(
                interrupt_fixture.root_context(),
                json!({ "target_agent_id": interrupt_child_agent_id.as_str() }),
            )
            .await
            .expect("未知路径失败后有效 InterruptAgent 仍应执行"),
    );
    assert_eq!(interrupt_output["outcome"], "interrupt_requested");

    let retry_fixture = fixture(2, 2);
    let retry_child = spawn_with_tool(&retry_fixture, "unknown_retry_target").await;
    let retry_child_agent_id = spawned_agent_id(&retry_child);
    let retry_child_turn_id = spawned_turn_id(&retry_child);
    retry_fixture
        .coordinator
        .complete_turn(
            &retry_child_agent_id,
            &retry_child_turn_id,
            AgentTurnOutcome::Failed {
                message: "准备重试的失败 Turn".to_owned(),
            },
        )
        .expect("重试目标初始 Turn 应进入失败终态");
    let retry_unknown_id = "unknown-retry-agent";
    let retry_error = RetryAgentTool::new(retry_fixture.coordinator.clone())
        .execute(
            retry_fixture.root_context(),
            json!({ "target_agent_id": retry_unknown_id }),
        )
        .await
        .expect_err("未知 ID 不能被 RetryAgent 当作恢复故障");
    assert_agent_not_found(retry_error, retry_unknown_id);
    let retry_output = output_json(
        RetryAgentTool::new(retry_fixture.coordinator.clone())
            .execute(
                retry_fixture.root_context(),
                json!({ "target_agent_id": retry_child_agent_id.as_str() }),
            )
            .await
            .expect("未知 ID 失败后有效 RetryAgent 仍应执行"),
    );
    assert_eq!(retry_output["outcome"], "retry_queued");
}

/// 已驻留的跨树目标仍返回固定跨树错误，且不回显另一棵树的身份。
#[tokio::test]
async fn resident_cross_tree_target_remains_forbidden_without_identity_leak() {
    let fixture = fixture(4, 4);
    let other_root = fixture
        .coordinator
        .register_root(RootAgentRequest {
            session_id: SessionId::new("other-tree-session").expect("跨树 Session 应有效"),
            profile: profile("other-tree"),
            per_root_turn_limit: 2,
        })
        .expect("第二棵根树应注册成功");
    fixture
        .coordinator
        .begin_root_turn(
            &other_root.agent_id,
            "另一棵树的 Turn",
            PlanGuard::inactive(),
        )
        .expect("第二棵根树 Turn 应启动");
    let foreign_agent_id = other_root.agent_id.as_str().to_owned();
    let error = SendMessageTool::new(fixture.coordinator.clone())
        .execute(
            fixture.root_context(),
            json!({
                "target_agent_id": foreign_agent_id,
                "message": "不得跨树投递"
            }),
        )
        .await
        .expect_err("驻留的跨树目标必须保持禁止");
    assert_eq!(error.code, "cross_tree_operation_forbidden");
    assert!(!error.message.contains(&foreign_agent_id));
}

/// StopAgent 重复请求返回同一 Turn，且 stale 来源 Turn 不能中断目标。
#[tokio::test]
async fn stop_is_idempotent_and_validates_causal_source_turn() {
    let fixture = fixture(3, 3);
    let child = spawn_with_tool(&fixture, "stop_target").await;
    let child_agent_id = spawned_agent_id(&child);
    let child_turn_id = spawned_turn_id(&child);
    let other_child = spawn_with_tool(&fixture, "other_stop_target").await;
    let other_child_agent_id = spawned_agent_id(&other_child);
    let tool = InterruptAgentTool::new(fixture.coordinator.clone());
    let input = json!({ "target_agent_id": child_agent_id.as_str() });
    let context = fixture.root_context();
    let first = output_json(
        tool.execute(context.clone(), input.clone())
            .await
            .expect("第一次停止请求应成功"),
    );
    let second = output_json(
        tool.execute(context.clone(), input.clone())
            .await
            .expect("重复停止请求应幂等成功"),
    );
    assert_eq!(first, second);
    assert_eq!(first["turn_id"], child_turn_id.as_str());
    assert!(
        fixture
            .execution
            .launch(&child_turn_id)
            .cancellation
            .is_cancelled(),
        "StopAgent 应取消执行端收到的同一 Turn 令牌"
    );
    let conflict = tool
        .execute(
            context.clone(),
            json!({ "target_agent_id": other_child_agent_id.as_str() }),
        )
        .await
        .expect_err("同一 ToolCall 改写停止目标必须冲突");
    assert_eq!(conflict.code, "collaboration_idempotency_conflict");

    let stale_context = tool_context(
        &fixture.root_session_id,
        &TurnId::new("stale-root-turn").expect("测试 stale Turn 非空"),
        &fixture.root_agent_id,
    );
    let error = tool
        .execute(stale_context, input.clone())
        .await
        .expect_err("stale 来源 Turn 必须被拒绝");
    assert_eq!(error.code, "stale_turn");

    fixture
        .coordinator
        .complete_turn(
            &child_agent_id,
            &child_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("取消后的过期成功".to_owned()),
            },
        )
        .expect("目标 Turn 应收敛为中断");
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &fixture.root_turn_id,
            AgentTurnOutcome::Completed {
                final_message: None,
            },
        )
        .expect("来源 Turn 应完成");
    let replayed_after_terminal = output_json(
        tool.execute(context, input)
            .await
            .expect("来源与目标终态后仍应重放首次 StopAgent 结果"),
    );
    assert_eq!(replayed_after_terminal, first);
}

/// send_message 只入队，不因目标空闲而启动新 Turn。
#[tokio::test]
async fn send_message_never_triggers_an_idle_agent_turn() {
    let fixture = fixture(2, 2);
    let child = spawn_with_tool(&fixture, "message_target").await;
    let child_agent_id = spawned_agent_id(&child);
    let child_turn_id = spawned_turn_id(&child);
    fixture
        .coordinator
        .complete_turn(
            &child_agent_id,
            &child_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("child done".to_owned()),
            },
        )
        .expect("子 Agent 初始 Turn 应完成");
    let send_tool = SendMessageTool::new(fixture.coordinator.clone());
    let queue_only = output_json(
        send_tool
            .execute(
                fixture.root_context(),
                json!({
                    "target_agent_id": child_agent_id.as_str(),
                    "message": "只排队"
                }),
            )
            .await
            .expect("QueueOnly 消息应成功"),
    );
    assert_eq!(queue_only["delivery"], "queue_only");
    assert!(queue_only.get("triggered_turn_id").is_none());
    assert!(matches!(
        fixture
            .coordinator
            .agent_status(&child_agent_id)
            .expect("QueueOnly 后状态应可读"),
        CollaborationAgentStatus::Completed { .. }
    ));
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child_agent_id)
            .expect("目标 mailbox 应可读取")
            .len(),
        1
    );
}

/// followup_task 唤醒空闲目标，运行中目标则只接收活动信号。
#[tokio::test]
async fn followup_task_triggers_idle_but_not_running_agent_turn() {
    let fixture = fixture(2, 2);
    let child = spawn_with_tool(&fixture, "followup_target").await;
    let child_agent_id = spawned_agent_id(&child);
    let child_turn_id = spawned_turn_id(&child);
    fixture
        .coordinator
        .complete_turn(
            &child_agent_id,
            &child_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("child done".to_owned()),
            },
        )
        .expect("子 Agent 初始 Turn 应完成");
    let followup_tool = FollowupTaskTool::new(fixture.coordinator.clone());

    let trigger_idle = output_json(
        followup_tool
            .execute(
                fixture.root_context(),
                json!({
                    "target_agent_id": child_agent_id.as_str(),
                    "message": "唤醒空闲目标"
                }),
            )
            .await
            .expect("TriggerTurn 应唤醒空闲 Agent"),
    );
    let triggered_turn_id = TurnId::new(
        trigger_idle["triggered_turn_id"]
            .as_str()
            .expect("空闲目标应返回新 Turn 标识"),
    )
    .expect("触发 Turn 标识应有效");
    assert_eq!(trigger_idle["delivery"], "trigger_turn");
    assert_eq!(
        fixture
            .coordinator
            .agent_status(&child_agent_id)
            .expect("唤醒后状态应可读"),
        CollaborationAgentStatus::Running {
            turn_id: triggered_turn_id
        }
    );

    let trigger_running = output_json(
        followup_tool
            .execute(
                fixture.root_context(),
                json!({
                    "target_agent_id": child_agent_id.as_str(),
                    "message": "运行中只发活动信号"
                }),
            )
            .await
            .expect("运行中目标应接收 TriggerTurn 消息"),
    );
    assert!(trigger_running["triggered_turn_id"].is_null());
    assert_eq!(
        fixture
            .coordinator
            .mailbox(&child_agent_id)
            .expect("目标 mailbox 应可读取")
            .len(),
        2
    );
}

/// list_agents 只公开身份和生命周期摘要，不泄露配置、工具或 mailbox 正文。
#[tokio::test]
async fn list_agents_excludes_profiles_tools_and_message_content() {
    let fixture = fixture(2, 2);
    let child = spawn_with_tool(&fixture, "listed_child").await;
    let child_agent_id = spawned_agent_id(&child);
    let secret = "list-agents-mailbox-secret-不得泄露";
    SendMessageTool::new(fixture.coordinator.clone())
        .execute(
            fixture.root_context(),
            json!({
                "target_agent_id": child_agent_id.as_str(),
                "message": secret
            }),
        )
        .await
        .expect("测试消息应进入子 Agent mailbox");

    let output = output_json(
        ListAgentsTool::new(fixture.coordinator.clone())
            .execute(fixture.root_context(), json!({}))
            .await
            .expect("list_agents 应返回同一根树摘要"),
    );
    let agents = output["agents"]
        .as_array()
        .expect("list_agents 应返回 Agent 数组");
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|agent| {
        agent["agent_id"] == fixture.root_agent_id.as_str() && agent["path"] == "/root"
    }));
    assert!(agents.iter().any(|agent| {
        agent["agent_id"] == child_agent_id.as_str() && agent["path"] == "/root/listed_child"
    }));
    for agent in agents {
        for forbidden in [
            "model",
            "reasoning_effort",
            "plan_guard",
            "cwd",
            "worktree_lease",
            "tool_snapshot",
            "messages",
            "mailbox",
        ] {
            assert!(
                agent.get(forbidden).is_none(),
                "list_agents 不得返回字段 {forbidden}"
            );
        }
    }
    let serialized = serde_json::to_string(&output).expect("列表摘要应可序列化");
    for secret_value in [secret, "model-root", "D:/workspace/root", "SendMessage"] {
        assert!(
            !serialized.contains(secret_value),
            "list_agents 不得泄露敏感配置或正文"
        );
    }
}

/// 所有协作工具都拒绝额外身份字段、超长 UTF-8 和越界数量。
#[tokio::test]
async fn inputs_enforce_strict_shape_utf8_bytes_and_numeric_limits() {
    let fixture = fixture(2, 2);
    let mut forged = agent_input("strict");
    forged
        .as_object_mut()
        .expect("Agent 输入应为对象")
        .insert("turn_id".to_owned(), json!(fixture.root_turn_id.as_str()));
    let error = spawn_tool(fixture.coordinator.clone(), profile("strict"))
        .execute(fixture.root_context(), forged)
        .await
        .expect_err("额外 Turn 字段必须被拒绝");
    assert_eq!(error.code, "invalid_input");

    let oversized_task = "界".repeat(MAX_INITIAL_TASK_BYTES / 3 + 1);
    let error = spawn_tool(fixture.coordinator.clone(), profile("large"))
        .execute(
            fixture.root_context(),
            json!({
                "task_name": "large",
                "message": oversized_task,
                "fork_turns": "none"
            }),
        )
        .await
        .expect_err("多字节初始任务必须按 UTF-8 字节拒绝");
    assert_eq!(error.code, "invalid_input");

    let oversized_target = "界".repeat(MAX_AGENT_ID_BYTES / 3 + 1);
    let error = InterruptAgentTool::new(fixture.coordinator.clone())
        .execute(
            fixture.root_context(),
            json!({ "target_agent_id": oversized_target }),
        )
        .await
        .expect_err("多字节目标标识必须按 UTF-8 字节拒绝");
    assert_eq!(error.code, "invalid_input");

    let oversized_message = "界".repeat(MAX_MESSAGE_BYTES / 3 + 1);
    let error = SendMessageTool::new(fixture.coordinator.clone())
        .execute(
            fixture.root_context(),
            json!({
                "target_agent_id": fixture.root_agent_id.as_str(),
                "message": oversized_message
            }),
        )
        .await
        .expect_err("多字节消息必须按 UTF-8 字节拒绝");
    assert_eq!(error.code, "invalid_input");

    let error = WaitAgentTool::new(fixture.coordinator.clone())
        .execute(
            fixture.root_context(),
            json!({ "timeout_ms": MAX_WAIT_TIMEOUT_MILLISECONDS + 1 }),
        )
        .await
        .expect_err("越界等待时长必须被拒绝");
    assert_eq!(error.code, "invalid_input");
}

/// 当前 Turn 已结束后，六类协作命令都不能凭旧 ToolContext 产生新副作用。
#[tokio::test]
async fn ended_parent_turn_rejects_all_new_collaboration_commands() {
    let fixture = fixture(2, 2);
    let child = spawn_with_tool(&fixture, "ended_target").await;
    let child_agent_id = spawned_agent_id(&child);
    fixture
        .coordinator
        .complete_turn(
            &fixture.root_agent_id,
            &fixture.root_turn_id,
            AgentTurnOutcome::Completed {
                final_message: Some("parent done".to_owned()),
            },
        )
        .expect("父 Turn 应完成");
    let stale_context = fixture.root_context();

    let agent_error = spawn_tool(fixture.coordinator.clone(), profile("late"))
        .execute(stale_context.clone(), agent_input("late"))
        .await
        .expect_err("已结束父 Turn 不得创建 Agent");
    let send_error = SendMessageTool::new(fixture.coordinator.clone())
        .execute(
            stale_context.clone(),
            json!({
                "target_agent_id": child_agent_id.as_str(),
                "message": "late"
            }),
        )
        .await
        .expect_err("已结束父 Turn 不得发送消息");
    let followup_error = FollowupTaskTool::new(fixture.coordinator.clone())
        .execute(
            stale_context.clone(),
            json!({
                "target_agent_id": child_agent_id.as_str(),
                "message": "late follow-up"
            }),
        )
        .await
        .expect_err("已结束父 Turn 不得触发后续任务");
    let stop_error = InterruptAgentTool::new(fixture.coordinator.clone())
        .execute(
            stale_context.clone(),
            json!({ "target_agent_id": child_agent_id.as_str() }),
        )
        .await
        .expect_err("已结束父 Turn 不得停止目标");
    let list_error = ListAgentsTool::new(fixture.coordinator.clone())
        .execute(stale_context.clone(), json!({}))
        .await
        .expect_err("已结束父 Turn 不得列出 Agent");
    let wait_error = WaitAgentTool::new(fixture.coordinator)
        .execute(stale_context, json!({ "timeout_ms": 0 }))
        .await
        .expect_err("已结束父 Turn 不得重新等待");
    for error in [
        agent_error,
        send_error,
        followup_error,
        stop_error,
        list_error,
        wait_error,
    ] {
        assert_eq!(error.code, "stale_turn");
        assert!(error.message.len() < 256, "安全错误必须保持有界");
    }
}
