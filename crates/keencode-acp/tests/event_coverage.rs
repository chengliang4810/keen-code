use keencode_acp::{
    AcpBoundaryError, AgentLifecycleStatus, BackgroundTaskKind, BackgroundTaskTerminalStatus,
    CompactionFailureKind, KeenCodeEvent, KeenCodeEventEnvelope, KeenCodeEventEnvelopeParams,
    McpOAuthEvent, McpOAuthNotification, RecoveryState, SystemNotificationLevel, TurnFailureKind,
};
use serde_json::{Value, json};

/// 构造一个带合法 Session 身份的扩展事件信封。
fn session_event(event: KeenCodeEvent) -> Result<KeenCodeEventEnvelope, AcpBoundaryError> {
    let authoritative = event.is_authoritative();
    let params = KeenCodeEventEnvelopeParams::for_session("session-a", 3, 1_700_000_000_000, event);
    if authoritative {
        KeenCodeEventEnvelope::new_authoritative(7, params)
    } else {
        KeenCodeEventEnvelope::new_transient(params)
    }
}

/// 构造一个带合法根 Turn 身份的扩展事件信封。
fn turn_event(event: KeenCodeEvent) -> Result<KeenCodeEventEnvelope, AcpBoundaryError> {
    let authoritative = event.is_authoritative();
    let params = KeenCodeEventEnvelopeParams::for_turn(
        "session-a",
        "turn-a",
        "agent-root",
        3,
        1_700_000_000_000,
        event,
    );
    if authoritative {
        KeenCodeEventEnvelope::new_authoritative(7, params)
    } else {
        KeenCodeEventEnvelope::new_transient(params)
    }
}

/// 验证前端所需事件使用 snake_case 类型和 camelCase 字段，并可严格往返。
#[test]
fn frontend_events_use_stable_typed_wire_shapes() {
    let cases = [
        (
            turn_event(KeenCodeEvent::SystemNotification {
                level: SystemNotificationLevel::Warning,
                message: "MCP 连接已中断".to_owned(),
            })
            .expect("系统通知事件应有效"),
            json!({
                "type": "system_notification",
                "level": "warning",
                "message": "MCP 连接已中断"
            }),
        ),
        (
            turn_event(KeenCodeEvent::ModelRetryScheduled {
                attempt: 2,
                max_attempts: 4,
                delay_ms: 800,
                message: "模型服务暂时不可用".to_owned(),
            })
            .expect("模型重试事件应有效"),
            json!({
                "type": "model_retry_scheduled",
                "attempt": 2,
                "maxAttempts": 4,
                "delayMs": 800,
                "message": "模型服务暂时不可用"
            }),
        ),
        (
            session_event(KeenCodeEvent::BackgroundTaskCompleted {
                task_id: "task-a".to_owned(),
                task_kind: BackgroundTaskKind::Shell,
                agent_id: None,
                status: BackgroundTaskTerminalStatus::Succeeded,
                duration_ms: 1_250,
                summary: Some("后台命令执行完成".to_owned()),
            })
            .expect("后台任务完成事件应有效"),
            json!({
                "type": "background_task_completed",
                "taskId": "task-a",
                "taskKind": "shell",
                "status": "succeeded",
                "durationMs": 1_250,
                "summary": "后台命令执行完成"
            }),
        ),
        (
            session_event(KeenCodeEvent::BackgroundTaskCompleted {
                task_id: "task-c".to_owned(),
                task_kind: BackgroundTaskKind::Shell,
                agent_id: None,
                status: BackgroundTaskTerminalStatus::Cancelled,
                duration_ms: 0,
                summary: None,
            })
            .expect("缺少展示摘要的后台任务完成事件仍应有效"),
            json!({
                "type": "background_task_completed",
                "taskId": "task-c",
                "taskKind": "shell",
                "status": "cancelled",
                "durationMs": 0
            }),
        ),
        (
            session_event(KeenCodeEvent::BackgroundTaskCompleted {
                task_id: "task-b".to_owned(),
                task_kind: BackgroundTaskKind::Agent,
                agent_id: Some("agent-child".to_owned()),
                status: BackgroundTaskTerminalStatus::Failed,
                duration_ms: 2_500,
                summary: Some("子 Agent 未能完成任务".to_owned()),
            })
            .expect("后台 Agent 完成事件应有效"),
            json!({
                "type": "background_task_completed",
                "taskId": "task-b",
                "taskKind": "agent",
                "agentId": "agent-child",
                "status": "failed",
                "durationMs": 2_500,
                "summary": "子 Agent 未能完成任务"
            }),
        ),
        (
            turn_event(KeenCodeEvent::ModelFirstStreamObserved).expect("模型首流事件应有效"),
            json!({ "type": "model_first_stream_observed" }),
        ),
    ];

    for (envelope, expected_event) in cases {
        let value = serde_json::to_value(&envelope).expect("事件信封应序列化");
        assert_eq!(value["event"], expected_event);
        if envelope.event().is_authoritative() {
            assert!(value.get("journalSequence").is_some());
            assert!(envelope.journal_sequence().is_some());
        } else {
            assert!(value.get("journalSequence").is_none());
            assert_eq!(envelope.journal_sequence(), None);
        }
        let raw = serde_json::to_vec(&value).expect("事件信封应编码为 JSON");
        assert_eq!(
            KeenCodeEventEnvelope::decode_raw(&raw).expect("事件信封应严格恢复"),
            envelope
        );
    }
}

/// 验证 OAuth 事件使用独立 JSON-RPC 通知，不再混入 Session 事件信封。
#[test]
fn oauth_events_use_strict_project_scoped_notifications() {
    let cases = [
        (
            McpOAuthEvent::AuthorizationRequired {
                project_path: "D:/projects/one".to_owned(),
                server_name: "docs".to_owned(),
                authorization_url: "https://auth.example/authorize?state=opaque".to_owned(),
            },
            json!({
                "type": "mcp_oauth_authorization_required",
                "projectPath": "D:/projects/one",
                "serverName": "docs",
                "authorizationUrl": "https://auth.example/authorize?state=opaque"
            }),
        ),
        (
            McpOAuthEvent::Authorized {
                project_path: "D:/projects/one".to_owned(),
                server_name: "docs".to_owned(),
            },
            json!({
                "type": "mcp_oauth_authorized",
                "projectPath": "D:/projects/one",
                "serverName": "docs"
            }),
        ),
        (
            McpOAuthEvent::Failed {
                project_path: "D:/projects/two".to_owned(),
                server_name: "docs".to_owned(),
                message: "授权被拒绝".to_owned(),
            },
            json!({
                "type": "mcp_oauth_failed",
                "projectPath": "D:/projects/two",
                "serverName": "docs",
                "message": "授权被拒绝"
            }),
        ),
    ];

    for (event, expected_params) in cases {
        let notification = McpOAuthNotification::new(event.clone()).expect("OAuth 事件应有效");
        let value = serde_json::to_value(&notification).expect("OAuth 通知应序列化");
        assert_eq!(
            value,
            json!({
                "jsonrpc": "2.0",
                "method": "keencode/mcp/oauth",
                "params": expected_params
            })
        );
        let raw = notification.encode().expect("OAuth 通知应编码");
        assert_eq!(
            McpOAuthNotification::decode_raw(&raw).expect("OAuth 通知应严格恢复"),
            notification
        );
        assert_eq!(notification.params(), &event);
    }

    let one = McpOAuthEvent::Authorized {
        project_path: "D:/projects/one".to_owned(),
        server_name: "docs".to_owned(),
    };
    let two = McpOAuthEvent::Authorized {
        project_path: "D:/projects/two".to_owned(),
        server_name: "docs".to_owned(),
    };
    assert_ne!(
        McpOAuthNotification::new(one).expect("项目一通知应有效"),
        McpOAuthNotification::new(two).expect("项目二通知应有效")
    );
}

/// 旧 Session 事件信封中的 OAuth 变体必须不再被恢复入口接受。
#[test]
fn oauth_session_event_envelopes_are_rejected_after_notification_split() {
    for event in [
        json!({
            "type": "mcp_oauth_authorization_required",
            "projectPath": "D:/projects/active",
            "serverName": "docs",
            "authorizationUrl": "https://auth.example/authorize"
        }),
        json!({
            "type": "mcp_oauth_authorized",
            "projectPath": "D:/projects/active",
            "serverName": "docs"
        }),
        json!({
            "type": "mcp_oauth_failed",
            "projectPath": "D:/projects/active",
            "serverName": "docs",
            "message": "授权失败"
        }),
    ] {
        let raw = serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "sessionId": "session-a",
            "deliverySequence": 1,
            "occurredAtMs": 1,
            "event": event
        }))
        .expect("旧事件信封应可构造测试输入");
        assert!(KeenCodeEventEnvelope::decode_raw(&raw).is_err());
    }
}

/// 锁定每种事件是否属于权威 Journal，避免新增事件时悄悄伪造序号。
#[test]
fn event_authority_classification_is_closed() {
    let authoritative = [
        KeenCodeEvent::TurnStarted {
            root_turn_id: "turn-root".to_owned(),
            parent_turn_id: None,
        },
        KeenCodeEvent::TurnCompleted,
        KeenCodeEvent::TurnCancelled,
        KeenCodeEvent::TurnFailed {
            failure_kind: TurnFailureKind::Internal,
            message: "内部失败".to_owned(),
        },
        KeenCodeEvent::AgentSpawned {
            agent_id: "agent-child".to_owned(),
            parent_agent_id: "agent-root".to_owned(),
            agent_path: "root/child".to_owned(),
            task: "检查子任务".to_owned(),
            parent_turn_id: "turn-root".to_owned(),
            root_turn_id: "turn-root".to_owned(),
        },
        KeenCodeEvent::AgentStatusChanged {
            agent_id: "agent-root".to_owned(),
            status: AgentLifecycleStatus::Running,
        },
        KeenCodeEvent::AgentMessageQueued {
            message_id: "message-a".to_owned(),
            from_agent_id: "agent-root".to_owned(),
            to_agent_id: "agent-child".to_owned(),
        },
        KeenCodeEvent::ContextCompactionCompleted {
            replaced_through_sequence: 1,
            estimated_tokens: 10,
        },
    ];
    assert!(authoritative.iter().all(KeenCodeEvent::is_authoritative));
    assert!(authoritative.iter().all(|event| !event.is_transient()));

    let transient = [
        KeenCodeEvent::ContextCompactionStarted {
            estimated_tokens: 10,
        },
        KeenCodeEvent::ContextCompactionFailed {
            failure_kind: CompactionFailureKind::Model,
        },
        KeenCodeEvent::RecoveryStateChanged {
            state: RecoveryState::Ready,
        },
        KeenCodeEvent::GoalChanged {
            goal_id: Some("goal-a".to_owned()),
            revision: 1,
            status: Some("active".to_owned()),
        },
        KeenCodeEvent::SystemNotification {
            level: SystemNotificationLevel::Info,
            message: "通知".to_owned(),
        },
        KeenCodeEvent::ModelRetryScheduled {
            attempt: 1,
            max_attempts: 2,
            delay_ms: 1,
            message: "稍后重试".to_owned(),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Succeeded,
            duration_ms: 1,
            summary: None,
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-agent".to_owned(),
            task_kind: BackgroundTaskKind::Agent,
            agent_id: Some("agent-child".to_owned()),
            status: BackgroundTaskTerminalStatus::Succeeded,
            duration_ms: 1,
            summary: None,
        },
        KeenCodeEvent::ModelFirstStreamObserved,
    ];
    assert!(transient.iter().all(KeenCodeEvent::is_transient));
    assert!(transient.iter().all(|event| !event.is_authoritative()));
}

/// 验证 Session 级事件与 Turn 级事件不能混用身份。
#[test]
fn frontend_event_identity_scope_is_explicit() {
    let session_scoped = [KeenCodeEvent::BackgroundTaskCompleted {
        task_id: "task-a".to_owned(),
        task_kind: BackgroundTaskKind::Agent,
        agent_id: Some("agent-child".to_owned()),
        status: BackgroundTaskTerminalStatus::Cancelled,
        duration_ms: 100,
        summary: Some("子 Agent 已取消".to_owned()),
    }];
    for event in session_scoped {
        assert!(session_event(event.clone()).is_ok());
        assert!(turn_event(event).is_err());
    }

    let turn_scoped = [
        KeenCodeEvent::ModelRetryScheduled {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 100,
            message: "稍后重试".to_owned(),
        },
        KeenCodeEvent::ModelFirstStreamObserved,
    ];
    for event in turn_scoped {
        assert!(turn_event(event.clone()).is_ok());
        assert!(session_event(event).is_err());
    }

    let system_notification = KeenCodeEvent::SystemNotification {
        level: SystemNotificationLevel::Info,
        message: "状态已更新".to_owned(),
    };
    assert!(session_event(system_notification.clone()).is_ok());
    assert!(turn_event(system_notification).is_ok());
}

/// 验证未知字段、越界值和可能携带凭据的授权地址都会被拒绝。
#[test]
fn frontend_events_reject_unknown_unbounded_or_sensitive_fields() {
    let unknown = json!({
        "type": "model_retry_scheduled",
        "attempt": 1,
        "maxAttempts": 3,
        "delayMs": 100,
        "message": "稍后重试",
        "rawError": "provider payload"
    });
    assert!(serde_json::from_value::<KeenCodeEvent>(unknown).is_err());
    for unknown_shape in [
        json!({
            "type": "system_notification",
            "level": "warn",
            "message": "旧等级"
        }),
        json!({
            "type": "background_task_completed",
            "taskId": "task-a",
            "succeeded": true
        }),
        json!({
            "type": "background_task_completed",
            "taskId": "task-a",
            "taskKind": "shell",
            "status": "done",
            "durationMs": 1,
            "summary": "未知终态"
        }),
    ] {
        assert!(serde_json::from_value::<KeenCodeEvent>(unknown_shape).is_err());
    }

    for event in [
        KeenCodeEvent::SystemNotification {
            level: SystemNotificationLevel::Info,
            message: "x".repeat(4097),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: Some("agent-child".to_owned()),
            status: BackgroundTaskTerminalStatus::Succeeded,
            duration_ms: 100,
            summary: Some("错误绑定".to_owned()),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Agent,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Failed,
            duration_ms: 100,
            summary: Some("缺少 Agent 标识".to_owned()),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Cancelled,
            duration_ms: 2_592_000_001,
            summary: Some("持续时间越界".to_owned()),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Cancelled,
            duration_ms: 100,
            summary: Some(String::new()),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Failed,
            duration_ms: 100,
            summary: Some("x".repeat(4097)),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Failed,
            duration_ms: 100,
            summary: Some("Authorization: Bearer unredacted-token-value".to_owned()),
        },
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-a".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Failed,
            duration_ms: 100,
            summary: Some(format!("请求包含 sk-{}", "x".repeat(26))),
        },
        KeenCodeEvent::ModelRetryScheduled {
            attempt: 3,
            max_attempts: 3,
            delay_ms: 100,
            message: "稍后重试".to_owned(),
        },
        KeenCodeEvent::ModelRetryScheduled {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 600_001,
            message: "稍后重试".to_owned(),
        },
    ] {
        assert!(event.validate().is_err());
    }

    assert!(
        KeenCodeEvent::BackgroundTaskCompleted {
            task_id: "task-safe".to_owned(),
            task_kind: BackgroundTaskKind::Shell,
            agent_id: None,
            status: BackgroundTaskTerminalStatus::Failed,
            duration_ms: 100,
            summary: Some("Authorization: Bearer [REDACTED]".to_owned()),
        }
        .validate()
        .is_ok()
    );

    for authorization_url in [
        "https://user:secret@auth.example/authorize",
        "https://auth.example/authorize?client_secret=secret",
        "https://auth.example/authorize#access_token=secret",
        "https://auth.example/authorize bad",
        "http://auth.example/authorize",
    ] {
        assert!(
            McpOAuthEvent::AuthorizationRequired {
                project_path: "D:/projects/active".to_owned(),
                server_name: "docs".to_owned(),
                authorization_url: authorization_url.to_owned(),
            }
            .validate()
            .is_err()
        );
    }

    let value: Value = serde_json::to_value(
        McpOAuthNotification::new(McpOAuthEvent::AuthorizationRequired {
            project_path: "D:/projects/active".to_owned(),
            server_name: "local".to_owned(),
            authorization_url: "http://127.0.0.1:3000/authorize".to_owned(),
        })
        .expect("本机 HTTP OAuth 地址应有效"),
    )
    .expect("合法事件应序列化");
    assert_eq!(
        value["params"]["authorizationUrl"],
        "http://127.0.0.1:3000/authorize"
    );

    for event in [
        McpOAuthEvent::Authorized {
            project_path: "D:/projects/active\u{0000}".to_owned(),
            server_name: "local".to_owned(),
        },
        McpOAuthEvent::Failed {
            project_path: "x".repeat(4097),
            server_name: "local".to_owned(),
            message: "授权失败".to_owned(),
        },
    ] {
        assert!(event.validate().is_err());
    }

    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","deliverySequence":1,"occurredAtMs":1,"event":{"type":"background_task_completed","taskId":"task-a","taskKind":"shell","status":"succeeded","durationMs":1,"summary":null}}"#,
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
}
