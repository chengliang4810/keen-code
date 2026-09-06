mod support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, CorruptionKind, DocumentLimits, Durability, GoalDocument, GoalFileStore, GoalRecord,
    GoalStatus, JournalConfig, MemoryDocument, MemoryEntry, MemoryFileStore, MessagePart,
    MessageRole, PersistedToolResult, RequestId, ResourceError, ScopeId, SessionEvent, SessionId,
    SessionJournal, SessionMessage, SessionOpen, SnapshotPolicy, ToolCompletionStatus, ToolEffect,
    ToolOutcome, ToolRequest, ToolResultPart, TranscriptSegment, TurnId,
};
use serde_json::json;
use tempfile::TempDir;

use support::TestJournalAppend;

/// 返回显式事件和日志上限的测试配置。
fn bounded_config(max_event_bytes: u64, max_log_bytes: u64) -> JournalConfig {
    JournalConfig {
        durability: Durability::Buffered,
        snapshot_policy: SnapshotPolicy::Disabled,
        max_event_bytes,
        max_log_bytes,
        max_records: 100_000,
        max_state_collection_items: 50_000,
    }
}

/// 使用指定配置打开一个健康 Session。
fn ready(root: &Path, session: &str, config: JournalConfig) -> SessionJournal {
    let opened = SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config,
    )
    .expect("Session 应打开");
    let SessionOpen::Ready(journal) = opened else {
        panic!("Session 不应损坏");
    };
    journal
}

/// 写入一个最小合法 SessionCreated 事件。
fn create_session(journal: &SessionJournal) {
    journal
        .append(SessionEvent::SessionCreated {
            title: "限制测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("SessionCreated 应成功");
}

/// 为集合上限测试创建一个 Running Turn。
fn start_turn(journal: &SessionJournal, name: &str) -> TurnId {
    let turn_id = TurnId::new(name).expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "集合上限测试".to_owned(),
        })
        .expect("Turn 应开始");
    turn_id
}

/// 构造模型 Round 完成事件与首个 Transcript 段的标准原子批次。
fn model_round_batch(
    turn_id: &TurnId,
    agent_id: &AgentId,
    segment: TranscriptSegment,
) -> SessionEvent {
    SessionEvent::AtomicBatch {
        events: vec![
            SessionEvent::ModelRoundCompleted {
                turn_id: turn_id.clone(),
                source_agent_id: agent_id.clone(),
                model_round: segment.model_round,
                requested_model: "storage-limit-test-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some("storage-limit-test-response".to_owned()),
                    model: Some("storage-limit-test-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason: StopReason::Completed,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 构造一个属于固定根 Agent 的只读工具请求。
fn root_tool_request(
    journal: &SessionJournal,
    turn_id: &TurnId,
    model_tool_call_id: &str,
    arguments: serde_json::Value,
) -> ToolRequest {
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    ToolRequest {
        request_id: RequestId::derive_model_tool_call(
            &journal.state().expect("状态应读取").session_id,
            turn_id,
            &agent_id,
            1,
            model_tool_call_id,
        )
        .expect("Request ID 应派生"),
        turn_id: turn_id.clone(),
        agent_id,
        model_round: 1,
        request_index: 0,
        model_tool_call_id: model_tool_call_id.to_owned(),
        tool_name: "read".to_owned(),
        arguments,
        effect: ToolEffect::ReadOnly,
    }
}

/// 构造一个 Session 级系统消息。
fn message(index: usize, text: String) -> SessionEvent {
    SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: format!("message-{index}"),
            turn_id: None,
            agent_id: None,
            role: MessageRole::System,
            content: vec![MessagePart::Text { text }],
        },
    }
}

/// 构造首次 CAS 可接受的零用量 Active Goal。
fn active_goal(id: &str) -> GoalRecord {
    GoalRecord {
        id: id.to_owned(),
        title: "持久目标".to_owned(),
        scope: "project".to_owned(),
        status: GoalStatus::Active,
        description: Some("验证 Goal 生命周期".to_owned()),
        progress_percent: Some(10),
        objective: "完成资源层验证".to_owned(),
        token_budget: Some(10_000),
        tokens_used: 0,
        time_used_seconds: 0,
        blocked_reason: None,
        completion_evidence: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    }
}

/// 使用稳定测试操作载荷提交 Goal CAS，并解包当前文档。
fn goal_cas(
    store: &GoalFileStore,
    operation_id: &str,
    expected_revision: u64,
    mut document: GoalDocument,
) -> Result<GoalDocument, ResourceError> {
    document.revision = expected_revision;
    store
        .compare_and_swap(
            operation_id,
            &("goal_test_operation_v1", operation_id),
            expected_revision,
            document,
        )
        .map(|outcome| outcome.into_document())
}

/// 验证 Active Goal 不可直接清除或换标识，终态不可重开，合法终态可清除。
#[test]
fn goal_cas_enforces_irreversible_single_goal_lifecycle() {
    let root = TempDir::new().expect("临时目录应创建");
    let store = GoalFileStore::open(root.path()).expect("GoalStore 应打开");
    let completed_scope = ScopeId::new("goal-completed").expect("Scope 应有效");
    let first = goal_cas(
        &store,
        "goal-create-completed",
        0,
        GoalDocument::new(
            completed_scope.clone(),
            Some(active_goal("019d0000-0000-7000-8000-000000000001")),
        ),
    )
    .expect("Active Goal 应创建");

    assert!(matches!(
        goal_cas(
            &store,
            "goal-clear-active",
            first.revision,
            GoalDocument::new(completed_scope.clone(), None)
        ),
        Err(ResourceError::InvalidGoalTransition(_))
    ));
    let mut replaced = first.goal.clone().expect("Goal 应存在");
    replaced.id = "019d0000-0000-7000-8000-000000000002".to_owned();
    replaced.updated_at_unix_ms += 1;
    assert!(matches!(
        goal_cas(
            &store,
            "goal-replace-id",
            first.revision,
            GoalDocument::new(completed_scope.clone(), Some(replaced))
        ),
        Err(ResourceError::InvalidGoalTransition(_))
    ));

    let mut completed = first.goal.clone().expect("Goal 应存在");
    completed.status = GoalStatus::Completed;
    completed.completion_evidence = Some("资源生命周期验收通过".to_owned());
    completed.updated_at_unix_ms += 1;
    let completed = goal_cas(
        &store,
        "goal-transition-completed",
        first.revision,
        GoalDocument::new(completed_scope.clone(), Some(completed)),
    )
    .expect("Active 应进入 Completed");
    let mut reactivated = completed.goal.clone().expect("Goal 应存在");
    reactivated.status = GoalStatus::Active;
    reactivated.completion_evidence = None;
    reactivated.updated_at_unix_ms += 1;
    assert!(matches!(
        goal_cas(
            &store,
            "goal-reactivate-completed",
            completed.revision,
            GoalDocument::new(completed_scope.clone(), Some(reactivated))
        ),
        Err(ResourceError::InvalidGoalTransition(_))
    ));
    let retired_id = completed.goal.as_ref().expect("Goal 应存在").id.clone();
    let cleared = goal_cas(
        &store,
        "goal-clear-completed",
        completed.revision,
        GoalDocument::new(completed_scope.clone(), None),
    )
    .expect("Completed Goal 应清除");
    assert!(cleared.goal.is_none());
    assert_eq!(cleared.retired_goal_ids, vec![retired_id.clone()]);
    assert!(matches!(
        goal_cas(
            &store,
            "goal-recreate-retired-id",
            cleared.revision,
            GoalDocument::new(completed_scope.clone(), Some(active_goal(&retired_id)))
        ),
        Err(ResourceError::InvalidGoalTransition(_))
    ));
    let replacement = goal_cas(
        &store,
        "goal-create-replacement",
        cleared.revision,
        GoalDocument::new(
            completed_scope,
            Some(active_goal("019d0000-0000-7000-8000-000000000003")),
        ),
    )
    .expect("清除终态后应创建新 Goal");
    assert_eq!(replacement.revision, 4);

    let blocked_scope = ScopeId::new("goal-blocked").expect("Scope 应有效");
    let active = goal_cas(
        &store,
        "goal-create-blocked",
        0,
        GoalDocument::new(
            blocked_scope.clone(),
            Some(active_goal("019d0000-0000-7000-8000-000000000004")),
        ),
    )
    .expect("Active Goal 应创建");
    let mut blocked = active.goal.clone().expect("Goal 应存在");
    blocked.status = GoalStatus::Blocked;
    blocked.blocked_reason = Some("等待外部输入".to_owned());
    blocked.updated_at_unix_ms += 1;
    let blocked = goal_cas(
        &store,
        "goal-transition-blocked",
        active.revision,
        GoalDocument::new(blocked_scope.clone(), Some(blocked)),
    )
    .expect("Active 应进入 Blocked");
    goal_cas(
        &store,
        "goal-clear-blocked",
        blocked.revision,
        GoalDocument::new(blocked_scope, None),
    )
    .expect("Blocked Goal 应清除");
}

/// 验证单事件超限在任何日志写入和 sequence 更新前失败。
#[test]
fn oversized_append_preserves_log_bytes_and_sequence() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "event-limit", bounded_config(512, 4 * 1024));
    create_session(&journal);
    let before_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let before_state = journal.state().expect("状态应读取");
    let result = journal.append(message(1, "x".repeat(2_048)));
    assert!(matches!(result, Err(ResourceError::EventTooLarge { .. })));
    assert_eq!(
        fs::read(journal.log_path()).expect("日志应读取"),
        before_bytes
    );
    assert_eq!(journal.state().expect("状态应读取"), before_state);
    let receipt = journal
        .append(message(2, "短消息".to_owned()))
        .expect("超限拒绝后 sequence 应可继续使用");
    assert_eq!(receipt.record.sequence, 2);
}

/// 验证日志总量超限时最后一次候选追加不会改变文件或内存状态。
#[test]
fn journal_limit_rejection_is_atomic() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "journal-limit", bounded_config(512, 512));
    create_session(&journal);

    let mut rejected = false;
    for index in 0..16 {
        let before_bytes = fs::read(journal.log_path()).expect("日志应读取");
        let before_state = journal.state().expect("状态应读取");
        match journal.append(message(index, "填充日志".to_owned())) {
            Ok(_) => {}
            Err(ResourceError::JournalTooLarge { actual, limit }) => {
                assert!(actual > limit);
                assert_eq!(
                    fs::read(journal.log_path()).expect("日志应读取"),
                    before_bytes
                );
                assert_eq!(journal.state().expect("状态应读取"), before_state);
                rejected = true;
                break;
            }
            Err(error) => panic!("应返回 JournalTooLarge，实际为 {error}"),
        }
    }
    assert!(rejected, "有限循环内应达到日志上限");
}

/// 验证事件数量和归约集合数量上限都在日志提交前原子拒绝。
#[test]
fn record_and_state_collection_limits_are_atomic() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut record_limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    record_limited.max_records = 1;
    let journal = ready(root.path(), "record-count-limit", record_limited);
    create_session(&journal);
    let before = fs::read(journal.log_path()).expect("日志应读取");
    assert!(matches!(
        journal.append(message(1, "不得写入".to_owned())),
        Err(ResourceError::JournalRecordLimit {
            actual: 2,
            limit: 1
        })
    ));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
    assert_eq!(journal.state().expect("状态应读取").last_sequence, 1);

    let mut collection_limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    collection_limited.max_state_collection_items = 1;
    let journal = ready(root.path(), "state-collection-limit", collection_limited);
    create_session(&journal);
    let first = keencode_resources::TurnId::new("turn-one").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: first.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: first.clone(),
            parent_turn_id: None,
            prompt_summary: "第一个 Turn".to_owned(),
        })
        .expect("首个 Turn 应开始");
    journal
        .append(SessionEvent::TurnCompleted { turn_id: first })
        .expect("首个 Turn 应完成");
    let before = fs::read(journal.log_path()).expect("日志应读取");
    let second = keencode_resources::TurnId::new("turn-two").expect("Turn ID 应有效");
    assert!(matches!(
        journal.append(SessionEvent::TurnStarted {
            turn_id: second.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: second,
            parent_turn_id: None,
            prompt_summary: "第二个 Turn".to_owned(),
        }),
        Err(ResourceError::StateCollectionLimit {
            collection: "turns",
            actual: 2,
            limit: 1
        })
    ));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
    assert_eq!(journal.state().expect("状态应读取").turns.len(), 1);
}

/// 验证 Transcript 记录与段达到集合上限后，下一段在写日志前被原子拒绝。
#[test]
fn transcript_record_and_segment_limits_are_atomic() {
    let root = TempDir::new().expect("临时目录应创建");
    let mut limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    limited.max_state_collection_items = 2;
    let journal = ready(root.path(), "transcript-collection-limit", limited);
    create_session(&journal);
    let turn_id = TurnId::new("turn-main").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证 Transcript 集合上限".to_owned(),
        })
        .expect("Turn 应开始");

    for index in 0..2_u32 {
        let segment = TranscriptSegment {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            model_round: 1,
            segment_index: index,
            expected_transcript_revision: u64::from(index),
            messages: vec![SessionMessage {
                message_id: format!("segment-message-{index}"),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::Text {
                    text: format!("第 {index} 段"),
                }],
            }],
        };
        let event = if index == 0 {
            model_round_batch(&turn_id, &agent_id, segment)
        } else {
            SessionEvent::TranscriptSegmentCommitted { segment }
        };
        journal.append(event).expect("上限内 Transcript 段应提交");
    }

    let before_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let before_state = journal.state().expect("状态应读取");
    let result = journal.append(SessionEvent::TranscriptSegmentCommitted {
        segment: TranscriptSegment {
            turn_id,
            source_agent_id: agent_id,
            model_round: 1,
            segment_index: 2,
            expected_transcript_revision: 2,
            messages: vec![SessionMessage {
                message_id: "segment-message-2".to_owned(),
                turn_id: Some(TurnId::new("turn-main").expect("Turn ID 应有效")),
                agent_id: Some(AgentId::new("root").expect("Agent ID 应有效")),
                role: MessageRole::Assistant,
                content: vec![MessagePart::Text {
                    text: "超限段".to_owned(),
                }],
            }],
        },
    });
    assert!(matches!(
        result,
        Err(ResourceError::StateCollectionLimit {
            collection: "transcript",
            actual: 3,
            limit: 2
        })
    ));
    assert_eq!(
        fs::read(journal.log_path()).expect("日志应读取"),
        before_bytes
    );
    assert_eq!(journal.state().expect("状态应读取"), before_state);
    assert_eq!(before_state.transcript.len(), 2);
    assert_eq!(before_state.transcript_segments().count(), 2);
    assert_eq!(before_state.transcript_revision, 2);
}

/// 验证工具结果、Transcript 嵌套结果和深层 JSON 集合都在写日志前受递归上限保护。
#[test]
fn recursive_state_collection_limits_cover_all_nested_payloads_atomically() {
    let root = TempDir::new().expect("临时目录应创建");

    let mut outcome_limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    outcome_limited.max_state_collection_items = 2;
    let journal = ready(root.path(), "tool-outcome-nested-limit", outcome_limited);
    create_session(&journal);
    let turn_id = start_turn(&journal, "turn-tool-outcome-limit");
    let request = root_tool_request(&journal, &turn_id, "call-outcome-limit", json!({}));
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("工具请求应记录");
    journal
        .append(SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        })
        .expect("工具执行起点应记录");
    let before_state = journal.state().expect("工具结果前状态应读取");
    let before_bytes = fs::read(journal.log_path()).expect("工具结果前日志应读取");
    assert!(matches!(
        journal.append(SessionEvent::ToolCompleted {
            request_id,
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: PersistedToolResult {
                    tool_call_id: "call-outcome-limit".to_owned(),
                    content: (0..3)
                        .map(|index| ToolResultPart::Text {
                            text: format!("结果 {index}"),
                        })
                        .collect(),
                    is_error: false,
                },
            },
        }),
        Err(ResourceError::StateCollectionLimit {
            collection: "tool_outcome_result_content",
            actual: 3,
            limit: 2
        })
    ));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), before_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        before_bytes
    );

    let mut transcript_limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    transcript_limited.max_state_collection_items = 2;
    let journal = ready(
        root.path(),
        "transcript-tool-result-nested-limit",
        transcript_limited,
    );
    create_session(&journal);
    let turn_id = start_turn(&journal, "turn-transcript-result-limit");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    let before_state = journal.state().expect("Transcript 前状态应读取");
    let before_bytes = fs::read(journal.log_path()).expect("Transcript 前日志应读取");
    assert!(matches!(
        journal.append(model_round_batch(
            &turn_id,
            &agent_id,
            TranscriptSegment {
                turn_id: turn_id.clone(),
                source_agent_id: agent_id.clone(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: 0,
                messages: vec![
                    SessionMessage {
                        message_id: "nested-call".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Assistant,
                        content: vec![MessagePart::ToolCall {
                            tool_call_id: "synthetic-call".to_owned(),
                            tool_name: "missing_tool".to_owned(),
                            arguments: json!({}),
                        }],
                    },
                    SessionMessage {
                        message_id: "nested-result".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Tool,
                        content: vec![MessagePart::ToolResult {
                            tool_call_id: "synthetic-call".to_owned(),
                            content: (0..3)
                                .map(|index| ToolResultPart::Text {
                                    text: format!("合成错误 {index}"),
                                })
                                .collect(),
                            is_error: true,
                        }],
                    },
                ],
            },
        )),
        Err(ResourceError::StateCollectionLimit {
            collection: "message_tool_result_content",
            actual: 3,
            limit: 2
        })
    ));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), before_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        before_bytes
    );

    let mut json_limited = bounded_config(1024 * 1024, 4 * 1024 * 1024);
    json_limited.max_state_collection_items = 3;
    let journal = ready(root.path(), "deep-json-collection-limit", json_limited);
    create_session(&journal);
    let turn_id = start_turn(&journal, "turn-deep-json-limit");
    let request = root_tool_request(
        &journal,
        &turn_id,
        "call-deep-json",
        json!({"outer": {"items": [1, 2, 3]}}),
    );
    let before_state = journal.state().expect("深层 JSON 前状态应读取");
    let before_bytes = fs::read(journal.log_path()).expect("深层 JSON 前日志应读取");
    assert!(matches!(
        journal.append(SessionEvent::ToolRequested { request }),
        Err(ResourceError::StateCollectionLimit {
            collection: "json_collection_items",
            actual: 5,
            limit: 3
        })
    ));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), before_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        before_bytes
    );
}

/// 验证加载先执行总量限制，并把超限完整行归类为稳定损坏事实。
#[test]
fn load_rejects_oversized_journal_and_reports_oversized_event() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "load-journal-limit",
        bounded_config(1024 * 1024, 4 * 1024 * 1024),
    );
    create_session(&journal);
    journal
        .append(message(1, "健康消息".to_owned()))
        .expect("消息应追加");
    let log_len = fs::metadata(journal.log_path())
        .expect("日志 metadata 应读取")
        .len();
    drop(journal);
    let result = SessionJournal::open(
        root.path(),
        SessionId::new("load-journal-limit").expect("Session ID 应有效"),
        bounded_config(1, log_len - 1),
    );
    assert!(matches!(result, Err(ResourceError::JournalTooLarge { .. })));

    let journal = ready(
        root.path(),
        "load-event-limit",
        bounded_config(1024 * 1024, 4 * 1024 * 1024),
    );
    create_session(&journal);
    let bytes = fs::read(journal.log_path()).expect("日志应读取");
    let event_len = u64::try_from(bytes.len()).expect("日志长度应可表示");
    drop(journal);
    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("load-event-limit").expect("Session ID 应有效"),
        bounded_config(event_len - 1, event_len),
    )
    .expect("事件超限应返回只读报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("超限事件必须进入只读报告");
    };
    assert_eq!(report.valid_records, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue.kind, CorruptionKind::EventTooLarge { line: 1, .. }))
    );
}

/// 验证过大的截断尾记录会阻止显式尾部恢复。
#[test]
fn oversized_truncated_tail_cannot_be_recovered_as_a_normal_tail() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "oversized-tail",
        bounded_config(1024 * 1024, 4 * 1024 * 1024),
    );
    create_session(&journal);
    let log_path = journal.log_path().to_owned();
    let valid_len = fs::metadata(&log_path).expect("日志 metadata 应读取").len();
    drop(journal);
    let tail = vec![b'x'; usize::try_from(valid_len + 1).expect("尾部长度应可表示")];
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .expect("日志应打开");
    file.write_all(&tail).expect("截断尾部应写入");
    drop(file);
    let total_len = fs::metadata(&log_path).expect("日志 metadata 应读取").len();
    let config = bounded_config(valid_len, total_len);

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("oversized-tail").expect("Session ID 应有效"),
        config,
    )
    .expect("损坏应返回只读报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("截断尾部必须进入只读报告");
    };
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue.kind, CorruptionKind::TruncatedTail { .. }))
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue.kind, CorruptionKind::EventTooLarge { line: 2, .. }))
    );
    assert!(matches!(
        SessionJournal::recover_truncated_tail(
            root.path(),
            SessionId::new("oversized-tail").expect("Session ID 应有效"),
            config,
        ),
        Err(ResourceError::TruncatedTailRecoveryNotApplicable)
    ));
}

/// 验证 Snapshot 编码和既有 Snapshot 读取都受配置上限约束。
#[test]
fn snapshot_size_limit_prevents_write_and_ignores_oversized_cache() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "snapshot-limit",
        bounded_config(1024 * 1024, 4 * 1024 * 1024),
    );
    create_session(&journal);
    let log_len = fs::metadata(journal.log_path())
        .expect("日志 metadata 应读取")
        .len();
    drop(journal);
    let tight = bounded_config(log_len, log_len);
    let journal = ready(root.path(), "snapshot-limit", tight);
    assert!(matches!(
        journal.write_snapshot(),
        Err(ResourceError::SnapshotTooLarge { .. })
    ));
    let snapshot_path = journal.snapshot_path().to_owned();
    assert!(!snapshot_path.exists());
    drop(journal);

    let oversized = vec![b'x'; usize::try_from(log_len + 1).expect("Snapshot 长度应可表示")];
    fs::write(&snapshot_path, &oversized).expect("超限 Snapshot 应写入测试夹具");
    let reopened = ready(root.path(), "snapshot-limit", tight);
    assert_eq!(reopened.state().expect("状态应恢复").last_sequence, 1);
    assert_eq!(
        fs::read(&snapshot_path).expect("Snapshot 应读取"),
        oversized
    );
}

/// 验证 Memory 与 Goal 的读写都在 JSON 分配和原子替换边界受限。
#[test]
fn document_size_limits_cover_memory_and_goal_reads_and_writes() {
    let root = TempDir::new().expect("临时目录应创建");
    let limits = DocumentLimits {
        max_document_bytes: 128,
    };
    let memory_scope = ScopeId::new("memory-write-limit").expect("Scope 应有效");
    let memories =
        MemoryFileStore::open_with_limits(root.path(), limits).expect("MemoryStore 应打开");
    let memory = MemoryDocument::new(
        memory_scope.clone(),
        vec![MemoryEntry {
            memory_id: "memory-large".to_owned(),
            content: "x".repeat(1_024),
            updated_at_unix_ms: 1,
            tags: Vec::new(),
        }],
    );
    assert!(matches!(
        memories.compare_and_swap(0, memory),
        Err(ResourceError::DocumentTooLarge { .. })
    ));
    assert!(
        !root
            .path()
            .join("memories")
            .join("memory-write-limit.json")
            .exists()
    );

    let goal_scope = ScopeId::new("goal-write-limit").expect("Scope 应有效");
    let goals = GoalFileStore::open_with_limits(root.path(), limits).expect("GoalStore 应打开");
    assert!(matches!(
        goal_cas(
            &goals,
            "goal-write-too-large",
            0,
            GoalDocument::new(
                goal_scope.clone(),
                Some(active_goal("019d0000-0000-7000-8000-000000000010")),
            )
        ),
        Err(ResourceError::DocumentTooLarge { .. })
    ));
    assert!(
        !root
            .path()
            .join("goals")
            .join("goal-write-limit.json")
            .exists()
    );

    let readable_memory_scope = ScopeId::new("memory-read-limit").expect("Scope 应有效");
    let default_memories = MemoryFileStore::open(root.path()).expect("MemoryStore 应打开");
    default_memories
        .compare_and_swap(
            0,
            MemoryDocument::new(
                readable_memory_scope.clone(),
                vec![MemoryEntry {
                    memory_id: "memory-readable".to_owned(),
                    content: "可读取文档".to_owned(),
                    updated_at_unix_ms: 1,
                    tags: Vec::new(),
                }],
            ),
        )
        .expect("Memory 测试文档应写入");
    let limited_memories = MemoryFileStore::open_with_limits(
        root.path(),
        DocumentLimits {
            max_document_bytes: 1,
        },
    )
    .expect("受限 MemoryStore 应打开");
    assert!(matches!(
        limited_memories.read(&readable_memory_scope),
        Err(ResourceError::DocumentTooLarge { .. })
    ));

    let readable_goal_scope = ScopeId::new("goal-read-limit").expect("Scope 应有效");
    let default_goals = GoalFileStore::open(root.path()).expect("GoalStore 应打开");
    goal_cas(
        &default_goals,
        "goal-readable-write",
        0,
        GoalDocument::new(
            readable_goal_scope.clone(),
            Some(active_goal("019d0000-0000-7000-8000-000000000011")),
        ),
    )
    .expect("Goal 测试文档应写入");
    let limited_goals = GoalFileStore::open_with_limits(
        root.path(),
        DocumentLimits {
            max_document_bytes: 1,
        },
    )
    .expect("受限 GoalStore 应打开");
    assert!(matches!(
        limited_goals.read(&readable_goal_scope),
        Err(ResourceError::DocumentTooLarge { .. })
    ));
}

/// 验证零上限和单事件大于总日志上限的配置在访问 Session 前被拒绝。
#[test]
fn invalid_storage_limits_are_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    for config in [bounded_config(0, 1), bounded_config(2, 1)] {
        assert!(matches!(
            SessionJournal::open(
                root.path(),
                SessionId::new("invalid-limits").expect("Session ID 应有效"),
                config,
            ),
            Err(ResourceError::UnsafePath(_))
        ));
    }
    assert!(matches!(
        MemoryFileStore::open_with_limits(
            root.path(),
            DocumentLimits {
                max_document_bytes: 0,
            },
        ),
        Err(ResourceError::UnsafePath(_))
    ));
    let mut invalid_records = bounded_config(1, 1);
    invalid_records.max_records = 0;
    assert!(matches!(
        SessionJournal::open(
            root.path(),
            SessionId::new("invalid-record-limit").expect("Session ID 应有效"),
            invalid_records,
        ),
        Err(ResourceError::UnsafePath(_))
    ));
    let mut invalid_collection = bounded_config(1, 1);
    invalid_collection.max_state_collection_items = 0;
    assert!(matches!(
        SessionJournal::open(
            root.path(),
            SessionId::new("invalid-collection-limit").expect("Session ID 应有效"),
            invalid_collection,
        ),
        Err(ResourceError::UnsafePath(_))
    ));
}
