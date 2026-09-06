mod support;

use std::fs;

use keencode_resources::{
    AgentId, IdempotentAppendOutcome, JournalConfig, MessageImageSource, MessagePart, MessageRole,
    PersistedToolResult, RequestId, ResourceError, SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION,
    SessionEvent, SessionEventId, SessionEventRecord, SessionId, SessionJournal, SessionMessage,
    SessionOpen, SessionState, SubAgentState, SubAgentStatus, ToolCompletionStatus, ToolEffect,
    ToolRequest, ToolResultPart, TurnId, TurnStopReason, reduce_record, side_effect_unknown_result,
};
use serde_json::json;
use tempfile::TempDir;

use support::TestJournalAppend;

/// 打开一个尚未创建业务状态的可写测试 Session。
fn journal(root: &TempDir, session: &str) -> SessionJournal {
    match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        JournalConfig::default(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
    }
}

/// 写入当前测试唯一的 Session 创建事件。
fn create_session(journal: &SessionJournal) {
    journal
        .append(SessionEvent::SessionCreated {
            title: "Runtime 持久化契约".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("Session 应创建");
}

/// 构造一个携带完整根 Agent 谱系的 Turn 起点。
fn root_turn_started(turn_id: &TurnId) -> SessionEvent {
    SessionEvent::TurnStarted {
        turn_id: turn_id.clone(),
        source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
        root_turn_id: turn_id.clone(),
        parent_turn_id: None,
        prompt_summary: "执行 Runtime 测试".to_owned(),
    }
}

/// 使用目标状态的下一序号构造一条可直接交给公开归约器的当前版本记录。
fn event_record(state: &SessionState, event_id: &str, event: SessionEvent) -> SessionEventRecord {
    SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new(event_id).expect("事件 ID 应有效"),
        session: state.session_id.clone(),
        sequence: state
            .last_sequence
            .checked_add(1)
            .expect("测试序号不应溢出"),
        time_unix_ms: 1,
        event,
    }
}

/// 返回目标 Agent 当前有效 Transcript 中按顺序排列的消息标识。
fn effective_message_ids(state: &SessionState, agent_id: &AgentId) -> Vec<String> {
    state
        .effective_transcript(agent_id)
        .expect("目标 Agent Transcript 应恢复")
        .into_iter()
        .map(|message| message.message_id)
        .collect()
}

/// 构造一个指定 Round 下标的根 Agent 只读工具请求。
fn tool_request(
    journal: &SessionJournal,
    turn_id: &TurnId,
    call_id: &str,
    request_index: u32,
) -> ToolRequest {
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    ToolRequest {
        request_id: RequestId::derive_model_tool_call(
            &journal.state().expect("状态应读取").session_id,
            turn_id,
            &agent_id,
            1,
            call_id,
        )
        .expect("Request ID 应派生"),
        turn_id: turn_id.clone(),
        agent_id,
        model_round: 1,
        request_index,
        model_tool_call_id: call_id.to_owned(),
        tool_name: "read".to_owned(),
        arguments: json!({"path": "src/lib.rs"}),
        effect: ToolEffect::ReadOnly,
    }
}

/// 验证 Turn 起点与完整用户消息只占一条物理记录，并可按批次正文幂等重试。
#[test]
fn atomic_batch_commits_turn_and_input_as_one_physical_record() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "atomic-turn-input");
    create_session(&journal);
    let turn_id = TurnId::new("turn-root").expect("Turn ID 应有效");
    let events = vec![
        root_turn_started(&turn_id),
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-user".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "实现功能".to_owned(),
                }],
            },
        },
    ];
    let batch_id = SessionEventId::new("batch-turn-input").expect("批次 ID 应有效");
    let first = journal
        .append_batch_idempotent(batch_id.clone(), 1, events.clone())
        .expect("原子批次应提交");
    assert!(matches!(first, IdempotentAppendOutcome::Appended(_)));
    let state = journal.state().expect("状态应读取");
    assert_eq!(state.last_sequence, 2);
    assert_eq!(state.turns.len(), 1);
    assert_eq!(state.raw_transcript_messages().len(), 1);
    assert_eq!(
        fs::read_to_string(journal.log_path())
            .expect("日志应读取")
            .lines()
            .count(),
        2
    );

    let repeated = journal
        .append_batch_idempotent(batch_id.clone(), 1, events)
        .expect("相同批次应安全重试");
    assert!(matches!(
        repeated,
        IdempotentAppendOutcome::AlreadyCommitted { .. }
    ));
    let conflict = journal
        .append_batch_idempotent(
            batch_id,
            2,
            vec![SessionEvent::SessionStatusChanged {
                status: keencode_resources::SessionStatus::Waiting,
            }],
        )
        .expect("相同标识的不同正文应返回明确冲突");
    assert!(matches!(
        conflict,
        IdempotentAppendOutcome::EventIdConflict { .. }
    ));
}

/// 验证公开归约器拒绝未原子配对的子 Agent Turn 时不会留下任何部分状态。
#[test]
fn public_reduce_rejects_unpaired_child_turn_events_transactionally() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "transactional-child-turn");
    create_session(&journal);
    let root_turn = TurnId::new("turn-root").expect("根 Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");
    let child_agent = AgentId::new("child").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child".to_owned(),
                task: "验证事务归约".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-child").expect("子 Turn ID 应有效");
    let child_start = SessionEvent::TurnStarted {
        turn_id: child_turn.clone(),
        source_agent_id: child_agent.clone(),
        root_turn_id: root_turn.clone(),
        parent_turn_id: Some(root_turn.clone()),
        prompt_summary: "开始子任务".to_owned(),
    };
    let pending_state = journal.state().expect("Pending 状态应读取");
    let mut rejected_start_state = pending_state.clone();
    let rejected_start = reduce_record(
        &mut rejected_start_state,
        event_record(
            &pending_state,
            "event-child-start-unpaired",
            child_start.clone(),
        ),
    );
    assert!(rejected_start.is_err());
    assert_eq!(rejected_start_state, pending_state);

    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                child_start,
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent,
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("配对子 Turn 应原子开始");
    let running_state = journal.state().expect("Running 状态应读取");
    for (event_id, event) in [
        (
            "event-child-complete-unpaired",
            SessionEvent::TurnCompleted {
                turn_id: child_turn.clone(),
            },
        ),
        (
            "event-child-stop-unpaired",
            SessionEvent::TurnStopped {
                turn_id: child_turn,
                reason: TurnStopReason::Failed,
                message: "合成失败".to_owned(),
            },
        ),
    ] {
        let mut rejected_terminal_state = running_state.clone();
        let result = reduce_record(
            &mut rejected_terminal_state,
            event_record(&running_state, event_id, event),
        );
        assert!(result.is_err());
        assert_eq!(rejected_terminal_state, running_state);
    }
}

/// 验证完整但无法归约的日志行不会污染损坏报告中的最后健康前缀状态。
#[test]
fn corrupt_replay_stops_before_unpaired_child_turn_without_polluting_prefix() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("corrupt-unpaired-child-turn").expect("Session ID 应有效");
    let journal = journal(&root, session_id.as_str());
    create_session(&journal);
    let root_turn = TurnId::new("turn-root").expect("根 Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");
    let child_agent = AgentId::new("child").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child".to_owned(),
                task: "验证损坏重放".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let baseline_state = journal.state().expect("健康前缀状态应读取");
    let invalid_record = event_record(
        &baseline_state,
        "event-corrupt-child-start",
        SessionEvent::TurnStarted {
            turn_id: TurnId::new("turn-child").expect("子 Turn ID 应有效"),
            source_agent_id: child_agent,
            root_turn_id: root_turn.clone(),
            parent_turn_id: Some(root_turn),
            prompt_summary: "未配对子任务".to_owned(),
        },
    );
    let log_path = journal.log_path().to_path_buf();
    drop(journal);
    let mut log_bytes = fs::read(&log_path).expect("健康日志应读取");
    log_bytes.extend(serde_json::to_vec(&invalid_record).expect("损坏记录应序列化"));
    log_bytes.push(b'\n');
    fs::write(&log_path, log_bytes).expect("损坏记录应写入测试日志");

    let opened = SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("损坏 Session 应返回只读报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("未配对子 Turn 必须被识别为损坏");
    };
    assert_eq!(report.valid_records, baseline_state.last_sequence as usize);
    assert_eq!(report.last_valid_state, baseline_state);
}

/// 验证批次中任一事件无效时，之前的候选事件不会部分进入状态或日志。
#[test]
fn invalid_atomic_batch_leaves_journal_byte_identical() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "atomic-batch-reject");
    create_session(&journal);
    let before = fs::read(journal.log_path()).expect("日志应读取");
    let turn_id = TurnId::new("turn-invalid").expect("Turn ID 应有效");
    let result = journal.append_batch_idempotent(
        SessionEventId::new("batch-invalid").expect("批次 ID 应有效"),
        1,
        vec![
            root_turn_started(&turn_id),
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: "message-empty".to_owned(),
                    turn_id: Some(turn_id.clone()),
                    agent_id: None,
                    role: MessageRole::User,
                    content: Vec::new(),
                },
            },
        ],
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
    assert!(
        !journal
            .state()
            .expect("状态应读取")
            .turns
            .contains_key(&turn_id)
    );
}

/// 验证空、超量、嵌套及再次创建 Session 的批次在序列化和写日志前统一拒绝。
#[test]
fn atomic_batch_shape_limits_leave_log_and_state_unchanged() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "atomic-batch-shape");
    create_session(&journal);
    let baseline_state = journal.state().expect("基线状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("基线日志应读取");
    let status_event = SessionEvent::SessionStatusChanged {
        status: keencode_resources::SessionStatus::Waiting,
    };
    let mut deeply_nested = status_event.clone();
    for _ in 0..4_096 {
        deeply_nested = SessionEvent::AtomicBatch {
            events: vec![deeply_nested],
        };
    }
    let invalid = [
        SessionEvent::AtomicBatch { events: Vec::new() },
        SessionEvent::AtomicBatch {
            events: vec![status_event.clone(); 1_025],
        },
        SessionEvent::AtomicBatch {
            events: vec![SessionEvent::AtomicBatch {
                events: vec![status_event],
            }],
        },
        SessionEvent::AtomicBatch {
            events: vec![SessionEvent::SessionCreated {
                title: "重复创建".to_owned(),
                project_root: "D:/other".to_owned(),
            }],
        },
        deeply_nested,
    ];
    for (index, event) in invalid.into_iter().enumerate() {
        let result = journal.append_idempotent(
            SessionEventId::new(format!("batch-shape-invalid-{index}")).expect("事件 ID 应有效"),
            baseline_state.last_sequence,
            event,
        );
        assert!(matches!(result, Err(ResourceError::Reduction(_))));
        assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
        assert_eq!(
            fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            baseline_log
        );
    }
}

/// 验证公开归约入口接管超深批次所有权，并在拒绝时以迭代方式释放完整事件树。
#[test]
fn public_reduce_owns_and_iteratively_rejects_deep_atomic_batch() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "public-deep-atomic-batch");
    create_session(&journal);
    let baseline = journal.state().expect("基线状态应读取");
    let mut deeply_nested = SessionEvent::SessionStatusChanged {
        status: keencode_resources::SessionStatus::Waiting,
    };
    for _ in 0..16_384 {
        deeply_nested = SessionEvent::AtomicBatch {
            events: vec![deeply_nested],
        };
    }
    let record = event_record(&baseline, "event-public-deep-batch", deeply_nested);
    let mut rejected = baseline.clone();

    assert!(reduce_record(&mut rejected, record).is_err());
    assert_eq!(rejected, baseline);
}

/// 验证同一任务树可按 root -> child -> root -> child 继续，并在重放后保留完整谱系。
#[test]
fn turn_lineage_is_validated_and_replayed() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("turn-lineage").expect("Session ID 应有效");
    let journal =
        match SessionJournal::open(root.path(), session_id.clone(), JournalConfig::default())
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
        };
    create_session(&journal);
    let root_turn = TurnId::new("turn-root").expect("Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");
    let child_agent = AgentId::new("child").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child".to_owned(),
                task: "审计实现".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-child").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "执行子任务".to_owned(),
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
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: root_turn.clone(),
        })
        .expect("根 Agent 旧 Turn 应先结束");
    let root_followup_turn = TurnId::new("turn-root-followup").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: root_followup_turn.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: root_turn.clone(),
            parent_turn_id: Some(child_turn.clone()),
            prompt_summary: "接收子 Agent 结果并继续".to_owned(),
        })
        .expect("根 Agent Followup 应保持原任务树谱系");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnCompleted {
                    turn_id: child_turn.clone(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Completed,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Agent 旧 Turn 应与完成状态原子结束");
    let child_followup_turn = TurnId::new("turn-child-followup").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_followup_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_followup_turn.clone()),
                    prompt_summary: "继续验证根 Agent 的后续要求".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_followup_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Agent 应可继续同一任务树");
    let before_stale = fs::read(journal.log_path()).expect("延迟终态前日志应读取");
    assert!(matches!(
        journal.append(SessionEvent::SubAgentStatusChanged {
            agent_id: child_agent.clone(),
            turn_id: Some(child_turn.clone()),
            status: SubAgentStatus::Completed,
            result_summary: Some("旧 Turn 延迟完成".to_owned()),
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(
        fs::read(journal.log_path()).expect("延迟终态拒绝后日志应读取"),
        before_stale
    );
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnCompleted {
                    turn_id: child_followup_turn.clone(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_followup_turn.clone()),
                    status: SubAgentStatus::Completed,
                    result_summary: Some("后续任务完成".to_owned()),
                },
            ],
        })
        .expect("后续子 Turn 应与完成状态原子结束");
    drop(journal);

    let replayed = match SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("完整谱系不应损坏：{:?}", report.issues),
    };
    let state = replayed.state().expect("重放状态应读取");
    let turn = state.turns.get(&child_turn).expect("子 Turn 应恢复");
    assert_eq!(turn.source_agent_id, child_agent.clone());
    assert_eq!(turn.root_turn_id, root_turn.clone());
    assert_eq!(turn.parent_turn_id.as_ref(), Some(&root_turn));
    let root_followup = state
        .turns
        .get(&root_followup_turn)
        .expect("根 Agent Followup 应恢复");
    assert_eq!(root_followup.source_agent_id.as_str(), "root");
    assert_eq!(root_followup.root_turn_id, root_turn);
    assert_eq!(root_followup.parent_turn_id.as_ref(), Some(&child_turn));
    let child_followup = state
        .turns
        .get(&child_followup_turn)
        .expect("子 Agent Followup 应恢复");
    assert_eq!(child_followup.source_agent_id, child_agent);
    assert_eq!(child_followup.root_turn_id, root_followup.root_turn_id);
    assert_eq!(
        child_followup.parent_turn_id.as_ref(),
        Some(&root_followup_turn)
    );
    let child_state = state
        .sub_agents
        .get(&child_followup.source_agent_id)
        .expect("长寿命子 Agent 状态应恢复");
    assert_eq!(child_state.status, SubAgentStatus::Completed);
    assert_eq!(
        child_state.current_turn_id.as_ref(),
        Some(&child_followup_turn)
    );
    assert_eq!(child_state.result_summary.as_deref(), Some("后续任务完成"));
}

/// 验证同一 root 或同一子 Agent 的重叠 Running Turn 会原子拒绝且不写入日志。
#[test]
fn overlapping_turns_for_same_agent_are_atomically_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "overlapping-turns");
    create_session(&journal);
    let root_turn = TurnId::new("turn-root-active").expect("根 Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");

    let baseline = journal.state().expect("根重叠前状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("根重叠前日志应读取");
    let overlapping_root = TurnId::new("turn-root-overlap").expect("根 Turn ID 应有效");
    assert!(matches!(
        journal.append(SessionEvent::AtomicBatch {
            events: vec![root_turn_started(&overlapping_root)],
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(journal.state().expect("根拒绝后状态应读取"), baseline);
    assert_eq!(
        fs::read(journal.log_path()).expect("根拒绝后日志应读取"),
        baseline_log
    );

    let child = AgentId::new("child-overlap").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child_overlap".to_owned(),
                task: "验证重叠 Turn".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-child-active").expect("子 Turn ID 应有效");
    let bypass_turn = TurnId::new("turn-child-bypass").expect("子 Turn ID 应有效");
    let baseline = journal.state().expect("穿越状态前基线应读取");
    let baseline_log = fs::read(journal.log_path()).expect("穿越状态前日志应读取");
    assert!(matches!(
        journal.append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: bypass_turn.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "不得跳过子 Agent 状态".to_owned(),
                },
                SessionEvent::TurnCompleted {
                    turn_id: bypass_turn,
                },
            ],
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(journal.state().expect("穿越拒绝后状态应读取"), baseline);
    assert_eq!(
        fs::read(journal.log_path()).expect("穿越拒绝后日志应读取"),
        baseline_log
    );
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "首个子 Turn".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("首个子 Turn 应开始");
    let baseline = journal.state().expect("子重叠前状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("子重叠前日志应读取");
    let overlapping_child = TurnId::new("turn-child-overlap").expect("子 Turn ID 应有效");
    assert!(matches!(
        journal.append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: overlapping_child.clone(),
                    source_agent_id: child.clone(),
                    root_turn_id: root_turn,
                    parent_turn_id: Some(child_turn),
                    prompt_summary: "重叠子 Turn".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child,
                    turn_id: Some(overlapping_child),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(journal.state().expect("子拒绝后状态应读取"), baseline);
    assert_eq!(
        fs::read(journal.log_path()).expect("子拒绝后日志应读取"),
        baseline_log
    );
}

/// 验证跨任务树、缺失父 Turn 和伪造根结构都在写日志前被拒绝。
#[test]
fn invalid_turn_lineage_is_rejected_without_persistence() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "invalid-turn-lineage");
    create_session(&journal);
    let first_root = TurnId::new("turn-root-first").expect("Turn ID 应有效");
    let second_root = TurnId::new("turn-root-second").expect("Turn ID 应有效");
    journal
        .append(root_turn_started(&first_root))
        .expect("第一棵任务树应创建");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: first_root.clone(),
        })
        .expect("第一棵任务树的根 Turn 应先结束");
    journal
        .append(root_turn_started(&second_root))
        .expect("第二棵任务树应创建");
    journal
        .append(SessionEvent::TurnCompleted {
            turn_id: second_root.clone(),
        })
        .expect("第二棵任务树的根 Turn 应先结束");
    let child_agent = AgentId::new("child-lineage").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child_lineage".to_owned(),
                task: "验证非法谱系".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");
    let child_turn = TurnId::new("turn-child-first-tree").expect("Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: first_root.clone(),
                    parent_turn_id: Some(first_root.clone()),
                    prompt_summary: "第一棵任务树的子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent,
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("合法子 Turn 应开始");

    let before = fs::read(journal.log_path()).expect("拒绝前日志应读取");
    let invalid = [
        SessionEvent::TurnStarted {
            turn_id: TurnId::new("turn-cross-tree").expect("Turn ID 应有效"),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: second_root,
            parent_turn_id: Some(child_turn.clone()),
            prompt_summary: "不得跨树继续".to_owned(),
        },
        SessionEvent::TurnStarted {
            turn_id: TurnId::new("turn-missing-parent").expect("Turn ID 应有效"),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: first_root.clone(),
            parent_turn_id: Some(TurnId::new("turn-forged-parent").expect("伪造父 Turn ID 应有效")),
            prompt_summary: "不得引用缺失父 Turn".to_owned(),
        },
        SessionEvent::TurnStarted {
            turn_id: TurnId::new("turn-forged-root").expect("Turn ID 应有效"),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: TurnId::new("turn-forged-root").expect("Turn ID 应有效"),
            parent_turn_id: Some(child_turn),
            prompt_summary: "不得伪装新的根用户 Turn".to_owned(),
        },
    ];
    for event in invalid {
        assert!(matches!(
            journal.append(event),
            Err(ResourceError::Reduction(_))
        ));
        assert_eq!(
            fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            before
        );
    }
}

/// 验证子 Agent 起点与无 Agent 身份的完整用户输入可原子提交，伪造归属则整体拒绝。
#[test]
fn child_turn_and_initial_user_input_are_atomic_and_agent_scoped() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("child-atomic-input").expect("Session ID 应有效");
    let journal =
        match SessionJournal::open(root.path(), session_id.clone(), JournalConfig::default())
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
        };
    create_session(&journal);
    let root_turn = TurnId::new("turn-root").expect("Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");
    let child_agent = AgentId::new("child-input").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: child_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child_input".to_owned(),
                task: "接收完整初始输入".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("子 Agent 应创建");

    let child_turn = TurnId::new("turn-child").expect("Turn ID 应有效");
    let before_lines = fs::read_to_string(journal.log_path())
        .expect("提交前日志应读取")
        .lines()
        .count();
    let expected_sequence = journal.state().expect("提交前状态应读取").last_sequence;
    let outcome = journal
        .append_batch_idempotent(
            SessionEventId::new("batch-child-input").expect("批次 ID 应有效"),
            expected_sequence,
            vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_agent.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn.clone()),
                    prompt_summary: "执行完整子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_agent.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
                SessionEvent::MessageAdded {
                    message: SessionMessage {
                        message_id: "message-child-input".to_owned(),
                        turn_id: Some(child_turn.clone()),
                        agent_id: None,
                        role: MessageRole::User,
                        content: vec![MessagePart::Text {
                            text: "请检查实现、运行测试并返回证据。".to_owned(),
                        }],
                    },
                },
            ],
        )
        .expect("子 Turn 与初始输入应原子提交");
    assert!(matches!(outcome, IdempotentAppendOutcome::Appended(_)));
    assert_eq!(
        fs::read_to_string(journal.log_path())
            .expect("提交后日志应读取")
            .lines()
            .count(),
        before_lines + 1
    );
    assert_eq!(
        journal
            .state()
            .expect("提交后状态应读取")
            .raw_transcript_messages()[0]
            .agent_id,
        None
    );

    let forged_agent = AgentId::new("child-input-forged").expect("子 Agent ID 应有效");
    journal
        .append(SessionEvent::SubAgentSpawned {
            agent: SubAgentState {
                agent_id: forged_agent.clone(),
                parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                agent_path: "/root/child_input_forged".to_owned(),
                task: "验证伪造输入归属".to_owned(),
                status: SubAgentStatus::Pending,
                current_turn_id: None,
                result_summary: None,
            },
        })
        .expect("伪造用例子 Agent 应创建");
    let forged_turn = TurnId::new("turn-child-forged").expect("Turn ID 应有效");
    let before_forged = fs::read(journal.log_path()).expect("伪造提交前日志应读取");
    let expected_sequence = journal.state().expect("伪造提交前状态应读取").last_sequence;
    let forged = journal.append_batch_idempotent(
        SessionEventId::new("batch-child-forged").expect("批次 ID 应有效"),
        expected_sequence,
        vec![
            SessionEvent::TurnStarted {
                turn_id: forged_turn.clone(),
                source_agent_id: forged_agent.clone(),
                root_turn_id: root_turn,
                parent_turn_id: Some(child_turn.clone()),
                prompt_summary: "伪造输入归属".to_owned(),
            },
            SessionEvent::SubAgentStatusChanged {
                agent_id: forged_agent,
                turn_id: Some(forged_turn.clone()),
                status: SubAgentStatus::Running,
                result_summary: None,
            },
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: "message-child-forged".to_owned(),
                    turn_id: Some(forged_turn.clone()),
                    agent_id: Some(AgentId::new("root").expect("根 Agent ID 应有效")),
                    role: MessageRole::User,
                    content: vec![MessagePart::Text {
                        text: "不得归属到其他 Agent".to_owned(),
                    }],
                },
            },
        ],
    );
    assert!(matches!(forged, Err(ResourceError::Reduction(_))));
    assert_eq!(
        fs::read(journal.log_path()).expect("伪造拒绝后日志应读取"),
        before_forged
    );
    assert!(
        !journal
            .state()
            .expect("伪造拒绝后状态应读取")
            .turns
            .contains_key(&forged_turn)
    );
    drop(journal);

    let replayed = match SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("合法子输入不应损坏：{:?}", report.issues),
    };
    let state = replayed.state().expect("重放状态应读取");
    assert!(state.turns.contains_key(&child_turn));
    assert_eq!(state.raw_transcript_messages().len(), 1);
    assert_eq!(state.raw_transcript_messages()[0].agent_id, None);
    assert_eq!(
        state
            .effective_transcript(&AgentId::new("root").expect("根 Agent ID 应有效"))
            .expect("根 Agent Transcript 应恢复")
            .len(),
        0
    );
    assert_eq!(
        state
            .effective_transcript(&AgentId::new("child-input").expect("子 Agent ID 应有效"))
            .expect("子 Agent Transcript 应恢复")
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["message-child-input"]
    );
}

/// 验证带 Turn 的无身份用户输入只进入所属 Agent，上下文隔离在实时状态和重放后完全一致。
#[test]
fn turn_scoped_user_inputs_are_isolated_live_and_after_replay() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("turn-input-scope").expect("Session ID 应有效");
    let journal =
        match SessionJournal::open(root.path(), session_id.clone(), JournalConfig::default())
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
        };
    create_session(&journal);
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let root_turn = TurnId::new("turn-root-scope").expect("根 Turn ID 应有效");
    journal
        .append(root_turn_started(&root_turn))
        .expect("根 Turn 应开始");
    journal
        .append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-root-input".to_owned(),
                turn_id: Some(root_turn.clone()),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "根任务输入".to_owned(),
                }],
            },
        })
        .expect("根 Turn 用户输入应记录");

    let child_one = AgentId::new("child-scope-one").expect("子 Agent ID 应有效");
    let child_two = AgentId::new("child-scope-two").expect("子 Agent ID 应有效");
    let child_one_turn = TurnId::new("turn-child-scope-one").expect("子 Turn ID 应有效");
    let child_two_turn = TurnId::new("turn-child-scope-two").expect("子 Turn ID 应有效");
    for (index, (agent_id, turn_id, message_id)) in [
        (&child_one, &child_one_turn, "message-child-one-input"),
        (&child_two, &child_two_turn, "message-child-two-input"),
    ]
    .into_iter()
    .enumerate()
    {
        journal
            .append(SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: agent_id.clone(),
                    parent_agent_id: root_agent.clone(),
                    agent_path: format!("/root/child_{index}"),
                    task: format!("隔离子任务 {index}"),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            })
            .expect("子 Agent 应创建");
        let expected_sequence = journal.state().expect("批次前状态应读取").last_sequence;
        journal
            .append_batch_idempotent(
                SessionEventId::new(format!("batch-child-scope-{index}")).expect("批次 ID 应有效"),
                expected_sequence,
                vec![
                    SessionEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                        source_agent_id: agent_id.clone(),
                        root_turn_id: root_turn.clone(),
                        parent_turn_id: Some(root_turn.clone()),
                        prompt_summary: format!("执行隔离子任务 {index}"),
                    },
                    SessionEvent::SubAgentStatusChanged {
                        agent_id: agent_id.clone(),
                        turn_id: Some(turn_id.clone()),
                        status: SubAgentStatus::Running,
                        result_summary: None,
                    },
                    SessionEvent::MessageAdded {
                        message: SessionMessage {
                            message_id: message_id.to_owned(),
                            turn_id: Some(turn_id.clone()),
                            agent_id: None,
                            role: MessageRole::User,
                            content: vec![MessagePart::Text {
                                text: format!("子任务完整输入 {index}"),
                            }],
                        },
                    },
                ],
            )
            .expect("子 Turn 和输入应原子提交");
    }
    journal
        .append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-session-shared".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "Session 级共享输入".to_owned(),
                }],
            },
        })
        .expect("Session 级用户输入应记录");

    let live = journal.state().expect("实时状态应读取");
    assert_eq!(
        effective_message_ids(&live, &root_agent),
        vec!["message-root-input", "message-session-shared"]
    );
    assert_eq!(
        effective_message_ids(&live, &child_one),
        vec!["message-child-one-input", "message-session-shared"]
    );
    assert_eq!(
        effective_message_ids(&live, &child_two),
        vec!["message-child-two-input", "message-session-shared"]
    );

    let mut tampered = serde_json::to_value(&live).expect("状态应编码");
    let child_message = tampered["transcript"]
        .as_array_mut()
        .expect("Transcript 应为数组")
        .iter_mut()
        .find(|record| record["payload"]["messageId"] == "message-child-one-input")
        .expect("子 Agent 输入应存在");
    child_message["payload"]["agentId"] = json!("root");
    let tampered: SessionState = serde_json::from_value(tampered).expect("篡改状态仍应可解析");
    assert!(matches!(
        tampered.validate_transcript_history(),
        Err(ResourceError::Reduction(_))
    ));

    drop(journal);
    let replayed = match SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("合法作用域不应损坏：{:?}", report.issues),
    };
    let replayed = replayed.state().expect("重放状态应读取");
    assert_eq!(
        effective_message_ids(&replayed, &root_agent),
        vec!["message-root-input", "message-session-shared"]
    );
    assert_eq!(
        effective_message_ids(&replayed, &child_one),
        vec!["message-child-one-input", "message-session-shared"]
    );
    assert_eq!(
        effective_message_ids(&replayed, &child_two),
        vec!["message-child-two-input", "message-session-shared"]
    );
}

/// 验证同一 Turn、Agent 与 Round 内的工具原始下标不能重复。
#[test]
fn tool_request_index_is_unique_within_round() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = journal(&root, "tool-request-index");
    create_session(&journal);
    let turn_id = TurnId::new("turn-tools").expect("Turn ID 应有效");
    journal
        .append(root_turn_started(&turn_id))
        .expect("Turn 应开始");
    journal
        .append(SessionEvent::ToolRequested {
            request: tool_request(&journal, &turn_id, "call-one", 0),
        })
        .expect("首个工具请求应记录");
    let before = fs::read(journal.log_path()).expect("日志应读取");
    let duplicate = journal.append(SessionEvent::ToolRequested {
        request: tool_request(&journal, &turn_id, "call-two", 0),
    });
    assert!(matches!(duplicate, Err(ResourceError::Reduction(_))));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
}

/// 验证越过执行起点后崩溃的工具只能由显式副作用未知终态收敛。
#[test]
fn started_tool_recovers_to_side_effect_unknown() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("tool-unknown").expect("Session ID 应有效");
    let journal =
        match SessionJournal::open(root.path(), session_id.clone(), JournalConfig::default())
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
        };
    create_session(&journal);
    let turn_id = TurnId::new("turn-tool").expect("Turn ID 应有效");
    journal
        .append(root_turn_started(&turn_id))
        .expect("Turn 应开始");
    let mut request = tool_request(&journal, &turn_id, "call-write", 0);
    request.effect = ToolEffect::ChangesState;
    let request_id = request.request_id.clone();
    let recovery_result = side_effect_unknown_result("call-write");
    journal
        .append(SessionEvent::ToolRequested { request })
        .expect("工具请求应记录");
    assert!(matches!(
        journal.append(SessionEvent::ToolSideEffectUnknown {
            request_id: request_id.clone(),
            result: recovery_result.clone(),
        }),
        Err(ResourceError::Reduction(_))
    ));
    journal
        .append(SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        })
        .expect("工具执行起点应记录");
    let invalid_results = [
        PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: Vec::new(),
            is_error: true,
        },
        PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: vec![ToolResultPart::Text {
                text: String::new(),
            }],
            is_error: true,
        },
        PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: vec![ToolResultPart::Text {
                text: "可以安全自动重试".to_owned(),
            }],
            is_error: true,
        },
        PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: vec![ToolResultPart::Image {
                source: MessageImageSource::Url {
                    url: "https://example.invalid/recovery.png".to_owned(),
                },
            }],
            is_error: true,
        },
        PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: recovery_result.content.clone(),
            is_error: false,
        },
        PersistedToolResult {
            tool_call_id: "other-call".to_owned(),
            content: recovery_result.content.clone(),
            is_error: true,
        },
    ];
    let before_invalid = fs::read(journal.log_path()).expect("错误恢复结果前日志应读取");
    for result in invalid_results {
        assert!(matches!(
            journal.append(SessionEvent::ToolSideEffectUnknown {
                request_id: request_id.clone(),
                result,
            }),
            Err(ResourceError::Reduction(_))
        ));
        assert_eq!(
            fs::read(journal.log_path()).expect("错误恢复结果后日志应读取"),
            before_invalid
        );
    }
    drop(journal);

    let replayed = match SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("执行起点不应损坏：{:?}", report.issues),
    };
    let recovered = replayed.state().expect("状态应读取");
    let lifecycle = recovered.tools.get(&request_id).expect("工具应恢复");
    assert!(lifecycle.execution_started);
    assert!(lifecycle.outcome.is_none());

    assert!(matches!(
        replayed.append(SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: keencode_resources::ToolOutcome {
                status: ToolCompletionStatus::SideEffectUnknown,
                result: recovery_result.clone(),
            },
        }),
        Err(ResourceError::Reduction(_))
    ));
    replayed
        .append(SessionEvent::ToolSideEffectUnknown {
            request_id: request_id.clone(),
            result: recovery_result,
        })
        .expect("显式副作用未知结果应收敛工具");
    assert_eq!(
        replayed
            .state()
            .expect("状态应读取")
            .tools
            .get(&request_id)
            .and_then(|tool| tool.outcome.as_ref())
            .map(|outcome| outcome.status),
        Some(ToolCompletionStatus::SideEffectUnknown)
    );
    assert_eq!(
        replayed
            .state()
            .expect("状态应读取")
            .tools
            .get(&request_id)
            .and_then(|tool| tool.outcome.as_ref())
            .map(|outcome| &outcome.result),
        Some(&side_effect_unknown_result("call-write"))
    );
}
