mod support;

use std::sync::Arc;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, ArtifactLimits, ArtifactMaterialization, ArtifactStore, JournalConfig,
    MessageImageSource, MessagePart, MessageRole, PersistedToolResult, ReasoningContinuation,
    RequestId, SessionEvent, SessionId, SessionJournal, SessionMessage, SessionOpen,
    ToolCompletionStatus, ToolEffect, ToolOutcome, ToolRequest, ToolResultPart, TranscriptSegment,
    TurnId,
};
use serde_json::json;
use tempfile::tempdir;

use support::TestJournalAppend;

/// 打开带真实 Artifact 校验器的全新 Session 日志。
fn journal_with_artifacts(
    root: &std::path::Path,
) -> (SessionJournal, Arc<ArtifactStore>, SessionId, TurnId) {
    let session_id = SessionId::new("session-transcript").expect("Session ID 应有效");
    let turn_id = TurnId::new("turn-transcript").expect("Turn ID 应有效");
    let artifacts = Arc::new(
        ArtifactStore::open(root, session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应可打开"),
    );
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        session_id.clone(),
        JournalConfig::default(),
        artifacts.clone(),
    )
    .expect("Session 日志应可打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
    };
    journal
        .append(SessionEvent::SessionCreated {
            title: "完整 Transcript".to_owned(),
            project_root: "D:/project".to_owned(),
        })
        .expect("Session 创建事件应成功");
    journal
        .append(SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 Agent ID 应有效"),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "验证完整消息".to_owned(),
        })
        .expect("Turn 开始事件应成功");
    (journal, artifacts, session_id, turn_id)
}

/// 验证 developer、推理续传、图片、工具调用和工具结果均可确定性重放。
#[test]
fn complete_provider_neutral_transcript_round_trips() {
    let root = tempdir().expect("临时目录应可创建");
    let (journal, artifacts, session_id, turn_id) = journal_with_artifacts(root.path());
    let image = artifacts
        .put(b"synthetic-image", Some("image/png".to_owned()))
        .expect("图片 Artifact 应可写入")
        .as_event_use();
    let large_result = artifacts
        .put(b"large-tool-result", Some("text/plain".to_owned()))
        .expect("工具结果 Artifact 应可写入")
        .as_event_use();
    let root_agent = AgentId::new("root").expect("Agent ID 应有效");
    let request_id =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &root_agent, 1, "call-1")
            .expect("Request ID 应派生");
    let persisted_result = PersistedToolResult {
        tool_call_id: "call-1".to_owned(),
        content: vec![
            ToolResultPart::Text {
                text: "读取完成".to_owned(),
            },
            ToolResultPart::Image {
                source: MessageImageSource::Url {
                    url: "https://example.invalid/image.png".to_owned(),
                },
            },
            ToolResultPart::Artifact {
                artifact: large_result.clone(),
                materialization: ArtifactMaterialization::Utf8Text,
            },
        ],
        is_error: false,
    };
    journal
        .append(SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id: root_agent.clone(),
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-1".to_owned(),
                tool_name: "Read".to_owned(),
                arguments: json!({"path": "README.md"}),
                effect: ToolEffect::ReadOnly,
            },
        })
        .expect("工具请求应持久化");
    journal
        .append(SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        })
        .expect("工具执行起点应持久化");
    journal
        .append(SessionEvent::ToolCompleted {
            request_id,
            outcome: ToolOutcome {
                status: ToolCompletionStatus::Succeeded,
                result: persisted_result.clone(),
            },
        })
        .expect("工具完整结果应持久化");

    let messages = vec![
        SessionMessage {
            message_id: "message-developer".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(root_agent.clone()),
            role: MessageRole::Developer,
            content: vec![MessagePart::Text {
                text: "开发约束".to_owned(),
            }],
        },
        SessionMessage {
            message_id: "message-user".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(root_agent.clone()),
            role: MessageRole::User,
            content: vec![
                MessagePart::Text {
                    text: "分析图片".to_owned(),
                },
                MessagePart::Image {
                    source: MessageImageSource::Artifact {
                        artifact: image.clone(),
                    },
                },
            ],
        },
        SessionMessage {
            message_id: "message-assistant".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(root_agent.clone()),
            role: MessageRole::Assistant,
            content: vec![
                MessagePart::Reasoning {
                    text: "检查输入".to_owned(),
                    summary: Some("需要调用读取工具".to_owned()),
                    continuation: Some(ReasoningContinuation {
                        kind: "opaque-test".to_owned(),
                        data: json!({"token": "continuation"}),
                    }),
                },
                MessagePart::ToolCall {
                    tool_call_id: "call-1".to_owned(),
                    tool_name: "Read".to_owned(),
                    arguments: json!({"path": "README.md"}),
                },
            ],
        },
        SessionMessage {
            message_id: "message-tool".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(root_agent.clone()),
            role: MessageRole::Tool,
            content: vec![MessagePart::ToolResult {
                tool_call_id: "call-1".to_owned(),
                content: persisted_result.content,
                is_error: false,
            }],
        },
    ];
    journal
        .append(SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::ModelRoundCompleted {
                    turn_id: turn_id.clone(),
                    source_agent_id: root_agent.clone(),
                    model_round: 1,
                    requested_model: "transcript-test-model".to_owned(),
                    metadata: ResponseMetadata {
                        response_id: Some("transcript-test-response".to_owned()),
                        model: Some("transcript-test-model".to_owned()),
                    },
                    usage: TokenUsage::unknown(),
                    stop_reason: StopReason::ToolUse,
                },
                SessionEvent::TranscriptSegmentCommitted {
                    segment: TranscriptSegment {
                        turn_id: turn_id.clone(),
                        source_agent_id: root_agent,
                        model_round: 1,
                        segment_index: 0,
                        expected_transcript_revision: 0,
                        messages: messages.clone(),
                    },
                },
            ],
        })
        .expect("完整模型 Round 应原子追加");
    let live = journal.state().expect("实时状态应可读取");
    assert_eq!(
        live.raw_transcript_messages()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        messages
    );
    drop(journal);

    let replayed = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id,
        JournalConfig::default(),
        artifacts,
    )
    .expect("Session 应可重放")
    {
        SessionOpen::Ready(journal) => journal.state().expect("重放状态应可读取"),
        SessionOpen::Corrupt(report) => panic!("完整日志不应损坏：{:?}", report.issues),
    };
    assert_eq!(replayed, live);
}

/// 验证角色错配、空续传状态和非对象工具参数不会进入权威日志。
#[test]
fn malformed_transcript_parts_are_rejected_before_append() {
    let root = tempdir().expect("临时目录应可创建");
    let (journal, _artifacts, _session_id, turn_id) = journal_with_artifacts(root.path());
    let cases = vec![
        SessionMessage {
            message_id: "bad-role".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::ToolCall {
                tool_call_id: "call-1".to_owned(),
                tool_name: "Read".to_owned(),
                arguments: json!({}),
            }],
        },
        SessionMessage {
            message_id: "bad-reasoning".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: None,
            role: MessageRole::Assistant,
            content: vec![MessagePart::Reasoning {
                text: String::new(),
                summary: None,
                continuation: Some(ReasoningContinuation {
                    kind: String::new(),
                    data: serde_json::Value::Null,
                }),
            }],
        },
        SessionMessage {
            message_id: "bad-tool-input".to_owned(),
            turn_id: Some(turn_id),
            agent_id: None,
            role: MessageRole::Assistant,
            content: vec![MessagePart::ToolCall {
                tool_call_id: "call-2".to_owned(),
                tool_name: "Read".to_owned(),
                arguments: json!(["README.md"]),
            }],
        },
    ];
    for message in cases {
        let error = journal
            .append(SessionEvent::MessageAdded { message })
            .expect_err("畸形消息必须被拒绝");
        assert!(error.to_string().contains("类型化内容"));
    }
    assert!(
        journal
            .state()
            .expect("状态应可读取")
            .raw_transcript_messages()
            .is_empty()
    );
}

/// 验证嵌套图片 Artifact 也必须经过当前 Session 的实体校验。
#[test]
fn nested_image_artifact_requires_real_entity() {
    let root = tempdir().expect("临时目录应可创建");
    let (journal, artifacts, _session_id, turn_id) = journal_with_artifacts(root.path());
    let missing = artifacts
        .put(b"temporary", Some("image/png".to_owned()))
        .expect("初始 Artifact 应可创建")
        .as_event_use();
    let artifact_path = journal
        .session_dir()
        .join("artifacts")
        .join(format!("{}.artifact", missing.artifact_id.as_str()));
    std::fs::remove_file(&artifact_path).expect("测试 Artifact 应可删除");
    let error = journal
        .append(SessionEvent::MessageAdded {
            message: SessionMessage {
                message_id: "missing-image".to_owned(),
                turn_id: Some(turn_id),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Image {
                    source: MessageImageSource::Artifact { artifact: missing },
                }],
            },
        })
        .expect_err("缺失图片实体必须被拒绝");
    assert!(error.to_string().contains("Artifact"));
}
