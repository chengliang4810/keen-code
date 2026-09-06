//! ACP 严格方法、出站 Elicitation、扩展事件和单调序号测试。

use agent_client_protocol_schema::{
    AgentNotification, AgentRequest, AuthMethod, AuthMethodAgent, AuthenticateRequest,
    ClientCapabilities, ClientResponse, CompleteElicitationNotification, ContentBlock,
    ContentChunk, CreateElicitationRequest, CreateElicitationResponse, CurrentModeUpdate,
    ElicitationAction, ElicitationCapabilities, ElicitationFormCapabilities, ElicitationFormMode,
    ElicitationSchema, ElicitationSessionScope, ElicitationUrlCapabilities, ElicitationUrlMode,
    Error as AcpRpcError, Plan, ProtocolVersion, RequestId, SessionMode, SessionModeState,
    SessionUpdate, SetSessionModeRequest, UsageUpdate,
};
use serde_json::{Value, json};

use crate::{
    AcpBoundaryError, AcpClientRequestEncoder, AcpIncomingFrame, AcpNotification, AcpRequest,
    AcpRequestDecoder, AcpRequestLimits, AcpResponseDecoder, AcpResponseEncoder, AcpResponseLimits,
    AcpResponsePayload, AgentLifecycleStatus, AuthenticateResponse, CancelBackgroundTaskResponse,
    DeleteSessionRequest, DeleteSessionResponse, ElicitationRouter, GenerateSessionTitleResponse,
    GoalClearResponse, GoalGetResponse, GoalMutationResponse, GoalRecord, GoalScope, GoalStatus,
    InitializeResponseDto, KeenCodeEvent, KeenCodeEventEnvelope, KeenCodeEventEnvelopeParams,
    KeenCodeEventLimits, LoadSessionResponse, MAX_REPLAY_EVENTS, McpConnectionStatus,
    McpListRequest, McpListResponse, McpOAuthCallbackRequest, McpOAuthCallbackResponse,
    McpOAuthCancelResponse, McpOAuthEvent, McpOAuthNotification, McpOAuthServerRequest,
    McpOAuthStartResponse, McpOAuthStatus, McpRuntimePhase, McpServerStatus, McpTransportKind,
    RecoveryState, RenameSessionResponse, ReplaySessionResponse, RewindCandidate,
    RewindCandidatesResponse, RewindSessionResponse, SESSION_UPDATE_DELIVERY_SCHEMA_VERSION,
    SessionDeleteCapabilities, SessionSequence, SessionUpdateDeliveryEnvelope,
    SessionUpdateDeliveryLimits, SetSessionModeResponse, SteerSessionResponse,
    SystemNotificationLevel, validate_set_session_mode_request,
};

/// 构造一条只包含文本的标准 Agent 消息更新。
fn agent_message_update(text: &str) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(text)))
}

/// 构造满足完整响应不变量的项目 Goal。
fn sample_goal(status: GoalStatus) -> GoalRecord {
    GoalRecord {
        id: "goal-a".to_owned(),
        title: "完成类型化响应".to_owned(),
        scope: GoalScope::Project,
        status,
        description: Some("覆盖编码、恢复和资源边界".to_owned()),
        progress_percent: Some(40),
        objective: "所有 ACP 方法都只能返回当前方法对应的类型化 DTO".to_owned(),
        token_budget: Some(20_000),
        tokens_used: 4_000,
        time_used_seconds: 120,
        blocked_reason: (status == GoalStatus::Blocked).then(|| "等待外部服务".to_owned()),
        completion_evidence: (status == GoalStatus::Completed)
            .then(|| "ACP 编码、恢复与边界测试全部通过".to_owned()),
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
    }
}

/// 使用固定请求标识编码类型化响应并返回原始字节和 JSON 视图。
fn encode_typed_result<T>(response: &T) -> (Vec<u8>, Value)
where
    T: AcpResponsePayload,
{
    let raw = AcpResponseEncoder::new()
        .encode_result(RequestId::Number(1), response)
        .expect("类型化响应应编码");
    let value = serde_json::from_slice(&raw).expect("类型化响应应为 JSON");
    (raw, value)
}

#[test]
fn session_sequence_is_monotonic_restorable_and_never_wraps() {
    let mut sequence = SessionSequence::new();
    assert_eq!(sequence.peek(), 1);
    assert_eq!(sequence.allocate().expect("应分配一"), 1);
    assert_eq!(sequence.allocate().expect("应分配二"), 2);
    assert_eq!(sequence.last_allocated(), 2);

    let mut restored = SessionSequence::restore(2).expect("应从已提交水位恢复");
    assert_eq!(restored.allocate().expect("恢复后应分配三"), 3);

    let mut final_sequence =
        SessionSequence::restore(u64::MAX - 1).expect("最大值前一个水位仍可恢复");
    assert_eq!(
        final_sequence.allocate().expect("最后一个序号仍可分配"),
        u64::MAX
    );
    assert_eq!(final_sequence.last_allocated(), u64::MAX);
    assert_eq!(
        final_sequence.allocate(),
        Err(AcpBoundaryError::SequenceExhausted)
    );
    assert_eq!(
        SessionSequence::restore(u64::MAX),
        Err(AcpBoundaryError::SequenceExhausted)
    );
}

#[test]
fn initialize_response_golden_always_mounts_typed_session_delete_capability() {
    let response = InitializeResponseDto::new(ProtocolVersion::V1);
    let value = serde_json::to_value(response).expect("初始化响应应序列化");
    assert_eq!(
        value,
        json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "audio": false,
                    "embeddedContext": false,
                    "image": false
                },
                "mcpCapabilities": { "http": false, "sse": false },
                "sessionCapabilities": { "delete": {} }
            },
            "authMethods": []
        })
    );
}

#[test]
fn authenticate_and_session_set_mode_match_standard_request_response_golden() {
    let decoder = AcpRequestDecoder::new();
    let authenticate = decoder
        .decode_request("authenticate", json!({ "methodId": "browser-oauth" }))
        .expect("标准认证请求应解码");
    assert!(matches!(authenticate, AcpRequest::Authenticate(_)));
    let set_mode = decoder
        .decode_request(
            "session/set_mode",
            json!({ "sessionId": "session-a", "modeId": "plan" }),
        )
        .expect("标准 Session 模式请求应解码");
    assert!(matches!(set_mode, AcpRequest::SetSessionMode(_)));
    assert_eq!(
        serde_json::to_value(AuthenticateResponse::new()).expect("认证响应应序列化"),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(SetSessionModeResponse::new()).expect("模式响应应序列化"),
        json!({})
    );
    for (raw, expected_method) in [
        (
            br#"{"jsonrpc":"2.0","id":"auth-1","method":"authenticate","params":{"methodId":"browser-oauth"}}"#.as_slice(),
            "authenticate",
        ),
        (
            br#"{"jsonrpc":"2.0","id":2,"method":"session/set_mode","params":{"sessionId":"session-a","modeId":"plan"}}"#.as_slice(),
            "session/set_mode",
        ),
    ] {
        let frame = decoder.decode_raw(raw).expect("标准控制面完整信封应解码");
        assert!(matches!(
            frame,
            AcpIncomingFrame::Request(frame) if frame.request().method() == expected_method
        ));
    }

    for (method, params) in [
        ("authenticate", json!({})),
        (
            "authenticate",
            json!({ "methodId": "browser-oauth", "unknown": true }),
        ),
        ("session/set_mode", json!({ "sessionId": "session-a" })),
        (
            "session/set_mode",
            json!({ "sessionId": "session-a", "modeId": "" }),
        ),
    ] {
        assert!(decoder.decode_request(method, params).is_err());
    }
}

#[test]
fn authenticate_and_session_mode_are_consistent_with_advertised_capabilities() {
    let response =
        InitializeResponseDto::new(ProtocolVersion::V1).auth_methods(vec![AuthMethod::Agent(
            AuthMethodAgent::new("browser-oauth", "Browser OAuth"),
        )]);
    response
        .validate_authenticate_request(&AuthenticateRequest::new("browser-oauth"))
        .expect("已公布认证方式应可选");
    assert_eq!(
        response.validate_authenticate_request(&AuthenticateRequest::new("missing")),
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    );
    let duplicated = InitializeResponseDto::new(ProtocolVersion::V1).auth_methods(vec![
        AuthMethod::Agent(AuthMethodAgent::new("same", "A")),
        AuthMethod::Agent(AuthMethodAgent::new("same", "B")),
    ]);
    assert_eq!(
        duplicated.validate(),
        Err(AcpBoundaryError::InvalidSemanticValue)
    );

    let state = SessionModeState::new(
        "default",
        vec![
            SessionMode::new("default", "Default"),
            SessionMode::new("plan", "Plan"),
        ],
    );
    validate_set_session_mode_request(&SetSessionModeRequest::new("session-a", "plan"), &state)
        .expect("已公布 Session 模式应可选");
    assert_eq!(
        validate_set_session_mode_request(
            &SetSessionModeRequest::new("session-a", "missing"),
            &state,
        ),
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    );
    let invalid_state = SessionModeState::new("missing", vec![SessionMode::new("plan", "Plan")]);
    assert_eq!(
        validate_set_session_mode_request(
            &SetSessionModeRequest::new("session-a", "plan"),
            &invalid_state,
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    );
}

#[test]
fn session_delete_supplement_matches_wire_shape_and_supports_meta_builder() {
    assert_eq!(
        serde_json::to_value(DeleteSessionRequest::new("session-a")).expect("删除请求应序列化"),
        json!({ "sessionId": "session-a" })
    );
    assert_eq!(
        serde_json::to_value(DeleteSessionResponse::new()).expect("删除响应应序列化"),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(SessionDeleteCapabilities::new()).expect("删除能力应序列化"),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(SessionDeleteCapabilities::new().meta(Some(
            serde_json::Map::from_iter([("trace".to_owned(), json!(true),)])
        )))
        .expect("带元数据删除能力应序列化"),
        json!({ "_meta": { "trace": true } })
    );
}

#[test]
fn decoder_accepts_standard_initialize_new_load_set_config_list_fork_and_cancel_golden() {
    let decoder = AcpRequestDecoder::new();
    let cases = [
        (
            "authenticate",
            json!({ "methodId": "browser-oauth" }),
            "authenticate",
        ),
        (
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
            "initialize",
        ),
        (
            "session/new",
            json!({ "cwd": "C:\\workspace", "mcpServers": [] }),
            "session/new",
        ),
        (
            "session/load",
            json!({
                "sessionId": "session-a",
                "cwd": "C:\\workspace",
                "mcpServers": []
            }),
            "session/load",
        ),
        (
            "session/set_config_option",
            json!({ "sessionId": "session-a", "configId": "model", "value": "gpt" }),
            "session/set_config_option",
        ),
        (
            "session/set_mode",
            json!({ "sessionId": "session-a", "modeId": "default" }),
            "session/set_mode",
        ),
        ("session/list", json!({}), "session/list"),
        (
            "session/fork",
            json!({ "sessionId": "session-a", "cwd": "C:\\workspace" }),
            "session/fork",
        ),
    ];
    for (method, params, expected) in cases {
        let request = decoder
            .decode_request(method, params)
            .unwrap_or_else(|_| panic!("标准方法 {method} 应解码"));
        assert_eq!(request.method(), expected);
    }

    let cancel = decoder
        .decode_notification("session/cancel", json!({ "sessionId": "session-a" }))
        .expect("标准取消通知应解码");
    assert!(matches!(cancel, AcpNotification::Cancel(_)));
    assert_invalid_standard_shapes(&decoder);
}

/// 逐项断言标准初始化、Session 生命周期、配置和取消方法拒绝非法形状。
fn assert_invalid_standard_shapes(decoder: &AcpRequestDecoder) {
    let invalid_requests = [
        (
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "unknown": true
            }),
        ),
        ("session/new", json!({ "cwd": "C:\\workspace" })),
        (
            "session/load",
            json!({ "cwd": "C:\\workspace", "mcpServers": [] }),
        ),
        (
            "session/set_config_option",
            json!({ "sessionId": "session-a", "configId": "model" }),
        ),
        ("session/list", json!({ "unknown": true })),
        ("session/fork", json!({ "sessionId": "session-a" })),
    ];
    for (method, params) in invalid_requests {
        assert!(
            matches!(
                decoder.decode_request(method, params),
                Err(AcpBoundaryError::InvalidParams)
            ),
            "标准方法 {method} 应拒绝非法形状"
        );
    }
    assert!(matches!(
        decoder.decode_notification(
            "session/cancel",
            json!({ "sessionId": "session-a", "unknown": true }),
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
}

#[test]
fn decoder_raw_transport_rejects_duplicate_keys_before_typed_decode() {
    let decoder = AcpRequestDecoder::new();
    let request = decoder
        .decode_raw(
            br#"{"jsonrpc":"2.0","id":"request-a","method":"keencode/session/rename","params":{"sessionId":"session-a","title":"A"}}"#,
        )
        .expect("唯一对象键应解码");
    assert!(matches!(
        request,
        AcpIncomingFrame::Request(frame)
            if frame.id() == &RequestId::Str("request-a".to_owned())
                && matches!(frame.request(), AcpRequest::RenameSession(_))
    ));

    assert!(matches!(
        decoder.decode_raw(
            br#"{"jsonrpc":"2.0","id":1,"method":"keencode/session/rename","params":{"sessionId":"session-a","title":"A","title":"B"}}"#,
        ),
        Err(AcpBoundaryError::DuplicateJsonKey)
    ));
    assert!(matches!(
        decoder.decode_raw(
            br#"{"jsonrpc":"2.0","method":"keencode/config/update","params":{"revision":1,"revision":2}}"#,
        ),
        Err(AcpBoundaryError::DuplicateJsonKey)
    ));
}

#[test]
fn decoder_full_frame_rejects_wrong_version_id_unknown_fields_and_wrong_channel() {
    let decoder = AcpRequestDecoder::new();
    let notification = decoder
        .decode_raw(
            br#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session-a"}}"#,
        )
        .expect("无 ID 取消应解码为通知");
    assert!(matches!(notification, AcpIncomingFrame::Notification(_)));

    for raw in [
        br#"{"jsonrpc":"1.0","id":1,"method":"session/list","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1.5,"method":"session/list","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":true,"method":"session/list","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":"","method":"session/list","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"session/list","params":{},"extra":true}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"session/cancel","params":{"sessionId":"session-a"}}"#
            .as_slice(),
        br#"{"jsonrpc":"2.0","method":"session/list","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"session/list"}"#.as_slice(),
    ] {
        assert!(decoder.decode_raw(raw).is_err());
    }

    for id in [json!(null), json!(-1), json!("request-a")] {
        let raw = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/list",
            "params": {}
        }))
        .expect("完整请求应序列化");
        assert!(matches!(
            decoder.decode_raw(&raw),
            Ok(AcpIncomingFrame::Request(_))
        ));
    }
}

#[test]
fn response_encoder_preserves_request_id_and_keeps_result_error_mutually_exclusive() {
    let encoder = AcpResponseEncoder::new();
    let response = SteerSessionResponse::new("session-a");
    let success = encoder
        .encode_result(RequestId::Str("request-a".to_owned()), &response)
        .expect("成功响应应编码");
    let success_value: Value = serde_json::from_slice(&success).expect("成功响应应为 JSON");
    assert_eq!(success_value["jsonrpc"], json!("2.0"));
    assert_eq!(success_value["id"], json!("request-a"));
    assert_eq!(
        success_value["result"],
        json!({ "sessionId": "session-a", "accepted": true })
    );
    assert!(success_value.get("error").is_none());
    let restored = AcpResponseDecoder::new()
        .decode_result::<SteerSessionResponse>(&success)
        .expect("成功响应应严格恢复");
    assert_eq!(restored.id(), &RequestId::Str("request-a".to_owned()));
    assert_eq!(restored.result(), &response);

    let error = encoder
        .encode_error(
            RequestId::Number(7),
            &AcpRpcError::invalid_params().data(json!({ "field": "modeId" })),
        )
        .expect("错误响应应编码");
    let error: Value = serde_json::from_slice(&error).expect("错误响应应为 JSON");
    assert_eq!(error["jsonrpc"], json!("2.0"));
    assert_eq!(error["id"], json!(7));
    assert_eq!(error["error"]["code"], json!(-32602));
    assert_eq!(error["error"]["data"], json!({ "field": "modeId" }));
    assert!(error.get("result").is_none());
}

#[test]
fn response_encoder_rejects_invalid_ids_and_complete_frame_resource_overflow() {
    let encoder = AcpResponseEncoder::new();
    assert!(matches!(
        encoder.encode_result(RequestId::Str(String::new()), &AuthenticateResponse::new()),
        Err(AcpBoundaryError::InvalidIdentifier)
    ));
    encoder
        .encode_result(RequestId::Null, &AuthenticateResponse::new())
        .expect("JSON-RPC 规范允许 null 请求 ID 原样回传");

    let byte_encoder = AcpResponseEncoder::with_limits(
        AcpResponseLimits::new(48, 8, 64).expect("测试响应上限有效"),
    )
    .expect("响应编码器上限有效");
    assert!(matches!(
        byte_encoder.encode_result(
            RequestId::Number(1),
            &RenameSessionResponse {
                session_id: "session-a".to_owned(),
                title: "long response".to_owned(),
                journal_sequence: 1,
            }
        ),
        Err(AcpBoundaryError::PayloadTooLarge { limit: 48 })
    ));

    let depth_encoder = AcpResponseEncoder::with_limits(
        AcpResponseLimits::new(1024, 2, 64).expect("测试响应上限有效"),
    )
    .expect("响应编码器上限有效");
    assert!(matches!(
        depth_encoder.encode_result(
            RequestId::Number(1),
            &GoalGetResponse {
                session_id: "session-a".to_owned(),
                revision: 1,
                goal: Some(sample_goal(GoalStatus::Active)),
            }
        ),
        Err(AcpBoundaryError::PayloadTooDeep { limit: 2 })
    ));

    let node_encoder = AcpResponseEncoder::with_limits(
        AcpResponseLimits::new(1024, 8, 4).expect("测试响应上限有效"),
    )
    .expect("响应编码器上限有效");
    assert!(matches!(
        node_encoder.encode_result(
            RequestId::Number(1),
            &SteerSessionResponse::new("session-a")
        ),
        Err(AcpBoundaryError::PayloadTooManyNodes { limit: 4 })
    ));
}

#[test]
fn load_session_response_uses_official_shape_and_strict_round_trip() {
    let response = LoadSessionResponse::new().modes(SessionModeState::new(
        "plan",
        vec![
            SessionMode::new("plan", "Plan"),
            SessionMode::new("default", "Default"),
        ],
    ));
    let raw = AcpResponseEncoder::new()
        .encode_result(RequestId::Number(9), &response)
        .expect("标准 session/load 响应应编码");
    let value: Value = serde_json::from_slice(&raw).expect("标准响应应为 JSON");
    assert_eq!(
        value,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {
                "modes": {
                    "currentModeId": "plan",
                    "availableModes": [
                        { "id": "plan", "name": "Plan" },
                        { "id": "default", "name": "Default" }
                    ]
                }
            }
        })
    );
    let restored = AcpResponseDecoder::new()
        .decode_result::<LoadSessionResponse>(&raw)
        .expect("标准 session/load 响应应严格恢复");
    let (id, restored_response) = restored.into_parts();
    assert_eq!(id, RequestId::Number(9));
    assert_eq!(restored_response, response);
}

#[test]
fn response_decoder_rejects_error_frames_unknown_fields_aliases_and_duplicates() {
    let decoder = AcpResponseDecoder::new();
    for raw in [
        br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"invalid"}}"#.as_slice(),
        br#"{"jsonrpc":"1.0","id":1,"result":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"result":{"unexpected":true}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"payload":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"result":{},"extra":true}"#.as_slice(),
    ] {
        assert!(decoder.decode_result::<LoadSessionResponse>(raw).is_err());
    }
    assert!(matches!(
        decoder.decode_result::<LoadSessionResponse>(
            br#"{"jsonrpc":"2.0","id":1,"id":2,"result":{}}"#,
        ),
        Err(AcpBoundaryError::DuplicateJsonKey)
    ));
}

#[test]
fn response_decoder_enforces_raw_bytes_depth_and_node_limits() {
    let raw = AcpResponseEncoder::new()
        .encode_result(
            RequestId::Number(1),
            &SteerSessionResponse::new("session-a"),
        )
        .expect("测试响应应编码");
    let byte_decoder = AcpResponseDecoder::with_limits(
        AcpResponseLimits::new(raw.len() - 1, 8, 64).expect("测试响应上限有效"),
    )
    .expect("响应解码器上限有效");
    assert!(matches!(
        byte_decoder.decode_result::<SteerSessionResponse>(&raw),
        Err(AcpBoundaryError::PayloadTooLarge { .. })
    ));

    let depth_decoder = AcpResponseDecoder::with_limits(
        AcpResponseLimits::new(1024, 1, 64).expect("测试响应上限有效"),
    )
    .expect("响应解码器上限有效");
    assert!(matches!(
        depth_decoder.decode_result::<SteerSessionResponse>(&raw),
        Err(AcpBoundaryError::PayloadTooDeep { limit: 1 })
    ));

    let node_decoder = AcpResponseDecoder::with_limits(
        AcpResponseLimits::new(1024, 8, 4).expect("测试响应上限有效"),
    )
    .expect("响应解码器上限有效");
    assert!(matches!(
        node_decoder.decode_result::<SteerSessionResponse>(&raw),
        Err(AcpBoundaryError::PayloadTooManyNodes { limit: 4 })
    ));
    assert_eq!(
        AcpResponseDecoder::new().limits(),
        AcpResponseLimits::default()
    );
}

#[test]
fn session_control_response_dtos_use_exact_current_shapes() {
    let generated_title = GenerateSessionTitleResponse {
        title: "新的自动标题".to_owned(),
    };
    let (generated_title_raw, generated_title_value) = encode_typed_result(&generated_title);
    assert_eq!(
        generated_title_value["result"],
        json!({ "title": "新的自动标题" })
    );
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<GenerateSessionTitleResponse>(&generated_title_raw)
            .expect("自动标题响应应恢复")
            .result(),
        &generated_title
    );

    let rename = RenameSessionResponse {
        session_id: "session-a".to_owned(),
        title: "新的标题".to_owned(),
        journal_sequence: 7,
    };
    let (rename_raw, rename_value) = encode_typed_result(&rename);
    assert_eq!(
        rename_value["result"],
        json!({
            "sessionId": "session-a",
            "title": "新的标题",
            "journalSequence": 7
        })
    );
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<RenameSessionResponse>(&rename_raw)
            .expect("重命名响应应恢复")
            .result(),
        &rename
    );

    let candidates = RewindCandidatesResponse {
        session_id: "session-a".to_owned(),
        candidates: vec![
            RewindCandidate {
                message_id: "message-a".to_owned(),
                preview: "先检查实现".to_owned(),
                created_at_ms: 1_700_000_000_000,
            },
            RewindCandidate {
                message_id: "message-b".to_owned(),
                preview: "再修改代码".to_owned(),
                created_at_ms: 1_700_000_001_000,
            },
        ],
    };
    let (candidates_raw, candidates_value) = encode_typed_result(&candidates);
    assert_eq!(
        candidates_value["result"],
        json!({
            "sessionId": "session-a",
            "candidates": [
                {
                    "messageId": "message-a",
                    "preview": "先检查实现",
                    "createdAtMs": 1_700_000_000_000_u64
                },
                {
                    "messageId": "message-b",
                    "preview": "再修改代码",
                    "createdAtMs": 1_700_000_001_000_u64
                }
            ]
        })
    );
    AcpResponseDecoder::new()
        .decode_result::<RewindCandidatesResponse>(&candidates_raw)
        .expect("回退候选响应应恢复");

    let rewind = RewindSessionResponse {
        session_id: "session-a".to_owned(),
        archived_session_id: "archive-a".to_owned(),
        through_journal_sequence: 4,
        reverted_files: false,
    };
    let (rewind_raw, rewind_value) = encode_typed_result(&rewind);
    assert_eq!(
        rewind_value["result"],
        json!({
            "sessionId": "session-a",
            "archivedSessionId": "archive-a",
            "throughJournalSequence": 4,
            "revertedFiles": false
        })
    );
    AcpResponseDecoder::new()
        .decode_result::<RewindSessionResponse>(&rewind_raw)
        .expect("回退响应应恢复");

    let cancelled = CancelBackgroundTaskResponse {
        session_id: "session-a".to_owned(),
        task_id: "task-a".to_owned(),
        cancelled: true,
    };
    let (cancelled_raw, cancelled_value) = encode_typed_result(&cancelled);
    assert_eq!(
        cancelled_value["result"],
        json!({ "sessionId": "session-a", "taskId": "task-a", "cancelled": true })
    );
    AcpResponseDecoder::new()
        .decode_result::<CancelBackgroundTaskResponse>(&cancelled_raw)
        .expect("后台任务取消响应应恢复");
}

#[test]
fn session_control_response_validation_rejects_duplicate_candidates_and_false_capabilities() {
    let duplicate = RewindCandidatesResponse {
        session_id: "session-a".to_owned(),
        candidates: vec![
            RewindCandidate {
                message_id: "message-a".to_owned(),
                preview: "一".to_owned(),
                created_at_ms: 1,
            },
            RewindCandidate {
                message_id: "message-a".to_owned(),
                preview: "二".to_owned(),
                created_at_ms: 2,
            },
        ],
    };
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(RequestId::Number(1), &duplicate),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(
            RequestId::Number(1),
            &SteerSessionResponse {
                session_id: "session-a".to_owned(),
                accepted: false,
            }
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(
            RequestId::Number(1),
            &RewindSessionResponse {
                session_id: "session-a".to_owned(),
                archived_session_id: "archive-a".to_owned(),
                through_journal_sequence: 1,
                reverted_files: true,
            }
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
}

#[test]
fn replay_response_reports_only_typed_delivery_cursors() {
    let response = ReplaySessionResponse {
        session_id: "session-a".to_owned(),
        start_after: 3,
        next_after: 5,
        through_journal_sequence: 8,
        through_delivery_sequence: 2,
        replayed_events: 2,
        has_more: true,
    };
    let (raw, value) = encode_typed_result(&response);
    assert_eq!(
        value["result"],
        json!({
            "sessionId": "session-a",
            "startAfter": 3,
            "nextAfter": 5,
            "throughJournalSequence": 8,
            "throughDeliverySequence": 2,
            "replayedEvents": 2,
            "hasMore": true
        })
    );
    assert!(value["result"].get("events").is_none());
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<ReplaySessionResponse>(&raw)
            .expect("重放接管响应应恢复")
            .result(),
        &response
    );
}

#[test]
fn replay_response_rejects_non_progressing_or_inconsistent_cursors() {
    let valid = ReplaySessionResponse {
        session_id: "session-a".to_owned(),
        start_after: 3,
        next_after: 5,
        through_journal_sequence: 8,
        through_delivery_sequence: 2,
        replayed_events: 2,
        has_more: true,
    };
    let mut less_delivery_waterline = valid.clone();
    less_delivery_waterline.through_delivery_sequence = 3;
    encode_typed_result(&less_delivery_waterline);
    encode_typed_result(&valid);

    let mut invalid = Vec::new();
    let mut reversed = valid.clone();
    reversed.start_after = 6;
    invalid.push(reversed);
    let mut beyond_tail = valid.clone();
    beyond_tail.next_after = 9;
    invalid.push(beyond_tail);
    let mut no_progress = valid.clone();
    no_progress.next_after = no_progress.start_after;
    invalid.push(no_progress);
    let mut wrong_more = valid.clone();
    wrong_more.has_more = false;
    invalid.push(wrong_more);
    let mut greater_delivery_waterline = valid.clone();
    greater_delivery_waterline.through_delivery_sequence = 1;
    invalid.push(greater_delivery_waterline);
    let mut over_limit = valid;
    over_limit.replayed_events = MAX_REPLAY_EVENTS + 1;
    invalid.push(over_limit);
    for response in invalid {
        assert!(
            AcpResponseEncoder::new()
                .encode_result(RequestId::Number(1), &response)
                .is_err()
        );
    }

    let terminal_empty = ReplaySessionResponse {
        session_id: "session-a".to_owned(),
        start_after: 8,
        next_after: 8,
        through_journal_sequence: 8,
        through_delivery_sequence: 0,
        replayed_events: 0,
        has_more: false,
    };
    encode_typed_result(&terminal_empty);

    let projected_empty_with_more = ReplaySessionResponse {
        session_id: "session-a".to_owned(),
        start_after: 3,
        next_after: 5,
        through_journal_sequence: 8,
        through_delivery_sequence: 2,
        replayed_events: 0,
        has_more: true,
    };
    encode_typed_result(&projected_empty_with_more);

    let projected_empty_terminal = ReplaySessionResponse {
        session_id: "session-a".to_owned(),
        start_after: 3,
        next_after: 8,
        through_journal_sequence: 8,
        through_delivery_sequence: 2,
        replayed_events: 0,
        has_more: false,
    };
    encode_typed_result(&projected_empty_terminal);
}

#[test]
fn goal_responses_use_complete_records_revisions_and_tombstones() {
    let active = sample_goal(GoalStatus::Active);
    let get = GoalGetResponse {
        session_id: "session-a".to_owned(),
        revision: 3,
        goal: Some(active.clone()),
    };
    let (get_raw, get_value) = encode_typed_result(&get);
    assert_eq!(
        get_value["result"],
        json!({
            "sessionId": "session-a",
            "revision": 3,
            "goal": {
                "id": "goal-a",
                "title": "完成类型化响应",
                "scope": "project",
                "status": "active",
                "description": "覆盖编码、恢复和资源边界",
                "progressPercent": 40,
                "objective": "所有 ACP 方法都只能返回当前方法对应的类型化 DTO",
                "tokenBudget": 20_000,
                "tokensUsed": 4_000,
                "timeUsedSeconds": 120,
                "createdAtMs": 1_700_000_000_000_u64,
                "updatedAtMs": 1_700_000_001_000_u64
            }
        })
    );
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<GoalGetResponse>(&get_raw)
            .expect("Goal 查询响应应恢复")
            .result(),
        &get
    );

    let completed = sample_goal(GoalStatus::Completed);
    let mutation = GoalMutationResponse {
        session_id: "session-a".to_owned(),
        revision: 4,
        goal: completed.clone(),
        deduplicated: false,
    };
    let (mutation_raw, mutation_value) = encode_typed_result(&mutation);
    assert_eq!(mutation_value["result"]["revision"], json!(4));
    assert_eq!(
        mutation_value["result"]["goal"]["status"],
        json!("completed")
    );
    assert_eq!(
        mutation_value["result"]["goal"]["completionEvidence"],
        json!("ACP 编码、恢复与边界测试全部通过")
    );
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<GoalMutationResponse>(&mutation_raw)
            .expect("Goal 变更响应应恢复")
            .result(),
        &mutation
    );

    let cleared = GoalClearResponse {
        session_id: "session-a".to_owned(),
        revision: 5,
        cleared_goal_id: "goal-a".to_owned(),
        deduplicated: true,
    };
    let (cleared_raw, cleared_value) = encode_typed_result(&cleared);
    assert_eq!(
        cleared_value["result"],
        json!({
            "sessionId": "session-a",
            "revision": 5,
            "clearedGoalId": "goal-a",
            "deduplicated": true
        })
    );
    AcpResponseDecoder::new()
        .decode_result::<GoalClearResponse>(&cleared_raw)
        .expect("Goal 清理响应应恢复");

    let empty = GoalGetResponse {
        session_id: "session-a".to_owned(),
        revision: 0,
        goal: None,
    };
    assert_eq!(
        encode_typed_result(&empty).1["result"],
        json!({ "sessionId": "session-a", "revision": 0 })
    );
}

#[test]
fn goal_response_validation_rejects_invalid_status_budget_time_and_revision() {
    let mut invalid_goals = Vec::new();
    let mut missing_reason = sample_goal(GoalStatus::Blocked);
    missing_reason.blocked_reason = None;
    invalid_goals.push(missing_reason);
    let mut unexpected_reason = sample_goal(GoalStatus::Active);
    unexpected_reason.blocked_reason = Some("不应存在".to_owned());
    invalid_goals.push(unexpected_reason);
    let mut missing_evidence = sample_goal(GoalStatus::Completed);
    missing_evidence.completion_evidence = None;
    invalid_goals.push(missing_evidence);
    let mut unexpected_evidence = sample_goal(GoalStatus::Blocked);
    unexpected_evidence.completion_evidence = Some("不应存在".to_owned());
    invalid_goals.push(unexpected_evidence);
    let mut invalid_progress = sample_goal(GoalStatus::Active);
    invalid_progress.progress_percent = Some(101);
    invalid_goals.push(invalid_progress);
    let mut zero_budget = sample_goal(GoalStatus::Active);
    zero_budget.token_budget = Some(0);
    invalid_goals.push(zero_budget);
    let mut zero_created = sample_goal(GoalStatus::Active);
    zero_created.created_at_ms = 0;
    invalid_goals.push(zero_created);
    let mut reversed_time = sample_goal(GoalStatus::Active);
    reversed_time.updated_at_ms = reversed_time.created_at_ms - 1;
    invalid_goals.push(reversed_time);
    for goal in invalid_goals {
        let response = GoalGetResponse {
            session_id: "session-a".to_owned(),
            revision: 1,
            goal: Some(goal),
        };
        assert!(
            AcpResponseEncoder::new()
                .encode_result(RequestId::Number(1), &response)
                .is_err()
        );
    }

    let zero_revision = GoalGetResponse {
        session_id: "session-a".to_owned(),
        revision: 0,
        goal: Some(sample_goal(GoalStatus::Active)),
    };
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(RequestId::Number(1), &zero_revision),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    let zero_mutation = GoalMutationResponse {
        session_id: "session-a".to_owned(),
        revision: 0,
        goal: sample_goal(GoalStatus::Active),
        deduplicated: false,
    };
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(RequestId::Number(1), &zero_mutation),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    let zero_clear = GoalClearResponse {
        session_id: "session-a".to_owned(),
        revision: 0,
        cleared_goal_id: "goal-a".to_owned(),
        deduplicated: false,
    };
    assert!(matches!(
        AcpResponseEncoder::new().encode_result(RequestId::Number(1), &zero_clear),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));

    assert!(matches!(
        AcpResponseDecoder::new().decode_result::<GoalGetResponse>(
            br#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"session-a","revision":0,"legacyGoal":null}}"#,
        ),
        Err(AcpBoundaryError::InvalidResponse)
    ));
}

#[test]
fn mcp_list_response_is_sorted_connection_state_without_sensitive_configuration() {
    let response = McpListResponse {
        init_phase: McpRuntimePhase::Ready,
        servers: vec![
            McpServerStatus {
                name: "docs".to_owned(),
                enabled: true,
                transport: McpTransportKind::StreamableHttp,
                connection_status: McpConnectionStatus::Connected,
                tools_count: 12,
                oauth_status: McpOAuthStatus::Authorized,
                error: None,
            },
            McpServerStatus {
                name: "local-tools".to_owned(),
                enabled: false,
                transport: McpTransportKind::Stdio,
                connection_status: McpConnectionStatus::Disabled,
                tools_count: 0,
                oauth_status: McpOAuthStatus::NotRequired,
                error: None,
            },
        ],
        error: None,
    };
    let (raw, value) = encode_typed_result(&response);
    assert_eq!(
        value["result"],
        json!({
            "initPhase": "ready",
            "servers": [
                {
                    "name": "docs",
                    "enabled": true,
                    "transport": "streamable_http",
                    "connectionStatus": "connected",
                    "toolsCount": 12,
                    "oauthStatus": "authorized"
                },
                {
                    "name": "local-tools",
                    "enabled": false,
                    "transport": "stdio",
                    "connectionStatus": "disabled",
                    "toolsCount": 0,
                    "oauthStatus": "not_required"
                }
            ]
        })
    );
    for sensitive in [
        "url",
        "target",
        "headers",
        "environment",
        "args",
        "accessToken",
        "refreshToken",
    ] {
        assert!(!value["result"].to_string().contains(sensitive));
    }
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<McpListResponse>(&raw)
            .expect("MCP 连接池状态应恢复")
            .result(),
        &response
    );
}

#[test]
fn mcp_list_response_rejects_unsorted_duplicate_and_inconsistent_states() {
    let connected = McpServerStatus {
        name: "a".to_owned(),
        enabled: true,
        transport: McpTransportKind::StreamableHttp,
        connection_status: McpConnectionStatus::Connected,
        tools_count: 1,
        oauth_status: McpOAuthStatus::Idle,
        error: None,
    };
    let encode = |servers: Vec<McpServerStatus>, phase, error| {
        AcpResponseEncoder::new().encode_result(
            RequestId::Number(1),
            &McpListResponse {
                init_phase: phase,
                servers,
                error,
            },
        )
    };

    let mut second = connected.clone();
    second.name = "b".to_owned();
    assert!(
        encode(
            vec![second.clone(), connected.clone()],
            McpRuntimePhase::Ready,
            None
        )
        .is_err()
    );
    assert!(
        encode(
            vec![connected.clone(), connected.clone()],
            McpRuntimePhase::Ready,
            None
        )
        .is_err()
    );
    assert!(encode(Vec::new(), McpRuntimePhase::Failed, None).is_err());
    assert!(
        encode(
            Vec::new(),
            McpRuntimePhase::Ready,
            Some("不应存在".to_owned())
        )
        .is_err()
    );

    let mut invalid_disabled = connected.clone();
    invalid_disabled.connection_status = McpConnectionStatus::Disabled;
    assert!(encode(vec![invalid_disabled], McpRuntimePhase::Ready, None).is_err());
    let mut failed_without_error = connected.clone();
    failed_without_error.connection_status = McpConnectionStatus::Failed;
    failed_without_error.tools_count = 0;
    assert!(encode(vec![failed_without_error], McpRuntimePhase::Ready, None).is_err());
    let mut disconnected_with_tools = connected.clone();
    disconnected_with_tools.connection_status = McpConnectionStatus::Disconnected;
    assert!(encode(vec![disconnected_with_tools], McpRuntimePhase::Ready, None).is_err());
    let mut stdio_oauth = connected;
    stdio_oauth.transport = McpTransportKind::Stdio;
    assert!(encode(vec![stdio_oauth], McpRuntimePhase::Ready, None).is_err());
}

#[test]
fn mcp_oauth_responses_only_acknowledge_async_state_machine_handoffs() {
    let start = McpOAuthStartResponse::new();
    let (start_raw, start_value) = encode_typed_result(&start);
    assert_eq!(start_value["result"], json!({ "status": "starting" }));
    assert_eq!(
        AcpResponseDecoder::new()
            .decode_result::<McpOAuthStartResponse>(&start_raw)
            .expect("OAuth start 响应应恢复")
            .result(),
        &start
    );

    let callback = McpOAuthCallbackResponse::new();
    let (callback_raw, callback_value) = encode_typed_result(&callback);
    assert_eq!(callback_value["result"], json!({ "status": "accepted" }));
    AcpResponseDecoder::new()
        .decode_result::<McpOAuthCallbackResponse>(&callback_raw)
        .expect("OAuth callback 响应应恢复");

    for cancelled in [true, false] {
        let response = McpOAuthCancelResponse::new(cancelled);
        let (raw, value) = encode_typed_result(&response);
        assert_eq!(value["result"], json!({ "cancelled": cancelled }));
        AcpResponseDecoder::new()
            .decode_result::<McpOAuthCancelResponse>(&raw)
            .expect("OAuth cancel 响应应恢复");
    }

    let combined = format!("{start_value}{callback_value}");
    for sensitive in [
        "serverName",
        "authorizationUrl",
        "state",
        "code",
        "verifier",
        "token",
    ] {
        assert!(!combined.contains(sensitive));
    }
}

#[test]
fn decoder_limits_raw_bytes_container_depth_and_total_nodes() {
    let byte_decoder = AcpRequestDecoder::with_limits(
        AcpRequestLimits::new(128, 48, 8, 32).expect("测试上限有效"),
    )
    .expect("解码器上限有效");
    assert!(matches!(
        byte_decoder.decode_raw(
            br#"{"jsonrpc":"2.0","id":1,"method":"keencode/session/rename","params":{"sessionId":"session-a","title":"long title"}}"#,
        ),
        Err(AcpBoundaryError::PayloadTooLarge { limit: 48 })
    ));

    let depth_decoder = AcpRequestDecoder::with_limits(
        AcpRequestLimits::new(128, 1024, 4, 64).expect("测试上限有效"),
    )
    .expect("解码器上限有效");
    assert!(
        matches!(
            depth_decoder.decode_request(
                "keencode/session/rename",
                json!({ "sessionId": "session-a", "title": "x", "a": { "b": 1 } }),
            ),
            Err(AcpBoundaryError::InvalidParams)
        ),
        "两层容器内的标量不得被误算为第三层"
    );
    assert!(matches!(
        depth_decoder.decode_raw(
            br#"{"jsonrpc":"2.0","id":1,"method":"keencode/session/rename","params":{"sessionId":"session-a","title":"x","a":{"b":{"c":{"d":1}}}}}"#,
        ),
        Err(AcpBoundaryError::PayloadTooDeep { limit: 4 })
    ));

    let node_decoder = AcpRequestDecoder::with_limits(
        AcpRequestLimits::new(128, 1024, 8, 7).expect("测试上限有效"),
    )
    .expect("解码器上限有效");
    assert!(matches!(
        node_decoder.decode_raw(
            br#"{"jsonrpc":"2.0","id":1,"method":"keencode/session/rename","params":{"sessionId":"session-a","title":"x","extra":true}}"#,
        ),
        Err(AcpBoundaryError::PayloadTooManyNodes { limit: 7 })
    ));
}

#[test]
fn decoder_rejects_aliases_unknown_fields_silent_defaults_and_wrong_channel() {
    let decoder = AcpRequestDecoder::new();
    assert!(matches!(
        decoder.decode_request(
            "session/rewind-candidates",
            json!({ "sessionId": "session-a" }),
        ),
        // 连字符是合法方法名字符，但旧别名仍不属于封闭路由表。
        Err(AcpBoundaryError::UnknownMethod)
    ));
    assert!(matches!(
        decoder.decode_request(
            "keencode/session/rewind_candidates",
            json!({ "sessionId": "session-a", "legacyField": true }),
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
    assert!(matches!(
        decoder.decode_request(
            "session/prompt",
            json!({
                "sessionId": "session-a",
                "prompt": [{ "type": "text", "text": "内容", "annotations": "非法" }]
            }),
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
    assert!(matches!(
        decoder.decode_notification("session/prompt", json!({})),
        Err(AcpBoundaryError::UnknownMethod)
    ));
    assert!(matches!(
        decoder.decode_request("keencode/mcp/list", Value::Null),
        Err(AcpBoundaryError::InvalidParams)
    ));
}

#[test]
fn keencode_extensions_use_only_keencode_namespace_without_session_aliases() {
    let decoder = AcpRequestDecoder::new();
    let requests = [
        (
            "keencode/session/steer",
            json!({ "sessionId": "session-a", "text": "继续检查" }),
        ),
        (
            "keencode/session/rename",
            json!({ "sessionId": "session-a", "title": "新标题" }),
        ),
        (
            "keencode/session/title",
            json!({ "sessionId": "session-a", "userMessage": "生成一个标题" }),
        ),
        (
            "keencode/session/rewind_candidates",
            json!({ "sessionId": "session-a" }),
        ),
        (
            "keencode/session/rewind",
            json!({
                "sessionId": "session-a",
                "targetMessageId": "message-a",
                "expectedText": "原始用户消息",
                "revertFiles": false
            }),
        ),
        (
            "keencode/session/replay",
            json!({ "sessionId": "session-a", "after": 1, "limit": 100 }),
        ),
        (
            "keencode/background/cancel",
            json!({ "sessionId": "session-a", "taskId": "task-a" }),
        ),
        ("keencode/goal/get", json!({ "sessionId": "session-a" })),
        (
            "keencode/goal/upsert",
            json!({
                "sessionId": "session-a",
                "goal": { "title": "目标", "objective": "完成目标" },
                "expectedRevision": 0,
                "requestNonce": "nonce-a"
            }),
        ),
        (
            "keencode/goal/transition",
            json!({
                "sessionId": "session-a",
                "goalId": "goal-a",
                "status": "completed",
                "completionEvidence": "目标验收通过",
                "expectedRevision": 1,
                "requestNonce": "nonce-b"
            }),
        ),
        (
            "keencode/goal/clear",
            json!({
                "sessionId": "session-a",
                "expectedRevision": 1,
                "requestNonce": "nonce-c"
            }),
        ),
    ];
    for (method, params) in requests {
        let request = decoder
            .decode_request(method, params)
            .expect("KeenCode 扩展请求应通过唯一方法名解码");
        assert_eq!(request.method(), method);
        assert!(
            matches!(
                decoder.decode_notification(method, json!({})),
                Err(AcpBoundaryError::UnknownMethod)
            ),
            "KeenCode 请求不能被通知通道接受：{method}"
        );
    }
    let notification = decoder
        .decode_notification("keencode/config/update", json!({ "revision": 1 }))
        .expect("KeenCode 配置通知应通过唯一方法名解码");
    assert_eq!(notification.method(), "keencode/config/update");
    assert!(
        matches!(
            decoder.decode_request("keencode/config/update", json!({ "revision": 1 })),
            Err(AcpBoundaryError::UnknownMethod)
        ),
        "KeenCode 配置通知不能被请求通道接受"
    );

    for method in [
        "session/steer",
        "session/rename",
        "session/rewind_candidates",
        "session/rewind",
        "session/replay",
        "session/cancel_background_task",
        "session/goal_get",
        "session/goal_upsert",
        "session/goal_transition",
        "session/goal_clear",
    ] {
        assert!(
            matches!(
                decoder.decode_request(method, json!({})),
                Err(AcpBoundaryError::UnknownMethod)
            ),
            "旧 Session 扩展方法不得继续解码：{method}"
        );
    }
    assert!(matches!(
        decoder.decode_notification("session/config_update", json!({ "revision": 1 })),
        Err(AcpBoundaryError::UnknownMethod)
    ));

    for method in [
        "keencode/session/rewind-candidates",
        "keencode/background/cancel-task",
        "keencode/goal/goal-get",
        "keencode/config-update",
    ] {
        assert!(matches!(
            decoder.decode_request(method, json!({})),
            Err(AcpBoundaryError::UnknownMethod)
        ));
    }
}

#[test]
fn extension_semantics_reject_invalid_rewind_replay_goal_and_oauth_values() {
    let decoder = AcpRequestDecoder::new();
    let invalid = [
        (
            "keencode/session/title",
            json!({ "sessionId": "session-a", "userMessage": "" }),
        ),
        (
            "keencode/session/rewind",
            json!({
                "sessionId": "session-a",
                "targetMessageId": "message-a",
                "expectedText": "",
                "revertFiles": false
            }),
        ),
        (
            "keencode/session/rewind",
            json!({
                "sessionId": "session-a",
                "targetMessageId": "message-a",
                "expectedText": "原始用户消息",
                "revertFiles": true
            }),
        ),
        (
            "keencode/session/replay",
            json!({ "sessionId": "session-a", "limit": 0 }),
        ),
        (
            "keencode/session/replay",
            json!({ "sessionId": "session-a", "limit": 1001 }),
        ),
        (
            "keencode/goal/upsert",
            json!({
                "sessionId": "session-a",
                "goal": { "title": "目标", "objective": "完成", "progressPercent": 101 },
                "expectedRevision": 0,
                "requestNonce": "nonce-a"
            }),
        ),
        (
            "keencode/goal/transition",
            json!({
                "sessionId": "session-a",
                "goalId": "goal-a",
                "status": "completed",
                "reason": "完成状态不能有原因",
                "completionEvidence": "验证通过",
                "expectedRevision": 1,
                "requestNonce": "nonce-a"
            }),
        ),
        (
            "keencode/goal/transition",
            json!({
                "sessionId": "session-a",
                "goalId": "goal-a",
                "status": "blocked",
                "completionEvidence": "阻塞状态不能携带完成证据",
                "expectedRevision": 1,
                "requestNonce": "nonce-a"
            }),
        ),
        (
            "keencode/mcp/oauth_callback",
            json!({
                "projectPath": "D:/projects/active",
                "serverName": "server-a",
                "code": "",
                "state": "state-a"
            }),
        ),
    ];
    for (method, params) in invalid {
        assert!(
            matches!(
                decoder.decode_request(method, params),
                Err(AcpBoundaryError::InvalidSemanticValue)
                    | Err(AcpBoundaryError::InvalidIdentifier)
            ),
            "{method} 应拒绝非法语义"
        );
    }
}

#[test]
fn extension_semantics_accept_current_valid_goal_background_and_replay_shapes() {
    let decoder = AcpRequestDecoder::new();
    let valid = [
        (
            "keencode/session/title",
            json!({ "sessionId": "session-a", "userMessage": "生成标题" }),
        ),
        (
            "keencode/session/replay",
            json!({ "sessionId": "session-a", "after": 1, "limit": 1000 }),
        ),
        (
            "keencode/background/cancel",
            json!({ "sessionId": "session-a", "taskId": "task-a" }),
        ),
        (
            "keencode/goal/upsert",
            json!({
                "sessionId": "session-a",
                "goal": {
                    "title": "实现 ACP",
                    "objective": "通过全部边界测试",
                    "progressPercent": 100
                },
                "expectedRevision": 0,
                "requestNonce": "nonce-a"
            }),
        ),
        (
            "keencode/goal/transition",
            json!({
                "sessionId": "session-a",
                "goalId": "goal-a",
                "status": "completed",
                "completionEvidence": "ACP 边界测试通过",
                "expectedRevision": 1,
                "requestNonce": "nonce-completed"
            }),
        ),
        (
            "keencode/goal/transition",
            json!({
                "sessionId": "session-a",
                "goalId": "goal-a",
                "status": "blocked",
                "reason": "等待外部状态",
                "expectedRevision": 1,
                "requestNonce": "nonce-b"
            }),
        ),
    ];
    for (method, params) in valid {
        assert!(
            decoder.decode_request(method, params).is_ok(),
            "{method} 应解码"
        );
    }
}

/// MCP 请求必须显式区分项目作用域；只有 list 允许缺失路径表示全局只读视图。
#[test]
fn mcp_requests_are_project_scoped_and_mcp_list_has_global_read_only_shape() {
    let decoder = AcpRequestDecoder::new();
    for params in [json!({}), json!({ "projectPath": null })] {
        let request = decoder
            .decode_request("keencode/mcp/list", params)
            .expect("MCP 全局列表请求应解码");
        assert!(matches!(
            request,
            AcpRequest::McpList(McpListRequest { project_path: None })
        ));
    }

    let list = decoder
        .decode_request(
            "keencode/mcp/list",
            json!({ "projectPath": "D:/projects/active" }),
        )
        .expect("MCP 项目列表请求应解码");
    assert!(matches!(
        list,
        AcpRequest::McpList(McpListRequest {
            project_path: Some(path)
        }) if path == "D:/projects/active"
    ));

    for method in ["keencode/mcp/oauth_start", "keencode/mcp/oauth_cancel"] {
        assert!(
            decoder
                .decode_request(
                    method,
                    json!({
                        "projectPath": "D:/projects/active",
                        "serverName": "docs"
                    })
                )
                .is_ok(),
            "{method} 应接受项目作用域请求"
        );
        assert!(
            decoder
                .decode_request(method, json!({ "serverName": "docs" }))
                .is_err()
        );
    }

    let callback = decoder
        .decode_request(
            "keencode/mcp/oauth_callback",
            json!({
                "projectPath": "D:/projects/active",
                "serverName": "docs",
                "code": "one-time-code",
                "state": "csrf-state"
            }),
        )
        .expect("OAuth 回调应接受项目作用域请求");
    assert!(matches!(callback, AcpRequest::McpOAuthCallback(_)));

    let server_request = McpOAuthServerRequest {
        project_path: "D:/projects/active".to_owned(),
        server_name: "docs".to_owned(),
    };
    let debug = format!("{server_request:?}");
    assert!(!debug.contains("D:/projects/active"));
    assert!(!debug.contains("docs"));

    let callback_request = McpOAuthCallbackRequest {
        project_path: "D:/projects/active".to_owned(),
        server_name: "docs".to_owned(),
        code: "one-time-code".to_owned(),
        state: "csrf-state".to_owned(),
    };
    let debug = format!("{callback_request:?}");
    for sensitive in ["D:/projects/active", "docs", "one-time-code", "csrf-state"] {
        assert!(!debug.contains(sensitive));
    }
}

/// MCP OAuth 通知必须使用固定 JSON-RPC 方法、严格字段和项目路径。
#[test]
fn mcp_oauth_notification_rejects_wrong_envelope_and_missing_project_path() {
    let event = McpOAuthEvent::Authorized {
        project_path: "D:/projects/active".to_owned(),
        server_name: "docs".to_owned(),
    };
    let notification = McpOAuthNotification::new(event).expect("OAuth 通知应创建");
    let raw = notification.encode().expect("OAuth 通知应编码");
    assert_eq!(
        McpOAuthNotification::decode_raw(&raw).expect("OAuth 通知应恢复"),
        notification
    );

    for raw in [
        br#"{"jsonrpc":"1.0","method":"keencode/mcp/oauth","params":{"type":"mcp_oauth_authorized","projectPath":"D:/projects/active","serverName":"docs"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"keencode/mcp/other","params":{"type":"mcp_oauth_authorized","projectPath":"D:/projects/active","serverName":"docs"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"keencode/mcp/oauth","params":{"type":"mcp_oauth_authorized","projectPath":"D:/projects/active","serverName":"docs"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"keencode/mcp/oauth","params":{"type":"mcp_oauth_authorized","serverName":"docs"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"keencode/mcp/oauth","params":{"type":"mcp_oauth_authorized","projectPath":"D:/projects/active","serverName":"docs","extra":true}}"#.as_slice(),
    ] {
        assert!(McpOAuthNotification::decode_raw(raw).is_err());
    }

    let missing_path = McpOAuthEvent::Authorized {
        project_path: String::new(),
        server_name: "docs".to_owned(),
    };
    assert!(McpOAuthNotification::new(missing_path).is_err());
    assert!(
        McpOAuthEvent::Authorized {
            project_path: "x".repeat(4097),
            server_name: "docs".to_owned(),
        }
        .validate()
        .is_err()
    );
    assert!(
        McpOAuthEvent::Authorized {
            project_path: "D:/projects/active\u{0000}".to_owned(),
            server_name: "docs".to_owned(),
        }
        .validate()
        .is_err()
    );
}

#[test]
fn elicitation_routes_through_negotiated_capability_and_standard_schema_enums() {
    let capabilities = ClientCapabilities::new().elicitation(Some(
        ElicitationCapabilities::new()
            .form(Some(ElicitationFormCapabilities::new()))
            .url(Some(ElicitationUrlCapabilities::new())),
    ));
    let router = ElicitationRouter::from_client_capabilities(&capabilities);
    assert!(router.supports_form());
    assert!(router.supports_url());

    let form = CreateElicitationRequest::new(
        ElicitationFormMode::new(
            ElicitationSessionScope::new("session-a"),
            ElicitationSchema::new(),
        ),
        "请选择执行方式",
    );
    let routed = router
        .route_create_request(form)
        .expect("已声明表单能力应路由");
    assert!(matches!(routed, AgentRequest::CreateElicitationRequest(_)));
    assert_eq!(routed.method(), "elicitation/create");

    let response = ElicitationRouter::route_create_response(CreateElicitationResponse::new(
        ElicitationAction::Decline,
    ));
    assert!(matches!(
        response,
        ClientResponse::CreateElicitationResponse(_)
    ));

    let completion = router
        .route_complete_notification(CompleteElicitationNotification::new("elicit-a"))
        .expect("已声明 URL 能力应路由完成通知");
    assert!(matches!(
        completion,
        AgentNotification::CompleteElicitationNotification(_)
    ));
    assert_eq!(completion.method(), "elicitation/complete");
}

#[test]
fn elicitation_client_request_encoder_emits_a_bounded_standard_frame() {
    let capabilities = ClientCapabilities::new().elicitation(Some(
        ElicitationCapabilities::new().form(Some(ElicitationFormCapabilities::new())),
    ));
    let router = ElicitationRouter::from_client_capabilities(&capabilities);
    let frame = AcpClientRequestEncoder::new()
        .elicitation_request_frame(
            RequestId::Str("elicitation-request-a".to_owned()),
            &router,
            CreateElicitationRequest::new(
                ElicitationFormMode::new(
                    ElicitationSessionScope::new("session-a"),
                    ElicitationSchema::new(),
                ),
                "请选择执行方式",
            ),
        )
        .expect("已声明能力的表单请求应编码");
    let value = serde_json::to_value(frame).expect("标准 Client Request 应可序列化");
    assert_eq!(value["jsonrpc"], json!("2.0"));
    assert_eq!(value["id"], json!("elicitation-request-a"));
    assert_eq!(value["method"], json!("elicitation/create"));
    assert_eq!(value["params"]["message"], json!("请选择执行方式"));

    let unavailable = ElicitationRouter::from_client_capabilities(&ClientCapabilities::new());
    let rejected = AcpClientRequestEncoder::new().elicitation_request_frame(
        RequestId::Str("elicitation-request-b".to_owned()),
        &unavailable,
        CreateElicitationRequest::new(
            ElicitationFormMode::new(
                ElicitationSessionScope::new("session-a"),
                ElicitationSchema::new(),
            ),
            "请选择执行方式",
        ),
    );
    assert!(matches!(
        rejected,
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    ));
}

#[test]
fn elicitation_client_response_decoder_accepts_only_strict_typed_results() {
    let decoder = AcpResponseDecoder::new();
    let decoded = decoder
        .decode_result::<CreateElicitationResponse>(
            br#"{
                "jsonrpc":"2.0",
                "id":"elicitation-request-a",
                "result":{
                    "action":"accept",
                    "content":{"target":"server","checks":["test"]}
                }
            }"#,
        )
        .expect("标准 Elicitation 成功响应应严格解码");
    let (request_id, response) = decoded.into_parts();
    assert_eq!(
        request_id,
        RequestId::Str("elicitation-request-a".to_owned())
    );
    let ElicitationAction::Accept(accepted) = response.action else {
        panic!("成功响应必须保留 accept 动作");
    };
    let content = accepted.content.expect("accept 响应应保留表单内容");
    assert_eq!(
        serde_json::to_value(content).expect("表单内容应可序列化"),
        json!({"target": "server", "checks": ["test"]})
    );

    for invalid in [
        br#"{"jsonrpc":"2.0","id":"elicitation-request-a","result":{"action":"unknown"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":"elicitation-request-a","result":{"action":"cancel","legacy":true}}"#.as_slice(),
    ] {
        assert!(
            matches!(
                decoder.decode_result::<CreateElicitationResponse>(invalid),
                Err(AcpBoundaryError::InvalidResponse)
            ),
            "Elicitation 响应不得接受未知动作或字段"
        );
    }
}

#[test]
fn elicitation_rejects_unadvertised_mode() {
    let router = ElicitationRouter::from_client_capabilities(&ClientCapabilities::new());
    let request = CreateElicitationRequest::new(
        ElicitationUrlMode::new(
            ElicitationSessionScope::new("session-a"),
            "elicit-a",
            "https://example.invalid/approve",
        ),
        "打开授权页面",
    );
    assert!(matches!(
        router.route_create_request(request),
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    ));
    assert!(matches!(
        router.route_complete_notification(CompleteElicitationNotification::new("elicit-a")),
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    ));
}

#[test]
fn elicitation_bounds_form_scope_schema_and_accepts_only_safe_urls() {
    let capabilities = ClientCapabilities::new().elicitation(Some(
        ElicitationCapabilities::new()
            .form(Some(ElicitationFormCapabilities::new()))
            .url(Some(ElicitationUrlCapabilities::new())),
    ));
    let router = ElicitationRouter::from_client_capabilities(&capabilities);

    for url in [
        "https://example.invalid/approve",
        "http://localhost:8080/approve",
        "http://127.0.0.1/approve",
        "http://[::1]/approve",
    ] {
        router
            .route_create_request(CreateElicitationRequest::new(
                ElicitationUrlMode::new(ElicitationSessionScope::new("session-a"), "elicit-a", url),
                "打开授权页面",
            ))
            .unwrap_or_else(|_| panic!("安全 URL {url} 应通过"));
    }
    for url in [
        "http://example.invalid/approve",
        "ftp://example.invalid/approve",
        "https://user:password@example.invalid/approve",
        "not-a-url",
    ] {
        assert!(matches!(
            router.route_create_request(CreateElicitationRequest::new(
                ElicitationUrlMode::new(
                    ElicitationSessionScope::new("session-a"),
                    "elicit-a",
                    url,
                ),
                "打开授权页面",
            )),
            Err(AcpBoundaryError::InvalidSemanticValue)
        ));
    }

    let oversized_scope = "s".repeat(257);
    assert!(matches!(
        router.route_create_request(CreateElicitationRequest::new(
            ElicitationFormMode::new(
                ElicitationSessionScope::new(oversized_scope),
                ElicitationSchema::new(),
            ),
            "请选择",
        )),
        Err(AcpBoundaryError::InvalidIdentifier)
    ));

    let mut inconsistent_schema = ElicitationSchema::new().string("name", false);
    inconsistent_schema.required = Some(vec!["missing".to_owned()]);
    assert!(matches!(
        router.route_create_request(CreateElicitationRequest::new(
            ElicitationFormMode::new(
                ElicitationSessionScope::new("session-a"),
                inconsistent_schema,
            ),
            "请填写",
        )),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));

    let mut oversized_schema = ElicitationSchema::new();
    for index in 0..129 {
        oversized_schema = oversized_schema.string(format!("field-{index}"), false);
    }
    assert!(matches!(
        router.route_create_request(CreateElicitationRequest::new(
            ElicitationFormMode::new(ElicitationSessionScope::new("session-a"), oversized_schema,),
            "请填写",
        )),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
}

#[test]
fn session_update_delivery_golden_round_trip_preserves_standard_update() {
    let envelope = SessionUpdateDeliveryEnvelope::new(
        "session-a",
        Some("turn-a".to_owned()),
        Some("agent-root".to_owned()),
        9,
        1_700_000_000_000,
        agent_message_update("已完成"),
    )
    .expect("标准 Session 更新投递应有效");
    let value = serde_json::to_value(&envelope).expect("投递信封应序列化");
    assert_eq!(
        value,
        json!({
            "schemaVersion": SESSION_UPDATE_DELIVERY_SCHEMA_VERSION,
            "sessionId": "session-a",
            "turnId": "turn-a",
            "sourceAgentId": "agent-root",
            "deliverySequence": 9,
            "occurredAtMs": 1_700_000_000_000_u64,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "已完成" }
            }
        })
    );
    let raw = serde_json::to_vec(&value).expect("合法投递应序列化");
    let restored =
        SessionUpdateDeliveryEnvelope::decode_raw(&raw).expect("合法投递应通过严格恢复入口");
    assert_eq!(restored, envelope);
    assert_eq!(restored.schema_version(), 1);
    assert_eq!(restored.session_id(), "session-a");
    assert_eq!(restored.turn_id(), Some("turn-a"));
    assert_eq!(restored.source_agent_id(), Some("agent-root"));
    assert_eq!(restored.delivery_sequence(), 9);
    assert_eq!(restored.occurred_at_ms(), 1_700_000_000_000);
    assert!(matches!(
        restored.update(),
        SessionUpdate::AgentMessageChunk(_)
    ));

    let usage = SessionUpdateDeliveryEnvelope::new(
        "session-a",
        Some("turn-a".to_owned()),
        Some("agent-root".to_owned()),
        10,
        1_700_000_000_001,
        SessionUpdate::UsageUpdate(UsageUpdate::new(12, 128)),
    )
    .expect("开启的标准 Usage 更新应可投递");
    assert_eq!(
        serde_json::to_value(usage).expect("Usage 投递应序列化")["update"]["sessionUpdate"],
        "usage_update"
    );
}

#[test]
fn session_update_delivery_rejects_unpaired_identity_unknown_version_and_zero_fields() {
    let base = json!({
        "schemaVersion": 1,
        "sessionId": "session-a",
        "turnId": "turn-a",
        "sourceAgentId": "agent-root",
        "deliverySequence": 1,
        "occurredAtMs": 1_700_000_000_000_u64,
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" }
        }
    });
    let mut invalid = Vec::new();
    let mut version = base.clone();
    version["schemaVersion"] = json!(2);
    invalid.push(version);
    let mut sequence = base.clone();
    sequence["deliverySequence"] = json!(0);
    invalid.push(sequence);
    let mut time = base.clone();
    time["occurredAtMs"] = json!(0);
    invalid.push(time);
    let mut missing_turn = base.clone();
    missing_turn
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("turnId");
    invalid.push(missing_turn);
    let mut missing_agent = base.clone();
    missing_agent
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("sourceAgentId");
    invalid.push(missing_agent);
    let mut missing_both = base;
    missing_both
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("turnId");
    missing_both
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("sourceAgentId");
    invalid.push(missing_both);
    for value in invalid {
        let raw = serde_json::to_vec(&value).expect("非法投递样本应序列化");
        assert!(SessionUpdateDeliveryEnvelope::decode_raw(&raw).is_err());
    }
}

#[test]
fn session_update_delivery_rejects_unknown_fields_old_alias_and_duplicate_keys() {
    let base = json!({
        "schemaVersion": 1,
        "sessionId": "session-a",
        "deliverySequence": 1,
        "occurredAtMs": 1,
        "update": { "sessionUpdate": "current_mode_update", "currentModeId": "plan" }
    });
    let mut unknown_envelope = base.clone();
    unknown_envelope["unexpected"] = json!(true);
    let mut unknown_update = base.clone();
    unknown_update["update"]["unexpected"] = json!(true);
    let mut old_alias = base;
    old_alias
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("deliverySequence");
    old_alias["sequence"] = json!(1);
    for value in [unknown_envelope, unknown_update, old_alias] {
        let raw = serde_json::to_vec(&value).expect("未知字段样本应序列化");
        assert!(matches!(
            SessionUpdateDeliveryEnvelope::decode_raw(&raw),
            Err(AcpBoundaryError::InvalidParams)
        ));
    }
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","sessionId":"session-b","deliverySequence":1,"occurredAtMs":1,"update":{"sessionUpdate":"current_mode_update","currentModeId":"plan"}}"#,
        ),
        Err(AcpBoundaryError::DuplicateJsonKey)
    ));
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::new(
            "session-a",
            Some("turn-a".to_owned()),
            Some("agent-root".to_owned()),
            1,
            1,
            SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new("plan")),
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","turnId":null,"sourceAgentId":null,"deliverySequence":1,"occurredAtMs":1,"update":{"sessionUpdate":"current_mode_update","currentModeId":"plan"}}"#,
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
}

#[test]
fn session_update_delivery_treats_plan_as_session_scoped() {
    let plan = SessionUpdate::Plan(Plan::new(Vec::new()));
    SessionUpdateDeliveryEnvelope::new("session-a", None, None, 1, 1, plan.clone())
        .expect("Session 级 Plan 更新应省略 Turn 和 Agent 身份");
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::new(
            "session-a",
            Some("turn-a".to_owned()),
            Some("agent-root".to_owned()),
            2,
            2,
            plan,
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
}

/// 用户消息既支持历史独立消息的 Session 级投递，也支持当前 Turn 级投递。
#[test]
fn session_update_delivery_allows_both_user_message_scopes() {
    let update = SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::from("hello")));
    SessionUpdateDeliveryEnvelope::new("session-a", None, None, 1, 1, update.clone())
        .expect("无 Turn 的历史用户消息应可投递");
    SessionUpdateDeliveryEnvelope::new(
        "session-a",
        Some("turn-a".to_owned()),
        Some("agent-root".to_owned()),
        2,
        2,
        update,
    )
    .expect("当前 Turn 的用户消息应可投递");
}

#[test]
fn session_update_delivery_enforces_bytes_depth_and_node_limits() {
    let valid = serde_json::to_vec(
        &SessionUpdateDeliveryEnvelope::new(
            "session-a",
            Some("turn-a".to_owned()),
            Some("agent-root".to_owned()),
            1,
            1,
            agent_message_update("hello"),
        )
        .expect("投递信封应有效"),
    )
    .expect("投递信封应序列化");
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::decode_raw_with_limits(
            &valid,
            SessionUpdateDeliveryLimits::new(32, 8, 64).expect("字节上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooLarge { limit: 32 })
    ));
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::decode_raw_with_limits(
            &valid,
            SessionUpdateDeliveryLimits::new(4096, 1, 64).expect("深度上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooDeep { limit: 1 })
    ));
    assert!(matches!(
        SessionUpdateDeliveryEnvelope::decode_raw_with_limits(
            &valid,
            SessionUpdateDeliveryLimits::new(4096, 8, 4).expect("节点上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooManyNodes { limit: 4 })
    ));
    let limits = SessionUpdateDeliveryLimits::default();
    assert_eq!(limits.max_bytes(), 256 * 1024);
    assert_eq!(limits.max_depth(), 32);
    assert_eq!(limits.max_nodes(), 16_384);
}

#[test]
fn keencode_event_golden_round_trip_uses_independent_journal_and_delivery_sequences() {
    let envelope = KeenCodeEventEnvelope::new_authoritative(
        7,
        KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-a",
            "agent-child",
            3,
            1_700_000_000_000,
            KeenCodeEvent::AgentStatusChanged {
                agent_id: "agent-child".to_owned(),
                status: AgentLifecycleStatus::Completed,
            },
        ),
    )
    .expect("扩展事件应有效");
    let value = serde_json::to_value(&envelope).expect("扩展事件应序列化");
    assert_eq!(
        value,
        json!({
            "schemaVersion": 1,
            "sessionId": "session-a",
            "turnId": "turn-a",
            "sourceAgentId": "agent-child",
            "journalSequence": 7,
            "deliverySequence": 3,
            "occurredAtMs": 1_700_000_000_000_u64,
            "event": {
                "type": "agent_status_changed",
                "agentId": "agent-child",
                "status": "completed"
            }
        })
    );
    let raw = serde_json::to_vec(&value).expect("合法事件应序列化");
    let restored = KeenCodeEventEnvelope::decode_raw(&raw).expect("合法事件应通过原始恢复入口");
    assert_eq!(restored, envelope);
    assert_eq!(restored.journal_sequence(), Some(7));
    assert_eq!(restored.delivery_sequence(), 3);

    for (journal_sequence, delivery_sequence) in [(91, 1), (2, 999)] {
        let independent = KeenCodeEventEnvelope::new_authoritative(
            journal_sequence,
            KeenCodeEventEnvelopeParams::for_turn(
                "session-a",
                "turn-a",
                "agent-a",
                delivery_sequence,
                1,
                KeenCodeEvent::TurnCompleted,
            ),
        )
        .expect("两种序号的大小关系不应被推导");
        assert_eq!(independent.journal_sequence(), Some(journal_sequence));
        assert_eq!(independent.delivery_sequence(), delivery_sequence);
    }
}

#[test]
fn keencode_event_construction_requires_matching_journal_authority() {
    let authoritative = KeenCodeEventEnvelopeParams::for_turn(
        "session-a",
        "turn-a",
        "agent-a",
        1,
        1,
        KeenCodeEvent::TurnCompleted,
    );
    assert!(KeenCodeEventEnvelope::new_authoritative(1, authoritative.clone()).is_ok());
    assert!(matches!(
        KeenCodeEventEnvelope::new_authoritative(0, authoritative.clone()),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    assert!(matches!(
        KeenCodeEventEnvelope::new_transient(authoritative),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));

    let transient = KeenCodeEventEnvelopeParams::for_turn(
        "session-a",
        "turn-a",
        "agent-a",
        2,
        2,
        KeenCodeEvent::ModelFirstStreamObserved,
    );
    let envelope = KeenCodeEventEnvelope::new_transient(transient.clone())
        .expect("临时事件应通过无 Journal 构造入口");
    assert!(!envelope.is_authoritative());
    assert_eq!(envelope.journal_sequence(), None);
    assert!(
        serde_json::to_value(&envelope)
            .expect("临时事件应序列化")
            .get("journalSequence")
            .is_none()
    );
    assert!(matches!(
        KeenCodeEventEnvelope::new_authoritative(1, transient),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));

    assert!(KeenCodeEventEnvelope::decode_raw(
        br#"{"schemaVersion":1,"sessionId":"session-a","turnId":"turn-a","sourceAgentId":"agent-a","deliverySequence":1,"occurredAtMs":1,"event":{"type":"turn_completed"}}"#,
    )
    .is_err());
    assert!(KeenCodeEventEnvelope::decode_raw(
        br#"{"schemaVersion":1,"sessionId":"session-a","turnId":"turn-a","sourceAgentId":"agent-a","journalSequence":1,"deliverySequence":1,"occurredAtMs":1,"event":{"type":"model_first_stream_observed"}}"#,
    )
    .is_err());
    assert!(KeenCodeEventEnvelope::decode_raw(
        br#"{"schemaVersion":1,"sessionId":"session-a","turnId":"turn-a","sourceAgentId":"agent-a","journalSequence":null,"deliverySequence":1,"occurredAtMs":1,"event":{"type":"model_first_stream_observed"}}"#,
    )
    .is_err());
}

#[test]
fn keencode_event_raw_restore_rejects_version_sequences_alias_unknown_and_identity_errors() {
    let base = json!({
        "schemaVersion": 1,
        "sessionId": "session-a",
        "turnId": "turn-a",
        "sourceAgentId": "agent-a",
        "journalSequence": 2,
        "deliverySequence": 1,
        "occurredAtMs": 1_700_000_000_000_u64,
        "event": { "type": "agent_status_changed", "agentId": "agent-a", "status": "running" }
    });
    let mut invalid = Vec::new();
    let mut version = base.clone();
    version["schemaVersion"] = json!(2);
    invalid.push(version);
    let mut journal_sequence = base.clone();
    journal_sequence["journalSequence"] = json!(0);
    invalid.push(journal_sequence);
    let mut delivery_sequence = base.clone();
    delivery_sequence["deliverySequence"] = json!(0);
    invalid.push(delivery_sequence);
    let mut old_alias = base.clone();
    old_alias
        .as_object_mut()
        .expect("基础样本应为对象")
        .remove("journalSequence");
    old_alias["sequence"] = json!(2);
    invalid.push(old_alias);
    let mut unknown = base.clone();
    unknown["unexpected"] = json!(true);
    invalid.push(unknown);
    let mut internal = base.clone();
    internal["event"]["unexpected"] = json!(true);
    invalid.push(internal);
    let mut cross = base;
    cross["sourceAgentId"] = json!("different-agent");
    invalid.push(cross);
    for value in invalid {
        let raw = serde_json::to_vec(&value).expect("非法事件样本应序列化");
        assert!(KeenCodeEventEnvelope::decode_raw(&raw).is_err());
    }
    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","turnId":null,"sourceAgentId":null,"deliverySequence":1,"occurredAtMs":1,"event":{"type":"recovery_state_changed","state":"ready"}}"#,
        ),
        Err(AcpBoundaryError::InvalidParams)
    ));
}

#[test]
fn turn_started_identity_rejects_self_parent_and_child_claiming_itself_as_root() {
    let root = KeenCodeEventEnvelope::new_authoritative(
        1,
        KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-root",
            "agent-root",
            1,
            1,
            KeenCodeEvent::TurnStarted {
                root_turn_id: "turn-root".to_owned(),
                parent_turn_id: None,
            },
        ),
    )
    .expect("根 Turn 应允许把自己声明为根且不携带父 Turn");
    assert_eq!(root.turn_id(), Some("turn-root"));

    let child = KeenCodeEventEnvelope::new_authoritative(
        2,
        KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-child",
            "agent-child",
            2,
            2,
            KeenCodeEvent::TurnStarted {
                root_turn_id: "turn-root".to_owned(),
                parent_turn_id: Some("turn-root".to_owned()),
            },
        ),
    )
    .expect("子 Turn 应绑定不同的根和直接父 Turn");
    assert_eq!(child.turn_id(), Some("turn-child"));

    for event in [
        KeenCodeEvent::TurnStarted {
            root_turn_id: "turn-root".to_owned(),
            parent_turn_id: Some("turn-child".to_owned()),
        },
        KeenCodeEvent::TurnStarted {
            root_turn_id: "turn-child".to_owned(),
            parent_turn_id: Some("turn-root".to_owned()),
        },
        KeenCodeEvent::TurnStarted {
            root_turn_id: "turn-root".to_owned(),
            parent_turn_id: Some("turn-intermediate".to_owned()),
        },
    ] {
        assert!(matches!(
            KeenCodeEventEnvelope::new_authoritative(
                2,
                KeenCodeEventEnvelopeParams::for_turn(
                    "session-a",
                    "turn-child",
                    "agent-child",
                    2,
                    2,
                    event,
                ),
            ),
            Err(AcpBoundaryError::InvalidSemanticValue)
        ));
    }
}

#[test]
fn agent_spawned_requires_root_parent_turn_for_single_level_agents() {
    let valid = KeenCodeEvent::AgentSpawned {
        agent_id: "agent-child".to_owned(),
        parent_agent_id: "agent-root".to_owned(),
        agent_path: "root/child".to_owned(),
        task: "检查子任务".to_owned(),
        parent_turn_id: "turn-root".to_owned(),
        root_turn_id: "turn-root".to_owned(),
    };
    KeenCodeEventEnvelope::new_authoritative(
        1,
        KeenCodeEventEnvelopeParams::for_turn("session-a", "turn-root", "agent-root", 1, 1, valid),
    )
    .expect("根 Agent 应能从根 Turn 创建单层子 Agent");

    let invalid = KeenCodeEvent::AgentSpawned {
        agent_id: "agent-grandchild".to_owned(),
        parent_agent_id: "agent-child".to_owned(),
        agent_path: "root/child/grandchild".to_owned(),
        task: "继续检查".to_owned(),
        parent_turn_id: "turn-child".to_owned(),
        root_turn_id: "turn-root".to_owned(),
    };
    assert!(matches!(
        KeenCodeEventEnvelope::new_authoritative(
            2,
            KeenCodeEventEnvelopeParams::for_turn(
                "session-a",
                "turn-child",
                "agent-child",
                2,
                2,
                invalid,
            ),
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
}

/// Agent 委派任务的协议边界必须与协作工具一致，并能容纳完整事件信封。
#[test]
fn agent_spawned_task_accepts_boundary_and_rejects_overflow() {
    // 协作工具对初始任务正文使用的 UTF-8 字节上限。
    const MAX_TASK_BYTES: usize = 256 * 1024;
    let boundary_task = "a".repeat(MAX_TASK_BYTES);
    let envelope = KeenCodeEventEnvelope::new_authoritative(
        2,
        KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-root",
            "agent-root",
            1,
            1,
            KeenCodeEvent::AgentSpawned {
                agent_id: "agent-child".to_owned(),
                parent_agent_id: "agent-root".to_owned(),
                agent_path: "root/child".to_owned(),
                task: boundary_task.clone(),
                parent_turn_id: "turn-root".to_owned(),
                root_turn_id: "turn-root".to_owned(),
            },
        ),
    )
    .expect("最大任务正文应能构造事件");
    let raw = serde_json::to_vec(&envelope).expect("最大任务事件应能编码");
    assert!(
        raw.len() <= KeenCodeEventLimits::default().max_bytes(),
        "最大任务正文连同事件信封也必须落在默认恢复边界内"
    );
    assert_eq!(
        KeenCodeEventEnvelope::decode_raw(&raw).expect("最大任务事件应能恢复"),
        envelope
    );

    let oversized_task = format!("{boundary_task}a");
    assert_eq!(
        KeenCodeEventEnvelope::new_authoritative(
            2,
            KeenCodeEventEnvelopeParams::for_turn(
                "session-a",
                "turn-root",
                "agent-root",
                1,
                1,
                KeenCodeEvent::AgentSpawned {
                    agent_id: "agent-child".to_owned(),
                    parent_agent_id: "agent-root".to_owned(),
                    agent_path: "root/child".to_owned(),
                    task: oversized_task,
                    parent_turn_id: "turn-root".to_owned(),
                    root_turn_id: "turn-root".to_owned(),
                },
            ),
        )
        .expect_err("超过任务正文边界必须拒绝"),
        AcpBoundaryError::InvalidSemanticValue
    );
}

#[test]
fn keencode_event_raw_restore_enforces_bytes_duplicates_depth_nodes_and_compaction_order() {
    let valid = serde_json::to_vec(
        &KeenCodeEventEnvelope::new_authoritative(
            2,
            KeenCodeEventEnvelopeParams::for_turn(
                "session-a",
                "turn-a",
                "agent-a",
                99,
                1,
                KeenCodeEvent::ContextCompactionCompleted {
                    replaced_through_sequence: 1,
                    estimated_tokens: 10,
                },
            ),
        )
        .expect("压缩事件应有效"),
    )
    .expect("压缩事件应序列化");
    KeenCodeEventEnvelope::decode_raw(&valid).expect("压缩覆盖序号早于 Journal 序号时应恢复");
    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","sessionId":"session-b","turnId":"turn-a","sourceAgentId":"agent-a","journalSequence":2,"deliverySequence":99,"occurredAtMs":1,"event":{"type":"turn_completed"}}"#,
        ),
        Err(AcpBoundaryError::DuplicateJsonKey)
    ));
    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw_with_limits(
            &valid,
            KeenCodeEventLimits::new(32, 8, 64).expect("事件字节上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooLarge { limit: 32 })
    ));
    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw_with_limits(
            &valid,
            KeenCodeEventLimits::new(4096, 1, 64).expect("事件深度上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooDeep { limit: 1 })
    ));
    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw_with_limits(
            &valid,
            KeenCodeEventLimits::new(4096, 8, 4).expect("事件节点上限有效"),
        ),
        Err(AcpBoundaryError::PayloadTooManyNodes { limit: 4 })
    ));
    for replaced in [2_u64, 3] {
        assert!(matches!(
            KeenCodeEventEnvelope::new_authoritative(
                2,
                KeenCodeEventEnvelopeParams::for_turn(
                    "session-a",
                    "turn-a",
                    "agent-a",
                    1,
                    1,
                    KeenCodeEvent::ContextCompactionCompleted {
                        replaced_through_sequence: replaced,
                        estimated_tokens: 10,
                    },
                ),
            ),
            Err(AcpBoundaryError::InvalidSemanticValue)
        ));
    }
}

#[test]
fn session_scoped_recovery_event_rejects_turn_identity() {
    assert!(matches!(
        KeenCodeEventEnvelope::new_transient(KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-a",
            "agent-a",
            1,
            1_700_000_000_000,
            KeenCodeEvent::RecoveryStateChanged {
                state: RecoveryState::Ready,
            },
        )),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
    assert!(
        serde_json::from_value::<KeenCodeEvent>(json!({
            "type": "goal_changed",
            "goalId": "goal-a",
            "revision": 0,
            "status": "active"
        }))
        .is_err(),
        "直接反序列化事件也必须执行内部语义校验"
    );
}

/// 系统通知只接受完整 Session 身份或完整 Turn 身份，不能接受部分身份。
#[test]
fn system_notification_accepts_session_or_turn_identity_but_not_partial_identity() {
    let event = KeenCodeEvent::SystemNotification {
        level: SystemNotificationLevel::Info,
        message: "准备恢复 Session".to_owned(),
    };
    assert!(
        KeenCodeEventEnvelope::new_transient(KeenCodeEventEnvelopeParams::for_session(
            "session-a",
            1,
            1,
            event.clone()
        ),)
        .is_ok()
    );
    assert!(
        KeenCodeEventEnvelope::new_transient(KeenCodeEventEnvelopeParams::for_turn(
            "session-a",
            "turn-a",
            "agent-a",
            2,
            2,
            event
        ),)
        .is_ok()
    );

    assert!(matches!(
        KeenCodeEventEnvelope::decode_raw(
            br#"{"schemaVersion":1,"sessionId":"session-a","turnId":"turn-a","deliverySequence":3,"occurredAtMs":3,"event":{"type":"system_notification","level":"warning","message":"identity-incomplete"}}"#,
        ),
        Err(AcpBoundaryError::InvalidSemanticValue)
    ));
}

#[test]
fn debug_output_never_contains_prompt_goal_or_oauth_contents() {
    let decoder = AcpRequestDecoder::new();
    let prompt = decoder
        .decode_request(
            "session/prompt",
            json!({
                "sessionId": "session-a",
                "prompt": [{ "type": "text", "text": "sensitive-prompt" }]
            }),
        )
        .expect("标准 Prompt 应解码");
    assert_eq!(
        format!("{prompt:?}"),
        "AcpRequest { method: \"session/prompt\" }"
    );
    assert!(!format!("{prompt:?}").contains("sensitive-prompt"));

    let oauth = decoder
        .decode_request(
            "keencode/mcp/oauth_callback",
            json!({
                "projectPath": "D:/projects/active",
                "serverName": "server-a",
                "code": "secret-code",
                "state": "secret-state"
            }),
        )
        .expect("OAuth 回调应解码");
    let debug = format!("{oauth:?}");
    assert!(!debug.contains("secret-code"));
    assert!(!debug.contains("secret-state"));
}

#[test]
fn invalid_limit_configurations_are_rejected() {
    assert_eq!(
        AcpRequestLimits::new(0, 1, 1, 1),
        Err(AcpBoundaryError::InvalidLimits)
    );
    assert_eq!(
        KeenCodeEventLimits::new(1024, 0, 32),
        Err(AcpBoundaryError::InvalidLimits)
    );
    assert_eq!(
        AcpResponseLimits::new(1024, 0, 32),
        Err(AcpBoundaryError::InvalidLimits)
    );
}
