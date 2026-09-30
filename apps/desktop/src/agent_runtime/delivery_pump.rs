//! Session 投递泵：ACP 帧、Runtime 事件与后台任务完成通知的桌面侧推送循环。
//!
//! 本模块只做"已通过投递边界的事件 → 桌面 sink"的搬运与观测记录，
//! 不持有任何会话状态；泵的生命周期由 `AgentRuntime` 的世代与门管理。

use super::*;

/// 实时增量攒批的投递窗口；对齐 ZCode v4 `continuous` 档位（约 33 帧/秒），
/// 在打字机观感无感的前提下把跨进程事件频率与 CPU 负载压低一个量级。
const DELTA_FLUSH_WINDOW_MS: u64 = 30;
/// 单次攒批允许的最大草稿条数；防止极端风暴下的对象数量反弹。
const DELTA_FLUSH_MAX_DELIVERIES: usize = 200;
/// 单次攒批允许的近似字节上限；按增量文本长度累计，防止单帧体积膨胀。
const DELTA_FLUSH_MAX_BYTES: usize = 64 * 1024;

/// 实时流式增量的类别；消息与思考块互不拼接。
#[derive(Clone, Copy, Eq, PartialEq)]
enum StreamDeltaKind {
    Message,
    Thought,
}

/// 投递泵内的实时增量攒批缓冲；只缓冲可安全拼接的流式文本草稿。
///
/// 任何 tool/权限/生命周期草稿都是屏障：缓冲被立即投出，屏障自身零延迟
/// 单独投递，保证交互语义不因攒批而延迟。
#[derive(Default)]
struct DeltaFlushBuffer {
    /// 尚未投递的实时草稿；相邻同 Turn/Agent 的同类增量已拼接进尾部条目。
    drafts: Vec<DeliveryDraft>,
    /// 随缓冲下一次投递转发的根 Turn 终态通知。
    notices: Vec<RootTaskTerminalNotice>,
    /// 缓冲草稿的近似字节量，用于触发上限 flush。
    bytes: usize,
    /// 首条增量入队时间加投递窗口；`None` 表示没有待 flush 内容。
    flush_at: Option<tokio::time::Instant>,
}

impl DeltaFlushBuffer {
    fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }

    fn reached_limit(&self) -> bool {
        self.drafts.len() >= DELTA_FLUSH_MAX_DELIVERIES || self.bytes >= DELTA_FLUSH_MAX_BYTES
    }
}

/// 提取实时草稿中可安全拼接的流式文本增量及其归属。
fn stream_delta_parts(draft: &DeliveryDraft) -> Option<(&str, &str, StreamDeltaKind, &str)> {
    let DeliveryDraft::SessionUpdate {
        turn_id,
        source_agent_id,
        update,
        ..
    } = draft
    else {
        return None;
    };
    let (kind, text) = match update.as_ref() {
        keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk) => {
            (StreamDeltaKind::Message, chunk_text(&chunk.content)?)
        }
        keencode_acp::schema::SessionUpdate::AgentThoughtChunk(chunk) => {
            (StreamDeltaKind::Thought, chunk_text(&chunk.content)?)
        }
        _ => return None,
    };
    Some((turn_id.as_deref()?, source_agent_id.as_deref()?, kind, text))
}

/// 只对纯文本内容块做拼接；其他内容块保持原样独立投递。
fn chunk_text(content: &keencode_acp::schema::ContentBlock) -> Option<&str> {
    match content {
        keencode_acp::schema::ContentBlock::Text(text) => Some(text.text.as_str()),
        _ => None,
    }
}

/// 把增量文本追加到缓冲尾部同类草稿；仅相邻同 Turn/Agent 的同类文本块合并。
///
/// 拼接保持投递语义等价：ACP chunk 允许任意分段，消费端只依赖最终拼接文本。
fn merge_delta_into_tail(
    tail: &mut DeliveryDraft,
    turn_id: &str,
    agent_id: &str,
    kind: StreamDeltaKind,
    delta: &str,
) -> bool {
    let DeliveryDraft::SessionUpdate {
        turn_id: tail_turn,
        source_agent_id: tail_agent,
        update,
        ..
    } = tail
    else {
        return false;
    };
    if tail_turn.as_deref() != Some(turn_id) || tail_agent.as_deref() != Some(agent_id) {
        return false;
    }
    let chunk = match (update.as_mut(), kind) {
        (
            keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk),
            StreamDeltaKind::Message,
        )
        | (
            keencode_acp::schema::SessionUpdate::AgentThoughtChunk(chunk),
            StreamDeltaKind::Thought,
        ) => chunk,
        _ => return false,
    };
    match &mut chunk.content {
        keencode_acp::schema::ContentBlock::Text(text) => {
            text.text.push_str(delta);
            true
        }
        _ => false,
    }
}

/// 尝试把实时草稿作为增量吸收进攒批缓冲；返回是否已吸收。
fn try_buffer_delta(pending: &mut DeltaFlushBuffer, draft: DeliveryDraft) -> bool {
    let Some((turn_id, agent_id, kind, delta)) = stream_delta_parts(&draft) else {
        return false;
    };
    let delta_len = delta.len();
    let merged = match pending.drafts.last_mut() {
        Some(tail) => merge_delta_into_tail(tail, turn_id, agent_id, kind, delta),
        None => false,
    };
    if !merged {
        if pending.flush_at.is_none() {
            pending.flush_at =
                Some(tokio::time::Instant::now() + Duration::from_millis(DELTA_FLUSH_WINDOW_MS));
        }
        pending.drafts.push(draft);
    }
    pending.bytes += delta_len;
    true
}

/// 把攒批缓冲一次性投出；失败冻结当前投递世代，成功后转发携带的终态通知。
fn flush_delta_buffer(
    session_id: &str,
    emitter: &Arc<dyn DeliveryEmitter>,
    sequence: &mut SessionSequence,
    pending: &mut DeltaFlushBuffer,
    lifecycle: &DeliveryLifecycle,
) -> Result<(), AgentRuntimeError> {
    let mut buffer = std::mem::take(pending);
    if buffer.drafts.is_empty() {
        // 通知不需要增量窗口；没有草稿时立即转发。
        for notice in buffer.notices.drain(..) {
            notify_root_task_terminal(emitter, Some(notice));
        }
        return Ok(());
    }
    match emit_batch(session_id, emitter, sequence, buffer.drafts) {
        Ok(_) => {
            for notice in buffer.notices {
                notify_root_task_terminal(emitter, Some(notice));
            }
            Ok(())
        }
        Err(error) => {
            lifecycle.mark_failed();
            Err(error)
        }
    }
}

/// 按序消化一个实时批次的草稿：增量合并进攒批缓冲，屏障草稿立即投递。
/// 终态通知跟随本批次之后的缓冲投递单元，保持「投递成功后才通知」的顺序。
/// 返回本批次消化结果；世代被冻结时携带拒绝原因。
fn enqueue_live_drafts(
    session_id: &str,
    emitter: &Arc<dyn DeliveryEmitter>,
    sequence: &mut SessionSequence,
    pending: &mut DeltaFlushBuffer,
    lifecycle: &DeliveryLifecycle,
    drafts: Vec<DeliveryDraft>,
    terminal_notice: Option<RootTaskTerminalNotice>,
) -> Result<(), AgentRuntimeError> {
    let batch_has_drafts = !drafts.is_empty();
    for draft in drafts {
        if stream_delta_parts(&draft).is_some() {
            try_buffer_delta(pending, draft);
            if pending.reached_limit() {
                flush_delta_buffer(session_id, emitter, sequence, pending, lifecycle)?;
            }
            continue;
        }
        // 屏障草稿：先投出已缓冲增量，屏障自身零延迟单独投递。
        flush_delta_buffer(session_id, emitter, sequence, pending, lifecycle)?;
        if let Some(error) = lifecycle.rejection() {
            return Err(error);
        }
        if let Err(error) = emit_batch(session_id, emitter, sequence, vec![draft]) {
            lifecycle.mark_failed();
            return Err(error);
        }
    }
    if let Some(notice) = terminal_notice {
        // 空投影批次保持完全静默：既无投递也不产生终态通知。
        if batch_has_drafts {
            pending.notices.push(notice);
        }
    }
    if !pending.notices.is_empty() && pending.drafts.is_empty() {
        flush_delta_buffer(session_id, emitter, sequence, pending, lifecycle)?;
    }
    match lifecycle.rejection() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 将已经通过桌面 ACP 投递边界的消息复制给当前 Web Session 连接。
///
/// `SessionUpdate` 信封当前只携带 Runtime delivery 序号，因此没有可用的
/// Journal 游标；Web adapter 会沿用该连接最近的权威水位。KeenCode 扩展事件
/// 自带 Journal 序号，可直接用于 gap/reconnect 游标。
pub(super) fn web_delivery_parts(delivery: &AcpDelivery) -> Option<(String, Option<u64>, Vec<u8>)> {
    let (session_id, journal_sequence) = match delivery {
        AcpDelivery::SessionUpdate { envelope } => (envelope.session_id().to_owned(), None),
        AcpDelivery::KeenCodeEvent { envelope } => (
            envelope.session_id().to_owned(),
            envelope.journal_sequence(),
        ),
        AcpDelivery::ClientRequest { .. } => return None,
    };
    let payload = serde_json::to_vec(delivery).ok()?;
    Some((session_id, journal_sequence, payload))
}

/// 从标准 Client Request 的 Session scope 读取定向投递所需的 Session 标识。
pub(super) fn client_request_session_id(request: &AcpClientRequestFrame) -> Option<String> {
    let value = serde_json::to_value(request).ok()?;
    value
        .pointer("/params/sessionId")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/params/session_id").and_then(Value::as_str))
        .map(str::to_owned)
}

/// 串行处理一个 Session 当前世代的全部桌面消息。
///
/// 实时增量草稿先进入攒批缓冲，按 `DELTA_FLUSH_WINDOW_MS` 惰性窗口批量投递；
/// 其余命令保持原顺序语义。恢复门内不攒批，仍由 `buffered_live` 缓存。
pub(super) async fn run_delivery_pump(
    session_id: String,
    emitter: Arc<dyn DeliveryEmitter>,
    mut receiver: mpsc::Receiver<DeliveryCommand>,
    mut recovering: bool,
    lifecycle: Arc<DeliveryLifecycle>,
) {
    let mut sequence = SessionSequence::new();
    let mut buffered_live = Vec::<BufferedLiveBatch>::new();
    let mut shutdown_acknowledged = None;
    let mut pending = DeltaFlushBuffer::default();
    loop {
        let command = if pending.is_empty() {
            receiver.recv().await
        } else {
            let flush_at = pending
                .flush_at
                .expect("非空攒批缓冲必须携带 flush 截止时间");
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(flush_at) => {
                    // 失败已在 flush 内冻结世代；后续命令由 rejection 短路。
                    if let Err(error) = flush_delta_buffer(&session_id, &emitter, &mut sequence, &mut pending, &lifecycle) {
                        tracing::error!(session_id, %error, "攒批窗口到期投递失败，投递世代已冻结");
                    }
                    continue;
                }
                command = receiver.recv() => command,
            }
        };
        let Some(command) = command else {
            // 发送端全部关闭；尽力投出已缓冲增量后结束当前世代。
            if let Err(error) = flush_delta_buffer(
                &session_id,
                &emitter,
                &mut sequence,
                &mut pending,
                &lifecycle,
            ) {
                tracing::error!(session_id, %error, "发送端已全部关闭，尾部增量投递失败");
            }
            break;
        };
        match command {
            DeliveryCommand::EmitBatch {
                drafts,
                terminal_notice,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else if recovering {
                    buffered_live.push(BufferedLiveBatch {
                        drafts,
                        terminal_notice,
                    });
                    Ok(())
                } else {
                    // 批次消化完成后再确认：屏障草稿的同步 flush 在 ack 前
                    // 完成；纯增量批次只进入攒批缓冲，不等待惰性窗口。
                    let outcome = enqueue_live_drafts(
                        &session_id,
                        &emitter,
                        &mut sequence,
                        &mut pending,
                        &lifecycle,
                        drafts,
                        terminal_notice,
                    );
                    let _ = acknowledged.send(outcome);
                    continue;
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::EmitReplayBatch {
                drafts,
                through_sequence,
                final_page,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else if !recovering {
                    Err(AgentRuntimeError::RuntimeOperationFailed)
                } else {
                    let replay_result = emit_batch(&session_id, &emitter, &mut sequence, drafts);
                    if final_page {
                        match replay_result {
                            Ok(history_delivery_sequence) => {
                                let mut retained = Vec::new();
                                let mut terminal_notices = Vec::new();
                                for buffered in buffered_live.drain(..) {
                                    let mut retained_batch = false;
                                    for draft in buffered.drafts {
                                        if draft_is_after_recovery_waterline(
                                            &draft,
                                            through_sequence,
                                        ) {
                                            retained_batch = true;
                                            retained.push(draft);
                                        }
                                    }
                                    if retained_batch && let Some(notice) = buffered.terminal_notice
                                    {
                                        terminal_notices.push(notice);
                                    }
                                }
                                let release_result =
                                    emit_batch(&session_id, &emitter, &mut sequence, retained)
                                        .map(|_| ());
                                if release_result.is_ok() {
                                    recovering = false;
                                    for notice in terminal_notices {
                                        notify_root_task_terminal(&emitter, Some(notice));
                                    }
                                }
                                release_result.map(|_| history_delivery_sequence)
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        replay_result
                    }
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::EmitClientRequest {
                connection_id,
                request,
                acknowledged,
            } => {
                let result = if let Some(error) = lifecycle.rejection() {
                    Err(error)
                } else {
                    // Client Request 属于屏障类投递：先清空攒批缓冲保持
                    // Session FIFO，再零延迟单独投递请求帧。
                    flush_delta_buffer(
                        &session_id,
                        &emitter,
                        &mut sequence,
                        &mut pending,
                        &lifecycle,
                    )
                    .and_then(|()| {
                        emitter.emit(&AcpDelivery::ClientRequest {
                            connection_id,
                            request: *request,
                        })
                    })
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::Shutdown { acknowledged } => {
                // 关闭前尽力投出已缓冲增量，避免窗口内的尾部增量丢失。
                // 失败只冻结世代并记录日志；关闭回执仍按设计返回成功，
                // 保证已知失败世代可以被安全替换（见 DeliveryLifecycle）。
                if let Err(error) = flush_delta_buffer(
                    &session_id,
                    &emitter,
                    &mut sequence,
                    &mut pending,
                    &lifecycle,
                ) {
                    tracing::error!(session_id, %error, "关闭前尾部增量投递失败，投递世代已冻结");
                }
                shutdown_acknowledged = Some(acknowledged);
                break;
            }
        }
    }
    lifecycle.mark_stopped();
    if let Some(acknowledged) = shutdown_acknowledged {
        let _ = acknowledged.send(Ok(()));
    }
}

/// 只有非取消的实时根 Turn 投影成功后才进入桌面通知边界。
pub(super) fn notify_root_task_terminal(
    emitter: &Arc<dyn DeliveryEmitter>,
    notice: Option<RootTaskTerminalNotice>,
) {
    let Some(notice) = notice else {
        return;
    };
    if notice.stop_reason == Some(TurnStopReason::Cancelled) {
        return;
    }
    emitter.notify_task_terminal(Some(&notice.task_title), notice.stop_reason);
}

/// 把 Session 唯一后台任务 Manager 的终态广播映射为无 Journal 游标的桌面事件。
pub(super) async fn run_background_task_completion_pump(
    runtime: Weak<AgentRuntime>,
    session_id: String,
    mut completions: tokio::sync::broadcast::Receiver<BackgroundTaskCompletion>,
    mut cancelled: oneshot::Receiver<()>,
) {
    loop {
        let completion = tokio::select! {
            _ = &mut cancelled => break,
            result = completions.recv() => match result {
                Ok(completion) => completion,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        };
        if completion.session_id != session_id {
            continue;
        }
        if completion.status == BackgroundTaskStatus::Failed {
            tracing::error!(session_id, task_id = %completion.task_id, "background shell failed");
        }
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        // 完成中继：上层 AcpHost 通知泵把终态格式化为 task-notification 并
        // 注入主对话；无订阅者时静默丢弃。
        let _ = runtime.task_notification_tx.send(TaskTerminalNotice {
            session_id: session_id.clone(),
            task_id: completion.task_id.clone(),
            kind: TaskNoticeKind::Shell,
            status_text: completion.status.as_str(),
            duration_ms: completion.duration_ms,
            summary: Some(completion.summary.clone()).filter(|summary| !summary.is_empty()),
            agent_id: None,
        });
        let Some(event) = background_task_completion_event(&completion) else {
            continue;
        };
        let delivery = match runtime.session_delivery(&session_id) {
            Ok(delivery) => delivery,
            Err(_) => break,
        };
        if delivery
            .send_batch(vec![DeliveryDraft::KeenCodeEvent {
                turn_id: None,
                source_agent_id: None,
                journal_sequence: None,
                occurred_at_ms: unix_time_ms(),
                event,
            }])
            .await
            .is_err()
        {
            break;
        }
    }
}

/// 构造一个通过 ACP 严格校验的后台 Shell 完成事件；可疑摘要会被安全省略。
pub(super) fn background_task_completion_event(
    completion: &BackgroundTaskCompletion,
) -> Option<KeenCodeEvent> {
    let status = match completion.status {
        BackgroundTaskStatus::Running => return None,
        BackgroundTaskStatus::Succeeded => BackgroundTaskTerminalStatus::Succeeded,
        BackgroundTaskStatus::Failed => BackgroundTaskTerminalStatus::Failed,
        BackgroundTaskStatus::Cancelled => BackgroundTaskTerminalStatus::Cancelled,
    };
    validated_background_task_completion_event(
        &completion.task_id,
        BackgroundTaskKind::Shell,
        None,
        status,
        completion.duration_ms,
        Some(&completion.summary),
    )
}

/// 仅记录执行元数据；工具正文仍由带 sequence 的权威 Journal 保留。
pub(super) fn log_runtime_event(
    session_id: &str,
    state: &SessionState,
    sequence: u64,
    event: &SessionEvent,
    observability: Option<&crate::diagnostics::observability::ObservabilityStore>,
) {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                log_runtime_event(session_id, state, sequence, event, observability);
            }
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            ..
        } => {
            tracing::info!(target: "keencode_diagnostics", session_id, sequence, turn_id = %turn_id, agent_id = %source_agent_id, "turn started");
            if let Some(observability) = observability {
                observability.increment_counter("agent.turns.started", 1);
                observability.record_metric(
                    "agent.turn.started",
                    1.0,
                    "count",
                    [("agent_id".to_owned(), source_agent_id.as_str().to_owned())],
                );
            }
        }
        SessionEvent::TurnCompleted { turn_id } => {
            tracing::info!(target: "keencode_diagnostics", session_id, sequence, turn_id = %turn_id, "turn completed");
            record_agent_turn_terminal(session_id, state, turn_id, "completed", observability);
        }
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            tracing::warn!(session_id, sequence, turn_id = %turn_id, ?reason, error = %message, "turn stopped");
            let status = match reason {
                TurnStopReason::Cancelled => "cancelled",
                _ => "failed",
            };
            record_agent_turn_terminal(session_id, state, turn_id, status, observability);
        }
        SessionEvent::ToolExecutionStarted { request_id } => {
            if let Some(observability) = observability {
                observability.increment_counter("agent.tools.started", 1);
                if let Ok(request) = tool_request(state, request_id.as_str()) {
                    observability.record_metric(
                        "agent.tool.started",
                        1.0,
                        "count",
                        [
                            ("tool".to_owned(), request.tool_name.clone()),
                            ("effect".to_owned(), format!("{:?}", request.effect)),
                        ],
                    );
                }
            }
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => {
            record_tool_terminal(state, request_id.as_str(), outcome, observability);
            if outcome.status == ToolCompletionStatus::Failed || outcome.result.is_error {
                let request = tool_request(state, request_id.as_str()).ok();
                tracing::error!(session_id, sequence, request_id = %request_id,
                    turn_id = request.map(|r| r.turn_id.as_str()).unwrap_or(""),
                    agent_id = request.map(|r| r.agent_id.as_str()).unwrap_or(""),
                    tool = request.map(|r| r.tool_name.as_str()).unwrap_or(""),
                    status = ?outcome.status, "tool failed; details in session events.jsonl");
            }
        }
        SessionEvent::ToolSideEffectUnknown { request_id, .. } => {
            tracing::error!(session_id, sequence, request_id = %request_id, "tool side effect unknown; details in session events.jsonl");
            if let Some(observability) = observability {
                observability.increment_counter("agent.tools.side_effect_unknown", 1);
            }
        }
        _ => {}
    }
}

/// 把权威 Turn 终态投影为有界 Agent metric/trace；正文和绝对路径不进入属性。
pub(super) fn record_agent_turn_terminal(
    session_id: &str,
    state: &SessionState,
    turn_id: &ResourceTurnId,
    status: &str,
    observability: Option<&crate::diagnostics::observability::ObservabilityStore>,
) {
    let Some(observability) = observability else {
        return;
    };
    let Some(turn) = state.turns.get(turn_id) else {
        observability.increment_counter("agent.turns.observation_missing", 1);
        return;
    };
    let duration_ms = turn
        .completed_at_unix_ms
        .map(|completed| completed.saturating_sub(turn.started_at_unix_ms));
    observability.increment_counter(&format!("agent.turns.{status}"), 1);
    if let Some(duration_ms) = duration_ms {
        observability.record_histogram("agent.turn_duration_ms", duration_ms as f64);
    }
    let mut attributes = BTreeMap::from([
        (
            "agent_id".to_owned(),
            turn.source_agent_id.as_str().to_owned(),
        ),
        (
            "root_turn".to_owned(),
            (turn.root_turn_id == turn.turn_id).to_string(),
        ),
    ]);
    if let Some(parent) = &turn.parent_turn_id {
        attributes.insert("parent_turn".to_owned(), parent.as_str().to_owned());
    }
    observability.record_trace(crate::diagnostics::observability::TraceSample {
        trace_id: format!("{session_id}:{}", turn.turn_id),
        span_id: format!("agent-turn:{}", turn.turn_id),
        parent_span_id: turn
            .parent_turn_id
            .as_ref()
            .map(|parent| format!("agent-turn:{parent}")),
        name: "agent.turn".to_owned(),
        started_at_ms: turn.started_at_unix_ms,
        duration_ms,
        ttft_ms: None,
        status: status.to_owned(),
        attributes,
    });
}

/// 从权威工具生命周期读取执行耗时；工具参数和结果只保留在 Session Journal。
pub(super) fn record_tool_terminal(
    state: &SessionState,
    request_id: &str,
    outcome: &keencode_resources::ToolOutcome,
    observability: Option<&crate::diagnostics::observability::ObservabilityStore>,
) {
    let Some(observability) = observability else {
        return;
    };
    let Some(lifecycle) = state
        .tools
        .iter()
        .find(|(known_request_id, _)| known_request_id.as_str() == request_id)
        .map(|(_, lifecycle)| lifecycle)
    else {
        observability.increment_counter("agent.tools.observation_missing", 1);
        return;
    };
    let status = match outcome.status {
        ToolCompletionStatus::Succeeded => "succeeded",
        ToolCompletionStatus::Failed => "failed",
        ToolCompletionStatus::Cancelled => "cancelled",
        ToolCompletionStatus::SideEffectUnknown => "side_effect_unknown",
    };
    observability.increment_counter(&format!("agent.tools.completed.{status}"), 1);
    let started = lifecycle
        .execution_started_at_unix_ms
        .unwrap_or(lifecycle.requested_at_unix_ms);
    if let Some(completed) = lifecycle.completed_at_unix_ms {
        observability.record_histogram(
            "agent.tool_duration_ms",
            completed.saturating_sub(started) as f64,
        );
    }
    observability.record_trace(crate::diagnostics::observability::TraceSample {
        trace_id: format!("tool:{request_id}"),
        span_id: format!("tool:{request_id}"),
        parent_span_id: Some(format!("agent-turn:{}", lifecycle.request.turn_id)),
        name: "agent.tool".to_owned(),
        started_at_ms: started,
        duration_ms: lifecycle
            .completed_at_unix_ms
            .map(|completed| completed.saturating_sub(started)),
        ttft_ms: None,
        status: status.to_owned(),
        attributes: BTreeMap::from([
            ("tool".to_owned(), lifecycle.request.tool_name.clone()),
            (
                "effect".to_owned(),
                format!("{:?}", lifecycle.request.effect),
            ),
        ]),
    });
}

/// 将 Runtime 有界广播订阅映射到当前 Session 投递世代，Lag 时显式要求重放。
pub(super) async fn run_runtime_event_pump(
    runtime: Weak<AgentRuntime>,
    session_id: String,
    generation: u64,
    mut subscription: RuntimeEventSubscription,
    mut cancelled: oneshot::Receiver<()>,
    mut provider_projection: ProviderProjection,
) {
    loop {
        let received = tokio::select! {
            _ = &mut cancelled => break,
            received = subscription.recv() => received,
        };
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let lagged = matches!(&received, Err(RuntimeEventReceiveError::Lagged(_)));
        let mut terminal_notice = None;
        let drafts = match received {
            Ok(delivery) => match delivery.payload {
                RuntimeEventPayload::Transient(event) => map_transient_event(&event),
                RuntimeEventPayload::Authoritative(record) => {
                    let session = match runtime.runtime_manager.get(session_id.clone()) {
                        Ok(session) => session,
                        Err(error) => {
                            tracing::error!(session_id, %error, "Runtime 事件投递失败");
                            break;
                        }
                    };
                    let snapshot = match session.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            tracing::error!(session_id, %error, "Runtime 事件投递失败");
                            break;
                        }
                    };
                    log_runtime_event(
                        &session_id,
                        &snapshot.state,
                        record.sequence,
                        &record.event,
                        runtime.observability.as_deref(),
                    );
                    terminal_notice = root_task_terminal_notice(&snapshot.state, &record.event);
                    // 子代理终态 → 主对话通知中继（ZCode 语义）：父会话空闲时
                    // 由通知泉开启读取 mailbox 的新模型轮。
                    if let SessionEvent::SubAgentStatusChanged {
                        agent_id,
                        turn_id: Some(sub_turn_id),
                        status,
                        result_summary,
                    } = &record.event
                        && matches!(
                            status,
                            SubAgentStatus::Completed
                                | SubAgentStatus::Failed
                                | SubAgentStatus::Interrupted
                                | SubAgentStatus::Stopped
                        )
                    {
                        let duration_ms = snapshot
                            .state
                            .turns
                            .get(sub_turn_id)
                            .and_then(|turn| {
                                turn.completed_at_unix_ms.map(|completed| {
                                    completed.saturating_sub(turn.started_at_unix_ms)
                                })
                            })
                            .unwrap_or(0);
                        let status_text = match status {
                            SubAgentStatus::Completed => "succeeded",
                            SubAgentStatus::Failed => "failed",
                            _ => "cancelled",
                        };
                        let _ = runtime.task_notification_tx.send(TaskTerminalNotice {
                            session_id: session_id.clone(),
                            task_id: sub_turn_id.as_str().to_owned(),
                            kind: TaskNoticeKind::Agent,
                            status_text,
                            duration_ms,
                            summary: result_summary.clone().filter(|summary| !summary.is_empty()),
                            agent_id: Some(agent_id.as_str().to_owned()),
                        });
                    }
                    match map_authoritative_record_with_projection(
                        &session,
                        &snapshot.state,
                        &record,
                        AuthoritativeProjectionMode::Live,
                        &mut provider_projection,
                        None,
                    ) {
                        Ok(mapped) => mapped.commit(),
                        Err(error) => {
                            tracing::error!(session_id, %error, "Runtime 事件投递失败");
                            break;
                        }
                    }
                }
                RuntimeEventPayload::Control(RuntimeControlEvent::SessionClosed) => break,
            },
            Err(RuntimeEventReceiveError::Lagged(skipped)) => {
                tracing::warn!(session_id, ?skipped, "Runtime 事件订阅滞后，开始恢复");
                if let Some(observability) = runtime.observability.as_deref() {
                    observability.increment_counter("runtime.events.lagged", 1);
                    observability.record_metric(
                        "runtime.events.skipped",
                        skipped.missed_events as f64,
                        "count",
                        [("session_scope".to_owned(), "single_session".to_owned())],
                    );
                }
                vec![DeliveryDraft::KeenCodeEvent {
                    turn_id: None,
                    source_agent_id: None,
                    journal_sequence: None,
                    occurred_at_ms: unix_time_ms(),
                    event: KeenCodeEvent::RecoveryStateChanged {
                        state: keencode_acp::RecoveryState::Replaying,
                    },
                }]
            }
            Err(RuntimeEventReceiveError::Closed) => break,
        };
        if drafts.is_empty() {
            continue;
        }
        let sender = match runtime.session_delivery(&session_id) {
            Ok(sender) => sender,
            Err(error) => {
                tracing::error!(session_id, %error, "Runtime 事件投递失败");
                break;
            }
        };
        if let Err(error) = sender.send_live_batch(drafts, terminal_notice).await {
            tracing::error!(session_id, %error, "Runtime 实时事件发送失败");
            if let Some(observability) = runtime.observability.as_deref() {
                observability.increment_counter("runtime.events.delivery_failed", 1);
            }
            break;
        }
        if lagged {
            let session = match runtime.runtime_manager.get(session_id.clone()) {
                Ok(session) => session,
                Err(error) => {
                    tracing::error!(session_id, %error, "Runtime 事件投递失败");
                    break;
                }
            };
            // 丢失事件后仍须先订阅再刷新基线，避免索引读取期间出现新缺口；
            // 实际已丢失的历史区间继续由外层 replay 水位修复。
            let next_subscription = match session.subscribe() {
                Ok(subscription) => subscription,
                Err(error) => {
                    tracing::error!(session_id, %error, "Runtime 事件投递失败");
                    break;
                }
            };
            provider_projection = match provider_projection_from_history(&session) {
                Ok(provider) => provider,
                Err(error) => {
                    tracing::error!(session_id, %error, "Runtime 事件投影索引恢复失败");
                    break;
                }
            };
            subscription = next_subscription;
        }
    }
    if let Some(runtime) = runtime.upgrade()
        && let Ok(mut pumps) = runtime.live_pumps.lock()
        && pumps
            .get(&session_id)
            .is_some_and(|(current, _)| *current == generation)
    {
        pumps.remove(&session_id);
    }
}

/// 一次实时根任务终态通知所需的最小非敏感数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RootTaskTerminalNotice {
    /// Session 当前用户可见标题；空标题由通知模块替换为稳定回退文案。
    pub(super) task_title: String,
    /// 正常完成为 `None`，非正常终态保留 Runtime 的结构化停止原因。
    pub(super) stop_reason: Option<TurnStopReason>,
}

/// 从单条或原子批次权威事件中提取根 Turn 唯一终态，忽略子 Agent 终态。
pub(super) fn root_task_terminal_notice(
    state: &SessionState,
    event: &SessionEvent,
) -> Option<RootTaskTerminalNotice> {
    match event {
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .find_map(|event| root_task_terminal_notice(state, event)),
        SessionEvent::TurnCompleted { turn_id } => root_turn_terminal_notice(state, turn_id, None),
        SessionEvent::TurnStopped {
            turn_id, reason, ..
        } => root_turn_terminal_notice(state, turn_id, Some(*reason)),
        _ => None,
    }
}

/// 只为根 Agent 的顶层用户 Turn 构造通知，防止后台子 Agent 完成时打扰用户。
pub(super) fn root_turn_terminal_notice(
    state: &SessionState,
    turn_id: &ResourceTurnId,
    stop_reason: Option<TurnStopReason>,
) -> Option<RootTaskTerminalNotice> {
    let turn = state.turns.get(turn_id)?;
    if turn.source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID
        || turn.parent_turn_id.is_some()
        || turn.root_turn_id.as_str() != turn_id.as_str()
    {
        return None;
    }
    Some(RootTaskTerminalNotice {
        task_title: state.title.clone(),
        stop_reason,
    })
}

/// 将可信 Agent 实时事件映射为标准 ACP 增量或 KeenCode 临时生命周期事件。
pub(super) fn map_transient_event(event: &AgentStreamEvent) -> Vec<DeliveryDraft> {
    let occurred_at_ms = unix_time_ms();
    let update = match event.kind() {
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::TextDelta { delta, .. },
        } => Some(keencode_acp::schema::SessionUpdate::AgentMessageChunk(
            keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                delta.clone(),
            )),
        )),
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::ReasoningDelta { delta, .. }
                | ModelStreamEvent::ReasoningSummaryDelta { delta, .. },
        } => Some(keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
            keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                delta.clone(),
            )),
        )),
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::ToolCallStart { .. },
        } => None,
        AgentStreamEventKind::ModelEvent {
            event: ModelStreamEvent::MessageStart { .. },
        } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ModelFirstStreamObserved,
            }];
        }
        AgentStreamEventKind::ModelFailure { error } => {
            tracing::error!(session_id = %event.session_id(), turn_id = %event.turn_id(), agent_id = %event.source_agent_id(), %error, "model round failed");
            return Vec::new();
        }
        AgentStreamEventKind::ContextCompactionStarted { estimated_tokens } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ContextCompactionStarted {
                    estimated_tokens: *estimated_tokens,
                },
            }];
        }
        AgentStreamEventKind::ContextCompactionFailed { failure_kind } => {
            tracing::error!(session_id = %event.session_id(), turn_id = %event.turn_id(), agent_id = %event.source_agent_id(), ?failure_kind, "context compaction failed");
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ContextCompactionFailed {
                    failure_kind: match failure_kind {
                        ContextCompactionFailureKind::Model => CompactionFailureKind::Model,
                        ContextCompactionFailureKind::Budget => CompactionFailureKind::Budget,
                        ContextCompactionFailureKind::Storage => CompactionFailureKind::Storage,
                        ContextCompactionFailureKind::InvalidResult => {
                            CompactionFailureKind::InvalidResult
                        }
                    },
                },
            }];
        }
        AgentStreamEventKind::ContextCompactionTruncated { estimated_tokens } => {
            return vec![DeliveryDraft::KeenCodeEvent {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                journal_sequence: None,
                occurred_at_ms,
                event: KeenCodeEvent::ContextCompactionTruncated {
                    estimated_tokens: *estimated_tokens,
                },
            }];
        }
        AgentStreamEventKind::ModelEvent {
            event:
                ModelStreamEvent::DecodeTiming { .. }
                | ModelStreamEvent::MessageEnd { .. }
                | ModelStreamEvent::Usage { .. }
                | ModelStreamEvent::ToolCallArgumentsDelta { .. }
                | ModelStreamEvent::ToolCallEnd { .. }
                | ModelStreamEvent::ReasoningContinuation { .. },
        } => None,
    };
    update
        .map(|update| {
            vec![DeliveryDraft::SessionUpdate {
                turn_id: Some(event.turn_id().as_str().to_owned()),
                source_agent_id: Some(event.source_agent_id().as_str().to_owned()),
                occurred_at_ms,
                journal_sequence: None,
                update: Box::new(update),
            }]
        })
        .unwrap_or_default()
}

/// 将后台任务的 Unix 毫秒启动时间格式化为 UTC RFC 3339 毫秒文本。
pub(super) fn background_task_started_at(unix_ms: u64) -> Result<String, AgentRuntimeError> {
    let unix_ms = i64::try_from(unix_ms).map_err(runtime_operation_failed)?;
    Utc.timestamp_millis_opt(unix_ms)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)
}

/// 将单调时钟持续时间转换为不会溢出的前端毫秒数。
pub(super) fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 恢复完成时只释放水位之后的权威事件；门内临时增量由后续权威提交恢复。
pub(super) fn draft_is_after_recovery_waterline(
    draft: &DeliveryDraft,
    through_sequence: u64,
) -> bool {
    match draft {
        DeliveryDraft::SessionUpdate {
            journal_sequence: Some(sequence),
            ..
        }
        | DeliveryDraft::KeenCodeEvent {
            journal_sequence: Some(sequence),
            ..
        } => *sequence > through_sequence,
        DeliveryDraft::SessionUpdate {
            journal_sequence: None,
            ..
        }
        | DeliveryDraft::KeenCodeEvent {
            journal_sequence: None,
            ..
        } => false,
    }
}

/// 连续分配序号并把一个不可交错批次合并为单次桌面边界投递。
/// 返回最后成功的桌面投递序号；任何失败都会中止后续条目的投递。
pub(super) fn emit_batch(
    session_id: &str,
    emitter: &Arc<dyn DeliveryEmitter>,
    sequence: &mut SessionSequence,
    drafts: Vec<DeliveryDraft>,
) -> Result<u64, AgentRuntimeError> {
    if drafts.is_empty() {
        // 空批次不越过投递边界，避免无意义的边界调用与空帧。
        return Ok(sequence.last_allocated());
    }
    let mut deliveries = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let delivery_sequence = sequence
            .allocate()
            .map_err(|_| AgentRuntimeError::DeliverySequenceExhausted)?;
        deliveries.push(materialize_delivery(session_id, delivery_sequence, draft)?);
    }
    emitter.emit_all(&deliveries)?;
    Ok(sequence.last_allocated())
}

/// 把一个草稿绑定到当前 Session 与新分配的投递序号。
pub(super) fn materialize_delivery(
    session_id: &str,
    delivery_sequence: u64,
    draft: DeliveryDraft,
) -> Result<AcpDelivery, AgentRuntimeError> {
    match draft {
        DeliveryDraft::SessionUpdate {
            turn_id,
            source_agent_id,
            occurred_at_ms,
            journal_sequence: _,
            update,
        } => SessionUpdateDeliveryEnvelope::new(
            session_id,
            turn_id,
            source_agent_id,
            delivery_sequence,
            occurred_at_ms,
            *update,
        )
        .map(|envelope| AcpDelivery::SessionUpdate { envelope })
        .map_err(|_| AgentRuntimeError::DeliveryPoisoned),
        DeliveryDraft::KeenCodeEvent {
            turn_id,
            source_agent_id,
            journal_sequence,
            occurred_at_ms,
            event,
        } => {
            let params = match (turn_id, source_agent_id) {
                (Some(turn_id), Some(source_agent_id)) => KeenCodeEventEnvelopeParams::for_turn(
                    session_id,
                    turn_id,
                    source_agent_id,
                    delivery_sequence,
                    occurred_at_ms,
                    event,
                ),
                (None, None) => KeenCodeEventEnvelopeParams::for_session(
                    session_id,
                    delivery_sequence,
                    occurred_at_ms,
                    event,
                ),
                _ => return Err(AgentRuntimeError::DeliveryPoisoned),
            };
            match journal_sequence {
                Some(journal_sequence) => {
                    KeenCodeEventEnvelope::new_authoritative(journal_sequence, params)
                }
                None => KeenCodeEventEnvelope::new_transient(params),
            }
            .map(|envelope| AcpDelivery::KeenCodeEvent { envelope })
            .map_err(|_| AgentRuntimeError::DeliveryPoisoned)
        }
    }
}

#[cfg(test)]
pub(super) fn map_authoritative_record(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    mode: AuthoritativeProjectionMode,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    let mut provider = ProviderProjection::default();
    Ok(
        map_authoritative_record_with_projection(session, state, record, mode, &mut provider, None)
            .map_err(from_runtime)?
            .commit(),
    )
}
/// 将 Agent Goal 状态转换为 ACP GoalChanged 使用的稳定小写名称。
pub(super) fn goal_status_name(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Completed => "completed",
        GoalStatus::Blocked => "blocked",
    }
}

/// 实时增量攒批的专项回归：批次形状、拼接等价、屏障与窗口行为。
#[cfg(test)]
mod coalesce_tests {
    use super::*;

    /// 记录每次桌面边界调用的批量形状，用于断言攒批与拼接等价性。
    #[derive(Default)]
    struct BatchRecordingEmitter {
        batches: Mutex<Vec<Vec<Value>>>,
        terminals: Mutex<Vec<(String, Option<TurnStopReason>)>>,
    }

    impl BatchRecordingEmitter {
        fn emit_call_count(&self) -> usize {
            self.batches.lock().unwrap().len()
        }

        fn snapshot(&self) -> Vec<Vec<Value>> {
            self.batches.lock().unwrap().clone()
        }
    }

    impl DeliveryEmitter for BatchRecordingEmitter {
        fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
            let value =
                serde_json::to_value(delivery).map_err(|_| AgentRuntimeError::DesktopEmitFailed)?;
            self.batches.lock().unwrap().push(vec![value]);
            Ok(())
        }

        fn emit_all(&self, deliveries: &[AcpDelivery]) -> Result<(), AgentRuntimeError> {
            let batch = deliveries
                .iter()
                .map(|delivery| {
                    serde_json::to_value(delivery).map_err(|_| AgentRuntimeError::DesktopEmitFailed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.batches.lock().unwrap().push(batch);
            Ok(())
        }

        fn notify_task_terminal(
            &self,
            task_title: Option<&str>,
            stop_reason: Option<TurnStopReason>,
        ) {
            self.terminals
                .lock()
                .unwrap()
                .push((task_title.unwrap_or_default().to_owned(), stop_reason));
        }
    }

    fn message_chunk(turn: &str, agent: &str, delta: &str) -> DeliveryDraft {
        DeliveryDraft::SessionUpdate {
            turn_id: Some(turn.to_owned()),
            source_agent_id: Some(agent.to_owned()),
            occurred_at_ms: unix_time_ms(),
            journal_sequence: None,
            update: Box::new(keencode_acp::schema::SessionUpdate::AgentMessageChunk(
                keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                    delta.to_owned(),
                )),
            )),
        }
    }

    fn thought_chunk(turn: &str, agent: &str, delta: &str) -> DeliveryDraft {
        DeliveryDraft::SessionUpdate {
            turn_id: Some(turn.to_owned()),
            source_agent_id: Some(agent.to_owned()),
            occurred_at_ms: unix_time_ms(),
            journal_sequence: None,
            update: Box::new(keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
                keencode_acp::schema::ContentChunk::new(keencode_acp::schema::ContentBlock::from(
                    delta.to_owned(),
                )),
            )),
        }
    }

    /// 生命周期类屏障草稿：任何一次出现都应立即清空攒批缓冲。
    fn barrier(turn: &str, agent: &str) -> DeliveryDraft {
        DeliveryDraft::KeenCodeEvent {
            turn_id: Some(turn.to_owned()),
            source_agent_id: Some(agent.to_owned()),
            journal_sequence: None,
            occurred_at_ms: unix_time_ms(),
            event: KeenCodeEvent::ModelFirstStreamObserved,
        }
    }

    fn envelope_chunk_text(delivery: &Value) -> String {
        delivery
            .pointer("/envelope/update/content/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    fn envelope_update_kind(delivery: &Value) -> String {
        delivery
            .pointer("/envelope/update/sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    #[tokio::test]
    async fn coalesces_adjacent_deltas_and_flushes_on_barrier() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-coalesce", emitter.clone(), false);
        for delta in ["你", "好", "！"] {
            sender
                .send_batch(vec![message_chunk("turn-1", "agent-1", delta)])
                .await
                .unwrap();
        }
        // 屏障草稿触发同步 flush：屏障 ack 返回时缓冲增量与屏障均已投出。
        sender
            .send_batch(vec![barrier("turn-1", "agent-1")])
            .await
            .unwrap();
        let batches = emitter.snapshot();
        assert_eq!(batches.len(), 2, "缓冲增量与屏障应为两次边界调用");
        assert_eq!(batches[0].len(), 1, "相邻同类增量应拼接为单条投递");
        assert_eq!(envelope_chunk_text(&batches[0][0]), "你好！");
        assert_eq!(envelope_update_kind(&batches[0][0]), "agent_message_chunk");
        assert_eq!(batches[1].len(), 1, "屏障草稿应零延迟单独投递");
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn keeps_distinct_streams_unmerged_in_one_batch() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-distinct", emitter.clone(), false);
        sender
            .send_batch(vec![message_chunk("turn-1", "agent-1", "a")])
            .await
            .unwrap();
        sender
            .send_batch(vec![thought_chunk("turn-1", "agent-1", "t")])
            .await
            .unwrap();
        sender
            .send_batch(vec![message_chunk("turn-2", "agent-1", "b")])
            .await
            .unwrap();
        sender
            .send_batch(vec![barrier("turn-2", "agent-1")])
            .await
            .unwrap();
        let batches = emitter.snapshot();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 3, "不同 Turn 或类别的流不得互相拼接");
        assert_eq!(envelope_chunk_text(&batches[0][0]), "a");
        assert_eq!(envelope_update_kind(&batches[0][1]), "agent_thought_chunk");
        assert_eq!(envelope_chunk_text(&batches[0][2]), "b");
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn flushes_buffered_delta_before_shutdown() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-shutdown", emitter.clone(), false);
        // 单批次 [增量, 屏障, 增量]：屏障清空前缓冲，尾增量留待关闭 flush。
        sender
            .send_batch(vec![
                message_chunk("turn-1", "agent-1", "a"),
                barrier("turn-1", "agent-1"),
                message_chunk("turn-1", "agent-1", "b"),
            ])
            .await
            .unwrap();
        assert_eq!(emitter.emit_call_count(), 2, "屏障应立即投出缓冲与自身");
        assert_eq!(envelope_chunk_text(&emitter.snapshot()[0][0]), "a");
        // 关闭前 flush 尾部增量；shutdown ack 返回时投递已完成。
        sender.shutdown().await.unwrap();
        assert_eq!(emitter.emit_call_count(), 3);
        assert_eq!(envelope_chunk_text(&emitter.snapshot()[2][0]), "b");
    }

    #[tokio::test]
    async fn flushes_delta_after_idle_window_without_barrier() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-window", emitter.clone(), false);
        sender
            .send_batch(vec![message_chunk("turn-1", "agent-1", "a")])
            .await
            .unwrap();
        assert_eq!(emitter.emit_call_count(), 0, "ack 即时返回不代表已完成投递");
        tokio::time::sleep(Duration::from_millis(DELTA_FLUSH_WINDOW_MS + 80)).await;
        assert_eq!(emitter.emit_call_count(), 1, "惰性窗口到期后应 flush 缓冲");
        assert_eq!(envelope_chunk_text(&emitter.snapshot()[0][0]), "a");
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn flushes_when_byte_budget_is_reached() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-bytes", emitter.clone(), false);
        let big = "x".repeat(40 * 1024);
        sender
            .send_batch(vec![message_chunk("turn-1", "agent-1", &big)])
            .await
            .unwrap();
        sender
            .send_batch(vec![message_chunk("turn-1", "agent-1", &big)])
            .await
            .unwrap();
        assert_eq!(
            emitter.emit_call_count(),
            1,
            "达到字节预算应立即 flush 且两段已拼接"
        );
        assert_eq!(emitter.snapshot()[0].len(), 1);
        assert_eq!(
            envelope_chunk_text(&emitter.snapshot()[0][0]).len(),
            80 * 1024
        );
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn respects_delivery_count_budget_across_streams() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-count", emitter.clone(), false);
        // 交替 Turn 让每条增量都是新缓冲条目；上限内允许窗口或上限触发拆批。
        for index in 0..DELTA_FLUSH_MAX_DELIVERIES {
            let turn = if index % 2 == 0 { "turn-a" } else { "turn-b" };
            sender
                .send_batch(vec![message_chunk(turn, "agent-1", "x")])
                .await
                .unwrap();
        }
        sender
            .send_batch(vec![barrier("turn-a", "agent-1")])
            .await
            .unwrap();
        let batches = emitter.snapshot();
        let total: usize = batches.iter().map(|batch| batch.len()).sum();
        assert_eq!(
            total,
            DELTA_FLUSH_MAX_DELIVERIES + 1,
            "投递总量必须守恒（含末尾屏障）"
        );
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= DELTA_FLUSH_MAX_DELIVERIES),
            "单次边界调用不得超过条数预算"
        );
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_gate_buffers_live_deltas_without_batching() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-gate", emitter.clone(), true);
        sender
            .send_live_batch(vec![message_chunk("turn-1", "agent-1", "a")], None)
            .await
            .unwrap();
        assert_eq!(emitter.emit_call_count(), 0, "恢复门内既不投递也不攒批");
        // 无 Journal 序号的临时增量在末页释放时被水位丢弃，不进入桌面。
        sender.send_replay_batch(Vec::new(), 0, true).await.unwrap();
        assert_eq!(emitter.emit_call_count(), 0);
        sender.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn forwards_terminal_notice_after_barrier_delivery() {
        let emitter = Arc::new(BatchRecordingEmitter::default());
        let sender = SessionDeliverySender::spawn("session-notice", emitter.clone(), false);
        sender
            .send_live_batch(
                vec![barrier("turn-1", "agent-1")],
                Some(RootTaskTerminalNotice {
                    task_title: "任务标题".to_owned(),
                    stop_reason: None,
                }),
            )
            .await
            .unwrap();
        assert_eq!(emitter.emit_call_count(), 1, "屏障草稿应立即投出");
        {
            let terminals = emitter.terminals.lock().unwrap();
            assert_eq!(terminals.len(), 1, "终态通知应随屏障投递成功同步转发");
            assert_eq!(terminals[0].0, "任务标题");
            assert_eq!(terminals[0].1, None);
        }
        sender.shutdown().await.unwrap();
    }
}
