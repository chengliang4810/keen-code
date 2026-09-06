mod support;

use std::fs;

use keencode_resources::{
    AgentId, IdempotentAppendOutcome, JournalConfig, RequestId, ResourceError,
    SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION, SessionEvent, SessionEventId, SessionEventRecord,
    SessionId, SessionJournal, SessionOpen, SessionState, SubAgentState, SubAgentStatus,
    ToolEffect, ToolRequest, TurnId, TurnStatus, TurnStopReason, reduce_record,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use support::TestJournalAppend;

/// 打开一个可写测试 Session，并拒绝意外的损坏报告。
fn open_journal(root: &TempDir, session: &str) -> SessionJournal {
    match SessionJournal::open(
        root.path(),
        SessionId::new(session).expect("Session ID 应有效"),
        JournalConfig::default(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    }
}

/// 创建测试 Session，并开始一个根 Agent Turn。
fn start_root_turn(journal: &SessionJournal, turn_name: &str) -> TurnId {
    if !journal.state().expect("状态应读取").created {
        journal
            .append(SessionEvent::SessionCreated {
                title: "停止原因测试".to_owned(),
                project_root: "D:/workspace".to_owned(),
            })
            .expect("Session 应创建");
    }
    let turn_id = TurnId::new(turn_name).expect("Turn ID 应有效");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证结构化停止原因".to_owned(),
        })
        .expect("根 Turn 应开始");
    turn_id
}

/// 返回停止原因应推导的粗粒度 Turn 状态。
fn expected_turn_status(reason: TurnStopReason) -> TurnStatus {
    match reason {
        TurnStopReason::Cancelled => TurnStatus::Cancelled,
        TurnStopReason::Failed
        | TurnStopReason::LimitReached
        | TurnStopReason::ContextBlocked
        | TurnStopReason::ModelOutputLimit
        | TurnStopReason::ModelRefusal => TurnStatus::Failed,
    }
}

/// 构造一条用于验证外部反序列化状态的下一序号事件。
fn next_record(state: &SessionState, event_id: &str) -> SessionEventRecord {
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
        event: SessionEvent::SessionRenamed {
            title: "状态摘要验证".to_owned(),
        },
    }
}

/// 递归按对象键排序，复现生产 Snapshot 状态摘要的规范 JSON 规则。
fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_json(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value,
    }
}

/// 计算测试夹具状态的规范 JSON SHA-256。
fn canonical_state_sha256(state: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&canonicalize_json(state.clone())).expect("规范状态应编码")
        )
    )
}

/// 验证所有停止原因均由 Journal 无损重放，且事件不再保存冗余 status。
#[test]
fn all_stop_reasons_round_trip_through_journal() {
    assert_eq!(SESSION_EVENT_VERSION, 7);
    let root = TempDir::new().expect("临时目录应创建");
    for (index, reason) in [
        TurnStopReason::Cancelled,
        TurnStopReason::Failed,
        TurnStopReason::LimitReached,
        TurnStopReason::ContextBlocked,
        TurnStopReason::ModelOutputLimit,
        TurnStopReason::ModelRefusal,
    ]
    .into_iter()
    .enumerate()
    {
        let session_name = format!("stop-replay-{index}");
        let journal = open_journal(&root, &session_name);
        let turn_id = start_root_turn(&journal, &format!("turn-stop-{index}"));
        journal
            .append(SessionEvent::TurnStopped {
                turn_id: turn_id.clone(),
                reason,
                message: format!("停止原因 {index}"),
            })
            .expect("Turn 应停止");
        let live = journal.state().expect("实时状态应读取");
        let live_turn = live.turns.get(&turn_id).expect("实时 Turn 应存在");
        assert_eq!(live_turn.status, expected_turn_status(reason));
        assert_eq!(live_turn.stop_reason, Some(reason));
        assert_eq!(
            live_turn.outcome_message.as_deref(),
            Some(format!("停止原因 {index}").as_str())
        );

        let terminal_event: Value = fs::read_to_string(journal.log_path())
            .expect("日志应读取")
            .lines()
            .last()
            .map(|line| serde_json::from_str(line).expect("终态事件应为 JSON"))
            .expect("终态事件应存在");
        assert_eq!(terminal_event["version"], json!(SESSION_EVENT_VERSION));
        assert!(terminal_event["payload"].get("reason").is_some());
        assert!(terminal_event["payload"].get("status").is_none());
        drop(journal);

        let replayed = open_journal(&root, &session_name);
        let replayed_state = replayed.state().expect("重放状态应读取");
        let replayed_turn = replayed_state
            .turns
            .get(&turn_id)
            .expect("重放 Turn 应存在");
        assert_eq!(replayed_turn, live_turn);
    }
}

/// 验证空或仅空白的停止说明在写入日志前被拒绝。
#[test]
fn empty_or_blank_stop_message_is_rejected_transactionally() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-empty-message");
    let turn_id = start_root_turn(&journal, "turn-empty-message");
    let baseline_state = journal.state().expect("基线状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("基线日志应读取");

    for message in ["", " \t\r\n "] {
        assert!(matches!(
            journal.append(SessionEvent::TurnStopped {
                turn_id: turn_id.clone(),
                reason: TurnStopReason::Failed,
                message: message.to_owned(),
            }),
            Err(ResourceError::Reduction(_))
        ));
        assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
        assert_eq!(
            fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            baseline_log
        );
    }
}

/// 验证同一 Turn 不能写入第二个终态。
#[test]
fn repeated_terminal_event_is_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-repeat");
    let turn_id = start_root_turn(&journal, "turn-repeat");
    journal
        .append(SessionEvent::TurnStopped {
            turn_id: turn_id.clone(),
            reason: TurnStopReason::LimitReached,
            message: "达到轮次上限".to_owned(),
        })
        .expect("首个终态应接受");
    let baseline_state = journal.state().expect("终态状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("终态日志应读取");
    assert!(matches!(
        journal.append(SessionEvent::TurnStopped {
            turn_id,
            reason: TurnStopReason::ContextBlocked,
            message: "不得覆盖终态".to_owned(),
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline_state);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_log
    );
}

/// 验证公开归约器拒绝 Running、Completed 与停止状态中的伪造停止字段。
#[test]
fn public_reducer_rejects_inconsistent_turn_stop_fields() {
    let root = TempDir::new().expect("临时目录应创建");

    let running_journal = open_journal(&root, "stop-forged-running-state");
    let running_turn = start_root_turn(&running_journal, "turn-forged-running-state");
    let mut forged_running = running_journal.state().expect("Running 状态应读取");
    forged_running
        .turns
        .get_mut(&running_turn)
        .expect("Running Turn 应存在")
        .stop_reason = Some(TurnStopReason::Failed);
    let running_before = forged_running.clone();
    let running_record = next_record(&forged_running, "event-after-forged-running");
    assert!(reduce_record(&mut forged_running, running_record).is_err());
    assert_eq!(forged_running, running_before);

    let completed_journal = open_journal(&root, "stop-forged-completed-state");
    let completed_turn = start_root_turn(&completed_journal, "turn-forged-completed-state");
    completed_journal
        .append(SessionEvent::TurnCompleted {
            turn_id: completed_turn.clone(),
        })
        .expect("Turn 应完成");
    let mut forged_completed = completed_journal.state().expect("Completed 状态应读取");
    let completed = forged_completed
        .turns
        .get_mut(&completed_turn)
        .expect("Completed Turn 应存在");
    completed.stop_reason = Some(TurnStopReason::Cancelled);
    completed.outcome_message = Some("伪造停止说明".to_owned());
    let completed_before = forged_completed.clone();
    let completed_record = next_record(&forged_completed, "event-after-forged-completed");
    assert!(reduce_record(&mut forged_completed, completed_record).is_err());
    assert_eq!(forged_completed, completed_before);

    let stopped_journal = open_journal(&root, "stop-forged-stopped-state");
    let stopped_turn = start_root_turn(&stopped_journal, "turn-forged-stopped-state");
    stopped_journal
        .append(SessionEvent::TurnStopped {
            turn_id: stopped_turn.clone(),
            reason: TurnStopReason::ContextBlocked,
            message: "上下文阻塞".to_owned(),
        })
        .expect("Turn 应停止");
    let mut forged_stopped = stopped_journal.state().expect("停止状态应读取");
    forged_stopped
        .turns
        .get_mut(&stopped_turn)
        .expect("停止 Turn 应存在")
        .stop_reason = Some(TurnStopReason::Cancelled);
    let stopped_before = forged_stopped.clone();
    let stopped_record = next_record(&forged_stopped, "event-after-forged-stopped");
    assert!(reduce_record(&mut forged_stopped, stopped_record).is_err());
    assert_eq!(forged_stopped, stopped_before);
}

/// 验证仍有未收敛工具时，任何停止原因都不能提前结束 Turn。
#[test]
fn stop_reason_cannot_bypass_unconverged_tool_resources() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-open-tool");
    let turn_id = start_root_turn(&journal, "turn-open-tool");
    let agent_id = AgentId::new("root").expect("根 Agent ID 应有效");
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        &turn_id,
        &agent_id,
        1,
        "call-open-tool",
    )
    .expect("Request ID 应派生");
    journal
        .append(SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id,
                turn_id: turn_id.clone(),
                agent_id,
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-open-tool".to_owned(),
                tool_name: "read".to_owned(),
                arguments: json!({"path": "src/lib.rs"}),
                effect: ToolEffect::ReadOnly,
            },
        })
        .expect("工具请求应记录");
    let baseline = journal.state().expect("工具状态应读取");
    assert!(matches!(
        journal.append(SessionEvent::TurnStopped {
            turn_id,
            reason: TurnStopReason::Cancelled,
            message: "工具尚未收敛".to_owned(),
        }),
        Err(ResourceError::Reduction(_))
    ));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline);
}

/// 验证子 Agent 终态必须同时匹配 Agent、Turn 与停止原因矩阵。
#[test]
fn child_terminal_pairing_rejects_cross_agent_and_wrong_reason_status() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-child-cross-pair");
    let root_turn = start_root_turn(&journal, "turn-root-parent");
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let child_a = AgentId::new("child-a").expect("子 Agent ID 应有效");
    let child_b = AgentId::new("child-b").expect("子 Agent ID 应有效");
    for child in [&child_a, &child_b] {
        journal
            .append(SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: child.clone(),
                    parent_agent_id: root_agent.clone(),
                    agent_path: format!("/root/{}", child.as_str().replace('-', "_")),
                    task: "验证原子终态配对".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            })
            .expect("子 Agent 应创建");
    }
    let child_turn = TurnId::new("turn-child-a").expect("子 Turn ID 应有效");
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn.clone(),
                    source_agent_id: child_a.clone(),
                    root_turn_id: root_turn.clone(),
                    parent_turn_id: Some(root_turn),
                    prompt_summary: "运行子任务".to_owned(),
                },
                SessionEvent::SubAgentStatusChanged {
                    agent_id: child_a.clone(),
                    turn_id: Some(child_turn.clone()),
                    status: SubAgentStatus::Running,
                    result_summary: None,
                },
            ],
        })
        .expect("子 Turn 应原子开始");
    let baseline = journal.state().expect("子 Turn 状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("子 Turn 日志应读取");

    for events in [
        vec![
            SessionEvent::TurnStopped {
                turn_id: child_turn.clone(),
                reason: TurnStopReason::Cancelled,
                message: "取消子任务".to_owned(),
            },
            SessionEvent::SubAgentStatusChanged {
                agent_id: child_b.clone(),
                turn_id: Some(child_turn.clone()),
                status: SubAgentStatus::Interrupted,
                result_summary: None,
            },
        ],
        vec![
            SessionEvent::TurnStopped {
                turn_id: child_turn.clone(),
                reason: TurnStopReason::ContextBlocked,
                message: "上下文阻塞".to_owned(),
            },
            SessionEvent::SubAgentStatusChanged {
                agent_id: child_a.clone(),
                turn_id: Some(child_turn.clone()),
                status: SubAgentStatus::Interrupted,
                result_summary: None,
            },
        ],
    ] {
        assert!(matches!(
            journal.append(SessionEvent::AtomicBatch { events }),
            Err(ResourceError::Reduction(_))
        ));
        assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline);
        assert_eq!(
            fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            baseline_log
        );
    }
}

/// 验证所有停止原因分别要求 Interrupted 或 Failed 子 Agent 终态。
#[test]
fn every_stop_reason_matches_child_agent_terminal_matrix() {
    let root = TempDir::new().expect("临时目录应创建");
    for (index, reason) in [
        TurnStopReason::Cancelled,
        TurnStopReason::Failed,
        TurnStopReason::LimitReached,
        TurnStopReason::ContextBlocked,
        TurnStopReason::ModelOutputLimit,
        TurnStopReason::ModelRefusal,
    ]
    .into_iter()
    .enumerate()
    {
        let session_name = format!("stop-child-matrix-{index}");
        let journal = open_journal(&root, &session_name);
        let root_turn = start_root_turn(&journal, &format!("turn-root-matrix-{index}"));
        let child = AgentId::new(format!("child-matrix-{index}")).expect("子 Agent ID 应有效");
        journal
            .append(SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: child.clone(),
                    parent_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
                    agent_path: format!("/root/child_matrix_{index}"),
                    task: "验证停止矩阵".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            })
            .expect("子 Agent 应创建");
        let child_turn =
            TurnId::new(format!("turn-child-matrix-{index}")).expect("子 Turn ID 应有效");
        journal
            .append(SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnStarted {
                        turn_id: child_turn.clone(),
                        source_agent_id: child.clone(),
                        root_turn_id: root_turn.clone(),
                        parent_turn_id: Some(root_turn),
                        prompt_summary: "运行矩阵子任务".to_owned(),
                    },
                    SessionEvent::SubAgentStatusChanged {
                        agent_id: child.clone(),
                        turn_id: Some(child_turn.clone()),
                        status: SubAgentStatus::Running,
                        result_summary: None,
                    },
                ],
            })
            .expect("子 Turn 应开始");
        let (status, result_summary) = if reason == TurnStopReason::Cancelled {
            (SubAgentStatus::Interrupted, None)
        } else {
            (SubAgentStatus::Failed, Some("子任务停止".to_owned()))
        };
        journal
            .append(SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnStopped {
                        turn_id: child_turn.clone(),
                        reason,
                        message: "子 Turn 停止".to_owned(),
                    },
                    SessionEvent::SubAgentStatusChanged {
                        agent_id: child.clone(),
                        turn_id: Some(child_turn.clone()),
                        status: status.clone(),
                        result_summary,
                    },
                ],
            })
            .expect("子 Turn 终态应符合停止原因矩阵");
        let state = journal.state().expect("子终态应读取");
        assert_eq!(state.turns[&child_turn].stop_reason, Some(reason));
        assert_eq!(state.sub_agents[&child].status, status);
    }
}

/// 验证同一事件标识不能通过改写停止原因绕过幂等正文绑定。
#[test]
fn same_event_id_with_changed_reason_is_conflict() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-event-id-conflict");
    let turn_id = start_root_turn(&journal, "turn-event-id-conflict");
    let expected_sequence = journal.state().expect("状态应读取").last_sequence;
    let event_id = SessionEventId::new("event-stop-conflict").expect("事件 ID 应有效");
    let first = journal
        .append_idempotent(
            event_id.clone(),
            expected_sequence,
            SessionEvent::TurnStopped {
                turn_id: turn_id.clone(),
                reason: TurnStopReason::Failed,
                message: "执行失败".to_owned(),
            },
        )
        .expect("首个终态应提交");
    assert!(matches!(first, IdempotentAppendOutcome::Appended(_)));
    let baseline = fs::read(journal.log_path()).expect("首个终态日志应读取");
    let conflict = journal
        .append_idempotent(
            event_id,
            expected_sequence,
            SessionEvent::TurnStopped {
                turn_id,
                reason: TurnStopReason::LimitReached,
                message: "执行失败".to_owned(),
            },
        )
        .expect("正文冲突应作为幂等结果返回");
    assert!(matches!(
        conflict,
        IdempotentAppendOutcome::EventIdConflict { .. }
    ));
    assert_eq!(
        fs::read(journal.log_path()).expect("冲突后日志应读取"),
        baseline
    );
}

/// 验证自 Hash 正确但伪造 stopReason 的 v6 Snapshot 仍由权威 Journal 重建。
#[test]
fn forged_snapshot_stop_reason_is_rebuilt_from_journal() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "stop-snapshot-forgery");
    let turn_id = start_root_turn(&journal, "turn-snapshot-forgery");
    journal
        .append(SessionEvent::TurnStopped {
            turn_id: turn_id.clone(),
            reason: TurnStopReason::LimitReached,
            message: "达到限制".to_owned(),
        })
        .expect("Turn 应停止");
    journal.write_snapshot().expect("Snapshot 应写入");
    let snapshot_path = journal.snapshot_path().to_owned();
    drop(journal);

    let mut snapshot: Value =
        serde_json::from_slice(&fs::read(&snapshot_path).expect("Snapshot 应读取"))
            .expect("Snapshot 应为 JSON");
    snapshot["state"]["turns"][turn_id.as_str()]["stopReason"] = json!("context_blocked");
    snapshot["stateSha256"] = json!(canonical_state_sha256(&snapshot["state"]));
    let mut encoded = serde_json::to_vec_pretty(&snapshot).expect("伪造 Snapshot 应编码");
    encoded.push(b'\n');
    fs::write(&snapshot_path, encoded).expect("伪造 Snapshot 应写入");

    let reopened = open_journal(&root, "stop-snapshot-forgery");
    let state = reopened.state().expect("权威状态应恢复");
    assert_eq!(
        state.turns[&turn_id].stop_reason,
        Some(TurnStopReason::LimitReached)
    );
    drop(reopened);
    let rebuilt: Value =
        serde_json::from_slice(&fs::read(snapshot_path).expect("重建 Snapshot 应读取"))
            .expect("重建 Snapshot 应为 JSON");
    assert_eq!(
        rebuilt["state"]["turns"][turn_id.as_str()]["stopReason"],
        json!("limit_reached")
    );
}
