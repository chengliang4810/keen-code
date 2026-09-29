//! 协作领域的输入校验、容量不变量与恢复树校验。

use super::*;

/// 校验可以稳定嵌入 AgentPath 的子 Agent 名称。
pub(super) fn validate_child_name(name: &str) -> Result<(), AgentPathError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(AgentPathError::InvalidChildName {
            name: name.to_owned(),
        })
    }
}

/// 校验必须存在且受字节边界保护的协作文本。
pub(super) fn validate_required_text(
    value: &str,
    field: &'static str,
) -> Result<(), CollaborationError> {
    if value.trim().is_empty() {
        return Err(CollaborationError::EmptyMessage);
    }
    validate_optional_text(value, field)
}

/// 校验允许为空但不能突破内存边界的协作文本。
pub(super) fn validate_optional_text(
    value: &str,
    field: &'static str,
) -> Result<(), CollaborationError> {
    if value.len() > MAX_COLLABORATION_TEXT_BYTES {
        return Err(CollaborationError::TextTooLarge {
            field,
            maximum_bytes: MAX_COLLABORATION_TEXT_BYTES,
        });
    }
    Ok(())
}

/// 校验子 Agent 生命周期内稳定、可向同树 Agent 展示的职责摘要。
pub(super) fn validate_agent_assignment(value: &str) -> Result<(), CollaborationError> {
    if value.trim().is_empty()
        || value.len() > MAX_AGENT_ASSIGNMENT_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(CollaborationError::InvalidAssignment);
    }
    Ok(())
}

/// 在 UTF-8 字符边界内截断文本，并确保固定后缀也计入最终字节上限。
pub(super) fn bounded_utf8_with_suffix(value: &str, maximum: usize, suffix: &str) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    if maximum == 0 {
        return String::new();
    }
    let suffix = if suffix.len() <= maximum {
        suffix
    } else {
        let mut suffix_boundary = maximum;
        while suffix_boundary > 0 && !suffix.is_char_boundary(suffix_boundary) {
            suffix_boundary -= 1;
        }
        &suffix[..suffix_boundary]
    };
    let payload_maximum = maximum.saturating_sub(suffix.len());
    let mut payload_boundary = payload_maximum.min(value.len());
    while payload_boundary > 0 && !value.is_char_boundary(payload_boundary) {
        payload_boundary -= 1;
    }
    let mut bounded = String::with_capacity(payload_boundary.saturating_add(suffix.len()));
    bounded.push_str(&value[..payload_boundary]);
    bounded.push_str(suffix);
    bounded
}

/// 校验上下文继承数量，避免恢复或工具输入请求无界历史。
pub(super) fn validate_context_inheritance(
    inheritance: &ContextInheritance,
) -> Result<(), CollaborationError> {
    if let ContextInheritance::RecentTurns { count } = inheritance
        && (*count == 0 || *count > MAX_RECENT_TURNS)
    {
        return Err(CollaborationError::InvalidContextInheritance);
    }
    Ok(())
}

/// 校验 spawn 时已经冻结的规范 Provider 中立消息，禁止延迟到容量调度时读取父 Transcript。
pub(super) fn validate_context_snapshot(
    inheritance: &ContextInheritance,
    snapshot: &[String],
) -> Result<(), CollaborationError> {
    if matches!(inheritance, ContextInheritance::None) && !snapshot.is_empty() {
        return Err(CollaborationError::InvalidContextInheritance);
    }
    if snapshot.len() > MAX_CONTEXT_SNAPSHOT_MESSAGES {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 冻结上下文消息数量",
            maximum: MAX_CONTEXT_SNAPSHOT_MESSAGES,
        });
    }
    let mut total_bytes = 0_usize;
    for encoded in snapshot {
        total_bytes = total_bytes.checked_add(encoded.len()).ok_or(
            CollaborationError::ResourceLimitExceeded {
                resource: "Agent 冻结上下文字节数",
                maximum: MAX_CONTEXT_SNAPSHOT_BYTES,
            },
        )?;
        if encoded.is_empty() || total_bytes > MAX_CONTEXT_SNAPSHOT_BYTES {
            return Err(CollaborationError::ResourceLimitExceeded {
                resource: "Agent 冻结上下文字节数",
                maximum: MAX_CONTEXT_SNAPSHOT_BYTES,
            });
        }
        let message = serde_json::from_str::<Message>(encoded).map_err(|_| {
            CollaborationError::InvalidAgentProfile {
                message: "冻结上下文不是 Provider 中立消息",
            }
        })?;
        message
            .validate()
            .map_err(|_| CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息无效",
            })?;
        let canonical = serde_json::to_string(&message).map_err(|_| {
            CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息无法规范编码",
            }
        })?;
        if canonical != *encoded {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息不是规范 JSON",
            });
        }
    }
    Ok(())
}

/// 校验 spawn 前冻结的扩展 Agent 模板，不信任冷恢复文件中的路径与资源上限。
pub(super) fn validate_agent_template_snapshot(
    template: &AgentTemplateSnapshot,
) -> Result<(), CollaborationError> {
    if template.name.trim().is_empty()
        || template.name.trim() != template.name
        || template.name.len() > MAX_PROFILE_FIELD_BYTES
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "Agent 模板名称为空、包含首尾空白或过长",
        });
    }
    validate_required_text(&template.system_prompt, "Agent 模板系统提示")?;
    if template
        .max_turns
        .is_some_and(|turns| turns == 0 || turns > MAX_AGENT_TEMPLATE_TURNS)
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "Agent 模板轮次上限无效",
        });
    }
    if template.allowed_write_dirs.len() > MAX_AGENT_TEMPLATE_WRITE_DIRS {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 模板额外写目录数量",
            maximum: MAX_AGENT_TEMPLATE_WRITE_DIRS,
        });
    }
    let mut directories = HashSet::new();
    for directory in &template.allowed_write_dirs {
        let Some(text) = directory.to_str() else {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "Agent 模板额外写目录必须是 UTF-8 相对路径",
            });
        };
        if text.is_empty()
            || text.len() > MAX_PROFILE_FIELD_BYTES
            || text.contains(':')
            || directory.is_absolute()
            || directory
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || !directories.insert(directory)
        {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "Agent 模板额外写目录为空、越界、过长或重复",
            });
        }
    }
    Ok(())
}

/// 校验 Agent 的模型、Plan、目录和冻结工具快照。
pub(super) fn validate_agent_profile(profile: &AgentProfile) -> Result<(), CollaborationError> {
    if profile.model.trim().is_empty() || profile.model.len() > MAX_PROFILE_FIELD_BYTES {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "模型标识为空或过长",
        });
    }
    if profile
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| effort.trim().is_empty() || effort.len() > MAX_PROFILE_FIELD_BYTES)
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "推理强度为空或过长",
        });
    }
    if !profile.cwd.is_absolute() {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "工作目录必须是绝对路径",
        });
    }
    if profile.tool_snapshot.len() > MAX_TOOL_SNAPSHOT_ENTRIES {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 工具快照数量",
            maximum: MAX_TOOL_SNAPSHOT_ENTRIES,
        });
    }
    let mut names = HashSet::new();
    for name in &profile.tool_snapshot {
        if name.trim().is_empty()
            || name.len() > MAX_PROFILE_FIELD_BYTES
            || !names.insert(name.as_str())
        {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "工具快照包含空值、超长名称或重复名称",
            });
        }
    }
    Ok(())
}

/// 将子 Agent 请求收紧到父 Agent 已生效的计划只读边界。
pub(super) fn constrain_child_profile(
    parent: &AgentProfile,
    parent_turn_plan_guard: PlanGuard,
    requested: &AgentProfile,
) -> AgentProfile {
    let mut effective = requested.clone();
    effective.plan_guard = effective_child_plan_guard(
        parent.plan_guard,
        parent_turn_plan_guard,
        requested.plan_guard,
    );
    effective
}

/// 合并父 Agent 配置、来源 Turn 和目标请求中的最严格 Plan 守卫。
pub(super) fn effective_child_plan_guard(
    parent_plan_guard: PlanGuard,
    parent_turn_plan_guard: PlanGuard,
    requested_plan_guard: PlanGuard,
) -> PlanGuard {
    if matches!(parent_plan_guard.state(), PlanGuardState::ReadOnly)
        || matches!(parent_turn_plan_guard.state(), PlanGuardState::ReadOnly)
        || matches!(requested_plan_guard.state(), PlanGuardState::ReadOnly)
    {
        PlanGuard::read_only()
    } else {
        PlanGuard::inactive()
    }
}

/// 合并两个来源，只要任一方只读就保持只读。
pub(super) fn strictest_plan_guard(left: PlanGuard, right: PlanGuard) -> PlanGuard {
    if matches!(left.state(), PlanGuardState::ReadOnly)
        || matches!(right.state(), PlanGuardState::ReadOnly)
    {
        PlanGuard::read_only()
    } else {
        PlanGuard::inactive()
    }
}

/// 校验执行器终态文本，避免失败补偿或模型输出绕过正文边界。
pub(super) fn validate_turn_outcome(outcome: &AgentTurnOutcome) -> Result<(), CollaborationError> {
    match outcome {
        AgentTurnOutcome::Completed {
            final_message: Some(message),
        } => validate_optional_text(message, "Agent 最终文本"),
        AgentTurnOutcome::Failed { message } => validate_required_text(message, "Agent 失败说明"),
        AgentTurnOutcome::Completed {
            final_message: None,
        }
        | AgentTurnOutcome::Interrupted => Ok(()),
    }
}

/// 校验活跃 Turn 与 Coordinator/根树子 Agent 槽位投影严格一致。
pub(super) fn validate_turn_capacity_invariants(
    state: &CoordinatorState,
) -> Result<(), CollaborationError> {
    let mut expected_by_root = HashMap::<AgentId, usize>::new();
    let mut expected_global = 0usize;
    for (turn_id, active) in &state.active_turns {
        let Some(agent) = state.agents.get(&active.agent_id) else {
            return Err(CollaborationError::InvalidRecovery {
                message: "活跃 Turn 引用了未知 Agent".to_owned(),
            });
        };
        if turn_id != &active.turn_id
            || agent.definition.root_agent_id != active.root_agent_id
            || !state.roots.contains_key(&active.root_agent_id)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "活跃 Turn 标识或根树归属不一致".to_owned(),
            });
        }
        let is_child = agent.definition.depth == AgentDepth::CHILD;
        if is_child != active.global_permit.is_some() {
            return Err(CollaborationError::InvalidRecovery {
                message: "根 Turn 与子 Agent 全局 permit 归属不一致".to_owned(),
            });
        }
        if is_child {
            expected_global = expected_global
                .checked_add(1)
                .ok_or(CollaborationError::SequenceExhausted)?;
            let root_in_use = expected_by_root
                .entry(active.root_agent_id.clone())
                .or_default();
            *root_in_use = root_in_use
                .checked_add(1)
                .ok_or(CollaborationError::SequenceExhausted)?;
        }
    }
    if state.global_in_use != expected_global {
        return Err(CollaborationError::InvalidRecovery {
            message: "Coordinator 子 Agent 槽位计数与活跃 Turn 不一致".to_owned(),
        });
    }
    for root in state.roots.values() {
        if root.in_use
            != expected_by_root
                .get(&root.root_agent_id)
                .copied()
                .unwrap_or(0)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "根树子 Agent 槽位计数与活跃 Turn 不一致".to_owned(),
            });
        }
    }
    Ok(())
}

/// 校验协调器容量不变量及 Agent、mailbox、steer、幂等记录和总文本硬配额。
pub(super) fn validate_coordinator_quotas(
    state: &CoordinatorState,
) -> Result<(), CollaborationError> {
    validate_turn_capacity_invariants(state)?;
    if state.collaboration_invocations.len() > MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器协作工具幂等记录数量",
            maximum: MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR,
        });
    }
    if state.root_turn_bindings.len() > MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器外部根 Turn 幂等绑定数量",
            maximum: MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR,
        });
    }
    let agent_count = state
        .roots
        .values()
        .map(|root| root.known_agents.len())
        .sum::<usize>();
    if agent_count > MAX_AGENTS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器 Agent 身份数量",
            maximum: MAX_AGENTS_PER_COORDINATOR,
        });
    }
    let mailbox_count = state
        .roots
        .values()
        .map(|root| root.mailbox_count)
        .sum::<usize>();
    if mailbox_count > MAX_MAILBOX_MESSAGES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费 mailbox 消息数量",
            maximum: MAX_MAILBOX_MESSAGES_PER_COORDINATOR,
        });
    }
    let mailbox_bytes = state
        .roots
        .values()
        .map(|root| root.mailbox_bytes)
        .sum::<usize>();
    if mailbox_bytes > MAX_MAILBOX_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费 mailbox 正文字节数",
            maximum: MAX_MAILBOX_BYTES_PER_COORDINATOR,
        });
    }
    let resident_steer_count = state
        .agents
        .values()
        .map(|agent| agent.steers.len())
        .sum::<usize>();
    let evicted_steer_count = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.steer_count)
        .sum::<usize>();
    let steer_count = resident_steer_count.saturating_add(evicted_steer_count);
    if steer_count > MAX_PENDING_STEERS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费用户 steer 数量",
            maximum: MAX_PENDING_STEERS_PER_COORDINATOR,
        });
    }
    let resident_steer_bytes = state
        .agents
        .values()
        .map(|agent| agent.steer_bytes)
        .sum::<usize>();
    let evicted_steer_bytes = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.steer_bytes)
        .sum::<usize>();
    let steer_bytes = resident_steer_bytes.saturating_add(evicted_steer_bytes);
    if steer_bytes > MAX_PENDING_STEER_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费用户 steer 正文字节数",
            maximum: MAX_PENDING_STEER_BYTES_PER_COORDINATOR,
        });
    }
    let definition_bytes = state
        .roots
        .values()
        .flat_map(|root| root.known_agents.values())
        .map(agent_definition_text_bytes)
        .sum::<usize>();
    let resident_dynamic_bytes = state
        .agents
        .values()
        .map(|agent| {
            agent.last_turn.as_ref().map_or(0, |turn| {
                turn.prompt
                    .as_ref()
                    .map_or(0, String::len)
                    .saturating_add(turn_outcome_text_bytes(&turn.outcome))
            })
        })
        .sum::<usize>();
    let evicted_dynamic_bytes = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.dynamic_text_bytes)
        .sum::<usize>();
    let resident_initial_triggered_turn_bytes = state
        .agents
        .values()
        .flat_map(|agent| agent.mailbox.iter())
        .map(|entry| {
            entry
                .initial_triggered_turn_id
                .as_ref()
                .map_or(0, |turn_id| turn_id.as_str().len())
        })
        .sum::<usize>();
    let evicted_initial_triggered_turn_bytes = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.initial_triggered_turn_bytes)
        .sum::<usize>();
    let pending_prompt_bytes = state
        .pending_turns
        .iter()
        .map(|turn| turn.prompt.as_ref().map_or(0, String::len))
        .sum::<usize>();
    let active_prompt_bytes = state
        .active_turns
        .values()
        .map(|turn| turn.prompt.as_ref().map_or(0, String::len))
        .sum::<usize>();
    let invocation_bytes =
        state
            .collaboration_invocations
            .iter()
            .fold(0usize, |total, (key, record)| {
                total.saturating_add(collaboration_invocation_text_bytes(key, record))
            });
    let root_turn_binding_bytes =
        state
            .root_turn_bindings
            .iter()
            .fold(0_usize, |total, (turn_id, binding)| {
                total
                    .saturating_add(turn_id.as_str().len())
                    .saturating_add(binding.root_agent_id.as_str().len())
                    .saturating_add(binding.prompt_digest.len())
            });
    let retained_text_bytes = mailbox_bytes
        .saturating_add(steer_bytes)
        .saturating_add(definition_bytes)
        .saturating_add(resident_dynamic_bytes)
        .saturating_add(evicted_dynamic_bytes)
        .saturating_add(resident_initial_triggered_turn_bytes)
        .saturating_add(evicted_initial_triggered_turn_bytes)
        .saturating_add(pending_prompt_bytes)
        .saturating_add(active_prompt_bytes)
        .saturating_add(invocation_bytes)
        .saturating_add(root_turn_binding_bytes);
    if retained_text_bytes > MAX_RETAINED_TEXT_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器保留文本总字节数",
            maximum: MAX_RETAINED_TEXT_BYTES_PER_COORDINATOR,
        });
    }
    Ok(())
}

/// 校验一条恢复幂等记录的树归属、操作类型与首次结果引用。
pub(super) fn validate_recovered_collaboration_invocation(
    state: &CoordinatorState,
    invocation: &RecoveredCollaborationInvocation,
    spawned_agent_ids: &mut HashSet<AgentId>,
    message_ids: &mut HashSet<MailboxMessageId>,
) -> Result<(), CollaborationError> {
    let source_definition = state
        .roots
        .values()
        .find_map(|root| root.known_agents.get(&invocation.key.source_agent_id))
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的来源 Agent 不属于任何恢复根树".to_owned(),
        })?;
    let root = state
        .roots
        .get(&source_definition.root_agent_id)
        .expect("幂等记录来源 Agent 的根树应存在");
    if !state_turn_belongs_to_root(
        state,
        &source_definition.root_agent_id,
        &invocation.key.source_turn_id,
    ) {
        return Err(CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的来源 Turn 跨根或尚未分配".to_owned(),
        });
    }

    match (invocation.kind, &invocation.output) {
        (
            CollaborationInvocationKind::SpawnAgent,
            CollaborationInvocationOutput::SpawnedAgent(spawned),
        ) => {
            let definition = root
                .known_agents
                .get(&spawned.agent.agent_id)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "SpawnAgent 幂等结果引用了未知子 Agent".to_owned(),
                })?;
            let initial_turn_sequence =
                turn_sequence_for_root(&source_definition.root_agent_id, &spawned.initial_turn_id);
            let source_turn_sequence = turn_sequence_for_root(
                &source_definition.root_agent_id,
                &invocation.key.source_turn_id,
            );
            if source_definition.depth != AgentDepth::ROOT
                || !spawned_agent_ids.insert(spawned.agent.agent_id.clone())
                || definition.depth != AgentDepth::CHILD
                || definition.parent_agent_id.as_ref() != Some(&invocation.key.source_agent_id)
                || definition.session_id != spawned.agent.session_id
                || definition.path != spawned.agent.path
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    &spawned.initial_turn_id,
                )
                || initial_turn_sequence.is_none()
                || source_turn_sequence.is_some_and(|source| {
                    initial_turn_sequence.is_some_and(|initial| initial <= source)
                })
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SpawnAgent 幂等结果与来源、定义或初始 Turn 不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::SendMessage,
            CollaborationInvocationOutput::Message {
                message_id,
                triggered_turn_id,
            },
        ) => {
            if !message_ids.insert(message_id.clone())
                || triggered_turn_id.as_ref().is_some_and(|turn_id| {
                    !turn_id_belongs_to_root(
                        &source_definition.root_agent_id,
                        root.next_turn_sequence,
                        turn_id,
                    ) || turn_sequence_for_root(&source_definition.root_agent_id, turn_id).is_none()
                        || turn_sequence_for_root(
                            &source_definition.root_agent_id,
                            &invocation.key.source_turn_id,
                        )
                        .is_some_and(|source| {
                            turn_sequence_for_root(&source_definition.root_agent_id, turn_id)
                                .is_some_and(|triggered| triggered <= source)
                        })
                })
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SendMessage 幂等结果的消息或触发 Turn 无效".to_owned(),
                });
            }
            let matching_entries = state
                .agents
                .values()
                .flat_map(|agent| agent.mailbox.iter())
                .filter(|entry| &entry.message.message_id == message_id)
                .collect::<Vec<_>>();
            if matching_entries.len() > 1 {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SendMessage 幂等结果对应多个 mailbox 条目".to_owned(),
                });
            }
            if let Some(entry) = matching_entries.first() {
                let message = &entry.message;
                let expected_input_digest = collaboration_invocation_input_digest(
                    &CollaborationInvocationInput::SendMessage {
                        target_agent_id: message.target_agent_id.clone(),
                        content: message.content.clone(),
                        delivery: message.delivery,
                    },
                );
                if message.source_agent_id != invocation.key.source_agent_id
                    || message.kind != MailboxMessageKind::AgentMessage
                    || message.related_turn_id.as_ref() != Some(&invocation.key.source_turn_id)
                    || message.parent_turn_id.as_ref() != Some(&invocation.key.source_turn_id)
                    || message.root_turn_id.as_ref().is_none_or(|turn_id| {
                        !state_turn_belongs_to_root(
                            state,
                            &source_definition.root_agent_id,
                            turn_id,
                        )
                    })
                    || invocation.input_digest != expected_input_digest
                    || entry.initial_triggered_turn_id.as_ref() != triggered_turn_id.as_ref()
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "SendMessage 幂等结果与未消费 mailbox 条目不一致".to_owned(),
                    });
                }
            }
        }
        (
            CollaborationInvocationKind::StopAgent,
            CollaborationInvocationOutput::StoppedAgent {
                target_agent_id,
                stopped_turn_id,
            },
        ) => {
            let target_definition = root.known_agents.get(target_agent_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "StopAgent 幂等结果引用了未知目标 Agent".to_owned(),
                }
            })?;
            if target_agent_id == &invocation.key.source_agent_id
                || target_definition.depth != AgentDepth::CHILD
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    stopped_turn_id,
                )
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "StopAgent 幂等结果与来源、目标或停止 Turn 不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::SteerAgent,
            CollaborationInvocationOutput::UserSteer(steer),
        ) => {
            validate_required_text(&steer.content, "恢复用户 steer")?;
            if steer.sequence == 0 || steer.turn_id != invocation.key.source_turn_id {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SteerAgent 幂等结果与来源 Turn 或序号不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::RetryAgent,
            CollaborationInvocationOutput::RetriedAgent {
                target_agent_id,
                retry_turn_id,
            },
        ) => {
            let target_definition = root.known_agents.get(target_agent_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "RetryAgent 幂等结果引用了未知目标 Agent".to_owned(),
                }
            })?;
            let retry_sequence =
                turn_sequence_for_root(&source_definition.root_agent_id, retry_turn_id);
            let source_sequence = turn_sequence_for_root(
                &source_definition.root_agent_id,
                &invocation.key.source_turn_id,
            );
            if target_definition.root_agent_id != source_definition.root_agent_id
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    retry_turn_id,
                )
                || retry_sequence.is_none()
                || source_sequence
                    .is_some_and(|source| retry_sequence.is_some_and(|retry| retry <= source))
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "RetryAgent 幂等结果与来源、目标或新 Turn 不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::ResumeAgent,
            CollaborationInvocationOutput::ResumedAgent {
                target_agent_id,
                resume_turn_id,
            },
        ) => {
            let target_definition = root.known_agents.get(target_agent_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "ResumeAgent 幂等结果引用了未知目标 Agent".to_owned(),
                }
            })?;
            let resume_sequence =
                turn_sequence_for_root(&source_definition.root_agent_id, resume_turn_id);
            let source_sequence = turn_sequence_for_root(
                &source_definition.root_agent_id,
                &invocation.key.source_turn_id,
            );
            if target_definition.root_agent_id != source_definition.root_agent_id
                || target_definition.depth != AgentDepth::CHILD
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    resume_turn_id,
                )
                || resume_sequence.is_none()
                || source_sequence
                    .is_some_and(|source| resume_sequence.is_some_and(|resume| resume <= source))
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "ResumeAgent 幂等结果与来源、目标或新 Turn 不一致".to_owned(),
                });
            }
        }
        _ => {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作工具幂等记录的输入与结果类型不匹配".to_owned(),
            });
        }
    }
    Ok(())
}

/// 校验可在进程冷启动时整体恢复的自包含根树。
pub(super) fn validate_restorable_tree(
    tree: &RecoveredAgentTree,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<AgentDefinition, CollaborationError> {
    if tree.per_root_turn_limit == 0 || tree.next_checkpoint_revision == 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "完整恢复快照的容量或局部 checkpoint counter 无效".to_owned(),
        });
    }
    let root_definition = tree
        .known_agents
        .iter()
        .find(|definition| definition.depth == AgentDepth::ROOT)
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "完整恢复快照缺少根 Agent".to_owned(),
        })?;
    validate_recovered_tree(
        tree,
        &tree.root_agent_id,
        &tree.root_session_id,
        &root_definition,
        tree.per_root_turn_limit,
        tree.lifecycle,
        external_root_turn_roots,
    )?;
    Ok(root_definition)
}

/// 统一表示当前或历史恢复 Turn 的不可变因果字段。
pub(super) struct RecoveredTurnFields<'a> {
    /// 当前校验的 Turn 标识。
    turn_id: &'a TurnId,
    /// 创建该 Turn 的触发原因。
    cause: &'a AgentTurnCause,
    /// 根任务或初始子任务携带的可选输入。
    prompt: Option<&'a String>,
    /// 直接触发当前 Turn 的父 Turn。
    parent_turn_id: Option<&'a TurnId>,
    /// 当前调用链最初的根 Turn。
    root_turn_id: &'a TurnId,
}

/// 校验一个历史或当前 Turn 的因果字段与根命名空间单调序号一致。
pub(super) fn validate_recovered_turn_fields(
    definition: &AgentDefinition,
    fields: RecoveredTurnFields<'_>,
    next_turn_sequence: u64,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<(), CollaborationError> {
    let RecoveredTurnFields {
        turn_id,
        cause,
        prompt,
        parent_turn_id,
        root_turn_id,
    } = fields;
    let belongs = |candidate: &TurnId| {
        turn_id_belongs_to_root(&definition.root_agent_id, next_turn_sequence, candidate)
            || external_root_turn_roots.get(candidate) == Some(&definition.root_agent_id)
    };
    let turn_sequence = turn_sequence_for_root(&definition.root_agent_id, turn_id);
    let parent_sequence =
        parent_turn_id.and_then(|parent| turn_sequence_for_root(&definition.root_agent_id, parent));
    let root_sequence = turn_sequence_for_root(&definition.root_agent_id, root_turn_id);
    if !belongs(turn_id)
        || !belongs(root_turn_id)
        || parent_turn_id.is_some_and(|parent| parent == turn_id || !belongs(parent))
        || parent_sequence
            .zip(turn_sequence)
            .is_some_and(|(parent, current)| parent >= current)
        || root_sequence
            .zip(turn_sequence)
            .is_some_and(|(root, current)| root > current)
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复 Turn 引用了越界、跨根或自循环的 Turn 标识".to_owned(),
        });
    }
    if let Some(prompt) = prompt {
        validate_required_text(prompt, "恢复 Turn 输入").map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
    }
    let valid_cause = match cause {
        AgentTurnCause::RootUser => {
            definition.depth == AgentDepth::ROOT
                && prompt.is_some()
                && parent_turn_id.is_none()
                && root_turn_id == turn_id
        }
        AgentTurnCause::InitialTask => {
            definition.depth == AgentDepth::CHILD
                && turn_sequence.is_some()
                && prompt.is_some()
                && parent_turn_id.is_some()
                && root_turn_id != turn_id
        }
        AgentTurnCause::Followup { message_id } => {
            turn_sequence.is_some()
                && prompt.is_none()
                && parent_turn_id.is_some()
                && !message_id.as_str().trim().is_empty()
                && message_id.as_str().len() <= MAX_PROFILE_FIELD_BYTES
                && root_turn_id != turn_id
        }
        AgentTurnCause::Retry { previous_turn_id } => {
            parent_turn_id.is_some()
                && previous_turn_id != turn_id
                && belongs(previous_turn_id)
                && turn_sequence_for_root(&definition.root_agent_id, previous_turn_id)
                    .zip(turn_sequence)
                    .is_some_and(|(previous, current)| previous < current)
                && root_turn_id != turn_id
        }
    };
    if !valid_cause {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复 Turn 的原因、输入或父 Turn 关系无效".to_owned(),
        });
    }
    Ok(())
}

/// 校验冷恢复树的所有者、根定义、单层父链、Turn 和消息边界。
pub(super) fn validate_recovered_tree(
    tree: &RecoveredAgentTree,
    expected_root_agent_id: &AgentId,
    expected_root_session_id: &SessionId,
    expected_root_definition: &AgentDefinition,
    expected_turn_limit: usize,
    expected_lifecycle: RecoveredRootLifecycle,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<(), CollaborationError> {
    if tree.root_agent_id != *expected_root_agent_id
        || tree.root_session_id != *expected_root_session_id
        || tree.lifecycle != expected_lifecycle
        || tree.per_root_turn_limit != expected_turn_limit
        || !tree.live && tree.lifecycle != RecoveredRootLifecycle::Open
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树所有者、容量、live 或关闭清理状态无效".to_owned(),
        });
    }
    if tree.next_turn_sequence == 0 || tree.next_checkpoint_revision == 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树的 Turn 或局部 checkpoint 单调序号不能为零".to_owned(),
        });
    }
    if tree.known_agents.is_empty()
        || tree.known_agents.len() > MAX_AGENTS_PER_ROOT
        || tree.agents.is_empty()
        || tree.agents.len() != tree.known_agents.len()
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树不是覆盖全部已知 Agent 的自包含 checkpoint".to_owned(),
        });
    }

    let mut known_by_id = HashMap::new();
    let mut known_paths = HashSet::new();
    let mut session_ids = HashSet::new();
    let mut worktree_leases = HashSet::new();
    let mut known_root_count = 0usize;
    for definition in &tree.known_agents {
        validate_agent_profile(&definition.profile).map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
        validate_context_inheritance(&definition.context_inheritance).map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
        validate_context_snapshot(
            &definition.context_inheritance,
            &definition.context_snapshot,
        )
        .map_err(|error| CollaborationError::InvalidRecovery {
            message: error.to_string(),
        })?;
        if let Some(template) = &definition.agent_template {
            validate_agent_template_snapshot(template).map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
        }
        if definition.root_agent_id != tree.root_agent_id
            || definition.root_session_id != tree.root_session_id
            || definition.depth != definition.path.depth()
            || known_by_id
                .insert(definition.agent_id.clone(), definition)
                .is_some()
            || !known_paths.insert(definition.path.clone())
            || !session_ids.insert(definition.session_id.clone())
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "已知 Agent 的所有者、深度、标识、Session 或路径不一致".to_owned(),
            });
        }
        if let Some(worktree_lease) = &definition.profile.worktree_lease
            && !worktree_leases.insert(worktree_lease.clone())
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复树重复绑定同一 Worktree lease".to_owned(),
            });
        }
        match definition.depth {
            depth if depth == AgentDepth::ROOT => {
                known_root_count = known_root_count.saturating_add(1);
                if definition.agent_id != tree.root_agent_id
                    || definition.parent_agent_id.is_some()
                    || definition.path != AgentPath::root()
                    || definition.assignment.is_some()
                    || definition.session_id != tree.root_session_id
                    || definition.context_inheritance != ContextInheritance::None
                    || !definition.context_snapshot.is_empty()
                    || definition.agent_template.is_some()
                    || definition != expected_root_definition
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复根 Agent 定义无效".to_owned(),
                    });
                }
            }
            depth if depth == AgentDepth::CHILD => {
                let assignment = definition.assignment.as_deref().ok_or_else(|| {
                    CollaborationError::InvalidRecovery {
                        message: "恢复子 Agent 缺少稳定职责".to_owned(),
                    }
                })?;
                validate_agent_assignment(assignment).map_err(|error| {
                    CollaborationError::InvalidRecovery {
                        message: error.to_string(),
                    }
                })?;
                if definition.parent_agent_id.as_ref() != Some(&tree.root_agent_id)
                    || !definition.path.as_str().starts_with("/root/")
                    || matches!(definition.context_inheritance, ContextInheritance::None)
                        && !definition.context_snapshot.is_empty()
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复子 Agent 父链或路径无效".to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 Agent 超过单层限制".to_owned(),
                });
            }
        }
    }
    if known_root_count != 1 {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树根定义不一致".to_owned(),
        });
    }

    let recovered_by_id = tree
        .agents
        .iter()
        .map(|agent| (agent.definition.agent_id.clone(), agent))
        .collect::<HashMap<_, _>>();
    let mut resident_ids = HashSet::new();
    let mut message_ids = HashSet::new();
    let mut completion_sources = HashSet::new();
    let mut tree_user_count = 0usize;
    let mut tree_user_bytes = 0usize;
    let mut tree_completion_count = 0usize;
    let mut tree_completion_bytes = 0usize;
    for agent in &tree.agents {
        let definition = &agent.definition;
        let turn_belongs = |turn_id: &TurnId| {
            turn_id_belongs_to_root(&tree.root_agent_id, tree.next_turn_sequence, turn_id)
                || external_root_turn_roots.get(turn_id) == Some(&tree.root_agent_id)
        };
        if known_by_id.get(&definition.agent_id).copied() != Some(definition)
            || !resident_ids.insert(definition.agent_id.clone())
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "驻留 Agent 不属于已知定义或发生重复".to_owned(),
            });
        }
        let current_turn_id = recovered_current_turn_id(&agent.status);
        let interrupted_or_failed_turn_id = match &agent.status {
            CollaborationAgentStatus::Interrupted { turn_id }
            | CollaborationAgentStatus::Failed { turn_id, .. } => Some(turn_id),
            _ => None,
        };
        let claim_turn_is_valid = |claim_turn_id: &TurnId| {
            current_turn_id == Some(claim_turn_id)
                || interrupted_or_failed_turn_id == Some(claim_turn_id)
        };
        match (
            agent.mailbox_claim_turn_id.as_ref(),
            agent.mailbox_claim_through_sequence,
        ) {
            (None, None) => {}
            (Some(claim_turn_id), Some(through_sequence)) => {
                let claimed = agent
                    .mailbox
                    .iter()
                    .take_while(|entry| entry.message.sequence <= through_sequence)
                    .collect::<Vec<_>>();
                if !claim_turn_is_valid(claim_turn_id)
                    || claimed.last().map(|entry| entry.message.sequence) != Some(through_sequence)
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复 mailbox claim 未绑定当前或可重试 Turn 的完整 FIFO 前缀"
                            .to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox claim 的 Turn 与最大序号必须同时存在".to_owned(),
                });
            }
        }
        match (
            agent.steer_claim_turn_id.as_ref(),
            agent.steer_claim_through_sequence,
        ) {
            (None, None) => {}
            (Some(claim_turn_id), Some(through_sequence)) => {
                let claimed = agent
                    .pending_steers
                    .iter()
                    .filter(|steer| {
                        &steer.turn_id == claim_turn_id && steer.sequence <= through_sequence
                    })
                    .collect::<Vec<_>>();
                if !claim_turn_is_valid(claim_turn_id)
                    || claimed.last().map(|steer| steer.sequence) != Some(through_sequence)
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复用户 steer claim 未绑定当前或可重试 Turn 的完整批次"
                            .to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复用户 steer claim 的 Turn 与最大序号必须同时存在".to_owned(),
                });
            }
        }
        if current_turn_id.is_some() && !tree.live {
            return Err(CollaborationError::InvalidRecovery {
                message: "静止 checkpoint 不能包含未决 Turn".to_owned(),
            });
        }
        match tree.lifecycle {
            RecoveredRootLifecycle::Open => {
                if matches!(
                    agent.status,
                    CollaborationAgentStatus::PendingInit | CollaborationAgentStatus::Stopped
                ) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "开放恢复树包含不可恢复的 Agent 状态".to_owned(),
                    });
                }
            }
            RecoveredRootLifecycle::Closing => {
                if !matches!(
                    agent.status,
                    CollaborationAgentStatus::Cancelling { .. } | CollaborationAgentStatus::Stopped
                ) || !agent.mailbox.is_empty()
                    || !agent.pending_steers.is_empty()
                    || agent.start_pending
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "Closing 根树只能包含已清空的 Cancelling 或 Stopped Agent"
                            .to_owned(),
                    });
                }
            }
            RecoveredRootLifecycle::CleanupPending => {
                if agent.status != CollaborationAgentStatus::Stopped
                    || !agent.mailbox.is_empty()
                    || !agent.pending_steers.is_empty()
                    || current_turn_id.is_some()
                    || agent.start_pending
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "CleanupPending 根树必须只包含已清空的 Stopped Agent".to_owned(),
                    });
                }
            }
        }

        if let Some(turn_id) = current_turn_id {
            let source_agent_id = agent.current_source_agent_id.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少来源 Agent".to_owned(),
                }
            })?;
            let cause = agent.current_turn_cause.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少触发原因".to_owned(),
                }
            })?;
            let root_turn_id = agent.current_root_turn_id.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少根 Turn".to_owned(),
                }
            })?;
            let current_plan_guard =
                agent
                    .current_plan_guard
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "live checkpoint 当前 Turn 缺少 Plan 守卫".to_owned(),
                    })?;
            if matches!(
                definition.profile.plan_guard.state(),
                PlanGuardState::ReadOnly
            ) && matches!(current_plan_guard.state(), PlanGuardState::Inactive)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 放宽了 Agent 的 Plan 守卫".to_owned(),
                });
            }
            if !known_by_id.contains_key(source_agent_id)
                || matches!(cause, AgentTurnCause::RootUser)
                    && source_agent_id != &definition.agent_id
                || matches!(cause, AgentTurnCause::InitialTask)
                    && definition.parent_agent_id.as_ref() != Some(source_agent_id)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 的来源 Agent 无效".to_owned(),
                });
            }
            validate_recovered_turn_fields(
                definition,
                RecoveredTurnFields {
                    turn_id,
                    cause,
                    prompt: agent.current_turn_prompt.as_ref(),
                    parent_turn_id: agent.current_parent_turn_id.as_ref(),
                    root_turn_id,
                },
                tree.next_turn_sequence,
                external_root_turn_roots,
            )?;
            if agent
                .last_turn
                .as_ref()
                .is_some_and(|last_turn| &last_turn.turn_id == turn_id)
                || matches!(
                    agent.status,
                    CollaborationAgentStatus::WaitingCapacity { .. }
                ) && agent.start_pending
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 与最近 Turn、outbox 或 steer 不一致"
                        .to_owned(),
                });
            }
        } else if agent.current_source_agent_id.is_some()
            || agent.current_turn_cause.is_some()
            || agent.current_turn_prompt.is_some()
            || agent.current_parent_turn_id.is_some()
            || agent.current_root_turn_id.is_some()
            || agent.current_plan_guard.is_some()
            || agent.start_pending
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "非活跃 Agent 不能保留当前 Turn 或 StartTurn outbox 元数据".to_owned(),
            });
        }

        if tree.lifecycle == RecoveredRootLifecycle::Open
            && current_turn_id.is_none()
            && !recovered_terminal_state_matches(agent)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 Agent 状态与最近 Turn 终态不一致".to_owned(),
            });
        }

        if let Some(last_turn) = &agent.last_turn {
            validate_recovered_turn_fields(
                definition,
                RecoveredTurnFields {
                    turn_id: &last_turn.turn_id,
                    cause: &last_turn.cause,
                    prompt: last_turn.prompt.as_ref(),
                    parent_turn_id: last_turn.parent_turn_id.as_ref(),
                    root_turn_id: &last_turn.root_turn_id,
                },
                tree.next_turn_sequence,
                external_root_turn_roots,
            )?;
            validate_turn_outcome(&last_turn.outcome).map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
        }

        let steer_turn_id = current_turn_id.or(match &agent.status {
            CollaborationAgentStatus::Interrupted { turn_id }
            | CollaborationAgentStatus::Failed { turn_id, .. } => Some(turn_id),
            _ => None,
        });
        if agent.pending_steers.len() > MAX_PENDING_STEERS_PER_AGENT {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 steer 数量超过限制".to_owned(),
            });
        }
        let mut last_steer_sequence = 0u64;
        let mut steer_bytes = 0usize;
        for steer in &agent.pending_steers {
            validate_required_text(&steer.content, "恢复用户 steer").map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
            if steer.sequence <= last_steer_sequence
                || steer_turn_id != Some(&steer.turn_id)
                || !turn_belongs(&steer.turn_id)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 steer 的序号或 Turn 归属无效".to_owned(),
                });
            }
            steer_bytes = steer_bytes
                .checked_add(steer.content.len())
                .ok_or(CollaborationError::SequenceExhausted)?;
            last_steer_sequence = steer.sequence;
        }
        if steer_bytes > MAX_PENDING_STEER_BYTES_PER_AGENT
            || agent.next_steer_sequence <= last_steer_sequence
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 steer 的累计字节或下一序号无效".to_owned(),
            });
        }

        let mut last_sequence = 0u64;
        let mut agent_user_count = 0usize;
        let mut agent_user_bytes = 0usize;
        let mut agent_completion_count = 0usize;
        let mut agent_completion_bytes = 0usize;
        for mailbox in &agent.mailbox {
            validate_required_text(&mailbox.message.content, "恢复 mailbox 正文").map_err(
                |error| CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                },
            )?;
            let source_definition = known_by_id
                .get(&mailbox.message.source_agent_id)
                .copied()
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox 来源 Agent 不存在".to_owned(),
                })?;
            if mailbox.message.message_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
                || mailbox.message.target_agent_id != definition.agent_id
                || mailbox.message.source_agent_path != source_definition.path
                || matches!(
                    source_definition.profile.plan_guard.state(),
                    PlanGuardState::ReadOnly
                ) && matches!(
                    mailbox.message.source_plan_guard.state(),
                    PlanGuardState::Inactive
                )
                || mailbox.message.related_turn_id.is_none()
                || mailbox
                    .message
                    .related_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.parent_turn_id.is_none()
                || mailbox
                    .message
                    .parent_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.root_turn_id.is_none()
                || mailbox
                    .message
                    .root_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.sequence <= last_sequence
                || !message_ids.insert(mailbox.message.message_id.clone())
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox 的来源、目标、Turn 或 FIFO 序列无效".to_owned(),
                });
            }
            let is_agent_message = matches!(mailbox.message.kind, MailboxMessageKind::AgentMessage);
            let invalid_initial_trigger =
                mailbox
                    .initial_triggered_turn_id
                    .as_ref()
                    .is_some_and(|triggered| {
                        mailbox.message.delivery != MailboxDelivery::TriggerTurn
                            || !is_agent_message
                            || !turn_belongs(triggered)
                    });
            let invalid_current_claim = mailbox.claimed_turn_id.as_ref().is_some_and(|claimed| {
                mailbox.message.delivery != MailboxDelivery::TriggerTurn
                    || !is_agent_message
                    || current_turn_id != Some(claimed)
                    || !turn_belongs(claimed)
            });
            if invalid_initial_trigger
                || invalid_current_claim
                || mailbox.message.delivery == MailboxDelivery::QueueOnly
                    && (mailbox.initial_triggered_turn_id.is_some()
                        || mailbox.claimed_turn_id.is_some())
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox 保留了无效的 TriggerTurn 首次触发或当前归属".to_owned(),
                });
            }
            match &mailbox.message.kind {
                MailboxMessageKind::AgentMessage => {
                    if mailbox.message.related_turn_id != mailbox.message.parent_turn_id {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "Agent mailbox 的来源与直接父 Turn 因果不一致".to_owned(),
                        });
                    }
                    agent_user_count = agent_user_count
                        .checked_add(1)
                        .ok_or(CollaborationError::SequenceExhausted)?;
                    agent_user_bytes = agent_user_bytes
                        .checked_add(mailbox.message.content.len())
                        .ok_or(CollaborationError::SequenceExhausted)?;
                }
                MailboxMessageKind::ChildTurnFinished { outcome } => {
                    validate_turn_outcome(outcome).map_err(|error| {
                        CollaborationError::InvalidRecovery {
                            message: error.to_string(),
                        }
                    })?;
                    let source = known_by_id
                        .get(&mailbox.message.source_agent_id)
                        .copied()
                        .ok_or_else(|| CollaborationError::InvalidRecovery {
                            message: "完成消息来源 Agent 不存在".to_owned(),
                        })?;
                    let source_agent =
                        recovered_by_id
                            .get(&source.agent_id)
                            .copied()
                            .ok_or_else(|| CollaborationError::InvalidRecovery {
                                message: "完成消息来源 Agent 不在自包含 checkpoint 中".to_owned(),
                            })?;
                    let source_last_turn = source_agent.last_turn.as_ref().ok_or_else(|| {
                        CollaborationError::InvalidRecovery {
                            message: "完成消息来源 Agent 缺少最近终态".to_owned(),
                        }
                    })?;
                    let expected_outcome = bounded_completion_outcome(&source_last_turn.outcome);
                    let expected_content = bounded_text(
                        &child_completion_content(&source.path, &expected_outcome),
                        MAX_COMPLETION_NOTIFICATION_BYTES,
                    );
                    if source.depth != AgentDepth::CHILD
                        || mailbox.message.source_plan_guard != source.profile.plan_guard
                        || mailbox.message.target_agent_id != tree.root_agent_id
                        || mailbox.message.delivery != MailboxDelivery::QueueOnly
                        || mailbox.initial_triggered_turn_id.is_some()
                        || mailbox.claimed_turn_id.is_some()
                        || mailbox.message.related_turn_id.as_ref()
                            != Some(&source_last_turn.turn_id)
                        || mailbox.message.parent_turn_id != source_last_turn.parent_turn_id
                        || mailbox.message.root_turn_id.as_ref()
                            != Some(&source_last_turn.root_turn_id)
                        || outcome != &expected_outcome
                        || mailbox.message.content != expected_content
                        || mailbox.message.content.len() > MAX_COMPLETION_NOTIFICATION_BYTES
                        || !completion_sources.insert(source.agent_id.clone())
                    {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "子 Agent 完成消息不是来源 Agent 的最新有界 QueueOnly 终态"
                                .to_owned(),
                        });
                    }
                    agent_completion_count = agent_completion_count
                        .checked_add(1)
                        .ok_or(CollaborationError::SequenceExhausted)?;
                    agent_completion_bytes = agent_completion_bytes
                        .checked_add(mailbox.message.content.len())
                        .ok_or(CollaborationError::SequenceExhausted)?;
                }
            }
            last_sequence = mailbox.message.sequence;
        }
        if agent_user_count > MAX_MAILBOX_MESSAGES_PER_AGENT
            || agent_user_bytes > MAX_MAILBOX_BYTES_PER_AGENT
            || agent_completion_count > MAX_COMPLETION_MESSAGES_PER_TREE
            || agent_completion_bytes > MAX_COMPLETION_BYTES_PER_TREE
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 Agent 的普通消息或完成通知超过独立边界".to_owned(),
            });
        }
        tree_user_count = tree_user_count
            .checked_add(agent_user_count)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_user_bytes = tree_user_bytes
            .checked_add(agent_user_bytes)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_completion_count = tree_completion_count
            .checked_add(agent_completion_count)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_completion_bytes = tree_completion_bytes
            .checked_add(agent_completion_bytes)
            .ok_or(CollaborationError::SequenceExhausted)?;
        if agent.next_mailbox_sequence <= last_sequence {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 mailbox 的下一序号无效".to_owned(),
            });
        }
    }
    if resident_ids != known_by_id.keys().cloned().collect::<HashSet<_>>()
        || !resident_ids.contains(&tree.root_agent_id)
        || tree_user_count > MAX_USER_MAILBOX_MESSAGES_PER_TREE
        || tree_user_bytes > MAX_USER_MAILBOX_BYTES_PER_TREE
        || tree_completion_count > MAX_COMPLETION_MESSAGES_PER_TREE
        || tree_completion_bytes > MAX_COMPLETION_BYTES_PER_TREE
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树缺少 Agent，或普通消息与完成通知突破树级独立边界".to_owned(),
        });
    }
    Ok(())
}
