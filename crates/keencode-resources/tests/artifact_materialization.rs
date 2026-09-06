use std::fs;
use std::sync::Arc;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, ArtifactId, ArtifactLimits, ArtifactMaterialization, ArtifactMaterialized,
    ArtifactStore, ArtifactUse, Durability, IdempotentAppendOutcome, JournalConfig, MessagePart,
    MessageRole, PersistedToolResult, RequestId, ResourceError, SessionEvent, SessionEventId,
    SessionId, SessionJournal, SessionMessage, SessionOpen, SessionState, SnapshotPolicy,
    ToolCompletionStatus, ToolEffect, ToolOutcome, ToolRequest, ToolResultPart, TranscriptSegment,
    TurnId, side_effect_unknown_result,
};
use serde_json::json;
use tempfile::TempDir;

/// 返回关闭自动 Snapshot 的 Artifact 恢复测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开带真实 Artifact 校验器并初始化 Running Turn 的 Session。
fn running_journal(
    root: &std::path::Path,
    session: &str,
) -> (SessionJournal, Arc<ArtifactStore>, TurnId, AgentId) {
    let session_id = SessionId::new(session).expect("Session ID 应有效");
    let artifacts = Arc::new(
        ArtifactStore::open(root, session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应打开"),
    );
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        session_id,
        config(),
        artifacts.clone(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    };
    append(
        &journal,
        "event-create",
        SessionEvent::SessionCreated {
            title: "Artifact 测试".to_owned(),
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
            prompt_summary: "恢复大结果".to_owned(),
        },
    );
    (
        journal,
        artifacts,
        turn_id,
        AgentId::new("root").expect("Agent ID 应有效"),
    )
}

/// 使用当前 sequence 和显式测试标识提交一条事件。
fn append(journal: &SessionJournal, event_id: &str, event: SessionEvent) {
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
                requested_model: "artifact-test-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some("artifact-test-response".to_owned()),
                    model: Some("artifact-test-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason: StopReason::Completed,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 构造引用指定 Artifact 的首个 Assistant Transcript 段及其模型 Round 完成事件。
fn artifact_segment(turn_id: &TurnId, agent_id: &AgentId, artifact: ArtifactUse) -> SessionEvent {
    model_round_batch(
        turn_id,
        agent_id,
        TranscriptSegment {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            model_round: 1,
            segment_index: 0,
            expected_transcript_revision: 0,
            messages: vec![SessionMessage {
                message_id: "message-artifact".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: Some(agent_id.clone()),
                role: MessageRole::Assistant,
                content: vec![
                    MessagePart::Text {
                        text: "二进制结果已保存用于审计".to_owned(),
                    },
                    MessagePart::Artifact {
                        artifact,
                        materialization: ArtifactMaterialization::Binary,
                    },
                ],
            }],
        },
    )
}

/// 为 materialization 校验创建一个已越过执行起点的工具请求。
fn started_tool(journal: &SessionJournal, turn_id: &TurnId, agent_id: &AgentId) -> RequestId {
    started_named_tool(journal, turn_id, agent_id, "call-artifact", 0)
}

/// 为指定 Provider 调用标识创建一个已越过执行起点的只读工具请求。
fn started_named_tool(
    journal: &SessionJournal,
    turn_id: &TurnId,
    agent_id: &AgentId,
    tool_call_id: &str,
    request_index: u32,
) -> RequestId {
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        turn_id,
        agent_id,
        1,
        tool_call_id,
    )
    .expect("Request ID 应派生");
    let request_event_id = format!("event-tool-request-{tool_call_id}");
    append(
        journal,
        &request_event_id,
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id: agent_id.clone(),
                model_round: 1,
                request_index,
                model_tool_call_id: tool_call_id.to_owned(),
                tool_name: "read_binary".to_owned(),
                arguments: json!({"path": "asset.bin"}),
                effect: ToolEffect::ReadOnly,
            },
        },
    );
    let start_event_id = format!("event-tool-start-{tool_call_id}");
    append(
        journal,
        &start_event_id,
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    request_id
}

/// 构造带指定恢复类型的工具完成事件。
fn completed_with_artifact(
    request_id: &RequestId,
    artifact: ArtifactUse,
    materialization: ArtifactMaterialization,
) -> SessionEvent {
    SessionEvent::ToolCompleted {
        request_id: request_id.clone(),
        outcome: ToolOutcome {
            status: ToolCompletionStatus::Succeeded,
            result: PersistedToolResult {
                tool_call_id: "call-artifact".to_owned(),
                content: vec![ToolResultPart::Artifact {
                    artifact,
                    materialization,
                }],
                is_error: false,
            },
        },
    }
}

/// 验证仅持有 ArtifactUse 时可在重启恢复路径读取完整字节和有界预览。
#[test]
fn artifact_use_can_be_read_and_previewed_without_artifact_ref() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("artifact-use-read").expect("Session ID 应有效");
    let store = ArtifactStore::open(
        root.path(),
        session_id.clone(),
        ArtifactLimits {
            max_preview_bytes: 4,
            ..ArtifactLimits::default()
        },
    )
    .expect("ArtifactStore 应打开");
    let reference = store
        .put("中文内容".as_bytes(), Some("text/plain".to_owned()))
        .expect("Artifact 应保存")
        .as_event_use();
    drop(store);

    let reopened = ArtifactStore::open(
        root.path(),
        session_id,
        ArtifactLimits {
            max_preview_bytes: 4,
            ..ArtifactLimits::default()
        },
    )
    .expect("ArtifactStore 应重开");
    assert_eq!(
        reopened.read_use(&reference).expect("完整字节应恢复"),
        "中文内容".as_bytes()
    );
    let preview = reopened.preview_use(&reference).expect("预览应恢复");
    assert_eq!(preview.text, "中");
    assert!(preview.truncated);
    assert!(preview.source_is_utf8);
}

/// 验证三种物化结果真实恢复，非 UTF-8 文本和同内容 MIME 重标注被拒绝。
#[test]
fn typed_materialization_validates_bytes_and_frozen_media_type() {
    let root = TempDir::new().expect("临时目录应创建");
    let store = ArtifactStore::open(
        root.path(),
        SessionId::new("artifact-typed-materialization").expect("Session ID 应有效"),
        ArtifactLimits::default(),
    )
    .expect("ArtifactStore 应打开");
    let text = store
        .put("可恢复文本".as_bytes(), Some("Text/Plain".to_owned()))
        .expect("文本 Artifact 应保存")
        .as_event_use();
    assert_eq!(text.media_type.as_deref(), Some("text/plain"));
    assert_eq!(
        store
            .materialize_use(&text, ArtifactMaterialization::Utf8Text)
            .expect("UTF-8 文本应恢复"),
        ArtifactMaterialized::Utf8Text("可恢复文本".to_owned())
    );

    let image_bytes = [0x89, b'P', b'N', b'G'];
    let image = store
        .put(&image_bytes, Some("image/png".to_owned()))
        .expect("图片 Artifact 应保存")
        .as_event_use();
    assert_eq!(
        store
            .materialize_use(&image, ArtifactMaterialization::Image)
            .expect("图片应恢复"),
        ArtifactMaterialized::Image {
            bytes: image_bytes.to_vec(),
            media_type: "image/png".to_owned(),
        }
    );

    let binary_bytes = [0xff, 0x00, 0x01];
    let binary = store
        .put(&binary_bytes, Some("application/octet-stream".to_owned()))
        .expect("二进制 Artifact 应保存")
        .as_event_use();
    assert_eq!(
        store
            .materialize_use(&binary, ArtifactMaterialization::Binary)
            .expect("二进制应恢复"),
        ArtifactMaterialized::Binary {
            bytes: binary_bytes.to_vec(),
            media_type: Some("application/octet-stream".to_owned()),
        }
    );
    assert!(matches!(
        store.materialize_use(&binary, ArtifactMaterialization::Utf8Text),
        Err(ResourceError::ArtifactMaterializationMismatch {
            materialization: "utf8_text"
        })
    ));
    assert!(matches!(
        store.put(&binary_bytes, Some("text/plain".to_owned())),
        Err(ResourceError::ArtifactMediaTypeMismatch)
    ));
}

/// 验证 UTF-8 物化同时遵守文本 MIME、结构化后缀与 charset 约束。
#[test]
fn utf8_materialization_requires_textual_media_type_and_compatible_charset() {
    let root = TempDir::new().expect("临时目录应创建");
    let store = ArtifactStore::open(
        root.path(),
        SessionId::new("artifact-utf8-media-contract").expect("Session ID 应有效"),
        ArtifactLimits::default(),
    )
    .expect("ArtifactStore 应打开");

    for (bytes, media_type) in [
        (b"ascii octet stream".as_slice(), "application/octet-stream"),
        (b"<svg></svg>".as_slice(), "image/svg+xml"),
        (
            b"latin declaration".as_slice(),
            "text/plain;charset=iso-8859-1",
        ),
    ] {
        let artifact = store
            .put(bytes, Some(media_type.to_owned()))
            .expect("测试 Artifact 应保存")
            .as_event_use();
        assert!(matches!(
            store.materialize_use(&artifact, ArtifactMaterialization::Utf8Text),
            Err(ResourceError::ArtifactMaterializationMismatch {
                materialization: "utf8_text"
            })
        ));
    }

    for (bytes, media_type) in [
        ("UTF-8 文本".as_bytes(), "Text/Plain; Charset=\"UTF-8\""),
        (br#"{"ok":true}"#.as_slice(), "application/json"),
        (
            br#"{"title":"problem"}"#.as_slice(),
            "application/problem+json",
        ),
        (b"ASCII".as_slice(), "text/plain;charset=us-ascii"),
    ] {
        let artifact = store
            .put(bytes, Some(media_type.to_owned()))
            .expect("文本 Artifact 应保存")
            .as_event_use();
        assert!(matches!(
            store
                .materialize_use(&artifact, ArtifactMaterialization::Utf8Text)
                .expect("兼容 MIME 应恢复"),
            ArtifactMaterialized::Utf8Text(_)
        ));
    }

    let non_ascii_us_ascii = store
        .put(
            "非 ASCII".as_bytes(),
            Some("text/plain;charset=us-ascii".to_owned()),
        )
        .expect("字节应先按 Artifact 保存")
        .as_event_use();
    assert!(matches!(
        store.materialize_use(&non_ascii_us_ascii, ArtifactMaterialization::Utf8Text),
        Err(ResourceError::ArtifactMaterializationMismatch { .. })
    ));

    let without_media_type = store
        .put("无 MIME 文本".as_bytes(), None)
        .expect("无 MIME UTF-8 应保存")
        .as_event_use();
    assert!(matches!(
        store
            .materialize_use(&without_media_type, ArtifactMaterialization::Utf8Text)
            .expect("无 MIME UTF-8 应恢复"),
        ArtifactMaterialized::Utf8Text(_)
    ));
    let invalid_utf8_without_media_type = store
        .put(&[0xff, 0xfe, 0xfd], None)
        .expect("无 MIME 二进制应保存")
        .as_event_use();
    assert!(matches!(
        store.materialize_use(
            &invalid_utf8_without_media_type,
            ArtifactMaterialization::Utf8Text
        ),
        Err(ResourceError::ArtifactMaterializationMismatch { .. })
    ));
}

/// 验证文本与图片 materialization 必须和 Artifact 媒体类型一致。
#[test]
fn artifact_materialization_rejects_incompatible_media_types() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, artifacts, turn_id, agent_id) =
        running_journal(root.path(), "artifact-media-type");
    let request_id = started_tool(&journal, &turn_id, &agent_id);
    let image = artifacts
        .put(&[0x89, b'P', b'N', b'G'], Some("image/png".to_owned()))
        .expect("图片 Artifact 应保存")
        .as_event_use();
    let text = artifacts
        .put(b"plain text", Some("text/plain".to_owned()))
        .expect("文本 Artifact 应保存")
        .as_event_use();
    let baseline = journal.state().expect("状态应读取");

    let wrong_text = journal.append_idempotent(
        SessionEventId::new("event-wrong-text").expect("事件 ID 应有效"),
        baseline.last_sequence,
        completed_with_artifact(&request_id, image, ArtifactMaterialization::Utf8Text),
    );
    assert!(matches!(
        wrong_text,
        Err(ResourceError::ArtifactMaterializationMismatch { .. })
    ));
    let wrong_image = journal.append_idempotent(
        SessionEventId::new("event-wrong-image").expect("事件 ID 应有效"),
        baseline.last_sequence,
        completed_with_artifact(&request_id, text, ArtifactMaterialization::Image),
    );
    assert!(matches!(
        wrong_image,
        Err(ResourceError::ArtifactMaterializationMismatch { .. })
    ));
    assert_eq!(journal.state().expect("状态应读取"), baseline);
}

/// 验证图片 Artifact 保存并恢复的是原始字节，而不是 Base64 文本代理。
#[test]
fn image_materialization_round_trips_raw_bytes() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, artifacts, turn_id, agent_id) =
        running_journal(root.path(), "artifact-image-bytes");
    let request_id = started_tool(&journal, &turn_id, &agent_id);
    let raw_image = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let image = artifacts
        .put(&raw_image, Some("image/png".to_owned()))
        .expect("图片 Artifact 应保存")
        .as_event_use();
    append(
        &journal,
        "event-tool-complete",
        completed_with_artifact(&request_id, image.clone(), ArtifactMaterialization::Image),
    );
    assert_eq!(
        artifacts
            .materialize_use(&image, ArtifactMaterialization::Image)
            .expect("图片字节应恢复"),
        ArtifactMaterialized::Image {
            bytes: raw_image.to_vec(),
            media_type: "image/png".to_owned(),
        }
    );
}

/// 验证 JSON/XML 大工具结果经过真实 ArtifactStore、Journal 和纯重放后仍可完整物化。
#[test]
fn json_and_xml_tool_result_artifacts_append_replay_and_materialize() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "artifact-structured-tool-results";
    let (journal, artifacts, turn_id, agent_id) = running_journal(root.path(), session);
    let json_artifact = artifacts
        .put(
            br#"{"ok":true,"items":[1,2]}"#,
            Some("application/problem+json; charset=utf-8".to_owned()),
        )
        .expect("JSON Artifact 应保存")
        .as_event_use();
    let xml_artifact = artifacts
        .put(
            br#"<result><ok>true</ok></result>"#,
            Some("application/atom+xml".to_owned()),
        )
        .expect("XML Artifact 应保存")
        .as_event_use();
    let json_request = started_named_tool(&journal, &turn_id, &agent_id, "call-json", 0);
    let xml_request = started_named_tool(&journal, &turn_id, &agent_id, "call-xml", 1);
    let json_result = PersistedToolResult {
        tool_call_id: "call-json".to_owned(),
        content: vec![ToolResultPart::Artifact {
            artifact: json_artifact.clone(),
            materialization: ArtifactMaterialization::Utf8Text,
        }],
        is_error: false,
    };
    let xml_result = PersistedToolResult {
        tool_call_id: "call-xml".to_owned(),
        content: vec![ToolResultPart::Artifact {
            artifact: xml_artifact.clone(),
            materialization: ArtifactMaterialization::Utf8Text,
        }],
        is_error: false,
    };
    for (event_id, request_id, result) in [
        ("event-complete-json", json_request, json_result.clone()),
        ("event-complete-xml", xml_request, xml_result.clone()),
    ] {
        append(
            &journal,
            event_id,
            SessionEvent::ToolCompleted {
                request_id,
                outcome: ToolOutcome {
                    status: ToolCompletionStatus::Succeeded,
                    result,
                },
            },
        );
    }
    append(
        &journal,
        "event-structured-segment",
        model_round_batch(
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
                        message_id: "message-structured-calls".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Assistant,
                        content: vec![
                            MessagePart::ToolCall {
                                tool_call_id: "call-json".to_owned(),
                                tool_name: "read_binary".to_owned(),
                                arguments: json!({"path": "asset.bin"}),
                            },
                            MessagePart::ToolCall {
                                tool_call_id: "call-xml".to_owned(),
                                tool_name: "read_binary".to_owned(),
                                arguments: json!({"path": "asset.bin"}),
                            },
                        ],
                    },
                    SessionMessage {
                        message_id: "message-structured-results".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Tool,
                        content: vec![
                            MessagePart::ToolResult {
                                tool_call_id: "call-json".to_owned(),
                                content: json_result.content,
                                is_error: false,
                            },
                            MessagePart::ToolResult {
                                tool_call_id: "call-xml".to_owned(),
                                content: xml_result.content,
                                is_error: false,
                            },
                        ],
                    },
                ],
            },
        ),
    );
    drop(journal);
    drop(artifacts);

    let session_id = SessionId::new(session).expect("Session ID 应有效");
    let reopened_artifacts = Arc::new(
        ArtifactStore::open(root.path(), session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应重开"),
    );
    let reopened = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id,
        config(),
        reopened_artifacts.clone(),
    )
    .expect("Session 应重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("结构化结果不应损坏：{:?}", report.issues),
    };
    let effective = reopened
        .state()
        .expect("重放状态应读取")
        .effective_transcript(&agent_id)
        .expect("有效 Transcript 应恢复");
    let result_parts = effective
        .iter()
        .find(|message| message.message_id == "message-structured-results")
        .expect("工具结果消息应恢复")
        .content
        .iter()
        .flat_map(|part| match part {
            MessagePart::ToolResult { content, .. } => content.as_slice(),
            _ => &[],
        })
        .collect::<Vec<_>>();
    assert_eq!(result_parts.len(), 2);
    let materialized = result_parts
        .iter()
        .map(|part| match part {
            ToolResultPart::Artifact {
                artifact,
                materialization,
            } => reopened_artifacts
                .materialize_use(artifact, *materialization)
                .expect("重放 Artifact 应物化"),
            _ => panic!("结构化结果应保持 Artifact 引用"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        materialized,
        vec![
            ArtifactMaterialized::Utf8Text("{\"ok\":true,\"items\":[1,2]}".to_owned()),
            ArtifactMaterialized::Utf8Text("<result><ok>true</ok></result>".to_owned()),
        ]
    );
}

/// 验证顶层 Artifact 角色矩阵，以及 Binary 引用只保留在审计历史而不进入模型上下文。
#[test]
fn artifact_roles_and_binary_model_visibility_are_enforced() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, artifacts, turn_id, agent_id) =
        running_journal(root.path(), "artifact-role-visibility");
    let text_artifact = artifacts
        .put(br#"{"visible":true}"#, Some("application/json".to_owned()))
        .expect("文本 Artifact 应保存")
        .as_event_use();
    let image_artifact = artifacts
        .put(&[0x89, b'P', b'N', b'G'], Some("image/png".to_owned()))
        .expect("图片 Artifact 应保存")
        .as_event_use();
    let binary_artifact = artifacts
        .put(
            &[0xff, 0x00, 0x01],
            Some("application/octet-stream".to_owned()),
        )
        .expect("二进制 Artifact 应保存")
        .as_event_use();

    for (index, role) in [
        MessageRole::System,
        MessageRole::Developer,
        MessageRole::User,
        MessageRole::Assistant,
    ]
    .into_iter()
    .enumerate()
    {
        append(
            &journal,
            &format!("event-text-role-{index}"),
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: format!("message-text-role-{index}"),
                    turn_id: Some(turn_id.clone()),
                    agent_id: if role == MessageRole::Assistant {
                        Some(agent_id.clone())
                    } else {
                        None
                    },
                    role,
                    content: vec![MessagePart::Artifact {
                        artifact: text_artifact.clone(),
                        materialization: ArtifactMaterialization::Utf8Text,
                    }],
                },
            },
        );
    }
    append(
        &journal,
        "event-user-image-artifact",
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-user-image-artifact".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Artifact {
                    artifact: image_artifact.clone(),
                    materialization: ArtifactMaterialization::Image,
                }],
            },
        },
    );
    append(
        &journal,
        "event-audit-binary",
        SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "message-audit-binary".to_owned(),
                turn_id: Some(turn_id.clone()),
                agent_id: None,
                role: MessageRole::System,
                content: vec![
                    MessagePart::Text {
                        text: "二进制内容已保存".to_owned(),
                    },
                    MessagePart::Artifact {
                        artifact: binary_artifact.clone(),
                        materialization: ArtifactMaterialization::Binary,
                    },
                ],
            },
        },
    );

    let baseline = journal.state().expect("角色反例前状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("角色反例前日志应读取");
    for (index, message) in [
        SessionMessage {
            message_id: "message-binary-only".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: None,
            role: MessageRole::System,
            content: vec![MessagePart::Artifact {
                artifact: binary_artifact.clone(),
                materialization: ArtifactMaterialization::Binary,
            }],
        },
        SessionMessage {
            message_id: "message-system-image".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: None,
            role: MessageRole::System,
            content: vec![MessagePart::Artifact {
                artifact: image_artifact.clone(),
                materialization: ArtifactMaterialization::Image,
            }],
        },
        SessionMessage {
            message_id: "message-assistant-image".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Assistant,
            content: vec![MessagePart::Artifact {
                artifact: image_artifact,
                materialization: ArtifactMaterialization::Image,
            }],
        },
        SessionMessage {
            message_id: "message-tool-artifact".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Tool,
            content: vec![MessagePart::Artifact {
                artifact: text_artifact,
                materialization: ArtifactMaterialization::Utf8Text,
            }],
        },
    ]
    .into_iter()
    .enumerate()
    {
        let result = journal.append_idempotent(
            SessionEventId::new(format!("event-invalid-role-{index}")).expect("事件 ID 应有效"),
            baseline.last_sequence,
            SessionEvent::MessageAdded { message },
        );
        assert!(matches!(result, Err(ResourceError::Reduction(_))));
        assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline);
        assert_eq!(
            fs::read(journal.log_path()).expect("拒绝后日志应读取"),
            baseline_log
        );
    }

    let request_id = started_tool(&journal, &turn_id, &agent_id);
    let persisted_result = PersistedToolResult {
        tool_call_id: "call-artifact".to_owned(),
        content: vec![
            ToolResultPart::Text {
                text: "模型可见结果".to_owned(),
            },
            ToolResultPart::Artifact {
                artifact: binary_artifact,
                materialization: ArtifactMaterialization::Binary,
            },
        ],
        is_error: false,
    };
    append(
        &journal,
        "event-complete-binary-result",
        SessionEvent::ToolCompleted {
            request_id,
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: persisted_result.clone(),
            },
        },
    );
    let expected_revision = journal
        .state()
        .expect("段提交前状态应读取")
        .transcript_revision;
    append(
        &journal,
        "event-binary-result-segment",
        model_round_batch(
            &turn_id,
            &agent_id,
            TranscriptSegment {
                turn_id: turn_id.clone(),
                source_agent_id: agent_id.clone(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: expected_revision,
                messages: vec![
                    SessionMessage {
                        message_id: "message-binary-call".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Assistant,
                        content: vec![MessagePart::ToolCall {
                            tool_call_id: "call-artifact".to_owned(),
                            tool_name: "read_binary".to_owned(),
                            arguments: json!({"path": "asset.bin"}),
                        }],
                    },
                    SessionMessage {
                        message_id: "message-binary-result".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: Some(agent_id.clone()),
                        role: MessageRole::Tool,
                        content: vec![MessagePart::ToolResult {
                            tool_call_id: "call-artifact".to_owned(),
                            content: persisted_result.content,
                            is_error: false,
                        }],
                    },
                ],
            },
        ),
    );
    let state = journal.state().expect("可见性状态应读取");
    let mut tampered_role = serde_json::to_value(&state).expect("状态应编码");
    let image_message = tampered_role["transcript"]
        .as_array_mut()
        .expect("Transcript 应为数组")
        .iter_mut()
        .find(|record| record["payload"]["messageId"] == "message-user-image-artifact")
        .expect("用户图片消息应存在");
    image_message["payload"]["role"] = json!("assistant");
    image_message["payload"]["agentId"] = json!("root");
    let tampered_role: SessionState =
        serde_json::from_value(tampered_role).expect("篡改状态仍应可解析");
    assert!(matches!(
        tampered_role.validate_transcript_history(),
        Err(ResourceError::Reduction(_))
    ));
    let raw_audit = state
        .raw_transcript_messages()
        .into_iter()
        .find(|message| message.message_id == "message-audit-binary")
        .expect("审计消息应保留");
    assert_eq!(raw_audit.content.len(), 2);
    let effective = state
        .effective_transcript(&agent_id)
        .expect("有效 Transcript 应重建");
    let visible_audit = effective
        .iter()
        .find(|message| message.message_id == "message-audit-binary")
        .expect("审计说明应进入模型上下文");
    assert_eq!(
        visible_audit.content,
        vec![MessagePart::Text {
            text: "二进制内容已保存".to_owned(),
        }]
    );
    let visible_result = effective
        .iter()
        .find(|message| message.message_id == "message-binary-result")
        .expect("工具结果应进入模型上下文");
    let MessagePart::ToolResult { content, .. } = &visible_result.content[0] else {
        panic!("工具结果消息应保持类型");
    };
    assert_eq!(
        content,
        &[ToolResultPart::Text {
            text: "模型可见结果".to_owned(),
        }]
    );
}

/// 验证实体合法的 Binary Artifact 也不能伪装成副作用未知的固定模型错误结果。
#[test]
fn side_effect_unknown_rejects_binary_only_result_after_artifact_validation() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, artifacts, turn_id, agent_id) =
        running_journal(root.path(), "artifact-side-effect-unknown");
    let request_id = RequestId::derive_model_tool_call(
        &journal.state().expect("状态应读取").session_id,
        &turn_id,
        &agent_id,
        1,
        "call-stateful",
    )
    .expect("Request ID 应派生");
    append(
        &journal,
        "event-stateful-request",
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id,
                agent_id,
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-stateful".to_owned(),
                tool_name: "write".to_owned(),
                arguments: json!({"path": "output.bin"}),
                effect: ToolEffect::ChangesState,
            },
        },
    );
    append(
        &journal,
        "event-stateful-start",
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    );
    let binary = artifacts
        .put(
            &[0xff, 0x00, 0x01],
            Some("application/octet-stream".to_owned()),
        )
        .expect("Binary Artifact 应保存")
        .as_event_use();
    let baseline = journal.state().expect("错误恢复结果前状态应读取");
    let baseline_log = fs::read(journal.log_path()).expect("错误恢复结果前日志应读取");
    let result = journal.append_idempotent(
        SessionEventId::new("event-stateful-binary-result").expect("事件 ID 应有效"),
        baseline.last_sequence,
        SessionEvent::ToolSideEffectUnknown {
            request_id: request_id.clone(),
            result: PersistedToolResult {
                tool_call_id: "call-stateful".to_owned(),
                content: vec![ToolResultPart::Artifact {
                    artifact: binary,
                    materialization: ArtifactMaterialization::Binary,
                }],
                is_error: true,
            },
        },
    );
    assert!(matches!(result, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("拒绝后状态应读取"), baseline);
    assert_eq!(
        fs::read(journal.log_path()).expect("拒绝后日志应读取"),
        baseline_log
    );
    append(
        &journal,
        "event-stateful-canonical-result",
        SessionEvent::ToolSideEffectUnknown {
            request_id,
            result: side_effect_unknown_result("call-stateful"),
        },
    );
}

/// 验证缺失或篡改 Artifact 会在写日志前拒绝整个 Transcript 段。
#[test]
fn missing_or_tampered_artifact_rejects_entire_segment() {
    let root = TempDir::new().expect("临时目录应创建");
    let (journal, artifacts, turn_id, agent_id) =
        running_journal(root.path(), "artifact-segment-atomic");
    let baseline_bytes = fs::read(journal.log_path()).expect("日志应读取");
    let baseline_state = journal.state().expect("状态应读取");
    let missing = ArtifactUse {
        artifact_id: ArtifactId::new("0".repeat(64)).expect("Artifact ID 应有效"),
        sha256: "0".repeat(64),
        size_bytes: 1,
        media_type: Some("text/plain".to_owned()),
    };
    let missing_result = journal.append_idempotent(
        SessionEventId::new("event-missing").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        artifact_segment(&turn_id, &agent_id, missing),
    );
    assert!(matches!(
        missing_result,
        Err(ResourceError::ArtifactNotFound)
    ));

    let stored = artifacts
        .put(b"original", Some("text/plain".to_owned()))
        .expect("Artifact 应保存")
        .as_event_use();
    let artifact_path = root
        .path()
        .join("sessions")
        .join("artifact-segment-atomic")
        .join("artifacts")
        .join(format!("{}.artifact", stored.artifact_id.as_str()));
    fs::write(&artifact_path, b"tampered").expect("Artifact 应篡改");
    let tampered_result = journal.append_idempotent(
        SessionEventId::new("event-tampered").expect("事件 ID 应有效"),
        baseline_state.last_sequence,
        artifact_segment(&turn_id, &agent_id, stored),
    );
    assert!(matches!(
        tampered_result,
        Err(ResourceError::ArtifactHashMismatch)
    ));
    assert_eq!(
        fs::read(journal.log_path()).expect("日志应读取"),
        baseline_bytes
    );
    assert_eq!(journal.state().expect("状态应读取"), baseline_state);
}
