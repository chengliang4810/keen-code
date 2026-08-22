//! [`run_session_loop`] 的 helper 子流程（L5：自 `peri-acp/src/host/exec/executor_helpers.rs`
//! 物理迁入，ACP 侧保留 re-export 桥）。
//!
//! 本文件承载以下四个被 orchestrator 串起来的子流程：
//!
//! - [`intercept_immediate_command`]：slash 命令拦截（Immediate 直接返回，不构建 agent）
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer（经注入闭包）
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`]：close channel + 等待 pump drain + recall 提取
//!
//! 共享类型（原 ACP `executor.rs` 定义）随本文件迁入：[`ExecOutcome`] /
//! [`ModeNoticeBooking`] / [`mark_permission_mode_notified`]。
//!
//! # 依赖反转（§0）
//!
//! 本模块只依赖 peri-acp-types / peri-model / crate 内部：
//! - 事件发射经 [`EventPublisher`] 端口（ACP/Controller 适配层实现），
//!   事件消费经 [`EventSubscriber`] 端口（包装 Controller 订阅）
//! - 命令拦截经注入的 `command_lookup` 闭包（ACP 协议面注册表）+ 注入的
//!   `compact_config_loader` 闭包（`load_compact_config` 语义留在 ACP）
//! - stage 装配经注入的 `StageBuildFn`（ACP 侧从 `SessionContext` 投影
//!   `StageBuildInput` 并补齐注入面）；Langfuse tracer 由 ACP 闭包捕获，
//!   本模块不触碰观测实现
//! - cancel cascade 经注入的 `cancel_cascade` 闭包（ACP 侧 `SessionManager`）
//!
//! # Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 优先于 `cancel.cancelled()`；二者均会触发 `push_done`）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason，
//!   顺序与原实现一致
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"，
//!   顺序不变（pump 必须先 close sender 才能退出 recv 循环）

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use peri_acp_types::{
    command::{AgentCommand, CommandContext, CommandKind, CommandResult, PromptStopReason},
    compact::CompactConfig,
    error::AgentError,
    event::{
        AgentEventHandler, BackgroundTaskResult, DoneKind, EventPublisher, EventSink,
        EventSubscriber, ExecutorEvent, SubscriptionError, TurnErrorKind, TurnStatus,
    },
    event_v2::EventHandles,
    frozen::{ChildHandlerFactory, FrozenData, ThreadPersistence},
    goal::GoalController,
    identity::EventDeliveryClass,
    messages::{BaseMessage, MessageContent},
    permission::PermissionMode,
    runtime::UnstampedEvent,
    session::PromptResult,
    store::ThreadStore,
    tasks::{BgTaskKind, TaskManager},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::agent::{
    agent_context::AgentContext,
    async_tasks::TaskManager as AgentTaskManager,
    react::AgentInput,
    stages::{run_react_loop, LoopResult},
    state::AgentState,
};
use crate::session::{
    exec::stage_builder::{CachedLlmInstances, V2AgentOutput},
    MessageTranscript,
};

// ── 注入面类型别名（clippy type_complexity）────────────────────────────────

/// 命令注册表查找闭包（ACP 协议面注册表注入）。
pub type CommandLookupFn =
    Arc<dyn Fn(&str) -> Option<(Arc<dyn AgentCommand>, String)> + Send + Sync>;

/// Langfuse trace 收尾闭包（构造 JoinHandle 后由 pump 在 pump_done 之后 drop）。
pub type LangfuseEndFn =
    Arc<dyn Fn(Option<String>) -> Option<tokio::task::JoinHandle<()>> + Send + Sync>;

/// EventBus forwarder 启动器闭包（ACP 侧持有 Langfuse bridge 构造；
/// 参数 = event_handles / 主 agent_id / 事件消费闭包）。
pub type ForwarderLauncherFn = Arc<
    dyn Fn(EventHandles, String, Box<dyn Fn(UnstampedEvent, ExecutorEvent) + Send + Sync>)
        + Send
        + Sync,
>;

// ── 共享类型（L5：自 ACP executor.rs 迁入）──────────────────────────────────

/// Agent 执行后的最终输出（state + 停止原因）。
pub struct ExecOutcome {
    pub ok: bool,
    pub stop_reason: PromptStopReason,
    /// A Full Compact committed during this turn and replaced prior visible history.
    pub history_replaced_by_compaction: bool,
    pub agent_state: AgentState,
}

/// D2：mode 通知的"检测"与"记账"分离载体。
///
/// `text` 已随 agent_input 生成（ACP `run_session_loop`）；`last_notified` / `mode`
/// 供 [`build_and_execute_agent_v2`] 在 Phase 6 入队点调用
/// [`mark_permission_mode_notified`]。不直接持有闭包，保持类型简单可测。
pub struct ModeNoticeBooking {
    /// 已生成的受控通知文本（与 agent_input 一起入队）。
    pub text: String,
    /// session 级 last-notified 原子值。
    pub last_notified: Arc<AtomicU8>,
    /// 本次记账的 mode。
    pub mode: PermissionMode,
}

/// 通知已随消息入队（模型可消费）后记账：记录"已通知该 mode"。
///
/// 只在 ACP `permission_mode_notice_if_changed` 判定有通知、且该通知已随
/// agent_input 推入 v2 MessageQueue 时调用（见 [`ModeNoticeBooking`]）。
pub fn mark_permission_mode_notified(last_notified: &AtomicU8, mode: PermissionMode) {
    last_notified.store(mode as u8, Ordering::Relaxed);
}

// ── Intercept Request parameter object ─────────────────────────────────────

/// 命令拦截请求（参数对象，避免 12 个位置参数）。
///
/// L5 依赖反转：`peri_config`（ACP provider 配置）不进入本结构——compact
/// 配置经 [`InterceptRequest::compact_config_loader`] 注入闭包按
/// `load_compact_config` 语义预填（env overrides 每轮重新应用）；
/// 命令注册表查找经 [`InterceptRequest::command_lookup`] 注入（ACP 协议面
/// 注册表，`default_prompt_command_registry` 语义）。
pub struct InterceptRequest<'a> {
    // ── 消息上下文 ──
    pub content: &'a MessageContent,
    pub history: &'a [BaseMessage],
    // ── 会话上下文 ──
    pub cwd: &'a str,
    pub session_id: &'a str,
    pub cancel: &'a CancellationToken,
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    pub thread_id: Option<String>,
    // ── 运行时服务 ──
    pub event_sink: &'a Arc<dyn EventSink>,
    pub auxiliary_model: &'a Option<Arc<dyn peri_model::Model>>,
    // ── 注入面（L5 依赖反转）──
    /// 命令注册表查找（ACP 协议面注册表；`None` = 未注册/非 Immediate）。
    pub command_lookup: CommandLookupFn,
    /// compact 配置装载（ACP 侧 `load_compact_config` 语义，含 env overrides）。
    pub compact_config_loader: Arc<dyn Fn() -> CompactConfig + Send + Sync>,
}

/// 命令拦截：检查 content 是否为 Immediate 类型 slash 命令。
///
/// 返回 `Some(PromptResult)` 表示已处理（agent 不构建）；
/// 返回 `None` 表示继续走 agent 管线。
///
/// [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
/// 否则 TUI 界面永久卡在 loading 状态（issue_2026-05-29-immediate-command-missing-push-done）。
pub async fn intercept_immediate_command(req: InterceptRequest<'_>) -> Option<PromptResult> {
    let text = req.content.text_content();
    let stripped = text.strip_prefix('/')?;
    if stripped.is_empty() {
        return None;
    }

    // 命令注册表查找经注入闭包（ACP 协议面注册表；语义与迁移前
    // `default_prompt_command_registry().find` 一致——接收已 strip `/`
    // 前缀的命令文本，命令名 + 参数在闭包内解析）。
    let (cmd, args) = (req.command_lookup)(stripped)?;
    if cmd.kind() != CommandKind::Immediate {
        // Passthrough/Transform → fall through to normal agent flow
        return None;
    }

    tracing::debug!(
        command = %cmd.name(),
        history_len = req.history.len(),
        "Immediate command intercepted"
    );
    let ctx = CommandContext {
        session_id: req.session_id.to_string(),
        history: req.history.to_vec(),
        cwd: req.cwd.to_string(),
        // L5：compact 配置由装配点预填（env overrides 每轮重新应用，
        // 语义与原 compact_pipeline::load_compact_config 一致）。
        compact_config: (req.compact_config_loader)(),
        auxiliary_model: req.auxiliary_model.clone(),
        event_sink: req.event_sink.clone(),
        args: args.to_string(),
        cancel_token: req.cancel.clone(),
        thread_store: req.thread_store,
        thread_id: req.thread_id,
    };
    let result = tokio::select! {
        r = cmd.execute(ctx) => r,
        _ = req.cancel.cancelled() => {
            tracing::info!(session_id = %req.session_id, "Immediate command cancelled");
            CommandResult {
                messages: req.history.to_vec(),
                stop_reason: PromptStopReason::Cancelled,
            }
        }
    };
    // Immediate 命令跳过 agent event pump，必须手动发送 push_done
    // 通知 TUI agent 执行完成，否则界面永久卡在 loading 状态。
    // 命令 turn 无 request_id（None），但仍是前台 Turn 终态。
    req.event_sink
        .push_done(
            req.session_id,
            result.stop_reason.as_wire(),
            None,
            DoneKind::Turn,
        )
        .await;
    Some(PromptResult {
        messages: result.messages,
        ok: true,
        stop_reason: result.stop_reason,
        history_replaced_by_compaction: false,
        recall_items: Vec::new(),
    })
}

// ── Spawn Pump Request parameter object ─────────────────────────────────────

/// 事件泵启动请求（参数对象）。
pub struct SpawnPumpRequest {
    /// 事件订阅端口（ACP 适配层包装 Controller 订阅；泵消费广播并按
    /// session_id 过滤——事件三层化出口：发射点统一经
    /// `EventPublisher`，泵经 [`EventSubscriber::recv`] 消费）。
    pub subscription: Box<dyn EventSubscriber>,
    /// 事件发射点集合的关闭信号：所有发射点（forwarder / v1 直发）结束、
    /// `event_tx` 全部 drop 时触发（`closed()`），泵随后 drain 广播在途事件。
    pub event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    pub stop_reason_rx: oneshot::Receiver<PromptStopReason>,
    pub sink: Arc<dyn EventSink>,
    pub session_id: String,
    pub effective_context_window: u32,
    /// Langfuse trace 启动闭包（L5：ACP 侧捕获 tracer + 本轮输入，pump 任务
    /// 开头调用 `on_turn_start`——trace 语义与迁移前一致）。
    pub langfuse_on_turn_start: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Langfuse trace 收尾闭包（L5：ACP 侧捕获 tracer，构造 JoinHandle 后
    /// 由调用方在 pump_done 之后 drop——fire-and-forget，不得阻塞管线）。
    pub langfuse_on_turn_end: Option<LangfuseEndFn>,
    /// 本轮 prompt 的 requestId——push_done 时透传回带（TUI stale 判定用）。
    pub request_id: Option<String>,
}

/// 后台事件泵句柄，通过 oneshot channel 与 pump_done_rx 配对。
pub struct PumpHandle {
    pub pump_done_rx: oneshot::Receiver<()>,
}

/// 单条事件的处理（Langfuse error_kind 捕获 / bg callback unstable 事件 /
/// 协议化推送）。事件循环与 drain 分支共用，保证两条路径处理语义一致。
async fn pump_process(
    exec_event: &ExecutorEvent,
    last_error: &mut Option<String>,
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    effective_context_window: u32,
) {
    // Capture error_kind from TurnEnded for on_turn_end at pump tail
    if let ExecutorEvent::TurnEnded { error_kind, .. } = exec_event {
        *last_error = error_kind.as_ref().map(|k| format!("{:?}", k));
    }

    // 4. bg callback: MessageAdded → TUI flush-then-push.
    //    agent ReAct 循环在消费 MQ Defer 消息时通过 EventBus 发出
    //    SyntheticUserMessage → mapper → ExecutorEvent::MessageAdded。
    //    TUI bridge 收到 BgCallbackBubble 后会先 flush current_turn 到
    //    committed，再 push bg callback，把同一轮 TurnDone 的 AI 内容
    //    分割为「bg 前」和「bg 后」两段。
    if matches!(exec_event, ExecutorEvent::MessageAdded(_)) {
        if let ExecutorEvent::MessageAdded(msg) = exec_event {
            let text = msg.content();
            sink.push_unstable_event(
                session_id,
                "bg-callback-user-message".to_string(),
                serde_json::json!({ "text": text }),
            )
            .await;
        }
    }

    sink.push_event(session_id, exec_event, effective_context_window)
        .await;
}

/// 启动主事件泵任务。
///
/// 任务循环（事件三层化：发射点 → [`EventPublisher`] → 本泵订阅）：
/// 1. trace_start → 订阅事件流（按 session_id 过滤）→ 协议化推送 sink
/// 2. 发射点全部结束（`event_rx.closed()`）→ drain 广播在途事件 → 退出事件循环
/// 3. trace_end + push_done → signal pump completion（在 Langfuse flush 之前）
/// 4. Langfuse flush（fire-and-forget，不得阻塞管线）
pub fn spawn_event_pump(req: SpawnPumpRequest) -> PumpHandle {
    let SpawnPumpRequest {
        mut subscription,
        mut event_rx,
        stop_reason_rx,
        sink,
        session_id,
        effective_context_window,
        langfuse_on_turn_start,
        langfuse_on_turn_end,
        request_id,
    } = req;

    let (pump_done_tx, pump_done_rx) = oneshot::channel();

    if langfuse_on_turn_end.is_some() {
        debug!(session_id = %session_id, "Langfuse tracer received for turn");
    }

    tokio::spawn(async move {
        // Start Langfuse trace
        if let Some(ref f) = langfuse_on_turn_start {
            f();
        }

        let mut last_error: Option<String> = None;

        loop {
            tokio::select! {
                biased;
                msg = subscription.recv() => {
                    match msg {
                        Ok(m) => {
                            // 订阅是全局广播：只处理本 session 的事件
                            if m.envelope.session_id == session_id {
                                if let Some(ev) = m.event {
                                    pump_process(
                                        &ev, &mut last_error, &sink, &session_id,
                                        effective_context_window,
                                    ).await;
                                }
                            }
                        }
                        Err(SubscriptionError::Lagged(n)) => {
                            tracing::warn!(n, "event subscription lagged, events dropped");
                        }
                        Err(SubscriptionError::Closed) => break,
                    }
                }
                ev = event_rx.recv() => {
                    match ev {
                        // 防御分支：event_tx 的遗留直发（如有遗漏的发送点）
                        Some(exec_event) => {
                            pump_process(
                                &exec_event, &mut last_error, &sink, &session_id,
                                effective_context_window,
                            ).await;
                        }
                        None => {
                            // 发射点集合全部结束（event_tx 全 drop）：drain 广播中
                            // 已入 buffer 的在途事件（broadcast send 同步入 buffer，
                            // 关闭后到达的事件与 pump 退出后的丢弃语义一致——
                            // 对应迁移前 close_channel 后 forwarder 检查 None 丢弃）。
                            loop {
                                match subscription.try_recv() {
                                    Ok(Some(m)) if m.envelope.session_id == session_id => {
                                        if let Some(ev) = m.event {
                                            pump_process(
                                                &ev, &mut last_error, &sink, &session_id,
                                                effective_context_window,
                                            ).await;
                                        }
                                    }
                                    Ok(Some(_)) => {}
                                    Ok(None) => break,
                                    Err(SubscriptionError::Lagged(n)) => {
                                        tracing::warn!(n, "event subscription lagged, events dropped");
                                        break;
                                    }
                                    Err(SubscriptionError::Closed) => break,
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }

        // End Langfuse trace and flush（构造 JoinHandle；drop = detach，不阻塞管线）
        let langfuse_flush = langfuse_on_turn_end.as_ref().and_then(|f| f(last_error));

        // Resolve stop_reason from the oneshot channel set by executor
        let stop_reason = stop_reason_rx.await.unwrap_or(PromptStopReason::EndTurn);
        sink.push_done(
            &session_id,
            stop_reason.as_wire(),
            request_id.as_deref(),
            DoneKind::Turn,
        )
        .await;

        // Signal pump completion BEFORE Langfuse flush.
        // Langfuse is telemetry — it must never block the execution pipeline.
        // Without this, a slow/unreachable Langfuse API blocks pump_done_tx,
        // which blocks wait_for_pump(), which blocks run_session_loop() from
        // returning, which holds the prompt_lock and prevents the next prompt
        // from starting. Ctrl+C can't recover because the new prompt's cancel
        // is a fresh token.
        let _ = pump_done_tx.send(());

        // Langfuse flush: fire-and-forget. The spawned task runs independently;
        // worst-case it blocks for ~150s (HTTP 30s × 3 retries + backoff) then
        // logs warnings. The pump has already signaled completion above, so this
        // never blocks the execution pipeline.
        drop(langfuse_flush);
    });

    PumpHandle { pump_done_rx }
}

// ── Collect Result Request parameter object ─────────────────────────────────

/// 结果收集请求（参数对象）。
pub struct CollectRequest<'a> {
    pub event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    pub pump_handle: PumpHandle,
    pub session_id: &'a str,
    pub exec_outcome: ExecOutcome,
}

/// 最终结果收集：close channel → 等待 pump drain → 提取 recall items。
///
/// 顺序约束：必须先 close event_tx，pump 才能退出 recv 循环；然后等待 pump_done。
pub async fn collect_result(req: CollectRequest<'_>) -> PromptResult {
    let CollectRequest {
        event_tx,
        pump_handle,
        session_id,
        mut exec_outcome,
    } = req;

    close_channel(event_tx);
    wait_for_pump(pump_handle.pump_done_rx, session_id).await;

    let recall_items = exec_outcome.agent_state.drain_recall();
    PromptResult {
        messages: exec_outcome.agent_state.into_messages(),
        ok: exec_outcome.ok,
        stop_reason: exec_outcome.stop_reason,
        history_replaced_by_compaction: exec_outcome.history_replaced_by_compaction,
        recall_items,
    }
}

pub fn close_channel(
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
) {
    let mut tx_guard = event_tx.lock();
    *tx_guard = None;
}

pub async fn wait_for_pump(pump_done_rx: oneshot::Receiver<()>, session_id: &str) {
    match tokio::time::timeout(std::time::Duration::from_secs(10), pump_done_rx).await {
        Ok(Ok(())) => debug!(session_id, "Event pump done"),
        Ok(Err(_)) => error!(session_id, "Event pump done channel closed unexpectedly"),
        Err(_) => error!(
            session_id,
            "Event pump timed out (10s) — Langfuse flush may have blocked push_done"
        ),
    }
}

// ── v2 stages 装配与 ReAct 循环驱动 ────────────────────────────────────────

/// stage 装配请求（注入 `StageBuildFn` 的输入；L5：自原
/// `build_and_execute_agent_v2` 的 stage 相关参数打包）。
///
/// `langfuse_tracer` 不进入本结构——stage 构建的 Langfuse bridge 由 ACP
/// 装配面闭包捕获（`StageBuildInput::langfuse_bridge_factory` 注入点）。
#[allow(clippy::type_complexity)]
pub struct StageBuildRequest {
    pub cached_llm: Option<CachedLlmInstances>,
    pub system_prompt: String,
    pub subagent_system_prompt: Option<String>,
    pub frozen: FrozenData,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub agent_overrides: Option<peri_acp_types::agents::AgentOverrides>,
    pub preload_skills: Vec<String>,
    pub child_handler_factory: Option<ChildHandlerFactory>,
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub thread_persistence: ThreadPersistence,
    pub goal_controller: Option<Arc<dyn GoalController>>,
    pub task_manager: Option<Arc<AgentTaskManager>>,
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
}

/// stage 装配注入面（ACP 侧从 `SessionContext` 投影 `StageBuildInput` 并补齐
/// LLM 构造 / 渲染 / 观测注入后调用 stage 装配本体）。
pub type StageBuildFn =
    Arc<dyn Fn(StageBuildRequest) -> (V2AgentOutput, Option<CachedLlmInstances>) + Send + Sync>;

/// v2 执行请求（L5：原 `build_and_execute_agent_v2` 22 参数对象化；
/// ACP 特有构造全部经注入面接入，本模块只消费契约化输入）。
#[allow(clippy::type_complexity)]
pub struct V2ExecuteRequest {
    // ── 会话数据 ──
    pub session_id: String,
    pub cwd: String,
    pub cancel: CancellationToken,
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    pub thread_id: Option<String>,
    pub agent_input: AgentInput,
    pub history: Vec<BaseMessage>,
    pub cached_llm: Option<CachedLlmInstances>,
    pub task_manager: Option<Arc<dyn TaskManager>>,
    pub mode_notice_booking: Option<ModeNoticeBooking>,
    /// 当前 turn 仅供模型读取、不得进入 ThreadStore 或对外历史的运行时提醒。
    pub runtime_reminder: Option<String>,
    pub continuation: bool,
    // ── stage 装配输入（透传 StageBuildRequest）──
    pub system_prompt: String,
    pub subagent_system_prompt: Option<String>,
    pub frozen: FrozenData,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub agent_overrides: Option<peri_acp_types::agents::AgentOverrides>,
    pub preload_skills: Vec<String>,
    pub child_handler_factory: Option<ChildHandlerFactory>,
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub thread_persistence: ThreadPersistence,
    pub goal_controller: Option<Arc<dyn GoalController>>,
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    // ── 注入面（L5 依赖反转）──
    /// 事件发射端口（ACP/Controller 适配层；Phase 2/3/4/7/9 统一发射点）。
    pub publisher: Arc<dyn EventPublisher>,
    /// stage 装配注入面（ACP 侧投影 `StageBuildInput` + 补齐注入）。
    pub stage_build: StageBuildFn,
    /// LLM 缓存回写（ACP 侧 AgentPool；Phase 1 装配产物入池）。
    pub store_llm: Arc<dyn Fn(CachedLlmInstances) + Send + Sync>,
    /// cancel cascade 子 agent（ACP 侧 SessionManager；循环失败后触发）。
    pub cancel_cascade: Arc<dyn Fn(&str) + Send + Sync>,
    /// EventBus forwarder 启动器（ACP 侧持有 Langfuse bridge 构造）。
    pub forwarder_launcher: ForwarderLauncherFn,
}

/// 通过注入的 stage 装配面构造 StageContext，再由 [`run_react_loop`] 驱动循环
/// （P5 后的单一执行路径）。
///
/// 关键设计：
/// - LLM/middleware 装配由 ACP 注入面完成（构造 `AgentComponents` → `StageContext`）
/// - 工具执行由 `stages/tool_dispatch` 完成（每轮从 `shared_tools` 取）
/// - 事件出口：v2 stages 通过 EventBus emit 三层事件（Render/State/Observe），
///   本函数经注入的 forwarder 启动器将其映射为 `ExecutorEvent` 并统一发射，
///   复用 event_tx / pump 管线
/// - 历史消息：seed 到 transcript；用户输入：作为 Prompt push 到 v2 queue
///
/// 调用前已完成副作用（register/deregister、event_handler、goal_controller）。
/// 所有副作用与 v1 一致。
pub async fn build_and_execute_agent_v2(req: V2ExecuteRequest) -> ExecOutcome {
    use peri_acp_types::session::{MessageKind, MessageSource as V2MessageSource, QueuedMessage};

    // Phase 1: build StageContext（内部消费 AgentComponents）
    let concrete_tm: Option<Arc<AgentTaskManager>> = req.task_manager.clone().and_then(|tm| {
        let tm_any: Arc<dyn std::any::Any + Send + Sync> =
            tm as Arc<dyn std::any::Any + Send + Sync>;
        tm_any.downcast::<AgentTaskManager>().ok()
    });
    let (v2_out, new_cache) = (req.stage_build)(StageBuildRequest {
        cached_llm: req.cached_llm,
        system_prompt: req.system_prompt,
        subagent_system_prompt: req.subagent_system_prompt,
        frozen: req.frozen,
        event_handler: req.event_handler,
        agent_overrides: req.agent_overrides,
        preload_skills: req.preload_skills,
        child_handler_factory: req.child_handler_factory,
        auxiliary_model: req.auxiliary_model,
        thread_persistence: req.thread_persistence,
        goal_controller: req.goal_controller,
        task_manager: concrete_tm,
        on_bg_complete: req.on_bg_complete,
    });
    if let Some(cache) = new_cache {
        (req.store_llm)(cache);
    }

    // Phase 2: bg event pump（复用 V2AgentOutput.bg_event_rx）
    // 事件三层化：发射点统一经 `EventPublisher`（身份：v2 循环 turn_id / 主
    // agent_id——bg 子 agent 事件归属当前 turn），消费端为主 pump（已在
    // run_session_loop 中订阅，按 session_id 过滤推送）。
    {
        let mut bg_event_rx = v2_out.bg_event_rx;
        let publisher = Arc::clone(&req.publisher);
        let bg_session_id = req.session_id.clone();
        let bg_turn_id = v2_out.context.turn_id().to_string();
        let bg_agent_id = v2_out.context.session.agent_id.to_string();
        tokio::spawn(async move {
            let mut bg_event_count: u64 = 0;
            while let Some(bg_event) = bg_event_rx.recv().await {
                bg_event_count += 1;
                let source = UnstampedEvent::new(
                    bg_turn_id.clone(),
                    bg_agent_id.clone(),
                    None,
                    EventDeliveryClass::Critical,
                );
                publisher.publish_event(&bg_session_id, &source, bg_event);
            }
            tracing::debug!(
                total = bg_event_count,
                "bg-event-pump: all senders dropped, exiting"
            );
        });
    }

    // Phase 3: todo forwarder（同 v1，复用 V2AgentOutput.todo_rx）
    {
        let mut todo_rx = v2_out.todo_rx;
        let publisher = Arc::clone(&req.publisher);
        let sid = req.session_id.clone();
        tokio::spawn(async move {
            while let Some(todos) = todo_rx.recv().await {
                let entries: Vec<peri_acp_types::event::TodoEntry> = todos
                    .into_iter()
                    .map(|t| peri_acp_types::event::TodoEntry {
                        content: t.content,
                        active_form: t.active_form,
                        status: match t.status {
                            peri_acp_types::tools::TodoStatus::Pending => {
                                peri_acp_types::event::TodoStatus::Pending
                            }
                            peri_acp_types::tools::TodoStatus::InProgress => {
                                peri_acp_types::event::TodoStatus::InProgress
                            }
                            peri_acp_types::tools::TodoStatus::Completed => {
                                peri_acp_types::event::TodoStatus::Completed
                            }
                        },
                    })
                    .collect();
                // todo 更新是事件流的一部分，发射点统一经 EventPublisher
                let source = UnstampedEvent::new(
                    String::new(),
                    String::new(),
                    None,
                    EventDeliveryClass::Critical,
                );
                publisher.publish_event(&sid, &source, ExecutorEvent::TodoUpdate(entries));
            }
        });
    }

    // Phase 4: EventBus forwarder（v2 → ExecutorEvent）
    // 通过 tokio::select! 同时排空 render / state / observe 三层通道，
    // 将 v2 事件经 mapper_v2 映射为 ExecutorEvent，经注入的 launcher 启动
    // 转发任务（biased select 顺序不变量单点保持在 ACP 侧 forwarder）。
    // 注意：不直接 push 到 event_sink —— spawn_event_pump 已订阅事件流并
    // 负责推送 sink（含 push_done 同步）。直推会造成 TUI 双重渲染。
    //
    // [TRAP] TurnCompleted 在 render_tx 通道（与同迭代 TextChunk/ToolStarted/
    // ToolEnded 共享 FIFO），不能放回 state_tx：跨通道 biased select! 只保证
    // 单次迭代内的优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
    // TurnCompleted 被消费，污染 partial，渲染出"新文本在旧工具之前"的错乱。
    {
        let publisher = Arc::clone(&req.publisher);
        let sid = req.session_id.clone();
        let agent_id = v2_out.context.session.agent_id.to_string();
        (req.forwarder_launcher)(
            v2_out.event_handles,
            agent_id,
            Box::new(move |source, exec_ev| {
                publisher.publish_event(&sid, &source, exec_ev);
            }),
        );
    }

    // Phase 5: seed transcript（history 作为 ancestor 之外的自有消息）
    // 首轮用户 turn 判定需在 history move 前捕获（Phase 5.9 使用）。
    let is_first_user_turn = !req.continuation && req.history.is_empty();
    {
        let transcript_arc = v2_out.session.transcript();
        let mut transcript = transcript_arc.write();
        transcript.append_batch(req.history);
    }

    // Phase 5.5: restore compact flags from persistence (if available)
    {
        if let (Some(store), Some(tid)) = (req.thread_store.as_ref(), req.thread_id.as_ref()) {
            match store.load_message_flags(tid).await {
                Ok(flags) if !flags.is_empty() => {
                    let transcript_arc = v2_out.session.transcript();
                    let mut transcript = transcript_arc.write();
                    transcript.set_flags_batch(flags);
                    tracing::debug!(
                        thread_id = %tid,
                        "Phase 5.5: restored compact flags from persistence"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(
                        thread_id = %tid,
                        error = %e,
                        "Phase 5.5: failed to load compact flags"
                    );
                }
            }
        }
    }

    // Phase 6: push 用户输入到 v2 queue（Receive 阶段消费）
    // [AsyncContinuation] continuation 内部续跑不 push 空 user prompt——
    // 空 human 不进入 transcript（保持 keepgoing 的"不写入空 human"约束由
    // 显式分支承担，而非复用 keepgoing 语义）；loop 仅消费已 route 的
    // Defer/Info 消息（bg 结果等）。
    // [P3/D2] 记账点：通知文本随本条消息推入模型可见的 v2 MessageQueue 后，
    // 才标记"已通知该 mode"。入队前失败/取消不记账——下一 turn 重新检测仍会
    // 生成通知（可重复重试，恰好可见一次）；已入队的消息由 Receive drain_all
    // 消费进 transcript，不会重复注入也不会丢失。
    if !req.continuation {
        v2_out.context.session.queue.push(QueuedMessage::new(
            MessageKind::Prompt,
            V2MessageSource::UserInput,
            BaseMessage::human(req.agent_input.content),
        ));
        if let Some(reminder) = req.runtime_reminder.as_deref() {
            v2_out.context.session.queue.push(QueuedMessage::new(
                MessageKind::Info,
                V2MessageSource::SystemInjected,
                BaseMessage::human(reminder),
            ));
        }
        if let Some(booking) = &req.mode_notice_booking {
            mark_permission_mode_notified(&booking.last_notified, booking.mode);
        }

        // Phase 6.2: 首轮用户 turn 的一次性受控通知（MCP 概览等）。
        // 仅在首个模型可见 turn（history 为空且非 continuation）触发：收集
        // middleware chain 的 `first_turn_reminder` 非空贡献，作为 Info 消息
        // （`<system-reminder>` 包裹，见 append_messages_to_transcript）在用户
        // Prompt **之后**入队——Receive drain 顺序为 user 输入在前、reminder
        // 在后（"加入到 user prompt"语义，不抢在用户输入前）。
        // 纯生成无记账：入队前失败/取消无副作用，下个首 turn 重新生成。
        if is_first_user_turn {
            let mut cx = AgentContext::from_stage(&v2_out.context);
            match v2_out
                .context
                .runtime
                .middleware_chain
                .run_first_turn_reminders(&mut cx)
                .await
            {
                Ok(reminders) if !reminders.is_empty() => {
                    for text in reminders {
                        v2_out.context.session.queue.push(QueuedMessage::new(
                            MessageKind::Info,
                            V2MessageSource::SystemInjected,
                            BaseMessage::human(text),
                        ));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "[v2] first_turn_reminder hooks failed");
                }
            }
        }
    }

    // Phase 6.5: clone recall_buffer 的 Arc，便于 Phase 8.5 在 context 被
    // run_react_loop 消费后仍可访问累积的 recall。
    let recall_buffer = Arc::clone(&v2_out.context.recall_buffer);

    // Phase 7: 运行 v2 ReAct 循环（max_iterations 与 v1 一致 = 500）
    // langfuse v2: capture turn_id before move, emit TurnStarted
    let loop_turn_id = v2_out.context.turn_id().to_string();
    {
        // TurnStarted 是事件流的一部分，发射点统一经 EventPublisher
        // （v1 直发路径的身份：turn_id 取自 v2 循环，agent_id 为空降级）。
        let source = UnstampedEvent::new(
            loop_turn_id.clone(),
            String::new(),
            None,
            EventDeliveryClass::Critical,
        );
        req.publisher.publish_event(
            &req.session_id,
            &source,
            ExecutorEvent::TurnStarted {
                turn_id: loop_turn_id.clone(),
                session_id: req.session_id.clone(),
            },
        );
    }
    let turn_started_at = v2_out.context.session.turn.started_at;
    let loop_result = run_react_loop(v2_out.context, 500).await;

    // 每个用户 Turn 都必须有一条可持久恢复的 Assistant Turn 记录。成功回答和
    // 部分流错误会在 stage 内直接附加元数据；完全无输出、工具后失败或外层取消
    // 在这里补一条 metadata-only 消息。该消息会被模型适配器过滤。
    {
        let transcript = v2_out.session.transcript();
        let has_terminal_record = transcript
            .read()
            .durable_visible_messages()
            .last()
            .and_then(|message| message.turn_metadata())
            .is_some();
        if !has_terminal_record {
            let (status, incomplete, error_kind) = match &loop_result {
                LoopResult::Completed => ("completed", false, None),
                LoopResult::Interrupted => ("cancelled", true, Some("cancelled".to_owned())),
                LoopResult::Error(error) if matches!(error, AgentError::Interrupted) => {
                    ("cancelled", true, Some("cancelled".to_owned()))
                }
                LoopResult::Error(error) => {
                    ("failed", true, Some(error.user_facing_code().to_owned()))
                }
            };
            transcript
                .write()
                .append(BaseMessage::ai("").with_turn_metadata(
                    status,
                    u64::try_from(turn_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    incomplete,
                    error_kind,
                ));
        }
    }

    // Phase 8: 从 transcript 提取最终消息列表，构造 AgentState（兼容下游 PromptResult）
    // 前置：显式 flush 剩余积压，确保最终回答已落库。Drop 层 Shutdown 优雅关闭是
    // 根因兜底（覆盖全部 6 个 run_react_loop 调用方），此处是主路径双保险——
    // 让会话恢复方在 turn 结束即可读到完整历史，不依赖 drop 时序。失败不阻断
    // 内存路径（后续 Drop 仍会尝试 flush）。
    // [SAFE] 先在 guard 作用域内同步提取 Send 的 writer 通道句柄（guard 语句结束即
    // drop，不跨 await），再经关联函数 `flush_via_tx` 异步等待 barrier——调用链
    // future 不持有 parking_lot guard，保持 Send（peri-tui 在 tokio::spawn 中调用本链）。
    let flush_tx = v2_out.session.transcript().read().persist_tx_handle();
    if let Some(tx) = flush_tx {
        if let Err(e) = MessageTranscript::flush_via_tx(&tx).await {
            tracing::warn!(session_id = %req.session_id, error = %e, "[v2] phase 8 transcript flush failed");
        }
    }
    let (messages, history_replaced_by_compaction) = {
        let transcript = v2_out.session.transcript();
        let transcript = transcript.read();
        let messages: Vec<BaseMessage> = transcript
            .durable_visible_messages()
            .into_iter()
            .cloned()
            .collect();
        (messages, transcript.full_compaction_committed())
    };
    let mut agent_state = AgentState::with_messages(req.cwd.clone(), messages);
    agent_state.set_context("session_id", &req.session_id);
    agent_state.set_context("run_id", uuid::Uuid::now_v7().to_string());

    // Phase 8.5: 把 v2 recall_buffer（middleware hook 期间累积）灌入 agent_state。
    // 下游 collect_result() 调用 agent_state.drain_recall() 取出 recall_items，
    // 必须先迁移到 agent_state 才能复用 v1 的 drain 路径。
    //
    // v2 路径下 middleware hook 在临时 AgentState 上 push_recall（见
    // middleware_runner::restore_from_agent_state），restore 时 drain 到
    // StageContext.recall_buffer；循环结束后（context 已被 run_react_loop
    // 消费）从 Phase 6.5 clone 的 Arc 取回累积的 recall。
    {
        let recalls: Vec<String> = recall_buffer.write().drain(..).collect();
        for r in recalls {
            agent_state.push_recall(r);
        }
    }

    // Phase 9: 映射 LoopResult → ExecOutcome
    let (ok, stop_reason) = match loop_result {
        LoopResult::Completed => (true, PromptStopReason::EndTurn),
        LoopResult::Interrupted => (false, PromptStopReason::Cancelled),
        LoopResult::Error(ref e) => {
            error!(session_id = %req.session_id, error = %e, "[v2] loop failed");
            // 对非 Interrupted/MaxIterations 的致命错误，通知 TUI 显示红色错误提示
            // issue: spec/issues/2026-07-22-llm-api-error-silently-swallowed-in-tui.md
            if !matches!(e, AgentError::Interrupted)
                && !matches!(e, AgentError::MaxIterationsExceeded(_))
                && !req.cancel.is_cancelled()
            {
                // 发射点统一经 EventPublisher
                let source = UnstampedEvent::new(
                    String::new(),
                    String::new(),
                    None,
                    EventDeliveryClass::Critical,
                );
                req.publisher.publish_event(
                    &req.session_id,
                    &source,
                    ExecutorEvent::AgentExecutionFailed {
                        code: e.user_facing_code().to_owned(),
                        message: e.user_facing_message(),
                    },
                );
            }
            let reason = if req.cancel.is_cancelled() || matches!(e, AgentError::Interrupted) {
                PromptStopReason::Cancelled
            } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
                PromptStopReason::MaxTurnRequests
            } else {
                PromptStopReason::EndTurn
            };
            (false, reason)
        }
    };

    // langfuse v2: emit TurnEnded
    {
        let (status, error_kind) = match loop_result {
            LoopResult::Completed => (TurnStatus::Done, None),
            LoopResult::Interrupted => (TurnStatus::Interrupted, Some(TurnErrorKind::Interrupted)),
            LoopResult::Error(ref e) => {
                let kind = if matches!(e, AgentError::Interrupted) {
                    TurnErrorKind::Interrupted
                } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
                    TurnErrorKind::MaxIterations
                } else {
                    TurnErrorKind::LlmFailure
                };
                (TurnStatus::Error, Some(kind))
            }
        };
        // 发射点统一经 EventPublisher（TurnEnded 是事件流终末事件）
        let source = UnstampedEvent::new(
            loop_turn_id.clone(),
            String::new(),
            None,
            EventDeliveryClass::Critical,
        );
        req.publisher.publish_event(
            &req.session_id,
            &source,
            ExecutorEvent::TurnEnded {
                turn_id: loop_turn_id,
                session_id: req.session_id.clone(),
                status,
                error_kind,
            },
        );
    }

    // Cancel cascade children when this agent is cancelled
    if stop_reason == PromptStopReason::Cancelled {
        (req.cancel_cascade)(&req.session_id);
    }

    ExecOutcome {
        ok,
        stop_reason,
        history_replaced_by_compaction,
        agent_state,
    }
}

#[cfg(test)]
#[path = "executor_helpers_test.rs"]
mod tests;
