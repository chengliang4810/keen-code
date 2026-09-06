//! Agent 实时事件出口的顺序、背压、取消与终止栅栏回归测试。

use std::collections::{HashMap, VecDeque};
use std::future::pending;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{Stream, stream};
use keencode_model::{
    ContentBlock, ImageContent, Message, MessageRole, ModelError, ModelFuture, ModelProvider,
    ModelRequest, ModelResponse, ModelStream, ModelStreamEvent, OpaqueReasoningState,
    ProviderCapabilities, ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
    TokenUsage, ToolCall, ToolDefinition, ToolResult, ToolResultContent, collect_model_stream,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::event::AgentToolRoundBinding;
use crate::runner::AgentToolRoundPermit;

use super::*;

/// 创建测试所需的非空 Session 标识。
fn test_session_id() -> SessionId {
    SessionId::new("stream-session").expect("测试 Session 标识应当有效")
}

/// 创建测试所需的非空 Turn 标识。
fn test_turn_id() -> TurnId {
    TurnId::new("stream-turn").expect("测试 Turn 标识应当有效")
}

/// 创建测试所需的非空 Agent 标识。
fn test_agent_id() -> AgentId {
    AgentId::new("stream-agent").expect("测试 Agent 标识应当有效")
}

/// 创建包含一个或多个完整工具调用的模型响应。
fn tool_reply(calls: &[(&str, &str, Value)]) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("生命周期测试工具数量应在 u32 范围内");
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

/// 创建正常结束的一段文本模型响应。
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

/// 创建一份由 Provider 明确报告的完整 Token 用量，供失败流记账测试复用。
fn reported_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: Some(17),
        output_tokens: Some(5),
        reasoning_tokens: Some(2),
        cache_read_tokens: Some(3),
        cache_write_tokens: Some(1),
        total_tokens: Some(22),
    }
}

/// 创建 Permit 白盒测试使用的单工具冻结 Assistant 消息。
fn frozen_tool_assistant(tool_call_id: &str) -> Message {
    Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            tool_call: ToolCall::new(tool_call_id, "LifecycleProbe", json!({ "value": "frozen" })),
        }],
    )
}

/// 创建 Permit 白盒测试使用的完整工具响应事实。
fn tool_round_completion() -> ModelRoundCompletion {
    ModelRoundCompletion {
        metadata: ResponseMetadata::default(),
        usage: TokenUsage::unknown(),
        stop_reason: StopReason::ToolUse,
    }
}

/// 创建使用固定可信身份和模型的最小 Turn 请求。
fn test_turn_request() -> TurnRequest {
    TurnRequest::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model",
        vec![Message::text(MessageRole::User, "验证实时事件")],
        PlanGuard::inactive(),
    )
}

/// 使用指定 Provider、实时 Sink、权威提交 Sink 和限制创建不包含工具的 Runner。
fn event_runner(
    provider: Arc<dyn ModelProvider>,
    event_sink: Arc<dyn AgentEventSink>,
    commit_sink: Arc<dyn AgentCommitSink>,
    limits: RunLimits,
) -> AgentRunner {
    AgentRunner::new(provider, ToolRegistry::new(), limits)
        .with_event_sink(event_sink)
        .with_commit_sink(commit_sink)
}

/// 不记录外部容量且由测试 Sink 显式返回的一次性预留。
struct TestToolRoundReservation;

impl AgentToolRoundReservation for TestToolRoundReservation {
    /// 消费测试预留，不执行额外动作。
    fn consume(self: Box<Self>) {}

    /// 释放测试预留，不执行额外动作。
    fn release(self: Box<Self>) {}

    /// 测试 Sink 不保留外部容量，仅显式接收不确定事件。
    fn retain_indeterminate(self: Box<Self>, _event: AgentCommitEvent) {}
}

/// 为不关注预留生命周期的测试 Sink 创建显式放行结果。
fn accepted_tool_round_reservation() -> Box<dyn AgentToolRoundReservation> {
    Box::new(TestToolRoundReservation)
}

/// 记录核心 Permit 包装层选择消费还是释放的测试计数器。
#[derive(Default)]
struct PermitLifecycleCounters {
    /// 匹配 Round 成功提交后消费预留的次数。
    consumed: AtomicUsize,
    /// 未完成匹配提交而显式释放预留的次数。
    released: AtomicUsize,
    /// 提交结果不确定时转入恢复保留的次数。
    retained: AtomicUsize,
    /// 转入恢复保留的完整权威事件。
    retained_events: Mutex<Vec<AgentCommitEvent>>,
}

/// 不实现 Drop、仅通过核心包装层回调记录结束方式的测试预留。
struct RecordingToolRoundReservation {
    /// 当前预留共享的结束方式计数器。
    counters: Arc<PermitLifecycleCounters>,
}

impl AgentToolRoundReservation for RecordingToolRoundReservation {
    /// 记录匹配 Round 已成功提交且预留只消费一次。
    fn consume(self: Box<Self>) {
        self.counters.consumed.fetch_add(1, Ordering::SeqCst);
    }

    /// 记录核心 Permit Drop 路径显式释放预留。
    fn release(self: Box<Self>) {
        self.counters.released.fetch_add(1, Ordering::SeqCst);
    }

    /// 记录无法确认提交结果时保留的恢复事件。
    fn retain_indeterminate(self: Box<Self>, event: AgentCommitEvent) {
        self.counters.retained.fetch_add(1, Ordering::SeqCst);
        self.counters
            .retained_events
            .lock()
            .expect("Permit 恢复事件锁不应损坏")
            .push(event);
    }
}

/// 为需要断言消费、释放或保留次数的测试创建记录型预留。
fn recording_tool_round_reservation(
    counters: Arc<PermitLifecycleCounters>,
) -> Box<dyn AgentToolRoundReservation> {
    Box::new(RecordingToolRoundReservation { counters })
}

/// 保存已确认事件并提供无轮询异步等待的测试 Sink。
#[derive(Default)]
struct RecordingSink {
    /// 按实时 Sink 确认顺序保存的模型事件信封。
    events: Mutex<Vec<AgentStreamEvent>>,
    /// 按同步确认顺序保存的工具 Round 预检候选。
    preflights: Mutex<Vec<AgentToolRoundPreflight>>,
    /// 按同步提交顺序保存的权威事件信封。
    commits: Mutex<Vec<AgentCommitEvent>>,
    /// 按实际调用顺序保存 Runner 提交的模型用量事实。
    usages: Mutex<Vec<ModelRoundUsage>>,
    /// 按实际调用先后保存预检与权威提交的统一时间线。
    authoritative: Mutex<Vec<AuthoritativeObservation>>,
    /// 记录工具 Round Permit 的消费与释放次数。
    permit_lifecycle: Arc<PermitLifecycleCounters>,
    /// 每次新增事件后唤醒等待指定数量的测试任务。
    changed: Notify,
}

impl RecordingSink {
    /// 返回已确认事件的独立快照。
    fn snapshot(&self) -> Vec<AgentStreamEvent> {
        self.events.lock().expect("事件测试锁不应损坏").clone()
    }

    /// 返回已同步确认权威事件的独立快照。
    fn commit_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.commits.lock().expect("权威事件测试锁不应损坏").clone()
    }

    /// 返回 Runner 已同步提交的模型用量事实快照。
    fn usages(&self) -> Vec<ModelRoundUsage> {
        self.usages.lock().expect("模型用量测试锁不应损坏").clone()
    }

    /// 返回已经同步确认的工具 Round 预检候选快照。
    fn preflight_snapshot(&self) -> Vec<AgentToolRoundPreflight> {
        self.preflights
            .lock()
            .expect("工具 Round 预检测试锁不应损坏")
            .clone()
    }

    /// 返回预检和权威提交共享的严格调用顺序。
    fn authoritative_snapshot(&self) -> Vec<AuthoritativeObservation> {
        self.authoritative
            .lock()
            .expect("权威调用时间线测试锁不应损坏")
            .clone()
    }

    /// 返回已经成功消费的工具 Round Permit 数量。
    fn consumed_permits(&self) -> usize {
        self.permit_lifecycle.consumed.load(Ordering::SeqCst)
    }

    /// 返回因提前退出而释放的工具 Round Permit 数量。
    fn released_permits(&self) -> usize {
        self.permit_lifecycle.released.load(Ordering::SeqCst)
    }

    /// 等待至少指定数量的事件被可靠接收。
    async fn wait_for_count(&self, expected: usize) {
        loop {
            let changed = self.changed.notified();
            if self.events.lock().expect("事件测试锁不应损坏").len() >= expected {
                return;
            }
            changed.await;
        }
    }
}

impl AgentEventSink for RecordingSink {
    /// 在返回前同步保存事件，模拟已经可靠进入 Session 投影队列。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        self.events
            .lock()
            .expect("事件测试锁不应损坏")
            .push(event.clone());
        self.changed.notify_waiters();
        Box::pin(async { Ok(()) })
    }
}

impl AgentCommitSink for RecordingSink {
    /// 在返回前保存模型用量，模拟 Runtime 已确认用量记账。
    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        self.usages
            .lock()
            .expect("模型用量测试锁不应损坏")
            .push(usage.clone());
        Ok(())
    }

    /// 在返回前保存工具 Round 预检候选，模拟持久层完成只读验证。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        self.preflights
            .lock()
            .expect("工具 Round 预检测试锁不应损坏")
            .push(round.clone());
        self.authoritative
            .lock()
            .expect("权威调用时间线测试锁不应损坏")
            .push(AuthoritativeObservation::Preflight(Box::new(round.clone())));
        Ok(recording_tool_round_reservation(
            self.permit_lifecycle.clone(),
        ))
    }

    /// 在返回前同步保存权威事件，模拟已经持久提交到 Session 日志。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.commits
            .lock()
            .expect("权威事件测试锁不应损坏")
            .push(event.clone());
        self.authoritative
            .lock()
            .expect("权威调用时间线测试锁不应损坏")
            .push(AuthoritativeObservation::Commit(Box::new(event.clone())));
        Ok(())
    }
}

/// 动态输入两阶段提交测试共享的持久状态探针。
#[derive(Default)]
struct DynamicInputProbe {
    /// Transcript 已可靠保存动态消息后置为真。
    persisted: AtomicBool,
    /// 外部持久 claim 已被成功确认后置为真。
    acknowledged: AtomicBool,
    /// 确认回执的总调用次数。
    acknowledgement_attempts: AtomicUsize,
    /// 确认发生在 Transcript 提交前时记录协议违规。
    acknowledged_before_persist: AtomicBool,
    /// 确认回执在成功前仍需返回的失败次数。
    acknowledgement_failures_remaining: AtomicUsize,
}

/// 仅在指定安全边界开始返回同一个未确认动态输入批次的测试 Source。
struct OneShotDynamicInputSource {
    /// 第几次 claim 开始暴露测试消息，序号从一开始。
    delivery_claim: usize,
    /// 尚未确认时反复返回的冻结消息。
    message: Message,
    /// 两阶段提交状态探针。
    probe: Arc<DynamicInputProbe>,
    /// 当前已收到的 claim 次数。
    claims: AtomicUsize,
}

impl OneShotDynamicInputSource {
    /// 创建在指定 claim 序号开始投递消息的 Source。
    fn new(delivery_claim: usize, message: Message, probe: Arc<DynamicInputProbe>) -> Self {
        Self {
            delivery_claim,
            message,
            probe,
            claims: AtomicUsize::new(0),
        }
    }
}

impl AgentDynamicInputSource for OneShotDynamicInputSource {
    /// 在到达指定边界且回执尚未成功时返回同一持久 claim。
    fn claim(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _source_agent_id: &AgentId,
        _boundary: AgentDynamicInputBoundary,
        _maximum: usize,
    ) -> Result<AgentDynamicInputBatch, AgentDynamicInputError> {
        let claim = self.claims.fetch_add(1, Ordering::SeqCst) + 1;
        if claim < self.delivery_claim || self.probe.acknowledged.load(Ordering::SeqCst) {
            return Ok(AgentDynamicInputBatch::empty());
        }
        Ok(AgentDynamicInputBatch::new(
            vec![self.message.clone()],
            Arc::new(DynamicInputProbeAcknowledgement {
                probe: self.probe.clone(),
            }),
        ))
    }
}

/// 检查 Transcript 先于外部 claim 确认完成的测试回执。
struct DynamicInputProbeAcknowledgement {
    /// 两阶段提交状态探针。
    probe: Arc<DynamicInputProbe>,
}

impl AgentDynamicInputAcknowledgement for DynamicInputProbeAcknowledgement {
    /// 按预设次数失败，并拒绝把提交前确认伪装为成功。
    fn acknowledge(&self) -> Result<(), AgentDynamicInputError> {
        self.probe
            .acknowledgement_attempts
            .fetch_add(1, Ordering::SeqCst);
        if !self.probe.persisted.load(Ordering::SeqCst) {
            self.probe
                .acknowledged_before_persist
                .store(true, Ordering::SeqCst);
            return Err(AgentDynamicInputError::new("Transcript 尚未提交动态输入"));
        }
        if self
            .probe
            .acknowledgement_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(AgentDynamicInputError::new("模拟动态输入确认失败"));
        }
        self.probe.acknowledged.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 可选择拒绝动态 Round，并记录成功提交与全部尝试的测试 Sink。
struct DynamicInputCommitSink {
    /// 两阶段提交状态探针。
    probe: Arc<DynamicInputProbe>,
    /// 是否明确拒绝所有动态 Round 提交。
    reject_dynamic_round: bool,
    /// 按调用顺序保存的全部提交尝试。
    attempts: Mutex<Vec<AgentCommitEvent>>,
    /// 按调用顺序保存的成功提交。
    committed: Mutex<Vec<AgentCommitEvent>>,
}

impl DynamicInputCommitSink {
    /// 创建接受或拒绝动态 Round 的测试 Sink。
    fn new(probe: Arc<DynamicInputProbe>, reject_dynamic_round: bool) -> Self {
        Self {
            probe,
            reject_dynamic_round,
            attempts: Mutex::new(Vec::new()),
            committed: Mutex::new(Vec::new()),
        }
    }

    /// 返回全部提交尝试的独立快照。
    fn attempt_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.attempts
            .lock()
            .expect("动态输入提交尝试锁不应损坏")
            .clone()
    }

    /// 返回成功提交的独立快照。
    fn commit_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.committed
            .lock()
            .expect("动态输入成功提交锁不应损坏")
            .clone()
    }
}

impl AgentCommitSink for DynamicInputCommitSink {
    /// 动态输入测试不执行工具，意外预检仍返回显式测试 Permit。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(accepted_tool_round_reservation())
    }

    /// 只在动态 Round 成功提交后更新持久状态探针。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.attempts
            .lock()
            .expect("动态输入提交尝试锁不应损坏")
            .push(event.clone());
        if matches!(event.kind(), AgentCommitEventKind::RoundCommitted { .. })
            && self.reject_dynamic_round
        {
            return Err(AgentCommitSinkError::rejected("模拟动态输入提交失败"));
        }
        self.committed
            .lock()
            .expect("动态输入成功提交锁不应损坏")
            .push(event.clone());
        if matches!(event.kind(), AgentCommitEventKind::RoundCommitted { .. }) {
            self.probe.persisted.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// RecordingSink 观察到的同步预检或权威提交。
#[derive(Clone, Debug, PartialEq)]
enum AuthoritativeObservation {
    /// 工具 Round 的只读持久化预检。
    Preflight(Box<AgentToolRoundPreflight>),
    /// 已经同步确认的权威提交事件。
    Commit(Box<AgentCommitEvent>),
}

/// 按冻结 Assistant 消息编码字节数拒绝工具 Round 的测试 Sink。
struct BoundedPreflightSink {
    /// 允许的最大测试消息编码字节数。
    maximum_bytes: usize,
    /// 按实际调用顺序保存的全部预检候选。
    preflights: Mutex<Vec<AgentToolRoundPreflight>>,
    /// 预检通过后实际收到的权威提交。
    commits: Mutex<Vec<AgentCommitEvent>>,
}

impl BoundedPreflightSink {
    /// 创建使用指定消息编码上限的测试 Sink。
    fn new(maximum_bytes: usize) -> Self {
        Self {
            maximum_bytes,
            preflights: Mutex::new(Vec::new()),
            commits: Mutex::new(Vec::new()),
        }
    }

    /// 返回全部预检候选的独立快照。
    fn preflight_snapshot(&self) -> Vec<AgentToolRoundPreflight> {
        self.preflights
            .lock()
            .expect("有界预检 Sink 候选锁不应损坏")
            .clone()
    }

    /// 返回预检后收到的权威提交快照。
    fn commit_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.commits
            .lock()
            .expect("有界预检 Sink 提交锁不应损坏")
            .clone()
    }
}

impl AgentCommitSink for BoundedPreflightSink {
    /// 对完整冻结 Assistant 消息执行确定性 JSON 编码上限检查。
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        self.preflights
            .lock()
            .expect("有界预检 Sink 候选锁不应损坏")
            .push(round.clone());
        let actual = serde_json::to_vec(&(round.assistant_message(), round.pre_tool_context()))
            .expect("测试工具 Round 已知消息应可编码")
            .len();
        if actual > self.maximum_bytes {
            return Err(AgentToolRoundPreflightError::unpersistable(format!(
                "工具 Round 不可持久化：消息大小 {actual} 超过测试限制 {}",
                self.maximum_bytes
            )));
        }
        Ok(accepted_tool_round_reservation())
    }

    /// 保存所有通过预检后发生的权威提交。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.commits
            .lock()
            .expect("有界预检 Sink 提交锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

/// 每次工具 Round 预检都返回同一稳定错误的测试 Sink。
struct RejectingPreflightSink {
    /// 预检应返回且必须保留分类的安全错误。
    error: AgentToolRoundPreflightError,
    /// 实际进入预检的次数。
    preflight_count: AtomicUsize,
    /// 错误返回后不应收到的权威提交。
    commits: Mutex<Vec<AgentCommitEvent>>,
}

impl RejectingPreflightSink {
    /// 创建返回指定错误的工具 Round 预检 Sink。
    fn new(error: AgentToolRoundPreflightError) -> Self {
        Self {
            error,
            preflight_count: AtomicUsize::new(0),
            commits: Mutex::new(Vec::new()),
        }
    }

    /// 返回工具 Round 预检的实际调用次数。
    fn preflight_count(&self) -> usize {
        self.preflight_count.load(Ordering::SeqCst)
    }

    /// 返回预检失败后意外收到的权威提交快照。
    fn commit_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.commits
            .lock()
            .expect("拒绝预检 Sink 提交锁不应损坏")
            .clone()
    }
}

impl AgentCommitSink for RejectingPreflightSink {
    /// 记录调用并返回构造时冻结的分类错误。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        self.preflight_count.fetch_add(1, Ordering::SeqCst);
        Err(self.error.clone())
    }

    /// 保存预检失败后任何违反栅栏的权威提交。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.commits
            .lock()
            .expect("拒绝预检 Sink 提交锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

/// 同步阻塞工具 Round 预检直到测试线程显式释放的 Sink。
#[derive(Default)]
struct BlockingPreflightSink {
    /// 预检是否已经进入同步 Sink。
    entered: AtomicBool,
    /// 预检进入后唤醒测试任务。
    entered_notify: Notify,
    /// 测试是否已经允许预检返回。
    released: Mutex<bool>,
    /// 唤醒阻塞预检线程的条件变量。
    release: Condvar,
    /// 预检返回后实际收到的权威提交。
    commits: Mutex<Vec<AgentCommitEvent>>,
    /// 记录预检成功后 Permit 的消费或释放。
    permit_lifecycle: Arc<PermitLifecycleCounters>,
}

impl BlockingPreflightSink {
    /// 等待工具 Round 预检实际进入同步 Sink。
    async fn wait_until_entered(&self) {
        loop {
            let entered = self.entered_notify.notified();
            if self.entered.load(Ordering::Acquire) {
                return;
            }
            entered.await;
        }
    }

    /// 允许当前阻塞的预检返回成功。
    fn release(&self) {
        *self.released.lock().expect("预检释放锁不应损坏") = true;
        self.release.notify_all();
    }

    /// 返回预检完成后收到的权威提交快照。
    fn commit_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.commits
            .lock()
            .expect("阻塞预检 Sink 提交锁不应损坏")
            .clone()
    }

    /// 返回预检完成后因提前退出释放的 Permit 数量。
    fn released_permits(&self) -> usize {
        self.permit_lifecycle.released.load(Ordering::SeqCst)
    }
}

impl AgentCommitSink for BlockingPreflightSink {
    /// 通知测试后同步等待释放，模拟不可由 Turn 取消中断的持久层检查。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        self.entered.store(true, Ordering::Release);
        self.entered_notify.notify_waiters();
        let mut released = self.released.lock().expect("预检释放锁不应损坏");
        while !*released {
            released = self.release.wait(released).expect("预检释放锁不应损坏");
        }
        Ok(recording_tool_round_reservation(
            self.permit_lifecycle.clone(),
        ))
    }

    /// 保存预检成功后发生的权威提交。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.commits
            .lock()
            .expect("阻塞预检 Sink 提交锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

/// 生命周期测试可以定位的事件类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlledLifecycleClass {
    /// 工具请求事件。
    Requested,
    /// 工具执行起点事件。
    Started,
    /// 工具唯一终态事件。
    Completed,
}

/// 命中目标生命周期事件时执行的测试动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlledSinkAction {
    /// 同步阻塞权威提交，直到测试线程显式释放。
    Block,
    /// 立即拒绝目标事件且不把它放入已确认集合。
    Reject,
    /// 立即拒绝目标事件及后续内容完全相同的重投事件。
    RejectAlways,
    /// 对目标事件及后续内容完全相同的重投持续返回无法确认是否已提交。
    IndeterminateAlways,
}

/// 精确控制第 N 个生命周期事件并保存其他已确认事件的 Sink。
struct ControlledLifecycleSink {
    /// 需要控制的生命周期类别。
    target_class: ControlledLifecycleClass,
    /// 从一开始计算的同类事件序号。
    target_ordinal: usize,
    /// 命中目标事件时采用的阻塞或拒绝动作。
    action: ControlledSinkAction,
    /// 拒绝或不确定结果返回给 Runner 的测试诊断。
    error_message: String,
    /// 已尝试发送的工具请求数量。
    requested: AtomicUsize,
    /// 已尝试发送的工具执行起点数量。
    started: AtomicUsize,
    /// 已尝试发送的工具唯一终态数量。
    completed: AtomicUsize,
    /// 目标事件是否已经进入 Sink。
    target_entered: AtomicBool,
    /// 目标事件进入后唤醒测试任务。
    entered: Notify,
    /// 同步阻塞目标提交时保存测试线程是否已经允许返回。
    block_released: Mutex<bool>,
    /// 唤醒正在同步提交中等待的 Runner 线程。
    block_release: Condvar,
    /// 需要持续失败时保存首次命中的完整稳定事件。
    repeated_target: Mutex<Option<AgentCommitEvent>>,
    /// 非目标且可靠确认的事件集合。
    accepted: Mutex<Vec<AgentCommitEvent>>,
    /// 记录工具 Round Permit 的消费与释放次数。
    permit_lifecycle: Arc<PermitLifecycleCounters>,
}

impl ControlledLifecycleSink {
    /// 创建只控制指定类别和序号的测试 Sink。
    fn new(
        target_class: ControlledLifecycleClass,
        target_ordinal: usize,
        action: ControlledSinkAction,
    ) -> Self {
        let error_message = match action {
            ControlledSinkAction::IndeterminateAlways => {
                "生命周期测试 Sink 无法确认事件是否提交".to_owned()
            }
            ControlledSinkAction::Block
            | ControlledSinkAction::Reject
            | ControlledSinkAction::RejectAlways => "生命周期测试 Sink 拒绝事件".to_owned(),
        };
        Self {
            target_class,
            target_ordinal,
            action,
            error_message,
            requested: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            target_entered: AtomicBool::new(false),
            entered: Notify::new(),
            block_released: Mutex::new(false),
            block_release: Condvar::new(),
            repeated_target: Mutex::new(None),
            accepted: Mutex::new(Vec::new()),
            permit_lifecycle: Arc::new(PermitLifecycleCounters::default()),
        }
    }

    /// 覆盖目标事件失败时返回的诊断，用于验证组合错误仍执行 UTF-8 有界化。
    fn with_error_message(mut self, message: impl Into<String>) -> Self {
        self.error_message = message.into();
        self
    }

    /// 返回当前已由 Sink 可靠确认的事件快照。
    fn snapshot(&self) -> Vec<AgentCommitEvent> {
        self.accepted
            .lock()
            .expect("受控生命周期 Sink 锁不应损坏")
            .clone()
    }

    /// 返回成功提交匹配 Round 后消费的 Permit 数量。
    fn consumed_permits(&self) -> usize {
        self.permit_lifecycle.consumed.load(Ordering::SeqCst)
    }

    /// 返回未完成匹配 Round 而释放的 Permit 数量。
    fn released_permits(&self) -> usize {
        self.permit_lifecycle.released.load(Ordering::SeqCst)
    }

    /// 返回提交不确定后转入恢复保留的 Permit 数量。
    fn retained_permits(&self) -> usize {
        self.permit_lifecycle.retained.load(Ordering::SeqCst)
    }

    /// 返回与 Permit 一同转入恢复保留的完整事件。
    fn retained_events(&self) -> Vec<AgentCommitEvent> {
        self.permit_lifecycle
            .retained_events
            .lock()
            .expect("受控生命周期 Sink 恢复事件锁不应损坏")
            .clone()
    }

    /// 返回目标 ToolCompleted 事件进入 Sink 的总尝试次数。
    fn completed_attempts(&self) -> usize {
        self.completed.load(Ordering::SeqCst)
    }

    /// 返回需要持续失败时冻结用于识别重投的首个目标事件。
    fn repeated_target_snapshot(&self) -> Option<AgentCommitEvent> {
        self.repeated_target
            .lock()
            .expect("受控生命周期 Sink 重投目标锁不应损坏")
            .clone()
    }

    /// 等待目标事件实际进入 Sink。
    async fn wait_until_target_entered(&self) {
        loop {
            let entered = self.entered.notified();
            if self.target_entered.load(Ordering::Acquire) {
                return;
            }
            entered.await;
        }
    }

    /// 释放已经进入同步提交的目标事件。
    fn release_target(&self) {
        *self
            .block_released
            .lock()
            .expect("受控生命周期 Sink 释放锁不应损坏") = true;
        self.block_release.notify_all();
    }

    /// 计算受控类别事件各自独立的一基序号。
    fn classified_ordinal(
        &self,
        event: &AgentCommitEvent,
    ) -> Option<(ControlledLifecycleClass, usize)> {
        match event.kind() {
            AgentCommitEventKind::ToolRequested { .. } => Some((
                ControlledLifecycleClass::Requested,
                self.requested.fetch_add(1, Ordering::SeqCst) + 1,
            )),
            AgentCommitEventKind::ToolExecutionStarted { .. } => Some((
                ControlledLifecycleClass::Started,
                self.started.fetch_add(1, Ordering::SeqCst) + 1,
            )),
            AgentCommitEventKind::ToolCompleted { .. } => Some((
                ControlledLifecycleClass::Completed,
                self.completed.fetch_add(1, Ordering::SeqCst) + 1,
            )),
            AgentCommitEventKind::ContextCompactionApplied { .. }
            | AgentCommitEventKind::ModelRoundCommitted { .. }
            | AgentCommitEventKind::RoundCommitted { .. }
            | AgentCommitEventKind::DynamicInputCommitted { .. } => None,
        }
    }
}

impl AgentCommitSink for ControlledLifecycleSink {
    /// 生命周期测试只控制权威提交，工具 Round 预检始终立即通过。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(recording_tool_round_reservation(
            self.permit_lifecycle.clone(),
        ))
    }

    /// 对目标生命周期提交施加一次动作，其他权威事件立即可靠保存。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        let is_first_target = self
            .classified_ordinal(event)
            .is_some_and(|(class, ordinal)| {
                class == self.target_class && ordinal == self.target_ordinal
            });
        let repeats_target = matches!(
            self.action,
            ControlledSinkAction::RejectAlways | ControlledSinkAction::IndeterminateAlways
        );
        let is_repeated_target = repeats_target
            && self
                .repeated_target
                .lock()
                .expect("受控生命周期 Sink 重投目标锁不应损坏")
                .as_ref()
                .is_some_and(|target| target == event);
        let is_target = is_first_target || is_repeated_target;
        if is_target {
            if is_first_target && repeats_target {
                *self
                    .repeated_target
                    .lock()
                    .expect("受控生命周期 Sink 重投目标锁不应损坏") = Some(event.clone());
            }
            self.target_entered.store(true, Ordering::Release);
            self.entered.notify_waiters();
            return match self.action {
                ControlledSinkAction::Block => {
                    let mut released = self
                        .block_released
                        .lock()
                        .expect("受控生命周期 Sink 阻塞锁不应损坏");
                    while !*released {
                        released = self
                            .block_release
                            .wait(released)
                            .expect("受控生命周期 Sink 阻塞锁不应损坏");
                    }
                    drop(released);
                    self.accepted
                        .lock()
                        .expect("受控生命周期 Sink 锁不应损坏")
                        .push(event.clone());
                    Ok(())
                }
                ControlledSinkAction::Reject | ControlledSinkAction::RejectAlways => {
                    Err(AgentCommitSinkError::rejected(self.error_message.clone()))
                }
                ControlledSinkAction::IndeterminateAlways => Err(
                    AgentCommitSinkError::indeterminate(self.error_message.clone()),
                ),
            };
        }
        self.accepted
            .lock()
            .expect("受控生命周期 Sink 锁不应损坏")
            .push(event.clone());
        Ok(())
    }
}

impl AgentEventSink for ControlledLifecycleSink {
    /// 生命周期测试不控制实时模型事件，始终立即确认。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

/// 首个 ToolCompleted 已幂等追加却返回错误的测试 Sink。
#[derive(Default)]
struct AppendThenFailOnceSink {
    /// 按 Runtime 投递身份保存的唯一事件。
    stored: Mutex<HashMap<AgentEventId, AgentCommitEvent>>,
    /// 按实际投递顺序记录 ToolCompleted 身份。
    completed_attempt_ids: Mutex<Vec<AgentEventId>>,
    /// 首个 ToolCompleted 是否已在追加后返回过错误。
    completed_failed_once: AtomicBool,
}

impl AppendThenFailOnceSink {
    /// 返回实际投递过的 ToolCompleted 身份序列。
    fn completed_attempt_ids(&self) -> Vec<AgentEventId> {
        self.completed_attempt_ids
            .lock()
            .expect("幂等 Sink 终态投递锁不应损坏")
            .clone()
    }

    /// 返回幂等存储中唯一的 ToolCompleted 事件。
    fn stored_completions(&self) -> Vec<AgentCommitEvent> {
        self.stored
            .lock()
            .expect("幂等 Sink 事件锁不应损坏")
            .values()
            .filter(|event| matches!(event.kind(), AgentCommitEventKind::ToolCompleted { .. }))
            .cloned()
            .collect()
    }
}

impl AgentCommitSink for AppendThenFailOnceSink {
    /// 幂等提交测试不控制工具 Round 预检，始终立即通过。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(accepted_tool_round_reservation())
    }

    /// 先按投递身份幂等追加，再仅对首个 ToolCompleted 模拟确认失败。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        let storage_result = {
            let mut stored = self.stored.lock().expect("幂等 Sink 事件锁不应损坏");
            match stored.get(event.event_id()) {
                Some(existing) if existing != event => {
                    Err(AgentCommitSinkError::rejected("相同投递身份对应了不同事件"))
                }
                Some(_) => Ok(()),
                None => {
                    stored.insert(event.event_id().clone(), event.clone());
                    Ok(())
                }
            }
        };
        storage_result?;
        if matches!(event.kind(), AgentCommitEventKind::ToolCompleted { .. }) {
            self.completed_attempt_ids
                .lock()
                .expect("幂等 Sink 终态投递锁不应损坏")
                .push(event.event_id().clone());
            if !self.completed_failed_once.swap(true, Ordering::SeqCst) {
                return Err(AgentCommitSinkError::indeterminate(
                    "事件已追加但确认响应失败",
                ));
            }
        }
        Ok(())
    }
}

/// 按预设结果处理权威提交并记录同一事件重投行为的测试 Sink。
struct SequencedCommitSink {
    /// 每次进入权威提交时依次返回的确定结果。
    outcomes: Mutex<VecDeque<Result<(), AgentCommitSinkError>>>,
    /// 按实际调用顺序保存的完整权威事件。
    attempts: Mutex<Vec<AgentCommitEvent>>,
    /// 是否在返回预设结果前先按事件身份执行幂等追加。
    append_before_return: bool,
    /// 模拟已经按稳定身份写入持久层的唯一事件。
    stored: Mutex<HashMap<AgentEventId, AgentCommitEvent>>,
}

impl SequencedCommitSink {
    /// 创建使用指定提交结果序列和追加时机的测试 Sink。
    fn new(
        outcomes: impl IntoIterator<Item = Result<(), AgentCommitSinkError>>,
        append_before_return: bool,
    ) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            attempts: Mutex::new(Vec::new()),
            append_before_return,
            stored: Mutex::new(HashMap::new()),
        }
    }

    /// 返回每次提交收到的完整事件快照。
    fn attempt_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.attempts
            .lock()
            .expect("序列提交 Sink 调用锁不应损坏")
            .clone()
    }

    /// 返回模拟持久层最终保存的唯一事件快照。
    fn stored_snapshot(&self) -> Vec<AgentCommitEvent> {
        self.stored
            .lock()
            .expect("序列提交 Sink 存储锁不应损坏")
            .values()
            .cloned()
            .collect()
    }
}

impl AgentCommitSink for SequencedCommitSink {
    /// 序列提交测试由核心 Permit 直接持有预留，不通过该入口创建 Permit。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(accepted_tool_round_reservation())
    }

    /// 记录同一事件对象的重投，并可在返回不确定结果前模拟幂等追加。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        self.attempts
            .lock()
            .expect("序列提交 Sink 调用锁不应损坏")
            .push(event.clone());
        if self.append_before_return {
            let mut stored = self.stored.lock().expect("序列提交 Sink 存储锁不应损坏");
            match stored.get(event.event_id()) {
                Some(existing) if existing != event => {
                    return Err(AgentCommitSinkError::rejected(
                        "相同事件身份不能对应不同权威内容",
                    ));
                }
                Some(_) => {}
                None => {
                    stored.insert(event.event_id().clone(), event.clone());
                }
            }
        }
        self.outcomes
            .lock()
            .expect("序列提交 Sink 结果锁不应损坏")
            .pop_front()
            .expect("序列提交 Sink 不应收到超出预设次数的调用")
    }
}

/// 超长错误测试需要拒绝的权威事件类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OversizedCommitTarget {
    /// 拒绝 Transcript 段提交，覆盖普通权威提交路径。
    RoundCommitted,
    /// 拒绝工具唯一终态，覆盖有限重投路径。
    ToolCompleted,
}

/// 对指定权威事件返回超长多字节诊断并记录稳定事件身份的测试 Sink。
struct OversizedCommitErrorSink {
    /// 当前测试需要拒绝的权威事件类别。
    target: OversizedCommitTarget,
    /// 每次拒绝时原样返回给 Runner 的超长诊断。
    message: String,
    /// 按实际提交顺序保存目标事件身份。
    attempt_ids: Mutex<Vec<AgentEventId>>,
}

impl OversizedCommitErrorSink {
    /// 创建只拒绝指定权威事件类别的超长错误 Sink。
    fn new(target: OversizedCommitTarget, message: String) -> Self {
        Self {
            target,
            message,
            attempt_ids: Mutex::new(Vec::new()),
        }
    }

    /// 返回目标事件每次提交使用的稳定身份。
    fn attempt_ids(&self) -> Vec<AgentEventId> {
        self.attempt_ids
            .lock()
            .expect("超长错误 Sink 身份锁不应损坏")
            .clone()
    }
}

impl AgentCommitSink for OversizedCommitErrorSink {
    /// 超长权威提交错误测试不控制工具 Round 预检，始终立即通过。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(accepted_tool_round_reservation())
    }

    /// 非目标事件立即确认，目标事件返回同一超长多字节诊断。
    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        let is_target = match self.target {
            OversizedCommitTarget::RoundCommitted => is_round_commit(event),
            OversizedCommitTarget::ToolCompleted => {
                matches!(event.kind(), AgentCommitEventKind::ToolCompleted { .. })
            }
        };
        if !is_target {
            return Ok(());
        }
        self.attempt_ids
            .lock()
            .expect("超长错误 Sink 身份锁不应损坏")
            .push(event.event_id().clone());
        Err(AgentCommitSinkError::rejected(self.message.clone()))
    }
}

/// 在真实执行入口核验执行起点已经被 Sink 确认的状态变更工具。
struct LifecycleTool {
    /// 与 Runner 共用的可靠事件记录器。
    sink: Arc<RecordingSink>,
    /// 工具确实观察到执行起点的测试标记。
    observed_start: Arc<AtomicBool>,
}

impl AgentTool for LifecycleTool {
    /// 返回不接受额外字段的最小测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "LifecycleWrite",
            "验证工具生命周期事件顺序",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    /// 把测试工具固定分类为状态变更。
    fn effect(&self, _input: &serde_json::Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ChangesState)
    }

    /// 状态变更测试工具始终作为顺序屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 进入实现时断言执行起点事件已经可靠送达。
    fn execute(&self, context: ToolContext, _input: serde_json::Value) -> ToolFuture<'_> {
        let events = self.sink.commit_snapshot();
        assert!(matches!(
            events.last().map(AgentCommitEvent::kind),
            Some(AgentCommitEventKind::ToolExecutionStarted { tool_call_id })
                if tool_call_id == &context.tool_call_id
        ));
        self.observed_start.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(ToolOutput::text("生命周期工具完成")) })
    }
}

/// 可配置副作用和并发策略，并按输入制造确定完成顺序的生命周期测试工具。
struct LifecycleProbeTool {
    /// 每次调用固定返回的副作用分类。
    effect: ToolEffect,
    /// 每次调用固定返回的并发策略。
    concurrency: ToolConcurrency,
    /// 按真实进入工具实现的顺序记录 value。
    started: Arc<Mutex<Vec<String>>>,
    /// 按工具 Future 实际完成的顺序记录 value。
    completed: Arc<Mutex<Vec<String>>>,
}

impl LifecycleProbeTool {
    /// 创建使用指定副作用和并发策略的测试工具。
    fn new(effect: ToolEffect, concurrency: ToolConcurrency) -> Self {
        Self {
            effect,
            concurrency,
            started: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 返回真实进入工具实现的输入顺序。
    fn started(&self) -> Vec<String> {
        self.started
            .lock()
            .expect("生命周期工具启动顺序锁不应损坏")
            .clone()
    }

    /// 返回工具 Future 的实际完成顺序。
    fn completed(&self) -> Vec<String> {
        self.completed
            .lock()
            .expect("生命周期工具完成顺序锁不应损坏")
            .clone()
    }
}

impl AgentTool for LifecycleProbeTool {
    /// 返回只接受字符串 value 的固定工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "LifecycleProbe",
            "验证工具请求、执行和完成生命周期",
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    /// 返回构造测试工具时冻结的副作用分类。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    /// 返回构造测试工具时冻结的并发策略。
    fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    /// slow、medium、fast 输入依次缩短延迟，稳定制造逆序完成。
    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        let value = input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_owned();
        self.started
            .lock()
            .expect("生命周期工具启动顺序锁不应损坏")
            .push(value.clone());
        let completed = self.completed.clone();
        Box::pin(async move {
            if value == "pending" {
                return pending::<Result<ToolOutput, ToolError>>().await;
            }
            let delay_ms = match value.as_str() {
                "slow" => 60,
                "medium" => 30,
                _ => 0,
            };
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            completed
                .lock()
                .expect("生命周期工具完成顺序锁不应损坏")
                .push(value.clone());
            Ok(ToolOutput::text(format!("完成：{value}")))
        })
    }
}

/// 返回空图片地址以触发统一输出校验失败的测试工具。
struct InvalidOutputTool;

impl AgentTool for InvalidOutputTool {
    /// 返回不接受额外字段的最小测试工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "InvalidOutput",
            "验证无效工具输出的失败语义",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    /// 无效输出测试不产生外部副作用。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 单个无效输出调用作为顺序屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 返回不符合统一图片约束的结果，不包含任意底层错误文本。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        Box::pin(async {
            Ok(ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_url(""),
                }],
            })
        })
    }
}

/// 超限状态变更工具原始输出中不得进入事件、错误或 Transcript 的测试标记。
const OVERSIZED_SIDE_EFFECT_SECRET: &str = "raw-side-effect-output-must-not-leak";

/// 返回超过单文本块硬上限的状态变更工具，用于禁止自动重试回归。
struct OversizedSideEffectOutputTool;

impl AgentTool for OversizedSideEffectOutputTool {
    /// 返回不接受额外字段的固定工具定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "OversizedSideEffectOutput",
            "验证副作用工具输出超限后的固定失败语义",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    /// 明确声明工具执行可能改变项目内外状态。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ChangesState)
    }

    /// 副作用输出测试必须作为顺序屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 模拟副作用已经发生后返回超过单文本块硬上限的成功输出。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        Box::pin(async {
            Ok(ToolOutput::text(format!(
                "{OVERSIZED_SIDE_EFFECT_SECRET}{}",
                "s".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1)
            )))
        })
    }
}

/// 把工具 value 改写成指定文本的 PreToolUse 测试 Hook。
struct RewriteValuePreToolHook {
    /// 写入最终工具参数的完整 value。
    value: String,
}

/// 在 PreToolUse 阶段追加指定上下文的测试 Hook。
struct AddPreToolContextHook {
    /// 追加到下一模型 Round 且必须参与持久化预检的正文。
    text: String,
}

impl AgentHook for AddPreToolContextHook {
    /// 返回上下文合计预检测试使用的稳定 Hook 名称。
    fn name(&self) -> &str {
        "add-pre-tool-context-hook"
    }

    /// 放行工具并返回一条必须与 Assistant 消息共同预检的上下文。
    fn pre_tool_use(
        &self,
        _context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let text = self.text.clone();
        Box::pin(async move {
            Ok(PreToolUseOutput {
                action: PreToolUseAction::Allow,
                context: vec![HookContextAddition::new(text)],
            })
        })
    }
}

/// 为两个顺序工具返回唯一 PreTool 上下文，并让第二段 PostTool 上下文触发总预算溢出。
struct PreAndOversizedPostContextHook {
    /// 每个工具 PreToolUse 正文使用的稳定前缀。
    pre_text: String,
    /// 第一个工具在预算内成功计费、但最终应原子丢弃的 PostToolUse 正文。
    first_post_text: String,
    /// 第二个工具导致 PostTool 总预算超限且最终应丢弃的正文。
    oversized_post_text: String,
}

impl AgentHook for PreAndOversizedPostContextHook {
    /// 返回跨阶段上下文预算回归使用的稳定 Hook 名称。
    fn name(&self) -> &str {
        "pre-and-oversized-post-context-hook"
    }

    /// 放行工具并返回一条预算内的 PreToolUse 上下文。
    fn pre_tool_use(
        &self,
        context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let pre_text = format!("{}：{}", self.pre_text, context.tool_call_id);
        Box::pin(async move {
            Ok(PreToolUseOutput {
                action: PreToolUseAction::Allow,
                context: vec![HookContextAddition::new(pre_text)],
            })
        })
    }

    /// 首个工具返回预算内上下文，第二个工具返回导致累计预算溢出的上下文。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let post_text = if context.tool_call_id == "call-post-budget-first" {
            self.first_post_text.clone()
        } else {
            self.oversized_post_text.clone()
        };
        Box::pin(async move {
            Ok(ToolHookOutput {
                context: vec![HookContextAddition::new(post_text)],
            })
        })
    }
}

impl AgentHook for RewriteValuePreToolHook {
    /// 返回工具参数改写测试使用的稳定 Hook 名称。
    fn name(&self) -> &str {
        "rewrite-value-pre-tool-hook"
    }

    /// 返回已经标记 ModifyInput 的完整替换参数。
    fn pre_tool_use(
        &self,
        _context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let value = self.value.clone();
        Box::pin(async move {
            Ok(PreToolUseOutput {
                action: PreToolUseAction::ModifyInput {
                    input: json!({ "value": value }),
                },
                context: Vec::new(),
            })
        })
    }
}

/// 在 PostToolUse 入口通知测试并等待显式释放的 Hook。
struct GatedPostToolHook {
    /// PostHook 实际进入后唤醒断言任务。
    entered: Arc<Notify>,
    /// 测试确认完成事件后释放 PostHook。
    release: Arc<Notify>,
}

impl AgentHook for GatedPostToolHook {
    /// 返回生命周期门控 Hook 的稳定名称。
    fn name(&self) -> &str {
        "gated-post-tool-hook"
    }

    /// 通知已进入并等待测试显式释放。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(ToolHookOutput::default())
        })
    }
}

/// 每次 PostToolUse 都返回稳定回调错误的测试 Hook。
struct FailingPostToolHook;

impl AgentHook for FailingPostToolHook {
    /// 返回失败生命周期 Hook 的稳定名称。
    fn name(&self) -> &str {
        "failing-post-tool-hook"
    }

    /// 返回合成错误，验证已提交工具终态不会被改写。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        Box::pin(async {
            Err(HookCallbackError::new(
                "post_failed",
                "生命周期测试 PostHook 失败",
            ))
        })
    }
}

/// 每次 PostToolUse 都永久挂起的超时测试 Hook。
struct PendingPostToolHook;

impl AgentHook for PendingPostToolHook {
    /// 返回超时生命周期 Hook 的稳定名称。
    fn name(&self) -> &str {
        "pending-post-tool-hook"
    }

    /// 永久挂起，由 HookRuntime 的硬时限终止。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        Box::pin(pending())
    }
}

/// 记录可靠完成后实际运行 PostToolUse 的工具调用标识。
struct CountingPostToolHook {
    /// 按 Hook 进入顺序保存工具调用标识。
    calls: Arc<Mutex<Vec<String>>>,
}

/// 同时记录成功与失败分支，用于证明恢复冻结后没有运行任何 PostToolUse Hook。
struct CountingAnyPostToolHook {
    /// 成功或失败 PostToolUse Hook 的总进入次数。
    calls: Arc<AtomicUsize>,
}

/// 首次要求补充上下文并在第二次候选响应上停止的测试 Hook。
#[derive(Default)]
struct ContinueStopOnceHook {
    /// 已经进入 Stop Hook 的候选响应数量。
    calls: AtomicUsize,
}

impl AgentHook for ContinueStopOnceHook {
    /// 返回多段 Round 提交测试所需的稳定名称。
    fn name(&self) -> &str {
        "continue-stop-once-hook"
    }

    /// 首次追加用户级上下文，后续候选响应直接停止。
    fn stop(
        &self,
        _context: StopHookContext,
    ) -> HookFuture<'_, Result<StopHookOutput, HookCallbackError>> {
        let output = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            StopHookOutput {
                action: StopHookAction::Continue,
                context: vec![HookContextAddition::new("继续核验")],
            }
        } else {
            StopHookOutput::stop()
        };
        Box::pin(async move { Ok(output) })
    }
}

impl AgentHook for CountingPostToolHook {
    /// 返回完成投递失败测试 Hook 的稳定名称。
    fn name(&self) -> &str {
        "counting-post-tool-hook"
    }

    /// 记录工具调用标识并立即正常完成。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.calls
            .lock()
            .expect("PostHook 调用记录锁不应损坏")
            .push(context.tool_call_id);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }
}

impl AgentHook for CountingAnyPostToolHook {
    /// 返回覆盖成功与失败分支的稳定测试名称。
    fn name(&self) -> &str {
        "counting-any-post-tool-hook"
    }

    /// 记录成功工具对应的 PostToolUse Hook 进入。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }

    /// 记录失败或取消工具对应的 PostToolUseFailure Hook 进入。
    fn post_tool_use_failure(
        &self,
        _context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }
}

/// 记录无效输出失败 Hook，并在入口核验工具终态尚未提交。
struct InvalidOutputFailureHook {
    /// 与 Runner 共用的可靠事件记录器。
    sink: Arc<RecordingSink>,
    /// 按进入顺序保存失败 Hook 上下文。
    contexts: Arc<Mutex<Vec<PostToolUseFailureContext>>>,
}

impl AgentHook for InvalidOutputFailureHook {
    /// 返回无效输出回归 Hook 的稳定名称。
    fn name(&self) -> &str {
        "invalid-output-failure-hook"
    }

    /// 核验最终结果先进入失败 Hook，再由 Runner 提交唯一 Failed 终态。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        assert!(completed_tools(&self.sink.commit_snapshot()).is_empty());
        self.contexts
            .lock()
            .expect("无效输出 Hook 上下文锁不应损坏")
            .push(context);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }
}

/// 让成功 PostHook 超出模型可见预算，并记录随后失败 Hook 收到的最终结果。
struct OversizedPostThenRecordFailureHook {
    /// 按进入顺序保存容量失败后的 PostToolUseFailure 上下文。
    failure_contexts: Arc<Mutex<Vec<PostToolUseFailureContext>>>,
}

impl AgentHook for OversizedPostThenRecordFailureHook {
    /// 返回 PostHook 容量终态一致性回归使用的稳定名称。
    fn name(&self) -> &str {
        "oversized-post-then-record-failure-hook"
    }

    /// 返回必然超过固定 PostHook 模型可见字节上限的单条上下文。
    fn post_tool_use(
        &self,
        _context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
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
            .expect("PostHook 容量失败上下文锁不应损坏")
            .push(context);
        Box::pin(async { Ok(ToolHookOutput::default()) })
    }
}

/// 记录副作用输出超限结果，并让失败 Hook 自身返回错误。
struct FailingOutputLimitFailureHook {
    /// 保存失败 Hook 实际观察到的固定副作用超限结果。
    failure_contexts: Arc<Mutex<Vec<PostToolUseFailureContext>>>,
}

impl AgentHook for FailingOutputLimitFailureHook {
    /// 返回失败 Hook 不能覆盖副作用终态回归使用的稳定名称。
    fn name(&self) -> &str {
        "failing-output-limit-failure-hook"
    }

    /// 记录最终结果后返回普通 Hook 错误，验证更强 ToolOutputLimit 保持不变。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        self.failure_contexts
            .lock()
            .expect("副作用输出超限失败 Hook 上下文锁不应损坏")
            .push(context);
        Box::pin(async {
            Err(HookCallbackError::new(
                "failure_hook_failed",
                "副作用输出超限后的失败 Hook 合成错误",
            ))
        })
    }
}

/// 判断权威事件是否提交了模型 Round 或同 Round 的后续 Transcript 段。
fn is_round_commit(event: &AgentCommitEvent) -> bool {
    matches!(
        event.kind(),
        AgentCommitEventKind::ModelRoundCommitted { .. }
            | AgentCommitEventKind::RoundCommitted { .. }
    )
}

/// 返回模型 Round 或同 Round 后续 Transcript 段冻结的消息。
fn round_commit_messages(event: &AgentCommitEvent) -> Option<&[Message]> {
    match event.kind() {
        AgentCommitEventKind::ModelRoundCommitted { messages, .. }
        | AgentCommitEventKind::RoundCommitted { messages, .. } => Some(messages.as_slice()),
        _ => None,
    }
}

/// 从事件流中提取第一段包含工具结果的 Round 提交顺序。
fn committed_tool_result_ids(events: &[AgentCommitEvent]) -> Vec<String> {
    events
        .iter()
        .find_map(|event| {
            let messages = round_commit_messages(event)?;
            let ids = messages
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|block| match block {
                    ContentBlock::ToolResult { tool_result } => {
                        Some(tool_result.tool_call_id.clone())
                    }
                    ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::Image { .. }
                    | ContentBlock::ToolCall { .. } => None,
                })
                .collect::<Vec<_>>();
            (!ids.is_empty()).then_some(ids)
        })
        .unwrap_or_default()
}

/// 按确认顺序提取工具请求标识。
fn requested_tool_ids(events: &[AgentCommitEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event.kind() {
            AgentCommitEventKind::ToolRequested { tool_call_id, .. } => {
                Some(tool_call_id.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

/// 按提交顺序提取工具请求标识和原始模型位置。
fn requested_tool_indices(events: &[AgentCommitEvent]) -> Vec<(String, u32)> {
    events
        .iter()
        .filter_map(|event| match event.kind() {
            AgentCommitEventKind::ToolRequested {
                request_index,
                tool_call_id,
                ..
            } => Some((tool_call_id.as_str().to_owned(), *request_index)),
            _ => None,
        })
        .collect()
}

/// 按确认顺序提取工具执行起点标识。
fn started_tool_ids(events: &[AgentCommitEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event.kind() {
            AgentCommitEventKind::ToolExecutionStarted { tool_call_id } => {
                Some(tool_call_id.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

/// 按确认顺序提取工具终态标识和分类。
fn completed_tools(events: &[AgentCommitEvent]) -> Vec<(String, ToolCompletionStatus)> {
    events
        .iter()
        .filter_map(|event| match event.kind() {
            AgentCommitEventKind::ToolCompleted {
                tool_call_id,
                status,
                ..
            } => Some((tool_call_id.as_str().to_owned(), *status)),
            _ => None,
        })
        .collect()
}

/// 从队列逐次提供调用流，便于构造可控实时 Provider。
struct StreamQueueProvider {
    /// 当前模型能力快照。
    capabilities: ProviderCapabilities,
    /// 每次 `stream` 调用原子取出的唯一事件流。
    streams: Mutex<VecDeque<ModelStream>>,
}

impl StreamQueueProvider {
    /// 创建按模型调用顺序返回给 Runner 的流队列。
    fn new(
        capabilities: ProviderCapabilities,
        streams: impl IntoIterator<Item = ModelStream>,
    ) -> Self {
        Self {
            capabilities,
            streams: Mutex::new(streams.into_iter().collect()),
        }
    }
}

impl ModelProvider for StreamQueueProvider {
    /// 返回测试冻结的能力快照。
    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    /// 校验请求并按调用顺序取出唯一测试流。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let result = request.validate().and_then(|()| {
            self.streams
                .lock()
                .map_err(|_| ModelError::Protocol {
                    message: "事件测试 Provider 队列锁已损坏".to_owned(),
                })?
                .pop_front()
                .ok_or_else(|| ModelError::ProviderUnavailable {
                    message: "事件测试 Provider 没有剩余流".to_owned(),
                    status_code: None,
                    retryable: false,
                })
        });
        Box::pin(async move { result })
    }
}

/// 把指定 Round 已确认的原始 Provider 事件重新归约为完整响应。
async fn replay_round(events: &[AgentStreamEvent], model_round: u32) -> ModelResponse {
    let items = events
        .iter()
        .filter(|event| event.model_round() == model_round)
        .filter_map(|event| match event.kind() {
            AgentStreamEventKind::ModelEvent { event } => Some(Ok(event.clone())),
            AgentStreamEventKind::ModelFailure { .. }
            | AgentStreamEventKind::ContextCompactionStarted { .. }
            | AgentStreamEventKind::ContextCompactionFailed { .. } => None,
        })
        .collect::<Vec<Result<ModelStreamEvent, ModelError>>>();
    let model_stream: ModelStream = Box::pin(stream::iter(items));
    collect_model_stream(model_stream)
        .await
        .expect("成功 Round 的已确认事件应当可以精确重放")
}

/// 同一模型 Round 内多次提交相同内容也必须取得不同的 Runtime 投递身份。
#[test]
fn event_ids_distinguish_round_commits_in_same_model_round() {
    let kind = || AgentCommitEventKind::RoundCommitted {
        segment_index: 0,
        messages: vec![Message::text(MessageRole::User, "同 Round 事件")],
    };
    let first = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        kind(),
    );
    let second = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        kind(),
    );

    assert_ne!(first.event_id(), second.event_id());
    assert_eq!(first.model_round(), second.model_round());
    assert_eq!(first.kind(), second.kind());
}

/// Permit 必须拒绝错误 Round 身份，并由核心包装层显式释放未消费预留。
#[test]
fn tool_round_permit_rejects_mismatched_identity_and_releases_reservation() {
    let assistant = frozen_tool_assistant("call-identity");
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        Vec::new(),
    );
    let counters = Arc::new(PermitLifecycleCounters::default());
    let sink = Arc::new(SequencedCommitSink::new([Ok(())], false));
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let wrong_event = AgentCommitEvent::new(
        test_session_id(),
        TurnId::new("wrong-turn").expect("错误身份测试 Turn 应有效"),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tool_round_completion(),
            messages: vec![assistant],
        },
    );

    let error = permit
        .commit(wrong_event)
        .expect_err("错误 Round 身份不得使用 Permit 提交");

    assert!(matches!(
        error,
        AgentRunError::Internal { message }
            if message == "工具 Round Permit 与提交事件身份不匹配"
    ));
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 0);
    assert_eq!(counters.released.load(Ordering::SeqCst), 1);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 0);
    assert!(sink.attempt_snapshot().is_empty());
}

/// Permit 必须拒绝被篡改的 Provider 响应事实，并在进入 Sink 前释放未消费预留。
#[test]
fn tool_round_permit_rejects_tampered_completion_before_sink() {
    let assistant = frozen_tool_assistant("call-tampered-completion");
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        Vec::new(),
    );
    let counters = Arc::new(PermitLifecycleCounters::default());
    let sink = Arc::new(SequencedCommitSink::new([Ok(())], false));
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let mut tampered_completion = tool_round_completion();
    tampered_completion.stop_reason = StopReason::Completed;
    let wrong_event = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tampered_completion,
            messages: vec![
                assistant,
                Message::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResult::text("call-tampered-completion", "完成", false),
                    }],
                ),
            ],
        },
    );

    let error = permit
        .commit(wrong_event)
        .expect_err("被篡改的 Provider 响应事实不得进入权威 Sink");

    assert!(matches!(
        error,
        AgentRunError::Internal { message }
            if message == "工具 Round Permit 只能提交预检冻结的模型 Round"
    ));
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 0);
    assert_eq!(counters.released.load(Ordering::SeqCst), 1);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 0);
    assert!(sink.attempt_snapshot().is_empty());
}

/// Permit 必须在进入 Sink 前拒绝任何已冻结消息差异，并释放未消费预留。
#[test]
fn tool_round_permit_rejects_frozen_message_mismatch_before_sink() {
    let assistant = frozen_tool_assistant("call-frozen");
    let expected_pre = Message::text(MessageRole::User, "冻结的 PreToolUse 上下文");
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        vec![expected_pre],
    );
    let counters = Arc::new(PermitLifecycleCounters::default());
    let sink = Arc::new(SequencedCommitSink::new([Ok(())], false));
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let wrong_event = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tool_round_completion(),
            messages: vec![
                assistant,
                Message::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResult::text("call-frozen", "完成", false),
                    }],
                ),
                Message::text(MessageRole::User, "被篡改的 PreToolUse 上下文"),
            ],
        },
    );

    let error = permit
        .commit(wrong_event)
        .expect_err("冻结消息差异不得进入权威 Sink");

    assert!(matches!(
        error,
        AgentRunError::Internal { message }
            if message == "工具 Round Permit 与提交事件冻结正文不匹配"
    ));
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 0);
    assert_eq!(counters.released.load(Ordering::SeqCst), 1);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 0);
    assert!(sink.attempt_snapshot().is_empty());
}

/// Permit 必须在进入 Sink 前拒绝与冻结 ToolCall 未一一配对的 ToolResult 标识。
#[test]
fn tool_round_permit_rejects_wrong_tool_result_id_before_sink() {
    let assistant = frozen_tool_assistant("call-expected-result");
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        Vec::new(),
    );
    let counters = Arc::new(PermitLifecycleCounters::default());
    let sink = Arc::new(SequencedCommitSink::new([Ok(())], false));
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let wrong_event = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tool_round_completion(),
            messages: vec![
                assistant,
                Message::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResult::text("call-wrong-result", "完成", false),
                    }],
                ),
            ],
        },
    );

    let error = permit
        .commit(wrong_event)
        .expect_err("错误 ToolResult 标识不得进入权威 Sink");

    assert!(matches!(
        error,
        AgentRunError::Internal { message }
            if message == "工具 Round Permit 与提交事件冻结正文不匹配"
    ));
    assert!(sink.attempt_snapshot().is_empty());
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 0);
    assert_eq!(counters.released.load(Ordering::SeqCst), 1);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 0);
}

/// Round 首次已追加但确认不确定时，Permit 必须以同一身份重投并在确认后消费预留。
#[test]
fn round_commit_retry_after_indeterminate_reuses_event_and_consumes_reservation() {
    let assistant = frozen_tool_assistant("call-retry");
    let pre_context = vec![Message::text(MessageRole::User, "前置上下文")];
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        pre_context.clone(),
    );
    let sink = Arc::new(SequencedCommitSink::new(
        [
            Err(AgentCommitSinkError::indeterminate("事件已追加但确认丢失")),
            Ok(()),
        ],
        true,
    ));
    let counters = Arc::new(PermitLifecycleCounters::default());
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let event = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tool_round_completion(),
            messages: vec![
                assistant,
                Message::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResult::text("call-retry", "完成", false),
                    }],
                ),
                pre_context[0].clone(),
                Message::text(MessageRole::User, "后置上下文"),
            ],
        },
    );
    let expected_event = event.clone();

    permit
        .commit(event)
        .expect("第二次以相同身份确认后应提交成功");

    let attempts = sink.attempt_snapshot();
    assert_eq!(
        attempts,
        vec![expected_event.clone(), expected_event.clone()]
    );
    assert_eq!(attempts[0].event_id(), attempts[1].event_id());
    assert_eq!(sink.stored_snapshot(), vec![expected_event]);
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 1);
    assert_eq!(counters.released.load(Ordering::SeqCst), 0);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 0);
}

/// Round 连续不确定后必须保留原始事件供恢复，且不得消费或释放对应预留。
#[test]
fn repeated_indeterminate_round_commit_retains_event_and_reservation() {
    let assistant = frozen_tool_assistant("call-retain");
    let pre_context = vec![Message::text(MessageRole::User, "前置上下文")];
    let preflight = AgentToolRoundPreflight::new(
        AgentToolRoundBinding::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model".to_owned(),
            1,
            0,
        ),
        tool_round_completion(),
        assistant.clone(),
        pre_context.clone(),
    );
    let sink = Arc::new(SequencedCommitSink::new(
        [
            Err(AgentCommitSinkError::indeterminate("首次确认结果未知")),
            Err(AgentCommitSinkError::indeterminate("再次确认结果未知")),
        ],
        true,
    ));
    let counters = Arc::new(PermitLifecycleCounters::default());
    let permit = AgentToolRoundPermit::new(
        preflight,
        sink.clone(),
        recording_tool_round_reservation(counters.clone()),
    );
    let event = AgentCommitEvent::new(
        test_session_id(),
        test_turn_id(),
        test_agent_id(),
        "stream-model".to_owned(),
        1,
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            completion: tool_round_completion(),
            messages: vec![
                assistant,
                Message::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResult::text("call-retain", "完成", false),
                    }],
                ),
                pre_context[0].clone(),
            ],
        },
    );
    let expected_event = event.clone();

    let error = permit
        .commit(event)
        .expect_err("连续不确定提交必须进入恢复保留状态");

    assert!(matches!(
        error,
        AgentRunError::CommitSink(ref error)
            if error.kind() == AgentCommitSinkErrorKind::Indeterminate
                && error.message() == "再次确认结果未知"
    ));
    let attempts = sink.attempt_snapshot();
    assert_eq!(
        attempts,
        vec![expected_event.clone(), expected_event.clone()]
    );
    assert_eq!(attempts[0].event_id(), attempts[1].event_id());
    assert_eq!(counters.consumed.load(Ordering::SeqCst), 0);
    assert_eq!(counters.released.load(Ordering::SeqCst), 0);
    assert_eq!(counters.retained.load(Ordering::SeqCst), 1);
    assert_eq!(
        *counters
            .retained_events
            .lock()
            .expect("Permit 恢复事件锁不应损坏"),
        vec![expected_event]
    );
}

/// 普通权威事件一旦出现不确定结果，最终明确拒绝也不得把错误降级为 Rejected。
#[tokio::test]
async fn ordinary_commit_preserves_indeterminate_kind_across_rejected_retry() {
    let sink = Arc::new(SequencedCommitSink::new(
        [
            Err(AgentCommitSinkError::indeterminate("首次提交结果未知")),
            Err(AgentCommitSinkError::rejected("再次提交被明确拒绝")),
        ],
        false,
    ));
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("触发普通权威提交")],
    ));
    let result = AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::CommitSink(ref error))
            if error.kind() == AgentCommitSinkErrorKind::Indeterminate
                && error.message() == "首次提交结果未知"
    ));
    let attempts = sink.attempt_snapshot();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(attempts[0].event_id(), attempts[1].event_id());
}

/// 普通权威提交错误必须按 UTF-8 边界限制为 1024 字节且不泄漏诊断尾部。
#[tokio::test]
async fn ordinary_commit_error_is_utf8_bounded_without_tail_leakage() {
    let retained = "界".repeat(341);
    let secret_tail = "尾部机密不得泄漏";
    let sink = Arc::new(OversizedCommitErrorSink::new(
        OversizedCommitTarget::RoundCommitted,
        format!("{retained}{secret_tail}"),
    ));
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("触发 Transcript 提交")],
    ));
    let result = AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let error = match result.error.as_ref() {
        Some(AgentRunError::CommitSink(error)) => error,
        other => panic!("普通权威提交应返回有界 CommitSink 错误，实际为 {other:?}"),
    };
    assert_eq!(error.message(), retained);
    assert_eq!(error.message().len(), 1_023);
    assert!(!error.message().contains(secret_tail));
    let attempt_ids = sink.attempt_ids();
    assert_eq!(attempt_ids.len(), 2);
    assert_eq!(attempt_ids[0], attempt_ids[1]);
}

/// Provider 未结束时文本和 Usage 也必须实时、有序到达，并可重放为最终响应。
#[tokio::test]
async fn events_are_live_ordered_trusted_and_replayable() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let provider_stream: ModelStream = Box::pin(stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    }));
    let provider: Arc<dyn ModelProvider> = Arc::new(StreamQueueProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        },
        [provider_stream],
    ));
    let sink = Arc::new(RecordingSink::default());
    let runner = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default());
    let task = tokio::spawn(async move { runner.run_turn(test_turn_request()).await });

    sender
        .send(Ok(ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        }))
        .expect("测试模型流应当仍在接收");
    sender
        .send(Ok(ModelStreamEvent::TextDelta {
            index: 0,
            delta: "实".to_owned(),
        }))
        .expect("测试模型流应当仍在接收");
    sink.wait_for_count(2).await;
    assert!(!task.is_finished(), "MessageEnd 前 Turn 不应已经返回");

    let usage = TokenUsage {
        input_tokens: Some(7),
        output_tokens: Some(2),
        reasoning_tokens: Some(0),
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: Some(9),
    };
    sender
        .send(Ok(ModelStreamEvent::Usage {
            usage: usage.clone(),
        }))
        .expect("测试模型流应当仍在接收");
    sender
        .send(Ok(ModelStreamEvent::TextDelta {
            index: 0,
            delta: "时".to_owned(),
        }))
        .expect("测试模型流应当仍在接收");
    sender
        .send(Ok(ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        }))
        .expect("测试模型流应当仍在接收");
    let result = task.await.expect("实时事件测试任务不应 panic");
    assert!(result.is_success());

    let events = sink.snapshot();
    assert_eq!(events.len(), 5);
    for event in &events {
        assert_eq!(event.session_id(), &test_session_id());
        assert_eq!(event.turn_id(), &test_turn_id());
        assert_eq!(event.source_agent_id(), &test_agent_id());
        assert_eq!(event.model(), "stream-model");
        assert_eq!(event.model_round(), 1);
    }
    assert!(matches!(
        events[0].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageStart { .. }
        }
    ));
    assert!(matches!(
        events[1].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::TextDelta { delta, .. }
        } if delta == "实"
    ));
    assert!(matches!(
        events[2].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::Usage { usage: actual }
        } if actual == &usage
    ));
    assert!(matches!(
        events[4].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed
            }
        }
    ));
    let commits = sink.commit_snapshot();
    assert!(
        sink.preflight_snapshot().is_empty(),
        "无工具响应不应进入工具 Round 预检"
    );
    assert!(matches!(
        commits.as_slice(),
        [event]
            if matches!(
                event.kind(),
                AgentCommitEventKind::ModelRoundCommitted {
                    segment_index: 0,
                    messages
                    , ..
                } if matches!(messages.as_slice(), [Message { role: MessageRole::Assistant, .. }])
            )
    ));
    let replayed = replay_round(&events, 1).await;
    assert_eq!(result.final_response, Some(replayed));
}

/// 工具请求、执行起点、结果和 Round 提交必须形成可持久化的严格顺序。
#[tokio::test]
async fn tool_lifecycle_is_emitted_before_side_effect_and_round_commit() {
    let first_reply = ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: "call-lifecycle".to_owned(),
            name: "LifecycleWrite".to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: "call-lifecycle".to_owned(),
            delta: "{}".to_owned(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: "call-lifecycle".to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ]);
    let second_reply = ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: "已完成".to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [first_reply, second_reply],
    ));
    let sink = Arc::new(RecordingSink::default());
    let observed_start = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleTool {
            sink: sink.clone(),
            observed_start: observed_start.clone(),
        }))
        .expect("生命周期测试工具应注册");
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(TurnRequest::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model",
            vec![Message::text(MessageRole::User, "执行生命周期工具")],
            PlanGuard::inactive(),
        ))
        .await;

    assert!(result.is_success());
    assert!(observed_start.load(Ordering::SeqCst));
    let preflights = sink.preflight_snapshot();
    assert_eq!(preflights.len(), 1);
    assert_eq!(preflights[0].session_id(), &test_session_id());
    assert_eq!(preflights[0].turn_id(), &test_turn_id());
    assert_eq!(preflights[0].source_agent_id(), &test_agent_id());
    assert_eq!(preflights[0].model(), "stream-model");
    assert_eq!(preflights[0].model_round(), 1);
    assert_eq!(preflights[0].segment_index(), 0);
    let lifecycle = sink.commit_snapshot();
    assert_eq!(lifecycle.len(), 5);
    assert!(matches!(
        lifecycle[0].kind(),
        AgentCommitEventKind::ToolRequested {
            request_index: 0,
            tool_call_id,
            call,
            effect: ToolEffect::ChangesState,
        } if tool_call_id.as_str() == "call-lifecycle"
            && call.id == "call-lifecycle"
            && call.name == "LifecycleWrite"
            && call.arguments == json!({})
    ));
    assert!(matches!(
        lifecycle[1].kind(),
        AgentCommitEventKind::ToolExecutionStarted { tool_call_id }
            if tool_call_id.as_str() == "call-lifecycle"
    ));
    assert!(matches!(
        lifecycle[2].kind(),
        AgentCommitEventKind::ToolCompleted {
            tool_call_id,
            status: ToolCompletionStatus::Succeeded,
            result,
        } if tool_call_id.as_str() == "call-lifecycle"
            && result.tool_call_id == "call-lifecycle"
            && !result.is_error
    ));
    assert!(matches!(
        lifecycle[3].kind(),
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            messages,
            ..
        } if messages.len() == 2
    ));
    assert!(matches!(
        lifecycle[4].kind(),
        AgentCommitEventKind::ModelRoundCommitted {
            segment_index: 0,
            messages,
            ..
        } if messages.len() == 1
    ));
    let authoritative = sink.authoritative_snapshot();
    assert_eq!(authoritative.len(), 6);
    assert!(matches!(
        &authoritative[0],
        AuthoritativeObservation::Preflight(round)
            if round.as_ref() == &preflights[0]
    ));
    assert!(matches!(
        &authoritative[1],
        AuthoritativeObservation::Commit(event)
            if matches!(event.kind(), AgentCommitEventKind::ToolRequested { .. })
    ));
    assert!(matches!(
        &authoritative[2],
        AuthoritativeObservation::Commit(event)
            if matches!(event.kind(), AgentCommitEventKind::ToolExecutionStarted { .. })
    ));
    assert!(matches!(
        &authoritative[3],
        AuthoritativeObservation::Commit(event)
            if matches!(event.kind(), AgentCommitEventKind::ToolCompleted { .. })
    ));
    assert!(matches!(
        &authoritative[4],
        AuthoritativeObservation::Commit(event)
            if matches!(event.kind(), AgentCommitEventKind::ModelRoundCommitted { .. })
    ));
    assert!(matches!(
        &authoritative[5],
        AuthoritativeObservation::Commit(event)
            if matches!(event.kind(), AgentCommitEventKind::ModelRoundCommitted { .. })
    ));
    let AgentCommitEventKind::ModelRoundCommitted { messages, .. } = lifecycle[3].kind() else {
        panic!("首个 Round 应提交工具交换")
    };
    assert_eq!(preflights[0].assistant_message(), &messages[0]);
    assert_eq!(sink.consumed_permits(), 1);
    assert_eq!(sink.released_permits(), 0);
}

/// Plan 预检拒绝必须保留完整工具交换，但不能伪造任何真实工具生命周期。
#[tokio::test]
async fn plan_rejection_commits_error_pair_without_tool_lifecycle() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("call-plan-rejected", "LifecycleWrite", json!({}))]),
            text_reply("只读调研已完成"),
        ],
    ));
    let sink = Arc::new(RecordingSink::default());
    let observed_start = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleTool {
            sink: sink.clone(),
            observed_start: observed_start.clone(),
        }))
        .expect("Plan 生命周期测试工具应注册");

    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(TurnRequest::new(
            test_session_id(),
            test_turn_id(),
            test_agent_id(),
            "stream-model",
            vec![Message::text(MessageRole::User, "只读分析，不得写入")],
            PlanGuard::read_only(),
        ))
        .await;

    assert!(result.is_success());
    assert!(!observed_start.load(Ordering::SeqCst));
    assert_eq!(result.state.step_count(), 0);
    assert_eq!(
        sink.preflight_snapshot().len(),
        1,
        "Transcript 容量预留不属于工具执行生命周期"
    );
    let commits = sink.commit_snapshot();
    assert!(requested_tool_ids(&commits).is_empty());
    assert!(started_tool_ids(&commits).is_empty());
    assert!(completed_tools(&commits).is_empty());
    assert_eq!(sink.consumed_permits(), 1);
    assert_eq!(sink.released_permits(), 0);

    let messages = commits
        .iter()
        .find_map(|event| match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted { messages, .. }
                if messages.iter().any(|message| {
                    message.content.iter().any(|block| {
                        matches!(block, ContentBlock::ToolResult { tool_result }
                            if tool_result.tool_call_id == "call-plan-rejected")
                    })
                }) =>
            {
                Some(messages)
            }
            _ => None,
        })
        .expect("Plan 拒绝的完整工具交换必须进入 Transcript");
    assert!(matches!(
        messages.first().map(|message| message.content.as_slice()),
        Some([ContentBlock::ToolCall { tool_call }])
            if tool_call.id == "call-plan-rejected"
                && tool_call.name == "LifecycleWrite"
                && tool_call.arguments == json!({})
    ));
    assert!(matches!(
        messages.get(1).map(|message| message.content.as_slice()),
        Some([ContentBlock::ToolResult { tool_result }])
            if tool_result.tool_call_id == "call-plan-rejected" && tool_result.is_error
    ));
}

/// 超大推理续传状态必须在任何工具生命周期和真实副作用前被持久化预检拒绝。
#[tokio::test]
async fn oversized_reasoning_continuation_is_context_blocked_before_tool_side_effect() {
    let continuation = "续".repeat(2_048);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::ReasoningContinuation {
                index: 0,
                continuation: OpaqueReasoningState::new(
                    "oversized-test-state",
                    json!({ "encrypted": continuation }),
                ),
            },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: "call-oversized-reasoning".to_owned(),
                name: "LifecycleProbe".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: "call-oversized-reasoning".to_owned(),
                delta: json!({ "value": "must-not-run" }).to_string(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: "call-oversized-reasoning".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(BoundedPreflightSink::new(1_024));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolRoundPreflight(ref error))
            if error.kind() == AgentToolRoundPreflightErrorKind::Unpersistable
    ));
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ContextBlocked)
    );
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    let preflights = sink.preflight_snapshot();
    assert_eq!(preflights.len(), 1);
    assert!(matches!(
        preflights[0].assistant_message().content.as_slice(),
        [
            ContentBlock::Reasoning { .. },
            ContentBlock::ToolCall { .. }
        ]
    ));
}

/// 单个工具调用均可持久化时，完整 Assistant 批次仍必须按合计大小接受预检。
#[tokio::test]
async fn aggregate_tool_calls_are_rejected_when_combined_assistant_exceeds_limit() {
    let maximum_bytes = 1_024;
    let value = "v".repeat(650);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            (
                "call-aggregate-a",
                "LifecycleProbe",
                json!({ "value": value }),
            ),
            (
                "call-aggregate-b",
                "LifecycleProbe",
                json!({ "value": "v".repeat(650) }),
            ),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(BoundedPreflightSink::new(maximum_bytes));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolRoundPreflight(ref error))
            if error.kind() == AgentToolRoundPreflightErrorKind::Unpersistable
    ));
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ContextBlocked)
    );
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    let preflights = sink.preflight_snapshot();
    let round = preflights.first().expect("完整工具批次应进入一次预检");
    let empty_context = Vec::<Message>::new();
    for block in &round.assistant_message().content {
        let isolated = Message::new(MessageRole::Assistant, vec![block.clone()]);
        let isolated_bytes = serde_json::to_vec(&(&isolated, &empty_context))
            .expect("单个工具调用应可编码")
            .len();
        assert!(
            isolated_bytes <= maximum_bytes,
            "单个工具调用应小于预检上限，实际为 {isolated_bytes} 字节"
        );
    }
    let aggregate_bytes =
        serde_json::to_vec(&(round.assistant_message(), round.pre_tool_context()))
            .expect("完整工具批次应可编码")
            .len();
    assert!(aggregate_bytes > maximum_bytes);
}

/// PreToolUse 修改后的最终参数必须替换原始参数并参与副作用前持久化预检。
#[tokio::test]
async fn modified_pre_tool_input_is_frozen_before_persistability_preflight() {
    let modified_value = "改".repeat(512);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-modified-preflight",
            "LifecycleProbe",
            json!({ "value": "small-original" }),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(RewriteValuePreToolHook {
            value: modified_value.clone(),
        }))
        .expect("参数改写 Hook 应注册");
    let sink = Arc::new(BoundedPreflightSink::new(512));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolRoundPreflight(ref error))
            if error.kind() == AgentToolRoundPreflightErrorKind::Unpersistable
    ));
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ContextBlocked)
    );
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    let preflights = sink.preflight_snapshot();
    let round = preflights.first().expect("改写后的工具 Round 应进入预检");
    assert!(matches!(
        round.assistant_message().content.as_slice(),
        [ContentBlock::ToolCall { tool_call }]
            if tool_call.arguments == json!({ "value": modified_value })
    ));
}

/// Assistant 自身可持久化时，已知 PreToolUse 上下文仍必须计入同一预检候选。
#[tokio::test]
async fn assistant_and_pre_tool_context_are_checked_as_one_candidate() {
    let maximum_bytes = 1_024;
    let context_text = "上".repeat(512);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-context-preflight",
            "LifecycleProbe",
            json!({ "value": "small" }),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(AddPreToolContextHook {
            text: context_text.clone(),
        }))
        .expect("PreToolUse 上下文 Hook 应注册");
    let sink = Arc::new(BoundedPreflightSink::new(maximum_bytes));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::ToolRoundPreflight(ref error))
            if error.kind() == AgentToolRoundPreflightErrorKind::Unpersistable
    ));
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::ContextBlocked)
    );
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    let preflights = sink.preflight_snapshot();
    let round = preflights
        .first()
        .expect("包含 Hook 上下文的 Round 应进入预检");
    assert_eq!(round.pre_tool_context().len(), 1);
    assert!(matches!(
        round.pre_tool_context()[0].content.as_slice(),
        [ContentBlock::Text { text }] if text.ends_with(&context_text)
    ));
    let assistant_bytes = serde_json::to_vec(&(round.assistant_message(), Vec::<Message>::new()))
        .expect("Assistant 消息应可编码")
        .len();
    assert!(assistant_bytes <= maximum_bytes);
    let combined_bytes = serde_json::to_vec(&(round.assistant_message(), round.pre_tool_context()))
        .expect("Assistant 与 Hook 上下文应可编码")
        .len();
    assert!(combined_bytes > maximum_bytes);
}

/// PostToolUse 上下文超预算时只能丢弃 Post 内容，已预检 PreTool 内容必须精确保留。
#[tokio::test]
async fn post_tool_budget_failure_preserves_preflighted_pre_tool_context() {
    let pre_text = "必须保留的前置上下文".to_owned();
    let first_post_text = "首个工具已成功计费的后置上下文".to_owned();
    let oversized_post_text = "后".repeat(512);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            (
                "call-post-budget-first",
                "LifecycleProbe",
                json!({ "value": "first" }),
            ),
            (
                "call-post-budget-overflow",
                "LifecycleProbe",
                json!({ "value": "second" }),
            ),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PreAndOversizedPostContextHook {
            pre_text,
            first_post_text,
            oversized_post_text,
        }))
        .expect("跨阶段上下文预算 Hook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(
            HookRuntime::new(
                hooks,
                HookLimits {
                    max_context_bytes: 1_024,
                    ..HookLimits::default()
                },
            )
            .expect("Hook 配置应有效"),
        )
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let attempted = match result.error.as_ref() {
        Some(AgentRunError::Hook(HookError::ContextBytesExceeded {
            maximum: 1_024,
            attempted,
        })) => *attempted,
        other => panic!("第二段 PostToolUse 应触发上下文预算错误，实际为 {other:?}"),
    };
    assert!(attempted > 1_024);
    assert_eq!(
        tool.started(),
        vec!["first".to_owned(), "second".to_owned()]
    );
    let preflights = sink.preflight_snapshot();
    let preflight = preflights
        .first()
        .expect("两个工具的完整 Round 应完成一次预检");
    let expected_pre_context = preflight.pre_tool_context().to_vec();
    assert_eq!(expected_pre_context.len(), 2);
    assert!(matches!(
        expected_pre_context[0].content.as_slice(),
        [ContentBlock::Text { text }] if text.contains("call-post-budget-first")
    ));
    assert!(matches!(
        expected_pre_context[1].content.as_slice(),
        [ContentBlock::Text { text }] if text.contains("call-post-budget-overflow")
    ));
    let expected_pre_bytes = expected_pre_context
        .iter()
        .map(|message| match message.content.as_slice() {
            [ContentBlock::Text { text }] => text.len(),
            other => panic!("PreToolUse 上下文应为单个文本块，实际为 {other:?}"),
        })
        .sum::<usize>();
    assert_eq!(result.hook_context_bytes, expected_pre_bytes);
    let commits = sink.commit_snapshot();
    let committed_messages = commits
        .iter()
        .find_map(|event| match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted { messages, .. }
                if messages.iter().any(|message| {
                    message.content.iter().any(|block| {
                        matches!(block, ContentBlock::ToolResult { tool_result }
                            if tool_result.tool_call_id == "call-post-budget-first")
                    })
                }) =>
            {
                Some(messages)
            }
            _ => None,
        })
        .expect("工具 Round 应通过 Permit 完成提交");
    let mut expected_messages = vec![
        preflight.assistant_message().clone(),
        Message::new(
            MessageRole::Tool,
            vec![
                ContentBlock::ToolResult {
                    tool_result: ToolResult::text("call-post-budget-first", "完成：first", false),
                },
                ContentBlock::ToolResult {
                    tool_result: ToolResult::text(
                        "call-post-budget-overflow",
                        crate::tool::TOOL_OUTPUT_LIMIT_RESULT,
                        true,
                    ),
                },
            ],
        ),
    ];
    expected_messages.extend(expected_pre_context);
    assert_eq!(committed_messages, expected_messages.as_slice());
    assert_eq!(sink.consumed_permits(), 1);
    assert_eq!(sink.released_permits(), 0);
}

/// 持久层暂不可用必须保留错误分类、限制诊断并以普通失败结束 Turn。
#[tokio::test]
async fn unavailable_preflight_is_failed_with_utf8_bounded_diagnostic() {
    let retained = "界".repeat(341);
    let secret_tail = "尾部机密不得泄漏";
    let sink = Arc::new(RejectingPreflightSink::new(
        AgentToolRoundPreflightError::unavailable(format!("{retained}{secret_tail}")),
    ));
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-unavailable-preflight",
            "LifecycleProbe",
            json!({ "value": "must-not-run" }),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let error = match result.error.as_ref() {
        Some(AgentRunError::ToolRoundPreflight(error)) => error,
        other => panic!("持久层暂不可用应返回预检错误，实际为 {other:?}"),
    };
    assert_eq!(error.kind(), AgentToolRoundPreflightErrorKind::Unavailable);
    assert_eq!(error.message(), retained);
    assert_eq!(error.message().len(), 1_023);
    assert!(!error.message().contains(secret_tail));
    assert!(result.state.is_terminal());
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert_eq!(sink.preflight_count(), 1);
    assert!(sink.commit_snapshot().is_empty());
    assert!(tool.started().is_empty());
}

/// 同步预检开始后发生取消时，检查返回后不得进入任何工具生命周期或真实执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_during_sync_preflight_fences_tool_lifecycle() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-cancel-preflight",
            "LifecycleProbe",
            json!({ "value": "must-not-run" }),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(BlockingPreflightSink::default());
    let runner =
        AgentRunner::new(provider, tools, RunLimits::default()).with_commit_sink(sink.clone());
    let cancellation = TurnCancellation::new();
    let mut request = test_turn_request();
    request.set_cancellation(cancellation.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    sink.wait_until_entered().await;
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    cancellation.cancel();
    sink.release();
    let result = task.await.expect("同步预检取消测试任务不应 panic");

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
    assert_eq!(sink.released_permits(), 1);
}

/// 纯内存 Noop Sink 必须允许正常工具 Round 完成且不改变既有工具循环。
#[tokio::test]
async fn noop_preflight_preserves_normal_tool_round() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "call-noop-preflight",
                "LifecycleProbe",
                json!({ "value": "fast" }),
            )]),
            text_reply("Noop 预检后完成"),
        ],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(Arc::new(NoopAgentCommitSink))
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert!(result.state.is_terminal());
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Completed)
    );
    assert_eq!(tool.started(), vec!["fast".to_owned()]);
}

/// 工具请求位置必须保留模型原始索引，预检淘汰的调用只形成稳定间隙。
#[tokio::test]
async fn tool_request_indices_preserve_model_positions_across_preflight_gaps() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                ("call-index-a", "LifecycleProbe", json!({"value": "a"})),
                ("call-index-gap", "MissingTool", json!({})),
                ("call-index-c", "LifecycleProbe", json!({"value": "c"})),
            ]),
            text_reply("索引验证完成"),
        ],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    let commits = sink.commit_snapshot();
    assert_eq!(
        requested_tool_indices(&commits),
        vec![
            ("call-index-a".to_owned(), 0),
            ("call-index-c".to_owned(), 2),
        ]
    );
    assert_eq!(
        committed_tool_result_ids(&commits),
        vec![
            "call-index-a".to_owned(),
            "call-index-gap".to_owned(),
            "call-index-c".to_owned(),
        ]
    );
}

/// 同一模型 Round 的多段提交必须递增，新模型 Round 必须从零重新开始。
#[tokio::test]
async fn round_segment_indices_increment_then_reset_on_next_model_round() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("候选回答"), text_reply("最终回答")],
    ));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(ContinueStopOnceHook::default()))
        .expect("单次继续 Stop Hook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, ToolRegistry::new(), RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    let segments = sink
        .commit_snapshot()
        .into_iter()
        .filter_map(|event| match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted { segment_index, .. }
            | AgentCommitEventKind::RoundCommitted { segment_index, .. } => {
                Some((event.model_round(), *segment_index))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(segments, vec![(1, 0), (1, 1), (2, 0)]);
}

/// 第二个同步请求提交开始后发生取消，已开始提交完成但第三个请求不得再提交。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_during_second_tool_request_cannot_skip_started_commit() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("call-requested-a", "LifecycleProbe", json!({"value": "a"})),
            ("call-requested-b", "LifecycleProbe", json!({"value": "b"})),
            ("call-requested-c", "LifecycleProbe", json!({"value": "c"})),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Requested,
        2,
        ControlledSinkAction::Block,
    ));
    let cancellation = TurnCancellation::new();
    let mut request = test_turn_request();
    request.set_cancellation(cancellation.clone());
    let runner = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    sink.wait_until_target_entered().await;
    cancellation.cancel();
    sink.release_target();
    let result = task.await.expect("请求取消测试任务不应 panic");

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(result.state.step_count(), 0);
    assert!(tool.started().is_empty());
    let events = sink.snapshot();
    assert_eq!(
        requested_tool_ids(&events),
        vec!["call-requested-a".to_owned(), "call-requested-b".to_owned(),]
    );
    assert!(started_tool_ids(&events).is_empty());
    assert_eq!(
        completed_tools(&events),
        vec![
            (
                "call-requested-a".to_owned(),
                ToolCompletionStatus::Cancelled,
            ),
            (
                "call-requested-b".to_owned(),
                ToolCompletionStatus::Cancelled,
            ),
        ]
    );
}

/// 请求 Sink 持续拒绝后，已确认前缀必须收敛且 Turn 保留原始投递错误。
#[tokio::test]
async fn tool_request_sink_failure_closes_confirmed_prefix() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[
            ("call-request-a", "LifecycleProbe", json!({"value": "a"})),
            ("call-request-b", "LifecycleProbe", json!({"value": "b"})),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Requested,
        2,
        ControlledSinkAction::RejectAlways,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::CommitSink(ref error))
            if error.message() == "生命周期测试 Sink 拒绝事件"
    ));
    assert!(tool.started().is_empty());
    let events = sink.snapshot();
    assert_eq!(
        requested_tool_ids(&events),
        vec!["call-request-a".to_owned()]
    );
    assert_eq!(
        completed_tools(&events),
        vec![("call-request-a".to_owned(), ToolCompletionStatus::Cancelled,)]
    );
    assert!(!events.iter().any(is_round_commit));
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 1);
}

/// 执行起点同步提交开始后发生取消，起点不得被取消竞态抹掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_during_execution_start_preserves_started_commit() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-start-wait",
            "LifecycleProbe",
            json!({"value": "write"}),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Started,
        1,
        ControlledSinkAction::Block,
    ));
    let cancellation = TurnCancellation::new();
    let mut request = test_turn_request();
    request.set_cancellation(cancellation.clone());
    let runner = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    sink.wait_until_target_entered().await;
    cancellation.cancel();
    sink.release_target();
    let result = task.await.expect("执行起点取消测试任务不应 panic");

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.started(), vec!["write".to_owned()]);
    let events = sink.snapshot();
    assert_eq!(
        requested_tool_ids(&events),
        vec!["call-start-wait".to_owned()]
    );
    assert_eq!(
        started_tool_ids(&events),
        vec!["call-start-wait".to_owned()]
    );
    assert_eq!(
        completed_tools(&events),
        vec![(
            "call-start-wait".to_owned(),
            ToolCompletionStatus::Cancelled,
        )]
    );
}

/// 顺序工具的执行起点被 Sink 明确拒绝时不得计入 Step 或进入工具实现。
#[tokio::test]
async fn serial_execution_start_rejection_does_not_count_step() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-start-rejected",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Started,
        1,
        ControlledSinkAction::RejectAlways,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 0);
    assert!(tool.started().is_empty());
    let events = sink.snapshot();
    assert!(started_tool_ids(&events).is_empty());
    assert_eq!(
        completed_tools(&events),
        vec![(
            "call-start-rejected".to_owned(),
            ToolCompletionStatus::Cancelled,
        )]
    );
    assert!(!events.iter().any(is_round_commit));
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 1);
}

/// 并行只读工具按真实完成顺序提交终态，但 Round 中的结果保持模型索引顺序。
#[tokio::test]
async fn parallel_tool_completions_are_live_while_round_results_stay_ordered() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[
                ("call-slow", "LifecycleProbe", json!({"value": "slow"})),
                ("call-medium", "LifecycleProbe", json!({"value": "medium"})),
                ("call-fast", "LifecycleProbe", json!({"value": "fast"})),
            ]),
            text_reply("并行完成"),
        ],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert_eq!(
        tool.completed(),
        vec!["fast".to_owned(), "medium".to_owned(), "slow".to_owned()]
    );
    let events = sink.commit_snapshot();
    assert_eq!(
        started_tool_ids(&events),
        vec![
            "call-slow".to_owned(),
            "call-medium".to_owned(),
            "call-fast".to_owned(),
        ]
    );
    assert_eq!(
        completed_tools(&events),
        vec![
            ("call-fast".to_owned(), ToolCompletionStatus::Succeeded),
            ("call-medium".to_owned(), ToolCompletionStatus::Succeeded,),
            ("call-slow".to_owned(), ToolCompletionStatus::Succeeded),
        ]
    );
    assert_eq!(
        committed_tool_result_ids(&events),
        vec![
            "call-slow".to_owned(),
            "call-medium".to_owned(),
            "call-fast".to_owned(),
        ]
    );
}

/// 并行段第 N 个起点投递失败时，已启动前缀必须排空，后缀不得执行。
#[tokio::test]
async fn parallel_start_failure_drains_started_prefix_and_cancels_suffix() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            ("call-start-a", "LifecycleProbe", json!({"value": "a"})),
            ("call-start-b", "LifecycleProbe", json!({"value": "b"})),
            ("call-start-c", "LifecycleProbe", json!({"value": "c"})),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Started,
        2,
        ControlledSinkAction::RejectAlways,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.started(), vec!["a".to_owned()]);
    assert_eq!(tool.completed(), vec!["a".to_owned()]);
    let events = sink.snapshot();
    assert_eq!(started_tool_ids(&events), vec!["call-start-a".to_owned()]);
    assert_eq!(
        completed_tools(&events),
        vec![
            ("call-start-a".to_owned(), ToolCompletionStatus::Cancelled,),
            ("call-start-b".to_owned(), ToolCompletionStatus::Cancelled,),
            ("call-start-c".to_owned(), ToolCompletionStatus::Cancelled,),
        ]
    );
}

/// 并行段起点投递失败必须取消已启动的永久待定兄弟，并只等待单工具清理宽限。
#[tokio::test]
async fn parallel_start_failure_boundedly_drains_pending_sibling() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            (
                "call-start-pending",
                "LifecycleProbe",
                json!({"value": "pending"}),
            ),
            (
                "call-start-rejected",
                "LifecycleProbe",
                json!({"value": "fast"}),
            ),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Started,
        2,
        ControlledSinkAction::RejectAlways,
    ));
    let limits = RunLimits::default()
        .with_tool_cancel_grace_ms(20)
        .expect("正数工具清理宽限应有效");
    let runner = AgentRunner::new(provider, tools, limits)
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        runner.run_turn(test_turn_request()),
    )
    .await
    .expect("并行永久待定兄弟应在清理宽限后结束");

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(tool.started(), vec!["pending".to_owned()]);
    assert!(tool.completed().is_empty());
    let events = sink.snapshot();
    assert_eq!(
        started_tool_ids(&events),
        vec!["call-start-pending".to_owned()]
    );
    assert_eq!(
        completed_tools(&events),
        vec![
            (
                "call-start-pending".to_owned(),
                ToolCompletionStatus::Cancelled,
            ),
            (
                "call-start-rejected".to_owned(),
                ToolCompletionStatus::Cancelled,
            ),
        ]
    );
}

/// PostHook 阻塞期间不得提前冻结工具终态，释放后才能提交唯一完成事件。
#[tokio::test]
async fn tool_completion_waits_for_post_hook_finalization() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "call-gated-hook",
                "LifecycleProbe",
                json!({"value": "fast"}),
            )]),
            text_reply("Hook 已释放"),
        ],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool).expect("生命周期工具应注册");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(GatedPostToolHook {
            entered: entered.clone(),
            release: release.clone(),
        }))
        .expect("门控 PostHook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let runner = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let task = tokio::spawn(async move { runner.run_turn(test_turn_request()).await });

    entered.notified().await;
    let waiting_events = sink.commit_snapshot();
    assert!(completed_tools(&waiting_events).is_empty());
    assert!(!waiting_events.iter().any(is_round_commit));
    release.notify_one();
    let result = task.await.expect("门控 PostHook 测试任务不应 panic");
    assert!(result.is_success());
    assert_eq!(
        completed_tools(&sink.commit_snapshot()),
        vec![(
            "call-gated-hook".to_owned(),
            ToolCompletionStatus::Succeeded,
        )]
    );
}

/// PostHook 主动失败不能改写或重复已经提交的成功工具终态。
#[tokio::test]
async fn post_hook_failure_preserves_single_success_completion() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-hook-failure",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(FailingPostToolHook))
        .expect("失败 PostHook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(result.error, Some(AgentRunError::Hook(_))));
    assert_eq!(
        completed_tools(&sink.commit_snapshot()),
        vec![(
            "call-hook-failure".to_owned(),
            ToolCompletionStatus::Succeeded,
        )]
    );
}

/// PostHook 超时不能改写或重复已经提交的成功工具终态。
#[tokio::test]
async fn post_hook_timeout_preserves_single_success_completion() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-hook-timeout",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(PendingPostToolHook))
        .expect("超时 PostHook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(
            HookRuntime::new(
                hooks,
                HookLimits {
                    max_callback_ms: 20,
                    ..HookLimits::default()
                },
            )
            .expect("Hook 配置应有效"),
        )
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Hook(HookError::TimedOut {
            phase: HookPhase::PostToolUse,
            maximum_ms: 20,
            ..
        }))
    ));
    assert_eq!(
        completed_tools(&sink.commit_snapshot()),
        vec![(
            "call-hook-timeout".to_owned(),
            ToolCompletionStatus::Succeeded,
        )]
    );
}

/// 副作用工具的成功 PostHook 超限后，失败 Hook、ToolCompleted 与 Transcript 必须复用固定结果。
#[tokio::test]
async fn side_effect_post_hook_limit_freezes_one_failure_result_before_completion() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-side-effect-post-limit",
            "LifecycleProbe",
            json!({"value": "write"}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ChangesState,
            ToolConcurrency::Exclusive,
        )))
        .expect("副作用生命周期工具应注册");
    let failure_contexts = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(OversizedPostThenRecordFailureHook {
            failure_contexts: failure_contexts.clone(),
        }))
        .expect("PostHook 容量失败记录器应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: None,
        })
    );
    let expected = ToolResult::text(
        "call-side-effect-post-limit",
        crate::tool::SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT,
        true,
    );
    let events = sink.commit_snapshot();
    let completed = events
        .iter()
        .find_map(|event| match event.kind() {
            AgentCommitEventKind::ToolCompleted {
                tool_call_id,
                status,
                result,
            } if tool_call_id.as_str() == "call-side-effect-post-limit" => {
                assert_eq!(*status, ToolCompletionStatus::Failed);
                Some(result.clone())
            }
            _ => None,
        })
        .expect("PostHook 超限后必须提交唯一工具终态");
    assert_eq!(completed, expected);
    let failure_contexts = failure_contexts
        .lock()
        .expect("PostHook 容量失败上下文锁不应损坏");
    assert_eq!(failure_contexts.len(), 1);
    assert_eq!(
        failure_contexts[0].failure,
        ToolHookFailureKind::OutputLimitExceeded
    );
    assert_eq!(failure_contexts[0].result, expected);
    let transcript = events
        .iter()
        .find_map(|event| match event.kind() {
            AgentCommitEventKind::ModelRoundCommitted { messages, .. }
            | AgentCommitEventKind::RoundCommitted { messages, .. } => messages
                .iter()
                .flat_map(|message| &message.content)
                .find_map(|block| match block {
                    ContentBlock::ToolResult { tool_result }
                        if tool_result.tool_call_id == "call-side-effect-post-limit" =>
                    {
                        Some(tool_result.clone())
                    }
                    _ => None,
                }),
            _ => None,
        })
        .expect("PostHook 超限固定结果必须进入 Transcript");
    assert_eq!(transcript, expected);
    assert_eq!(provider.requests().expect("模型请求快照应可读取").len(), 1);
}

/// 副作用输出已超限时，失败 Hook 错误不得覆盖禁止重试的 ToolOutputLimit 终态。
#[tokio::test]
async fn side_effect_output_limit_survives_failure_hook_error_without_retry() {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-side-effect-output-limit",
            "OversizedSideEffectOutput",
            json!({}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(OversizedSideEffectOutputTool))
        .expect("副作用超限工具应注册");
    let failure_contexts = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(FailingOutputLimitFailureHook {
            failure_contexts: failure_contexts.clone(),
        }))
        .expect("副作用超限失败 Hook 应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: None,
        })
    );
    assert_eq!(provider.requests().expect("模型请求快照应可读取").len(), 1);
    let expected = ToolResult::text(
        "call-side-effect-output-limit",
        crate::tool::SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT,
        true,
    );
    let events = sink.commit_snapshot();
    let completed = events
        .iter()
        .find_map(|event| match event.kind() {
            AgentCommitEventKind::ToolCompleted {
                tool_call_id,
                status,
                result,
            } if tool_call_id.as_str() == "call-side-effect-output-limit" => {
                assert_eq!(*status, ToolCompletionStatus::Failed);
                Some(result.clone())
            }
            _ => None,
        })
        .expect("副作用超限后必须提交固定失败终态");
    assert_eq!(completed, expected);
    let failure_contexts = failure_contexts
        .lock()
        .expect("副作用超限失败 Hook 上下文锁不应损坏");
    assert_eq!(failure_contexts.len(), 1);
    assert_eq!(failure_contexts[0].result, expected);
}

/// 副作用输出超限后的 ToolCompleted 明确拒绝必须与稳定机器码组合保留且禁止 Round 提交。
#[tokio::test]
async fn side_effect_output_limit_combines_rejected_completion_without_leak() {
    let retained_commit_message = "界".repeat(341);
    let commit_secret_tail = "明确拒绝诊断尾部不得泄漏";
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-side-effect-rejected-completion",
            "OversizedSideEffectOutput",
            json!({}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(OversizedSideEffectOutputTool))
        .expect("副作用超限工具应注册");
    let sink = Arc::new(
        ControlledLifecycleSink::new(
            ControlledLifecycleClass::Completed,
            1,
            ControlledSinkAction::RejectAlways,
        )
        .with_error_message(format!("{retained_commit_message}{commit_secret_tail}")),
    );
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let combined = result.error.as_ref().expect("副作用超限必须终止 Turn");
    let completion_error = match combined {
        AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: Some(error),
        } => error,
        other => panic!("副作用超限与明确拒绝应形成组合错误，实际为 {other:?}"),
    };
    assert_eq!(completion_error.kind(), AgentCommitSinkErrorKind::Rejected);
    assert_eq!(completion_error.message(), retained_commit_message);
    assert_eq!(completion_error.message().len(), 1_023);
    assert!(
        combined
            .to_string()
            .contains("tool_output_limit_exceeded_side_effect")
    );
    assert!(combined.to_string().contains(&retained_commit_message));
    assert!(!combined.to_string().contains(commit_secret_tail));
    assert!(!combined.to_string().contains(OVERSIZED_SIDE_EFFECT_SECRET));

    let attempted = sink
        .repeated_target_snapshot()
        .expect("明确拒绝的 ToolCompleted 应冻结同一重投事件");
    let expected = ToolResult::text(
        "call-side-effect-rejected-completion",
        crate::tool::SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT,
        true,
    );
    assert!(matches!(
        attempted.kind(),
        AgentCommitEventKind::ToolCompleted {
            status: ToolCompletionStatus::Failed,
            result,
            ..
        } if result == &expected
    ));
    assert_eq!(sink.completed_attempts(), 2);
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 1);
    assert_eq!(sink.retained_permits(), 0);
    assert!(!sink.snapshot().iter().any(is_round_commit));
    assert_eq!(provider.requests().expect("模型请求快照应可读取").len(), 1);
}

/// 副作用输出超限后的 ToolCompleted 不确定提交必须组合保留分类、事件和恢复预留。
#[tokio::test]
async fn side_effect_output_limit_combines_indeterminate_completion_without_leak() {
    let retained_commit_message = "错".repeat(341);
    let commit_secret_tail = "不确定提交诊断尾部不得泄漏";
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-side-effect-indeterminate-completion",
            "OversizedSideEffectOutput",
            json!({}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(OversizedSideEffectOutputTool))
        .expect("副作用超限工具应注册");
    let sink = Arc::new(
        ControlledLifecycleSink::new(
            ControlledLifecycleClass::Completed,
            1,
            ControlledSinkAction::IndeterminateAlways,
        )
        .with_error_message(format!("{retained_commit_message}{commit_secret_tail}")),
    );
    let result = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let combined = result.error.as_ref().expect("副作用超限必须终止 Turn");
    let completion_error = match combined {
        AgentRunError::ToolOutputLimit {
            code: ToolOutputErrorCode::SideEffectLimitExceeded,
            completion_commit_error: Some(error),
        } => error,
        other => panic!("副作用超限与不确定提交应形成组合错误，实际为 {other:?}"),
    };
    assert_eq!(
        completion_error.kind(),
        AgentCommitSinkErrorKind::Indeterminate
    );
    assert_eq!(completion_error.message(), retained_commit_message);
    assert_eq!(completion_error.message().len(), 1_023);
    assert!(
        combined
            .to_string()
            .contains("tool_output_limit_exceeded_side_effect")
    );
    assert!(combined.to_string().contains(&retained_commit_message));
    assert!(!combined.to_string().contains(commit_secret_tail));
    assert!(!combined.to_string().contains(OVERSIZED_SIDE_EFFECT_SECRET));

    let retained = sink.retained_events();
    assert_eq!(retained.len(), 1);
    assert_eq!(sink.repeated_target_snapshot().as_ref(), retained.first());
    let expected = ToolResult::text(
        "call-side-effect-indeterminate-completion",
        crate::tool::SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT,
        true,
    );
    assert!(matches!(
        retained[0].kind(),
        AgentCommitEventKind::ToolCompleted {
            status: ToolCompletionStatus::Failed,
            result,
            ..
        } if result == &expected
    ));
    assert_eq!(sink.completed_attempts(), 2);
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 0);
    assert_eq!(sink.retained_permits(), 1);
    assert!(!sink.snapshot().iter().any(is_round_commit));
    assert_eq!(provider.requests().expect("模型请求快照应可读取").len(), 1);
}

/// 超过 Round 内容块上限的 65 个工具调用必须在任何工具执行与生命周期预检前拒绝。
#[tokio::test]
async fn sixty_five_tool_calls_are_rejected_before_execution() {
    let ids = (0..=TOOL_OUTPUT_LIMITS.max_round_content_blocks)
        .map(|index| format!("call-round-block-{index}"))
        .collect::<Vec<_>>();
    let calls = ids
        .iter()
        .map(|id| (id.as_str(), "LifecycleProbe", json!({"value": "unused"})))
        .collect::<Vec<_>>();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&calls)],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(RecordingSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::InvalidResponse {
            message: "模型工具调用数量超过 Round 固定结果容量".to_owned(),
        })
    );
    assert_eq!(result.state.step_count(), 0);
    assert!(tool.started().is_empty());
    assert!(sink.commit_snapshot().is_empty());
}

/// ToolCompleted 首次被明确拒绝后必须使用同一事件有限重投并且只提交一个终态。
#[tokio::test]
async fn tool_completion_retries_one_explicit_rejection_without_duplicate() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[
                (
                    "call-complete-slow",
                    "LifecycleProbe",
                    json!({"value": "slow"}),
                ),
                (
                    "call-complete-medium",
                    "LifecycleProbe",
                    json!({"value": "medium"}),
                ),
                (
                    "call-complete-fast",
                    "LifecycleProbe",
                    json!({"value": "fast"}),
                ),
            ]),
            text_reply("终态重投后继续"),
        ],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(CountingPostToolHook {
            calls: hook_calls.clone(),
        }))
        .expect("计数 PostHook 应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::Reject,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert_eq!(
        tool.completed(),
        vec!["fast".to_owned(), "medium".to_owned(), "slow".to_owned()]
    );
    let events = sink.snapshot();
    assert_eq!(started_tool_ids(&events).len(), 3);
    assert_eq!(sink.completed.load(Ordering::SeqCst), 4);
    assert_eq!(
        completed_tools(&events),
        vec![
            (
                "call-complete-fast".to_owned(),
                ToolCompletionStatus::Succeeded,
            ),
            (
                "call-complete-medium".to_owned(),
                ToolCompletionStatus::Succeeded,
            ),
            (
                "call-complete-slow".to_owned(),
                ToolCompletionStatus::Succeeded,
            ),
        ]
    );
    assert_eq!(
        *hook_calls.lock().expect("PostHook 调用记录锁不应损坏"),
        vec![
            "call-complete-fast".to_owned(),
            "call-complete-medium".to_owned(),
            "call-complete-slow".to_owned(),
        ]
    );
    assert_eq!(
        committed_tool_result_ids(&events),
        vec![
            "call-complete-slow".to_owned(),
            "call-complete-medium".to_owned(),
            "call-complete-fast".to_owned(),
        ]
    );
}

/// Sink 已追加 ToolCompleted 却返回错误时，Runtime 重投必须复用身份并只留下一条记录。
#[tokio::test]
async fn completion_retry_reuses_event_id_for_idempotent_append() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "call-idempotent-complete",
                "LifecycleProbe",
                json!({"value": "fast"}),
            )]),
            text_reply("幂等终态后继续"),
        ],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let sink = Arc::new(AppendThenFailOnceSink::default());
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    let attempt_ids = sink.completed_attempt_ids();
    assert_eq!(attempt_ids.len(), 2);
    assert_eq!(attempt_ids[0], attempt_ids[1]);
    let stored = sink.stored_completions();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].event_id(), &attempt_ids[0]);
    assert!(matches!(
        stored[0].kind(),
        AgentCommitEventKind::ToolCompleted {
            status: ToolCompletionStatus::Succeeded,
            ..
        }
    ));
}

/// ToolCompleted 两次失败后的错误必须有界，且重投仍复用同一稳定事件身份。
#[tokio::test]
async fn tool_completion_commit_error_is_utf8_bounded_and_reuses_event_id() {
    let retained = "界".repeat(341);
    let secret_tail = "工具终态尾部机密不得泄漏";
    let sink = Arc::new(OversizedCommitErrorSink::new(
        OversizedCommitTarget::ToolCompleted,
        format!("{retained}{secret_tail}"),
    ));
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-bounded-complete",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    let error = match result.error.as_ref() {
        Some(AgentRunError::CommitSink(error)) => error,
        other => panic!("工具终态提交应返回有界 CommitSink 错误，实际为 {other:?}"),
    };
    assert_eq!(error.message(), retained);
    assert_eq!(error.message().len(), 1_023);
    assert!(!error.message().contains(secret_tail));
    let attempt_ids = sink.attempt_ids();
    assert_eq!(attempt_ids.len(), 2);
    assert_eq!(attempt_ids[0], attempt_ids[1]);
}

/// PostHook 先冻结最终结果；ToolCompleted 持续拒绝后必须终止 Turn 且不提交 Round。
#[tokio::test]
async fn permanently_rejected_completion_runs_finalizing_hook_but_fences_round() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-complete-rejected",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(CountingPostToolHook {
            calls: hook_calls.clone(),
        }))
        .expect("计数 PostHook 应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::RejectAlways,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(sink.completed.load(Ordering::SeqCst), 2);
    assert!(completed_tools(&sink.snapshot()).is_empty());
    assert_eq!(
        *hook_calls.lock().expect("PostHook 调用记录锁不应损坏"),
        vec!["call-complete-rejected".to_owned()]
    );
    assert!(!sink.snapshot().iter().any(is_round_commit));
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 1);
}

/// ToolCompleted 确认持续丢失时必须保留预检正文与事件，禁止 Round 提交。
#[tokio::test]
async fn indeterminate_tool_completion_retains_round_reservation_for_recovery() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [tool_reply(&[(
            "call-complete-indeterminate",
            "LifecycleProbe",
            json!({"value": "fast"}),
        )])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::Exclusive,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(CountingPostToolHook {
            calls: hook_calls.clone(),
        }))
        .expect("计数 PostHook 应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::IndeterminateAlways,
    ));
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::CommitSink(ref error))
            if error.kind() == AgentCommitSinkErrorKind::Indeterminate
                && error.message() == "生命周期测试 Sink 无法确认事件是否提交"
    ));
    assert_eq!(tool.started(), vec!["fast".to_owned()]);
    assert_eq!(tool.completed(), vec!["fast".to_owned()]);
    assert_eq!(
        *hook_calls.lock().expect("PostHook 调用记录锁不应损坏"),
        vec!["call-complete-indeterminate".to_owned()]
    );
    assert_eq!(sink.completed_attempts(), 2);
    let retained = sink.retained_events();
    assert_eq!(retained.len(), 1);
    assert_eq!(sink.repeated_target_snapshot().as_ref(), retained.first());
    assert!(matches!(
        retained[0].kind(),
        AgentCommitEventKind::ToolCompleted {
            tool_call_id,
            status: ToolCompletionStatus::Succeeded,
            result,
        } if tool_call_id.as_str() == "call-complete-indeterminate"
            && result.tool_call_id == "call-complete-indeterminate"
    ));
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 0);
    assert_eq!(sink.retained_permits(), 1);
    assert!(!sink.snapshot().iter().any(is_round_commit));
}

/// 并行工具首个终态确认不确定后必须排空兄弟，且只运行已冻结首项结果的 Hook。
#[tokio::test]
async fn parallel_indeterminate_completion_fences_sibling_hooks_and_round() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            (
                "call-complete-indeterminate-first",
                "LifecycleProbe",
                json!({"value": "slow"}),
            ),
            (
                "call-complete-indeterminate-pending",
                "LifecycleProbe",
                json!({"value": "pending"}),
            ),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let post_hook_calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(CountingAnyPostToolHook {
            calls: post_hook_calls.clone(),
        }))
        .expect("全分支 PostHook 计数器应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::IndeterminateAlways,
    ));
    let limits = RunLimits::default()
        .with_tool_cancel_grace_ms(20)
        .expect("正数工具清理宽限应有效");
    let runner = AgentRunner::new(provider, tools, limits)
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_commit_sink(sink.clone());
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        runner.run_turn(test_turn_request()),
    )
    .await
    .expect("恢复冻结后应在工具清理宽限内排空并结束");

    assert!(matches!(
        result.error,
        Some(AgentRunError::CommitSink(ref error))
            if error.kind() == AgentCommitSinkErrorKind::Indeterminate
                && error.message() == "生命周期测试 Sink 无法确认事件是否提交"
    ));
    assert_eq!(result.state.step_count(), 2);
    assert_eq!(
        tool.started(),
        vec!["slow".to_owned(), "pending".to_owned()]
    );
    assert_eq!(tool.completed(), vec!["slow".to_owned()]);
    let accepted = sink.snapshot();
    assert_eq!(
        started_tool_ids(&accepted),
        vec![
            "call-complete-indeterminate-first".to_owned(),
            "call-complete-indeterminate-pending".to_owned(),
        ]
    );
    assert!(completed_tools(&accepted).is_empty());
    assert_eq!(sink.completed_attempts(), 2);
    let retained = sink.retained_events();
    assert_eq!(retained.len(), 1);
    assert_eq!(sink.repeated_target_snapshot().as_ref(), retained.first());
    assert!(matches!(
        retained[0].kind(),
        AgentCommitEventKind::ToolCompleted {
            tool_call_id,
            status: ToolCompletionStatus::Succeeded,
            result,
        } if tool_call_id.as_str() == "call-complete-indeterminate-first"
            && result.tool_call_id == "call-complete-indeterminate-first"
    ));
    assert_eq!(post_hook_calls.load(Ordering::SeqCst), 1);
    assert_eq!(sink.retained_permits(), 1);
    assert_eq!(sink.consumed_permits(), 0);
    assert_eq!(sink.released_permits(), 0);
    assert!(!accepted.iter().any(is_round_commit));
}

/// 同步 ToolCompleted 提交不受实时事件超时限制，释放后仍可继续 Hook 与 Round。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_completion_commit_is_not_limited_by_event_sink_timeout() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "call-complete-timeout",
                "LifecycleProbe",
                json!({"value": "fast"}),
            )]),
            text_reply("同步终态提交完成"),
        ],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(LifecycleProbeTool::new(
            ToolEffect::ReadOnly,
            ToolConcurrency::Exclusive,
        )))
        .expect("生命周期工具应注册");
    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(CountingPostToolHook {
            calls: hook_calls.clone(),
        }))
        .expect("计数 PostHook 应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::Block,
    ));
    let limits = RunLimits::default()
        .with_event_sink_timeout_ms(20)
        .expect("正数 Sink 时限应有效");
    let runner = AgentRunner::new(provider, tools, limits)
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let task = tokio::spawn(async move { runner.run_turn(test_turn_request()).await });

    sink.wait_until_target_entered().await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!task.is_finished(), "同步提交不得被实时事件时限取消");
    sink.release_target();
    let result = task.await.expect("同步提交时限测试任务不应 panic");

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(sink.completed.load(Ordering::SeqCst), 1);
    assert_eq!(
        completed_tools(&sink.snapshot()),
        vec![(
            "call-complete-timeout".to_owned(),
            ToolCompletionStatus::Succeeded,
        )]
    );
    assert_eq!(
        *hook_calls.lock().expect("PostHook 调用记录锁不应损坏"),
        vec!["call-complete-timeout".to_owned()]
    );
    assert!(sink.snapshot().iter().any(is_round_commit));
}

/// 并行段首个工具终态永久被拒绝时必须取消并有界排空永久待定兄弟。
#[tokio::test]
async fn parallel_completion_failure_boundedly_drains_pending_sibling() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            parallel_tool_calls: true,
            ..ProviderCapabilities::default()
        },
        [tool_reply(&[
            (
                "call-complete-fast",
                "LifecycleProbe",
                json!({"value": "fast"}),
            ),
            (
                "call-complete-pending",
                "LifecycleProbe",
                json!({"value": "pending"}),
            ),
        ])],
    ));
    let tool = Arc::new(LifecycleProbeTool::new(
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
    ));
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).expect("生命周期工具应注册");
    let sink = Arc::new(ControlledLifecycleSink::new(
        ControlledLifecycleClass::Completed,
        1,
        ControlledSinkAction::RejectAlways,
    ));
    let limits = RunLimits::default()
        .with_tool_cancel_grace_ms(20)
        .expect("正数工具清理宽限应有效");
    let runner = AgentRunner::new(provider, tools, limits)
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone());
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        runner.run_turn(test_turn_request()),
    )
    .await
    .expect("并行永久待定兄弟应在清理宽限后结束");

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert_eq!(result.state.step_count(), 2);
    assert_eq!(
        tool.started(),
        vec!["fast".to_owned(), "pending".to_owned()]
    );
    assert_eq!(tool.completed(), vec!["fast".to_owned()]);
    assert_eq!(sink.completed.load(Ordering::SeqCst), 3);
    let events = sink.snapshot();
    assert_eq!(
        completed_tools(&events),
        vec![(
            "call-complete-pending".to_owned(),
            ToolCompletionStatus::Cancelled,
        )]
    );
    assert!(!events.iter().any(is_round_commit));
}

/// 无效 ToolOutput 必须先形成稳定失败 Hook 输入，再提交唯一 Failed 终态。
#[tokio::test]
async fn invalid_tool_output_becomes_failed_result_before_failure_hook() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[("call-invalid-output", "InvalidOutput", json!({}))]),
            text_reply("无效输出已交给模型"),
        ],
    ));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(InvalidOutputTool))
        .expect("无效输出工具应注册");
    let sink = Arc::new(RecordingSink::default());
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks
        .register(Arc::new(InvalidOutputFailureHook {
            sink: sink.clone(),
            contexts: contexts.clone(),
        }))
        .expect("无效输出失败 Hook 应注册");
    let result = AgentRunner::new(provider, tools, RunLimits::default())
        .with_hook_runtime(HookRuntime::new(hooks, HookLimits::default()).expect("Hook 配置应有效"))
        .with_event_sink(sink.clone())
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.step_count(), 1);
    assert_eq!(
        completed_tools(&sink.commit_snapshot()),
        vec![(
            "call-invalid-output".to_owned(),
            ToolCompletionStatus::Failed,
        )]
    );
    let contexts = contexts.lock().expect("无效输出 Hook 上下文锁不应损坏");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].failure, ToolHookFailureKind::InvalidOutput);
    assert!(contexts[0].result.is_error);
    assert!(matches!(
        contexts[0].result.content.as_slice(),
        [ToolResultContent::Text { text }] if text == "工具返回了无效输出"
    ));
}

/// Provider 最终形成无效空内容时，Agent 必须失败且不得提交 Round Transcript。
#[tokio::test]
async fn invalid_empty_model_content_never_commits_round() {
    let cases = vec![
        (
            "空文本",
            vec![
                ModelStreamEvent::MessageStart {
                    metadata: ResponseMetadata::default(),
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    delta: String::new(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: StopReason::Completed,
                },
            ],
        ),
        (
            "空推理",
            vec![
                ModelStreamEvent::MessageStart {
                    metadata: ResponseMetadata::default(),
                },
                ModelStreamEvent::ReasoningDelta {
                    index: 0,
                    delta: String::new(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: StopReason::Completed,
                },
            ],
        ),
        (
            "显式空推理摘要",
            vec![
                ModelStreamEvent::MessageStart {
                    metadata: ResponseMetadata::default(),
                },
                ModelStreamEvent::ReasoningDelta {
                    index: 0,
                    delta: "有效推理正文".to_owned(),
                },
                ModelStreamEvent::ReasoningSummaryDelta {
                    index: 0,
                    delta: String::new(),
                },
                ModelStreamEvent::MessageEnd {
                    stop_reason: StopReason::Completed,
                },
            ],
        ),
    ];

    for (name, events) in cases {
        let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
            ProviderCapabilities::default(),
            [ScriptedReply::events(events)],
        ));
        let sink = Arc::new(RecordingSink::default());
        let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
            .run_turn(test_turn_request())
            .await;

        assert!(
            matches!(result.error, Some(AgentRunError::Model(_))),
            "{name} 应作为模型内容错误终止"
        );
        assert!(
            !sink.commit_snapshot().iter().any(is_round_commit),
            "{name} 不得产生 RoundCommitted"
        );
    }
}

/// 推理和分块工具参数必须保持 Provider 原序，下一次调用使用新的 Round。
#[tokio::test]
async fn reasoning_and_chunked_tool_arguments_keep_round_order() {
    let first_reply = ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            delta: "检查".to_owned(),
        },
        ModelStreamEvent::ReasoningSummaryDelta {
            index: 0,
            delta: "读取文件".to_owned(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 1,
            id: "call-1".to_owned(),
            name: "Read".to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 1,
            id: "call-1".to_owned(),
            delta: "{\"path\":".to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 1,
            id: "call-1".to_owned(),
            delta: "\"README.md\"}".to_owned(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 1,
            id: "call-1".to_owned(),
        },
        ModelStreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: Some(5),
                output_tokens: Some(4),
                ..TokenUsage::unknown()
            },
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ]);
    let second_reply = ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: "完成".to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [first_reply, second_reply],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;
    assert!(result.is_success());

    let events = sink.snapshot();
    let first_round = events
        .iter()
        .filter(|event| event.model_round() == 1)
        .collect::<Vec<_>>();
    assert_eq!(first_round.len(), 9);
    assert!(matches!(
        first_round[4].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallArgumentsDelta { delta, .. }
        } if delta == "{\"path\":"
    ));
    assert!(matches!(
        first_round[5].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallArgumentsDelta { delta, .. }
        } if delta == "\"README.md\"}"
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.model_round() == 2)
            .count(),
        3
    );

    let replayed = replay_round(&events, 1).await;
    assert!(matches!(
        replayed.content.as_slice(),
        [ContentBlock::Reasoning { .. }, ContentBlock::ToolCall { tool_call }]
            if tool_call.arguments == json!({ "path": "README.md" })
    ));
}

/// Provider 中途错误必须在已确认前缀之后形成唯一错误边界。
#[tokio::test]
async fn provider_error_is_emitted_as_single_failure_boundary() {
    let provider_error = ModelError::Transport {
        message: "测试连接中断".to_owned(),
        retryable: true,
    };
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Err(provider_error.clone()),
        ])],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Model(provider_error.clone()))
    );
    let events = sink.snapshot();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].kind(),
        AgentStreamEventKind::ModelFailure { error } if error == &provider_error
    ));
}

/// 断言失败模型调用只记账已确认用量，且绝不提交不完整 Transcript。
fn assert_failed_usage_without_transcript(
    result: &TurnResult,
    sink: &RecordingSink,
    expected_usage: &TokenUsage,
) {
    assert!(result.error.is_some(), "失败流必须返回错误终态");
    let usages = sink.usages();
    assert_eq!(usages.len(), 1, "失败调用只能产生一次用量记账");
    assert_eq!(usages[0].model_round(), 1);
    assert_eq!(usages[0].call_attempt(), 1);
    assert_eq!(usages[0].purpose(), ModelCallPurpose::AgentRound);
    assert_eq!(&usages[0].completion().usage, expected_usage);
    assert!(
        !sink.commit_snapshot().iter().any(|event| matches!(
            event.kind(),
            AgentCommitEventKind::ModelRoundCommitted { .. }
        )),
        "失败调用不得提交 ModelRoundCommitted"
    );
}

/// Provider 在 Usage 后传输失败时仍须提交已确认用量，不得提交不完整 Transcript。
#[tokio::test]
async fn provider_transport_error_after_usage_commits_usage_without_transcript() {
    let usage = reported_usage();
    let provider_error = ModelError::Transport {
        message: "Usage 后连接中断".to_owned(),
        retryable: false,
    };
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::Usage {
                usage: usage.clone(),
            }),
            Err(provider_error.clone()),
        ])],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::Model(provider_error)),
        "传输错误分类必须保留"
    );
    assert_failed_usage_without_transcript(&result, &sink, &usage);
}

/// Provider 在 Usage 后提前关闭时仍须提交已确认用量，不得提交不完整 Transcript。
#[tokio::test]
async fn provider_eof_after_usage_commits_usage_without_transcript() {
    let usage = reported_usage();
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::Usage {
                usage: usage.clone(),
            }),
        ])],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::StreamInterrupted { .. }))
    ));
    assert_failed_usage_without_transcript(&result, &sink, &usage);
}

/// Provider 在 Usage 后违反事件顺序时仍须提交已确认用量，不得提交不完整 Transcript。
#[tokio::test]
async fn provider_protocol_error_after_usage_commits_usage_without_transcript() {
    let usage = reported_usage();
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::Usage {
                usage: usage.clone(),
            }),
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
        ])],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::Model(ModelError::Protocol { .. }))
    ));
    assert_failed_usage_without_transcript(&result, &sink, &usage);
}

/// Provider 在 Usage 后被取消时仍须提交已确认用量，不得提交不完整 Transcript。
#[tokio::test]
async fn provider_cancellation_after_usage_commits_usage_without_transcript() {
    let usage = reported_usage();
    let usage_for_stream = usage.clone();
    let provider_stream: ModelStream = Box::pin(stream::unfold(0_u8, move |index| {
        let usage = usage_for_stream.clone();
        async move {
            match index {
                0 => Some((
                    Ok(ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    }),
                    1,
                )),
                1 => Some((Ok(ModelStreamEvent::Usage { usage }), 2)),
                _ => pending::<Option<(Result<ModelStreamEvent, ModelError>, u8)>>().await,
            }
        }
    }));
    let provider: Arc<dyn ModelProvider> = Arc::new(StreamQueueProvider::new(
        ProviderCapabilities::default(),
        [provider_stream],
    ));
    let sink = Arc::new(RecordingSink::default());
    let cancellation = TurnCancellation::new();
    let mut request = test_turn_request();
    request.set_cancellation(cancellation.clone());
    let sink_for_task = sink.clone();
    let task = tokio::spawn(async move {
        event_runner(
            provider,
            sink_for_task.clone(),
            sink_for_task.clone(),
            RunLimits::default(),
        )
        .run_turn(request)
        .await
    });

    // 首个开始事件和 Usage 都必须先被 Sink 确认，才能验证取消后的部分用量保留。
    sink.wait_for_count(2).await;
    cancellation.cancel();
    let result = task.await.expect("取消失败流测试任务不应 panic");

    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_failed_usage_without_transcript(&result, &sink, &usage);
}

/// 在每次轮询时计数并按队列返回事件的测试流。
struct CountingStream {
    /// 尚未被 Provider 消费的事件或错误。
    events: VecDeque<Result<ModelStreamEvent, ModelError>>,
    /// 底层 Provider 实际发生的 `poll_next` 次数。
    polls: Arc<AtomicUsize>,
}

impl Stream for CountingStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    /// 记录轮询并立即返回下一项，便于证明终止栅栏没有读取迟到事件。
    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(self.events.pop_front())
    }
}

/// 在指定序号主动失败且只保存此前已确认事件的 Sink。
struct RejectingSink {
    /// 从一开始计算的失败事件序号。
    fail_at: usize,
    /// 已进入 Sink 的事件数量。
    attempts: AtomicUsize,
    /// 失败前已经可靠接收的事件。
    accepted: Mutex<Vec<AgentStreamEvent>>,
}

/// 永远拒绝失败模型调用的明确用量提交，用于验证权威记账错误优先级。
struct RejectingUsageCommitSink;

impl AgentCommitSink for RejectingUsageCommitSink {
    /// 失败流测试不进入工具 Round，显式返回无状态预留。
    fn preflight_tool_round(
        &self,
        _round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        Ok(accepted_tool_round_reservation())
    }

    /// 用量提交持续拒绝，模拟权威 Goal 记账失败。
    fn commit_model_round_usage(
        &self,
        _usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        Err(AgentCommitSinkError::rejected("失败流用量提交拒绝"))
    }

    /// 失败流不会形成其他权威事件；意外收到时保持无状态确认。
    fn commit(&self, _event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        Ok(())
    }
}

impl RejectingSink {
    /// 创建在指定事件序号主动失败的 Sink。
    fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            attempts: AtomicUsize::new(0),
            accepted: Mutex::new(Vec::new()),
        }
    }
}

impl AgentEventSink for RejectingSink {
    /// 失败事件不进入已接收集合，符合 Sink 的原子确认契约。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        let result = if attempt == self.fail_at {
            Err(AgentEventSinkError::new("测试 Sink 拒绝事件"))
        } else {
            self.accepted
                .lock()
                .expect("拒绝 Sink 测试锁不应损坏")
                .push(event.clone());
            Ok(())
        };
        Box::pin(async move { result })
    }
}

/// Sink 主动失败必须终止 Turn，且归约器和 Provider 都不能越过失败事件。
#[tokio::test]
async fn sink_failure_stops_reduction_and_late_provider_polls() {
    let polls = Arc::new(AtomicUsize::new(0));
    let provider_stream: ModelStream = Box::pin(CountingStream {
        events: VecDeque::from([
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::TextDelta {
                index: 0,
                delta: "不会确认".to_owned(),
            }),
            Ok(ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            }),
        ]),
        polls: polls.clone(),
    });
    let provider: Arc<dyn ModelProvider> = Arc::new(StreamQueueProvider::new(
        ProviderCapabilities::default(),
        [provider_stream],
    ));
    let sink = Arc::new(RejectingSink::new(2));
    let result = event_runner(
        provider,
        sink.clone(),
        Arc::new(NoopAgentCommitSink),
        RunLimits::default(),
    )
    .run_turn(test_turn_request())
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::EventSink(
            AgentEventDeliveryError::SinkFailed { ref message }
        )) if message == "测试 Sink 拒绝事件"
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(result.final_response.is_none());
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(
        sink.accepted
            .lock()
            .expect("拒绝 Sink 测试锁不应损坏")
            .len(),
        1
    );
}

/// 实时事件 Sink 失败后若明确用量记账也失败，必须优先返回权威 CommitSink 错误。
#[tokio::test]
async fn usage_commit_failure_takes_priority_over_event_sink_failure() {
    let usage = reported_usage();
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::Usage { usage },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "触发实时 Sink 失败".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    ));
    let event_sink = Arc::new(RejectingSink::new(3));
    let result = event_runner(
        provider,
        event_sink,
        Arc::new(RejectingUsageCommitSink),
        RunLimits::default(),
    )
    .run_turn(test_turn_request())
    .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::CommitSink(ref error))
            if error.message() == "失败流用量提交拒绝"
    ));
    assert_eq!(result.state.terminal_reason(), Some(TerminalReason::Failed));
    assert!(result.final_response.is_none());
}

/// 永不确认任何事件的 Sink，用于验证硬背压时限。
struct PendingSink;

impl AgentEventSink for PendingSink {
    /// 保持 Future 待定，直到 Runner 的单事件时限将其丢弃。
    fn send<'a>(&'a self, _event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(pending())
    }
}

/// 永久背压必须在配置时限内成为独立 Sink 超时，而不是挂住 Turn。
#[tokio::test]
async fn sink_backpressure_is_bounded_by_per_event_timeout() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    ));
    let limits = RunLimits::default()
        .with_event_sink_timeout_ms(20)
        .expect("正数 Sink 时限应当有效");
    let result = event_runner(
        provider,
        Arc::new(PendingSink),
        Arc::new(NoopAgentCommitSink),
        limits,
    )
    .run_turn(test_turn_request())
    .await;

    assert_eq!(
        result.error,
        Some(AgentRunError::EventSink(
            AgentEventDeliveryError::TimedOut { maximum_ms: 20 }
        ))
    );
}

/// 只阻塞文本事件并在 Future 被丢弃前不发布它的取消测试 Sink。
#[derive(Default)]
struct BlockingTextSink {
    /// 已经可靠接收的非阻塞事件。
    accepted: Mutex<Vec<AgentStreamEvent>>,
    /// 文本事件是否已经进入阻塞 Future。
    text_entered: AtomicBool,
    /// 文本 Future 开始等待后唤醒取消测试任务。
    entered: Notify,
}

impl BlockingTextSink {
    /// 等待文本事件已经进入仍未确认的 Sink Future。
    async fn wait_until_text_entered(&self) {
        loop {
            let entered = self.entered.notified();
            if self.text_entered.load(Ordering::Acquire) {
                return;
            }
            entered.await;
        }
    }
}

impl AgentEventSink for BlockingTextSink {
    /// 文本保持待定且不发布；其他事件立即可靠保存。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        if matches!(
            event.kind(),
            AgentStreamEventKind::ModelEvent {
                event: ModelStreamEvent::TextDelta { .. }
            }
        ) {
            self.text_entered.store(true, Ordering::Release);
            self.entered.notify_waiters();
            return Box::pin(pending());
        }
        self.accepted
            .lock()
            .expect("取消 Sink 测试锁不应损坏")
            .push(event.clone());
        Box::pin(async { Ok(()) })
    }
}

/// 取消赢得 Sink 竞态后只能发送取消边界，未确认文本和后续事件都不得迟到。
#[tokio::test]
async fn cancellation_drops_unacknowledged_event_and_fences_late_events() {
    let polls = Arc::new(AtomicUsize::new(0));
    let provider_stream: ModelStream = Box::pin(CountingStream {
        events: VecDeque::from([
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::TextDelta {
                index: 0,
                delta: "尚未确认".to_owned(),
            }),
            Ok(ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            }),
        ]),
        polls: polls.clone(),
    });
    let provider: Arc<dyn ModelProvider> = Arc::new(StreamQueueProvider::new(
        ProviderCapabilities::default(),
        [provider_stream],
    ));
    let sink = Arc::new(BlockingTextSink::default());
    let runner = event_runner(
        provider,
        sink.clone(),
        Arc::new(NoopAgentCommitSink),
        RunLimits::default(),
    );
    let cancellation = TurnCancellation::new();
    let mut request = test_turn_request();
    request.set_cancellation(cancellation.clone());
    let task = tokio::spawn(async move { runner.run_turn(request).await });

    sink.wait_until_text_entered().await;
    cancellation.cancel();
    let result = task.await.expect("取消实时事件测试任务不应 panic");
    assert_eq!(result.error, Some(AgentRunError::Cancelled));
    assert_eq!(
        result.state.terminal_reason(),
        Some(TerminalReason::Cancelled)
    );
    assert_eq!(polls.load(Ordering::SeqCst), 2);

    let accepted = sink
        .accepted
        .lock()
        .expect("取消 Sink 测试锁不应损坏")
        .clone();
    assert_eq!(accepted.len(), 2);
    assert!(matches!(
        accepted[0].kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageStart { .. }
        }
    ));
    assert!(matches!(
        accepted[1].kind(),
        AgentStreamEventKind::ModelFailure {
            error: ModelError::Cancelled { .. }
        }
    ));
}

/// MessageEnd 是 Round 的不可逆完成边界，底层流中的迟到事件不得再被读取或发送。
#[tokio::test]
async fn message_end_fences_provider_events_that_arrive_late() {
    let polls = Arc::new(AtomicUsize::new(0));
    let provider_stream: ModelStream = Box::pin(CountingStream {
        events: VecDeque::from([
            Ok(ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }),
            Ok(ModelStreamEvent::TextDelta {
                index: 0,
                delta: "完成".to_owned(),
            }),
            Ok(ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            }),
            Ok(ModelStreamEvent::TextDelta {
                index: 0,
                delta: "迟到".to_owned(),
            }),
        ]),
        polls: polls.clone(),
    });
    let provider: Arc<dyn ModelProvider> = Arc::new(StreamQueueProvider::new(
        ProviderCapabilities::default(),
        [provider_stream],
    ));
    let sink = Arc::new(RecordingSink::default());
    let result = event_runner(provider, sink.clone(), sink.clone(), RunLimits::default())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert_eq!(polls.load(Ordering::SeqCst), 3);
    let events = sink.snapshot();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        sink.commit_snapshot().as_slice(),
        [event]
            if matches!(
                event.kind(),
                AgentCommitEventKind::ModelRoundCommitted {
                    segment_index: 0,
                    ..
                }
            )
    ));
    assert!(!events.iter().any(|event| matches!(
        event.kind(),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::TextDelta { delta, .. }
        } if delta == "迟到"
    )));
}

/// RunLimits 必须拒绝无界等价的零毫秒 Sink 接收时限。
#[test]
fn zero_event_sink_timeout_is_rejected() {
    assert_eq!(
        RunLimits::default().with_event_sink_timeout_ms(0),
        Err(RunLimitsError::ZeroEventSinkTimeout)
    );
}

/// 动态消息必须先作为 RoundCommitted 进入 Transcript，随后才能确认外部持久 claim。
#[tokio::test]
async fn dynamic_input_is_acknowledged_only_after_round_commit() {
    let probe = Arc::new(DynamicInputProbe::default());
    let dynamic_message = Message::text(MessageRole::Developer, "mailbox 已送达");
    let source = Arc::new(OneShotDynamicInputSource::new(
        1,
        dynamic_message.clone(),
        probe.clone(),
    ));
    let sink = Arc::new(DynamicInputCommitSink::new(probe.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("已处理动态消息")],
    ));

    let result = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
        .with_dynamic_input_source(source)
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert!(probe.persisted.load(Ordering::SeqCst));
    assert!(probe.acknowledged.load(Ordering::SeqCst));
    assert_eq!(probe.acknowledgement_attempts.load(Ordering::SeqCst), 1);
    assert!(!probe.acknowledged_before_persist.load(Ordering::SeqCst));
    let commits = sink.commit_snapshot();
    assert!(matches!(
        commits.as_slice(),
        [dynamic, completion]
            if matches!(
                dynamic.kind(),
                AgentCommitEventKind::RoundCommitted { messages, .. }
                    if messages == &vec![dynamic_message.clone()]
            ) && matches!(
                completion.kind(),
                AgentCommitEventKind::ModelRoundCommitted { .. }
            )
    ));
    let requests = provider.requests().expect("模型请求记录应可读取");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.contains(&dynamic_message));
}

/// Transcript 提交明确失败时不得调用回执，外部持久 claim 必须保持未确认。
#[tokio::test]
async fn dynamic_input_commit_failure_keeps_claim_unacknowledged() {
    let probe = Arc::new(DynamicInputProbe::default());
    let source = Arc::new(OneShotDynamicInputSource::new(
        1,
        Message::text(MessageRole::User, "提交失败后仍需保留"),
        probe.clone(),
    ));
    let sink = Arc::new(DynamicInputCommitSink::new(probe.clone(), true));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("不应调用")],
    ));

    let result = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
        .with_dynamic_input_source(source)
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(result.error, Some(AgentRunError::CommitSink(_))));
    assert!(!probe.persisted.load(Ordering::SeqCst));
    assert!(!probe.acknowledged.load(Ordering::SeqCst));
    assert_eq!(probe.acknowledgement_attempts.load(Ordering::SeqCst), 0);
    assert!(sink.commit_snapshot().is_empty());
    let attempts = sink.attempt_snapshot();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(
        provider
            .requests()
            .expect("模型请求记录应可读取")
            .is_empty()
    );
}

/// 回执持续失败时正文已经唯一持久化，但 Turn 必须失败且不得把 claim 标记为已确认。
#[tokio::test]
async fn dynamic_input_ack_failure_does_not_duplicate_persisted_body() {
    let probe = Arc::new(DynamicInputProbe::default());
    probe
        .acknowledgement_failures_remaining
        .store(2, Ordering::SeqCst);
    let dynamic_message = Message::text(MessageRole::Developer, "确认失败正文");
    let source = Arc::new(OneShotDynamicInputSource::new(
        1,
        dynamic_message.clone(),
        probe.clone(),
    ));
    let sink = Arc::new(DynamicInputCommitSink::new(probe.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("不应调用")],
    ));

    let result = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
        .with_dynamic_input_source(source)
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(matches!(
        result.error,
        Some(AgentRunError::DynamicInputAcknowledgement { .. })
    ));
    assert!(probe.persisted.load(Ordering::SeqCst));
    assert!(!probe.acknowledged.load(Ordering::SeqCst));
    assert_eq!(probe.acknowledgement_attempts.load(Ordering::SeqCst), 2);
    assert!(!probe.acknowledged_before_persist.load(Ordering::SeqCst));
    let commits = sink.commit_snapshot();
    assert!(matches!(
        commits.as_slice(),
        [event]
            if matches!(
                event.kind(),
                AgentCommitEventKind::RoundCommitted { messages, .. }
                    if messages == &vec![dynamic_message.clone()]
            )
    ));
    assert_eq!(
        result
            .messages
            .iter()
            .filter(|message| *message == &dynamic_message)
            .count(),
        1
    );
    assert!(
        provider
            .requests()
            .expect("模型请求记录应可读取")
            .is_empty()
    );
}

/// 最终响应流式生成期间到达的输入必须阻止当前 Turn 提前完成并触发下一模型 Round。
#[tokio::test]
async fn dynamic_input_arriving_after_final_candidate_triggers_next_model_round() {
    let probe = Arc::new(DynamicInputProbe::default());
    let dynamic_message = Message::text(MessageRole::User, "请在结束前补充这一点");
    let source = Arc::new(OneShotDynamicInputSource::new(
        2,
        dynamic_message.clone(),
        probe.clone(),
    ));
    let sink = Arc::new(DynamicInputCommitSink::new(probe.clone(), false));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [text_reply("第一版结论"), text_reply("补充后的最终结论")],
    ));

    let result = AgentRunner::new(provider.clone(), ToolRegistry::new(), RunLimits::default())
        .with_dynamic_input_source(source)
        .with_commit_sink(sink.clone())
        .run_turn(test_turn_request())
        .await;

    assert!(result.is_success());
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(
        result.final_response.as_ref().and_then(|response| {
            response.content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
        }),
        Some("补充后的最终结论")
    );
    assert!(probe.acknowledged.load(Ordering::SeqCst));
    assert!(!probe.acknowledged_before_persist.load(Ordering::SeqCst));
    let requests = provider.requests().expect("模型请求记录应可读取");
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].messages.contains(&dynamic_message));
    assert!(requests[1].messages.contains(&dynamic_message));
    let commits = sink.commit_snapshot();
    assert!(matches!(
        commits.as_slice(),
        [first_completion, dynamic, second_completion]
            if matches!(
                first_completion.kind(),
                AgentCommitEventKind::ModelRoundCommitted { .. }
            ) && matches!(
                dynamic.kind(),
                AgentCommitEventKind::RoundCommitted { messages, .. }
                    if messages == &vec![dynamic_message.clone()]
            ) && matches!(
                second_completion.kind(),
                AgentCommitEventKind::ModelRoundCommitted { .. }
            )
    ));
}
