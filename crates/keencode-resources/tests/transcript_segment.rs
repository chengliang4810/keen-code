use std::fs;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, Durability, IdempotentAppendOutcome, JournalConfig, MessagePart, MessageRole,
    PersistedToolResult, RequestId, ResourceError, SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION,
    SessionEvent, SessionEventId, SessionEventRecord, SessionId, SessionJournal, SessionMessage,
    SessionOpen, SessionState, SnapshotPolicy, ToolCompletionStatus, ToolEffect, ToolOutcome,
    ToolRequest, ToolResultPart, TranscriptSegment, TurnId, reduce_record,
};
use serde_json::json;
use tempfile::TempDir;

/// 返回关闭自动 Snapshot 的 Transcript 原子性测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开并初始化一个带 Running Turn 的健康日志。
fn running_journal(root: &std::path::Path, session: &str) -> (SessionJournal, TurnId, AgentId) {
    let session_id = SessionId::new(session).expect("Session ID 应有效");
    let journal = match SessionJournal::open(root, session_id, config()).expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    append(
        &journal,
        "event-create",
        SessionEvent::SessionCreated {
            title: "Transcript 测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    );
    let turn_id = TurnId::new("turn-main").expect("Turn ID 应有效");
    append(
        &journal,
        "event-turn",
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "执行工具".to_owned(),
        },
    );
    (
        journal,
        turn_id,
        AgentId::new("root").expect("Agent ID 应有效"),
    )
}

/// 把零号 Transcript 段封装为带模型 Round 完成事件的标准原子批次。
fn atomic_model_round_event(segment: TranscriptSegment) -> SessionEvent {
    let turn_id = segment.turn_id.clone();
    let source_agent_id = segment.source_agent_id.clone();
    let model_round = segment.model_round;
    let stop_reason = if segment.messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|part| matches!(part, MessagePart::ToolCall { .. }))
    }) {
        StopReason::ToolUse
    } else {
        StopReason::Completed
    };
    SessionEvent::AtomicBatch {
        events: vec![
            SessionEvent::ModelRoundCompleted {
                turn_id,
                source_agent_id,
                model_round,
                requested_model: "transcript-segment-test-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some(format!("transcript-segment-response-{model_round}")),
                    model: Some("transcript-segment-test-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 为合法测试提交自动补齐零号 Transcript 段的模型 Round 原子配对。
fn append(journal: &SessionJournal, event_id: &str, event: SessionEvent) {
    let event = match event {
        SessionEvent::TranscriptSegmentCommitted { segment } if segment.segment_index == 0 => {
            atomic_model_round_event(segment)
        }
        event => event,
    };
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

/// 构造一条可验证公开归约入口不会接受伪造既有状态的普通消息记录。
fn public_message_record(
    state: &SessionState,
    turn_id: &TurnId,
    agent_id: &AgentId,
    suffix: &str,
) -> SessionEventRecord {
    SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new(format!("event-public-state-{suffix}"))
            .expect("事件 ID 应有效"),
        session: state.session_id.clone(),
        sequence: state
            .last_sequence
            .checked_add(1)
            .expect("测试序号不应溢出"),
        time_unix_ms: 1,
        event: SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: format!("message-public-state-{suffix}"),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::Text {
                    text: "继续执行".to_owned(),
                }],
            },
        },
    }
}

/// 断言公开归约器在拒绝伪造状态时不会修改该状态。
fn assert_public_state_rejected(
    mut state: SessionState,
    turn_id: &TurnId,
    agent_id: &AgentId,
    suffix: &str,
) {
    let before = state.clone();
    let record = public_message_record(&state, turn_id, agent_id, suffix);
    assert!(reduce_record(&mut state, record).is_err());
    assert_eq!(state, before);
}

/// 为 Transcript 配对测试持久化一个已完成的只读工具生命周期。
fn completed_tool(journal: &SessionJournal, turn_id: &TurnId, agent_id: &AgentId) -> RequestId {
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        turn_id,
        agent_id,
        1,
        "call-read",
    )
    .expect("Request ID 应派生");
    append(
        journal,
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
                arguments: json!({"path": "src/lib.rs"}),
                effect: ToolEffect::ReadOnly,
            },
        },
    );
    append(
        journal,
        "event-tool-start",
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    append(
        journal,
        "event-tool-complete",
        SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: persisted_result(),
            },
        },
    );
    request_id
}

/// 为顺序测试持久化一个带指定 Round 下标和调用标识的已完成工具生命周期。
fn indexed_tool_request(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
    request_index: u32,
    tool_call_id: &str,
) -> ToolRequest {
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        turn_id,
        agent_id,
        1,
        tool_call_id,
    )
    .expect("Request ID 应派生");
    ToolRequest {
        request_id,
        turn_id: turn_id.clone(),
        agent_id: agent_id.clone(),
        model_round: 1,
        request_index,
        model_tool_call_id: tool_call_id.to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: json!({"path": format!("{tool_call_id}.rs")}),
        effect: ToolEffect::ReadOnly,
    }
}

/// 为顺序测试持久化一个带指定 Round 下标和调用标识的已完成工具生命周期。
fn completed_indexed_tool(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
    request_index: u32,
    tool_call_id: &str,
) -> RequestId {
    let request = indexed_tool_request(journal, turn_id, agent_id, request_index, tool_call_id);
    let request_id = request.request_id.clone();
    append(
        journal,
        &format!("event-tool-request-{tool_call_id}"),
        SessionEvent::ToolRequested { request },
    );
    append(
        journal,
        &format!("event-tool-start-{tool_call_id}"),
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    append(
        journal,
        &format!("event-tool-complete-{tool_call_id}"),
        SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: indexed_result(tool_call_id, false),
            },
        },
    );
    request_id
}

/// 构造顺序测试所需的模型可见工具结果。
fn indexed_result(tool_call_id: &str, is_error: bool) -> PersistedToolResult {
    PersistedToolResult {
        tool_call_id: tool_call_id.to_owned(),
        content: vec![ToolResultPart::Text {
            text: format!("{tool_call_id} 结果"),
        }],
        is_error,
    }
}

/// 按给定顺序构造多个工具调用；布尔值表示该调用是否为无生命周期的合成错误。
fn indexed_tool_segment(
    turn_id: &TurnId,
    agent_id: &AgentId,
    calls: &[(&str, bool)],
) -> TranscriptSegment {
    TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 0,
        messages: vec![
            SessionMessage {
                message_id: "message-indexed-assistant".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: calls
                    .iter()
                    .map(|(tool_call_id, _)| MessagePart::ToolCall {
                        tool_call_id: (*tool_call_id).to_owned(),
                        tool_name: "read_file".to_owned(),
                        arguments: json!({"path": format!("{tool_call_id}.rs")}),
                    })
                    .collect(),
            },
            SessionMessage {
                message_id: "message-indexed-tool".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Tool,
                content: calls
                    .iter()
                    .map(|(tool_call_id, is_error)| {
                        let result = indexed_result(tool_call_id, *is_error);
                        MessagePart::ToolResult {
                            tool_call_id: result.tool_call_id,
                            content: result.content,
                            is_error: result.is_error,
                        }
                    })
                    .collect(),
            },
        ],
    }
}

/// 构造与已完成工具生命周期一致的模型可见结果。
fn persisted_result() -> PersistedToolResult {
    PersistedToolResult {
        tool_call_id: "call-read".to_owned(),
        content: vec![ToolResultPart::Text {
            text: "文件内容".to_owned(),
        }],
        is_error: false,
    }
}

/// 构造包含完整工具调用与工具结果的单一 Transcript 段。
fn tool_segment(turn_id: &TurnId, agent_id: &AgentId) -> TranscriptSegment {
    TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 0,
        messages: vec![
            SessionMessage {
                message_id: "message-assistant".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::ToolCall {
                    tool_call_id: "call-read".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments: json!({"path": "src/lib.rs"}),
                }],
            },
            SessionMessage {
                message_id: "message-tool".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Tool,
                content: vec![MessagePart::ToolResult {
                    tool_call_id: "call-read".to_owned(),
                    content: persisted_result().content,
                    is_error: false,
                }],
            },
        ],
    }
}

/// 构造不含工具调用的单条 Assistant 消息段。
fn text_segment(
    turn_id: &TurnId,
    agent_id: &AgentId,
    segment_index: u32,
    expected_transcript_revision: u64,
    message_id: &str,
) -> TranscriptSegment {
    TranscriptSegment {
        turn_id: turn_id.clone(),
        source_agent_id: agent_id.clone(),
        model_round: 1,
        segment_index,
        expected_transcript_revision,
        messages: vec![SessionMessage {
            message_id: message_id.to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Assistant,
            content: vec![MessagePart::Text {
                text: "模型输出".to_owned(),
            }],
        }],
    }
}

/// 验证完整工具交换只增加一条物理 JSONL 记录并可完整重放。
#[test]
fn tool_exchange_is_one_physical_transcript_record() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = running_journal(root.path(), "segment-tool");
    let request_id = completed_tool(&journal, &turn_id, &agent_id);
    let before_lines = fs::read_to_string(journal.log_path())
        .expect("日志应读取")
        .lines()
        .count();
    assert!(matches!(
        journal.append_idempotent(
            SessionEventId::new("event-premature-turn-complete").expect("事件 ID 应有效"),
            journal.state().expect("状态应读取").last_sequence,
            SessionEvent::TurnCompleted {
                turn_id: turn_id.clone(),
            },
        ),
        Err(ResourceError::Reduction(_))
    ));
    append(
        &journal,
        "event-segment",
        SessionEvent::TranscriptSegmentCommitted {
            segment: tool_segment(&turn_id, &agent_id),
        },
    );
    let after_lines = fs::read_to_string(journal.log_path())
        .expect("日志应读取")
        .lines()
        .count();
    assert_eq!(after_lines, before_lines + 1);
    let state = journal.state().expect("状态应读取");
    assert_eq!(state.raw_transcript_messages().len(), 2);
    assert_eq!(state.transcript_revision, 1);
    assert_eq!(state.transcript_segments().count(), 1);
    assert!(
        state
            .tools
            .get(&request_id)
            .expect("工具生命周期应存在")
            .transcript_segment
            .is_some()
    );
}

/// 验证批内重复、孤儿结果、半个交换和生命周期不一致均整段拒绝。
#[test]
fn invalid_tool_segments_leave_log_and_state_unchanged() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = running_journal(root.path(), "segment-invalid");
    completed_tool(&journal, &turn_id, &agent_id);
    let baseline_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let baseline_state = journal.state().expect("状态应读取");

    let mut duplicate = text_segment(&turn_id, &agent_id, 0, 0, "duplicate");
    duplicate.messages.push(duplicate.messages[0].clone());
    let mut orphan = text_segment(&turn_id, &agent_id, 0, 0, "orphan");
    orphan.messages[0].role = MessageRole::Tool;
    orphan.messages[0].content = vec![MessagePart::ToolResult {
        tool_call_id: "missing-call".to_owned(),
        content: Vec::new(),
        is_error: false,
    }];
    let mut incomplete = tool_segment(&turn_id, &agent_id);
    incomplete.messages.pop();
    let mut mismatch = tool_segment(&turn_id, &agent_id);
    mismatch.messages[0].content = vec![MessagePart::ToolCall {
        tool_call_id: "call-read".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: json!({"path": "different.rs"}),
    }];

    for (index, segment) in [duplicate, orphan, incomplete, mismatch]
        .into_iter()
        .enumerate()
    {
        let result = journal.append_idempotent(
            SessionEventId::new(format!("event-invalid-{index}")).expect("事件 ID 应有效"),
            baseline_state.last_sequence,
            atomic_model_round_event(segment),
        );
        assert!(matches!(result, Err(ResourceError::Reduction(_))));
        assert_eq!(
            fs::read(journal.log_path()).expect("日志应读取"),
            baseline_bytes
        );
        assert_eq!(journal.state().expect("状态应读取"), baseline_state);
    }
}

/// 验证无生命周期只允许 Agent 合成错误，且工具结果不能早于调用。
#[test]
fn synthesized_tool_errors_and_message_order_follow_contract() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = running_journal(root.path(), "segment-synthetic-error");
    let mut error_segment = tool_segment(&turn_id, &agent_id);
    if let MessagePart::ToolResult { is_error, .. } = &mut error_segment.messages[1].content[0] {
        *is_error = true;
    }
    append(
        &journal,
        "event-synthetic-error",
        SessionEvent::TranscriptSegmentCommitted {
            segment: error_segment,
        },
    );

    let (success_journal, turn_id, agent_id) =
        running_journal(root.path(), "segment-synthetic-success");
    let success = success_journal.append_idempotent(
        SessionEventId::new("event-synthetic-success").expect("事件 ID 应有效"),
        success_journal.state().expect("状态应读取").last_sequence,
        atomic_model_round_event(tool_segment(&turn_id, &agent_id)),
    );
    assert!(matches!(success, Err(ResourceError::Reduction(_))));

    let mut reversed = tool_segment(&turn_id, &agent_id);
    reversed.messages.reverse();
    let reversed_result = success_journal.append_idempotent(
        SessionEventId::new("event-result-before-call").expect("事件 ID 应有效"),
        success_journal.state().expect("状态应读取").last_sequence,
        atomic_model_round_event(reversed),
    );
    assert!(matches!(reversed_result, Err(ResourceError::Reduction(_))));
}

/// 验证真实工具调用按 Transcript 出现顺序匹配严格递增的 request_index。
#[test]
fn reversed_real_tool_request_indexes_are_rejected_atomically() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) =
        running_journal(root.path(), "segment-request-index-reversed");
    completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    completed_indexed_tool(&journal, &turn_id, &agent_id, 1, "call-one");
    let baseline_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let baseline_state = journal.state().expect("状态应读取");

    let result = journal.append_idempotent(
        SessionEventId::new("event-reversed-request-index").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        atomic_model_round_event(indexed_tool_segment(
            &turn_id,
            &agent_id,
            &[("call-one", false), ("call-zero", false)],
        )),
    );

    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(
        fs::read(journal.log_path()).expect("日志应读取"),
        baseline_bytes
    );
    assert_eq!(journal.state().expect("状态应读取"), baseline_state);
}

/// 验证 request_index 允许间隙，并在日志重放后保留调用顺序和生命周期消费状态。
#[test]
fn increasing_tool_request_indexes_with_gaps_replay() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "segment-request-index-gap";
    let (journal, turn_id, agent_id) = running_journal(root.path(), session);
    let first = completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    let second = completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    append(
        &journal,
        "event-request-index-gap",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(
                &turn_id,
                &agent_id,
                &[("call-zero", false), ("call-two", false)],
            ),
        },
    );
    drop(journal);

    let replayed = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let state = replayed.state().expect("重放状态应读取");
    let call_ids = state
        .raw_transcript_messages()
        .into_iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            MessagePart::ToolCall { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(call_ids, vec!["call-zero", "call-two"]);
    assert!(state.tools[&first].transcript_segment.is_some());
    assert!(state.tools[&second].transcript_segment.is_some());
}

/// 验证高位生命周期不能越过已存在的低位，并可在低到高提交后完整重放。
#[test]
fn higher_request_index_waits_for_lower_then_converges_and_replays() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "segment-cross-request-index-reversed";
    let (journal, turn_id, agent_id) = running_journal(root.path(), session);
    let lower_id = completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    let higher_id = completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    let baseline_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let baseline_state = journal.state().expect("状态应读取");
    let early_higher = journal.append_idempotent(
        SessionEventId::new("event-cross-index-two-early").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        atomic_model_round_event(indexed_tool_segment(
            &turn_id,
            &agent_id,
            &[("call-two", false)],
        )),
    );
    assert!(matches!(early_higher, Err(ResourceError::Reduction(_))));
    assert_eq!(
        fs::read(journal.log_path()).expect("日志应读取"),
        baseline_bytes
    );
    assert_eq!(journal.state().expect("状态应读取"), baseline_state);

    append(
        &journal,
        "event-cross-index-zero",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(&turn_id, &agent_id, &[("call-zero", false)]),
        },
    );
    let mut higher = indexed_tool_segment(&turn_id, &agent_id, &[("call-two", false)]);
    higher.segment_index = 1;
    higher.expected_transcript_revision = 1;
    higher.messages[0].message_id = "message-indexed-assistant-higher-after-lower".to_owned();
    higher.messages[1].message_id = "message-indexed-tool-higher-after-lower".to_owned();
    append(
        &journal,
        "event-cross-index-two",
        SessionEvent::TranscriptSegmentCommitted { segment: higher },
    );
    drop(journal);

    let replayed = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let state = replayed.state().expect("重放状态应读取");
    assert_eq!(state.transcript_segments().count(), 2);
    assert!(state.tools[&lower_id].transcript_segment.is_some());
    assert!(state.tools[&higher_id].transcript_segment.is_some());
}

/// 验证同一 Round 跨段递增时允许 request_index 间隙并可重放。
#[test]
fn increasing_tool_request_indexes_across_segments_replay() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "segment-cross-request-index-gap";
    let (journal, turn_id, agent_id) = running_journal(root.path(), session);
    let first = completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    let second = completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    append(
        &journal,
        "event-cross-index-zero",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(&turn_id, &agent_id, &[("call-zero", false)]),
        },
    );
    let mut higher = indexed_tool_segment(&turn_id, &agent_id, &[("call-two", false)]);
    higher.segment_index = 1;
    higher.expected_transcript_revision = 1;
    higher.messages[0].message_id = "message-indexed-assistant-higher".to_owned();
    higher.messages[1].message_id = "message-indexed-tool-higher".to_owned();
    append(
        &journal,
        "event-cross-index-two",
        SessionEvent::TranscriptSegmentCommitted { segment: higher },
    );
    drop(journal);

    let replayed = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let state = replayed.state().expect("重放状态应读取");
    assert_eq!(state.transcript_segments().count(), 2);
    assert!(state.tools[&first].transcript_segment.is_some());
    assert!(state.tools[&second].transcript_segment.is_some());
}

/// 验证合成错误不参与 request_index 排序，也不能掩盖真实调用逆序。
#[test]
fn synthesized_tool_error_does_not_change_real_request_index_order() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) =
        running_journal(root.path(), "segment-request-index-synthetic");
    completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    append(
        &journal,
        "event-request-index-synthetic",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(
                &turn_id,
                &agent_id,
                &[
                    ("call-zero", false),
                    ("call-synthetic", true),
                    ("call-two", false),
                ],
            ),
        },
    );

    let (reversed, reversed_turn, reversed_agent) =
        running_journal(root.path(), "segment-request-index-synthetic-reversed");
    completed_indexed_tool(&reversed, &reversed_turn, &reversed_agent, 0, "call-zero");
    completed_indexed_tool(&reversed, &reversed_turn, &reversed_agent, 2, "call-two");
    let baseline_bytes = fs::read(reversed.log_path()).expect("日志应读取");
    let baseline_state = reversed.state().expect("状态应读取");
    let result = reversed.append_idempotent(
        SessionEventId::new("event-request-index-synthetic-reversed").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        atomic_model_round_event(indexed_tool_segment(
            &reversed_turn,
            &reversed_agent,
            &[
                ("call-two", false),
                ("call-synthetic", true),
                ("call-zero", false),
            ],
        )),
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(
        fs::read(reversed.log_path()).expect("日志应读取"),
        baseline_bytes
    );
    assert_eq!(reversed.state().expect("状态应读取"), baseline_state);
}

/// 验证合成错误调用标识跨同一 Round 的段仍唯一，但不会占用真实 request_index 水位。
#[test]
fn synthesized_tool_call_id_is_unique_across_segments_without_affecting_index_order() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) =
        running_journal(root.path(), "segment-synthetic-cross-segment");
    append(
        &journal,
        "event-synthetic-first",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(&turn_id, &agent_id, &[("call-synthetic", true)]),
        },
    );

    let baseline_bytes = fs::read(journal.log_path()).expect("重复合成调用前日志应读取");
    let baseline_state = journal.state().expect("重复合成调用前状态应读取");
    let mut duplicate = indexed_tool_segment(&turn_id, &agent_id, &[("call-synthetic", true)]);
    duplicate.segment_index = 1;
    duplicate.expected_transcript_revision = 1;
    duplicate.messages[0].message_id = "message-synthetic-assistant-duplicate".to_owned();
    duplicate.messages[1].message_id = "message-synthetic-tool-duplicate".to_owned();
    let result = journal.append_idempotent(
        SessionEventId::new("event-synthetic-duplicate").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        SessionEvent::TranscriptSegmentCommitted { segment: duplicate },
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_bytes
    );

    let colliding_request =
        indexed_tool_request(&journal, &turn_id, &agent_id, 0, "call-synthetic");
    let collision = journal.append_idempotent(
        SessionEventId::new("event-real-collides-with-synthetic").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        SessionEvent::ToolRequested {
            request: colliding_request,
        },
    );
    assert!(matches!(collision, Err(ResourceError::Reduction(_))));
    assert_eq!(
        journal.state().expect("冲突拒绝后状态应读取"),
        baseline_state
    );
    assert_eq!(
        fs::read(journal.log_path()).expect("冲突拒绝后日志应读取"),
        baseline_bytes
    );

    completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-real-zero");
    let mut real = indexed_tool_segment(&turn_id, &agent_id, &[("call-real-zero", false)]);
    real.segment_index = 1;
    real.expected_transcript_revision = 1;
    real.messages[0].message_id = "message-real-assistant-after-synthetic".to_owned();
    real.messages[1].message_id = "message-real-tool-after-synthetic".to_owned();
    append(
        &journal,
        "event-real-after-synthetic",
        SessionEvent::TranscriptSegmentCommitted { segment: real },
    );
    assert_eq!(
        journal
            .state()
            .expect("提交后状态应读取")
            .transcript_segments()
            .count(),
        2
    );
}

/// 验证已经物化高位空洞后，不能再补建低于消费水位的真实生命周期。
#[test]
fn tool_request_cannot_move_below_consumed_request_index_watermark() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) =
        running_journal(root.path(), "segment-request-index-consumed-watermark");
    completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    append(
        &journal,
        "event-consume-index-two",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(&turn_id, &agent_id, &[("call-two", false)]),
        },
    );
    let baseline_bytes = fs::read(journal.log_path()).expect("低位请求前日志应读取");
    let baseline_state = journal.state().expect("低位请求前状态应读取");

    let result = journal.append_idempotent(
        SessionEventId::new("event-late-index-zero").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        SessionEvent::ToolRequested {
            request: indexed_tool_request(&journal, &turn_id, &agent_id, 0, "call-zero"),
        },
    );

    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_bytes
    );
}

/// 验证公开归约器拒绝无法由事件流产生的工具索引、段引用和双向消费关系。
#[test]
fn public_reducer_rejects_forged_tool_transcript_consumption_state() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) =
        running_journal(root.path(), "public-forged-tool-consumption");
    let lower_id = completed_indexed_tool(&journal, &turn_id, &agent_id, 0, "call-zero");
    let higher_id = completed_indexed_tool(&journal, &turn_id, &agent_id, 2, "call-two");
    append(
        &journal,
        "event-public-consume-zero",
        SessionEvent::TranscriptSegmentCommitted {
            segment: indexed_tool_segment(&turn_id, &agent_id, &[("call-zero", false)]),
        },
    );
    let mut higher = indexed_tool_segment(&turn_id, &agent_id, &[("call-two", false)]);
    higher.segment_index = 1;
    higher.expected_transcript_revision = 1;
    higher.messages[0].message_id = "message-public-higher-assistant".to_owned();
    higher.messages[1].message_id = "message-public-higher-tool".to_owned();
    append(
        &journal,
        "event-public-consume-two",
        SessionEvent::TranscriptSegmentCommitted { segment: higher },
    );
    let baseline = journal.state().expect("完整消费状态应读取");

    let mut mismatched_map_key = baseline.clone();
    let lifecycle = mismatched_map_key
        .tools
        .remove(&lower_id)
        .expect("低位生命周期应存在");
    let wrong_key = RequestId::derive_model_tool_call(
        &baseline.session_id,
        &turn_id,
        &agent_id,
        1,
        "call-wrong-map-key",
    )
    .expect("错误 map key 夹具仍应是有效 Request ID");
    mismatched_map_key.tools.insert(wrong_key, lifecycle);
    assert_public_state_rejected(mismatched_map_key, &turn_id, &agent_id, "map-key");

    let mut nonexistent_reference = baseline.clone();
    nonexistent_reference
        .tools
        .get_mut(&lower_id)
        .expect("低位生命周期应存在")
        .transcript_segment
        .as_mut()
        .expect("低位生命周期应已物化")
        .segment_index = 99;
    assert_public_state_rejected(nonexistent_reference, &turn_id, &agent_id, "reference");

    let mut missing_reverse_link = baseline.clone();
    missing_reverse_link
        .tools
        .get_mut(&lower_id)
        .expect("低位生命周期应存在")
        .transcript_segment = None;
    assert_public_state_rejected(missing_reverse_link, &turn_id, &agent_id, "reverse-link");

    let mut reversed_indexes = baseline.clone();
    reversed_indexes
        .tools
        .get_mut(&lower_id)
        .expect("低位生命周期应存在")
        .request
        .request_index = 2;
    reversed_indexes
        .tools
        .get_mut(&higher_id)
        .expect("高位生命周期应存在")
        .request
        .request_index = 0;
    assert_public_state_rejected(reversed_indexes, &turn_id, &agent_id, "order");

    let mut lower_than_watermark = baseline.clone();
    let mut forged_unconsumed = lower_than_watermark
        .tools
        .get(&higher_id)
        .expect("高位生命周期应存在")
        .clone();
    let forged_id = RequestId::derive_model_tool_call(
        &baseline.session_id,
        &turn_id,
        &agent_id,
        1,
        "call-one-late",
    )
    .expect("伪造低位请求 ID 应有效");
    forged_unconsumed.request.request_id = forged_id.clone();
    forged_unconsumed.request.request_index = 1;
    forged_unconsumed.request.model_tool_call_id = "call-one-late".to_owned();
    forged_unconsumed.transcript_segment = None;
    lower_than_watermark
        .tools
        .insert(forged_id, forged_unconsumed);
    assert_public_state_rejected(lower_than_watermark, &turn_id, &agent_id, "watermark");
}

/// 验证一个工具生命周期不能被后续 Transcript 段再次消费。
#[test]
fn tool_lifecycle_can_be_materialized_only_once() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = running_journal(root.path(), "segment-consumed-once");
    completed_tool(&journal, &turn_id, &agent_id);
    append(
        &journal,
        "event-first-segment",
        SessionEvent::TranscriptSegmentCommitted {
            segment: tool_segment(&turn_id, &agent_id),
        },
    );
    let mut duplicate = tool_segment(&turn_id, &agent_id);
    duplicate.segment_index = 1;
    duplicate.expected_transcript_revision = 1;
    duplicate.messages[0].message_id = "message-assistant-duplicate".to_owned();
    duplicate.messages[1].message_id = "message-tool-duplicate".to_owned();
    let result = journal.append_idempotent(
        SessionEventId::new("event-duplicate-consumption").expect("事件 ID 应有效"),
        journal.state().expect("状态应读取").last_sequence,
        SessionEvent::TranscriptSegmentCommitted { segment: duplicate },
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
}

/// 验证同一模型 Round 的段序号连续推进，并由 Transcript revision 串行化。
#[test]
fn same_round_accepts_consecutive_segment_indexes() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, turn_id, agent_id) = running_journal(root.path(), "segment-index");
    append(
        &journal,
        "event-segment-0",
        SessionEvent::TranscriptSegmentCommitted {
            segment: text_segment(&turn_id, &agent_id, 0, 0, "message-0"),
        },
    );
    append(
        &journal,
        "event-segment-1",
        SessionEvent::TranscriptSegmentCommitted {
            segment: text_segment(&turn_id, &agent_id, 1, 1, "message-1"),
        },
    );
    let state = journal.state().expect("状态应读取");
    assert_eq!(state.transcript_revision, 2);
    assert_eq!(state.transcript_segments().count(), 2);
    assert_eq!(state.raw_transcript_messages().len(), 2);
}

/// 验证单行 Transcript 段发生掉电截断时不会恢复其中任何消息。
#[test]
fn truncated_segment_line_recovers_zero_messages_from_that_segment() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "segment-truncated";
    let (journal, turn_id, agent_id) = running_journal(root.path(), session);
    append(
        &journal,
        "event-segment",
        SessionEvent::TranscriptSegmentCommitted {
            segment: text_segment(&turn_id, &agent_id, 0, 0, "message-truncated"),
        },
    );
    let log_path = journal.log_path().to_path_buf();
    let bytes = fs::read(&log_path).expect("日志应读取");
    let previous_newline = bytes[..bytes.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .expect("段前应存在完整记录");
    let segment_start = previous_newline + 1;
    let truncated_end = segment_start + (bytes.len() - segment_start) / 2;
    fs::write(&log_path, &bytes[..truncated_end]).expect("截断日志应写入");
    drop(journal);

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("截断日志应返回报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("截断段必须进入只读损坏状态");
    };
    assert!(report.last_valid_state.raw_transcript_messages().is_empty());
    assert_eq!(report.last_valid_state.transcript_segments().count(), 0);
    assert_eq!(report.last_valid_state.transcript_revision, 0);
}
