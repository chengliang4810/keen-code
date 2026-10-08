//! 原生 Session 事件订阅：只传变化的尾块，不轮询或复制整个历史。

use super::{NativeHost, projection, ui_error};
use crate::native_ui::model::*;
use keencode_agent::{AgentStreamEvent, AgentStreamEventKind};
use keencode_model::ModelStreamEvent;
use keencode_resources::{self as resource, SessionEvent};
use keencode_runtime::{
    RuntimeControlEvent, RuntimeEventPayload, RuntimeEventReceiveError, RuntimeSession,
};
use std::{
    collections::{BTreeSet, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

struct Subscription {
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    registration: Option<SubscriptionRegistration>,
}
impl NativeUiSubscription for Subscription {
    fn cancel(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.registration.take();
    }
}

/// 注册先于后台观察者启动；出错、取消和 Drop 都由同一个 guard 幂等解除。
struct SubscriptionRegistration {
    host: Weak<super::NativeHostInner>,
    session_id: String,
    displayed_through: Arc<AtomicU64>,
}

struct SubscriptionSnapshot {
    fact: ConversationFact,
    hot_through: u64,
    stream_messages: HashSet<String>,
    stream_blocks: HashSet<(String, String)>,
    pending_ids: HashSet<String>,
}

fn subscription_snapshot(
    host: &NativeHost,
    session: &RuntimeSession,
    session_id: &str,
) -> Result<SubscriptionSnapshot, NativeUiError> {
    let mut fact = projection::conversation(host, session, None)?;
    let (hot_through, hot_messages) = host.hot_snapshot(session_id);
    let stream_messages = hot_messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<HashSet<_>>();
    let stream_blocks = hot_messages
        .iter()
        .flat_map(|message| {
            message.blocks.iter().filter_map(|block| match block {
                MessageBlock::Markdown { block_id, .. }
                | MessageBlock::Reasoning { block_id, .. } => {
                    Some((message.message_id.clone(), block_id.clone()))
                }
                _ => None,
            })
        })
        .collect::<HashSet<_>>();
    let pending = host.pending_messages(session_id);
    let pending_ids = pending
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<HashSet<_>>();
    // Snapshot 还要携带当前未落盘的流式尾部和输入队列，才能在丢批后恢复可见事实。
    Arc::make_mut(&mut fact.messages).extend(hot_messages.into_iter().map(Arc::new));
    Arc::make_mut(&mut fact.messages).extend(pending.into_iter().map(Arc::new));
    Ok(SubscriptionSnapshot {
        fact,
        hot_through,
        stream_messages,
        stream_blocks,
        pending_ids,
    })
}

/// 同一次快照替换的订阅水位与尾块身份必须一起更新，避免历史和流式内容重复。
struct ResyncCursor<'a> {
    delivery_sequence: &'a mut u64,
    journal_sequence: &'a mut u64,
    hot_through: &'a mut u64,
    stream_messages: &'a mut HashSet<String>,
    stream_blocks: &'a mut HashSet<(String, String)>,
    pending_ids: &'a mut HashSet<String>,
}

fn resync_snapshot_batch(
    host: &NativeHost,
    session: &RuntimeSession,
    session_id: &str,
    cursor: ResyncCursor<'_>,
) -> Result<NativeEventBatch, NativeUiError> {
    let ResyncCursor {
        delivery_sequence,
        journal_sequence,
        hot_through,
        stream_messages,
        stream_blocks,
        pending_ids,
    } = cursor;
    let snapshot = subscription_snapshot(host, session, session_id)?;
    let mut fact = snapshot.fact;
    let journal = fact.session.last_sequence;
    host.inner
        .attention
        .mark_read(session_id, journal)
        .map_err(ui_error)?;
    fact.session.unread = false;
    *journal_sequence = journal;
    *hot_through = snapshot.hot_through;
    *stream_messages = snapshot.stream_messages;
    *stream_blocks = snapshot.stream_blocks;
    *pending_ids = snapshot.pending_ids;
    *delivery_sequence = (*delivery_sequence).saturating_add(1);
    Ok(NativeEventBatch {
        session_id: session_id.to_owned(),
        delivery_sequence: *delivery_sequence,
        journal_sequence: journal,
        events: vec![NativeUiEvent::Snapshot(Box::new(fact))],
    })
}

impl Drop for SubscriptionRegistration {
    fn drop(&mut self) {
        let Some(inner) = self.host.upgrade() else {
            return;
        };
        // 查询 Runtime 必须先于订阅锁，投影会在状态锁内读取订阅计数。
        let idle = inner
            .runtime
            .runtime_manager()
            .get(self.session_id.clone())
            .ok()
            .and_then(|session| {
                let active = session.has_active_work().ok()?;
                let empty = session
                    .read_state(|state| state.input_queue.items.is_empty())
                    .ok()?;
                Some(!active && empty)
            })
            .unwrap_or(false);
        if let Err(error) = inner.attention.mark_read(
            &self.session_id,
            self.displayed_through.load(Ordering::Acquire),
        ) {
            tracing::warn!(%error, "保存原生已读水位失败");
        }
        let mut subscribers = inner
            .native_session_subscribers
            .lock()
            .expect("原生订阅锁已损坏");
        if let Some(count) = subscribers.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                subscribers.remove(&self.session_id);
            }
        }
        if subscribers.contains_key(&self.session_id) {
            return;
        }
        // 这里只回收无任务、无队列的热缓存与观察者，不取消任何 Agent 工作。
        if idle {
            if let Some(supervisor) = inner
                .queue_supervisors
                .lock()
                .expect("原生队列锁已损坏")
                .get(&self.session_id)
            {
                supervisor.cancel();
            }
            inner
                .hot
                .lock()
                .expect("原生热流锁已损坏")
                .remove(&self.session_id);
            drop(subscribers);
            NativeHost { inner }.schedule_idle_native_release(self.session_id.clone());
        }
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl NativeHost {
    pub(super) async fn subscribe_native(
        &self,
        session_id: String,
        sink: Arc<dyn Fn(NativeEventBatch) + Send + Sync>,
    ) -> Result<Box<dyn NativeUiSubscription>, NativeUiError> {
        let session = self.open_session(&session_id)?;
        let displayed_through = Arc::new(AtomicU64::new(0));
        let registration = {
            let mut subscribers = self
                .inner
                .native_session_subscribers
                .lock()
                .expect("原生订阅锁已损坏");
            *subscribers.entry(session_id.clone()).or_default() += 1;
            SubscriptionRegistration {
                host: Arc::downgrade(&self.inner),
                session_id: session_id.clone(),
                displayed_through: displayed_through.clone(),
            }
        };
        self.ensure_queue_supervisor(&session_id)?;
        let mut source = session.subscribe().map_err(ui_error)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.wait_hot_through(&session_id, source.last_observed_delivery_sequence()),
        )
        .await
        .map_err(|_| {
            NativeUiError::new(
                "subscription-start-timeout",
                "会话事件观察者未完成初始化，请重新打开会话",
                true,
            )
        })?;
        let mut permissions = self.inner.runtime.permissions().subscribe_pending();
        let mut questions = self
            .inner
            .runtime
            .elicitation_coordinator()
            .subscribe_pending();
        let mut local_changes = self.inner.local_changes.subscribe();
        let initial = subscription_snapshot(self, &session, &session_id)?;
        let initial_journal = initial.fact.session.last_sequence;
        self.inner
            .attention
            .mark_read(&session_id, initial_journal)
            .map_err(ui_error)?;
        let mut initial_fact = initial.fact;
        initial_fact.session.unread = false;
        sink(NativeEventBatch {
            session_id: session_id.clone(),
            delivery_sequence: 1,
            journal_sequence: initial_journal,
            events: vec![NativeUiEvent::Snapshot(Box::new(initial_fact))],
        });
        displayed_through.store(initial_journal, Ordering::Release);
        let mut hot_through = initial.hot_through;
        let mut stream_messages = initial.stream_messages;
        let mut stream_blocks = initial.stream_blocks;
        let mut pending_ids = initial.pending_ids;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let host = self.clone();
        let task = self.inner.executor.spawn(async move {
            let mut delivery_sequence = 1u64;
            let mut journal_sequence = initial_journal;
            loop {
                let mut events = Vec::new(); let mut closed = false;
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    item = source.recv() => match item {
                        Ok(delivery) => { host.observe_hot_event(&delivery); match delivery.payload {
                            RuntimeEventPayload::Transient(event) => {
                                if delivery.delivery_sequence <= hot_through { continue; }
                                project_stream(&event, &mut stream_messages, &mut stream_blocks, &mut events);
                            }
                            RuntimeEventPayload::Authoritative(record) => {
                                // 快照读取期间完成的段已在历史页中，但初始 hot 尾块可能仍在。
                                // 即使事实无需再次投影，也必须消费其流式块释放事件。
                                remove_committed_streams(&record.event, &mut events);
                                remove_finished_streams(&record.event, &stream_messages, &mut events);
                                for event in &events { if let NativeUiEvent::RemoveMessage { message_id } = event {
                                    stream_messages.remove(message_id); stream_blocks.retain(|(id, _)| id != message_id);
                                }}
                                if record.sequence > journal_sequence {
                                    let completed_rewind_turn_ids =
                                        if has_file_change_event(&record.event) {
                                            match projection::completed_rewind_turn_ids(
                                                &host,
                                                &session_id,
                                            ) {
                                                Ok(ids) => Some(ids),
                                                Err(error) => {
                                                    tracing::error!(
                                                        %error,
                                                        "读取文件撤销事实失败"
                                                    );
                                                    events.push(resync(delivery_sequence));
                                                    None
                                                }
                                            }
                                        } else {
                                            Some(BTreeSet::new())
                                        };
                                    if let Some(completed_rewind_turn_ids) =
                                        completed_rewind_turn_ids
                                    {
                                        match session.read_state(|state| {
                                            let mut source = Vec::new();
                                            projection::event_messages(&record.event, &mut source);
                                            (
                                                source
                                                    .into_iter()
                                                    .filter_map(|message| {
                                                        projection::message_fact(
                                                            &session,
                                                            state,
                                                            message,
                                                            &completed_rewind_turn_ids,
                                                        )
                                                    })
                                                    .collect::<Vec<_>>(),
                                                projection::session_fact(&host, state),
                                                state.transcript_revision,
                                            )
                                        }) {
                                            Ok((messages, mut fact, revision)) => {
                                                journal_sequence = record.sequence;
                                                fact.last_sequence = record.sequence;
                                                for message in messages {
                                                    for block in &message.blocks {
                                                        if let MessageBlock::Tool(tool) = block {
                                                            events.push(NativeUiEvent::RemoveMessage {
                                                                message_id: format!(
                                                                    "tool:{}",
                                                                    tool.request_id
                                                                ),
                                                            });
                                                        }
                                                    }
                                                    events.push(NativeUiEvent::MessageUpserted(
                                                        message,
                                                    ));
                                                }
                                                project_control(
                                                    &record.event,
                                                    &session,
                                                    &completed_rewind_turn_ids,
                                                    &mut events,
                                                );
                                                events.push(NativeUiEvent::SessionChanged(fact));
                                                if matches!(
                                                    record.event,
                                                    SessionEvent::CompactionApplied { .. }
                                                ) {
                                                    events.push(NativeUiEvent::HistoryInvalidated {
                                                        transcript_revision: revision,
                                                    });
                                                }
                                            }
                                            Err(error) => {
                                                tracing::error!(%error, "原生投影失败");
                                                events.push(resync(delivery_sequence));
                                            }
                                        }
                                    }
                                }
                                }
                            RuntimeEventPayload::ModelRetryScheduled(retry) => events.push(NativeUiEvent::MessageUpserted(UiMessage {
                                message_id: format!("retry:{}", retry.turn_id), turn_id: Some(retry.turn_id), role: MessageRole::System,
                                blocks: vec![MessageBlock::Markdown { block_id: "retry-status".into(), source: format!("模型请求重试 {}/{}：{}", retry.attempt, retry.max_attempts, retry.message), streaming: false }], feedback: None, created_at_unix_ms: Some(retry.occurred_at_ms),
                            })),
                            RuntimeEventPayload::Control(RuntimeControlEvent::SessionClosed) => { events.push(NativeUiEvent::Closed); closed = true; }
                        }},
                        Err(RuntimeEventReceiveError::Lagged(_)) => {
                            match resync_snapshot_batch(
                                &host,
                                &session,
                                &session_id,
                                ResyncCursor {
                                    delivery_sequence: &mut delivery_sequence,
                                    journal_sequence: &mut journal_sequence,
                                    hot_through: &mut hot_through,
                                    stream_messages: &mut stream_messages,
                                    stream_blocks: &mut stream_blocks,
                                    pending_ids: &mut pending_ids,
                                },
                            ) {
                                Ok(batch) => {
                                    displayed_through.store(
                                        batch.journal_sequence,
                                        Ordering::Release,
                                    );
                                    sink(batch);
                                }
                                Err(error) => {
                                    tracing::error!(%error, "原生订阅丢批后重建快照失败");
                                    events.push(resync(delivery_sequence));
                                }
                            }
                        }
                        Err(RuntimeEventReceiveError::Closed) => { events.push(NativeUiEvent::Closed); closed = true; }
                    },
                    change = permissions.recv() => match change {
                        Ok(change) if change.display_session_id == session_id => {
                            refresh_pending(&host, &session_id, &mut pending_ids, &mut events);
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            refresh_pending(&host, &session_id, &mut pending_ids, &mut events);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    change = questions.recv() => match change {
                        Ok(change) if change.display_session_id == session_id => {
                            refresh_pending(&host, &session_id, &mut pending_ids, &mut events);
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            refresh_pending(&host, &session_id, &mut pending_ids, &mut events);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    change = local_changes.recv() => match change {
                        Ok((id, event)) if id == session_id => match event {
                            Some(event) => events.push(event),
                            None => refresh_pending(&host, &session_id, &mut pending_ids, &mut events),
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            match resync_snapshot_batch(
                                &host,
                                &session,
                                &session_id,
                                ResyncCursor {
                                    delivery_sequence: &mut delivery_sequence,
                                    journal_sequence: &mut journal_sequence,
                                    hot_through: &mut hot_through,
                                    stream_messages: &mut stream_messages,
                                    stream_blocks: &mut stream_blocks,
                                    pending_ids: &mut pending_ids,
                                },
                            ) {
                                Ok(batch) => {
                                    displayed_through.store(
                                        batch.journal_sequence,
                                        Ordering::Release,
                                    );
                                    sink(batch);
                                }
                                Err(error) => {
                                    tracing::error!(%error, "原生本地变更丢批后重建快照失败");
                                    events.push(resync(delivery_sequence));
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Ok(_) => continue,
                    },
                }
                if events.is_empty() { continue; }
                delivery_sequence += 1;
                sink(NativeEventBatch { session_id: session_id.clone(), delivery_sequence, journal_sequence, events });
                displayed_through.store(journal_sequence, Ordering::Release);
                if closed { break; }
            }
        });
        Ok(Box::new(Subscription {
            cancel,
            task: Some(task),
            registration: Some(registration),
        }))
    }
}
fn refresh_pending(
    host: &NativeHost,
    session_id: &str,
    ids: &mut HashSet<String>,
    events: &mut Vec<NativeUiEvent>,
) {
    let messages = host.pending_messages(session_id);
    let next = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<HashSet<_>>();
    events.extend(
        ids.difference(&next)
            .map(|id| NativeUiEvent::RemoveMessage {
                message_id: id.clone(),
            }),
    );
    events.extend(messages.into_iter().map(NativeUiEvent::MessageUpserted));
    *ids = next;
}
fn resync(sequence: u64) -> NativeUiEvent {
    NativeUiEvent::ResyncRequired {
        first_missing_delivery_sequence: sequence + 1,
        last_missing_delivery_sequence: sequence + 1,
    }
}

fn has_file_change_event(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::ToolFileChangePrepared { .. }
        | SessionEvent::ToolFileChangeApplied { .. } => true,
        SessionEvent::AtomicBatch { events } => events.iter().any(has_file_change_event),
        _ => false,
    }
}

fn stream_id(turn: &str, agent: &str, round: u32) -> String {
    format!("stream:{turn}:{agent}:{round}")
}
fn project_stream(
    event: &AgentStreamEvent,
    messages: &mut HashSet<String>,
    blocks: &mut HashSet<(String, String)>,
    output: &mut Vec<NativeUiEvent>,
) {
    if event.source_agent_id().as_str() != resource::ROOT_AGENT_ID {
        return;
    }
    let message_id = stream_id(
        event.turn_id().as_str(),
        event.source_agent_id().as_str(),
        event.model_round(),
    );
    let AgentStreamEventKind::ModelEvent { event: model } = event.kind() else {
        return;
    };
    let (index, delta, reasoning) = match model {
        ModelStreamEvent::TextDelta { index, delta } => (*index, delta, false),
        ModelStreamEvent::ReasoningDelta { index, delta }
        | ModelStreamEvent::ReasoningSummaryDelta { index, delta } => (*index, delta, true),
        _ => return,
    };
    let block_id = format!(
        "{message_id}:{index}:{}",
        if reasoning { "reasoning" } else { "text" }
    );
    if messages.insert(message_id.clone()) {
        output.push(NativeUiEvent::MessageUpserted(UiMessage {
            message_id: message_id.clone(),
            turn_id: Some(event.turn_id().as_str().to_owned()),
            role: MessageRole::Assistant,
            blocks: Vec::new(),
            feedback: None,
            created_at_unix_ms: None,
        }));
        output.push(NativeUiEvent::RemoveMessage {
            message_id: format!("retry:{}", event.turn_id()),
        });
    }
    if blocks.insert((message_id.clone(), block_id.clone())) {
        output.push(NativeUiEvent::MessageBlockAppended {
            message_id: message_id.clone(),
            block: if reasoning {
                MessageBlock::Reasoning {
                    block_id: block_id.clone(),
                    source: String::new(),
                    streaming: true,
                }
            } else {
                MessageBlock::Markdown {
                    block_id: block_id.clone(),
                    source: String::new(),
                    streaming: true,
                }
            },
        });
    }
    output.push(if reasoning {
        NativeUiEvent::ReasoningDelta {
            message_id,
            block_id,
            append: delta.clone(),
            completed: false,
        }
    } else {
        NativeUiEvent::MarkdownDelta {
            message_id,
            block_id,
            append: delta.clone(),
            completed: false,
        }
    });
}
fn remove_committed_streams(event: &SessionEvent, output: &mut Vec<NativeUiEvent>) {
    match event {
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            output.push(NativeUiEvent::RemoveMessage {
                message_id: stream_id(
                    segment.turn_id.as_str(),
                    segment.source_agent_id.as_str(),
                    segment.model_round,
                ),
            })
        }
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                remove_committed_streams(event, output);
            }
        }
        _ => {}
    }
}
fn remove_finished_streams(
    event: &SessionEvent,
    messages: &HashSet<String>,
    output: &mut Vec<NativeUiEvent>,
) {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                remove_finished_streams(event, messages, output);
            }
        }
        SessionEvent::TurnStopped { turn_id, .. } | SessionEvent::TurnCompleted { turn_id } => {
            let prefix = format!("stream:{turn_id}:");
            output.extend(
                messages
                    .iter()
                    .filter(|id| id.starts_with(&prefix))
                    .map(|id| NativeUiEvent::RemoveMessage {
                        message_id: id.clone(),
                    }),
            );
            output.push(NativeUiEvent::RemoveMessage {
                message_id: format!("retry:{turn_id}"),
            });
        }
        _ => {}
    }
}
fn project_control(
    event: &SessionEvent,
    session: &keencode_runtime::RuntimeSession,
    completed_rewind_turn_ids: &BTreeSet<String>,
    output: &mut Vec<NativeUiEvent>,
) {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                project_control(event, session, completed_rewind_turn_ids, output);
            }
        }
        SessionEvent::InputQueueChanged { queue, .. } => output.push(
            NativeUiEvent::InputQueueChanged(projection::input_queue_facts(queue)),
        ),
        SessionEvent::ToolRequested { request } => {
            if let Ok(Some(tool)) = session.read_state(|state| {
                state.tools.get(&request.request_id).map(|tool| {
                    projection::tool_message_fact(session, tool, completed_rewind_turn_ids)
                })
            }) {
                output.push(NativeUiEvent::MessageUpserted(tool));
            }
        }
        SessionEvent::ToolExecutionStarted { request_id }
        | SessionEvent::ToolCompleted { request_id, .. }
        | SessionEvent::ToolSideEffectUnknown { request_id, .. } => {
            if let Ok(Some(tool)) = session.read_state(|state| {
                state
                    .tools
                    .get(request_id)
                    .map(|tool| projection::tool_fact(session, tool))
            }) {
                output.push(NativeUiEvent::ToolChanged(tool));
            }
        }
        SessionEvent::ToolFileChangePrepared { request_id, .. }
        | SessionEvent::ToolFileChangeApplied { request_id } => {
            if let Ok(Some((tool, standalone, messages))) = session.read_state(|state| {
                let tool = state.tools.get(request_id)?;
                Some((
                    projection::tool_fact(session, tool),
                    projection::tool_message_fact(session, tool, completed_rewind_turn_ids),
                    projection::messages_for_tool(
                        session,
                        state,
                        request_id,
                        completed_rewind_turn_ids,
                    ),
                ))
            }) {
                output.push(NativeUiEvent::ToolChanged(tool));
                output.push(NativeUiEvent::MessageUpserted(standalone));
                output.extend(messages.into_iter().map(NativeUiEvent::MessageUpserted));
            }
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            ..
        } if source_agent_id.as_str() == resource::ROOT_AGENT_ID => {
            output.push(NativeUiEvent::TurnChanged {
                turn_id: turn_id.as_str().to_owned(),
                status: SessionStatus::Running,
            })
        }
        SessionEvent::TurnCompleted { turn_id } => output.push(NativeUiEvent::TurnChanged {
            turn_id: turn_id.as_str().to_owned(),
            status: SessionStatus::Completed,
        }),
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            output.push(NativeUiEvent::TurnChanged {
                turn_id: turn_id.as_str().to_owned(),
                status: if reason.status() == resource::TurnStatus::Cancelled {
                    SessionStatus::Interrupted
                } else {
                    SessionStatus::Failed
                },
            });
            output.push(NativeUiEvent::MessageUpserted(UiMessage {
                message_id: format!("stopped:{turn_id}"),
                turn_id: Some(turn_id.as_str().to_owned()),
                role: MessageRole::System,
                blocks: vec![MessageBlock::Error {
                    block_id: format!("stop:{turn_id}"),
                    code: format!("{reason:?}"),
                    message: projection::bounded_text(message, 64 * 1024),
                }],
                feedback: None,
                created_at_unix_ms: None,
            }));
        }
        _ => {}
    }
}
