//! 第一阶段领域状态机的单元测试。

use super::*;
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelStreamEvent, ProviderCapabilities,
    ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason, StructuredOutputCapability,
    StructuredOutputConfig, StructuredOutputEnforcement, StructuredOutputFailureKind, ToolChoice,
    ToolDefinition,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

/// 与 Runtime 保留结果工具名称保持一致的测试常量。
const STRUCTURED_RESULT_TOOL: &str = "__keencode_structured_output";

/// 创建测试所需的非空 Session 标识。
fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("测试 Session 标识应当有效")
}

/// 创建测试所需的非空 Turn 标识。
fn turn_id(value: &str) -> TurnId {
    TurnId::new(value).expect("测试 Turn 标识应当有效")
}

/// 创建测试所需的非空 Agent 标识。
fn agent_id(value: &str) -> AgentId {
    AgentId::new(value).expect("测试 Agent 标识应当有效")
}

/// 创建测试所需的非空工具调用标识。
fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("测试工具调用标识应当有效")
}

/// ToolCall 身份以 UTF-8 字节数执行 1024 字节硬边界。
#[test]
fn tool_call_id_is_non_empty_and_utf8_bounded() {
    assert!(ToolCallId::new("x".repeat(MAX_TOOL_CALL_ID_BYTES)).is_ok());
    assert_eq!(
        ToolCallId::new("x".repeat(MAX_TOOL_CALL_ID_BYTES + 1)),
        Err(IdentifierError::TooLong {
            maximum_bytes: MAX_TOOL_CALL_ID_BYTES
        })
    );
    assert!(ToolCallId::new("你".repeat(341)).is_ok());
    assert_eq!(
        ToolCallId::new("你".repeat(342)),
        Err(IdentifierError::TooLong {
            maximum_bytes: MAX_TOOL_CALL_ID_BYTES
        })
    );
}

/// 非法完成不能写入终态，首次合法终态写入后也不能被覆盖。
#[test]
fn terminal_transition_is_legal_and_single_assignment() {
    let mut turn = TurnState::new(turn_id("turn-1"), agent_id("agent-1"));

    assert_eq!(
        turn.finish(TerminalReason::Completed),
        Err(TurnTransitionError::InvalidTerminalTransition {
            from: TurnPhase::Created,
            reason: TerminalReason::Completed,
        })
    );
    assert!(!turn.is_terminal());

    turn.transition_to(TurnPhase::PreparingContext)
        .expect("应当进入上下文准备阶段");
    assert_eq!(turn.begin_round(), Ok(1));
    turn.transition_to(TurnPhase::StreamingModel)
        .expect("应当进入模型流阶段");
    turn.transition_to(TurnPhase::CommittingRound)
        .expect("应当进入提交阶段");
    assert_eq!(turn.finish(TerminalReason::Completed), Ok(()));
    assert_eq!(turn.terminal_reason(), Some(TerminalReason::Completed));

    assert_eq!(
        turn.finish(TerminalReason::Failed),
        Err(TurnTransitionError::AlreadyTerminal {
            reason: TerminalReason::Completed,
        })
    );
    assert_eq!(turn.terminal_reason(), Some(TerminalReason::Completed));
}

/// Round 只在请求模型时递增，Step 只在实际工具执行阶段递增。
#[test]
fn round_and_step_counts_follow_runtime_phases() {
    let mut turn = TurnState::new(turn_id("turn-2"), agent_id("agent-1"));
    turn.transition_to(TurnPhase::PreparingContext)
        .expect("应当进入上下文准备阶段");
    assert_eq!(turn.begin_round(), Ok(1));
    turn.transition_to(TurnPhase::StreamingModel)
        .expect("应当进入模型流阶段");
    turn.transition_to(TurnPhase::SchedulingTools)
        .expect("应当进入工具调度阶段");

    assert_eq!(
        turn.record_step(),
        Err(TurnTransitionError::InvalidCounterPhase {
            counter: CounterKind::Step,
            phase: TurnPhase::SchedulingTools,
        })
    );
    turn.transition_to(TurnPhase::ExecutingTools)
        .expect("应当进入工具执行阶段");
    assert_eq!(turn.record_step(), Ok(1));
    assert_eq!(turn.round_count(), 1);
    assert_eq!(turn.step_count(), 1);
}

/// Plan 只读守卫必须拒绝状态变更，并在普通模式直接允许执行。
#[test]
fn plan_guard_authorizes_only_inactive_or_read_only_effects() {
    assert_eq!(
        PlanGuard::read_only().authorize(ToolEffect::ChangesState),
        Err(PlanGuardError::StateChangeDenied)
    );
    assert_eq!(
        PlanGuard::read_only().authorize(ToolEffect::ReadOnly),
        Ok(())
    );
    assert_eq!(
        PlanGuard::inactive().authorize(ToolEffect::ChangesState),
        Ok(())
    );
}

/// 根 Agent 只能创建一层子 Agent，子 Agent 不能继续递归创建。
#[test]
fn agent_depth_enforces_single_layer() {
    let child = AgentDepth::ROOT
        .child()
        .expect("根 Agent 应当能创建子 Agent");
    assert_eq!(child, AgentDepth::CHILD);
    assert!(!child.can_spawn_child());
    assert_eq!(
        child.child(),
        Err(AgentDepthError::ExceedsSingleLayer { requested: 2 })
    );
    assert_eq!(
        AgentDepth::new(2),
        Err(AgentDepthError::ExceedsSingleLayer { requested: 2 })
    );
}

/// QueueOnly 只排队，只有 TriggerTurn 可以唤醒空闲 Agent。
#[test]
fn mailbox_delivery_has_explicit_wake_semantics() {
    assert!(!MailboxDelivery::QueueOnly.wakes_idle_agent());
    assert!(MailboxDelivery::TriggerTurn.wakes_idle_agent());

    let completed = AgentStatus::Completed {
        final_message: Some("done".to_string()),
    };
    assert!(completed.is_turn_final());
    assert!(completed.can_receive_messages());
    assert!(!AgentStatus::Stopped.can_receive_messages());
}

/// 创建一段正常文本模型响应。
fn text_reply(text: &str) -> ScriptedReply {
    text_reply_with_stop(text, StopReason::Completed)
}

/// 创建带指定结束原因的文本模型响应。
fn text_reply_with_stop(text: &str, stop_reason: StopReason) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
        ModelStreamEvent::MessageEnd { stop_reason },
    ])
}

/// 创建包含一个或多个完整工具调用的模型响应。
fn tool_reply(calls: &[(&str, &str, Value)]) -> ScriptedReply {
    tool_reply_with_stop(calls, StopReason::ToolUse)
}

/// 创建带指定结束原因的完整工具调用模型响应。
fn tool_reply_with_stop(calls: &[(&str, &str, Value)], stop_reason: StopReason) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("测试调用数量应在 u32 范围内");
        events.push(ModelStreamEvent::ToolCallStart {
            index,
            id: (*id).to_owned(),
            name: (*name).to_owned(),
        });
        events.push(ModelStreamEvent::ToolCallArgumentsDelta {
            index,
            id: (*id).to_owned(),
            delta: arguments.to_string(),
        });
        events.push(ModelStreamEvent::ToolCallEnd {
            index,
            id: (*id).to_owned(),
        });
    }
    events.push(ModelStreamEvent::MessageEnd { stop_reason });
    ScriptedReply::events(events)
}

/// 创建最小用户 Turn 请求。
fn turn_request(plan_guard: PlanGuard) -> TurnRequest {
    TurnRequest::new(
        session_id("session-runner"),
        turn_id("turn-runner"),
        agent_id("agent-runner"),
        "test-model",
        vec![Message::text(MessageRole::User, "执行合成测试")],
        plan_guard,
    )
}

/// 记录输入并返回固定文本的测试工具。
struct RecordingTool {
    /// 提供给模型的精确工具名称。
    name: String,
    /// 每次调用采用的副作用分类。
    effect: ToolEffect,
    /// 工具声明的并发方式。
    concurrency: ToolConcurrency,
    /// 已真正执行的输入。
    calls: Mutex<Vec<Value>>,
    /// Runner 交给工具且输入无法覆盖的可信 ToolCall 身份。
    tool_call_ids: Mutex<Vec<ToolCallId>>,
}

impl RecordingTool {
    /// 创建没有历史调用的记录工具。
    fn new(name: &str, effect: ToolEffect, concurrency: ToolConcurrency) -> Self {
        Self {
            name: name.to_owned(),
            effect,
            concurrency,
            calls: Mutex::new(Vec::new()),
            tool_call_ids: Mutex::new(Vec::new()),
        }
    }

    /// 返回实际执行次数。
    fn call_count(&self) -> usize {
        self.calls.lock().expect("工具测试锁不应损坏").len()
    }

    /// 返回工具实际观察到的可信 ToolCall 身份。
    fn tool_call_ids(&self) -> Vec<ToolCallId> {
        self.tool_call_ids
            .lock()
            .expect("ToolCall 身份测试锁不应损坏")
            .clone()
    }
}

impl AgentTool for RecordingTool {
    /// 返回要求字符串 `value` 的合成 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name.clone(),
            "执行无外部依赖的合成工具",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 返回测试预设的副作用分类。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    /// 返回测试预设的并发方式。
    fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    /// 保存输入并返回确定性文本。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        self.calls.lock().expect("工具测试锁不应损坏").push(input);
        self.tool_call_ids
            .lock()
            .expect("ToolCall 身份测试锁不应损坏")
            .push(context.tool_call_id);
        Box::pin(async { Ok(ToolOutput::text("synthetic-result")) })
    }
}

/// 为每个模型实时事件引入可观测延迟，证明墙钟耗时来自实际请求路径。
struct DelayedModelEventSink;

impl AgentEventSink for DelayedModelEventSink {
    /// 延迟确认每个模型事件，使完整响应耗时稳定大于零毫秒。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(())
        })
    }
}

/// 记录模型 Round 用量提交，按配置拒绝前若干次并观察工具是否已执行。
struct ModelRoundUsageProbeSink {
    /// 用于验证用量提交发生时工具尚未执行。
    tool: Arc<RecordingTool>,
    /// 从首个调用开始需要明确拒绝的次数。
    rejected_attempts: usize,
    /// 用量提交总调用次数。
    attempts: AtomicUsize,
    /// 每次重投收到的完整不可变用量事实。
    usages: Mutex<Vec<ModelRoundUsage>>,
    /// 首轮用量提交期间工具始终未执行的观测结果。
    first_round_preceded_tool: AtomicBool,
}

impl ModelRoundUsageProbeSink {
    /// 创建一个没有历史提交的模型用量探针。
    fn new(tool: Arc<RecordingTool>, rejected_attempts: usize) -> Self {
        Self {
            tool,
            rejected_attempts,
            attempts: AtomicUsize::new(0),
            usages: Mutex::new(Vec::new()),
            first_round_preceded_tool: AtomicBool::new(true),
        }
    }

    /// 返回按实际调用顺序捕获的用量事实。
    fn usages(&self) -> Vec<ModelRoundUsage> {
        self.usages.lock().expect("模型用量探针锁不应损坏").clone()
    }
}

impl AgentCommitSink for ModelRoundUsageProbeSink {
    /// 记录完整用量事实，并在配置的前若干次返回明确拒绝。
    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        if usage.model_round() == 1 && self.tool.call_count() != 0 {
            self.first_round_preceded_tool
                .store(false, Ordering::SeqCst);
        }
        self.usages
            .lock()
            .expect("模型用量探针锁不应损坏")
            .push(usage.clone());
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < self.rejected_attempts {
            Err(AgentCommitSinkError::rejected("测试拒绝模型 Round 用量"))
        } else {
            Ok(())
        }
    }

    /// 其余工具 Round 预检委托无状态默认实现。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        NoopAgentCommitSink.preflight_tool_round(round)
    }

    /// 其余权威事件委托无状态默认实现。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        NoopAgentCommitSink.commit(event)
    }
}

/// 记录模型终止 Round 的权威提交，验证非正常响应只保留安全内容。
#[derive(Default)]
struct ModelTerminationCommitProbe {
    /// 按提交顺序保存完整权威事件。
    events: Mutex<Vec<AgentCommitEvent>>,
}

impl ModelTerminationCommitProbe {
    /// 返回已经确认的权威事件快照。
    fn events(&self) -> Vec<AgentCommitEvent> {
        self.events
            .lock()
            .expect("模型终止提交探针锁不应损坏")
            .clone()
    }
}

impl AgentCommitSink for ModelTerminationCommitProbe {
    /// 委托默认无状态预检，模型终止测试不包含实际工具 Round。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        NoopAgentCommitSink.preflight_tool_round(round)
    }

    /// 保存模型终止对应的 Transcript/完成事实。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.events
            .lock()
            .expect("模型终止提交探针锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

/// 记录 Stop Hook 调用次数，用于确认模型非正常终态不会自动续跑。
struct StopHookProbe {
    /// Stop Hook 被调用的总次数。
    calls: Arc<AtomicUsize>,
}

impl AgentHook for StopHookProbe {
    /// 返回测试 Hook 的稳定名称。
    fn name(&self) -> &str {
        "model-stop-probe"
    }

    /// 记录调用并接受候选，若错误调用仍应由测试失败暴露。
    fn stop(
        &self,
        _context: StopHookContext,
    ) -> HookFuture<'_, Result<StopHookOutput, HookCallbackError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(StopHookOutput::stop()) })
    }
}

/// 只有两个调用并发到达时才会完成的只读测试工具。
struct BarrierTool {
    /// 两个并发调用共享的异步屏障。
    barrier: Arc<Barrier>,
}

impl AgentTool for BarrierTool {
    /// 返回并行测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "parallel_probe",
            "验证相邻只读工具并发",
            json!({ "type": "object", "additionalProperties": true }),
        )
    }

    /// 把全部输入标记为只读。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 允许与相邻只读调用并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 等待两个调用同时进入执行阶段。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let barrier = self.barrier.clone();
        Box::pin(async move {
            barrier.wait().await;
            Ok(ToolOutput::text("parallel-ok"))
        })
    }
}

/// 记录同时执行峰值的副作用测试工具。
struct ExclusiveProbeTool {
    /// 当前正在执行的调用数量。
    active: Arc<AtomicUsize>,
    /// 观察到的最大并发调用数量。
    maximum: Arc<AtomicUsize>,
}

/// 收到取消后延迟完成清理并记录结果的测试工具。
struct CleanupOnCancelTool {
    /// 工具 Future 首次被轮询时发出的通知。
    started: Arc<Notify>,
    /// 模拟进程树和临时资源已经清理完成的标记。
    cleaned: Arc<AtomicBool>,
}

impl AgentTool for CleanupOnCancelTool {
    /// 返回取消清理测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "cleanup_on_cancel",
            "验证 Turn 取消后等待工具完成清理",
            json!({ "type": "object", "additionalProperties": false }),
        )
    }

    /// 测试调用本身不产生外部副作用。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 清理生命周期测试必须独占执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 等待取消，模拟异步清理后返回稳定错误。
    fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let started = self.started.clone();
        let cleaned = self.cleaned.clone();
        Box::pin(async move {
            started.notify_one();
            context.cancellation.cancelled().await;
            tokio::time::sleep(Duration::from_millis(30)).await;
            cleaned.store(true, Ordering::SeqCst);
            Err(ToolError::permanent("cancelled", "测试工具已清理"))
        })
    }
}

/// 故意忽略取消以验证清理窗口保持有界的测试工具。
struct StubbornTool {
    /// 工具 Future 首次被轮询时发出的通知。
    started: Arc<Notify>,
}

impl AgentTool for StubbornTool {
    /// 返回顽固工具测试定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "stubborn",
            "验证不观察取消的工具不会无限阻塞 Turn",
            json!({ "type": "object", "additionalProperties": false }),
        )
    }

    /// 测试调用本身不产生外部副作用。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 顽固工具测试必须独占执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 发出已启动通知后永久挂起。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let started = self.started.clone();
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<Result<ToolOutput, ToolError>>().await
        })
    }
}

impl AgentTool for ExclusiveProbeTool {
    /// 返回副作用屏障测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "exclusive_probe",
            "验证副作用工具顺序屏障",
            json!({ "type": "object", "additionalProperties": true }),
        )
    }

    /// 把全部调用标记为可能改变状态。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ChangesState)
    }

    /// 即使声明只读并发能力，副作用分类也必须强制顺序执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 短暂保持活动状态并记录并发峰值。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let active = self.active.clone();
        let maximum = self.maximum.clone();
        Box::pin(async move {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::text("exclusive-ok"))
        })
    }
}

/// 创建只包含指定脚本的 Agent Runner。
fn runner(provider: Arc<ScriptedProvider>, registry: ToolRegistry) -> AgentRunner {
    AgentRunner::new(provider, registry, RunLimits::default())
}

/// 文本 Turn 必须形成单一完成终态并提交 assistant 消息。
#[tokio::test]
async fn runner_completes_text_turn() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("done")],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.messages.len(), 2);
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 1);
}

/// 普通文本区分模型终止原因与协议错误，并保留截断或拒答已确认的文本。
#[tokio::test]
async fn ordinary_text_classifies_model_stop_reasons_and_protocol_errors() {
    for stop_reason in [
        StopReason::MaxOutputTokens,
        StopReason::ContentFilter,
        StopReason::Cancelled,
        StopReason::Other {
            reason: "provider_pause".to_owned(),
        },
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [text_reply_with_stop("partial", stop_reason.clone())],
        ));
        let result = runner(provider, ToolRegistry::new())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        let (terminal_reason, error, message_count) = match stop_reason {
            StopReason::MaxOutputTokens => (
                TerminalReason::ModelOutputLimit,
                AgentRunError::ModelOutputLimit,
                2,
            ),
            StopReason::ContentFilter => {
                (TerminalReason::ModelRefusal, AgentRunError::ModelRefusal, 2)
            }
            StopReason::Cancelled => (TerminalReason::Cancelled, AgentRunError::Cancelled, 2),
            StopReason::Other { .. } => (
                TerminalReason::Failed,
                AgentRunError::InvalidResponse {
                    message: String::new(),
                },
                1,
            ),
            StopReason::Completed | StopReason::ToolUse => unreachable!(),
        };
        assert_eq!(result.state.terminal_reason(), Some(terminal_reason));
        if matches!(error, AgentRunError::InvalidResponse { .. }) {
            assert!(matches!(
                result.error,
                Some(AgentRunError::InvalidResponse { .. })
            ));
        } else {
            assert_eq!(result.error, Some(error));
        }
        assert_eq!(result.state.step_count(), 0);
        assert_eq!(result.messages.len(), message_count);
    }
}

/// 普通工具调用只有 ToolUse 才可执行，非正常模型终态不执行或持久化未配对 ToolCall。
#[tokio::test]
async fn ordinary_tool_calls_classify_non_tool_use_stop_reasons_without_side_effects() {
    for stop_reason in [
        StopReason::Completed,
        StopReason::MaxOutputTokens,
        StopReason::ContentFilter,
        StopReason::Cancelled,
        StopReason::Other {
            reason: "provider_pause".to_owned(),
        },
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply_with_stop(
                &[("call-invalid-stop", "record", json!({"value": "write"}))],
                stop_reason.clone(),
            )],
        ));
        let tool = Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ChangesState,
            ToolConcurrency::Exclusive,
        ));
        let mut registry = ToolRegistry::new();
        registry
            .register(tool.clone())
            .expect("停止原因测试工具应可注册");
        let result = runner(provider, registry)
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        match stop_reason {
            StopReason::MaxOutputTokens => {
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::ModelOutputLimit)
                );
                assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
            }
            StopReason::ContentFilter => {
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::ModelRefusal)
                );
                assert_eq!(result.error, Some(AgentRunError::ModelRefusal));
            }
            StopReason::Cancelled => {
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::Cancelled)
                );
                assert_eq!(result.error, Some(AgentRunError::Cancelled));
            }
            StopReason::Completed | StopReason::Other { .. } => {
                assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
                assert!(matches!(
                    result.error,
                    Some(AgentRunError::InvalidResponse { .. })
                ));
            }
            StopReason::ToolUse => unreachable!(),
        }
        assert_eq!(result.state.step_count(), 0);
        assert_eq!(tool.call_count(), 0);
        assert_eq!(result.messages.len(), 1);
    }
}

/// 非正常响应只提交已确认的文本/推理，工具调用既不能执行也不能进入回放 Transcript。
#[tokio::test]
async fn model_output_limit_commits_safe_partial_content_without_tools_or_stop_hooks() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "已确认推理".to_owned(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: "call-not-persisted".to_owned(),
                name: "record".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: "call-not-persisted".to_owned(),
                delta: json!({"value": "never"}).to_string(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: "call-not-persisted".to_owned(),
            },
            ModelStreamEvent::TextDelta {
                index: 2,
                delta: "已确认文本".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::MaxOutputTokens,
            },
        ])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(tool.clone())
        .expect("终止测试工具应可注册");
    let stop_calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(StopHookProbe {
            calls: stop_calls.clone(),
        }))
        .expect("终止测试 Hook 应可注册");
    let sink = Arc::new(ModelTerminationCommitProbe::default());
    let result = AgentRunner::new(provider, registry, RunLimits::default())
        .with_hook_runtime(
            HookRuntime::new(hooks, HookLimits::default()).expect("终止测试 Hook 配置应有效"),
        )
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelOutputLimit)
    );
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    assert_eq!(stop_calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.messages.len(), 2);
    let assistant = &result.messages[1];
    assert!(assistant.content.iter().all(|block| {
        matches!(
            block,
            ContentBlock::Text { .. } | ContentBlock::Reasoning { .. }
        )
    }));
    let commits = sink.events();
    assert_eq!(commits.len(), 1);
    let AgentCommitEventKind::ModelRoundCommitted {
        completion,
        messages,
        ..
    } = commits[0].kind()
    else {
        panic!("模型非正常终态应提交模型完成事实");
    };
    assert_eq!(completion.stop_reason, StopReason::MaxOutputTokens);
    assert_eq!(messages, &result.messages[1..]);
    assert!(
        messages
            .iter()
            .flat_map(|message| &message.content)
            .all(|block| { !matches!(block, ContentBlock::ToolCall { .. }) })
    );
}

/// 空模型响应不伪造空 Assistant 消息，但仍提交带停止原因的模型完成事实。
#[tokio::test]
async fn model_refusal_with_empty_content_commits_completion_fact_without_message() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ContentFilter,
            },
        ])],
    ));
    let sink = Arc::new(ModelTerminationCommitProbe::default());
    let result = runner(provider, ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.error, Some(AgentRunError::ModelRefusal));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelRefusal)
    );
    assert_eq!(result.messages.len(), 1);
    let commits = sink.events();
    assert_eq!(commits.len(), 1);
    let AgentCommitEventKind::ModelRoundCommitted {
        completion,
        messages,
        ..
    } = commits[0].kind()
    else {
        panic!("空模型响应应提交模型完成事实");
    };
    assert_eq!(completion.stop_reason, StopReason::ContentFilter);
    assert!(messages.is_empty());
}

/// Provider 原生结构化输出必须在提交 Transcript 前完成 JSON Schema 校验。
#[tokio::test]
async fn runner_validates_native_structured_output_before_commit() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [text_reply("{\"answer\":42}")],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer", "minimum": 1}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(request)
        .await;

    assert!(result.is_success());
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(result.messages.len(), 2);
    let requests = provider.requests().expect("请求快照应可读取");
    assert!(requests[0].structured_output.is_some());
    assert!(
        requests[0]
            .tools
            .iter()
            .all(|tool| tool.name != STRUCTURED_RESULT_TOOL)
    );
}

/// 原生结构化输出的截断也必须先按模型终止原因结束，并保留已经确认的文本。
#[tokio::test]
async fn runner_classifies_native_structured_output_limit_before_schema_validation() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [text_reply_with_stop(
            "{\"answer\":",
            StopReason::MaxOutputTokens,
        )],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider, ToolRegistry::new())
        .run_turn(request)
        .await;

    assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelOutputLimit)
    );
    assert_eq!(result.structured_output, None);
    assert_eq!(result.messages.len(), 2);
}

/// 原生 Provider 忽略 Schema 时必须以原生约束失败分类结束且不提交坏响应。
#[tokio::test]
async fn runner_classifies_native_schema_violation_without_committing_output() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [text_reply("{\"answer\":0}")],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer", "minimum": 1}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider, ToolRegistry::new())
        .run_turn(request)
        .await;

    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::SchemaViolation,
            ..
        }))
    ));
    assert_eq!(result.messages.len(), 1);
    assert!(result.structured_output.is_none());
}

/// 不支持结构化输出的 Provider 必须在第一次网络调用前失败。
#[tokio::test]
async fn runner_rejects_unsupported_structured_output_before_model_call() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("must-not-run")],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({"type": "integer"}),
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(request)
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(
            ModelError::UnsupportedCapability { .. }
        ))
    ));
    assert!(provider.requests().expect("请求快照应可读取").is_empty());
    assert_eq!(provider.remaining_replies().expect("脚本数量应可读取"), 1);
}

/// Provider 没有原生结构化输出但支持工具调用时必须由 Runtime 自动模拟。
#[tokio::test]
async fn runner_falls_back_to_tool_emulation_when_native_output_is_unsupported() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::Unsupported,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[(
            "call-result",
            STRUCTURED_RESULT_TOOL,
            json!({"value": {"answer": 42}}),
        )])],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(request)
        .await;

    assert!(result.is_success());
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests[0].parallel_tool_calls, Some(false));
    assert!(requests[0].structured_output.is_none());
}

/// 工具模拟收到模型非正常终态时直接分类，普通完成原因仍按结构化协议校验。
#[tokio::test]
async fn runner_classifies_emulated_output_model_stop_reasons_before_protocol_checks() {
    for stop_reason in [
        StopReason::MaxOutputTokens,
        StopReason::ContentFilter,
        StopReason::Cancelled,
        StopReason::Completed,
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                tool_calling: true,
                ..ProviderCapabilities::default()
            },
            [tool_reply_with_stop(
                &[("call-result", STRUCTURED_RESULT_TOOL, json!({"value": 42}))],
                stop_reason.clone(),
            )],
        ));
        let mut request = turn_request(PlanGuard::inactive());
        request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
            "answer",
            json!({"type": "integer"}),
        ));

        let result = runner(provider, ToolRegistry::new())
            .run_turn(request)
            .await;

        match stop_reason {
            StopReason::MaxOutputTokens => {
                assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::ModelOutputLimit)
                );
            }
            StopReason::ContentFilter => {
                assert_eq!(result.error, Some(AgentRunError::ModelRefusal));
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::ModelRefusal)
                );
            }
            StopReason::Cancelled => {
                assert_eq!(result.error, Some(AgentRunError::Cancelled));
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::Cancelled)
                );
            }
            StopReason::Completed => assert!(matches!(
                result.error,
                Some(AgentRunError::Model(ModelError::StructuredOutput {
                    enforcement: StructuredOutputEnforcement::ToolEmulated,
                    failure: StructuredOutputFailureKind::EmulationProtocol,
                    ..
                }))
            )),
            StopReason::ToolUse | StopReason::Other { .. } => unreachable!(),
        }
        assert_eq!(result.messages.len(), 1);
        assert!(result.structured_output.is_none());
    }
}

/// 工具模拟必须注入保留工具、执行普通工具后再以零 Step 提交结构化结果。
#[tokio::test]
async fn runner_emulates_structured_output_after_regular_tool_loop() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("call-read", "record", json!({"value": "source"}))]),
            tool_reply(&[(
                "call-result",
                STRUCTURED_RESULT_TOOL,
                json!({"value": {"answer": 42}}),
            )]),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider.clone(), registry).run_turn(request).await;

    assert!(result.is_success());
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.call_count(), 1);
    assert_eq!(tool.tool_call_ids(), vec![tool_call_id("call-read")]);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(
        result
            .final_response
            .as_ref()
            .map(|response| &response.content),
        Some(&vec![ContentBlock::text("{\"answer\":42}")])
    );
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    for model_request in requests {
        assert!(model_request.structured_output.is_none());
        assert_eq!(model_request.parallel_tool_calls, Some(false));
        assert!(
            model_request
                .tools
                .iter()
                .any(|tool| tool.name == STRUCTURED_RESULT_TOOL)
        );
    }
}

/// 保留结果工具与普通工具混合返回时不得执行任何一个调用。
#[tokio::test]
async fn runner_rejects_mixed_emulated_result_and_regular_tool_calls() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            ("call-result", STRUCTURED_RESULT_TOOL, json!({"value": 1})),
            ("call-write", "record", json!({"value": "side-effect"})),
        ])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({"type": "integer"}),
    ));

    let result = runner(provider, registry).run_turn(request).await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::ToolEmulated,
            failure: StructuredOutputFailureKind::EmulationProtocol,
            ..
        }))
    ));
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.messages.len(), 1);
}

/// 完整 Tool Loop 必须把调用和结果配对后再发起第二轮模型请求。
#[tokio::test]
async fn runner_executes_tool_and_pairs_second_round() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "record", json!({ "value": "x" }))]),
            text_reply("complete"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");

    let result = runner(provider.clone(), registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.call_count(), 1);
    assert_eq!(tool.tool_call_ids(), vec![tool_call_id("call-1")]);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].parallel_tool_calls, Some(false));
    assert_eq!(requests[1].messages.len(), 3);
    let ContentBlock::ToolResult { tool_result } = &requests[1].messages[2].content[0] else {
        panic!("第二轮必须包含工具结果");
    };
    assert_eq!(tool_result.tool_call_id, "call-1");
    assert!(!tool_result.is_error);
}

/// 模型用量必须在工具执行前以相同身份和正文重试，并记录实际非零墙钟耗时。
#[tokio::test]
async fn model_round_usage_retries_stably_before_any_tool_execution() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-usage", "usage_probe", json!({ "value": "x" }))]),
            text_reply("complete"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "usage_probe",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("用量探针工具应注册");
    let sink = Arc::new(ModelRoundUsageProbeSink::new(tool.clone(), 1));

    let result = runner(provider, registry)
        .with_event_sink(Arc::new(DelayedModelEventSink))
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(tool.call_count(), 1);
    assert!(sink.first_round_preceded_tool.load(Ordering::SeqCst));
    assert_eq!(sink.attempts.load(Ordering::SeqCst), 3);
    let usages = sink.usages();
    assert_eq!(usages.len(), 3);
    assert_eq!(usages[0], usages[1]);
    assert_eq!(usages[0].session_id(), &session_id("session-runner"));
    assert_eq!(usages[0].turn_id(), &turn_id("turn-runner"));
    assert_eq!(usages[0].source_agent_id(), &agent_id("agent-runner"));
    assert_eq!(usages[0].model(), "test-model");
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::ToolUse);
    assert!(usages[0].elapsed_millis() > 0);
    assert_eq!(usages[2].model_round(), 2);
    assert_eq!(usages[2].completion().stop_reason, StopReason::Completed);
}

/// 模型用量在全部同步重试后仍失败时，Runner 必须阻止响应中的工具副作用。
#[tokio::test]
async fn model_round_usage_failure_blocks_tool_execution() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-usage-blocked",
            "usage_blocked",
            json!({ "value": "never" }),
        )])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "usage_blocked",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(tool.clone())
        .expect("用量失败探针工具应注册");
    let sink = Arc::new(ModelRoundUsageProbeSink::new(tool.clone(), usize::MAX));

    let result = runner(provider, registry)
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    assert_eq!(sink.attempts.load(Ordering::SeqCst), 2);
    let usages = sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0], usages[1]);
}

/// Provider 返回超长 ToolCall 身份时，Runner 必须在工具执行前稳定拒绝。
#[tokio::test]
async fn runner_rejects_oversized_tool_call_id_before_execution() {
    let oversized_id = "x".repeat(MAX_TOOL_CALL_ID_BYTES + 1);
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            &oversized_id,
            "record",
            json!({ "value": "never" }),
        )])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::InvalidResponse { message })
            if message.contains("1024")
    ));
    assert_eq!(tool.call_count(), 0);
    assert!(tool.tool_call_ids().is_empty());
    assert_eq!(result.state.step_count(), 0);
}

/// 未注册工具的超长调用身份也必须在构造立即结果前拒绝，不能击穿固定失败容量预留。
#[tokio::test]
async fn runner_rejects_oversized_unknown_tool_call_id_before_immediate_result() {
    let oversized_id = "x".repeat(MAX_TOOL_CALL_ID_BYTES + 1);
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(&oversized_id, "missing", json!({}))])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::InvalidResponse { message })
            if message.contains("1024")
    ));
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
}

/// 不符合冻结 JSON Schema 的输入必须形成配对错误结果，且不能进入工具语义或执行阶段。
#[tokio::test]
async fn invalid_tool_input_is_rejected_before_execution() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-invalid", "record", json!({ "value": 7 }))]),
            text_reply("recovered"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");

    let result = runner(provider.clone(), registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
    let requests = provider.requests().expect("请求快照应可读取");
    let ContentBlock::ToolResult { tool_result } = &requests[1].messages[2].content[0] else {
        panic!("无效输入必须产生配对工具结果");
    };
    assert_eq!(tool_result.tool_call_id, "call-invalid");
    assert!(tool_result.is_error);
}

/// Plan 模式必须拒绝副作用工具且不得进入实际执行阶段。
#[tokio::test]
async fn plan_mode_blocks_state_changing_side_effect() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-plan", "plan_write", json!({ "value": "x" }))]),
            text_reply("plan-only"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "plan_write",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::read_only()))
        .await;

    assert!(result.is_success());
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
}

/// 相邻并发安全只读调用必须真正同时进入执行器。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_tools_run_in_parallel_segment() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "parallel_probe", json!({ "value": "a" })),
                ("call-b", "parallel_probe", json!({ "value": "b" })),
            ]),
            text_reply("joined"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(BarrierTool {
            barrier: Arc::new(Barrier::new(2)),
        }))
        .expect("并发测试工具应可注册");
    let agent_runner = runner(provider, registry);
    let future = agent_runner.run_turn(turn_request(PlanGuard::inactive()));
    let result = tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .expect("并发工具不应因顺序执行卡在屏障");

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 2);
}

/// 任何副作用调用都必须形成顺序屏障，即使工具错误声明可并发。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_effect_tools_are_forced_sequential() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "exclusive_probe", json!({ "value": "a" })),
                ("call-b", "exclusive_probe", json!({ "value": "b" })),
            ]),
            text_reply("ordered"),
        ],
    ));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(ExclusiveProbeTool {
            active,
            maximum: maximum.clone(),
        }))
        .expect("顺序测试工具应可注册");
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 2);
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
}

/// 重复工具调用 ID 必须让 Turn 失败且不执行任何工具。
#[tokio::test]
async fn duplicate_tool_call_id_fails_turn() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("duplicate", "record", json!({ "value": "a" })),
            ("duplicate", "record", json!({ "value": "b" })),
        ])],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(matches!(
        result.error,
        Some(AgentRunError::DuplicateToolCallId { .. })
    ));
    assert_eq!(tool.call_count(), 0);
}

/// 后续 Round 也不能复用本 Turn 已经执行过的工具调用 ID。
#[tokio::test]
async fn duplicate_tool_call_id_across_rounds_fails_before_second_execution() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("reused", "record", json!({ "value": "first" }))]),
            tool_reply(&[("reused", "record", json!({ "value": "second" }))]),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(matches!(
        result.error,
        Some(AgentRunError::DuplicateToolCallId { id }) if id == "reused"
    ));
    assert_eq!(tool.call_count(), 1);
    assert_eq!(result.state.step_count(), 1);
}

/// 并发只读段超出剩余 Step 时必须整体拒绝且不得执行任何调用。
#[tokio::test]
async fn parallel_segment_step_limit_is_atomic() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "record", json!({ "value": "a" })),
                ("call-b", "record", json!({ "value": "b" })),
            ]),
            text_reply("step-limit-summary"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let limits = RunLimits::new(1, 1).expect("测试上限应有效");
    let result = AgentRunner::new(provider.clone(), registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert!(matches!(
        result.error,
        Some(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum: 1,
        })
    ));
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(result.state.round_count(), 2);
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    assert_eq!(result.messages.len(), 5);
    let results = result
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-a");
    assert_eq!(results[1].tool_call_id, "call-b");
    assert!(results.iter().all(|item| item.is_error));
}

/// ConfirmChanges 下允许调用只执行剩余额度内前缀，越界调用保留配对错误。
#[tokio::test]
async fn allowed_side_effects_stop_at_step_limit_with_paired_results() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "record", json!({"value": "a"})),
                ("call-b", "record", json!({"value": "b"})),
            ]),
            text_reply("step-limit-summary"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let limits = RunLimits::new(4, 1).expect("测试上限应有效");

    let result = AgentRunner::new(provider.clone(), registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum: 1,
        })
    ));
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.call_count(), 1);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    assert_eq!(result.messages.len(), 5);
    let results = result
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-a");
    assert!(!results[0].is_error);
    assert_eq!(results[1].tool_call_id, "call-b");
    assert!(results[1].is_error);
}

/// Step 上限后的可选总结请求失败时不能把已确定的上限终态覆盖为 Provider 失败。
#[tokio::test]
async fn step_limit_summary_provider_failure_preserves_limit_reason() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "record", json!({"value": "a"})),
                ("call-b", "record", json!({"value": "b"})),
            ]),
            ScriptedReply::new(vec![Err(ModelError::Protocol {
                message: "合成总结协议错误".to_owned(),
            })]),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let limits = RunLimits::new(1, 1).expect("测试上限应有效");

    let result = AgentRunner::new(provider.clone(), registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum: 1,
        })
    );
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
}

/// 总结请求违规复用工具调用 ID 时也不能覆盖已经确定的 Step 上限终态。
#[tokio::test]
async fn step_limit_summary_invalid_tool_call_preserves_limit_reason() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a", "record", json!({"value": "a"})),
                ("call-b", "record", json!({"value": "b"})),
            ]),
            tool_reply(&[("call-a", "record", json!({"value": "summary-must-not-run"}))]),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let limits = RunLimits::new(1, 1).expect("测试上限应有效");

    let result = AgentRunner::new(provider, registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::LimitReached {
            counter: CounterKind::Step,
            maximum: 1,
        })
    );
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.round_count(), 2);
}

/// Turn 取消必须等待工具在清理窗口内完成进程树和临时资源清理。
#[tokio::test]
async fn cancellation_waits_for_tool_cleanup_before_terminal_result() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-cleanup",
            "cleanup_on_cancel",
            json!({}),
        )])],
    ));
    let started = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(CleanupOnCancelTool {
            started: started.clone(),
            cleaned: cleaned.clone(),
        }))
        .expect("清理测试工具应可注册");
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let cancel_task = tokio::spawn(async move {
        started.notified().await;
        cancellation.cancel();
    });

    let result = runner(provider, registry).run_turn(request).await;
    cancel_task.await.expect("取消任务不应异常");

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(result.messages.len(), 3);
    let ContentBlock::ToolResult { tool_result } = &result.messages[2].content[0] else {
        panic!("取消后的工具调用必须保留配对结果");
    };
    assert_eq!(tool_result.tool_call_id, "call-cleanup");
    assert!(tool_result.is_error);
}

/// 不观察取消的工具必须在清理窗口结束后被丢弃，Turn 不能永久挂起。
#[tokio::test]
async fn cancellation_cleanup_grace_is_bounded() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[("call-stubborn", "stubborn", json!({}))])],
    ));
    let started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(StubbornTool {
            started: started.clone(),
        }))
        .expect("顽固测试工具应可注册");
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let cancel_task = tokio::spawn(async move {
        started.notified().await;
        cancellation.cancel();
    });
    let limits = RunLimits::new(4, 4)
        .expect("测试上限应有效")
        .with_tool_cancel_grace_ms(30)
        .expect("测试清理窗口应有效");
    let started_at = tokio::time::Instant::now();

    let result = AgentRunner::new(provider, registry, limits)
        .run_turn(request)
        .await;
    cancel_task.await.expect("取消任务不应异常");

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert!(started_at.elapsed() < Duration::from_secs(1));
}

/// 预先取消的 Turn 必须在模型调用前进入唯一取消终态。
#[tokio::test]
async fn pre_cancelled_turn_never_calls_model() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("must-not-run")],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    let cancellation = TurnCancellation::new();
    cancellation.cancel();
    request.set_cancellation(cancellation);

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(request)
        .await;

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert!(provider.requests().expect("请求快照应可读取").is_empty());
}
