//! 协作状态机的事件入队、Turn 调度与 mailbox 投递原语。
//!
//! 这些自由函数直接操作 `CoordinatorState`，由 `CollaborationCoordinator`
//! 的锁临界区调用，不做任何 IO。

use super::*;

impl CollaborationCoordinator {
    /// 为当前 Turn 持久 claim mailbox 最早前缀；重复调用在确认前返回同一批正文。
    pub fn consume_mailbox(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        maximum: usize,
    ) -> Result<Vec<MailboxMessage>, CollaborationError> {
        if maximum == 0 {
            return Err(CollaborationError::InvalidMailboxBatch);
        }
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            if let Some(claim) = &agent.mailbox_claim {
                if claim.turn_id != turn_id {
                    return Err(CollaborationError::InputClaimMismatch {
                        agent_id: agent_id.clone(),
                        turn_id: turn_id.clone(),
                        input_kind: "mailbox",
                    });
                }
                let messages = agent
                    .mailbox
                    .iter()
                    .take_while(|entry| entry.message.sequence <= claim.through_sequence)
                    .map(|entry| entry.message.clone())
                    .collect::<Vec<_>>();
                if messages.last().map(|message| message.sequence) != Some(claim.through_sequence) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "mailbox claim 未对应仍保留的完整 FIFO 前缀".to_owned(),
                    });
                }
                return Ok(Transition {
                    output: messages,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            if agent.mailbox.is_empty() {
                return Ok(Transition {
                    output: Vec::new(),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let count = maximum.min(agent.mailbox.len());
            let messages = agent
                .mailbox
                .iter()
                .take(count)
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>();
            let through_sequence = messages
                .last()
                .expect("非空 mailbox 前缀始终有最后一条")
                .sequence;
            let message_ids = messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>();
            let definition = agent.definition.clone();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.mailbox_claim = Some(InputBatchClaim {
                turn_id: turn_id.clone(),
                through_sequence,
            });
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::AgentMessagesClaimed {
                    message_ids,
                    through_sequence,
                },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: messages,
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 在 Runtime 已原子提交 Transcript 后确认并删除此前 claim 的 mailbox 前缀。
    pub fn acknowledge_mailbox(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        through_sequence: u64,
    ) -> Result<(), CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let agent = resident_agent(state, &agent_id)?;
            let Some(claim) = agent.mailbox_claim.clone() else {
                return Ok(Transition {
                    output: (),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            };
            if claim.turn_id != turn_id || claim.through_sequence != through_sequence {
                return Err(CollaborationError::InputClaimMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                    input_kind: "mailbox",
                });
            }
            let (definition, parent_turn_id, root_turn_id) =
                input_claim_event_context(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            let messages = agent
                .mailbox
                .iter()
                .take_while(|entry| entry.message.sequence <= through_sequence)
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>();
            if messages.last().map(|message| message.sequence) != Some(through_sequence) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "确认 mailbox claim 时找不到完整 FIFO 前缀".to_owned(),
                });
            }
            let count = messages.len();
            let consumed_bytes = messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>();
            let consumed_completion_count = messages
                .iter()
                .filter(|message| {
                    matches!(message.kind, MailboxMessageKind::ChildTurnFinished { .. })
                })
                .count();
            let consumed_completion_bytes = messages
                .iter()
                .filter(|message| {
                    matches!(message.kind, MailboxMessageKind::ChildTurnFinished { .. })
                })
                .map(|message| message.content.len())
                .sum::<usize>();
            let message_ids = messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.mailbox.drain(..count);
            agent.mailbox_claim = None;
            agent.mailbox_bytes =
                agent
                    .mailbox_bytes
                    .checked_sub(consumed_bytes)
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "确认 mailbox 时正文总字节数下溢".to_owned(),
                    })?;
            agent.completion_count = agent
                .completion_count
                .checked_sub(consumed_completion_count)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时完成通知计数下溢".to_owned(),
                })?;
            agent.completion_bytes = agent
                .completion_bytes
                .checked_sub(consumed_completion_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时完成通知字节下溢".to_owned(),
                })?;
            let root = state
                .roots
                .get_mut(&definition.root_agent_id)
                .expect("确认 mailbox 的根树应存在");
            root.mailbox_count = root.mailbox_count.checked_sub(count).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树计数下溢".to_owned(),
                }
            })?;
            root.mailbox_bytes =
                root.mailbox_bytes
                    .checked_sub(consumed_bytes)
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "确认 mailbox 时根树字节下溢".to_owned(),
                    })?;
            root.completion_count = root
                .completion_count
                .checked_sub(consumed_completion_count)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树完成计数下溢".to_owned(),
                })?;
            root.completion_bytes = root
                .completion_bytes
                .checked_sub(consumed_completion_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树完成字节下溢".to_owned(),
                })?;
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id,
                    root_turn_id: Some(root_turn_id),
                },
                CollaborationEventKind::AgentMessagesConsumed {
                    message_ids,
                    through_sequence,
                },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: (),
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 将用户 steer 持久化到指定活跃 Turn，并唤醒 WaitAgent 与执行器安全边界。
    pub fn steer_agent(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        content: impl Into<String>,
    ) -> Result<UserSteer, CollaborationError> {
        let content = content.into();
        validate_required_text(&content, "用户 Steer")?;
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let steer = queue_user_steer(
                state,
                &agent_id,
                &turn_id,
                content.clone(),
                Vec::new(),
                &mut events,
                &mut actions,
            )?;
            Ok(Transition {
                output: steer.clone(),
                events,
                actions,
            })
        })
    }

    /// 以外部稳定操作标识向当前运行 Turn 注入用户 steer，并跨进程持久去重。
    pub fn steer_active_agent_with_operation(
        &self,
        agent_id: &AgentId,
        operation_id: &ToolCallId,
        content: impl Into<String>,
        references: Vec<keencode_model::InputReference>,
    ) -> Result<UserSteer, CollaborationError> {
        let content = content.into();
        validate_required_text(&content, "用户 Steer")?;
        validate_input_references(&references)?;
        let agent_id = agent_id.clone();
        let operation_id = operation_id.clone();
        self.apply_transition(|state| {
            let invocation_input = CollaborationInvocationInput::SteerAgent {
                target_agent_id: agent_id.clone(),
                content: content.clone(),
                references: references.clone(),
            };
            let active_turn_id = match &resident_agent(state, &agent_id)?.status {
                CollaborationAgentStatus::Running { turn_id } => Some(turn_id.clone()),
                _ => None,
            };
            if let Some((key, record)) = state.collaboration_invocations.iter().find(|(key, _)| {
                key.source_agent_id == agent_id
                    && key.tool_call_id == operation_id
                    && (active_turn_id.as_ref() == Some(&key.source_turn_id)
                        || active_turn_id.is_none())
            }) {
                if record.kind == CollaborationInvocationKind::SteerAgent
                    && record.input_digest
                        == collaboration_invocation_input_digest(&invocation_input)
                    && let CollaborationInvocationOutput::UserSteer(steer) = &record.output
                {
                    return Ok(Transition {
                        output: steer.clone(),
                        events: Vec::new(),
                        actions: Vec::new(),
                    });
                }
                return Err(CollaborationError::IdempotencyConflict {
                    source_agent_id: key.source_agent_id.clone(),
                    source_turn_id: key.source_turn_id.clone(),
                    tool_call_id: key.tool_call_id.clone(),
                });
            }
            let Some(turn_id) = active_turn_id else {
                return Err(CollaborationError::TargetNotRunning {
                    agent_id: agent_id.clone(),
                });
            };
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let definition = resident_agent(state, &agent_id)?.definition.clone();
            let invocation_key = CollaborationInvocationKey {
                source_agent_id: agent_id.clone(),
                source_turn_id: turn_id.clone(),
                tool_call_id: operation_id.clone(),
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let steer = queue_user_steer(
                state,
                &agent_id,
                &turn_id,
                content.clone(),
                references.clone(),
                &mut events,
                &mut actions,
            )?;
            let receipt = record_collaboration_invocation(
                state,
                invocation_key,
                invocation_input,
                CollaborationInvocationOutput::UserSteer(steer.clone()),
            )?;
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::CollaborationInvocationCommitted {
                    receipt: Box::new(receipt),
                },
            )?;
            Ok(Transition {
                output: steer,
                events,
                actions,
            })
        })
    }

    /// 为当前 Turn 持久 claim 全部现有用户 steer；确认前重复调用返回同一批正文。
    pub fn consume_user_steers(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
    ) -> Result<Vec<UserSteer>, CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            if let Some(claim) = &agent.steer_claim {
                if claim.turn_id != turn_id {
                    return Err(CollaborationError::InputClaimMismatch {
                        agent_id: agent_id.clone(),
                        turn_id: turn_id.clone(),
                        input_kind: "用户 steer",
                    });
                }
                let steers = agent
                    .steers
                    .iter()
                    .filter(|steer| {
                        steer.turn_id == turn_id && steer.sequence <= claim.through_sequence
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if steers.last().map(|steer| steer.sequence) != Some(claim.through_sequence) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "用户 steer claim 未对应仍保留的完整批次".to_owned(),
                    });
                }
                return Ok(Transition {
                    output: steers,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let steers = agent
                .steers
                .iter()
                .filter(|steer| steer.turn_id == turn_id)
                .cloned()
                .collect::<Vec<_>>();
            if steers.is_empty() {
                return Ok(Transition {
                    output: Vec::new(),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let sequences = steers
                .iter()
                .map(|steer| steer.sequence)
                .collect::<Vec<_>>();
            let through_sequence = *sequences.last().expect("非空用户 steer 批次始终有最大序号");
            let definition = agent.definition.clone();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.steer_claim = Some(InputBatchClaim {
                turn_id: turn_id.clone(),
                through_sequence,
            });
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::AgentUserSteersClaimed { sequences },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: steers,
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 在 Runtime 已原子提交 Transcript 后确认并删除此前 claim 的用户 steer。
    pub fn acknowledge_user_steers(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        through_sequence: u64,
    ) -> Result<(), CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let agent = resident_agent(state, &agent_id)?;
            let Some(claim) = agent.steer_claim.clone() else {
                return Ok(Transition {
                    output: (),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            };
            if claim.turn_id != turn_id || claim.through_sequence != through_sequence {
                return Err(CollaborationError::InputClaimMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                    input_kind: "用户 steer",
                });
            }
            let (definition, parent_turn_id, root_turn_id) =
                input_claim_event_context(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            let steers = agent
                .steers
                .iter()
                .filter(|steer| steer.turn_id == turn_id && steer.sequence <= through_sequence)
                .cloned()
                .collect::<Vec<_>>();
            if steers.last().map(|steer| steer.sequence) != Some(through_sequence) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "确认用户 steer claim 时找不到完整批次".to_owned(),
                });
            }
            let sequences = steers
                .iter()
                .map(|steer| steer.sequence)
                .collect::<Vec<_>>();
            let consumed_bytes = steers.iter().map(UserSteer::payload_bytes).sum::<usize>();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent
                .steers
                .retain(|steer| steer.turn_id != turn_id || steer.sequence > through_sequence);
            agent.steer_claim = None;
            agent.steer_bytes = agent
                .steer_bytes
                .checked_sub(consumed_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认用户 steer 时正文总字节数下溢".to_owned(),
                })?;
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id,
                    root_turn_id: Some(root_turn_id),
                },
                CollaborationEventKind::AgentUserSteersConsumed { sequences },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: (),
                events,
                actions: Vec::new(),
            })
        })
    }
}

impl CollaborationCoordinator {
    /// 返回尚未消费的 mailbox 消息快照，不改变 exactly-once 状态。
    pub fn mailbox(&self, agent_id: &AgentId) -> Result<Vec<MailboxMessage>, CollaborationError> {
        let state = self.lock_state()?;
        let agent = resident_agent(&state, agent_id)?;
        Ok(agent
            .mailbox
            .iter()
            .map(|entry| entry.message.clone())
            .collect())
    }
}

/// 在候选状态内追加一条用户 steer、对应权威事件和安全边界唤醒动作。
pub(super) fn queue_user_steer(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    content: String,
    references: Vec<keencode_model::InputReference>,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<UserSteer, CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let root_agent_id = agent.definition.root_agent_id.clone();
    ensure_tree_open(state, &root_agent_id)?;
    match &agent.status {
        CollaborationAgentStatus::Running {
            turn_id: active_turn_id,
        } if active_turn_id == turn_id => {}
        CollaborationAgentStatus::Running { .. } => {
            return Err(CollaborationError::TurnMismatch {
                agent_id: agent_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        _ => {
            return Err(CollaborationError::TargetNotRunning {
                agent_id: agent_id.clone(),
            });
        }
    }
    let active = active_turn_for_agent(state, agent_id, turn_id)?;
    let agent = resident_agent(state, agent_id)?;
    if agent.steers.len() >= MAX_PENDING_STEERS_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 数量",
            maximum: MAX_PENDING_STEERS_PER_AGENT,
        });
    }
    let steer = UserSteer {
        sequence: agent.next_steer_sequence,
        turn_id: turn_id.clone(),
        content,
        references,
    };
    let next_steer_bytes = agent
        .steer_bytes
        .checked_add(steer.payload_bytes())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_steer_bytes > MAX_PENDING_STEER_BYTES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 总字节数",
            maximum: MAX_PENDING_STEER_BYTES_PER_AGENT,
        });
    }
    let sequence = agent.next_steer_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let definition = agent.definition.clone();
    let agent = state.agents.get_mut(agent_id).expect("Agent 在上方已校验");
    agent.next_steer_sequence = next_sequence;
    agent.steer_bytes = next_steer_bytes;
    agent.steers.push_back(steer.clone());
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: agent_id.clone(),
            turn_id: Some(turn_id.clone()),
            parent_turn_id: active.parent_turn_id,
            root_turn_id: Some(active.root_turn_id),
        },
        CollaborationEventKind::AgentUserSteered {
            steer: steer.clone(),
        },
    )?;
    mark_activity(state, agent_id, actions)?;
    queue_turn_signal(
        state,
        agent_id,
        turn_id,
        AgentTurnSignalKind::UserSteer,
        actions,
    )?;
    Ok(steer)
}

/// 返回已驻留 Agent，或生成类型化不存在错误。
pub(super) fn resident_agent<'a>(
    state: &'a CoordinatorState,
    agent_id: &AgentId,
) -> Result<&'a AgentEntry, CollaborationError> {
    state
        .agents
        .get(agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: agent_id.clone(),
        })
}

/// 为活跃或崩溃后中断的输入 claim 恢复稳定事件因果字段。
pub(super) fn input_claim_event_context(
    state: &CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<(AgentDefinition, Option<TurnId>, TurnId), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    if let Some(active) = state
        .active_turns
        .get(turn_id)
        .filter(|active| &active.agent_id == agent_id)
    {
        return Ok((
            agent.definition.clone(),
            active.parent_turn_id.clone(),
            active.root_turn_id.clone(),
        ));
    }
    if let Some(last_turn) = agent
        .last_turn
        .as_ref()
        .filter(|last_turn| &last_turn.turn_id == turn_id)
    {
        return Ok((
            agent.definition.clone(),
            last_turn.parent_turn_id.clone(),
            last_turn.root_turn_id.clone(),
        ));
    }
    Err(CollaborationError::InputClaimMismatch {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        input_kind: "可恢复",
    })
}

/// 将崩溃、取消或失败 Turn 遗留的输入及其 claim 原子重绑定到后续 Turn。
pub(super) fn rebind_pending_inputs(
    agent: &mut AgentEntry,
    previous_turn_id: &TurnId,
    turn_id: &TurnId,
) {
    for entry in &mut agent.mailbox {
        if entry.claimed_turn_id.as_ref() == Some(previous_turn_id) {
            entry.claimed_turn_id = Some(turn_id.clone());
        }
    }
    for steer in &mut agent.steers {
        if &steer.turn_id == previous_turn_id {
            steer.turn_id = turn_id.clone();
        }
    }
    if agent
        .mailbox_claim
        .as_ref()
        .is_some_and(|claim| &claim.turn_id == previous_turn_id)
    {
        agent
            .mailbox_claim
            .as_mut()
            .expect("mailbox claim 在上方已确认存在")
            .turn_id = turn_id.clone();
    }
    if agent
        .steer_claim
        .as_ref()
        .is_some_and(|claim| &claim.turn_id == previous_turn_id)
    {
        agent
            .steer_claim
            .as_mut()
            .expect("steer claim 在上方已确认存在")
            .turn_id = turn_id.clone();
    }
}

/// 在候选状态中分配下一全局事件序号并追加完整事件。
pub(super) fn push_event(
    state: &mut CoordinatorState,
    events: &mut Vec<CollaborationEvent>,
    agent: &AgentDefinition,
    link: EventLink,
    kind: CollaborationEventKind,
) -> Result<(), CollaborationError> {
    let sequence = state
        .last_event_sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    state.last_event_sequence = sequence;
    events.push(CollaborationEvent {
        session_id: agent.session_id.clone(),
        turn_id: link.turn_id,
        source_agent_id: link.source_agent_id,
        agent_id: agent.agent_id.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        agent_path: agent.path.clone(),
        parent_turn_id: link.parent_turn_id,
        root_turn_id: link.root_turn_id,
        sequence,
        kind,
    });
    Ok(())
}

/// 替换 Agent 状态并追加独立的 agent_status_changed 事件。
pub(super) fn set_status(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    current: CollaborationAgentStatus,
    link: EventLink,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let previous = agent.status.clone();
    if previous == current {
        return Ok(());
    }
    let definition = agent.definition.clone();
    state
        .agents
        .get_mut(agent_id)
        .expect("Agent 在上方已校验")
        .status = current.clone();
    push_event(
        state,
        events,
        &definition,
        link,
        CollaborationEventKind::AgentStatusChanged { previous, current },
    )
}

/// 在不修改状态时计算下一棵根树的单调命名空间身份。
pub(super) fn prospective_root_agent_id(
    state: &CoordinatorState,
) -> Result<AgentId, CollaborationError> {
    if state.next_root_sequence == 0 {
        return Err(CollaborationError::SequenceExhausted);
    }
    AgentId::new(format!(
        "root/{}/{}",
        state.root_identity_namespace, state.next_root_sequence
    ))
    .map_err(|_error| CollaborationError::IdentifierCollision { kind: "Root Agent" })
}

/// 校验根身份属于持久命名空间且序号已经由 counter 分配。
pub(super) fn root_agent_id_belongs_to_namespace(
    namespace: &AgentId,
    next_root_sequence: u64,
    root_agent_id: &AgentId,
) -> bool {
    let prefix = format!("root/{namespace}/");
    root_agent_id
        .as_str()
        .strip_prefix(&prefix)
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .is_some_and(|sequence| sequence > 0 && sequence < next_root_sequence)
}

/// 原子分配下一棵根树身份并推进持久单调 counter。
pub(super) fn allocate_root_agent_id(
    state: &mut CoordinatorState,
) -> Result<AgentId, CollaborationError> {
    let root_agent_id = prospective_root_agent_id(state)?;
    state.next_root_sequence = state
        .next_root_sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    Ok(root_agent_id)
}

/// 为根树分配跨恢复单调递增且永不复用的 TurnId。
pub(super) fn allocate_turn_id(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<TurnId, CollaborationError> {
    let root =
        state
            .roots
            .get_mut(root_agent_id)
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: root_agent_id.clone(),
            })?;
    let sequence = root.next_turn_sequence;
    root.next_turn_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    TurnId::new(format!("turn/{root_agent_id}/{sequence}"))
        .map_err(|_error| CollaborationError::IdentifierCollision { kind: "Turn" })
}

/// 校验 TurnId 属于指定根 Session 命名空间且序号已经分配。
pub(super) fn turn_id_belongs_to_root(
    root_agent_id: &AgentId,
    next_turn_sequence: u64,
    turn_id: &TurnId,
) -> bool {
    turn_sequence_for_root(root_agent_id, turn_id)
        .is_some_and(|sequence| sequence > 0 && sequence < next_turn_sequence)
}

/// 判断 Turn 是否属于根树的内部单调命名空间或已持久绑定的外部根 Turn。
pub(super) fn state_turn_belongs_to_root(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
    turn_id: &TurnId,
) -> bool {
    state.roots.get(root_agent_id).is_some_and(|root| {
        turn_id_belongs_to_root(root_agent_id, root.next_turn_sequence, turn_id)
            || state
                .root_turn_bindings
                .get(turn_id)
                .is_some_and(|binding| &binding.root_agent_id == root_agent_id)
    })
}

/// 从属于指定根身份的 TurnId 解析单调序号。
pub(super) fn turn_sequence_for_root(root_agent_id: &AgentId, turn_id: &TurnId) -> Option<u64> {
    let prefix = format!("turn/{root_agent_id}/");
    turn_id
        .as_str()
        .strip_prefix(&prefix)
        .and_then(|sequence| sequence.parse::<u64>().ok())
}

/// 将新 Turn 持久化入队，但不预约任何容量。
pub(super) fn queue_turn(
    state: &mut CoordinatorState,
    queued: QueuedTurn,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    ensure_tree_open(state, &queued.root_agent_id)?;
    let agent = resident_agent(state, &queued.agent_id)?;
    if !agent.status.is_idle() && agent.status != CollaborationAgentStatus::PendingInit {
        return Err(CollaborationError::TargetNotIdle {
            agent_id: queued.agent_id.clone(),
        });
    }
    if agent
        .mailbox_claim
        .as_ref()
        .is_some_and(|claim| claim.turn_id != queued.turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: queued.agent_id.clone(),
            turn_id: agent
                .mailbox_claim
                .as_ref()
                .expect("mailbox claim 在上方已确认存在")
                .turn_id
                .clone(),
            input_kind: "mailbox",
        });
    }
    if agent
        .steer_claim
        .as_ref()
        .is_some_and(|claim| claim.turn_id != queued.turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: queued.agent_id.clone(),
            turn_id: agent
                .steer_claim
                .as_ref()
                .expect("steer claim 在上方已确认存在")
                .turn_id
                .clone(),
            input_kind: "用户 steer",
        });
    }
    if !matches!(queued.cause, AgentTurnCause::Retry { .. })
        && let Some(steer) = agent
            .steers
            .iter()
            .find(|steer| steer.turn_id != queued.turn_id)
    {
        return Err(CollaborationError::PendingUserSteers {
            agent_id: queued.agent_id.clone(),
            turn_id: steer.turn_id.clone(),
        });
    }
    let definition = agent.definition.clone();
    if !state_turn_belongs_to_root(state, &queued.root_agent_id, &queued.turn_id)
        || state
            .pending_turns
            .iter()
            .any(|turn| turn.turn_id == queued.turn_id)
        || state.active_turns.contains_key(&queued.turn_id)
    {
        return Err(CollaborationError::IdentifierCollision { kind: "Turn" });
    }
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: queued.source_agent_id.clone(),
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        CollaborationEventKind::AgentTurnQueued {
            cause: queued.cause.clone(),
            prompt: queued.prompt.clone(),
        },
    )?;
    set_status(
        state,
        &queued.agent_id,
        CollaborationAgentStatus::WaitingCapacity {
            turn_id: queued.turn_id.clone(),
        },
        EventLink {
            source_agent_id: queued.source_agent_id.clone(),
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        events,
    )?;
    state.pending_turns.push_back(queued);
    Ok(())
}

/// 尚未取得容量的 Turn 被中断后是否允许自动认领 TriggerTurn 继续运行。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WaitingTurnInterruptionMode {
    /// 精确停止单个 Agent，保留既有自动 Followup 语义。
    Standalone,
    /// 用户取消根 Turn，保留动态输入但禁止同一根 Turn 树自动续跑。
    RootTurnCascade,
}

/// 规划一次精确取消；根 Agent 的当前 Turn 会覆盖同一根 Turn 标识下的全部未决 Turn。
pub(super) fn cancel_turn_transition(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<Transition<TurnCancellationDisposition>, CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let definition = agent.definition.clone();
    let status = agent.status.clone();
    let disposition = match &status {
        CollaborationAgentStatus::WaitingCapacity {
            turn_id: current_turn_id,
        }
        | CollaborationAgentStatus::Running {
            turn_id: current_turn_id,
        } if current_turn_id == turn_id => TurnCancellationDisposition::Requested,
        CollaborationAgentStatus::Cancelling {
            turn_id: current_turn_id,
        } if current_turn_id == turn_id => TurnCancellationDisposition::AlreadyRequested,
        CollaborationAgentStatus::WaitingCapacity { .. }
        | CollaborationAgentStatus::Running { .. }
        | CollaborationAgentStatus::Cancelling { .. } => {
            return Err(CollaborationError::TurnMismatch {
                agent_id: agent_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        _ if agent
            .last_turn
            .as_ref()
            .is_some_and(|last_turn| &last_turn.turn_id == turn_id) =>
        {
            return Ok(Transition {
                output: TurnCancellationDisposition::NotRunning,
                events: Vec::new(),
                actions: Vec::new(),
            });
        }
        _ => {
            return Err(CollaborationError::TurnMismatch {
                agent_id: agent_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
    };

    let root_turn_id = match &status {
        CollaborationAgentStatus::WaitingCapacity { .. } => state
            .pending_turns
            .iter()
            .find(|queued| queued.agent_id == *agent_id && queued.turn_id == *turn_id)
            .map(|queued| queued.root_turn_id.clone()),
        CollaborationAgentStatus::Running { .. } | CollaborationAgentStatus::Cancelling { .. } => {
            state
                .active_turns
                .get(turn_id)
                .filter(|active| active.agent_id == *agent_id)
                .map(|active| active.root_turn_id.clone())
        }
        _ => None,
    }
    .ok_or_else(|| CollaborationError::TurnMismatch {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
    })?;

    if definition.depth == AgentDepth::ROOT {
        return cancel_root_turn_tree_transition(
            state,
            &definition.root_agent_id,
            &root_turn_id,
            disposition,
        );
    }

    let mut events = Vec::new();
    let mut actions = Vec::new();
    match status {
        CollaborationAgentStatus::WaitingCapacity { .. } => interrupt_waiting_turn(
            state,
            agent_id,
            turn_id,
            agent_id,
            WaitingTurnInterruptionMode::Standalone,
            &mut events,
            &mut actions,
        )?,
        CollaborationAgentStatus::Running { .. } => {
            let active = state.active_turns.get(turn_id).cloned().ok_or_else(|| {
                CollaborationError::TurnMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                }
            })?;
            set_status(
                state,
                agent_id,
                CollaborationAgentStatus::Cancelling {
                    turn_id: turn_id.clone(),
                },
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                &mut events,
            )?;
            mark_activity(state, agent_id, &mut actions)?;
            actions.push(PostCommitAction::CancelTurn(active.cancellation));
        }
        CollaborationAgentStatus::Cancelling { .. } => {}
        _ => unreachable!("取消状态已在上方完整校验"),
    }
    Ok(Transition {
        output: disposition,
        events,
        actions,
    })
}

/// 原子取消一个根 Turn 树；终态 Agent 和其他根 Turn 的工作均保持原样。
pub(super) fn cancel_root_turn_tree_transition(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
    root_turn_id: &TurnId,
    disposition: TurnCancellationDisposition,
) -> Result<Transition<TurnCancellationDisposition>, CollaborationError> {
    let mut active_turns = state
        .active_turns
        .values()
        .filter(|active| {
            &active.root_agent_id == root_agent_id && &active.root_turn_id == root_turn_id
        })
        .cloned()
        .collect::<Vec<_>>();
    active_turns.sort_by(|left, right| {
        (
            left.agent_id != *root_agent_id,
            &left.agent_id,
            &left.turn_id,
        )
            .cmp(&(
                right.agent_id != *root_agent_id,
                &right.agent_id,
                &right.turn_id,
            ))
    });
    let mut waiting_turns = state
        .pending_turns
        .iter()
        .filter(|queued| {
            &queued.root_agent_id == root_agent_id && &queued.root_turn_id == root_turn_id
        })
        .cloned()
        .collect::<Vec<_>>();
    waiting_turns.sort_by(|left, right| {
        (
            left.agent_id != *root_agent_id,
            &left.agent_id,
            &left.turn_id,
        )
            .cmp(&(
                right.agent_id != *root_agent_id,
                &right.agent_id,
                &right.turn_id,
            ))
    });

    let mut events = Vec::new();
    let mut actions = Vec::new();
    for active in active_turns {
        let status = resident_agent(state, &active.agent_id)?.status.clone();
        match status {
            CollaborationAgentStatus::Running { ref turn_id } if turn_id == &active.turn_id => {
                set_status(
                    state,
                    &active.agent_id,
                    CollaborationAgentStatus::Cancelling {
                        turn_id: active.turn_id.clone(),
                    },
                    EventLink {
                        source_agent_id: root_agent_id.clone(),
                        turn_id: Some(active.turn_id.clone()),
                        parent_turn_id: active.parent_turn_id.clone(),
                        root_turn_id: Some(root_turn_id.clone()),
                    },
                    &mut events,
                )?;
                mark_activity(state, &active.agent_id, &mut actions)?;
                actions.push(PostCommitAction::CancelTurn(active.cancellation.clone()));
            }
            CollaborationAgentStatus::Cancelling { ref turn_id } if turn_id == &active.turn_id => {}
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "根 Turn 级联取消发现活跃账本与 Agent 状态不一致".to_owned(),
                });
            }
        }
        state
            .active_turns
            .get_mut(&active.turn_id)
            .expect("级联取消的活跃 Turn 在上方已校验")
            .cancelled_by_root_turn = true;
        remove_turn_signals(state, &active.agent_id, &active.turn_id);
        unclaim_mailbox_turn(state, &active.turn_id);
    }

    for queued in waiting_turns {
        interrupt_waiting_turn(
            state,
            &queued.agent_id,
            &queued.turn_id,
            root_agent_id,
            WaitingTurnInterruptionMode::RootTurnCascade,
            &mut events,
            &mut actions,
        )?;
    }
    schedule_root_turns(state, &mut events, &mut actions)?;
    Ok(Transition {
        output: disposition,
        events,
        actions,
    })
}

/// 原子释放一个子 Agent Turn 占用的 Coordinator 与根树计数。
pub(super) fn release_turn_capacity(
    state: &mut CoordinatorState,
    active: &ActiveTurn,
) -> Result<(), CollaborationError> {
    if active.global_permit.is_none() {
        return Ok(());
    }
    state.global_in_use =
        state
            .global_in_use
            .checked_sub(1)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "Coordinator 子 Agent 槽位计数下溢".to_owned(),
            })?;
    let root = state.roots.get_mut(&active.root_agent_id).ok_or_else(|| {
        CollaborationError::AgentNotFound {
            agent_id: active.root_agent_id.clone(),
        }
    })?;
    root.in_use =
        root.in_use
            .checked_sub(1)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "根树槽位计数下溢".to_owned(),
            })?;
    Ok(())
}

/// 执行器终态回调在输入 claim 上采用的内部收敛策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TurnCompletionMode {
    /// 普通终态，要求当前 Turn 已确认所有输入 claim。
    Normal,
    /// Turn 派发在执行副作用前被永久拒绝，释放并清理派发 claim。
    DispatchFailed,
    /// 动态输入已写入 Transcript 但 ack 未完成，保留输入 claim 供冷恢复。
    PendingDynamicInput,
    /// 应用退出时收敛 Turn；保留所有 steer 和动态输入 claim，不创建后续 Turn。
    Suspend,
}

/// 规划一个执行器终态，供公开回调和 durable StartTurn 补偿共同复用。
pub(super) fn complete_turn_transition(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    outcome: AgentTurnOutcome,
    mode: TurnCompletionMode,
) -> Result<Transition<TurnCompletionDisposition>, CollaborationError> {
    let Some(active) = state.active_turns.get(turn_id).cloned() else {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    };
    if &active.agent_id != agent_id {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    if state
        .roots
        .get(&active.root_agent_id)
        .is_some_and(|root| root.lifecycle != RecoveredRootLifecycle::Open)
    {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    }
    let Some(agent) = state.agents.get(agent_id) else {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    };
    let was_cancelling = matches!(agent.status, CollaborationAgentStatus::Cancelling { .. });
    let allows_pending_input_claim = !matches!(mode, TurnCompletionMode::Normal);
    let dispatch_failed = matches!(mode, TurnCompletionMode::DispatchFailed);
    if !allows_pending_input_claim
        && !was_cancelling
        && agent
            .mailbox_claim
            .as_ref()
            .is_some_and(|claim| &claim.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            input_kind: "mailbox",
        });
    }
    if !allows_pending_input_claim
        && !was_cancelling
        && agent
            .steer_claim
            .as_ref()
            .is_some_and(|claim| &claim.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            input_kind: "用户 steer",
        });
    }
    if !allows_pending_input_claim
        && !was_cancelling
        && agent.steers.iter().any(|steer| &steer.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingUserSteers {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let effective_outcome = if was_cancelling {
        AgentTurnOutcome::Interrupted
    } else {
        outcome
    };
    state.active_turns.remove(turn_id);
    state.start_outbox.remove(turn_id);
    remove_turn_signals(state, agent_id, turn_id);
    release_turn_capacity(state, &active)?;

    let mut events = Vec::new();
    let mut actions = Vec::new();
    if dispatch_failed {
        actions.push(PostCommitAction::CancelTurn(active.cancellation.clone()));
    }
    let definition = resident_agent(state, agent_id)?.definition.clone();
    let link = EventLink {
        source_agent_id: agent_id.clone(),
        turn_id: Some(turn_id.clone()),
        parent_turn_id: active.parent_turn_id.clone(),
        root_turn_id: Some(active.root_turn_id.clone()),
    };
    push_event(
        state,
        &mut events,
        &definition,
        link.clone(),
        terminal_event_kind(&effective_outcome),
    )?;
    set_status(
        state,
        agent_id,
        outcome_status(turn_id, &effective_outcome),
        link,
        &mut events,
    )?;
    let completed_turn = TurnRecord {
        turn_id: turn_id.clone(),
        cause: active.cause.clone(),
        prompt: active.prompt.clone(),
        parent_turn_id: active.parent_turn_id.clone(),
        root_turn_id: active.root_turn_id.clone(),
        outcome: effective_outcome.clone(),
    };
    let agent = state.agents.get_mut(agent_id).expect("Agent 在上方已校验");
    agent.last_turn = Some(completed_turn.clone());
    if was_cancelling
        && !active.cancelled_by_root_turn
        && !matches!(mode, TurnCompletionMode::Suspend)
    {
        let claimed_through = agent
            .steer_claim
            .as_ref()
            .filter(|claim| &claim.turn_id == turn_id)
            .map(|claim| claim.through_sequence);
        agent.steers.retain(|steer| {
            &steer.turn_id != turn_id
                || claimed_through.is_some_and(|through| steer.sequence <= through)
        });
        agent.steer_bytes = agent.steers.iter().map(UserSteer::payload_bytes).sum();
    }
    mark_activity(state, agent_id, &mut actions)?;

    let tree_closed = state
        .roots
        .get(&active.root_agent_id)
        .is_none_or(|root| root.lifecycle != RecoveredRootLifecycle::Open);
    if !tree_closed {
        if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
            queue_completion_message(
                state,
                CompletionDraft {
                    source_definition: &definition,
                    target_agent_id: &parent_agent_id,
                    outcome: &effective_outcome,
                    related_turn_id: turn_id,
                    parent_turn_id: active.parent_turn_id.clone(),
                    root_turn_id: active.root_turn_id.clone(),
                },
                &mut events,
                &mut actions,
            )?;
        }
        if dispatch_failed {
            let agent = state
                .agents
                .get_mut(agent_id)
                .expect("派发失败 Agent 在上方已校验");
            for mailbox in &mut agent.mailbox {
                if mailbox.claimed_turn_id.as_ref() == Some(turn_id) {
                    mailbox.claimed_turn_id = None;
                }
            }
        } else if matches!(mode, TurnCompletionMode::Normal) && !active.cancelled_by_root_turn {
            claim_followup_after_turn(state, agent_id, &completed_turn, &mut events)?;
        }
        schedule_root_turns(state, &mut events, &mut actions)?;
    }
    Ok(Transition {
        output: TurnCompletionDisposition::Committed,
        events,
        actions,
    })
}

/// 将尚未取得容量的 Turn 直接收敛为中断，并保留与运行中取消一致的后续语义。
pub(super) fn interrupt_waiting_turn(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    source_agent_id: &AgentId,
    mode: WaitingTurnInterruptionMode,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let position = state
        .pending_turns
        .iter()
        .position(|queued| &queued.agent_id == agent_id && &queued.turn_id == turn_id)
        .ok_or_else(|| CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        })?;
    let queued = state
        .pending_turns
        .remove(position)
        .expect("已查找到的等待 Turn 始终存在");
    if matches!(mode, WaitingTurnInterruptionMode::RootTurnCascade) {
        unclaim_mailbox_turn(state, turn_id);
    }
    let agent = resident_agent(state, agent_id)?;
    if agent.status
        != (CollaborationAgentStatus::WaitingCapacity {
            turn_id: turn_id.clone(),
        })
    {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let definition = agent.definition.clone();
    let outcome = AgentTurnOutcome::Interrupted;
    let completed_turn = TurnRecord {
        turn_id: queued.turn_id.clone(),
        cause: queued.cause.clone(),
        prompt: queued.prompt.clone(),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: queued.root_turn_id.clone(),
        outcome: outcome.clone(),
    };
    let link = EventLink {
        source_agent_id: source_agent_id.clone(),
        turn_id: Some(queued.turn_id.clone()),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: Some(queued.root_turn_id.clone()),
    };
    push_event(
        state,
        events,
        &definition,
        link.clone(),
        CollaborationEventKind::AgentTurnInterrupted,
    )?;
    set_status(
        state,
        agent_id,
        CollaborationAgentStatus::Interrupted {
            turn_id: queued.turn_id.clone(),
        },
        link,
        events,
    )?;
    state
        .agents
        .get_mut(agent_id)
        .expect("等待 Turn 的 Agent 在上方已校验")
        .last_turn = Some(completed_turn.clone());
    mark_activity(state, agent_id, actions)?;

    let tree_closed = state
        .roots
        .get(&queued.root_agent_id)
        .is_none_or(|root| root.lifecycle != RecoveredRootLifecycle::Open);
    if !tree_closed {
        if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
            queue_completion_message(
                state,
                CompletionDraft {
                    source_definition: &definition,
                    target_agent_id: &parent_agent_id,
                    outcome: &outcome,
                    related_turn_id: &queued.turn_id,
                    parent_turn_id: queued.parent_turn_id,
                    root_turn_id: queued.root_turn_id,
                },
                events,
                actions,
            )?;
        }
        if matches!(mode, WaitingTurnInterruptionMode::Standalone) {
            claim_followup_after_turn(state, agent_id, &completed_turn, events)?;
            schedule_root_turns(state, events, actions)?;
        }
    }
    Ok(())
}

/// 返回暂停时的 Agent 稳定顺序，保证子 Agent 先于根 Agent 收敛完成通知。
pub(super) fn suspend_agent_order(
    state: &CoordinatorState,
    left_agent_id: &AgentId,
    right_agent_id: &AgentId,
) -> std::cmp::Ordering {
    let left = state
        .agents
        .get(left_agent_id)
        .map(|agent| (agent.definition.depth, agent.definition.path.clone()));
    let right = state
        .agents
        .get(right_agent_id)
        .map(|agent| (agent.definition.depth, agent.definition.path.clone()));
    match (left, right) {
        (Some((left_depth, left_path)), Some((right_depth, right_path))) => right_depth
            .cmp(&left_depth)
            .then_with(|| left_path.cmp(&right_path)),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, None) => left_agent_id.cmp(right_agent_id),
    }
}

/// 释放指定 Turn 对 TriggerTurn mailbox 的临时归属，保留正文和动态输入 claim。
pub(super) fn unclaim_mailbox_turn(state: &mut CoordinatorState, turn_id: &TurnId) {
    for agent in state.agents.values_mut() {
        for entry in &mut agent.mailbox {
            if entry.claimed_turn_id.as_ref() == Some(turn_id) {
                entry.claimed_turn_id = None;
            }
        }
    }
}

/// 在应用暂停期间将尚未预约容量的 Turn 收敛为 Interrupted，不创建 Followup。
pub(super) fn interrupt_waiting_turn_for_suspend(
    state: &mut CoordinatorState,
    queued: &QueuedTurn,
    source_agent_id: &AgentId,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let position = state
        .pending_turns
        .iter()
        .position(|current| current.turn_id == queued.turn_id)
        .ok_or_else(|| CollaborationError::TurnMismatch {
            agent_id: queued.agent_id.clone(),
            turn_id: queued.turn_id.clone(),
        })?;
    let removed = state
        .pending_turns
        .remove(position)
        .expect("暂停时已查找到的等待 Turn 始终存在");
    if removed.agent_id != queued.agent_id
        || resident_agent(state, &queued.agent_id)?.status
            != (CollaborationAgentStatus::WaitingCapacity {
                turn_id: queued.turn_id.clone(),
            })
    {
        return Err(CollaborationError::TurnMismatch {
            agent_id: queued.agent_id.clone(),
            turn_id: queued.turn_id.clone(),
        });
    }
    let definition = resident_agent(state, &queued.agent_id)?.definition.clone();
    let outcome = AgentTurnOutcome::Interrupted;
    let completed_turn = TurnRecord {
        turn_id: queued.turn_id.clone(),
        cause: queued.cause.clone(),
        prompt: queued.prompt.clone(),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: queued.root_turn_id.clone(),
        outcome: outcome.clone(),
    };
    let link = EventLink {
        source_agent_id: source_agent_id.clone(),
        turn_id: Some(queued.turn_id.clone()),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: Some(queued.root_turn_id.clone()),
    };
    push_event(
        state,
        events,
        &definition,
        link.clone(),
        CollaborationEventKind::AgentTurnInterrupted,
    )?;
    set_status(
        state,
        &queued.agent_id,
        CollaborationAgentStatus::Interrupted {
            turn_id: queued.turn_id.clone(),
        },
        link,
        events,
    )?;
    state
        .agents
        .get_mut(&queued.agent_id)
        .expect("暂停时等待 Turn 的 Agent 在上方已校验")
        .last_turn = Some(completed_turn);
    mark_activity(state, &queued.agent_id, actions)?;
    if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
        queue_completion_message(
            state,
            CompletionDraft {
                source_definition: &definition,
                target_agent_id: &parent_agent_id,
                outcome: &outcome,
                related_turn_id: &queued.turn_id,
                parent_turn_id: queued.parent_turn_id.clone(),
                root_turn_id: queued.root_turn_id.clone(),
            },
            events,
            actions,
        )?;
    }
    Ok(())
}

/// 根 Turn 不消耗子 Agent 槽位，因此只要根树开放就立即调度。
pub(super) fn schedule_root_turns(
    state: &mut CoordinatorState,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    while let Some(position) = state.pending_turns.iter().position(|queued| {
        state
            .agents
            .get(&queued.agent_id)
            .is_some_and(|agent| agent.definition.depth == AgentDepth::ROOT)
            && state.roots.get(&queued.root_agent_id).is_some_and(|root| {
                root.lifecycle == RecoveredRootLifecycle::Open && !root.suspended
            })
    }) {
        schedule_turn_at_position(state, position, None, events, actions)?;
    }
    Ok(())
}

/// 将一个已选中的等待 Turn 提升为活跃 Turn。
pub(super) fn schedule_turn_at_position(
    state: &mut CoordinatorState,
    position: usize,
    global_permit: Option<Arc<GlobalTurnPermit>>,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<TurnId, CollaborationError> {
    let queued = state
        .pending_turns
        .remove(position)
        .expect("已查找到的队列位置始终存在");
    let agent = resident_agent(state, &queued.agent_id)?;
    if agent.status
        != (CollaborationAgentStatus::WaitingCapacity {
            turn_id: queued.turn_id.clone(),
        })
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "待调度 Turn 与 Agent 状态不一致".to_owned(),
        });
    }
    let definition = agent.definition.clone();
    let is_child = definition.depth == AgentDepth::CHILD;
    if is_child != global_permit.is_some() {
        return Err(CollaborationError::InvalidRecovery {
            message: "根 Turn 与子 Agent 全局槽位归属不一致".to_owned(),
        });
    }
    if is_child {
        state.global_in_use = state
            .global_in_use
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        let root = state
            .roots
            .get_mut(&queued.root_agent_id)
            .expect("待调度 Turn 的根树在上方已校验");
        if root.in_use >= root.turn_limit {
            return Err(CollaborationError::InvalidRecovery {
                message: "子 Agent Turn 超过根树并发上限".to_owned(),
            });
        }
        root.in_use = root
            .in_use
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
    }
    let cancellation = TurnCancellation::new();
    let active = ActiveTurn {
        agent_id: queued.agent_id.clone(),
        source_agent_id: queued.source_agent_id.clone(),
        root_agent_id: queued.root_agent_id.clone(),
        turn_id: queued.turn_id.clone(),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: queued.root_turn_id.clone(),
        cause: queued.cause.clone(),
        prompt: queued.prompt.clone(),
        plan_guard: queued.plan_guard,
        cancellation: cancellation.clone(),
        cancelled_by_root_turn: false,
        global_permit,
    };
    state
        .active_turns
        .insert(queued.turn_id.clone(), active.clone());
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: queued.source_agent_id.clone(),
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        CollaborationEventKind::AgentTurnStarted {
            cause: queued.cause.clone(),
        },
    )?;
    set_status(
        state,
        &queued.agent_id,
        CollaborationAgentStatus::Running {
            turn_id: queued.turn_id.clone(),
        },
        EventLink {
            source_agent_id: queued.source_agent_id,
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        events,
    )?;
    let launch = AgentTurnLaunch {
        agent: definition.clone(),
        turn_id: queued.turn_id.clone(),
        parent_turn_id: queued.parent_turn_id,
        root_turn_id: queued.root_turn_id,
        cause: queued.cause,
        prompt: queued.prompt,
        cancellation,
        plan_guard: queued.plan_guard,
        capabilities: AgentCapabilities {
            can_spawn_agent: definition.depth.can_spawn_child(),
        },
    };
    state
        .start_outbox
        .insert(launch.turn_id.clone(), launch.clone());
    actions.push(PostCommitAction::StartTurn(Box::new(launch)));
    Ok(queued.turn_id)
}

/// 递增 Agent 活动版本并安排提交后广播。
pub(super) fn mark_activity(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let agent =
        state
            .agents
            .get_mut(agent_id)
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: agent_id.clone(),
            })?;
    agent.activity_version = agent
        .activity_version
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    actions.push(PostCommitAction::NotifyWaiters {
        sender: agent.activity_sender.clone(),
        version: agent.activity_version,
    });
    Ok(())
}

/// 将同一 Turn 的安全边界信号合并到最新活动版本并登记可重试 outbox。
pub(super) fn queue_turn_signal(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    kind: AgentTurnSignalKind,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    if agent.status.active_turn_id() != Some(turn_id) {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let signal = AgentTurnSignal {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind,
        activity_version: agent.activity_version,
    };
    state
        .signal_outbox
        .insert(AgentTurnSignalKey::from_signal(&signal), signal.clone());
    actions.push(PostCommitAction::SignalTurn(signal));
    Ok(())
}

/// 当前 Turn 已自行消费全部输入时作废尚未送达的冗余 SignalTurn outbox。
pub(super) fn invalidate_quiet_turn_signal(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let mailbox_claimed_through = agent
        .mailbox_claim
        .as_ref()
        .map_or(0, |claim| claim.through_sequence);
    let mailbox_empty = !agent
        .mailbox
        .iter()
        .any(|entry| entry.message.sequence > mailbox_claimed_through);
    let steer_claimed_through = agent
        .steer_claim
        .as_ref()
        .filter(|claim| &claim.turn_id == turn_id)
        .map_or(0, |claim| claim.through_sequence);
    let steer_empty = !agent
        .steers
        .iter()
        .any(|steer| &steer.turn_id == turn_id && steer.sequence > steer_claimed_through);
    let mailbox_key = AgentTurnSignalKey {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind: AgentTurnSignalKind::MailboxAvailable,
    };
    if mailbox_empty {
        state.signal_outbox.remove(&mailbox_key);
    }
    let steer_key = AgentTurnSignalKey {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind: AgentTurnSignalKind::UserSteer,
    };
    if steer_empty {
        state.signal_outbox.remove(&steer_key);
    }
    Ok(())
}

/// 作废一个 Agent Turn 的全部独立信号类型，不影响其他 Agent 或 Turn。
pub(super) fn remove_turn_signals(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) {
    state
        .signal_outbox
        .retain(|key, _signal| &key.agent_id != agent_id || &key.turn_id != turn_id);
}

/// 为目标 mailbox 分配单调序号，持久入队并唤醒活跃等待者。
pub(super) fn queue_mailbox_message(
    state: &mut CoordinatorState,
    draft: MailboxDraft,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<MailboxMessage, CollaborationError> {
    if !matches!(draft.kind, MailboxMessageKind::AgentMessage) {
        return Err(CollaborationError::InvalidRecovery {
            message: "普通 mailbox 入口不能写入系统完成通知".to_owned(),
        });
    }
    validate_required_text(&draft.content, "mailbox 消息正文")?;
    if draft.message_id.as_str().len() > MAX_PROFILE_FIELD_BYTES {
        return Err(CollaborationError::TextTooLarge {
            field: "mailbox 消息标识",
            maximum_bytes: MAX_PROFILE_FIELD_BYTES,
        });
    }
    if state.agents.values().any(|agent| {
        agent
            .mailbox
            .iter()
            .any(|entry| entry.message.message_id == draft.message_id)
    }) {
        return Err(CollaborationError::IdentifierCollision {
            kind: "mailbox 消息",
        });
    }
    let target = resident_agent(state, &draft.target_agent_id)?;
    if !target.status.can_receive_messages() {
        return Err(CollaborationError::TargetStopped {
            agent_id: draft.target_agent_id,
        });
    }
    let user_count = target
        .mailbox
        .len()
        .checked_sub(target.completion_count)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "Agent 完成通知计数超过 mailbox 总数".to_owned(),
        })?;
    let user_bytes = target
        .mailbox_bytes
        .checked_sub(target.completion_bytes)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "Agent 完成通知字节超过 mailbox 总字节".to_owned(),
        })?;
    if user_count >= MAX_MAILBOX_MESSAGES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单 Agent 未消费普通 mailbox 消息数量",
            maximum: MAX_MAILBOX_MESSAGES_PER_AGENT,
        });
    }
    let next_user_bytes = user_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_user_bytes > MAX_MAILBOX_BYTES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单 Agent 未消费普通 mailbox 正文字节数",
            maximum: MAX_MAILBOX_BYTES_PER_AGENT,
        });
    }
    let root = state
        .roots
        .get(&target.definition.root_agent_id)
        .expect("mailbox 目标所属根树应存在");
    let root_user_count = root
        .mailbox_count
        .checked_sub(root.completion_count)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "根树完成通知计数超过 mailbox 总数".to_owned(),
        })?;
    let root_user_bytes = root
        .mailbox_bytes
        .checked_sub(root.completion_bytes)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "根树完成通知字节超过 mailbox 总字节".to_owned(),
        })?;
    if root_user_count >= MAX_USER_MAILBOX_MESSAGES_PER_TREE {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单棵根树未消费普通 mailbox 消息数量",
            maximum: MAX_USER_MAILBOX_MESSAGES_PER_TREE,
        });
    }
    let next_root_user_bytes = root_user_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_root_user_bytes > MAX_USER_MAILBOX_BYTES_PER_TREE {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单棵根树未消费普通 mailbox 正文字节数",
            maximum: MAX_USER_MAILBOX_BYTES_PER_TREE,
        });
    }
    let next_mailbox_bytes = target
        .mailbox_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    let definition = target.definition.clone();
    let root_agent_id = definition.root_agent_id.clone();
    let sequence = target.next_mailbox_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let message = MailboxMessage {
        message_id: draft.message_id,
        sequence,
        source_agent_id: draft.source_agent_id.clone(),
        source_agent_path: draft.source_agent_path,
        source_plan_guard: draft.source_plan_guard,
        target_agent_id: draft.target_agent_id.clone(),
        delivery: draft.delivery,
        kind: draft.kind,
        content: draft.content,
        related_turn_id: draft.related_turn_id.clone(),
        parent_turn_id: draft.parent_turn_id.clone(),
        root_turn_id: draft.root_turn_id.clone(),
    };
    let target = state
        .agents
        .get_mut(&draft.target_agent_id)
        .expect("mailbox 目标在上方已校验");
    target.next_mailbox_sequence = next_sequence;
    target.mailbox_bytes = next_mailbox_bytes;
    target.mailbox.push_back(MailboxEntry {
        message: message.clone(),
        initial_triggered_turn_id: draft.initial_triggered_turn_id,
        claimed_turn_id: draft.claimed_turn_id,
    });
    let root = state
        .roots
        .get_mut(&root_agent_id)
        .expect("mailbox 目标所属根树应存在");
    root.mailbox_count = root
        .mailbox_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.mailbox_bytes = root
        .mailbox_bytes
        .checked_add(message.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: draft.source_agent_id,
            turn_id: draft.related_turn_id,
            parent_turn_id: draft.parent_turn_id,
            root_turn_id: draft.root_turn_id,
        },
        CollaborationEventKind::AgentMessageQueued {
            message: message.clone(),
        },
    )?;
    mark_activity(state, &draft.target_agent_id, actions)?;
    Ok(message)
}

/// 在 UTF-8 边界内生成固定上限的可展示文本。
pub(super) fn bounded_text(value: &str, maximum: usize) -> String {
    const SUFFIX: &str = "\n\n[内容已截断，完整结果保留在子 Agent 终态中]";
    bounded_utf8_with_suffix(value, maximum, SUFFIX)
}

/// 将完整 Turn 终态缩减为 mailbox 使用的有界通知终态。
pub(super) fn bounded_completion_outcome(outcome: &AgentTurnOutcome) -> AgentTurnOutcome {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => AgentTurnOutcome::Completed {
            final_message: final_message
                .as_deref()
                .map(|message| bounded_text(message, MAX_COMPLETION_NOTIFICATION_BYTES / 2)),
        },
        AgentTurnOutcome::Interrupted => AgentTurnOutcome::Interrupted,
        AgentTurnOutcome::Failed { message } => AgentTurnOutcome::Failed {
            message: bounded_text(message, MAX_COMPLETION_NOTIFICATION_BYTES / 2),
        },
    }
}

/// 从不可复用 TurnId 派生不依赖可故障 ID 生成器的系统消息标识。
pub(super) fn completion_message_id(turn_id: &TurnId, sequence: u64) -> MailboxMessageId {
    let digest = Sha256::digest(turn_id.as_str().as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("写入 String 不会失败");
    }
    MailboxMessageId(format!("completion-{hex}-{sequence}"))
}

/// 为子 Agent 终态写入不会挤占普通消息配额的有界、可恢复系统通知。
pub(super) fn queue_completion_message(
    state: &mut CoordinatorState,
    draft: CompletionDraft<'_>,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<MailboxMessage, CollaborationError> {
    let CompletionDraft {
        source_definition,
        target_agent_id,
        outcome,
        related_turn_id,
        parent_turn_id,
        root_turn_id,
    } = draft;
    let target = resident_agent(state, target_agent_id)?;
    if !target.status.can_receive_messages() {
        return Err(CollaborationError::TargetStopped {
            agent_id: target_agent_id.clone(),
        });
    }
    let running_turn_id = match &target.status {
        CollaborationAgentStatus::Running { turn_id } => Some(turn_id.clone()),
        _ => None,
    };
    let root_agent_id = target.definition.root_agent_id.clone();
    let target_definition = target.definition.clone();
    let claimed_through_sequence = target
        .mailbox_claim
        .as_ref()
        .map_or(0, |claim| claim.through_sequence);
    let mut superseded_position = target.mailbox.iter().position(|entry| {
        entry.message.sequence > claimed_through_sequence
            && entry.message.source_agent_id == source_definition.agent_id
            && matches!(
                entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
    });
    if superseded_position.is_none()
        && state
            .roots
            .get(&root_agent_id)
            .is_some_and(|root| root.completion_count >= MAX_COMPLETION_MESSAGES_PER_TREE)
    {
        superseded_position = target.mailbox.iter().position(|entry| {
            entry.message.sequence > claimed_through_sequence
                && matches!(
                    entry.message.kind,
                    MailboxMessageKind::ChildTurnFinished { .. }
                )
        });
    }
    if let Some(position) = superseded_position {
        let removed = state
            .agents
            .get_mut(target_agent_id)
            .expect("完成通知目标在上方已校验")
            .mailbox
            .remove(position)
            .expect("完成通知位置在上方已查找");
        let removed_bytes = removed.message.content.len();
        let target = state
            .agents
            .get_mut(target_agent_id)
            .expect("完成通知目标在上方已校验");
        target.mailbox_bytes =
            target
                .mailbox_bytes
                .checked_sub(removed_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "替代完成通知时 Agent mailbox 字节下溢".to_owned(),
                })?;
        target.completion_count = target.completion_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时 Agent 完成计数下溢".to_owned(),
            }
        })?;
        target.completion_bytes = target
            .completion_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时 Agent 完成字节下溢".to_owned(),
            })?;
        let root = state
            .roots
            .get_mut(&root_agent_id)
            .expect("完成通知目标所属根树应存在");
        root.mailbox_count = root.mailbox_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树 mailbox 计数下溢".to_owned(),
            }
        })?;
        root.mailbox_bytes = root
            .mailbox_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树 mailbox 字节下溢".to_owned(),
            })?;
        root.completion_count = root.completion_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树完成计数下溢".to_owned(),
            }
        })?;
        root.completion_bytes = root
            .completion_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树完成字节下溢".to_owned(),
            })?;
        push_event(
            state,
            events,
            &target_definition,
            EventLink {
                source_agent_id: source_definition.agent_id.clone(),
                turn_id: Some(related_turn_id.clone()),
                parent_turn_id: parent_turn_id.clone(),
                root_turn_id: Some(root_turn_id.clone()),
            },
            CollaborationEventKind::AgentCompletionNotificationSuperseded {
                message_id: removed.message.message_id,
            },
        )?;
    }

    let notification_outcome = bounded_completion_outcome(outcome);
    let content = bounded_text(
        &child_completion_content(&source_definition.path, &notification_outcome),
        MAX_COMPLETION_NOTIFICATION_BYTES,
    );
    let target = resident_agent(state, target_agent_id)?;
    let sequence = target.next_mailbox_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let mut message_id = completion_message_id(related_turn_id, sequence);
    let mut collision_suffix = 0u64;
    while state.agents.values().any(|agent| {
        agent
            .mailbox
            .iter()
            .any(|entry| entry.message.message_id == message_id)
    }) {
        collision_suffix = collision_suffix
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        message_id = MailboxMessageId(format!(
            "{}-{collision_suffix}",
            completion_message_id(related_turn_id, sequence).as_str()
        ));
    }
    let message = MailboxMessage {
        message_id,
        sequence,
        source_agent_id: source_definition.agent_id.clone(),
        source_agent_path: source_definition.path.clone(),
        source_plan_guard: source_definition.profile.plan_guard,
        target_agent_id: target_agent_id.clone(),
        delivery: MailboxDelivery::QueueOnly,
        kind: MailboxMessageKind::ChildTurnFinished {
            outcome: notification_outcome,
        },
        content,
        related_turn_id: Some(related_turn_id.clone()),
        parent_turn_id: parent_turn_id.clone(),
        root_turn_id: Some(root_turn_id.clone()),
    };
    let message_bytes = message.content.len();
    let target = state
        .agents
        .get_mut(target_agent_id)
        .expect("完成通知目标在上方已校验");
    target.next_mailbox_sequence = next_sequence;
    target.mailbox_bytes = target
        .mailbox_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.completion_count = target
        .completion_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.completion_bytes = target
        .completion_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.mailbox.push_back(MailboxEntry {
        message: message.clone(),
        initial_triggered_turn_id: None,
        claimed_turn_id: None,
    });
    let root = state
        .roots
        .get_mut(&root_agent_id)
        .expect("完成通知目标所属根树应存在");
    root.mailbox_count = root
        .mailbox_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.mailbox_bytes = root
        .mailbox_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.completion_count = root
        .completion_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.completion_bytes = root
        .completion_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    push_event(
        state,
        events,
        &target_definition,
        EventLink {
            source_agent_id: source_definition.agent_id.clone(),
            turn_id: Some(related_turn_id.clone()),
            parent_turn_id,
            root_turn_id: Some(root_turn_id),
        },
        CollaborationEventKind::AgentMessageQueued {
            message: message.clone(),
        },
    )?;
    mark_activity(state, target_agent_id, actions)?;
    if let Some(turn_id) = running_turn_id {
        queue_turn_signal(
            state,
            target_agent_id,
            &turn_id,
            AgentTurnSignalKind::MailboxAvailable,
            actions,
        )?;
    }
    Ok(message)
}

/// 当正在运行的 Turn 结束时，为未在安全边界消费的 Followup 创建一个后续 Turn。
pub(super) fn claim_followup_after_turn(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    completed: &TurnRecord,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let root_agent_id = agent.definition.root_agent_id.clone();
    let Some(first) = agent
        .mailbox
        .iter()
        .find(|entry| {
            entry.message.delivery == MailboxDelivery::TriggerTurn
                && entry
                    .claimed_turn_id
                    .as_ref()
                    .is_none_or(|turn_id| turn_id == &completed.turn_id)
        })
        .cloned()
    else {
        return Ok(());
    };
    let plan_guard = agent
        .mailbox
        .iter()
        .filter(|entry| matches!(entry.message.kind, MailboxMessageKind::AgentMessage))
        .fold(agent.definition.profile.plan_guard, |guard, entry| {
            strictest_plan_guard(guard, entry.message.source_plan_guard)
        });
    let next_turn_id = allocate_turn_id(state, &root_agent_id)?;
    let agent = state
        .agents
        .get_mut(agent_id)
        .expect("Followup 目标在上方已校验");
    for entry in &mut agent.mailbox {
        if entry.message.delivery == MailboxDelivery::TriggerTurn
            && entry
                .claimed_turn_id
                .as_ref()
                .is_none_or(|turn_id| turn_id == &completed.turn_id)
        {
            entry.claimed_turn_id = Some(next_turn_id.clone());
        }
    }
    rebind_pending_inputs(agent, &completed.turn_id, &next_turn_id);
    let queued = QueuedTurn {
        agent_id: agent_id.clone(),
        root_agent_id,
        turn_id: next_turn_id,
        source_agent_id: first.message.source_agent_id.clone(),
        parent_turn_id: first.message.parent_turn_id.clone(),
        root_turn_id: first.message.root_turn_id.clone().ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "Followup mailbox 缺少原始根 Turn 因果".to_owned(),
            }
        })?,
        cause: AgentTurnCause::Followup {
            message_id: first.message.message_id,
        },
        prompt: None,
        plan_guard,
    };
    queue_turn(state, queued, events)
}

/// 将执行器终态转换为 Agent 空闲状态。
pub(super) fn outcome_status(
    turn_id: &TurnId,
    outcome: &AgentTurnOutcome,
) -> CollaborationAgentStatus {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => CollaborationAgentStatus::Completed {
            turn_id: turn_id.clone(),
            final_message: final_message.clone(),
        },
        AgentTurnOutcome::Interrupted => CollaborationAgentStatus::Interrupted {
            turn_id: turn_id.clone(),
        },
        AgentTurnOutcome::Failed { message } => CollaborationAgentStatus::Failed {
            turn_id: turn_id.clone(),
            message: message.clone(),
        },
    }
}

/// 将执行器终态转换为与 mailbox 入队分离的 Turn 终态事件。
pub(super) fn terminal_event_kind(outcome: &AgentTurnOutcome) -> CollaborationEventKind {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            CollaborationEventKind::AgentTurnCompleted {
                final_message: final_message.clone(),
            }
        }
        AgentTurnOutcome::Interrupted => CollaborationEventKind::AgentTurnInterrupted,
        AgentTurnOutcome::Failed { message } => CollaborationEventKind::AgentTurnFailed {
            message: message.clone(),
        },
    }
}

/// 为子 Agent 终态 mailbox 生成稳定且简短的完整文本。
pub(super) fn child_completion_content(path: &AgentPath, outcome: &AgentTurnOutcome) -> String {
    match outcome {
        AgentTurnOutcome::Completed {
            final_message: Some(message),
        } => format!("子 Agent {path} 已完成\n\n{message}"),
        AgentTurnOutcome::Completed {
            final_message: None,
        } => format!("子 Agent {path} 已完成"),
        AgentTurnOutcome::Interrupted => format!("子 Agent {path} 已中断"),
        AgentTurnOutcome::Failed { message } => {
            format!("子 Agent {path} 已失败\n\n{message}")
        }
    }
}
