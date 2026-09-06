//! 验证上下文压缩与项目级 Goal、Session 冷恢复之间的持久化边界。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keencode_agent::{
    AgentId, AgentRunner, ContextCompressionTrigger, ContextCompressor, ContextError,
    ContextFuture, ContextManager, ContextPolicy, ContextSummaryOutcome, ContextSummaryRequest,
    GoalController, GoalDraft, JsonContextTokenEstimator, PlanController, PlanGuard, RunLimits,
    TodoController, TodoItem, TodoStatus, ToolRegistry, TurnCancellation, TurnRequest,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelFuture, ModelProvider, ModelRequest,
    ModelStream, ModelStreamEvent, ProviderCapabilities, ResponseMetadata, ScriptedProvider,
    ScriptedReply, StopReason,
};
use keencode_resources::{
    AgentId as ResourceAgentId, COMPACTION_SUMMARY_PREFIX, GoalDocument, GoalFileStore,
    SubAgentState, SubAgentStatus, TurnId as ResourceTurnId, TurnStatus,
};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeSession, RuntimeTurnRequest,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::Notify;

/// 读取一个文本响应脚本，供真实 Agent Loop 和摘要 Prompt 检查复用。
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

/// 返回同一组已知模型能力，确保重开前后压缩规划输入完全一致。
fn compression_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        max_context_tokens: Some(4_096),
        max_output_tokens: Some(64),
        ..ProviderCapabilities::default()
    }
}

/// 返回主动压缩测试使用的确定性策略。
fn compression_policy() -> ContextPolicy {
    ContextPolicy {
        precompress_enabled: true,
        trigger_percent: 80,
        target_percent: 50,
        reserved_output_tokens: 64,
        forced_target_percent: 50,
        minimum_recent_units: 2,
        summary_max_output_tokens: 64,
    }
}

/// 保存摘要输入并始终返回同一条本地脚本摘要，不执行任何网络请求。
struct ScriptedCompressor {
    /// 用于确认压缩器收到的输入没有因冷重开发生变化。
    requests: Mutex<Vec<ContextSummaryRequest>>,
    /// 每次摘要调用返回的固定正文。
    summary: String,
}

impl ScriptedCompressor {
    /// 创建一个固定摘要和空请求记录。
    fn new(summary: impl Into<String>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            summary: summary.into(),
        }
    }

    /// 返回摘要调用输入的独立快照。
    fn requests(&self) -> Vec<ContextSummaryRequest> {
        self.requests.lock().expect("脚本摘要器锁不应中毒").clone()
    }
}

impl ContextCompressor for ScriptedCompressor {
    /// 记录输入并返回固定、可重复的摘要结果。
    fn summarize(
        &self,
        request: ContextSummaryRequest,
        _cancellation: TurnCancellation,
    ) -> ContextFuture<'_, Result<ContextSummaryOutcome, ContextError>> {
        self.requests
            .lock()
            .expect("脚本摘要器锁不应中毒")
            .push(request);
        let summary = self.summary.clone();
        Box::pin(async move { Ok(ContextSummaryOutcome::without_model_usage(summary)) })
    }
}

/// 在模型请求前等待通知，令测试可以观察到已经持久化的压缩和子 Agent。
struct GatedProvider {
    /// 放行模型请求的异步通知。
    gate: Arc<Notify>,
    /// 放行后返回固定响应并记录请求的脚本 Provider。
    inner: ScriptedProvider,
}

impl GatedProvider {
    /// 创建等待指定通知后返回固定脚本的 Provider。
    fn new(gate: Arc<Notify>, capabilities: ProviderCapabilities, reply: ScriptedReply) -> Self {
        Self {
            gate,
            inner: ScriptedProvider::new(capabilities, [reply]),
        }
    }

    /// 返回本地模拟 Provider 的实际模型请求次数。
    fn request_count(&self) -> usize {
        self.inner
            .requests()
            .expect("脚本 Provider 请求记录应可读取")
            .len()
    }
}

impl ModelProvider for GatedProvider {
    /// 返回脚本 Provider 的固定能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.inner.capabilities(model)
    }

    /// 等待放行后再进入脚本模型请求边界。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            gate.notified().await;
            self.inner.stream(request).await
        })
    }
}

/// 创建一个隔离的 Runtime Session。
fn create_session(root: &TempDir, session_id: &str) -> RuntimeSession {
    RuntimeSession::create_session(
        RuntimeConfig::new(root.path()),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "压缩冷恢复测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("Runtime Session 应创建成功")
}

/// 从资源层读取当前根 Agent 的有效模型消息。
fn root_transcript(session: &RuntimeSession) -> Vec<Message> {
    session
        .model_transcript_for_agent(&ResourceAgentId::new("root").expect("资源根 Agent 标识应有效"))
        .expect("根 Agent Transcript 应可读取")
}

/// 创建绑定当前 Runtime Session 的根 Turn 请求。
fn root_turn_request(
    session: &RuntimeSession,
    turn_id: &str,
    messages: Vec<Message>,
    input: Message,
    prompt_summary: &str,
) -> RuntimeTurnRequest {
    RuntimeTurnRequest::root(
        TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session 标识应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn 标识应有效"),
            AgentId::new("root").expect("Agent 根标识应有效"),
            "test-model",
            messages,
            PlanGuard::inactive(),
        ),
        vec![input],
        prompt_summary,
    )
}

/// 使用无压缩的脚本 Provider 写入一轮用户和助手历史。
async fn run_seed_turn(session: &RuntimeSession, turn_id: &str, prompt: String, response: &str) {
    let input = Message::text(MessageRole::User, prompt.clone());
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_reply(response)],
    ));
    let runner = session.bind_agent_runner(AgentRunner::new(
        provider,
        ToolRegistry::new(),
        RunLimits::new(4, 4).expect("运行限制应有效"),
    ));
    let result = runner
        .run_turn(root_turn_request(
            session,
            turn_id,
            vec![input.clone()],
            input,
            &prompt,
        ))
        .await
        .expect("历史 Turn 应完成权威提交");
    assert!(result.is_success(), "历史 Turn 应成功完成");
}

/// 写入两轮足以形成可压缩前缀、且重开后保持不变的历史 Transcript。
async fn seed_history(session: &RuntimeSession) {
    run_seed_turn(
        session,
        "seed-old",
        format!("旧历史；用户约束：不得调用真实网络；{}", "x".repeat(14_000)),
        "旧历史的助手结果",
    )
    .await;
    run_seed_turn(
        session,
        "seed-recent",
        "保留的近期用户问题".to_owned(),
        "保留的近期助手结果",
    )
    .await;
}

/// 等待资源快照观察到一个已经应用的压缩记录。
async fn wait_until_compacted(session: &RuntimeSession) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count = session
                .snapshot()
                .expect("Runtime 快照应可读取")
                .state
                .applied_compactions()
                .count();
            if count == 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("压缩应在测试超时前提交");
}

/// 等待指定根或子 Agent Turn 进入 Runtime Journal 的 Running 状态。
async fn wait_until_turn_running(session: &RuntimeSession, turn_id: &ResourceTurnId) {
    tokio::time::timeout(Duration::from_secs(3), async {
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
    .expect("Turn 应在测试超时前进入 Running");
}

/// 等待指定子 Agent Turn 进入 Running 状态。
async fn wait_until_child_running(session: &RuntimeSession, child_id: &ResourceAgentId) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let running = session
                .snapshot()
                .expect("Runtime 快照应可读取")
                .state
                .sub_agents
                .get(child_id)
                .is_some_and(|agent| {
                    agent.status == SubAgentStatus::Running && agent.current_turn_id.is_some()
                });
            if running {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("子 Agent 应在测试超时前进入 Running");
}

/// 记录 Goal 文件的真实路径、文件 SHA-256 和独立 GoalFileStore 读取结果。
#[derive(Debug, PartialEq, Eq)]
struct GoalFileEvidence {
    /// GoalFileStore 根据 ScopeId 生成的 JSON 文件路径。
    path: PathBuf,
    /// 文件原始字节（含换行）的 SHA-256。
    sha256: String,
    /// 从独立 GoalFileStore 读取并验证后的文档。
    read: Option<GoalDocument>,
}

/// 读取项目 Goal 文件并生成可跨压缩、重开比较的证据快照。
fn goal_file_evidence(root: &Path, scope: &keencode_resources::ScopeId) -> GoalFileEvidence {
    let path = root.join("goals").join(format!("{}.json", scope.as_str()));
    let bytes = fs::read(&path).expect("Goal 文件应存在");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = format!("{:x}", hasher.finalize());
    let read = GoalFileStore::open(root)
        .expect("独立 GoalFileStore 应打开")
        .read(scope)
        .expect("Goal 文件应可读取");
    GoalFileEvidence { path, sha256, read }
}

/// 验证生产摘要器发送的 Developer 指令包含摘要契约。
///
/// `ScriptedProvider` 只在本地模拟 Provider 并捕获请求，不证明真实模型会遵守这些语义。
async fn assert_summary_prompt_contract_with_scripted_provider() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_reply("本地摘要")],
    ));
    let compressor = keencode_agent::ProviderContextCompressor::new(provider.clone());
    compressor
        .summarize(
            ContextSummaryRequest {
                model: "test-model".to_owned(),
                messages: vec![Message::text(MessageRole::User, "目标和约束")],
                max_output_tokens: 64,
            },
            TurnCancellation::new(),
        )
        .await
        .expect("生产摘要器应可通过本地模拟 Provider 执行");
    let request = provider
        .requests()
        .expect("脚本 Provider 请求记录应可读取")
        .into_iter()
        .next()
        .expect("生产摘要器应发起一条请求");
    let instruction = request
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Developer)
        .and_then(|message| {
            message.content.iter().find_map(|content| match content {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
        .expect("生产摘要请求应包含 Developer 指令");
    assert!(instruction.contains("保留已确认的目标、约束"));
    assert!(instruction.contains("关键事实"));
    assert!(instruction.contains("尚未完成事项"));
}

/// 创建 Goal、Todo、Plan 和子 Agent，验证压缩只改变有效 Transcript 且冷恢复保持各状态。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_todo_and_child_survive_compaction_snapshot_and_cold_reopen() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "goal-compaction-cold-recovery";
    let session = create_session(&root, session_id);
    seed_history(&session).await;

    let persistent = keencode_runtime::PersistentAgentState::open(session.clone())
        .expect("持久状态控制器应打开");
    let goal_change = persistent
        .create_goal(
            "goal-create",
            GoalDraft {
                title: "压缩恢复目标".to_owned(),
                objective: "保留用户目标、约束和未完成事项，完成冷重开验证".to_owned(),
                description: Some("不得调用真实网络；必须保留用户约束".to_owned()),
                token_budget: Some(10_000),
                progress_percent: Some(25),
            },
        )
        .expect("active Goal 应创建");
    let goal_before = goal_change.current.clone();
    let todo_before = persistent
        .replace_todos(
            "todo-create",
            vec![TodoItem {
                content: "验证压缩后仍保留用户约束".to_owned(),
                status: TodoStatus::InProgress,
                active_form: "正在验证压缩后仍保留用户约束".to_owned(),
            }],
        )
        .expect("Todo 应写入")
        .current;
    let scope = persistent.project_scope().clone();
    let goal_evidence_before = goal_file_evidence(root.path(), &scope);
    assert!(goal_evidence_before.path.is_file());
    let stored_goal = goal_evidence_before
        .read
        .as_ref()
        .and_then(|document| document.goal.as_ref())
        .expect("Goal 文件读取结果应包含当前 Goal");
    assert_eq!(stored_goal.title, "压缩恢复目标");
    assert_eq!(
        stored_goal.objective,
        "保留用户目标、约束和未完成事项，完成冷重开验证"
    );
    assert_eq!(
        persistent.goal_snapshot().expect("Goal 快照应可读取"),
        goal_before
    );

    let plan_session_id =
        keencode_agent::SessionId::new(session_id).expect("Plan Session 标识应有效");
    let plan_agent = AgentId::new("root").expect("Plan 根 Agent 标识应有效");
    let plan_before = persistent
        .replace_plan(
            "plan-create",
            &plan_session_id,
            &plan_agent,
            "保留目标、约束、Todo 与子 Agent 状态".to_owned(),
        )
        .expect("Plan 应写入")
        .current;
    let runtime_before = session
        .snapshot()
        .expect("Plan 与 Todo 写入后的 Runtime Snapshot 应可读取");
    let runtime_plan_before = runtime_before.state.plan.clone();
    let runtime_todo_before = runtime_before.state.todos.clone();

    // 先让父 Turn 和首次子 Agent Turn 同时运行，确保子 Agent 在压缩前已经存在且可继续执行。
    let parent_turn_id = "goal-parent-turn";
    let parent_input = Message::text(MessageRole::User, "保持父 Turn 运行并完成目标。");
    let mut parent_messages = root_transcript(&session);
    parent_messages.push(parent_input.clone());
    let parent_gate = Arc::new(Notify::new());
    let parent_provider = Arc::new(GatedProvider::new(
        parent_gate.clone(),
        ProviderCapabilities::default(),
        completed_reply("父根任务完成"),
    ));
    let parent_runner = session.bind_agent_runner(AgentRunner::new(
        parent_provider.clone(),
        ToolRegistry::new(),
        RunLimits::new(4, 4).expect("父 Turn 运行限制应有效"),
    ));
    let parent_session = session.clone();
    let parent_task = tokio::spawn(async move {
        parent_runner
            .run_turn(root_turn_request(
                &parent_session,
                parent_turn_id,
                parent_messages,
                parent_input,
                "保持父 Turn 运行",
            ))
            .await
    });
    wait_until_turn_running(
        &session,
        &ResourceTurnId::new(parent_turn_id).expect("父 Turn 标识应有效"),
    )
    .await;

    let child_agent = AgentId::new("goal-child").expect("子 Agent 标识应有效");
    let child_resource_agent =
        ResourceAgentId::new(child_agent.as_str()).expect("资源子 Agent 标识应有效");
    let child_turn_id = keencode_agent::TurnId::new("goal-child-turn").expect("子 Turn 标识应有效");
    let child_input = Message::text(MessageRole::User, "验证 Goal 与压缩状态");
    let child_state = SubAgentState {
        agent_id: child_resource_agent.clone(),
        parent_agent_id: ResourceAgentId::new("root").expect("资源父 Agent 标识应有效"),
        agent_path: "/root/goal_child".to_owned(),
        task: "验证 Goal 与压缩状态".to_owned(),
        status: SubAgentStatus::Pending,
        current_turn_id: None,
        result_summary: None,
    };
    let child_gate = Arc::new(Notify::new());
    let child_provider = Arc::new(GatedProvider::new(
        child_gate.clone(),
        ProviderCapabilities::default(),
        completed_reply("子 Agent 完成"),
    ));
    let child_runner = session.bind_agent_runner(AgentRunner::new(
        child_provider,
        ToolRegistry::new(),
        RunLimits::new(4, 4).expect("子 Agent 运行限制应有效"),
    ));
    let child_session = session.clone();
    let child_task = tokio::spawn(async move {
        child_runner
            .run_turn(RuntimeTurnRequest::initial_child(
                TurnRequest::new(
                    keencode_agent::SessionId::new(child_session.session_id().as_str())
                        .expect("子 Agent Session 标识应有效"),
                    child_turn_id,
                    child_agent,
                    "test-model",
                    vec![child_input.clone()],
                    PlanGuard::inactive(),
                ),
                vec![child_input],
                parent_turn_id,
                parent_turn_id,
                "验证 Goal 与压缩状态",
                child_state,
            ))
            .await
    });
    wait_until_child_running(&session, &child_resource_agent).await;

    parent_gate.notify_one();
    let parent_result = parent_task
        .await
        .expect("父 Agent 任务不应 panic")
        .expect("父 Agent 终态应可提交");
    assert!(parent_result.is_success());
    assert_eq!(parent_provider.request_count(), 1);
    assert_eq!(
        session
            .snapshot()
            .expect("父 Turn 完成后的 Runtime Snapshot 应可读取")
            .state
            .sub_agents
            .get(&child_resource_agent)
            .expect("子 Agent 状态应存在")
            .status,
        SubAgentStatus::Running
    );

    // 第二个根 Turn 负责触发压缩；其 Provider 被挂起，便于在压缩提交后读取一致快照。
    let compaction_turn_id = "goal-compaction-root-turn";
    let current_input = Message::text(MessageRole::User, "继续任务并输出恢复结果。");
    let mut model_messages = root_transcript(&session);
    model_messages.push(current_input.clone());
    let compaction_gate = Arc::new(Notify::new());
    let compaction_provider = Arc::new(GatedProvider::new(
        compaction_gate.clone(),
        compression_capabilities(),
        completed_reply("压缩根任务完成"),
    ));
    let compressor = Arc::new(ScriptedCompressor::new(
        "摘要保留用户约束：不得调用真实网络；保留目标、约束和未完成事项。",
    ));
    let context = ContextManager::new(
        compression_policy(),
        Arc::new(JsonContextTokenEstimator),
        compressor.clone(),
    )
    .expect("确定性上下文策略应有效");
    let compaction_runner = session.bind_agent_runner(
        AgentRunner::new(
            compaction_provider.clone(),
            ToolRegistry::new(),
            RunLimits::new(4, 4).expect("压缩 Turn 运行限制应有效"),
        )
        .with_context_manager(context),
    );
    let compaction_session = session.clone();
    let compaction_task = tokio::spawn(async move {
        compaction_runner
            .run_turn(root_turn_request(
                &compaction_session,
                compaction_turn_id,
                model_messages,
                current_input,
                "继续压缩恢复目标",
            ))
            .await
    });
    wait_until_compacted(&session).await;
    assert_eq!(compressor.requests().len(), 1);

    let snapshot_during_compaction = session
        .snapshot()
        .expect("压缩提交后 Runtime Snapshot 应可读取");
    let child_during_compaction = snapshot_during_compaction
        .state
        .sub_agents
        .get(&child_resource_agent)
        .expect("压缩期间子 Agent 状态应存在")
        .clone();
    assert_eq!(child_during_compaction.status, SubAgentStatus::Running);
    assert!(child_during_compaction.current_turn_id.is_some());
    assert_eq!(snapshot_during_compaction.state.plan, runtime_plan_before);
    assert_eq!(snapshot_during_compaction.state.todos, runtime_todo_before);
    assert_eq!(
        persistent
            .plan_snapshot(&plan_session_id, &plan_agent)
            .expect("压缩期间 Plan 快照应可读取"),
        plan_before
    );
    assert_eq!(
        persistent.todo_snapshot().expect("压缩期间 Todo 应可读取"),
        todo_before
    );
    assert_eq!(
        persistent.goal_snapshot().expect("压缩期间 Goal 应可读取"),
        goal_before
    );

    child_gate.notify_one();
    let child_result = child_task
        .await
        .expect("子 Agent 任务不应 panic")
        .expect("子 Agent 终态应可提交");
    assert!(child_result.is_success());

    compaction_gate.notify_one();
    let compaction_result = compaction_task
        .await
        .expect("压缩根 Agent 任务不应 panic")
        .expect("压缩根 Agent 终态应可提交");
    assert!(compaction_result.is_success());
    assert_eq!(compaction_result.compactions.len(), 1);
    assert_eq!(compaction_provider.request_count(), 1);

    let snapshot_after = session
        .snapshot()
        .expect("压缩后 Runtime Snapshot 应可读取");
    let child_after = snapshot_after
        .state
        .sub_agents
        .get(&child_resource_agent)
        .expect("子 Agent 状态应存在")
        .clone();
    assert_eq!(child_after.status, SubAgentStatus::Completed);
    assert_eq!(snapshot_after.state.plan, runtime_plan_before);
    assert_eq!(snapshot_after.state.todos, runtime_todo_before);
    assert_eq!(
        persistent.todo_snapshot().expect("压缩后 Todo 应可读取"),
        todo_before
    );
    let effective_after = session
        .model_transcript_for_agent(&ResourceAgentId::new("root").expect("资源根 Agent 标识应有效"))
        .expect("压缩后有效 Transcript 应可读取");
    assert!(effective_after.iter().any(|message| {
        message.content.iter().any(|content| {
            matches!(content, ContentBlock::Text { text } if text.starts_with(COMPACTION_SUMMARY_PREFIX) && text.contains("不得调用真实网络"))
        })
    }));
    assert_eq!(
        persistent.goal_snapshot().expect("压缩后 Goal 应可读取"),
        goal_before
    );
    assert_eq!(
        persistent
            .plan_snapshot(&plan_session_id, &plan_agent)
            .expect("压缩后 Plan 快照应可读取"),
        plan_before
    );
    let goal_evidence_after = goal_file_evidence(root.path(), &scope);
    assert_eq!(goal_evidence_after, goal_evidence_before);

    drop(persistent);
    drop(session);
    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("Session 应可冷重开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => panic!("Session 不应损坏：{report:?}"),
    };
    let reopened_snapshot = reopened.snapshot().expect("重开 Snapshot 应可读取");
    assert_eq!(reopened_snapshot.state, snapshot_after.state);
    assert_eq!(
        reopened_snapshot
            .state
            .sub_agents
            .get(&child_resource_agent),
        Some(&child_after)
    );
    let reopened_persistent = keencode_runtime::PersistentAgentState::open(reopened.clone())
        .expect("重开后的持久状态控制器应打开");
    assert_eq!(
        reopened_persistent
            .todo_snapshot()
            .expect("重开后的 Todo 应可读取"),
        todo_before
    );
    assert_eq!(
        reopened_persistent
            .goal_snapshot()
            .expect("重开后的 Goal 应可读取"),
        goal_before
    );
    assert_eq!(
        reopened_persistent
            .plan_snapshot(&plan_session_id, &plan_agent)
            .expect("重开后的 Plan 快照应可读取"),
        plan_before
    );
    assert_eq!(
        goal_file_evidence(root.path(), &scope),
        goal_evidence_before
    );
    assert_summary_prompt_contract_with_scripted_provider().await;
}

/// 在同一有效 Transcript 和能力快照下，比较冷重开前后的压缩计划与资源 Digest。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_reopen_replans_compaction_deterministically_without_network_retry() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "compaction-replan-determinism";
    let session = create_session(&root, session_id);
    seed_history(&session).await;
    let root_agent = ResourceAgentId::new("root").expect("资源根 Agent 标识应有效");
    let before_state = session.snapshot().expect("重开前 Snapshot 应读取").state;
    let before_messages = root_transcript(&session);
    assert_eq!(before_messages.len(), 4);
    let request_before = ModelRequest::new("test-model", before_messages.clone());
    let capabilities = compression_capabilities();
    let turn_id = ResourceTurnId::new("seed-old").expect("Digest Turn 标识应有效");

    let compressor_before = Arc::new(ScriptedCompressor::new(
        "确定性摘要：保留目标、约束和未完成事项。",
    ));
    let manager_before = ContextManager::new(
        compression_policy(),
        Arc::new(JsonContextTokenEstimator),
        compressor_before.clone(),
    )
    .expect("重开前上下文策略应有效");
    let target_before = manager_before
        .precompression_target(&request_before, &capabilities)
        .expect("重开前应规划主动压缩");
    let outcome_before = manager_before
        .compact_with_capabilities(
            &request_before,
            ContextCompressionTrigger::Budget,
            target_before,
            &capabilities,
            &TurnCancellation::new(),
        )
        .await
        .expect("重开前压缩应成功");
    let resource_digest_before = before_state
        .compaction_source_digest_sha256(
            &turn_id,
            &root_agent,
            1,
            outcome_before.record.replaced_start_index,
            outcome_before.record.replaced_end_index_exclusive,
        )
        .expect("重开前资源 Digest 应可计算");
    assert_eq!(compressor_before.requests().len(), 1);

    drop(session);
    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("Session 应可冷重开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => panic!("Session 不应损坏：{report:?}"),
    };
    let after_state = reopened.snapshot().expect("重开后 Snapshot 应读取").state;
    let after_messages = root_transcript(&reopened);
    assert_eq!(after_messages, before_messages);
    assert_eq!(after_state, before_state);
    let request_after = ModelRequest::new("test-model", after_messages);
    let compressor_after = Arc::new(ScriptedCompressor::new(
        "确定性摘要：保留目标、约束和未完成事项。",
    ));
    let manager_after = ContextManager::new(
        compression_policy(),
        Arc::new(JsonContextTokenEstimator),
        compressor_after.clone(),
    )
    .expect("重开后上下文策略应有效");
    let target_after = manager_after
        .precompression_target(&request_after, &capabilities)
        .expect("重开后应规划主动压缩");
    assert_eq!(target_after, target_before);
    let outcome_after = manager_after
        .compact_with_capabilities(
            &request_after,
            ContextCompressionTrigger::Budget,
            target_after,
            &capabilities,
            &TurnCancellation::new(),
        )
        .await
        .expect("重开后压缩应成功");
    let resource_digest_after = after_state
        .compaction_source_digest_sha256(
            &turn_id,
            &root_agent,
            1,
            outcome_after.record.replaced_start_index,
            outcome_after.record.replaced_end_index_exclusive,
        )
        .expect("重开后资源 Digest 应可计算");

    assert_eq!(
        (
            outcome_before.record.replaced_start_index,
            outcome_before.record.replaced_end_index_exclusive,
            outcome_before.record.replaced_message_count,
            outcome_before.record.retained_message_count,
            outcome_before.record.estimated_tokens_before,
            outcome_before.record.estimated_tokens_after,
            outcome_before.record.source_digest_sha256.clone(),
        ),
        (
            outcome_after.record.replaced_start_index,
            outcome_after.record.replaced_end_index_exclusive,
            outcome_after.record.replaced_message_count,
            outcome_after.record.retained_message_count,
            outcome_after.record.estimated_tokens_before,
            outcome_after.record.estimated_tokens_after,
            outcome_after.record.source_digest_sha256.clone(),
        )
    );
    assert_eq!(resource_digest_after, resource_digest_before);
    assert_eq!(
        resource_digest_before,
        before_state
            .compaction_source_digest_sha256(
                &turn_id,
                &root_agent,
                1,
                outcome_after.record.replaced_start_index,
                outcome_after.record.replaced_end_index_exclusive,
            )
            .expect("相同资源 Transcript 的 Digest 应可重复计算")
    );
    assert_eq!(compressor_after.requests().len(), 1);
}
