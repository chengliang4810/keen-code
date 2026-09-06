mod support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, ArtifactId, ArtifactLimits, ArtifactMaterialization, ArtifactStore, ArtifactUse,
    CompactionRecord, ContextCompressionTrigger, CorruptionKind, DocumentOperationOutcome,
    Durability, GoalDocument, GoalFileStore, GoalRecord, GoalSnapshot, GoalStatus, JournalConfig,
    MAX_REPLAY_PAGE_RECORDS, MailboxMessage, MailboxMessageId, MailboxState, MemoryDocument,
    MemoryEntry, MemoryFileStore, MessagePart, MessageRole, PersistedToolResult, PlanState,
    ProviderProtocolSnapshot, ProviderSnapshot, ReasoningEffortSnapshot, RequestId, ResourceError,
    ScopeId, SessionEvent, SessionEventRecord, SessionId, SessionJournal, SessionMessage,
    SessionOpen, SessionState, SessionStatus, SnapshotPolicy, SubAgentState, SubAgentStatus,
    TerminalId, TerminalRecord, TodoItem, ToolCompletionStatus, ToolEffect, ToolOutcome,
    ToolRequest, ToolResultPart, TranscriptSegment, TurnId, TurnStopReason, WorktreeRecord,
    filesystem_capabilities, project_scope_id,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use support::TestJournalAppend;

/// 创建测试专用的强持久化配置。
fn config(snapshot_policy: SnapshotPolicy) -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy,
        ..JournalConfig::default()
    }
}

/// 推理强度持久格式必须使用公开七档名称，不泄漏模型 crate 的内部枚举拼写。
#[test]
fn reasoning_effort_snapshot_round_trips_stable_public_values() {
    let cases = [
        (ReasoningEffortSnapshot::Minimal, "minimal"),
        (ReasoningEffortSnapshot::Low, "low"),
        (ReasoningEffortSnapshot::Medium, "medium"),
        (ReasoningEffortSnapshot::High, "high"),
        (ReasoningEffortSnapshot::ExtraHigh, "xhigh"),
        (ReasoningEffortSnapshot::Maximum, "max"),
    ];
    for (effort, expected) in cases {
        let provider = ProviderSnapshot {
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
            context_window: Some(128_000),
            protocol: ProviderProtocolSnapshot::OpenAiResponses,
            config_fingerprint: "sha256:test".to_owned(),
            reasoning_effort: Some(effort),
        };
        let value = serde_json::to_value(&provider).expect("Provider Snapshot 应序列化");
        assert_eq!(value["reasoningEffort"], json!(expected));
        assert_eq!(
            serde_json::from_value::<ProviderSnapshot>(value)
                .expect("Provider Snapshot 应反序列化"),
            provider
        );
    }

    let disabled = ProviderSnapshot {
        provider_id: "provider".to_owned(),
        model: "model".to_owned(),
        context_window: None,
        protocol: ProviderProtocolSnapshot::OpenAiResponses,
        config_fingerprint: "sha256:test".to_owned(),
        reasoning_effort: None,
    };
    let value = serde_json::to_value(&disabled).expect("关闭推理的 Provider Snapshot 应序列化");
    assert_eq!(value["reasoningEffort"], Value::Null);
    assert_eq!(
        serde_json::from_value::<ProviderSnapshot>(value).expect("关闭推理应反序列化"),
        disabled
    );
}

/// 会话权威记录、嵌套消息和恢复状态都必须拒绝当前 Schema 之外的字段。
#[test]
fn persisted_session_structures_reject_unknown_fields() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "strict-shape", SnapshotPolicy::Disabled);
    create_session(&journal);
    let line = fs::read_to_string(journal.log_path())
        .expect("事件日志应读取")
        .lines()
        .next()
        .expect("应存在创建事件")
        .to_owned();
    let mut record: Value = serde_json::from_str(&line).expect("事件应是 JSON");
    record["unexpected"] = json!(true);
    assert!(serde_json::from_value::<SessionEventRecord>(record).is_err());

    let mut state = serde_json::to_value(journal.state().expect("状态应读取")).expect("状态应编码");
    state["unexpected"] = json!(true);
    assert!(serde_json::from_value::<SessionState>(state).is_err());

    let mut memory = serde_json::to_value(MemoryDocument::new(
        ScopeId::new("memory-scope").expect("作用域应有效"),
        vec![MemoryEntry {
            memory_id: "memory-1".to_owned(),
            content: "内容".to_owned(),
            updated_at_unix_ms: 1,
            tags: Vec::new(),
        }],
    ))
    .expect("Memory 文档应编码");
    memory["unexpected"] = json!(true);
    assert!(serde_json::from_value::<MemoryDocument>(memory).is_err());
}

/// 打开一个确认未损坏的 SessionJournal。
fn ready(root: &Path, session: &str, snapshot_policy: SnapshotPolicy) -> SessionJournal {
    match SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config(snapshot_policy),
    )
    .expect("Session 应可打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    }
}

/// 向空 Session 写入唯一创建事件。
fn create_session(journal: &SessionJournal) {
    journal
        .append(SessionEvent::SessionCreated {
            title: "测试会话".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("SessionCreated 应成功");
}

/// 创建一个不依赖 Turn 的唯一共享用户消息事件。
fn message_event(index: usize) -> SessionEvent {
    SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: format!("message-{index}"),
            turn_id: None,
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: format!("content-{index}"),
            }],
        },
    }
}

/// 验证权威事件分页使用独占 sequence 游标、稳定水位和固定单页上限。
#[test]
fn replay_page_is_bounded_and_cursor_stable() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-page", SnapshotPolicy::Disabled);
    create_session(&journal);
    for index in 1..=5 {
        journal
            .append(message_event(index))
            .expect("分页消息应追加");
    }

    let first = journal.read_page(None, 2).expect("首页应读取");
    assert_eq!(
        first
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(first.next_after, Some(2));
    assert_eq!(first.through_sequence, 6);
    assert!(first.has_more);

    let middle = journal
        .read_page(first.next_after, 3)
        .expect("中间页应读取");
    assert_eq!(
        middle
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert_eq!(middle.next_after, Some(5));
    assert_eq!(middle.through_sequence, 6);
    assert!(middle.has_more);

    let last = journal.read_page(middle.next_after, 3).expect("尾页应读取");
    assert_eq!(
        last.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![6]
    );
    assert_eq!(last.next_after, Some(6));
    assert_eq!(last.through_sequence, 6);
    assert!(!last.has_more);

    let empty = journal.read_page(Some(6), 1).expect("末尾游标应返回空页");
    assert!(empty.records.is_empty());
    assert_eq!(empty.next_after, None);
    assert_eq!(empty.through_sequence, 6);
    assert!(!empty.has_more);
    assert!(matches!(
        journal.read_page(Some(0), 1),
        Err(ResourceError::InvalidReplayCursor)
    ));
    assert!(matches!(
        journal.read_page(Some(99), 1),
        Err(ResourceError::InvalidReplayCursor)
    ));
    for limit in [0, MAX_REPLAY_PAGE_RECORDS + 1] {
        assert!(matches!(
            journal.read_page(None, limit),
            Err(ResourceError::InvalidReplayPageLimit { actual, limit: maximum })
                if actual == limit && maximum == MAX_REPLAY_PAGE_RECORDS
        ));
    }
}

/// 验证同一 Session 的另一实例追加后，旧实例分页读取会刷新权威水位和内容。
#[test]
fn replay_page_refreshes_external_append() {
    let root = TempDir::new().expect("临时目录应创建");
    let first = ready(root.path(), "replay-refresh", SnapshotPolicy::Disabled);
    let second = ready(root.path(), "replay-refresh", SnapshotPolicy::Disabled);
    create_session(&first);
    let initial = second.read_page(None, 8).expect("另一实例应读取创建事件");
    assert_eq!(initial.through_sequence, 1);
    assert_eq!(initial.next_after, Some(1));

    first.append(message_event(1)).expect("外部消息应追加");
    let refreshed = second
        .read_page(initial.next_after, 8)
        .expect("旧实例应刷新外部追加");
    assert_eq!(refreshed.through_sequence, 2);
    assert_eq!(refreshed.next_after, Some(2));
    assert_eq!(
        refreshed
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert!(!refreshed.has_more);
}

/// 构造同时保留模型可见说明与仅审计二进制引用的合法消息内容。
fn audit_artifact_content(artifact: ArtifactUse) -> Vec<MessagePart> {
    vec![
        MessagePart::Text {
            text: "Artifact 已保存用于审计".to_owned(),
        },
        MessagePart::Artifact {
            artifact,
            materialization: ArtifactMaterialization::Binary,
        },
    ]
}

/// 创建包含全部持久字段的测试 Goal。
fn goal_record(status: GoalStatus) -> GoalRecord {
    GoalRecord {
        id: "019d0000-0000-7000-8000-000000000001".to_owned(),
        title: "资源层目标".to_owned(),
        scope: "project".to_owned(),
        status,
        description: Some("验证无损持久化".to_owned()),
        progress_percent: Some(25),
        objective: "完成资源层".to_owned(),
        token_budget: Some(10_000),
        tokens_used: 2_500,
        time_used_seconds: 30,
        blocked_reason: if status == GoalStatus::Blocked {
            Some("需要外部输入".to_owned())
        } else {
            None
        },
        completion_evidence: if status == GoalStatus::Completed {
            Some("资源持久化与恢复测试通过".to_owned())
        } else {
            None
        },
        created_at_unix_ms: 1,
        updated_at_unix_ms: 2,
    }
}

/// 创建符合首次 CAS 生命周期约束的零用量 Active Goal。
fn new_goal_record() -> GoalRecord {
    let mut goal = goal_record(GoalStatus::Active);
    goal.tokens_used = 0;
    goal.time_used_seconds = 0;
    goal
}

/// 验证权威事件类型可使用同一 reducer 实时归约并从 Snapshot 加尾日志恢复。
#[test]
fn snapshot_replay_matches_live_state_for_authoritative_events() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "session-all",
        SnapshotPolicy::Every { events: 3 },
    );
    create_session(&journal);
    let turn_id = TurnId::new("turn-1").expect("Turn ID 应有效");
    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    let child_agent = AgentId::new("child").expect("Agent ID 应有效");
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        &turn_id,
        &root_agent,
        1,
        "call-request-1",
    )
    .expect("Request ID 应派生");
    let terminal_id = TerminalId::new("terminal-1").expect("Terminal ID 应有效");
    let worktree_path = root
        .path()
        .join("worktrees")
        .join("child")
        .to_string_lossy()
        .into_owned();
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: root_agent.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "实现功能".to_owned(),
        })
        .expect("Turn 应开始");
    journal.append(message_event(1)).expect("消息应追加");
    let compaction_digest = journal
        .state()
        .expect("状态应读取")
        .compaction_source_digest_sha256(&turn_id, &root_agent, 1, 0, 1)
        .expect("压缩 Digest 应计算");
    journal
        .append(SessionEvent::CompactionApplied {
            turn_id: turn_id.clone(),
            source_agent_id: root_agent.clone(),
            model_round: 1,
            compaction: CompactionRecord {
                trigger: ContextCompressionTrigger::Budget,
                estimated_tokens_before: 100,
                estimated_tokens_after: 50,
                replaced_start_index: 0,
                replaced_end_index_exclusive: 1,
                replaced_message_count: 1,
                retained_message_count: 1,
                source_digest_sha256: compaction_digest,
                summary: "已压缩创建、输入与消息上下文".to_owned(),
                expected_transcript_revision: 1,
                applied_transcript_revision: 2,
            },
        })
        .expect("上下文压缩应记录");
    journal
        .append(SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id: root_agent.clone(),
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-request-1".to_owned(),
                tool_name: "edit".to_owned(),
                arguments: json!({"path": "src/lib.rs"}),
                effect: ToolEffect::ChangesState,
            },
        })
        .expect("工具请求应记录");
    journal
        .append(SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        })
        .expect("工具执行起点应记录");
    journal
        .append(SessionEvent::TerminalStarted {
            terminal: TerminalRecord {
                terminal_id: terminal_id.clone(),
                request_id: request_id.clone(),
                command_display: "cargo test".to_owned(),
                working_directory: "D:/workspace".to_owned(),
                output_artifacts: Vec::new(),
                exit_code: None,
                cancelled: false,
                exited: false,
            },
        })
        .expect("终端应开始");
    journal
        .append(SessionEvent::TerminalExited {
            terminal_id,
            exit_code: Some(0),
            cancelled: false,
        })
        .expect("终端应退出");
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: PersistedToolResult {
                    tool_call_id: "call-request-1".to_owned(),
                    content: vec![ToolResultPart::Text {
                        text: "完成".to_owned(),
                    }],
                    is_error: false,
                },
            },
        })
        .expect("工具应完成");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::ModelRoundCompleted {
                    turn_id: turn_id.clone(),
                    source_agent_id: root_agent.clone(),
                    model_round: 1,
                    requested_model: "integration-test-model".to_owned(),
                    metadata: ResponseMetadata {
                        response_id: Some("integration-test-response".to_owned()),
                        model: Some("integration-test-model".to_owned()),
                    },
                    usage: TokenUsage::unknown(),
                    stop_reason: StopReason::Completed,
                },
                SessionEvent::TranscriptSegmentCommitted {
                    segment: TranscriptSegment {
                        turn_id: turn_id.clone(),
                        source_agent_id: root_agent.clone(),
                        model_round: 1,
                        segment_index: 0,
                        expected_transcript_revision: 2,
                        messages: vec![
                            SessionMessage {
                                message_id: "message-tool-call".to_owned(),
                                turn_id: Some(turn_id.clone()),
                                agent_id: Some(root_agent.clone()),
                                role: MessageRole::Assistant,
                                content: vec![MessagePart::ToolCall {
                                    tool_call_id: "call-request-1".to_owned(),
                                    tool_name: "edit".to_owned(),
                                    arguments: json!({"path": "src/lib.rs"}),
                                }],
                            },
                            SessionMessage {
                                message_id: "message-tool-result".to_owned(),
                                turn_id: Some(turn_id.clone()),
                                agent_id: Some(root_agent.clone()),
                                role: MessageRole::Tool,
                                content: vec![MessagePart::ToolResult {
                                    tool_call_id: "call-request-1".to_owned(),
                                    content: vec![ToolResultPart::Text {
                                        text: "完成".to_owned(),
                                    }],
                                    is_error: false,
                                }],
                            },
                        ],
                    },
                },
            ],
        })
        .expect("工具结果应物化到 Transcript");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: turn_id.clone(),
        })
        .expect("工具收敛后 Turn 应完成");
    journal
        .append(SessionEvent::TodoReplaced {
            items: vec![TodoItem {
                content: "验证".to_owned(),
                status: keencode_resources::TodoStatus::InProgress,
                active_form: "正在验证".to_owned(),
            }],
            operation_payload_sha256: "0".repeat(64),
            revision: 1,
        })
        .expect("Todo 应更新");
    journal
        .append(SessionEvent::PlanChanged {
            plan: PlanState {
                enabled: true,
                plan_artifact: None,
            },
        })
        .expect("Plan 应更新");
    journal
        .append(SessionEvent::ProviderSnapshotUpdated {
            provider: ProviderSnapshot {
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                context_window: Some(128_000),
                protocol: ProviderProtocolSnapshot::OpenAiResponses,
                config_fingerprint: "sha256:abc".to_owned(),
                reasoning_effort: None,
            },
        })
        .expect("Provider 应更新");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child".to_owned(),
                task: "检查".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn_id = TurnId::new("turn-child-1").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn_id.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: Some(turn_id.clone()),
                    prompt_summary: "执行子 Agent 检查".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn_id.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Agent Turn 与运行状态应原子开始");
    journal
        .append(SessionEvent::WorktreeAssigned {
            worktree: WorktreeRecord {
                agent_id: child_agent.clone(),
                path: worktree_path,
                branch: "feat/child".to_owned(),
                released: false,
            },
        })
        .expect("工作树应绑定");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnCompleted {
                    turn_id: child_turn_id.clone(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn_id.clone()),
                    status: SubAgentStatus::Completed,
                    result_summary: Some("检查通过".to_owned()),
                },
            ],
        })
        .expect("子 Agent 状态应更新");
    let mailbox_id = MailboxMessageId::new("mail-1").expect("Mailbox ID 应有效");
    journal
        .append(SessionEvent::MailboxMessageQueued {
            message: MailboxMessage {
                message_id: mailbox_id.clone(),
                from: child_agent.clone(),
                to: root_agent,
                related_turn_id: child_turn_id,
                body: "检查完成".to_owned(),
                artifact: None,
                state: MailboxState::Queued,
            },
        })
        .expect("邮箱消息应排队");
    journal
        .append(SessionEvent::MailboxMessageDelivered {
            message_id: mailbox_id,
        })
        .expect("邮箱消息应投递");
    journal
        .append(SessionEvent::WorktreeReleased {
            agent_id: child_agent,
        })
        .expect("工作树应释放");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("Session 应关闭");
    journal.write_snapshot().expect("Snapshot 应写入");
    let live = journal.state().expect("实时状态应读取");
    assert_eq!(live.applied_compactions().count(), 1);
    assert_eq!(live.status, SessionStatus::Closed);
    drop(journal);

    let reopened = ready(
        root.path(),
        "session-all",
        SnapshotPolicy::Every { events: 3 },
    );
    assert_eq!(reopened.state().expect("恢复状态应读取"), live);
    let log = fs::read_to_string(reopened.log_path()).expect("日志应读取");
    for (index, line) in log.lines().enumerate() {
        let value: Value = serde_json::from_str(line).expect("每行应是 JSON");
        for field in [
            "schema",
            "version",
            "session",
            "sequence",
            "timeUnixMs",
            "type",
            "payload",
        ] {
            assert!(value.get(field).is_some(), "缺少字段 {field}");
        }
        assert_eq!(value["sequence"], json!((index + 1) as u64));
    }
}

/// 验证中间空行是明确损坏，不能被当作不存在而改变真实行号。
#[test]
fn blank_jsonl_record_is_reported_as_invalid_json() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "blank-line", SnapshotPolicy::Disabled);
    create_session(&journal);
    journal.append(message_event(1)).expect("消息应追加");
    let log_path = journal.log_path().to_owned();
    drop(journal);
    let log = fs::read_to_string(&log_path).expect("日志应读取");
    let rewritten = log.replacen('\n', "\n\n", 1);
    fs::write(&log_path, rewritten).expect("测试应插入空行");

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("blank-line").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
    )
    .expect("损坏应作为报告返回");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("中间空行必须进入只读报告");
    };
    assert_eq!(report.valid_records, 1);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| { matches!(issue.kind, CorruptionKind::InvalidJson { line: 2 }) })
    );
}

/// 只有末尾换行的日志也属于损坏，不能被误当作尚未创建的空 Session。
#[test]
fn newline_only_jsonl_is_reported_as_invalid_json() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "newline-only", SnapshotPolicy::Disabled);
    let log_path = journal.log_path().to_owned();
    drop(journal);
    fs::write(&log_path, b"\n").expect("测试应写入空 JSONL 记录");

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("newline-only").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
    )
    .expect("损坏应作为报告返回");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("空 JSONL 记录必须进入只读报告");
    };
    assert_eq!(report.valid_records, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| { matches!(issue.kind, CorruptionKind::InvalidJson { line: 1 }) })
    );
}

/// 验证 v2 事件即使正文仍可反序列化，也必须按 v6 envelope 版本不匹配拒绝。
#[test]
fn v2_event_is_reported_as_envelope_mismatch_by_v6_reader() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "v2-event", SnapshotPolicy::Disabled);
    create_session(&journal);
    journal.append(message_event(1)).expect("消息应追加");
    let log_path = journal.log_path().to_owned();
    drop(journal);

    let mut lines = fs::read_to_string(&log_path)
        .expect("日志应读取")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("事件应是有效 JSON"))
        .collect::<Vec<_>>();
    lines[1]["version"] = json!(2);
    let rewritten = lines
        .iter()
        .map(|line| serde_json::to_string(line).expect("事件应编码"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&log_path, &rewritten).expect("旧版事件夹具应写入");

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("v2-event").expect("Session ID 应有效"),
        config(SnapshotPolicy::Disabled),
    )
    .expect("旧版事件应返回只读损坏报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("旧版事件不得作为当前事件读取");
    };
    assert_eq!(report.valid_records, 1);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue.kind, CorruptionKind::EnvelopeMismatch { line: 2 }))
    );
    assert_eq!(fs::read_to_string(log_path).expect("日志应保留"), rewritten);
}

/// 验证断电式尾记录不会被截断或静默修复，只返回最后有效状态。
#[test]
fn truncated_power_loss_tail_is_reported_read_only_without_repair() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "tail", SnapshotPolicy::Disabled);
    create_session(&journal);
    let log_path = journal.log_path().to_owned();
    drop(journal);
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .expect("日志应打开");
    file.write_all(b"{\"schema\":\"partial")
        .expect("尾记录应写入");
    file.sync_all().expect("尾记录应同步");
    let before = fs::read(&log_path).expect("损坏日志应读取");

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("tail").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
    )
    .expect("损坏应作为报告返回");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("截断尾记录必须进入只读报告");
    };
    assert_eq!(report.valid_records, 1);
    assert!(matches!(
        report.issues[0].kind,
        CorruptionKind::TruncatedTail { .. }
    ));
    assert_eq!(fs::read(&log_path).expect("日志应保留"), before);
}

/// 验证显式尾部恢复原样保留证据、只截断坏尾部，并允许 sequence 连续追加。
#[test]
fn truncated_tail_recovery_preserves_evidence_and_continues() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "tail-recovery", SnapshotPolicy::Disabled);
    create_session(&journal);
    let log_path = journal.log_path().to_owned();
    drop(journal);
    let damaged_tail = b"{\"schema\":\"partial";
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .expect("日志应打开");
    file.write_all(damaged_tail).expect("坏尾部应写入");
    file.sync_all().expect("坏尾部应同步");
    drop(file);

    let recovery = SessionJournal::recover_truncated_tail(
        root.path(),
        SessionId::new("tail-recovery").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
    )
    .expect("单一截断尾部应显式恢复");
    assert_eq!(recovery.preserved_bytes, damaged_tail.len() as u64);
    assert_eq!(
        fs::read(&recovery.evidence_path).expect("证据应读取"),
        damaged_tail
    );
    let receipt = recovery
        .journal
        .append(message_event(1))
        .expect("恢复后应可继续追加");
    assert_eq!(receipt.record.sequence, 2);
    drop(recovery.journal);
    let reopened = ready(root.path(), "tail-recovery", SnapshotPolicy::Disabled);
    assert_eq!(reopened.state().expect("状态应读取").last_sequence, 2);
}

/// 验证 sequence 缺口、重复和乱序分别产生稳定损坏分类。
#[test]
fn sequence_gap_duplicate_and_out_of_order_are_detected() {
    for (name, sequences, expected) in [
        ("gap", [1_u64, 3, 4], "gap"),
        ("duplicate", [1_u64, 1, 2], "duplicate"),
        ("out-of-order", [1_u64, 2, 1], "out_of_order"),
    ] {
        let root = TempDir::new().expect("临时目录应创建");
        let journal = ready(root.path(), name, SnapshotPolicy::Disabled);
        create_session(&journal);
        journal.append(message_event(1)).expect("消息应追加");
        journal.append(message_event(2)).expect("消息应追加");
        let log_path = journal.log_path().to_owned();
        let mut lines = fs::read_to_string(&log_path)
            .expect("日志应读取")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("行应有效"))
            .collect::<Vec<_>>();
        for (line, sequence) in lines.iter_mut().zip(sequences) {
            line["sequence"] = json!(sequence);
        }
        let rewritten = lines
            .iter()
            .map(|line| serde_json::to_string(line).expect("JSON 应写出"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        drop(journal);
        fs::write(&log_path, rewritten).expect("测试应改写日志");
        let opened = SessionJournal::open(
            root.path(),
            SessionId::new(name).expect("ID 应有效"),
            config(SnapshotPolicy::Disabled),
        )
        .expect("损坏应报告");
        let SessionOpen::Corrupt(report) = opened else {
            panic!("sequence 损坏必须只读");
        };
        assert!(report.issues.iter().any(|issue| matches!(
            (&issue.kind, expected),
            (CorruptionKind::SequenceGap { .. }, "gap")
                | (CorruptionKind::DuplicateSequence { .. }, "duplicate")
                | (CorruptionKind::OutOfOrderSequence { .. }, "out_of_order")
        )));
    }
}

/// 验证健康日志不会因单独损坏的 Snapshot 变为只读，且缓存会自动重建。
#[test]
fn corrupt_snapshot_is_ignored_and_rebuilt_from_healthy_journal() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "snapshot-bad",
        SnapshotPolicy::Every { events: 1 },
    );
    create_session(&journal);
    let path = journal.snapshot_path().to_owned();
    drop(journal);
    let mut snapshot: Value =
        serde_json::from_slice(&fs::read(&path).expect("Snapshot 应读取")).expect("JSON 应有效");
    snapshot["state"]["title"] = json!("篡改");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&snapshot).expect("Snapshot 应编码"),
    )
    .expect("Snapshot 应篡改");
    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("snapshot-bad").expect("ID 应有效"),
        config(SnapshotPolicy::Every { events: 1 }),
    )
    .expect("健康日志应可恢复");
    let SessionOpen::Ready(recovered) = opened else {
        panic!("单独损坏的 Snapshot 不得让健康日志只读");
    };
    assert_eq!(recovered.state().expect("状态应读取").title, "测试会话");
    drop(recovered);
    let rebuilt: Value = serde_json::from_slice(&fs::read(&path).expect("Snapshot 应重建"))
        .expect("重建 Snapshot 应是有效 JSON");
    assert_eq!(rebuilt["state"]["title"], json!("测试会话"));
    assert!(rebuilt["stateSha256"].as_str().is_some());
    assert!(rebuilt["throughLogSha256"].as_str().is_some());
}

/// 验证旧 v3 Snapshot 仅作为可丢弃缓存忽略，并由当前 v6 权威日志完整重建。
#[test]
fn v3_snapshot_is_ignored_and_rebuilt_from_v6_journal() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "v3-snapshot",
        SnapshotPolicy::Every { events: 1 },
    );
    create_session(&journal);
    journal.append(message_event(1)).expect("当前事件应追加");
    let expected_state = journal.state().expect("当前状态应读取");
    let snapshot_path = journal.snapshot_path().to_owned();
    drop(journal);

    let mut snapshot: Value =
        serde_json::from_slice(&fs::read(&snapshot_path).expect("Snapshot 应读取"))
            .expect("Snapshot 应是有效 JSON");
    snapshot["version"] = json!(3);
    fs::write(
        &snapshot_path,
        serde_json::to_vec_pretty(&snapshot).expect("旧版 Snapshot 应编码"),
    )
    .expect("旧版 Snapshot 夹具应写入");

    let reopened = ready(
        root.path(),
        "v3-snapshot",
        SnapshotPolicy::Every { events: 1 },
    );
    assert_eq!(reopened.state().expect("状态应从日志恢复"), expected_state);
    drop(reopened);
    let rebuilt: Value =
        serde_json::from_slice(&fs::read(snapshot_path).expect("重建 Snapshot 应读取"))
            .expect("重建 Snapshot 应有效");
    assert_eq!(rebuilt["version"], json!(4));
    assert_eq!(rebuilt["throughSequence"], json!(2));
    assert_eq!(rebuilt["state"]["lastSequence"], json!(2));
}

/// 验证即使同步伪造状态正文和自 Hash，Snapshot 也必须服从日志前缀的真实归约结果。
#[test]
fn self_consistent_but_log_inconsistent_snapshot_is_rebuilt() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(
        root.path(),
        "snapshot-semantic-forgery",
        SnapshotPolicy::Every { events: 1 },
    );
    create_session(&journal);
    let snapshot_path = journal.snapshot_path().to_owned();
    drop(journal);

    let mut snapshot: Value =
        serde_json::from_slice(&fs::read(&snapshot_path).expect("Snapshot 应读取"))
            .expect("Snapshot JSON 应有效");
    snapshot["state"]["title"] = json!("自洽但不属于日志的标题");
    let forged_state: keencode_resources::SessionState =
        serde_json::from_value(snapshot["state"].clone()).expect("伪造状态仍应结构有效");
    let state_bytes = serde_json::to_vec(&forged_state).expect("状态应编码");
    snapshot["stateSha256"] = json!(format!("{:x}", Sha256::digest(state_bytes)));
    let mut encoded = serde_json::to_vec_pretty(&snapshot).expect("Snapshot 应编码");
    encoded.push(b'\n');
    fs::write(&snapshot_path, encoded).expect("伪造 Snapshot 应写入");

    let reopened = ready(
        root.path(),
        "snapshot-semantic-forgery",
        SnapshotPolicy::Every { events: 1 },
    );
    assert_eq!(reopened.state().expect("状态应恢复").title, "测试会话");
    let rebuilt: Value =
        serde_json::from_slice(&fs::read(snapshot_path).expect("重建 Snapshot 应读取"))
            .expect("重建 Snapshot 应有效");
    assert_eq!(rebuilt["state"]["title"], json!("测试会话"));
}

/// 验证 Windows 可用的同实例并发 append 会得到无缺口唯一 sequence。
#[test]
fn windows_concurrent_append_is_serialized_without_gaps() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = Arc::new(ready(root.path(), "concurrent", SnapshotPolicy::Disabled));
    create_session(&journal);
    let mut threads = Vec::new();
    for index in 0..32 {
        let journal = Arc::clone(&journal);
        threads.push(thread::spawn(move || {
            journal
                .append(message_event(index))
                .expect("并发追加应成功")
        }));
    }
    let mut sequences = threads
        .into_iter()
        .map(|thread| thread.join().expect("线程应完成").record.sequence)
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (2_u64..=33).collect::<Vec<_>>());
    assert_eq!(
        journal
            .state()
            .expect("状态应读取")
            .raw_transcript_messages()
            .len(),
        32
    );
}

/// 验证两个独立 Journal 实例使用 OS 文件锁协调 sequence 和状态刷新。
#[test]
fn concurrent_append_across_instances_refreshes_external_changes() {
    let root = TempDir::new().expect("临时目录应创建");
    let first = ready(root.path(), "instances", SnapshotPolicy::Disabled);
    create_session(&first);
    let second = ready(root.path(), "instances", SnapshotPolicy::Disabled);
    let first = Arc::new(first);
    let second = Arc::new(second);
    let mut threads = Vec::new();
    for index in 0..24 {
        let journal = if index % 2 == 0 {
            Arc::clone(&first)
        } else {
            Arc::clone(&second)
        };
        threads.push(thread::spawn(move || {
            journal
                .append(message_event(index))
                .expect("跨实例追加应成功")
        }));
    }
    for thread in threads {
        thread.join().expect("线程应完成");
    }
    drop(first);
    drop(second);
    let reopened = ready(root.path(), "instances", SnapshotPolicy::Disabled);
    let state = reopened.state().expect("状态应读取");
    assert_eq!(state.last_sequence, 25);
    assert_eq!(state.raw_transcript_messages().len(), 24);
}

/// 验证一个 Journal 的只读状态查询也会立即观察另一个实例已经提交的事件。
#[test]
fn state_refreshes_cross_instance_commits_without_local_append() {
    let root = TempDir::new().expect("临时目录应创建");
    let first = ready(root.path(), "state-refresh", SnapshotPolicy::Disabled);
    create_session(&first);
    let second = ready(root.path(), "state-refresh", SnapshotPolicy::Disabled);
    second.append(message_event(1)).expect("第二实例应追加事件");

    let refreshed = first.state().expect("第一实例状态应刷新");
    assert_eq!(refreshed.last_sequence, 2);
    assert_eq!(refreshed.raw_transcript_messages().len(), 1);
}

/// 验证根与子 Agent 并发时，一个 Turn 完成不会把仍有运行 Turn 的 Session 置为 Idle。
#[test]
fn session_status_is_derived_from_all_running_turns() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "multi-turn", SnapshotPolicy::Disabled);
    create_session(&journal);
    let first = TurnId::new("turn-first").expect("Turn ID 应有效");
    let second = TurnId::new("turn-second").expect("Turn ID 应有效");
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let child_agent = AgentId::new("child-concurrent").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: first.clone(),
            source_agent_id: root_agent.clone(),
            root_turn_id: first.clone(),
            parent_turn_id: None,
            prompt_summary: "根并行任务".to_owned(),
        })
        .expect("根 Turn 应开始");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent,
                agent_path: "/root/child_concurrent".to_owned(),
                task: "子并行任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: second.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: first.clone(),
                    parent_turn_id: Some(first.clone()),
                    prompt_summary: "子并行任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(second.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Turn 与运行状态应原子开始");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: first.clone(),
        })
        .expect("第一个 Turn 应完成");
    assert_eq!(
        journal.state().expect("状态应读取").status,
        SessionStatus::Running
    );
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStopped {
                    turn_id: second.clone(),
                    reason: TurnStopReason::Cancelled,
                    message: "用户取消".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent,
                    turn_id: Some(second),
                    status: SubAgentStatus::Interrupted,
                    result_summary: None,
                },
            ],
        })
        .expect("第二个 Turn 应停止");
    assert_eq!(
        journal.state().expect("状态应读取").status,
        SessionStatus::Idle
    );
}

/// 验证路径穿越标识在接触文件系统前被拒绝。
#[test]
fn traversal_ids_are_rejected() {
    assert!(SessionId::new("../outside").is_err());
    assert!(ScopeId::new("..\\outside").is_err());
    assert!(keencode_resources::ArtifactId::new("a/b").is_err());
    assert!(SessionId::new("CON.json").is_err());
    assert!(SessionId::new("session.").is_err());
    assert!(keencode_resources::ArtifactId::new("a".repeat(64)).is_ok());
    assert!(keencode_resources::ArtifactId::new("A".repeat(64)).is_err());
}

/// 验证所有可映射到文件系统的标识只接受小写 ASCII，隔离 Windows 大小写别名。
#[test]
fn filesystem_ids_reject_windows_case_aliases() {
    assert!(SessionId::new("session-a").is_ok());
    assert!(TurnId::new("turn-a").is_ok());
    assert!(RequestId::new("a".repeat(64)).is_ok());
    assert!(TerminalId::new("terminal-a").is_ok());
    assert!(AgentId::new("agent-a").is_ok());
    assert!(MailboxMessageId::new("mail-a").is_ok());
    assert!(ScopeId::new("project-a").is_ok());

    assert!(SessionId::new("Session-a").is_err());
    assert!(TurnId::new("Turn-a").is_err());
    assert!(RequestId::new("A".repeat(64)).is_err());
    assert!(TerminalId::new("Terminal-a").is_err());
    assert!(AgentId::new("Agent-a").is_err());
    assert!(MailboxMessageId::new("Mail-a").is_err());
    assert!(ScopeId::new("Project-a").is_err());
}

/// 项目作用域只接受现有绝对目录，并对规范路径别名生成稳定标识。
#[test]
fn project_scope_requires_an_existing_absolute_directory() {
    let project = TempDir::new().expect("项目临时目录应创建");
    let canonical = fs::canonicalize(project.path()).expect("项目路径应规范化");
    assert_eq!(
        project_scope_id(project.path()).expect("原项目路径应派生"),
        project_scope_id(&canonical).expect("规范项目路径应派生")
    );
    assert!(project_scope_id(Path::new("relative-project")).is_err());
    assert!(project_scope_id(project.path().join("missing-directory")).is_err());
    let file = project.path().join("not-a-directory.txt");
    fs::write(&file, b"file").expect("普通文件应创建");
    assert!(matches!(
        project_scope_id(&file),
        Err(ResourceError::UnsafePath(_))
    ));
}

/// Windows 项目路径的 ASCII 大小写和两类目录分隔符必须映射到同一作用域。
#[cfg(windows)]
#[test]
fn project_scope_normalizes_windows_case_and_separators() {
    let project = TempDir::new().expect("项目临时目录应创建");
    let canonical = fs::canonicalize(project.path()).expect("项目路径应规范化");
    let original = canonical.to_string_lossy().into_owned();
    let alternate_case: String = original
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() {
                character.to_ascii_uppercase()
            } else if character.is_ascii_uppercase() {
                character.to_ascii_lowercase()
            } else {
                character
            }
        })
        .collect();
    let forward_slashes = original.replace('\\', "/");
    let expected = project_scope_id(&canonical).expect("规范路径应派生");
    assert_eq!(
        project_scope_id(Path::new(&alternate_case)).expect("大小写别名应派生"),
        expected
    );
    assert_eq!(
        project_scope_id(Path::new(&forward_slashes)).expect("分隔符别名应派生"),
        expected
    );
}

/// 验证 Provider 原始调用标识只能通过完整作用域派生，复杂长值仍稳定且跨 Round 隔离。
#[test]
fn request_id_derivation_is_domain_separated_and_stable() {
    let session_id = SessionId::new("request-derivation").expect("Session ID 应有效");
    let turn_id = TurnId::new("turn-main").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let provider_id = format!("厂商/调用:id?{}", "复杂值".repeat(2_048));
    let first =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 1, &provider_id)
            .expect("复杂 Provider ID 应派生");
    let repeated =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 1, &provider_id)
            .expect("相同输入应稳定派生");
    let next_round =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 2, &provider_id)
            .expect("下一 Round 应派生");
    assert_eq!(first, repeated);
    assert_ne!(first, next_round);
    assert_eq!(first.as_str().len(), 64);
    assert!(
        first
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );
    assert!(RequestId::new(provider_id).is_err());
}

/// 验证归约失败的候选事件不会占用 sequence，也不会写入 JSONL。
#[test]
fn rejected_event_is_not_committed() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "reject-before-write", SnapshotPolicy::Disabled);
    create_session(&journal);
    let before = fs::read(journal.log_path()).expect("日志应读取");
    let result = journal.append(SessionEvent::TodoReplaced {
        items: vec![TodoItem {
            content: String::new(),
            status: keencode_resources::TodoStatus::Pending,
            active_form: "正在验证".to_owned(),
        }],
        operation_payload_sha256: "0".repeat(64),
        revision: 1,
    });
    assert!(matches!(
        result,
        Err(keencode_resources::ResourceError::Reduction(_))
    ));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
    let receipt = journal.append(message_event(1)).expect("后续事件应成功");
    assert_eq!(receipt.record.sequence, 2);
}

/// 验证 Session 目录或事件文件是符号链接时拒绝跟随。
#[test]
fn symlinked_session_boundaries_are_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    fs::create_dir(root.path().join("sessions")).expect("sessions 应创建");
    let outside = TempDir::new().expect("外部目录应创建");
    let linked = root.path().join("sessions").join("linked");
    if !try_symlink_directory(outside.path(), &linked) {
        return;
    }
    let result = SessionJournal::open(
        root.path(),
        SessionId::new("linked").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
    );
    assert!(matches!(
        result,
        Err(keencode_resources::ResourceError::SymlinkRejected(_))
    ));
}

/// 验证 FlushAndSync 首次创建日志后可重开，并公开准确的平台目录同步与隔离边界。
#[test]
fn flush_and_sync_first_creation_reports_platform_guarantees() {
    let capabilities = filesystem_capabilities();
    assert!(!capabilities.strong_path_isolation);
    assert_eq!(capabilities.parent_directory_sync, cfg!(unix));

    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "first-sync", SnapshotPolicy::Disabled);
    assert!(!journal.log_path().exists());
    create_session(&journal);
    let log_path = journal.log_path().to_owned();
    assert!(log_path.is_file());
    drop(journal);
    let reopened = ready(root.path(), "first-sync", SnapshotPolicy::Disabled);
    assert_eq!(reopened.state().expect("状态应恢复").last_sequence, 1);
}

/// 验证缺失的持久化根目录会逐层创建，并继续提供平台声明的目录同步能力。
#[test]
fn nested_storage_root_is_created_for_durable_first_write() {
    let root = TempDir::new().expect("临时目录应创建");
    let nested = root.path().join("new").join("deep").join("storage");
    let journal = ready(&nested, "durable-directory-chain", SnapshotPolicy::Disabled);
    create_session(&journal);
    assert!(nested.join("sessions").is_dir());
    assert!(
        nested
            .join("sessions")
            .join("durable-directory-chain")
            .join("events.jsonl")
            .is_file()
    );
    assert_eq!(filesystem_capabilities().parent_directory_sync, cfg!(unix));
}

/// 验证 Artifact 隔离、去重、限制、Hash 和 UTF-8 预览。
#[test]
fn artifact_store_is_session_isolated_atomic_and_bounded() {
    let root = TempDir::new().expect("临时目录应创建");
    let limits = ArtifactLimits {
        max_artifact_bytes: 32,
        max_artifacts_per_session: 1,
        max_preview_bytes: 2,
    };
    let first = ArtifactStore::open(
        root.path(),
        SessionId::new("artifact-a").expect("ID 应有效"),
        limits,
    )
    .expect("ArtifactStore 应打开");
    let second = ArtifactStore::open(
        root.path(),
        SessionId::new("artifact-b").expect("ID 应有效"),
        limits,
    )
    .expect("ArtifactStore 应打开");
    let reference = first
        .put("A中B".as_bytes(), Some("text/plain".to_owned()))
        .expect("Artifact 应保存");
    assert_eq!(reference.preview.text, "A");
    assert!(reference.preview.truncated);
    assert_eq!(
        first.read(&reference).expect("Artifact 应读取"),
        "A中B".as_bytes()
    );
    assert!(second.read(&reference).is_err());
    let duplicate = first
        .put("A中B".as_bytes(), Some("text/plain".to_owned()))
        .expect("相同内容应去重");
    assert_eq!(duplicate.artifact_id, reference.artifact_id);
    assert!(matches!(
        first.put(b"different", None),
        Err(keencode_resources::ResourceError::ArtifactCountLimit { .. })
    ));
    assert!(matches!(
        first.put(&[0_u8; 33], None),
        Err(keencode_resources::ResourceError::ArtifactTooLarge { .. })
    ));
}

/// 验证 Session append 必须经可注入校验器核验 Artifact 的存在、作用域、大小和 Hash。
#[test]
fn journal_rejects_unresolved_or_mismatched_artifact_references() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("artifact-journal").expect("ID 应有效");
    let store = Arc::new(
        ArtifactStore::open(root.path(), session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应打开"),
    );
    let reference = store.put(b"verified", None).expect("Artifact 应保存");

    let journal = ready(root.path(), session_id.as_str(), SnapshotPolicy::Disabled);
    create_session(&journal);
    let unresolved = SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: "artifact-message-unresolved".to_owned(),
            turn_id: None,
            agent_id: None,
            role: MessageRole::System,
            content: audit_artifact_content(reference.as_event_use()),
        },
    };
    assert!(matches!(
        journal.append(unresolved),
        Err(ResourceError::ArtifactValidatorRequired)
    ));
    drop(journal);

    let journal = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id.clone(),
        config(SnapshotPolicy::Disabled),
        store.clone(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    let mut wrong_size = reference.as_event_use();
    wrong_size.size_bytes += 1;
    assert!(matches!(
        journal.append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "artifact-message-size".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: audit_artifact_content(wrong_size),
            },
        }),
        Err(ResourceError::ArtifactSizeMismatch { .. })
    ));
    let missing_hash = "a".repeat(64);
    let missing = ArtifactUse {
        artifact_id: ArtifactId::new(missing_hash.clone()).expect("Artifact ID 应有效"),
        sha256: missing_hash,
        size_bytes: 1,
        media_type: None,
    };
    assert!(matches!(
        journal.append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "artifact-message-missing".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: audit_artifact_content(missing),
            },
        }),
        Err(ResourceError::ArtifactNotFound)
    ));
    let tampered = store.put(b"tamper-me", None).expect("Artifact 应保存");
    let tampered_path = root
        .path()
        .join("sessions")
        .join(session_id.as_str())
        .join("artifacts")
        .join(format!("{}.artifact", tampered.artifact_id.as_str()));
    fs::write(&tampered_path, b"tamper-no").expect("测试应篡改同长度 Artifact");
    assert!(matches!(
        journal.append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "artifact-message-hash".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: audit_artifact_content(tampered.as_event_use()),
            },
        }),
        Err(ResourceError::ArtifactHashMismatch)
    ));
    let receipt = journal
        .append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "artifact-message-valid".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: audit_artifact_content(reference.as_event_use()),
            },
        })
        .expect("已核验 Artifact 应追加");
    assert_eq!(receipt.record.sequence, 2);

    let foreign = Arc::new(
        ArtifactStore::open(
            root.path(),
            SessionId::new("artifact-foreign").expect("ID 应有效"),
            ArtifactLimits::default(),
        )
        .expect("外部 ArtifactStore 应打开"),
    );
    let foreign_journal = match SessionJournal::open_with_artifact_validator(
        root.path(),
        SessionId::new("artifact-foreign-journal").expect("ID 应有效"),
        config(SnapshotPolicy::Disabled),
        foreign,
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    create_session(&foreign_journal);
    assert!(matches!(
        foreign_journal.append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "artifact-message-scope".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: audit_artifact_content(reference.as_event_use()),
            },
        }),
        Err(ResourceError::ArtifactScopeMismatch)
    ));
}

/// 验证内容寻址文件被外部篡改后，读取和去重写入都不会信任旧文件。
#[test]
fn tampered_artifact_is_rejected_by_hash() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = SessionId::new("artifact-tamper").expect("ID 应有效");
    let store = ArtifactStore::open(root.path(), session.clone(), ArtifactLimits::default())
        .expect("ArtifactStore 应打开");
    let reference = store.put(b"original", None).expect("Artifact 应保存");
    let artifact_path = root
        .path()
        .join("sessions")
        .join(session.as_str())
        .join("artifacts")
        .join(format!("{}.artifact", reference.artifact_id.as_str()));
    fs::write(&artifact_path, b"tampered").expect("测试应篡改 Artifact");

    assert!(matches!(
        store.read(&reference),
        Err(keencode_resources::ResourceError::ArtifactHashMismatch)
    ));
    assert!(matches!(
        store.put(b"original", None),
        Err(keencode_resources::ResourceError::ArtifactHashMismatch)
    ));
}

/// 验证磁盘实体超过限制时先按有界读取拒绝，而不是信任引用中的较小声明。
#[test]
fn oversized_artifact_entity_is_rejected_before_hash_or_size_comparison() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = SessionId::new("artifact-bounded-read").expect("Session ID 应有效");
    let limits = ArtifactLimits {
        max_artifact_bytes: 32,
        max_artifacts_per_session: 8,
        max_preview_bytes: 8,
    };
    let store =
        ArtifactStore::open(root.path(), session.clone(), limits).expect("ArtifactStore 应打开");
    let digest = "a".repeat(64);
    let path = root
        .path()
        .join("sessions")
        .join(session.as_str())
        .join("artifacts")
        .join(format!("{digest}.artifact"));
    fs::write(path, [b'x'; 33]).expect("超限实体夹具应写入");
    let reference = ArtifactUse {
        artifact_id: ArtifactId::new(digest.clone()).expect("Artifact ID 应有效"),
        sha256: digest,
        size_bytes: 1,
        media_type: None,
    };

    assert!(matches!(
        store.validate_use(&reference),
        Err(ResourceError::ArtifactTooLarge {
            actual: 33,
            limit: 32
        })
    ));
}

/// 验证 Memory 与 Goal 使用原子完整文档边界且不实现模型抽取。
#[test]
fn memory_and_goal_documents_round_trip_atomically() {
    let root = TempDir::new().expect("临时目录应创建");
    let scope = ScopeId::new("project-1").expect("Scope 应有效");
    let memories = MemoryFileStore::open(root.path()).expect("MemoryStore 应打开");
    let memory = MemoryDocument::new(
        scope.clone(),
        vec![MemoryEntry {
            memory_id: "memory-1".to_owned(),
            content: "使用 cargo test".to_owned(),
            updated_at_unix_ms: 1,
            tags: vec!["rust".to_owned()],
        }],
    );
    let memory = memories
        .compare_and_swap(0, memory)
        .expect("Memory 应以 revision 1 写入");
    assert_eq!(memory.revision, 1);
    assert_eq!(memories.read(&scope).expect("Memory 应读取"), Some(memory));
    let goals = GoalFileStore::open(root.path()).expect("GoalStore 应打开");
    let snapshot = GoalSnapshot {
        revision: 0,
        goal: Some(new_goal_record()),
        retired_goal_ids: Vec::new(),
    };
    let goal = GoalDocument::from_snapshot(scope.clone(), snapshot);
    let goal = goals
        .compare_and_swap("goal-round-trip", &"goal_create_v1", 0, goal)
        .expect("Goal 应以 revision 1 写入")
        .into_document();
    assert_eq!(goal.snapshot().revision, 1);
    assert_eq!(goal.snapshot().goal, Some(new_goal_record()));
    assert_eq!(goals.read(&scope).expect("Goal 应读取"), Some(goal));
    for status in [
        GoalStatus::Active,
        GoalStatus::Completed,
        GoalStatus::Blocked,
    ] {
        let snapshot = GoalSnapshot {
            revision: 7,
            goal: Some(goal_record(status)),
            retired_goal_ids: Vec::new(),
        };
        let document = GoalDocument::from_snapshot(scope.clone(), snapshot.clone());
        let encoded = serde_json::to_vec(&document).expect("Goal 文档应编码");
        let decoded: GoalDocument = serde_json::from_slice(&encoded).expect("Goal 文档应解码");
        assert_eq!(decoded.snapshot(), snapshot);
    }
}

/// Goal 收据必须在后续状态变化后继续去重原始请求，并拒绝同标识载荷冲突。
#[test]
fn goal_receipts_deduplicate_retries_after_later_changes() {
    let root = TempDir::new().expect("临时目录应创建");
    let scope = ScopeId::new("goal-receipt").expect("Goal 作用域应有效");
    let store = GoalFileStore::open(root.path()).expect("Goal Store 应打开");
    let first_operation = ("goal_upsert_v2", "第一版目标");
    let first = store
        .compare_and_swap(
            "goal-receipt-first",
            &first_operation,
            0,
            GoalDocument::from_snapshot(
                scope.clone(),
                GoalSnapshot {
                    revision: 0,
                    goal: Some(new_goal_record()),
                    retired_goal_ids: Vec::new(),
                },
            ),
        )
        .expect("首次 Goal 写入应成功");
    assert!(matches!(first, DocumentOperationOutcome::Applied(_)));
    let first = first.into_document();

    let mut updated_goal = first.goal.clone().expect("首次 Goal 应存在");
    updated_goal.progress_percent = Some(50);
    updated_goal.updated_at_unix_ms = 3;
    let second = store
        .compare_and_swap(
            "goal-receipt-later",
            &("goal_upsert_v2", "第二版目标"),
            first.revision,
            GoalDocument::from_snapshot(
                scope.clone(),
                GoalSnapshot {
                    revision: first.revision,
                    goal: Some(updated_goal),
                    retired_goal_ids: Vec::new(),
                },
            ),
        )
        .expect("后续 Goal 更新应成功")
        .into_document();

    let replay = store
        .compare_and_swap(
            "goal-receipt-first",
            &first_operation,
            first.revision,
            GoalDocument::from_snapshot(
                scope.clone(),
                GoalSnapshot {
                    revision: first.revision,
                    goal: Some(new_goal_record()),
                    retired_goal_ids: Vec::new(),
                },
            ),
        )
        .expect("后续状态变化后原始 Goal 请求仍应去重");
    assert!(replay.deduplicated());
    assert_eq!(replay.document(), &second);

    let mut conflicting_goal = second.goal.clone().expect("后续 Goal 应存在");
    conflicting_goal.title = "冲突目标".to_owned();
    assert!(matches!(
        store.compare_and_swap(
            "goal-receipt-first",
            &("goal_upsert_v2", "另一份载荷"),
            second.revision,
            GoalDocument::from_snapshot(
                scope,
                GoalSnapshot {
                    revision: second.revision,
                    goal: Some(conflicting_goal),
                    retired_goal_ids: Vec::new(),
                },
            ),
        ),
        Err(ResourceError::OperationConflict)
    ));
}

/// 验证两个独立 MemoryStore 的并发 CAS 只有一个能提交相同 revision。
#[test]
fn concurrent_document_updates_return_stable_revision_conflict() {
    let root = TempDir::new().expect("临时目录应创建");
    let scope = ScopeId::new("project-cas").expect("Scope 应有效");
    let first = MemoryFileStore::open(root.path()).expect("MemoryStore 应打开");
    let initial = first
        .compare_and_swap(0, MemoryDocument::new(scope.clone(), Vec::new()))
        .expect("初始文档应写入");
    assert_eq!(initial.revision, 1);
    let first = Arc::new(first);
    let second = Arc::new(MemoryFileStore::open(root.path()).expect("MemoryStore 应打开"));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for (index, store) in [Arc::clone(&first), Arc::clone(&second)]
        .into_iter()
        .enumerate()
    {
        let scope = scope.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let mut document = store
                .read(&scope)
                .expect("Memory 应读取")
                .expect("Memory 应存在");
            document.entries.push(MemoryEntry {
                memory_id: format!("memory-{index}"),
                content: format!("并发更新 {index}"),
                updated_at_unix_ms: index as u64 + 2,
                tags: Vec::new(),
            });
            barrier.wait();
            store.compare_and_swap(document.revision, document)
        }));
    }
    let results = threads
        .into_iter()
        .map(|thread| thread.join().expect("线程应完成"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let conflicts = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert!(matches!(
        conflicts.as_slice(),
        [ResourceError::RevisionConflict {
            expected: 1,
            actual: 2
        }]
    ));
    assert_eq!(
        first
            .read(&scope)
            .expect("Memory 应读取")
            .expect("Memory 应存在")
            .revision,
        2
    );
}

/// 验证磁盘文档声明的 Scope 必须与读取目标文件名一致。
#[test]
fn document_scope_mismatch_is_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let requested = ScopeId::new("project-a").expect("Scope 应有效");
    let foreign = ScopeId::new("project-b").expect("Scope 应有效");
    let memories = MemoryFileStore::open(root.path()).expect("MemoryStore 应打开");
    let foreign_memory = MemoryDocument::new(
        foreign.clone(),
        vec![MemoryEntry {
            memory_id: "memory-1".to_owned(),
            content: "外部作用域".to_owned(),
            updated_at_unix_ms: 1,
            tags: Vec::new(),
        }],
    );
    let mut foreign_memory = foreign_memory;
    foreign_memory.revision = 1;
    fs::write(
        root.path().join("memories").join("project-a.json"),
        serde_json::to_vec(&foreign_memory).expect("Memory 应编码"),
    )
    .expect("测试 Memory 应写入");
    assert!(memories.read(&requested).is_err());

    let goals = GoalFileStore::open(root.path()).expect("GoalStore 应打开");
    let mut foreign_goal = GoalDocument::new(foreign, None);
    foreign_goal.revision = 1;
    fs::write(
        root.path().join("goals").join("project-a.json"),
        serde_json::to_vec(&foreign_goal).expect("Goal 应编码"),
    )
    .expect("测试 Goal 应写入");
    assert!(goals.read(&requested).is_err());
}

/// 验证 Memory 与 Goal 的目标文件不能被符号链接重定向到存储根目录外。
#[test]
fn symlinked_document_targets_are_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let outside = TempDir::new().expect("外部目录应创建");
    let scope = ScopeId::new("linked-project").expect("Scope 应有效");
    let outside_file = outside.path().join("outside.json");
    fs::write(&outside_file, b"{}\n").expect("外部文件应创建");

    let memories = MemoryFileStore::open(root.path()).expect("MemoryStore 应打开");
    let memory_target = root.path().join("memories").join("linked-project.json");
    if try_symlink_file(&outside_file, &memory_target) {
        let memory = MemoryDocument::new(
            scope.clone(),
            vec![MemoryEntry {
                memory_id: "memory-1".to_owned(),
                content: "不能越界".to_owned(),
                updated_at_unix_ms: 1,
                tags: Vec::new(),
            }],
        );
        assert!(matches!(
            memories.compare_and_swap(0, memory),
            Err(keencode_resources::ResourceError::SymlinkRejected(_))
        ));
        fs::remove_file(&memory_target).expect("Memory 测试链接应移除");
    }

    let goals = GoalFileStore::open(root.path()).expect("GoalStore 应打开");
    let goal_target = root.path().join("goals").join("linked-project.json");
    if try_symlink_file(&outside_file, &goal_target) {
        let goal = GoalDocument::new(scope, None);
        assert!(matches!(
            goals.compare_and_swap("goal-symlink", &"goal_clear_v1", 0, goal),
            Err(keencode_resources::ResourceError::SymlinkRejected(_))
        ));
    }
    assert_eq!(
        fs::read(&outside_file).expect("外部文件应保持不变"),
        b"{}\n"
    );
}

/// 在支持的系统上创建目录符号链接。
fn try_symlink_directory(source: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(source, target).is_ok()
    }
}

/// 在支持的系统上创建文件符号链接。
fn try_symlink_file(source: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(source, target).is_ok()
    }
}
