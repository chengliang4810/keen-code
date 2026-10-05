//! 协作领域的规范化编码与字节计量。
//!
//! CanonicalDigest 提供版本化、定长标签和长度前缀的 SHA-256 累加器，
//! 供事件去重、调用幂等和恢复校验使用。

use super::*;

/// 对稳定领域值进行版本化、定长标签和长度前缀编码的 SHA-256 累加器。
pub(super) struct CanonicalDigest {
    /// 当前规范编码的增量摘要状态。
    digest: Sha256,
}

impl CanonicalDigest {
    /// 以不可变格式版本前缀开始一次规范编码。
    pub(super) fn new(version: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update((version.len() as u64).to_be_bytes());
        digest.update(version);
        Self { digest }
    }

    /// 编码一个枚举或结构字段标签。
    pub(super) fn tag(&mut self, value: u8) {
        self.digest.update([value]);
    }

    /// 编码一个布尔值。
    pub(super) fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    /// 以大端固定宽度编码无符号整数。
    pub(super) fn u64(&mut self, value: u64) {
        self.digest.update(value.to_be_bytes());
    }

    /// 以 UTF-8 字节长度和原始字节编码文本。
    pub(super) fn text(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.digest.update(value.as_bytes());
    }

    /// 完成摘要并返回固定小写十六进制文本。
    pub(super) fn finish_hex(self) -> String {
        let bytes = self.finish_bytes();
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
        }
        encoded
    }

    /// 完成摘要并返回固定 32 字节结果。
    pub(super) fn finish_bytes(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

/// 非空引用进入所有摘要边界；无引用请求保持原来的正文摘要。
fn encode_input_references(
    encoder: &mut CanonicalDigest,
    references: &[keencode_model::InputReference],
) {
    if references.is_empty() {
        return;
    }
    encoder.tag(254);
    encoder.u64(references.len() as u64);
    for reference in references {
        encoder.text(&reference.name);
        encoder.text(&reference.path);
    }
}

/// 规范编码一个可选文本。
pub(super) fn encode_optional_text(encoder: &mut CanonicalDigest, value: Option<&str>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value);
    }
}

/// 规范编码一个可选 Turn 标识。
pub(super) fn encode_optional_turn(encoder: &mut CanonicalDigest, value: Option<&TurnId>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value.as_str());
    }
}

/// 规范编码一个可选 Agent 标识。
pub(super) fn encode_optional_agent(encoder: &mut CanonicalDigest, value: Option<&AgentId>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value.as_str());
    }
}

/// 规范编码平台路径；持久层必须按同一文本恢复后再计算批次标识。
pub(super) fn encode_path(encoder: &mut CanonicalDigest, value: &Path) {
    encoder.text(&value.as_os_str().to_string_lossy());
}

/// 规范编码上下文继承策略。
pub(super) fn encode_context_inheritance(
    encoder: &mut CanonicalDigest,
    inheritance: &ContextInheritance,
) {
    match inheritance {
        ContextInheritance::None => encoder.tag(0),
        ContextInheritance::All => encoder.tag(1),
        ContextInheritance::RecentTurns { count } => {
            encoder.tag(2);
            encoder.u64(u64::from(*count));
        }
    }
}

/// 规范编码 Agent 创建时冻结的运行配置。
pub(super) fn encode_agent_profile(encoder: &mut CanonicalDigest, profile: &AgentProfile) {
    encoder.text(&profile.model);
    encode_optional_text(encoder, profile.reasoning_effort.as_deref());
    encoder.tag(match profile.plan_guard.state() {
        PlanGuardState::Inactive => 0,
        PlanGuardState::ReadOnly => 1,
    });
    encode_path(encoder, &profile.cwd);
    encoder.bool(profile.worktree_lease.is_some());
    if let Some(worktree_lease) = &profile.worktree_lease {
        encoder.text(worktree_lease.as_str());
    }
    encoder.u64(profile.tool_snapshot.len() as u64);
    for tool_name in &profile.tool_snapshot {
        encoder.text(tool_name);
    }
}

/// 规范编码 spawn 前冻结的扩展 Agent 模板。
pub(super) fn encode_agent_template_snapshot(
    encoder: &mut CanonicalDigest,
    template: Option<&AgentTemplateSnapshot>,
) {
    encoder.bool(template.is_some());
    if let Some(template) = template {
        encoder.bool(template.inject_agents_md);
        encoder.text(&template.name);
        encoder.text(&template.system_prompt);
        encoder.bool(template.max_turns.is_some());
        if let Some(max_turns) = template.max_turns {
            encoder.u64(u64::from(max_turns));
        }
        encoder.u64(template.allowed_write_dirs.len() as u64);
        for directory in &template.allowed_write_dirs {
            encode_path(encoder, directory);
        }
    }
}

/// 规范编码 Agent 创建时冻结的完整定义。
pub(super) fn encode_agent_definition(encoder: &mut CanonicalDigest, definition: &AgentDefinition) {
    encoder.text(definition.agent_id.as_str());
    encoder.text(definition.session_id.as_str());
    encoder.text(definition.root_agent_id.as_str());
    encoder.text(definition.root_session_id.as_str());
    encode_optional_agent(encoder, definition.parent_agent_id.as_ref());
    encoder.text(definition.path.as_str());
    encode_optional_text(encoder, definition.assignment.as_deref());
    encoder.tag(definition.depth.value());
    encode_context_inheritance(encoder, &definition.context_inheritance);
    encoder.u64(definition.context_snapshot.len() as u64);
    for message in &definition.context_snapshot {
        encoder.text(message);
    }
    encode_agent_template_snapshot(encoder, definition.agent_template.as_ref());
    encode_agent_profile(encoder, &definition.profile);
}

/// 规范编码协作工具的可信幂等键。
pub(super) fn encode_collaboration_invocation_key(
    encoder: &mut CanonicalDigest,
    key: &CollaborationInvocationKey,
) {
    encoder.text(key.source_agent_id.as_str());
    encoder.text(key.source_turn_id.as_str());
    encoder.text(key.tool_call_id.as_str());
}

/// 规范编码协作工具的完整强类型业务输入。
pub(super) fn encode_collaboration_invocation_input(
    encoder: &mut CanonicalDigest,
    input: &CollaborationInvocationInput,
) {
    match input {
        CollaborationInvocationInput::SpawnAgent(request) => {
            encoder.tag(0);
            encoder.text(&request.task_name);
            encoder.text(&request.initial_task);
            encoder.text(&request.assignment);
            encode_context_inheritance(encoder, &request.context_inheritance);
            encoder.u64(request.context_snapshot.len() as u64);
            for message in &request.context_snapshot {
                encoder.text(message);
            }
            encode_agent_template_snapshot(encoder, request.agent_template.as_ref());
            encode_agent_profile(encoder, &request.profile);
        }
        CollaborationInvocationInput::SendMessage {
            target_agent_id,
            content,
            delivery,
        } => {
            encoder.tag(1);
            encoder.text(target_agent_id.as_str());
            encoder.text(content);
            encoder.tag(match delivery {
                MailboxDelivery::QueueOnly => 0,
                MailboxDelivery::TriggerTurn => 1,
            });
        }
        CollaborationInvocationInput::StopAgent { target_agent_id } => {
            encoder.tag(2);
            encoder.text(target_agent_id.as_str());
        }
        CollaborationInvocationInput::SteerAgent {
            target_agent_id,
            content,
            references,
        } => {
            encoder.tag(3);
            encoder.text(target_agent_id.as_str());
            encoder.text(content);
            encode_input_references(encoder, references);
        }
        CollaborationInvocationInput::RetryAgent { target_agent_id } => {
            encoder.tag(4);
            encoder.text(target_agent_id.as_str());
        }
        CollaborationInvocationInput::ResumeAgent { target_agent_id } => {
            encoder.tag(5);
            encoder.text(target_agent_id.as_str());
        }
    }
}

/// 对完整规范协作工具输入计算版本化 SHA-256 摘要。
pub(super) fn collaboration_invocation_input_digest(
    input: &CollaborationInvocationInput,
) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.invocation-input.v3");
    encode_collaboration_invocation_input(&mut encoder, input);
    encoder.finish_bytes()
}

/// 对外部根 Turn 的完整用户输入计算版本化摘要，供跨恢复幂等绑定使用。
pub fn root_turn_prompt_digest(prompt: &str) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.root-turn-prompt.v1");
    encoder.text(prompt);
    encoder.finish_bytes()
}

/// 规范编码协作工具首次提交的成功结果。
pub(super) fn encode_collaboration_invocation_output(
    encoder: &mut CanonicalDigest,
    output: &CollaborationInvocationOutput,
) {
    match output {
        CollaborationInvocationOutput::SpawnedAgent(spawned) => {
            encoder.tag(0);
            encoder.text(spawned.agent.agent_id.as_str());
            encoder.text(spawned.agent.session_id.as_str());
            encoder.text(spawned.agent.path.as_str());
            encoder.text(spawned.initial_turn_id.as_str());
        }
        CollaborationInvocationOutput::Message {
            message_id,
            triggered_turn_id,
        } => {
            encoder.tag(1);
            encoder.text(message_id.as_str());
            encode_optional_turn(encoder, triggered_turn_id.as_ref());
        }
        CollaborationInvocationOutput::StoppedAgent {
            target_agent_id,
            stopped_turn_id,
        } => {
            encoder.tag(2);
            encoder.text(target_agent_id.as_str());
            encoder.text(stopped_turn_id.as_str());
        }
        CollaborationInvocationOutput::UserSteer(steer) => {
            encoder.tag(3);
            encoder.u64(steer.sequence);
            encoder.text(steer.turn_id.as_str());
            encoder.text(&steer.content);
            encode_input_references(encoder, &steer.references);
        }
        CollaborationInvocationOutput::RetriedAgent {
            target_agent_id,
            retry_turn_id,
        } => {
            encoder.tag(4);
            encoder.text(target_agent_id.as_str());
            encoder.text(retry_turn_id.as_str());
        }
        CollaborationInvocationOutput::ResumedAgent {
            target_agent_id,
            resume_turn_id,
        } => {
            encoder.tag(5);
            encoder.text(target_agent_id.as_str());
            encoder.text(resume_turn_id.as_str());
        }
    }
}

/// 规范编码与业务事件同批提交的协作幂等凭据。
pub(super) fn encode_collaboration_invocation_receipt(
    encoder: &mut CanonicalDigest,
    receipt: &CollaborationInvocationReceipt,
) {
    encode_collaboration_invocation_key(encoder, &receipt.key);
    encoder.tag(match receipt.kind {
        CollaborationInvocationKind::SpawnAgent => 0,
        CollaborationInvocationKind::SendMessage => 1,
        CollaborationInvocationKind::StopAgent => 2,
        CollaborationInvocationKind::SteerAgent => 3,
        CollaborationInvocationKind::RetryAgent => 4,
        CollaborationInvocationKind::ResumeAgent => 5,
    });
    encoder.u64(receipt.input_digest.len() as u64);
    for byte in receipt.input_digest {
        encoder.tag(byte);
    }
    encode_collaboration_invocation_output(encoder, &receipt.output);
}

/// 规范编码 Agent 调度状态。
pub(super) fn encode_agent_status(
    encoder: &mut CanonicalDigest,
    status: &CollaborationAgentStatus,
) {
    match status {
        CollaborationAgentStatus::PendingInit => encoder.tag(0),
        CollaborationAgentStatus::Idle => encoder.tag(1),
        CollaborationAgentStatus::WaitingCapacity { turn_id } => {
            encoder.tag(2);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Running { turn_id } => {
            encoder.tag(3);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Cancelling { turn_id } => {
            encoder.tag(4);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Completed {
            turn_id,
            final_message,
        } => {
            encoder.tag(5);
            encoder.text(turn_id.as_str());
            encode_optional_text(encoder, final_message.as_deref());
        }
        CollaborationAgentStatus::Interrupted { turn_id } => {
            encoder.tag(6);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Failed { turn_id, message } => {
            encoder.tag(7);
            encoder.text(turn_id.as_str());
            encoder.text(message);
        }
        CollaborationAgentStatus::Stopped => encoder.tag(8),
    }
}

/// 规范编码 Turn 的触发原因。
pub(super) fn encode_turn_cause(encoder: &mut CanonicalDigest, cause: &AgentTurnCause) {
    match cause {
        AgentTurnCause::RootUser => encoder.tag(0),
        AgentTurnCause::InitialTask => encoder.tag(1),
        AgentTurnCause::Followup { message_id } => {
            encoder.tag(2);
            encoder.text(message_id.as_str());
        }
        AgentTurnCause::Retry { previous_turn_id } => {
            encoder.tag(3);
            encoder.text(previous_turn_id.as_str());
        }
    }
}

/// 规范编码 Turn 终态。
pub(super) fn encode_turn_outcome(encoder: &mut CanonicalDigest, outcome: &AgentTurnOutcome) {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            encoder.tag(0);
            encode_optional_text(encoder, final_message.as_deref());
        }
        AgentTurnOutcome::Interrupted => encoder.tag(1),
        AgentTurnOutcome::Failed { message } => {
            encoder.tag(2);
            encoder.text(message);
        }
    }
}

/// 规范编码 mailbox 消息和完整因果字段。
pub(super) fn encode_mailbox_message(encoder: &mut CanonicalDigest, message: &MailboxMessage) {
    encoder.text(message.message_id.as_str());
    encoder.u64(message.sequence);
    encoder.text(message.source_agent_id.as_str());
    encoder.text(message.source_agent_path.as_str());
    encoder.tag(match message.source_plan_guard.state() {
        PlanGuardState::Inactive => 0,
        PlanGuardState::ReadOnly => 1,
    });
    encoder.text(message.target_agent_id.as_str());
    encoder.tag(match message.delivery {
        MailboxDelivery::QueueOnly => 0,
        MailboxDelivery::TriggerTurn => 1,
    });
    match &message.kind {
        MailboxMessageKind::AgentMessage => encoder.tag(0),
        MailboxMessageKind::ChildTurnFinished { outcome } => {
            encoder.tag(1);
            encode_turn_outcome(encoder, outcome);
        }
    }
    encoder.text(&message.content);
    encode_optional_turn(encoder, message.related_turn_id.as_ref());
    encode_optional_turn(encoder, message.parent_turn_id.as_ref());
    encode_optional_turn(encoder, message.root_turn_id.as_ref());
}

/// 规范编码一个 Collaboration 领域事件，新增字段必须在此显式纳入批次身份。
pub(super) fn encode_collaboration_event(
    encoder: &mut CanonicalDigest,
    event: &CollaborationEvent,
) {
    encoder.text(event.session_id.as_str());
    encode_optional_turn(encoder, event.turn_id.as_ref());
    encoder.text(event.source_agent_id.as_str());
    encoder.text(event.agent_id.as_str());
    encode_optional_agent(encoder, event.parent_agent_id.as_ref());
    encoder.text(event.agent_path.as_str());
    encode_optional_turn(encoder, event.parent_turn_id.as_ref());
    encode_optional_turn(encoder, event.root_turn_id.as_ref());
    encoder.u64(event.sequence);
    match &event.kind {
        CollaborationEventKind::AgentSpawned {
            definition,
            initial_status,
            per_root_turn_limit,
        } => {
            encoder.tag(0);
            encode_agent_definition(encoder, definition);
            encode_agent_status(encoder, initial_status);
            encoder.bool(per_root_turn_limit.is_some());
            if let Some(limit) = per_root_turn_limit {
                encoder.u64(*limit as u64);
            }
        }
        CollaborationEventKind::AgentStatusChanged { previous, current } => {
            encoder.tag(1);
            encode_agent_status(encoder, previous);
            encode_agent_status(encoder, current);
        }
        CollaborationEventKind::AgentMessageQueued { message } => {
            encoder.tag(2);
            encode_mailbox_message(encoder, message);
        }
        CollaborationEventKind::AgentMessagesConsumed {
            message_ids,
            through_sequence,
        } => {
            encoder.tag(3);
            encoder.u64(message_ids.len() as u64);
            for message_id in message_ids {
                encoder.text(message_id.as_str());
            }
            encoder.u64(*through_sequence);
        }
        CollaborationEventKind::AgentCompletionNotificationSuperseded { message_id } => {
            encoder.tag(4);
            encoder.text(message_id.as_str());
        }
        CollaborationEventKind::AgentTurnQueued { cause, prompt } => {
            encoder.tag(5);
            encode_turn_cause(encoder, cause);
            encode_optional_text(encoder, prompt.as_deref());
        }
        CollaborationEventKind::AgentTurnStarted { cause } => {
            encoder.tag(6);
            encode_turn_cause(encoder, cause);
        }
        CollaborationEventKind::AgentTurnDispatchAcknowledged => encoder.tag(7),
        CollaborationEventKind::AgentTurnCompleted { final_message } => {
            encoder.tag(8);
            encode_optional_text(encoder, final_message.as_deref());
        }
        CollaborationEventKind::AgentTurnInterrupted => encoder.tag(9),
        CollaborationEventKind::AgentTurnFailed { message } => {
            encoder.tag(10);
            encoder.text(message);
        }
        CollaborationEventKind::AgentUserSteered { steer } => {
            encoder.tag(11);
            encoder.u64(steer.sequence);
            encoder.text(steer.turn_id.as_str());
            encoder.text(&steer.content);
            encode_input_references(encoder, &steer.references);
        }
        CollaborationEventKind::AgentUserSteersConsumed { sequences } => {
            encoder.tag(12);
            encoder.u64(sequences.len() as u64);
            for sequence in sequences {
                encoder.u64(*sequence);
            }
        }
        CollaborationEventKind::AgentTreeClosing => encoder.tag(13),
        CollaborationEventKind::AgentTreeQuiesced => encoder.tag(14),
        CollaborationEventKind::AgentTreeCleanupCompleted => encoder.tag(15),
        CollaborationEventKind::CollaborationInvocationCommitted { receipt } => {
            encoder.tag(16);
            encode_collaboration_invocation_receipt(encoder, receipt);
        }
        CollaborationEventKind::AgentMessagesClaimed {
            message_ids,
            through_sequence,
        } => {
            encoder.tag(17);
            encoder.u64(message_ids.len() as u64);
            for message_id in message_ids {
                encoder.text(message_id.as_str());
            }
            encoder.u64(*through_sequence);
        }
        CollaborationEventKind::AgentUserSteersClaimed { sequences } => {
            encoder.tag(18);
            encoder.u64(sequences.len() as u64);
            for sequence in sequences {
                encoder.u64(*sequence);
            }
        }
    }
}

/// 规范编码可恢复 Turn 快照。
pub(super) fn encode_recovered_turn(encoder: &mut CanonicalDigest, turn: &RecoveredTurn) {
    encoder.text(turn.turn_id.as_str());
    encode_turn_cause(encoder, &turn.cause);
    encode_optional_text(encoder, turn.prompt.as_deref());
    encode_optional_turn(encoder, turn.parent_turn_id.as_ref());
    encoder.text(turn.root_turn_id.as_str());
    encode_turn_outcome(encoder, &turn.outcome);
}

/// 规范编码一个完整单 Agent 恢复状态。
pub(super) fn encode_recovered_agent(encoder: &mut CanonicalDigest, agent: &RecoveredAgent) {
    encode_agent_definition(encoder, &agent.definition);
    encode_agent_status(encoder, &agent.status);
    encoder.u64(agent.mailbox.len() as u64);
    for mailbox in &agent.mailbox {
        encode_mailbox_message(encoder, &mailbox.message);
        encode_optional_turn(encoder, mailbox.initial_triggered_turn_id.as_ref());
        encode_optional_turn(encoder, mailbox.claimed_turn_id.as_ref());
    }
    encoder.u64(agent.next_mailbox_sequence);
    encode_optional_turn(encoder, agent.mailbox_claim_turn_id.as_ref());
    encoder.bool(agent.mailbox_claim_through_sequence.is_some());
    if let Some(sequence) = agent.mailbox_claim_through_sequence {
        encoder.u64(sequence);
    }
    encoder.u64(agent.next_steer_sequence);
    encode_optional_turn(encoder, agent.steer_claim_turn_id.as_ref());
    encoder.bool(agent.steer_claim_through_sequence.is_some());
    if let Some(sequence) = agent.steer_claim_through_sequence {
        encoder.u64(sequence);
    }
    encoder.bool(agent.last_turn.is_some());
    if let Some(turn) = &agent.last_turn {
        encode_recovered_turn(encoder, turn);
    }
    encode_optional_agent(encoder, agent.current_source_agent_id.as_ref());
    encoder.bool(agent.current_turn_cause.is_some());
    if let Some(cause) = &agent.current_turn_cause {
        encode_turn_cause(encoder, cause);
    }
    encode_optional_text(encoder, agent.current_turn_prompt.as_deref());
    encode_optional_turn(encoder, agent.current_parent_turn_id.as_ref());
    encode_optional_turn(encoder, agent.current_root_turn_id.as_ref());
    encoder.bool(agent.current_plan_guard.is_some());
    if let Some(plan_guard) = agent.current_plan_guard {
        encoder.tag(match plan_guard.state() {
            PlanGuardState::Inactive => 0,
            PlanGuardState::ReadOnly => 1,
        });
    }
    encoder.u64(agent.pending_steers.len() as u64);
    for steer in &agent.pending_steers {
        encoder.u64(steer.sequence);
        encoder.text(steer.turn_id.as_str());
        encoder.text(&steer.content);
        encode_input_references(encoder, &steer.references);
    }
    encoder.bool(agent.start_pending);
}

/// 对局部驱逐 checkpoint 计算版本化规范摘要。
pub(super) fn recovered_agent_checkpoint_digest(checkpoint: &RecoveredAgentCheckpoint) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.agent-checkpoint.v5");
    encoder.text(checkpoint.root_agent_id.as_str());
    encoder.u64(checkpoint.revision);
    encode_recovered_agent(&mut encoder, &checkpoint.agent);
    encoder.finish_bytes()
}

/// 返回 Turn 终态中由模型或执行器提供的动态文本字节数。
pub(super) fn turn_outcome_text_bytes(outcome: &AgentTurnOutcome) -> usize {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            final_message.as_ref().map_or(0, String::len)
        }
        AgentTurnOutcome::Interrupted => 0,
        AgentTurnOutcome::Failed { message } => message.len(),
    }
}

/// 返回最近 Turn 记录中除 mailbox 与 steer 外的动态文本字节数。
pub(super) fn recovered_turn_text_bytes(turn: &RecoveredTurn) -> usize {
    turn.prompt
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(turn_outcome_text_bytes(&turn.outcome))
}

/// 返回单 Agent 恢复状态中除 mailbox 与 steer 外的动态文本字节数。
pub(super) fn recovered_agent_dynamic_text_bytes(agent: &RecoveredAgent) -> usize {
    agent
        .last_turn
        .as_ref()
        .map_or(0, recovered_turn_text_bytes)
        .saturating_add(agent.current_turn_prompt.as_ref().map_or(0, String::len))
}

/// 返回不可变 Agent 配置中由用户提供的文本字节数。
pub(super) fn agent_definition_text_bytes(definition: &AgentDefinition) -> usize {
    let mut total = definition.profile.model.len();
    total = total.saturating_add(definition.assignment.as_ref().map_or(0, String::len));
    total = total.saturating_add(
        definition
            .profile
            .reasoning_effort
            .as_ref()
            .map_or(0, String::len),
    );
    total = total.saturating_add(
        definition
            .profile
            .worktree_lease
            .as_ref()
            .map_or(0, |lease| lease.as_str().len()),
    );
    total = total.saturating_add(
        definition
            .profile
            .tool_snapshot
            .iter()
            .map(String::len)
            .sum::<usize>(),
    );
    total = total.saturating_add(
        definition
            .context_snapshot
            .iter()
            .map(String::len)
            .sum::<usize>(),
    );
    if let Some(template) = &definition.agent_template {
        total = total
            .saturating_add(template.name.len())
            .saturating_add(template.system_prompt.len())
            .saturating_add(
                template
                    .allowed_write_dirs
                    .iter()
                    .map(|directory| directory.as_os_str().to_string_lossy().len())
                    .sum::<usize>(),
            );
    }
    total
}

/// 返回一条协作工具幂等记录保留的键、摘要与结果字节数。
pub(super) fn collaboration_invocation_text_bytes(
    key: &CollaborationInvocationKey,
    record: &CollaborationInvocationRecord,
) -> usize {
    let total = key
        .source_agent_id
        .as_str()
        .len()
        .saturating_add(key.source_turn_id.as_str().len())
        .saturating_add(key.tool_call_id.as_str().len())
        .saturating_add(record.input_digest.len());
    total.saturating_add(match &record.output {
        CollaborationInvocationOutput::SpawnedAgent(spawned) => spawned
            .agent
            .agent_id
            .as_str()
            .len()
            .saturating_add(spawned.agent.session_id.as_str().len())
            .saturating_add(spawned.agent.path.as_str().len())
            .saturating_add(spawned.initial_turn_id.as_str().len()),
        CollaborationInvocationOutput::Message {
            message_id,
            triggered_turn_id,
        } => message_id.as_str().len().saturating_add(
            triggered_turn_id
                .as_ref()
                .map_or(0, |turn_id| turn_id.as_str().len()),
        ),
        CollaborationInvocationOutput::StoppedAgent {
            target_agent_id,
            stopped_turn_id,
        } => target_agent_id
            .as_str()
            .len()
            .saturating_add(stopped_turn_id.as_str().len()),
        CollaborationInvocationOutput::UserSteer(steer) => steer
            .turn_id
            .as_str()
            .len()
            .saturating_add(steer.payload_bytes()),
        CollaborationInvocationOutput::RetriedAgent {
            target_agent_id,
            retry_turn_id,
        } => target_agent_id
            .as_str()
            .len()
            .saturating_add(retry_turn_id.as_str().len()),
        CollaborationInvocationOutput::ResumedAgent {
            target_agent_id,
            resume_turn_id,
        } => target_agent_id
            .as_str()
            .len()
            .saturating_add(resume_turn_id.as_str().len()),
    })
}
