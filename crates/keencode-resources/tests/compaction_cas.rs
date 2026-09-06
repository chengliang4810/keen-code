use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use keencode_resources::{
    AgentId, CompactionRecord, ContextCompressionTrigger, Durability, IdempotentAppendOutcome,
    JournalConfig, MessagePart, MessageRole, ResourceError, SessionEvent, SessionEventId,
    SessionId, SessionJournal, SessionMessage, SessionOpen, SnapshotPolicy, TurnId,
};
use tempfile::TempDir;

/// 返回关闭自动 Snapshot 的压缩 CAS 测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开并写入两条有效消息，返回 revision 为二的运行 Session。
fn journal_with_messages(
    root: &std::path::Path,
    session: &str,
) -> (SessionJournal, TurnId, AgentId) {
    let journal = match SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    append(
        &journal,
        "event-create",
        SessionEvent::SessionCreated {
            title: "压缩测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    );
    let turn_id = TurnId::new("turn-main").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    append(
        &journal,
        "event-turn",
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "压缩上下文".to_owned(),
        },
    );
    for index in 0..2 {
        append(
            &journal,
            &format!("event-message-{index}"),
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: format!("message-{index}"),
                    turn_id: Some(turn_id.clone()),
                    agent_id: Some(agent_id.clone()),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Text {
                        text: format!("上下文 {index}"),
                    }],
                },
            },
        );
    }
    (journal, turn_id, agent_id)
}

/// 使用当前 sequence 和显式测试标识提交一条事件。
fn append(journal: &SessionJournal, event_id: &str, event: SessionEvent) {
    let expected_sequence = journal.state().expect("状态应读取").last_sequence;
    let outcome = journal
        .append_idempotent(
            SessionEventId::new(event_id).expect("事件 ID 应有效"),
            expected_sequence,
            event,
        )
        .expect("事件应通过预校验");
    assert!(matches!(outcome, IdempotentAppendOutcome::Appended(_)));
}

/// 根据当前有效 Transcript 构造覆盖两条消息的合法压缩记录。
fn valid_compaction(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
) -> CompactionRecord {
    let state = journal.state().expect("状态应读取");
    CompactionRecord {
        trigger: ContextCompressionTrigger::Budget,
        estimated_tokens_before: 200,
        estimated_tokens_after: 80,
        replaced_start_index: 0,
        replaced_end_index_exclusive: 2,
        replaced_message_count: 2,
        retained_message_count: 1,
        source_digest_sha256: state
            .compaction_source_digest_sha256(turn_id, agent_id, 1, 0, 2)
            .expect("压缩来源 Digest 应计算"),
        summary: "压缩摘要".to_owned(),
        expected_transcript_revision: state.transcript_revision,
        applied_transcript_revision: state.transcript_revision + 1,
    }
}

/// 构造绑定当前 Turn 与 Agent 的压缩事件。
fn compaction_event(
    turn_id: &TurnId,
    agent_id: &AgentId,
    compaction: CompactionRecord,
) -> SessionEvent {
    SessionEvent::CompactionApplied {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        compaction,
    }
}

/// 验证正确 revision 成功推进一次，过期 revision 不产生第二次压缩。
#[test]
fn compaction_requires_current_transcript_revision() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = journal_with_messages(root.path(), "compaction-revision");
    let compaction = valid_compaction(&journal, &turn_id, &agent_id);
    append(
        &journal,
        "event-compaction",
        compaction_event(&turn_id, &agent_id, compaction.clone()),
    );
    let state = journal.state().expect("状态应读取");
    assert_eq!(state.transcript_revision, 3);
    assert_eq!(
        state
            .effective_transcript(&agent_id)
            .expect("有效 Transcript 应重建")
            .len(),
        1
    );
    assert_eq!(state.applied_compactions().count(), 1);

    let result = journal.append_idempotent(
        SessionEventId::new("event-stale-compaction").expect("事件 ID 应有效"),
        state.last_sequence,
        compaction_event(&turn_id, &agent_id, compaction),
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(
        journal
            .state()
            .expect("状态应读取")
            .applied_compactions()
            .count(),
        1
    );
}

/// 验证范围、计数、保留量、摘要、Token 和 revision 的全部形状不变量原子生效。
#[test]
fn invalid_compaction_invariants_do_not_change_log_or_state() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = journal_with_messages(root.path(), "compaction-invalid");
    let baseline_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let baseline_state = journal.state().expect("状态应读取");
    let mut candidates = Vec::new();
    let valid = valid_compaction(&journal, &turn_id, &agent_id);

    let mut empty_range = valid.clone();
    empty_range.replaced_end_index_exclusive = 0;
    candidates.push(empty_range);
    let mut count_mismatch = valid.clone();
    count_mismatch.replaced_message_count = 1;
    candidates.push(count_mismatch);
    let mut retained_mismatch = valid.clone();
    retained_mismatch.retained_message_count = 2;
    candidates.push(retained_mismatch);
    let mut invalid_digest = valid.clone();
    invalid_digest.source_digest_sha256 = "not-a-digest".to_owned();
    candidates.push(invalid_digest);
    let mut empty_summary = valid.clone();
    empty_summary.summary = "   ".to_owned();
    candidates.push(empty_summary);
    let mut applied_revision_gap = valid;
    applied_revision_gap.applied_transcript_revision = 4;
    candidates.push(applied_revision_gap);

    for (index, compaction) in candidates.into_iter().enumerate() {
        let result = journal.append_idempotent(
            SessionEventId::new(format!("event-invalid-{index}")).expect("事件 ID 应有效"),
            baseline_state.last_sequence,
            compaction_event(&turn_id, &agent_id, compaction),
        );
        assert!(matches!(result, Err(ResourceError::Reduction(_))));
        assert_eq!(
            fs::read(journal.log_path()).expect("日志应读取"),
            baseline_bytes
        );
        assert_eq!(journal.state().expect("状态应读取"), baseline_state);
    }
}

/// 验证两个实例基于同一 Transcript revision 时至多一个压缩提交成功。
#[test]
fn concurrent_compaction_cas_commits_only_once() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "compaction-race";
    let (first, turn_id, agent_id) = journal_with_messages(root.path(), session);
    let second = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("第二实例应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let expected_sequence = first.state().expect("状态应读取").last_sequence;
    let compaction = valid_compaction(&first, &turn_id, &agent_id);
    let barrier = Arc::new(Barrier::new(2));
    let first_barrier = barrier.clone();
    let first_turn = turn_id.clone();
    let first_agent = agent_id.clone();
    let first_compaction = compaction.clone();
    let first_handle = thread::spawn(move || {
        first_barrier.wait();
        first
            .append_idempotent(
                SessionEventId::new("event-compaction-first").expect("事件 ID 应有效"),
                expected_sequence,
                compaction_event(&first_turn, &first_agent, first_compaction),
            )
            .expect("第一实例应返回结果")
    });
    let second_turn = turn_id.clone();
    let second_agent = agent_id.clone();
    let second_compaction = compaction.clone();
    let second_handle = thread::spawn(move || {
        barrier.wait();
        second
            .append_idempotent(
                SessionEventId::new("event-compaction-second").expect("事件 ID 应有效"),
                expected_sequence,
                compaction_event(&second_turn, &second_agent, second_compaction),
            )
            .expect("第二实例应返回结果")
    });
    let outcomes = [
        first_handle.join().expect("第一线程应结束"),
        second_handle.join().expect("第二线程应结束"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, IdempotentAppendOutcome::Appended(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(outcome, IdempotentAppendOutcome::SequenceConflict { .. })
            })
            .count(),
        1
    );

    let reopened = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应重开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let state = reopened.state().expect("状态应读取");
    assert_eq!(state.applied_compactions().count(), 1);
    let stale_retry = reopened.append_idempotent(
        SessionEventId::new("event-compaction-retry").expect("事件 ID 应有效"),
        state.last_sequence,
        compaction_event(&turn_id, &agent_id, compaction),
    );
    assert!(matches!(stale_retry, Err(ResourceError::Reduction(_))));
}
