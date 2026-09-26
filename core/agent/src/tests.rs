//! 第一阶段领域状态机的单元测试。

use super::*;
use crate::tool::{
    SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT, TOOL_OUTPUT_LIMIT_RESULT, TRUNCATION_MARKER,
    TRUNCATION_PREVIEW_KEEP_BYTES, TRUNCATION_SENTINEL_PREFIX,
};
use futures_util::stream;
use keencode_model::{
    ContentBlock, ImageContent, Message, MessageRole, ModelError, ModelStreamEvent,
    ProviderCapabilities, ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
    StructuredOutputCapability, StructuredOutputConfig, StructuredOutputEnforcement,
    StructuredOutputFailureKind, TokenUsage, ToolCall, ToolChoice, ToolDefinition, ToolResult,
    ToolResultContent, collect_model_stream,
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

/// 创建没有任何内容块的空模型响应。
fn empty_reply_with_stop(stop_reason: StopReason) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::MessageEnd { stop_reason },
    ])
}

/// 创建携带推理和文本增量的原生结构化候选。
fn structured_text_reply(reasoning: &str, text: &str) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    if !reasoning.is_empty() {
        events.push(ModelStreamEvent::ReasoningDelta {
            index: 0,
            delta: reasoning.to_owned(),
        });
    }
    if !text.is_empty() {
        events.push(ModelStreamEvent::TextDelta {
            index: 1,
            delta: text.to_owned(),
        });
    }
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::Completed,
    });
    ScriptedReply::events(events)
}

/// 创建按正文、用量、计时和结束事件顺序排列的原生结构化候选。
fn structured_text_reply_with_telemetry(
    text: &str,
    input_tokens: u64,
    duration_ms: u64,
) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
        ModelStreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: Some(input_tokens),
                output_tokens: Some(1),
                total_tokens: Some(input_tokens.saturating_add(1)),
                ..TokenUsage::unknown()
            },
        },
        ModelStreamEvent::DecodeTiming { duration_ms },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ])
}

/// 创建包含推理、保留结果工具调用和可选遥测的工具模拟结构化候选。
fn structured_result_reply(
    reasoning: &str,
    id: &str,
    value: Value,
    input_tokens: u64,
    duration_ms: Option<u64>,
) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    if !reasoning.is_empty() {
        events.push(ModelStreamEvent::ReasoningDelta {
            index: 0,
            delta: reasoning.to_owned(),
        });
    }
    events.extend([
        ModelStreamEvent::ToolCallStart {
            index: 1,
            id: id.to_owned(),
            name: STRUCTURED_RESULT_TOOL.to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 1,
            id: id.to_owned(),
            delta: json!({"value": value}).to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 1,
            id: id.to_owned(),
        },
        ModelStreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: Some(input_tokens),
                output_tokens: Some(2),
                total_tokens: Some(input_tokens.saturating_add(2)),
                ..TokenUsage::unknown()
            },
        },
    ]);
    if let Some(duration_ms) = duration_ms {
        events.push(ModelStreamEvent::DecodeTiming { duration_ms });
    }
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::ToolUse,
    });
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

/// 创建使用整数 Schema 的原生结构化 Turn 请求。
fn native_structured_turn_request() -> TurnRequest {
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
    request
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

/// 以明确延迟返回成功或失败的工具，用于核验 PostHook 墙钟耗时。
struct DelayedOutcomeTool;

impl AgentTool for DelayedOutcomeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "delayed_outcome",
            "验证 PostHook 收到真实工具耗时",
            json!({
                "type": "object",
                "properties": {"fail": {"type": "boolean"}},
                "required": ["fail"],
                "additionalProperties": false
            }),
        )
    }

    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        let fail = input.get("fail").and_then(Value::as_bool) == Some(true);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if fail {
                Err(ToolError::permanent("delayed_failure", "合成工具失败"))
            } else {
                Ok(ToolOutput::text("合成工具成功"))
            }
        })
    }
}

/// 分别记录成功与失败 PostHook 看到的实测耗时。
#[derive(Default)]
struct ToolDurationHook {
    success: Mutex<Vec<u64>>,
    failure: Mutex<Vec<u64>>,
}

impl AgentHook for ToolDurationHook {
    fn name(&self) -> &str {
        "tool-duration-hook"
    }

    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.success
            .lock()
            .expect("成功耗时测试锁不应损坏")
            .push(context.duration_ms);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }

    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.failure
            .lock()
            .expect("失败耗时测试锁不应损坏")
            .push(context.duration_ms);
        Box::pin(async { Ok(ToolHookOutput::default()) })
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

/// 保存实时模型事件，验证结构化候选只在校验成功后对外可见。
#[derive(Default)]
struct RecordingModelEventSink {
    /// 按 Sink 确认顺序保存事件快照。
    events: Mutex<Vec<AgentStreamEvent>>,
}

impl RecordingModelEventSink {
    /// 返回实时事件快照。
    fn events(&self) -> Vec<AgentStreamEvent> {
        self.events
            .lock()
            .expect("结构化实时事件测试锁不应损坏")
            .clone()
    }

    /// 返回实时出口已经确认的文本增量。
    fn text(&self) -> String {
        self.events()
            .iter()
            .filter_map(|event| match event.kind() {
                AgentStreamEventKind::ModelEvent {
                    event: ModelStreamEvent::TextDelta { delta, .. },
                } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 返回实时出口已经确认的推理增量。
    fn reasoning(&self) -> String {
        self.events()
            .iter()
            .filter_map(|event| match event.kind() {
                AgentStreamEventKind::ModelEvent {
                    event:
                        ModelStreamEvent::ReasoningDelta { delta, .. }
                        | ModelStreamEvent::ReasoningSummaryDelta { delta, .. },
                } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl AgentEventSink for RecordingModelEventSink {
    /// 在返回前保存已经可靠接收的实时事件。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        self.events
            .lock()
            .expect("结构化实时事件测试锁不应损坏")
            .push(event.clone());
        Box::pin(async { Ok(()) })
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

/// 观察取消令牌后立即返回错误的挂起测试工具，用于外层墙钟超时切断。
struct HungTimeoutTool {
    /// 工具声明的外层墙钟上限。
    timeout: Option<Duration>,
    /// 工具声明的并发方式。
    concurrency: ToolConcurrency,
    /// 工具 Future 首次被轮询时发出的通知。
    started: Arc<Notify>,
    /// 工具是否真实观察到取消令牌。
    observed_cancellation: Arc<AtomicBool>,
}

impl AgentTool for HungTimeoutTool {
    /// 返回挂起测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "hung_timeout",
            "验证外层墙钟超时切断挂起工具",
            json!({ "type": "object", "additionalProperties": false }),
        )
    }

    /// 测试调用本身不产生外部副作用。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 返回测试预设的并发方式。
    fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    /// 返回测试预设的外层墙钟上限。
    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// 发出已启动通知后永久挂起，直到取消令牌触发才返回错误。
    fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let started = self.started.clone();
        let observed = self.observed_cancellation.clone();
        Box::pin(async move {
            started.notify_one();
            context.cancellation.cancelled().await;
            observed.store(true, Ordering::SeqCst);
            Err(ToolError::permanent("cancelled", "挂起工具已观察取消"))
        })
    }
}

/// 声明自管超时并短暂休眠后成功的测试工具，覆盖无外层墙钟的执行分支。
struct SelfManagedTool {
    /// 工具 Future 首次被轮询时发出的通知。
    started: Arc<Notify>,
}

impl AgentTool for SelfManagedTool {
    /// 返回自管超时测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "self_managed",
            "验证自管超时工具不受外层墙钟影响",
            json!({ "type": "object", "additionalProperties": false }),
        )
    }

    /// 测试调用本身不产生外部副作用。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 自管超时测试必须独占执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 自管超时：不施加外层墙钟。
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// 发出已启动通知后短暂休眠并成功返回。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let started = self.started.clone();
        Box::pin(async move {
            started.notify_one();
            tokio::time::sleep(Duration::from_millis(120)).await;
            Ok(ToolOutput::text("self-managed-ok"))
        })
    }
}

/// 提取一个 Turn 结果中按出现顺序排列的全部工具结果。
fn turn_tool_results(result: &TurnResult) -> Vec<&ToolResult> {
    result
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect()
}

/// 返回测试工具结果的唯一文本内容。
fn turn_tool_result_text(result: &ToolResult) -> &str {
    match result.content.as_slice() {
        [keencode_model::ToolResultContent::Text { text }] => text,
        _ => panic!("测试工具结果必须只包含一个文本块"),
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

/// 向单个 Runner 只发布一次目录变化，并记录实际读取边界。
struct OneShotToolCatalogUpdate {
    update: Mutex<Option<AgentToolCatalogDelta>>,
    calls: AtomicUsize,
    acknowledgements: AtomicUsize,
}

impl OneShotToolCatalogUpdate {
    fn new(update: AgentToolCatalogDelta) -> Self {
        Self {
            update: Mutex::new(Some(update)),
            calls: AtomicUsize::new(0),
            acknowledgements: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn acknowledgements(&self) -> usize {
        self.acknowledgements.load(Ordering::SeqCst)
    }
}

impl AgentToolCatalogUpdateSource for OneShotToolCatalogUpdate {
    fn take_update(&self) -> Result<Option<AgentToolCatalogDelta>, AgentToolCatalogUpdateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.update.lock().expect("目录变化测试锁不应损坏").take())
    }

    fn acknowledge_update(&self, _generation: u64) -> Result<(), AgentToolCatalogUpdateError> {
        self.acknowledgements.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 记录用量锚点在后续轮次从哪个持久消息位置开始增量估算。
#[derive(Default)]
struct UsageAnchorSuffixProbe {
    reads: Mutex<Vec<(usize, usize)>>,
}

impl UsageAnchorSuffixProbe {
    fn reads(&self) -> Vec<(usize, usize)> {
        self.reads.lock().expect("用量锚点测试锁不应损坏").clone()
    }
}

impl ContextTokenEstimator for UsageAnchorSuffixProbe {
    fn estimate_request(&self, request: &keencode_model::ModelRequest) -> u64 {
        JsonContextTokenEstimator.estimate_request(request)
    }

    fn estimate_messages(&self, messages: &[Message]) -> u64 {
        JsonContextTokenEstimator.estimate_messages(messages)
    }

    fn estimate_messages_from(
        &self,
        messages: &keencode_model::ModelMessages,
        start: usize,
    ) -> u64 {
        self.reads
            .lock()
            .expect("用量锚点测试锁不应损坏")
            .push((start, messages.len()));
        JsonContextTokenEstimator.estimate_messages_from(messages, start)
    }
}

/// 判断一条消息是否为 Runtime 私有的延迟工具目录通知。
fn is_tool_catalog_update_message(message: &Message) -> bool {
    message.role == MessageRole::Developer
        && message.is_meta
        && message.content.iter().any(|content| {
            matches!(
                content,
                ContentBlock::Text { text }
                    if text.contains("KeenCode Runtime 已在当前 Reason 边界原子更新延迟工具目录")
                        && text.contains("\"catalogGeneration\"")
            )
        })
}

fn tool_catalog_update_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|message| is_tool_catalog_update_message(message))
        .count()
}

#[test]
fn tool_catalog_delta_accepts_definition_only_generation_changes() {
    let delta = AgentToolCatalogDelta::new(7, Vec::new(), Vec::new())
        .expect("实现或配置换代可以保持工具名称集合不变");
    assert_eq!(delta.generation(), 7);
    assert!(delta.added().is_empty());
    assert!(delta.removed().is_empty());
}

#[test]
fn tool_catalog_delta_accepts_full_catalog_replacement() {
    let added = (0..512)
        .map(|index| format!("new_tool_{index:03}"))
        .collect::<Vec<_>>();
    let removed = (0..512)
        .map(|index| format!("old_tool_{index:03}"))
        .collect::<Vec<_>>();
    let delta = AgentToolCatalogDelta::new(8, added, removed)
        .expect("完整替换 512 项目录应允许同时报告全部新增和移除名称");

    assert_eq!(delta.added().len(), 512);
    assert_eq!(delta.removed().len(), 512);
}

#[test]
fn tool_catalog_delta_rejects_either_snapshot_over_capacity() {
    let oversized = (0..513)
        .map(|index| format!("tool_{index:03}"))
        .collect::<Vec<_>>();

    assert!(AgentToolCatalogDelta::new(9, oversized.clone(), Vec::new()).is_err());
    assert!(AgentToolCatalogDelta::new(9, Vec::new(), oversized).is_err());
}

/// 成功与失败 PostHook 都必须收到工具实际执行的墙钟耗时。
#[tokio::test]
async fn post_tool_hooks_receive_measured_execution_duration() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                (
                    "duration-success",
                    "delayed_outcome",
                    json!({"fail": false}),
                ),
                ("duration-failure", "delayed_outcome", json!({"fail": true})),
            ]),
            text_reply("完成"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(DelayedOutcomeTool))
        .expect("耗时测试工具应可注册");
    let hook = Arc::new(ToolDurationHook::default());
    let mut hooks = HookRegistry::new();
    hooks
        .register(hook.clone())
        .expect("耗时测试 Hook 应可注册");

    let result = runner(provider, registry)
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let success = hook.success.lock().expect("成功耗时测试锁不应损坏");
    let failure = hook.failure.lock().expect("失败耗时测试锁不应损坏");
    assert_eq!(success.len(), 1);
    assert_eq!(failure.len(), 1);
    assert!(success[0] >= 20, "成功工具耗时应来自 25ms 真实延迟");
    assert!(failure[0] >= 20, "失败工具耗时应来自 25ms 真实延迟");
}

/// 同一逻辑 Round 的空响应重试必须继续携带同一通知，但通知不能进入权威 Transcript。
#[tokio::test]
async fn tool_catalog_update_is_transient_and_reused_by_same_round_retry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply("重试完成"),
        ],
    ));
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(
            2,
            vec!["mcp__new__tool".to_owned()],
            vec!["mcp__old__tool".to_owned()],
        )
        .unwrap(),
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_tool_catalog_update_source(updates.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(updates.calls(), 1, "同一逻辑 Round 的重试不得重复读取目录");
    assert_eq!(
        updates.acknowledgements(),
        1,
        "同一逻辑 Round 只需在首次成功 Provider 请求后确认一次"
    );
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_catalog_update_count(&requests[0].messages), 1);
    assert_eq!(tool_catalog_update_count(&requests[1].messages), 1);
    assert_eq!(
        serde_json::to_vec(&requests[0]).unwrap(),
        serde_json::to_vec(&requests[1]).unwrap(),
        "空响应重试应逐字节复用包含目录通知的请求"
    );
    assert_eq!(tool_catalog_update_count(&result.messages), 0);
}

/// Provider 的不可恢复失败不能确认目录变化，下一次 Runner 才能重新投递。
#[tokio::test]
async fn tool_catalog_update_is_not_acknowledged_after_provider_failure() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [invalid_request_error_reply("不可恢复的测试失败")],
    ));
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(11, vec!["mcp__new__tool".to_owned()], Vec::new()).unwrap(),
    ));
    let result = runner(provider, ToolRegistry::new())
        .with_tool_catalog_update_source(updates.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.error.is_some());
    assert_eq!(updates.acknowledgements(), 0);
}

/// 目录通知只属于观察到换代的模型 Round，后续 Round 不得再次注入。
#[tokio::test]
async fn tool_catalog_update_is_delivered_once_per_runner_generation() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("catalog-call", "record", json!({"value": "work"}))]),
            text_reply("完成"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(3, vec!["mcp__new__tool".to_owned()], Vec::new()).unwrap(),
    ));
    let result = runner(provider.clone(), registry)
        .with_tool_catalog_update_source(updates.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(updates.calls(), 2);
    assert_eq!(updates.acknowledgements(), 1);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_catalog_update_count(&requests[0].messages), 1);
    assert_eq!(tool_catalog_update_count(&requests[1].messages), 0);
    assert_eq!(tool_catalog_update_count(&result.messages), 0);
}

/// 瞬时通知不能把用量锚点的持久消息水位向前推一位。
#[tokio::test]
async fn tool_catalog_update_does_not_advance_persistent_usage_anchor() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_context_tokens: Some(1_000_000),
            ..ProviderCapabilities::default()
        },
        [
            ScriptedReply::events([
                ModelStreamEvent::MessageStart {
                    metadata: ResponseMetadata::default(),
                },
                ModelStreamEvent::ToolCallStart {
                    index: 0,
                    id: "catalog-anchor-call".to_owned(),
                    name: "record".to_owned(),
                },
                ModelStreamEvent::ToolCallArgumentsDelta {
                    index: 0,
                    id: "catalog-anchor-call".to_owned(),
                    delta: json!({"value": "work"}).to_string(),
                },
                ModelStreamEvent::ToolCallEnd {
                    index: 0,
                    id: "catalog-anchor-call".to_owned(),
                },
                ModelStreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: Some(100),
                        output_tokens: Some(10),
                        total_tokens: Some(110),
                        ..TokenUsage::unknown()
                    },
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: StopReason::ToolUse,
                },
            ]),
            text_reply("完成"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(5, vec!["mcp__new__tool".to_owned()], Vec::new()).unwrap(),
    ));
    let estimator = Arc::new(UsageAnchorSuffixProbe::default());
    let context = ContextManager::new(
        ContextPolicy::default(),
        estimator.clone(),
        Arc::new(ProviderContextCompressor::new(provider.clone())),
    )
    .unwrap();

    let result = runner(provider, registry)
        .with_context_manager(context)
        .with_tool_catalog_update_source(updates)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let reads = estimator.reads();
    assert!(!reads.is_empty(), "第二轮应使用用量锚点执行增量估算");
    assert!(
        reads
            .iter()
            .all(|&(start, message_count)| start == 1 && message_count == 3),
        "锚点应跨过首轮唯一持久 user 消息，不得计入瞬时通知：{reads:?}"
    );
}

/// Provider 超限触发的摘要输入不能看到瞬时目录通知；恢复采样仍复用原通知。
#[tokio::test]
async fn tool_catalog_update_is_excluded_from_compaction_input() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            context_overflow_error_reply(),
            text_reply("强制摘要"),
            text_reply("压缩后完成"),
        ],
    ));
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(4, Vec::new(), Vec::new()).unwrap(),
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_tool_catalog_update_source(updates.clone())
        .run_turn(turn_request_with_messages(compactable_tool_history()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(updates.calls(), 1);
    assert_eq!(updates.acknowledgements(), 1);
    assert_eq!(result.compactions.len(), 1);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 3);
    assert_eq!(tool_catalog_update_count(&requests[0].messages), 1);
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    assert!(requests[1].tools.is_empty());
    assert_eq!(tool_catalog_update_count(&requests[1].messages), 0);
    assert_eq!(tool_catalog_update_count(&requests[2].messages), 1);
    assert_eq!(tool_catalog_update_count(&result.messages), 0);
}

/// 即使目录通知本身超过小窗口，也必须在 Provider 调用前安全失败；通知仍
/// 不得被送入压缩输入或通过远端超限来兜底。
#[tokio::test]
async fn oversized_tool_catalog_update_fails_closed_before_provider_call() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_context_tokens: Some(128),
            ..ProviderCapabilities::default()
        },
        [text_reply("不应被调用")],
    ));
    let added = (0..512)
        .map(|index| format!("added_{index:03}_{}", "a".repeat(240)))
        .collect::<Vec<_>>();
    let removed = (0..512)
        .map(|index| format!("removed_{index:03}_{}", "b".repeat(239)))
        .collect::<Vec<_>>();
    let updates = Arc::new(OneShotToolCatalogUpdate::new(
        AgentToolCatalogDelta::new(6, added, removed).unwrap(),
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_tool_catalog_update_source(updates.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(result.error, Some(AgentRunError::Context(_))));
    assert!(provider.requests().unwrap().is_empty());
    assert_eq!(updates.acknowledgements(), 0);
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

/// 未请求结构化输出时，JSON 外观及其合法性都不能改变普通文本完成语义。
#[tokio::test]
async fn ordinary_turn_keeps_invalid_json_like_text_as_text() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("{not valid json")],
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert!(result.structured_output.is_none());
    assert_eq!(
        result
            .final_response
            .as_ref()
            .map(|response| &response.content),
        Some(&vec![ContentBlock::text("{not valid json")])
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 1);
}

/// 主 Turn 请求按能力快照接线输出上限：设置值优先于窗口派生。
#[tokio::test]
async fn main_turn_request_wires_configured_max_output_tokens() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_context_tokens: Some(200_000),
            max_output_tokens: Some(96_000),
            ..ProviderCapabilities::default()
        },
        [text_reply("完成")],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_output_tokens, Some(96_000));
}

/// 没有设置值时按已知窗口派生默认输出上限：窗口四分之一并封顶到 32_000。
#[tokio::test]
async fn main_turn_request_derives_max_output_tokens_from_known_window() {
    for (window, expected) in [
        (200_000_u64, Some(32_000_u32)),
        (8_000, Some(2_000)),
        (u64::MAX, Some(32_000)),
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                max_context_tokens: Some(window),
                ..ProviderCapabilities::default()
            },
            [text_reply("完成")],
        ));
        let result = runner(provider.clone(), ToolRegistry::new())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        assert!(result.is_success(), "window={window}: {:?}", result.error);
        let requests = provider.requests().expect("请求快照应可读取");
        assert_eq!(requests.len(), 1, "window={window}");
        assert_eq!(requests[0].max_output_tokens, expected, "window={window}");
    }
}

/// 设置值超过 u32 范围时饱和到 u32::MAX，请求仍然有效。
#[tokio::test]
async fn main_turn_request_saturates_configured_max_output_tokens_to_u32() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(u64::MAX),
            ..ProviderCapabilities::default()
        },
        [text_reply("完成")],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_output_tokens, Some(u32::MAX));
}

/// 设置值和窗口都未知时保持 None，由 Adapter 按各协议兜底。
#[tokio::test]
async fn main_turn_request_keeps_max_output_tokens_unset_without_capabilities() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("完成")],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_output_tokens, None);
}

/// 普通文本区分模型终止原因与协议错误，并保留截断或拒答已确认的文本。
/// MaxOutputTokens 纯文本截断改由输出上限有界恢复处理（见
/// `max_output_truncation_recovers_with_instruction_and_completes`），不再产生终态。
#[tokio::test]
async fn ordinary_text_classifies_model_stop_reasons_and_protocol_errors() {
    for stop_reason in [
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
            StopReason::MaxOutputTokens | StopReason::Completed | StopReason::ToolUse => {
                unreachable!()
            }
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

/// 完整工具调用由内容块决定是否执行；兼容 Provider 的三种非终止原因均可继续。
#[tokio::test]
async fn ordinary_tool_calls_follow_complete_content_before_stop_reason() {
    for stop_reason in [
        StopReason::ToolUse,
        StopReason::Completed,
        StopReason::Other {
            reason: "provider_pause".to_owned(),
        },
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply_with_stop(
                    &[("call-complete-stop", "record", json!({"value": "write"}))],
                    stop_reason.clone(),
                ),
                text_reply("工具已执行"),
            ],
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

        assert!(result.is_success(), "{stop_reason:?}: {:?}", result.error);
        assert_eq!(result.state.step_count(), 1);
        assert_eq!(tool.call_count(), 1);
        assert_eq!(result.messages.len(), 4);
        assert_eq!(turn_tool_results(&result).len(), 1);
    }
}

/// 缺少终止原因的完整工具调用不能因为被归一为 Other 而获得执行资格。
#[tokio::test]
async fn ordinary_tool_calls_reject_missing_other_stop_reasons_before_execution() {
    for reason in [
        "",
        "   ",
        "\t\n",
        "missing_finish_reason",
        "missing_stop_reason",
        "missing_status",
        "missing",
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply_with_stop(
                &[("call-missing-stop", "record", json!({"value": "write"}))],
                StopReason::Other {
                    reason: reason.to_owned(),
                },
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
            .expect("缺失终止原因测试工具应可注册");
        let result = runner(provider, registry)
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        assert!(matches!(
            result.error,
            Some(AgentRunError::InvalidResponse { ref message })
                if message.contains("不允许执行工具")
        ));
        assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
        assert_eq!(result.state.step_count(), 0);
        assert_eq!(tool.call_count(), 0);
        assert_eq!(result.messages.len(), 1);
    }
}

/// MaxOutputTokens、ContentFilter 和 Cancelled 始终阻止工具执行。
#[tokio::test]
async fn ordinary_tool_calls_keep_terminal_stop_reasons_fail_closed() {
    for stop_reason in [
        StopReason::MaxOutputTokens,
        StopReason::ContentFilter,
        StopReason::Cancelled,
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
            StopReason::Completed | StopReason::ToolUse | StopReason::Other { .. } => {
                unreachable!()
            }
        }
        assert_eq!(result.state.step_count(), 0);
        assert_eq!(tool.call_count(), 0);
        assert_eq!(result.messages.len(), 1);
    }
}

/// 兼容端点用结束原因自报失败时，必须展示上游原因而不是归咎于工具或本地协议。
#[tokio::test]
async fn provider_failure_stop_reason_reports_upstream_cause_not_tools() {
    // 纯文本响应：保留已收到的正文，并把上游原因作为终态错误上报。
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply_with_stop(
            "已收到的部分正文",
            StopReason::Other {
                reason: "error".to_owned(),
            },
        )],
    ));
    let result = runner(provider, ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    match &result.error {
        Some(AgentRunError::Model(ModelError::ProviderUnavailable { message, .. })) => {
            assert!(message.contains("error"), "{message}");
        }
        other => panic!("应归因为带上游原因的上游错误，实际为 {other:?}"),
    }
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(result.messages.len(), 2);
    assert!(
        result
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(
                |block| matches!(block, ContentBlock::Text { text } if text == "已收到的部分正文")
            )
    );

    // 带完整工具调用：同样按上游失败收尾，绝不进入工具执行或报成工具相关错误。
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply_with_stop(
            &[("call-upstream-error", "record", json!({"value": "write"}))],
            StopReason::Other {
                reason: "server_error".to_owned(),
            },
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
        .expect("上游失败测试工具应可注册");
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    match &result.error {
        Some(AgentRunError::Model(ModelError::ProviderUnavailable { message, .. })) => {
            assert!(message.contains("server_error"), "{message}");
            assert!(
                !message.contains("工具"),
                "上游失败不得归咎于工具：{message}"
            );
        }
        other => panic!("应有工具调用时仍归因为上游错误，实际为 {other:?}"),
    }
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(tool.call_count(), 0);
    // 分类也必须落在上游故障而不是未知错误，便于前端与 Hook 区分原因。
    assert_eq!(
        agent_run_error_category(result.error.as_ref().expect("上游失败应有终态错误")),
        "server_error"
    );
}

/// 悬空或参数 JSON 不完整的工具块在模型流归约阶段失败，绝不能进入执行器。
#[tokio::test]
async fn malformed_or_incomplete_tool_blocks_fail_closed_before_execution() {
    for malformed in [false, true] {
        let mut events = vec![
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 0,
                id: "call-incomplete".to_owned(),
                name: "record".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 0,
                id: "call-incomplete".to_owned(),
                delta: if malformed {
                    "{\"value\":".to_owned()
                } else {
                    "{\"value\":1}".to_owned()
                },
            },
        ];
        if malformed {
            events.push(ModelStreamEvent::ToolCallEnd {
                index: 0,
                id: "call-incomplete".to_owned(),
            });
        }
        events.push(ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        });

        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [ScriptedReply::events(events)],
        ));
        let tool = Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ChangesState,
            ToolConcurrency::Exclusive,
        ));
        let mut registry = ToolRegistry::new();
        registry
            .register(tool.clone())
            .expect("不完整工具测试工具应可注册");
        let result = runner(provider, registry)
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        assert!(matches!(
            result.error,
            Some(AgentRunError::Model(ModelError::Protocol { .. }))
        ));
        assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
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

/// 空响应有界重试：第一次空、第二次正常时 Turn 成功，消息只包含第二次内容，
/// 两次真实调用按同一 Round 的不同调用尝试分别记账。
#[tokio::test]
async fn empty_response_retries_once_and_completes_with_second_content() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply("第二次响应"),
        ],
    ));
    let sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.messages.len(), 2);
    let texts = result.messages[1]
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["第二次响应"]);
    let usages = sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
}

/// 空响应有界重试：连续两次空响应在消耗唯一重试机会后按原语义进入 InvalidResponse 终态。
#[tokio::test]
async fn consecutive_empty_responses_fail_with_invalid_response_after_one_retry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            empty_reply_with_stop(StopReason::Completed),
        ],
    ));
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(usage_sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::InvalidResponse {
            message: "模型响应没有任何内容块".to_owned(),
        })
    );
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.messages.len(), 1);
    // 两次采样的用量都按同一 Round 的独立调用尝试提交。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
}

/// 原生结构化输出在五次纠正均失败后，以第六个响应的 MissingOutput 诊断结束。
#[tokio::test]
async fn structured_native_empty_responses_fail_after_five_corrections() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        (0..6).map(|_| empty_reply_with_stop(StopReason::Completed)),
    ));
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
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
        .with_commit_sink(usage_sink.clone())
        .run_turn(request)
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::MissingOutput,
            ..
        }))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 6);
    assert_eq!(result.messages.len(), 1);
    assert!(result.structured_output.is_none());
    // 六次采样的用量都按同一 Round 的独立调用尝试提交。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 6);
    for (index, usage) in usages.iter().enumerate() {
        assert_eq!(usage.model_round(), 1);
        assert_eq!(usage.call_attempt(), u32::try_from(index + 1).unwrap());
    }
}

/// 空响应有界重试同样适用于工具模拟结构化输出：重试后收到合法保留结果调用即成功。
#[tokio::test]
async fn tool_emulated_empty_response_retries_then_accepts_result_tool() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            empty_reply_with_stop(StopReason::Completed),
            tool_reply(&[(
                "call-result",
                STRUCTURED_RESULT_TOOL,
                json!({"value": {"answer": 42}}),
            )]),
        ],
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
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(usage_sink.clone())
        .run_turn(request)
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.state.step_count(), 0);
    // 两次采样的用量都按同一 Round 的独立调用尝试提交。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::ToolUse);
}

/// 显式上限的摘要轮是 ToolChoice::None 的特殊轮：空响应直接返回已确定的限流终态，不消耗重试。
#[tokio::test]
async fn limit_summary_empty_response_keeps_limit_error_without_retry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
            empty_reply_with_stop(StopReason::Completed),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let result = AgentRunner::new(
        provider.clone(),
        registry,
        RunLimits {
            max_rounds: Some(1),
            ..RunLimits::default()
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::LimitReached {
            counter: CounterKind::Round,
            maximum: 1,
        })
    );
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(tool.call_count(), 1);
}

/// 空响应重试后的非空响应仍要经过终止原因检查：纯文本截断不因发生过空响应
/// 重试而绕过检查，改按输出上限有界恢复注入续跑指令继续下一 Round；
/// 空响应重试预算与恢复预算互不挤占。
#[tokio::test]
async fn empty_response_retry_then_max_output_tokens_recovers_without_terminal() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply_with_stop("partial", StopReason::MaxOutputTokens),
            text_reply("续跑完成"),
        ],
    ));
    let sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 3);
    assert_eq!(result.messages.len(), 4);
    assert!(result.messages[2].is_meta);
    let usages = sink.usages();
    assert_eq!(usages.len(), 3);
    // 空响应与其重试同属 Round 1 的两次调用尝试；截断部分响应照常记账。
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(
        usages[1].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[2].model_round(), 2);
    assert_eq!(usages[2].call_attempt(), 3);
    assert_eq!(usages[2].completion().stop_reason, StopReason::Completed);
}

/// 在确认到指定序号的模型流事件时触发 Turn 取消的实时 Sink。
struct CancelOnNthModelEventSink {
    /// 在事件确认期间触发的取消令牌。
    cancellation: TurnCancellation,
    /// 触发取消前仍允许正常确认的模型流事件计数。
    remaining_before_cancel: AtomicUsize,
}

impl AgentEventSink for CancelOnNthModelEventSink {
    /// 确认模型事件并在计数归零时取消 Turn；失败边界与运行时事件不计数。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async move {
            if matches!(event.kind(), AgentStreamEventKind::ModelEvent { .. })
                && self.remaining_before_cancel.fetch_sub(1, Ordering::SeqCst) == 1
            {
                self.cancellation.cancel();
            }
            Ok(())
        })
    }
}

/// 取消发生在重试的 request_model 内部时，Turn 以 Cancelled 终态结束且不产生 InvalidResponse。
#[tokio::test]
async fn empty_response_retry_cancelled_inside_retry_request_model() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply("取消后不应被归约成完整响应"),
        ],
    ));
    let cancellation = TurnCancellation::new();
    // 尝试 1 的空响应产生两个模型事件；第 3 个事件是重试请求的 MessageStart。
    let cancel_sink = Arc::new(CancelOnNthModelEventSink {
        cancellation: cancellation.clone(),
        remaining_before_cancel: AtomicUsize::new(3),
    });
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation);
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_event_sink(cancel_sink)
        .with_commit_sink(usage_sink.clone())
        .run_turn(request)
        .await;

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    // 重试请求已经真实发起：取消是在重试 request_model 内部被观察的。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.messages.len(), 1);
    // 半提交语义：尝试 1 的用量在重试发起前已独立提交，重试失败不回滚。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
}

/// 空响应重试的已知不对称：重试的 request_model 报 ContextLengthExceeded 时直接以
/// Failed(Model) 终态传播，不走强制压缩 match（重试请求与刚被接受的请求逐字节相同）。
#[tokio::test]
async fn empty_response_retry_context_overflow_propagates_model_failure_without_compression() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            ScriptedReply::new(vec![
                Ok(ModelStreamEvent::MessageStart {
                    metadata: ResponseMetadata::default(),
                }),
                Ok(ModelStreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: Some(11),
                        output_tokens: Some(2),
                        total_tokens: Some(13),
                        ..TokenUsage::unknown()
                    },
                }),
                Err(ModelError::ContextLengthExceeded {
                    message: "重试请求上下文超限".to_owned(),
                }),
            ]),
        ],
    ));
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(usage_sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    // 若未来把该错误误路由进强制压缩 match，终态会变成 ContextBlocked/StillExceeded。
    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(
            ModelError::ContextLengthExceeded { .. }
        ))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(result.state.round_count(), 1);
    assert!(result.compactions.is_empty());
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    // 半提交 Round：尝试 1 的成功用量与重试失败的用量都已独立提交。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(
        usages[1].completion().stop_reason,
        StopReason::Other {
            reason: "model_error".to_owned(),
        }
    );
}

/// 交叉 Round 预算：空响应重试机会不跨 Round 保留，Round 2 的首次空响应直接 InvalidResponse 终态。
#[tokio::test]
async fn empty_response_retry_budget_does_not_carry_across_rounds() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            tool_reply(&[("call-round-1", "record", json!({"value": "work"}))]),
            empty_reply_with_stop(StopReason::Completed),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), registry)
        .with_commit_sink(usage_sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::InvalidResponse {
            message: "模型响应没有任何内容块".to_owned(),
        })
    );
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    // Round 1 消耗了唯一重试；Round 2 的空响应没有再获得第三次重试请求。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 3);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.call_count(), 1);
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 3);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[2].model_round(), 2);
    assert_eq!(usages[2].call_attempt(), 3);
}

/// 空响应重试成功后仍要经过 loop 后交叉校验：stop_reason 为 tool_use 但没有工具调用块时
/// 按 InvalidResponse 终态结束，重试不放宽响应完整性约束。
#[tokio::test]
async fn empty_response_retry_success_still_cross_checks_stop_reason() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply_with_stop("有文本但以 tool_use 结束", StopReason::ToolUse),
        ],
    ));
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(usage_sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::InvalidResponse {
            message: "模型以工具调用结束但没有返回工具调用内容块".to_owned(),
        })
    );
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.messages.len(), 1);
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::ToolUse);
}

/// 空响应重试必须逐字节复用同一请求：Provider 记录的两次请求序列化后完全相等。
#[tokio::test]
async fn empty_response_retry_resends_byte_identical_request() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::Completed),
            text_reply("第二次响应"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    let first = serde_json::to_vec(&requests[0]).expect("首次请求应可序列化");
    let retry = serde_json::to_vec(&requests[1]).expect("重试请求应可序列化");
    assert_eq!(first, retry);
}

/// 创建模型流直接返回 400 InvalidRequest 错误的脚本响应，等价于
/// Provider 层把 "max_tokens must be between 1 and N" 类 400 归一后的形态。
fn invalid_request_error_reply(message: &str) -> ScriptedReply {
    ScriptedReply::new(vec![Err(ModelError::InvalidRequest {
        message: message.to_owned(),
    })])
}

/// 创建模型流直接返回上下文超限错误的脚本响应。
fn context_overflow_error_reply() -> ScriptedReply {
    ScriptedReply::new(vec![Err(ModelError::ContextLengthExceeded {
        message: "重试请求上下文超限".to_owned(),
    })])
}

/// 创建可被强制压缩的带工具交换历史。
fn compactable_tool_history() -> Vec<Message> {
    vec![
        Message::text(MessageRole::System, "system 必须原样保留"),
        Message::text(MessageRole::Developer, "developer 必须原样保留"),
        Message::text(MessageRole::User, "旧问题".repeat(200)),
        Message::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::text("准备读取文件"),
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new("call-1", "read", json!({ "path": "a.rs" })),
                },
            ],
        ),
        Message::new(
            MessageRole::Tool,
            vec![ContentBlock::ToolResult {
                tool_result: ToolResult::text("call-1", "文件内容".repeat(200), false),
            }],
        ),
        Message::text(MessageRole::User, "近期问题"),
        Message::text(MessageRole::Assistant, "近期回答"),
    ]
}

/// 创建携带自定义历史的最小用户 Turn 请求。
fn turn_request_with_messages(messages: Vec<Message>) -> TurnRequest {
    TurnRequest::new(
        session_id("session-runner"),
        turn_id("turn-runner"),
        agent_id("agent-runner"),
        "test-model",
        messages,
        PlanGuard::inactive(),
    )
}

/// 配置输出上限被厂商 400 判定超限后，Turn 以不携带输出上限的降级请求重试
/// 一次并完成；匹配大小写不敏感，降级请求不再携带任何输出上限。
#[tokio::test]
async fn max_tokens_invalid_request_degrades_to_none_and_completes_turn() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            // 大写形态同时钉住启发式匹配的大小写不敏感语义。
            invalid_request_error_reply("MAX_TOKENS must be between 1 and 8192"),
            text_reply("恢复成功"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_output_tokens, Some(8_192));
    assert_eq!(requests[1].max_output_tokens, None);
    assert_eq!(result.state.round_count(), 1);
}

/// Turn 内记忆降级：置位后第二轮请求直接不携带输出上限，不再先吃一次 400。
#[tokio::test]
async fn max_tokens_degradation_wires_none_for_subsequent_rounds() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            tool_reply(&[("call-round-1", "record", json!({ "value": "work" }))]),
            text_reply("完成"),
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

    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].max_output_tokens, Some(8_192));
    assert_eq!(requests[1].max_output_tokens, None);
    assert_eq!(requests[2].max_output_tokens, None);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.call_count(), 1);
}

/// 400 消息不含 "max_tokens" 时不触发降级：保持既有 InvalidRequest 终态、无重试。
#[tokio::test]
async fn invalid_request_without_max_tokens_message_keeps_terminal_semantics() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            invalid_request_error_reply("请求包含不允许的参数"),
            text_reply("不应被消费"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::InvalidRequest { .. }))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 1);
    assert_eq!(provider.remaining_replies(), Ok(1));
}

/// 请求本就不携带输出上限时，含 "max_tokens" 的 400 也不匹配降级臂：
/// 保持既有终态并防止无限降级。
#[tokio::test]
async fn max_tokens_invalid_request_without_wired_limit_keeps_terminal_semantics() {
    // 能力快照无设置值、无窗口：接线结果本身就是 None。
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            text_reply("不应被消费"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::InvalidRequest { .. }))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_output_tokens, None);
    assert_eq!(provider.remaining_replies(), Ok(1));
}

/// 降级重试再遇上下文超限时按臂顺序进入既有强制压缩臂：
/// 先降级一次、再压缩一次，恢复请求仍不携带输出上限。
#[tokio::test]
async fn degraded_retry_context_overflow_walks_forced_compaction_arm() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            context_overflow_error_reply(),
            text_reply("强制摘要"),
            text_reply("恢复成功"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request_with_messages(compactable_tool_history()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].max_output_tokens, Some(8_192));
    assert_eq!(requests[1].max_output_tokens, None);
    // 第三个请求是强制压缩摘要请求：无工具且 tool_choice 为 None。
    assert!(requests[2].tools.is_empty());
    assert_eq!(requests[2].tool_choice, ToolChoice::None);
    // 摘要请求不受主请求降级影响：按策略默认 16_000 发起，接线时被
    // Provider 能力 8_192 钳制，仍与主请求降级后的 None 无关。
    assert_eq!(requests[2].max_output_tokens, Some(8_192));
    // 恢复请求仍保持降级状态，不重新携带输出上限。
    assert_eq!(requests[3].max_output_tokens, None);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.compactions.len(), 1);
    assert_eq!(
        result.compactions[0].trigger,
        ContextCompressionTrigger::ProviderOverflow
    );
}

/// 强制压缩臂的重试错误回到错误处理循环按臂顺序重新判定：压缩重试返回
/// "max_tokens" 400 时不绕过降级臂，降级为不携带输出上限的请求重试并完成。
#[tokio::test]
async fn forced_compaction_retry_max_tokens_invalid_request_degrades_and_completes_turn() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            context_overflow_error_reply(),
            text_reply("强制摘要"),
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            text_reply("恢复成功"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request_with_messages(compactable_tool_history()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 4);
    // 首错 CLE：首次请求仍携带接线输出上限。
    assert_eq!(requests[0].max_output_tokens, Some(8_192));
    // 第二个请求是强制压缩摘要请求。
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    // 压缩重试仍携带原输出上限并命中 400，随后进入降级臂。
    assert_eq!(requests[2].max_output_tokens, Some(8_192));
    // 降级重试不携带输出上限并成功完成。
    assert_eq!(requests[3].max_output_tokens, None);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.compactions.len(), 1);
    assert_eq!(
        result.compactions[0].trigger,
        ContextCompressionTrigger::ProviderOverflow
    );
}

/// 降级重试被取消时按取消优先语义以 Cancelled 终态结束，且重试请求已真实发起。
#[tokio::test]
async fn degraded_retry_cancelled_inside_retry_request_model() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            text_reply("取消后不应被归约成完整响应"),
        ],
    ));
    let cancellation = TurnCancellation::new();
    // 首次 400 是纯错误流（0 个模型事件）；降级重试的第一个 MessageStart 触发取消。
    let cancel_sink = Arc::new(CancelOnNthModelEventSink {
        cancellation: cancellation.clone(),
        remaining_before_cancel: AtomicUsize::new(1),
    });
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation);
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_event_sink(cancel_sink)
        .with_commit_sink(usage_sink.clone())
        .run_turn(request)
        .await;

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    // 降级重试已经真实发起：取消是在重试 request_model 内部被观察的。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    // 两次失败调用都没有明确 Usage 事实，不伪造已消耗用量。
    assert!(usage_sink.usages().is_empty());
}

/// 每个 Turn 只降级一次：第二轮在已置位后再次出现含 "max_tokens" 的 400
/// 直接按既有终态结束，不允许第二次降级重试。
#[tokio::test]
async fn max_tokens_invalid_request_degrades_only_once_per_turn() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            max_output_tokens: Some(8_192),
            ..ProviderCapabilities::default()
        },
        [
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            tool_reply(&[("call-round-1", "record", json!({ "value": "work" }))]),
            invalid_request_error_reply("max_tokens must be between 1 and 8192"),
            text_reply("不应被消费"),
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

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::InvalidRequest { .. }))
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].max_output_tokens, Some(8_192));
    assert_eq!(requests[1].max_output_tokens, None);
    assert_eq!(requests[2].max_output_tokens, None);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(provider.remaining_replies(), Ok(1));
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

/// 原生结构化输出首次 Schema 失败后，在同一 Round 的第一次私有纠正中成功。
#[tokio::test]
async fn runner_corrects_first_invalid_native_structured_response() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [text_reply("{\"answer\":0}"), text_reply("{\"answer\":42}")],
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

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(result.messages.len(), 2);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    assert!(requests[1].tools.is_empty());
    assert!(requests[1].structured_output.is_some());
    assert_eq!(requests[1].messages.len(), 3);
    assert!(requests[1].messages[2].is_meta);
}

/// 合法原生结构化候选的实时正文与权威 Assistant 内容保持一致。
#[tokio::test]
async fn structured_native_live_sink_publishes_valid_candidate_only() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [structured_text_reply("最终推理", "{\"answer\":42}")],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(event_sink.text(), "{\"answer\":42}");
    assert_eq!(event_sink.reasoning(), "最终推理");
    assert!(result.messages[1].content.iter().any(|block| {
        matches!(block, ContentBlock::Text { text } if text == "{\"answer\":42}")
    }));
}

/// 结构化候选通过校验后，实时事件保持 Provider 顺序并可完整重放。
#[tokio::test]
async fn structured_native_live_sink_preserves_order_for_replay() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [structured_text_reply_with_telemetry(
            "{\"answer\":42}",
            17,
            23,
        )],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let model_events = event_sink
        .events()
        .into_iter()
        .filter_map(|event| match event.into_kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(event),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. }
            | AgentStreamEventKind::ContextCompactionTruncated { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        model_events.as_slice(),
        [
            ModelStreamEvent::MessageStart { .. },
            ModelStreamEvent::TextDelta { delta: text, .. },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::DecodeTiming { duration_ms: 23 },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ] if text == "{\"answer\":42}"
            && usage.input_tokens == Some(17)
            && usage.output_tokens == Some(1)
            && usage.total_tokens == Some(18)
    ));

    let replayed = collect_model_stream(Box::pin(stream::iter(
        model_events.into_iter().map(Ok::<_, ModelError>),
    )))
    .await
    .expect("实时 Sink 已确认事件应当可以完整重放");
    assert_eq!(
        replayed.content, result.messages[1].content,
        "实时事件重放结果必须与权威 Assistant 内容一致"
    );
    assert_eq!(replayed.usage.input_tokens, Some(17));
    assert_eq!(replayed.metadata.decode_duration_ms, Some(23));
}

/// 工具模拟结构化候选通过校验后，推理、工具调用和遥测按原始顺序重放。
#[tokio::test]
async fn structured_emulated_live_sink_preserves_tool_event_order_for_replay() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [structured_result_reply(
            "先推理",
            "result-1",
            json!({"answer": 42}),
            17,
            None,
        )],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let events = event_sink.events();
    assert!(events.iter().all(|event| event.model_round() == 1));
    let model_events = events
        .into_iter()
        .filter_map(|event| match event.into_kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(event),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. }
            | AgentStreamEventKind::ContextCompactionTruncated { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        model_events.as_slice(),
        [
            ModelStreamEvent::MessageStart { .. },
            ModelStreamEvent::ReasoningDelta { delta: reasoning, .. },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: start_id,
                name,
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: arguments_id,
                delta: arguments,
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: end_id,
            },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ] if reasoning == "先推理"
            && start_id == "result-1"
            && name == STRUCTURED_RESULT_TOOL
            && arguments_id == "result-1"
            && arguments == &json!({"value": {"answer": 42}}).to_string()
            && end_id == "result-1"
            && usage.input_tokens == Some(17)
            && usage.output_tokens == Some(2)
            && usage.total_tokens == Some(19)
    ));

    let replayed = collect_model_stream(Box::pin(stream::iter(
        model_events.into_iter().map(Ok::<_, ModelError>),
    )))
    .await
    .expect("实时 Sink 已确认事件应当可以完整重放");
    assert_eq!(replayed.stop_reason, StopReason::ToolUse);
    assert!(replayed.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolCall { tool_call }
                if tool_call.id == "result-1"
                    && tool_call.name == STRUCTURED_RESULT_TOOL
        )
    }));
    assert_eq!(replayed.usage.input_tokens, Some(17));
}

/// 原生结构化候选先坏后好时，实时 Sink 与冷恢复权威消息都只包含最终候选。
#[tokio::test]
async fn structured_native_live_sink_drops_invalid_candidate_before_correction() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            structured_text_reply("坏推理", "{\"answer\":0}"),
            structured_text_reply("好推理", "{\"answer\":42}"),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(event_sink.text(), "{\"answer\":42}");
    assert_eq!(event_sink.reasoning(), "好推理");
    assert_eq!(result.messages.len(), 2);
    assert!(result.messages[1].content.iter().any(|block| {
        matches!(block, ContentBlock::Text { text } if text == "{\"answer\":42}")
    }));
    assert!(
        !result.messages[1].content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text == "{\"answer\":0}")
        })
    );

    let events = event_sink.events();
    assert!(events.iter().all(|event| event.model_round() == 1));
    let model_events = events
        .into_iter()
        .filter_map(|event| match event.into_kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(event),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. }
            | AgentStreamEventKind::ContextCompactionTruncated { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        model_events.as_slice(),
        [
            ModelStreamEvent::MessageStart { .. },
            ModelStreamEvent::ReasoningDelta { delta: reasoning, .. },
            ModelStreamEvent::TextDelta { delta: text, .. },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ] if reasoning == "好推理" && text == "{\"answer\":42}"
    ));
    let replayed = collect_model_stream(Box::pin(stream::iter(
        model_events.into_iter().map(Ok::<_, ModelError>),
    )))
    .await
    .expect("有效候选的实时事件应当可以完整重放");
    assert_eq!(replayed.content, result.messages[1].content);
}

/// 无效原生候选的用量、计时和结束事件都不能残留在实时出口。
#[tokio::test]
async fn structured_native_live_sink_drops_invalid_candidate_telemetry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            structured_text_reply_with_telemetry("{\"answer\":0}", 101, 11),
            structured_text_reply_with_telemetry("{\"answer\":42}", 202, 22),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let model_events = event_sink
        .events()
        .into_iter()
        .filter_map(|event| match event.into_kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(event),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. }
            | AgentStreamEventKind::ContextCompactionTruncated { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        model_events.as_slice(),
        [
            ModelStreamEvent::MessageStart { .. },
            ModelStreamEvent::TextDelta { delta, .. },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::DecodeTiming { duration_ms: 22 },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ] if delta == "{\"answer\":42}" && usage.input_tokens == Some(202)
    ));
    assert!(!model_events.iter().any(|event| {
        matches!(
            event,
            ModelStreamEvent::Usage { usage } if usage.input_tokens == Some(101)
        ) || matches!(event, ModelStreamEvent::DecodeTiming { duration_ms: 11 })
    }));
}

/// 连续六个原生坏候选耗尽纠正预算时，实时出口不泄漏任何候选正文。
#[tokio::test]
async fn structured_native_live_sink_hides_all_candidates_after_budget_exhaustion() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        (0..6)
            .map(|index| structured_text_reply("坏推理", &format!("bad-{index}")))
            .collect::<Vec<_>>(),
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StructuredOutput { .. }))
    ));
    assert_eq!(event_sink.text(), "");
    assert_eq!(event_sink.reasoning(), "");
    assert_eq!(result.messages.len(), 1);
}

/// 工具模拟的坏文本候选不能进入实时出口，合法保留结果由权威提交恢复。
#[tokio::test]
async fn structured_emulated_live_sink_hides_invalid_text_candidate() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            structured_text_reply("坏推理", "bad-result"),
            tool_reply(&[(
                "valid-result",
                STRUCTURED_RESULT_TOOL,
                json!({"value": {"answer": 42}}),
            )]),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
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
        .with_event_sink(event_sink.clone())
        .run_turn(request)
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(event_sink.text(), "");
    assert_eq!(event_sink.reasoning(), "");
    assert_eq!(result.messages.len(), 2);
    assert!(result.messages[1].content.iter().any(|block| {
        matches!(block, ContentBlock::Text { text } if text == "{\"answer\":42}")
    }));
}

/// 工具模拟中无效保留结果调用的完整生命周期不能泄漏到实时出口。
#[tokio::test]
async fn structured_emulated_live_sink_hides_invalid_result_tool_call() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            structured_result_reply("坏推理", "bad-result", json!({"answer": 0}), 101, Some(11)),
            structured_result_reply(
                "好推理",
                "good-result",
                json!({"answer": 42}),
                202,
                Some(22),
            ),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(event_sink.reasoning(), "好推理");
    let events = event_sink.events();
    assert!(events.iter().all(|event| event.model_round() == 1));
    let model_events = events
        .into_iter()
        .filter_map(|event| match event.into_kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(event),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. }
            | AgentStreamEventKind::ContextCompactionTruncated { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        model_events.as_slice(),
        [
            ModelStreamEvent::MessageStart { .. },
            ModelStreamEvent::ReasoningDelta { delta: reasoning, .. },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: start_id,
                name,
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: arguments_id,
                delta: arguments,
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: end_id,
            },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::DecodeTiming { duration_ms: 22 },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ] if reasoning == "好推理"
            && start_id == "good-result"
            && name == STRUCTURED_RESULT_TOOL
            && arguments_id == "good-result"
            && arguments == &json!({"value": {"answer": 42}}).to_string()
            && end_id == "good-result"
            && usage.input_tokens == Some(202)
    ));
    assert!(!model_events.iter().any(|event| {
        matches!(event, ModelStreamEvent::ToolCallStart { id, .. } if id == "bad-result")
            || matches!(event, ModelStreamEvent::ToolCallArgumentsDelta { id, .. } if id == "bad-result")
            || matches!(event, ModelStreamEvent::ToolCallEnd { id, .. } if id == "bad-result")
            || matches!(event, ModelStreamEvent::Usage { usage } if usage.input_tokens == Some(101))
            || matches!(event, ModelStreamEvent::DecodeTiming { duration_ms: 11 })
    }));
}

/// 纠正请求遭遇 Provider 错误时保留错误边界，但不泄漏之前的坏候选。
#[tokio::test]
async fn structured_live_sink_keeps_provider_failure_without_bad_candidate() {
    let provider_error = ModelError::ProviderUnavailable {
        message: "offline".to_owned(),
        status_code: None,
        retryable: true,
    };
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            structured_text_reply("坏推理", "bad-result"),
            ScriptedReply::new(vec![Err(provider_error.clone())]),
            structured_text_reply("不可达", "{\"answer\":42}"),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(native_structured_turn_request())
        .await;

    assert_eq!(result.error, Some(AgentRunError::Model(provider_error)));
    assert_eq!(event_sink.text(), "");
    assert_eq!(event_sink.reasoning(), "");
    assert!(event_sink.events().iter().any(|event| {
        matches!(
            event.kind(),
            AgentStreamEventKind::ModelFailure {
                error: ModelError::ProviderUnavailable { .. }
            }
        )
    }));
    assert_eq!(result.messages.len(), 1);
}

/// 纠正请求被 Provider 取消时只保留取消边界，实时出口与冷恢复均不包含坏候选。
#[tokio::test]
async fn structured_live_sink_keeps_cancellation_without_bad_candidate() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            structured_text_reply("坏推理", "bad-result"),
            ScriptedReply::new(vec![Err(ModelError::Cancelled {
                message: "provider cancelled correction".to_owned(),
            })]),
        ],
    ));
    let event_sink = Arc::new(RecordingModelEventSink::default());
    let request = native_structured_turn_request();
    let result = runner(provider, ToolRegistry::new())
        .with_event_sink(event_sink.clone())
        .run_turn(request)
        .await;

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(event_sink.text(), "");
    assert_eq!(event_sink.reasoning(), "");
    assert!(event_sink.events().iter().any(|event| {
        matches!(
            event.kind(),
            AgentStreamEventKind::ModelFailure {
                error: ModelError::Cancelled { .. }
            }
        )
    }));
    assert_eq!(result.messages.len(), 1);
}

/// 原生结构化输出的截断先经终止检查并按输出上限有界续跑，不提前做 Schema 校验：
/// 纯文本截断注入续跑指令后，由下一轮完整响应交付结构化输出并通过 Schema 校验。
#[tokio::test]
async fn runner_recovers_truncated_native_structured_output_before_schema_validation() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            text_reply_with_stop("{\"answer\":", StopReason::MaxOutputTokens),
            text_reply("{\"answer\":42}"),
        ],
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

    // 截断响应未被 Schema 校验失败终结，而是走输出上限有界恢复。
    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(result.messages.len(), 4);
    assert!(result.messages[2].is_meta);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].structured_output.is_some());
    assert!(requests[1].structured_output.is_some());
}

/// 原生 Provider 忽略 Schema 时必须以原生约束失败分类结束且不提交坏响应。
#[tokio::test]
async fn runner_classifies_native_schema_violation_without_committing_output() {
    let mut replies = (0..5)
        .map(|_| text_reply("{\"answer\":0}"))
        .collect::<Vec<_>>();
    replies.push(text_reply("{\"answer\":\"last-invalid\"}"));
    replies.push(text_reply("{\"answer\":42}"));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        replies,
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

    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(matches!(
        result.error.as_ref(),
        Some(AgentRunError::Model(ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::SchemaViolation,
            message,
        })) if message.contains("string")
    ));
    assert_eq!(result.messages.len(), 1);
    assert!(result.structured_output.is_none());
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 6);
    assert_eq!(provider.remaining_replies(), Ok(1));
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
        StopReason::Other {
            reason: "provider_specific".to_owned(),
        },
    ] {
        let is_completed = stop_reason == StopReason::Completed;
        let invalid_reply = || {
            tool_reply_with_stop(
                &[("call-result", STRUCTURED_RESULT_TOOL, json!({"value": 42}))],
                stop_reason.clone(),
            )
        };
        let mut replies = if is_completed {
            (0..6).map(|_| invalid_reply()).collect::<Vec<_>>()
        } else {
            vec![invalid_reply()]
        };
        // 非完整终态必须留下该响应；Completed 协议错误也只能消费五次纠正。
        replies.push(tool_reply(&[(
            "unused-valid",
            STRUCTURED_RESULT_TOOL,
            json!({"value": 42}),
        )]));
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                tool_calling: true,
                ..ProviderCapabilities::default()
            },
            replies,
        ));
        let mut request = turn_request(PlanGuard::inactive());
        request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
            "answer",
            json!({"type": "integer"}),
        ));

        let result = runner(provider.clone(), ToolRegistry::new())
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
            StopReason::Other { .. } => assert!(matches!(
                result.error,
                Some(AgentRunError::Model(ModelError::StructuredOutput {
                    enforcement: StructuredOutputEnforcement::ToolEmulated,
                    failure: StructuredOutputFailureKind::Incomplete,
                    ..
                }))
            )),
            StopReason::ToolUse => unreachable!(),
        }
        assert_eq!(result.messages.len(), 1);
        assert!(result.structured_output.is_none());
        let expected_requests = if is_completed { 6 } else { 1 };
        assert_eq!(
            provider.requests().expect("请求快照应可读取").len(),
            expected_requests
        );
        assert_eq!(provider.remaining_replies(), Ok(1));
    }
}

/// ToolUse 终态缺少任何调用块属于普通响应不变量错误，不能冒充结构化纠正候选。
#[tokio::test]
async fn structured_tool_use_without_calls_remains_invalid_response() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            empty_reply_with_stop(StopReason::ToolUse),
            tool_reply(&[("unused-valid", STRUCTURED_RESULT_TOOL, json!({"value": 42}))]),
        ],
    ));
    let mut request = turn_request(PlanGuard::inactive());
    request.model_request_mut().structured_output = Some(StructuredOutputConfig::new(
        "answer",
        json!({"type": "integer"}),
    ));

    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(request)
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::InvalidResponse {
            message: "模型以工具调用结束但没有返回工具调用内容块".to_owned(),
        })
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 1);
    assert_eq!(provider.remaining_replies(), Ok(1));
    assert_eq!(result.messages.len(), 1);
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

/// 未注册业务工具仍形成普通工具错误 Round，不能被结构化纠正逻辑吞掉。
#[tokio::test]
async fn runner_keeps_unknown_business_tool_in_regular_tool_loop() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("unknown-call", "unknown_tool", json!({}))]),
            tool_reply(&[(
                "result-call",
                STRUCTURED_RESULT_TOOL,
                json!({"value": {"answer": 42}}),
            )]),
        ],
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

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    let tool_results = turn_tool_results(&result);
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_call_id, "unknown-call");
    assert!(tool_results[0].is_error);
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
}

/// 保留结果工具与普通工具混合返回时不得执行调用，而应私有配对后纠正。
#[tokio::test]
async fn runner_corrects_mixed_emulated_result_and_regular_tool_calls() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[
                ("call-result", STRUCTURED_RESULT_TOOL, json!({"value": 1})),
                ("call-write", "record", json!({"value": "side-effect"})),
            ]),
            tool_reply(&[(
                "corrected-result",
                STRUCTURED_RESULT_TOOL,
                json!({"value": 42}),
            )]),
        ],
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

    let result = runner(provider.clone(), registry).run_turn(request).await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.structured_output, Some(json!(42)));
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.messages.len(), 2);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].tools.len(), 1);
    assert_eq!(requests[1].tools[0].name, STRUCTURED_RESULT_TOOL);
    assert_eq!(requests[1].messages.len(), 3);
    let correction_results = requests[1].messages[2]
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(correction_results.len(), 2);
    assert!(correction_results.iter().all(|result| result.is_error));
    assert_eq!(correction_results[0].tool_call_id, "call-result");
    assert_eq!(correction_results[1].tool_call_id, "call-write");
}

/// 第五次结构化纠正仍属于首个逻辑 Round，并按真实调用顺序分别提交用量。
#[tokio::test]
async fn runner_accepts_emulated_output_on_fifth_correction() {
    let mut replies = (0..5)
        .map(|index| {
            let id = format!("invalid-{index}");
            tool_reply(&[(&id, STRUCTURED_RESULT_TOOL, json!({"value": {"answer": 0}}))])
        })
        .collect::<Vec<_>>();
    replies.push(tool_reply(&[(
        "valid-5",
        STRUCTURED_RESULT_TOOL,
        json!({"value": {"answer": 42}}),
    )]));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        replies,
    ));
    let usage_sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
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
        .with_commit_sink(usage_sink.clone())
        .run_turn(request)
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    // 私有坏候选和纠正结果都不能进入权威 Transcript。
    assert_eq!(result.messages.len(), 2);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 6);
    for request in &requests[1..] {
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, STRUCTURED_RESULT_TOOL);
        assert_eq!(request.tool_choice, ToolChoice::Required);
        assert_eq!(request.messages.len(), 3);
    }
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 6);
    for (index, usage) in usages.iter().enumerate() {
        assert_eq!(usage.model_round(), 1);
        assert_eq!(usage.call_attempt(), u32::try_from(index + 1).unwrap());
    }
}

/// 结构化纠正预算属于单个 Turn；复用同一 Runner 开始下一 Turn 时必须重新获得五次。
#[tokio::test]
async fn structured_correction_budget_resets_for_each_turn() {
    let mut replies = Vec::new();
    for turn in 0..2 {
        for correction in 0..5 {
            replies.push(text_reply(&format!("invalid-{turn}-{correction}")));
        }
        replies.push(text_reply(&format!("{{\"answer\":{}}}", turn + 1)));
    }
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        replies,
    ));
    let runner = runner(provider.clone(), ToolRegistry::new());

    for expected in [1, 2] {
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
        let result = runner.run_turn(request).await;
        assert!(result.is_success(), "{:?}", result.error);
        assert_eq!(result.structured_output, Some(json!({"answer": expected})));
    }

    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 12);
    assert_eq!(provider.remaining_replies(), Ok(0));
}

/// 私有纠正调用中的取消、Provider 错误与上下文溢出必须立即终止当前 Turn。
#[tokio::test]
async fn runner_propagates_structured_correction_call_errors_immediately() {
    for expected in [
        ModelError::Cancelled {
            message: "cancelled".to_owned(),
        },
        ModelError::ProviderUnavailable {
            message: "offline".to_owned(),
            status_code: None,
            retryable: true,
        },
        ModelError::ContextLengthExceeded {
            message: "correction too large".to_owned(),
        },
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                structured_output: StructuredOutputCapability::Native,
                ..ProviderCapabilities::default()
            },
            [
                text_reply("not-json"),
                ScriptedReply::new(vec![Err(expected.clone())]),
                text_reply("{\"answer\":42}"),
            ],
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

        match &expected {
            ModelError::Cancelled { .. } => {
                assert_eq!(result.error, Some(AgentRunError::Cancelled));
                assert_eq!(
                    result.state.terminal_reason(),
                    Some(TerminalReason::Cancelled)
                );
            }
            _ => {
                assert_eq!(result.error, Some(AgentRunError::Model(expected.clone())));
                assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
            }
        }
        assert_eq!(result.messages.len(), 1);
        assert!(result.structured_output.is_none());
        assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
        assert_eq!(provider.remaining_replies(), Ok(1));
    }
}

/// 原生纠正请求已明确禁用工具；Provider 违规返回的调用不得进入业务执行器。
#[tokio::test]
async fn native_correction_never_executes_provider_tool_calls() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
        [
            text_reply("not-json"),
            tool_reply(&[("forbidden-call", "record", json!({"value": "write"}))]),
            text_reply("{\"answer\":42}"),
        ],
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
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider.clone(), registry).run_turn(request).await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::Incomplete,
            ..
        }))
    ));
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.messages.len(), 1);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    assert_eq!(provider.remaining_replies(), Ok(1));
}

/// 工具模拟纠正只接受保留结果工具；违规业务调用必须继续私有纠正而非执行。
#[tokio::test]
async fn emulated_correction_never_executes_provider_business_tools() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            structured_output: StructuredOutputCapability::ToolEmulated,
            ..ProviderCapabilities::default()
        },
        [
            text_reply("bad-result"),
            tool_reply(&[("forbidden-call", "record", json!({"value": "write"}))]),
            tool_reply(&[(
                "corrected-result",
                STRUCTURED_RESULT_TOOL,
                json!({"value": {"answer": 42}}),
            )]),
        ],
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
        json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    ));

    let result = runner(provider.clone(), registry).run_turn(request).await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.structured_output, Some(json!({"answer": 42})));
    assert_eq!(tool.call_count(), 0);
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(result.state.round_count(), 1);
    assert_eq!(result.messages.len(), 2);
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 3);
    for correction in &requests[1..] {
        assert_eq!(correction.tools.len(), 1);
        assert_eq!(correction.tools[0].name, STRUCTURED_RESULT_TOOL);
        assert_eq!(correction.tool_choice, ToolChoice::Required);
    }
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

/// ScriptedProvider 返回带缓存字段的用量时，命中率按“缓存读取 / 含缓存总输入”
/// 随用量事实一起提交，且提交事件携带的用量 JSON 含 camelCase 缓存字段
/// （Session Journal 的 ModelRoundCompleted 持久化同一 TokenUsage 结构）。
#[tokio::test]
async fn model_round_usage_carries_cache_hit_rate_from_reported_usage() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "缓存观测响应".to_owned(),
            },
            ModelStreamEvent::Usage {
                usage: TokenUsage {
                    input_tokens: Some(1_000),
                    output_tokens: Some(20),
                    cache_read_tokens: Some(800),
                    cache_write_tokens: Some(200),
                    ..TokenUsage::unknown()
                },
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    ));
    let sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));

    let result = runner(provider, ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let usages = sink.usages();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].completion().usage.cache_read_tokens, Some(800));
    assert_eq!(usages[0].completion().usage.cache_write_tokens, Some(200));
    assert_eq!(usages[0].cache_hit_rate(), Some(0.8));
    // 权威事件与 Journal 持久化的是同一 TokenUsage：缓存字段以 camelCase 落盘。
    let persisted = serde_json::to_value(&usages[0].completion().usage).unwrap();
    assert_eq!(persisted["inputTokens"], json!(1_000));
    assert_eq!(persisted["cacheReadTokens"], json!(800));
    assert_eq!(persisted["cacheWriteTokens"], json!(200));
}

/// 无缓存字段（未报告缓存读取与写入）的普通响应：命中率保持 `None`，不臆造为零。
#[tokio::test]
async fn model_round_usage_without_cache_fields_has_no_hit_rate() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("纯文本响应")],
    ));
    let sink = Arc::new(ModelRoundUsageProbeSink::new(
        Arc::new(RecordingTool::new(
            "unused",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )),
        0,
    ));

    let result = runner(provider, ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let usages = sink.usages();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].completion().usage.cache_read_tokens, None);
    assert_eq!(usages[0].completion().usage.cache_write_tokens, None);
    assert_eq!(usages[0].cache_hit_rate(), None);
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

mod goal_loop_tests {
    use super::*;

    fn draft() -> GoalDraft {
        GoalDraft {
            title: "交付".to_owned(),
            objective: "实现并验证修复".to_owned(),
            description: None,
            token_budget: None,
            progress_percent: None,
        }
    }

    fn state(owner: &str, create: bool) -> Arc<InMemoryRuntimeState> {
        let state = Arc::new(InMemoryRuntimeState::new(session_id(owner)));
        if create {
            state.create_goal("initial-goal", draft()).unwrap();
        }
        state
    }

    /// 用测试状态工具驱动生命周期，验证 Runner 不凭候选回复文本猜测完成。
    struct GoalTestTool(Arc<InMemoryRuntimeState>, Option<TurnCancellation>);
    impl AgentTool for GoalTestTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "goal_test",
                "Change test goal",
                json!({
                    "type": "object", "properties": {"action": {"type": "string"}}, "required": ["action"],
                }),
            )
        }
        fn effect(&self, _: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::Exclusive
        }
        fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
            Box::pin(async move {
                let op = context.tool_call_id.as_str();
                match input["action"].as_str().unwrap() {
                    "create" => {
                        self.0.create_goal(op, draft()).unwrap();
                    }
                    "budget" => {
                        self.0
                            .update_goal(
                                op,
                                GoalPatch {
                                    token_budget: Some(Some(10)),
                                    ..GoalPatch::default()
                                },
                            )
                            .unwrap();
                        self.0
                            .record_goal_usage(
                                "record-budget",
                                GoalUsageDelta {
                                    tokens: 10,
                                    elapsed_seconds: 1,
                                },
                            )
                            .unwrap();
                    }
                    "edit" => {
                        self.0
                            .update_goal(
                                op,
                                GoalPatch {
                                    title: Some("新交付目标".to_owned()),
                                    objective: Some("按照编辑后的目标实现并验证".to_owned()),
                                    description: Some(Some("放弃与新目标冲突的旧计划".to_owned())),
                                    ..GoalPatch::default()
                                },
                            )
                            .unwrap();
                    }
                    "cancel" => self.1.as_ref().unwrap().cancel(),
                    action => {
                        let blocked = action == "block";
                        self.0
                            .transition_goal(
                                op,
                                GoalTransition {
                                    status: if blocked {
                                        GoalStatus::Blocked
                                    } else {
                                        GoalStatus::Completed
                                    },
                                    blocked_reason: blocked.then(|| "需要用户提供凭据".to_owned()),
                                    completion_evidence: (!blocked)
                                        .then(|| "修复和回归均验证通过".to_owned()),
                                },
                            )
                            .unwrap();
                        if action == "replace" {
                            self.0.clear_goal("clear-old-goal").unwrap();
                            self.0.create_goal("replacement-goal", draft()).unwrap();
                        }
                    }
                }
                Ok(ToolOutput::text("goal state updated"))
            })
        }
    }

    struct PassingHook;
    impl AgentHook for PassingHook {
        fn name(&self) -> &str {
            "passing"
        }
    }

    fn goal_runner(
        provider: Arc<ScriptedProvider>,
        state: Arc<InMemoryRuntimeState>,
        limits: RunLimits,
    ) -> AgentRunner {
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(GoalTestTool(state.clone(), None)))
            .unwrap();
        AgentRunner::new(provider, tools, limits).with_goal_controller(state)
    }

    #[tokio::test]
    async fn active_goal_continues_past_stop_hook_budget_until_completed() {
        let state = state("session-runner", true);
        let mut replies = (0..12)
            .map(|_| text_reply("only a proposal, not done"))
            .collect::<Vec<_>>();
        replies.push(tool_reply(&[(
            "complete-goal",
            "goal_test",
            json!({"action": "complete"}),
        )]));
        replies.push(text_reply("verified and completed"));
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            replies,
        ));
        let mut hooks = HookRegistry::new();
        hooks.register(Arc::new(PassingHook)).unwrap();
        let result = goal_runner(provider.clone(), state.clone(), RunLimits::default())
            .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).unwrap())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;
        assert!(result.is_success(), "{:?}", result.error);
        assert_eq!(provider.requests().unwrap().len(), 14);
        assert_eq!(
            state.goal_snapshot().unwrap().goal.unwrap().status,
            GoalStatus::Completed
        );
        let requests = provider.requests().unwrap();
        let audit = requests[0]
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Developer)
            .flat_map(|message| message.content.iter())
            .find_map(|block| match block {
                ContentBlock::Text { text } if text.contains("audit the actual current state") => {
                    Some(text)
                }
                _ => None,
            })
            .expect("活跃目标请求必须注入逐项证据审计");
        assert!(audit.contains("every explicit requirement"));
        assert!(audit.contains("actually covers the requirements"));
        assert!(audit.contains("Goal complete with evidence for every requirement"));
        assert!(!audit.contains("runtime will run a completion verifier"));
        assert!(
            requests[0]
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Developer)
        );
    }

    #[tokio::test]
    async fn externally_scheduled_goal_finishes_its_current_turn() {
        let state = state("session-runner", true);
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [text_reply("first iteration complete")],
        ));
        let result = goal_runner(provider.clone(), state.clone(), RunLimits::default())
            .with_external_goal_continuation()
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;
        assert!(result.is_success(), "{:?}", result.error);
        assert_eq!(provider.requests().unwrap().len(), 1);
        assert_eq!(
            state.goal_snapshot().unwrap().goal.unwrap().status,
            GoalStatus::Active
        );
    }

    #[tokio::test]
    async fn goal_created_during_turn_also_continues() {
        let state = state("session-runner", false);
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("create-goal", "goal_test", json!({"action": "create"}))]),
                text_reply("unfinished"),
                tool_reply(&[("block-goal", "goal_test", json!({"action": "block"}))]),
                text_reply("needs credentials"),
            ],
        ));
        let result = goal_runner(provider.clone(), state.clone(), RunLimits::default())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;
        assert!(result.is_success(), "{:?}", result.error);
        assert_eq!(provider.requests().unwrap().len(), 4);
        assert_eq!(
            state.goal_snapshot().unwrap().goal.unwrap().status,
            GoalStatus::Blocked
        );
    }

    #[tokio::test]
    async fn edited_goal_injects_previous_and_current_content_at_continuation_boundary() {
        let state = state("session-runner", true);
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("edit-goal", "goal_test", json!({"action": "edit"}))]),
                text_reply("继续旧计划"),
                tool_reply(&[(
                    "complete-edited-goal",
                    "goal_test",
                    json!({"action": "complete"}),
                )]),
                text_reply("新目标已经完成"),
            ],
        ));

        let result = goal_runner(provider.clone(), state, RunLimits::default())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;

        assert!(result.is_success(), "{:?}", result.error);
        let requests = provider.requests().unwrap();
        assert_eq!(requests.len(), 4);
        let update = requests[2]
            .messages
            .iter()
            .rev()
            .find(|message| {
                message.role == MessageRole::Developer
                    && message.content.iter().any(|block| {
                        matches!(
                            block,
                            ContentBlock::Text { text }
                                if text.contains("The user updated the active goal")
                        )
                    })
            })
            .expect("目标续跑边界应注入编辑通知");
        let text = update
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(text.contains("实现并验证修复"));
        assert!(text.contains("按照编辑后的目标实现并验证"));
        assert!(text.contains("Previous goal data"));
        assert!(text.contains("Current goal data"));
    }

    #[tokio::test]
    async fn other_sessions_plan_and_child_runners_are_not_forced_to_continue() {
        for (owner, guard, attach) in [
            ("other-session", PlanGuard::inactive(), true),
            ("session-runner", PlanGuard::read_only(), true),
            ("session-runner", PlanGuard::inactive(), false),
        ] {
            let state = state(owner, true);
            let provider = Arc::new(ScriptedProvider::new(
                ProviderCapabilities::default(),
                [text_reply("answer or plan")],
            ));
            let mut runner =
                AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default());
            if attach {
                runner = runner.with_goal_controller(state);
            }
            let result = runner.run_turn(turn_request(guard)).await;
            assert!(result.is_success(), "{:?}", result.error);
            assert_eq!(provider.requests().unwrap().len(), 1);
            assert!(
                !result
                    .messages
                    .iter()
                    .any(|m| m.role == MessageRole::Developer)
            );
        }
    }

    #[tokio::test]
    async fn replaced_goal_is_not_adopted_by_old_turn() {
        let state = state("session-runner", true);
        let old_id = state.goal_snapshot().unwrap().goal.unwrap().id;
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("replace-goal", "goal_test", json!({"action": "replace"}))]),
                text_reply("old task finished"),
            ],
        ));
        let result = goal_runner(provider.clone(), state.clone(), RunLimits::default())
            .run_turn(turn_request(PlanGuard::inactive()))
            .await;
        assert!(result.is_success(), "{:?}", result.error);
        assert_eq!(provider.requests().unwrap().len(), 2);
        assert_ne!(state.goal_snapshot().unwrap().goal.unwrap().id, old_id);
    }

    #[tokio::test]
    async fn explicit_limits_override_active_goal_with_one_summary() {
        for budget in [false, true] {
            let state = state("session-runner", true);
            let provider = Arc::new(ScriptedProvider::new(
                ProviderCapabilities::default(),
                [
                    if budget {
                        tool_reply(&[("use-budget", "goal_test", json!({"action": "budget"}))])
                    } else {
                        text_reply("unfinished")
                    },
                    text_reply("budget exhausted, work remains"),
                ],
            ));
            let limits = RunLimits {
                max_rounds: (!budget).then_some(1),
                ..RunLimits::default()
            };
            let result = goal_runner(provider.clone(), state.clone(), limits)
                .run_turn(turn_request(PlanGuard::inactive()))
                .await;
            assert_eq!(
                result.state.terminal_reason(),
                Some(TerminalReason::LimitReached)
            );
            assert_eq!(
                result.error,
                Some(if budget {
                    AgentRunError::GoalBudgetReached { maximum: 10 }
                } else {
                    AgentRunError::LimitReached {
                        counter: CounterKind::Round,
                        maximum: 1,
                    }
                })
            );
            let requests = provider.requests().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].tool_choice, ToolChoice::None);
            assert!(requests[1].tools.is_empty());
            assert_eq!(
                state.goal_snapshot().unwrap().goal.unwrap().status,
                GoalStatus::Active
            );
        }
    }

    #[tokio::test]
    async fn cancellation_does_not_restart_active_goal() {
        let state = state("session-runner", true);
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "cancel",
                "goal_test",
                json!({"action": "cancel"}),
            )])],
        ));
        let cancellation = TurnCancellation::new();
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(GoalTestTool(
                state.clone(),
                Some(cancellation.clone()),
            )))
            .unwrap();
        let mut request = turn_request(PlanGuard::inactive());
        request.set_cancellation(cancellation);
        let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
            .with_goal_controller(state.clone())
            .run_turn(request)
            .await;
        assert_eq!(
            result.state.terminal_reason(),
            Some(TerminalReason::Cancelled),
            "{:?}",
            result.error
        );
        assert_eq!(provider.requests().unwrap().len(), 1);
        assert_eq!(
            state.goal_snapshot().unwrap().goal.unwrap().status,
            GoalStatus::Active
        );
    }
}

/// 默认任务可跨过原有 64 Round / 256 Step 上限，相同成功读取不触发熔断。
#[tokio::test]
async fn default_limits_allow_long_running_tasks() {
    let mut replies = Vec::new();
    for round in 0..65 {
        let ids = (0..4)
            .map(|step| format!("call-{round}-{step}"))
            .collect::<Vec<_>>();
        let calls = ids
            .iter()
            .map(|id| (id.as_str(), "record", json!({"value": "poll"})))
            .collect::<Vec<_>>();
        replies.push(tool_reply(&calls));
    }
    replies.push(text_reply("done"));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        replies,
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).unwrap();
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 66);
    assert_eq!(result.state.step_count(), 260);
    assert_eq!(tool.call_count(), 260);
}

/// 只有显式总量耗尽才注入一次总结；正常请求中没有轮数倒计时。
#[tokio::test]
async fn explicit_limits_add_one_tool_free_summary_at_exhaustion() {
    for (counter, limits) in [
        (
            CounterKind::Round,
            RunLimits {
                max_rounds: Some(1),
                ..RunLimits::default()
            },
        ),
        (
            CounterKind::Step,
            RunLimits {
                max_steps: Some(1),
                ..RunLimits::default()
            },
        ),
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
                text_reply("completed work; remaining tasks"),
            ],
        ));
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(RecordingTool::new(
                "record",
                ToolEffect::ChangesState,
                ToolConcurrency::Exclusive,
            )))
            .unwrap();
        let request = turn_request(PlanGuard::inactive());
        let original_messages = request.model_request().messages.clone();
        let result = AgentRunner::new(provider.clone(), registry, limits)
            .run_turn(request)
            .await;
        assert_eq!(
            result.error,
            Some(AgentRunError::LimitReached {
                counter,
                maximum: 1
            })
        );
        assert_eq!(
            result.state.terminal_reason(),
            Some(TerminalReason::LimitReached)
        );
        let requests = provider.requests().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages, original_messages);
        assert!(requests[1].tools.is_empty());
        assert_eq!(requests[1].tool_choice, ToolChoice::None);
        assert!(requests[1].structured_output.is_none());
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Developer)
                .count(),
            1
        );
    }
}

/// 恰好在最后一个允许的正常 Round 自然完成，不追加多余总结。
#[tokio::test]
async fn final_allowed_round_can_complete_normally() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("done")],
    ));
    let limits = RunLimits {
        max_rounds: Some(1),
        ..RunLimits::default()
    };
    let result = AgentRunner::new(provider.clone(), ToolRegistry::new(), limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(provider.requests().unwrap().len(), 1);
}

/// Round 总结失败或违规请求工具都不能再次执行工具，且保留耗尽终态。
#[tokio::test]
async fn round_limit_summary_cannot_resume_tools_or_replace_limit_reason() {
    for summary in [
        ScriptedReply::new(vec![Err(ModelError::Protocol {
            message: "summary failed".to_owned(),
        })]),
        tool_reply(&[("call-summary", "record", json!({"value": "must not run"}))]),
    ] {
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
                summary,
            ],
        ));
        let tool = Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        ));
        let mut registry = ToolRegistry::new();
        registry.register(tool.clone()).unwrap();
        let result = AgentRunner::new(
            provider.clone(),
            registry,
            RunLimits {
                max_rounds: Some(1),
                ..RunLimits::default()
            },
        )
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
        assert_eq!(
            result.error,
            Some(AgentRunError::LimitReached {
                counter: CounterKind::Round,
                maximum: 1
            })
        );
        assert_eq!(tool.call_count(), 1);
        assert_eq!(provider.requests().unwrap().len(), 2);
    }
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

/// 未覆盖 timeout() 的工具默认获得有界外层墙钟上限。
#[test]
fn agent_tool_default_wall_clock_timeout_is_bounded() {
    let tool = RecordingTool::new(
        "bare_default",
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    );
    assert_eq!(AgentTool::timeout(&tool), Some(Duration::from_secs(120)));
    assert_eq!(DEFAULT_TOOL_TIMEOUT, Duration::from_secs(120));
}

/// 挂起工具必须在外层墙钟超时后被切断，产出超时错误结果且 Turn 继续收敛。
#[tokio::test]
async fn outer_wall_clock_cuts_off_hung_tool_and_turn_continues() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-hung", "hung_timeout", json!({}))]),
            text_reply("done"),
        ],
    ));
    let observed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(HungTimeoutTool {
            timeout: Some(Duration::from_millis(80)),
            concurrency: ToolConcurrency::Exclusive,
            started: Arc::new(Notify::new()),
            observed_cancellation: observed.clone(),
        }))
        .expect("挂起测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 1);
    // 超时必须取消本调用的执行令牌，让工具在清理宽限内观察到取消。
    assert!(observed.load(Ordering::SeqCst));
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    let text = turn_tool_result_text(results[0]);
    assert!(
        text.starts_with("tool_timeout：")
            && text.contains("hung_timeout")
            && text.contains("后超时"),
        "超时结果文本不符合契约：{text}"
    );
    // 超时后循环继续：第二轮模型请求收敛为正常文本终态。
    assert_eq!(result.state.round_count(), 2);
    let final_text = &result.messages[result.messages.len() - 1];
    assert!(
        final_text.content.iter().any(|block| matches!(
            block,
            ContentBlock::Text { text } if text == "done"
        )),
        "超时后 Turn 必须以最终文本收敛：{:?}",
        final_text.content
    );
}

/// 同一挂起工具反复外层墙钟超时必须计入重复失败指纹并触发既有熔断。
#[tokio::test]
async fn repeated_tool_timeouts_count_toward_failure_loop_terminal() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("timeouts-1", "hung_timeout", json!({}))]),
            tool_reply(&[("timeouts-2", "hung_timeout", json!({}))]),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(HungTimeoutTool {
            timeout: Some(Duration::from_millis(60)),
            concurrency: ToolConcurrency::Exclusive,
            started: Arc::new(Notify::new()),
            observed_cancellation: Arc::new(AtomicBool::new(false)),
        }))
        .expect("挂起测试工具应可注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(2)
        .expect("重复失败上限应有效");

    let result = AgentRunner::new(provider, registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolLoop {
            kind: ToolLoopKind::RepeatedFailure,
            tool_name: "hung_timeout".to_owned(),
            maximum: 2,
        })
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|item| item.is_error));
}

/// Turn 取消与外层墙钟超时竞争时必须保留取消文案、取消终态与取消错误。
#[tokio::test]
async fn turn_cancellation_takes_priority_over_tool_timeout() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[("call-hung", "hung_timeout", json!({}))])],
    ));
    let started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(HungTimeoutTool {
            timeout: Some(Duration::from_millis(500)),
            concurrency: ToolConcurrency::Exclusive,
            started: started.clone(),
            observed_cancellation: Arc::new(AtomicBool::new(false)),
        }))
        .expect("挂起测试工具应可注册");
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
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    let text = turn_tool_result_text(results[0]);
    assert!(text.contains("Turn 取消而中止"), "应保留取消文案：{text}");
    assert!(!text.contains("超时"), "不得回退为超时文案：{text}");
}

/// 自管超时（None）工具不施加外层墙钟，短暂休眠后完整返回成功结果。
#[tokio::test]
async fn self_managed_timeout_tool_finishes_without_outer_wall_clock() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-self", "self_managed", json!({}))]),
            text_reply("done"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(SelfManagedTool {
            started: Arc::new(Notify::new()),
        }))
        .expect("自管超时测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error);
    assert_eq!(turn_tool_result_text(results[0]), "self-managed-ok");
}

/// 并行段失败收窄所需的固定失败测试工具：等待兄弟启动信号后返回真实 ToolError。
struct FailingSiblingTool {
    /// 提供给模型的精确工具名称。
    name: String,
    /// 本调用执行前等待的兄弟启动信号；`None` 表示不等直接失败。
    wait_for: Option<Arc<Notify>>,
    /// 本调用被执行时发出的启动信号。
    started: Arc<Notify>,
    /// 返回给模型的固定错误码。
    error_code: String,
}

impl AgentTool for FailingSiblingTool {
    /// 返回要求可选字符串 `value` 的合成 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name.clone(),
            "验证并行失败不再取消兄弟",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "additionalProperties": false
            }),
        )
    }

    /// 把全部调用标记为只读。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 允许与相邻只读调用并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 等待兄弟进入执行阶段后返回固定真实错误，避免失败先于兄弟执行。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let wait_for = self.wait_for.clone();
        let started = self.started.clone();
        let code = self.error_code.clone();
        Box::pin(async move {
            started.notify_one();
            if let Some(signal) = wait_for {
                signal.notified().await;
            }
            Err(ToolError::permanent(code, "并行失败收窄测试固定错误"))
        })
    }
}

/// 并行只读段中的单个超时只切断自身并按失败继续，不波及兄弟只读调用。
#[tokio::test]
async fn parallel_segment_timeout_does_not_cancel_sibling_read_only_calls() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-hung", "hung_timeout", json!({})),
                ("call-ok", "recorder", json!({"value": "x"})),
            ]),
            text_reply("done"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(HungTimeoutTool {
            timeout: Some(Duration::from_millis(80)),
            concurrency: ToolConcurrency::ParallelReadOnly,
            started: Arc::new(Notify::new()),
            observed_cancellation: Arc::new(AtomicBool::new(false)),
        }))
        .expect("挂起测试工具应可注册");
    registry
        .register(Arc::new(RecordingTool::new(
            "recorder",
            ToolEffect::ReadOnly,
            ToolConcurrency::ParallelReadOnly,
        )))
        .expect("记录测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        if result.tool_call_id == "call-hung" {
            assert!(result.is_error);
            assert!(turn_tool_result_text(result).contains("后超时"));
        } else {
            assert_eq!(result.tool_call_id, "call-ok");
            assert!(!result.is_error);
            assert_eq!(turn_tool_result_text(result), "synthetic-result");
        }
    }
}

/// 并行只读段中一个工具真实失败时兄弟必须正常完成：失败结果与成功结果各归其位。
#[tokio::test]
async fn parallel_segment_failure_does_not_cancel_sibling_read_only_call() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-fail", "parallel_fail", json!({})),
                ("call-slow", "parallel_slow", json!({"value": "x"})),
            ]),
            text_reply("done"),
        ],
    ));
    struct SlowSiblingTool;
    impl AgentTool for SlowSiblingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "parallel_slow".to_owned(),
                "验证慢兄弟在失败判定后仍正常完成",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "additionalProperties": false
                }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            // 慢兄弟执行时长远超失败兄弟的即时失败：单次普通失败在新旧
            // 语义下都不取消兄弟（旧代码只在熔断终态/终态错误时取消段），
            // 本用例锁定各归其位的基本语义；真正的行为增量由下面的熔断
            // tripwire 用例覆盖。
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(ToolOutput::text("sibling-ok"))
            })
        }
    }
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(FailingSiblingTool {
            name: "parallel_fail".to_owned(),
            wait_for: None,
            started: Arc::new(Notify::new()),
            error_code: "boom".to_owned(),
        }))
        .expect("失败测试工具应可注册");
    registry
        .register(Arc::new(SlowSiblingTool))
        .expect("慢兄弟测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        if result.tool_call_id == "call-fail" {
            assert!(result.is_error);
            let text = turn_tool_result_text(result);
            assert!(text.contains("boom"), "失败结果应回流真实错误码：{text}");
            assert!(!text.contains("中止"), "失败兄弟不得被段取消切断：{text}");
        } else {
            assert_eq!(result.tool_call_id, "call-slow");
            assert!(!result.is_error);
            assert_eq!(turn_tool_result_text(result), "sibling-ok");
        }
    }
}

/// 同段三工具一失败一超时一成功时三个结果各归其位，无一被"中止"波及。
#[tokio::test]
async fn parallel_segment_failure_timeout_and_success_each_keep_own_result() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-ok", "tri_ok", json!({"value": "x"})),
                ("call-fail", "tri_fail", json!({})),
                ("call-hung", "hung_timeout", json!({})),
            ]),
            text_reply("done"),
        ],
    ));
    let ok_started = Arc::new(Notify::new());
    struct TriOkTool {
        started: Arc<Notify>,
    }
    impl AgentTool for TriOkTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "tri_ok".to_owned(),
                "验证三兄弟各自结果",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "additionalProperties": false
                }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let started = self.started.clone();
            Box::pin(async move {
                started.notify_one();
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(ToolOutput::text("tri-ok"))
            })
        }
    }
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(TriOkTool {
            started: ok_started.clone(),
        }))
        .expect("三兄弟成功工具应可注册");
    registry
        .register(Arc::new(FailingSiblingTool {
            name: "tri_fail".to_owned(),
            wait_for: Some(ok_started.clone()),
            started: Arc::new(Notify::new()),
            error_code: "tri-boom".to_owned(),
        }))
        .expect("三兄弟失败工具应可注册");
    registry
        .register(Arc::new(HungTimeoutTool {
            timeout: Some(Duration::from_millis(60)),
            concurrency: ToolConcurrency::ParallelReadOnly,
            started: Arc::new(Notify::new()),
            observed_cancellation: Arc::new(AtomicBool::new(false)),
        }))
        .expect("三兄弟挂起工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 3);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 3);
    for result in results {
        match result.tool_call_id.as_str() {
            "call-fail" => {
                assert!(result.is_error);
                let text = turn_tool_result_text(result);
                assert!(text.contains("tri-boom"), "失败位应回流真实错误：{text}");
            }
            "call-hung" => {
                assert!(result.is_error);
                assert!(turn_tool_result_text(result).contains("后超时"));
            }
            "call-ok" => {
                assert!(!result.is_error);
                assert_eq!(turn_tool_result_text(result), "tri-ok");
            }
            other => panic!("三兄弟段不应出现多余结果：{other}"),
        }
    }
}

/// 用户取消 mid-segment 时整段取消：并行兄弟收到取消终态，Turn 进入 Cancelled。
#[tokio::test]
async fn parallel_segment_user_cancellation_still_cancels_whole_segment() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("call-a", "cancel_probe", json!({"value": "a"})),
            ("call-b", "cancel_probe", json!({"value": "b"})),
        ])],
    ));
    struct CancelProbeTool {
        started: Arc<Notify>,
    }
    impl AgentTool for CancelProbeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "cancel_probe".to_owned(),
                "验证用户取消仍取消整段",
                json!({ "type": "object", "additionalProperties": true }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let started = self.started.clone();
            Box::pin(async move {
                started.notify_one();
                // 挂起等待取消：取消恒走执行竞速 select 的左臂（Ok(raw) +
                // terminal_error=Cancelled），工具返回的 Err 在此路径不可达。
                context.cancellation.cancelled().await;
                std::future::pending().await
            })
        }
    }
    let started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(CancelProbeTool {
            started: started.clone(),
        }))
        .expect("取消探针工具应可注册");
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let cancel_task = tokio::spawn(async move {
        started.notified().await;
        // 等首个兄弟进入执行后再取消，确保取消发生在段执行中途。
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancellation.cancel();
    });

    let result = runner(provider, registry).run_turn(request).await;
    cancel_task.await.expect("取消任务不应异常");

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.is_error);
        assert!(
            turn_tool_result_text(result).contains("Turn 取消而中止"),
            "用户取消后兄弟应为取消固定结果：{}",
            turn_tool_result_text(result)
        );
    }
}

/// 只读工具输出超限固定结果（无落盘通道）按普通失败回流，不取消兄弟且不终止 Turn。
#[tokio::test]
async fn parallel_segment_read_only_output_limit_result_does_not_cancel_sibling() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-big", "oversized", json!({})),
                ("call-ok", "limit_sibling", json!({"value": "x"})),
            ]),
            text_reply("done"),
        ],
    ));
    struct OversizedTool;
    impl AgentTool for OversizedTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "oversized".to_owned(),
                "验证只读超限固定结果",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            Box::pin(async {
                Ok(ToolOutput::text("y".repeat(
                    crate::tool::TOOL_OUTPUT_LIMITS.max_text_bytes + 1,
                )))
            })
        }
    }
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(OversizedTool))
        .expect("超限测试工具应可注册");
    registry
        .register(Arc::new(RecordingTool::new(
            "limit_sibling",
            ToolEffect::ReadOnly,
            ToolConcurrency::ParallelReadOnly,
        )))
        .expect("超限兄弟测试工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        if result.tool_call_id == "call-big" {
            assert!(result.is_error);
            assert_eq!(turn_tool_result_text(result), TOOL_OUTPUT_LIMIT_RESULT);
        } else {
            assert_eq!(result.tool_call_id, "call-ok");
            assert!(!result.is_error);
            assert_eq!(turn_tool_result_text(result), "synthetic-result");
        }
    }
}

/// 并行兄弟失败各自计入自己的熔断指纹：不同工具交替失败不触发重复失败终态。
#[tokio::test]
async fn parallel_segment_sibling_failures_count_toward_own_fingerprints() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-a1", "fp_a", json!({"value": "a"})),
                ("call-b1", "fp_b", json!({"value": "b"})),
            ]),
            text_reply("done"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(FailingSiblingTool {
            name: "fp_a".to_owned(),
            wait_for: None,
            started: Arc::new(Notify::new()),
            error_code: "err-a".to_owned(),
        }))
        .expect("指纹工具 A 应可注册");
    registry
        .register(Arc::new(FailingSiblingTool {
            name: "fp_b".to_owned(),
            wait_for: None,
            started: Arc::new(Notify::new()),
            error_code: "err-b".to_owned(),
        }))
        .expect("指纹工具 B 应可注册");
    // 终态阈值为 2：若兄弟失败互相污染计数，两个不同指纹失败将误触发熔断。
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(2)
        .expect("重复失败上限应有效");

    let result = AgentRunner::new(provider, registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.is_error);
        let text = turn_tool_result_text(result);
        if result.tool_call_id == "call-a1" {
            assert!(text.contains("err-a"), "A 失败应计入自己的指纹：{text}");
        } else {
            assert!(text.contains("err-b"), "B 失败应计入自己的指纹：{text}");
        }
    }
}

/// #26：连续只读 + 声明并发安全的副作用工具同批并发启动。
///
/// 混合段用 Barrier/Notify 证明启动重叠：只读探针等待副作用探针的进入
/// 信号，副作用探针等待只读探针的进入信号；若任一顺序执行则测试超时。
#[tokio::test]
async fn parallel_safe_batch_runs_mixed_read_only_and_side_effect_concurrently() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-r", "mixed_read", json!({})),
                ("call-w", "mixed_safe_write", json!({})),
            ]),
            text_reply("done"),
        ],
    ));
    struct MixedReadTool {
        entered: Arc<Notify>,
        peer_entered: Arc<Notify>,
    }
    impl AgentTool for MixedReadTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "mixed_read".to_owned(),
                "验证混合批只读并发",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            let peer = self.peer_entered.clone();
            Box::pin(async move {
                entered.notify_one();
                peer.notified().await;
                Ok(ToolOutput::text("mixed-read-ok"))
            })
        }
    }
    struct MixedSafeWriteTool {
        entered: Arc<Notify>,
        peer_entered: Arc<Notify>,
    }
    impl AgentTool for MixedSafeWriteTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "mixed_safe_write".to_owned(),
                "验证混合批副作用并发",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelSafe
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            let peer = self.peer_entered.clone();
            Box::pin(async move {
                entered.notify_one();
                peer.notified().await;
                Ok(ToolOutput::text("mixed-write-ok"))
            })
        }
    }
    let read_entered = Arc::new(Notify::new());
    let write_entered = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(MixedReadTool {
            entered: read_entered.clone(),
            peer_entered: write_entered.clone(),
        }))
        .expect("混合批只读工具应可注册");
    registry
        .register(Arc::new(MixedSafeWriteTool {
            entered: write_entered.clone(),
            peer_entered: read_entered.clone(),
        }))
        .expect("混合批副作用工具应可注册");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        runner(provider, registry).run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("混合安全批应并发启动，不得超时");

    assert!(result.is_success(), "{:?}", result.error);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    assert!(!results[0].is_error);
    assert_eq!(turn_tool_result_text(results[0]), "mixed-read-ok");
    assert!(!results[1].is_error);
    assert_eq!(turn_tool_result_text(results[1]), "mixed-write-ok");
}

/// #26：批内副作用工具失败时 abort 排队兄弟。
///
/// 失败工具等慢兄弟进入执行后即时失败；慢兄弟在批取消后收到取消固定结果，
/// 调度层把其归因为被 abort，重写为失败结果"并行工具调用 {name} 失败，
/// 已取消排队等待"，含失败工具名；失败工具自身保留真实错误，Turn 不终止。
#[tokio::test]
async fn parallel_safe_side_effect_failure_aborts_queued_sibling() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-fail", "abort_fail", json!({})),
                ("call-slow", "abort_slow", json!({})),
            ]),
            text_reply("done"),
        ],
    ));
    struct AbortFailTool {
        sibling_entered: Arc<Notify>,
    }
    impl AgentTool for AbortFailTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "abort_fail".to_owned(),
                "验证副作用失败 abort 兄弟",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelSafe
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let signal = self.sibling_entered.clone();
            Box::pin(async move {
                signal.notified().await;
                Err(ToolError::permanent(
                    "abort-boom",
                    "副作用失败 abort 固定错误",
                ))
            })
        }
    }
    struct AbortSlowTool {
        entered: Arc<Notify>,
    }
    impl AgentTool for AbortSlowTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "abort_slow".to_owned(),
                "验证被 abort 的排队兄弟",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelSafe
        }
        fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            Box::pin(async move {
                entered.notify_one();
                context.cancellation.cancelled().await;
                std::future::pending().await
            })
        }
    }
    let sibling_entered = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(AbortFailTool {
            sibling_entered: sibling_entered.clone(),
        }))
        .expect("abort 失败工具应可注册");
    registry
        .register(Arc::new(AbortSlowTool {
            entered: sibling_entered.clone(),
        }))
        .expect("abort 慢兄弟工具应可注册");

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runner(provider, registry).run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("abort 排队兄弟不应挂起");

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.is_error, "失败与被 abort 位都应为错误结果");
        let text = turn_tool_result_text(result);
        if result.tool_call_id == "call-fail" {
            assert!(text.contains("abort-boom"), "失败位保留真实错误：{text}");
        } else {
            assert_eq!(result.tool_call_id, "call-slow");
            assert!(
                text.contains("并行工具调用 abort_fail 失败，已取消排队等待"),
                "被 abort 位应含失败工具名：{text}"
            );
        }
    }
}

/// #26 回归：只读失败不触发批 abort，慢兄弟正常完成（#28 语义保持）。
#[tokio::test]
async fn parallel_safe_read_only_failure_keeps_sibling_unaffected() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-fail", "mixed_ro_fail", json!({})),
                ("call-slow", "mixed_ro_slow", json!({})),
            ]),
            text_reply("done"),
        ],
    ));
    struct MixedReadOnlyFailTool {
        sibling_entered: Arc<Notify>,
    }
    impl AgentTool for MixedReadOnlyFailTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "mixed_ro_fail".to_owned(),
                "验证混合批只读失败不 abort",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let signal = self.sibling_entered.clone();
            Box::pin(async move {
                signal.notified().await;
                Err(ToolError::permanent("ro-boom", "混合批只读固定失败"))
            })
        }
    }
    struct MixedReadOnlySlowTool {
        entered: Arc<Notify>,
    }
    impl AgentTool for MixedReadOnlySlowTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "mixed_ro_slow".to_owned(),
                "验证只读失败后慢兄弟仍完成",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            Box::pin(async move {
                entered.notify_one();
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(ToolOutput::text("ro-slow-ok"))
            })
        }
    }
    let sibling_entered = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(MixedReadOnlyFailTool {
            sibling_entered: sibling_entered.clone(),
        }))
        .expect("混合批只读失败工具应可注册");
    registry
        .register(Arc::new(MixedReadOnlySlowTool {
            entered: sibling_entered.clone(),
        }))
        .expect("混合批只读慢兄弟应可注册");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        runner(provider, registry).run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("只读失败不得阻塞兄弟");

    assert!(result.is_success(), "{:?}", result.error);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        let text = turn_tool_result_text(result);
        if result.tool_call_id == "call-fail" {
            assert!(result.is_error);
            assert!(text.contains("ro-boom"), "失败位保留真实错误：{text}");
        } else {
            assert_eq!(result.tool_call_id, "call-slow");
            assert!(!result.is_error);
            assert_eq!(text, "ro-slow-ok");
        }
    }
}

/// #26：Exclusive 工具独占串行——前后 safe 调用分属不同批。
///
/// 前/后只读探针都等待 Exclusive 工具的进入/完成围栏；若被同批并发则
/// 围栏顺序被破坏或测试超时。
#[tokio::test]
async fn parallel_safe_exclusive_tool_splits_batches_serially() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-before", "split_read", json!({})),
                ("call-mid", "split_exclusive", json!({})),
                ("call-after", "split_safe", json!({})),
            ]),
            text_reply("done"),
        ],
    ));
    struct SplitReadTool {
        started: Arc<Notify>,
        finished: Arc<Notify>,
    }
    impl AgentTool for SplitReadTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "split_read".to_owned(),
                "验证 Exclusive 前批只读",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let started = self.started.clone();
            let finished = self.finished.clone();
            Box::pin(async move {
                started.notify_one();
                tokio::time::sleep(Duration::from_millis(50)).await;
                finished.notify_one();
                Ok(ToolOutput::text("split-read-ok"))
            })
        }
    }
    struct SplitExclusiveTool {
        barrier_entered: Arc<AtomicBool>,
        read_finished: Arc<Notify>,
        done: Arc<Notify>,
    }
    impl AgentTool for SplitExclusiveTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "split_exclusive".to_owned(),
                "验证独占屏障",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::Exclusive
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let barrier_entered = self.barrier_entered.clone();
            let read_finished = self.read_finished.clone();
            let done = self.done.clone();
            Box::pin(async move {
                barrier_entered.store(true, Ordering::SeqCst);
                read_finished.notified().await;
                done.notify_one();
                Ok(ToolOutput::text("split-exclusive-ok"))
            })
        }
    }
    struct SplitSafeAfterTool {
        barrier_entered: Arc<AtomicBool>,
        exclusive_done: Arc<Notify>,
    }
    impl AgentTool for SplitSafeAfterTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "split_safe".to_owned(),
                "验证 Exclusive 后批 safe",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelSafe
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let barrier_entered = self.barrier_entered.clone();
            let exclusive_done = self.exclusive_done.clone();
            Box::pin(async move {
                // 后批工具必须在 Exclusive 启动之后才进入执行。
                assert!(
                    barrier_entered.load(Ordering::SeqCst),
                    "后批 safe 工具必须在 Exclusive 启动后才执行"
                );
                exclusive_done.notified().await;
                Ok(ToolOutput::text("split-safe-ok"))
            })
        }
    }
    let read_started = Arc::new(Notify::new());
    let read_finished = Arc::new(Notify::new());
    let barrier_entered = Arc::new(AtomicBool::new(false));
    let exclusive_done = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(SplitReadTool {
            started: read_started.clone(),
            finished: read_finished.clone(),
        }))
        .expect("前批只读工具应可注册");
    registry
        .register(Arc::new(SplitExclusiveTool {
            barrier_entered: barrier_entered.clone(),
            read_finished: read_finished.clone(),
            done: exclusive_done.clone(),
        }))
        .expect("独占工具应可注册");
    registry
        .register(Arc::new(SplitSafeAfterTool {
            barrier_entered: barrier_entered.clone(),
            exclusive_done: exclusive_done.clone(),
        }))
        .expect("后批 safe 工具应可注册");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        runner(provider, registry).run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("Exclusive 分批串行不得超时");

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.step_count(), 3);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 3);
    assert_eq!(turn_tool_result_text(results[0]), "split-read-ok");
    assert_eq!(turn_tool_result_text(results[1]), "split-exclusive-ok");
    assert_eq!(turn_tool_result_text(results[2]), "split-safe-ok");
}

/// #26：11 个 safe 工具按上限 10 分两批——第二批在第一批完成后才启动。
#[tokio::test]
async fn parallel_safe_batch_width_caps_at_ten() {
    let calls: Vec<(String, String, Value)> = (0..11)
        .map(|i| (format!("call-{i}"), "width_probe".to_owned(), json!({})))
        .collect();
    let call_refs: Vec<(&str, &str, Value)> = calls
        .iter()
        .map(|(id, name, args)| (id.as_str(), name.as_str(), args.clone()))
        .collect();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&call_refs), text_reply("done")],
    ));
    struct WidthBatchTool {
        entered: Arc<Mutex<Vec<String>>>,
        first_batch_barrier: Arc<Barrier>,
    }
    impl AgentTool for WidthBatchTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "width_probe".to_owned(),
                "验证并行宽度上限",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            let barrier = self.first_batch_barrier.clone();
            let id = context.tool_call_id.to_string();
            Box::pin(async move {
                entered.lock().expect("宽度测试锁不应损坏").push(id.clone());
                if id == "call-10" {
                    // 第 11 个调用排队到第二批：进入时前 10 个必须都已进入。
                    // 若实现把 11 个同批并发，第 11 个会早于部分前批进入。
                    assert_eq!(
                        entered.lock().expect("宽度测试锁不应损坏").len(),
                        11,
                        "第 11 个调用必须最后进入执行（分批排队）"
                    );
                } else {
                    // 前 10 个在屏障互相等待：仅当 10 个真正并发执行才能通过，
                    // 证明同批重叠；通过后短暂保持执行态，确保第 11 个排队。
                    barrier.wait().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok(ToolOutput::text("width-ok"))
            })
        }
    }
    let entered = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(WidthBatchTool {
            entered: entered.clone(),
            first_batch_barrier: Arc::new(Barrier::new(10)),
        }))
        .expect("宽度探针工具应可注册");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        runner(provider, registry).run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("宽度上限分批不得超时");

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.step_count(), 11);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 11);
    assert!(results.iter().all(|r| !r.is_error));
    let entered = entered.lock().expect("宽度测试锁不应损坏").clone();
    assert_eq!(entered.len(), 11);
    assert_eq!(
        entered[10], "call-10",
        "第 11 个调用必须最后进入执行（分批排队）：{entered:?}"
    );
}

/// #26：入参校验失败的工具视为非安全——独占串行，不与前后 safe 同批。
///
/// 模型第二项调用传非法输入（缺必填 `value`），RecordingTool 的 definition
/// 要求必填，被冻结为 Immediate 结果；前后 safe 调用各成一批串行执行，
/// 启动顺序严格为 before → after。
#[tokio::test]
async fn parallel_safe_invalid_input_tool_is_not_batched() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-before", "invalid_order", json!({"value": "a"})),
                ("call-bad", "invalid_order", json!({})),
                ("call-after", "invalid_order", json!({"value": "b"})),
            ]),
            text_reply("done"),
        ],
    ));
    struct OrderProbeTool {
        entered: Arc<Mutex<Vec<String>>>,
    }
    impl AgentTool for OrderProbeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "invalid_order".to_owned(),
                "验证校验失败独占串行",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                    "additionalProperties": false
                }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ChangesState)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelSafe
        }
        fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let entered = self.entered.clone();
            let id = context.tool_call_id.to_string();
            Box::pin(async move {
                entered.lock().expect("顺序测试锁不应损坏").push(id);
                Ok(ToolOutput::text("order-ok"))
            })
        }
    }
    let entered = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(OrderProbeTool {
            entered: entered.clone(),
        }))
        .expect("顺序探针工具应可注册");

    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        entered.lock().expect("顺序测试锁不应损坏").clone(),
        vec!["call-before".to_owned(), "call-after".to_owned()],
        "仅合法调用应真实执行且按序串行"
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 3);
    assert!(!results[0].is_error);
    assert!(results[1].is_error);
    assert!(
        turn_tool_result_text(results[1]).contains("工具输入无效"),
        "校验失败位应为输入无效固定结果：{}",
        turn_tool_result_text(results[1])
    );
    assert!(!results[2].is_error);
}

/// #26：用户取消 mid-batch 整批取消——混合批兄弟均为取消固定结果。
#[tokio::test]
async fn parallel_safe_batch_user_cancellation_cancels_whole_batch() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("call-a", "mixed_cancel_probe", json!({})),
            ("call-b", "mixed_cancel_probe", json!({})),
        ])],
    ));
    struct MixedCancelProbeTool {
        started: Arc<Notify>,
    }
    impl AgentTool for MixedCancelProbeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "mixed_cancel_probe".to_owned(),
                "验证混合批用户取消整批",
                json!({ "type": "object", "additionalProperties": true }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let started = self.started.clone();
            Box::pin(async move {
                started.notify_one();
                // 挂起等待取消：取消恒走执行竞速 select 的左臂（Ok(raw) +
                // terminal_error=Cancelled），工具返回的 Err 在此路径不可达。
                context.cancellation.cancelled().await;
                std::future::pending().await
            })
        }
    }
    let started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(MixedCancelProbeTool {
            started: started.clone(),
        }))
        .expect("混合取消探针工具应可注册");
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let cancel_task = tokio::spawn(async move {
        started.notified().await;
        // 等首个兄弟进入执行后再取消，确保取消发生在批执行中途。
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancellation.cancel();
    });

    let result = runner(provider, registry).run_turn(request).await;
    cancel_task.await.expect("取消任务不应异常");

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.is_error);
        assert!(
            turn_tool_result_text(result).contains("Turn 取消而中止"),
            "用户取消后兄弟应为取消固定结果：{}",
            turn_tool_result_text(result)
        );
    }
}

/// 熔断终态 tripwire：同指纹在段内达到终态阈值时，旧语义会取消运行中的
/// 慢兄弟（切断为"中止"），新语义下慢兄弟必须正常完成且 Turn 以 ToolLoop 终态结束。
#[tokio::test]
async fn parallel_segment_terminal_does_not_cancel_running_sibling() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-f1", "trip_fail", json!({})),
                ("call-f2", "trip_fail", json!({})),
                ("call-slow", "trip_slow", json!({"value": "x"})),
            ]),
            text_reply("done"),
        ],
    ));
    struct TripFailTool {
        started: Arc<Notify>,
    }
    impl AgentTool for TripFailTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "trip_fail".to_owned(),
                "验证熔断终态不取消慢兄弟",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "additionalProperties": false
                }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let started = self.started.clone();
            Box::pin(async move {
                started.notify_one();
                Err(ToolError::permanent("trip-boom", "熔断 tripwire 固定错误"))
            })
        }
    }
    struct TripSlowTool {
        fail_started: Arc<Notify>,
    }
    impl AgentTool for TripSlowTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "trip_slow".to_owned(),
                "验证熔断时慢兄弟仍完成",
                json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
            )
        }
        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }
        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            let fail_started = self.fail_started.clone();
            Box::pin(async move {
                // 等两次失败都已回流（熔断已触发）后再返回成功：旧语义下本
                // 调用会被段取消切断，新语义下必须正常完成。
                fail_started.notified().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(ToolOutput::text("trip-slow-ok"))
            })
        }
    }
    let fail_started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(TripFailTool {
            started: fail_started.clone(),
        }))
        .expect("熔断测试工具应可注册");
    registry
        .register(Arc::new(TripSlowTool {
            fail_started: fail_started.clone(),
        }))
        .expect("慢兄弟测试工具应可注册");
    // 终态阈值 2：同指纹（同名同输入）失败两次即熔断。
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(2)
        .expect("重复失败上限应有效");
    let result = AgentRunner::new(provider, registry, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolLoop {
            kind: ToolLoopKind::RepeatedFailure,
            ..
        })
    ));
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 3);
    for result in results {
        if result.tool_call_id == "call-slow" {
            assert!(!result.is_error);
            assert_eq!(turn_tool_result_text(result), "trip-slow-ok");
        } else {
            assert!(result.is_error);
            assert!(turn_tool_result_text(result).contains("trip-boom"));
        }
    }
}

/// 同时记录模型 Round 用量与权威提交事件的输出上限恢复路径探针。
struct MaxOutputRecoveryProbe {
    /// 按提交顺序捕获的用量事实。
    usages: Mutex<Vec<ModelRoundUsage>>,
    /// 按提交顺序保存完整权威事件。
    events: Mutex<Vec<AgentCommitEvent>>,
}

impl MaxOutputRecoveryProbe {
    /// 创建一个没有历史记录的恢复路径探针。
    fn new() -> Self {
        Self {
            usages: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    /// 返回按实际调用顺序捕获的用量事实。
    fn usages(&self) -> Vec<ModelRoundUsage> {
        self.usages.lock().expect("恢复探针用量锁不应损坏").clone()
    }

    /// 返回按提交顺序捕获的权威事件快照。
    fn events(&self) -> Vec<AgentCommitEvent> {
        self.events.lock().expect("恢复探针事件锁不应损坏").clone()
    }
}

impl AgentCommitSink for MaxOutputRecoveryProbe {
    /// 记录完整用量事实并直接确认。
    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        self.usages
            .lock()
            .expect("恢复探针用量锁不应损坏")
            .push(usage.clone());
        Ok(())
    }

    /// 委托默认无状态预检，恢复测试不包含实际工具 Round 预检分支。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        NoopAgentCommitSink.preflight_tool_round(round)
    }

    /// 记录全部权威事件并直接确认。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.events
            .lock()
            .expect("恢复探针事件锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

/// 断言消息列表中没有续跑指令形态的 User is_meta 消息。
fn assert_no_recovery_instruction(messages: &[Message]) {
    assert!(
        messages
            .iter()
            .all(|message| !(message.is_meta && matches!(message.role, MessageRole::User))),
        "不应注入输出上限续跑指令消息"
    );
}

/// 第一次截断（纯文本、stop_reason=max_tokens）注入续跑指令后第二次正常完成：
/// 部分响应照常提交，指令消息夹在截断 assistant 消息与最终 assistant 消息之间，
/// 两次采样按独立 Round 的 call_attempt 1/2 记账，Turn 以 Completed 终态结束。
#[tokio::test]
async fn max_output_truncation_recovers_with_instruction_and_completes() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("半截输出", StopReason::MaxOutputTokens),
            text_reply("从中断处继续的完整内容"),
        ],
    ));
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.state.round_count(), 2);
    // 续跑请求等价性：第二次请求携带全部已提交消息（初始输入、截断部分响应与
    // 续跑指令），与最终 Transcript 除最终响应外的消息逐条一致。
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests[1].messages.as_slice(), &result.messages[..3]);
    // Transcript 形态：初始 user → 截断 assistant → 续跑指令（user is_meta）→ 最终 assistant。
    assert_eq!(result.messages.len(), 4);
    assert!(matches!(result.messages[1].role, MessageRole::Assistant));
    assert!(matches!(
        result.messages[1].content[0],
        ContentBlock::Text { .. }
    ));
    let instruction = &result.messages[2];
    assert!(instruction.is_meta);
    assert!(matches!(instruction.role, MessageRole::User));
    let ContentBlock::Text { text } = &instruction.content[0] else {
        panic!("续跑指令应为纯文本消息");
    };
    assert!(text.contains("上一条回复因达到输出上限被截断"));
    assert!(text.contains("请从中断处直接继续，不要重复已有内容，不要道歉。"));
    assert!(matches!(result.messages[3].role, MessageRole::Assistant));
    // 两次真实采样按独立 Round 记账：Round 1 截断、Round 2 完成。
    let usages = sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(
        usages[0].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[1].model_round(), 2);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
    // Round 1 依次提交带截断完成事实的部分响应与续跑指令消息段。
    let events = sink.events();
    let round_1_commits: Vec<_> = events
        .iter()
        .filter(|event| event.model_round() == 1)
        .collect();
    assert_eq!(round_1_commits.len(), 2);
    let AgentCommitEventKind::ModelRoundCommitted {
        completion,
        messages: partial,
        ..
    } = round_1_commits[0].kind()
    else {
        panic!("截断部分响应应作为模型完成事实提交");
    };
    assert_eq!(completion.stop_reason, StopReason::MaxOutputTokens);
    assert_eq!(partial, &result.messages[1..2]);
    let AgentCommitEventKind::RoundCommitted { messages, .. } = round_1_commits[1].kind() else {
        panic!("续跑指令应作为普通 Round 消息段提交");
    };
    assert_eq!(messages, &result.messages[2..3]);
}

/// 连续 9 次纯文本截断：前 8 次各注入一条续跑指令，第 9 次预算耗尽后
/// 保持 ModelOutputLimit 终态。低输出预算下允许长报告分段完成，但恢复仍有硬上限。
#[tokio::test]
async fn max_output_truncation_recovers_at_most_eight_times_then_terminal() {
    let replies = (1..=9)
        .map(|index| text_reply_with_stop(&format!("第 {index} 段"), StopReason::MaxOutputTokens))
        .collect::<Vec<_>>();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        replies,
    ));
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelOutputLimit)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 9);
    // 初始 user + 9 段截断 assistant + 8 条续跑指令。
    assert_eq!(result.messages.len(), 18);
    for message_index in (2..17).step_by(2) {
        assert!(matches!(
            result.messages[message_index].role,
            MessageRole::User
        ));
        assert!(result.messages[message_index].is_meta);
    }
    assert!(matches!(result.messages[17].role, MessageRole::Assistant));
    // 九个 Round 各自独立记账。
    let usages = sink.usages();
    assert_eq!(usages.len(), 9);
    for (index, usage) in usages.iter().enumerate() {
        assert_eq!(usage.model_round(), (index + 1) as u32);
        assert_eq!(usage.call_attempt(), (index + 1) as u32);
        assert_eq!(usage.completion().stop_reason, StopReason::MaxOutputTokens);
    }
}

/// 截断响应包含工具调用块时不恢复：工具参数可能已被截断，续跑不安全，
/// 保持既有 ModelOutputLimit 终态且工具不执行。
#[tokio::test]
async fn max_output_truncation_with_tool_call_block_stays_terminal() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply_with_stop(
            &[("call-truncated", "record", json!({"value": "x"}))],
            StopReason::MaxOutputTokens,
        )],
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

    assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelOutputLimit)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 1);
    assert_eq!(tool.call_count(), 0);
    assert_no_recovery_instruction(&result.messages);
}

/// ContentFilter 终止原因不属于输出上限恢复范围：保持既有 ModelRefusal 终态。
#[tokio::test]
async fn content_filter_stop_reason_does_not_recover() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("拒答前文", StopReason::ContentFilter),
            text_reply("不应被请求的续跑响应"),
        ],
    ));
    let result = runner(provider.clone(), ToolRegistry::new())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(result.error, Some(AgentRunError::ModelRefusal));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelRefusal)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 1);
    assert_no_recovery_instruction(&result.messages);
}

/// 显式上限的总结 Round 中发生截断时不恢复：直接按既有 ModelOutputLimit 终态结束。
#[tokio::test]
async fn limit_summary_pending_max_output_truncation_stays_terminal() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
            text_reply_with_stop("总结也被截断", StopReason::MaxOutputTokens),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let result = AgentRunner::new(
        provider.clone(),
        registry,
        RunLimits {
            max_rounds: Some(1),
            ..RunLimits::default()
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(result.error, Some(AgentRunError::ModelOutputLimit));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ModelOutputLimit)
    );
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(tool.call_count(), 1);
    // 总结 Round 的截断不注入续跑指令。
    assert_no_recovery_instruction(&result.messages);
}

/// 续跑指令提交后、下一轮采样期间取消：Turn 以 Cancelled 终态结束，
/// 已提交的部分响应与指令消息保留，不回滚半提交状态。
#[tokio::test]
async fn max_output_recovery_cancelled_inside_next_round_request() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("半截输出", StopReason::MaxOutputTokens),
            text_reply("取消后不应被消费"),
        ],
    ));
    let cancellation = TurnCancellation::new();
    // Round 1 截断响应产生 3 个模型事件；第 4 个事件是续跑 Round 的 MessageStart。
    let cancel_sink = Arc::new(CancelOnNthModelEventSink {
        cancellation: cancellation.clone(),
        remaining_before_cancel: AtomicUsize::new(4),
    });
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation);
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_event_sink(cancel_sink)
        .with_commit_sink(sink.clone())
        .run_turn(request)
        .await;

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    // 续跑请求已经真实发起：取消是在续跑 request_model 内部被观察的。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.messages.len(), 3);
    assert!(result.messages[2].is_meta);
    // 半提交语义：Round 1 的用量在续跑发起前已独立提交，续跑失败不回滚。
    let usages = sink.usages();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
}

/// 恢复轮的响应为空：空响应重试预算独立可用，与输出上限续跑预算互不吃挤。
#[tokio::test]
async fn max_output_recovery_then_empty_response_keeps_independent_retry_budget() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("半截输出", StopReason::MaxOutputTokens),
            empty_reply_with_stop(StopReason::Completed),
            text_reply("重试后的完整内容"),
        ],
    ));
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    // 三个真实请求：截断采样、恢复轮空响应、同一 Round 的空响应重试。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 3);
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.messages.len(), 4);
    assert!(result.messages[2].is_meta);
    let usages = sink.usages();
    assert_eq!(usages.len(), 3);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(
        usages[0].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[1].model_round(), 2);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[2].model_round(), 2);
    assert_eq!(usages[2].call_attempt(), 3);
    assert_eq!(usages[2].completion().stop_reason, StopReason::Completed);
}

/// 空内容 + MaxOutputTokens 不属于续跑恢复：没有可续跑的截断正文，空部分响应段
/// 也会被资源层 reducer 拒绝。它走既有空响应重试重采样一次并完成，不注入续跑指令。
#[tokio::test]
async fn empty_max_output_tokens_response_retries_instead_of_recovery() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            empty_reply_with_stop(StopReason::MaxOutputTokens),
            text_reply("重采样后的完整内容"),
        ],
    ));
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let result = runner(provider.clone(), ToolRegistry::new())
        .with_commit_sink(sink.clone())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    // 一次重采样：截断空响应与同一 Round 的重试，共两次真实请求。
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.state.round_count(), 1);
    assert_no_recovery_instruction(&result.messages);
    assert_eq!(result.messages.len(), 2);
    // 两次采样按同一 Round 的独立调用尝试记账。
    let usages = sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(
        usages[0].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
}

/// 恢复轮遇上下文超限：强制压缩臂保持可达，压缩预算与续跑预算互不挤占——
/// 压缩重试完成工具轮后，第二次截断仍按剩余续跑预算恢复并最终完成。
#[tokio::test]
async fn recovery_round_context_overflow_walks_forced_compaction_and_keeps_recovery_budget() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("半截输出", StopReason::MaxOutputTokens),
            // 恢复轮请求直接上下文超限，进入强制压缩臂。
            context_overflow_error_reply(),
            text_reply("强制摘要"),
            // 压缩重试返回工具调用，进入下一 Round。
            tool_reply(&[("call-compacted", "record", json!({"value": "work"}))]),
            // 第二次纯文本截断：续跑预算未被压缩轮注入或消耗。
            text_reply_with_stop("再次截断", StopReason::MaxOutputTokens),
            text_reply("恢复后完成"),
        ],
    ));
    let tool = Arc::new(RecordingTool::new(
        "record",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("测试工具应可注册");
    let sink = Arc::new(MaxOutputRecoveryProbe::new());
    let result = runner(provider.clone(), registry)
        .with_commit_sink(sink.clone())
        .run_turn(turn_request_with_messages(compactable_tool_history()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    let requests = provider.requests().expect("请求快照应可读取");
    assert_eq!(requests.len(), 6);
    // 第 3 个请求是强制压缩摘要请求：无工具且 tool_choice 为 None。
    assert!(requests[2].tools.is_empty());
    assert_eq!(requests[2].tool_choice, ToolChoice::None);
    // 压缩恰好发生一次，触发来源为 Provider 上下文超限。
    assert_eq!(result.compactions.len(), 1);
    assert_eq!(
        result.compactions[0].trigger,
        ContextCompressionTrigger::ProviderOverflow
    );
    // 两次续跑各注入一条指令：压缩轮既没有额外注入，也没有消耗续跑预算。
    let instructions = result
        .messages
        .iter()
        .filter(|message| message.is_meta && matches!(message.role, MessageRole::User))
        .count();
    assert_eq!(instructions, 2);
    assert_eq!(tool.call_count(), 1);
    // 五次真实模型调用按序记账：上下文超限尝试没有明确用量事实不记账（缺席），
    // 摘要调用以聚合计账序号 0 与独立用途提交。
    let usages = sink.usages();
    assert_eq!(usages.len(), 5);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].purpose(), ModelCallPurpose::AgentRound);
    assert_eq!(
        usages[0].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[1].model_round(), 2);
    assert_eq!(usages[1].call_attempt(), 0);
    assert_eq!(
        usages[1].purpose(),
        ModelCallPurpose::ContextCompactionProviderOverflow
    );
    assert_eq!(usages[1].completion().stop_reason, StopReason::Completed);
    assert_eq!(usages[2].model_round(), 2);
    assert_eq!(usages[2].call_attempt(), 3);
    assert_eq!(usages[2].purpose(), ModelCallPurpose::AgentRound);
    assert_eq!(usages[2].completion().stop_reason, StopReason::ToolUse);
    assert_eq!(usages[3].model_round(), 3);
    assert_eq!(usages[3].call_attempt(), 4);
    assert_eq!(usages[3].purpose(), ModelCallPurpose::AgentRound);
    assert_eq!(
        usages[3].completion().stop_reason,
        StopReason::MaxOutputTokens
    );
    assert_eq!(usages[4].model_round(), 4);
    assert_eq!(usages[4].call_attempt(), 5);
    assert_eq!(usages[4].purpose(), ModelCallPurpose::AgentRound);
    assert_eq!(usages[4].completion().stop_reason, StopReason::Completed);
}

/// 输出预设内容并把完整正文写入测试工件目录的同步落盘通道。
struct TestArtifactSink {
    /// 工件保存目录。
    directory: std::path::PathBuf,
    /// 注入保存失败。
    fail: bool,
}

impl ToolOutputArtifactSink for TestArtifactSink {
    /// 保存失败时返回错误，驱动运行时回退既有固定拒绝语义。
    fn save_output(&self, label: &str, content: &str) -> std::io::Result<String> {
        if self.fail {
            return Err(std::io::Error::other("测试注入的工件保存失败"));
        }
        std::fs::create_dir_all(&self.directory)?;
        let path = self.directory.join(format!("keencode-{label}-test.log"));
        std::fs::write(&path, content)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

/// 返回预设 ToolOutput 并可选接入测试工件目录的合成工具。
struct ArtifactOutputTool {
    /// 提供给模型的精确工具名称。
    name: &'static str,
    /// 每次调用采用的副作用分类。
    effect: ToolEffect,
    /// 每次调用返回的预设输出。
    output: ToolOutput,
    /// 注入的输出落盘通道；`None` 覆盖未接入工件目录的工具形态。
    sink: Option<Arc<TestArtifactSink>>,
}

impl ArtifactOutputTool {
    /// 创建绑定临时工件目录的合成工具。
    fn new(
        name: &'static str,
        effect: ToolEffect,
        output: ToolOutput,
        directory: std::path::PathBuf,
        fail: bool,
    ) -> Self {
        Self {
            name,
            effect,
            output,
            sink: Some(Arc::new(TestArtifactSink { directory, fail })),
        }
    }

    /// 创建不提供输出落盘通道的合成工具，覆盖 sink 缺失的回退路径。
    fn new_without_sink(name: &'static str, effect: ToolEffect, output: ToolOutput) -> Self {
        Self {
            name,
            effect,
            output,
            sink: None,
        }
    }
}

impl AgentTool for ArtifactOutputTool {
    /// 返回要求字符串 `value` 的合成 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name,
            "输出预设内容的合成工具",
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

    /// 副作用工具按顺序屏障执行，保证 Round 聚合判定顺序确定。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 接入测试工件目录的落盘通道；未接入时保持默认 `None` 语义。
    fn output_artifact_sink(&self) -> Option<Arc<dyn ToolOutputArtifactSink>> {
        let sink: Arc<dyn ToolOutputArtifactSink> = self.sink.clone()?;
        Some(sink)
    }

    /// 返回预设输出。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let output = self.output.clone();
        Box::pin(async move { Ok(output) })
    }
}

/// 在系统临时目录创建本次测试专用的工件目录。
fn artifact_test_directory(label: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("keencode-agent-tests")
        .join(format!("{label}-{}", uuid::Uuid::now_v7().simple()))
}

/// 断言结果为一个文本预览块加一个截断说明块，并返回两段文本。
fn expect_truncated_text_with_sentinel(result: &ToolResult) -> (&str, &str) {
    assert!(!result.is_error, "截断回流不算失败：{:?}", result.content);
    let [
        ToolResultContent::Text { text: preview },
        ToolResultContent::Text { text: sentinel },
    ] = &result.content[..]
    else {
        panic!("截断结果应为预览块加截断说明两个文本块");
    };
    (preview.as_str(), sentinel.as_str())
}

/// 只读工具输出超过单结果文本上限时截断为首尾预览并落盘完整工件，Turn 继续。
#[tokio::test]
async fn read_only_over_limit_output_is_truncated_with_artifact_and_turn_continues() {
    let directory = artifact_test_directory("read-only-truncated");
    let body = "a".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1_024);
    let tool = Arc::new(ArtifactOutputTool::new(
        "big_read",
        ToolEffect::ReadOnly,
        ToolOutput::text(body.clone()),
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "big_read", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    // 工具 Round 之后模型继续发起了下一 Round，Turn 未因输出超限终止。
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    let (text, sentinel) = expect_truncated_text_with_sentinel(results[0]);
    assert_eq!(
        text.len(),
        TRUNCATION_PREVIEW_KEEP_BYTES * 2 + TRUNCATION_MARKER.len()
    );
    assert!(text.starts_with(&"a".repeat(TRUNCATION_PREVIEW_KEEP_BYTES)));
    assert!(text.ends_with(&"a".repeat(TRUNCATION_PREVIEW_KEEP_BYTES)));
    assert!(text.contains(TRUNCATION_MARKER));
    assert!(sentinel.starts_with(TRUNCATION_SENTINEL_PREFIX));
    assert!(sentinel.contains(&format!("完整输出共 {} 字节", body.len())));
    assert!(sentinel.contains("keencode-big_read-test.log"));
    let saved = std::fs::read_to_string(directory.join("keencode-big_read-test.log"))
        .expect("工件文件应已写出");
    assert_eq!(saved, body);
    let _ = std::fs::remove_dir_all(&directory);
}

/// 副作用工具输出超限同样截断落盘回流，不再进入 Turn 终态。
#[tokio::test]
async fn side_effect_over_limit_output_is_truncated_and_turn_continues() {
    let directory = artifact_test_directory("side-effect-truncated");
    let body = "改".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1_024);
    let tool = Arc::new(ArtifactOutputTool::new(
        "big_write",
        ToolEffect::ChangesState,
        ToolOutput::text(body.clone()),
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "big_write", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    let (text, sentinel) = expect_truncated_text_with_sentinel(results[0]);
    // 多字节字符边界会向内收拢，因此按"原输出前缀 + 标记 + 原输出后缀"断言。
    let marker_position = text.find(TRUNCATION_MARKER).expect("预览块应包含截断标记");
    assert!(body.starts_with(&text[..marker_position]));
    let tail = &text[marker_position + TRUNCATION_MARKER.len()..];
    assert!(body.ends_with(tail));
    assert!(marker_position <= TRUNCATION_PREVIEW_KEEP_BYTES);
    assert!(
        text.len() - marker_position - TRUNCATION_MARKER.len() <= TRUNCATION_PREVIEW_KEEP_BYTES
    );
    assert!(sentinel.contains("keencode-big_write-test.log"));
    let saved = std::fs::read_to_string(directory.join("keencode-big_write-test.log"))
        .expect("工件文件应已写出");
    assert_eq!(saved, body);
    let _ = std::fs::remove_dir_all(&directory);
}

/// 工件保存失败时只读工具回退既有固定拒绝语义，Turn 继续。
#[tokio::test]
async fn read_only_over_limit_output_falls_back_to_fixed_rejection_when_sink_fails() {
    let directory = artifact_test_directory("read-only-sink-failed");
    let body = "a".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1_024);
    let tool = Arc::new(ArtifactOutputTool::new(
        "big_read",
        ToolEffect::ReadOnly,
        ToolOutput::text(body),
        directory.clone(),
        true,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "big_read", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(turn_tool_result_text(results[0]), TOOL_OUTPUT_LIMIT_RESULT);
    assert!(!directory.join("keencode-big_read-test.log").exists());
}

/// 工件保存失败时副作用工具回退既有终态语义，不自动重试。
#[tokio::test]
async fn side_effect_over_limit_output_falls_back_to_terminal_when_sink_fails() {
    let directory = artifact_test_directory("side-effect-sink-failed");
    let body = "a".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1_024);
    let tool = Arc::new(ArtifactOutputTool::new(
        "big_write",
        ToolEffect::ChangesState,
        ToolOutput::text(body),
        directory,
        true,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-1",
            "big_write",
            json!({"value": "x"}),
        )])],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(
        matches!(result.error, Some(AgentRunError::ToolOutputLimit { .. })),
        "{:?}",
        result.error
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(
        turn_tool_result_text(results[0]),
        SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT
    );
}

/// 内容块数超过单结果上限时保留靠前块并附加截断说明，工件保存 JSON 全文。
#[tokio::test]
async fn block_count_over_limit_keeps_leading_blocks_with_sentinel() {
    let directory = artifact_test_directory("block-count-truncated");
    let output = ToolOutput {
        content: (0..=TOOL_OUTPUT_LIMITS.max_content_blocks)
            .map(|index| ToolResultContent::Text {
                text: format!("块{index}"),
            })
            .collect(),
    };
    let tool = Arc::new(ArtifactOutputTool::new(
        "block_tool",
        ToolEffect::ReadOnly,
        output,
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "block_tool", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    let truncated_result = results[0];
    assert!(!truncated_result.is_error);
    assert_eq!(
        truncated_result.content.len(),
        TOOL_OUTPUT_LIMITS.max_content_blocks
    );
    for (index, block) in truncated_result.content[..TOOL_OUTPUT_LIMITS.max_content_blocks - 1]
        .iter()
        .enumerate()
    {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: format!("块{index}")
            }
        );
    }
    let ToolResultContent::Text { text } = truncated_result.content.last().expect("结果非空")
    else {
        panic!("最后一块应为文本截断说明");
    };
    assert!(text.starts_with(TRUNCATION_SENTINEL_PREFIX));
    let saved = std::fs::read_to_string(directory.join("keencode-block_tool-test.log"))
        .expect("工件文件应已写出");
    let parsed: Vec<ToolResultContent> =
        serde_json::from_str(&saved).expect("多块工件应为内容块 JSON 全文");
    assert_eq!(parsed.len(), TOOL_OUTPUT_LIMITS.max_content_blocks + 1);
    let _ = std::fs::remove_dir_all(&directory);
}

/// 单结果合规但 Round 聚合超限时，后到结果截断落盘后整轮提交通过。
#[tokio::test]
async fn round_aggregate_over_limit_truncates_second_result_and_commits_round() {
    let directory = artifact_test_directory("round-aggregate-truncated");
    // 31 块 × 270KiB ≈ 8.2MiB：单结果全部合规；两个结果聚合刚超过 Round
    // 模型可见字节上限，且恰好需要收缩两个块即可重新容纳。
    let block_text = "x".repeat(270 * 1_024);
    let output = ToolOutput {
        content: (0..31)
            .map(|_| ToolResultContent::Text {
                text: block_text.clone(),
            })
            .collect(),
    };
    let first = Arc::new(ArtifactOutputTool::new(
        "round_first",
        ToolEffect::ReadOnly,
        output.clone(),
        directory.clone(),
        false,
    ));
    let second = Arc::new(ArtifactOutputTool::new(
        "round_second",
        ToolEffect::ReadOnly,
        output,
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(first).expect("首个工具应可注册");
    registry.register(second).expect("第二个工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-1", "round_first", json!({"value": "1"})),
                ("call-2", "round_second", json!({"value": "2"})),
            ]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    // 首个结果在聚合预算内，逐字节保持不变。
    assert!(!results[0].is_error);
    assert_eq!(results[0].content.len(), 31);
    for block in &results[0].content {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: block_text.clone()
            }
        );
    }
    // 第二个结果收缩了恰好两个块以回到可保留容量内，并附加截断说明。
    assert!(!results[1].is_error);
    assert_eq!(results[1].content.len(), 32);
    for block in &results[1].content[..2] {
        let ToolResultContent::Text { text } = block else {
            panic!("收缩结果应为文本块");
        };
        assert!(text.contains(TRUNCATION_MARKER));
    }
    for block in &results[1].content[2..31] {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: block_text.clone()
            }
        );
    }
    let ToolResultContent::Text { text: sentinel } = results[1].content.last().expect("结果非空")
    else {
        panic!("最后一块应为文本截断说明");
    };
    assert!(sentinel.starts_with(TRUNCATION_SENTINEL_PREFIX));
    // 整轮聚合模型可见字节回到硬上限内，提交总能通过。
    let round_model_visible: usize = results
        .iter()
        .flat_map(|result| &result.content)
        .map(|block| match block {
            ToolResultContent::Text { text } => text.len(),
            ToolResultContent::Image { .. } => 0,
        })
        .sum();
    assert!(round_model_visible <= TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes);
    // 第二个调用的完整正文已落盘为 JSON 工件。
    let saved = std::fs::read_to_string(directory.join("keencode-round_second-test.log"))
        .expect("工件文件应已写出");
    let parsed: Vec<ToolResultContent> =
        serde_json::from_str(&saved).expect("多块工件应为内容块 JSON 全文");
    assert_eq!(parsed.len(), 31);
    for block in &parsed {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: block_text.clone()
            }
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// 预算内结果逐字节保持不变且不产生任何工件文件。
#[tokio::test]
async fn within_budget_output_is_byte_identical_without_artifact() {
    let directory = artifact_test_directory("within-budget");
    let body = "正常输出内容".repeat(100);
    let tool = Arc::new(ArtifactOutputTool::new(
        "small_tool",
        ToolEffect::ReadOnly,
        ToolOutput::text(body.clone()),
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "small_tool", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].content,
        vec![ToolResultContent::Text { text: body }]
    );
    assert!(!results[0].is_error);
    assert!(
        std::fs::read_dir(&directory).is_err(),
        "预算内结果不应创建任何工件文件"
    );
}

/// 返回工件目录中已保存的文件名列表；目录不存在时测试断言失败。
fn artifact_file_names(directory: &std::path::Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .expect("工件目录应已创建")
        .map(|entry| {
            entry
                .expect("工件目录项应可读")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// 合法图片与长文本块挤满单结果 JSON 预算时，截断循环必须经塌缩快速终止。
///
/// 这是收缩不动点的运行时回归：图片的 JSON 占用是文本收缩无法触及的
/// 下界，修复前的收缩循环会因不动点永久空转并挂死整个 Turn。
#[tokio::test]
async fn images_with_long_text_over_json_limit_collapse_to_sentinel_and_turn_continues() {
    let directory = artifact_test_directory("images-json-collapse");
    let data = format!("{}==", "A".repeat(6_291_454));
    let output = ToolOutput {
        content: vec![
            ToolResultContent::Image {
                image: ImageContent::from_base64("image/png", data.clone()),
            },
            ToolResultContent::Image {
                image: ImageContent::from_base64("image/png", data),
            },
            ToolResultContent::Text {
                text: "x".repeat(300 * 1_024),
            },
        ],
    };
    let tool = Arc::new(ArtifactOutputTool::new(
        "image_tool",
        ToolEffect::ReadOnly,
        output,
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "image_tool", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error);
    let [ToolResultContent::Text { text }] = &results[0].content[..] else {
        panic!("图片占满 JSON 预算时应塌缩为仅剩截断说明块");
    };
    assert!(text.starts_with(TRUNCATION_SENTINEL_PREFIX));
    assert!(text.contains("keencode-image_tool-test.log"));
    // 完整正文已落盘：多块内容保存为内容块 JSON 全文，图片不丢失。
    let saved = std::fs::read_to_string(directory.join("keencode-image_tool-test.log"))
        .expect("工件文件应已写出");
    let parsed: Vec<ToolResultContent> =
        serde_json::from_str(&saved).expect("多块工件应为内容块 JSON 全文");
    assert_eq!(parsed.len(), 3);
    let _ = std::fs::remove_dir_all(&directory);
}

/// Round 聚合超限且无工件通道时，只读结果回退固定拒绝文本，Turn 继续。
#[tokio::test]
async fn read_only_round_aggregate_over_limit_without_sink_falls_back_to_fixed_rejection() {
    let directory = artifact_test_directory("round-aggregate-no-sink-read-only");
    // 首个结果 63 块恰好占满 Round 块预算并保留一个待生成失败槽位；
    // 第二个结果自身合规但聚合后再无 2 个块容量。
    let first = ToolOutput {
        content: (0..TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1)
            .map(|index| ToolResultContent::Text {
                text: format!("块{index}"),
            })
            .collect(),
    };
    let second = ToolOutput {
        content: vec![
            ToolResultContent::Text {
                text: "尾块一".to_owned(),
            },
            ToolResultContent::Text {
                text: "尾块二".to_owned(),
            },
        ],
    };
    let first_tool = Arc::new(ArtifactOutputTool::new_without_sink(
        "aggregate_first",
        ToolEffect::ReadOnly,
        first,
    ));
    let second_tool = Arc::new(ArtifactOutputTool::new_without_sink(
        "aggregate_second",
        ToolEffect::ReadOnly,
        second,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(first_tool).expect("首个工具应可注册");
    registry.register(second_tool).expect("第二个工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-1", "aggregate_first", json!({"value": "1"})),
                ("call-2", "aggregate_second", json!({"value": "2"})),
            ]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    // 首个结果在聚合预算内逐字节保持不变。
    assert!(!results[0].is_error);
    assert_eq!(
        results[0].content.len(),
        TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
    );
    // 第二个结果被整体替换为固定超限拒绝文本。
    assert!(results[1].is_error);
    assert_eq!(turn_tool_result_text(results[1]), TOOL_OUTPUT_LIMIT_RESULT);
    assert!(
        std::fs::read_dir(&directory).is_err(),
        "无工件通道时不应创建任何工件文件"
    );
}

/// Round 聚合超限且无工件通道时，副作用结果回退 ToolOutputLimit 终态。
#[tokio::test]
async fn side_effect_round_aggregate_over_limit_without_sink_is_terminal() {
    let directory = artifact_test_directory("round-aggregate-no-sink-side-effect");
    let first = ToolOutput {
        content: (0..TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1)
            .map(|index| ToolResultContent::Text {
                text: format!("块{index}"),
            })
            .collect(),
    };
    let second = ToolOutput {
        content: vec![
            ToolResultContent::Text {
                text: "尾块一".to_owned(),
            },
            ToolResultContent::Text {
                text: "尾块二".to_owned(),
            },
        ],
    };
    let first_tool = Arc::new(ArtifactOutputTool::new_without_sink(
        "aggregate_side_first",
        ToolEffect::ChangesState,
        first,
    ));
    let second_tool = Arc::new(ArtifactOutputTool::new_without_sink(
        "aggregate_side_second",
        ToolEffect::ChangesState,
        second,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(first_tool).expect("首个工具应可注册");
    registry.register(second_tool).expect("第二个工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("call-1", "aggregate_side_first", json!({"value": "1"})),
            ("call-2", "aggregate_side_second", json!({"value": "2"})),
        ])],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: None,
        })
    );
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    assert!(!results[0].is_error);
    assert_eq!(
        results[0].content.len(),
        TOOL_OUTPUT_LIMITS.max_round_content_blocks - 1
    );
    assert!(results[1].is_error);
    assert_eq!(
        turn_tool_result_text(results[1]),
        SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT
    );
    assert!(
        std::fs::read_dir(&directory).is_err(),
        "无工件通道时不应创建任何工件文件"
    );
}

/// 单结果截断落盘后再遇 Round 聚合超限时复用同一工件指针，只保存一个文件。
#[tokio::test]
async fn round_aggregate_shrink_reuses_saved_artifact_without_new_file() {
    let directory = artifact_test_directory("round-aggregate-artifact-reuse");
    // 首个结果 23 × 512KiB 合规且占据大部分 Round 模型可见预算。
    let first_block = "a".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes);
    let first = ToolOutput {
        content: (0..23)
            .map(|_| ToolResultContent::Text {
                text: first_block.clone(),
            })
            .collect(),
    };
    // 第二个结果 24 × 512KiB 超出单结果 JSON 上限：先在单结果阶段截断
    // 落盘，随后聚合超限继续复用该工件收缩其余文本块。
    let second_block = "b".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes);
    let second = ToolOutput {
        content: (0..24)
            .map(|_| ToolResultContent::Text {
                text: second_block.clone(),
            })
            .collect(),
    };
    let first_tool = Arc::new(ArtifactOutputTool::new_without_sink(
        "reuse_first",
        ToolEffect::ReadOnly,
        first,
    ));
    let second_tool = Arc::new(ArtifactOutputTool::new(
        "reuse_second",
        ToolEffect::ChangesState,
        second,
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(first_tool).expect("首个工具应可注册");
    registry.register(second_tool).expect("第二个工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-1", "reuse_first", json!({"value": "1"})),
                ("call-2", "reuse_second", json!({"value": "2"})),
            ]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.round_count(), 2);
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 2);
    // 首个结果在聚合预算内逐字节保持不变。
    assert!(!results[0].is_error);
    assert_eq!(results[0].content.len(), 23);
    for block in &results[0].content {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: first_block.clone()
            }
        );
    }
    // 第二个结果保持成功分类：聚合收缩把全部原始文本块收缩为有界预览，
    // 并在单结果与聚合两次截断中各附加一个指向同一工件的说明块。
    // 第二个结果保持成功分类：单结果截断收缩 1 块，聚合复用收缩再收缩
    // 15 块后恰好回到可保留容量（首结果占 12058624 字节，剩余
    // 4718592 字节），其余 8 块逐字节保留，并各附加一个同指针说明块。
    assert!(!results[1].is_error);
    assert_eq!(results[1].content.len(), 26);
    let mut previews = 0_usize;
    let mut fulls = 0_usize;
    for block in &results[1].content[..24] {
        let ToolResultContent::Text { text } = block else {
            panic!("收缩结果应为文本块");
        };
        if text.len() == TRUNCATION_PREVIEW_KEEP_BYTES * 2 + TRUNCATION_MARKER.len() {
            previews += 1;
            assert!(text.contains(TRUNCATION_MARKER));
            assert!(text.starts_with(&"b".repeat(TRUNCATION_PREVIEW_KEEP_BYTES)));
        } else {
            assert_eq!(text, &second_block, "未收缩块必须逐字节保留原输出");
            fulls += 1;
        }
    }
    assert_eq!(previews, 16, "单结果与聚合两次截断合计应收缩 16 块");
    assert_eq!(fulls, 8, "聚合收缩恰可容纳后剩余完整块应原样保留");
    let [
        ToolResultContent::Text {
            text: first_sentinel,
        },
        ToolResultContent::Text {
            text: last_sentinel,
        },
    ] = &results[1].content[24..]
    else {
        panic!("尾部两块应为文本截断说明");
    };
    // 两次截断说明指向同一工件指针，文本完全一致。
    assert_eq!(first_sentinel, last_sentinel);
    assert!(last_sentinel.starts_with(TRUNCATION_SENTINEL_PREFIX));
    assert!(last_sentinel.contains("keencode-reuse_second-test.log"));
    // 只有一个工件文件：聚合收缩复用同一指针，完整正文保持原始 24 块。
    assert_eq!(
        artifact_file_names(&directory),
        ["keencode-reuse_second-test.log"]
    );
    let saved = std::fs::read_to_string(directory.join("keencode-reuse_second-test.log"))
        .expect("工件文件应已写出");
    let parsed: Vec<ToolResultContent> =
        serde_json::from_str(&saved).expect("多块工件应为内容块 JSON 全文");
    assert_eq!(parsed.len(), 24);
    for block in &parsed {
        assert_eq!(
            block,
            &ToolResultContent::Text {
                text: second_block.clone()
            }
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// 记录成功与失败 PostHook 观察并注入超出 PostHook 模型可见预算的上下文。
struct OversizedPostContextHook {
    /// 按进入顺序保存成功 PostHook 看到的截断成功结果。
    post_results: Arc<Mutex<Vec<ToolResult>>>,
    /// 按进入顺序保存容量失败后失败 Hook 看到的最终固定结果。
    failure_contexts: Arc<Mutex<Vec<PostToolUseFailureContext>>>,
}

impl AgentHook for OversizedPostContextHook {
    /// 返回截断与 PostHook 容量组合回归使用的稳定名称。
    fn name(&self) -> &str {
        "oversized-post-context-hook"
    }

    /// 记录截断成功结果，并返回必然超过 PostHook 模型可见预算的上下文。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.post_results
            .lock()
            .expect("成功 PostHook 结果锁不应损坏")
            .push(context.result.clone());
        let text = "x".repeat(TOOL_OUTPUT_LIMITS.max_post_hook_model_visible_bytes + 1);
        Box::pin(async move {
            Ok(ToolHookOutput {
                context: vec![HookContextAddition::new(text)],
            })
        })
    }

    /// 记录容量替换后唯一固定失败结果，不再追加任何上下文。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.failure_contexts
            .lock()
            .expect("失败 PostHook 上下文锁不应损坏")
            .push(context);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }
}

/// 截断落盘成功后 PostHook 容量失败：结果原子替换为固定超限失败且双 Hook 各一次。
#[tokio::test]
async fn truncated_success_post_hook_capacity_failure_falls_back_to_fixed_rejection() {
    let directory = artifact_test_directory("truncated-post-hook-capacity");
    let body = "x".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 75_712);
    let tool = Arc::new(ArtifactOutputTool::new(
        "hook_trunc",
        ToolEffect::ReadOnly,
        ToolOutput::text(body.clone()),
        directory.clone(),
        false,
    ));
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("测试工具应可注册");
    let post_results = Arc::new(Mutex::new(Vec::new()));
    let failure_contexts = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(OversizedPostContextHook {
            post_results: post_results.clone(),
            failure_contexts: failure_contexts.clone(),
        }))
        .expect("PostHook 容量组合记录器应注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "hook_trunc", json!({"value": "x"}))]),
            text_reply("完成"),
        ],
    ));
    let result = runner(provider, registry)
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    // PostHook 容量失败属于 Hook 硬上限，Turn 以 Hook 错误终止。
    assert!(
        matches!(
            result.error,
            Some(AgentRunError::Hook(
                HookError::PostOutputModelVisibleBytesExceeded { maximum, .. }
            )) if maximum == TOOL_OUTPUT_LIMITS.max_post_hook_model_visible_bytes
        ),
        "{:?}",
        result.error
    );
    // 最终结果已同步替换为固定超限失败。
    let results = turn_tool_results(&result);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(turn_tool_result_text(results[0]), TOOL_OUTPUT_LIMIT_RESULT);
    // 成功 PostHook 恰好一次，看到的是截断落盘后的成功结果。
    let post_results = post_results.lock().expect("成功 PostHook 结果锁不应损坏");
    assert_eq!(post_results.len(), 1);
    assert!(!post_results[0].is_error);
    assert_eq!(post_results[0].content.len(), 2);
    let [
        ToolResultContent::Text { text: preview },
        ToolResultContent::Text { text: sentinel },
    ] = &post_results[0].content[..]
    else {
        panic!("截断成功结果应为预览块加截断说明两个文本块");
    };
    assert_eq!(
        preview.len(),
        TRUNCATION_PREVIEW_KEEP_BYTES * 2 + TRUNCATION_MARKER.len()
    );
    assert!(sentinel.contains("keencode-hook_trunc-test.log"));
    drop(post_results);
    // 失败 PostHook 恰好一次，看到固定超限失败分类与最终结果。
    let failure_contexts = failure_contexts
        .lock()
        .expect("失败 PostHook 上下文锁不应损坏");
    assert_eq!(failure_contexts.len(), 1);
    assert_eq!(
        failure_contexts[0].failure,
        ToolHookFailureKind::OutputLimitExceeded
    );
    assert_eq!(failure_contexts[0].result, results[0].clone());
    drop(failure_contexts);
    // 截断先于 PostHook 发生：完整正文工件已写出。
    let saved = std::fs::read_to_string(directory.join("keencode-hook_trunc-test.log"))
        .expect("工件文件应已写出");
    assert_eq!(saved, body);
    let _ = std::fs::remove_dir_all(&directory);
}

/// #24 主路径零拷贝：两轮 Turn 的 Provider 请求复用既有消息正文分配。
///
/// 100 条大文本历史 + 一轮工具往返后，第二轮请求的全部历史正文地址必须与
/// 第一轮相同；不能用指针相等或内容相等的宽松分支掩盖逐轮深拷贝。
#[tokio::test]
async fn consecutive_model_rounds_share_messages_snapshot() {
    let history: Vec<Message> = (0..100)
        .map(|index| {
            Message::text(
                MessageRole::User,
                format!("历史正文-{index}-{}", "正".repeat(1024)),
            )
        })
        .collect();
    let history_text_pointers = history
        .iter()
        .map(|message| {
            let ContentBlock::Text { text } = &message.content[0] else {
                panic!("历史测试消息必须为文本");
            };
            text.as_ptr()
        })
        .collect::<Vec<_>>();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
            text_reply("done"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .unwrap();
    let request = TurnRequest::new(
        session_id("session-shared-snapshot"),
        turn_id("turn-shared-snapshot"),
        agent_id("agent-shared-snapshot"),
        "test-model",
        history,
        PlanGuard::inactive(),
    );
    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(request)
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.len() > requests[0].messages.len());
    for (index, expected) in history_text_pointers.into_iter().enumerate() {
        for request in &requests {
            let ContentBlock::Text { text } = &request.messages[index].content[0] else {
                panic!("Provider 历史消息必须为文本");
            };
            assert_eq!(text.as_ptr(), expected, "第 {index} 条历史发生了深拷贝");
        }
    }
}

/// #24 持久分段隔离：跨轮 `commit` 追加动态段后，首轮请求快照不变。
///
/// 首轮请求被 Provider 记录后，后续追加的工具结果只出现在第二轮请求与
/// 最终 Transcript 中，不回写首轮快照。
#[tokio::test]
async fn committed_segments_do_not_rewrite_first_round_snapshot() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "record", json!({"value": "work"}))]),
            text_reply("done"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .unwrap();
    let request = turn_request(PlanGuard::inactive());
    let first_round_len = request.model_request().messages.len();
    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(request)
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    let requests = provider.requests().unwrap();
    assert_eq!(requests.len(), 2);
    // 首轮快照保持初始长度：后续工具 Round 的提交不得回写已发出的请求。
    assert_eq!(requests[0].messages.len(), first_round_len);
    // 第二轮携带全部已提交消息（初始输入 + assistant 调用 + 工具结果）。
    assert!(requests[1].messages.len() > requests[0].messages.len());
    assert_eq!(
        &requests[1].messages[..first_round_len],
        requests[0].messages.as_slice()
    );
    // 最终 Transcript 与第二轮请求前缀一致（除最终响应外）。
    assert!(result.messages.len() >= requests[1].messages.len());
    assert_eq!(
        &result.messages[..requests[1].messages.len()],
        requests[1].messages.as_slice()
    );
}

fn todo_reminder_message_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|message| {
            message.is_meta
                && message.content.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text } if text.contains("TodoReminder"))
                })
        })
        .count()
}

fn seeded_todo_state() -> Arc<InMemoryRuntimeState> {
    let state = Arc::new(InMemoryRuntimeState::new(session_id("session-runner")));
    state
        .replace_todos(
            "seed-todo-reminder",
            vec![TodoItem {
                content: "待收尾事项".to_owned(),
                status: TodoStatus::Pending,
                active_form: "正在收尾".to_owned(),
            }],
        )
        .expect("种子 Todo 应可写入");
    state
}

fn record_script(rounds: usize) -> Vec<ScriptedReply> {
    let mut replies = Vec::new();
    for index in 0..rounds {
        let id = format!("t{index}");
        replies.push(tool_reply(&[(&id, "record", json!({ "value": "x" }))]));
    }
    replies.push(text_reply("done"));
    replies
}

async fn todo_reminder_request_counts(
    state: Option<Arc<InMemoryRuntimeState>>,
    plan_guard: PlanGuard,
) -> Vec<usize> {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("record 工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        record_script(10),
    ));
    let mut agent_runner = runner(provider.clone(), registry);
    if let Some(state) = state {
        agent_runner = agent_runner.with_todo_controller(state);
    }
    let result = agent_runner.run_turn(turn_request(plan_guard)).await;
    assert!(result.is_success(), "{:?}", result.error);
    provider
        .requests()
        .unwrap()
        .iter()
        .map(|request| todo_reminder_message_count(&request.messages))
        .collect()
}

/// 连续 10 个模型轮未发起 TodoWrite 且列表非空时，第 10 轮采样前注入一次提醒。
#[tokio::test]
async fn todo_reminder_fires_after_ten_rounds_without_write() {
    let state = seeded_todo_state();
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("record 工具应可注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        record_script(10),
    ));
    let result = runner(provider.clone(), registry)
        .with_todo_controller(state)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    let counts: Vec<usize> = provider
        .requests()
        .unwrap()
        .iter()
        .map(|request| todo_reminder_message_count(&request.messages))
        .collect();
    assert_eq!(counts.len(), 11);
    assert!(counts[..9].iter().all(|count| *count == 0), "{counts:?}");
    assert_eq!(counts[9], 1, "{counts:?}");
    // 提醒进入历史后随请求携带；单请求计数不超过 1 说明窗口内没有第二次注入。
    assert_eq!(counts.iter().copied().max(), Some(1), "{counts:?}");
}

/// TodoWrite 调用重置提醒窗口：写入后 9 轮内即使达到提醒间隔也不再注入。
#[tokio::test]
async fn todo_write_call_resets_reminder_window() {
    let state = seeded_todo_state();
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(RecordingTool::new(
            "record",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("record 工具应可注册");
    // 重置按调用名判定：与真实 TodoWriteTool 同名的合成工具即可驱动窗口重置。
    registry
        .register(Arc::new(RecordingTool::new(
            "TodoWrite",
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("TodoWrite 工具应可注册");
    let call = |id: String, name: &str| tool_reply(&[(&id, name, json!({ "value": "x" }))]);
    let mut replies: Vec<ScriptedReply> = Vec::new();
    for i in 0..14 {
        replies.push(call(format!("t{i}"), "record"));
    }
    // 第 10 轮采样前触发唯一提醒；第 15 轮发起 TodoWrite 重置窗口。
    replies.push(call("todo-write".to_owned(), "TodoWrite"));
    for i in 15..23 {
        replies.push(call(format!("t{i}"), "record"));
    }
    replies.push(text_reply("done"));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        replies,
    ));
    let result = runner(provider.clone(), registry)
        .with_todo_controller(state)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;
    assert!(result.is_success(), "{:?}", result.error);
    let counts: Vec<usize> = provider
        .requests()
        .unwrap()
        .iter()
        .map(|request| todo_reminder_message_count(&request.messages))
        .collect();
    assert_eq!(counts.len(), 24);
    assert_eq!(counts[9], 1, "{counts:?}");
    // 未重置时第 20 轮会二次注入，使该请求计数变为 2。
    assert_eq!(counts.iter().copied().max(), Some(1), "{counts:?}");
}

/// 空列表与 Plan 只读模式都不注入提醒。
#[tokio::test]
async fn todo_reminder_skips_empty_list_and_read_only_mode() {
    let empty_state = Arc::new(InMemoryRuntimeState::new(session_id("session-runner")));
    let empty = todo_reminder_request_counts(Some(empty_state), PlanGuard::inactive()).await;
    assert!(
        empty.iter().all(|count| *count == 0),
        "空列表不应提醒: {empty:?}"
    );

    let read_only =
        todo_reminder_request_counts(Some(seeded_todo_state()), PlanGuard::read_only()).await;
    assert!(
        read_only.iter().all(|count| *count == 0),
        "只读模式不应提醒: {read_only:?}"
    );
}
