mod support;

use std::fs;
use std::path::Path;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, COMPACTION_SUMMARY_PREFIX, CompactionRecord, ContextCompressionTrigger, Durability,
    JournalConfig, MessagePart, MessageRole, PlanState, ResourceError, SessionEvent, SessionId,
    SessionJournal, SessionMessage, SessionOpen, SessionState, SnapshotPolicy, SubAgentState,
    SubAgentStatus, TodoItem, TodoStatus, TranscriptSegment, TurnId, WorktreeRecord,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::TestJournalAppend;

/// 返回关闭自动 Snapshot 的压缩恢复测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开并创建一个全新 Session。
fn created_journal(root: &Path, session: &str) -> SessionJournal {
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
    journal
        .append(SessionEvent::SessionCreated {
            title: "压缩恢复".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("Session 应创建");
    journal
}

/// 创建一个 Running Turn。
fn start_turn(journal: &SessionJournal, turn: &str) -> TurnId {
    let turn_id = TurnId::new(turn).expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: format!("执行 {turn}"),
        })
        .expect("Turn 应开始");
    turn_id
}

/// 向目标 Agent 的原始 Transcript 追加一条消息。
fn append_message(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: Option<&AgentId>,
    message_id: &str,
    role: MessageRole,
    text: &str,
) {
    journal
        .append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: message_id.to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: agent_id.cloned(),
                role,
                content: vec![MessagePart::Text {
                    text: text.to_owned(),
                }],
            },
        })
        .expect("消息应追加");
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
                requested_model: "compaction-test-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some("compaction-test-response".to_owned()),
                    model: Some("compaction-test-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason: StopReason::Completed,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 根据当前有效 Transcript 构造带真实 Digest 的压缩记录。
fn compaction(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
    model_round: u32,
    start: usize,
    end: usize,
    summary: &str,
) -> CompactionRecord {
    let state = journal.state().expect("状态应读取");
    let effective_len = state
        .effective_transcript(agent_id)
        .expect("有效 Transcript 应重建")
        .len();
    CompactionRecord {
        trigger: ContextCompressionTrigger::Budget,
        estimated_tokens_before: 1_000,
        estimated_tokens_after: 100,
        replaced_start_index: start,
        replaced_end_index_exclusive: end,
        replaced_message_count: end - start,
        retained_message_count: effective_len - (end - start) + 1,
        source_digest_sha256: state
            .compaction_source_digest_sha256(turn_id, agent_id, model_round, start, end)
            .expect("压缩来源 Digest 应计算"),
        summary: summary.to_owned(),
        expected_transcript_revision: state.transcript_revision,
        applied_transcript_revision: state.transcript_revision + 1,
    }
}

/// 提交一条绑定完整作用域的压缩事件。
fn apply_compaction(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
    model_round: u32,
    record: CompactionRecord,
) {
    journal
        .append(SessionEvent::CompactionApplied {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            model_round,
            compaction: record,
        })
        .expect("压缩应提交");
}

/// 反序列化一份结构合法但语义被篡改的状态，并断言有效 Transcript 自校验拒绝。
fn assert_tampered_state_rejected(value: Value, agent_id: &AgentId, case: &str) {
    let state: SessionState = serde_json::from_value(value).expect("篡改状态仍应结构合法");
    assert!(
        matches!(
            state.effective_transcript(agent_id),
            Err(ResourceError::Reduction(_))
        ),
        "篡改用例应被拒绝：{case}"
    );
}

/// 验证压缩只替换目标 Agent Transcript，不改写 Todo、Plan、活跃子 Agent 或工作树。
#[test]
fn compaction_preserves_authoritative_session_state_across_restart() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "compaction-authoritative-state";
    let journal = created_journal(root.path(), session_id);
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let child_agent = AgentId::new("child-state").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::PlanChanged {
            plan: PlanState {
                enabled: true,
                plan_artifact: None,
            },
        })
        .expect("Plan 应写入");
    let root_turn = start_turn(&journal, "turn-state-root");
    append_message(
        &journal,
        &root_turn,
        Some(&root_agent),
        "state-message-old",
        MessageRole::User,
        "需要压缩的旧上下文",
    );
    append_message(
        &journal,
        &root_turn,
        Some(&root_agent),
        "state-message-recent",
        MessageRole::Assistant,
        "保留的近期上下文",
    );
    journal
        .append(SessionEvent::TodoReplaced {
            items: vec![TodoItem {
                content: "等待子 Agent 验证".to_owned(),
                status: TodoStatus::InProgress,
                active_form: "正在等待子 Agent 验证".to_owned(),
            }],
            operation_payload_sha256: "a".repeat(64),
            revision: 1,
        })
        .expect("Todo 应写入");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child_state".to_owned(),
                task: "验证压缩状态保持".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-state-child").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "验证压缩状态保持".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn),
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
                path: "D:/workspace/.keencode/worktrees/child-state".to_owned(),
                branch: "feat/child-state".to_owned(),
                released: false,
            },
        })
        .expect("工作树应绑定");

    let before = journal.state().expect("压缩前状态应读取");
    let record = compaction(&journal, &root_turn, &root_agent, 1, 0, 1, "旧上下文摘要");
    apply_compaction(&journal, &root_turn, &root_agent, 1, record);
    let after = journal.state().expect("压缩后状态应读取");

    assert_eq!(after.todos, before.todos);
    assert_eq!(after.plan, before.plan);
    assert_eq!(after.sub_agents, before.sub_agents);
    assert_eq!(after.worktrees, before.worktrees);
    assert_eq!(after.project_root, before.project_root);
    assert_eq!(after.turns, before.turns);
    assert_eq!(after.transcript_revision, before.transcript_revision + 1);
    assert_eq!(
        after
            .effective_transcript(&root_agent)
            .expect("压缩后根 Transcript 应重建")
            .len(),
        2
    );

    journal.write_snapshot().expect("压缩后 Snapshot 应写入");
    drop(journal);
    let reopened = match SessionJournal::open(
        root.path(),
        SessionId::new(session_id).expect("Session ID 应有效"),
        config(),
    )
    .expect("压缩状态应重开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("压缩状态不应损坏：{:?}", report.issues),
    };
    assert_eq!(reopened.state().expect("重开状态应读取"), after);
}

/// 验证连续跨 Turn 压缩、压缩间和压缩后追加在三种恢复路径中完全一致。
#[test]
fn consecutive_compactions_replay_identically_from_live_log_and_snapshot_tail() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "compaction-chain";
    let journal = created_journal(root.path(), session);
    let agent = AgentId::new("root").expect("Agent ID 应有效");
    let first_turn = start_turn(&journal, "turn-first");
    append_message(
        &journal,
        &first_turn,
        Some(&agent),
        "message-1",
        MessageRole::User,
        "第一条",
    );
    append_message(
        &journal,
        &first_turn,
        Some(&agent),
        "message-2",
        MessageRole::Assistant,
        "第二条",
    );
    append_message(
        &journal,
        &first_turn,
        Some(&agent),
        "message-3",
        MessageRole::Assistant,
        "第三条",
    );
    let first = compaction(&journal, &first_turn, &agent, 1, 0, 2, "第一次摘要");
    apply_compaction(&journal, &first_turn, &agent, 1, first);
    journal.write_snapshot().expect("中间 Snapshot 应写入");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: first_turn,
        })
        .expect("第一 Turn 应结束");

    let second_turn = start_turn(&journal, "turn-second");
    append_message(
        &journal,
        &second_turn,
        Some(&agent),
        "message-4",
        MessageRole::Assistant,
        "第四条",
    );
    let second = compaction(&journal, &second_turn, &agent, 2, 0, 2, "第二次摘要");
    apply_compaction(&journal, &second_turn, &agent, 2, second);
    append_message(
        &journal,
        &second_turn,
        Some(&agent),
        "message-5",
        MessageRole::Assistant,
        "压缩后追加",
    );

    let live_state = journal.state().expect("实时状态应读取");
    let live = live_state
        .effective_transcript(&agent)
        .expect("实时有效 Transcript 应重建");
    assert_eq!(live.len(), 3);
    assert_eq!(live[0].role, MessageRole::User);
    assert!(matches!(
        &live[0].content[0],
        MessagePart::Text { text }
            if text == &format!("{COMPACTION_SUMMARY_PREFIX}第二次摘要")
    ));
    assert_eq!(live[2].message_id, "message-5");
    let snapshot_path = journal.snapshot_path().to_path_buf();
    drop(journal);

    let snapshot_tail = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Snapshot + tail 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("恢复不应损坏：{:?}", report.issues),
    };
    assert_eq!(snapshot_tail.state().expect("状态应读取"), live_state);
    assert_eq!(
        snapshot_tail
            .state()
            .expect("状态应读取")
            .effective_transcript(&agent)
            .expect("有效 Transcript 应重建"),
        live
    );
    drop(snapshot_tail);

    fs::remove_file(snapshot_path).expect("Snapshot 应删除以强制纯日志重放");
    let log_only = match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("纯日志应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("纯日志不应损坏：{:?}", report.issues),
    };
    assert_eq!(log_only.state().expect("状态应读取"), live_state);
    assert_eq!(
        log_only
            .state()
            .expect("状态应读取")
            .effective_transcript(&agent)
            .expect("有效 Transcript 应重建"),
        live
    );
}

/// 验证根与子 Agent 的压缩互不串扰，且 Turn、Agent 或 Round 变化会破坏 Digest。
#[test]
fn agent_compactions_are_isolated_and_digest_binds_turn_agent_and_round() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "compaction-agent-scope");
    let second_turn = start_turn(&journal, "turn-second");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: second_turn.clone(),
        })
        .expect("用于错误作用域校验的根 Turn 应先结束");
    let first_turn = start_turn(&journal, "turn-first");
    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    let child_agent = AgentId::new("child").expect("Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: root_agent.clone(),
                agent_path: "/root/child".to_owned(),
                task: "验证子 Agent 上下文隔离".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-child").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: first_turn.clone(),
                    parent_turn_id: Some(first_turn.clone()),
                    prompt_summary: "执行子 Agent 压缩".to_owned(),
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
    for (index, agent) in [&root_agent, &root_agent].into_iter().enumerate() {
        append_message(
            &journal,
            &first_turn,
            Some(agent),
            &format!("scope-message-{index}"),
            MessageRole::Assistant,
            &format!("作用域消息 {index}"),
        );
    }
    for (index, agent) in [&child_agent, &child_agent].into_iter().enumerate() {
        append_message(
            &journal,
            &child_turn,
            Some(agent),
            &format!("scope-child-message-{index}"),
            MessageRole::Assistant,
            &format!("子作用域消息 {index}"),
        );
    }
    let root_record = compaction(&journal, &first_turn, &root_agent, 1, 0, 2, "根摘要");
    let baseline = journal.state().expect("状态应读取");
    for (index, event) in [
        SessionEvent::CompactionApplied {
            turn_id: first_turn.clone(),
            source_agent_id: child_agent.clone(),
            model_round: 1,
            compaction: root_record.clone(),
        },
        SessionEvent::CompactionApplied {
            turn_id: second_turn,
            source_agent_id: root_agent.clone(),
            model_round: 1,
            compaction: root_record.clone(),
        },
        SessionEvent::CompactionApplied {
            turn_id: first_turn.clone(),
            source_agent_id: root_agent.clone(),
            model_round: 2,
            compaction: root_record.clone(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let result = journal.append_idempotent(
            keencode_resources::SessionEventId::new(format!("event-wrong-scope-{index}"))
                .expect("事件 ID 应有效"),
            baseline.last_sequence,
            event,
        );
        assert!(matches!(result, Err(ResourceError::Reduction(_))));
        assert_eq!(journal.state().expect("状态应读取"), baseline);
    }

    apply_compaction(&journal, &first_turn, &root_agent, 1, root_record);
    let state = journal.state().expect("状态应读取");
    let root_effective = state
        .effective_transcript(&root_agent)
        .expect("根上下文应重建");
    let child_effective = state
        .effective_transcript(&child_agent)
        .expect("子上下文应重建");
    assert_eq!(root_effective.len(), 1);
    assert_eq!(child_effective.len(), 2);

    let child_record = compaction(&journal, &child_turn, &child_agent, 1, 0, 2, "子摘要");
    apply_compaction(&journal, &child_turn, &child_agent, 1, child_record);
    let state = journal.state().expect("状态应读取");
    assert_eq!(
        state
            .effective_transcript(&root_agent)
            .expect("根上下文应重建"),
        root_effective
    );
    assert_eq!(
        state
            .effective_transcript(&child_agent)
            .expect("子上下文应重建")
            .len(),
        1
    );

    let mut tampered = serde_json::to_value(&state).expect("状态应编码");
    tampered["transcript"][5]["payload"]["record"]["sourceDigestSha256"] = json!("f".repeat(64));
    let tampered: SessionState = serde_json::from_value(tampered).expect("篡改状态仍应结构合法");
    assert!(matches!(
        tampered.effective_transcript(&root_agent),
        Err(ResourceError::Reduction(_))
    ));
    assert!(matches!(
        tampered.validate_transcript_history(),
        Err(ResourceError::Reduction(_))
    ));
}

/// 验证格式合法的伪 Digest、指令消息和拆开的工具交换都不能被压缩。
#[test]
fn compaction_rejects_forged_digest_instructions_and_split_tool_exchange() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "compaction-rejections");
    let turn = start_turn(&journal, "turn-main");
    let agent = AgentId::new("root").expect("Agent ID 应有效");
    append_message(
        &journal,
        &turn,
        Some(&agent),
        "message-user",
        MessageRole::User,
        "可压缩",
    );
    let mut forged = compaction(&journal, &turn, &agent, 1, 0, 1, "伪造摘要");
    forged.source_digest_sha256 = if forged.source_digest_sha256.starts_with('f') {
        "e".repeat(64)
    } else {
        "f".repeat(64)
    };
    assert!(matches!(
        journal.append(SessionEvent::CompactionApplied {
            turn_id: turn.clone(),
            source_agent_id: agent.clone(),
            model_round: 1,
            compaction: forged,
        }),
        Err(ResourceError::Reduction(_))
    ));

    append_message(
        &journal,
        &turn,
        None,
        "message-system",
        MessageRole::System,
        "不可压缩指令",
    );
    let instruction = compaction(&journal, &turn, &agent, 1, 1, 2, "错误指令摘要");
    assert!(matches!(
        journal.append(SessionEvent::CompactionApplied {
            turn_id: turn.clone(),
            source_agent_id: agent.clone(),
            model_round: 1,
            compaction: instruction,
        }),
        Err(ResourceError::Reduction(_))
    ));

    let segment = TranscriptSegment {
        turn_id: turn.clone(),
        source_agent_id: agent.clone(),
        model_round: 1,
        segment_index: 0,
        expected_transcript_revision: 2,
        messages: vec![
            SessionMessage {
                message_id: "synthetic-call".to_owned(),
                turn_id: Some(turn.clone()),
                agent_id: Some(agent.clone()),
                role: MessageRole::Assistant,
                content: vec![MessagePart::ToolCall {
                    tool_call_id: "unknown-call".to_owned(),
                    tool_name: "unknown_tool".to_owned(),
                    arguments: serde_json::json!({"value": 1}),
                }],
            },
            SessionMessage {
                message_id: "synthetic-result".to_owned(),
                turn_id: Some(turn.clone()),
                agent_id: Some(agent.clone()),
                role: MessageRole::Tool,
                content: vec![MessagePart::ToolResult {
                    tool_call_id: "unknown-call".to_owned(),
                    content: vec![],
                    is_error: true,
                }],
            },
        ],
    };
    journal
        .append(model_round_batch(&turn, &agent, segment))
        .expect("Agent 合成错误段应提交");
    let split = compaction(&journal, &turn, &agent, 1, 2, 3, "拆分工具摘要");
    assert!(matches!(
        journal.append(SessionEvent::CompactionApplied {
            turn_id: turn,
            source_agent_id: agent,
            model_round: 1,
            compaction: split,
        }),
        Err(ResourceError::Reduction(_))
    ));
}

/// 验证外部反序列化状态不能绕过 revision、计数、摘要或 Digest 自校验。
#[test]
fn deserialized_state_revalidates_compaction_and_revision_invariants() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = created_journal(root.path(), "compaction-state-validation");
    let turn = start_turn(&journal, "turn-main");
    let agent = AgentId::new("root").expect("Agent ID 应有效");
    append_message(
        &journal,
        &turn,
        Some(&agent),
        "message-user",
        MessageRole::User,
        "第一条",
    );
    journal
        .append(model_round_batch(
            &turn,
            &agent,
            TranscriptSegment {
                turn_id: turn.clone(),
                source_agent_id: agent.clone(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: 1,
                messages: vec![SessionMessage {
                    message_id: "message-assistant".to_owned(),
                    turn_id: Some(turn.clone()),
                    agent_id: Some(agent.clone()),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Text {
                        text: "第二条".to_owned(),
                    }],
                }],
            },
        ))
        .expect("Transcript 段应提交");
    append_message(
        &journal,
        &turn,
        Some(&agent),
        "message-after-segment",
        MessageRole::Assistant,
        "第三条",
    );
    let record = compaction(&journal, &turn, &agent, 2, 0, 2, "合法摘要");
    apply_compaction(&journal, &turn, &agent, 2, record);
    let baseline = serde_json::to_value(journal.state().expect("状态应读取")).expect("状态应编码");

    let restored: SessionState =
        serde_json::from_value(baseline.clone()).expect("未篡改状态应可反序列化");
    restored
        .validate_transcript_history()
        .expect("未篡改状态的完整 Transcript 历史应有效");
    let effective = restored
        .effective_transcript(&agent)
        .expect("未篡改状态应重建有效 Transcript");
    assert_eq!(effective.len(), 2);
    assert!(matches!(
        &effective[0].content[0],
        MessagePart::Text { text }
            if text == &format!("{COMPACTION_SUMMARY_PREFIX}合法摘要")
    ));
    assert_eq!(effective[1].message_id, "message-after-segment");

    let mut invalid_state_revision = baseline.clone();
    invalid_state_revision["transcriptRevision"] = json!(5);
    assert_tampered_state_rejected(invalid_state_revision, &agent, "状态 revision");

    let mut invalid_segment_revision = baseline.clone();
    invalid_segment_revision["transcript"][1]["payload"]["expectedTranscriptRevision"] = json!(0);
    assert_tampered_state_rejected(invalid_segment_revision, &agent, "段 revision");

    let mut invalid_expected_compaction_revision = baseline.clone();
    invalid_expected_compaction_revision["transcript"][3]["payload"]["record"]["expectedTranscriptRevision"] =
        json!(2);
    assert_tampered_state_rejected(
        invalid_expected_compaction_revision,
        &agent,
        "压缩 expected revision",
    );

    let mut invalid_compaction_revision = baseline.clone();
    invalid_compaction_revision["transcript"][3]["payload"]["record"]["appliedTranscriptRevision"] =
        json!(5);
    assert_tampered_state_rejected(invalid_compaction_revision, &agent, "压缩 applied revision");

    for (field, value) in [
        ("replacedMessageCount", json!(1)),
        ("retainedMessageCount", json!(3)),
        ("summary", json!("   ")),
        ("sourceDigestSha256", json!("f".repeat(64))),
    ] {
        let mut tampered = baseline.clone();
        tampered["transcript"][3]["payload"]["record"][field] = value;
        assert_tampered_state_rejected(tampered, &agent, field);
    }

    let mut tampered_source_message = baseline.clone();
    tampered_source_message["transcript"][0]["payload"]["content"][0]["text"] =
        json!("已篡改的第一条");
    assert_tampered_state_rejected(tampered_source_message, &agent, "来源消息正文");

    let mut tampered_compaction_turn = baseline.clone();
    tampered_compaction_turn["transcript"][3]["payload"]["turnId"] = json!("missing-turn");
    assert_tampered_state_rejected(tampered_compaction_turn, &agent, "压缩 Turn 归属");

    let mut tampered_session = baseline.clone();
    tampered_session["sessionId"] = json!("other-session");
    assert_tampered_state_rejected(tampered_session, &agent, "Session 作用域");

    let mut tampered_digest_revision = baseline.clone();
    let mut other_agent_message = tampered_digest_revision["transcript"][0].clone();
    other_agent_message["payload"]["messageId"] = json!("other-agent-message");
    other_agent_message["payload"]["agentId"] = json!("other-agent");
    tampered_digest_revision["transcript"]
        .as_array_mut()
        .expect("Transcript 应是数组")
        .insert(3, other_agent_message);
    tampered_digest_revision["transcriptRevision"] = json!(5);
    tampered_digest_revision["transcript"][4]["payload"]["record"]["expectedTranscriptRevision"] =
        json!(4);
    tampered_digest_revision["transcript"][4]["payload"]["record"]["appliedTranscriptRevision"] =
        json!(5);
    assert_tampered_state_rejected(tampered_digest_revision, &agent, "Digest revision 作用域");

    let mut tampered_range = baseline;
    tampered_range["transcript"][3]["payload"]["record"]["replacedStartIndex"] = json!(1);
    tampered_range["transcript"][3]["payload"]["record"]["replacedEndIndexExclusive"] = json!(3);
    assert_tampered_state_rejected(tampered_range, &agent, "替换范围");
}
