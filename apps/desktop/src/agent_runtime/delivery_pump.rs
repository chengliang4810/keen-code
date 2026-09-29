//! Session 投递泵：ACP 帧、Runtime 事件与后台任务完成通知的桌面侧推送循环。
//!
//! 本模块只做"已通过投递边界的事件 → 桌面 sink"的搬运与观测记录，
//! 不持有任何会话状态；泵的生命周期由 `AgentRuntime` 的世代与门管理。

use super::*;

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
    while let Some(command) = receiver.recv().await {
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
                    let result =
                        emit_batch(&session_id, &emitter, &mut sequence, drafts).map(|_| ());
                    if result.is_ok() {
                        notify_root_task_terminal(&emitter, terminal_notice);
                    }
                    result
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
                    emitter.emit(&AcpDelivery::ClientRequest {
                        connection_id,
                        request: *request,
                    })
                };
                if result.is_err() {
                    lifecycle.mark_failed();
                }
                let _ = acknowledged.send(result);
            }
            DeliveryCommand::Shutdown { acknowledged } => {
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

/// 连续分配序号并投递一个不可交错批次，返回最后成功的桌面投递序号。
pub(super) fn emit_batch(
    session_id: &str,
    emitter: &Arc<dyn DeliveryEmitter>,
    sequence: &mut SessionSequence,
    drafts: Vec<DeliveryDraft>,
) -> Result<u64, AgentRuntimeError> {
    for draft in drafts {
        let delivery_sequence = sequence
            .allocate()
            .map_err(|_| AgentRuntimeError::DeliverySequenceExhausted)?;
        let delivery = materialize_delivery(session_id, delivery_sequence, draft)?;
        emitter.emit(&delivery)?;
    }
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
