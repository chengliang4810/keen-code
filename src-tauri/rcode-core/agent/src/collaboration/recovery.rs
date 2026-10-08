//! 协作领域的 checkpoint 投影、调用重放与恢复装载。

use super::*;

impl CollaborationCoordinator {
    /// 生成可交给 Session Store 定期快照的根树冷恢复数据。
    pub fn checkpoint_root(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<RecoveredAgentTree, CollaborationError> {
        let state = self.lock_state()?;
        checkpoint_root_with_store(&state, root_agent_id, self.inner.store.as_ref())
    }

    /// 生成不含未决 Turn 或 durable outbox 的静止导出快照。
    pub fn checkpoint_quiescent_root(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<RecoveredAgentTree, CollaborationError> {
        let state = self.lock_state()?;
        let mut checkpoint =
            checkpoint_root_with_store(&state, root_agent_id, self.inner.store.as_ref())?;
        let has_current_turn = checkpoint.agents.iter().any(|agent| {
            matches!(
                agent.status,
                CollaborationAgentStatus::WaitingCapacity { .. }
                    | CollaborationAgentStatus::Running { .. }
                    | CollaborationAgentStatus::Cancelling { .. }
            )
        });
        if has_current_turn
            || state.close_outbox.contains_key(root_agent_id)
            || checkpoint.agents.iter().any(|agent| {
                recovered_current_turn_id(&agent.status).is_some_and(|turn_id| {
                    state
                        .signal_outbox
                        .keys()
                        .any(|key| &key.turn_id == turn_id)
                })
            })
            || checkpoint.agents.iter().any(|agent| agent.start_pending)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "静止 checkpoint 不能包含未决 Turn 或 durable outbox".to_owned(),
            });
        }
        checkpoint.live = false;
        Ok(checkpoint)
    }

    /// 在同一状态锁下生成包含空根集、全局水位和根身份 counter 的原子快照。
    pub fn checkpoint_coordinator(&self) -> Result<RecoveredCoordinator, CollaborationError> {
        let state = self.lock_state()?;
        checkpoint_coordinator_from_state(&state, self.inner.store.as_ref())
    }

    /// 返回测试可见的执行 fence 数量，用于证明已移除根不会形成无界墓碑。
    #[cfg(test)]
    pub(crate) fn execution_fence_count(&self) -> usize {
        self.inner
            .execution_fences
            .lock()
            .expect("测试执行 fence 锁不应中毒")
            .len()
    }

    /// 显式重试所有未确认的 StartTurn、SignalTurn、QuiesceTree 与 CloseTree 命令。
    pub fn reconcile_outbox(&self) -> Result<usize, CollaborationError> {
        let (mut starts, mut signals, mut quiesces, mut closes) = {
            let mut state = self.lock_state()?;
            let expected_sequence = state.last_event_sequence;
            self.verify_store_sequence(&mut state, expected_sequence, "durable outbox 对账")?;
            let starts = state.start_outbox.values().cloned().collect::<Vec<_>>();
            let signals = state.signal_outbox.values().cloned().collect::<Vec<_>>();
            let quiesces = state.quiesce_outbox.values().cloned().collect::<Vec<_>>();
            let closes = state.close_outbox.values().cloned().collect::<Vec<_>>();
            (starts, signals, quiesces, closes)
        };
        starts.sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
        signals.sort_by(|left, right| {
            (&left.agent_id, &left.turn_id, left.kind).cmp(&(
                &right.agent_id,
                &right.turn_id,
                right.kind,
            ))
        });
        quiesces.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
        closes.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
        let count = starts
            .len()
            .saturating_add(signals.len())
            .saturating_add(quiesces.len())
            .saturating_add(closes.len());
        let mut actions = starts
            .into_iter()
            .map(|launch| PostCommitAction::StartTurn(Box::new(launch)))
            .collect::<Vec<_>>();
        actions.extend(signals.into_iter().map(PostCommitAction::SignalTurn));
        actions.extend(quiesces.into_iter().map(PostCommitAction::QuiesceTree));
        actions.extend(closes.into_iter().map(PostCommitAction::CloseTree));
        let result = self.execute_actions(actions);
        let dispatch = self.request_global_dispatch();
        match (result, dispatch) {
            (Ok(()), Ok(())) => Ok(count),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }

    /// 将一个非活跃子 Agent 从内存驱逐，保留根树身份与路径占用。
    pub fn evict_idle_agent(&self, agent_id: &AgentId) -> Result<(), CollaborationError> {
        let mut state = self.lock_state()?;
        let agent = resident_agent(&state, agent_id)?;
        if agent.definition.depth == AgentDepth::ROOT
            || !agent.status.is_idle()
            || agent.mailbox_claim.is_some()
            || agent.steer_claim.is_some()
        {
            return Err(CollaborationError::TargetNotIdle {
                agent_id: agent_id.clone(),
            });
        }
        let root_agent_id = agent.definition.root_agent_id.clone();
        let recovered = recovered_agent_from_entry(&state, agent)?;
        let revision = state
            .roots
            .get(&root_agent_id)
            .expect("驱逐目标根树应存在")
            .next_checkpoint_revision;
        let checkpoint = RecoveredAgentCheckpoint {
            root_agent_id: root_agent_id.clone(),
            revision,
            agent: recovered,
        };
        self.inner
            .store
            .save_agent_checkpoint(&checkpoint)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?;
        let digest = recovered_agent_checkpoint_digest(&checkpoint);
        let steer_count = checkpoint.agent.pending_steers.len();
        let steer_bytes = checkpoint
            .agent
            .pending_steers
            .iter()
            .map(UserSteer::payload_bytes)
            .sum();
        let initial_triggered_turn_bytes = checkpoint
            .agent
            .mailbox
            .iter()
            .map(|mailbox| {
                mailbox
                    .initial_triggered_turn_id
                    .as_ref()
                    .map_or(0, |turn_id| turn_id.as_str().len())
            })
            .sum();
        let dynamic_text_bytes = recovered_agent_dynamic_text_bytes(&checkpoint.agent);
        let mut candidate = state.clone();
        let root = candidate
            .roots
            .get_mut(&root_agent_id)
            .expect("驱逐目标根树在上方已校验");
        root.next_checkpoint_revision = revision
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        root.evicted_agent_checkpoints.insert(
            agent_id.clone(),
            EvictedAgentCheckpointRef {
                revision,
                digest,
                steer_count,
                steer_bytes,
                initial_triggered_turn_bytes,
                dynamic_text_bytes,
            },
        );
        candidate.agents.remove(agent_id);
        validate_coordinator_quotas(&candidate)?;
        *state = candidate;
        Ok(())
    }
}

/// 从同一个协调器状态生成一棵根树的完整冷恢复快照。
pub(super) fn checkpoint_root_from_state(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<RecoveredAgentTree, CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    let mut agents = state
        .agents
        .values()
        .filter(|agent| &agent.definition.root_agent_id == root_agent_id)
        .map(|agent| recovered_agent_from_entry(state, agent))
        .collect::<Result<Vec<_>, _>>()?;
    agents.sort_by(|left, right| left.definition.path.cmp(&right.definition.path));
    let mut known_agents = root.known_agents.values().cloned().collect::<Vec<_>>();
    known_agents.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RecoveredAgentTree {
        root_agent_id: root.root_agent_id.clone(),
        root_session_id: root.root_session_id.clone(),
        per_root_turn_limit: root.turn_limit,
        lifecycle: root.lifecycle,
        live: true,
        next_turn_sequence: root.next_turn_sequence,
        next_checkpoint_revision: root.next_checkpoint_revision,
        known_agents,
        agents,
    })
}

/// 从同一个候选状态生成事件批次必须原子携带的完整协调器 checkpoint。
pub(super) fn checkpoint_coordinator_from_state(
    state: &CoordinatorState,
    store: &dyn CollaborationStore,
) -> Result<RecoveredCoordinator, CollaborationError> {
    let mut root_ids = state.roots.keys().cloned().collect::<Vec<_>>();
    root_ids.sort();
    let mut roots = root_ids
        .iter()
        .map(|root_agent_id| checkpoint_root_with_store(state, root_agent_id, store))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_recovered_root_order(&mut roots);
    let mut invocations = state
        .collaboration_invocations
        .iter()
        .map(|(key, record)| RecoveredCollaborationInvocation {
            key: key.clone(),
            kind: record.kind,
            input_digest: record.input_digest,
            output: record.output.clone(),
        })
        .collect::<Vec<_>>();
    normalize_recovered_invocation_order(&mut invocations);
    let mut root_turn_bindings = state
        .root_turn_bindings
        .iter()
        .map(|(turn_id, binding)| RecoveredRootTurnBinding {
            turn_id: turn_id.clone(),
            root_agent_id: binding.root_agent_id.clone(),
            prompt_digest: binding.prompt_digest,
            plan_guard: binding.plan_guard,
        })
        .collect::<Vec<_>>();
    root_turn_bindings.sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
    Ok(RecoveredCoordinator {
        last_event_sequence: state.last_event_sequence,
        root_identity_namespace: state.root_identity_namespace.clone(),
        next_root_sequence: state.next_root_sequence,
        roots,
        invocations,
        root_turn_bindings,
    })
}

/// 以驱逐前持久摘要为锚点合并非驻留 Agent，生成真正自包含的 checkpoint。
pub(super) fn checkpoint_root_with_store(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
    store: &dyn CollaborationStore,
) -> Result<RecoveredAgentTree, CollaborationError> {
    let mut checkpoint = checkpoint_root_from_state(state, root_agent_id)?;
    let root = state
        .roots
        .get(root_agent_id)
        .expect("checkpoint 根树在上方已校验");
    let resident_ids = checkpoint
        .agents
        .iter()
        .map(|agent| agent.definition.agent_id.clone())
        .collect::<HashSet<_>>();
    for definition in root.known_agents.values() {
        if resident_ids.contains(&definition.agent_id) {
            continue;
        }
        let expected = root
            .evicted_agent_checkpoints
            .get(&definition.agent_id)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "非驻留 Agent 缺少局部 checkpoint 引用".to_owned(),
            })?;
        let recovered_checkpoint = store
            .load_agent_checkpoint(&definition.agent_id)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: definition.agent_id.clone(),
            })?;
        if recovered_checkpoint.root_agent_id != *root_agent_id
            || recovered_checkpoint.revision != expected.revision
            || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
            || recovered_checkpoint.agent.definition != *definition
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "非驻留 Agent 局部 checkpoint 的根身份、修订号或摘要不一致".to_owned(),
            });
        }
        checkpoint.agents.push(recovered_checkpoint.agent);
    }
    checkpoint
        .agents
        .sort_by(|left, right| left.definition.path.cmp(&right.definition.path));
    if checkpoint.agents.len() != root.known_agents.len() {
        return Err(CollaborationError::InvalidRecovery {
            message: "自包含 checkpoint 未覆盖全部已知 Agent".to_owned(),
        });
    }
    Ok(checkpoint)
}

/// 从不可变 Agent 定义生成确定性排序的幂等全树静止命令。
pub(super) fn quiesce_request_from_definitions(
    root_agent_id: &AgentId,
    root_session_id: &SessionId,
    definitions: &[AgentDefinition],
) -> QuiesceAgentTree {
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|left, right| left.path.cmp(&right.path));
    QuiesceAgentTree {
        root_agent_id: root_agent_id.clone(),
        root_session_id: root_session_id.clone(),
        agent_ids: definitions
            .iter()
            .map(|definition| definition.agent_id.clone())
            .collect(),
    }
}

/// 从不可变 Agent 定义生成确定性排序的幂等全树清理命令。
pub(super) fn close_request_from_definitions(
    root_agent_id: &AgentId,
    root_session_id: &SessionId,
    definitions: &[AgentDefinition],
) -> CloseAgentTree {
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|left, right| left.path.cmp(&right.path));
    let agent_ids = definitions
        .iter()
        .map(|definition| definition.agent_id.clone())
        .collect();
    let mut worktree_leases = definitions
        .iter()
        .filter_map(|definition| definition.profile.worktree_lease.clone())
        .collect::<Vec<_>>();
    worktree_leases.sort();
    CloseAgentTree {
        root_agent_id: root_agent_id.clone(),
        root_session_id: root_session_id.clone(),
        agent_ids,
        worktree_leases,
    }
}

/// 在清理确认事件进入同一提交批次后卸载已关闭根树的全部驻留历史。
pub(super) fn unload_closed_root(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    if root.lifecycle != RecoveredRootLifecycle::CleanupPending || root.in_use != 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "只有已关闭且无槽位占用的根树可以卸载".to_owned(),
        });
    }
    if state
        .pending_turns
        .iter()
        .any(|turn| &turn.root_agent_id == root_agent_id)
        || state
            .active_turns
            .values()
            .any(|turn| &turn.root_agent_id == root_agent_id)
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "卸载已关闭根树时仍存在未决 Turn".to_owned(),
        });
    }
    let agent_ids = root.known_agents.keys().cloned().collect::<HashSet<_>>();
    state
        .collaboration_invocations
        .retain(|key, _record| !agent_ids.contains(&key.source_agent_id));
    state
        .root_turn_bindings
        .retain(|_turn_id, binding| &binding.root_agent_id != root_agent_id);
    state
        .agents
        .retain(|agent_id, _agent| !agent_ids.contains(agent_id));
    state
        .start_outbox
        .retain(|_turn_id, launch| &launch.agent.root_agent_id != root_agent_id);
    state
        .signal_outbox
        .retain(|_turn_id, signal| !agent_ids.contains(&signal.agent_id));
    state.close_outbox.remove(root_agent_id);
    state.roots.remove(root_agent_id);
    Ok(())
}

/// 在关闭根树前按驱逐摘要物化全部非驻留 Agent，使关闭 checkpoint 自包含。
pub(super) fn materialize_evicted_agents_for_root(
    state: &mut CoordinatorState,
    store: &dyn CollaborationStore,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    let mut evicted = root
        .evicted_agent_checkpoints
        .iter()
        .map(|(agent_id, checkpoint_ref)| {
            let definition = root
                .known_agents
                .get(agent_id)
                .expect("驱逐摘要必须对应已知 Agent")
                .clone();
            (
                definition.path.clone(),
                agent_id.clone(),
                checkpoint_ref.clone(),
                definition,
            )
        })
        .collect::<Vec<_>>();
    evicted.sort_by(|left, right| left.0.cmp(&right.0));
    for (_path, agent_id, expected, expected_definition) in evicted {
        let recovered_checkpoint = store
            .load_agent_checkpoint(&agent_id)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: agent_id.clone(),
            })?;
        if recovered_checkpoint.root_agent_id != *root_agent_id
            || recovered_checkpoint.revision != expected.revision
            || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
            || recovered_checkpoint.agent.definition != expected_definition
            || !recovered_checkpoint.agent.status.is_idle()
            || !recovered_terminal_state_matches(&recovered_checkpoint.agent)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "关闭根树时驱逐 Agent 与可信摘要或终态不一致".to_owned(),
            });
        }
        state.agents.insert(
            agent_id.clone(),
            agent_entry_from_recovered(&recovered_checkpoint.agent),
        );
        state
            .roots
            .get_mut(root_agent_id)
            .expect("物化驱逐 Agent 的根树在上方已校验")
            .evicted_agent_checkpoints
            .remove(&agent_id);
    }
    let root = state
        .roots
        .get(root_agent_id)
        .expect("物化驱逐 Agent 的根树在上方已校验");
    let resident_count = state
        .agents
        .values()
        .filter(|agent| &agent.definition.root_agent_id == root_agent_id)
        .count();
    if resident_count != root.known_agents.len() || !root.evicted_agent_checkpoints.is_empty() {
        return Err(CollaborationError::InvalidRecovery {
            message: "关闭根树前没有物化全部已知 Agent".to_owned(),
        });
    }
    Ok(())
}

/// 对同一可信调用身份执行成功重放，输入改变时返回稳定冲突。
pub(super) fn replay_collaboration_invocation(
    state: &CoordinatorState,
    key: &CollaborationInvocationKey,
    input: &CollaborationInvocationInput,
) -> Result<Option<CollaborationInvocationOutput>, CollaborationError> {
    let Some(record) = state.collaboration_invocations.get(key) else {
        return Ok(None);
    };
    let kind = collaboration_invocation_kind(input);
    let input_digest = collaboration_invocation_input_digest(input);
    if record.kind == kind && record.input_digest == input_digest {
        return Ok(Some(record.output.clone()));
    }
    Err(CollaborationError::IdempotencyConflict {
        source_agent_id: key.source_agent_id.clone(),
        source_turn_id: key.source_turn_id.clone(),
        tool_call_id: key.tool_call_id.clone(),
    })
}

/// 按根 Session 的稳定操作身份恢复幂等记录；同一操作不能指向另一目标。
///
/// 根授权入口无法在请求中携带来源 Turn，因此首次提交使用目标旧 Turn 作为
/// 幂等键。目标后续已经完成新的 Turn 时，仍需通过已持久化的目标引用找回该
/// 精确记录；只按来源与 ToolCall 取第一条记录会把另一个目标的操作误当重放。
pub(super) fn replay_root_resume_invocation(
    state: &CoordinatorState,
    source_agent_id: &AgentId,
    tool_call_id: &ToolCallId,
    target_agent_id: &AgentId,
    input: &CollaborationInvocationInput,
) -> Result<Option<CollaborationInvocationOutput>, CollaborationError> {
    replay_root_resume_invocation_records(
        state
            .collaboration_invocations
            .iter()
            .map(|(key, record)| (key, record.kind, &record.input_digest, &record.output)),
        source_agent_id,
        tool_call_id,
        target_agent_id,
        input,
    )
}

/// 在运行态账本中统一执行根恢复的摘要、冲突和重复检测。
pub(super) fn replay_root_resume_invocation_records<'a>(
    records: impl IntoIterator<
        Item = (
            &'a CollaborationInvocationKey,
            CollaborationInvocationKind,
            &'a [u8; 32],
            &'a CollaborationInvocationOutput,
        ),
    >,
    source_agent_id: &AgentId,
    tool_call_id: &ToolCallId,
    target_agent_id: &AgentId,
    input: &CollaborationInvocationInput,
) -> Result<Option<CollaborationInvocationOutput>, CollaborationError> {
    let input_digest = collaboration_invocation_input_digest(input);
    let mut matching = None;
    let mut conflict = None;
    for (key, kind, record_input_digest, output) in records {
        if key.source_agent_id != *source_agent_id
            || key.tool_call_id != *tool_call_id
            || kind != CollaborationInvocationKind::ResumeAgent
        {
            continue;
        }
        let output_target = match output {
            CollaborationInvocationOutput::ResumedAgent {
                target_agent_id, ..
            } => target_agent_id,
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "ResumeAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                });
            }
        };
        if output_target != target_agent_id || *record_input_digest != input_digest {
            conflict = Some(key.clone());
            continue;
        }
        if matching.replace(output.clone()).is_some() {
            conflict = Some(key.clone());
        }
    }
    if let Some(key) = conflict {
        return Err(CollaborationError::IdempotencyConflict {
            source_agent_id: key.source_agent_id,
            source_turn_id: key.source_turn_id,
            tool_call_id: key.tool_call_id,
        });
    }
    Ok(matching)
}

/// 将首次成功结果写入当前候选状态，禁止覆盖任何既有可信调用身份。
pub(super) fn record_collaboration_invocation(
    state: &mut CoordinatorState,
    key: CollaborationInvocationKey,
    input: CollaborationInvocationInput,
    output: CollaborationInvocationOutput,
) -> Result<CollaborationInvocationReceipt, CollaborationError> {
    let kind = collaboration_invocation_kind(&input);
    if !collaboration_invocation_types_match(kind, &output) {
        return Err(CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的输入与结果类型不匹配".to_owned(),
        });
    }
    if state.collaboration_invocations.contains_key(&key) {
        return Err(CollaborationError::IdempotencyConflict {
            source_agent_id: key.source_agent_id,
            source_turn_id: key.source_turn_id,
            tool_call_id: key.tool_call_id,
        });
    }
    let input_digest = collaboration_invocation_input_digest(&input);
    let receipt = CollaborationInvocationReceipt {
        key: key.clone(),
        kind,
        input_digest,
        output: output.clone(),
    };
    state.collaboration_invocations.insert(
        key,
        CollaborationInvocationRecord {
            kind,
            input_digest,
            output,
        },
    );
    Ok(receipt)
}

/// 返回完整临时业务输入对应的不含正文操作类型。
pub(super) fn collaboration_invocation_kind(
    input: &CollaborationInvocationInput,
) -> CollaborationInvocationKind {
    match input {
        CollaborationInvocationInput::SpawnAgent(_) => CollaborationInvocationKind::SpawnAgent,
        CollaborationInvocationInput::SendMessage { .. } => {
            CollaborationInvocationKind::SendMessage
        }
        CollaborationInvocationInput::StopAgent { .. } => CollaborationInvocationKind::StopAgent,
        CollaborationInvocationInput::SteerAgent { .. } => CollaborationInvocationKind::SteerAgent,
        CollaborationInvocationInput::RetryAgent { .. } => CollaborationInvocationKind::RetryAgent,
        CollaborationInvocationInput::ResumeAgent { .. } => {
            CollaborationInvocationKind::ResumeAgent
        }
    }
}

/// 判断持久幂等记录是否保存了与操作类型一致的结果。
pub(super) fn collaboration_invocation_types_match(
    kind: CollaborationInvocationKind,
    output: &CollaborationInvocationOutput,
) -> bool {
    matches!(
        (kind, output),
        (
            CollaborationInvocationKind::SpawnAgent,
            CollaborationInvocationOutput::SpawnedAgent(_)
        ) | (
            CollaborationInvocationKind::SendMessage,
            CollaborationInvocationOutput::Message { .. }
        ) | (
            CollaborationInvocationKind::StopAgent,
            CollaborationInvocationOutput::StoppedAgent { .. }
        ) | (
            CollaborationInvocationKind::SteerAgent,
            CollaborationInvocationOutput::UserSteer(_)
        ) | (
            CollaborationInvocationKind::RetryAgent,
            CollaborationInvocationOutput::RetriedAgent { .. }
        ) | (
            CollaborationInvocationKind::ResumeAgent,
            CollaborationInvocationOutput::ResumedAgent { .. }
        )
    )
}

/// 校验根树存在且尚未关闭。
pub(super) fn ensure_tree_open(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    if root.lifecycle != RecoveredRootLifecycle::Open || root.suspended {
        return Err(CollaborationError::TreeClosed {
            root_agent_id: root_agent_id.clone(),
        });
    }
    Ok(())
}

/// 当目标 Agent 未驻留时从 Session Store 恢复并校验其所有者和父链。
pub(super) fn ensure_agent_loaded(
    state: &mut CoordinatorState,
    store: &dyn CollaborationStore,
    source_agent_id: &AgentId,
    target_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    if state.agents.contains_key(target_agent_id) {
        return Ok(());
    }
    let source = resident_agent(state, source_agent_id)?;
    let source_root_agent_id = source.definition.root_agent_id.clone();
    let root = state
        .roots
        .get(&source_root_agent_id)
        .expect("来源 Agent 所属根树已驻留");
    let expected_target_definition =
        root.known_agents
            .get(target_agent_id)
            .cloned()
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: target_agent_id.clone(),
            })?;
    let expected = root
        .evicted_agent_checkpoints
        .get(target_agent_id)
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "非驻留目标缺少局部 checkpoint 引用".to_owned(),
        })?;
    let recovered_checkpoint = store
        .load_agent_checkpoint(target_agent_id)
        .map_err(|error| CollaborationError::Store {
            message: error.message().to_owned(),
        })?
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "已知 Agent 缺少局部 checkpoint".to_owned(),
        })?;
    let recovered_agent = &recovered_checkpoint.agent;
    if recovered_agent.definition != expected_target_definition
        || recovered_checkpoint.root_agent_id != source_root_agent_id
        || recovered_checkpoint.revision != expected.revision
        || !recovered_agent.status.is_idle()
        || !recovered_terminal_state_matches(recovered_agent)
        || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "冷加载 Agent 的定义、终态或驱逐摘要不一致".to_owned(),
        });
    }
    let entry = agent_entry_from_recovered(recovered_agent);
    state.agents.insert(target_agent_id.clone(), entry);
    state
        .roots
        .get_mut(&source_root_agent_id)
        .expect("冷加载目标所属根树应存在")
        .evicted_agent_checkpoints
        .remove(target_agent_id);
    Ok(())
}

/// 从完整协调器 checkpoint 校验并恢复全部协作工具幂等记录。
pub(super) fn restore_collaboration_invocations(
    state: &mut CoordinatorState,
    recovered: &[RecoveredCollaborationInvocation],
) -> Result<(), CollaborationError> {
    let mut records = HashMap::with_capacity(recovered.len());
    let mut spawned_agent_ids = HashSet::new();
    let mut message_ids = HashSet::new();
    for invocation in recovered {
        if invocation.key.source_agent_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
            || invocation.key.source_turn_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
            || !collaboration_invocation_types_match(invocation.kind, &invocation.output)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作工具幂等键越界或输入与结果类型不匹配".to_owned(),
            });
        }
        if records.contains_key(&invocation.key) {
            return Err(CollaborationError::InvalidRecovery {
                message: "协调器 checkpoint 包含重复协作工具幂等键".to_owned(),
            });
        }
        validate_recovered_collaboration_invocation(
            state,
            invocation,
            &mut spawned_agent_ids,
            &mut message_ids,
        )?;
        records.insert(
            invocation.key.clone(),
            CollaborationInvocationRecord {
                kind: invocation.kind,
                input_digest: invocation.input_digest,
                output: invocation.output.clone(),
            },
        );
    }
    if state
        .agents
        .values()
        .flat_map(|agent| agent.mailbox.iter())
        .filter(|entry| matches!(entry.message.kind, MailboxMessageKind::AgentMessage))
        .any(|entry| !message_ids.contains(&entry.message.message_id))
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "未消费 Agent mailbox 消息缺少唯一 SendMessage 幂等记录".to_owned(),
        });
    }
    state.collaboration_invocations = records;
    Ok(())
}

/// 校验非活跃 Agent 状态与最近 Turn 快照严格对应。
pub(super) fn recovered_terminal_state_matches(agent: &RecoveredAgent) -> bool {
    match (&agent.status, &agent.last_turn) {
        (CollaborationAgentStatus::Idle, None) => true,
        (
            CollaborationAgentStatus::Completed {
                turn_id,
                final_message,
            },
            Some(last_turn),
        ) => {
            &last_turn.turn_id == turn_id
                && matches!(
                    &last_turn.outcome,
                    AgentTurnOutcome::Completed {
                        final_message: recovered_message
                    } if recovered_message == final_message
                )
        }
        (CollaborationAgentStatus::Interrupted { turn_id }, Some(last_turn)) => {
            &last_turn.turn_id == turn_id
                && matches!(&last_turn.outcome, AgentTurnOutcome::Interrupted)
        }
        (CollaborationAgentStatus::Failed { turn_id, message }, Some(last_turn)) => {
            &last_turn.turn_id == turn_id
                && matches!(
                    &last_turn.outcome,
                    AgentTurnOutcome::Failed {
                        message: recovered_message
                    } if recovered_message == message
                )
        }
        _ => false,
    }
}

/// 返回快照状态中尚未收敛的当前 Turn 标识。
pub(super) fn recovered_current_turn_id(status: &CollaborationAgentStatus) -> Option<&TurnId> {
    match status {
        CollaborationAgentStatus::WaitingCapacity { turn_id }
        | CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => Some(turn_id),
        _ => None,
    }
}

/// 返回恢复 Agent 当前或最近 Turn 的规范排序文本。
pub(super) fn recovered_agent_sort_turn_id(agent: &RecoveredAgent) -> &str {
    recovered_current_turn_id(&agent.status)
        .or_else(|| agent.last_turn.as_ref().map(|turn| &turn.turn_id))
        .map_or("", TurnId::as_str)
}

/// 按 `(root_agent_id, agent_path, turn_id)` 规范化多根恢复顺序。
pub(super) fn normalize_recovered_root_order(roots: &mut [RecoveredAgentTree]) {
    for tree in roots.iter_mut() {
        tree.known_agents
            .sort_by(|left, right| left.path.cmp(&right.path));
        tree.agents.sort_by(|left, right| {
            (&left.definition.path, recovered_agent_sort_turn_id(left))
                .cmp(&(&right.definition.path, recovered_agent_sort_turn_id(right)))
        });
    }
    roots.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
}

/// 按 `(source_agent_id, source_turn_id, tool_call_id)` 规范化幂等记录顺序。
pub(super) fn normalize_recovered_invocation_order(
    invocations: &mut [RecoveredCollaborationInvocation],
) {
    invocations.sort_by(|left, right| left.key.cmp(&right.key));
}

/// 将已经完整校验的冷恢复 Agent 转换为驻留状态与新的唤醒通道。
pub(super) fn agent_entry_from_recovered(agent: &RecoveredAgent) -> AgentEntry {
    let (sender, _receiver) = watch::channel(0);
    let completion_count = agent
        .mailbox
        .iter()
        .filter(|entry| {
            matches!(
                &entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
        })
        .count();
    let completion_bytes = agent
        .mailbox
        .iter()
        .filter(|entry| {
            matches!(
                &entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
        })
        .map(|entry| entry.message.content.len())
        .sum();
    AgentEntry {
        definition: agent.definition.clone(),
        status: agent.status.clone(),
        mailbox_bytes: agent
            .mailbox
            .iter()
            .map(|entry| entry.message.content.len())
            .sum(),
        completion_count,
        completion_bytes,
        mailbox: agent
            .mailbox
            .iter()
            .cloned()
            .map(|entry| MailboxEntry {
                message: entry.message,
                initial_triggered_turn_id: entry.initial_triggered_turn_id,
                claimed_turn_id: entry.claimed_turn_id,
            })
            .collect(),
        next_mailbox_sequence: agent.next_mailbox_sequence,
        mailbox_claim: agent
            .mailbox_claim_turn_id
            .clone()
            .zip(agent.mailbox_claim_through_sequence)
            .map(|(turn_id, through_sequence)| InputBatchClaim {
                turn_id,
                through_sequence,
            }),
        steers: agent.pending_steers.iter().cloned().collect(),
        steer_bytes: agent
            .pending_steers
            .iter()
            .map(UserSteer::payload_bytes)
            .sum(),
        next_steer_sequence: agent.next_steer_sequence,
        steer_claim: agent
            .steer_claim_turn_id
            .clone()
            .zip(agent.steer_claim_through_sequence)
            .map(|(turn_id, through_sequence)| InputBatchClaim {
                turn_id,
                through_sequence,
            }),
        last_turn: agent.last_turn.clone().map(|turn| TurnRecord {
            turn_id: turn.turn_id,
            cause: turn.cause,
            prompt: turn.prompt,
            parent_turn_id: turn.parent_turn_id,
            root_turn_id: turn.root_turn_id,
            outcome: turn.outcome,
        }),
        activity_version: 0,
        activity_sender: sender,
    }
}

/// 将驻留 Agent 投影为 Session Store 定期快照数据。
pub(super) fn recovered_agent_from_entry(
    state: &CoordinatorState,
    agent: &AgentEntry,
) -> Result<RecoveredAgent, CollaborationError> {
    let current = match &agent.status {
        CollaborationAgentStatus::WaitingCapacity { turn_id } => {
            let queued = state
                .pending_turns
                .iter()
                .find(|queued| {
                    queued.agent_id == agent.definition.agent_id && &queued.turn_id == turn_id
                })
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "等待容量 Agent 缺少 pending Turn".to_owned(),
                })?;
            Some((
                queued.source_agent_id.clone(),
                queued.cause.clone(),
                queued.prompt.clone(),
                queued.parent_turn_id.clone(),
                queued.root_turn_id.clone(),
                queued.plan_guard,
                false,
            ))
        }
        CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => {
            let active = state.active_turns.get(turn_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "活跃 Agent 缺少 active Turn".to_owned(),
                }
            })?;
            Some((
                active.source_agent_id.clone(),
                active.cause.clone(),
                active.prompt.clone(),
                active.parent_turn_id.clone(),
                active.root_turn_id.clone(),
                active.plan_guard,
                state.start_outbox.contains_key(turn_id),
            ))
        }
        _ => None,
    };
    Ok(RecoveredAgent {
        definition: agent.definition.clone(),
        status: agent.status.clone(),
        mailbox: agent
            .mailbox
            .iter()
            .map(|entry| RecoveredMailboxMessage {
                message: entry.message.clone(),
                initial_triggered_turn_id: entry.initial_triggered_turn_id.clone(),
                claimed_turn_id: entry.claimed_turn_id.clone(),
            })
            .collect(),
        next_mailbox_sequence: agent.next_mailbox_sequence,
        mailbox_claim_turn_id: agent
            .mailbox_claim
            .as_ref()
            .map(|claim| claim.turn_id.clone()),
        mailbox_claim_through_sequence: agent
            .mailbox_claim
            .as_ref()
            .map(|claim| claim.through_sequence),
        next_steer_sequence: agent.next_steer_sequence,
        steer_claim_turn_id: agent
            .steer_claim
            .as_ref()
            .map(|claim| claim.turn_id.clone()),
        steer_claim_through_sequence: agent
            .steer_claim
            .as_ref()
            .map(|claim| claim.through_sequence),
        last_turn: agent.last_turn.as_ref().map(|turn| RecoveredTurn {
            turn_id: turn.turn_id.clone(),
            cause: turn.cause.clone(),
            prompt: turn.prompt.clone(),
            parent_turn_id: turn.parent_turn_id.clone(),
            root_turn_id: turn.root_turn_id.clone(),
            outcome: turn.outcome.clone(),
        }),
        current_source_agent_id: current.as_ref().map(|current| current.0.clone()),
        current_turn_cause: current.as_ref().map(|current| current.1.clone()),
        current_turn_prompt: current.as_ref().and_then(|current| current.2.clone()),
        current_parent_turn_id: current.as_ref().and_then(|current| current.3.clone()),
        current_root_turn_id: current.as_ref().map(|current| current.4.clone()),
        current_plan_guard: current.as_ref().map(|current| current.5),
        pending_steers: agent.steers.iter().cloned().collect(),
        start_pending: current.as_ref().is_some_and(|current| current.6),
    })
}
