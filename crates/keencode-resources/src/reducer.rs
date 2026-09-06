use thiserror::Error;

use crate::transcript::{compaction_summary_message_id, validate_compaction_source};
use crate::{
    AppliedCompaction, ArtifactUse, DynamicInputReceipt, MailboxState, SESSION_EVENT_SCHEMA,
    SESSION_EVENT_VERSION, SessionEvent, SessionEventRecord, SessionState, SessionStatus,
    SubAgentStatus, ToolLifecycle, TranscriptRecord, TranscriptSegmentReference, TurnState,
    TurnStatus, TurnStopReason,
};

/// 一个类型化事件不满足当前状态不变量。
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ReductionError {
    /// 不包含事件正文的稳定中文说明。
    pub message: String,
}

impl ReductionError {
    /// 创建一项归约失败。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 单条物理 Journal 记录允许携带的最大原子子事件数量。
pub(crate) const MAX_ATOMIC_BATCH_EVENTS: usize = 1_024;
/// 子 Agent 完成或失败摘要允许占用的最大 UTF-8 字节数。
const MAX_SUB_AGENT_RESULT_SUMMARY_BYTES: usize = 64 * 1024;
/// 子 Agent 路径名称允许占用的最大 ASCII 字节数。
const MAX_SUB_AGENT_PATH_NAME_BYTES: usize = 64;
/// 独立标题结果允许持久化的最大 UTF-8 字节数。
const MAX_GENERATED_TITLE_BYTES: usize = 512;

/// 工具生命周期在单个 Turn、Agent 与模型 Round 内共享的排序作用域。
type ToolRoundKey = (crate::TurnId, crate::AgentId, u32);

/// 在序列化、Artifact 遍历和归约前以固定成本校验原子批次外形。
pub(crate) fn validate_atomic_batch_shape(event: &SessionEvent) -> Result<(), ReductionError> {
    let SessionEvent::AtomicBatch { events } = event else {
        return Ok(());
    };
    if events.is_empty()
        || events.len() > MAX_ATOMIC_BATCH_EVENTS
        || events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::AtomicBatch { .. } | SessionEvent::SessionCreated { .. }
            )
        })
    {
        return Err(ReductionError::new(
            "原子批次不能为空、超过上限、嵌套或再次创建 Session",
        ));
    }
    Ok(())
}

/// 校验拥有所有权的原子批次，并以迭代方式释放非法递归值以避免深嵌套 Drop 栈耗尽。
pub(crate) fn validate_owned_atomic_batch_shape(
    event: SessionEvent,
) -> Result<SessionEvent, ReductionError> {
    match validate_atomic_batch_shape(&event) {
        Ok(()) => Ok(event),
        Err(error) => {
            drop_session_event_iteratively(event);
            Err(error)
        }
    }
}

/// 使用显式堆栈释放 SessionEvent，避免非法 AtomicBatch 的递归析构占用调用栈。
fn drop_session_event_iteratively(event: SessionEvent) {
    let mut pending = vec![event];
    while let Some(event) = pending.pop() {
        if let SessionEvent::AtomicBatch { events } = event {
            pending.extend(events);
        }
    }
}

/// 使用与实时追加完全相同的规则把一个事件应用到 Session 状态。
pub fn reduce_record(
    state: &mut SessionState,
    record: SessionEventRecord,
) -> Result<(), ReductionError> {
    let SessionEventRecord {
        schema,
        version,
        event_id,
        session,
        sequence,
        time_unix_ms,
        event,
    } = record;
    // 先接管并验证递归事件所有权；任何后续提前返回都只会析构已证明非递归的值。
    let event = validate_owned_atomic_batch_shape(event)?;
    let record = SessionEventRecord {
        schema,
        version,
        event_id,
        session,
        sequence,
        time_unix_ms,
        event,
    };
    // 公开入口允许恢复器直接传入外部反序列化状态，因此必须先证明当前前缀健康；
    // 这样任何拒绝都发生在本次事件第一次写状态之前。
    validate_turn_stop_consistency(state)?;
    validate_tool_transcript_consumption_order(state)?;
    reduce_record_from_valid_state(state, &record)
}

/// 对 Journal 已从健康前缀归约出的状态应用事件，避免重放时反复扫描完整工具历史。
pub(crate) fn reduce_record_from_valid_state(
    state: &mut SessionState,
    record: &SessionEventRecord,
) -> Result<(), ReductionError> {
    validate_sub_agent_turn_consistency(state)?;
    validate_standalone_sub_agent_turn_event(state, &record.event)?;
    reduce_record_inner(state, record, false)?;
    state.updated_at_unix_ms = record.time_unix_ms;
    if matches!(record.event, SessionEvent::SessionCreated { .. }) {
        state.created_at_unix_ms = record.time_unix_ms;
    }
    Ok(())
}

/// 应用一个事件；原子批次内部允许仅在批次最终状态消失的生命周期中间态。
fn reduce_record_inner(
    state: &mut SessionState,
    record: &SessionEventRecord,
    inside_atomic_batch: bool,
) -> Result<(), ReductionError> {
    validate_envelope(state, record)?;
    if state.status == SessionStatus::Closed
        && !matches!(record.event, SessionEvent::SessionClosed {})
    {
        return Err(ReductionError::new("Session 关闭后不能继续追加事件"));
    }
    if !state.created && !matches!(record.event, SessionEvent::SessionCreated { .. }) {
        return Err(ReductionError::new(
            "SessionCreated 必须是 Session 的第一个事件",
        ));
    }

    match &record.event {
        SessionEvent::SessionCreated {
            title,
            project_root,
        } => {
            if state.created || record.sequence != 1 {
                return Err(ReductionError::new(
                    "SessionCreated 只能出现一次且 sequence 为 1",
                ));
            }
            if title.trim().is_empty() || project_root.trim().is_empty() {
                return Err(ReductionError::new("Session 标题和项目根目录不能为空"));
            }
            state.created = true;
            state.title = title.clone();
            state.project_root = project_root.clone();
            state.status = SessionStatus::Idle;
        }
        SessionEvent::SessionRenamed { title } => {
            let title = title.trim();
            if title.is_empty() {
                return Err(ReductionError::new("Session 标题不能为空"));
            }
            state.title = title.to_owned();
        }
        SessionEvent::AtomicBatch { events } => {
            validate_atomic_batch_shape(&record.event)?;
            validate_atomic_model_round_pairing(events)?;
            validate_atomic_sub_agent_turn_pairing(state, events)?;
            let physical_sequence = record.sequence;
            let mut candidate = state.clone();
            for event in events {
                let sequence = candidate
                    .last_sequence
                    .checked_add(1)
                    .ok_or_else(|| ReductionError::new("原子批次内部 sequence 计算溢出"))?;
                let nested = SessionEventRecord::new(
                    record.event_id.clone(),
                    record.session.clone(),
                    sequence,
                    record.time_unix_ms,
                    event.clone(),
                );
                reduce_record_inner(&mut candidate, &nested, true)?;
            }
            validate_sub_agent_turn_consistency(&candidate)?;
            candidate.last_sequence = physical_sequence;
            *state = candidate;
            return Ok(());
        }
        SessionEvent::SessionStatusChanged { status } => {
            if *status == SessionStatus::Closed {
                return Err(ReductionError::new(
                    "关闭 Session 必须使用 SessionClosed 事件",
                ));
            }
            let derived = derived_session_status(state);
            if (derived != SessionStatus::Idle && *status != derived)
                || (derived == SessionStatus::Idle && *status == SessionStatus::Running)
            {
                return Err(ReductionError::new(
                    "显式 Session 状态不能覆盖运行中的 Turn、待终态工具或终端推导的状态",
                ));
            }
            state.status = status.clone();
        }
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            prompt_summary,
        } => {
            let root_user_turn_valid = source_agent_id.as_str() == crate::ROOT_AGENT_ID
                && root_turn_id == turn_id
                && parent_turn_id.is_none();
            let followup_turn_valid = root_turn_id != turn_id
                && state.is_registered_agent(source_agent_id)
                && state.turns.get(root_turn_id).is_some_and(|root| {
                    root.source_agent_id.as_str() == crate::ROOT_AGENT_ID
                        && root.root_turn_id == *root_turn_id
                        && root.parent_turn_id.is_none()
                })
                && parent_turn_id.as_ref().is_some_and(|parent_turn_id| {
                    state
                        .turns
                        .get(parent_turn_id)
                        .is_some_and(|parent| parent.root_turn_id == *root_turn_id)
                });
            let source_agent_already_running = state.turns.values().any(|turn| {
                turn.source_agent_id == *source_agent_id && turn.status == TurnStatus::Running
            });
            if prompt_summary.trim().is_empty()
                || state.turns.contains_key(turn_id)
                || source_agent_already_running
                || (!root_user_turn_valid && !followup_turn_valid)
            {
                return Err(ReductionError::new(
                    "Turn 标识、输入摘要、Agent 身份或父子谱系无效",
                ));
            }
            state.turns.insert(
                turn_id.clone(),
                TurnState {
                    turn_id: turn_id.clone(),
                    source_agent_id: source_agent_id.clone(),
                    root_turn_id: root_turn_id.clone(),
                    parent_turn_id: parent_turn_id.clone(),
                    prompt_summary: prompt_summary.clone(),
                    started_at_unix_ms: record.time_unix_ms,
                    completed_at_unix_ms: None,
                    status: TurnStatus::Running,
                    stop_reason: None,
                    outcome_message: None,
                },
            );
            refresh_derived_status(state);
        }
        SessionEvent::TurnCompleted { turn_id } => {
            ensure_turn_resources_finished(state, turn_id)?;
            let turn = running_turn(state, turn_id)?;
            turn.status = TurnStatus::Completed;
            turn.completed_at_unix_ms = Some(record.time_unix_ms);
            turn.stop_reason = None;
            turn.outcome_message = None;
            refresh_derived_status(state);
        }
        SessionEvent::TurnStopped {
            turn_id,
            reason,
            message,
        } => {
            if message.trim().is_empty() {
                return Err(ReductionError::new("TurnStopped 结果说明不能为空"));
            }
            ensure_turn_resources_finished(state, turn_id)?;
            let turn = running_turn(state, turn_id)?;
            turn.status = reason.status();
            turn.completed_at_unix_ms = Some(record.time_unix_ms);
            turn.stop_reason = Some(*reason);
            turn.outcome_message = Some(message.clone());
            refresh_derived_status(state);
        }
        SessionEvent::MessageAdded { message } => {
            if !valid_standalone_message_shape(message)
                || !valid_message_agent_identity(state, message)
            {
                return Err(ReductionError::new(
                    "消息标识、角色或类型化内容不满足约束；工具交换必须使用原子 Transcript 段",
                ));
            }
            if state.contains_transcript_message_id(&message.message_id) {
                return Err(ReductionError::new("消息标识重复"));
            }
            if let Some(turn_id) = &message.turn_id {
                match state.turns.get(turn_id) {
                    Some(turn)
                        if turn.status == TurnStatus::Running
                            && message_agent_matches_source(message, &turn.source_agent_id) => {}
                    Some(_) => {
                        return Err(ReductionError::new(
                            "消息不能追加到已终态 Turn 或其他 Agent 的 Turn",
                        ));
                    }
                    None => return Err(ReductionError::new("消息引用了不存在的 Turn")),
                }
            }
            state.transcript_revision = state
                .transcript_revision
                .checked_add(1)
                .ok_or_else(|| ReductionError::new("Transcript revision 溢出"))?;
            state
                .transcript
                .push(TranscriptRecord::MessageAdded(message.clone()));
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            if segment.segment_index == 0
                && !inside_atomic_batch
                && !is_dynamic_input_segment(segment)
            {
                return Err(ReductionError::new(
                    "首个模型输出 Transcript 段必须与模型 Round 完成事件原子提交",
                ));
            }
            let consumed_request_ids = validate_transcript_segment(state, segment)?;
            let applied_revision = state
                .transcript_revision
                .checked_add(1)
                .ok_or_else(|| ReductionError::new("Transcript revision 溢出"))?;
            state.transcript_revision = applied_revision;
            state
                .transcript
                .push(TranscriptRecord::SegmentCommitted(segment.clone()));
            let reference = TranscriptSegmentReference {
                turn_id: segment.turn_id.clone(),
                source_agent_id: segment.source_agent_id.clone(),
                model_round: segment.model_round,
                segment_index: segment.segment_index,
                transcript_revision: applied_revision,
            };
            for request_id in consumed_request_ids {
                state
                    .tools
                    .get_mut(&request_id)
                    .expect("工具生命周期已在 Transcript 段预校验中确认存在")
                    .transcript_segment = Some(reference.clone());
            }
        }
        SessionEvent::DynamicInputReceiptCommitted {
            turn_id,
            source_agent_id,
            model_round,
            segment_index,
            kind,
            through_sequence,
        } => {
            if !inside_atomic_batch
                || *model_round == 0
                || *through_sequence == 0
                || !state.turns.get(turn_id).is_some_and(|turn| {
                    turn.status == TurnStatus::Running && turn.source_agent_id == *source_agent_id
                })
            {
                return Err(ReductionError::new(
                    "动态输入回执必须在运行中的同 Agent Turn 原子提交",
                ));
            }
            let segment_matches = matches!(
                state.transcript.last(),
                Some(TranscriptRecord::SegmentCommitted(segment))
                    if segment.turn_id == *turn_id
                        && segment.source_agent_id == *source_agent_id
                        && segment.model_round == *model_round
                        && segment.segment_index == *segment_index
                        && is_dynamic_input_segment(segment)
            );
            if !segment_matches
                || state.dynamic_input_receipts.iter().any(|receipt| {
                    receipt.turn_id == *turn_id
                        && receipt.source_agent_id == *source_agent_id
                        && receipt.model_round == *model_round
                        && receipt.segment_index == *segment_index
                        && receipt.kind == *kind
                        && receipt.through_sequence == *through_sequence
                })
            {
                return Err(ReductionError::new(
                    "动态输入回执没有匹配紧邻的动态 Transcript 段或已经重复",
                ));
            }
            state.dynamic_input_receipts.push(DynamicInputReceipt {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round: *model_round,
                segment_index: *segment_index,
                kind: *kind,
                through_sequence: *through_sequence,
                transcript_revision: state.transcript_revision,
            });
        }
        SessionEvent::ModelRoundCompleted {
            turn_id,
            source_agent_id,
            model_round,
            requested_model,
            metadata,
            usage,
            stop_reason,
        } => {
            if !inside_atomic_batch {
                return Err(ReductionError::new(
                    "模型 Round 必须与对应 Transcript 段原子提交",
                ));
            }
            ensure_turn_running(state, turn_id)?;
            let expected_round = state
                .model_rounds
                .iter()
                .filter(|existing| existing.turn_id == *turn_id)
                .map(|existing| existing.model_round)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| ReductionError::new("模型 Round 序号溢出"))?;
            let turn_matches = state
                .turns
                .get(turn_id)
                .is_some_and(|turn| turn.source_agent_id == *source_agent_id);
            if *model_round != expected_round
                || requested_model.trim().is_empty()
                || !turn_matches
                || metadata.validate().is_err()
                || matches!(
                    stop_reason,
                    keencode_model::StopReason::Other { reason } if reason.trim().is_empty()
                )
            {
                return Err(ReductionError::new(
                    "模型 Round 身份、序号、模型或响应元数据无效",
                ));
            }
            state.model_rounds.push(crate::ModelRoundState {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round: *model_round,
                requested_model: requested_model.clone(),
                metadata: metadata.clone(),
                usage: usage.clone(),
                stop_reason: stop_reason.clone(),
                completed_at_unix_ms: record.time_unix_ms,
            });
        }
        SessionEvent::ToolRequested { request } => {
            let turn_is_running = state.turns.get(&request.turn_id).is_some_and(|turn| {
                turn.status == TurnStatus::Running && turn.source_agent_id == request.agent_id
            });
            let model_call_is_unique = state.tools.values().all(|tool| {
                tool.request.turn_id != request.turn_id
                    || tool.request.agent_id != request.agent_id
                    || tool.request.model_round != request.model_round
                    || tool.request.model_tool_call_id != request.model_tool_call_id
            });
            let request_index_is_unique = state.tools.values().all(|tool| {
                tool.request.turn_id != request.turn_id
                    || tool.request.agent_id != request.agent_id
                    || tool.request.model_round != request.model_round
                    || tool.request.request_index != request.request_index
            });
            let request_index_is_after_consumed_watermark = state
                .tools
                .values()
                .filter(|tool| {
                    tool_matches_round(
                        tool,
                        &request.turn_id,
                        &request.agent_id,
                        request.model_round,
                    ) && tool.transcript_segment.is_some()
                })
                .map(|tool| tool.request.request_index)
                .max()
                .is_none_or(|consumed| request.request_index > consumed);
            let transcript_call_id_is_unique = state
                .transcript_segments()
                .filter(|segment| {
                    segment.turn_id == request.turn_id
                        && segment.source_agent_id == request.agent_id
                        && segment.model_round == request.model_round
                })
                .flat_map(|segment| segment.messages.iter())
                .flat_map(|message| message.content.iter())
                .all(|part| {
                    !matches!(
                        part,
                        crate::MessagePart::ToolCall { tool_call_id, .. }
                            if tool_call_id == &request.model_tool_call_id
                    )
                });
            let derived_request_id = crate::RequestId::derive_model_tool_call(
                &state.session_id,
                &request.turn_id,
                &request.agent_id,
                request.model_round,
                &request.model_tool_call_id,
            );
            if request.tool_name.trim().is_empty()
                || !request.arguments.is_object()
                || !state.is_registered_agent(&request.agent_id)
                || !turn_is_running
                || request.model_round == 0
                || request.model_tool_call_id.trim().is_empty()
                || request.model_tool_call_id.len() > 1_024
                || state.plan.enabled && request.effect == crate::ToolEffect::ChangesState
                || !model_call_is_unique
                || !request_index_is_unique
                || !request_index_is_after_consumed_watermark
                || !transcript_call_id_is_unique
                || state.tools.contains_key(&request.request_id)
                || !derived_request_id.is_ok_and(|derived| derived == request.request_id)
            {
                return Err(ReductionError::new(
                    "工具请求名称、参数、运行 Turn 或标识不满足约束",
                ));
            }
            state.tools.insert(
                request.request_id.clone(),
                ToolLifecycle {
                    request: request.clone(),
                    requested_at_unix_ms: record.time_unix_ms,
                    execution_started: false,
                    execution_started_at_unix_ms: None,
                    outcome: None,
                    completed_at_unix_ms: None,
                    file_change: None,
                    transcript_segment: None,
                },
            );
            refresh_derived_status(state);
        }
        SessionEvent::ToolExecutionStarted { request_id } => {
            let turn_id = state
                .tools
                .get(request_id)
                .ok_or_else(|| ReductionError::new("执行起点引用了不存在的工具请求"))?
                .request
                .turn_id
                .clone();
            ensure_turn_running(state, &turn_id)?;
            let tool = state
                .tools
                .get_mut(request_id)
                .expect("工具请求已经在不可变检查中确认存在");
            if tool.execution_started || tool.outcome.is_some() {
                return Err(ReductionError::new("工具已经开始或已经结束"));
            }
            tool.execution_started = true;
            tool.execution_started_at_unix_ms = Some(record.time_unix_ms);
            refresh_derived_status(state);
        }
        SessionEvent::ToolFileChangePrepared { request_id, change } => {
            let tool = state
                .tools
                .get(request_id)
                .ok_or_else(|| ReductionError::new("文件变更准备引用了不存在的工具请求"))?;
            let turn_id = tool.request.turn_id.clone();
            ensure_turn_running(state, &turn_id)?;
            if tool.request.effect != crate::ToolEffect::ChangesState
                || !tool.execution_started
                || tool.outcome.is_some()
                || tool.file_change.is_some()
                || !valid_tool_file_change_shape(change)
            {
                return Err(ReductionError::new(
                    "文件变更准备必须属于已开始且未结束的副作用工具，且快照形状有效",
                ));
            }
            state
                .tools
                .get_mut(request_id)
                .expect("工具请求已经在不可变检查中确认存在")
                .file_change = Some(change.clone());
        }
        SessionEvent::ToolFileChangeApplied { request_id } => {
            let tool = state
                .tools
                .get(request_id)
                .ok_or_else(|| ReductionError::new("文件变更应用引用了不存在的工具请求"))?;
            let turn_id = tool.request.turn_id.clone();
            ensure_turn_running(state, &turn_id)?;
            if tool.request.effect != crate::ToolEffect::ChangesState
                || !tool.execution_started
                || tool.outcome.is_some()
            {
                return Err(ReductionError::new(
                    "文件变更应用必须属于已开始且未结束的工具",
                ));
            }
            if !tool
                .file_change
                .as_ref()
                .is_some_and(valid_tool_file_change_shape)
            {
                return Err(ReductionError::new("文件变更必须先准备且只能应用一次"));
            }
            state
                .tools
                .get_mut(request_id)
                .expect("工具请求已经在不可变检查中确认存在")
                .file_change
                .as_mut()
                .expect("文件变更已经在不可变检查中确认存在")
                .applied = true;
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => {
            let turn_id = state
                .tools
                .get(request_id)
                .ok_or_else(|| ReductionError::new("结果引用了不存在的工具请求"))?
                .request
                .turn_id
                .clone();
            ensure_turn_running(state, &turn_id)?;
            if state
                .terminals
                .values()
                .any(|terminal| terminal.request_id == *request_id && !terminal.exited)
            {
                return Err(ReductionError::new("工具关联终端仍在运行"));
            }
            let tool = state
                .tools
                .get_mut(request_id)
                .expect("工具请求已经在不可变检查中确认存在");
            if tool.outcome.is_some() {
                return Err(ReductionError::new("工具请求已经结束"));
            }
            let valid_outcome = valid_tool_outcome(outcome)
                && outcome.status != crate::ToolCompletionStatus::SideEffectUnknown
                && outcome.result.tool_call_id == tool.request.model_tool_call_id;
            let execution_allows_outcome = match outcome.status {
                crate::ToolCompletionStatus::Succeeded
                | crate::ToolCompletionStatus::Failed
                | crate::ToolCompletionStatus::SideEffectUnknown => tool.execution_started,
                crate::ToolCompletionStatus::Cancelled => true,
            };
            if !valid_outcome || !execution_allows_outcome {
                return Err(ReductionError::new("工具结果字段或执行状态无效"));
            }
            tool.outcome = Some(outcome.clone());
            tool.completed_at_unix_ms = Some(record.time_unix_ms);
            refresh_derived_status(state);
        }
        SessionEvent::ToolSideEffectUnknown { request_id, result } => {
            let turn_id = state
                .tools
                .get(request_id)
                .ok_or_else(|| ReductionError::new("恢复结果引用了不存在的工具请求"))?
                .request
                .turn_id
                .clone();
            ensure_turn_running(state, &turn_id)?;
            if state
                .terminals
                .values()
                .any(|terminal| terminal.request_id == *request_id && !terminal.exited)
            {
                return Err(ReductionError::new("副作用未知工具关联的终端仍在运行"));
            }
            let tool = state
                .tools
                .get_mut(request_id)
                .expect("工具请求已经在不可变检查中确认存在");
            let canonical_result =
                crate::side_effect_unknown_result(&tool.request.model_tool_call_id);
            let outcome = crate::ToolOutcome {
                status: crate::ToolCompletionStatus::SideEffectUnknown,
                result: result.clone(),
            };
            if tool.outcome.is_some()
                || !tool.execution_started
                || tool.request.effect != crate::ToolEffect::ChangesState
                || result != &canonical_result
            {
                return Err(ReductionError::new(
                    "只有已开始的副作用工具可在恢复时标记为状态未知",
                ));
            }
            tool.outcome = Some(outcome);
            tool.completed_at_unix_ms = Some(record.time_unix_ms);
            refresh_derived_status(state);
        }
        SessionEvent::TerminalStarted { terminal } => {
            let tool = state.tools.get(&terminal.request_id);
            let tool_can_execute = tool.is_some_and(|tool| {
                tool.outcome.is_none()
                    && tool.execution_started
                    && state
                        .turns
                        .get(&tool.request.turn_id)
                        .is_some_and(|turn| turn.status == TurnStatus::Running)
            });
            if terminal.command_display.trim().is_empty()
                || terminal.working_directory.trim().is_empty()
                || !terminal.output_artifacts.is_empty()
                || terminal.exit_code.is_some()
                || terminal.cancelled
                || terminal.exited
                || !tool_can_execute
                || state.terminals.contains_key(&terminal.terminal_id)
            {
                return Err(ReductionError::new(
                    "终端初始记录或关联工具执行状态不满足约束",
                ));
            }
            state
                .terminals
                .insert(terminal.terminal_id.clone(), terminal.clone());
        }
        SessionEvent::TerminalOutputRecorded {
            terminal_id,
            artifact,
        } => {
            if !valid_artifact_use(artifact) {
                return Err(ReductionError::new("终端输出 Artifact 引用无效"));
            }
            let terminal = state
                .terminals
                .get(terminal_id)
                .ok_or_else(|| ReductionError::new("输出引用了不存在的终端"))?;
            if terminal.exited {
                return Err(ReductionError::new("终端退出后不能追加输出"));
            }
            let request_id = terminal.request_id.clone();
            ensure_tool_turn_running(state, &request_id)?;
            let terminal = state
                .terminals
                .get_mut(terminal_id)
                .expect("终端已经在不可变检查中确认存在");
            terminal.output_artifacts.push(artifact.clone());
        }
        SessionEvent::TerminalExited {
            terminal_id,
            exit_code,
            cancelled,
        } => {
            let terminal = state
                .terminals
                .get(terminal_id)
                .ok_or_else(|| ReductionError::new("退出事件引用了不存在的终端"))?;
            if terminal.exited {
                return Err(ReductionError::new("终端已经退出"));
            }
            let request_id = terminal.request_id.clone();
            ensure_tool_turn_running(state, &request_id)?;
            let terminal = state
                .terminals
                .get_mut(terminal_id)
                .expect("终端已经在不可变检查中确认存在");
            terminal.exit_code = *exit_code;
            terminal.cancelled = *cancelled;
            terminal.exited = true;
        }
        SessionEvent::CompactionApplied {
            turn_id,
            source_agent_id,
            model_round,
            compaction,
        } => {
            ensure_turn_running(state, turn_id)?;
            if !state.is_registered_agent(source_agent_id)
                || state
                    .turns
                    .get(turn_id)
                    .is_none_or(|turn| turn.source_agent_id != *source_agent_id)
            {
                return Err(ReductionError::new(
                    "上下文压缩引用了未注册或不属于 Turn 的 Agent",
                ));
            }
            let effective = state
                .effective_transcript(source_agent_id)
                .map_err(|error| ReductionError::new(error.to_string()))?;
            let removed = compaction
                .replaced_end_index_exclusive
                .checked_sub(compaction.replaced_start_index);
            let expected_retained = effective
                .len()
                .checked_sub(compaction.replaced_message_count)
                .and_then(|count| count.checked_add(1));
            let actual_digest = state
                .compaction_source_digest_sha256(
                    turn_id,
                    source_agent_id,
                    *model_round,
                    compaction.replaced_start_index,
                    compaction.replaced_end_index_exclusive,
                )
                .map_err(|error| ReductionError::new(error.to_string()))?;
            let source_range =
                compaction.replaced_start_index..compaction.replaced_end_index_exclusive;
            validate_compaction_source(&effective, source_range)
                .map_err(|error| ReductionError::new(error.to_string()))?;
            if *model_round == 0
                || compaction.expected_transcript_revision != state.transcript_revision
                || compaction.expected_transcript_revision.checked_add(1)
                    != Some(compaction.applied_transcript_revision)
                || compaction.replaced_start_index >= compaction.replaced_end_index_exclusive
                || compaction.replaced_end_index_exclusive > effective.len()
                || removed != Some(compaction.replaced_message_count)
                || expected_retained != Some(compaction.retained_message_count)
                || !valid_sha256(&compaction.source_digest_sha256)
                || compaction.source_digest_sha256 != actual_digest
                || compaction.summary.trim().is_empty()
            {
                return Err(ReductionError::new("上下文压缩范围或摘要无效"));
            }
            let applied = AppliedCompaction {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round: *model_round,
                record: compaction.clone(),
            };
            if state.contains_transcript_message_id(&compaction_summary_message_id(&applied)) {
                return Err(ReductionError::new("上下文压缩摘要消息标识冲突"));
            }
            state
                .transcript
                .push(TranscriptRecord::CompactionApplied(applied));
            state.transcript_revision = compaction.applied_transcript_revision;
        }
        SessionEvent::TodoReplaced {
            items,
            operation_payload_sha256,
            revision,
        } => {
            if !valid_sha256(operation_payload_sha256)
                || items.len() > 100
                || has_duplicate(items.iter().map(|item| item.content.as_str()))
                || items
                    .iter()
                    .filter(|item| item.status == crate::TodoStatus::InProgress)
                    .count()
                    > 1
                || items.iter().any(|item| {
                    item.content.trim().is_empty()
                        || item.content.trim() != item.content
                        || item.content.chars().count() > 500
                        || item.active_form.trim().is_empty()
                        || item.active_form.trim() != item.active_form
                        || item.active_form.chars().count() > 500
                })
                || !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item.status == crate::TodoStatus::Completed)
            {
                return Err(ReductionError::new(
                    "Todo 列表数量、字段、状态或完成收起语义无效",
                ));
            }
            let expected_revision = if state.todos.items == *items {
                state.todos.revision
            } else {
                state
                    .todos
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| ReductionError::new("Todo revision 溢出"))?
            };
            if *revision != expected_revision {
                return Err(ReductionError::new("Todo revision 与当前权威状态不一致"));
            }
            if state.todos.items != *items {
                state.todos.revision = *revision;
                state.todos.items = items.clone();
            }
        }
        SessionEvent::PlanChanged { plan } => {
            if plan
                .plan_artifact
                .as_ref()
                .is_some_and(|artifact| !valid_artifact_use(artifact))
            {
                return Err(ReductionError::new("Plan Artifact 引用无效"));
            }
            if !state.plan.enabled
                && plan.enabled
                && (state
                    .turns
                    .values()
                    .any(|turn| turn.status == TurnStatus::Running)
                    || state.tools.values().any(|tool| tool.outcome.is_none())
                    || state.terminals.values().any(|terminal| !terminal.exited))
            {
                return Err(ReductionError::new(
                    "只能在没有活动 Turn、工具和终端时开启 Plan",
                ));
            }
            state.plan = plan.clone();
        }
        SessionEvent::ProviderSnapshotUpdated { provider } => {
            if provider.provider_id.trim().is_empty()
                || provider.model.trim().is_empty()
                || provider.config_fingerprint.trim().is_empty()
            {
                return Err(ReductionError::new("Provider Snapshot 字段不能为空"));
            }
            state.provider = Some(provider.clone());
        }
        SessionEvent::TitleGenerated { result } => {
            if !valid_control_operation_id(&result.operation_id)
                || !valid_sha256(&result.input_sha256)
                || result.title.trim().is_empty()
                || result.title.trim() != result.title
                || result.title.len() > MAX_GENERATED_TITLE_BYTES
            {
                return Err(ReductionError::new("标题生成结果字段无效"));
            }
            if let Some(existing) = state.generated_titles.get(&result.operation_id) {
                if existing != result {
                    return Err(ReductionError::new("标题生成 operationId 正文冲突"));
                }
            } else {
                state
                    .generated_titles
                    .insert(result.operation_id.clone(), result.clone());
            }
        }
        SessionEvent::SubAgentSpawned { agent } => {
            if agent.task.trim().is_empty()
                || agent.agent_id == agent.parent_agent_id
                || agent.parent_agent_id.as_str() != crate::ROOT_AGENT_ID
                || !valid_sub_agent_path(&agent.agent_path)
                || state.sub_agents.contains_key(&agent.agent_id)
                || state.sub_agents.contains_key(&agent.parent_agent_id)
                || state
                    .sub_agents
                    .values()
                    .any(|existing| existing.agent_path == agent.agent_path)
                || agent.status != SubAgentStatus::Pending
                || agent.current_turn_id.is_some()
                || agent.result_summary.is_some()
            {
                return Err(ReductionError::new(
                    "子 Agent 标识、初态、根父 Agent 或单层关系无效",
                ));
            }
            state
                .sub_agents
                .insert(agent.agent_id.clone(), agent.clone());
        }
        SessionEvent::SubAgentStatusChanged {
            agent_id,
            turn_id,
            status,
            result_summary,
        } => {
            let agent = state
                .sub_agents
                .get(agent_id)
                .ok_or_else(|| ReductionError::new("状态引用了不存在的子 Agent"))?;
            if !valid_sub_agent_transition(
                state,
                agent,
                turn_id.as_ref(),
                status,
                result_summary.as_deref(),
            ) {
                return Err(ReductionError::new("子 Agent 状态迁移或结果摘要不满足约束"));
            }
            let agent = state
                .sub_agents
                .get_mut(agent_id)
                .expect("子 Agent 已在不可变校验中确认存在");
            agent.status = status.clone();
            agent.current_turn_id = turn_id.clone();
            agent.result_summary = result_summary.clone();
        }
        SessionEvent::MailboxMessageQueued { message } => {
            if message.body.trim().is_empty() && message.artifact.is_none() {
                return Err(ReductionError::new("邮箱消息正文和 Artifact 不能同时为空"));
            }
            let route_is_valid = state
                .sub_agents
                .get(&message.from)
                .is_some_and(|agent| agent.parent_agent_id == message.to)
                || state
                    .sub_agents
                    .get(&message.to)
                    .is_some_and(|agent| agent.parent_agent_id == message.from);
            let source_turn_is_valid = state
                .turns
                .get(&message.related_turn_id)
                .is_some_and(|turn| turn.source_agent_id == message.from);
            if message.state != MailboxState::Queued
                || state.mailbox.contains_key(&message.message_id)
                || !route_is_valid
                || !source_turn_is_valid
                || message
                    .artifact
                    .as_ref()
                    .is_some_and(|artifact| !valid_artifact_use(artifact))
            {
                return Err(ReductionError::new(
                    "邮箱消息标识、路由、Artifact 或初始状态无效",
                ));
            }
            state
                .mailbox
                .insert(message.message_id.clone(), message.clone());
        }
        SessionEvent::MailboxMessageDelivered { message_id } => {
            let message = state
                .mailbox
                .get_mut(message_id)
                .ok_or_else(|| ReductionError::new("投递引用了不存在的邮箱消息"))?;
            if message.state == MailboxState::Delivered {
                return Err(ReductionError::new("邮箱消息已经投递"));
            }
            message.state = MailboxState::Delivered;
        }
        SessionEvent::WorktreeAssigned { worktree } => {
            let path_identity = normalized_worktree_path(&worktree.path);
            let agent_is_active = state
                .sub_agents
                .get(&worktree.agent_id)
                .is_some_and(|agent| {
                    matches!(
                        agent.status,
                        SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting
                    )
                });
            if worktree.path.trim().is_empty()
                || worktree.branch.trim().is_empty()
                || worktree.released
                || path_identity.is_none()
                || !agent_is_active
                || state
                    .worktrees
                    .get(&worktree.agent_id)
                    .is_some_and(|current| !current.released)
                || state.worktrees.values().any(|current| {
                    !current.released && normalized_worktree_path(&current.path) == path_identity
                })
            {
                return Err(ReductionError::new("工作树绑定无效或路径已被占用"));
            }
            state
                .worktrees
                .insert(worktree.agent_id.clone(), worktree.clone());
        }
        SessionEvent::WorktreeReleased { agent_id } => {
            let worktree = state
                .worktrees
                .get_mut(agent_id)
                .ok_or_else(|| ReductionError::new("释放了不存在的工作树绑定"))?;
            if worktree.released {
                return Err(ReductionError::new("工作树已经释放"));
            }
            worktree.released = true;
        }
        SessionEvent::SessionClosed {} => {
            if state.status == SessionStatus::Closed {
                return Err(ReductionError::new("Session 已经关闭"));
            }
            if state
                .turns
                .values()
                .any(|turn| turn.status == TurnStatus::Running)
                || state.tools.values().any(|tool| tool.outcome.is_none())
                || state.terminals.values().any(|terminal| !terminal.exited)
                || state.sub_agents.values().any(|agent| {
                    matches!(
                        agent.status,
                        SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting
                    )
                })
                || state.worktrees.values().any(|worktree| !worktree.released)
            {
                return Err(ReductionError::new(
                    "Session 仍有运行 Turn、未完成工具、终端、活跃子 Agent 或工作树",
                ));
            }
            state.status = SessionStatus::Closed;
        }
    }
    // 顶层子 Agent Turn 事件已在第一次写状态前拒绝；其余单事件的分支校验都在
    // 修改前完成。批次则只修改独立 candidate，并在替换原状态前完成一致性校验。
    debug_assert!(inside_atomic_batch || validate_sub_agent_turn_consistency(state).is_ok());
    state.last_sequence = record.sequence;
    Ok(())
}

/// 校验单层子 Agent 路径采用 `/root/<name>`，名称仅含 1 至 64 个小写 ASCII 字母、数字或下划线。
fn valid_sub_agent_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("/root/") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= MAX_SUB_AGENT_PATH_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// 在修改状态前拒绝必须与子 Agent 状态事件原子配对的独立 Turn 生命周期事件。
fn validate_standalone_sub_agent_turn_event(
    state: &SessionState,
    event: &SessionEvent,
) -> Result<(), ReductionError> {
    let child_turn_event = match event {
        SessionEvent::TurnStarted {
            source_agent_id, ..
        } => source_agent_id.as_str() != crate::ROOT_AGENT_ID,
        SessionEvent::TurnCompleted { turn_id } | SessionEvent::TurnStopped { turn_id, .. } => {
            state
                .turns
                .get(turn_id)
                .is_some_and(|turn| turn.source_agent_id.as_str() != crate::ROOT_AGENT_ID)
        }
        SessionEvent::SessionCreated { .. }
        | SessionEvent::SessionRenamed { .. }
        | SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::AtomicBatch { .. }
        | SessionEvent::MessageAdded { .. }
        | SessionEvent::TranscriptSegmentCommitted { .. }
        | SessionEvent::DynamicInputReceiptCommitted { .. }
        | SessionEvent::ModelRoundCompleted { .. }
        | SessionEvent::ToolRequested { .. }
        | SessionEvent::ToolExecutionStarted { .. }
        | SessionEvent::ToolFileChangePrepared { .. }
        | SessionEvent::ToolFileChangeApplied { .. }
        | SessionEvent::ToolCompleted { .. }
        | SessionEvent::ToolSideEffectUnknown { .. }
        | SessionEvent::TerminalStarted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::CompactionApplied { .. }
        | SessionEvent::TodoReplaced { .. }
        | SessionEvent::PlanChanged { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::TitleGenerated { .. }
        | SessionEvent::SubAgentSpawned { .. }
        | SessionEvent::SubAgentStatusChanged { .. }
        | SessionEvent::MailboxMessageQueued { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => false,
    };
    if child_turn_event {
        Err(ReductionError::new(
            "子 Agent Turn 生命周期必须与对应 Agent 状态事件原子提交",
        ))
    } else {
        Ok(())
    }
}

/// 校验原子批次没有把子 Agent Turn 从 Pending 直接穿越到终态而跳过状态事件。
fn validate_atomic_sub_agent_turn_pairing(
    state: &SessionState,
    events: &[SessionEvent],
) -> Result<(), ReductionError> {
    for event in events {
        match event {
            SessionEvent::TurnStarted {
                turn_id,
                source_agent_id,
                ..
            } if source_agent_id.as_str() != crate::ROOT_AGENT_ID => {
                let paired = events.iter().any(|event| {
                    matches!(
                        event,
                        SessionEvent::SubAgentStatusChanged {
                            agent_id,
                            turn_id: Some(status_turn_id),
                            status: SubAgentStatus::Running,
                            ..
                        } if agent_id == source_agent_id && status_turn_id == turn_id
                    )
                });
                if !paired {
                    return Err(ReductionError::new(
                        "子 Agent Turn 起点必须与 Running 状态原子配对",
                    ));
                }
            }
            SessionEvent::TurnCompleted { turn_id } => {
                validate_atomic_sub_agent_terminal_pairing(
                    state,
                    events,
                    turn_id,
                    &[SubAgentStatus::Completed, SubAgentStatus::Stopped],
                )?;
            }
            SessionEvent::TurnStopped {
                turn_id, reason, ..
            } => {
                let expected = match reason {
                    TurnStopReason::Cancelled => {
                        &[SubAgentStatus::Interrupted, SubAgentStatus::Stopped][..]
                    }
                    TurnStopReason::Failed
                    | TurnStopReason::LimitReached
                    | TurnStopReason::ContextBlocked
                    | TurnStopReason::ModelOutputLimit
                    | TurnStopReason::ModelRefusal => {
                        &[SubAgentStatus::Failed, SubAgentStatus::Stopped][..]
                    }
                };
                validate_atomic_sub_agent_terminal_pairing(state, events, turn_id, expected)?;
            }
            SessionEvent::SessionCreated { .. }
            | SessionEvent::SessionRenamed { .. }
            | SessionEvent::SessionStatusChanged { .. }
            | SessionEvent::AtomicBatch { .. }
            | SessionEvent::MessageAdded { .. }
            | SessionEvent::TranscriptSegmentCommitted { .. }
            | SessionEvent::DynamicInputReceiptCommitted { .. }
            | SessionEvent::ModelRoundCompleted { .. }
            | SessionEvent::ToolRequested { .. }
            | SessionEvent::ToolExecutionStarted { .. }
            | SessionEvent::ToolFileChangePrepared { .. }
            | SessionEvent::ToolFileChangeApplied { .. }
            | SessionEvent::ToolCompleted { .. }
            | SessionEvent::ToolSideEffectUnknown { .. }
            | SessionEvent::TerminalStarted { .. }
            | SessionEvent::TerminalOutputRecorded { .. }
            | SessionEvent::TerminalExited { .. }
            | SessionEvent::CompactionApplied { .. }
            | SessionEvent::TodoReplaced { .. }
            | SessionEvent::PlanChanged { .. }
            | SessionEvent::ProviderSnapshotUpdated { .. }
            | SessionEvent::TitleGenerated { .. }
            | SessionEvent::SubAgentSpawned { .. }
            | SessionEvent::SubAgentStatusChanged { .. }
            | SessionEvent::MailboxMessageQueued { .. }
            | SessionEvent::MailboxMessageDelivered { .. }
            | SessionEvent::WorktreeAssigned { .. }
            | SessionEvent::WorktreeReleased { .. }
            | SessionEvent::SessionClosed {} => {}
            SessionEvent::TurnStarted { .. } => {}
        }
    }
    Ok(())
}

/// 若目标属于子 Agent，则要求批次包含与 Turn 终态对应的同 Agent 状态事件。
fn validate_atomic_sub_agent_terminal_pairing(
    state: &SessionState,
    events: &[SessionEvent],
    turn_id: &crate::TurnId,
    expected_statuses: &[SubAgentStatus],
) -> Result<(), ReductionError> {
    let source_agent_id = state
        .turns
        .get(turn_id)
        .map(|turn| turn.source_agent_id.clone())
        .or_else(|| {
            events.iter().find_map(|event| match event {
                SessionEvent::TurnStarted {
                    turn_id: started_turn_id,
                    source_agent_id,
                    ..
                } if started_turn_id == turn_id => Some(source_agent_id.clone()),
                _ => None,
            })
        });
    let Some(source_agent_id) = source_agent_id else {
        return Ok(());
    };
    if source_agent_id.as_str() == crate::ROOT_AGENT_ID {
        return Ok(());
    }
    let paired = events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::SubAgentStatusChanged {
                agent_id,
                turn_id: Some(status_turn_id),
                status,
                ..
            } if agent_id == &source_agent_id
                && status_turn_id == turn_id
                && expected_statuses.contains(status)
        )
    });
    if !paired {
        return Err(ReductionError::new(
            "子 Agent Turn 终态必须与对应 Agent 状态原子配对",
        ));
    }
    Ok(())
}

/// 校验 envelope 与状态中的 Session 和 sequence 完全连续。
fn validate_envelope(
    state: &SessionState,
    record: &SessionEventRecord,
) -> Result<(), ReductionError> {
    if record.schema != SESSION_EVENT_SCHEMA || record.version != SESSION_EVENT_VERSION {
        return Err(ReductionError::new("事件 schema 或 version 不受支持"));
    }
    if record.session != state.session_id {
        return Err(ReductionError::new("事件 Session 标识不匹配"));
    }
    if record.time_unix_ms < state.updated_at_unix_ms {
        return Err(ReductionError::new("事件时间不能早于已提交状态"));
    }
    let expected = state.last_sequence.saturating_add(1);
    if record.sequence != expected {
        return Err(ReductionError::new(format!(
            "事件 sequence 不连续：期望 {expected}，实际 {}",
            record.sequence
        )));
    }
    Ok(())
}

/// 要求唯一模型完成事件与同批响应 Transcript 段双向且紧邻配对。
fn validate_atomic_model_round_pairing(events: &[SessionEvent]) -> Result<(), ReductionError> {
    let mut completion = None;
    let mut first_segment = None;
    let mut first_zero_segment = None;
    for (index, event) in events.iter().enumerate() {
        match event {
            SessionEvent::ModelRoundCompleted {
                turn_id,
                source_agent_id,
                model_round,
                ..
            } => {
                if completion
                    .replace((index, turn_id, source_agent_id, *model_round))
                    .is_some()
                {
                    return Err(ReductionError::new(
                        "同一原子批次只能包含一个模型 Round 完成事件",
                    ));
                }
            }
            SessionEvent::TranscriptSegmentCommitted { segment } => {
                first_segment.get_or_insert((index, segment));
                if segment.segment_index == 0
                    && first_zero_segment.replace((index, segment)).is_some()
                {
                    return Err(ReductionError::new(
                        "同一原子批次不能包含多个首个 Transcript 段",
                    ));
                }
            }
            SessionEvent::DynamicInputReceiptCommitted { .. } => {}
            SessionEvent::SessionCreated { .. }
            | SessionEvent::SessionRenamed { .. }
            | SessionEvent::SessionStatusChanged { .. }
            | SessionEvent::SessionClosed {}
            | SessionEvent::TurnStarted { .. }
            | SessionEvent::AtomicBatch { .. }
            | SessionEvent::TurnCompleted { .. }
            | SessionEvent::TurnStopped { .. }
            | SessionEvent::MessageAdded { .. }
            | SessionEvent::ToolRequested { .. }
            | SessionEvent::ToolExecutionStarted { .. }
            | SessionEvent::ToolFileChangePrepared { .. }
            | SessionEvent::ToolFileChangeApplied { .. }
            | SessionEvent::ToolCompleted { .. }
            | SessionEvent::ToolSideEffectUnknown { .. }
            | SessionEvent::TerminalStarted { .. }
            | SessionEvent::TerminalOutputRecorded { .. }
            | SessionEvent::TerminalExited { .. }
            | SessionEvent::CompactionApplied { .. }
            | SessionEvent::TodoReplaced { .. }
            | SessionEvent::PlanChanged { .. }
            | SessionEvent::ProviderSnapshotUpdated { .. }
            | SessionEvent::TitleGenerated { .. }
            | SessionEvent::SubAgentSpawned { .. }
            | SessionEvent::SubAgentStatusChanged { .. }
            | SessionEvent::MailboxMessageQueued { .. }
            | SessionEvent::MailboxMessageDelivered { .. }
            | SessionEvent::WorktreeAssigned { .. }
            | SessionEvent::WorktreeReleased { .. } => {}
        }
    }

    match (completion, first_segment, first_zero_segment) {
        (None, _, None) => Ok(()),
        (None, _, Some((_, segment))) if is_dynamic_input_segment(segment) => Ok(()),
        (
            Some((completion_index, turn_id, source_agent_id, model_round)),
            Some((index, segment)),
            _,
        ) if completion_index.checked_add(1) == Some(index)
            && segment.turn_id == *turn_id
            && segment.source_agent_id == *source_agent_id
            && segment.model_round == model_round =>
        {
            Ok(())
        }
        (Some(_), Some(_), _) => Err(ReductionError::new(
            "模型 Round 与同批首个 Transcript 段的顺序、Turn、Agent 或 Round 不匹配",
        )),
        (Some(_), None, _) => Err(ReductionError::new(
            "模型 Round 缺少同批次对应的首个 Transcript 段",
        )),
        (None, _, Some(_)) => Err(ReductionError::new(
            "首个模型输出 Transcript 段缺少同批次对应的模型 Round 完成事件",
        )),
    }
}

/// 判断 Transcript 段是否只包含可在采样前独立持久化的 mailbox 或 Steer 输入。
fn is_dynamic_input_segment(segment: &crate::TranscriptSegment) -> bool {
    !segment.messages.is_empty()
        && segment.messages.iter().all(|message| {
            matches!(
                message.role,
                crate::MessageRole::User | crate::MessageRole::Developer
            )
        })
}

/// 返回仍处于 Running 的目标 Turn。
fn running_turn<'a>(
    state: &'a mut SessionState,
    turn_id: &crate::TurnId,
) -> Result<&'a mut TurnState, ReductionError> {
    let turn = state
        .turns
        .get_mut(turn_id)
        .ok_or_else(|| ReductionError::new("事件引用了不存在的 Turn"))?;
    if turn.status != TurnStatus::Running {
        return Err(ReductionError::new("Turn 已经结束"));
    }
    Ok(turn)
}

/// 校验外部状态中粗粒度状态、精确停止原因与安全说明严格一致。
fn validate_turn_stop_consistency(state: &SessionState) -> Result<(), ReductionError> {
    let consistent = state.turns.values().all(|turn| match turn.status {
        TurnStatus::Running | TurnStatus::Completed => {
            turn.stop_reason.is_none() && turn.outcome_message.is_none()
        }
        TurnStatus::Failed => {
            matches!(
                turn.stop_reason,
                Some(
                    TurnStopReason::Failed
                        | TurnStopReason::LimitReached
                        | TurnStopReason::ContextBlocked
                        | TurnStopReason::ModelOutputLimit
                        | TurnStopReason::ModelRefusal
                )
            ) && turn
                .outcome_message
                .as_ref()
                .is_some_and(|message| !message.trim().is_empty())
        }
        TurnStatus::Cancelled => {
            turn.stop_reason == Some(TurnStopReason::Cancelled)
                && turn
                    .outcome_message
                    .as_ref()
                    .is_some_and(|message| !message.trim().is_empty())
        }
    });
    if consistent {
        Ok(())
    } else {
        Err(ReductionError::new("Turn 状态、停止原因与结果说明不一致"))
    }
}

/// 确认目标 Turn 仍处于 Running。
fn ensure_turn_running(
    state: &SessionState,
    turn_id: &crate::TurnId,
) -> Result<(), ReductionError> {
    match state.turns.get(turn_id) {
        Some(turn) if turn.status == TurnStatus::Running => Ok(()),
        Some(_) => Err(ReductionError::new("工具或终端不能继续写入已结束 Turn")),
        None => Err(ReductionError::new("工具或终端引用了不存在的 Turn")),
    }
}

/// 从工具请求定位所属 Turn 并确认其仍在运行。
fn ensure_tool_turn_running(
    state: &SessionState,
    request_id: &crate::RequestId,
) -> Result<(), ReductionError> {
    let tool = state
        .tools
        .get(request_id)
        .ok_or_else(|| ReductionError::new("终端引用了不存在的工具请求"))?;
    ensure_turn_running(state, &tool.request.turn_id)
}

/// Turn 进入终态前要求全部工具、终端和模型可见工具结果已经完成。
fn ensure_turn_resources_finished(
    state: &SessionState,
    turn_id: &crate::TurnId,
) -> Result<(), ReductionError> {
    ensure_turn_running(state, turn_id)?;
    let has_open_or_unmaterialized_tool = state.tools.values().any(|tool| {
        tool.request.turn_id == *turn_id
            && (tool.outcome.is_none() || tool.transcript_segment.is_none())
    });
    let has_open_terminal = state.terminals.values().any(|terminal| {
        !terminal.exited
            && state
                .tools
                .get(&terminal.request_id)
                .is_some_and(|tool| tool.request.turn_id == *turn_id)
    });
    if has_open_or_unmaterialized_tool || has_open_terminal {
        Err(ReductionError::new(
            "Turn 仍有未完成工具、未物化工具结果或终端，不能进入终态",
        ))
    } else {
        Ok(())
    }
}

/// 根据全部 Turn、工具和终端推导 Session 生命周期状态。
fn derived_session_status(state: &SessionState) -> SessionStatus {
    if state
        .turns
        .values()
        .any(|turn| turn.status == TurnStatus::Running)
        || state.tools.values().any(|tool| tool.outcome.is_none())
        || state.terminals.values().any(|terminal| !terminal.exited)
    {
        SessionStatus::Running
    } else {
        SessionStatus::Idle
    }
}

/// 把绝对工作树路径转换为用于本机唯一性比较的稳定词法身份。
fn normalized_worktree_path(path: &str) -> Option<String> {
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => return None,
            other => {
                #[cfg(windows)]
                if matches!(other, std::path::Component::Normal(_))
                    && other.as_os_str().to_string_lossy().ends_with(['.', ' '])
                {
                    return None;
                }
                normalized.push(other.as_os_str());
            }
        }
    }
    let identity = normalized.to_string_lossy().into_owned();
    #[cfg(windows)]
    let identity = {
        let identity = identity.replace('\\', "/").to_lowercase();
        if let Some(path) = identity.strip_prefix("//?/unc/") {
            format!("//{path}")
        } else {
            identity
                .strip_prefix("//?/")
                .unwrap_or(&identity)
                .to_owned()
        }
    };
    Some(identity)
}

/// 使用当前权威事件状态刷新派生 Session 状态。
fn refresh_derived_status(state: &mut SessionState) {
    state.status = derived_session_status(state);
}

/// 校验长寿命子 Agent 的状态迁移、Turn 绑定与结果摘要矩阵。
fn valid_sub_agent_transition(
    state: &SessionState,
    agent: &crate::SubAgentState,
    turn_id: Option<&crate::TurnId>,
    next: &SubAgentStatus,
    result_summary: Option<&str>,
) -> bool {
    let transition_allowed = match agent.status {
        SubAgentStatus::Pending => {
            matches!(next, SubAgentStatus::Running | SubAgentStatus::Stopped)
        }
        SubAgentStatus::Running => matches!(
            next,
            SubAgentStatus::Waiting
                | SubAgentStatus::Completed
                | SubAgentStatus::Failed
                | SubAgentStatus::Interrupted
                | SubAgentStatus::Stopped
        ),
        SubAgentStatus::Waiting => matches!(
            next,
            SubAgentStatus::Running
                | SubAgentStatus::Completed
                | SubAgentStatus::Failed
                | SubAgentStatus::Interrupted
                | SubAgentStatus::Stopped
        ),
        SubAgentStatus::Completed | SubAgentStatus::Failed | SubAgentStatus::Interrupted => {
            matches!(next, SubAgentStatus::Running | SubAgentStatus::Stopped)
        }
        SubAgentStatus::Stopped => false,
    };
    let optional_summary_valid = result_summary.is_none_or(|summary| {
        !summary.trim().is_empty() && summary.len() <= MAX_SUB_AGENT_RESULT_SUMMARY_BYTES
    });
    let summary_valid = match next {
        SubAgentStatus::Completed => optional_summary_valid,
        SubAgentStatus::Failed => result_summary.is_some() && optional_summary_valid,
        SubAgentStatus::Pending
        | SubAgentStatus::Running
        | SubAgentStatus::Waiting
        | SubAgentStatus::Interrupted
        | SubAgentStatus::Stopped => result_summary.is_none(),
    };
    let target_turn = turn_id.and_then(|turn_id| state.turns.get(turn_id));
    let target_turn_matches_agent =
        target_turn.is_some_and(|turn| turn.source_agent_id == agent.agent_id);
    let turn_binding_valid = match next {
        SubAgentStatus::Running => {
            target_turn_matches_agent
                && target_turn.is_some_and(|turn| turn.status == TurnStatus::Running)
                && match agent.status {
                    SubAgentStatus::Pending => agent.current_turn_id.is_none(),
                    SubAgentStatus::Waiting => agent.current_turn_id.as_ref() == turn_id,
                    SubAgentStatus::Completed
                    | SubAgentStatus::Failed
                    | SubAgentStatus::Interrupted => agent.current_turn_id.as_ref() != turn_id,
                    SubAgentStatus::Running | SubAgentStatus::Stopped => false,
                }
        }
        SubAgentStatus::Waiting => {
            agent.current_turn_id.as_ref() == turn_id
                && target_turn_matches_agent
                && target_turn.is_some_and(|turn| turn.status == TurnStatus::Running)
        }
        SubAgentStatus::Completed => {
            agent.current_turn_id.as_ref() == turn_id
                && target_turn_matches_agent
                && target_turn.is_some_and(|turn| turn.status == TurnStatus::Completed)
        }
        SubAgentStatus::Failed => {
            agent.current_turn_id.as_ref() == turn_id
                && target_turn_matches_agent
                && target_turn.is_some_and(|turn| turn.status == TurnStatus::Failed)
        }
        SubAgentStatus::Interrupted => {
            agent.current_turn_id.as_ref() == turn_id
                && target_turn_matches_agent
                && target_turn.is_some_and(|turn| turn.status == TurnStatus::Cancelled)
        }
        SubAgentStatus::Stopped => {
            agent.current_turn_id.as_ref() == turn_id
                && !state.turns.values().any(|turn| {
                    turn.source_agent_id == agent.agent_id && turn.status == TurnStatus::Running
                })
        }
        SubAgentStatus::Pending => false,
    };
    transition_allowed && summary_valid && turn_binding_valid
}

/// 校验每个子 Agent 状态只引用自身最新 Turn，且活跃态与 Turn 终态一致。
fn validate_sub_agent_turn_consistency(state: &SessionState) -> Result<(), ReductionError> {
    for agent in state.sub_agents.values() {
        let current_turn = agent
            .current_turn_id
            .as_ref()
            .and_then(|turn_id| state.turns.get(turn_id));
        let current_belongs_to_agent =
            current_turn.is_some_and(|turn| turn.source_agent_id == agent.agent_id);
        let has_running_turn = state.turns.values().any(|turn| {
            turn.source_agent_id == agent.agent_id && turn.status == TurnStatus::Running
        });
        let consistent = match agent.status {
            SubAgentStatus::Pending => agent.current_turn_id.is_none() && !has_running_turn,
            SubAgentStatus::Running | SubAgentStatus::Waiting => {
                current_belongs_to_agent
                    && current_turn.is_some_and(|turn| turn.status == TurnStatus::Running)
                    && has_running_turn
            }
            SubAgentStatus::Completed => {
                current_belongs_to_agent
                    && current_turn.is_some_and(|turn| turn.status == TurnStatus::Completed)
                    && !has_running_turn
            }
            SubAgentStatus::Failed => {
                current_belongs_to_agent
                    && current_turn.is_some_and(|turn| turn.status == TurnStatus::Failed)
                    && !has_running_turn
            }
            SubAgentStatus::Interrupted => {
                current_belongs_to_agent
                    && current_turn.is_some_and(|turn| turn.status == TurnStatus::Cancelled)
                    && !has_running_turn
            }
            SubAgentStatus::Stopped => {
                !has_running_turn
                    && agent.current_turn_id.as_ref().is_none_or(|turn_id| {
                        state.turns.get(turn_id).is_some_and(|turn| {
                            turn.source_agent_id == agent.agent_id
                                && turn.status != TurnStatus::Running
                        })
                    })
            }
        };
        if !consistent {
            return Err(ReductionError::new(
                "子 Agent 状态、当前 Turn 与 Turn 生命周期不一致",
            ));
        }
    }
    Ok(())
}

/// 校验外部状态中的工具索引、Transcript 段引用与生命周期消费关系可以由事件流产生。
fn validate_tool_transcript_consumption_order(state: &SessionState) -> Result<(), ReductionError> {
    let mut requests_by_call = std::collections::BTreeMap::new();
    let mut indexes_by_round =
        std::collections::BTreeMap::<ToolRoundKey, std::collections::BTreeSet<u32>>::new();
    for (request_id, lifecycle) in &state.tools {
        if request_id != &lifecycle.request.request_id {
            return Err(ReductionError::new(
                "工具集合键与生命周期内部请求标识不一致",
            ));
        }
        let round = tool_round_key(lifecycle);
        if !indexes_by_round
            .entry(round.clone())
            .or_default()
            .insert(lifecycle.request.request_index)
        {
            return Err(ReductionError::new(
                "同一模型 Round 的工具 request_index 重复",
            ));
        }
        let call = (
            round.0,
            round.1,
            round.2,
            lifecycle.request.model_tool_call_id.clone(),
        );
        if requests_by_call.insert(call, request_id.clone()).is_some() {
            return Err(ReductionError::new("同一模型 Round 的工具调用标识重复"));
        }
    }

    let mut consumed_requests = std::collections::BTreeSet::new();
    let mut last_consumed_index = std::collections::BTreeMap::<ToolRoundKey, u32>::new();
    for record in &state.transcript {
        let TranscriptRecord::SegmentCommitted(segment) = record else {
            continue;
        };
        let transcript_revision = segment
            .expected_transcript_revision
            .checked_add(1)
            .ok_or_else(|| ReductionError::new("Transcript revision 溢出"))?;
        let expected_reference = TranscriptSegmentReference {
            turn_id: segment.turn_id.clone(),
            source_agent_id: segment.source_agent_id.clone(),
            model_round: segment.model_round,
            segment_index: segment.segment_index,
            transcript_revision,
        };
        let round = (
            segment.turn_id.clone(),
            segment.source_agent_id.clone(),
            segment.model_round,
        );
        for part in segment
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
        {
            let crate::MessagePart::ToolCall { tool_call_id, .. } = part else {
                continue;
            };
            let call = (
                round.0.clone(),
                round.1.clone(),
                round.2,
                tool_call_id.clone(),
            );
            let Some(request_id) = requests_by_call.get(&call) else {
                continue;
            };
            let Some(lifecycle) = state.tools.get(request_id) else {
                return Err(ReductionError::new("工具调用索引引用了不存在的生命周期"));
            };
            if lifecycle.transcript_segment.as_ref() != Some(&expected_reference)
                || !consumed_requests.insert(request_id.clone())
                || last_consumed_index
                    .get(&round)
                    .is_some_and(|previous| lifecycle.request.request_index <= *previous)
            {
                return Err(ReductionError::new(
                    "工具生命周期与 Transcript 段引用或 request_index 消费顺序不一致",
                ));
            }
            last_consumed_index.insert(round.clone(), lifecycle.request.request_index);
        }
    }

    for (request_id, lifecycle) in &state.tools {
        let was_consumed = consumed_requests.contains(request_id);
        if lifecycle.transcript_segment.is_some() != was_consumed {
            return Err(ReductionError::new(
                "工具生命周期与 Transcript 段缺少双向对应关系",
            ));
        }
        if !was_consumed
            && last_consumed_index
                .get(&tool_round_key(lifecycle))
                .is_some_and(|consumed| lifecycle.request.request_index <= *consumed)
        {
            return Err(ReductionError::new(
                "未物化工具的 request_index 不能低于已消费水位",
            ));
        }
    }
    Ok(())
}

/// 返回工具生命周期所属的 Turn、Agent 与逻辑模型 Round。
fn tool_round_key(tool: &ToolLifecycle) -> ToolRoundKey {
    (
        tool.request.turn_id.clone(),
        tool.request.agent_id.clone(),
        tool.request.model_round,
    )
}

/// 判断工具生命周期是否属于同一个 Turn、Agent 与逻辑模型 Round。
fn tool_matches_round(
    tool: &crate::ToolLifecycle,
    turn_id: &crate::TurnId,
    agent_id: &crate::AgentId,
    model_round: u32,
) -> bool {
    tool.request.turn_id == *turn_id
        && tool.request.agent_id == *agent_id
        && tool.request.model_round == model_round
}

/// 判断一个字符串迭代器是否包含重复值。
fn has_duplicate<'a>(values: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    values.into_iter().any(|value| !seen.insert(value))
}

/// 原子校验一个 Transcript 段的 CAS、顺序、消息身份与工具配对。
fn validate_transcript_segment(
    state: &SessionState,
    segment: &crate::TranscriptSegment,
) -> Result<Vec<crate::RequestId>, ReductionError> {
    ensure_turn_running(state, &segment.turn_id)?;
    if !state.is_registered_agent(&segment.source_agent_id)
        || state
            .turns
            .get(&segment.turn_id)
            .is_none_or(|turn| turn.source_agent_id != segment.source_agent_id)
    {
        return Err(ReductionError::new(
            "Transcript 段引用了未注册或不属于 Turn 的 Agent",
        ));
    }
    let expected_segment_index = state
        .transcript_segments()
        .filter(|current| {
            current.turn_id == segment.turn_id
                && current.source_agent_id == segment.source_agent_id
                && current.model_round == segment.model_round
        })
        .map(|current| current.segment_index)
        .max()
        .map_or(Some(0), |index| index.checked_add(1))
        .ok_or_else(|| ReductionError::new("Transcript 段序号溢出"))?;
    if segment.model_round == 0
        || segment.segment_index != expected_segment_index
        || segment.expected_transcript_revision != state.transcript_revision
        || segment.messages.is_empty()
    {
        return Err(ReductionError::new(
            "Transcript 段 Round、段序号、revision 或消息为空",
        ));
    }

    let existing_tool_call_ids = state
        .transcript_segments()
        .filter(|current| {
            current.turn_id == segment.turn_id
                && current.source_agent_id == segment.source_agent_id
                && current.model_round == segment.model_round
        })
        .flat_map(|current| current.messages.iter())
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            crate::MessagePart::ToolCall { tool_call_id, .. } => Some(tool_call_id.as_str()),
            crate::MessagePart::Text { .. }
            | crate::MessagePart::Reasoning { .. }
            | crate::MessagePart::Image { .. }
            | crate::MessagePart::ToolResult { .. }
            | crate::MessagePart::Artifact { .. } => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut message_ids = std::collections::BTreeSet::new();
    let mut calls = std::collections::BTreeMap::<String, (&str, &serde_json::Value)>::new();
    let mut call_order = Vec::new();
    let mut results = std::collections::BTreeMap::<String, crate::PersistedToolResult>::new();
    for message in &segment.messages {
        let agent_matches = message_agent_matches_source(message, &segment.source_agent_id);
        if !valid_message_shape(message)
            || !message_ids.insert(message.message_id.as_str())
            || state.contains_transcript_message_id(&message.message_id)
            || message.turn_id.as_ref() != Some(&segment.turn_id)
            || !agent_matches
        {
            return Err(ReductionError::new("Transcript 段消息身份、角色或内容无效"));
        }
        for part in &message.content {
            match part {
                crate::MessagePart::ToolCall {
                    tool_call_id,
                    tool_name,
                    arguments,
                } => {
                    if existing_tool_call_ids.contains(tool_call_id.as_str())
                        || calls
                            .insert(tool_call_id.clone(), (tool_name.as_str(), arguments))
                            .is_some()
                    {
                        return Err(ReductionError::new(
                            "同一模型 Round 的 Transcript 工具调用标识重复",
                        ));
                    }
                    call_order.push(tool_call_id.clone());
                }
                crate::MessagePart::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    if !calls.contains_key(tool_call_id) {
                        return Err(ReductionError::new(
                            "Transcript 段工具结果不能早于对应工具调用",
                        ));
                    }
                    let result = crate::PersistedToolResult {
                        tool_call_id: tool_call_id.clone(),
                        content: content.clone(),
                        is_error: *is_error,
                    };
                    if results.insert(tool_call_id.clone(), result).is_some() {
                        return Err(ReductionError::new("Transcript 段工具结果标识重复"));
                    }
                }
                crate::MessagePart::Text { .. }
                | crate::MessagePart::Reasoning { .. }
                | crate::MessagePart::Image { .. }
                | crate::MessagePart::Artifact { .. } => {}
            }
        }
    }
    if results.len() != calls.len()
        || results
            .keys()
            .any(|tool_call_id| !calls.contains_key(tool_call_id))
    {
        return Err(ReductionError::new(
            "Transcript 段的工具调用与结果不是完整一一配对",
        ));
    }
    let mut consumed_request_ids = Vec::new();
    let mut previous_request_index = state
        .tools
        .values()
        .filter(|tool| {
            tool_matches_round(
                tool,
                &segment.turn_id,
                &segment.source_agent_id,
                segment.model_round,
            ) && tool.transcript_segment.is_some()
        })
        .map(|tool| tool.request.request_index)
        .max();
    for tool_call_id in call_order {
        let (tool_name, arguments) = calls
            .get(&tool_call_id)
            .copied()
            .expect("工具调用顺序必须引用已校验的调用");
        let lifecycle = state.tools.values().find(|tool| {
            tool_matches_round(
                tool,
                &segment.turn_id,
                &segment.source_agent_id,
                segment.model_round,
            ) && tool.request.model_tool_call_id == tool_call_id
        });
        let Some(lifecycle) = lifecycle else {
            if results
                .get(&tool_call_id)
                .is_some_and(|result| result.is_error)
            {
                continue;
            }
            return Err(ReductionError::new(
                "没有工具生命周期的 Transcript 工具交换只能保存 Agent 合成错误结果",
            ));
        };
        if previous_request_index
            .is_some_and(|previous| lifecycle.request.request_index <= previous)
        {
            return Err(ReductionError::new(
                "Transcript 段中的真实工具调用必须按 request_index 严格递增",
            ));
        }
        let has_unconsumed_lower_request = state.tools.values().any(|tool| {
            tool_matches_round(
                tool,
                &segment.turn_id,
                &segment.source_agent_id,
                segment.model_round,
            ) && tool.transcript_segment.is_none()
                && tool.request.request_index < lifecycle.request.request_index
                && !consumed_request_ids.contains(&tool.request.request_id)
        });
        if has_unconsumed_lower_request {
            return Err(ReductionError::new(
                "Transcript 段不能越过已存在但尚未物化的更低 request_index",
            ));
        }
        previous_request_index = Some(lifecycle.request.request_index);
        let Some(outcome) = lifecycle.outcome.as_ref() else {
            return Err(ReductionError::new(
                "Transcript 段引用了尚未结束的工具生命周期",
            ));
        };
        if lifecycle.transcript_segment.is_some() {
            return Err(ReductionError::new(
                "工具生命周期已经被其他 Transcript 段消费",
            ));
        }
        if lifecycle.request.tool_name != tool_name
            || &lifecycle.request.arguments != arguments
            || results.get(&tool_call_id) != Some(&outcome.result)
        {
            return Err(ReductionError::new(
                "Transcript 段与工具生命周期的调用或结果不一致",
            ));
        }
        consumed_request_ids.push(lifecycle.request.request_id.clone());
    }
    Ok(consumed_request_ids)
}

/// 校验一条消息的显式 Agent 与 Turn/段来源一致，并允许输入类消息省略 Agent 身份。
fn message_agent_matches_source(
    message: &crate::SessionMessage,
    source_agent_id: &crate::AgentId,
) -> bool {
    match message.role {
        crate::MessageRole::Assistant | crate::MessageRole::Tool => {
            message.agent_id.as_ref() == Some(source_agent_id)
        }
        crate::MessageRole::System | crate::MessageRole::Developer | crate::MessageRole::User => {
            message
                .agent_id
                .as_ref()
                .is_none_or(|agent_id| agent_id == source_agent_id)
        }
    }
}

/// 校验独立消息的显式 Agent 已注册，且 Assistant/Tool 消息不能缺失身份。
fn valid_message_agent_identity(state: &SessionState, message: &crate::SessionMessage) -> bool {
    let registered = message
        .agent_id
        .as_ref()
        .is_none_or(|agent_id| state.is_registered_agent(agent_id));
    let required_identity_present = match message.role {
        crate::MessageRole::Assistant | crate::MessageRole::Tool => message.agent_id.is_some(),
        crate::MessageRole::System | crate::MessageRole::Developer | crate::MessageRole::User => {
            true
        }
    };
    registered && required_identity_present
}

/// 校验工具终态、错误位和所有模型可见结果块保持一致。
fn valid_tool_outcome(outcome: &crate::ToolOutcome) -> bool {
    let error_flag_matches = match outcome.status {
        crate::ToolCompletionStatus::Succeeded => !outcome.result.is_error,
        crate::ToolCompletionStatus::Failed
        | crate::ToolCompletionStatus::Cancelled
        | crate::ToolCompletionStatus::SideEffectUnknown => outcome.result.is_error,
    };
    error_flag_matches
        && !outcome.result.tool_call_id.trim().is_empty()
        && outcome.result.tool_call_id.len() <= 1_024
        && outcome.result.content.iter().all(valid_tool_result_part)
}

/// 校验文件变更快照只保存跨平台绝对路径和不含正文的有效快照引用。
fn valid_tool_file_change_shape(change: &crate::ToolFileChange) -> bool {
    !change.path.trim().is_empty()
        && valid_cross_platform_absolute_path(&change.path)
        && !change.applied
        && change.after.validate_shape().is_ok()
        && change
            .before
            .as_ref()
            .is_none_or(|snapshot| snapshot.validate_shape().is_ok())
}

/// 识别 Unix、Windows 驱动器和 Windows UNC 绝对路径，不依赖当前运行平台。
fn valid_cross_platform_absolute_path(path: &str) -> bool {
    if path.is_empty() || path.chars().any(char::is_control) {
        return false;
    }
    if path.starts_with('/') {
        return true;
    }
    if let Some(unc_path) = path.strip_prefix("\\\\") {
        let components = unc_path
            .split(['\\', '/'])
            .filter(|component| !component.is_empty());
        return components.take(2).count() == 2;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// 判断字符串是否为固定 64 位小写十六进制 SHA-256。
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// 校验桌面控制 operationId 与 Runtime 公共边界使用同一有限字符规则。
fn valid_control_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// 校验事件中的内容寻址引用没有让标识、摘要或媒体类型互相矛盾。
fn valid_artifact_use(artifact: &ArtifactUse) -> bool {
    artifact.artifact_id.as_str() == artifact.sha256
        && artifact.media_type.as_deref().is_none_or(|media_type| {
            !media_type.trim().is_empty()
                && !media_type.contains('\r')
                && !media_type.contains('\n')
        })
}

/// 校验一条持久消息的角色、内容形状和恢复所需字段。
fn valid_message_part(role: &crate::MessageRole, part: &crate::MessagePart) -> bool {
    let shape_valid = match part {
        crate::MessagePart::Text { text } => !text.is_empty(),
        crate::MessagePart::Reasoning {
            text,
            summary,
            continuation,
        } => {
            (!text.is_empty()
                || summary.as_deref().is_some_and(|value| !value.is_empty())
                || continuation.is_some())
                && summary.as_deref().is_none_or(|value| !value.is_empty())
                && continuation
                    .as_ref()
                    .is_none_or(|state| !state.kind.trim().is_empty() && !state.data.is_null())
        }
        crate::MessagePart::Image { source } => valid_image_source(source),
        crate::MessagePart::ToolCall {
            tool_call_id,
            tool_name,
            arguments,
        } => {
            !tool_call_id.trim().is_empty()
                && tool_call_id.len() <= 1_024
                && !tool_name.trim().is_empty()
                && arguments.is_object()
        }
        crate::MessagePart::ToolResult {
            tool_call_id,
            content,
            ..
        } => {
            !tool_call_id.trim().is_empty()
                && tool_call_id.len() <= 1_024
                && content.iter().all(valid_tool_result_part)
        }
        crate::MessagePart::Artifact {
            artifact,
            materialization,
        } => {
            valid_artifact_use(artifact)
                && match materialization {
                    crate::ArtifactMaterialization::Utf8Text => {
                        crate::artifact::text_materialization_media_type_matches(
                            artifact.media_type.as_deref(),
                        )
                    }
                    crate::ArtifactMaterialization::Image => artifact
                        .media_type
                        .as_deref()
                        .is_some_and(crate::artifact::image_materialization_matches),
                    crate::ArtifactMaterialization::Binary => true,
                }
        }
    };
    shape_valid
        && match role {
            crate::MessageRole::System | crate::MessageRole::Developer => match part {
                crate::MessagePart::Text { .. } => true,
                crate::MessagePart::Artifact {
                    materialization, ..
                } => !matches!(materialization, crate::ArtifactMaterialization::Image),
                crate::MessagePart::Reasoning { .. }
                | crate::MessagePart::Image { .. }
                | crate::MessagePart::ToolCall { .. }
                | crate::MessagePart::ToolResult { .. } => false,
            },
            crate::MessageRole::User => matches!(
                part,
                crate::MessagePart::Text { .. }
                    | crate::MessagePart::Image { .. }
                    | crate::MessagePart::Artifact { .. }
            ),
            crate::MessageRole::Assistant => match part {
                crate::MessagePart::Text { .. }
                | crate::MessagePart::Reasoning { .. }
                | crate::MessagePart::ToolCall { .. } => true,
                crate::MessagePart::Artifact {
                    materialization, ..
                } => !matches!(materialization, crate::ArtifactMaterialization::Image),
                crate::MessagePart::Image { .. } | crate::MessagePart::ToolResult { .. } => false,
            },
            crate::MessageRole::Tool => matches!(part, crate::MessagePart::ToolResult { .. }),
        }
}

/// 校验一条持久消息共有的标识、角色、模型可见内容和类型化内容形状。
pub(crate) fn valid_message_shape(message: &crate::SessionMessage) -> bool {
    !message.message_id.trim().is_empty()
        && !message.content.is_empty()
        && message_has_model_visible_content(message)
        && message
            .content
            .iter()
            .all(|part| valid_message_part(&message.role, part))
}

/// 校验独立消息不携带必须由 Transcript 段原子保存的工具交换。
pub(crate) fn valid_standalone_message_shape(message: &crate::SessionMessage) -> bool {
    valid_message_shape(message)
        && message.content.iter().all(|part| {
            !matches!(
                part,
                crate::MessagePart::ToolCall { .. } | crate::MessagePart::ToolResult { .. }
            )
        })
}

/// 判断一条顶层消息在移除仅审计 Binary Artifact 后仍保留模型可见内容。
fn message_has_model_visible_content(message: &crate::SessionMessage) -> bool {
    message.content.iter().any(|part| {
        !matches!(
            part,
            crate::MessagePart::Artifact {
                materialization: crate::ArtifactMaterialization::Binary,
                ..
            }
        )
    })
}

/// 校验图片来源可以在恢复时重新解析且不会内联无界二进制正文。
fn valid_image_source(source: &crate::MessageImageSource) -> bool {
    match source {
        crate::MessageImageSource::Url { url } => {
            !url.trim().is_empty()
                && url.len() <= 16 * 1024
                && !url.contains('\r')
                && !url.contains('\n')
        }
        crate::MessageImageSource::Artifact { artifact } => valid_artifact_use(artifact),
    }
}

/// 校验工具结果的嵌套内容块。
fn valid_tool_result_part(part: &crate::ToolResultPart) -> bool {
    match part {
        crate::ToolResultPart::Text { .. } => true,
        crate::ToolResultPart::Image { source } => valid_image_source(source),
        crate::ToolResultPart::Artifact {
            artifact,
            materialization,
        } => {
            valid_artifact_use(artifact)
                && match materialization {
                    crate::ArtifactMaterialization::Utf8Text => {
                        crate::artifact::text_materialization_media_type_matches(
                            artifact.media_type.as_deref(),
                        )
                    }
                    crate::ArtifactMaterialization::Image => artifact
                        .media_type
                        .as_deref()
                        .is_some_and(crate::artifact::image_materialization_matches),
                    crate::ArtifactMaterialization::Binary => true,
                }
        }
    }
}
