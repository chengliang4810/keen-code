//! 权威 Journal 事件到 ACP 客户端投递的中立投影。
//!
//! 输入是 `SessionEventRecord` 与 Provider 投影状态，输出是
//! `DeliveryDraft`（SessionUpdate / KeenCodeEvent 加身份与序号事实），
//! 不绑定任何宿主传输（桌面 Tauri 事件或 headless NDJSON）。
//! 桌面 Host 与 headless Host 共享同一份投影语义，避免两侧漂移。

use std::collections::HashMap;
use std::sync::Arc;

use keencode_acp::{
    AgentLifecycleStatus, BackgroundTaskKind, BackgroundTaskTerminalStatus, FILE_CHANGE_META_KEY,
    FileChangeReference, FileSnapshotInfo, KeenCodeEvent, TurnFailureKind, schema,
};
use keencode_model::{ContentBlock, ImageSource};
use keencode_resources::{
    AgentId as ResourceAgentId, MessageRole as ResourceMessageRole, PersistedToolResult,
    ProviderSnapshot, RequestId, SessionEvent, SessionEventRecord, SessionMessage, SessionState,
    SubAgentStatus, TodoStatus, ToolCompletionStatus, ToolFileChange, TranscriptRecord,
    TranscriptSegment, TurnId as ResourceTurnId, TurnStatus, TurnStopReason,
};
use serde_json::json;

use crate::{RuntimeError, RuntimeSession};
use keencode_resources::ResourceError;

/// 面向 UI 的失败消息上限；与桌面侧展示上限保持一致。
pub const MAX_UI_ERROR_MESSAGE_BYTES: usize = 4 * 1024;

// 文件变更内联上限；超出改为 ResourceLink 引用，避免投递载荷失控。
const INLINE_FILE_CHANGE_BYTES: u64 = 32 * 1024;
const INLINE_FILE_CHANGE_JSON_BYTES: usize = 64 * 1024;

/// Session 默认 Provider 不能代表历史 Turn 实际使用的配置。模型 Round 只按
/// `turn_id` 读取原子持久的快照；缺失时保持未知，不回退到其他 Turn 或当前默认值。
/// 历史索引作为不可变基线，实时新事件只增量写入 `observed`，避免每条事件复制整张表。
#[derive(Debug, Default)]
pub struct ProviderProjection {
    /// 从完整 Journal 可重建索引获得的不可变 Turn 快照。
    pub indexed: Arc<HashMap<ResourceTurnId, ProviderSnapshot>>,
    /// 建立基线后从实时事件观察到的新 Turn 快照。
    pub observed: HashMap<ResourceTurnId, ProviderSnapshot>,
}

impl ProviderProjection {
    /// 从共享历史索引创建增量投影。
    pub fn from_indexed(indexed: Arc<HashMap<ResourceTurnId, ProviderSnapshot>>) -> Self {
        Self {
            indexed,
            observed: HashMap::new(),
        }
    }

    /// 返回指定 Turn 的权威 Provider，缺失时绝不回退到 Session 默认值。
    pub fn for_turn(&self, turn_id: &ResourceTurnId) -> Option<&ProviderSnapshot> {
        self.observed
            .get(turn_id)
            .or_else(|| self.indexed.get(turn_id))
    }

    /// 按 Journal 事件顺序增量更新 Turn Provider，并记录可回滚的当条事件变更。
    pub fn observe_event(
        &mut self,
        event: &SessionEvent,
        delta: &mut ProviderProjectionDelta,
    ) -> Result<(), RuntimeError> {
        match event {
            SessionEvent::AtomicBatch { events } => {
                for nested in events {
                    self.observe_event(nested, delta)?;
                }
            }
            SessionEvent::TurnProviderSnapshotRecorded {
                turn_id, provider, ..
            } => {
                if let Some(existing) = self.for_turn(turn_id) {
                    if existing != provider {
                        return Err(RuntimeError::ProjectionInconsistent);
                    }
                } else {
                    self.observed.insert(turn_id.clone(), provider.clone());
                    delta.inserted.push(turn_id.clone());
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// 撤销当条物理记录导入的增量，不触碰其他 Turn 快照。
    pub fn rollback(&mut self, delta: ProviderProjectionDelta) {
        for turn_id in delta.inserted.into_iter().rev() {
            self.observed.remove(&turn_id);
        }
    }

    /// 将索引基线和实时增量合并为新的共享基线。
    pub fn into_indexed(self) -> Arc<HashMap<ResourceTurnId, ProviderSnapshot>> {
        if self.observed.is_empty() {
            return self.indexed;
        }
        let mut providers =
            Arc::try_unwrap(self.indexed).unwrap_or_else(|shared| (*shared).clone());
        providers.extend(self.observed);
        Arc::new(providers)
    }
}
/// 一条物理 Journal 记录对 Provider 投影的有界变更。
#[derive(Debug, Default)]
pub struct ProviderProjectionDelta {
    /// 本记录首次观察到的 Turn；回滚时只删除这些增量。
    pub inserted: Vec<ResourceTurnId>,
}

/// 一个尚未分配当前桌面世代序号的投递草稿。
pub enum DeliveryDraft {
    /// 标准 ACP Session 更新草稿。
    SessionUpdate {
        /// 产生更新的可选 Turn。
        turn_id: Option<String>,
        /// 产生更新的可选 Agent。
        source_agent_id: Option<String>,
        /// 更新发生时的 UTC Unix 毫秒时间。
        occurred_at_ms: u64,
        /// 权威重放事件的 Journal 序号；实时增量为空。
        journal_sequence: Option<u64>,
        /// 原样保留的标准 ACP 更新。
        update: Box<keencode_acp::schema::SessionUpdate>,
    },
    /// KeenCode 生命周期扩展事件草稿。
    KeenCodeEvent {
        /// 产生事件的可选 Turn。
        turn_id: Option<String>,
        /// 产生事件的可选 Agent。
        source_agent_id: Option<String>,
        /// 权威事件的 Journal 序号；临时事件为空。
        journal_sequence: Option<u64>,
        /// 事件发生时的 UTC Unix 毫秒时间。
        occurred_at_ms: u64,
        /// 生命周期事件正文。
        event: KeenCodeEvent,
    },
}

/// 权威消息在实时流与历史重放中的投影方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthoritativeProjectionMode {
    /// 实时投影跳过已由模型增量发送的 Assistant 文本与推理。
    Live,
    /// 历史重放从持久 Transcript 重建完整 Assistant 内容。
    Replay,
}

/// 同一物理 Journal 记录递归投影其普通事件或原子批次时共享的只读上下文。
#[derive(Clone, Copy)]
pub struct AuthoritativeRecordContext<'a> {
    session: &'a RuntimeSession,
    state: &'a SessionState,
    record: &'a SessionEventRecord,
    mode: AuthoritativeProjectionMode,
    /// 会话正文脱敏钩子；headless 等纯文本宿主用它移除回显中的凭据。
    secret_redactor: Option<&'a dyn Fn(&str) -> String>,
}
/// 一条物理记录的投影结果；调用方必须在接受或拒绝该记录后显式提交或回滚。
#[must_use]
pub struct MappedAuthoritativeRecord {
    /// 投影产出的投递草稿；调用方接受后由宿主分配世代序号。
    pub drafts: Vec<DeliveryDraft>,
    /// 本记录对 Provider 投影的增量；接受时提交、拒绝时回滚。
    pub provider_delta: ProviderProjectionDelta,
}

impl MappedAuthoritativeRecord {
    /// 接受当前记录的 Provider 增量并返回投影草稿。
    pub fn commit(self) -> Vec<DeliveryDraft> {
        self.drafts
    }

    /// 拒绝当前记录，撤销它已导入的 Provider 增量。
    pub fn rollback(self, provider: &mut ProviderProjection) {
        provider.rollback(self.provider_delta);
    }
}

/// 将一条权威 Journal 记录映射为 live 与 replay 共用语义的 ACP 草稿集合。
/// 使用指定 Provider 投影映射一条权威 Journal 记录，错误时立即回滚当条增量。
pub fn map_authoritative_record_with_projection(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    mode: AuthoritativeProjectionMode,
    provider: &mut ProviderProjection,
    secret_redactor: Option<&dyn Fn(&str) -> String>,
) -> Result<MappedAuthoritativeRecord, RuntimeError> {
    let mut provider_delta = ProviderProjectionDelta::default();
    let drafts = match map_authoritative_event(
        AuthoritativeRecordContext {
            session,
            state,
            record,
            mode,
            secret_redactor,
        },
        &record.event,
        provider,
        &mut provider_delta,
        None,
    ) {
        Ok(drafts) => drafts,
        Err(error) => {
            provider.rollback(provider_delta);
            return Err(error);
        }
    };
    Ok(MappedAuthoritativeRecord {
        drafts,
        provider_delta,
    })
}

/// 递归映射普通事件或原子批次，并让批次内全部投递共享同一 Journal sequence。
pub fn map_authoritative_event(
    context: AuthoritativeRecordContext<'_>,
    event: &SessionEvent,
    provider: &mut ProviderProjection,
    provider_delta: &mut ProviderProjectionDelta,
    atomic_siblings: Option<&[SessionEvent]>,
) -> Result<Vec<DeliveryDraft>, RuntimeError> {
    let AuthoritativeRecordContext {
        session,
        state,
        record,
        mode,
        secret_redactor,
    } = context;
    if mode == AuthoritativeProjectionMode::Replay {
        if replay_hides_root_turn_lifecycle(state, event) {
            // 编辑重发会保留根 Turn 的终态骨架，以维持子 Agent 与 Mailbox
            // 控制面引用；没有对应真实用户消息时，这个骨架不属于对话投影。
            return Ok(Vec::new());
        }
        let request_id = match event {
            SessionEvent::ToolRequested { request } => Some(&request.request_id),
            SessionEvent::ToolExecutionStarted { request_id }
            | SessionEvent::ToolFileChangePrepared { request_id, .. }
            | SessionEvent::ToolFileChangeApplied { request_id }
            | SessionEvent::ToolCompleted { request_id, .. }
            | SessionEvent::ToolSideEffectUnknown { request_id, .. } => Some(request_id),
            _ => None,
        };
        if request_id
            .and_then(|request_id| state.tools.get(request_id))
            .is_some_and(|lifecycle| lifecycle.transcript_segment.is_some())
        {
            // 完整轮次按 Transcript 中推理、文本、工具的语义顺序恢复；尚未提交
            // Transcript 的崩溃/在途工具仍由各自权威生命周期事件正常投影。
            return Ok(Vec::new());
        }
    }
    let drafts = match event {
        SessionEvent::AtomicBatch { events } => {
            let mut drafts = Vec::new();
            for nested in events {
                drafts.extend(map_authoritative_event(
                    context,
                    nested,
                    provider,
                    provider_delta,
                    Some(events),
                )?);
            }
            drafts
        }
        SessionEvent::SessionCreated { title, .. } | SessionEvent::SessionRenamed { title, .. } => {
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::SessionInfoUpdate(
                    keencode_acp::schema::SessionInfoUpdate::new().title(title.clone()),
                ),
            )]
        }
        SessionEvent::SessionWorkspaceChanged { project_root, .. } => {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "keencode/projectRoot".to_owned(),
                serde_json::Value::String(project_root.clone()),
            );
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::SessionInfoUpdate(
                    keencode_acp::schema::SessionInfoUpdate::new().meta(meta),
                ),
            )]
        }
        SessionEvent::CommandReceiptCommitted { .. } => Vec::new(),
        SessionEvent::SessionPreferenceSet {
            pinned, archived, ..
        } => {
            // 偏好变更经 _meta 投影给前端；标题键不参与，键集与既有
            // session_info_update 白名单保持兼容。
            let mut meta = serde_json::Map::new();
            if let Some(pinned) = pinned {
                meta.insert(
                    "keencode/pinned".to_owned(),
                    serde_json::Value::Bool(*pinned),
                );
            }
            if let Some(archived) = archived {
                meta.insert(
                    "keencode/archived".to_owned(),
                    serde_json::Value::Bool(*archived),
                );
            }
            if meta.is_empty() {
                return Ok(Vec::new());
            }
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::SessionInfoUpdate(
                    keencode_acp::schema::SessionInfoUpdate::new().meta(meta),
                ),
            )]
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            ..
        } => {
            vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(source_agent_id.as_str()),
                KeenCodeEvent::TurnStarted {
                    root_turn_id: root_turn_id.as_str().to_owned(),
                    parent_turn_id: parent_turn_id
                        .as_ref()
                        .map(|turn_id| turn_id.as_str().to_owned()),
                },
            )]
        }
        SessionEvent::TurnCompleted { turn_id } => {
            let agent_id = turn_agent_id(state, turn_id.as_str())?;
            vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id),
                KeenCodeEvent::TurnCompleted,
            )]
        }
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            let agent_id = turn_agent_id(state, turn_id.as_str())?;
            let message =
                keencode_model::redact_error_secrets_bounded(message, MAX_UI_ERROR_MESSAGE_BYTES);
            let event = match reason {
                TurnStopReason::Cancelled => KeenCodeEvent::TurnCancelled,
                TurnStopReason::Failed => KeenCodeEvent::TurnFailed {
                    failure_kind: batch_failure_kind(atomic_siblings, turn_id),
                    message: message.clone(),
                },
                TurnStopReason::LimitReached => KeenCodeEvent::TurnFailed {
                    failure_kind: TurnFailureKind::Internal,
                    message: message.clone(),
                },
                TurnStopReason::ContextBlocked => KeenCodeEvent::TurnFailed {
                    failure_kind: TurnFailureKind::Context,
                    message: message.clone(),
                },
                TurnStopReason::ModelOutputLimit | TurnStopReason::ModelRefusal => {
                    KeenCodeEvent::TurnFailed {
                        failure_kind: TurnFailureKind::Model,
                        message: message.clone(),
                    }
                }
            };
            vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id),
                event,
            )]
        }
        SessionEvent::MessageAdded { message } => {
            map_persisted_message(session, state, record, message, mode, None, secret_redactor)?
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            let mut drafts = Vec::new();
            for message in &segment.messages {
                drafts.extend(map_persisted_message(
                    session,
                    state,
                    record,
                    message,
                    mode,
                    Some(segment),
                    secret_redactor,
                )?);
            }
            drafts
        }
        SessionEvent::DynamicInputReceiptCommitted {
            turn_id,
            source_agent_id,
            user_inputs,
            ..
        } => {
            user_inputs
                .iter()
                .map(|input| {
                    // 稳定身份来自已消费的权威队列，不从正文或当前插件目录推断。
                    let mut meta = schema::Meta::new();
                    meta.insert(
                        "keencode/messageId".into(),
                        json!(format!("{}:steer:{}", turn_id.as_str(), input.sequence)),
                    );
                    meta.insert("keencode/startsNewTurn".into(), json!(false));
                    if !input.references.is_empty() {
                        meta.insert("keencode/messageReferences".into(), json!(input.references));
                    }
                    let text = redact_session_text(secret_redactor, &input.text);
                    session_update_draft(
                        record,
                        Some(turn_id.as_str()),
                        Some(source_agent_id.as_str()),
                        schema::SessionUpdate::UserMessageChunk(
                            schema::ContentChunk::new(schema::ContentBlock::from(text))
                                .meta(Some(meta)),
                        ),
                    )
                })
                .collect()
        }
        SessionEvent::ToolRequested { request } => vec![session_update_draft(
            record,
            Some(request.turn_id.as_str()),
            Some(request.agent_id.as_str()),
            keencode_acp::schema::SessionUpdate::ToolCall(
                keencode_acp::schema::ToolCall::new(
                    request.model_tool_call_id.clone(),
                    request.tool_name.clone(),
                )
                .raw_input(request.arguments.clone()),
            ),
        )],
        SessionEvent::ToolExecutionStarted { request_id } => {
            let request = tool_request(state, request_id.as_str())?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        keencode_acp::schema::ToolCallUpdateFields::new()
                            .status(keencode_acp::schema::ToolCallStatus::InProgress),
                    ),
                ),
            )]
        }
        SessionEvent::ToolFileChangePrepared { request_id, change } => {
            change_update_drafts(session, state, record, request_id, change)?
        }
        SessionEvent::ToolFileChangeApplied { request_id } => {
            let change = state
                .tools
                .get(request_id)
                .and_then(|tool| tool.file_change.as_ref())
                .ok_or(RuntimeError::ProjectionInconsistent)?;
            change_update_drafts(session, state, record, request_id, change)?
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => {
            let request = tool_request(state, request_id.as_str())?;
            let fields = with_change_content(
                session,
                state,
                request_id,
                completed_fields(outcome.status, &outcome.result)?,
            )?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        fields,
                    )
                    .meta(Some(outcome_meta(outcome.status)?)),
                ),
            )]
        }
        SessionEvent::ToolSideEffectUnknown { request_id, result } => {
            let request = tool_request(state, request_id.as_str())?;
            let fields = with_change_content(
                session,
                state,
                request_id,
                completed_fields(ToolCompletionStatus::SideEffectUnknown, result)?,
            )?;
            vec![session_update_draft(
                record,
                Some(request.turn_id.as_str()),
                Some(request.agent_id.as_str()),
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        request.model_tool_call_id.clone(),
                        fields,
                    )
                    .meta(Some(outcome_meta(ToolCompletionStatus::SideEffectUnknown)?)),
                ),
            )]
        }
        SessionEvent::CompactionApplied {
            turn_id,
            source_agent_id,
            compaction,
            ..
        } => vec![keencode_event_draft(
            record,
            Some(turn_id.as_str()),
            Some(source_agent_id.as_str()),
            KeenCodeEvent::ContextCompactionCompleted {
                replaced_through_sequence: record.sequence.saturating_sub(1),
                estimated_tokens: compaction.estimated_tokens_after,
            },
        )],
        SessionEvent::SubAgentSpawned { agent } => {
            let siblings = atomic_siblings.ok_or(RuntimeError::ProjectionInconsistent)?;
            let mut matching_turns = siblings.iter().filter_map(|sibling| match sibling {
                SessionEvent::TurnStarted {
                    turn_id,
                    source_agent_id,
                    root_turn_id,
                    parent_turn_id: Some(parent_turn_id),
                    ..
                } if source_agent_id == &agent.agent_id => {
                    Some((turn_id, root_turn_id, parent_turn_id))
                }
                _ => None,
            });
            let (_child_turn_id, root_turn_id, parent_turn_id) = matching_turns
                .next()
                .ok_or(RuntimeError::ProjectionInconsistent)?;
            if matching_turns.next().is_some() {
                return Err(RuntimeError::ProjectionInconsistent);
            }
            vec![keencode_event_draft(
                record,
                Some(parent_turn_id.as_str()),
                Some(agent.parent_agent_id.as_str()),
                KeenCodeEvent::AgentSpawned {
                    agent_id: agent.agent_id.as_str().to_owned(),
                    parent_agent_id: agent.parent_agent_id.as_str().to_owned(),
                    agent_path: agent.agent_path.clone(),
                    task: agent.task.clone(),
                    parent_turn_id: parent_turn_id.as_str().to_owned(),
                    root_turn_id: root_turn_id.as_str().to_owned(),
                },
            )]
        }
        SessionEvent::SubAgentStatusChanged {
            agent_id,
            turn_id,
            status,
            result_summary,
        } => {
            let Some(turn_id) = turn_id.as_ref() else {
                return Ok(Vec::new());
            };
            let mut drafts = vec![keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(agent_id.as_str()),
                KeenCodeEvent::AgentStatusChanged {
                    agent_id: agent_id.as_str().to_owned(),
                    status: map_agent_status(status),
                },
            )];
            if let Some(completion) = agent_background_task_completion_draft(
                state,
                record,
                agent_id,
                turn_id,
                status,
                result_summary.as_deref(),
            )? {
                drafts.push(completion);
            }
            drafts
        }
        SessionEvent::MailboxMessageQueued { message } => vec![keencode_event_draft(
            record,
            Some(message.related_turn_id.as_str()),
            Some(message.from.as_str()),
            KeenCodeEvent::AgentMessageQueued {
                message_id: message.message_id.as_str().to_owned(),
                from_agent_id: message.from.as_str().to_owned(),
                to_agent_id: message.to.as_str().to_owned(),
            },
        )],
        SessionEvent::ModelRoundCompleted {
            turn_id,
            source_agent_id,
            requested_model,
            usage,
            metadata,
            ..
        } => {
            let mut drafts = model_round_usage_draft(
                record,
                provider.for_turn(turn_id),
                turn_id,
                source_agent_id,
                requested_model,
                usage,
            );
            // 每个原子批次至多提交一个模型 Round；记录 ID 在 live/replay 中稳定。
            // 即使用量全未知也保留这次请求，避免把部分请求之和冒充整轮总量。
            drafts.push(keencode_event_draft(
                record,
                Some(turn_id.as_str()),
                Some(source_agent_id.as_str()),
                KeenCodeEvent::ModelUsageReported {
                    observation_id: record.event_id.as_str().to_owned(),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens: usage.total_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    cache_creation_tokens: usage.cache_write_tokens,
                    decode_duration_ms: metadata.decode_duration_ms,
                },
            ));
            drafts
        }
        SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TerminalStarted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. } => Vec::new(),
        SessionEvent::TodoReplaced {
            items, revision, ..
        } => {
            let mut meta = keencode_acp::schema::Meta::new();
            meta.insert("_keencode".to_owned(), json!({ "todoRevision": revision }));
            vec![session_update_draft(
                record,
                None,
                None,
                keencode_acp::schema::SessionUpdate::Plan(
                    keencode_acp::schema::Plan::new(
                        items
                            .iter()
                            .map(|item| {
                                keencode_acp::schema::PlanEntry::new(
                                    item.content.clone(),
                                    keencode_acp::schema::PlanEntryPriority::Medium,
                                    match item.status {
                                        TodoStatus::Pending => {
                                            keencode_acp::schema::PlanEntryStatus::Pending
                                        }
                                        TodoStatus::InProgress => {
                                            keencode_acp::schema::PlanEntryStatus::InProgress
                                        }
                                        TodoStatus::Completed => {
                                            keencode_acp::schema::PlanEntryStatus::Completed
                                        }
                                    },
                                )
                            })
                            .collect(),
                    )
                    .meta(meta),
                ),
            )]
        }
        // Assistant 反馈是桌面行元数据，不映射为 ACP 标准会话更新；V4 snapshot
        // 会在同一权威 Journal 事件后重新投影该行。
        SessionEvent::AssistantFeedbackSet { .. } => Vec::new(),
        SessionEvent::PlanChanged { plan } => vec![session_update_draft(
            record,
            None,
            None,
            keencode_acp::schema::SessionUpdate::CurrentModeUpdate(
                keencode_acp::schema::CurrentModeUpdate::new(if plan.enabled {
                    "plan"
                } else {
                    "default"
                }),
            ),
        )],
        SessionEvent::FollowupModeChanged { .. } | SessionEvent::InputQueueChanged { .. } => {
            Vec::new()
        }
        SessionEvent::ProviderSnapshotUpdated { .. } => Vec::new(),
        SessionEvent::TurnProviderSnapshotRecorded { .. } => {
            provider.observe_event(event, provider_delta)?;
            Vec::new()
        }
        SessionEvent::OnErrorHookQueued { .. }
        | SessionEvent::OnErrorHookReceiptCommitted { .. } => Vec::new(),
        // 工作流的 JSON 契约由桌面网关直接投影，ACP 标准会话更新没有对应的控制节点类型。
        SessionEvent::WorkflowEventCommitted { .. } => Vec::new(),
        SessionEvent::TitleGenerated { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => Vec::new(),
    };
    Ok(drafts)
}

/// 从与 Turn 终态同批提交的 OnError outbox 推导 UI 失败分类。
///
/// `TurnStopped` 只携带停止原因，Provider 中立分类由同批 `OnErrorHookQueued` 提供；
/// 只有 Provider 边界失败属于模型失败，缺失或未知分类保守归为内部错误。
pub fn batch_failure_kind(
    siblings: Option<&[SessionEvent]>,
    turn_id: &ResourceTurnId,
) -> TurnFailureKind {
    let category = siblings.and_then(|events| {
        events.iter().find_map(|event| match event {
            SessionEvent::OnErrorHookQueued { invocation } if &invocation.turn_id == turn_id => {
                Some(invocation.error_category.as_str())
            }
            _ => None,
        })
    });
    match category {
        Some(
            "authentication_failed"
            | "billing_error"
            | "model_not_found"
            | "rate_limit"
            | "overloaded"
            | "invalid_request"
            | "server_error",
        ) => TurnFailureKind::Model,
        _ => TurnFailureKind::Internal,
    }
}

/// 回放时隐藏仅为控制面引用保留、但已没有独立真实用户消息的根 Turn 生命周期。
pub fn replay_hides_root_turn_lifecycle(state: &SessionState, event: &SessionEvent) -> bool {
    let turn_id = match event {
        SessionEvent::TurnStarted { turn_id, .. }
        | SessionEvent::TurnCompleted { turn_id }
        | SessionEvent::TurnStopped { turn_id, .. } => turn_id,
        _ => return false,
    };
    let Some(turn) = state.turns.get(turn_id) else {
        return false;
    };
    let is_root_turn = turn.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
        && turn.root_turn_id == turn.turn_id
        && turn.parent_turn_id.is_none();
    // Goal 自动续跑只有模型专用输入，但已有真实 Assistant 轨迹时仍须回放轮次边界。
    if is_root_turn
        && turn_id.as_str().starts_with("turn-goal-")
        && state.transcript.iter().any(|record| {
            matches!(record, TranscriptRecord::SegmentCommitted(segment)
            if segment.turn_id == *turn_id
                && segment.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID
                && segment.messages.iter().any(|message| {
                    !message.is_meta && message.role == ResourceMessageRole::Assistant
                }))
        })
    {
        return false;
    }
    is_root_turn
        && !state.transcript.iter().any(|record| {
            matches!(
                record,
                TranscriptRecord::MessageAdded(message)
                    if !message.is_meta
                        && message.role == ResourceMessageRole::User
                        && message.agent_id.is_none()
                        && message.turn_id.as_ref() == Some(turn_id)
            )
        })
}

/// 通过标准 ACP 扩展槽携带资源锚点，不冒充要求 UUID 的未启用 messageId 字段。
pub fn persisted_message_meta(message_id: &str) -> keencode_acp::schema::Meta {
    serde_json::Map::from_iter([(
        "keencode/messageId".to_owned(),
        serde_json::Value::String(message_id.to_owned()),
    )])
}

/// 引用随每个消息块携带，冷回放和实时投递采用同一权威元数据，不能从当前目录反推历史身份。
fn persisted_message_references_meta(message: &SessionMessage) -> schema::Meta {
    let mut meta = persisted_message_meta(&message.message_id);
    if !message.references.is_empty() {
        meta.insert(
            "keencode/messageReferences".into(),
            json!(message.references),
        );
    }
    meta
}

/// 会话正文脱敏；无钩子时原样返回，纯文本宿主用来移除回显中的凭据。
fn redact_session_text(redactor: Option<&dyn Fn(&str) -> String>, text: &str) -> String {
    redactor.map_or_else(|| text.to_owned(), |redact| redact(text))
}
/// 将一条已物化的持久消息拆为标准用户、Agent 或推理内容更新。
pub fn map_persisted_message(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    message: &SessionMessage,
    mode: AuthoritativeProjectionMode,
    segment: Option<&TranscriptSegment>,
    secret_redactor: Option<&dyn Fn(&str) -> String>,
) -> Result<Vec<DeliveryDraft>, RuntimeError> {
    if message.is_meta
        || matches!(
            message.role,
            ResourceMessageRole::System | ResourceMessageRole::Developer
        )
    {
        return Ok(Vec::new());
    }
    let materialized = session.materialize_message(message)?;
    let turn_id = message.turn_id.as_ref().map(|turn_id| turn_id.as_str());
    let agent_id = match (message.agent_id.as_ref(), turn_id) {
        (Some(agent_id), _) => Some(agent_id.as_str()),
        (None, Some(turn_id)) => Some(turn_agent_id(state, turn_id)?),
        (None, None) => None,
    };
    let mut drafts = Vec::new();
    for block in materialized.content {
        if mode == AuthoritativeProjectionMode::Replay
            && let Some(segment) = segment
            && let Some(tool_drafts) = replay_segment_tool_drafts(
                session,
                state,
                record,
                segment,
                &block,
                secret_redactor,
            )?
        {
            drafts.extend(tool_drafts);
            continue;
        }
        let update = match block {
            ContentBlock::Text { text } => {
                if message.role == ResourceMessageRole::Tool
                    || (message.role == ResourceMessageRole::Assistant
                        && mode == AuthoritativeProjectionMode::Live)
                {
                    continue;
                }
                let text = redact_session_text(secret_redactor, &text);
                let chunk = keencode_acp::schema::ContentChunk::new(
                    keencode_acp::schema::ContentBlock::from(text),
                )
                .meta(Some(persisted_message_references_meta(message)));
                if message.role == ResourceMessageRole::User {
                    keencode_acp::schema::SessionUpdate::UserMessageChunk(chunk)
                } else {
                    keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk)
                }
            }
            ContentBlock::Reasoning { reasoning } => {
                if message.role != ResourceMessageRole::Assistant
                    || mode == AuthoritativeProjectionMode::Live
                {
                    continue;
                }
                let reasoning_text = redact_session_text(secret_redactor, &reasoning.text);
                keencode_acp::schema::SessionUpdate::AgentThoughtChunk(
                    keencode_acp::schema::ContentChunk::new(
                        keencode_acp::schema::ContentBlock::from(reasoning_text),
                    )
                    .meta(Some(persisted_message_references_meta(message))),
                )
            }
            ContentBlock::Image { image } => {
                if !matches!(
                    message.role,
                    ResourceMessageRole::User | ResourceMessageRole::Assistant
                ) {
                    continue;
                }
                let content = match image.source {
                    ImageSource::Base64 { media_type, data } => {
                        keencode_acp::schema::ContentBlock::Image(
                            keencode_acp::schema::ImageContent::new(data, media_type),
                        )
                    }
                    ImageSource::Url { url } => keencode_acp::schema::ContentBlock::ResourceLink(
                        keencode_acp::schema::ResourceLink::new("image", url),
                    ),
                };
                let chunk = keencode_acp::schema::ContentChunk::new(content)
                    .meta(Some(persisted_message_references_meta(message)));
                if message.role == ResourceMessageRole::User {
                    keencode_acp::schema::SessionUpdate::UserMessageChunk(chunk)
                } else {
                    keencode_acp::schema::SessionUpdate::AgentMessageChunk(chunk)
                }
            }
            ContentBlock::ToolCall { tool_call } => {
                if message.role != ResourceMessageRole::Assistant
                    || (mode == AuthoritativeProjectionMode::Live
                        && persisted_tool_lifecycle_exists(state, turn_id, agent_id, &tool_call.id))
                {
                    continue;
                }
                keencode_acp::schema::SessionUpdate::ToolCall(
                    keencode_acp::schema::ToolCall::new(tool_call.id, tool_call.name)
                        .raw_input(tool_call.arguments),
                )
            }
            ContentBlock::ToolResult { tool_result } => {
                if message.role != ResourceMessageRole::Tool
                    || (mode == AuthoritativeProjectionMode::Live
                        && persisted_tool_lifecycle_exists(
                            state,
                            turn_id,
                            agent_id,
                            &tool_result.tool_call_id,
                        ))
                {
                    continue;
                }
                let status = if tool_result.is_error {
                    keencode_acp::schema::ToolCallStatus::Failed
                } else {
                    keencode_acp::schema::ToolCallStatus::Completed
                };
                // raw_output 统一为 camelCase 的完整 ToolResult 信封（toolCallId/content/isError），
                // 与 completed_fields 的形状约定一致；裸数组形状会让前端的
                // 图片/Artifact 识别退化为原始 JSON 文本。错误正文同样脱敏。
                let mut projected = tool_result.clone();
                if projected.is_error {
                    for part in &mut projected.content {
                        if let keencode_model::ToolResultContent::Text { text } = part {
                            *text = keencode_model::redact_error_secrets(text);
                        }
                    }
                }
                let raw_output = serde_json::to_value(projected).map_err(|error| {
                    RuntimeError::Resource(ResourceError::Json(error.to_string()))
                })?;
                keencode_acp::schema::SessionUpdate::ToolCallUpdate(
                    keencode_acp::schema::ToolCallUpdate::new(
                        tool_result.tool_call_id,
                        keencode_acp::schema::ToolCallUpdateFields::new()
                            .status(status)
                            .raw_output(raw_output),
                    ),
                )
            }
        };
        // Session 级用户消息按资源层契约可以省略 Turn 与 Agent 身份；Assistant/Tool
        // 更新仍必须具备可审查的来源边界。
        if !matches!(message.role, ResourceMessageRole::User)
            && (turn_id.is_none() || agent_id.is_none())
        {
            return Err(RuntimeError::ProjectionInconsistent);
        }
        drafts.push(session_update_draft(record, turn_id, agent_id, update));
    }
    Ok(drafts)
}

/// 在已提交段的语义位置重建工具生命周期，精确绑定 Round/段而非仅凭可能复用的模型 ID。
pub fn replay_segment_tool_drafts(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    segment: &TranscriptSegment,
    block: &ContentBlock,
    secret_redactor: Option<&dyn Fn(&str) -> String>,
) -> Result<Option<Vec<DeliveryDraft>>, RuntimeError> {
    let (tool_call_id, is_request) = match block {
        ContentBlock::ToolCall { tool_call } => (tool_call.id.as_str(), true),
        ContentBlock::ToolResult { tool_result } => (tool_result.tool_call_id.as_str(), false),
        _ => return Ok(None),
    };
    let lifecycle = state.tools.values().find(|lifecycle| {
        lifecycle.request.model_tool_call_id == tool_call_id
            && lifecycle
                .transcript_segment
                .as_ref()
                .is_some_and(|reference| {
                    reference.turn_id == segment.turn_id
                        && reference.source_agent_id == segment.source_agent_id
                        && reference.model_round == segment.model_round
                        && reference.segment_index == segment.segment_index
                        && Some(reference.transcript_revision)
                            == segment.expected_transcript_revision.checked_add(1)
                })
    });
    let Some(lifecycle) = lifecycle else {
        // 未进入执行生命周期的模型可见错误仍由 Transcript 工具块自身恢复。
        return Ok(None);
    };
    let mut events = Vec::with_capacity(2);
    if is_request {
        events.push((
            lifecycle.requested_at_unix_ms,
            SessionEvent::ToolRequested {
                request: lifecycle.request.clone(),
            },
        ));
        if let Some(started_at) = lifecycle.execution_started_at_unix_ms {
            events.push((
                started_at,
                SessionEvent::ToolExecutionStarted {
                    request_id: lifecycle.request.request_id.clone(),
                },
            ));
        }
    } else {
        events.push((
            lifecycle
                .completed_at_unix_ms
                .ok_or(RuntimeError::ProjectionInconsistent)?,
            SessionEvent::ToolCompleted {
                request_id: lifecycle.request.request_id.clone(),
                outcome: lifecycle
                    .outcome
                    .clone()
                    .ok_or(RuntimeError::ProjectionInconsistent)?,
            },
        ));
    }
    let mut drafts = Vec::new();
    let mut provider = ProviderProjection::default();
    let mut provider_delta = ProviderProjectionDelta::default();
    for (time_unix_ms, event) in events {
        // 保留生命周期真实时间和与 live 相同的 raw output；Journal 游标则归属于
        // 当前原子段，不能倒退到已被前页消费的物理请求记录。
        let projected = SessionEventRecord {
            schema: record.schema.clone(),
            version: record.version,
            event_id: record.event_id.clone(),
            session: record.session.clone(),
            sequence: record.sequence,
            time_unix_ms,
            event,
        };
        drafts.extend(map_authoritative_event(
            AuthoritativeRecordContext {
                session,
                state,
                record: &projected,
                mode: AuthoritativeProjectionMode::Live,
                secret_redactor,
            },
            &projected.event,
            &mut provider,
            &mut provider_delta,
            None,
        )?);
    }
    Ok(Some(drafts))
}

/// 判断一个 Transcript 工具块是否已有完整资源层 lifecycle，存在时由专用事件投影。
pub fn persisted_tool_lifecycle_exists(
    state: &SessionState,
    turn_id: Option<&str>,
    agent_id: Option<&str>,
    model_tool_call_id: &str,
) -> bool {
    let (Some(turn_id), Some(agent_id)) = (turn_id, agent_id) else {
        return false;
    };
    state.tools.values().any(|lifecycle| {
        lifecycle.request.turn_id.as_str() == turn_id
            && lifecycle.request.agent_id.as_str() == agent_id
            && lifecycle.request.model_tool_call_id == model_tool_call_id
    })
}

/// 构造带权威 Journal sequence 的标准 Session 更新草稿。
pub fn session_update_draft(
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    update: keencode_acp::schema::SessionUpdate,
) -> DeliveryDraft {
    DeliveryDraft::SessionUpdate {
        turn_id: turn_id.map(str::to_owned),
        source_agent_id: source_agent_id.map(str::to_owned),
        occurred_at_ms: record.time_unix_ms,
        journal_sequence: Some(record.sequence),
        update: Box::new(update),
    }
}

/// 将权威模型 Round 的明确用量投影为标准 ACP 上下文用量更新。
///
/// `total_tokens` 优先使用 Provider 明确报告的总数；缺少总数时才在输入和
/// 输出都明确报告时相加。上下文窗口或用量任一未知都不生成更新，避免把
/// 未知值伪造成零或把不完整的 Token 统计展示为事实。
pub fn model_round_usage_draft(
    record: &SessionEventRecord,
    provider: Option<&ProviderSnapshot>,
    turn_id: &ResourceTurnId,
    source_agent_id: &ResourceAgentId,
    requested_model: &str,
    usage: &keencode_model::TokenUsage,
) -> Vec<DeliveryDraft> {
    let Some(context_window) = provider
        .filter(|provider| provider.model == requested_model)
        .and_then(|provider| provider.context_window)
        .filter(|context_window| *context_window > 0)
    else {
        return Vec::new();
    };
    let used = usage.total_tokens.or_else(|| {
        usage
            .input_tokens
            .zip(usage.output_tokens)
            .and_then(|(input, output)| input.checked_add(output))
    });
    let Some(used) = used else {
        return Vec::new();
    };
    vec![session_update_draft(
        record,
        Some(turn_id.as_str()),
        Some(source_agent_id.as_str()),
        keencode_acp::schema::SessionUpdate::UsageUpdate(keencode_acp::schema::UsageUpdate::new(
            used,
            context_window,
        )),
    )]
}

/// 构造带权威 Journal sequence 的 KeenCode 生命周期草稿。
pub fn keencode_event_draft(
    record: &SessionEventRecord,
    turn_id: Option<&str>,
    source_agent_id: Option<&str>,
    event: KeenCodeEvent,
) -> DeliveryDraft {
    DeliveryDraft::KeenCodeEvent {
        turn_id: turn_id.map(str::to_owned),
        source_agent_id: source_agent_id.map(str::to_owned),
        journal_sequence: Some(record.sequence),
        occurred_at_ms: record.time_unix_ms,
        event,
    }
}

/// 从当前一致快照解析 Turn 的 Agent 身份。
pub fn turn_agent_id<'a>(state: &'a SessionState, turn_id: &str) -> Result<&'a str, RuntimeError> {
    state
        .turns
        .iter()
        .find(|(known_turn_id, _)| known_turn_id.as_str() == turn_id)
        .map(|(_, turn)| turn.source_agent_id.as_str())
        .ok_or(RuntimeError::ProjectionInconsistent)
}

/// 从当前一致快照解析工具生命周期的不可变请求。
pub fn tool_request<'a>(
    state: &'a SessionState,
    request_id: &str,
) -> Result<&'a keencode_resources::ToolRequest, RuntimeError> {
    state
        .tools
        .iter()
        .find(|(known_request_id, _)| known_request_id.as_str() == request_id)
        .map(|(_, lifecycle)| &lifecycle.request)
        .ok_or(RuntimeError::ProjectionInconsistent)
}

/// 将资源层单层 Agent 状态映射为桌面生命周期状态。
pub fn map_agent_status(status: &SubAgentStatus) -> AgentLifecycleStatus {
    match status {
        SubAgentStatus::Pending => AgentLifecycleStatus::Pending,
        SubAgentStatus::Running => AgentLifecycleStatus::Running,
        SubAgentStatus::Waiting => AgentLifecycleStatus::Waiting,
        SubAgentStatus::Completed => AgentLifecycleStatus::Completed,
        SubAgentStatus::Failed => AgentLifecycleStatus::Failed,
        SubAgentStatus::Interrupted => AgentLifecycleStatus::Interrupted,
        SubAgentStatus::Stopped => AgentLifecycleStatus::Stopped,
    }
}

/// 构造一个通过 ACP 严格校验的后台完成事件；不安全摘要会被单独省略。
pub fn validated_background_task_completion_event(
    task_id: &str,
    task_kind: BackgroundTaskKind,
    agent_id: Option<&str>,
    status: BackgroundTaskTerminalStatus,
    duration_ms: u64,
    summary: Option<&str>,
) -> Option<KeenCodeEvent> {
    let summary = summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned);
    let event = KeenCodeEvent::BackgroundTaskCompleted {
        task_id: task_id.to_owned(),
        task_kind,
        agent_id: agent_id.map(str::to_owned),
        status,
        duration_ms,
        summary,
    };
    if event.validate().is_ok() {
        return Some(event);
    }
    let fallback = KeenCodeEvent::BackgroundTaskCompleted {
        task_id: task_id.to_owned(),
        task_kind,
        agent_id: agent_id.map(str::to_owned),
        status,
        duration_ms,
        summary: None,
    };
    fallback.validate().is_ok().then_some(fallback)
}

/// 从子 Agent 权威终态与 Turn 时间生成 Session 级后台完成草稿。
pub fn agent_background_task_completion_draft(
    state: &SessionState,
    record: &SessionEventRecord,
    agent_id: &ResourceAgentId,
    turn_id: &ResourceTurnId,
    status: &SubAgentStatus,
    result_summary: Option<&str>,
) -> Result<Option<DeliveryDraft>, RuntimeError> {
    let (terminal_status, expected_turn_status) = match status {
        SubAgentStatus::Completed => (
            BackgroundTaskTerminalStatus::Succeeded,
            TurnStatus::Completed,
        ),
        SubAgentStatus::Failed => (BackgroundTaskTerminalStatus::Failed, TurnStatus::Failed),
        SubAgentStatus::Interrupted | SubAgentStatus::Stopped => (
            BackgroundTaskTerminalStatus::Cancelled,
            TurnStatus::Cancelled,
        ),
        SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting => {
            return Ok(None);
        }
    };
    let turn = state
        .turns
        .get(turn_id)
        .filter(|turn| turn.source_agent_id == *agent_id && turn.status == expected_turn_status)
        .ok_or(RuntimeError::ProjectionInconsistent)?;
    let completed_at_unix_ms = turn
        .completed_at_unix_ms
        .ok_or(RuntimeError::ProjectionInconsistent)?;
    let event = validated_background_task_completion_event(
        turn_id.as_str(),
        BackgroundTaskKind::Agent,
        Some(agent_id.as_str()),
        terminal_status,
        completed_at_unix_ms.saturating_sub(turn.started_at_unix_ms),
        result_summary,
    )
    .ok_or(RuntimeError::ProjectionInconsistent)?;
    Ok(Some(DeliveryDraft::KeenCodeEvent {
        turn_id: None,
        source_agent_id: None,
        journal_sequence: None,
        occurred_at_ms: record.time_unix_ms,
        event,
    }))
}

/// 标准 ACP 无取消枚举；保持其状态合法，并用自有元数据保留不可丢失的真实终态。
pub fn completed_fields(
    outcome: ToolCompletionStatus,
    result: &PersistedToolResult,
) -> Result<schema::ToolCallUpdateFields, RuntimeError> {
    let status = match outcome {
        ToolCompletionStatus::Succeeded => schema::ToolCallStatus::Completed,
        ToolCompletionStatus::Failed
        | ToolCompletionStatus::Cancelled
        | ToolCompletionStatus::SideEffectUnknown => schema::ToolCallStatus::Failed,
    };
    let mut projected = result.clone();
    if projected.is_error || outcome != ToolCompletionStatus::Succeeded {
        for part in &mut projected.content {
            if let keencode_resources::ToolResultPart::Text { text } = part {
                *text = keencode_model::redact_error_secrets(text);
            }
        }
    }
    let raw = serde_json::to_value(projected).map_err(|_| RuntimeError::ProjectionInconsistent)?;
    // 正文只保留在当前唯一结果结构中；重复复制到 content 会放大转义后的投递大小。
    Ok(schema::ToolCallUpdateFields::new()
        .status(status)
        .raw_output(raw))
}

/// 元数据位于标准 ToolCallUpdate 顶层，不扩展 ACP 的状态枚举或 rawOutput 形状。
pub fn outcome_meta(outcome: ToolCompletionStatus) -> Result<schema::Meta, RuntimeError> {
    let precise =
        serde_json::to_value(outcome).map_err(|_| RuntimeError::ProjectionInconsistent)?;
    Ok(serde_json::Map::from_iter([(
        "keencode/toolOutcome".to_owned(),
        precise,
    )]))
}
pub fn change_reference(
    session: &RuntimeSession,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> FileChangeReference {
    let info = |snapshot: &keencode_resources::FileSnapshot| FileSnapshotInfo {
        size_bytes: snapshot.size_bytes,
        sha256: snapshot.sha256.clone(),
    };
    FileChangeReference {
        session_id: session.session_id().as_str().to_owned(),
        request_id: request_id.as_str().to_owned(),
        path: change.path.clone(),
        before: change.before.as_ref().map(info),
        after: info(&change.after),
        applied: change.applied,
    }
}

/// 小型已应用 UTF-8 变更使用标准 Diff，其余使用标准 ResourceLink 和命名空间元数据。
pub fn change_content(
    session: &RuntimeSession,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> Result<Vec<schema::ToolCallContent>, RuntimeError> {
    let total_bytes = change
        .before
        .as_ref()
        .map_or(0, |before| before.size_bytes)
        .saturating_add(change.after.size_bytes);
    if change.applied && total_bytes <= INLINE_FILE_CHANGE_BYTES {
        let before = change
            .before
            .as_ref()
            .map(|before| session.read_file_snapshot(before))
            .transpose()
            .map_err(|_| RuntimeError::ProjectionInconsistent)?;
        let after = session
            .read_file_snapshot(&change.after)
            .map_err(|_| RuntimeError::ProjectionInconsistent)?;
        let before_text = before.map(String::from_utf8).transpose();
        let after_text = String::from_utf8(after);
        if let (Ok(before_text), Ok(after_text)) = (before_text, after_text)
            && !before_text
                .as_ref()
                .is_some_and(|value| value.contains('\0'))
            && !after_text.contains('\0')
        {
            let content = vec![schema::ToolCallContent::Diff(
                schema::Diff::new(&change.path, after_text).old_text(before_text),
            )];
            if serde_json::to_vec(&content)
                .map_err(|_| RuntimeError::ProjectionInconsistent)?
                .len()
                <= INLINE_FILE_CHANGE_JSON_BYTES
            {
                return Ok(content);
            }
        }
    }
    let reference = change_reference(session, request_id, change);
    // Session 和 Request ID 已由资源层约束为单个安全 ASCII 路径段。
    let uri = format!(
        "keencode://sessions/{}/file-changes/{}",
        reference.session_id, reference.request_id
    );
    let mut meta = schema::Meta::new();
    meta.insert(
        FILE_CHANGE_META_KEY.to_owned(),
        serde_json::to_value(reference).map_err(|_| RuntimeError::ProjectionInconsistent)?,
    );
    Ok(vec![schema::ToolCallContent::Content(
        schema::Content::new(schema::ContentBlock::ResourceLink(
            schema::ResourceLink::new("文件变更快照", uri)
                .meta(meta)
                .description(if change.applied {
                    "已应用的持久文件快照"
                } else {
                    "已准备快照，尚未确认文件应用结果"
                }),
        )),
    )])
}

/// 工具终态更新始终携带权威快照，确保 live、在途恢复和 Transcript 冷重放语义一致。
pub fn with_change_content(
    session: &RuntimeSession,
    state: &SessionState,
    request_id: &RequestId,
    mut fields: schema::ToolCallUpdateFields,
) -> Result<schema::ToolCallUpdateFields, RuntimeError> {
    match state
        .tools
        .get(request_id)
        .and_then(|tool| tool.file_change.as_ref())
    {
        Some(change) => {
            let mut content = fields.content.take().unwrap_or_default();
            content.extend(change_content(session, request_id, change)?);
            Ok(fields.content(content))
        }
        None => Ok(fields),
    }
}

/// 以标准工具更新投递 Prepared/Applied，不把准备阶段误报成实际文件变更。
pub fn change_update_drafts(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> Result<Vec<DeliveryDraft>, RuntimeError> {
    let request = tool_request(state, request_id.as_str())?;
    Ok(vec![session_update_draft(
        record,
        Some(request.turn_id.as_str()),
        Some(request.agent_id.as_str()),
        schema::SessionUpdate::ToolCallUpdate(schema::ToolCallUpdate::new(
            request.model_tool_call_id.clone(),
            schema::ToolCallUpdateFields::new()
                .content(change_content(session, request_id, change)?),
        )),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_resources::ToolResultPart;

    /// 用真实信封序列化验证单段内联上限，转义不能被重复正文放大到投递边界之外。
    #[test]
    fn escaped_inline_result_fits_delivery_budget() {
        let result = PersistedToolResult {
            tool_call_id: "call-budget".to_owned(),
            content: vec![ToolResultPart::Text {
                text: "\\".repeat(64 * 1024),
            }],
            is_error: false,
        };
        let update = schema::ToolCallUpdate::new(
            result.tool_call_id.clone(),
            completed_fields(ToolCompletionStatus::Succeeded, &result).unwrap(),
        )
        .meta(Some(outcome_meta(ToolCompletionStatus::Succeeded).unwrap()));
        let envelope = keencode_acp::SessionUpdateDeliveryEnvelope::new(
            "session-budget",
            Some("turn-budget".to_owned()),
            Some("root".to_owned()),
            1,
            1,
            schema::SessionUpdate::ToolCallUpdate(update),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(
            bytes.len() <= keencode_acp::SessionUpdateDeliveryLimits::default().max_bytes(),
            "工具投递实际为 {} 字节",
            bytes.len()
        );
        keencode_acp::SessionUpdateDeliveryEnvelope::decode_raw(&bytes).unwrap();
    }

    /// 多段文本加上实际 Diff 内容后仍只编码一份结果，且标准内容不会覆盖原始输出。
    #[test]
    fn multipart_text_and_diff_fit_delivery_budget_without_loss() {
        let result = PersistedToolResult {
            tool_call_id: "call-multipart".to_owned(),
            content: vec![
                ToolResultPart::Text {
                    text: "\\".repeat(32 * 1024),
                },
                ToolResultPart::Text {
                    text: "\"".repeat(32 * 1024),
                },
            ],
            is_error: false,
        };
        let fields = completed_fields(ToolCompletionStatus::Succeeded, &result)
            .unwrap()
            .content(vec![schema::ToolCallContent::Diff(
                schema::Diff::new("result.txt", "after".repeat(4 * 1024))
                    .old_text("before".repeat(4 * 1024)),
            )]);
        let envelope = keencode_acp::SessionUpdateDeliveryEnvelope::new(
            "session-budget",
            Some("turn-budget".to_owned()),
            Some("root".to_owned()),
            1,
            1,
            schema::SessionUpdate::ToolCallUpdate(schema::ToolCallUpdate::new(
                "call-multipart",
                fields,
            )),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let recovered = keencode_acp::SessionUpdateDeliveryEnvelope::decode_raw(&bytes).unwrap();
        let value = serde_json::to_value(recovered).unwrap();
        assert_eq!(
            value["update"]["rawOutput"],
            serde_json::to_value(result).unwrap()
        );
        assert_eq!(
            value["update"]["content"][0]["newText"],
            "after".repeat(4 * 1024)
        );
    }

    /// 四种真实终态均保留，模型可见错误标志与界面取消语义互不混淆。
    #[test]
    fn exact_outcomes_preserve_standard_status_and_unique_raw_result() {
        for (outcome, precise, status) in [
            (ToolCompletionStatus::Succeeded, "succeeded", "completed"),
            (ToolCompletionStatus::Failed, "failed", "failed"),
            (ToolCompletionStatus::Cancelled, "cancelled", "failed"),
            (
                ToolCompletionStatus::SideEffectUnknown,
                "side_effect_unknown",
                "failed",
            ),
        ] {
            let result = PersistedToolResult {
                tool_call_id: "call-native".to_owned(),
                content: vec![ToolResultPart::Text {
                    text: "第一行\n第二行".to_owned(),
                }],
                is_error: outcome != ToolCompletionStatus::Succeeded,
            };
            let update = schema::ToolCallUpdate::new(
                result.tool_call_id.clone(),
                completed_fields(outcome, &result).unwrap(),
            )
            .meta(Some(outcome_meta(outcome).unwrap()));
            let value = serde_json::to_value(update).unwrap();
            assert_eq!(value["status"], status);
            assert_eq!(value["_meta"]["keencode/toolOutcome"], precise);
            assert_eq!(value["rawOutput"], serde_json::to_value(&result).unwrap());
            assert!(
                value.get("content").is_none(),
                "文本不得在标准内容中再复制一份"
            );
        }
    }

    /// ACP 错误投影做末端防御性脱敏；成功工具输出仍保持字节语义。
    #[test]
    fn projection_redacts_failed_text_but_preserves_success_text() {
        let secret_text = concat!(
            "工具失败 request_id=req-acp-tool ",
            "Authorization: Bearer acp-tool-secret ",
            "details={\"apiKey\":\"nested-acp-tool-secret\"}"
        );
        let failed = PersistedToolResult {
            tool_call_id: "call-failed".to_owned(),
            content: vec![ToolResultPart::Text {
                text: secret_text.to_owned(),
            }],
            is_error: true,
        };
        let fields = completed_fields(ToolCompletionStatus::Failed, &failed).unwrap();
        let fields = serde_json::to_value(fields).expect("错误投影应可序列化");
        let raw = &fields["rawOutput"];
        assert_eq!(raw["toolCallId"], "call-failed");
        assert_eq!(raw["isError"], true);
        let safe = raw["content"][0]["text"]
            .as_str()
            .expect("错误文本应保留字符串形状");
        assert!(safe.contains("request_id=req-acp-tool"));
        assert!(safe.contains("Authorization: Bearer [REDACTED]"));
        assert!(!safe.contains("acp-tool-secret"));
        assert!(!safe.contains("nested-acp-tool-secret"));

        let succeeded = PersistedToolResult {
            tool_call_id: "call-succeeded".to_owned(),
            content: vec![ToolResultPart::Text {
                text: secret_text.to_owned(),
            }],
            is_error: false,
        };
        let fields = completed_fields(ToolCompletionStatus::Succeeded, &succeeded).unwrap();
        let fields = serde_json::to_value(fields).expect("成功投影应可序列化");
        let raw = &fields["rawOutput"];
        assert_eq!(raw["content"][0]["text"], secret_text);
    }
}
