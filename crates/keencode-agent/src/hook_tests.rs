//! Provider 中立 Hook 生命周期与 Agent Loop 集成测试。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keencode_model::{
    ContentBlock, Message, MessageRole, ModelError, ModelRequest, ModelStreamEvent,
    ProviderCapabilities, ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
    ToolDefinition, ToolResult, ToolResultContent,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;

/// 创建一段正常文本响应。
fn text_reply(text: &str) -> ScriptedReply {
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

/// 创建包含一个或多个完整工具调用的模型响应。
fn tool_reply(calls: &[(&str, &str, Value)]) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("测试工具数量应在 u32 范围内");
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
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::ToolUse,
    });
    ScriptedReply::events(events)
}

/// 创建在流读取阶段返回协议错误的响应。
fn protocol_error_reply() -> ScriptedReply {
    ScriptedReply::new(vec![Err(ModelError::Protocol {
        message: "合成协议错误".to_owned(),
    })])
}

/// 创建固定身份、模型和初始用户消息的 Turn。
fn turn_request(plan: PlanGuard) -> TurnRequest {
    TurnRequest::new(
        SessionId::new("hook-session").expect("测试 Session 标识有效"),
        TurnId::new("hook-turn").expect("测试 Turn 标识有效"),
        AgentId::new("hook-agent").expect("测试 Agent 标识有效"),
        "hook-model",
        vec![Message::text(MessageRole::User, "执行 Hook 集成测试")],
        plan,
    )
}

/// 根据输入动态分类副作用并记录实际执行的合成工具。
struct ProbeTool {
    /// 工具调用和校验顺序日志。
    events: Arc<Mutex<Vec<String>>>,
    /// 实际进入 execute 的最终输入。
    calls: Mutex<Vec<Value>>,
    /// 为真时工具返回稳定失败。
    fail: bool,
}

impl ProbeTool {
    /// 创建成功或失败行为固定的测试工具。
    fn new(events: Arc<Mutex<Vec<String>>>, fail: bool) -> Self {
        Self {
            events,
            calls: Mutex::new(Vec::new()),
            fail,
        }
    }

    /// 返回实际执行输入快照。
    fn calls(&self) -> Vec<Value> {
        self.calls.lock().expect("工具锁不应损坏").clone()
    }
}

impl AgentTool for ProbeTool {
    /// 返回只接受字符串 value 的冻结工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "probe",
            "验证 Hook 的合成工具",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// value 为 write 时分类为状态变更，否则分类为只读。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let value = input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("effect:{value}"));
        Ok(if value == "write" {
            ToolEffect::ChangesState
        } else {
            ToolEffect::ReadOnly
        })
    }

    /// 测试工具按顺序独占执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 保存最终输入并返回预设成功或失败结果。
    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        let value = input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_owned();
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("execute:{value}"));
        self.calls.lock().expect("工具锁不应损坏").push(input);
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err(ToolError::permanent("probe_failed", "合成工具失败"))
            } else {
                Ok(ToolOutput::text(format!("result:{value}")))
            }
        })
    }
}

/// 启动后永久等待，由父 Turn 取消路径负责有界回收的工具。
struct PendingTool {
    /// 首次进入 execute 后通知测试任务。
    started: Arc<Notify>,
}

impl AgentTool for PendingTool {
    /// 返回与普通探针相同的输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "probe",
            "验证取消后 Failure Hook 的合成工具",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 取消测试工具不产生外部状态变更。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 取消测试工具独占运行，避免并发结果干扰。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 通知已经启动后保持挂起，验证 Runtime 的取消宽限窗口。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let started = self.started.clone();
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<Result<ToolOutput, ToolError>>().await
        })
    }
}

/// 使用输入决定完成延迟的并行只读工具。
struct ParallelTool;

impl AgentTool for ParallelTool {
    /// 返回支持任意字符串 value 的并行工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "parallel_probe",
            "验证并行 Hook 上下文提交顺序",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 所有测试输入均为只读。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 允许相邻调用真正并行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// slow 输入晚于 fast 输入完成，制造与模型顺序相反的完成顺序。
    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        let value = input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_owned();
        Box::pin(async move {
            if value == "slow" {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            Ok(ToolOutput::text(format!("parallel:{value}")))
        })
    }
}

/// 按预设成功布尔序列返回结果的循环保护测试工具。
struct SequencedTool {
    /// true 表示本次成功，false 表示返回相同稳定错误。
    outcomes: Mutex<VecDeque<bool>>,
}

impl AgentTool for SequencedTool {
    /// 返回只接受字符串 value 的固定定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "sequence_probe",
            "验证重复失败成功重置",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 全部序列调用均为只读。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 保持顺序以便精确验证失败计数重置。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 消费一个预设结果，队列耗尽视为测试配置错误。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let succeeded = self
            .outcomes
            .lock()
            .expect("序列工具锁不应损坏")
            .pop_front()
            .expect("序列工具应有剩余结果");
        Box::pin(async move {
            if succeeded {
                Ok(ToolOutput::text("sequence-success"))
            } else {
                Err(ToolError::permanent("same_failure", "相同合成失败"))
            }
        })
    }
}

/// 可按调用顺序返回成功或指定错误码的连续失败语义测试结果。
enum CodedSequenceOutcome {
    /// 本次工具调用成功。
    Success,
    /// 本次工具调用返回指定稳定错误码。
    Failure(&'static str),
}

/// 按预设错误码序列验证不同失败与任意成功都会重置连续计数。
struct CodedSequencedTool {
    /// 等待按调用顺序消费的成功或失败结果。
    outcomes: Mutex<VecDeque<CodedSequenceOutcome>>,
}

impl AgentTool for CodedSequencedTool {
    /// 返回只接受字符串 value 的固定定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "coded_sequence_probe",
            "验证真正连续的重复失败计数",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 全部序列调用均为只读。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 保持严格顺序以便确定失败指纹的相邻关系。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 消费一个预设结果并返回相应成功值或稳定错误码。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let outcome = self
            .outcomes
            .lock()
            .expect("带错误码序列工具锁不应损坏")
            .pop_front()
            .expect("带错误码序列工具应有剩余结果");
        Box::pin(async move {
            match outcome {
                CodedSequenceOutcome::Success => Ok(ToolOutput::text("coded-sequence-success")),
                CodedSequenceOutcome::Failure(code) => {
                    Err(ToolError::permanent(code, "带错误码的合成失败"))
                }
            }
        })
    }
}

/// 为每个并行工具结果追加带输入标识的上下文。
struct ParallelHook {
    /// 实际完成 PostToolUse 的顺序。
    completed: Arc<Mutex<Vec<String>>>,
}

impl AgentHook for ParallelHook {
    /// 返回并行顺序测试使用的稳定名称。
    fn name(&self) -> &str {
        "parallel-hook"
    }

    /// 按实际完成时间记录输入，并追加可在模型请求中观察的上下文。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let value = context
            .input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_owned();
        self.completed
            .lock()
            .expect("并行完成顺序锁不应损坏")
            .push(value.clone());
        Box::pin(async move {
            Ok(ToolHookOutput {
                context: vec![HookContextAddition::new(format!("context:{value}"))],
            })
        })
    }
}

/// 在指定阶段永久挂起，用于验证 Runtime 的 Hook 回调硬超时。
struct PendingPhaseHook {
    /// 测试期间不会自行返回的唯一阶段。
    phase: HookPhase,
    /// 指定阶段实际进入永久挂起回调的次数。
    calls: Arc<AtomicUsize>,
    /// 可选通知指定阶段已经进入隔离工作线程。
    started: Option<Arc<Notify>>,
}

impl AgentHook for PendingPhaseHook {
    /// 返回回调超时测试使用的稳定名称。
    fn name(&self) -> &str {
        "pending-hook"
    }

    /// 只在 PreToolUse 测试中挂起，其他情况直接放行。
    fn pre_tool_use(
        &self,
        _context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        if self.phase == HookPhase::PreToolUse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.started {
                started.notify_one();
            }
            Box::pin(std::future::pending())
        } else {
            Box::pin(async { Ok(PreToolUseOutput::allow()) })
        }
    }

    /// 只在 PostToolUse 测试中挂起，其他情况返回空上下文。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        if self.phase == HookPhase::PostToolUse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.started {
                started.notify_one();
            }
            Box::pin(std::future::pending())
        } else {
            Box::pin(async { Ok(ToolHookOutput::default()) })
        }
    }

    /// 只在 PostToolUseFailure 测试中挂起，其他情况返回空上下文。
    fn post_tool_use_failure(
        &self,
        _context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        if self.phase == HookPhase::PostToolUseFailure {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.started {
                started.notify_one();
            }
            Box::pin(std::future::pending())
        } else {
            Box::pin(async { Ok(ToolHookOutput::default()) })
        }
    }

    /// 只在 PostCompact 测试中挂起，其他情况直接放行。
    fn post_compact(
        &self,
        _context: PostCompactHookContext,
    ) -> HookFuture<'_, Result<(), HookCallbackError>> {
        if self.phase == HookPhase::PostCompact {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.started {
                started.notify_one();
            }
            Box::pin(std::future::pending())
        } else {
            Box::pin(async { Ok(()) })
        }
    }
}

/// 等待外部通知后才返回的启动 Hook，用于验证取消后的迟到成功不会被交付。
struct DeferredTurnStartHook {
    name: &'static str,
    state: Arc<DeferredTurnStartState>,
    wait_for_release: bool,
    cancel_on_prepare: Option<TurnCancellation>,
}

struct DeferredTurnStartState {
    started: Arc<Notify>,
    finished: Arc<Notify>,
    release: Arc<Notify>,
    delivered: AtomicUsize,
    aborted: AtomicUsize,
}

impl AgentHook for DeferredTurnStartHook {
    fn name(&self) -> &str {
        self.name
    }

    fn handles_turn_start(&self) -> bool {
        true
    }

    fn turn_start_prepare(&self, _context: &TurnStartHookContext) {
        if let Some(cancellation) = &self.cancel_on_prepare {
            cancellation.cancel();
        }
    }

    fn turn_start(
        &self,
        _context: TurnStartHookContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let state = Arc::clone(&self.state);
        let wait_for_release = self.wait_for_release;
        Box::pin(async move {
            state.started.notify_one();
            if wait_for_release {
                state.release.notified().await;
            }
            state.finished.notify_one();
            Ok(ToolHookOutput::default())
        })
    }

    fn turn_start_delivered(&self, _context: &TurnStartHookContext) {
        self.state.delivered.fetch_add(1, Ordering::SeqCst);
    }

    fn turn_start_aborted(&self, _context: &TurnStartHookContext, _reason: TurnStartAbortReason) {
        self.state.aborted.fetch_add(1, Ordering::SeqCst);
    }
}

/// Hook 隔离层需要覆盖的同步、非协作、Tokio 与 panic 回调模式。
enum IsolationHookMode {
    /// 在回调方法返回 Future 前同步阻塞工作线程。
    SynchronousBlock,
    /// 在 Future 首次 poll 时执行不让出线程的阻塞逻辑。
    NonCooperativePoll,
    /// 在隔离线程中正常等待 Tokio 定时器。
    TokioTimer,
    /// 在隔离工作线程中主动 panic，验证 Join 失败熔断。
    WorkerPanic,
    /// 仅第一次进入时 panic，后续调用用于验证有界自动恢复。
    WorkerPanicOnce,
    /// Future 永久挂起，用于验证超时后仍存活 worker 的跨代容量边界。
    Pending,
}

/// 记录进入次数并按指定模式运行的 Hook 隔离测试实现。
struct IsolationProbeHook {
    /// 当前回调采用的隔离测试模式。
    mode: IsolationHookMode,
    /// Hook 方法被实际进入的总次数。
    calls: Arc<AtomicUsize>,
    /// 阻塞或异步等待使用的固定毫秒数。
    delay_ms: u64,
}

impl AgentHook for IsolationProbeHook {
    /// 返回隔离测试使用的稳定 Hook 名称。
    fn name(&self) -> &str {
        "isolation-hook"
    }

    /// 按模式同步阻塞、非协作阻塞、等待 Tokio timer 或触发工作线程 panic。
    fn pre_tool_use(
        &self,
        _context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            IsolationHookMode::SynchronousBlock => {
                std::thread::sleep(Duration::from_millis(self.delay_ms));
                Box::pin(async { Ok(PreToolUseOutput::allow()) })
            }
            IsolationHookMode::NonCooperativePoll => {
                let delay_ms = self.delay_ms;
                Box::pin(async move {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    Ok(PreToolUseOutput::allow())
                })
            }
            IsolationHookMode::TokioTimer => {
                let delay_ms = self.delay_ms;
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    Ok(PreToolUseOutput::allow())
                })
            }
            IsolationHookMode::WorkerPanic => {
                panic!("合成 Hook 工作线程 panic");
            }
            IsolationHookMode::WorkerPanicOnce if call == 0 => {
                panic!("合成 Hook 工作线程首次 panic");
            }
            IsolationHookMode::WorkerPanicOnce => Box::pin(async { Ok(PreToolUseOutput::allow()) }),
            IsolationHookMode::Pending => Box::pin(std::future::pending()),
        }
    }
}

/// 使用调用方提供名称验证 Registry 边界的最小 Hook。
struct NamedHook {
    /// 注册时返回的原始名称。
    name: String,
}

impl AgentHook for NamedHook {
    /// 返回名称边界测试设置的原始值。
    fn name(&self) -> &str {
        &self.name
    }
}

/// 注册后可切换返回名称，用于验证 Runtime 冻结注册身份。
struct MutableNameHook {
    /// 零表示初始名称，非零表示实现已经切换名称。
    changed: AtomicUsize,
}

impl AgentHook for MutableNameHook {
    /// 根据测试开关返回两个不同但均合法的名称。
    fn name(&self) -> &str {
        if self.changed.load(Ordering::SeqCst) == 0 {
            "frozen-hook"
        } else {
            "changed-hook"
        }
    }

    /// 返回固定错误，让测试观察错误绑定的是冻结名称还是动态名称。
    fn pre_tool_use(
        &self,
        _context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        Box::pin(async {
            Err(HookCallbackError::new(
                "expected_failure",
                "用于验证冻结名称",
            ))
        })
    }
}

/// 通过队列精确控制每个生命周期阶段输出的测试 Hook。
struct ProbeHook {
    /// 跨阶段共享的调用顺序日志。
    events: Arc<Mutex<Vec<String>>>,
    /// 每次 PreToolUse 调用消费一个输出。
    pre: Mutex<VecDeque<Result<PreToolUseOutput, HookCallbackError>>>,
    /// PostToolUse 的固定输出。
    post: Result<ToolHookOutput, HookCallbackError>,
    /// PostToolUseFailure 的固定输出。
    failure: Result<ToolHookOutput, HookCallbackError>,
    /// 每次 Stop 调用消费一个输出。
    stop: Mutex<VecDeque<Result<StopHookOutput, HookCallbackError>>>,
    /// OnError 的固定输出。
    on_error: Result<(), HookCallbackError>,
    /// OnError 收到的最终错误与终态上下文。
    on_error_contexts: Mutex<Vec<OnErrorHookContext>>,
    /// 成功 Post Hook 调用次数。
    post_count: AtomicUsize,
    /// 失败 Post Hook 调用次数。
    failure_count: AtomicUsize,
    /// Stop Hook 调用次数。
    stop_count: AtomicUsize,
}

impl ProbeHook {
    /// 创建默认放行工具并接受候选完成的 Hook。
    fn new(events: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            events,
            pre: Mutex::new(VecDeque::new()),
            post: Ok(ToolHookOutput::default()),
            failure: Ok(ToolHookOutput::default()),
            stop: Mutex::new(VecDeque::new()),
            on_error: Ok(()),
            on_error_contexts: Mutex::new(Vec::new()),
            post_count: AtomicUsize::new(0),
            failure_count: AtomicUsize::new(0),
            stop_count: AtomicUsize::new(0),
        }
    }

    /// 设置下一次 PreToolUse 输出。
    fn with_pre(self, output: Result<PreToolUseOutput, HookCallbackError>) -> Self {
        self.pre
            .lock()
            .expect("Pre Hook 队列锁不应损坏")
            .push_back(output);
        self
    }

    /// 设置成功 PostToolUse 输出。
    fn with_post(mut self, output: Result<ToolHookOutput, HookCallbackError>) -> Self {
        self.post = output;
        self
    }

    /// 设置失败 PostToolUseFailure 输出。
    fn with_failure(mut self, output: Result<ToolHookOutput, HookCallbackError>) -> Self {
        self.failure = output;
        self
    }

    /// 追加一次 Stop Hook 输出。
    fn with_stop(self, output: Result<StopHookOutput, HookCallbackError>) -> Self {
        self.stop
            .lock()
            .expect("Stop Hook 队列锁不应损坏")
            .push_back(output);
        self
    }

    /// 设置 OnError 固定输出。
    fn with_on_error(mut self, output: Result<(), HookCallbackError>) -> Self {
        self.on_error = output;
        self
    }

    /// 返回 OnError 收到的上下文快照。
    fn on_error_contexts(&self) -> Vec<OnErrorHookContext> {
        self.on_error_contexts
            .lock()
            .expect("OnError Hook 锁不应损坏")
            .clone()
    }
}

impl AgentHook for ProbeHook {
    /// 返回测试注册表中的稳定名称。
    fn name(&self) -> &str {
        "probe-hook"
    }

    /// 记录当前输入并消费预设动作。
    fn pre_tool_use(
        &self,
        context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let value = context
            .input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("pre:{value}"));
        let output = self
            .pre
            .lock()
            .expect("Pre Hook 队列锁不应损坏")
            .pop_front()
            .unwrap_or_else(|| Ok(PreToolUseOutput::allow()));
        Box::pin(async move { output })
    }

    /// 记录成功结果及最终输入。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.post_count.fetch_add(1, Ordering::SeqCst);
        let value = context
            .input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("post:{value}"));
        let output = self.post.clone();
        Box::pin(async move { output })
    }

    /// 记录失败分类及最终输入。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.failure_count.fetch_add(1, Ordering::SeqCst);
        let value = context
            .input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("failure:{value}:{:?}", context.failure));
        let output = self.failure.clone();
        Box::pin(async move { output })
    }

    /// 记录最终失败上下文并返回预设观察结果。
    fn on_error(
        &self,
        context: OnErrorHookContext,
    ) -> HookFuture<'_, Result<(), HookCallbackError>> {
        self.on_error_contexts
            .lock()
            .expect("OnError Hook 锁不应损坏")
            .push(context);
        let output = self.on_error.clone();
        Box::pin(async move { output })
    }

    /// 记录候选轮次并消费预设停止决定。
    fn stop(
        &self,
        context: StopHookContext,
    ) -> HookFuture<'_, Result<StopHookOutput, HookCallbackError>> {
        self.stop_count.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .expect("事件锁不应损坏")
            .push(format!("stop:{}", context.stop_hook_round));
        let output = self
            .stop
            .lock()
            .expect("Stop Hook 队列锁不应损坏")
            .pop_front()
            .unwrap_or_else(|| Ok(StopHookOutput::stop()));
        Box::pin(async move { output })
    }
}

/// 记录 OnError 通知顺序，可选择返回观察失败。
struct OnErrorSequenceHook {
    /// 注册表中的唯一名称。
    name: &'static str,
    /// 所有观察者共享的通知顺序。
    calls: Arc<Mutex<Vec<String>>>,
    /// 是否返回合成观察失败。
    fail: bool,
}

impl AgentHook for OnErrorSequenceHook {
    fn name(&self) -> &str {
        self.name
    }

    fn on_error(
        &self,
        _context: OnErrorHookContext,
    ) -> HookFuture<'_, Result<(), HookCallbackError>> {
        self.calls
            .lock()
            .expect("OnError 顺序锁不应损坏")
            .push(self.name.to_owned());
        let result = if self.fail {
            Err(HookCallbackError::new("observer_failed", "合成观察失败"))
        } else {
            Ok(())
        };
        Box::pin(async move { result })
    }
}

/// 记录 PostCompact 通知顺序，可选择返回观察失败。
struct PostCompactSequenceHook {
    /// 注册表中的唯一名称。
    name: &'static str,
    /// 所有观察者共享的通知顺序。
    calls: Arc<Mutex<Vec<String>>>,
    /// 本次 PostCompact 回调的固定结果。
    outcome: Result<(), HookCallbackError>,
}

impl AgentHook for PostCompactSequenceHook {
    /// 返回注册表中的稳定名称。
    fn name(&self) -> &str {
        self.name
    }

    /// 记录通知顺序并返回预设观察结果。
    fn post_compact(
        &self,
        _context: PostCompactHookContext,
    ) -> HookFuture<'_, Result<(), HookCallbackError>> {
        self.calls
            .lock()
            .expect("PostCompact 顺序锁不应损坏")
            .push(self.name.to_owned());
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

/// 构造直接调用 Hook Runtime 所需的最小压缩结果上下文。
fn post_compact_context() -> PostCompactHookContext {
    PostCompactHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("post-compact-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("post-compact-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("post-compact-agent").expect("Agent 标识应有效"),
        },
        model_round: 1,
        record: ContextCompressionRecord {
            kind: ContextCompactionKind::Summary,
            trigger: ContextCompressionTrigger::Budget,
            estimated_tokens_before: 100,
            estimated_tokens_after: 50,
            replaced_start_index: 1,
            replaced_end_index_exclusive: 2,
            replaced_message_count: 1,
            retained_message_count: 2,
            source_digest_sha256: String::new(),
            summary: "合成摘要".to_owned(),
            projections: Vec::new(),
            policy_version: 1,
        },
    }
}

/// 使用一个工具和 Hook 创建完整 Runner。
fn runner(
    provider: Arc<ScriptedProvider>,
    tool: Arc<dyn AgentTool>,
    hook: Arc<dyn AgentHook>,
    limits: HookLimits,
) -> AgentRunner {
    let mut tools = ToolRegistry::new();
    tools.register(tool).expect("测试工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("测试 Hook 应成功注册");
    AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, limits).expect("测试 HookLimits 应有效"))
}

/// 从 Transcript 按顺序提取全部工具结果。
fn tool_results(result: &TurnResult) -> Vec<&ToolResult> {
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

/// 返回测试 ToolResult 的唯一文本内容。
fn tool_result_text(result: &ToolResult) -> &str {
    match result.content.as_slice() {
        [ToolResultContent::Text { text }] => text,
        _ => panic!("测试工具结果必须只包含一个文本块"),
    }
}

/// Hook 修改输入后必须重新分类，并使用最终输入执行与通知 Post Hook。
#[tokio::test]
async fn 修改后输入贯穿重新分类执行和post_hook() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Ok(PreToolUseOutput {
            action: PreToolUseAction::ModifyInput {
                input: json!({"value": "write"}),
            },
            context: Vec::new(),
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-1", "probe", json!({"value": "read"}))]),
            text_reply("done"),
        ],
    ));

    let result = runner(provider, tool.clone(), hook.clone(), HookLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(tool.calls(), vec![json!({"value": "write"})]);
    let authoritative_call = result
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolCall { tool_call } if tool_call.id == "call-1" => Some(tool_call),
            _ => None,
        })
        .expect("Transcript 应保留最终执行的权威工具调用");
    assert_eq!(authoritative_call.arguments, json!({"value": "write"}));
    assert_eq!(hook.post_count.load(Ordering::SeqCst), 1);
    assert_eq!(hook.failure_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        *events.lock().expect("事件锁不应损坏"),
        vec![
            "effect:read",
            "pre:read",
            "effect:write",
            "execute:write",
            "post:write",
            "stop:1",
        ]
    );
}

/// Hook 把只读输入改为副作用输入时，Plan 守卫必须阻止执行。
#[tokio::test]
async fn 修改后的副作用输入不能绕过plan只读守卫() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Ok(PreToolUseOutput {
            action: PreToolUseAction::ModifyInput {
                input: json!({"value": "write"}),
            },
            context: Vec::new(),
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-plan", "probe", json!({"value": "read"}))]),
            text_reply("done"),
        ],
    ));

    let result = runner(provider, tool.clone(), hook, HookLimits::default())
        .run_turn(turn_request(PlanGuard::read_only()))
        .await;

    assert!(result.is_success());
    assert!(tool.calls().is_empty());
    assert_eq!(tool_results(&result).len(), 1);
    assert!(tool_results(&result)[0].is_error);
    assert_eq!(
        *events.lock().expect("事件锁不应损坏"),
        vec!["effect:read", "pre:read", "effect:write", "stop:1"]
    );
}

/// PreToolUse Block 必须跳过工具，同时只形成一个配对结果。
#[tokio::test]
async fn pre_hook_block只生成一个工具结果() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Ok(PreToolUseOutput {
            action: PreToolUseAction::Block {
                message: "策略阻止".to_owned(),
            },
            context: Vec::new(),
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-block", "probe", json!({"value": "read"}))]),
            text_reply("done"),
        ],
    ));

    let result = runner(provider, tool.clone(), hook.clone(), HookLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert!(tool.calls().is_empty());
    assert_eq!(hook.post_count.load(Ordering::SeqCst), 0);
    assert_eq!(hook.failure_count.load(Ordering::SeqCst), 0);
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-block");
    assert!(results[0].is_error);
}

/// 工具失败只能调用 PostToolUseFailure，不能调用成功 Hook。
#[tokio::test]
async fn 工具失败只调用failure_hook并继续模型循环() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_failure(Ok(ToolHookOutput {
            context: vec![HookContextAddition::new("failure-context")],
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-fail", "probe", json!({"value": "read"}))]),
            text_reply("done"),
        ],
    ));

    let result = runner(provider.clone(), tool, hook.clone(), HookLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(hook.post_count.load(Ordering::SeqCst), 0);
    assert_eq!(hook.failure_count.load(Ordering::SeqCst), 1);
    assert_eq!(tool_results(&result).len(), 1);
    assert!(tool_results(&result)[0].is_error);
    let requests = provider.requests().expect("模型请求快照可读取");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message.is_meta
            && message.content.iter().any(|block| match block {
                ContentBlock::Text { text } => text.contains("failure-context"),
                _ => false,
            })
    }));
}

/// 顺序批次首个工具后的 Hook 失败不能把尚未启动的后续调用计入实际 Step。
#[tokio::test]
async fn post_hook失败只记录已经启动的step() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_post(Err(HookCallbackError::new(
            "post_failed",
            "合成 PostToolUse 失败",
        ))),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let result = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[
                ("call-post-first", "probe", json!({"value": "first"})),
                ("call-post-second", "probe", json!({"value": "second"})),
            ])],
        )),
        tool.clone(),
        hook,
        HookLimits::default(),
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Hook(HookError::Callback {
            phase: HookPhase::PostToolUse,
            hook_name: "probe-hook".to_owned(),
            code: "post_failed".to_owned(),
            message: "合成 PostToolUse 失败".to_owned(),
        }))
    );
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.calls().len(), 1);
    let results = tool_results(&result);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-post-first");
    assert_eq!(results[1].tool_call_id, "call-post-second");
    assert!(!results[0].is_error);
    assert!(results[1].is_error);
}

/// Stop Hook Continue 必须追加用户级上下文并重新请求模型。
#[tokio::test]
async fn stop_hook_continue追加上下文并再次请求模型() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone())
            .with_stop(Ok(StopHookOutput {
                action: StopHookAction::Continue,
                context: vec![HookContextAddition::new("继续检查")],
            }))
            .with_stop(Ok(StopHookOutput::stop())),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("candidate"), text_reply("final")],
    ));

    let result = runner(provider.clone(), tool, hook.clone(), HookLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(hook.stop_count.load(Ordering::SeqCst), 2);
    assert!(hook.on_error_contexts().is_empty());
    let requests = provider.requests().expect("模型请求快照可读取");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message.is_meta
            && message.content.iter().any(|block| match block {
                ContentBlock::Text { text } => {
                    text.contains("不得覆盖 system") && text.contains("继续检查")
                }
                _ => false,
            })
    }));
}

/// Provider 协议错误必须直接失败、跳过 Stop，并在最终终态后通知一次 OnError。
#[tokio::test]
async fn provider协议错误不运行stop_hook() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [protocol_error_reply()],
    ));
    let result = runner(
        provider,
        Arc::new(ProbeTool::new(events.clone(), false)),
        hook.clone(),
        HookLimits::default(),
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(result.error, Some(AgentRunError::Model(_))));
    assert_eq!(hook.stop_count.load(Ordering::SeqCst), 0);
    let contexts = hook.on_error_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].terminal_reason, TerminalReason::Failed);
    assert!(matches!(
        &contexts[0].error,
        AgentRunError::Model(ModelError::Protocol { message }) if message == "合成协议错误"
    ));
}

/// OnError 自身失败只能记录观察失败，不能覆盖原始 Provider 错误或递归触发。
#[tokio::test]
async fn on_error失败保留原错误且不递归() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_on_error(Err(HookCallbackError::new(
            "observer_failed",
            "合成 OnError 失败",
        ))),
    );
    let result = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [protocol_error_reply()],
        )),
        Arc::new(ProbeTool::new(events, false)),
        hook.clone(),
        HookLimits::default(),
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::Protocol { ref message }))
            if message == "合成协议错误"
    ));
    assert_eq!(hook.on_error_contexts().len(), 1);
}

/// 单个 OnError 观察者失败不能阻止后续观察者收到同一最终失败。
#[tokio::test]
async fn on_error失败后继续通知剩余观察者() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(OnErrorSequenceHook {
            name: "first-observer",
            calls: calls.clone(),
            fail: true,
        }))
        .expect("首个观察 Hook 应成功注册");
    hooks
        .register(Arc::new(OnErrorSequenceHook {
            name: "second-observer",
            calls: calls.clone(),
            fail: false,
        }))
        .expect("第二个观察 Hook 应成功注册");
    let result = AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [protocol_error_reply()],
        )),
        ToolRegistry::new(),
        RunLimits::default(),
    )
    .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::Protocol { ref message }))
            if message == "合成协议错误"
    ));
    assert_eq!(
        calls.lock().expect("OnError 顺序锁不应损坏").as_slice(),
        ["first-observer", "second-observer"]
    );
}

/// 取消只撤销未交付的启动结果；旧 worker 迟到成功不能提交，新候选仍可重试。
#[tokio::test]
async fn 取消后启动hook迟到成功不得提交且重载候选可重试() {
    let state = Arc::new(DeferredTurnStartState {
        started: Arc::new(Notify::new()),
        finished: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        delivered: AtomicUsize::new(0),
        aborted: AtomicUsize::new(0),
    });
    let mut first_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    first_registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "deferred-turn-start",
            state: state.clone(),
            wait_for_release: true,
            cancel_on_prepare: None,
        }))
        .expect("首个启动 Hook 应成功注册");
    let first_runtime = HookRuntime::new(first_registry, HookLimits::default())
        .expect("首个 Hook Runtime 配置应有效");
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("late-start-session").expect("Session 标识有效"),
            turn_id: TurnId::new("late-start-turn-1").expect("Turn 标识有效"),
            source_agent_id: AgentId::new("root").expect("Agent 标识有效"),
        },
        prompt: "首次启动".to_owned(),
        has_history: false,
    };
    let cancellation = TurnCancellation::new();
    let task_cancellation = cancellation.clone();
    let first_task = tokio::spawn(async move {
        first_runtime
            .run_turn_start(context, &task_cancellation)
            .await
    });
    state.started.notified().await;
    cancellation.cancel();
    let first_error = match first_task.await.expect("取消的启动 Hook 任务应退出") {
        Ok(_) => panic!("取消应阻止启动 Hook 交付"),
        Err(error) => error,
    };
    assert!(matches!(first_error, HookError::Cancelled { .. }));
    assert_eq!(state.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(state.delivered.load(Ordering::SeqCst), 0);

    let mut reloaded_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    reloaded_registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "deferred-turn-start",
            state: state.clone(),
            wait_for_release: false,
            cancel_on_prepare: None,
        }))
        .expect("重载候选启动 Hook 应成功注册");
    let reloaded_runtime = HookRuntime::new(reloaded_registry, HookLimits::default())
        .expect("重载候选 Hook Runtime 配置应有效");
    let reloaded_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("late-start-session").expect("Session 标识有效"),
            turn_id: TurnId::new("late-start-turn-2").expect("Turn 标识有效"),
            source_agent_id: AgentId::new("root").expect("Agent 标识有效"),
        },
        prompt: "重载后重试".to_owned(),
        has_history: true,
    };
    reloaded_runtime
        .run_turn_start(reloaded_context, &TurnCancellation::new())
        .await
        .expect("重载候选应可重新执行启动 Hook");
    assert_eq!(state.delivered.load(Ordering::SeqCst), 1);

    state.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), state.finished.notified())
        .await
        .expect("旧 worker 应在释放后迟到完成");
    assert_eq!(state.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(state.aborted.load(Ordering::SeqCst), 1);
}

/// 外层启动 Future 被直接丢弃时，已经完成但尚未交付的 Hook 也必须回滚。
#[tokio::test]
async fn 外层启动future直接丢弃会回滚全部待交付状态() {
    let first_state = Arc::new(DeferredTurnStartState {
        started: Arc::new(Notify::new()),
        finished: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        delivered: AtomicUsize::new(0),
        aborted: AtomicUsize::new(0),
    });
    let second_state = Arc::new(DeferredTurnStartState {
        started: Arc::new(Notify::new()),
        finished: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        delivered: AtomicUsize::new(0),
        aborted: AtomicUsize::new(0),
    });
    let mut registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "first-deferred-turn-start",
            state: first_state.clone(),
            wait_for_release: false,
            cancel_on_prepare: None,
        }))
        .expect("首个启动 Hook 应成功注册");
    registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "second-deferred-turn-start",
            state: second_state.clone(),
            wait_for_release: true,
            cancel_on_prepare: None,
        }))
        .expect("第二个启动 Hook 应成功注册");
    let runtime =
        HookRuntime::new(registry, HookLimits::default()).expect("Hook Runtime 配置应有效");
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("drop-start-session").expect("Session 标识有效"),
            turn_id: TurnId::new("drop-start-turn").expect("Turn 标识有效"),
            source_agent_id: AgentId::new("root").expect("Agent 标识有效"),
        },
        prompt: "丢弃外层启动 Future".to_owned(),
        has_history: false,
    };
    let task_cancellation = TurnCancellation::new();
    let task =
        tokio::spawn(async move { runtime.run_turn_start(context, &task_cancellation).await });
    second_state.started.notified().await;
    task.abort();
    assert!(task.await.expect_err("外层任务应被直接取消").is_cancelled());

    assert_eq!(first_state.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(first_state.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(second_state.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(second_state.aborted.load(Ordering::SeqCst), 1);

    second_state.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), second_state.finished.notified())
        .await
        .expect("被丢弃的旧启动 worker 应最终返回");
    assert_eq!(first_state.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(second_state.delivered.load(Ordering::SeqCst), 0);
}

/// 前一个启动回调完成后发生取消时，完成状态仍必须等到最终交付才能提交。
#[tokio::test]
async fn 启动回调完成后取消仍然回滚未交付状态() {
    let first_state = Arc::new(DeferredTurnStartState {
        started: Arc::new(Notify::new()),
        finished: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        delivered: AtomicUsize::new(0),
        aborted: AtomicUsize::new(0),
    });
    let second_state = Arc::new(DeferredTurnStartState {
        started: Arc::new(Notify::new()),
        finished: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        delivered: AtomicUsize::new(0),
        aborted: AtomicUsize::new(0),
    });
    let cancellation = TurnCancellation::new();
    let mut registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "completed-before-cancel",
            state: first_state.clone(),
            wait_for_release: false,
            cancel_on_prepare: None,
        }))
        .expect("首个启动 Hook 应成功注册");
    registry
        .register(Arc::new(DeferredTurnStartHook {
            name: "cancel-on-next-prepare",
            state: second_state.clone(),
            wait_for_release: false,
            cancel_on_prepare: Some(cancellation.clone()),
        }))
        .expect("取消测试 Hook 应成功注册");
    let runtime =
        HookRuntime::new(registry, HookLimits::default()).expect("Hook Runtime 配置应有效");
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("completed-cancel-session").expect("Session 标识有效"),
            turn_id: TurnId::new("completed-cancel-turn").expect("Turn 标识有效"),
            source_agent_id: AgentId::new("root").expect("Agent 标识有效"),
        },
        prompt: "回调完成后取消".to_owned(),
        has_history: false,
    };
    let result = runtime.run_turn_start(context, &cancellation).await;
    assert!(matches!(result, Err(HookError::Cancelled { .. })));
    assert_eq!(first_state.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(first_state.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(second_state.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(second_state.aborted.load(Ordering::SeqCst), 1);
}

/// 单个 PostCompact 观察者失败不能阻止后续观察者收到同一压缩记录。
#[tokio::test]
async fn post_compact失败后继续通知剩余观察者() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PostCompactSequenceHook {
            name: "first-observer",
            calls: calls.clone(),
            outcome: Err(HookCallbackError::new(
                "observer_failed",
                "合成 PostCompact 失败",
            )),
        }))
        .expect("首个 PostCompact 观察 Hook 应成功注册");
    hooks
        .register(Arc::new(PostCompactSequenceHook {
            name: "second-observer",
            calls: calls.clone(),
            outcome: Ok(()),
        }))
        .expect("第二个 PostCompact 观察 Hook 应成功注册");
    let runtime = HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效");

    let result = runtime
        .run_post_compact(post_compact_context(), &TurnCancellation::new())
        .await;

    assert_eq!(
        result,
        Err(HookError::Callback {
            phase: HookPhase::PostCompact,
            hook_name: "first-observer".to_owned(),
            code: "observer_failed".to_owned(),
            message: "合成 PostCompact 失败".to_owned(),
        })
    );
    assert_eq!(
        calls.lock().expect("PostCompact 顺序锁不应损坏").as_slice(),
        ["first-observer", "second-observer"]
    );
}

/// 首个 PostCompact 观察者超时后仍应通知后续观察者，并在共享 Store 中保持熔断。
#[tokio::test]
async fn post_compact超时后继续通知且跨registry共享熔断() {
    let circuits = HookCircuitStore::new();
    let pending_calls = Arc::new(AtomicUsize::new(0));
    let observer_calls = Arc::new(Mutex::new(Vec::new()));
    let mut first_registry = HookRegistry::with_circuit_store(circuits.clone());
    first_registry
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostCompact,
            calls: pending_calls.clone(),
            started: None,
        }))
        .expect("首个 PostCompact 观察 Hook 应成功注册");
    first_registry
        .register(Arc::new(PostCompactSequenceHook {
            name: "second-observer",
            calls: observer_calls.clone(),
            outcome: Ok(()),
        }))
        .expect("第二个 PostCompact 观察 Hook 应成功注册");

    let mut second_registry = HookRegistry::with_circuit_store(circuits);
    second_registry
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostCompact,
            calls: pending_calls.clone(),
            started: None,
        }))
        .expect("跨 Registry 的 PostCompact 观察 Hook 应成功注册");

    let limits = HookLimits {
        max_stop_hook_rounds: 1,
        max_context_bytes: 1_024,
        max_callback_ms: 20,
    };
    let first_runtime = HookRuntime::new(first_registry, limits).expect("首个 Hook 配置应有效");
    let second_runtime = HookRuntime::new(second_registry, limits).expect("第二个 Hook 配置应有效");

    let first = first_runtime
        .run_post_compact(post_compact_context(), &TurnCancellation::new())
        .await;
    assert!(matches!(
        first,
        Err(HookError::TimedOut {
            phase: HookPhase::PostCompact,
            hook_name,
            maximum_ms: 20,
        }) if hook_name == "pending-hook"
    ));
    assert_eq!(pending_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        observer_calls
            .lock()
            .expect("PostCompact 顺序锁不应损坏")
            .as_slice(),
        ["second-observer"]
    );

    let second = second_runtime
        .run_post_compact(post_compact_context(), &TurnCancellation::new())
        .await;
    assert!(matches!(
        second,
        Err(HookError::CircuitOpen {
            phase: HookPhase::PostCompact,
            hook_name,
        }) if hook_name == "pending-hook"
    ));
    assert_eq!(pending_calls.load(Ordering::SeqCst), 1);
}

/// Stop Hook 连续要求继续时必须在配置轮次上限处稳定终止。
#[tokio::test]
async fn stop_hook达到轮次上限后失败() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()).with_stop(Ok(StopHookOutput {
        action: StopHookAction::Continue,
        context: vec![HookContextAddition::new("仍需继续")],
    })));
    let result = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [text_reply("candidate")],
        )),
        Arc::new(ProbeTool::new(events.clone(), false)),
        hook,
        HookLimits {
            max_stop_hook_rounds: 1,
            max_context_bytes: 1_024,
            max_callback_ms: 1_000,
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Hook(HookError::StopRoundsExceeded {
            maximum: 1,
        }))
    );
}

/// 超出 Hook 上下文字节预算时必须用固定失败替换结果且不提交超限上下文。
#[tokio::test]
async fn hook上下文超限保留配对工具结果() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()).with_post(Ok(ToolHookOutput {
        context: vec![HookContextAddition::new("超过预算的上下文")],
    })));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-budget",
            "probe",
            json!({"value": "read"}),
        )])],
    ));
    let result = runner(
        provider,
        Arc::new(ProbeTool::new(events.clone(), false)),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1,
            max_callback_ms: 1_000,
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Hook(HookError::ContextBytesExceeded {
            maximum: 1,
            ..
        }))
    ));
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-budget");
    assert!(results[0].is_error);
    assert!(matches!(
        results[0].content.as_slice(),
        [ToolResultContent::Text { text }]
            if text == crate::tool::TOOL_OUTPUT_LIMIT_RESULT
    ));
    assert!(!result.messages.iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::Text { text } => text.contains("超过预算的上下文"),
            _ => false,
        })
    }));
}

/// 顺序工具的 PostHook 上下文超限后必须立即阻止同批后续副作用。
#[tokio::test]
async fn post_hook预算超限不会执行同批后续副作用() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()).with_post(Ok(ToolHookOutput {
        context: vec![HookContextAddition::new("超过预算的 PostHook 上下文")],
    })));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("post-budget-read", "probe", json!({"value": "read"})),
            ("post-budget-write", "probe", json!({"value": "write"})),
        ])],
    ));

    let result = runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1,
            max_callback_ms: 1_000,
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Hook(HookError::ContextBytesExceeded {
            maximum: 1,
            ..
        }))
    ));
    assert_eq!(tool.calls(), vec![json!({"value": "read"})]);
    assert_eq!(result.state.step_count(), 1);
    let results = tool_results(&result);
    assert_eq!(results.len(), 2);
    assert_eq!(results[1].tool_call_id, "post-budget-write");
    assert!(results[1].is_error);
    assert!(!result.messages.iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::Text { text } => text.contains("超过预算的 PostHook 上下文"),
            _ => false,
        })
    }));
}

/// PreToolUse 上下文超限必须在任何工具副作用开始前原子阻止整个批次。
#[tokio::test]
async fn pre_hook上下文超限不启动工具() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Ok(PreToolUseOutput {
            action: PreToolUseAction::Allow,
            context: vec![HookContextAddition::new("超过预算的前置上下文")],
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-pre-budget",
            "probe",
            json!({"value": "write"}),
        )])],
    ));
    let result = runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1,
            max_callback_ms: 1_000,
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Hook(HookError::ContextBytesExceeded {
            maximum: 1,
            ..
        }))
    ));
    assert!(tool.calls().is_empty());
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-pre-budget");
    assert!(results[0].is_error);
}

/// Block 原因按完整 ToolResult 文本计入 Hook 预算，不能借包装前缀绕过上限。
#[tokio::test]
async fn pre_hook_block完整可见文本计入上下文预算() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let block_message = "预算内原文";
    let visible_text = format!("PreToolUse Hook 阻止了工具执行：{block_message}");
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Ok(PreToolUseOutput {
            action: PreToolUseAction::Block {
                message: block_message.to_owned(),
            },
            context: Vec::new(),
        })),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-block-budget",
            "probe",
            json!({"value": "write"}),
        )])],
    ));
    let result = runner(
        provider.clone(),
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: block_message.len(),
            max_callback_ms: 1_000,
        },
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Hook(HookError::ContextBytesExceeded {
            maximum: block_message.len(),
            attempted: visible_text.len(),
        }))
    );
    assert!(tool.calls().is_empty());
    assert_eq!(provider.requests().expect("请求快照可读取").len(), 1);
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-block-budget");
    assert!(results[0].is_error);
    assert_eq!(
        tool_result_text(results[0]),
        "Hook 输出超过上下文预算，工具未执行"
    );
    assert!(!tool_result_text(results[0]).contains(block_message));
}

/// PreToolUse 回调错误必须稳定分类，并为同批全部调用各保留一个结果。
#[tokio::test]
async fn pre_hook回调错误稳定分类并补齐同批结果() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Err(HookCallbackError::new(
            "policy_unavailable",
            "策略服务不可用",
        ))),
    );
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let result = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[
                ("call-error-1", "probe", json!({"value": "read"})),
                ("call-error-2", "probe", json!({"value": "read"})),
            ])],
        )),
        tool.clone(),
        hook,
        HookLimits::default(),
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Hook(HookError::Callback {
            phase: HookPhase::PreToolUse,
            hook_name: "probe-hook".to_owned(),
            code: "policy_unavailable".to_owned(),
            message: "策略服务不可用".to_owned(),
        }))
    );
    assert!(tool.calls().is_empty());
    let results = tool_results(&result);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-error-1");
    assert_eq!(results[1].tool_call_id, "call-error-2");
    assert!(results.iter().all(|result| result.is_error));
    assert_eq!(
        tool_result_text(results[0]),
        "PreToolUse Hook 失败，工具未执行"
    );
    assert!(!tool_result_text(results[0]).contains("策略服务不可用"));
}

/// Hook 主动错误的超长 UTF-8 字段必须安全截断且不得进入 ToolResult。
#[tokio::test]
async fn hook回调错误字段按utf8硬上限截断() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let original_message = "错".repeat(3_000);
    let hook = Arc::new(
        ProbeHook::new(events.clone()).with_pre(Err(HookCallbackError::new(
            "error-code",
            original_message.clone(),
        ))),
    );
    let result = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "call-long-hook-error",
                "probe",
                json!({"value": "read"}),
            )])],
        )),
        Arc::new(ProbeTool::new(events.clone(), false)),
        hook,
        HookLimits::default(),
    )
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    let message = match &result.error {
        Some(AgentRunError::Hook(HookError::Callback { message, .. })) => message,
        error => panic!("应返回有界 Hook 回调错误，实际为 {error:?}"),
    };
    assert!(message.len() <= 4 * 1_024);
    assert!(original_message.starts_with(message));
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(
        tool_result_text(results[0]),
        "PreToolUse Hook 失败，工具未执行"
    );
    assert!(!tool_result_text(results[0]).contains('错'));
}

/// Turn 取消已经启动的工具后仍必须实际调用 Failure Hook 并提交配对结果。
#[tokio::test]
async fn 工具取消调用failure_hook后以cancelled结束() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let started = Arc::new(Notify::new());
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(PendingTool {
            started: started.clone(),
        }))
        .expect("取消测试工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(hook.clone())
        .expect("取消测试 Hook 应成功注册");
    let limits = RunLimits::default()
        .with_tool_cancel_grace_ms(20)
        .expect("取消宽限窗口应有效");
    let runner = AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "call-cancel",
                "probe",
                json!({"value": "read"}),
            )])],
        )),
        tools,
        limits,
    )
    .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"));
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    started.notified().await;
    cancellation.cancel();
    let result = task.await.expect("取消测试任务不应 panic");

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(hook.post_count.load(Ordering::SeqCst), 0);
    assert_eq!(hook.failure_count.load(Ordering::SeqCst), 1);
    assert_eq!(hook.stop_count.load(Ordering::SeqCst), 0);
    assert!(hook.on_error_contexts().is_empty());
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-cancel");
    assert!(results[0].is_error);
}

/// 并行工具即使逆序完成，结果和 Hook 上下文也必须按模型原始顺序提交。
#[tokio::test]
async fn 并行工具结果和hook上下文按模型顺序提交() {
    let completed = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ParallelHook {
        completed: completed.clone(),
    });
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(ParallelTool))
        .expect("并行工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("并行 Hook 应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[
                ("call-slow", "parallel_probe", json!({"value": "slow"})),
                ("call-fast", "parallel_probe", json!({"value": "fast"})),
            ]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(
        *completed.lock().expect("并行完成顺序锁不应损坏"),
        vec!["fast", "slow"]
    );
    let results = tool_results(&result);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-slow");
    assert_eq!(results[1].tool_call_id, "call-fast");
    let requests = provider.requests().expect("模型请求快照可读取");
    let contexts = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } if text.contains("context:") => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contexts.len(), 2);
    assert!(contexts[0].contains("context:slow"));
    assert!(contexts[1].contains("context:fast"));
}

/// 成功调用同一工具和参数可以继续轮询，不能仅凭重复参数中断任务。
#[tokio::test]
async fn 连续相同成功工具调用不会熔断() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("loop-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("loop-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("loop-3", "probe", json!({"value": "read"}))]),
            tool_reply(&[("loop-4", "probe", json!({"value": "read"}))]),
            text_reply("done"),
        ],
    ));

    let result = runner(provider, tool.clone(), hook, HookLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(tool.calls().len(), 4);
    let results = tool_results(&result);
    assert_eq!(results.len(), 4);
    assert_eq!(results[3].tool_call_id, "loop-4");
    assert!(!results[3].is_error);
}

/// 第三次相同真实 ToolError 必须在提交该次结果后触发失败循环熔断。
#[tokio::test]
async fn 重复真实工具失败达到上限后熔断() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("失败循环工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("失败循环 Hook 应成功注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(3)
        .expect("循环上限应有效");
    let result = AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("fail-1", "probe", json!({"value": "read"}))]),
                tool_reply(&[("fail-2", "probe", json!({"value": "read"}))]),
                tool_reply(&[("fail-3", "probe", json!({"value": "read"}))]),
            ],
        )),
        tools,
        limits,
    )
    .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
    .run_turn(turn_request(PlanGuard::inactive()))
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolLoop {
            kind: ToolLoopKind::RepeatedFailure,
            tool_name: "probe".to_owned(),
            maximum: 3,
        })
    );
    assert_eq!(tool.calls().len(), 3);
    assert_eq!(tool_results(&result).len(), 3);
    assert!(tool_results(&result).iter().all(|item| item.is_error));
}

/// 提取一个模型请求中包含重复失败提醒标记的全部消息。
fn repeated_failure_reminder_messages(request: &ModelRequest) -> Vec<&Message> {
    request
        .messages
        .iter()
        .filter(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text } if text.contains("连续真实失败"))
            })
        })
        .collect()
}

/// 返回测试提醒消息的唯一文本内容。
fn reminder_text(message: &Message) -> &str {
    match message.content.as_slice() {
        [ContentBlock::Text { text }] => text,
        _ => panic!("测试提醒消息必须只包含一个文本块"),
    }
}

/// 返回每个模型请求中包含重复失败提醒标记的消息数量。
fn reminder_counts_per_request(requests: &[ModelRequest]) -> Vec<usize> {
    requests
        .iter()
        .map(|request| repeated_failure_reminder_messages(request).len())
        .collect()
}

/// 第三次相同真实失败必须注入恰好一条一次性运行时提醒并让循环继续。
#[tokio::test]
async fn 重复失败达到提醒阈值注入一次提醒并继续循环() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("失败循环工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("失败循环 Hook 应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("remind-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("remind-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("remind-3", "probe", json!({"value": "read"}))]),
            tool_reply(&[("remind-4", "probe", json!({"value": "read"}))]),
            text_reply("已改变方法完成"),
        ],
    ));
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(result.state.step_count(), 4);
    assert_eq!(tool.calls().len(), 4);
    let requests = provider.requests().expect("模型请求快照可读取");
    assert_eq!(requests.len(), 5);
    // 提醒消息会随 Transcript 累计出现在后续请求中；数量不增长即只注入了一次。
    assert_eq!(reminder_counts_per_request(&requests), vec![0, 0, 0, 1, 1]);
    let reminders = repeated_failure_reminder_messages(&requests[3]);
    let reminder = reminders[0];
    assert_eq!(reminder.role, MessageRole::User);
    assert!(reminder.is_meta);
    let text = reminder_text(reminder);
    assert!(text.contains("工具 probe 使用相同输入已连续真实失败 3 次"));
    assert!(text.contains("改变方法或调整参数"));
}

/// 第六次相同真实失败必须终止 Turn，且全程只注入一次运行时提醒。
#[tokio::test]
async fn 重复失败达到终止阈值终止且全程只提醒一次() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("失败循环工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("失败循环 Hook 应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("terminal-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("terminal-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("terminal-3", "probe", json!({"value": "read"}))]),
            tool_reply(&[("terminal-4", "probe", json!({"value": "read"}))]),
            tool_reply(&[("terminal-5", "probe", json!({"value": "read"}))]),
            tool_reply(&[("terminal-6", "probe", json!({"value": "read"}))]),
        ],
    ));
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolLoop {
            kind: ToolLoopKind::RepeatedFailure,
            tool_name: "probe".to_owned(),
            maximum: 6,
        })
    );
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::LimitReached)
    );
    assert_eq!(tool.calls().len(), 6);
    let requests = provider.requests().expect("模型请求快照可读取");
    assert_eq!(requests.len(), 6);
    // 最终请求中提醒数量仍为 1，证明第 4-6 次失败没有再次注入。
    assert_eq!(
        reminder_counts_per_request(&requests),
        vec![0, 0, 0, 1, 1, 1]
    );
}

/// 成功重置连续段后，相同指纹再次连续失败必须再次获得运行时提醒。
#[tokio::test]
async fn 成功重置后相同指纹再次连续失败再次提醒() {
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(SequencedTool {
            outcomes: Mutex::new(VecDeque::from([
                false, false, false, true, false, false, false,
            ])),
        }))
        .expect("序列工具应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("re-remind-1", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-2", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-3", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-4", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-5", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-6", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("re-remind-7", "sequence_probe", json!({"value": "same"}))]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.step_count(), 7);
    let requests = provider.requests().expect("模型请求快照可读取");
    assert_eq!(requests.len(), 8);
    // 两段连续失败各自注入一条提醒；成功重置后计数从零重新累计。
    assert_eq!(
        reminder_counts_per_request(&requests),
        vec![0, 0, 0, 1, 1, 1, 1, 2]
    );
}

/// 交替不同失败指纹必须持续重置连续段，既不提醒也不终止。
#[tokio::test]
async fn 交替不同失败指纹不提醒也不终止() {
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(CodedSequencedTool {
            outcomes: Mutex::new(VecDeque::from([
                CodedSequenceOutcome::Failure("failure-a"),
                CodedSequenceOutcome::Failure("failure-b"),
                CodedSequenceOutcome::Failure("failure-a"),
                CodedSequenceOutcome::Failure("failure-b"),
                CodedSequenceOutcome::Failure("failure-a"),
                CodedSequenceOutcome::Failure("failure-b"),
            ])),
        }))
        .expect("错误码序列工具应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "alternate-1",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "alternate-2",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "alternate-3",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "alternate-4",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "alternate-5",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "alternate-6",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.state.step_count(), 6);
    let requests = provider.requests().expect("模型请求快照可读取");
    assert!(
        reminder_counts_per_request(&requests)
            .iter()
            .all(|count| *count == 0)
    );
}

/// 运行时提醒必须保持 Hook 上下文一致的信任边界前缀与元消息语义。
#[tokio::test]
async fn 运行时提醒消息保持信任边界与元标记() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("失败循环工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("失败循环 Hook 应成功注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("boundary-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("boundary-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("boundary-3", "probe", json!({"value": "read"}))]),
            text_reply("已放弃并说明原因"),
        ],
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    let reminders = result
        .messages
        .iter()
        .filter(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text } if text.contains("连续真实失败"))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(reminders.len(), 1);
    let reminder = reminders[0];
    assert_eq!(reminder.role, MessageRole::User);
    assert!(reminder.is_meta);
    let text = reminder_text(reminder);
    assert!(text.starts_with("以下内容由 KeenCode Runtime 自动追加"));
    assert!(text.contains("而非用户指令"));
    assert!(text.contains("来源：KeenCode Agent Runtime / RepeatedToolFailure"));
    assert!(text.len() <= 64 * 1_024);
}

/// 顺序调用达到重复失败阈值后必须立即阻止同批后续副作用工具。
#[tokio::test]
async fn 重复失败熔断不会执行同批后续副作用() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(ProbeHook::new(events.clone()));
    let tool = Arc::new(ProbeTool::new(events.clone(), true));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("failure-stop-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("failure-stop-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[
                ("failure-stop-3", "probe", json!({"value": "read"})),
                ("must-not-write", "probe", json!({"value": "write"})),
            ]),
        ],
    ));

    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("失败循环工具应成功注册");
    let mut hooks = HookRegistry::new();
    hooks.register(hook).expect("失败循环 Hook 应成功注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(3)
        .expect("循环上限应有效");
    let result = AgentRunner::new(provider, tools, limits)
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolLoop {
            kind: ToolLoopKind::RepeatedFailure,
            maximum: 3,
            ..
        })
    ));
    assert_eq!(
        tool.calls(),
        vec![
            json!({"value": "read"}),
            json!({"value": "read"}),
            json!({"value": "read"}),
        ]
    );
    assert_eq!(result.state.step_count(), 3);
    let results = tool_results(&result);
    assert_eq!(results.len(), 4);
    assert_eq!(results[3].tool_call_id, "must-not-write");
    assert!(results[3].is_error);
}

/// 同工具和输入成功一次后，之前相同错误码的失败计数必须重新从零开始。
#[tokio::test]
async fn 工具成功重置对应重复失败计数() {
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(SequencedTool {
            outcomes: Mutex::new(VecDeque::from([false, false, true, false, false])),
        }))
        .expect("序列工具应成功注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(3)
        .expect("循环上限应有效");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("reset-1", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("reset-2", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("reset-3", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("reset-4", "sequence_probe", json!({"value": "same"}))]),
            tool_reply(&[("reset-5", "sequence_probe", json!({"value": "same"}))]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider, tools, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 5);
    assert_eq!(tool_results(&result).len(), 5);
}

/// 相同调用返回不同错误码时必须开启新的失败连续段，不能按错误码累计历史次数。
#[tokio::test]
async fn 不同失败指纹重置连续失败计数() {
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(CodedSequencedTool {
            outcomes: Mutex::new(VecDeque::from([
                CodedSequenceOutcome::Failure("failure-a"),
                CodedSequenceOutcome::Failure("failure-b"),
                CodedSequenceOutcome::Failure("failure-a"),
                CodedSequenceOutcome::Failure("failure-a"),
            ])),
        }))
        .expect("错误码序列工具应成功注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(3)
        .expect("连续失败上限应有效");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "code-reset-1",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "code-reset-2",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "code-reset-3",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            tool_reply(&[(
                "code-reset-4",
                "coded_sequence_probe",
                json!({"value": "same"}),
            )]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider, tools, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 4);
    assert_eq!(tool_results(&result).len(), 4);
}

/// 任意成功结果都必须终止之前的失败连续段，即使成功调用使用不同输入。
#[tokio::test]
async fn 不同调用成功重置全部连续失败计数() {
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(CodedSequencedTool {
            outcomes: Mutex::new(VecDeque::from([
                CodedSequenceOutcome::Failure("same-failure"),
                CodedSequenceOutcome::Success,
                CodedSequenceOutcome::Failure("same-failure"),
                CodedSequenceOutcome::Failure("same-failure"),
            ])),
        }))
        .expect("成功重置序列工具应成功注册");
    let limits = RunLimits::default()
        .with_repeated_failure_terminal_threshold(3)
        .expect("连续失败上限应有效");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "success-reset-1",
                "coded_sequence_probe",
                json!({"value": "target"}),
            )]),
            tool_reply(&[(
                "success-reset-2",
                "coded_sequence_probe",
                json!({"value": "different"}),
            )]),
            tool_reply(&[(
                "success-reset-3",
                "coded_sequence_probe",
                json!({"value": "target"}),
            )]),
            tool_reply(&[(
                "success-reset-4",
                "coded_sequence_probe",
                json!({"value": "target"}),
            )]),
            text_reply("done"),
        ],
    ));
    let result = AgentRunner::new(provider, tools, limits)
        .run_turn(turn_request(PlanGuard::inactive()))
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 4);
    assert_eq!(tool_results(&result).len(), 4);
}

/// PreToolUse 永久挂起时必须在配置的回调上限内失败且不得启动工具。
#[tokio::test]
async fn pre_hook永久挂起按硬超时终止() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PreToolUse,
            calls: calls.clone(),
            started: None,
        }))
        .expect("挂起 Hook 应成功注册");
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let mut tools = ToolRegistry::new();
    tools
        .register(tool.clone())
        .expect("PreToolUse 超时测试工具应成功注册");
    let runner = AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "call-pre-timeout",
                "probe",
                json!({"value": "write"}),
            )])],
        )),
        tools,
        RunLimits::default(),
    )
    .with_hook_runtime(
        HookRuntime::new(
            hooks,
            HookLimits {
                max_stop_hook_rounds: 2,
                max_context_bytes: 1_024,
                max_callback_ms: 20,
            },
        )
        .expect("Hook 超时配置应有效"),
    );

    let result = tokio::time::timeout(
        Duration::from_millis(500),
        runner.run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("PreToolUse 永久挂起必须有界结束");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        result.error,
        Some(AgentRunError::Hook(HookError::TimedOut {
            phase: HookPhase::PreToolUse,
            hook_name: "pending-hook".to_owned(),
            maximum_ms: 20,
        }))
    );
    assert!(tool.calls().is_empty());
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-pre-timeout");
    assert!(results[0].is_error);
}

/// 取消已经进入隔离线程的非协作 Hook 后必须永久熔断，避免后续 Turn 继续残留线程。
#[tokio::test]
async fn 取消中的pre_hook永久熔断且后续turn不再进入() {
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let hook = Arc::new(PendingPhaseHook {
        phase: HookPhase::PreToolUse,
        calls: calls.clone(),
        started: Some(started.clone()),
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-cancel-hook-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-cancel-hook-2", "probe", json!({"value": "read"}))]),
        ],
    ));
    let runner = Arc::new(runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 1_000,
        },
    ));
    let cancellation = TurnCancellation::new();
    let mut first_request = turn_request(PlanGuard::inactive());
    first_request.set_cancellation(cancellation.clone());
    let first_runner = runner.clone();
    let first_task = tokio::spawn(async move { first_runner.run_turn(first_request).await });

    started.notified().await;
    cancellation.cancel();
    let first = tokio::time::timeout(Duration::from_millis(100), first_task)
        .await
        .expect("取消中的 Hook 必须立即结束 Turn")
        .expect("取消测试任务不应 panic");
    assert_eq!(first.error, Some(AgentRunError::Cancelled));

    let second = tokio::time::timeout(
        Duration::from_millis(100),
        runner.run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("取消后熔断的 Hook 必须立即拒绝下一 Turn");
    assert_eq!(
        second.error,
        Some(AgentRunError::Hook(HookError::CircuitOpen {
            phase: HookPhase::PreToolUse,
            hook_name: "pending-hook".to_owned(),
        }))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(tool.calls().is_empty());
}

/// 同一 Hook 的并发等待者必须共用单入口，首个挂起线程熔断后不得再创建线程。
#[tokio::test]
async fn 并发post_hook最多残留一个隔离线程() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostToolUse,
            calls: calls.clone(),
            started: None,
        }))
        .expect("并发挂起 Hook 应成功注册");
    let runtime = HookRuntime::new(
        hooks,
        HookLimits {
            max_stop_hook_rounds: 1,
            max_context_bytes: 1_024,
            max_callback_ms: 20,
        },
    )
    .expect("并发 Hook 配置应有效");
    let context = PostToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("gate-session").expect("测试 Session 标识有效"),
            turn_id: TurnId::new("gate-turn").expect("测试 Turn 标识有效"),
            source_agent_id: AgentId::new("gate-agent").expect("测试 Agent 标识有效"),
        },
        tool_call_id: "gate-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
        result: ToolResult::text("gate-call", "ok", false),
        duration_ms: 7,
    };
    let cancellation = TurnCancellation::new();

    let (first, second) = tokio::join!(
        runtime.run_post_tool_use(context.clone(), &cancellation),
        runtime.run_post_tool_use(context, &cancellation),
    );
    let errors = [
        first.err().expect("首个挂起 Hook 应超时"),
        second.err().expect("等待者应被熔断拒绝"),
    ];

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(errors.iter().any(|error| matches!(
        error,
        HookError::TimedOut {
            phase: HookPhase::PostToolUse,
            maximum_ms: 20,
            ..
        }
    )));
    assert!(errors.iter().any(|error| matches!(
        error,
        HookError::CircuitOpen {
            phase: HookPhase::PostToolUse,
            ..
        }
    )));
}

/// 同一扩展候选为不同 Turn 构建 Registry 时仍必须共用单入口与熔断状态。
#[tokio::test]
async fn 候选级store跨registry共享单入口() {
    let circuits = HookCircuitStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut first_registry = HookRegistry::with_circuit_store(circuits.clone());
    first_registry
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostToolUse,
            calls: calls.clone(),
            started: None,
        }))
        .expect("首个候选 Hook 应成功注册");
    let mut second_registry = HookRegistry::with_circuit_store(circuits);
    second_registry
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostToolUse,
            calls: calls.clone(),
            started: None,
        }))
        .expect("第二个候选 Hook 应成功注册");
    let limits = HookLimits {
        max_stop_hook_rounds: 1,
        max_context_bytes: 1_024,
        max_callback_ms: 20,
    };
    let first_runtime = HookRuntime::new(first_registry, limits).expect("Hook 配置应有效");
    let second_runtime = HookRuntime::new(second_registry, limits).expect("Hook 配置应有效");
    let context = PostToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("shared-gate-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("shared-gate-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("shared-gate-agent").expect("Agent 标识应有效"),
        },
        tool_call_id: "shared-gate-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
        result: ToolResult::text("shared-gate-call", "ok", false),
        duration_ms: 7,
    };
    let cancellation = TurnCancellation::new();

    let (first, second) = tokio::join!(
        first_runtime.run_post_tool_use(context.clone(), &cancellation),
        second_runtime.run_post_tool_use(context, &cancellation),
    );
    let errors = [
        first.err().expect("首个跨 Registry Hook 应超时"),
        second.err().expect("第二个跨 Registry Hook 应被熔断拒绝"),
    ];

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(errors.iter().any(|error| matches!(
        error,
        HookError::TimedOut {
            phase: HookPhase::PostToolUse,
            maximum_ms: 20,
            ..
        }
    )));
    assert!(errors.iter().any(|error| matches!(
        error,
        HookError::CircuitOpen {
            phase: HookPhase::PostToolUse,
            ..
        }
    )));
}

/// 已确认退出的 panic worker 只能由同候选后续 Registry 串行恢复一次。
#[tokio::test]
async fn 候选级store跨registry共享恢复状态() {
    let circuits = HookCircuitStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::WorkerPanicOnce,
        calls: calls.clone(),
        delay_ms: 0,
    });
    let mut first_registry = HookRegistry::with_circuit_store(circuits.clone());
    first_registry
        .register(hook.clone())
        .expect("首个候选 Hook 应成功注册");
    let mut second_registry = HookRegistry::with_circuit_store(circuits);
    second_registry
        .register(hook)
        .expect("第二个候选 Hook 应成功注册");
    let first_runtime =
        HookRuntime::new(first_registry, HookLimits::default()).expect("Hook 配置应有效");
    let second_runtime =
        HookRuntime::new(second_registry, HookLimits::default()).expect("Hook 配置应有效");
    let context = PreToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("shared-recovery-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("shared-recovery-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("shared-recovery-agent").expect("Agent 标识应有效"),
        },
        tool_call_id: "shared-recovery-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
    };
    let cancellation = TurnCancellation::new();

    let first = first_runtime
        .run_pre_tool_use(context.clone(), &cancellation)
        .await;
    assert!(matches!(
        first,
        Err(HookError::WorkerFailed {
            phase: HookPhase::PreToolUse,
            ..
        })
    ));
    let during_backoff = second_runtime
        .run_pre_tool_use(context.clone(), &cancellation)
        .await;
    assert!(matches!(
        during_backoff,
        Err(HookError::CircuitOpen {
            phase: HookPhase::PreToolUse,
            ..
        })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::sleep(crate::hook::HOOK_WORKER_RECOVERY_BACKOFF + Duration::from_millis(20)).await;
    second_runtime
        .run_pre_tool_use(context, &cancellation)
        .await
        .expect("退避后同候选后续 Registry 应完成唯一恢复");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Hook 声明确实变化时必须丢弃旧代次熔断，同时保留共享 worker 容量边界。
#[tokio::test]
async fn hook声明变化重置永久熔断() {
    let old_circuits = HookCircuitStore::new();
    let old_calls = Arc::new(AtomicUsize::new(0));
    let old_hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::WorkerPanic,
        calls: old_calls.clone(),
        delay_ms: 0,
    });
    let mut first_registry = HookRegistry::with_circuit_store(old_circuits.clone());
    first_registry
        .register(old_hook.clone())
        .expect("旧候选首个 Hook 应成功注册");
    let mut recovery_registry = HookRegistry::with_circuit_store(old_circuits.clone());
    recovery_registry
        .register(old_hook.clone())
        .expect("旧候选恢复 Hook 应成功注册");
    let mut rejected_registry = HookRegistry::with_circuit_store(old_circuits.clone());
    rejected_registry
        .register(old_hook)
        .expect("旧候选拒绝 Hook 应成功注册");
    let first_runtime =
        HookRuntime::new(first_registry, HookLimits::default()).expect("Hook 配置应有效");
    let recovery_runtime =
        HookRuntime::new(recovery_registry, HookLimits::default()).expect("Hook 配置应有效");
    let rejected_runtime =
        HookRuntime::new(rejected_registry, HookLimits::default()).expect("Hook 配置应有效");
    let context = PreToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("reload-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("reload-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("reload-agent").expect("Agent 标识应有效"),
        },
        tool_call_id: "reload-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
    };
    let cancellation = TurnCancellation::new();

    assert!(matches!(
        first_runtime
            .run_pre_tool_use(context.clone(), &cancellation)
            .await,
        Err(HookError::WorkerFailed { .. })
    ));
    tokio::time::sleep(crate::hook::HOOK_WORKER_RECOVERY_BACKOFF + Duration::from_millis(20)).await;
    assert!(matches!(
        recovery_runtime
            .run_pre_tool_use(context.clone(), &cancellation)
            .await,
        Err(HookError::WorkerFailed { .. })
    ));
    assert!(matches!(
        rejected_runtime
            .run_pre_tool_use(context.clone(), &cancellation)
            .await,
        Err(HookError::CircuitOpen { .. })
    ));
    assert_eq!(old_calls.load(Ordering::SeqCst), 2);

    let new_calls = Arc::new(AtomicUsize::new(0));
    let changed_circuits = old_circuits.for_changed_hooks();
    let mut reloaded_registry = HookRegistry::with_circuit_store(changed_circuits);
    reloaded_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::TokioTimer,
            calls: new_calls.clone(),
            delay_ms: 1,
        }))
        .expect("新候选 Hook 应成功注册");
    HookRuntime::new(reloaded_registry, HookLimits::default())
        .expect("新候选 Hook 配置应有效")
        .run_pre_tool_use(context, &cancellation)
        .await
        .expect("Hook 声明变化后应重置旧代次永久熔断");
    assert_eq!(new_calls.load(Ordering::SeqCst), 1);
}

/// 未退出 worker 必须跨独立项目 Store 与 Hook 代次共享容量，无关候选不得绕过熔断。
#[tokio::test]
async fn 未退出worker跨独立store与hook代次受共享容量限制() {
    let worker_admission = HookWorkerAdmission::with_worker_limit(2);
    let first_project_circuits = HookCircuitStore::with_worker_admission(worker_admission.clone());
    let second_project_circuits = HookCircuitStore::with_worker_admission(worker_admission);
    let limits = HookLimits {
        max_stop_hook_rounds: 1,
        max_context_bytes: 1_024,
        max_callback_ms: 20,
    };
    let first_context = PreToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("first-project-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("first-project-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("first-project-agent").expect("Agent 标识应有效"),
        },
        tool_call_id: "first-project-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
    };
    let second_context = PreToolUseContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("second-project-session").expect("Session 标识应有效"),
            turn_id: TurnId::new("second-project-turn").expect("Turn 标识应有效"),
            source_agent_id: AgentId::new("second-project-agent").expect("Agent 标识应有效"),
        },
        tool_call_id: "second-project-call".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "read"}),
    };
    let cancellation = TurnCancellation::new();

    let first_pending_calls = Arc::new(AtomicUsize::new(0));
    let mut first_registry = HookRegistry::with_circuit_store(first_project_circuits.clone());
    first_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::Pending,
            calls: first_pending_calls.clone(),
            delay_ms: 0,
        }))
        .expect("首代挂起 Hook 应成功注册");
    let first_runtime = HookRuntime::new(first_registry, limits).expect("Hook 配置应有效");
    assert!(matches!(
        first_runtime
            .run_pre_tool_use(first_context.clone(), &cancellation)
            .await,
        Err(HookError::TimedOut {
            phase: HookPhase::PreToolUse,
            maximum_ms: 20,
            ..
        })
    ));
    assert_eq!(first_pending_calls.load(Ordering::SeqCst), 1);

    let unrelated_calls = Arc::new(AtomicUsize::new(0));
    let mut unrelated_registry = HookRegistry::with_circuit_store(first_project_circuits.clone());
    unrelated_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::TokioTimer,
            calls: unrelated_calls.clone(),
            delay_ms: 1,
        }))
        .expect("无关候选 Hook 应成功注册");
    let unrelated_runtime = HookRuntime::new(unrelated_registry, limits).expect("Hook 配置应有效");
    assert!(matches!(
        unrelated_runtime
            .run_pre_tool_use(first_context.clone(), &cancellation)
            .await,
        Err(HookError::CircuitOpen {
            phase: HookPhase::PreToolUse,
            ..
        })
    ));
    assert_eq!(unrelated_calls.load(Ordering::SeqCst), 0);

    let changed_circuits = first_project_circuits.for_changed_hooks();
    let changed_healthy_calls = Arc::new(AtomicUsize::new(0));
    let mut changed_registry = HookRegistry::with_circuit_store(changed_circuits.clone());
    changed_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::TokioTimer,
            calls: changed_healthy_calls.clone(),
            delay_ms: 1,
        }))
        .expect("变更后的健康 Hook 应成功注册");
    HookRuntime::new(changed_registry, limits)
        .expect("Hook 配置应有效")
        .run_pre_tool_use(first_context, &cancellation)
        .await
        .expect("真实 Hook 声明变化后应允许健康实现恢复");
    assert_eq!(changed_healthy_calls.load(Ordering::SeqCst), 1);

    let second_pending_calls = Arc::new(AtomicUsize::new(0));
    let mut second_pending_registry =
        HookRegistry::with_circuit_store(second_project_circuits.clone());
    second_pending_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::Pending,
            calls: second_pending_calls.clone(),
            delay_ms: 0,
        }))
        .expect("第二个挂起 Hook 应成功注册");
    assert!(matches!(
        HookRuntime::new(second_pending_registry, limits)
            .expect("Hook 配置应有效")
            .run_pre_tool_use(second_context.clone(), &cancellation)
            .await,
        Err(HookError::TimedOut {
            phase: HookPhase::PreToolUse,
            maximum_ms: 20,
            ..
        })
    ));
    assert_eq!(second_pending_calls.load(Ordering::SeqCst), 1);

    let capacity_calls = Arc::new(AtomicUsize::new(0));
    let mut capacity_registry =
        HookRegistry::with_circuit_store(second_project_circuits.for_changed_hooks());
    capacity_registry
        .register(Arc::new(IsolationProbeHook {
            mode: IsolationHookMode::TokioTimer,
            calls: capacity_calls.clone(),
            delay_ms: 1,
        }))
        .expect("容量拒绝测试 Hook 应成功注册");
    assert!(matches!(
        HookRuntime::new(capacity_registry, limits)
            .expect("Hook 配置应有效")
            .run_pre_tool_use(second_context, &cancellation)
            .await,
        Err(HookError::WorkerCapacityExceeded {
            phase: HookPhase::PreToolUse,
            maximum: 2,
            ..
        })
    ));
    assert_eq!(capacity_calls.load(Ordering::SeqCst), 0);
}

/// 已取消工具的 Failure Hook 不观察 Turn 取消，正常完成后不得误开熔断。
#[tokio::test]
async fn 已取消工具的failure_hook正常完成后不熔断() {
    let hook = Arc::new(ProbeHook::new(Arc::new(Mutex::new(Vec::new()))));
    let mut hooks = HookRegistry::new();
    hooks
        .register(hook.clone())
        .expect("Failure Hook 应成功注册");
    let runtime = HookRuntime::new(hooks, HookLimits::default()).expect("默认 HookLimits 应有效");
    let cancellation = TurnCancellation::new();
    cancellation.cancel();
    let context = PostToolUseFailureContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("failure-session").expect("测试 Session 标识有效"),
            turn_id: TurnId::new("failure-turn").expect("测试 Turn 标识有效"),
            source_agent_id: AgentId::new("failure-agent").expect("测试 Agent 标识有效"),
        },
        tool_call_id: "call-cancelled-failure".to_owned(),
        tool_name: "probe".to_owned(),
        input: json!({"value": "cancelled"}),
        result: ToolResult::text("call-cancelled-failure", "Turn 已取消", true),
        failure: ToolHookFailureKind::Cancelled,
        duration_ms: 7,
    };

    runtime
        .run_post_tool_use_failure(context.clone(), &cancellation)
        .await
        .expect("取消后的 Failure Hook 首次调用应正常完成");
    runtime
        .run_post_tool_use_failure(context, &cancellation)
        .await
        .expect("取消后的 Failure Hook 不应因取消状态误熔断");

    assert_eq!(hook.failure_count.load(Ordering::SeqCst), 2);
}

/// Hook 方法在返回 Future 前同步阻塞也必须按时结束 Turn，并永久熔断后续进入。
#[tokio::test]
async fn 同步阻塞hook超时后熔断且不再进入() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::SynchronousBlock,
        calls: calls.clone(),
        delay_ms: 200,
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-sync-timeout-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-sync-timeout-2", "probe", json!({"value": "read"}))]),
        ],
    ));
    let runner = runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 20,
        },
    );

    let first = tokio::time::timeout(
        Duration::from_millis(100),
        runner.run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("同步构造阻塞不能阻塞 Turn");
    assert_eq!(
        first.error,
        Some(AgentRunError::Hook(HookError::TimedOut {
            phase: HookPhase::PreToolUse,
            hook_name: "isolation-hook".to_owned(),
            maximum_ms: 20,
        }))
    );

    let second = tokio::time::timeout(
        Duration::from_millis(100),
        runner.run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("已熔断 Hook 必须立即返回");
    assert_eq!(
        second.error,
        Some(AgentRunError::Hook(HookError::CircuitOpen {
            phase: HookPhase::PreToolUse,
            hook_name: "isolation-hook".to_owned(),
        }))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(tool.calls().is_empty());
}

/// Future 的 poll 内不让出线程时也必须由隔离层在硬超时内结束 Turn。
#[tokio::test]
async fn 非协作future_poll不会阻塞turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::NonCooperativePoll,
        calls: calls.clone(),
        delay_ms: 200,
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let runner = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "call-poll-timeout",
                "probe",
                json!({"value": "read"}),
            )])],
        )),
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 20,
        },
    );

    let result = tokio::time::timeout(
        Duration::from_millis(100),
        runner.run_turn(turn_request(PlanGuard::inactive())),
    )
    .await
    .expect("非协作 Future poll 不能阻塞 Turn");

    assert!(matches!(
        result.error,
        Some(AgentRunError::Hook(HookError::TimedOut {
            phase: HookPhase::PreToolUse,
            maximum_ms: 20,
            ..
        }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(tool.calls().is_empty());
}

/// 隔离线程必须进入当前 Tokio Runtime，使正常异步 timer Hook 仍可完成。
#[tokio::test]
async fn 隔离hook保持tokio_timer能力() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::TokioTimer,
        calls: calls.clone(),
        delay_ms: 10,
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let runner = runner(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [
                tool_reply(&[("call-tokio-timer", "probe", json!({"value": "read"}))]),
                text_reply("done"),
            ],
        )),
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 200,
        },
    );

    let result = runner.run_turn(turn_request(PlanGuard::inactive())).await;

    assert!(result.is_success());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool.calls().len(), 1);
}

/// Hook 工作线程 panic 的当前调用稳定失败，退避前拒绝，安全边界后恢复一次。
#[tokio::test]
async fn hook工作线程失败后按需退避恢复() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::WorkerPanicOnce,
        calls: calls.clone(),
        delay_ms: 0,
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-panic-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-panic-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-panic-3", "probe", json!({"value": "read"}))]),
            text_reply("recovered"),
        ],
    ));
    let runner = runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 200,
        },
    );

    let first = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert_eq!(
        first.error,
        Some(AgentRunError::Hook(HookError::WorkerFailed {
            phase: HookPhase::PreToolUse,
            hook_name: "isolation-hook".to_owned(),
        }))
    );
    let second = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(matches!(
        second.error,
        Some(AgentRunError::Hook(HookError::CircuitOpen {
            phase: HookPhase::PreToolUse,
            ..
        }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tokio::time::sleep(crate::hook::HOOK_WORKER_RECOVERY_BACKOFF + Duration::from_millis(20)).await;

    let recovered = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(recovered.is_success(), "{:#?}", recovered.error);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(tool.calls().len(), 1);
}

/// 唯一恢复 worker 再次 panic 后永久熔断，后续调用不形成后台重启循环。
#[tokio::test]
async fn hook工作线程连续失败耗尽唯一恢复机会() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(IsolationProbeHook {
        mode: IsolationHookMode::WorkerPanic,
        calls: calls.clone(),
        delay_ms: 0,
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(ProbeTool::new(events, false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-repeat-panic-1", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-repeat-panic-2", "probe", json!({"value": "read"}))]),
            tool_reply(&[("call-repeat-panic-3", "probe", json!({"value": "read"}))]),
        ],
    ));
    let runner = runner(
        provider,
        tool.clone(),
        hook,
        HookLimits {
            max_stop_hook_rounds: 2,
            max_context_bytes: 1_024,
            max_callback_ms: 200,
        },
    );

    let first = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(matches!(
        first.error,
        Some(AgentRunError::Hook(HookError::WorkerFailed { .. }))
    ));
    tokio::time::sleep(crate::hook::HOOK_WORKER_RECOVERY_BACKOFF + Duration::from_millis(20)).await;
    let recovery = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(matches!(
        recovery.error,
        Some(AgentRunError::Hook(HookError::WorkerFailed { .. }))
    ));
    tokio::time::sleep(crate::hook::HOOK_WORKER_RECOVERY_BACKOFF + Duration::from_millis(20)).await;
    let permanently_open = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(matches!(
        permanently_open.error,
        Some(AgentRunError::Hook(HookError::CircuitOpen { .. }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(tool.calls().is_empty());
}

/// Turn 取消后 Failure Hook 永久挂起也必须受硬超时约束并保留取消终态。
#[tokio::test]
async fn 取消后failure_hook永久挂起仍有界结束() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PendingPhaseHook {
            phase: HookPhase::PostToolUseFailure,
            calls: calls.clone(),
            started: None,
        }))
        .expect("挂起 Failure Hook 应成功注册");
    let started = Arc::new(Notify::new());
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(PendingTool {
            started: started.clone(),
        }))
        .expect("取消超时测试工具应成功注册");
    let limits = RunLimits::default()
        .with_tool_cancel_grace_ms(20)
        .expect("取消清理窗口应有效");
    let runner = AgentRunner::new(
        Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [tool_reply(&[(
                "call-cancel-timeout",
                "probe",
                json!({"value": "read"}),
            )])],
        )),
        tools,
        limits,
    )
    .with_hook_runtime(
        HookRuntime::new(
            hooks,
            HookLimits {
                max_stop_hook_rounds: 2,
                max_context_bytes: 1_024,
                max_callback_ms: 20,
            },
        )
        .expect("Failure Hook 超时配置应有效"),
    );
    let cancellation = TurnCancellation::new();
    let mut request = turn_request(PlanGuard::inactive());
    request.set_cancellation(cancellation.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    started.notified().await;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .expect("取消后的 Failure Hook 永久挂起必须有界结束")
        .expect("取消超时测试任务不应 panic");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    let results = tool_results(&result);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-cancel-timeout");
    assert!(results[0].is_error);
}

/// Hook 回调超时上限为零时必须在 Runtime 构造阶段拒绝配置。
#[test]
fn hook回调超时上限不能为零() {
    let error = HookRuntime::new(
        HookRegistry::new(),
        HookLimits {
            max_stop_hook_rounds: 1,
            max_context_bytes: 1,
            max_callback_ms: 0,
        },
    )
    .err();

    assert_eq!(error, Some(HookLimitsError::ZeroCallbackTimeout));
}

/// Hook 名称必须满足固定长度和安全字符边界，且重复名称不能注册。
#[test]
fn hook注册名称执行严格边界校验() {
    let mut registry = HookRegistry::new();
    let empty = registry
        .register(Arc::new(NamedHook {
            name: "   ".to_owned(),
        }))
        .expect_err("纯空白 Hook 名称必须被拒绝");
    assert_eq!(empty, HookRegistrationError::EmptyName);

    let invalid = registry
        .register(Arc::new(NamedHook {
            name: "safe\nname".to_owned(),
        }))
        .expect_err("包含换行的 Hook 名称必须被拒绝");
    assert_eq!(
        invalid,
        HookRegistrationError::InvalidNameCharacter { byte_index: 4 }
    );

    let too_long = registry
        .register(Arc::new(NamedHook {
            name: "a".repeat(129),
        }))
        .expect_err("超长 Hook 名称必须被拒绝");
    assert_eq!(
        too_long,
        HookRegistrationError::NameTooLong {
            maximum_bytes: 128,
            actual_bytes: 129,
        }
    );

    registry
        .register(Arc::new(NamedHook {
            name: "检查器/check:v1".to_owned(),
        }))
        .expect("安全的中英文 Hook 名称应可注册");
    let duplicate = registry
        .register(Arc::new(NamedHook {
            name: "检查器/check:v1".to_owned(),
        }))
        .expect_err("重复 Hook 名称必须被拒绝");
    assert_eq!(
        duplicate,
        HookRegistrationError::DuplicateName {
            name: "检查器/check:v1".to_owned(),
        }
    );
}

/// Hook 实现注册后改变 name 返回值也不能改变 Runtime 使用的可信身份。
#[tokio::test]
async fn hook注册后冻结名称身份() {
    let hook = Arc::new(MutableNameHook {
        changed: AtomicUsize::new(0),
    });
    let mut registry = HookRegistry::new();
    registry
        .register(hook.clone())
        .expect("初始 Hook 名称应成功注册");
    hook.changed.store(1, Ordering::SeqCst);
    let runtime = HookRuntime::new(registry, HookLimits::default()).expect("Hook 配置应有效");

    let error = runtime
        .run_pre_tool_use(
            PreToolUseContext {
                invocation: HookInvocationContext {
                    session_id: SessionId::new("freeze-session").expect("Session 标识应有效"),
                    turn_id: TurnId::new("freeze-turn").expect("Turn 标识应有效"),
                    source_agent_id: AgentId::new("freeze-agent").expect("Agent 标识应有效"),
                },
                tool_call_id: "freeze-call".to_owned(),
                tool_name: "probe".to_owned(),
                input: json!({"value": "read"}),
            },
            &TurnCancellation::new(),
        )
        .await
        .err()
        .expect("测试 Hook 应返回固定错误");

    assert_eq!(
        error,
        HookError::Callback {
            phase: HookPhase::PreToolUse,
            hook_name: "frozen-hook".to_owned(),
            code: "expected_failure".to_owned(),
            message: "用于验证冻结名称".to_owned(),
        }
    );
}

/// 启动 Hook 上下文必须在首次模型采样前入账，且回合内只调用一次。
#[tokio::test]
async fn turn_start_context_reaches_first_model_request() {
    struct StartHook;
    impl AgentHook for StartHook {
        fn name(&self) -> &str {
            "test:start"
        }
        fn handles_turn_start(&self) -> bool {
            true
        }
        fn turn_start(
            &self,
            _context: TurnStartHookContext,
        ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
            Box::pin(async {
                Ok(ToolHookOutput {
                    context: vec![HookContextAddition::new("startup guidance")],
                })
            })
        }
    }
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("done")],
    ));
    let mut registry = HookRegistry::new();
    registry.register(Arc::new(StartHook)).unwrap();
    let runner = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
        .with_hook_runtime(HookRuntime::new(registry, HookLimits::default()).unwrap());
    let result = runner.run_turn(turn_request(PlanGuard::inactive())).await;
    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(result.messages.iter().filter(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text { text } if text.contains("startup guidance")))).count(), 1);
    let requests = provider.requests().unwrap();
    assert!(requests[0].messages.iter().any(|message| message.is_meta && message.content.iter().any(|block| matches!(block, ContentBlock::Text { text } if text.contains("startup guidance")))));
}
