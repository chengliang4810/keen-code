use keencode_resources::{
    COMMAND_RECEIPT_SCHEMA, CommandReceipt, CommandReceiptStatus, SESSION_EVENT_SCHEMA,
    SESSION_EVENT_VERSION, SessionEvent, SessionEventId, SessionEventRecord, SessionId,
    SessionState, reduce_record,
};

fn record(sequence: u64, event_id: &str, event: SessionEvent) -> SessionEventRecord {
    SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new(event_id).expect("测试事件 ID 应有效"),
        session: SessionId::new("command-receipt-resource-test").expect("测试 Session ID 应有效"),
        sequence,
        time_unix_ms: sequence,
        event,
    }
}

fn admitted(command_id: &str, digest: &str) -> CommandReceipt {
    CommandReceipt {
        schema: COMMAND_RECEIPT_SCHEMA.to_owned(),
        scope: "session:command-receipt-resource-test".to_owned(),
        command_id: command_id.to_owned(),
        command_type: "sendText".to_owned(),
        payload_sha256: digest.to_owned(),
        status: CommandReceiptStatus::Admitted,
    }
}

fn created() -> SessionEvent {
    SessionEvent::SessionCreated {
        title: "命令收据测试".to_owned(),
        project_root: "C:/command-receipt-resource-test".to_owned(),
    }
}

#[test]
fn receipt_state_machine_replays_terminal_and_rejects_regression() {
    let mut state = SessionState::empty(
        SessionId::new("command-receipt-resource-test").expect("测试 Session ID 应有效"),
    );
    reduce_record(&mut state, record(1, "session-created", created())).expect("Session 应创建");
    let first = admitted("command-1", &"a".repeat(64));
    reduce_record(
        &mut state,
        record(
            2,
            "receipt-admitted",
            SessionEvent::CommandReceiptCommitted {
                receipt: first.clone(),
            },
        ),
    )
    .expect("admission 应归约");
    let completed = CommandReceipt {
        status: CommandReceiptStatus::Completed {
            ack: serde_json::json!({
                "commandId": "command-1",
                "status": "accepted",
                "revisionAtDecision": 1
            }),
        },
        ..first.clone()
    };
    reduce_record(
        &mut state,
        record(
            3,
            "receipt-completed",
            SessionEvent::CommandReceiptCommitted {
                receipt: completed.clone(),
            },
        ),
    )
    .expect("completed 应归约");
    assert_eq!(state.command_receipts.values().next(), Some(&completed));

    let regression = reduce_record(
        &mut state,
        record(
            4,
            "receipt-regression",
            SessionEvent::CommandReceiptCommitted { receipt: first },
        ),
    );
    assert!(regression.is_err(), "终态不能回退为 admitted");
}

#[test]
fn receipt_payload_conflict_and_invalid_terminal_first_are_rejected() {
    let mut state = SessionState::empty(
        SessionId::new("command-receipt-resource-test").expect("测试 Session ID 应有效"),
    );
    reduce_record(&mut state, record(1, "session-created", created())).expect("Session 应创建");
    let invalid = CommandReceipt {
        status: CommandReceiptStatus::Completed {
            ack: serde_json::json!({"status": "accepted"}),
        },
        ..admitted("command-invalid", &"b".repeat(64))
    };
    assert!(
        reduce_record(
            &mut state,
            record(
                2,
                "receipt-invalid",
                SessionEvent::CommandReceiptCommitted { receipt: invalid },
            ),
        )
        .is_err()
    );

    let first = admitted("command-conflict", &"c".repeat(64));
    reduce_record(
        &mut state,
        record(
            2,
            "receipt-conflict-first",
            SessionEvent::CommandReceiptCommitted {
                receipt: first.clone(),
            },
        ),
    )
    .expect("首次 admission 应归约");
    let conflict = CommandReceipt {
        payload_sha256: "d".repeat(64),
        ..first
    };
    assert!(
        reduce_record(
            &mut state,
            record(
                3,
                "receipt-conflict-second",
                SessionEvent::CommandReceiptCommitted { receipt: conflict },
            ),
        )
        .is_err(),
        "同 commandId 绑定不同 payload 必须冲突"
    );
}
