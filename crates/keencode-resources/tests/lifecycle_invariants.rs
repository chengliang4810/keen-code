mod support;

use std::path::Path;
use std::sync::Arc;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, ArtifactLimits, ArtifactStore, CompactionRecord, ContextCompressionTrigger,
    Durability, JournalConfig, MailboxMessage, MailboxMessageId, MailboxState, MessagePart,
    MessageRole, PersistedToolResult, PlanState, RequestId, ResourceError, SessionEvent, SessionId,
    SessionJournal, SessionMessage, SessionOpen, SessionStatus, SnapshotPolicy, SubAgentState,
    SubAgentStatus, TerminalId, TerminalRecord, ToolCompletionStatus, ToolEffect, ToolOutcome,
    ToolRequest, ToolResultPart, TranscriptRecord, TranscriptSegment, TurnId, TurnStatus,
    TurnStopReason, WorktreeRecord,
};
use serde_json::json;
use tempfile::TempDir;

use support::TestJournalAppend;

/// 返回不自动写 Snapshot 的生命周期测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::Buffered,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开并初始化一个生命周期测试 Session。
fn created_journal(root: &Path, session: &str) -> SessionJournal {
    let opened = SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应打开");
    let SessionOpen::Ready(journal) = opened else {
        panic!("全新 Session 不应损坏");
    };
    journal
        .append(SessionEvent::SessionCreated {
            title: "生命周期测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("Session 应创建");
    journal
}

/// 为测试 Session 创建一个 Running Turn。
fn start_turn(journal: &SessionJournal, turn: &str) -> TurnId {
    let turn_id = TurnId::new(turn).expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "执行测试任务".to_owned(),
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
                requested_model: "lifecycle-test-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some("lifecycle-test-response".to_owned()),
                    model: Some("lifecycle-test-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason: StopReason::Completed,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 构造一个参数有效的工具请求。
fn tool_request(
    journal: &SessionJournal,
    request: &str,
    turn_id: &TurnId,
    changes_files: bool,
) -> ToolRequest {
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        turn_id,
        &agent_id,
        1,
        request,
    )
    .expect("Request ID 应派生");
    ToolRequest {
        request_id,
        turn_id: turn_id.clone(),
        agent_id,
        model_round: 1,
        request_index: u32::try_from(journal.state().expect("状态应读取").tools.len())
            .expect("测试工具数量应在 u32 范围内"),
        model_tool_call_id: request.to_owned(),
        tool_name: "test_tool".to_owned(),
        arguments: json!({"value": 1}),
        effect: if changes_files {
            ToolEffect::ChangesState
        } else {
            ToolEffect::ReadOnly
        },
    }
}

/// 构造与测试请求 Provider 调用标识严格配对的完整工具结果。
fn tool_outcome(
    journal: &SessionJournal,
    request_id: &RequestId,
    status: ToolCompletionStatus,
) -> ToolOutcome {
    let tool_call_id = journal
        .state()
        .expect("状态应读取")
        .tools
        .get(request_id)
        .expect("工具请求应存在")
        .request
        .model_tool_call_id
        .clone();
    ToolOutcome {
        status,
        result: PersistedToolResult {
            tool_call_id,
            content: vec![ToolResultPart::Text {
                text: "测试工具结果".to_owned(),
            }],
            is_error: status != ToolCompletionStatus::Succeeded,
        },
    }
}

/// 持久化工具越过副作用执行边界前的执行起点。
fn start_tool_execution(journal: &SessionJournal, request_id: &RequestId) {
    journal
        .append(SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        })
        .expect("工具执行起点应记录");
}

/// 把一个已终态工具生命周期恢复成可继续发送给模型的原子 Transcript 段。
fn materialize_tool(journal: &SessionJournal, request_id: &RequestId) {
    let state = journal.state().expect("状态应读取");
    let lifecycle = state
        .tools
        .get(request_id)
        .expect("工具生命周期应存在")
        .clone();
    let outcome = lifecycle.outcome.expect("工具结果应已终态");
    let segment_index = state
        .transcript_segments()
        .filter(|segment| {
            segment.turn_id == lifecycle.request.turn_id
                && segment.source_agent_id == lifecycle.request.agent_id
                && segment.model_round == lifecycle.request.model_round
        })
        .map(|segment| segment.segment_index)
        .max()
        .map_or(0, |index| index + 1);
    let segment = TranscriptSegment {
        turn_id: lifecycle.request.turn_id.clone(),
        source_agent_id: lifecycle.request.agent_id.clone(),
        model_round: lifecycle.request.model_round,
        segment_index,
        expected_transcript_revision: state.transcript_revision,
        messages: vec![
            SessionMessage {
                message_id: format!("tool-call-{request_id}"),
                turn_id: Some(lifecycle.request.turn_id.clone()),
                agent_id: Some(lifecycle.request.agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::ToolCall {
                    tool_call_id: lifecycle.request.model_tool_call_id.clone(),
                    tool_name: lifecycle.request.tool_name,
                    arguments: lifecycle.request.arguments,
                }],
            },
            SessionMessage {
                message_id: format!("tool-result-{request_id}"),
                turn_id: Some(lifecycle.request.turn_id.clone()),
                agent_id: Some(lifecycle.request.agent_id.clone()),
                role: MessageRole::Tool,
                content: vec![MessagePart::ToolResult {
                    tool_call_id: outcome.result.tool_call_id,
                    content: outcome.result.content,
                    is_error: outcome.result.is_error,
                }],
            },
        ],
    };
    let event = if segment_index == 0 {
        model_round_batch(
            &lifecycle.request.turn_id,
            &lifecycle.request.agent_id,
            segment,
        )
    } else {
        SessionEvent::TranscriptSegmentCommitted { segment }
    };
    journal
        .append(event)
        .expect("工具生命周期应物化到 Transcript");
}

/// 构造尚未退出且没有输出的终端初始记录。
fn terminal_record(terminal: &str, request_id: &RequestId) -> TerminalRecord {
    TerminalRecord {
        terminal_id: TerminalId::new(terminal).expect("Terminal ID 应有效"),
        request_id: request_id.clone(),
        command_display: "cargo test".to_owned(),
        working_directory: "D:/workspace".to_owned(),
        output_artifacts: Vec::new(),
        exit_code: None,
        cancelled: false,
        exited: false,
    }
}

/// 断言事件在 reducer 阶段被拒绝。
fn assert_reduction_error<T>(result: Result<T, ResourceError>) {
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
}

/// 验证显式 Session 状态和消息不能覆盖 Turn 与工具派生的不变量。
#[test]
fn session_status_and_turn_messages_follow_authoritative_lifecycle() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "status-lifecycle");
    let first_turn = start_turn(&journal, "turn-first");

    assert_reduction_error(journal.append(SessionEvent::SessionStatusChanged {
        status: SessionStatus::Idle,
    }));
    assert_eq!(
        journal.state().expect("状态应读取").status,
        SessionStatus::Running
    );

    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: first_turn.clone(),
        })
        .expect("Turn 应完成");
    assert_reduction_error(journal.append(SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: "late-message".to_owned(),
            turn_id: Some(first_turn),
            agent_id: None,
            role: keencode_resources::MessageRole::Assistant,
            content: vec![keencode_resources::MessagePart::Text {
                text: "迟到消息".to_owned(),
            }],
        },
    }));

    let second_turn = start_turn(&journal, "turn-second");
    let request = tool_request(&journal, "request-running", &second_turn, true);
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("待执行工具应记录");
    assert_eq!(
        journal.state().expect("状态应读取").status,
        SessionStatus::Running
    );
    assert_reduction_error(journal.append(SessionEvent::SessionStatusChanged {
        status: SessionStatus::Idle,
    }));
    assert_eq!(
        journal.state().expect("状态应读取").status,
        SessionStatus::Running
    );
}

/// 验证工具无需审批即可执行，但成功或失败结果仍必须越过持久化执行起点。
#[test]
fn tool_lifecycle_requires_execution_start_without_approval_state() {
    let root = TempDir::new().expect("临时目录应创建");

    let journal = created_journal(root.path(), "tool-execution-start");
    let turn = start_turn(&journal, "turn-execution-start");
    let request = tool_request(&journal, "request-write", &turn, true);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("变更工具请求应直接记录");
    assert_reduction_error(journal.append(SessionEvent::ToolCompleted {
        request_id: request_id.clone(),
        outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Succeeded),
    }));
    start_tool_execution(&journal, &request_id);
    assert_reduction_error(journal.append(SessionEvent::ToolExecutionStarted {
        request_id: request_id.clone(),
    }));
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Succeeded),
        })
        .expect("已开始的变更工具应完成");

    let journal = created_journal(root.path(), "tool-read-only");
    let turn = start_turn(&journal, "turn-read");
    let request = tool_request(&journal, "request-read", &turn, false);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("只读工具应记录");
    start_tool_execution(&journal, &request_id);
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Succeeded),
        })
        .expect("已开始的只读工具应完成");

    let journal = created_journal(root.path(), "tool-cancelled-before-start");
    let turn = start_turn(&journal, "turn-cancelled-before-start");
    let request = tool_request(&journal, "request-cancelled-before-start", &turn, true);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("待执行工具应记录");
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Cancelled),
        })
        .expect("尚未开始的工具应能安全取消");
    materialize_tool(&journal, &request_id);
    journal
        .append(SessionEvent::TurnStopped {
            turn_id: turn,
            reason: TurnStopReason::Cancelled,
            message: "用户取消 Turn".to_owned(),
        })
        .expect("取消待执行工具后 Turn 应可收敛");
}

/// 验证崩溃恢复能区分未执行请求与已经越过副作用起点的未知结果。
#[test]
fn tool_execution_start_survives_replay_as_unknown_side_effect() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("tool-execution-recovery").expect("Session ID 应有效");
    let journal = created_journal(root.path(), session_id.as_str());
    let turn = start_turn(&journal, "turn-execution-recovery");
    let request = tool_request(&journal, "request-execution-recovery", &turn, true);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("工具请求应记录");
    start_tool_execution(&journal, &request_id);
    assert_reduction_error(journal.append(SessionEvent::ToolExecutionStarted {
        request_id: request_id.clone(),
    }));
    assert_reduction_error(journal.append(SessionEvent::TurnCompleted {
        turn_id: turn.clone(),
    }));
    let live = journal.state().expect("实时状态应读取");
    let lifecycle = live.tools.get(&request_id).expect("工具状态应存在");
    assert!(lifecycle.execution_started);
    assert!(lifecycle.outcome.is_none());
    drop(journal);

    let replayed = match SessionJournal::open(root.path(), session_id, config())
        .expect("Session 应可重新打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => panic!("完整执行起点日志不应损坏"),
    };
    let state = replayed.state().expect("重放状态应读取");
    let lifecycle = state.tools.get(&request_id).expect("重放工具状态应存在");
    assert!(lifecycle.execution_started);
    assert!(lifecycle.outcome.is_none());
    replayed
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&replayed, &request_id, ToolCompletionStatus::Cancelled),
        })
        .expect("未知执行可由显式取消结果收敛");
    materialize_tool(&replayed, &request_id);
    replayed
        .append(SessionEvent::TurnStopped {
            turn_id: turn,
            reason: TurnStopReason::Cancelled,
            message: "恢复后取消".to_owned(),
        })
        .expect("工具收敛后 Turn 应可取消");
}

/// 验证终端的执行前置条件、无退出码终态以及重复退出和迟到输出拒绝。
#[test]
fn terminal_lifecycle_distinguishes_running_from_exit_without_code() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("terminal-lifecycle").expect("Session ID 应有效");
    let artifacts = Arc::new(
        ArtifactStore::open(root.path(), session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应打开"),
    );
    let opened = SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id,
        config(),
        artifacts.clone(),
    )
    .expect("Session 应打开");
    let SessionOpen::Ready(journal) = opened else {
        panic!("全新 Session 不应损坏");
    };
    journal
        .append(SessionEvent::SessionCreated {
            title: "终端生命周期".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("Session 应创建");
    let turn = start_turn(&journal, "turn-terminal");

    let pending_request = tool_request(&journal, "request-pending", &turn, true);
    let pending_id = pending_request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested {
            request: pending_request,
        })
        .expect("待执行工具应记录");
    assert_reduction_error(journal.append(SessionEvent::TerminalStarted {
        terminal: terminal_record("terminal-pending", &pending_id),
    }));
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: pending_id.clone(),
            outcome: tool_outcome(&journal, &pending_id, ToolCompletionStatus::Cancelled),
        })
        .expect("未开始工具应由取消结果收敛");

    let request = tool_request(&journal, "request-terminal", &turn, false);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("终端工具应记录");
    start_tool_execution(&journal, &request_id);
    let terminal = terminal_record("terminal-main", &request_id);
    let terminal_id = terminal.terminal_id.clone();
    journal
        .append(SessionEvent::TerminalStarted { terminal })
        .expect("终端应启动");
    let output = artifacts
        .put(b"terminal output", Some("text/plain".to_owned()))
        .expect("终端输出 Artifact 应保存")
        .as_event_use();
    journal
        .append(SessionEvent::TerminalOutputRecorded {
            terminal_id: terminal_id.clone(),
            artifact: output.clone(),
        })
        .expect("运行中终端应接受输出");
    journal
        .append(SessionEvent::TerminalExited {
            terminal_id: terminal_id.clone(),
            exit_code: None,
            cancelled: false,
        })
        .expect("无退出码的正常结束应记录");
    let state = journal.state().expect("状态应读取");
    let terminal = state.terminals.get(&terminal_id).expect("终端应存在");
    assert!(terminal.exited);
    assert_eq!(terminal.exit_code, None);
    assert!(!terminal.cancelled);
    assert_reduction_error(journal.append(SessionEvent::TerminalExited {
        terminal_id: terminal_id.clone(),
        exit_code: Some(0),
        cancelled: false,
    }));
    assert_reduction_error(journal.append(SessionEvent::TerminalOutputRecorded {
        terminal_id,
        artifact: output,
    }));
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Succeeded),
        })
        .expect("终端退出后工具应完成");
    assert_reduction_error(journal.append(SessionEvent::TerminalStarted {
        terminal: terminal_record("terminal-after-complete", &request_id),
    }));
}

/// 验证终态 Turn 引用和压缩范围只能严格指向已有日志前缀。
#[test]
fn terminal_turn_references_and_compaction_ranges_are_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "turn-and-compaction");
    let turn = start_turn(&journal, "turn-complete");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: turn.clone(),
        })
        .expect("Turn 应完成");

    assert_reduction_error(journal.append(SessionEvent::ToolRequested {
        request: tool_request(&journal, "request-late", &turn, false),
    }));
    assert_reduction_error(journal.append(SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: "message-late".to_owned(),
            turn_id: Some(turn),
            agent_id: None,
            role: keencode_resources::MessageRole::Assistant,
            content: vec![keencode_resources::MessagePart::Text {
                text: "迟到".to_owned(),
            }],
        },
    }));

    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    assert_reduction_error(journal.append(SessionEvent::CompactionApplied {
        turn_id: TurnId::new("turn-complete").expect("Turn ID 应有效"),
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
            source_digest_sha256: "0".repeat(64),
            summary: "终态 Turn 不得压缩".to_owned(),
            expected_transcript_revision: 0,
            applied_transcript_revision: 1,
        },
    }));

    let compaction_turn = start_turn(&journal, "turn-compaction");
    for index in 0..2 {
        journal
            .append(SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: format!("compaction-message-{index}"),
                    turn_id: Some(compaction_turn.clone()),
                    agent_id: Some(root_agent.clone()),
                    role: keencode_resources::MessageRole::Assistant,
                    content: vec![keencode_resources::MessagePart::Text {
                        text: format!("压缩消息 {index}"),
                    }],
                },
            })
            .expect("压缩前消息应追加");
    }
    let compaction_digest = journal
        .state()
        .expect("状态应读取")
        .compaction_source_digest_sha256(&compaction_turn, &root_agent, 1, 0, 2)
        .expect("压缩 Digest 应计算");
    journal
        .append(SessionEvent::CompactionApplied {
            turn_id: compaction_turn.clone(),
            source_agent_id: root_agent.clone(),
            model_round: 1,
            compaction: CompactionRecord {
                trigger: ContextCompressionTrigger::Budget,
                estimated_tokens_before: 100,
                estimated_tokens_after: 50,
                replaced_start_index: 0,
                replaced_end_index_exclusive: 2,
                replaced_message_count: 2,
                retained_message_count: 1,
                source_digest_sha256: compaction_digest,
                summary: "有效压缩".to_owned(),
                expected_transcript_revision: 2,
                applied_transcript_revision: 3,
            },
        })
        .expect("正确 Transcript revision 应可压缩");
    assert_reduction_error(journal.append(SessionEvent::CompactionApplied {
        turn_id: compaction_turn,
        source_agent_id: root_agent,
        model_round: 2,
        compaction: CompactionRecord {
            trigger: ContextCompressionTrigger::ProviderOverflow,
            estimated_tokens_before: 100,
            estimated_tokens_after: 50,
            replaced_start_index: 0,
            replaced_end_index_exclusive: 1,
            replaced_message_count: 1,
            retained_message_count: 1,
            source_digest_sha256: "1".repeat(64),
            summary: "过期 revision".to_owned(),
            expected_transcript_revision: 2,
            applied_transcript_revision: 3,
        },
    }));
}

/// 验证所有 Agent 归属均绑定固定 root 或已注册 child，非法事件在写日志前原子拒绝。
#[test]
fn agent_identity_is_registered_scoped_and_revalidated_after_deserialization() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "agent-identity");
    let turn_id = start_turn(&journal, "turn-agent-identity");
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let unknown_agent = AgentId::new("unknown-agent").expect("未知 Agent ID 应有效");
    let baseline_state = journal.state().expect("基线状态应读取");
    let baseline_bytes = std::fs::read(journal.log_path()).expect("基线日志应读取");

    let unknown_request_id = RequestId::derive_model_tool_call(
        &baseline_state.session_id,
        &turn_id,
        &unknown_agent,
        1,
        "unknown-call",
    )
    .expect("未知 Agent Request ID 应可派生");
    let invalid_events = vec![
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "assistant-without-agent".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::Assistant,
                content: vec![MessagePart::Text {
                    text: "不得成为共享 Assistant 消息".to_owned(),
                }],
            },
        },
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "user-with-unknown-agent".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(unknown_agent.clone()),
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "未知作用域".to_owned(),
                }],
            },
        },
        model_round_batch(
            &turn_id,
            &unknown_agent,
            TranscriptSegment {
                turn_id: turn_id.clone(),
                source_agent_id: unknown_agent.clone(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: 0,
                messages: vec![SessionMessage {
                    message_id: "unknown-segment-message".to_owned(),
                    turn_id: Some(turn_id.clone()),
                    agent_id: Some(unknown_agent.clone()),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Text {
                        text: "未知段".to_owned(),
                    }],
                }],
            },
        ),
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: unknown_request_id,
                turn_id: turn_id.clone(),
                agent_id: unknown_agent.clone(),
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "unknown-call".to_owned(),
                tool_name: "read".to_owned(),
                arguments: json!({"path": "README.md"}),
                effect: ToolEffect::ReadOnly,
            },
        },
        SessionEvent::CompactionApplied {
            turn_id: turn_id.clone(),
            source_agent_id: unknown_agent.clone(),
            model_round: 1,
            compaction: CompactionRecord {
                trigger: ContextCompressionTrigger::Budget,
                estimated_tokens_before: 10,
                estimated_tokens_after: 5,
                replaced_start_index: 0,
                replaced_end_index_exclusive: 1,
                replaced_message_count: 1,
                retained_message_count: 1,
                source_digest_sha256: "0".repeat(64),
                summary: "未知 Agent 摘要".to_owned(),
                expected_transcript_revision: 0,
                applied_transcript_revision: 1,
            },
        },
        SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: AgentId::new("invalid-child").expect("子 Agent ID 应有效"),
                parent_agent_id: AgentId::new("invented-root").expect("父 Agent ID 应有效"),
                agent_path: "/root/invalid_child".to_owned(),
                task: "非法父级".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        },
    ];
    for event in invalid_events {
        assert_reduction_error(journal.append(event));
        assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
        assert_eq!(
            std::fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            baseline_bytes
        );
    }

    let child_agent = AgentId::new("child-agent").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child_agent".to_owned(),
                task: "验证上下文隔离".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("固定 root 应可创建 child");
    let child_turn = TurnId::new("turn-child-agent").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: Some(turn_id.clone()),
                    prompt_summary: "验证子 Agent 身份".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Agent Turn 与运行状态应原子开始");
    for (message_id, message_turn_id, agent_id, role, text) in [
        (
            "root-assistant",
            Some(turn_id.clone()),
            Some(root_agent.clone()),
            MessageRole::Assistant,
            "根 Agent 内容",
        ),
        (
            "child-assistant",
            Some(child_turn),
            Some(child_agent.clone()),
            MessageRole::Assistant,
            "子 Agent 内容",
        ),
        ("shared-user", None, None, MessageRole::User, "共享用户输入"),
    ] {
        journal
            .append(SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: message_id.to_owned(),
                    turn_id: message_turn_id,
                    agent_id,
                    role,
                    content: vec![MessagePart::Text {
                        text: text.to_owned(),
                    }],
                },
            })
            .expect("已注册 Agent 或共享用户消息应追加");
    }

    let state = journal.state().expect("身份状态应读取");
    let root_messages = state
        .effective_transcript(&root_agent)
        .expect("根 Agent Transcript 应恢复");
    let child_messages = state
        .effective_transcript(&child_agent)
        .expect("子 Agent Transcript 应恢复");
    assert_eq!(
        root_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["root-assistant", "shared-user"]
    );
    assert_eq!(
        child_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["child-assistant", "shared-user"]
    );

    let mut tampered_registry = state.clone();
    tampered_registry
        .sub_agents
        .get_mut(&child_agent)
        .expect("子 Agent 注册应存在")
        .parent_agent_id = unknown_agent;
    assert!(matches!(
        tampered_registry.validate_transcript_history(),
        Err(ResourceError::Reduction(_))
    ));

    let mut tampered = state;
    let root_message = tampered
        .transcript
        .iter_mut()
        .find_map(|record| match record {
            TranscriptRecord::MessageAdded(message) if message.message_id == "root-assistant" => {
                Some(message)
            }
            TranscriptRecord::MessageAdded(_)
            | TranscriptRecord::SegmentCommitted(_)
            | TranscriptRecord::CompactionApplied(_) => None,
        })
        .expect("根 Agent 消息应存在");
    root_message.agent_id = None;
    assert!(matches!(
        tampered.validate_transcript_history(),
        Err(ResourceError::Reduction(_))
    ));
}

/// 验证单层长寿命子 Agent 的根父级、Turn 绑定、终态摘要与后续恢复矩阵。
#[test]
fn sub_agent_lifecycle_enforces_single_root_and_terminal_summary() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "sub-agent-lifecycle");
    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    let child = AgentId::new("child-one").expect("Agent ID 应有效");
    let root_turn = start_turn(&journal, "turn-sub-agent-root");

    assert_reduction_error(journal.append(SessionEvent::SubAgentSpawned {
        agent: SubAgentState {
            agent_id: child.clone(),
            parent_agent_id: root_agent.clone(),
            agent_path: "/root/child_one".to_owned(),
            task: "无效初态".to_owned(),
            status: SubAgentStatus::Running,
            current_turn_id: None,
            result_summary: None,
        },
    }));
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child_one".to_owned(),
                task: "有效任务".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应以 Pending 创建");
    assert_reduction_error(journal.append(SessionEvent::SubAgentSpawned {
        agent: SubAgentState {
            agent_id: AgentId::new("child-two").expect("Agent ID 应有效"),
            parent_agent_id: AgentId::new("other-root").expect("Agent ID 应有效"),
            agent_path: "/root/child_two".to_owned(),
            task: "不同根父级".to_owned(),
            status: SubAgentStatus::Pending,
            current_turn_id: None,
            result_summary: None,
        },
    }));
    assert_reduction_error(journal.append(SessionEvent::SubAgentStatusChanged {
        agent_id: child.clone(),
        turn_id: None,
        status: SubAgentStatus::Waiting,
        result_summary: None,
    }));
    assert_reduction_error(journal.append(SessionEvent::SubAgentStatusChanged {
        agent_id: child.clone(),
        turn_id: None,
        status: SubAgentStatus::Running,
        result_summary: Some("非终态不得带摘要".to_owned()),
    }));
    let child_turn = TurnId::new("turn-sub-agent-first").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "执行首个子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("Pending 应与首个 Turn 原子进入 Running");
    assert_reduction_error(journal.append(SessionEvent::AtomicBatch {
        events: vec![
            SessionEvent::TurnCompleted {
                turn_id: child_turn.clone(),
            },
            SessionEvent::SubAgentStatusChanged {
                agent_id: child.clone(),
                turn_id: Some(child_turn.clone()),
                status: SubAgentStatus::Completed,
                result_summary: Some("   ".to_owned()),
            },
        ],
    }));
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnCompleted {
                    turn_id: child_turn.clone(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Completed,
                    result_summary: None,
                },
            ],
        })
        .expect("Completed 可无摘要但必须与 Turn 终态原子提交");

    let followup_turn = TurnId::new("turn-sub-agent-followup").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: followup_turn.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn,
                    parent_turn_id: Some(child_turn),
                    prompt_summary: "执行后续子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child.clone(),
                    turn_id: Some(followup_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("Completed 子 Agent 应以新 Turn 恢复 Running");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStopped {
                    turn_id: followup_turn.clone(),
                    reason: TurnStopReason::Cancelled,
                    message: "中断后续任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child,
                    turn_id: Some(followup_turn),
                    status: SubAgentStatus::Interrupted,
                    result_summary: None,
                },
            ],
        })
        .expect("取消 Turn 应把子 Agent 收敛为 Interrupted");
}

/// 验证子 Agent 路径格式、Session 内唯一性和邮箱消息的来源 Turn 因果绑定。
#[test]
fn sub_agent_path_and_mailbox_causality_are_strict() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "sub-agent-path-mailbox-causality");
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let root_turn = start_turn(&journal, "turn-mailbox-root");
    for (index, invalid_path) in [
        "/root/".to_owned(),
        "/root/Upper".to_owned(),
        "/root/two/levels".to_owned(),
        "/root/中文".to_owned(),
        format!("/root/{}", "a".repeat(65)),
        "/other/child".to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        assert_reduction_error(journal.append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id:
                    AgentId::new(format!("invalid-path-{index}")).expect("子 Agent ID 应有效"),
                parent_agent_id: root_agent.clone(),
                agent_path: invalid_path,
                task: "验证路径".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        }));
    }

    let child = AgentId::new("mailbox-child").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/mailbox_child".to_owned(),
                task: "验证邮箱因果关系".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("合法子 Agent 路径应创建");
    assert_reduction_error(journal.append(SessionEvent::SubAgentSpawned {
        agent: SubAgentState {
            agent_id: AgentId::new("mailbox-child-alias").expect("子 Agent ID 应有效"),
            parent_agent_id: root_agent.clone(),
            agent_path: "/root/mailbox_child".to_owned(),
            task: "重复路径".to_owned(),
            status: SubAgentStatus::Pending,
            current_turn_id: None,
            result_summary: None,
        },
    }));

    let child_turn = TurnId::new("turn-mailbox-child").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "执行邮箱子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Agent Turn 应开始");

    let message = |message_id: &str, from: AgentId, to: AgentId, related_turn_id: TurnId| {
        SessionEvent::MailboxMessageQueued {
            message: MailboxMessage {
                message_id: MailboxMessageId::new(message_id).expect("邮箱消息 ID 应有效"),
                from,
                to,
                related_turn_id,
                body: "因果消息".to_owned(),
                artifact: None,
                state: MailboxState::Queued,
            },
        }
    };
    assert_reduction_error(journal.append(message(
        "mail-wrong-source-turn",
        child.clone(),
        root_agent.clone(),
        root_turn.clone(),
    )));
    assert_reduction_error(journal.append(message(
        "mail-missing-turn",
        child.clone(),
        root_agent.clone(),
        TurnId::new("turn-mailbox-missing").expect("缺失 Turn ID 格式仍应有效"),
    )));
    assert_reduction_error(journal.append(message(
        "mail-root-wrong-turn",
        root_agent.clone(),
        child.clone(),
        child_turn.clone(),
    )));
    journal
        .append(message(
            "mail-child-valid",
            child.clone(),
            root_agent.clone(),
            child_turn,
        ))
        .expect("子 Agent 消息应绑定自身 Turn");
    journal
        .append(message("mail-root-valid", root_agent, child, root_turn))
        .expect("根 Agent 消息应绑定自身 Turn");
}

/// 验证关闭 Session 前必须分别收敛 Turn、工具、终端和子 Agent。
#[test]
fn session_close_rejects_each_incomplete_resource_class() {
    let root = TempDir::new().expect("临时目录应创建");

    let journal = created_journal(root.path(), "close-running-turn");
    let turn = start_turn(&journal, "turn-running");
    assert_reduction_error(journal.append(SessionEvent::SessionClosed {}));
    journal
        .append(SessionEvent::TurnCompleted { turn_id: turn })
        .expect("Turn 应完成");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("Turn 收敛后应关闭");

    let journal = created_journal(root.path(), "close-pending-tool");
    let turn = start_turn(&journal, "turn-tool");
    let request = tool_request(&journal, "request-open", &turn, false);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("工具应记录");
    assert_reduction_error(journal.append(SessionEvent::TurnCompleted {
        turn_id: turn.clone(),
    }));
    assert_reduction_error(journal.append(SessionEvent::SessionClosed {}));
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Cancelled),
        })
        .expect("工具应收敛");
    materialize_tool(&journal, &request_id);
    journal
        .append(SessionEvent::TurnCompleted { turn_id: turn })
        .expect("工具收敛后 Turn 应完成");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("工具收敛后应关闭");

    let journal = created_journal(root.path(), "close-running-terminal");
    let turn = start_turn(&journal, "turn-terminal-close");
    let request = tool_request(&journal, "request-terminal-open", &turn, false);
    let request_id = request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("工具应记录");
    let terminal = terminal_record("terminal-open", &request_id);
    let terminal_id = terminal.terminal_id.clone();
    start_tool_execution(&journal, &request_id);
    journal
        .append(SessionEvent::TerminalStarted { terminal })
        .expect("终端应启动");
    assert_reduction_error(journal.append(SessionEvent::TurnCompleted {
        turn_id: turn.clone(),
    }));
    assert_reduction_error(journal.append(SessionEvent::SessionClosed {}));
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
            outcome: tool_outcome(&journal, &request_id, ToolCompletionStatus::Succeeded),
        })
        .expect("工具应完成");
    materialize_tool(&journal, &request_id);
    journal
        .append(SessionEvent::TurnCompleted { turn_id: turn })
        .expect("终端和工具收敛后 Turn 应完成");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("终端和工具收敛后应关闭");

    let journal = created_journal(root.path(), "close-active-sub-agent");
    let child = AgentId::new("child-active").expect("Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child.clone(),
                parent_agent_id: AgentId::new("root").expect("Agent ID 应有效"),
                agent_path: "/root/child_active".to_owned(),
                task: "等待执行".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    assert_reduction_error(journal.append(SessionEvent::SessionClosed {}));
    journal
        .append(SessionEvent::SubAgentStatusChanged {
            agent_id: child,
            turn_id: None,
            status: SubAgentStatus::Stopped,
            result_summary: None,
        })
        .expect("子 Agent 应停止");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("子 Agent 收敛后应关闭");
}

/// 验证结构化停止原因推导粗粒度取消状态。
#[test]
fn stopped_turn_derives_cancellation_terminal_status() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "turn-stopped-status");
    let turn = start_turn(&journal, "turn-stopped");
    journal
        .append(SessionEvent::TurnStopped {
            turn_id: turn.clone(),
            reason: TurnStopReason::Cancelled,
            message: "用户取消".to_owned(),
        })
        .expect("取消终态应接受");
    let state = journal.state().expect("停止状态应读取");
    let stopped = state.turns.get(&turn).expect("Turn 应存在");
    assert_eq!(stopped.status, TurnStatus::Cancelled);
    assert_eq!(stopped.stop_reason, Some(TurnStopReason::Cancelled));
}

/// 验证 Plan 始终只读，且不能在活动 Turn、工具或终端中途开启。
#[test]
fn plan_mode_is_authoritatively_read_only() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "plan-read-only");
    journal
        .append(SessionEvent::PlanChanged {
            plan: PlanState {
                enabled: true,
                plan_artifact: None,
            },
        })
        .expect("空闲 Session 应开启 Plan");
    let turn = start_turn(&journal, "turn-plan");
    assert_reduction_error(journal.append(SessionEvent::ToolRequested {
        request: tool_request(&journal, "request-plan-write", &turn, true),
    }));
    let read_request = tool_request(&journal, "request-plan-read", &turn, false);
    let read_request_id = read_request.request_id.clone();
    journal
        .append(SessionEvent::ToolRequested {
            request: read_request,
        })
        .expect("Plan 应允许只读工具");
    start_tool_execution(&journal, &read_request_id);
    journal
        .append(SessionEvent::ToolCompleted {
            request_id: read_request_id.clone(),
            outcome: tool_outcome(&journal, &read_request_id, ToolCompletionStatus::Succeeded),
        })
        .expect("只读工具应完成");
    materialize_tool(&journal, &read_request_id);
    journal
        .append(SessionEvent::TurnCompleted { turn_id: turn })
        .expect("Plan Turn 应完成");

    let journal = created_journal(root.path(), "plan-mid-turn");
    let turn = start_turn(&journal, "turn-active");
    assert_reduction_error(journal.append(SessionEvent::PlanChanged {
        plan: PlanState {
            enabled: true,
            plan_artifact: None,
        },
    }));
    journal
        .append(SessionEvent::TurnCompleted { turn_id: turn })
        .expect("无工具 Turn 应完成");
}

/// 验证工作树只绑定活跃子 Agent，按规范路径去重，并在关闭前完成释放。
#[test]
fn worktree_lifecycle_uses_normalized_identity_and_blocks_close() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "worktree-lifecycle");
    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    let first = AgentId::new("child-first").expect("Agent ID 应有效");
    let second = AgentId::new("child-second").expect("Agent ID 应有效");
    for agent_id in [&first, &second] {
        journal
            .append(SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: agent_id.clone(),
                    parent_agent_id: root_agent.clone(),
                    agent_path: format!("/root/{}", agent_id.as_str().replace('-', "_")),
                    task: "检查工作树".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            })
            .expect("子 Agent 应创建");
    }
    let path = root
        .path()
        .join("worktrees")
        .join("shared")
        .to_string_lossy()
        .into_owned();
    journal
        .append(SessionEvent::WorktreeAssigned {
            worktree: WorktreeRecord {
                agent_id: first.clone(),
                path: path.clone(),
                branch: "feat/first".to_owned(),
                released: false,
            },
        })
        .expect("首个工作树应绑定");
    let alias = format!("{}/.", path.replace('\\', "/"));
    assert_reduction_error(journal.append(SessionEvent::WorktreeAssigned {
        worktree: WorktreeRecord {
            agent_id: second.clone(),
            path: alias,
            branch: "feat/second".to_owned(),
            released: false,
        },
    }));
    #[cfg(windows)]
    assert_reduction_error(journal.append(SessionEvent::WorktreeAssigned {
        worktree: WorktreeRecord {
            agent_id: second.clone(),
            path: path.to_uppercase(),
            branch: "feat/second".to_owned(),
            released: false,
        },
    }));
    journal
        .append(SessionEvent::SubAgentStatusChanged {
            agent_id: second.clone(),
            turn_id: None,
            status: SubAgentStatus::Stopped,
            result_summary: None,
        })
        .expect("第二个子 Agent 应停止");
    assert_reduction_error(
        journal.append(SessionEvent::WorktreeAssigned {
            worktree: WorktreeRecord {
                agent_id: second.clone(),
                path: root
                    .path()
                    .join("worktrees")
                    .join("stopped")
                    .to_string_lossy()
                    .into_owned(),
                branch: "feat/stopped".to_owned(),
                released: false,
            },
        }),
    );
    journal
        .append(SessionEvent::SubAgentStatusChanged {
            agent_id: first.clone(),
            turn_id: None,
            status: SubAgentStatus::Stopped,
            result_summary: None,
        })
        .expect("第一个子 Agent 应停止");
    assert_reduction_error(journal.append(SessionEvent::SessionClosed {}));
    journal
        .append(SessionEvent::WorktreeReleased { agent_id: first })
        .expect("关闭前应释放工作树");
    journal
        .append(SessionEvent::SessionClosed {})
        .expect("全部工作树释放后应关闭");
}
