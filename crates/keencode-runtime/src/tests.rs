use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use base64::Engine as _;
use keencode_agent::{
    AgentCommitEvent, AgentCommitEventKind, AgentCommitSink, AgentCommitSinkError,
    AgentDynamicInputAcknowledgement, AgentDynamicInputBatch, AgentDynamicInputBoundary,
    AgentDynamicInputError, AgentDynamicInputKind, AgentDynamicInputReceipt,
    AgentDynamicInputSource, AgentEventFuture, AgentEventSink, AgentRunner, AgentStreamEvent,
    AgentTool, AgentToolRoundPreflight, AgentToolRoundPreflightError,
    AgentToolRoundPreflightErrorKind, AgentToolRoundReservation, ModelRoundUsage, PlanGuard,
    RunLimits, TOOL_OUTPUT_LIMITS, TerminalReason, ToolConcurrency, ToolContext,
    ToolEffect as AgentToolEffect, ToolError, ToolFuture, ToolOutput, ToolRegistry, TurnRequest,
};
use keencode_model::{
    ContentBlock, ImageContent, ImageSource, Message, MessageRole as ModelMessageRole, ModelError,
    ModelStreamEvent, ProviderCapabilities, ReasoningContent, ResponseMetadata, ScriptedProvider,
    ScriptedReply, StopReason, TokenUsage, ToolCall, ToolDefinition, ToolResult,
};
use keencode_resources::{
    AgentId, ArtifactMaterialization, ArtifactStore, MailboxMessage, MailboxMessageId,
    MailboxState, MessageImageSource, MessagePart, PlanState, SessionEvent, SessionEventId,
    SessionEventRecord, SessionId, SessionMessage, SessionStatus, SnapshotPolicy, SubAgentStatus,
    TerminalRecord, ToolCompletionStatus, ToolEffect, ToolRequest, ToolResultPart,
    TranscriptRecord, TurnStatus, TurnStopReason,
};
use tempfile::TempDir;
use tokio::sync::Notify;

use super::{
    ArtifactMode, ArtifactProbe, ControlState, CreateSessionRequest, MAX_PERSISTED_IMAGE_URL_BYTES,
    MAX_RUNTIME_TERMINAL_MESSAGE_BYTES, OpenSessionResult, RoundKey, RuntimeCatchUpDirective,
    RuntimeConfig, RuntimeControlEvent, RuntimeError, RuntimeEventPayload,
    RuntimeEventReceiveError, RuntimeManager, RuntimeModelRoundUsageSink, RuntimeSession,
    RuntimeTurnRequest, StateCollectionItems, TurnCancellationOutcome, UnstartedTurnTermination,
    UnstartedTurnTerminationOutcome, UnstartedTurnTerminationRequest, append_resource_event,
    append_runtime_resource_event, charge_confirmed_reservation_event,
    charge_materialized_reservation_artifacts, commit_agent_event, encoded_record_len,
    ensure_commit_capacity, inject_runtime_input_commit_fault, inject_runtime_input_commit_faults,
    inject_runtime_lifecycle_failure, inject_runtime_lifecycle_indeterminate,
    inject_runtime_lifecycle_visible_indeterminate, journal_len, map_image_source, map_message,
    map_tool_result, mark_event_confirmed, mark_event_indeterminate, materialized_probe_artifacts,
    preflight_round, preflight_round_candidate, recovery_event_id, recovery_gate_allows_event,
    recovery_turn_stopped_event, refresh_recovery_required, runtime_lifecycle_event_id,
};

/// 创建使用资源层默认限制的隔离 Runtime 配置。
fn config(root: &TempDir) -> RuntimeConfig {
    RuntimeConfig::new(root.path())
}

/// 创建指定标识的最小有效 Session。
fn create(root: &TempDir, session_id: &str) -> RuntimeSession {
    RuntimeSession::create_session(
        config(root),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "Runtime 测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("全新 Session 应创建")
}

/// 创建供 RuntimeManager 使用的最小有效 Session 请求。
fn manager_create_request(root: &TempDir, session_id: &str) -> CreateSessionRequest {
    CreateSessionRequest {
        session_id: session_id.to_owned(),
        title: "Runtime Manager 测试".to_owned(),
        project_root: root.path().display().to_string(),
    }
}

/// 收集当前已可达的 Runtime 投递，短暂无事件后结束且不关闭 Session。
async fn drain_runtime_events(
    subscription: &mut super::RuntimeEventSubscription,
) -> Vec<super::RuntimeEventDelivery> {
    let mut deliveries = Vec::new();
    while let Ok(result) =
        tokio::time::timeout(Duration::from_millis(50), subscription.recv()).await
    {
        match result {
            Ok(delivery) => deliveries.push(delivery),
            Err(RuntimeEventReceiveError::Lagged(_)) => continue,
            Err(RuntimeEventReceiveError::Closed) => break,
        }
    }
    deliveries
}

/// 追加测试资源事件并使用稳定的显式事件标识。
fn append(session: &RuntimeSession, event_id: &str, event: SessionEvent) {
    append_resource_event(
        &session.inner.journal,
        SessionEventId::new(event_id).expect("测试事件 ID 应有效"),
        event,
    )
    .expect("测试事件应提交");
}

/// 为容量预检创建一个正在运行的根 Turn，并返回对应 Round 身份。
fn start_turn(session: &RuntimeSession, turn_id: &str, segment_index: u32) -> RoundKey {
    let turn_id_value = keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    append(
        session,
        &format!("event-start-{turn_id}"),
        SessionEvent::TurnStarted {
            turn_id: turn_id_value,
            source_agent_id: agent_id,
            root_turn_id: keencode_resources::TurnId::new(turn_id).expect("根 Turn ID 应有效"),
            parent_turn_id: None,
            prompt_summary: "工具容量预检".to_owned(),
        },
    );
    RoundKey {
        session_id: session.session_id().as_str().to_owned(),
        turn_id: turn_id.to_owned(),
        agent_id: "root".to_owned(),
        model: "test-model".to_owned(),
        model_round: 1,
        segment_index,
    }
}

/// 创建包含指定数量唯一工具调用的最终 Assistant 消息。
fn assistant_with_tool_calls(count: usize) -> Message {
    Message::new(
        ModelMessageRole::Assistant,
        (0..count)
            .map(|index| ContentBlock::ToolCall {
                tool_call: ToolCall::new(
                    format!("call-{index}"),
                    "Read",
                    serde_json::json!({"path": format!("file-{index}.rs")}),
                ),
            })
            .collect(),
    )
}

/// 创建只包含一个状态变更工具调用的脚本化模型响应。
fn state_changing_tool_reply() -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: "call-runtime-write".to_owned(),
            name: "RuntimeWrite".to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: "call-runtime-write".to_owned(),
            delta: serde_json::json!({"path": "x"}).to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: "call-runtime-write".to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ])
}

/// 创建一个正常完成且只返回单段文本的脚本化模型响应。
fn completed_text_reply(text: &str) -> ScriptedReply {
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

/// 模型输出上限和拒答必须贯穿真实 Runner 与 Journal，根和子 Turn 均不得报告完成。
#[tokio::test]
async fn model_stop_reasons_survive_runtime_and_cold_replay() {
    for (index, (model_reason, terminal_reason, resource_reason)) in [
        (
            StopReason::MaxOutputTokens,
            TerminalReason::ModelOutputLimit,
            TurnStopReason::ModelOutputLimit,
        ),
        (
            StopReason::ContentFilter,
            TerminalReason::ModelRefusal,
            TurnStopReason::ModelRefusal,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for child in [false, true] {
            let root = TempDir::new().expect("临时目录应创建");
            let session_id = format!("model-stop-{index}-{child}");
            let session = create(&root, &session_id);
            if child {
                register_pending_child(&session, "parent-turn", "child-stop");
            }
            let provider = Arc::new(ScriptedProvider::new(
                ProviderCapabilities {
                    streaming: true,
                    ..ProviderCapabilities::default()
                },
                [ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::TextDelta {
                        index: 0,
                        delta: "不能作为完整任务结果".to_owned(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: model_reason.clone(),
                    },
                ])],
            ));
            let bound = session.bind_agent_runner(AgentRunner::new(
                provider.clone(),
                ToolRegistry::new(),
                RunLimits::default(),
            ));
            let request = if child {
                child_runtime_turn(
                    &session,
                    "stopped-turn",
                    "child-stop",
                    "parent-turn",
                    "parent-turn",
                    "验证子模型终态",
                )
            } else {
                root_runtime_turn(&session, "stopped-turn", "验证根模型终态")
            };
            let result = bound
                .run_turn(request)
                .await
                .expect("Runtime 应提交类型化模型停止");
            assert_eq!(result.state.terminal_reason(), Some(terminal_reason));
            assert!(!result.is_success());
            assert_eq!(provider.requests().expect("请求应读取").len(), 1);
            let turn_id = keencode_resources::TurnId::new("stopped-turn").unwrap();
            let snapshot = session.snapshot().expect("热状态应读取");
            assert_eq!(snapshot.state.turns[&turn_id].status, TurnStatus::Failed);
            assert_eq!(
                snapshot.state.turns[&turn_id].stop_reason,
                Some(resource_reason)
            );
            if child {
                assert_eq!(
                    snapshot.state.sub_agents[&AgentId::new("child-stop").unwrap()].status,
                    SubAgentStatus::Failed
                );
            }
            drop(bound);
            drop(session);
            let OpenSessionResult::Ready(reopened) =
                RuntimeSession::open_session(config(&root), &session_id).expect("停止原因应冷恢复")
            else {
                panic!("Journal 不应损坏")
            };
            let cold = reopened.snapshot().expect("冷状态应读取");
            assert_eq!(cold.state.turns[&turn_id].status, TurnStatus::Failed);
            assert_eq!(
                cold.state.turns[&turn_id].stop_reason,
                Some(resource_reason)
            );
            if child {
                assert_eq!(
                    cold.state.sub_agents[&AgentId::new("child-stop").unwrap()].status,
                    SubAgentStatus::Failed
                );
            }
        }
    }
}

/// 创建携带指定响应元数据与用量快照的正常完成脚本响应。
fn completed_text_reply_with_facts(
    text: &str,
    metadata: ResponseMetadata,
    usage: Option<TokenUsage>,
) -> ScriptedReply {
    let mut events = vec![
        ModelStreamEvent::MessageStart { metadata },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
    ];
    if let Some(usage) = usage {
        events.push(ModelStreamEvent::Usage { usage });
    }
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::Completed,
    });
    ScriptedReply::events(events)
}

/// 创建携带指定响应元数据与用量快照的状态变更工具脚本响应。
fn state_changing_tool_reply_with_facts(
    metadata: ResponseMetadata,
    usage: TokenUsage,
) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart { metadata },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: "call-runtime-write-facts".to_owned(),
            name: "RuntimeWrite".to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: "call-runtime-write-facts".to_owned(),
            delta: serde_json::json!({"path": "facts"}).to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: "call-runtime-write-facts".to_owned(),
        },
        ModelStreamEvent::Usage { usage },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ])
}

/// 从当前 Session 权威状态生成完全一致的 Agent 计划守卫。
fn current_plan_guard(session: &RuntimeSession) -> PlanGuard {
    let state = session.snapshot().expect("Session 模式应读取").state;
    if state.plan.enabled {
        PlanGuard::read_only()
    } else {
        PlanGuard::inactive()
    }
}

/// 创建绑定当前 Session 的根 Agent Runtime Turn 请求。
fn root_runtime_turn(session: &RuntimeSession, turn_id: &str, prompt: &str) -> RuntimeTurnRequest {
    let input = Message::text(ModelMessageRole::User, prompt);
    let plan = current_plan_guard(session);
    RuntimeTurnRequest::root(
        TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
            "test-model",
            vec![input.clone()],
            plan,
        ),
        vec![input],
        prompt,
    )
}

/// 创建带单个内联图片输入且与当前 Session 模式一致的根 Runtime Turn。
fn image_runtime_turn(
    session: &RuntimeSession,
    turn_id: &str,
    base64: &str,
    prompt: &str,
) -> RuntimeTurnRequest {
    let input = Message::new(
        ModelMessageRole::User,
        vec![ContentBlock::Image {
            image: ImageContent::from_base64("image/png", base64),
        }],
    );
    let plan = current_plan_guard(session);
    RuntimeTurnRequest::root(
        TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
            "test-model",
            vec![input.clone()],
            plan,
        ),
        vec![input],
        prompt,
    )
}

/// 创建绑定当前 Session 和既有谱系的单层子 Agent Runtime Turn 请求。
fn child_runtime_turn(
    session: &RuntimeSession,
    turn_id: &str,
    child_agent_id: &str,
    root_turn_id: &str,
    parent_turn_id: &str,
    prompt: &str,
) -> RuntimeTurnRequest {
    let input = Message::text(ModelMessageRole::User, prompt);
    let plan = current_plan_guard(session);
    RuntimeTurnRequest::child(
        TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new(turn_id).expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new(child_agent_id).expect("Agent 子身份应有效"),
            "test-model",
            vec![input.clone()],
            plan,
        ),
        vec![input],
        root_turn_id,
        parent_turn_id,
        prompt,
    )
}

/// 为 Runtime 子 Turn 测试持久创建根 Turn 和 Pending 子 Agent。
fn register_pending_child(session: &RuntimeSession, root_turn_id: &str, child_agent_id: &str) {
    let root_turn = keencode_resources::TurnId::new(root_turn_id).expect("根 Turn ID 应有效");
    append(
        session,
        &format!("event-root-{root_turn_id}"),
        SessionEvent::TurnStarted {
            turn_id: root_turn.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: root_turn,
            parent_turn_id: None,
            prompt_summary: "根任务".to_owned(),
        },
    );
    append(
        session,
        &format!("event-spawn-{child_agent_id}"),
        SessionEvent::SubAgentSpawned {
            agent: keencode_resources::SubAgentState {
                agent_id: AgentId::new(child_agent_id).expect("子 Agent ID 应有效"),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: format!("/root/{}", child_agent_id.replace('-', "_")),
                task: "子任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    );
}

/// 创建尚未启动 Turn 终态对账使用的最小子 Agent 请求。
fn unstarted_turn_termination_request(
    turn_id: &str,
    child_agent_id: &str,
    root_turn_id: &str,
    parent_turn_id: &str,
    task: &str,
    prompt_summary: &str,
    initial_task: bool,
) -> UnstartedTurnTerminationRequest {
    UnstartedTurnTerminationRequest {
        agent: keencode_resources::SubAgentState {
            agent_id: AgentId::new(child_agent_id).expect("子 Agent ID 应有效"),
            parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            agent_path: format!("/root/{}", child_agent_id.replace('-', "_")),
            task: task.to_owned(),
            status: SubAgentStatus::Interrupted,
            current_turn_id: Some(
                keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"),
            ),
            result_summary: None,
        },
        turn_id: keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"),
        root_turn_id: keencode_resources::TurnId::new(root_turn_id).expect("根 Turn ID 应有效"),
        parent_turn_id: keencode_resources::TurnId::new(parent_turn_id).expect("父 Turn ID 应有效"),
        prompt_summary: prompt_summary.to_owned(),
        initial_task,
        termination: UnstartedTurnTermination::Interrupted,
    }
}

/// 创建指定稳定说明的未启动 Failed Turn 终态请求。
fn unstarted_failed_turn_termination_request(
    turn_id: &str,
    child_agent_id: &str,
    root_turn_id: &str,
    task: &str,
    initial_task: bool,
    message: &str,
) -> UnstartedTurnTerminationRequest {
    let mut request = unstarted_turn_termination_request(
        turn_id,
        child_agent_id,
        root_turn_id,
        root_turn_id,
        task,
        task,
        initial_task,
    );
    request.termination = UnstartedTurnTermination::Failed {
        message: message.to_owned(),
    };
    request
}

/// 读取 Runtime Journal 的全部类型化物理记录。
fn journal_records(session: &RuntimeSession) -> Vec<SessionEventRecord> {
    std::fs::read_to_string(session.inner.journal.log_path())
        .expect("Runtime Journal 应读取")
        .lines()
        .map(|line| serde_json::from_str(line).expect("Runtime Journal 记录应可反序列化"))
        .collect()
}

/// 断言子 Agent 停止原因与状态变化存在于同一条 AtomicBatch 物理记录。
fn assert_child_terminal_pair(
    session: &RuntimeSession,
    turn_id: &str,
    child_agent_id: &str,
    expected_reason: TurnStopReason,
    expected_status: SubAgentStatus,
) {
    let turn_id = keencode_resources::TurnId::new(turn_id).expect("子 Turn ID 应有效");
    let child_agent_id = AgentId::new(child_agent_id).expect("子 Agent ID 应有效");
    assert!(journal_records(session).iter().any(|record| {
        let SessionEvent::AtomicBatch { events } = &record.event else {
            return false;
        };
        events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::TurnStopped {
                    turn_id: stopped_turn_id,
                    reason,
                    ..
                } if stopped_turn_id == &turn_id && reason == &expected_reason
            )
        }) && events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::SubAgentStatusChanged {
                    agent_id,
                    turn_id: Some(status_turn_id),
                    status,
                    ..
                } if agent_id == &child_agent_id
                    && status_turn_id == &turn_id
                    && status == &expected_status
            )
        })
    }));
}

/// 只在首个 Provider 实时事件上阻塞，用于确定性观察执行中 Turn。
struct FirstModelEventGate {
    /// 首个实时事件已经进入 Sink 的通知。
    entered: Notify,
    /// 允许首个实时事件继续提交给 Agent 归约器的通知。
    release: Notify,
    /// 保证后续实时事件不再重复阻塞。
    blocked_once: AtomicBool,
}

impl FirstModelEventGate {
    /// 创建尚未阻塞过任何实时事件的测试栅栏。
    fn new() -> Self {
        Self {
            entered: Notify::new(),
            release: Notify::new(),
            blocked_once: AtomicBool::new(false),
        }
    }

    /// 有界等待首个 Provider 事件已经进入 Sink。
    async fn wait_until_entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .expect("首个 Provider 事件应在五秒内进入测试栅栏");
    }

    /// 解除首个 Provider 事件的测试阻塞。
    fn release(&self) {
        self.release.notify_one();
    }
}

impl AgentEventSink for FirstModelEventGate {
    /// 可靠接收实时事件，并仅让首个事件等待测试显式放行。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        if self.blocked_once.swap(true, Ordering::SeqCst) {
            return Box::pin(async { Ok(()) });
        }
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        })
    }
}

/// 对每个临时事件明确返回失败，用于验证 Publisher 不提前暴露未确认投递。
struct RejectLiveEventSink;

impl AgentEventSink for RejectLiveEventSink {
    /// 拒绝事件且不产生任何外部副作用。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async {
            Err(keencode_agent::AgentEventSinkError::new("测试拒绝实时事件"))
        })
    }
}

/// 记录动态 mailbox 输入已经在 Transcript 提交后获得确认。
struct FollowupInputAcknowledgement {
    /// 确认方法是否已经执行。
    acknowledged: Arc<AtomicBool>,
}

impl AgentDynamicInputAcknowledgement for FollowupInputAcknowledgement {
    /// 幂等记录动态输入已完成两阶段确认。
    fn acknowledge(&self) -> Result<(), AgentDynamicInputError> {
        self.acknowledged.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 首次采样边界返回一条 mailbox 消息，后续边界返回空批次。
struct OneShotFollowupInputSource {
    /// 动态输入端口被调用的次数。
    claims: AtomicUsize,
    /// 首次 claim 返回的完整 Provider 中立消息。
    message: Message,
    /// Transcript 提交后确认回执的共享观测位。
    acknowledged: Arc<AtomicBool>,
}

impl AgentDynamicInputSource for OneShotFollowupInputSource {
    /// 仅在首次 claim 返回 mailbox 消息，并要求 Runner 完成两阶段确认。
    fn claim(
        &self,
        _session_id: &keencode_agent::SessionId,
        _turn_id: &keencode_agent::TurnId,
        _source_agent_id: &keencode_agent::AgentId,
        _boundary: AgentDynamicInputBoundary,
        _maximum: usize,
    ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError> {
        if self.claims.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(AgentDynamicInputBatch::new_with_receipts(
                vec![self.message.clone()],
                vec![AgentDynamicInputReceipt::new(
                    AgentDynamicInputKind::Mailbox,
                    1,
                )],
                Arc::new(FollowupInputAcknowledgement {
                    acknowledged: self.acknowledged.clone(),
                }),
            ))
        } else {
            Ok(AgentDynamicInputBatch::empty())
        }
    }
}

/// 真实越过执行起点并返回一个有界结果的状态变更测试工具。
struct RuntimeWriteTool {
    /// 标记工具实现是否确实被调用。
    executed: Arc<AtomicBool>,
}

/// 记录 Runtime 是否把模型 Round 用量直接交给注入出口，并可确定性拒绝提交。
struct RuntimeUsageProbeSink {
    /// 用于验证首次用量提交严格早于响应中的工具副作用。
    tool_executed: Arc<AtomicBool>,
    /// 从首次调用开始需要明确拒绝的次数。
    rejected_attempts: usize,
    /// 用量出口收到的总调用次数。
    attempts: AtomicUsize,
    /// 每次有界重试收到的完整不可变用量事实。
    usages: Mutex<Vec<ModelRoundUsage>>,
    /// 首轮用量提交期间工具始终尚未执行的观测结果。
    first_round_preceded_tool: AtomicBool,
}

impl RuntimeUsageProbeSink {
    /// 创建一个没有历史调用的 Runtime 用量探针。
    fn new(tool_executed: Arc<AtomicBool>, rejected_attempts: usize) -> Self {
        Self {
            tool_executed,
            rejected_attempts,
            attempts: AtomicUsize::new(0),
            usages: Mutex::new(Vec::new()),
            first_round_preceded_tool: AtomicBool::new(true),
        }
    }

    /// 返回按实际同步调用顺序捕获的用量事实。
    fn usages(&self) -> Vec<ModelRoundUsage> {
        self.usages
            .lock()
            .expect("Runtime 用量探针锁不应损坏")
            .clone()
    }
}

impl RuntimeModelRoundUsageSink for RuntimeUsageProbeSink {
    /// 保存完整用量，并按配置让 Runtime 的有界重试收到明确拒绝。
    fn commit(&self, usage: &ModelRoundUsage) -> Result<(), AgentCommitSinkError> {
        if usage.model_round() == 1 && self.tool_executed.load(Ordering::SeqCst) {
            self.first_round_preceded_tool
                .store(false, Ordering::SeqCst);
        }
        self.usages
            .lock()
            .expect("Runtime 用量探针锁不应损坏")
            .push(usage.clone());
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < self.rejected_attempts {
            Err(AgentCommitSinkError::rejected(
                "测试拒绝 Runtime 模型 Round 用量",
            ))
        } else {
            Ok(())
        }
    }
}

impl AgentTool for RuntimeWriteTool {
    /// 返回仅接受一个 path 字符串的固定工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "RuntimeWrite",
            "验证 Runtime 部分生命周期后的 reservation 保留",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    /// 测试工具固定声明为会改变状态。
    fn effect(&self, _input: &serde_json::Value) -> Result<AgentToolEffect, ToolError> {
        Ok(AgentToolEffect::ChangesState)
    }

    /// 状态变更工具始终作为独占顺序屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 记录真实执行并返回一个最小成功结果。
    fn execute(&self, _context: ToolContext, _input: serde_json::Value) -> ToolFuture<'_> {
        self.executed.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(ToolOutput::text("Runtime 写入完成")) })
    }
}

/// 生命周期事件委托真实 Runtime，仅对最终 Round 持续返回明确拒绝。
struct RejectFinalRoundSink {
    /// 真实 Session、Journal、Artifact 与 reservation 控制面。
    inner: Arc<super::RuntimeSessionInner>,
    /// 最终 Round 被 Agent 有界重投的次数。
    round_attempts: AtomicUsize,
}

impl AgentCommitSink for RejectFinalRoundSink {
    /// 使用真实 Runtime 完成副作用前预检和 reservation 签发。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        preflight_round(&self.inner, round)
    }

    /// 工具生命周期走真实 Journal；最终 Round 模拟持续且明确的下游拒绝。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        if matches!(
            event.kind(),
            AgentCommitEventKind::ModelRoundCommitted { .. }
        ) {
            self.round_attempts.fetch_add(1, Ordering::SeqCst);
            return Err(AgentCommitSinkError::rejected("测试最终 Round 持续拒绝"));
        }
        commit_agent_event(&self.inner, event)
    }
}

/// 验证 create/open/snapshot 共享同一身份，且 lease 在句柄存活期间拒绝竞争者。
#[test]
fn create_open_snapshot_and_lease_competition() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-create-open");
    let snapshot = session.snapshot().expect("Snapshot 应读取");
    assert!(snapshot.state.created);
    assert_eq!(snapshot.state.status, SessionStatus::Idle);
    assert_eq!(snapshot.state.last_sequence, 1);
    assert!(!snapshot.closed);
    assert!(!snapshot.recovery_required);
    assert_eq!(snapshot.active_reservations, 0);
    assert_eq!(snapshot.pending_indeterminate_events, 0);

    assert!(matches!(
        RuntimeSession::open_session(config(&root), "runtime-create-open"),
        Err(RuntimeError::SessionBusy)
    ));
    drop(session);

    let reopened = match RuntimeSession::open_session(config(&root), "runtime-create-open")
        .expect("Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("健康 Session 不应损坏：{:?}", report.issues)
        }
    };
    assert_eq!(reopened.session_id().as_str(), "runtime-create-open");
    assert_eq!(
        reopened.snapshot().expect("重开 Snapshot 应读取").state,
        snapshot.state
    );
}

/// 验证创建与冷打开只在健康 Journal 状态下保留被权威事件引用的 Artifact。
#[test]
fn create_and_open_reclaim_complete_unreferenced_artifacts() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session_id = SessionId::new("runtime-artifact-recovery").expect("Session ID 应有效");
    let precreate_store = ArtifactStore::open(
        &runtime_config.storage_root,
        session_id,
        runtime_config.artifacts,
    )
    .expect("预创建 ArtifactStore 应打开");
    precreate_store
        .put(b"precreate-orphan", Some("text/plain".to_owned()))
        .expect("预创建完整孤儿应写入");
    assert_eq!(
        precreate_store
            .capacity()
            .expect("预创建容量应读取")
            .committed_unique_artifacts,
        1
    );
    drop(precreate_store);

    let session = RuntimeSession::create_session(
        runtime_config.clone(),
        CreateSessionRequest {
            session_id: "runtime-artifact-recovery".to_owned(),
            title: "Runtime Artifact 恢复".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("创建 Session 时应回收尚未引用的完整 Artifact");
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("创建后容量应读取")
            .committed_unique_artifacts,
        0
    );
    session
        .inner
        .artifacts
        .put(b"open-orphan", Some("text/plain".to_owned()))
        .expect("冷打开前完整孤儿应写入");
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("冷打开前容量应读取")
            .committed_unique_artifacts,
        1
    );
    drop(session);

    let reopened = match RuntimeSession::open_session(runtime_config, "runtime-artifact-recovery")
        .expect("带完整孤儿的健康 Session 应冷打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("未引用 Artifact 不应损坏健康 Journal：{:?}", report.issues)
        }
    };
    assert_eq!(
        reopened
            .inner
            .artifacts
            .capacity()
            .expect("冷打开后容量应读取")
            .committed_unique_artifacts,
        0
    );
}

/// 验证 Runtime 绑定入口真实驱动 Provider 中立 Agent Loop，并提交 Turn 起止与最终消息。
#[tokio::test]
async fn bound_agent_runner_commits_complete_turn_lifecycle() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-bound-runner");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("Runtime 纵向链路完成")],
    ));
    let runner = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default());
    let bound = session.bind_agent_runner(runner);
    let turn_id = "turn-bound-runner";
    let input = Message::text(ModelMessageRole::User, "执行完整纵向链路");
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );

    let result = bound
        .run_turn(RuntimeTurnRequest::root(
            request,
            vec![input],
            "执行完整纵向链路",
        ))
        .await
        .expect("绑定 Runtime 的 Turn 应完整执行");
    assert!(result.is_success());
    assert_eq!(provider.requests().expect("模型请求应可读取").len(), 1);

    let snapshot = session.snapshot().expect("完整 Turn Snapshot 应读取");
    let turn = snapshot
        .state
        .turns
        .get(&keencode_resources::TurnId::new(turn_id).expect("资源 Turn ID 应有效"))
        .expect("Turn 应进入权威状态");
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(snapshot.state.status, SessionStatus::Idle);
    assert_eq!(snapshot.state.transcript.len(), 2);
    assert!(matches!(
        &snapshot.state.transcript[0],
        TranscriptRecord::MessageAdded(message)
            if message.role == keencode_resources::MessageRole::User
                && message.content == vec![MessagePart::Text {
                    text: "执行完整纵向链路".to_owned()
                }]
    ));
    let TranscriptRecord::SegmentCommitted(segment) = &snapshot.state.transcript[1] else {
        panic!("最终模型响应必须作为原子 Transcript 段提交");
    };
    assert!(matches!(
        segment.messages.as_slice(),
        [message]
            if message.content == vec![MessagePart::Text {
                text: "Runtime 纵向链路完成".to_owned()
            }]
    ));
}

/// 验证 Runtime 绑定入口把每轮明确用量原样同步交给注入出口，且首轮提交先于工具执行。
#[tokio::test]
async fn bound_agent_runner_delegates_model_round_usage_before_tool_execution() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-bound-usage");
    let first_usage = TokenUsage {
        input_tokens: Some(11),
        output_tokens: Some(7),
        reasoning_tokens: Some(3),
        cache_read_tokens: Some(2),
        cache_write_tokens: Some(1),
        total_tokens: Some(18),
    };
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            state_changing_tool_reply_with_facts(
                ResponseMetadata {
                    response_id: Some("response-runtime-usage-tool".to_owned()),
                    model: Some("provider-runtime-usage-tool".to_owned()),
                },
                first_usage.clone(),
            ),
            completed_text_reply_with_facts(
                "用量绑定完成",
                ResponseMetadata {
                    response_id: Some("response-runtime-usage-final".to_owned()),
                    model: Some("provider-runtime-usage-final".to_owned()),
                },
                None,
            ),
        ],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("RuntimeWrite 应注册");
    let usage_sink = Arc::new(RuntimeUsageProbeSink::new(executed.clone(), 0));
    let runner = session.bind_agent_runner_with_usage_sink(
        AgentRunner::new(provider, tools, RunLimits::default()),
        usage_sink.clone(),
    );

    let result = runner
        .run_turn(root_runtime_turn(
            &session,
            "turn-bound-usage",
            "验证 Runtime 用量绑定",
        ))
        .await
        .expect("Runtime 用量提交成功时 Turn 应执行");

    assert!(result.is_success());
    assert!(executed.load(Ordering::SeqCst));
    assert!(usage_sink.first_round_preceded_tool.load(Ordering::SeqCst));
    assert_eq!(usage_sink.attempts.load(Ordering::SeqCst), 2);
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].session_id().as_str(), "runtime-bound-usage");
    assert_eq!(usages[0].turn_id().as_str(), "turn-bound-usage");
    assert_eq!(usages[0].source_agent_id().as_str(), "root");
    assert_eq!(usages[0].model(), "test-model");
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::ToolUse);
    assert_eq!(usages[0].completion().usage, first_usage);
    assert_eq!(usages[1].model_round(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].completion().usage, TokenUsage::unknown());
}

/// 验证注入的 Runtime 用量出口持续失败会形成 Turn 失败并阻止工具副作用。
#[tokio::test]
async fn bound_agent_runner_propagates_usage_failure_and_blocks_tool_execution() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-bound-usage-failure");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [state_changing_tool_reply_with_facts(
            ResponseMetadata::default(),
            TokenUsage {
                input_tokens: Some(5),
                output_tokens: Some(3),
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                total_tokens: Some(8),
            },
        )],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("RuntimeWrite 应注册");
    let usage_sink = Arc::new(RuntimeUsageProbeSink::new(executed.clone(), usize::MAX));
    let runner = session.bind_agent_runner_with_usage_sink(
        AgentRunner::new(provider, tools, RunLimits::default()),
        usage_sink.clone(),
    );

    let result = runner
        .run_turn(root_runtime_turn(
            &session,
            "turn-bound-usage-failure",
            "验证 Runtime 用量失败传播",
        ))
        .await
        .expect("用量拒绝应持久形成失败终态");

    assert!(matches!(
        result.error,
        Some(keencode_agent::AgentRunError::CommitSink(_))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(!executed.load(Ordering::SeqCst));
    assert!(usage_sink.first_round_preceded_tool.load(Ordering::SeqCst));
    assert_eq!(usage_sink.attempts.load(Ordering::SeqCst), 2);
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0], usages[1]);
    let state = session.snapshot().expect("失败终态应可读取").state;
    assert_eq!(state.model_rounds.len(), 0);
    assert_eq!(
        state
            .turns
            .get(
                &keencode_resources::TurnId::new("turn-bound-usage-failure")
                    .expect("失败 Turn ID 应有效"),
            )
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Failed)
    );
}

/// 验证绑定入口在任何模型请求或 Session 写入前拒绝跨 Session Turn。
#[tokio::test]
async fn bound_agent_runner_rejects_cross_session_request_before_side_effects() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-bound-session-a");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("不应被调用")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let input = Message::text(ModelMessageRole::User, "不应执行");
    let request = TurnRequest::new(
        keencode_agent::SessionId::new("runtime-bound-session-b")
            .expect("不同 Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-cross-session").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );

    assert!(matches!(
        bound
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                "拒绝跨 Session",
            ))
            .await,
        Err(RuntimeError::InvalidTurnRequest)
    ));
    assert!(provider.requests().expect("模型请求应可读取").is_empty());
    let snapshot = session.snapshot().expect("拒绝后 Snapshot 应读取");
    assert!(snapshot.state.turns.is_empty());
    assert!(snapshot.state.transcript.is_empty());
}

/// 验证计划守卫与 Session 权威状态不一致时在 Provider 和 Turn 写入前拒绝。
#[tokio::test]
async fn bound_agent_runner_rejects_session_mode_mismatch_before_side_effects() {
    let plan_root = TempDir::new().expect("计划测试目录应创建");
    let plan_session = create(&plan_root, "runtime-plan-mismatch");
    append(
        &plan_session,
        "event-enable-plan",
        SessionEvent::PlanChanged {
            plan: PlanState {
                enabled: true,
                plan_artifact: None,
            },
        },
    );
    let plan_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("不应调用")],
    ));
    let plan_bound = plan_session.bind_agent_runner(AgentRunner::new(
        plan_provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let plan_input = Message::text(ModelMessageRole::User, "计划守卫不一致");
    let plan_request = TurnRequest::new(
        keencode_agent::SessionId::new(plan_session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-plan-mismatch").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        vec![plan_input.clone()],
        PlanGuard::inactive(),
    );
    assert!(matches!(
        plan_bound
            .run_turn(RuntimeTurnRequest::root(
                plan_request,
                vec![plan_input],
                "计划守卫不一致",
            ))
            .await,
        Err(RuntimeError::InvalidTurnRequest)
    ));
    assert!(
        plan_provider
            .requests()
            .expect("计划 Provider 请求应读取")
            .is_empty()
    );
    let plan_snapshot = plan_session.snapshot().expect("计划拒绝状态应读取");
    assert_eq!(plan_snapshot.state.last_sequence, 2);
    assert!(plan_snapshot.state.turns.is_empty());
}

/// 验证子 Agent Turn 起点和正常终态分别以单一物理事件原子配对状态变化。
#[tokio::test]
async fn bound_child_turn_pairs_running_and_completed_status_atomically() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-child-completed");
    let root_turn_id = "turn-root-child-completed";
    let child_turn_id = "turn-child-completed";
    let child_agent_id = "child-completed";
    register_pending_child(&session, root_turn_id, child_agent_id);
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("子任务完成")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider,
        ToolRegistry::new(),
        RunLimits::default(),
    ));

    let result = bound
        .run_turn(child_runtime_turn(
            &session,
            child_turn_id,
            child_agent_id,
            root_turn_id,
            root_turn_id,
            "执行子任务",
        ))
        .await
        .expect("子 Agent Turn 应完整执行");
    assert!(result.is_success());

    let snapshot = session.snapshot().expect("子 Agent 完成状态应读取");
    let child_turn = keencode_resources::TurnId::new(child_turn_id).expect("子 Turn ID 应有效");
    let child_agent = AgentId::new(child_agent_id).expect("子 Agent ID 应有效");
    assert_eq!(
        snapshot
            .state
            .turns
            .get(&child_turn)
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Completed)
    );
    assert_eq!(
        snapshot
            .state
            .sub_agents
            .get(&child_agent)
            .map(|agent| agent.status.clone()),
        Some(SubAgentStatus::Completed)
    );
    assert_eq!(
        snapshot
            .state
            .sub_agents
            .get(&child_agent)
            .and_then(|agent| agent.result_summary.clone()),
        Some("子任务完成".to_owned())
    );

    let records = journal_records(&session);
    assert!(records.iter().any(|record| {
        let SessionEvent::AtomicBatch { events } = &record.event else {
            return false;
        };
        events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::TurnStarted {
                    turn_id,
                    source_agent_id,
                    ..
                } if turn_id == &child_turn && source_agent_id == &child_agent
            )
        }) && events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::SubAgentStatusChanged {
                    agent_id,
                    turn_id: Some(status_turn_id),
                    status: SubAgentStatus::Running,
                    ..
                } if agent_id == &child_agent && status_turn_id == &child_turn
            )
        })
    }));
    assert!(records.iter().any(|record| {
        let SessionEvent::AtomicBatch { events } = &record.event else {
            return false;
        };
        events.iter().any(|event| {
            matches!(event, SessionEvent::TurnCompleted { turn_id } if turn_id == &child_turn)
        }) && events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::SubAgentStatusChanged {
                    agent_id,
                    turn_id: Some(status_turn_id),
                    status: SubAgentStatus::Completed,
                    result_summary: Some(summary),
                } if agent_id == &child_agent && status_turn_id == &child_turn
                    && summary == "子任务完成"
            )
        })
    }));
}

/// 首次子 Agent 身份、Turn 起点与 Running 状态必须形成同一物理原子批次。
#[tokio::test]
async fn initial_child_turn_atomically_persists_identity_and_allows_stricter_plan() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-initial-child-atomic");
    let root_turn =
        keencode_resources::TurnId::new("turn-root-initial-child").expect("根 Turn ID 应有效");
    append(
        &session,
        "event-root-initial-child",
        SessionEvent::TurnStarted {
            turn_id: root_turn.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: root_turn.clone(),
            parent_turn_id: None,
            prompt_summary: "根任务".to_owned(),
        },
    );
    let child_agent = AgentId::new("child-initial-atomic").expect("子 Agent ID 应有效");
    let child_turn =
        keencode_resources::TurnId::new("turn-child-initial-atomic").expect("子 Turn ID 应有效");
    let input = Message::text(ModelMessageRole::User, "执行首次子任务");
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(child_turn.as_str()).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new(child_agent.as_str()).expect("Agent 子身份应有效"),
        "test-model",
        vec![input.clone()],
        PlanGuard::read_only(),
    );
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("首次子任务完成")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider,
        ToolRegistry::new(),
        RunLimits::default(),
    ));

    let result = bound
        .run_turn(RuntimeTurnRequest::initial_child(
            request,
            vec![input],
            root_turn.as_str(),
            root_turn.as_str(),
            "执行首次子任务",
            keencode_resources::SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child_runtime".to_owned(),
                task: "执行首次子任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        ))
        .await
        .expect("首次子 Agent Turn 应完整执行");

    assert!(result.is_success());
    let records = journal_records(&session);
    let start = records
        .iter()
        .find_map(|record| match &record.event {
            SessionEvent::AtomicBatch { events }
                if events.iter().any(|event| {
                    matches!(
                        event,
                        SessionEvent::TurnStarted { turn_id, .. } if turn_id == &child_turn
                    )
                }) =>
            {
                Some(events)
            }
            _ => None,
        })
        .expect("首次子 Agent 起点应存在");
    assert!(matches!(
        start.as_slice(),
        [
            SessionEvent::SubAgentSpawned { agent },
            SessionEvent::TurnStarted { turn_id, .. },
            SessionEvent::SubAgentStatusChanged {
                agent_id,
                turn_id: Some(status_turn_id),
                status: SubAgentStatus::Running,
                ..
            },
            SessionEvent::MessageAdded { .. }
        ] if agent.agent_id == child_agent
            && turn_id == &child_turn
            && agent_id == &child_agent
            && status_turn_id == &child_turn
    ));
}

/// 已完成子 Agent 的零初始消息 followup 必须先 claim mailbox，再进行首次模型采样。
#[tokio::test]
async fn registered_child_followup_allows_zero_initial_messages_and_claims_before_sampling() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-child-empty-followup");
    let root_turn = "turn-child-followup-root";
    let first_turn = "turn-child-followup-first";
    let followup_turn = "turn-child-followup-next";
    let child_agent_id = "child-followup";
    register_pending_child(&session, root_turn, child_agent_id);

    let first_runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [completed_text_reply("首次任务完成")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        first_runner
            .run_turn(child_runtime_turn(
                &session,
                first_turn,
                child_agent_id,
                root_turn,
                root_turn,
                "执行首次任务",
            ))
            .await
            .expect("首次子 Agent Turn 应执行")
            .is_success()
    );

    let child_agent = AgentId::new(child_agent_id).expect("子 Agent ID 应有效");
    let history = session
        .model_transcript_for_agent(&child_agent)
        .expect("子 Agent 历史应物化");
    assert!(!history.is_empty());
    let mailbox_input = Message::text(ModelMessageRole::Developer, "处理 mailbox 后续任务");
    let acknowledged = Arc::new(AtomicBool::new(false));
    let dynamic_input = Arc::new(OneShotFollowupInputSource {
        claims: AtomicUsize::new(0),
        message: mailbox_input.clone(),
        acknowledged: acknowledged.clone(),
    });
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("后续任务完成")],
    ));
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(followup_turn).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new(child_agent_id).expect("Agent 子身份应有效"),
        "test-model",
        history.clone(),
        current_plan_guard(&session),
    );
    let runner = session.bind_agent_runner(
        AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
            .with_dynamic_input_source(dynamic_input.clone()),
    );
    let result = runner
        .run_turn(RuntimeTurnRequest::child(
            request,
            Vec::new(),
            root_turn,
            first_turn,
            "处理 mailbox followup",
        ))
        .await
        .expect("已注册子 Agent 的零消息 followup 应执行");

    assert!(result.is_success(), "followup 失败：{:?}", result.error);
    assert!(acknowledged.load(Ordering::SeqCst));
    assert!(dynamic_input.claims.load(Ordering::SeqCst) >= 1);
    let requests = provider.requests().expect("Provider 请求应读取");
    assert_eq!(requests.len(), 1);
    let mut expected_messages = history;
    expected_messages.push(mailbox_input);
    assert_eq!(requests[0].messages, expected_messages);
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .filter(|message| *message == &expected_messages[expected_messages.len() - 1])
            .count(),
        1,
        "动态消息在 Provider 请求中只能出现一次"
    );
    let snapshot = session.snapshot().expect("followup Snapshot 应读取");
    assert_eq!(snapshot.state.dynamic_input_receipts.len(), 1);
    let effective = session
        .model_transcript_for_agent(&child_agent)
        .expect("followup 有效 Transcript 应物化");
    assert_eq!(
        effective
            .iter()
            .filter(|message| *message == &expected_messages[expected_messages.len() - 1])
            .count(),
        1,
        "有效 Transcript 中动态消息只能保留一次"
    );
    assert!(snapshot.state.transcript.iter().any(|record| matches!(
        record,
        TranscriptRecord::SegmentCommitted(segment)
            if segment.turn_id.as_str() == followup_turn
                && segment.source_agent_id == child_agent
                && segment.messages.iter().any(|message| {
                    message.role == keencode_resources::MessageRole::Developer
                })
    )));
}

/// Provider 在动态输入提交后失败时，重试请求与冷恢复仍须保留且只保留一份动态消息。
#[tokio::test]
async fn provider_failure_after_dynamic_input_preserves_retry_and_cold_history() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session_id = "runtime-dynamic-input-provider-failure";
    let session = create(&root, session_id);
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let first_turn = "turn-dynamic-input-provider-failure";
    let retry_turn = "turn-dynamic-input-provider-retry";
    let first_input = Message::text(ModelMessageRole::User, "首次请求输入");
    let dynamic_message = Message::text(ModelMessageRole::Developer, "Provider 失败前的 mailbox");
    let acknowledged = Arc::new(AtomicBool::new(false));
    let dynamic_input = Arc::new(OneShotFollowupInputSource {
        claims: AtomicUsize::new(0),
        message: dynamic_message.clone(),
        acknowledged: acknowledged.clone(),
    });
    let failed_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![Err(
            ModelError::ProviderUnavailable {
                message: "模拟 Provider 失败".to_owned(),
                status_code: None,
                retryable: false,
            },
        )])],
    ));
    let first_request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(first_turn).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new(root_agent.as_str()).expect("Agent 根身份应有效"),
        "test-model",
        vec![first_input.clone()],
        current_plan_guard(&session),
    );
    let failed_runner = session.bind_agent_runner(
        AgentRunner::new(
            failed_provider.clone(),
            ToolRegistry::new(),
            RunLimits::default(),
        )
        .with_dynamic_input_source(dynamic_input.clone()),
    );
    let failed_result = failed_runner
        .run_turn(RuntimeTurnRequest::root(
            first_request,
            vec![first_input.clone()],
            "首次请求输入",
        ))
        .await
        .expect("Provider 失败也应形成可恢复 Turn 终态");
    assert_eq!(
        failed_result.state.terminal_reason(),
        Some(TerminalReason::Failed)
    );
    assert!(
        acknowledged.load(Ordering::SeqCst),
        "动态输入 receipt 应完成 ack"
    );
    assert_eq!(dynamic_input.claims.load(Ordering::SeqCst), 1);
    let failed_requests = failed_provider
        .requests()
        .expect("失败 Provider 请求应读取");
    assert_eq!(failed_requests.len(), 1);
    assert_eq!(
        failed_requests[0]
            .messages
            .iter()
            .filter(|message| *message == &dynamic_message)
            .count(),
        1,
        "失败前的 Provider 请求只能注入一份动态消息"
    );

    let state_after_failure = session.snapshot().expect("失败后的 Snapshot 应读取");
    assert_eq!(state_after_failure.state.dynamic_input_receipts.len(), 1);
    let history_after_failure = session
        .model_transcript_for_agent(&root_agent)
        .expect("失败后的 Agent Transcript 应物化");
    assert_eq!(
        history_after_failure
            .iter()
            .filter(|message| *message == &dynamic_message)
            .count(),
        1,
        "Provider 失败后有效 Transcript 仍应保留一份动态消息"
    );

    let retry_input = Message::text(ModelMessageRole::User, "重试请求输入");
    let mut retry_messages = history_after_failure.clone();
    retry_messages.push(retry_input.clone());
    let retry_request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(retry_turn).expect("重试 Turn ID 应有效"),
        keencode_agent::AgentId::new(root_agent.as_str()).expect("Agent 根身份应有效"),
        "test-model",
        retry_messages.clone(),
        current_plan_guard(&session),
    );
    let retry_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("重试完成")],
    ));
    let retry_runner = session.bind_agent_runner(
        AgentRunner::new(
            retry_provider.clone(),
            ToolRegistry::new(),
            RunLimits::default(),
        )
        .with_dynamic_input_source(dynamic_input.clone()),
    );
    let retry_result = retry_runner
        .run_turn(RuntimeTurnRequest::root(
            retry_request,
            vec![retry_input],
            "重试请求输入",
        ))
        .await
        .expect("重试 Turn 应执行");
    assert!(
        retry_result.is_success(),
        "重试失败：{:?}",
        retry_result.error
    );
    let retry_requests = retry_provider.requests().expect("重试 Provider 请求应读取");
    assert_eq!(retry_requests.len(), 1);
    assert_eq!(retry_requests[0].messages, retry_messages);
    assert_eq!(
        retry_requests[0]
            .messages
            .iter()
            .filter(|message| *message == &dynamic_message)
            .count(),
        1,
        "重试 Provider 请求不得重复注入动态消息"
    );

    drop(retry_runner);
    drop(failed_runner);
    drop(session);
    let reopened = match RuntimeSession::open_session(runtime_config, session_id)
        .expect("Session 应冷打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!(
                "动态输入失败回归测试的 Journal 不应损坏：{:?}",
                report.issues
            )
        }
    };
    let cold_history = reopened
        .model_transcript_for_agent(&root_agent)
        .expect("冷恢复 Agent Transcript 应物化");
    assert_eq!(
        cold_history
            .iter()
            .filter(|message| *message == &dynamic_message)
            .count(),
        1,
        "drop/reopen 后 Agent Transcript 只能保留一份动态消息"
    );
}

/// 根 Turn 与首次子 Agent Turn 即使模型模板已有历史，也不得省略本次初始消息。
#[tokio::test]
async fn root_and_initial_child_turns_reject_zero_initial_messages() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-empty-initial-rejected");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        std::iter::empty::<ScriptedReply>(),
    ));
    let runner = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let mut root_request = root_runtime_turn(&session, "turn-empty-root", "根输入");
    root_request.input_messages.clear();
    assert!(matches!(
        runner.run_turn(root_request).await,
        Err(RuntimeError::InvalidTurnRequest)
    ));

    let root_turn =
        keencode_resources::TurnId::new("turn-empty-child-root").expect("根 Turn ID 应有效");
    append(
        &session,
        "event-empty-child-root",
        SessionEvent::TurnStarted {
            turn_id: root_turn.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: root_turn.clone(),
            parent_turn_id: None,
            prompt_summary: "根任务".to_owned(),
        },
    );
    let child_agent = AgentId::new("child-empty-initial").expect("子 Agent ID 应有效");
    let template_message = Message::text(ModelMessageRole::Developer, "父任务上下文");
    let child_request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-empty-child").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new(child_agent.as_str()).expect("Agent 子身份应有效"),
        "test-model",
        vec![template_message],
        current_plan_guard(&session),
    );
    assert!(matches!(
        runner
            .run_turn(RuntimeTurnRequest::initial_child(
                child_request,
                Vec::new(),
                root_turn.as_str(),
                root_turn.as_str(),
                "首次子任务",
                keencode_resources::SubAgentState {
                    agent_id: child_agent,
                    parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                    agent_path: "/root/child_empty_initial".to_owned(),
                    task: "首次子任务".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            ))
            .await,
        Err(RuntimeError::InvalidTurnRequest)
    ));
    assert!(
        session
            .snapshot()
            .expect("拒绝后状态应读取")
            .state
            .sub_agents
            .is_empty()
    );
    assert!(provider.requests().expect("Provider 请求应读取").is_empty());
}

/// 验证已经完成的相同 Turn 顺序重投不会再次调用 Provider。
#[tokio::test]
async fn sequential_duplicate_turn_never_reinvokes_provider() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-sequential-duplicate");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("唯一响应")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));

    assert!(
        bound
            .run_turn(root_runtime_turn(
                &session,
                "turn-sequential-duplicate",
                "只执行一次",
            ))
            .await
            .expect("首次 Turn 应执行")
            .is_success()
    );
    assert!(matches!(
        bound
            .run_turn(root_runtime_turn(
                &session,
                "turn-sequential-duplicate",
                "只执行一次",
            ))
            .await,
        Err(RuntimeError::TurnAlreadyFinished)
    ));
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
}

/// 验证正在执行的相同 Turn 并发重投立即拒绝且不会产生第二次 Provider 调用。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_turn_invokes_provider_once() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-concurrent-duplicate");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("并发唯一响应")],
    ));
    let gate = Arc::new(FirstModelEventGate::new());
    let bound = Arc::new(
        session.bind_agent_runner(
            AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
                .with_event_sink(gate.clone()),
        ),
    );
    let first_request = root_runtime_turn(&session, "turn-concurrent-duplicate", "并发只执行一次");
    let first_bound = bound.clone();
    let first = tokio::spawn(async move { first_bound.run_turn(first_request).await });
    gate.wait_until_entered().await;

    assert!(matches!(
        bound
            .run_turn(root_runtime_turn(
                &session,
                "turn-concurrent-duplicate",
                "并发只执行一次",
            ))
            .await,
        Err(RuntimeError::TurnAlreadyRunning)
    ));
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
    gate.release();
    assert!(
        first
            .await
            .expect("首个并发任务不应异常")
            .expect("首个并发 Turn 应完成")
            .is_success()
    );
}

/// 验证三条 Journal 的精确边界仍保留失败终态，而两条边界会在 Provider 前拒绝。
#[tokio::test]
async fn runtime_turn_max_records_boundary_reserves_terminal_record() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut exact_config = config(&root);
    exact_config.journal.max_records = 3;
    let exact_session = RuntimeSession::create_session(
        exact_config,
        CreateSessionRequest {
            session_id: "runtime-turn-records-exact".to_owned(),
            title: "Turn 终态精确记录边界".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("三条记录边界 Session 应创建");
    let exact_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("最终 Transcript 将因容量被拒绝")],
    ));
    let exact_bound = exact_session.bind_agent_runner(AgentRunner::new(
        exact_provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let exact_result = exact_bound
        .run_turn(root_runtime_turn(
            &exact_session,
            "turn-records-exact",
            "验证终态保留",
        ))
        .await
        .expect("Agent 失败终态本身应可靠提交");
    assert!(!exact_result.is_success());
    let exact_snapshot = exact_session.snapshot().expect("精确边界状态应读取");
    assert_eq!(exact_snapshot.state.last_sequence, 3);
    assert_eq!(
        exact_snapshot
            .state
            .turns
            .get(&keencode_resources::TurnId::new("turn-records-exact").expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Failed)
    );
    assert!(!exact_snapshot.recovery_required);
    assert_eq!(
        exact_provider
            .requests()
            .expect("Provider 请求应读取")
            .len(),
        1
    );

    let second_root = TempDir::new().expect("第二个临时目录应创建");
    let mut insufficient_config = config(&second_root);
    insufficient_config.journal.max_records = 2;
    let insufficient_session = RuntimeSession::create_session(
        insufficient_config,
        CreateSessionRequest {
            session_id: "runtime-turn-records-insufficient".to_owned(),
            title: "Turn 终态不足记录边界".to_owned(),
            project_root: second_root.path().display().to_string(),
        },
    )
    .expect("两条记录边界 Session 应创建");
    let insufficient_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("不应调用")],
    ));
    let insufficient_bound = insufficient_session.bind_agent_runner(AgentRunner::new(
        insufficient_provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(matches!(
        insufficient_bound
            .run_turn(root_runtime_turn(
                &insufficient_session,
                "turn-records-insufficient",
                "容量不足",
            ))
            .await,
        Err(RuntimeError::TurnUnpersistable)
    ));
    assert!(
        insufficient_provider
            .requests()
            .expect("Provider 请求应读取")
            .is_empty()
    );
    assert_eq!(
        insufficient_session
            .snapshot()
            .expect("拒绝后状态应读取")
            .state
            .last_sequence,
        1
    );
}

/// 验证绑定 Turn 的唯一终态与工具冷恢复终态共享同一条最坏记录预算。
#[tokio::test]
async fn bound_tool_round_does_not_double_reserve_mutually_exclusive_terminal_record() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_records = 8;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-bound-tool-terminal-boundary".to_owned(),
            title: "绑定工具终态精确边界".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("精确记录边界 Session 应创建");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [state_changing_tool_reply()],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("Runtime 状态变更工具应注册");
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider,
        tools,
        RunLimits::new(1, 1).expect("单 Round 单 Step 上限应有效"),
    ));

    let result = bound
        .run_turn(root_runtime_turn(
            &session,
            "turn-bound-tool-terminal-boundary",
            "在精确边界执行工具",
        ))
        .await
        .expect("共享终态预算后工具 Round 应执行并可靠结束");
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert!(executed.load(Ordering::SeqCst));
    let snapshot = session.snapshot().expect("精确边界状态应读取");
    assert_eq!(snapshot.state.last_sequence, 7);
    assert!(!snapshot.recovery_required);
    assert_eq!(snapshot.active_reservations, 0);
}

/// 验证取消、普通失败和运行上限都与子 Agent 对应终态原子配对。
#[tokio::test]
async fn bound_child_turn_pairs_cancelled_failed_and_limit_statuses_atomically() {
    let cancelled_root = TempDir::new().expect("取消测试目录应创建");
    let cancelled_session = create(&cancelled_root, "runtime-child-cancelled");
    register_pending_child(&cancelled_session, "turn-root-cancelled", "child-cancelled");
    let cancelled_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("取消前已经发起调用")],
    ));
    let cancelled_gate = Arc::new(FirstModelEventGate::new());
    let cancelled_bound = Arc::new(
        cancelled_session.bind_agent_runner(
            AgentRunner::new(
                cancelled_provider.clone(),
                ToolRegistry::new(),
                RunLimits::default(),
            )
            .with_event_sink(cancelled_gate.clone()),
        ),
    );
    let cancelled_input = Message::text(ModelMessageRole::User, "取消子任务");
    let cancelled_request = TurnRequest::new(
        keencode_agent::SessionId::new(cancelled_session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-child-cancelled").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("child-cancelled").expect("Agent 子身份应有效"),
        "test-model",
        vec![cancelled_input.clone()],
        PlanGuard::inactive(),
    );
    let cancelled_runtime_request = RuntimeTurnRequest::child(
        cancelled_request,
        vec![cancelled_input],
        "turn-root-cancelled",
        "turn-root-cancelled",
        "取消子任务",
    );
    let cancelled_task_runner = cancelled_bound.clone();
    let cancelled_task = tokio::spawn(async move {
        cancelled_task_runner
            .run_turn(cancelled_runtime_request)
            .await
    });
    cancelled_gate.wait_until_entered().await;
    assert_eq!(
        cancelled_session
            .cancel_turn("turn-child-cancelled")
            .expect("Runtime 权威取消应成功"),
        TurnCancellationOutcome::Requested
    );
    let cancelled_result = cancelled_task
        .await
        .expect("取消任务不应异常")
        .expect("取消终态应可靠提交");
    assert_eq!(
        cancelled_result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(
        cancelled_provider
            .requests()
            .expect("取消 Provider 请求应读取")
            .len(),
        1
    );
    assert_child_terminal_pair(
        &cancelled_session,
        "turn-child-cancelled",
        "child-cancelled",
        TurnStopReason::Cancelled,
        SubAgentStatus::Interrupted,
    );

    let failed_root = TempDir::new().expect("失败测试目录应创建");
    let failed_session = create(&failed_root, "runtime-child-failed");
    register_pending_child(&failed_session, "turn-root-failed", "child-failed");
    let failed_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![Err(
            ModelError::ProviderUnavailable {
                message: "\0".repeat(MAX_RUNTIME_TERMINAL_MESSAGE_BYTES),
                status_code: None,
                retryable: false,
            },
        )])],
    ));
    let failed_bound = failed_session.bind_agent_runner(AgentRunner::new(
        failed_provider,
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let failed_result = failed_bound
        .run_turn(child_runtime_turn(
            &failed_session,
            "turn-child-failed",
            "child-failed",
            "turn-root-failed",
            "turn-root-failed",
            "失败子任务",
        ))
        .await
        .expect("包含最坏 JSON 转义字符的失败终态应可靠提交");
    assert_eq!(
        failed_result.state.terminal_reason(),
        Some(TerminalReason::Failed)
    );
    let failed_snapshot = failed_session.snapshot().expect("失败子 Agent 状态应读取");
    let failed_summary = failed_snapshot
        .state
        .sub_agents
        .get(&AgentId::new("child-failed").expect("子 Agent ID 应有效"))
        .and_then(|agent| agent.result_summary.as_ref())
        .expect("失败子 Agent 应保留有界摘要");
    assert!(failed_summary.len() <= MAX_RUNTIME_TERMINAL_MESSAGE_BYTES);
    assert!(!failed_snapshot.recovery_required);
    assert_child_terminal_pair(
        &failed_session,
        "turn-child-failed",
        "child-failed",
        TurnStopReason::Failed,
        SubAgentStatus::Failed,
    );

    let limit_root = TempDir::new().expect("上限测试目录应创建");
    let limit_session = create(&limit_root, "runtime-child-limit");
    register_pending_child(&limit_session, "turn-root-limit", "child-limit");
    let limit_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [state_changing_tool_reply()],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("上限测试工具应注册");
    let limit_bound = limit_session.bind_agent_runner(AgentRunner::new(
        limit_provider,
        tools,
        RunLimits::new(1, 1).expect("单 Round 单 Step 上限应有效"),
    ));
    let limit_result = limit_bound
        .run_turn(child_runtime_turn(
            &limit_session,
            "turn-child-limit",
            "child-limit",
            "turn-root-limit",
            "turn-root-limit",
            "达到子任务上限",
        ))
        .await
        .expect("运行上限终态应可靠提交");
    assert_eq!(
        limit_result.state.terminal_reason(),
        Some(TerminalReason::LimitReached),
        "非预期终态错误：{:?}",
        limit_result.error
    );
    assert!(executed.load(Ordering::SeqCst));
    assert_child_terminal_pair(
        &limit_session,
        "turn-child-limit",
        "child-limit",
        TurnStopReason::LimitReached,
        SubAgentStatus::Failed,
    );
}

/// 验证终态追加结果不确定时只重投冻结事件，并原样返回首次 Agent 结果。
#[tokio::test]
async fn terminal_indeterminate_retry_never_reinvokes_provider() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-terminal-indeterminate");
    let turn_id = "turn-terminal-indeterminate";
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("首次冻结结果")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let terminal_event_id = runtime_lifecycle_event_id(
        session.session_id(),
        &keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"),
        "turn-terminal",
    )
    .expect("终态事件 ID 应派生");
    inject_runtime_lifecycle_indeterminate(&terminal_event_id);

    assert!(matches!(
        bound
            .run_turn(root_runtime_turn(&session, turn_id, "验证终态重投"))
            .await,
        Err(RuntimeError::RecoveryRequired)
    ));
    let pending = session.snapshot().expect("待重投状态应读取");
    assert!(pending.recovery_required);
    assert_eq!(pending.pending_indeterminate_events, 1);
    assert_eq!(
        pending
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Running)
    );

    let retried = bound
        .run_turn(root_runtime_turn(&session, turn_id, "验证终态重投"))
        .await
        .expect("相同请求应只重投冻结终态");
    assert!(retried.is_success());
    assert!(matches!(
        retried
            .final_response
            .as_ref()
            .and_then(|response| response.content.first()),
        Some(ContentBlock::Text { text }) if text == "首次冻结结果"
    ));
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
    let completed = session.snapshot().expect("重投完成状态应读取");
    assert!(!completed.recovery_required);
    assert_eq!(completed.pending_indeterminate_events, 0);
}

/// 验证 TurnStarted 已写入可见但结果不确定时通过相同事件 ID 热对账后才调用 Provider。
#[tokio::test]
async fn visible_start_indeterminate_retry_reconciles_before_provider() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-visible-start-indeterminate");
    let turn_id = "turn-visible-start-indeterminate";
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("起点对账后唯一响应")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let start_event_id = runtime_lifecycle_event_id(
        session.session_id(),
        &keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"),
        "turn-started",
    )
    .expect("起点事件 ID 应派生");
    inject_runtime_lifecycle_visible_indeterminate(&start_event_id);

    assert!(matches!(
        bound
            .run_turn(root_runtime_turn(&session, turn_id, "验证可见起点对账"))
            .await,
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(provider.requests().expect("Provider 请求应读取").is_empty());
    let pending = session.snapshot().expect("可见起点待对账状态应读取");
    assert!(pending.recovery_required);
    assert_eq!(pending.pending_indeterminate_events, 1);
    assert_eq!(
        pending
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Running)
    );

    let result = bound
        .run_turn(root_runtime_turn(&session, turn_id, "验证可见起点对账"))
        .await
        .expect("相同起点应命中 AlreadyCommitted 并继续唯一 Provider 调用");
    assert!(result.is_success());
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
    let completed = session.snapshot().expect("起点对账完成状态应读取");
    assert!(!completed.recovery_required);
    assert_eq!(completed.pending_indeterminate_events, 0);
    assert_eq!(
        completed
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Completed)
    );
}

/// 验证 TurnStarted 明确未追加时立即回收输入 Artifact，并允许同一热 Session 复用槽位。
#[tokio::test]
async fn rejected_start_commit_reclaims_artifact_capacity_before_hot_retry() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.artifacts.max_artifacts_per_session = 1;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-rejected-start-artifact".to_owned(),
            title: "TurnStarted 明确失败".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("单槽位 Session 应创建");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("热路径复用 Artifact 槽位")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let failed_turn_id =
        keencode_resources::TurnId::new("turn-rejected-start-artifact").expect("Turn ID 应有效");
    let start_event_id =
        runtime_lifecycle_event_id(session.session_id(), &failed_turn_id, "turn-started")
            .expect("起点事件 ID 应派生");
    inject_runtime_lifecycle_failure(&start_event_id);

    assert!(matches!(
        bound
            .run_turn(image_runtime_turn(
                &session,
                failed_turn_id.as_str(),
                "aGVsbG8=",
                "明确拒绝首个起点",
            ))
            .await,
        Err(RuntimeError::Resource(_))
    ));
    assert!(provider.requests().expect("Provider 请求应读取").is_empty());
    let failed_snapshot = session.snapshot().expect("明确失败状态应读取");
    assert!(!failed_snapshot.recovery_required);
    assert!(failed_snapshot.state.turns.is_empty());
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("明确失败后容量应读取")
            .committed_unique_artifacts,
        0
    );

    let result = bound
        .run_turn(image_runtime_turn(
            &session,
            "turn-hot-artifact-retry",
            "d29ybGQ=",
            "复用唯一 Artifact 槽位",
        ))
        .await
        .expect("明确失败回收后热 Session 应复用唯一槽位");
    assert!(result.is_success());
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("热重试后容量应读取")
            .committed_unique_artifacts,
        1
    );
}

/// 验证执行 Future 被中止后热路径冻结，重新打开 Session 才能保守收敛。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_runtime_turn_requires_and_completes_cold_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session = create(&root, "runtime-aborted-turn");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("不应到达终态")],
    ));
    let gate = Arc::new(FirstModelEventGate::new());
    let bound = Arc::new(
        session.bind_agent_runner(
            AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
                .with_event_sink(gate.clone()),
        ),
    );
    let turn_id = "turn-aborted-runtime";
    let request = root_runtime_turn(&session, turn_id, "中止执行 Future");
    let task_bound = bound.clone();
    let task = tokio::spawn(async move { task_bound.run_turn(request).await });
    gate.wait_until_entered().await;
    task.abort();
    assert!(
        task.await
            .expect_err("中止任务应返回 JoinError")
            .is_cancelled()
    );

    let frozen = session.snapshot().expect("中止后的冻结状态应读取");
    assert!(frozen.recovery_required);
    assert_eq!(
        frozen
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Running)
    );

    drop(bound);
    drop(session);
    let recovered = match RuntimeSession::open_session(runtime_config, "runtime-aborted-turn")
        .expect("中止 Session 应可重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("已确认起点不应损坏 Journal：{:?}", report.issues)
        }
    };
    let recovered_snapshot = recovered.snapshot().expect("冷恢复状态应读取");
    assert!(!recovered_snapshot.recovery_required);
    assert_eq!(
        recovered_snapshot
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Failed)
    );
}

/// 验证输入 Artifact 中途物化失败后会幂等补齐并由同一 TurnStarted 原子引用。
#[tokio::test]
async fn starting_artifact_commit_retry_does_not_leak_orphan_or_reservation() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-starting-artifact-retry");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("输入 Artifact 已可靠提交")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let turn_id =
        keencode_resources::TurnId::new("turn-starting-artifact-retry").expect("Turn ID 应有效");
    let input_messages = vec![
        Message::new(
            ModelMessageRole::User,
            vec![ContentBlock::Image {
                image: ImageContent::from_base64("image/png", "aGVsbG8="),
            }],
        ),
        Message::new(
            ModelMessageRole::User,
            vec![ContentBlock::Image {
                image: ImageContent::from_base64("image/png", "d29ybGQ="),
            }],
        ),
    ];
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id.as_str()).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        input_messages.clone(),
        PlanGuard::inactive(),
    );
    inject_runtime_input_commit_fault(&turn_id, 1);

    let result = bound
        .run_turn(RuntimeTurnRequest::root(
            request,
            input_messages,
            "补齐输入 Artifact",
        ))
        .await
        .expect("一次性物化故障应通过相同内容寻址输入补齐");
    assert!(result.is_success());
    assert_eq!(provider.requests().expect("Provider 请求应读取").len(), 1);
    let snapshot = session.snapshot().expect("Artifact 补齐状态应读取");
    assert!(!snapshot.recovery_required);
    assert_eq!(snapshot.active_reservations, 0);
    assert_eq!(snapshot.state.transcript.len(), 3);
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("Artifact 容量应读取")
            .committed_unique_artifacts,
        2
    );
    assert!(
        session
            .inner
            .control
            .lock()
            .expect("控制面锁应可用")
            .turn_executions
            .is_empty()
    );
}

/// 验证输入 Artifact 无法在有界重试内补齐时建立可冷恢复硬栅栏且释放终态预留。
#[tokio::test]
async fn persistent_starting_artifact_commit_failure_requires_cold_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session = create(&root, "runtime-starting-artifact-hard-fence");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("不应调用")],
    ));
    let bound = session.bind_agent_runner(AgentRunner::new(
        provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let turn_id = keencode_resources::TurnId::new("turn-starting-artifact-hard-fence")
        .expect("Turn ID 应有效");
    let input_messages = vec![
        Message::new(
            ModelMessageRole::User,
            vec![ContentBlock::Image {
                image: ImageContent::from_base64("image/png", "aGVsbG8="),
            }],
        ),
        Message::new(
            ModelMessageRole::User,
            vec![ContentBlock::Image {
                image: ImageContent::from_base64("image/png", "d29ybGQ="),
            }],
        ),
    ];
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id.as_str()).expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        input_messages.clone(),
        PlanGuard::inactive(),
    );
    inject_runtime_input_commit_faults(&turn_id, 1, 2);

    assert!(matches!(
        bound
            .run_turn(RuntimeTurnRequest::root(
                request,
                input_messages,
                "输入 Artifact 持续失败",
            ))
            .await,
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(provider.requests().expect("Provider 请求应读取").is_empty());
    let frozen = session.snapshot().expect("硬栅栏状态应读取");
    assert!(frozen.recovery_required);
    assert_eq!(frozen.active_reservations, 0);
    assert!(frozen.state.turns.is_empty());
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("硬栅栏建立后 Artifact 容量应读取")
            .committed_unique_artifacts,
        0
    );
    {
        let control = session.inner.control.lock().expect("控制面锁应可用");
        let execution = control
            .turn_executions
            .get(turn_id.as_str())
            .expect("失败 Turn 应保留冻结摘要");
        assert!(matches!(
            execution,
            super::RuntimeTurnExecution::Abandoned { .. }
        ));
        assert_eq!(execution.terminal_journal_bytes(), 0);
    }

    drop(bound);
    drop(session);
    let reopened =
        match RuntimeSession::open_session(runtime_config, "runtime-starting-artifact-hard-fence")
            .expect("硬栅栏 Session 应可重新打开")
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(report) => {
                panic!("未写入 Turn 起点不应损坏 Journal：{:?}", report.issues)
            }
        };
    let recovered = reopened.snapshot().expect("冷恢复状态应读取");
    assert!(!recovered.recovery_required);
    assert!(recovered.state.turns.is_empty());
    assert_eq!(
        reopened
            .inner
            .artifacts
            .capacity()
            .expect("冷打开后 Artifact 容量应读取")
            .committed_unique_artifacts,
        0
    );
}

/// 验证无效子谱系和超大 AtomicBatch 都在图片 Artifact 物化与 Provider 前拒绝。
#[tokio::test]
async fn invalid_lineage_and_oversized_input_batch_reject_before_artifact_commit() {
    let lineage_root = TempDir::new().expect("谱系测试目录应创建");
    let lineage_session = create(&lineage_root, "runtime-invalid-lineage-artifact");
    let lineage_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("不应调用")],
    ));
    let lineage_bound = lineage_session.bind_agent_runner(AgentRunner::new(
        lineage_provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let image = Message::new(
        ModelMessageRole::User,
        vec![ContentBlock::Image {
            image: ImageContent::from_base64("image/png", "aGVsbG8="),
        }],
    );
    let lineage_remaining = lineage_session
        .inner
        .artifacts
        .capacity()
        .expect("初始 Artifact 容量应读取")
        .remaining();
    let lineage_request = TurnRequest::new(
        keencode_agent::SessionId::new(lineage_session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-invalid-lineage").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("missing-child").expect("Agent 子身份应有效"),
        "test-model",
        vec![image.clone()],
        PlanGuard::inactive(),
    );
    assert!(matches!(
        lineage_bound
            .run_turn(RuntimeTurnRequest::child(
                lineage_request,
                vec![image.clone()],
                "missing-root",
                "missing-parent",
                "无效谱系",
            ))
            .await,
        Err(RuntimeError::InvalidTurnRequest)
    ));
    assert_eq!(
        lineage_session
            .inner
            .artifacts
            .capacity()
            .expect("拒绝后 Artifact 容量应读取")
            .remaining(),
        lineage_remaining
    );
    assert!(
        lineage_provider
            .requests()
            .expect("谱系 Provider 请求应读取")
            .is_empty()
    );

    let batch_root = TempDir::new().expect("批次测试目录应创建");
    let batch_session = create(&batch_root, "runtime-oversized-input-batch");
    let batch_provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [completed_text_reply("不应调用")],
    ));
    let batch_bound = batch_session.bind_agent_runner(AgentRunner::new(
        batch_provider.clone(),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let mut input_messages = vec![image];
    input_messages.extend(
        (1..1_024).map(|index| Message::text(ModelMessageRole::User, format!("输入-{index}"))),
    );
    let batch_remaining = batch_session
        .inner
        .artifacts
        .capacity()
        .expect("初始 Artifact 容量应读取")
        .remaining();
    let batch_request = TurnRequest::new(
        keencode_agent::SessionId::new(batch_session.session_id().as_str())
            .expect("Agent Session ID 应有效"),
        keencode_agent::TurnId::new("turn-oversized-input-batch").expect("Agent Turn ID 应有效"),
        keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
        "test-model",
        input_messages.clone(),
        PlanGuard::inactive(),
    );
    assert!(matches!(
        batch_bound
            .run_turn(RuntimeTurnRequest::root(
                batch_request,
                input_messages,
                "超大输入批次",
            ))
            .await,
        Err(RuntimeError::InvalidTurnRequest)
    ));
    assert_eq!(
        batch_session
            .inner
            .artifacts
            .capacity()
            .expect("拒绝后 Artifact 容量应读取")
            .remaining(),
        batch_remaining
    );
    assert!(
        batch_provider
            .requests()
            .expect("批次 Provider 请求应读取")
            .is_empty()
    );
    assert_eq!(
        batch_session
            .snapshot()
            .expect("批次拒绝状态应读取")
            .state
            .last_sequence,
        1
    );
}

/// 验证 Provider 执行期间建立的恢复栅栏阻止随后终态越过冻结边界。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_fence_created_during_provider_execution_blocks_terminal_commit() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-provider-recovery-fence");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("不得越过恢复栅栏")],
    ));
    let gate = Arc::new(FirstModelEventGate::new());
    let bound = Arc::new(
        session.bind_agent_runner(
            AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
                .with_event_sink(gate.clone()),
        ),
    );
    let turn_id = "turn-provider-recovery-fence";
    let request = root_runtime_turn(&session, turn_id, "执行中建立恢复栅栏");
    let task_bound = bound.clone();
    let task = tokio::spawn(async move { task_bound.run_turn(request).await });
    gate.wait_until_entered().await;
    {
        let mut control = session.inner.control.lock().expect("控制面锁应可用");
        mark_event_indeterminate(&mut control, "external-indeterminate-event");
    }
    gate.release();
    assert!(matches!(
        task.await.expect("执行任务不应异常"),
        Err(RuntimeError::RecoveryRequired)
    ));
    let frozen = session.snapshot().expect("恢复栅栏状态应读取");
    assert!(frozen.recovery_required);
    assert_eq!(
        frozen
            .state
            .turns
            .get(&keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"))
            .map(|turn| turn.status.clone()),
        Some(TurnStatus::Running)
    );
    assert!(!journal_records(&session).iter().any(|record| {
        matches!(
            &record.event,
            SessionEvent::TurnCompleted { turn_id: completed_turn_id }
                if completed_turn_id.as_str() == turn_id
        ) || matches!(
            &record.event,
            SessionEvent::TurnStopped { turn_id: stopped_turn_id, .. }
                if stopped_turn_id.as_str() == turn_id
        )
    }));
}

/// 验证首次不确定结果只允许相同事件继续对账，确认后才解除全局恢复栅栏。
#[test]
fn indeterminate_event_gate_allows_only_same_identity_until_confirmed() {
    let mut control = ControlState::default();
    assert!(recovery_gate_allows_event(&control, "event-a"));
    assert!(recovery_gate_allows_event(&control, "event-b"));

    mark_event_indeterminate(&mut control, "event-a");
    mark_event_indeterminate(&mut control, "event-a");
    assert!(control.recovery_required);
    assert_eq!(control.pending_indeterminate.len(), 1);
    assert!(recovery_gate_allows_event(&control, "event-a"));
    assert!(!recovery_gate_allows_event(&control, "event-b"));

    mark_event_confirmed(&mut control, "event-a");
    assert!(!control.recovery_required);
    assert!(control.pending_indeterminate.is_empty());
    assert!(recovery_gate_allows_event(&control, "event-b"));

    control.hard_recovery_required = true;
    refresh_recovery_required(&mut control);
    mark_event_confirmed(&mut control, "event-a");
    assert!(control.recovery_required);
    assert!(!recovery_gate_allows_event(&control, "event-a"));
}

/// 验证默认配置能为 Agent 提交出口可见的工具生命周期、最终事件和未知 Artifact 保留容量。
#[test]
fn default_runtime_reserves_agent_visible_tool_round_persistence_budget() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-default-reservation");
    session
        .inner
        .artifacts
        .put(b"existing-artifact", Some("text/plain".to_owned()))
        .expect("已有 Artifact 应写入");
    let key = start_turn(&session, "turn-default-reservation", 7);
    let assistant = assistant_with_tool_calls(2);
    let reservation = preflight_round_candidate(&session.inner, key.clone(), &assistant, &[])
        .expect("默认配置应能签发工具 Round reservation");

    let control = session.inner.control.lock().expect("控制面锁应可用");
    let entry = control
        .reservations
        .get(&key)
        .expect("签发后应保存 reservation");
    assert_eq!(entry.reserved_journal_records, 8);
    assert!(
        entry.reserved_journal_bytes
            > u64::try_from(TOOL_OUTPUT_LIMITS.max_round_json_bytes)
                .expect("Round JSON 上限应能表示为 u64")
                * 2
    );
    assert_eq!(
        entry.reserved_unknown_artifacts,
        TOOL_OUTPUT_LIMITS.max_round_content_blocks
    );
    assert_eq!(entry.tool_request_sha256.len(), 2);
    assert!(entry.missing_artifact_ids.is_empty());
    drop(control);
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("Artifact 容量应读取")
            .committed_unique_artifacts,
        1
    );
    assert_eq!(
        session
            .snapshot()
            .expect("Snapshot 应读取")
            .state
            .last_sequence,
        2,
        "预检不得追加任何生命周期事件"
    );
    reservation.release();
    assert_eq!(
        session
            .snapshot()
            .expect("释放后 Snapshot 应读取")
            .active_reservations,
        0
    );
}

/// 验证最小状态集合限制会在状态变更工具任何生命周期或执行副作用前拒绝 Round。
#[test]
fn minimal_state_collection_limit_rejects_tool_round_before_side_effect() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_state_collection_items = 1;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-minimal-state-items".to_owned(),
            title: "最小状态集合限制".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("最小状态集合测试 Session 应创建");
    let key = start_turn(&session, "turn-minimal-state-items", 0);
    let assistant = Message::new(
        ModelMessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            tool_call: ToolCall::new("call-write", "Write", serde_json::json!({"path": "x"})),
        }],
    );
    let error = match preflight_round_candidate(&session.inner, key, &assistant, &[]) {
        Ok(reservation) => {
            reservation.release();
            panic!("状态集合上限 1 必须在工具执行前拒绝 Round");
        }
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
    let snapshot = session.snapshot().expect("拒绝后 Snapshot 应读取");
    assert_eq!(snapshot.state.last_sequence, 2);
    assert!(snapshot.state.tools.is_empty());
    assert_eq!(snapshot.active_reservations, 0);
}

/// 验证单工具 Round 的十六维状态集合预算在精确最大维度边界成功签发。
#[test]
fn state_collection_reservation_succeeds_at_exact_boundary() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_state_collection_items = 67;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-state-items-boundary".to_owned(),
            title: "状态集合边界".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("状态集合边界 Session 应创建");
    let key = start_turn(&session, "turn-state-items-boundary", 0);
    let reservation = preflight_round_candidate(
        &session.inner,
        key.clone(),
        &assistant_with_tool_calls(1),
        &[],
    )
    .expect("单工具 Round 应在最大维度 67 的精确边界签发");
    let control = session.inner.control.lock().expect("控制面锁应可用");
    let items = control
        .reservations
        .get(&key)
        .expect("reservation 应存在")
        .reserved_state_items;
    assert_eq!(items.transcript, 1);
    assert_eq!(items.messages, 67);
    assert_eq!(items.transcript_segments, 1);
    assert_eq!(items.tools, 1);
    assert_eq!(items.message_parts, 67);
    assert_eq!(
        items.message_tool_result_content,
        TOOL_OUTPUT_LIMITS.max_round_content_blocks
    );
    assert_eq!(
        items.tool_outcome_result_content,
        TOOL_OUTPUT_LIMITS.max_round_content_blocks
    );
    assert_eq!(items.json_collection_items, 2);
    drop(control);
    reservation.release();
}

/// 验证工具参数在 tools 与最终 Transcript 中会被分别计量，且边界外参数在预检拒绝。
#[test]
fn state_collection_budget_counts_tool_arguments_twice() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_state_collection_items = 67;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-json-state-items".to_owned(),
            title: "JSON 状态集合计量".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("JSON 状态集合测试 Session 应创建");
    let key = start_turn(&session, "turn-json-state-items", 0);
    let arguments = |count: usize| {
        serde_json::Value::Object(
            (0..count)
                .map(|index| (format!("field-{index}"), serde_json::Value::Null))
                .collect(),
        )
    };
    let boundary = Message::new(
        ModelMessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            tool_call: ToolCall::new("call-json", "Write", arguments(33)),
        }],
    );
    let reservation = preflight_round_candidate(&session.inner, key.clone(), &boundary, &[])
        .expect("33 个参数成员复制计量后为 66，应在 67 边界内");
    reservation.release();
    let oversized = Message::new(
        ModelMessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            tool_call: ToolCall::new("call-json", "Write", arguments(34)),
        }],
    );
    let error = match preflight_round_candidate(&session.inner, key, &oversized, &[]) {
        Ok(reservation) => {
            reservation.release();
            panic!("34 个参数成员在 tools 与 Transcript 双份计量后应超过 67");
        }
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
}

/// 验证两个单独可签发的 Round 不得并发超卖同一状态集合剩余容量。
#[test]
fn concurrent_rounds_cannot_oversubscribe_state_collection_capacity() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_state_collection_items = 67;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-concurrent-state-items".to_owned(),
            title: "并发状态集合容量".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("并发状态集合测试 Session 应创建");
    let first_key = start_turn(&session, "turn-state-first", 0);
    let mut second_key = first_key.clone();
    second_key.model_round = 2;
    second_key.segment_index = 1;
    let first = preflight_round_candidate(
        &session.inner,
        first_key,
        &assistant_with_tool_calls(1),
        &[],
    )
    .expect("首个 Round 应单独适配 67 边界");
    let error = match preflight_round_candidate(
        &session.inner,
        second_key.clone(),
        &assistant_with_tool_calls(1),
        &[],
    ) {
        Ok(reservation) => {
            reservation.release();
            panic!("第二个并发 Round 不得超卖首个 Round 已保护的集合容量");
        }
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
    first.release();
    let second = preflight_round_candidate(
        &session.inner,
        second_key,
        &assistant_with_tool_calls(1),
        &[],
    )
    .expect("释放未使用的首个 reservation 后第二个 Round 应可单独签发");
    second.release();
}

/// 验证工具结果已进真实 Journal 后最终 Round 持续拒绝会保留尾部容量并冻结后续写入。
#[tokio::test]
async fn rejected_final_round_after_tool_completion_retains_reservation_and_freezes_runtime() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-rejected-final-round");
    let key = start_turn(&session, "turn-rejected-final-round", 0);
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [state_changing_tool_reply()],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("Runtime 状态变更测试工具应注册");
    let sink = Arc::new(RejectFinalRoundSink {
        inner: session.inner.clone(),
        round_attempts: AtomicUsize::new(0),
    });
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new(key.turn_id.clone()).expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
            key.model.clone(),
            vec![Message::text(ModelMessageRole::User, "执行状态变更工具")],
            PlanGuard::inactive(),
        ))
        .await;
    assert!(!result.is_success());
    assert!(executed.load(Ordering::SeqCst));
    assert_eq!(sink.round_attempts.load(Ordering::SeqCst), 2);

    let snapshot = session.snapshot().expect("最终 Round 拒绝后状态应读取");
    assert!(snapshot.recovery_required);
    assert_eq!(snapshot.active_reservations, 1);
    assert_eq!(snapshot.retained_reservations, 1);
    assert_eq!(snapshot.state.tools.len(), 1);
    assert!(
        snapshot
            .state
            .tools
            .values()
            .all(|tool| tool.execution_started && tool.outcome.is_some())
    );
    assert!(snapshot.state.transcript.is_empty());
    let frozen_sequence = snapshot.state.last_sequence;
    {
        let control = session.inner.control.lock().expect("控制面锁应可用");
        let entry = control
            .reservations
            .get(&key)
            .expect("已产生生命周期进度的 reservation 必须保留");
        assert!(entry.abandoned_after_progress);
        assert_eq!(entry.committed_event_ids.len(), 3);
        assert_eq!(entry.reserved_journal_records, 2);
        assert_eq!(entry.reserved_state_items.tools, 0);
        assert_eq!(
            entry.reserved_state_items.tool_outcome_result_content,
            TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
        );
        assert_eq!(entry.reserved_state_items.json_collection_items, 1);
        assert!(!recovery_gate_allows_event(&control, "unrelated-event"));
    }
    let mut next_key = key;
    next_key.model_round = 2;
    next_key.segment_index = 1;
    let error = match preflight_round_candidate(
        &session.inner,
        next_key,
        &assistant_with_tool_calls(1),
        &[],
    ) {
        Ok(reservation) => {
            reservation.release();
            panic!("恢复尾部 reservation 存活时不得签发后续 Round");
        }
        Err(error) => error,
    };
    assert_eq!(error.kind(), AgentToolRoundPreflightErrorKind::Unavailable);
    assert_eq!(
        session
            .snapshot()
            .expect("冻结后状态应再次读取")
            .state
            .last_sequence,
        frozen_sequence,
        "后续预检不得侵占恢复尾部 Journal 空间"
    );
    drop(sink);
    drop(session);
    let recovered =
        match RuntimeSession::open_session(config(&root), "runtime-rejected-final-round")
            .expect("冻结 Runtime 应能通过重新打开完成尾部恢复")
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(report) => {
                panic!("已确认生命周期不应形成损坏日志：{:?}", report.issues)
            }
        };
    let recovered_snapshot = recovered.snapshot().expect("恢复后状态应读取");
    assert!(!recovered_snapshot.recovery_required);
    assert_eq!(recovered_snapshot.active_reservations, 0);
    assert_eq!(recovered_snapshot.state.transcript.len(), 1);
    assert!(
        recovered_snapshot
            .state
            .tools
            .values()
            .all(|tool| tool.transcript_segment.is_some())
    );
}

/// 验证无审批工具完整生命周期后仍为恢复 Transcript 和 TurnStopped 各保留一条记录。
#[tokio::test]
async fn tool_lifecycle_exact_record_boundary_preserves_cold_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_records = 7;
    let session = RuntimeSession::create_session(
        runtime_config.clone(),
        CreateSessionRequest {
            session_id: "runtime-confirm-recovery-boundary".to_owned(),
            title: "工具恢复记录边界".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("精确恢复边界 Session 应创建");
    let key = start_turn(&session, "turn-confirm-recovery-boundary", 0);
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [state_changing_tool_reply()],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("Runtime 状态变更测试工具应注册");
    let sink = Arc::new(RejectFinalRoundSink {
        inner: session.inner.clone(),
        round_attempts: AtomicUsize::new(0),
    });
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(TurnRequest::new(
            keencode_agent::SessionId::new(session.session_id().as_str())
                .expect("Agent Session ID 应有效"),
            keencode_agent::TurnId::new(key.turn_id.clone()).expect("Agent Turn ID 应有效"),
            keencode_agent::AgentId::new("root").expect("Agent 根身份应有效"),
            key.model.clone(),
            vec![Message::text(ModelMessageRole::User, "执行状态变更工具")],
            PlanGuard::inactive(),
        ))
        .await;
    assert!(!result.is_success());
    assert!(executed.load(Ordering::SeqCst));
    assert_eq!(sink.round_attempts.load(Ordering::SeqCst), 2);

    let snapshot = session.snapshot().expect("最终 Round 拒绝后状态应读取");
    assert_eq!(snapshot.state.last_sequence, 5);
    assert!(snapshot.recovery_required);
    let tool = snapshot
        .state
        .tools
        .values()
        .next()
        .expect("工具生命周期应存在");
    assert!(tool.execution_started && tool.outcome.is_some());
    {
        let control = session.inner.control.lock().expect("控制面锁应可用");
        let entry = control
            .reservations
            .get(&key)
            .expect("已产生生命周期进度的 reservation 必须保留");
        assert_eq!(entry.committed_event_ids.len(), 3);
        assert_eq!(
            entry.reserved_journal_records, 2,
            "必须同时保留恢复 Transcript 和 TurnStopped 记录"
        );
    }

    drop(sink);
    drop(session);
    let recovered =
        match RuntimeSession::open_session(runtime_config, "runtime-confirm-recovery-boundary")
            .expect("七条 Journal 精确边界应能完成冷恢复")
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(report) => {
                panic!("工具生命周期不应形成损坏日志：{:?}", report.issues)
            }
        };
    let recovered_snapshot = recovered.snapshot().expect("边界恢复后状态应读取");
    assert_eq!(recovered_snapshot.state.last_sequence, 7);
    assert_eq!(recovered_snapshot.state.transcript.len(), 1);
    assert_eq!(
        recovered_snapshot
            .state
            .turns
            .values()
            .next()
            .map(|turn| &turn.status),
        Some(&TurnStatus::Failed)
    );
}

/// 验证子 Agent Round 按 AtomicBatch 真实恢复载荷预留字节，并严格守住一字节边界。
#[test]
fn child_round_reserves_atomic_recovery_bytes_at_exact_boundary() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut session = create(&root, "runtime-child-byte-boundary");
    let root_agent = AgentId::new("root").expect("根 Agent 应有效");
    let child_agent = AgentId::new("beta").expect("子 Agent 应有效");
    let root_turn = keencode_resources::TurnId::new("turn-root-00").expect("根 Turn 应有效");
    let child_turn = keencode_resources::TurnId::new("turn-beta-00").expect("子 Turn 应有效");
    append(
        &session,
        "event-root-byte-turn",
        SessionEvent::TurnStarted {
            turn_id: root_turn.clone(),
            source_agent_id: root_agent.clone(),
            root_turn_id: root_turn.clone(),
            parent_turn_id: None,
            prompt_summary: "根任务".to_owned(),
        },
    );
    append(
        &session,
        "event-child-byte-spawn",
        SessionEvent::SubAgentSpawned {
            agent: keencode_resources::SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child_byte".to_owned(),
                task: "子任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    );
    append(
        &session,
        "event-child-byte-turn",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        },
    );
    let root_key = RoundKey {
        session_id: session.session_id().as_str().to_owned(),
        turn_id: root_turn.as_str().to_owned(),
        agent_id: root_agent.as_str().to_owned(),
        model: "test-model".to_owned(),
        model_round: 1,
        segment_index: 0,
    };
    let child_key = RoundKey {
        session_id: session.session_id().as_str().to_owned(),
        turn_id: child_turn.as_str().to_owned(),
        agent_id: child_agent.as_str().to_owned(),
        model: "test-model".to_owned(),
        model_round: 1,
        segment_index: 0,
    };
    let assistant = assistant_with_tool_calls(1);

    let root_reservation =
        preflight_round_candidate(&session.inner, root_key.clone(), &assistant, &[])
            .expect("根 Agent Round 应签发 reservation");
    let root_reserved_bytes = session
        .inner
        .control
        .lock()
        .expect("控制面锁应可用")
        .reservations
        .get(&root_key)
        .expect("根 Agent reservation 应存在")
        .reserved_journal_bytes;
    root_reservation.release();
    let child_reservation =
        preflight_round_candidate(&session.inner, child_key.clone(), &assistant, &[])
            .expect("子 Agent Round 应签发 reservation");
    let child_reserved_bytes = session
        .inner
        .control
        .lock()
        .expect("控制面锁应可用")
        .reservations
        .get(&child_key)
        .expect("子 Agent reservation 应存在")
        .reserved_journal_bytes;
    child_reservation.release();

    let state = session.inner.journal.state().expect("Journal 状态应读取");
    let root_recovery_event = recovery_turn_stopped_event(&root_turn, &root_agent);
    let child_recovery_event = recovery_turn_stopped_event(&child_turn, &child_agent);
    let root_recovery_bytes = encoded_record_len(
        &state.session_id,
        &recovery_event_id("turn-stopped", root_turn.as_str()).expect("根 Turn 恢复事件 ID 应生成"),
        &root_recovery_event,
    )
    .expect("根 Turn 恢复事件应编码");
    let child_recovery_bytes = encoded_record_len(
        &state.session_id,
        &recovery_event_id("turn-stopped", child_turn.as_str())
            .expect("子 Turn 恢复事件 ID 应生成"),
        &child_recovery_event,
    )
    .expect("子 Turn 恢复 AtomicBatch 应编码");
    assert!(child_recovery_bytes > root_recovery_bytes);
    let reserved_delta = child_reserved_bytes
        .checked_sub(root_reserved_bytes)
        .expect("子 Agent reservation 必须包含更大的恢复载荷");
    let recovery_delta = child_recovery_bytes
        .checked_sub(root_recovery_bytes)
        .expect("子 Agent 恢复 AtomicBatch 必须大于根 TurnStopped");
    assert_eq!(
        reserved_delta, recovery_delta,
        "除恢复 Turn 载荷外的等长 Round 预算应完全相同"
    );

    let current_log_bytes = journal_len(&session.inner.journal).expect("Journal 长度应读取");
    let exact_log_limit = current_log_bytes
        .checked_add(child_reserved_bytes)
        .expect("精确 Journal 字节边界不应溢出");
    Arc::get_mut(&mut session.inner)
        .expect("没有其他 Runtime 强引用时应能调整测试配置")
        .config
        .journal
        .max_log_bytes = exact_log_limit;
    let exact = preflight_round_candidate(&session.inner, child_key.clone(), &assistant, &[])
        .expect("子 Agent Round 应在 AtomicBatch 字节精确边界签发");
    exact.release();

    Arc::get_mut(&mut session.inner)
        .expect("释放 reservation 后应能再次调整测试配置")
        .config
        .journal
        .max_log_bytes = exact_log_limit - 1;
    let error = match preflight_round_candidate(&session.inner, child_key, &assistant, &[]) {
        Ok(reservation) => {
            reservation.release();
            panic!("子 Agent Round 不得超卖恢复 AtomicBatch 的最后一字节")
        }
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
}

/// 验证未知输出不足 64 个 Artifact 槽位时会在任何生命周期事件前拒绝。
#[test]
fn preflight_rejects_before_side_effect_when_unknown_artifact_slots_are_insufficient() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.artifacts.max_artifacts_per_session =
        TOOL_OUTPUT_LIMITS.max_round_content_blocks;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-artifact-capacity".to_owned(),
            title: "Artifact 容量测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("容量测试 Session 应创建");
    session
        .inner
        .artifacts
        .put(b"occupied", Some("text/plain".to_owned()))
        .expect("应占用一个 Artifact 槽位");
    let key = start_turn(&session, "turn-artifact-capacity", 3);
    let error =
        match preflight_round_candidate(&session.inner, key, &assistant_with_tool_calls(1), &[]) {
            Ok(reservation) => {
                reservation.release();
                panic!("剩余 63 个槽位必须在工具执行前拒绝");
            }
            Err(error) => error,
        };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
    let snapshot = session.snapshot().expect("拒绝后 Snapshot 应读取");
    assert_eq!(snapshot.state.last_sequence, 2);
    assert_eq!(snapshot.active_reservations, 0);
}

/// 模拟未知 Artifact 已落盘但 Journal 尚未确认的重投，并验证容量账本不会再次占用同一槽位。
fn exercise_materialized_artifact_retry(session_id: &str, indeterminate: bool) {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.max_inline_text_bytes = 8;
    runtime_config.artifacts.max_artifacts_per_session =
        TOOL_OUTPUT_LIMITS.max_round_content_blocks;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "Artifact 重投容量测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("Artifact 重投容量测试 Session 应创建");
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("初始 Artifact 容量应读取")
            .remaining(),
        TOOL_OUTPUT_LIMITS.max_round_content_blocks,
        "测试必须从恰好 64 个可用槽位开始"
    );

    let key = start_turn(&session, &format!("turn-{session_id}"), 1);
    let reservation = preflight_round_candidate(
        &session.inner,
        key.clone(),
        &assistant_with_tool_calls(1),
        &[],
    )
    .expect("恰好 64 个未知槽位应允许签发 reservation");
    let result = ToolResult::text("call-0", "首次工具输出必须形成新的未知 Artifact", false);
    let mut initial_probe = ArtifactProbe::default();
    map_tool_result(
        &session.inner,
        &result,
        ArtifactMode::Probe,
        &mut initial_probe,
    )
    .expect("首次工具结果 Probe 应成功");
    assert_eq!(initial_probe.missing_ids.len(), 1);
    let event_bytes = 1_024;
    let tool_event_state_items = StateCollectionItems {
        tool_outcome_result_content: 1,
        ..StateCollectionItems::default()
    };
    {
        let state = session.inner.journal.state().expect("Journal 状态应读取");
        let control = session.inner.control.lock().expect("控制面锁应可用");
        ensure_commit_capacity(
            &session.inner,
            &control,
            &state,
            event_bytes,
            Some(&key),
            &initial_probe.missing_ids,
            tool_event_state_items,
        )
        .expect("首次未知 Artifact 应由 64 个预留槽位覆盖");
    }

    map_tool_result(
        &session.inner,
        &result,
        ArtifactMode::Commit,
        &mut ArtifactProbe::default(),
    )
    .expect("首次工具结果 Artifact 应物化");
    let materialized = materialized_probe_artifacts(&session.inner, &initial_probe)
        .expect("初始 Probe 候选应可核对落盘状态");
    assert_eq!(materialized.len(), 1);
    {
        let mut control = session.inner.control.lock().expect("控制面锁应可用");
        charge_materialized_reservation_artifacts(
            &mut control,
            Some(&key),
            &materialized,
            &initial_probe.missing_uses,
        )
        .expect("落盘 Artifact 应立即从 reservation 扣除");
        let entry = control
            .reservations
            .get(&key)
            .expect("reservation 应继续保留");
        assert_eq!(
            entry.reserved_unknown_artifacts,
            TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
        );
        assert_eq!(entry.materialized_artifact_ids.len(), 1);
        if indeterminate {
            mark_event_indeterminate(&mut control, "event-tool-completed");
            assert!(recovery_gate_allows_event(&control, "event-tool-completed"));
            assert!(!recovery_gate_allows_event(&control, "event-round"));
        } else {
            assert!(!control.recovery_required, "明确拒绝不应冻结恢复栅栏");
        }
    }
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("物化后 Artifact 容量应读取")
            .remaining(),
        TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
    );

    let mut retry_probe = ArtifactProbe::default();
    map_tool_result(
        &session.inner,
        &result,
        ArtifactMode::Probe,
        &mut retry_probe,
    )
    .expect("重投工具结果 Probe 应成功");
    assert!(
        retry_probe.missing_ids.is_empty(),
        "已落盘 Artifact 的重投 Probe 必须为空"
    );
    {
        let state = session.inner.journal.state().expect("Journal 状态应读取");
        let mut control = session.inner.control.lock().expect("控制面锁应可用");
        ensure_commit_capacity(
            &session.inner,
            &control,
            &state,
            event_bytes,
            Some(&key),
            &retry_probe.missing_ids,
            tool_event_state_items,
        )
        .expect("重投不得因 63 个剩余槽位与旧 64 槽 reservation 自我拒绝");
        charge_materialized_reservation_artifacts(
            &mut control,
            Some(&key),
            &materialized,
            &initial_probe.missing_uses,
        )
        .expect("重复核对同一 Artifact 应幂等");
        assert_eq!(
            control
                .reservations
                .get(&key)
                .expect("reservation 应保留")
                .reserved_unknown_artifacts,
            TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
        );
        charge_confirmed_reservation_event(
            &mut control,
            Some(&key),
            "event-tool-completed",
            event_bytes,
            tool_event_state_items,
        )
        .expect("重投确认后应只扣除一次 Journal 预算");
        if indeterminate {
            mark_event_confirmed(&mut control, "event-tool-completed");
        }
        assert!(recovery_gate_allows_event(&control, "event-round"));
        ensure_commit_capacity(
            &session.inner,
            &control,
            &state,
            event_bytes,
            Some(&key),
            &retry_probe.missing_ids,
            StateCollectionItems {
                transcript: 1,
                messages: 2,
                transcript_segments: 1,
                message_parts: 2,
                message_tool_result_content: 1,
                json_collection_items: 1,
                ..StateCollectionItems::default()
            },
        )
        .expect("重投确认后最终 Round 应可继续使用剩余 reservation");
    }
    reservation.release();
}

/// 验证 Artifact 先落盘而 Journal 结果不确定时，相同事件重投确认后仍可提交最终 Round。
#[test]
fn materialized_artifact_survives_indeterminate_journal_retry() {
    exercise_materialized_artifact_retry("runtime-artifact-indeterminate", true);
}

/// 验证 Artifact 先落盘而 Journal 明确拒绝时，重新映射不会因 Probe 为空而自我拒绝。
#[test]
fn materialized_artifact_survives_rejected_journal_retry() {
    exercise_materialized_artifact_retry("runtime-artifact-rejected", false);
}

/// 验证共享已知 Artifact 被一个 Round 物化后会从所有并发 reservation 清除陈旧缺失项。
#[test]
fn shared_known_artifact_is_reconciled_across_concurrent_reservations() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.max_inline_text_bytes = 8;
    runtime_config.artifacts.max_artifacts_per_session =
        TOOL_OUTPUT_LIMITS.max_round_content_blocks * 2 + 1;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-shared-known-artifact".to_owned(),
            title: "共享已知 Artifact".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("共享 Artifact 测试 Session 应创建");
    let first_key = start_turn(&session, "turn-shared-first", 0);
    let mut second_key = first_key.clone();
    second_key.model_round = 2;
    second_key.segment_index = 1;
    let shared_text = "两个并发 Round 使用完全相同的大文本 Artifact";
    let assistant = |call_id: &str| {
        Message::new(
            ModelMessageRole::Assistant,
            vec![
                ContentBlock::Text {
                    text: shared_text.to_owned(),
                },
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new(
                        call_id,
                        "Read",
                        serde_json::json!({"path": "shared.rs"}),
                    ),
                },
            ],
        )
    };
    let first_assistant = assistant("call-shared-first");
    let second_assistant = assistant("call-shared-second");
    let first = preflight_round_candidate(&session.inner, first_key.clone(), &first_assistant, &[])
        .expect("首个共享 Artifact Round 应签发");
    let second =
        preflight_round_candidate(&session.inner, second_key.clone(), &second_assistant, &[])
            .expect("第二个 Round 应在共享已知 ID 去重后的精确容量边界签发");
    {
        let control = session.inner.control.lock().expect("控制面锁应可用");
        assert_eq!(
            control
                .reservations
                .get(&first_key)
                .expect("首个 reservation 应存在")
                .missing_artifact_ids,
            control
                .reservations
                .get(&second_key)
                .expect("第二个 reservation 应存在")
                .missing_artifact_ids
        );
    }

    let mut first_probe = ArtifactProbe::default();
    map_message(
        &session.inner,
        &first_key,
        0,
        &first_assistant,
        ArtifactMode::Probe,
        &mut first_probe,
    )
    .expect("首个 Assistant Probe 应成功");
    assert_eq!(first_probe.missing_ids.len(), 1);
    map_message(
        &session.inner,
        &first_key,
        0,
        &first_assistant,
        ArtifactMode::Commit,
        &mut ArtifactProbe::default(),
    )
    .expect("首个 Assistant 应物化共享 Artifact");
    let materialized = materialized_probe_artifacts(&session.inner, &first_probe)
        .expect("共享 Artifact 应核对为已落盘");
    {
        let mut control = session.inner.control.lock().expect("控制面锁应可用");
        charge_materialized_reservation_artifacts(
            &mut control,
            Some(&first_key),
            &materialized,
            &first_probe.missing_uses,
        )
        .expect("共享 Artifact 应从全部 reservation 清账");
        let first_entry = control
            .reservations
            .get(&first_key)
            .expect("首个 reservation 应保留");
        let second_entry = control
            .reservations
            .get(&second_key)
            .expect("第二个 reservation 应保留");
        assert!(first_entry.missing_artifact_ids.is_empty());
        assert!(second_entry.missing_artifact_ids.is_empty());
        assert_eq!(first_entry.materialized_artifact_ids.len(), 1);
        assert!(second_entry.materialized_artifact_ids.is_empty());
        assert_eq!(
            first_entry.reserved_unknown_artifacts,
            TOOL_OUTPUT_LIMITS.max_round_content_blocks
        );
        assert_eq!(
            second_entry.reserved_unknown_artifacts,
            TOOL_OUTPUT_LIMITS.max_round_content_blocks
        );
    }
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("共享 Artifact 物化后容量应读取")
            .remaining(),
        TOOL_OUTPUT_LIMITS.max_round_content_blocks * 2
    );
    let mut second_retry_probe = ArtifactProbe::default();
    map_message(
        &session.inner,
        &second_key,
        0,
        &second_assistant,
        ArtifactMode::Probe,
        &mut second_retry_probe,
    )
    .expect("第二个 Assistant 重投 Probe 应成功");
    assert!(second_retry_probe.missing_ids.is_empty());
    {
        let state = session.inner.journal.state().expect("Journal 状态应读取");
        let control = session.inner.control.lock().expect("控制面锁应可用");
        ensure_commit_capacity(
            &session.inner,
            &control,
            &state,
            1_024,
            Some(&second_key),
            &second_retry_probe.missing_ids,
            StateCollectionItems {
                tools: 1,
                json_collection_items: 1,
                ..StateCollectionItems::default()
            },
        )
        .expect("共享 Artifact 交错物化后第二个 Round 不得因陈旧 missing ID 自我拒绝");
    }
    first.consume();
    second.release();
    assert_eq!(
        session
            .snapshot()
            .expect("清理后 Snapshot 应读取")
            .active_reservations,
        0
    );
}

/// 验证旧式 1 MiB 单事件限制面对多个边界内文本块时也会在执行前拒绝最坏最终事件。
#[test]
fn one_mib_event_limit_rejects_multi_block_round_before_tool_execution() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.max_event_bytes = 1024 * 1024;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-one-mib-event".to_owned(),
            title: "单事件容量测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("单事件容量测试 Session 应创建");
    let key = start_turn(&session, "turn-one-mib-event", 1);
    let mut content = vec![ContentBlock::ToolCall {
        tool_call: ToolCall::new("call-boundary", "Read", serde_json::json!({"path": "x"})),
    }];
    content.extend((0..8).map(|_| ContentBlock::Text {
        text: "x".repeat(60 * 1024),
    }));
    let assistant = Message::new(ModelMessageRole::Assistant, content);
    let error = match preflight_round_candidate(&session.inner, key, &assistant, &[]) {
        Ok(reservation) => {
            reservation.release();
            panic!("最坏 ToolResult 与最终 Round 超过 1 MiB 时必须预检拒绝");
        }
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        AgentToolRoundPreflightErrorKind::Unpersistable
    );
    let snapshot = session.snapshot().expect("拒绝后 Snapshot 应读取");
    assert_eq!(snapshot.state.last_sequence, 2);
    assert_eq!(snapshot.active_reservations, 0);
}

/// 验证普通消息和工具结果的大文本在 Probe 与 Commit 中生成相同 UTF-8 Artifact。
#[test]
fn large_message_and_tool_text_use_stable_utf8_artifacts() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.max_inline_text_bytes = 8;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-large-text".to_owned(),
            title: "大文本测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("大文本测试 Session 应创建");
    let key = RoundKey {
        session_id: "runtime-large-text".to_owned(),
        turn_id: "turn-large-text".to_owned(),
        agent_id: "root".to_owned(),
        model: "test-model".to_owned(),
        model_round: 1,
        segment_index: 0,
    };
    let message_text = "超过八字节的普通消息文本";
    let message = Message::text(ModelMessageRole::User, message_text);
    let mut probe = ArtifactProbe::default();
    let probed_message = map_message(
        &session.inner,
        &key,
        0,
        &message,
        ArtifactMode::Probe,
        &mut probe,
    )
    .expect("大消息文本 Probe 应成功");
    assert_eq!(probe.missing_ids.len(), 1);
    assert_eq!(
        session
            .inner
            .artifacts
            .capacity()
            .expect("Probe 后 Artifact 容量应读取")
            .committed_unique_artifacts,
        0
    );
    let message_artifact = match &probed_message.content[0] {
        MessagePart::Artifact {
            artifact,
            materialization: ArtifactMaterialization::Utf8Text,
        } => artifact.clone(),
        other => panic!("大消息文本必须映射为 UTF-8 Artifact：{other:?}"),
    };
    assert_eq!(message_artifact.media_type.as_deref(), Some("text/plain"));

    let mut commit_probe = ArtifactProbe::default();
    let committed_message = map_message(
        &session.inner,
        &key,
        0,
        &message,
        ArtifactMode::Commit,
        &mut commit_probe,
    )
    .expect("大消息文本 Commit 应成功");
    assert_eq!(committed_message, probed_message);
    assert_eq!(
        session
            .inner
            .artifacts
            .read_use(&message_artifact)
            .expect("消息 Artifact 应可读取"),
        message_text.as_bytes()
    );

    let tool_text = "超过八字节的工具执行结果";
    let result = ToolResult::text("call-large-text", tool_text, false);
    let mut tool_probe = ArtifactProbe::default();
    let probed_result = map_tool_result(
        &session.inner,
        &result,
        ArtifactMode::Probe,
        &mut tool_probe,
    )
    .expect("大工具文本 Probe 应成功");
    assert_eq!(tool_probe.missing_ids.len(), 1);
    let tool_artifact = match &probed_result.content[0] {
        ToolResultPart::Artifact {
            artifact,
            materialization: ArtifactMaterialization::Utf8Text,
        } => artifact.clone(),
        other => panic!("大工具文本必须映射为 UTF-8 Artifact：{other:?}"),
    };
    let mut tool_commit_probe = ArtifactProbe::default();
    let committed_result = map_tool_result(
        &session.inner,
        &result,
        ArtifactMode::Commit,
        &mut tool_commit_probe,
    )
    .expect("大工具文本 Commit 应成功");
    assert_eq!(committed_result, probed_result);
    assert_eq!(
        session
            .inner
            .artifacts
            .read_use(&tool_artifact)
            .expect("工具结果 Artifact 应可读取"),
        tool_text.as_bytes()
    );
}

/// 权威 Transcript 恢复必须完整物化 UTF-8 与图片 Artifact，而不是向 Provider 发送引用。
#[test]
fn model_transcript_materializes_text_and_image_artifacts() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-model-transcript-artifacts");
    let text = "需要从 Artifact 恢复的完整文本";
    let text_artifact = session
        .inner
        .artifacts
        .put(text.as_bytes(), Some("text/plain".to_owned()))
        .expect("文本 Artifact 应写入")
        .as_event_use();
    let image_bytes = [0x89, b'P', b'N', b'G'];
    let image_artifact = session
        .inner
        .artifacts
        .put(&image_bytes, Some("image/png".to_owned()))
        .expect("图片 Artifact 应写入")
        .as_event_use();
    append(
        &session,
        "event-model-transcript-text",
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-model-transcript-text".to_owned(),
                turn_id: None,
                agent_id: None,
                role: keencode_resources::MessageRole::User,
                content: vec![MessagePart::Artifact {
                    artifact: text_artifact,
                    materialization: ArtifactMaterialization::Utf8Text,
                }],
            },
        },
    );
    append(
        &session,
        "event-model-transcript-image",
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-model-transcript-image".to_owned(),
                turn_id: None,
                agent_id: None,
                role: keencode_resources::MessageRole::User,
                content: vec![MessagePart::Image {
                    source: MessageImageSource::Artifact {
                        artifact: image_artifact,
                    },
                }],
            },
        },
    );

    let transcript = session.model_transcript().expect("模型 Transcript 应物化");
    assert_eq!(transcript[0], Message::text(ModelMessageRole::User, text));
    assert_eq!(
        transcript[1],
        Message::new(
            ModelMessageRole::User,
            vec![ContentBlock::Image {
                image: ImageContent::from_base64(
                    "image/png",
                    base64::engine::general_purpose::STANDARD.encode(image_bytes),
                ),
            }],
        )
    );
}

/// Binary Artifact 只允许审计或下载，不能伪装成模型可读文本。
#[test]
fn model_transcript_rejects_binary_artifact_materialization() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-model-transcript-binary");
    let artifact = session
        .inner
        .artifacts
        .put(b"binary", Some("application/octet-stream".to_owned()))
        .expect("Binary Artifact 应写入")
        .as_event_use();
    append(
        &session,
        "event-model-transcript-binary",
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-model-transcript-binary".to_owned(),
                turn_id: None,
                agent_id: None,
                role: keencode_resources::MessageRole::User,
                content: vec![
                    MessagePart::Text {
                        text: "可见文本".to_owned(),
                    },
                    MessagePart::Artifact {
                        artifact,
                        materialization: ArtifactMaterialization::Binary,
                    },
                ],
            },
        },
    );
    assert!(matches!(
        session.model_transcript(),
        Err(RuntimeError::RecoveryRequired)
    ));
}

/// 验证无法保持推理语义的大文本会明确拒绝而不是降级成普通 Artifact。
#[test]
fn oversized_reasoning_is_rejected_instead_of_retyped() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.max_inline_text_bytes = 8;
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-large-reasoning".to_owned(),
            title: "推理文本测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("推理文本测试 Session 应创建");
    let key = RoundKey {
        session_id: "runtime-large-reasoning".to_owned(),
        turn_id: "turn-large-reasoning".to_owned(),
        agent_id: "root".to_owned(),
        model: "test-model".to_owned(),
        model_round: 1,
        segment_index: 0,
    };
    let message = Message::new(
        ModelMessageRole::Assistant,
        vec![ContentBlock::Reasoning {
            reasoning: ReasoningContent::new("超过内联限制的推理文本"),
        }],
    );
    let error = map_message(
        &session.inner,
        &key,
        0,
        &message,
        ArtifactMode::Probe,
        &mut ArtifactProbe::default(),
    )
    .expect_err("大推理文本必须拒绝");
    assert!(matches!(error, RuntimeError::ReasoningTooLarge));
}

/// 验证图片 URL 在任何 Artifact 写入前拒绝空白、超长和换行注入。
#[test]
fn image_url_persistence_validation_is_bounded_and_single_line() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-image-url");
    for invalid in [
        "   ".to_owned(),
        "https://example.invalid/image.png\r\nforged".to_owned(),
        "https://example.invalid/image image.png".to_owned(),
        "ftp://example.invalid/image.png".to_owned(),
        "https://example.invalid/图片.png".to_owned(),
        "https://".to_owned(),
        "https:///image.png".to_owned(),
        "https://user:secret@example.invalid/image.png".to_owned(),
        "https://example.invalid\\@attacker.invalid/image.png".to_owned(),
        "https://example.invalid:bad/image.png".to_owned(),
        "https://example.invalid:65536/image.png".to_owned(),
        "https://127.1/image.png".to_owned(),
        "https://127.000.000.001/image.png".to_owned(),
        "https://0x7f000001/image.png".to_owned(),
        "https://example.invalid/%GG".to_owned(),
        "https://example.invalid/%0".to_owned(),
        "x".repeat(MAX_PERSISTED_IMAGE_URL_BYTES + 1),
    ] {
        let error = map_image_source(
            &session.inner,
            &ImageSource::Url { url: invalid },
            ArtifactMode::Probe,
            &mut ArtifactProbe::default(),
        )
        .expect_err("非法图片 URL 必须在 Probe 阶段拒绝");
        assert!(matches!(error, RuntimeError::InvalidImageUrl));
    }

    let valid = "https://example.invalid/image.png".to_owned();
    assert_eq!(
        map_image_source(
            &session.inner,
            &ImageSource::Url { url: valid.clone() },
            ArtifactMode::Probe,
            &mut ArtifactProbe::default(),
        )
        .expect("有界单行 URL 应保持原样"),
        MessageImageSource::Url { url: valid }
    );
}

/// 验证非法图片媒体类型在 Probe 阶段拒绝，合法 Base64 的 Probe 与 Commit 引用一致。
#[test]
fn base64_image_media_type_is_validated_before_artifact_commit() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-base64-image");
    for invalid_media_type in [
        "text/plain",
        "image/",
        "image/png/extra",
        "image/png;charset=utf-8",
        "image/PNG",
        "image/p ng",
        "image/foo%bar",
        "image/foo'bar",
        "image/foo*bar",
        "image/foo`bar",
        "image/foo|bar",
        "image/foo~bar",
        "image/+suffix",
        "image/.hidden",
        "image/-private",
        "image/_private",
    ] {
        let error = map_image_source(
            &session.inner,
            &ImageSource::Base64 {
                media_type: invalid_media_type.to_owned(),
                data: "aGVsbG8=".to_owned(),
            },
            ArtifactMode::Probe,
            &mut ArtifactProbe::default(),
        )
        .expect_err("非法媒体类型必须在 Probe 阶段拒绝");
        assert!(matches!(error, RuntimeError::InvalidImageData));
    }

    let source = ImageSource::Base64 {
        media_type: "image/png".to_owned(),
        data: "aGVsbG8=".to_owned(),
    };
    let mut probe = ArtifactProbe::default();
    let probed = map_image_source(&session.inner, &source, ArtifactMode::Probe, &mut probe)
        .expect("合法 Base64 图片 Probe 应成功");
    assert_eq!(probe.missing_ids.len(), 1);
    let committed = map_image_source(
        &session.inner,
        &source,
        ArtifactMode::Commit,
        &mut ArtifactProbe::default(),
    )
    .expect("合法 Base64 图片 Commit 应成功");
    assert_eq!(committed, probed);
    let MessageImageSource::Artifact { artifact } = committed else {
        panic!("合法 Base64 图片必须 Artifact 化");
    };
    assert_eq!(artifact.media_type.as_deref(), Some("image/png"));
    assert_eq!(
        session
            .inner
            .artifacts
            .read_use(&artifact)
            .expect("图片 Artifact 应可读取"),
        b"hello"
    );
}

/// 验证 Agent 接受的 RFC 6838 restricted-name 图片类型可由 Runtime 无损写入 ArtifactStore。
#[test]
fn agent_image_media_type_contract_matches_runtime_artifact_store() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-media-type-contract");
    for (media_type, data, expected) in [
        ("image/vnd.test+json", "AA==", 0_u8),
        ("image/x.foo-bar_2", "AQ==", 1_u8),
        ("image/a!#$&^z", "Ag==", 2_u8),
    ] {
        assert!(keencode_agent::is_canonical_image_media_type(media_type));
        let source = ImageSource::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        };
        let mut probe = ArtifactProbe::default();
        let probed = map_image_source(&session.inner, &source, ArtifactMode::Probe, &mut probe)
            .expect("Agent 接受的 restricted-name 媒体类型应通过 Runtime Probe");
        let committed = map_image_source(
            &session.inner,
            &source,
            ArtifactMode::Commit,
            &mut ArtifactProbe::default(),
        )
        .expect("Agent 接受的 restricted-name 媒体类型应写入 ArtifactStore");
        assert_eq!(committed, probed);
        let MessageImageSource::Artifact { artifact } = committed else {
            panic!("内联图片必须 Artifact 化");
        };
        assert_eq!(artifact.media_type.as_deref(), Some(media_type));
        assert_eq!(
            session
                .inner
                .artifacts
                .read_use(&artifact)
                .expect("跨层媒体类型 Artifact 应可读取"),
            [expected]
        );
    }
    for media_type in [
        "image/foo%bar",
        "image/foo'bar",
        "image/foo*bar",
        "image/foo`bar",
        "image/foo|bar",
        "image/foo~bar",
    ] {
        assert!(!keencode_agent::is_canonical_image_media_type(media_type));
    }
}

/// 验证 data URL 与 Base64 图片采用同一严格解析及 Artifact 持久化路径。
#[test]
fn data_image_url_is_artifactized_with_stable_probe_and_commit_reference() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-data-url-image");
    for invalid in [
        "data:text/plain;base64,aGVsbG8=",
        "data:image/;base64,aGVsbG8=",
        "data:image/png/extra;base64,aGVsbG8=",
        "data:image/png;charset=utf-8;base64,aGVsbG8=",
        "data:image/PNG;base64,aGVsbG8=",
        "data:image/png,aGVsbG8=",
        "data:image/png;base64;base64,aGVsbG8=",
        "data:image/png;base64,not-base64",
    ] {
        let error = map_image_source(
            &session.inner,
            &ImageSource::Url {
                url: invalid.to_owned(),
            },
            ArtifactMode::Probe,
            &mut ArtifactProbe::default(),
        )
        .expect_err("非法 data URL 必须在 Probe 阶段拒绝");
        assert!(matches!(error, RuntimeError::InvalidImageData));
    }

    let source = ImageSource::Url {
        url: "data:image/png;base64,aGVsbG8=".to_owned(),
    };
    let mut probe = ArtifactProbe::default();
    let probed = map_image_source(&session.inner, &source, ArtifactMode::Probe, &mut probe)
        .expect("合法 data URL Probe 应成功");
    assert_eq!(probe.missing_ids.len(), 1);
    let committed = map_image_source(
        &session.inner,
        &source,
        ArtifactMode::Commit,
        &mut ArtifactProbe::default(),
    )
    .expect("合法 data URL Commit 应成功");
    assert_eq!(committed, probed);
    assert!(matches!(committed, MessageImageSource::Artifact { .. }));
}

/// 验证损坏日志只返回只读报告，不把部分状态包装成可提交 RuntimeSession。
#[test]
fn corrupt_log_opens_read_only() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-corrupt");
    let log_path = session.inner.journal.log_path().to_owned();
    drop(session);
    let mut log = OpenOptions::new()
        .append(true)
        .open(log_path)
        .expect("事件日志应打开");
    log.write_all(b"{invalid-json}\n")
        .expect("损坏测试行应写入");
    drop(log);

    match RuntimeSession::open_session(config(&root), "runtime-corrupt")
        .expect("损坏属于显式打开结果")
    {
        OpenSessionResult::Ready(_) => panic!("损坏日志不得产生可提交 Runtime"),
        OpenSessionResult::Corrupt(report) => {
            assert_eq!(report.valid_records, 1);
            assert_eq!(report.last_valid_state.last_sequence, 1);
            assert!(!report.issues.is_empty());
        }
    }
}

/// 验证 Snapshot 写入失败只降级缓存状态，已经追加的 Journal 事件仍返回成功并推进状态。
#[test]
fn snapshot_failure_does_not_reclassify_committed_journal_event() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.journal.snapshot_policy = SnapshotPolicy::Every { events: 3 };
    let session = RuntimeSession::create_session(
        runtime_config,
        CreateSessionRequest {
            session_id: "runtime-snapshot-failure".to_owned(),
            title: "Snapshot 失败测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("Snapshot 失败测试 Session 应创建");
    start_turn(&session, "turn-snapshot-failure", 0);
    std::fs::create_dir(session.inner.journal.snapshot_path())
        .expect("Snapshot 目标目录应制造可恢复缓存写入失败");
    append(
        &session,
        "event-snapshot-failure-committed",
        SessionEvent::SessionRenamed {
            title: "Journal 已提交".to_owned(),
        },
    );
    let snapshot = session.snapshot().expect("Journal 提交后的状态应读取");
    assert_eq!(snapshot.state.last_sequence, 3);
    assert_eq!(snapshot.state.title, "Journal 已提交");
    assert!(!snapshot.recovery_required);
}

/// 验证冷恢复严格按终端退出、未知副作用、Transcript 物化和 Turn 停止收敛。
#[test]
fn cold_recovery_orders_terminal_tool_transcript_and_turn() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-cold-recovery");
    let session_id = session.session_id().clone();
    let turn_id = keencode_resources::TurnId::new("turn-recovery").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    append(
        &session,
        "event-turn",
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证冷恢复".to_owned(),
        },
    );
    let request_id = keencode_resources::RequestId::derive_model_tool_call(
        &session_id,
        &turn_id,
        &agent_id,
        1,
        "call-shell",
    )
    .expect("请求 ID 应派生");
    append(
        &session,
        "event-tool-request",
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id: agent_id.clone(),
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-shell".to_owned(),
                tool_name: "Shell".to_owned(),
                arguments: serde_json::json!({"command": "test"}),
                effect: ToolEffect::ChangesState,
            },
        },
    );
    append(
        &session,
        "event-tool-started",
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    let terminal_id =
        keencode_resources::TerminalId::new("terminal-recovery").expect("Terminal ID 应有效");
    append(
        &session,
        "event-terminal-started",
        SessionEvent::TerminalStarted {
            terminal: TerminalRecord {
                terminal_id: terminal_id.clone(),
                request_id: request_id.clone(),
                command_display: "test".to_owned(),
                working_directory: root.path().display().to_string(),
                output_artifacts: Vec::new(),
                exit_code: None,
                cancelled: false,
                exited: false,
            },
        },
    );
    let log_path = session.inner.journal.log_path().to_owned();
    drop(session);

    let recovered = match RuntimeSession::open_session(config(&root), "runtime-cold-recovery")
        .expect("遗留状态应自动保守恢复")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("合法遗留状态不应损坏：{:?}", report.issues)
        }
    };
    let state = recovered.snapshot().expect("恢复状态应读取").state;
    let terminal = state.terminals.get(&terminal_id).expect("终端记录应保留");
    assert!(terminal.exited);
    assert!(terminal.cancelled);
    assert_eq!(terminal.exit_code, None);
    let tool = state.tools.get(&request_id).expect("工具记录应保留");
    assert_eq!(
        tool.outcome.as_ref().map(|outcome| outcome.status),
        Some(ToolCompletionStatus::SideEffectUnknown)
    );
    assert!(tool.transcript_segment.is_some());
    let turn = state.turns.get(&turn_id).expect("Turn 应保留");
    assert_eq!(turn.status, TurnStatus::Failed);

    let records = std::fs::read_to_string(log_path)
        .expect("恢复日志应读取")
        .lines()
        .map(|line| {
            serde_json::from_str::<SessionEventRecord>(line).expect("每行应为类型化事件记录")
        })
        .collect::<Vec<_>>();
    let terminal_position = records
        .iter()
        .position(|record| matches!(&record.event, SessionEvent::TerminalExited { .. }))
        .expect("应记录 TerminalExited");
    let unknown_position = records
        .iter()
        .position(|record| matches!(&record.event, SessionEvent::ToolSideEffectUnknown { .. }))
        .expect("应记录 ToolSideEffectUnknown");
    let transcript_position = records
        .iter()
        .rposition(|record| {
            matches!(
                &record.event,
                SessionEvent::AtomicBatch { events }
                    if events.iter().any(|event| {
                        matches!(event, SessionEvent::ModelRoundCompleted { .. })
                    }) && events.iter().any(|event| {
                        matches!(event, SessionEvent::TranscriptSegmentCommitted { .. })
                    })
            )
        })
        .expect("应在同一 AtomicBatch 记录恢复模型 Round 与 Transcript");
    let stopped_position = records
        .iter()
        .position(|record| matches!(&record.event, SessionEvent::TurnStopped { .. }))
        .expect("应记录 TurnStopped");
    assert!(terminal_position < unknown_position);
    assert!(unknown_position < transcript_position);
    assert!(transcript_position < stopped_position);
}

/// 验证子 Agent 遗留 Turn 与 Failed 状态在同一物理事件内原子收敛。
#[test]
fn cold_recovery_pairs_child_turn_and_agent_state() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-child-recovery");
    let root_turn = keencode_resources::TurnId::new("turn-root").expect("根 Turn 应有效");
    let child_turn = keencode_resources::TurnId::new("turn-child").expect("子 Turn 应有效");
    let root_agent = AgentId::new("root").expect("根 Agent 应有效");
    let child_agent = AgentId::new("child-one").expect("子 Agent 应有效");
    append(
        &session,
        "event-root-turn",
        SessionEvent::TurnStarted {
            turn_id: root_turn.clone(),
            source_agent_id: root_agent.clone(),
            root_turn_id: root_turn.clone(),
            parent_turn_id: None,
            prompt_summary: "根任务".to_owned(),
        },
    );
    append(
        &session,
        "event-child-spawn",
        SessionEvent::SubAgentSpawned {
            agent: keencode_resources::SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent,
                agent_path: "/root/child_recovery".to_owned(),
                task: "子任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    );
    append(
        &session,
        "event-child-start",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "执行子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        },
    );
    drop(session);

    let recovered = match RuntimeSession::open_session(config(&root), "runtime-child-recovery")
        .expect("子 Agent 遗留状态应恢复")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("合法子 Agent 状态不应损坏：{:?}", report.issues)
        }
    };
    let state = recovered.snapshot().expect("恢复 Snapshot 应读取").state;
    assert_eq!(
        state.turns.get(&child_turn).map(|turn| &turn.status),
        Some(&TurnStatus::Failed)
    );
    assert_eq!(
        state
            .sub_agents
            .get(&child_agent)
            .map(|agent| &agent.status),
        Some(&SubAgentStatus::Failed)
    );
    assert_eq!(
        state.turns.get(&root_turn).map(|turn| &turn.status),
        Some(&TurnStatus::Failed)
    );
}

/// 验证尚未来得及创建 Turn 的 Pending 子 Agent 会在冷恢复末尾停止并允许关闭 Session。
#[test]
fn cold_recovery_stops_pending_sub_agent_without_turn() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "runtime-pending-child-recovery");
    let root_agent = AgentId::new("root").expect("根 Agent 应有效");
    let child_agent = AgentId::new("child-pending").expect("子 Agent 应有效");
    append(
        &session,
        "event-pending-child-spawn",
        SessionEvent::SubAgentSpawned {
            agent: keencode_resources::SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent,
                agent_path: "/root/child_pending".to_owned(),
                task: "尚未开始的子任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    );
    drop(session);

    let recovered =
        match RuntimeSession::open_session(config(&root), "runtime-pending-child-recovery")
            .expect("Pending 子 Agent 遗留状态应恢复")
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(report) => {
                panic!("合法 Pending 子 Agent 状态不应损坏：{:?}", report.issues)
            }
        };
    let state = recovered.snapshot().expect("恢复状态应读取").state;
    let agent = state
        .sub_agents
        .get(&child_agent)
        .expect("Pending 子 Agent 记录应保留");
    assert_eq!(agent.status, SubAgentStatus::Stopped);
    assert_eq!(agent.current_turn_id, None);
    assert_eq!(agent.result_summary, None);
    append(
        &recovered,
        "event-close-after-pending-recovery",
        SessionEvent::SessionClosed {},
    );
    assert_eq!(
        recovered.snapshot().expect("关闭后状态应读取").state.status,
        SessionStatus::Closed
    );
}

/// 验证 Manager 原子拒绝进程内重复注册，并在关闭唯一持有后允许重新打开。
#[test]
fn runtime_manager_rejects_duplicate_registration_and_reopens_after_close() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建");
    let session = manager
        .create(manager_create_request(&root, "manager-registration"))
        .expect("Manager 应创建 Session");
    assert_eq!(
        manager
            .get("manager-registration")
            .expect("Manager 应返回已注册 Session")
            .session_id(),
        session.session_id()
    );
    assert!(matches!(
        manager.create(manager_create_request(&root, "manager-registration")),
        Err(RuntimeError::SessionAlreadyRegistered)
    ));
    assert!(matches!(
        manager.open("manager-registration"),
        Err(RuntimeError::SessionAlreadyRegistered)
    ));
    assert!(matches!(
        manager.get("manager-unknown"),
        Err(RuntimeError::SessionNotRegistered)
    ));
    drop(session);
    manager
        .close("manager-registration")
        .expect("Manager 应关闭已注册 Session");
    assert!(matches!(
        manager.close("manager-registration"),
        Err(RuntimeError::SessionNotRegistered)
    ));

    let reopened = match manager
        .open("manager-registration")
        .expect("关闭后应允许重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("健康 Session 不应损坏：{:?}", report.issues)
        }
    };
    assert_eq!(reopened.session_id().as_str(), "manager-registration");
}

/// 验证 close_all 可以跨越已经关闭的登记句柄并在重试时保持幂等。
#[test]
fn runtime_manager_close_all_is_idempotent_after_partial_close() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建");
    let session_a = manager
        .create(manager_create_request(&root, "close-all-session-a"))
        .expect("Session A 应创建");
    let session_b = manager
        .create(manager_create_request(&root, "close-all-session-b"))
        .expect("Session B 应创建");

    session_a
        .close_runtime()
        .expect("预先关闭 Session A 应成功");
    manager.close_all().expect("close_all 应跨越已关闭 Session");

    assert!(
        manager
            .registered_session_ids()
            .expect("注册表应可读")
            .is_empty()
    );
    assert!(session_a.snapshot().expect("Session A 快照应可读").closed);
    assert!(session_b.snapshot().expect("Session B 快照应可读").closed);
    manager.close_all().expect("重复 close_all 应保持幂等");
}

/// 验证两个线程同时创建相同 Session 时只有一个完成进程内注册。
#[test]
fn runtime_manager_serializes_concurrent_duplicate_create() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = Arc::new(RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建"));
    let barrier = Arc::new(Barrier::new(3));
    let first_manager = manager.clone();
    let first_barrier = barrier.clone();
    let first_root = root.path().display().to_string();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_manager.create(CreateSessionRequest {
            session_id: "manager-create-race".to_owned(),
            title: "并发创建一".to_owned(),
            project_root: first_root,
        })
    });
    let second_manager = manager.clone();
    let second_barrier = barrier.clone();
    let second_root = root.path().display().to_string();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_manager.create(CreateSessionRequest {
            session_id: "manager-create-race".to_owned(),
            title: "并发创建二".to_owned(),
            project_root: second_root,
        })
    });
    barrier.wait();
    let first = first.join().expect("首个创建线程不应异常");
    let second = second.join().expect("次个创建线程不应异常");
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        usize::from(matches!(first, Err(RuntimeError::SessionAlreadyRegistered)))
            + usize::from(matches!(
                second,
                Err(RuntimeError::SessionAlreadyRegistered)
            )),
        1
    );
}

/// 验证 Runtime 覆盖调用方取消令牌，并按 Session 与 Turn 精确、幂等地取消执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_manager_cancellation_is_authoritative_idempotent_and_session_scoped() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = Arc::new(RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建"));
    let session_a = manager
        .create(manager_create_request(&root, "cancel-session-a"))
        .expect("Session A 应创建");
    let session_b = manager
        .create(manager_create_request(&root, "cancel-session-b"))
        .expect("Session B 应创建");
    let gate_a = Arc::new(FirstModelEventGate::new());
    let gate_b = Arc::new(FirstModelEventGate::new());
    let provider_a = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("Session A 响应")],
    ));
    let provider_b = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [completed_text_reply("Session B 响应")],
    ));
    let runner_a = Arc::new(
        session_a.bind_agent_runner(
            AgentRunner::new(provider_a, ToolRegistry::new(), RunLimits::default())
                .with_event_sink(gate_a.clone()),
        ),
    );
    let runner_b = Arc::new(
        session_b.bind_agent_runner(
            AgentRunner::new(provider_b, ToolRegistry::new(), RunLimits::default())
                .with_event_sink(gate_b.clone()),
        ),
    );
    let turn_id = "same-turn-id";
    let request_a = root_runtime_turn(&session_a, turn_id, "运行 Session A");
    let caller_token = request_a.request.cancellation().clone();
    let request_b = root_runtime_turn(&session_b, turn_id, "运行 Session B");
    let task_runner_a = runner_a.clone();
    let task_a = tokio::spawn(async move { task_runner_a.run_turn(request_a).await });
    let task_runner_b = runner_b.clone();
    let mut task_b = tokio::spawn(async move { task_runner_b.run_turn(request_b).await });
    gate_a.wait_until_entered().await;
    gate_b.wait_until_entered().await;

    caller_token.cancel();
    assert_eq!(
        manager
            .cancel_turn("cancel-session-a", turn_id)
            .expect("请求令牌取消后管理器取消应幂等"),
        TurnCancellationOutcome::AlreadyRequested
    );
    let result_a = task_a
        .await
        .expect("Session A 任务不应异常")
        .expect("Session A 取消终态应持久化");
    assert_eq!(
        result_a.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(
        manager
            .cancel_turn("cancel-session-a", turn_id)
            .expect("终态后取消应返回稳定结果"),
        TurnCancellationOutcome::NotRunning
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut task_b)
            .await
            .is_err(),
        "取消 Session A 不得影响相同 Turn ID 的 Session B"
    );
    assert!(matches!(
        manager.cancel_turn("cancel-session-missing", turn_id),
        Err(RuntimeError::SessionNotRegistered)
    ));
    gate_b.release();
    assert!(
        task_b
            .await
            .expect("Session B 任务不应异常")
            .expect("Session B Turn 应完成")
            .is_success()
    );
}

/// 验证 Manager close 使旧句柄失效、取消所属活跃 Turn，且不影响其他 Session。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_manager_close_cancels_only_owned_work_and_fences_old_handles() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = Arc::new(RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建"));
    let session_a = manager
        .create(manager_create_request(&root, "close-session-a"))
        .expect("Session A 应创建");
    let session_b = manager
        .create(manager_create_request(&root, "close-session-b"))
        .expect("Session B 应创建");
    let mut close_subscription = session_a.subscribe().expect("Session A 应订阅");
    let gate_a = Arc::new(FirstModelEventGate::new());
    let gate_b = Arc::new(FirstModelEventGate::new());
    let runner_a = Arc::new(
        session_a.bind_agent_runner(
            AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities {
                        streaming: true,
                        ..ProviderCapabilities::default()
                    },
                    [completed_text_reply("A 不应正常完成")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            )
            .with_event_sink(gate_a.clone()),
        ),
    );
    let runner_b = Arc::new(
        session_b.bind_agent_runner(
            AgentRunner::new(
                Arc::new(ScriptedProvider::new(
                    ProviderCapabilities {
                        streaming: true,
                        ..ProviderCapabilities::default()
                    },
                    [completed_text_reply("B 正常完成")],
                )),
                ToolRegistry::new(),
                RunLimits::default(),
            )
            .with_event_sink(gate_b.clone()),
        ),
    );
    let task_runner_a = runner_a.clone();
    let task_session_a = session_a.clone();
    let task_a = tokio::spawn(async move {
        task_runner_a
            .run_turn(root_runtime_turn(
                &task_session_a,
                "close-running-a",
                "关闭时取消 A",
            ))
            .await
    });
    let task_runner_b = runner_b.clone();
    let task_session_b = session_b.clone();
    let mut task_b = tokio::spawn(async move {
        task_runner_b
            .run_turn(root_runtime_turn(
                &task_session_b,
                "close-running-b",
                "B 不受影响",
            ))
            .await
    });
    gate_a.wait_until_entered().await;
    gate_b.wait_until_entered().await;

    manager.close("close-session-a").expect("Session A 应关闭");
    assert!(session_a.snapshot().expect("关闭状态应可读取").closed);
    let result_a = task_a
        .await
        .expect("Session A 任务不应异常")
        .expect("Session A 应提交取消终态");
    assert_eq!(
        result_a.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    let close_deliveries = drain_runtime_events(&mut close_subscription).await;
    assert!(matches!(
        close_deliveries.last().map(|delivery| &delivery.payload),
        Some(RuntimeEventPayload::Control(
            RuntimeControlEvent::SessionClosed
        ))
    ));
    assert!(matches!(
        close_subscription.recv().await,
        Err(RuntimeEventReceiveError::Closed)
    ));
    assert!(matches!(
        session_a.subscribe(),
        Err(RuntimeError::SessionClosed)
    ));
    assert!(matches!(
        runner_a
            .run_turn(root_runtime_turn(
                &session_a,
                "close-old-handle-turn",
                "旧句柄不得执行",
            ))
            .await,
        Err(RuntimeError::SessionClosed)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut task_b)
            .await
            .is_err(),
        "关闭 Session A 不得取消 Session B"
    );
    gate_b.release();
    assert!(
        task_b
            .await
            .expect("Session B 任务不应异常")
            .expect("Session B Turn 应完成")
            .is_success()
    );
}

/// 验证统一投递序列先发 Turn 权威起点，再发临时流，最后发权威终态。
#[tokio::test]
async fn runtime_delivery_orders_authoritative_and_transient_payloads() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "delivery-payload-order");
    let mut subscription = session.subscribe().expect("Session 应订阅");
    let runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("顺序响应")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        runner
            .run_turn(root_runtime_turn(
                &session,
                "delivery-payload-order-turn",
                "验证统一顺序",
            ))
            .await
            .expect("Turn 应执行")
            .is_success()
    );
    let deliveries = drain_runtime_events(&mut subscription).await;
    assert!(deliveries.len() >= 6);
    assert!(
        deliveries
            .windows(2)
            .all(|pair| pair[1].delivery_sequence == pair[0].delivery_sequence + 1)
    );
    assert!(matches!(
        deliveries.first().map(|delivery| &delivery.payload),
        Some(RuntimeEventPayload::Authoritative(SessionEventRecord {
            event: SessionEvent::AtomicBatch { .. },
            ..
        }))
    ));
    assert!(
        deliveries
            .iter()
            .any(|delivery| matches!(delivery.payload, RuntimeEventPayload::Transient(_)))
    );
    assert!(matches!(
        deliveries.last().map(|delivery| &delivery.payload),
        Some(RuntimeEventPayload::Authoritative(SessionEventRecord {
            event: SessionEvent::TurnCompleted { .. },
            ..
        }))
    ));
}

/// 验证下游拒绝临时事件时 Publisher 不会提前暴露该临时载荷。
#[tokio::test]
async fn runtime_publisher_emits_no_transient_when_downstream_rejects() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "delivery-downstream-reject");
    let mut subscription = session.subscribe().expect("Session 应订阅");
    let runner = session.bind_agent_runner(
        AgentRunner::new(
            Arc::new(ScriptedProvider::new(
                ProviderCapabilities {
                    streaming: true,
                    ..ProviderCapabilities::default()
                },
                [completed_text_reply("不得发布")],
            )),
            ToolRegistry::new(),
            RunLimits::default(),
        )
        .with_event_sink(Arc::new(RejectLiveEventSink)),
    );
    assert!(
        !runner
            .run_turn(root_runtime_turn(
                &session,
                "delivery-downstream-reject-turn",
                "下游拒绝",
            ))
            .await
            .expect("失败终态仍应提交")
            .is_success()
    );
    let deliveries = drain_runtime_events(&mut subscription).await;
    assert!(!deliveries.is_empty());
    assert!(
        deliveries
            .iter()
            .all(|delivery| { matches!(delivery.payload, RuntimeEventPayload::Authoritative(_)) })
    );
}

/// 验证可见但结果不确定的 Journal 新追加只发布一次，热对账不重复投递。
#[tokio::test]
async fn runtime_authoritative_delivery_deduplicates_already_committed_retry() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create(&root, "delivery-authoritative-dedup");
    let mut subscription = session.subscribe().expect("Session 应订阅");
    let turn_id = "delivery-authoritative-dedup-turn";
    let start_event_id = runtime_lifecycle_event_id(
        session.session_id(),
        &keencode_resources::TurnId::new(turn_id).expect("Turn ID 应有效"),
        "turn-started",
    )
    .expect("起点事件 ID 应派生");
    inject_runtime_lifecycle_visible_indeterminate(&start_event_id);
    let runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("唯一响应")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(matches!(
        runner
            .run_turn(root_runtime_turn(&session, turn_id, "首次结果不确定"))
            .await,
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(
        runner
            .run_turn(root_runtime_turn(&session, turn_id, "首次结果不确定"))
            .await
            .expect("相同请求应完成热对账")
            .is_success()
    );
    let deliveries = drain_runtime_events(&mut subscription).await;
    assert_eq!(
        deliveries
            .iter()
            .filter(|delivery| {
                matches!(
                    &delivery.payload,
                    RuntimeEventPayload::Authoritative(record)
                        if record.event_id == start_event_id
                )
            })
            .count(),
        1
    );
}

/// 验证每个 Session 的实时投递序号独立从一开始严格递增。
#[tokio::test]
async fn runtime_live_delivery_sequences_are_independent_per_session() {
    let root = TempDir::new().expect("临时目录应创建");
    let manager = RuntimeManager::new(config(&root)).expect("RuntimeManager 应创建");
    let session_a = manager
        .create(manager_create_request(&root, "delivery-session-a"))
        .expect("Session A 应创建");
    let session_b = manager
        .create(manager_create_request(&root, "delivery-session-b"))
        .expect("Session B 应创建");
    let mut subscriber_a = session_a.subscribe().expect("Session A 应订阅");
    let mut subscriber_b = session_b.subscribe().expect("Session B 应订阅");
    let runner_a = session_a.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("A")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let runner_b = session_b.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("B")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        runner_a
            .run_turn(root_runtime_turn(&session_a, "delivery-turn-a", "A"))
            .await
            .expect("Session A Turn 应执行")
            .is_success()
    );
    assert!(
        runner_b
            .run_turn(root_runtime_turn(&session_b, "delivery-turn-b", "B"))
            .await
            .expect("Session B Turn 应执行")
            .is_success()
    );
    let first_a = subscriber_a.recv().await.expect("Session A 首事件应收到");
    let second_a = subscriber_a.recv().await.expect("Session A 次事件应收到");
    let first_b = subscriber_b.recv().await.expect("Session B 首事件应收到");
    assert_eq!(first_a.delivery_sequence, 1);
    assert_eq!(second_a.delivery_sequence, 2);
    assert_eq!(first_b.delivery_sequence, 1);
    assert_eq!(first_a.session_id(), "delivery-session-a");
    assert_eq!(first_b.session_id(), "delivery-session-b");
    let first_a_clone = first_a.clone();
    assert!(Arc::ptr_eq(&first_a.session_id, &second_a.session_id));
    assert!(Arc::ptr_eq(&first_a.session_id, &first_a_clone.session_id));
    assert!(!Arc::ptr_eq(&first_a.session_id, &first_b.session_id));
}

/// 验证慢订阅者收到显式丢失范围和 Snapshot 加 Journal 重放指令。
#[tokio::test]
async fn runtime_live_subscription_reports_lag_and_catch_up_contract() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut runtime_config = config(&root);
    runtime_config.live_event_capacity = 1;
    let session = RuntimeSession::create_session(
        runtime_config,
        manager_create_request(&root, "delivery-lag"),
    )
    .expect("Session 应创建");
    let mut subscriber = session.subscribe().expect("实时事件应订阅");
    let runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("产生多个事件")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        runner
            .run_turn(root_runtime_turn(&session, "delivery-lag-turn", "触发丢失"))
            .await
            .expect("Turn 应执行")
            .is_success()
    );
    let lag = match subscriber.recv().await {
        Err(RuntimeEventReceiveError::Lagged(lag)) => lag,
        other => panic!("慢订阅者应收到显式 Lagged，实际为 {other:?}"),
    };
    assert!(lag.missed_events >= 1);
    assert_eq!(lag.first_missed_delivery_sequence, 1);
    assert_eq!(lag.last_missed_delivery_sequence, lag.missed_events);
    assert_eq!(
        lag.catch_up,
        RuntimeCatchUpDirective::ReloadSnapshotAndReplayJournal
    );
    let latest = subscriber
        .recv()
        .await
        .expect("Lagged 后应继续收到当前缓冲事件");
    assert_eq!(
        latest.delivery_sequence,
        lag.last_missed_delivery_sequence + 1
    );
    let caught_up = session.snapshot().expect("Lag 后 Snapshot 应读取");
    let replay = session.replay(None, 100).expect("Lag 后 Journal 应重放");
    assert_eq!(replay.through_sequence, caught_up.state.last_sequence);
    assert_eq!(
        replay.records.last().map(|record| record.sequence),
        Some(caught_up.state.last_sequence)
    );

    append_runtime_resource_event(
        &session.inner,
        SessionEventId::new("delivery-after-catch-up").expect("事件 ID 应有效"),
        SessionEvent::SessionRenamed {
            title: "追赶后继续收敛".to_owned(),
        },
    )
    .expect("追赶后的权威事件应追加");
    let after_catch_up = subscriber.recv().await.expect("追赶后应继续接收权威事件");
    assert!(matches!(
        after_catch_up.payload,
        RuntimeEventPayload::Authoritative(SessionEventRecord {
            event: SessionEvent::SessionRenamed { .. },
            ..
        })
    ));
}

/// 验证 Runtime 只暴露有界类型化重放，并可与冷打开 Snapshot 对账。
#[tokio::test]
async fn runtime_snapshot_and_paged_replay_match_cold_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session = RuntimeSession::create_session(
        runtime_config.clone(),
        manager_create_request(&root, "runtime-paged-replay"),
    )
    .expect("Session 应创建");
    let runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply("持久化响应")],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        runner
            .run_turn(root_runtime_turn(
                &session,
                "runtime-paged-replay-turn",
                "验证重放",
            ))
            .await
            .expect("Turn 应执行")
            .is_success()
    );
    let live = session.snapshot().expect("实时 Snapshot 应读取");
    let mut after = None;
    let mut sequences = Vec::new();
    loop {
        let page = session.replay(after, 1).expect("Runtime 重放页应读取");
        sequences.extend(page.records.iter().map(|record| record.sequence));
        if !page.has_more {
            assert_eq!(page.through_sequence, live.state.last_sequence);
            break;
        }
        after = page.next_after;
    }
    assert_eq!(
        sequences,
        (1..=live.state.last_sequence).collect::<Vec<_>>()
    );
    drop(runner);
    drop(session);
    let reopened = match RuntimeSession::open_session(runtime_config, "runtime-paged-replay")
        .expect("Session 应冷打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("健康 Session 不应损坏：{:?}", report.issues)
        }
    };
    assert_eq!(
        reopened.snapshot().expect("冷恢复 Snapshot 应读取").state,
        live.state
    );
}

/// 验证普通完成 Round 保留请求模型、元数据、结束原因和全未知 TokenUsage。
#[tokio::test]
async fn runtime_completed_round_preserves_unknown_usage_and_reopens_identically() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session = RuntimeSession::create_session(
        runtime_config.clone(),
        manager_create_request(&root, "runtime-round-unknown-usage"),
    )
    .expect("Session 应创建");
    let metadata = ResponseMetadata {
        response_id: Some("response-unknown-usage".to_owned()),
        model: Some("provider-actual-model".to_owned()),
    };
    let runner = session.bind_agent_runner(AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                ..ProviderCapabilities::default()
            },
            [completed_text_reply_with_facts(
                "未知用量",
                metadata.clone(),
                None,
            )],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    assert!(
        runner
            .run_turn(root_runtime_turn(
                &session,
                "runtime-round-unknown-turn",
                "验证未知用量",
            ))
            .await
            .expect("Turn 应执行")
            .is_success()
    );
    let live = session.snapshot().expect("实时 Snapshot 应读取").state;
    assert_eq!(live.model_rounds.len(), 1);
    let round = &live.model_rounds[0];
    assert_eq!(round.model_round, 1);
    assert_eq!(round.requested_model, "test-model");
    assert_eq!(round.metadata, metadata);
    assert_eq!(round.stop_reason, StopReason::Completed);
    assert_eq!(round.usage, TokenUsage::unknown());
    drop(runner);
    drop(session);

    let reopened = match RuntimeSession::open_session(runtime_config, "runtime-round-unknown-usage")
        .expect("Session 应冷打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("健康 Session 不应损坏：{:?}", report.issues)
        }
    };
    assert_eq!(
        reopened.snapshot().expect("冷恢复 Snapshot 应读取").state,
        live
    );
}

/// 尚未启动 Turn 首次取消必须在单条 Runtime 原子批次中补齐子 Agent 生命周期。
#[test]
fn unstarted_turn_termination_commits_initial_lifecycle_and_is_idempotent() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-waiting-capacity-initial");
    start_turn(&session, "root-waiting-initial", 0);
    let request = unstarted_turn_termination_request(
        "child-waiting-initial",
        "child-waiting",
        "root-waiting-initial",
        "root-waiting-initial",
        "initial task from last turn",
        "initial task from last turn",
        true,
    );

    assert_eq!(
        session
            .record_unstarted_turn_termination(request.clone())
            .expect("首次未启动 Turn 取消应写入 Journal"),
        UnstartedTurnTerminationOutcome::Committed
    );
    let records = journal_records(&session);
    let batches = records
        .iter()
        .filter_map(|record| match &record.event {
            SessionEvent::AtomicBatch { events } => Some(events),
            _ => None,
        })
        .collect::<Vec<_>>();
    let lifecycle = batches
        .iter()
        .find(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    SessionEvent::TurnStopped {
                        turn_id,
                        reason: TurnStopReason::Cancelled,
                        ..
                    } if turn_id.as_str() == "child-waiting-initial"
                )
            })
        })
        .expect("应存在未启动 Turn 取消原子批次");
    assert_eq!(lifecycle.len(), 5);
    assert!(matches!(lifecycle[0], SessionEvent::SubAgentSpawned { .. }));
    assert!(lifecycle.iter().any(|event| matches!(
        event,
        SessionEvent::SubAgentStatusChanged {
            status: SubAgentStatus::Running,
            ..
        }
    )));
    assert!(lifecycle.iter().any(|event| matches!(
        event,
        SessionEvent::SubAgentStatusChanged {
            status: SubAgentStatus::Interrupted,
            ..
        }
    )));
    assert!(!lifecycle.iter().any(|event| matches!(
        event,
        SessionEvent::MessageAdded { .. }
            | SessionEvent::ModelRoundCompleted { .. }
            | SessionEvent::TranscriptSegmentCommitted { .. }
    )));
    let state = session.snapshot().expect("状态应读取").state;
    assert_eq!(
        state
            .sub_agents
            .get(&AgentId::new("child-waiting").expect("Agent ID 应有效"))
            .expect("子 Agent 应存在")
            .status,
        SubAgentStatus::Interrupted
    );
    assert_eq!(
        session
            .record_unstarted_turn_termination(request)
            .expect("相同未启动 Turn 取消应幂等重试"),
        UnstartedTurnTerminationOutcome::AlreadyCommitted
    );
    assert_eq!(
        journal_records(&session)
            .iter()
            .filter(|record| matches!(record.event, SessionEvent::AtomicBatch { .. }))
            .count(),
        1,
        "幂等重试不得追加第二个生命周期批次"
    );
}

/// 后续 Followup/Retry 必须复用 Journal 中既有 Agent 定义且不重复写入 Spawned。
#[test]
fn unstarted_turn_termination_reuses_existing_agent_definition() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-waiting-capacity-followup");
    register_pending_child(&session, "root-waiting-followup", "child-waiting-followup");
    let request = unstarted_turn_termination_request(
        "child-waiting-followup-turn",
        "child-waiting-followup",
        "root-waiting-followup",
        "root-waiting-followup",
        "子任务",
        "followup summary",
        false,
    );
    assert_eq!(
        session
            .record_unstarted_turn_termination(request)
            .expect("既有子 Agent 的未启动 Turn 取消应写入 Journal"),
        UnstartedTurnTerminationOutcome::Committed
    );
    let state = session.snapshot().expect("状态应读取").state;
    assert_eq!(
        state
            .sub_agents
            .get(&AgentId::new("child-waiting-followup").expect("Agent ID 应有效"))
            .expect("子 Agent 应存在")
            .task,
        "子任务",
        "Followup/Retry 请求必须与 Journal 的任务定义一致"
    );
    let records = journal_records(&session);
    let batch = records
        .iter()
        .find_map(|record| match &record.event {
            SessionEvent::AtomicBatch { events }
                if events.iter().any(|event| {
                    matches!(
                        event,
                        SessionEvent::TurnStopped {
                            turn_id,
                            reason: TurnStopReason::Cancelled,
                            ..
                        } if turn_id.as_str() == "child-waiting-followup-turn"
                    )
                }) =>
            {
                Some(events)
            }
            _ => None,
        })
        .expect("应存在 Followup 取消批次");
    assert_eq!(batch.len(), 4);
    assert!(
        !batch
            .iter()
            .any(|event| matches!(event, SessionEvent::SubAgentSpawned { .. }))
    );
}

/// 相同 Turn 的不同正文必须硬冻结 Runtime，不能静默接受幂等冲突。
#[test]
fn unstarted_turn_termination_conflict_and_running_state_fail_closed() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-waiting-capacity-conflict");
    start_turn(&session, "root-waiting-conflict", 0);
    let request = unstarted_turn_termination_request(
        "child-waiting-conflict-turn",
        "child-waiting-conflict",
        "root-waiting-conflict",
        "root-waiting-conflict",
        "task",
        "summary",
        true,
    );
    session
        .record_unstarted_turn_termination(request.clone())
        .expect("首次批次应提交");
    let mut conflict = request;
    conflict.prompt_summary = "different summary".to_owned();
    assert!(matches!(
        session.record_unstarted_turn_termination(conflict),
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(session.is_recovery_required());

    let running_storage = TempDir::new().expect("测试目录应创建");
    let running = create(&running_storage, "runtime-waiting-capacity-running");
    register_pending_child(&running, "root-waiting-running", "child-waiting-running");
    append(
        &running,
        "event-child-waiting-running",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: keencode_resources::TurnId::new("child-running-turn")
                        .expect("Turn ID 应有效"),
                    source_agent_id: AgentId::new("child-waiting-running")
                        .expect("Agent ID 应有效"),
                    root_turn_id: keencode_resources::TurnId::new("root-waiting-running")
                        .expect("根 Turn ID 应有效"),
                    parent_turn_id: Some(
                        keencode_resources::TurnId::new("root-waiting-running")
                            .expect("父 Turn ID 应有效"),
                    ),
                    prompt_summary: "running".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: AgentId::new("child-waiting-running").expect("Agent ID 应有效"),
                    turn_id: Some(
                        keencode_resources::TurnId::new("child-running-turn")
                            .expect("Turn ID 应有效"),
                    ),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        },
    );
    let request = unstarted_turn_termination_request(
        "child-waiting-new-turn",
        "child-waiting-running",
        "root-waiting-running",
        "root-waiting-running",
        "task",
        "summary",
        false,
    );
    assert!(matches!(
        running.record_unstarted_turn_termination(request),
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(running.is_recovery_required());
}

/// 冷打开后重复对账只返回 AlreadyCommitted，且不生成新的生命周期记录。
#[test]
fn unstarted_turn_termination_survives_cold_open() {
    let storage = TempDir::new().expect("测试目录应创建");
    let runtime_config = config(&storage);
    let session = create(&storage, "runtime-waiting-capacity-cold");
    start_turn(&session, "root-waiting-cold", 0);
    let request = unstarted_turn_termination_request(
        "child-waiting-cold-turn",
        "child-waiting-cold",
        "root-waiting-cold",
        "root-waiting-cold",
        "cold task",
        "cold summary",
        true,
    );
    session
        .record_unstarted_turn_termination(request.clone())
        .expect("首次批次应提交");
    drop(session);
    let reopened =
        match RuntimeSession::open_session(runtime_config, "runtime-waiting-capacity-cold")
            .expect("Session 应冷打开")
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(report) => {
                panic!("健康 Session 不应损坏：{:?}", report.issues)
            }
        };
    assert_eq!(
        reopened
            .record_unstarted_turn_termination(request)
            .expect("冷打开后的对账应幂等命中"),
        UnstartedTurnTerminationOutcome::AlreadyCommitted
    );
    assert_eq!(
        reopened
            .snapshot()
            .expect("冷恢复 Snapshot 应读取")
            .state
            .sub_agents
            .len(),
        1
    );
}

/// 首次 Failed 终态必须补齐 Agent 与 Turn，并允许随后排队关联 mailbox 消息。
#[test]
fn unstarted_failed_initial_fills_agent_turn_and_allows_mailbox() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-unstarted-failed-initial");
    start_turn(&session, "root-unstarted-failed", 0);
    let request = unstarted_failed_turn_termination_request(
        "child-unstarted-failed-turn",
        "child-unstarted-failed",
        "root-unstarted-failed",
        "工具快照失效的子任务",
        true,
        "工具快照失效，Turn 在启动前失败",
    );
    assert_eq!(
        session
            .record_unstarted_turn_termination(request)
            .expect("首次 Failed 终态应提交"),
        UnstartedTurnTerminationOutcome::Committed
    );

    let state = session.snapshot().expect("Failed 状态应读取").state;
    let child_id = AgentId::new("child-unstarted-failed").expect("子 Agent 标识应有效");
    let child_turn_id =
        keencode_resources::TurnId::new("child-unstarted-failed-turn").expect("子 Turn 标识应有效");
    assert_eq!(
        state
            .sub_agents
            .get(&child_id)
            .expect("Failed 终态应补齐 Agent")
            .status,
        SubAgentStatus::Failed
    );
    let turn = state
        .turns
        .get(&child_turn_id)
        .expect("Failed 终态应补齐 Turn");
    assert_eq!(turn.status, TurnStatus::Failed);
    assert_eq!(turn.stop_reason, Some(TurnStopReason::Failed));
    assert_eq!(
        turn.outcome_message.as_deref(),
        Some("工具快照失效，Turn 在启动前失败")
    );

    session
        .queue_mailbox_message(MailboxMessage {
            message_id: MailboxMessageId::new("message-unstarted-failed")
                .expect("邮箱消息标识应有效"),
            from: child_id,
            to: AgentId::new("root").expect("根 Agent 标识应有效"),
            related_turn_id: child_turn_id,
            body: "Failed 终态可被父 Agent 消费".to_owned(),
            artifact: None,
            state: MailboxState::Queued,
        })
        .expect("Failed 终态之后应可排队 mailbox 消息");
    assert_eq!(
        session
            .snapshot()
            .expect("mailbox 状态应读取")
            .state
            .mailbox
            .len(),
        1
    );
}

/// 相同 Failed 正文重复提交不得追加批次，终态或说明变化必须冲突冻结。
#[test]
fn unstarted_failed_idempotency_and_termination_conflicts_fail_closed() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-unstarted-failed-idempotency");
    start_turn(&session, "root-unstarted-idempotency", 0);
    let request = unstarted_failed_turn_termination_request(
        "child-unstarted-idempotency-turn",
        "child-unstarted-idempotency",
        "root-unstarted-idempotency",
        "稳定失败任务",
        true,
        "稳定 Failed 说明",
    );
    assert_eq!(
        session
            .record_unstarted_turn_termination(request.clone())
            .expect("首次 Failed 终态应提交"),
        UnstartedTurnTerminationOutcome::Committed
    );
    let batches = journal_records(&session)
        .iter()
        .filter(|record| matches!(record.event, SessionEvent::AtomicBatch { .. }))
        .count();
    assert_eq!(
        session
            .record_unstarted_turn_termination(request.clone())
            .expect("相同 Failed 正文应幂等重试"),
        UnstartedTurnTerminationOutcome::AlreadyCommitted
    );
    assert_eq!(
        journal_records(&session)
            .iter()
            .filter(|record| matches!(record.event, SessionEvent::AtomicBatch { .. }))
            .count(),
        batches,
        "幂等重试不得追加第二个批次"
    );

    let mut changed_message = request.clone();
    changed_message.termination = UnstartedTurnTermination::Failed {
        message: "修改后的 Failed 说明".to_owned(),
    };
    assert!(matches!(
        session.record_unstarted_turn_termination(changed_message),
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(session.is_recovery_required());

    let other_storage = TempDir::new().expect("第二个测试目录应创建");
    let other = create(&other_storage, "runtime-unstarted-kind-conflict");
    start_turn(&other, "root-unstarted-kind-conflict", 0);
    let interrupted = unstarted_turn_termination_request(
        "child-unstarted-kind-conflict-turn",
        "child-unstarted-kind-conflict",
        "root-unstarted-kind-conflict",
        "root-unstarted-kind-conflict",
        "终态类型冲突任务",
        "终态类型冲突任务",
        true,
    );
    other
        .record_unstarted_turn_termination(interrupted.clone())
        .expect("原始 Interrupted 终态应提交");
    let mut changed_kind = interrupted;
    changed_kind.termination = UnstartedTurnTermination::Failed {
        message: "同一 Turn 改为 Failed".to_owned(),
    };
    assert!(matches!(
        other.record_unstarted_turn_termination(changed_kind),
        Err(RuntimeError::RecoveryRequired)
    ));
    assert!(other.is_recovery_required());

    let blank = unstarted_failed_turn_termination_request(
        "child-unstarted-blank-turn",
        "child-unstarted-blank",
        "root-unstarted-kind-conflict",
        "空白说明任务",
        true,
        " \t",
    );
    assert!(matches!(
        other.record_unstarted_turn_termination(blank),
        Err(RuntimeError::InvalidTurnRequest)
    ));
}

/// 已有 Agent 的后续 Failed Turn 不得在补偿批次中重复 Spawned。
#[test]
fn unstarted_failed_followup_does_not_duplicate_spawn() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-unstarted-failed-followup");
    start_turn(&session, "root-unstarted-followup", 0);
    session
        .record_unstarted_turn_termination(unstarted_turn_termination_request(
            "child-unstarted-followup-initial",
            "child-unstarted-followup",
            "root-unstarted-followup",
            "root-unstarted-followup",
            "后续 Failed 子任务",
            "后续 Failed 子任务",
            true,
        ))
        .expect("首次 Interrupted 终态应提交");
    session
        .record_unstarted_turn_termination(unstarted_failed_turn_termination_request(
            "child-unstarted-followup-failed",
            "child-unstarted-followup",
            "root-unstarted-followup",
            "后续 Failed 子任务",
            false,
            "后续 Turn 在启动前失败",
        ))
        .expect("已有 Agent 的后续 Failed 终态应提交");

    let records = journal_records(&session);
    let failed_batch = records
        .iter()
        .filter_map(|record| match &record.event {
            SessionEvent::AtomicBatch { events }
                if events.iter().any(|event| {
                    matches!(
                        event,
                        SessionEvent::TurnStopped {
                            turn_id,
                            reason: TurnStopReason::Failed,
                            ..
                        } if turn_id.as_str() == "child-unstarted-followup-failed"
                    )
                }) =>
            {
                Some(events)
            }
            _ => None,
        })
        .next()
        .expect("应找到后续 Failed 批次");
    assert_eq!(failed_batch.len(), 4);
    assert!(
        !failed_batch
            .iter()
            .any(|event| matches!(event, SessionEvent::SubAgentSpawned { .. }))
    );
}

/// Failed 批次的明确 I/O 失败不得冻结 Runtime，随后相同批次必须可重试。
#[test]
fn unstarted_failed_io_failure_can_retry() {
    let storage = TempDir::new().expect("测试目录应创建");
    let session = create(&storage, "runtime-unstarted-failed-io");
    start_turn(&session, "root-unstarted-io", 0);
    let request = unstarted_failed_turn_termination_request(
        "child-unstarted-io-turn",
        "child-unstarted-io",
        "root-unstarted-io",
        "I/O 失败重试任务",
        true,
        "模拟 Journal 写入失败后重试",
    );
    let event_id = super::runtime_unstarted_turn_termination_event_id(
        session.session_id(),
        &request.agent.agent_id,
        &request.turn_id,
    )
    .expect("未启动终态事件标识应派生");
    let before_failure = session.snapshot().expect("明确失败前的状态应读取");
    inject_runtime_lifecycle_failure(&event_id);

    assert!(matches!(
        session.record_unstarted_turn_termination(request.clone()),
        Err(RuntimeError::Resource(_))
    ));
    let after_failure = session.snapshot().expect("明确失败后的状态应读取");
    assert!(!after_failure.recovery_required);
    assert_eq!(after_failure.state, before_failure.state);
    assert_eq!(
        after_failure.state.last_sequence,
        before_failure.state.last_sequence
    );
    assert_eq!(after_failure.state.turns.len(), 1);
    assert!(
        after_failure
            .state
            .turns
            .keys()
            .any(|turn_id| turn_id.as_str() == "root-unstarted-io")
    );
    assert!(!after_failure.state.turns.contains_key(&request.turn_id));
    assert!(after_failure.state.sub_agents.is_empty());
    assert_eq!(
        session
            .record_unstarted_turn_termination(request)
            .expect("明确失败后相同批次应可重试"),
        UnstartedTurnTerminationOutcome::Committed
    );
}

/// Journal 已有 Running、终态 Turn 或错误父链时，补偿不得覆盖既有事实。
#[test]
fn unstarted_termination_does_not_override_running_terminal_or_wrong_parent() {
    let running_storage = TempDir::new().expect("运行态测试目录应创建");
    let running = create(&running_storage, "runtime-unstarted-running");
    register_pending_child(
        &running,
        "root-unstarted-running",
        "child-unstarted-running",
    );
    append(
        &running,
        "event-unstarted-running-child",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: keencode_resources::TurnId::new("child-unstarted-running-existing")
                        .expect("子 Turn 标识应有效"),
                    source_agent_id: AgentId::new("child-unstarted-running")
                        .expect("子 Agent 标识应有效"),
                    root_turn_id: keencode_resources::TurnId::new("root-unstarted-running")
                        .expect("根 Turn 标识应有效"),
                    parent_turn_id: Some(
                        keencode_resources::TurnId::new("root-unstarted-running")
                            .expect("父 Turn 标识应有效"),
                    ),
                    prompt_summary: "已有 Running 子 Turn".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: AgentId::new("child-unstarted-running").expect("子 Agent 标识应有效"),
                    turn_id: Some(
                        keencode_resources::TurnId::new("child-unstarted-running-existing")
                            .expect("子 Turn 标识应有效"),
                    ),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        },
    );
    assert!(matches!(
        running.record_unstarted_turn_termination(unstarted_failed_turn_termination_request(
            "child-unstarted-running-new",
            "child-unstarted-running",
            "root-unstarted-running",
            "不能覆盖 Running Agent",
            false,
            "不能覆盖 Running Agent",
        )),
        Err(RuntimeError::RecoveryRequired)
    ));

    let terminal_storage = TempDir::new().expect("终态测试目录应创建");
    let terminal = create(&terminal_storage, "runtime-unstarted-terminal");
    register_pending_child(
        &terminal,
        "root-unstarted-terminal",
        "child-unstarted-terminal",
    );
    let terminal_turn = keencode_resources::TurnId::new("child-unstarted-terminal-existing")
        .expect("终态子 Turn 标识应有效");
    append(
        &terminal,
        "event-unstarted-terminal-child",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: terminal_turn.clone(),
                    source_agent_id: AgentId::new("child-unstarted-terminal")
                        .expect("子 Agent 标识应有效"),
                    root_turn_id: keencode_resources::TurnId::new("root-unstarted-terminal")
                        .expect("根 Turn 标识应有效"),
                    parent_turn_id: Some(
                        keencode_resources::TurnId::new("root-unstarted-terminal")
                            .expect("父 Turn 标识应有效"),
                    ),
                    prompt_summary: "已有终态子 Turn".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: AgentId::new("child-unstarted-terminal")
                        .expect("子 Agent 标识应有效"),
                    turn_id: Some(terminal_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
                SessionEvent::TurnStopped {
                    turn_id: terminal_turn.clone(),
                    reason: TurnStopReason::Failed,
                    message: "已有终态子 Turn".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: AgentId::new("child-unstarted-terminal")
                        .expect("子 Agent 标识应有效"),
                    turn_id: Some(terminal_turn.clone()),
                    status: SubAgentStatus::Failed,
                    result_summary: Some("已有终态子 Turn".to_owned()),
                },
            ],
        },
    );
    assert!(matches!(
        terminal.record_unstarted_turn_termination(unstarted_failed_turn_termination_request(
            "child-unstarted-terminal-existing",
            "child-unstarted-terminal",
            "root-unstarted-terminal",
            "不能覆盖终态 Turn",
            false,
            "新的失败说明",
        )),
        Err(RuntimeError::RecoveryRequired)
    ));

    let parent_storage = TempDir::new().expect("错误父链测试目录应创建");
    let parent = create(&parent_storage, "runtime-unstarted-parent");
    start_turn(&parent, "root-unstarted-parent", 0);
    assert!(matches!(
        parent.record_unstarted_turn_termination(unstarted_failed_turn_termination_request(
            "child-unstarted-parent",
            "child-unstarted-parent",
            "missing-parent-root",
            "错误父链任务",
            true,
            "错误父链不得补偿",
        )),
        Err(RuntimeError::RecoveryRequired)
    ));
}

/// 验证工具 Round 的显式零用量不会与后一 Round 的全未知用量合并。
#[tokio::test]
async fn runtime_tool_round_preserves_explicit_zero_usage_without_merging_unknown() {
    let root = TempDir::new().expect("临时目录应创建");
    let runtime_config = config(&root);
    let session = RuntimeSession::create_session(
        runtime_config.clone(),
        manager_create_request(&root, "runtime-round-explicit-zero"),
    )
    .expect("Session 应创建");
    let first_metadata = ResponseMetadata {
        response_id: Some("response-tool-zero".to_owned()),
        model: Some("provider-tool-model".to_owned()),
    };
    let second_metadata = ResponseMetadata {
        response_id: Some("response-final-unknown".to_owned()),
        model: Some("provider-final-model".to_owned()),
    };
    let explicit_zero = TokenUsage {
        input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_tokens: Some(0),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        total_tokens: Some(0),
    };
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            state_changing_tool_reply_with_facts(first_metadata.clone(), explicit_zero.clone()),
            completed_text_reply_with_facts("工具后完成", second_metadata.clone(), None),
        ],
    ));
    let executed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(RuntimeWriteTool {
            executed: executed.clone(),
        }))
        .expect("RuntimeWrite 应注册");
    let runner = session.bind_agent_runner(AgentRunner::new(provider, tools, RunLimits::default()));
    assert!(
        runner
            .run_turn(root_runtime_turn(
                &session,
                "runtime-round-zero-turn",
                "验证工具 Round 用量",
            ))
            .await
            .expect("Turn 应执行")
            .is_success()
    );
    assert!(executed.load(Ordering::SeqCst));
    let live = session.snapshot().expect("实时 Snapshot 应读取").state;
    assert_eq!(live.model_rounds.len(), 2);
    let tool_round = &live.model_rounds[0];
    assert_eq!(tool_round.model_round, 1);
    assert_eq!(tool_round.requested_model, "test-model");
    assert_eq!(tool_round.metadata, first_metadata);
    assert_eq!(tool_round.stop_reason, StopReason::ToolUse);
    assert_eq!(tool_round.usage, explicit_zero);
    let final_round = &live.model_rounds[1];
    assert_eq!(final_round.model_round, 2);
    assert_eq!(final_round.requested_model, "test-model");
    assert_eq!(final_round.metadata, second_metadata);
    assert_eq!(final_round.stop_reason, StopReason::Completed);
    assert_eq!(final_round.usage, TokenUsage::unknown());
    assert_ne!(tool_round.usage, final_round.usage);
    drop(runner);
    drop(session);

    let reopened = match RuntimeSession::open_session(runtime_config, "runtime-round-explicit-zero")
        .expect("Session 应冷打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("健康 Session 不应损坏：{:?}", report.issues)
        }
    };
    assert_eq!(
        reopened.snapshot().expect("冷恢复 Snapshot 应读取").state,
        live
    );
}
