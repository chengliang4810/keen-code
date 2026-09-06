use std::fs;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, CompactionRecord, ContextCompressionTrigger, Durability, DynamicInputKind,
    IdempotentAppendOutcome, JournalConfig, MessagePart, MessageRole, PersistedToolResult,
    RequestId, ResourceError, SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION, SessionEvent,
    SessionEventId, SessionEventRecord, SessionId, SessionJournal, SessionMessage, SessionOpen,
    SessionState, SnapshotPolicy, SubAgentState, SubAgentStatus, ToolCompletionStatus, ToolEffect,
    ToolOutcome, ToolRequest, ToolResultPart, TranscriptSegment, TurnId, reduce_record,
};
use serde_json::json;
use tempfile::TempDir;

/// 返回强制同步且关闭自动 Snapshot 的模型 Round 测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开一个指定标识的可写测试 Session。
fn open_journal(root: &std::path::Path, session: &str) -> SessionJournal {
    match SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    }
}

/// 使用当前 sequence 和稳定事件标识提交一条事件，并返回实际物理记录。
fn append_record(
    journal: &SessionJournal,
    event_id: &str,
    event: SessionEvent,
) -> SessionEventRecord {
    let expected_sequence = journal.state().expect("状态应读取").last_sequence;
    match journal
        .append_idempotent(
            SessionEventId::new(event_id).expect("事件 ID 应有效"),
            expected_sequence,
            event,
        )
        .expect("事件应提交")
    {
        IdempotentAppendOutcome::Appended(receipt) => receipt.record,
        other => panic!("全新事件应首次提交，实际为 {other:?}"),
    }
}

/// 使用当前 sequence 把多项事件提交为一条不可分割的物理记录。
fn append_batch_record(
    journal: &SessionJournal,
    event_id: &str,
    events: Vec<SessionEvent>,
) -> SessionEventRecord {
    let expected_sequence = journal.state().expect("状态应读取").last_sequence;
    match journal
        .append_batch_idempotent(
            SessionEventId::new(event_id).expect("批次 ID 应有效"),
            expected_sequence,
            events,
        )
        .expect("原子批次应提交")
    {
        IdempotentAppendOutcome::Appended(receipt) => receipt.record,
        other => panic!("全新批次应首次提交，实际为 {other:?}"),
    }
}

/// 创建 Session，并返回创建事件的真实时间戳记录。
fn create_session(journal: &SessionJournal, event_id: &str) -> SessionEventRecord {
    append_record(
        journal,
        event_id,
        SessionEvent::SessionCreated {
            title: "模型 Round 持久化测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    )
}

/// 开始一个根 Agent Turn，并返回 Turn 身份与真实物理记录。
fn start_root_turn(
    journal: &SessionJournal,
    event_id: &str,
    turn: &str,
) -> (TurnId, AgentId, SessionEventRecord) {
    let turn_id = TurnId::new(turn).expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    let record = append_record(
        journal,
        event_id,
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证模型 Round".to_owned(),
        },
    );
    (turn_id, agent_id, record)
}

/// 构造 Provider 中立且字段完整的模型 Round 事件。
fn model_round_event(
    turn_id: &TurnId,
    agent_id: &AgentId,
    model_round: u32,
    requested_model: &str,
    usage: TokenUsage,
    stop_reason: StopReason,
) -> SessionEvent {
    SessionEvent::ModelRoundCompleted {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round,
        requested_model: requested_model.to_owned(),
        metadata: ResponseMetadata {
            response_id: Some(format!("response-{model_round}")),
            model: Some(format!("actual-{requested_model}")),
        },
        usage,
        stop_reason,
    }
}

/// 构造一个模型 Round 的首个纯文本 Transcript 段。
fn text_segment(
    turn_id: &TurnId,
    agent_id: &AgentId,
    model_round: u32,
    expected_revision: u64,
    suffix: &str,
) -> TranscriptSegment {
    TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round,
        segment_index: 0,
        expected_transcript_revision: expected_revision,
        messages: vec![SessionMessage {
            message_id: format!("message-{suffix}"),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Assistant,
            content: vec![MessagePart::Text {
                text: format!("第 {model_round} 轮完成"),
            }],
        }],
    }
}

/// 构造模型 Round 元数据与首个 Transcript 段的标准原子批次。
fn model_round_batch(
    turn_id: &TurnId,
    agent_id: &AgentId,
    model_round: u32,
    expected_revision: u64,
    suffix: &str,
    usage: TokenUsage,
    stop_reason: StopReason,
) -> Vec<SessionEvent> {
    vec![
        model_round_event(
            turn_id,
            agent_id,
            model_round,
            "test-model",
            usage,
            stop_reason,
        ),
        SessionEvent::TranscriptSegmentCommitted {
            segment: text_segment(turn_id, agent_id, model_round, expected_revision, suffix),
        },
    ]
}

/// 构造所有字段均由 Provider 明确报告为零的 Token 用量。
fn explicit_zero_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_tokens: Some(0),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        total_tokens: Some(0),
    }
}

/// 断言一个非法原子批次既不改变内存状态，也不在 Journal 留下任何字节。
fn assert_batch_rejected_without_side_effects(
    journal: &SessionJournal,
    event_id: &str,
    events: Vec<SessionEvent>,
) {
    let before_state = journal.state().expect("拒绝前状态应读取");
    let before_log = fs::read(journal.log_path()).expect("拒绝前日志应读取");
    let result = journal.append_batch_idempotent(
        SessionEventId::new(event_id).expect("批次 ID 应有效"),
        before_state.last_sequence,
        events,
    );
    assert!(
        matches!(result, Err(ResourceError::Reduction(_))),
        "非法批次必须由归约器拒绝，实际为 {result:?}"
    );
    assert_eq!(journal.state().expect("拒绝后状态应读取"), before_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        before_log
    );
}

/// 使用显式测试时间构造下一条公开归约记录。
fn record_at(
    state: &SessionState,
    event_id: &str,
    time_unix_ms: u64,
    event: SessionEvent,
) -> SessionEventRecord {
    SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new(event_id).expect("事件 ID 应有效"),
        session: state.session_id.clone(),
        sequence: state
            .last_sequence
            .checked_add(1)
            .expect("测试 sequence 不应溢出"),
        time_unix_ms,
        event,
    }
}

/// 验证模型 Round 元数据与首段只占一条物理记录，且未知和显式零用量可冷恢复。
#[test]
fn model_round_and_first_segment_commit_atomically_and_recover_exactly() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "model-round-reopen";
    let journal = open_journal(root.path(), session);
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    let before_lines = fs::read_to_string(journal.log_path())
        .expect("日志应读取")
        .lines()
        .count();

    let first_record = append_batch_record(
        &journal,
        "batch-round-one",
        model_round_batch(
            &turn_id,
            &agent_id,
            1,
            0,
            "round-one",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );
    let second_record = append_batch_record(
        &journal,
        "batch-round-two",
        model_round_batch(
            &turn_id,
            &agent_id,
            2,
            1,
            "round-two",
            explicit_zero_usage(),
            StopReason::Completed,
        ),
    );

    assert_eq!(
        fs::read_to_string(journal.log_path())
            .expect("日志应读取")
            .lines()
            .count(),
        before_lines + 2
    );
    for record in [&first_record, &second_record] {
        let SessionEvent::AtomicBatch { events } = &record.event else {
            panic!("模型 Round 必须保存为 AtomicBatch")
        };
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            SessionEvent::ModelRoundCompleted { .. }
        ));
        assert!(matches!(
            events[1],
            SessionEvent::TranscriptSegmentCommitted { .. }
        ));
    }

    let live_state = journal.state().expect("实时状态应读取");
    assert_eq!(live_state.model_rounds.len(), 2);
    assert_eq!(live_state.transcript_revision, 2);
    assert_eq!(live_state.model_rounds[0].usage, TokenUsage::unknown());
    assert_eq!(live_state.model_rounds[1].usage, explicit_zero_usage());
    assert_eq!(
        live_state.model_rounds[0].completed_at_unix_ms,
        first_record.time_unix_ms
    );
    assert_eq!(
        live_state.model_rounds[1].completed_at_unix_ms,
        second_record.time_unix_ms
    );
    journal.write_snapshot().expect("Snapshot 应写入");
    drop(journal);

    let reopened = open_journal(root.path(), session);
    let recovered_state = reopened.state().expect("冷恢复状态应读取");
    assert_eq!(recovered_state, live_state);
    assert_eq!(recovered_state.model_rounds[0].usage.input_tokens, None);
    assert_eq!(recovered_state.model_rounds[1].usage.input_tokens, Some(0));
}

/// 验证每个 Turn 都从 Round 1 开始，并在各自 Turn 内严格连续。
#[test]
fn model_round_sequence_is_strict_and_resets_for_each_turn() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-sequence");
    create_session(&journal, "event-create");
    let (first_turn, agent_id, _) = start_root_turn(&journal, "event-turn-one", "turn-one");
    append_batch_record(
        &journal,
        "batch-turn-one-round-one",
        model_round_batch(
            &first_turn,
            &agent_id,
            1,
            0,
            "turn-one-round-one",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );
    append_batch_record(
        &journal,
        "batch-turn-one-round-two",
        model_round_batch(
            &first_turn,
            &agent_id,
            2,
            1,
            "turn-one-round-two",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );
    append_record(
        &journal,
        "event-turn-one-complete",
        SessionEvent::TurnCompleted {
            turn_id: first_turn.clone(),
        },
    );

    let (second_turn, _, _) = start_root_turn(&journal, "event-turn-two", "turn-two");
    append_batch_record(
        &journal,
        "batch-turn-two-round-one",
        model_round_batch(
            &second_turn,
            &agent_id,
            1,
            2,
            "turn-two-round-one",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );

    let state = journal.state().expect("Round 状态应读取");
    let rounds = state
        .model_rounds
        .iter()
        .map(|round| (round.turn_id.clone(), round.model_round))
        .collect::<Vec<_>>();
    assert_eq!(
        rounds,
        vec![(first_turn.clone(), 1), (first_turn, 2), (second_turn, 1)]
    );
}

/// 验证模型 Round 元数据不能脱离对应 Transcript 段单独提交。
#[test]
fn standalone_model_round_is_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-unpaired");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    let baseline_state = journal.state().expect("基线状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("基线日志应读取");

    let round_result = journal.append_idempotent(
        SessionEventId::new("event-unpaired-round").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        model_round_event(
            &turn_id,
            &agent_id,
            1,
            "test-model",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );
    assert!(matches!(round_result, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_log
    );

    let wrapped_result = journal.append_batch_idempotent(
        SessionEventId::new("batch-unpaired-round").expect("批次 ID 应有效"),
        baseline_state.last_sequence,
        vec![model_round_event(
            &turn_id,
            &agent_id,
            1,
            "test-model",
            TokenUsage::unknown(),
            StopReason::Completed,
        )],
    );
    assert!(matches!(wrapped_result, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_log
    );
}

/// 验证原子批次中的模型元数据只能配对同一 Turn、Agent 和 Round 的 Transcript 段。
#[test]
fn model_round_and_segment_identity_must_match() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-pair-identity");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");

    assert_batch_rejected_without_side_effects(
        &journal,
        "batch-mismatched-round",
        vec![
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: text_segment(&turn_id, &agent_id, 2, 0, "mismatched-round"),
            },
        ],
    );
}

/// 验证重复、跳号、错误 Agent、空模型与空扩展停止原因全部事务性拒绝。
#[test]
fn invalid_model_rounds_are_rejected_without_partial_state() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-invalid");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    append_batch_record(
        &journal,
        "batch-round-one",
        model_round_batch(
            &turn_id,
            &agent_id,
            1,
            0,
            "valid-round-one",
            TokenUsage::unknown(),
            StopReason::Completed,
        ),
    );
    let wrong_agent = AgentId::new("wrong-agent").expect("错误 Agent ID 也应满足语法");
    let revision = journal.state().expect("状态应读取").transcript_revision;

    let cases = vec![
        (
            "batch-duplicate-round",
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            text_segment(&turn_id, &agent_id, 1, revision, "duplicate"),
        ),
        (
            "batch-skipped-round",
            model_round_event(
                &turn_id,
                &agent_id,
                3,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            text_segment(&turn_id, &agent_id, 3, revision, "skipped"),
        ),
        (
            "batch-wrong-agent",
            model_round_event(
                &turn_id,
                &wrong_agent,
                2,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            text_segment(&turn_id, &wrong_agent, 2, revision, "wrong-agent"),
        ),
        (
            "batch-empty-model",
            model_round_event(
                &turn_id,
                &agent_id,
                2,
                "   ",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            text_segment(&turn_id, &agent_id, 2, revision, "empty-model"),
        ),
        (
            "batch-empty-other-reason",
            model_round_event(
                &turn_id,
                &agent_id,
                2,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Other {
                    reason: "  ".to_owned(),
                },
            ),
            text_segment(&turn_id, &agent_id, 2, revision, "empty-other-reason"),
        ),
    ];

    for (event_id, round, segment) in cases {
        assert_batch_rejected_without_side_effects(
            &journal,
            event_id,
            vec![round, SessionEvent::TranscriptSegmentCommitted { segment }],
        );
    }
}

/// 验证 AtomicBatch 后续事件失败时，先应用的合法模型 Round 不会留下半状态。
#[test]
fn failing_later_batch_event_rolls_back_model_round() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-batch-rollback");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    let invalid_segment = TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 0,
        messages: Vec::new(),
    };

    assert_batch_rejected_without_side_effects(
        &journal,
        "batch-invalid-later-event",
        vec![
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: invalid_segment,
            },
        ],
    );
    assert!(
        journal
            .state()
            .expect("回滚后状态应读取")
            .model_rounds
            .is_empty()
    );
}

/// 验证 Turn、Tool 与模型 Round 时间都精确取自物理事件，并在重启后保持一致。
#[test]
fn event_timestamps_are_exact_monotonic_and_survive_reopen() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "model-round-timestamps";
    let journal = open_journal(root.path(), session);
    let created_record = create_session(&journal, "event-create");
    let (turn_id, agent_id, turn_record) = start_root_turn(&journal, "event-turn", "turn-main");
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        &turn_id,
        &agent_id,
        1,
        "call-read",
    )
    .expect("Request ID 应派生");
    let arguments = json!({"path": "src/lib.rs"});
    let request_record = append_record(
        &journal,
        "event-tool-request",
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id: agent_id.clone(),
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-read".to_owned(),
                tool_name: "read_file".to_owned(),
                arguments: arguments.clone(),
                effect: ToolEffect::ReadOnly,
            },
        },
    );
    let started_record = append_record(
        &journal,
        "event-tool-started",
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    let result = PersistedToolResult {
        tool_call_id: "call-read".to_owned(),
        content: vec![ToolResultPart::Text {
            text: "文件内容".to_owned(),
        }],
        is_error: false,
    };
    let completed_record = append_record(
        &journal,
        "event-tool-completed",
        SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: result.clone(),
            },
        },
    );
    let segment = TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 0,
        messages: vec![
            SessionMessage {
                message_id: "message-tool-call".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::ToolCall {
                    tool_call_id: "call-read".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments,
                }],
            },
            SessionMessage {
                message_id: "message-tool-result".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Tool,
                content: vec![MessagePart::ToolResult {
                    tool_call_id: "call-read".to_owned(),
                    content: result.content.clone(),
                    is_error: false,
                }],
            },
        ],
    };
    let round_record = append_batch_record(
        &journal,
        "batch-model-round",
        vec![
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::ToolUse,
            ),
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    );
    let turn_completed_record = append_record(
        &journal,
        "event-turn-completed",
        SessionEvent::TurnCompleted {
            turn_id: turn_id.clone(),
        },
    );

    let timestamps = [
        created_record.time_unix_ms,
        turn_record.time_unix_ms,
        request_record.time_unix_ms,
        started_record.time_unix_ms,
        completed_record.time_unix_ms,
        round_record.time_unix_ms,
        turn_completed_record.time_unix_ms,
    ];
    assert!(timestamps.windows(2).all(|pair| pair[0] <= pair[1]));
    let live_state = journal.state().expect("时间戳状态应读取");
    let turn = live_state.turns.get(&turn_id).expect("Turn 应存在");
    let tool = live_state.tools.get(&request_id).expect("Tool 应存在");
    assert_eq!(live_state.created_at_unix_ms, created_record.time_unix_ms);
    assert_eq!(turn.started_at_unix_ms, turn_record.time_unix_ms);
    assert_eq!(
        turn.completed_at_unix_ms,
        Some(turn_completed_record.time_unix_ms)
    );
    assert_eq!(tool.requested_at_unix_ms, request_record.time_unix_ms);
    assert_eq!(
        tool.execution_started_at_unix_ms,
        Some(started_record.time_unix_ms)
    );
    assert_eq!(
        tool.completed_at_unix_ms,
        Some(completed_record.time_unix_ms)
    );
    assert_eq!(
        live_state.model_rounds[0].completed_at_unix_ms,
        round_record.time_unix_ms
    );
    assert_eq!(
        live_state.updated_at_unix_ms,
        turn_completed_record.time_unix_ms
    );
    drop(journal);

    let reopened = open_journal(root.path(), session);
    assert_eq!(reopened.state().expect("重启状态应读取"), live_state);
}

/// 验证公开归约器允许相同时间，但拒绝任何倒退时间且不修改已有状态。
#[test]
fn reducer_rejects_regressing_event_timestamps_transactionally() {
    let session_id = SessionId::new("model-round-explicit-time").expect("Session ID 应有效");
    let mut state = SessionState::empty(session_id.clone());
    let created = record_at(
        &state,
        "event-create",
        100,
        SessionEvent::SessionCreated {
            title: "显式时间测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    );
    reduce_record(&mut state, created).expect("创建时间应归约");
    let turn_id = TurnId::new("turn-main").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    let turn_started = record_at(
        &state,
        "event-turn",
        200,
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证时间".to_owned(),
        },
    );
    reduce_record(&mut state, turn_started).expect("Turn 时间应归约");
    assert_eq!(state.turns[&turn_id].started_at_unix_ms, 200);

    let request_id =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 1, "call-read")
            .expect("Request ID 应派生");
    let request = ToolRequest {
        request_id: request_id.clone(),
        turn_id: turn_id.clone(),
        agent_id,
        model_round: 1,
        request_index: 0,
        model_tool_call_id: "call-read".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: json!({"path": "src/lib.rs"}),
        effect: ToolEffect::ReadOnly,
    };
    let request_record = record_at(
        &state,
        "event-tool-request",
        300,
        SessionEvent::ToolRequested { request },
    );
    reduce_record(&mut state, request_record).expect("工具请求时间应归约");
    let started_record = record_at(
        &state,
        "event-tool-started",
        400,
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    reduce_record(&mut state, started_record).expect("工具执行时间应归约");
    let completed_record = record_at(
        &state,
        "event-tool-completed",
        500,
        SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: PersistedToolResult {
                    tool_call_id: "call-read".to_owned(),
                    content: vec![ToolResultPart::Text {
                        text: "完成".to_owned(),
                    }],
                    is_error: false,
                },
            },
        },
    );
    reduce_record(&mut state, completed_record).expect("工具完成时间应归约");
    assert_eq!(state.tools[&request_id].requested_at_unix_ms, 300);
    assert_eq!(
        state.tools[&request_id].execution_started_at_unix_ms,
        Some(400)
    );
    assert_eq!(state.tools[&request_id].completed_at_unix_ms, Some(500));

    let equal_time_record = record_at(
        &state,
        "event-equal-time",
        500,
        SessionEvent::SessionRenamed {
            title: "相同时间合法".to_owned(),
        },
    );
    reduce_record(&mut state, equal_time_record).expect("相同毫秒时间应允许");
    let before_regression = state.clone();
    let regressing = record_at(
        &state,
        "event-regressing-time",
        499,
        SessionEvent::SessionRenamed {
            title: "倒退时间必须拒绝".to_owned(),
        },
    );
    assert!(reduce_record(&mut state, regressing).is_err());
    assert_eq!(state, before_regression);
}

/// 验证系统墙钟落后于 Journal 水位时，新追加记录会钳制为既有时间并可再次冷恢复。
#[test]
fn journal_append_clamps_regressing_system_clock() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "model-round-future-clock";
    let session_id = SessionId::new(session).expect("Session ID 应有效");
    let session_dir = root.path().join("sessions").join(session);
    fs::create_dir_all(&session_dir).expect("Session 目录应创建");
    let future_time_unix_ms = 4_000_000_000_000;
    let created = SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new("event-create").expect("事件 ID 应有效"),
        session: session_id,
        sequence: 1,
        time_unix_ms: future_time_unix_ms,
        event: SessionEvent::SessionCreated {
            title: "未来墙钟测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    };
    let mut bytes = serde_json::to_vec(&created).expect("创建记录应编码");
    bytes.push(b'\n');
    fs::write(session_dir.join("events.jsonl"), bytes).expect("未来时间夹具应写入");

    let journal = open_journal(root.path(), session);
    assert_eq!(
        journal.state().expect("夹具状态应读取").updated_at_unix_ms,
        future_time_unix_ms
    );
    let appended = append_record(
        &journal,
        "event-renamed",
        SessionEvent::SessionRenamed {
            title: "墙钟回退后标题".to_owned(),
        },
    );
    assert_eq!(appended.time_unix_ms, future_time_unix_ms);
    let live_state = journal.state().expect("钳制后状态应读取");
    assert_eq!(live_state.updated_at_unix_ms, future_time_unix_ms);
    drop(journal);

    let reopened = open_journal(root.path(), session);
    assert_eq!(reopened.state().expect("重启状态应读取"), live_state);
}

/// 采样前动态输入可先占用零号段，随后模型完成事件必须与同 Round 的下一段原子配对。
#[test]
fn dynamic_input_segment_can_precede_model_completion_in_same_round() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-dynamic-input-first");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    append_record(
        &journal,
        "event-dynamic-input",
        SessionEvent::TranscriptSegmentCommitted {
            segment: TranscriptSegment {
                turn_id: turn_id.clone(),
                source_agent_id: agent_id.clone(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: 0,
                messages: vec![SessionMessage {
                    message_id: "message-dynamic-input".to_owned(),
                    turn_id: Some(turn_id.clone()),
                    agent_id: None,
                    role: MessageRole::Developer,
                    content: vec![MessagePart::Text {
                        text: "mailbox followup".to_owned(),
                    }],
                }],
            },
        },
    );
    let mut response_segment = text_segment(&turn_id, &agent_id, 1, 1, "after-dynamic-input");
    response_segment.segment_index = 1;
    append_batch_record(
        &journal,
        "event-model-after-dynamic-input",
        vec![
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: response_segment,
            },
        ],
    );

    let state = journal.state().expect("动态输入后的状态应读取");
    assert_eq!(state.transcript_revision, 2);
    assert_eq!(state.transcript_segments().count(), 2);
    assert_eq!(state.model_rounds.len(), 1);
}

/// 带 Mailbox/UserSteer 回执的动态段必须保留在有效 Transcript，参与压缩 Digest，且冷恢复一致。
#[test]
fn dynamic_input_receipts_preserve_effective_history_and_compaction_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "model-round-dynamic-input-receipts";
    let journal = open_journal(root.path(), session);
    create_session(&journal, "event-create");
    let (turn_id, root_agent, _) = start_root_turn(&journal, "event-turn", "turn-main");
    let child_agent = AgentId::new("dynamic-child").expect("子 Agent ID 应有效");
    append_record(
        &journal,
        "event-spawn-dynamic-child",
        SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/dynamic_child".to_owned(),
                task: "验证动态输入隔离".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    );

    let dynamic_segment = TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: root_agent.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 0,
        messages: vec![
            SessionMessage {
                message_id: "message-mailbox-input".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::Developer,
                content: vec![MessagePart::Text {
                    text: "来自 mailbox 的后续消息".to_owned(),
                }],
            },
            SessionMessage {
                message_id: "message-user-steer-input".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "来自用户 Steer 的补充要求".to_owned(),
                }],
            },
        ],
    };
    append_batch_record(
        &journal,
        "event-dynamic-input-with-receipts",
        vec![
            SessionEvent::TranscriptSegmentCommitted {
                segment: dynamic_segment,
            },
            SessionEvent::DynamicInputReceiptCommitted {
                turn_id: turn_id.clone(),
                source_agent_id: root_agent.clone(),
                model_round: 1,
                segment_index: 0,
                kind: DynamicInputKind::Mailbox,
                through_sequence: 7,
            },
            SessionEvent::DynamicInputReceiptCommitted {
                turn_id: turn_id.clone(),
                source_agent_id: root_agent.clone(),
                model_round: 1,
                segment_index: 0,
                kind: DynamicInputKind::UserSteer,
                through_sequence: 11,
            },
        ],
    );

    let mut response_segment = text_segment(&turn_id, &root_agent, 1, 1, "after-dynamic-input");
    response_segment.segment_index = 1;
    append_batch_record(
        &journal,
        "event-model-after-dynamic-input",
        vec![
            model_round_event(
                &turn_id,
                &root_agent,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: response_segment,
            },
        ],
    );

    let state = journal.state().expect("动态输入状态应读取");
    assert_eq!(state.dynamic_input_receipts.len(), 2);
    let effective = state
        .effective_transcript(&root_agent)
        .expect("根 Agent 有效 Transcript 应包含动态输入");
    assert_eq!(
        effective
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "message-mailbox-input",
            "message-user-steer-input",
            "message-after-dynamic-input"
        ]
    );
    assert!(
        state
            .effective_transcript(&child_agent)
            .expect("子 Agent 有效 Transcript 应可读取")
            .is_empty(),
        "根 Agent 的动态输入不能泄露给其他 Agent"
    );

    // 先验证包含两类动态消息的来源范围可生成 Digest；旧逻辑会因 receipt 把段剔除而越界。
    let digest_with_dynamic = state
        .compaction_source_digest_sha256(&turn_id, &root_agent, 1, 0, 2)
        .expect("动态输入应参与压缩来源 Digest");
    assert_eq!(digest_with_dynamic.len(), 64);

    // Developer 具有更高优先级，不能被压缩；压缩 User Steer + Assistant 仍应保留 mailbox。
    let compaction = CompactionRecord {
        trigger: ContextCompressionTrigger::Budget,
        estimated_tokens_before: 100,
        estimated_tokens_after: 20,
        replaced_start_index: 1,
        replaced_end_index_exclusive: 3,
        replaced_message_count: 2,
        retained_message_count: 2,
        source_digest_sha256: state
            .compaction_source_digest_sha256(&turn_id, &root_agent, 1, 1, 3)
            .expect("可压缩动态范围的 Digest 应生成"),
        summary: "保留动态输入事实后的摘要".to_owned(),
        expected_transcript_revision: state.transcript_revision,
        applied_transcript_revision: state.transcript_revision + 1,
    };
    append_record(
        &journal,
        "event-compaction-after-dynamic-input",
        SessionEvent::CompactionApplied {
            turn_id: turn_id.clone(),
            source_agent_id: root_agent.clone(),
            model_round: 1,
            compaction,
        },
    );

    let live_state = journal.state().expect("压缩后状态应读取");
    let live_effective = live_state
        .effective_transcript(&root_agent)
        .expect("压缩后根 Agent Transcript 应恢复");
    assert_eq!(live_effective.len(), 2);
    assert_eq!(live_effective[0].message_id, "message-mailbox-input");
    assert_eq!(live_effective[1].role, MessageRole::User);
    assert_eq!(
        live_state
            .effective_transcript(&child_agent)
            .expect("压缩后子 Agent Transcript 应恢复")
            .len(),
        0
    );
    drop(journal);

    let reopened = open_journal(root.path(), session);
    assert_eq!(
        reopened.state().expect("冷恢复状态应读取"),
        live_state,
        "压缩后的权威状态冷恢复必须一致"
    );
    assert_eq!(
        reopened
            .state()
            .expect("冷恢复 Transcript 状态应读取")
            .effective_transcript(&root_agent)
            .expect("冷恢复根 Agent Transcript 应恢复"),
        live_effective
    );
}

/// 验证每个模型输出零号 Transcript 段都必须由同批次对应模型完成事件领头。
#[test]
fn first_transcript_segment_requires_preceding_model_completion() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-reverse-pairing");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");
    let segment = text_segment(&turn_id, &agent_id, 1, 0, "unpaired-first-segment");

    assert_batch_rejected_without_side_effects(
        &journal,
        "batch-segment-without-completion",
        vec![SessionEvent::TranscriptSegmentCommitted {
            segment: segment.clone(),
        }],
    );
    assert_batch_rejected_without_side_effects(
        &journal,
        "batch-segment-before-completion",
        vec![
            SessionEvent::TranscriptSegmentCommitted {
                segment: segment.clone(),
            },
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "test-model",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
        ],
    );

    let before_state = journal.state().expect("独立段拒绝前状态应读取");
    let before_log = fs::read(journal.log_path()).expect("独立段拒绝前日志应读取");
    let result = journal.append_idempotent(
        SessionEventId::new("event-standalone-first-segment").expect("事件 ID 应有效"),
        before_state.last_sequence,
        SessionEvent::TranscriptSegmentCommitted { segment },
    );
    assert!(
        matches!(result, Err(ResourceError::Reduction(_))),
        "零号 Transcript 段不得独立提交，实际为 {result:?}"
    );
    assert_eq!(
        journal.state().expect("独立段拒绝后状态应读取"),
        before_state
    );
    assert_eq!(
        fs::read(journal.log_path()).expect("独立段拒绝后日志应读取"),
        before_log
    );
}

/// 验证一个物理批次不能用不同模型的多个完成事件分别认领多个首段。
#[test]
fn atomic_batch_rejects_multiple_model_round_completions() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(root.path(), "model-round-multiple-completions");
    create_session(&journal, "event-create");
    let (turn_id, agent_id, _) = start_root_turn(&journal, "event-turn", "turn-main");

    assert_batch_rejected_without_side_effects(
        &journal,
        "batch-two-model-completions",
        vec![
            model_round_event(
                &turn_id,
                &agent_id,
                1,
                "model-alpha",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: text_segment(&turn_id, &agent_id, 1, 0, "model-alpha"),
            },
            model_round_event(
                &turn_id,
                &agent_id,
                2,
                "model-beta",
                TokenUsage::unknown(),
                StopReason::Completed,
            ),
            SessionEvent::TranscriptSegmentCommitted {
                segment: text_segment(&turn_id, &agent_id, 2, 1, "model-beta"),
            },
        ],
    );
}
