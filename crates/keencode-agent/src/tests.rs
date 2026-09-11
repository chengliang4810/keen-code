//! 第一阶段领域状态机的单元测试。

use super::*;
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelStreamEvent, ProviderCapabilities,
    ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason, StructuredOutputCapability,
    StructuredOutputConfig, StructuredOutputEnforcement, StructuredOutputFailureKind, TokenUsage,
    ToolCall, ToolChoice, ToolDefinition, ToolResult,
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

/// 空响应有界重试同样适用于原生结构化输出：连续两次空响应以 MissingOutput 终态结束。
#[tokio::test]
async fn structured_native_empty_responses_fail_with_missing_output_after_one_retry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            structured_output: StructuredOutputCapability::Native,
            ..ProviderCapabilities::default()
        },
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
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 2);
    assert_eq!(result.messages.len(), 1);
    assert!(result.structured_output.is_none());
    // 两次采样的用量都按同一 Round 的独立调用尝试提交。
    let usages = usage_sink.usages();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[1].model_round(), 1);
    assert_eq!(usages[1].call_attempt(), 2);
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
    // 恢复请求仍保持降级状态，不重新携带输出上限。
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

/// 原生结构化输出的截断先按模型终止原因处理、不提前做 Schema 校验：
/// 纯文本截断注入续跑指令后，由下一轮完整响应完成结构化输出。
#[tokio::test]
async fn runner_classifies_native_structured_output_limit_before_schema_validation() {
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
        assert!(
            requests[0]
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Developer)
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

/// 连续 3 次纯文本截断：前 2 次各注入一条续跑指令，第 3 次预算耗尽后
/// 保持既有 ModelOutputLimit 终态，截断部分响应仍照常提交。
#[tokio::test]
async fn max_output_truncation_recovers_at_most_twice_then_terminal() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            text_reply_with_stop("第一段", StopReason::MaxOutputTokens),
            text_reply_with_stop("第二段", StopReason::MaxOutputTokens),
            text_reply_with_stop("第三段", StopReason::MaxOutputTokens),
        ],
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
    assert_eq!(provider.requests().expect("请求快照应可读取").len(), 3);
    // 初始 user → 三段截断 assistant，中间各夹一条续跑指令。
    assert_eq!(result.messages.len(), 6);
    assert!(matches!(result.messages[2].role, MessageRole::User));
    assert!(result.messages[2].is_meta);
    assert!(matches!(result.messages[4].role, MessageRole::User));
    assert!(result.messages[4].is_meta);
    assert!(matches!(result.messages[5].role, MessageRole::Assistant));
    // 三个 Round 各自独立记账。
    let usages = sink.usages();
    assert_eq!(usages.len(), 3);
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
