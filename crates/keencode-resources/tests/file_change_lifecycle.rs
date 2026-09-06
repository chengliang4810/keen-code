//! 文件变更快照跨重启、分支与归档生命周期的集成回归。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
use keencode_resources::{
    AgentId, ArtifactLimits, ArtifactStore, Durability, FileSnapshot, IdempotentAppendOutcome,
    JournalConfig, MessagePart, MessageRole, PersistedToolResult, RequestId, ResourceError,
    SessionEditUserRequest, SessionEvent, SessionEventId, SessionForkRequest, SessionId,
    SessionJournal, SessionLease, SessionLeaseAcquire, SessionMessage, SessionOpen, SessionState,
    SnapshotPolicy, ToolCompletionStatus, ToolEffect, ToolFileChange, ToolOutcome, ToolRequest,
    ToolResultPart, TranscriptSegment, TurnId, fork_session, prepare_edit_user,
    side_effect_unknown_result,
};
use tempfile::TempDir;

/// 文件变更工具在测试夹具中的最终状态。
#[derive(Clone, Copy)]
enum InitialToolState {
    /// 只提交 Prepared，模拟工作区写入前崩溃。
    Prepared,
    /// 提交 Applied、Completed 和 TurnCompleted，模拟完整成功写入。
    AppliedAndCompleted,
}

/// 保存测试 Session、快照原始字节和请求标识，确保冷恢复后能逐字节比对。
struct FileChangeFixture {
    /// 隔离测试数据的临时根目录。
    root: TempDir,
    /// 被测试的源 Session 标识。
    session_id: SessionId,
    /// 包含文件变更证据的工具请求标识。
    request_id: RequestId,
    /// 文件变更所属 Turn 标识。
    turn_id: TurnId,
    /// Prepared 事件中的写前原始字节。
    before_bytes: Vec<u8>,
    /// Prepared 事件中的写后原始字节。
    after_bytes: Vec<u8>,
    /// 已持久化的写后快照结构。
    after_snapshot: FileSnapshot,
}

/// 使用真实持久化配置，关闭自动 Session Snapshot 以便测试明确走 JSONL 冷重放。
fn journal_config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 使用较小 Artifact 块，使每份文件快照必须跨多个实体复制和恢复。
fn artifact_limits() -> ArtifactLimits {
    ArtifactLimits {
        max_artifact_bytes: 4,
        ..ArtifactLimits::default()
    }
}

/// 打开带真实 Artifact 校验器的 Session Journal 和 ArtifactStore。
fn open_session(
    root: &Path,
    session_id: &SessionId,
) -> Result<(SessionJournal, Arc<ArtifactStore>), ResourceError> {
    let artifacts = Arc::new(ArtifactStore::open(
        root,
        session_id.clone(),
        artifact_limits(),
    )?);
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        session_id.clone(),
        journal_config(),
        artifacts.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => return Err(ResourceError::CorruptReadOnly),
    };
    Ok((journal, artifacts))
}

/// 获取指定 Session 的独占 lease，供 ArtifactStore 冷恢复接口校验作用域。
fn acquire_lease(root: &Path, session_id: &SessionId) -> Result<SessionLease, ResourceError> {
    match SessionLease::try_acquire(root, session_id.clone())? {
        SessionLeaseAcquire::Acquired(lease) => Ok(lease),
        SessionLeaseAcquire::Busy { .. } => Err(ResourceError::SessionMutationBusy),
    }
}

/// 通过 Journal 的幂等追加入口提交一条测试事件。
fn append(
    journal: &SessionJournal,
    event_id: &str,
    event: SessionEvent,
) -> Result<(), ResourceError> {
    let expected_sequence = journal.state()?.last_sequence;
    match journal.append_idempotent(
        SessionEventId::new(event_id.to_owned())?,
        expected_sequence,
        event,
    )? {
        IdempotentAppendOutcome::Appended(_) | IdempotentAppendOutcome::AlreadyCommitted { .. } => {
            Ok(())
        }
        IdempotentAppendOutcome::SequenceConflict { .. }
        | IdempotentAppendOutcome::EventIdConflict { .. } => Err(ResourceError::Reduction(
            "测试事件追加发生意外幂等冲突".to_owned(),
        )),
        IdempotentAppendOutcome::Indeterminate { error } => Err(error),
    }
}

/// 创建并持久化一份完整的原始文件快照。
fn persist_snapshot(store: &ArtifactStore, bytes: &[u8]) -> FileSnapshot {
    let snapshot = store.plan_file_snapshot(bytes).expect("文件快照应可规划");
    store
        .persist_file_snapshot(&snapshot, bytes)
        .expect("文件快照块应写入 ArtifactStore");
    snapshot
}

/// 构造一个与工具调用严格配对的成功结果。
fn success_outcome() -> ToolOutcome {
    ToolOutcome {
        status: ToolCompletionStatus::Succeeded,
        result: PersistedToolResult {
            tool_call_id: "call-file-change".to_owned(),
            content: vec![ToolResultPart::Text {
                text: "文件写入完成".to_owned(),
            }],
            is_error: false,
        },
    }
}

/// 将模型 Round 完成记录和首个工具 Transcript 段原子写入 Journal。
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
                requested_model: "file-change-lifecycle-model".to_owned(),
                metadata: ResponseMetadata {
                    response_id: Some("file-change-lifecycle-response".to_owned()),
                    model: Some("file-change-lifecycle-model".to_owned()),
                },
                usage: TokenUsage::unknown(),
                stop_reason: StopReason::Completed,
            },
            SessionEvent::TranscriptSegmentCommitted { segment },
        ],
    }
}

/// 把已结束的文件工具调用物化为真实的 Assistant ToolCall/ToolResult Transcript 段。
fn materialize_tool(journal: &SessionJournal, request_id: &RequestId) {
    let state = journal.state().expect("工具物化前状态应读取");
    let lifecycle = state
        .tools
        .get(request_id)
        .expect("待物化工具应存在")
        .clone();
    let outcome = lifecycle.outcome.expect("工具物化前必须已经结束");
    let segment = TranscriptSegment {
        turn_id: lifecycle.request.turn_id.clone(),
        source_agent_id: lifecycle.request.agent_id.clone(),
        model_round: lifecycle.request.model_round,
        segment_index: 0,
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
    append(
        journal,
        "tool-transcript-segment",
        model_round_batch(
            &lifecycle.request.turn_id,
            &lifecycle.request.agent_id,
            segment,
        ),
    )
    .expect("工具调用和结果应物化到 Transcript");
}

/// 创建一条根 Agent 用户 Turn，并让其进入可产生副作用的工具执行阶段。
fn append_first_turn_start(
    journal: &SessionJournal,
    turn_id: &TurnId,
    user_message_id: &str,
) -> Result<(), ResourceError> {
    append(
        journal,
        "turn-1-start",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                    source_agent_id: AgentId::new("root")?,
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: None,
                    prompt_summary: "记录文件快照生命周期".to_owned(),
                },
                SessionEvent::MessageAdded {
                    message: SessionMessage {
                        message_id: user_message_id.to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: None,
                        role: MessageRole::User,
                        content: vec![MessagePart::Text {
                            text: "第一轮用户消息".to_owned(),
                        }],
                    },
                },
            ],
        },
    )
}

/// 创建一条包含目标用户消息的第二个根 Turn，用于编辑前归档测试。
fn append_second_turn(journal: &SessionJournal, turn_id: &TurnId) -> Result<(), ResourceError> {
    append(
        journal,
        "turn-2-start",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                    source_agent_id: AgentId::new("root")?,
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: None,
                    prompt_summary: "第二轮用户消息".to_owned(),
                },
                SessionEvent::MessageAdded {
                    message: SessionMessage {
                        message_id: "user-message-turn-2".to_owned(),
                        turn_id: Some(turn_id.clone()),
                        agent_id: None,
                        role: MessageRole::User,
                        content: vec![MessagePart::Text {
                            text: "第二轮用户消息".to_owned(),
                        }],
                    },
                },
            ],
        },
    )?;
    append(
        journal,
        "turn-2-completed",
        SessionEvent::TurnCompleted {
            turn_id: turn_id.clone(),
        },
    )
}

/// 创建包含写前、写后快照的真实 Journal，并按指定状态结束工具生命周期。
fn create_fixture(state: InitialToolState, with_second_turn: bool) -> FileChangeFixture {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("file-change-lifecycle").expect("Session ID 应有效");
    let (journal, artifacts) = open_session(root.path(), &session_id).expect("Session 应打开");
    append(
        &journal,
        "session-created",
        SessionEvent::SessionCreated {
            title: "文件变更生命周期".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("SessionCreated 应提交");

    let turn_id = TurnId::new("turn-file-change").expect("Turn ID 应有效");
    append_first_turn_start(&journal, &turn_id, "user-message-turn-1").expect("第一轮 Turn 应开始");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let request_id =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 1, "call-file-change")
            .expect("Request ID 应派生");
    append(
        &journal,
        "tool-requested",
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id,
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-file-change".to_owned(),
                tool_name: "write_file".to_owned(),
                arguments: serde_json::json!({"path": "result.bin"}),
                effect: ToolEffect::ChangesState,
            },
        },
    )
    .expect("ToolRequested 应提交");
    append(
        &journal,
        "tool-execution-started",
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    )
    .expect("ToolExecutionStarted 应提交");

    let before_bytes = b"\xef\xbb\xbfold\r\n\0\xff".to_vec();
    let after_bytes = b"\xef\xbb\xbfnew\n\r\0\x80".to_vec();
    let before_snapshot = persist_snapshot(&artifacts, &before_bytes);
    let after_snapshot = persist_snapshot(&artifacts, &after_bytes);
    append(
        &journal,
        "file-change-prepared",
        SessionEvent::ToolFileChangePrepared {
            request_id: request_id.clone(),
            change: ToolFileChange {
                path: root.path().join("result.bin").display().to_string(),
                before: Some(before_snapshot.clone()),
                after: after_snapshot.clone(),
                applied: false,
            },
        },
    )
    .expect("ToolFileChangePrepared 应提交");

    if matches!(state, InitialToolState::AppliedAndCompleted) {
        append(
            &journal,
            "file-change-applied",
            SessionEvent::ToolFileChangeApplied {
                request_id: request_id.clone(),
            },
        )
        .expect("ToolFileChangeApplied 应提交");
        append(
            &journal,
            "tool-completed",
            SessionEvent::ToolCompleted {
                request_id: request_id.clone(),
                outcome: success_outcome(),
            },
        )
        .expect("ToolCompleted 应提交");
        materialize_tool(&journal, &request_id);
        append(
            &journal,
            "turn-1-completed",
            SessionEvent::TurnCompleted {
                turn_id: turn_id.clone(),
            },
        )
        .expect("第一轮 Turn 应完成");
        if with_second_turn {
            let second_turn_id = TurnId::new("turn-edit-target").expect("第二个 Turn ID 应有效");
            append_second_turn(&journal, &second_turn_id).expect("第二轮 Turn 应完成");
        }
    }

    drop(journal);
    drop(artifacts);
    FileChangeFixture {
        root,
        session_id,
        request_id,
        turn_id,
        before_bytes,
        after_bytes,
        after_snapshot,
    }
}

/// 在 Session 已关闭后追加恢复得到的 ToolSideEffectUnknown，再关闭并交给冷恢复流程。
fn recover_unknown_then_close(fixture: &FileChangeFixture) {
    let (journal, artifacts) = open_session(fixture.root.path(), &fixture.session_id)
        .expect("Prepared Session 应可重新打开");
    append(
        &journal,
        "tool-side-effect-unknown",
        SessionEvent::ToolSideEffectUnknown {
            request_id: fixture.request_id.clone(),
            result: side_effect_unknown_result("call-file-change"),
        },
    )
    .expect("ToolSideEffectUnknown 应提交");
    materialize_tool(&journal, &fixture.request_id);
    append(
        &journal,
        "turn-1-completed-after-recovery",
        SessionEvent::TurnCompleted {
            turn_id: fixture.turn_id.clone(),
        },
    )
    .expect("恢复后的 Turn 应完成");
    drop(journal);
    drop(artifacts);
}

/// 冷打开 Journal、取得 lease，并执行 ArtifactStore 的权威状态恢复与孤儿回收。
fn cold_recover(
    root: &Path,
    session_id: &SessionId,
) -> Result<(SessionState, Arc<ArtifactStore>), ResourceError> {
    let lease = acquire_lease(root, session_id)?;
    let (journal, artifacts) = open_session(root, session_id)?;
    let state = journal.state()?;
    artifacts.recover_for_state(&lease, &state)?;
    drop(journal);
    drop(lease);
    Ok((state, artifacts))
}

/// 按目标 Session 作用域重新派生测试工具请求标识。
fn request_id_for_session(session_id: &SessionId, turn_id: &TurnId) -> RequestId {
    RequestId::derive_model_tool_call(
        session_id,
        turn_id,
        &AgentId::new("root").expect("Agent ID 应有效"),
        1,
        "call-file-change",
    )
    .expect("目标 Session 工具请求 ID 应派生")
}

/// 验证分支或归档只重绑 RequestId，而不改变工具请求和 Transcript 正文。
fn assert_rebound_tool(
    state: &SessionState,
    fixture: &FileChangeFixture,
    target_request_id: &RequestId,
) {
    assert_ne!(
        target_request_id, &fixture.request_id,
        "不同 Session 的工具请求 ID 必须重新派生"
    );
    assert!(
        !state.tools.contains_key(&fixture.request_id),
        "目标 Session 不能继续使用源 Session 的工具请求 ID"
    );
    let lifecycle = state
        .tools
        .get(target_request_id)
        .expect("目标 Session 应存在重绑后的工具生命周期");
    assert_eq!(lifecycle.request.turn_id, fixture.turn_id);
    assert_eq!(lifecycle.request.agent_id, AgentId::new("root").unwrap());
    assert_eq!(lifecycle.request.model_round, 1);
    assert_eq!(lifecycle.request.request_index, 0);
    assert_eq!(lifecycle.request.model_tool_call_id, "call-file-change");
    assert_eq!(lifecycle.request.tool_name, "write_file");
    assert_eq!(
        lifecycle.request.arguments,
        serde_json::json!({"path": "result.bin"})
    );
    assert!(
        lifecycle.transcript_segment.is_some(),
        "目标工具生命周期应保留已物化 Transcript"
    );
    assert!(state.raw_transcript_messages().iter().any(|message| {
        message.content.iter().any(|part| {
            matches!(
                part,
                MessagePart::ToolCall {
                    tool_call_id,
                    tool_name,
                    ..
                } if tool_call_id == "call-file-change" && tool_name == "write_file"
            )
        })
    }));
    assert!(state.raw_transcript_messages().iter().any(|message| {
        message.content.iter().any(|part| {
            matches!(
                part,
                MessagePart::ToolResult {
                    tool_call_id,
                    content,
                    is_error: false,
                } if tool_call_id == "call-file-change"
                    && content == &vec![ToolResultPart::Text {
                        text: "文件写入完成".to_owned(),
                    }]
            )
        })
    }));
}

/// 从恢复状态提取文件变更证据并读取两份快照的完整原始字节。
fn assert_snapshot_bytes(
    state: &SessionState,
    artifacts: &ArtifactStore,
    request_id: &RequestId,
    before_bytes: &[u8],
    after_bytes: &[u8],
) -> ToolFileChange {
    let change = state
        .tools
        .get(request_id)
        .and_then(|tool| tool.file_change.clone())
        .expect("恢复状态必须保留文件变更证据");
    let before = change.before.as_ref().expect("测试夹具必须有写前快照");
    artifacts
        .validate_file_snapshot(before)
        .expect("写前快照完整性应通过");
    artifacts
        .validate_file_snapshot(&change.after)
        .expect("写后快照完整性应通过");
    assert_eq!(
        artifacts
            .read_file_snapshot(before)
            .expect("写前快照完整字节应可读取"),
        before_bytes
    );
    assert_eq!(
        artifacts
            .read_file_snapshot(&change.after)
            .expect("写后快照完整字节应可读取"),
        after_bytes
    );
    change
}

/// 返回一个快照块在指定 Session Artifact 目录中的实体路径。
fn artifact_content_path(
    root: &Path,
    session_id: &SessionId,
    snapshot: &FileSnapshot,
) -> std::path::PathBuf {
    root.join("sessions")
        .join(session_id.as_str())
        .join("artifacts")
        .join(format!(
            "{}.artifact",
            snapshot
                .chunks
                .first()
                .expect("测试快照必须至少有一个块")
                .artifact_id
                .as_str()
        ))
}

/// Prepared 未 Applied 时先恢复为 SideEffectUnknown，冷恢复仍保留 before/after 完整字节。
#[test]
fn prepared_unknown_then_cold_recovery_retains_both_snapshots() {
    let fixture = create_fixture(InitialToolState::Prepared, false);
    recover_unknown_then_close(&fixture);

    let (state, artifacts) =
        cold_recover(fixture.root.path(), &fixture.session_id).expect("冷恢复应成功");
    let change = assert_snapshot_bytes(
        &state,
        &artifacts,
        &fixture.request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
    assert!(!change.applied, "未确认写入的快照不能被标记为 Applied");
    assert_eq!(
        state.tools[&fixture.request_id]
            .outcome
            .as_ref()
            .map(|outcome| outcome.status),
        Some(ToolCompletionStatus::SideEffectUnknown)
    );
}

/// Applied + Completed 后重新打开 Journal，冷恢复仍保留两份快照的完整字节。
#[test]
fn applied_completed_cold_recovery_retains_both_snapshots() {
    let fixture = create_fixture(InitialToolState::AppliedAndCompleted, false);

    let (state, artifacts) =
        cold_recover(fixture.root.path(), &fixture.session_id).expect("冷恢复应成功");
    let change = assert_snapshot_bytes(
        &state,
        &artifacts,
        &fixture.request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
    assert!(change.applied, "已提交 Applied 事件的快照应保留应用状态");
    assert_eq!(
        state.tools[&fixture.request_id]
            .outcome
            .as_ref()
            .map(|outcome| outcome.status),
        Some(ToolCompletionStatus::Succeeded)
    );
}

/// fork 后目标 Session 必须拥有独立 Artifact 实体，并能按状态中的快照引用读回完整字节。
#[test]
fn fork_cold_recovery_copies_file_change_snapshots_across_sessions() {
    let fixture = create_fixture(InitialToolState::AppliedAndCompleted, false);
    let result = fork_session(
        fixture.root.path(),
        journal_config(),
        artifact_limits(),
        SessionForkRequest {
            source_session_id: fixture.session_id.clone(),
            operation_id: "fork-file-snapshot".to_owned(),
            title: Some("文件快照分支".to_owned()),
        },
    )
    .expect("fork 应完成");

    let (state, artifacts) =
        cold_recover(fixture.root.path(), &result.session_id).expect("目标 Session 冷恢复应成功");
    let target_request_id = request_id_for_session(&result.session_id, &fixture.turn_id);
    assert_rebound_tool(&state, &fixture, &target_request_id);
    let change = assert_snapshot_bytes(
        &state,
        &artifacts,
        &target_request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
    assert!(change.applied);
    assert!(
        artifact_content_path(fixture.root.path(), &result.session_id, &change.after).is_file(),
        "目标 Session 必须存在复制后的快照实体"
    );
    assert_eq!(artifacts.session_id(), &result.session_id);
}

/// 编辑前归档必须复制完整文件变更快照；源截断后和归档分支都可冷恢复读取。
#[test]
fn edit_archive_cold_recovery_copies_file_change_snapshots() {
    let fixture = create_fixture(InitialToolState::AppliedAndCompleted, true);
    let result = prepare_edit_user(
        fixture.root.path(),
        journal_config(),
        artifact_limits(),
        SessionEditUserRequest {
            source_session_id: fixture.session_id.clone(),
            target_message_id: "user-message-turn-2".to_owned(),
            expected_text: "第二轮用户消息".to_owned(),
            operation_id: "archive-file-snapshot".to_owned(),
        },
    )
    .expect("编辑前归档应完成");

    let (archive_state, archive_artifacts) =
        cold_recover(fixture.root.path(), &result.archived_session_id)
            .expect("归档 Session 冷恢复应成功");
    let archive_request_id = request_id_for_session(&result.archived_session_id, &fixture.turn_id);
    assert_rebound_tool(&archive_state, &fixture, &archive_request_id);
    let archive_change = assert_snapshot_bytes(
        &archive_state,
        &archive_artifacts,
        &archive_request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
    assert!(archive_change.applied);
    assert!(
        artifact_content_path(
            fixture.root.path(),
            &result.archived_session_id,
            &archive_change.after,
        )
        .is_file()
    );

    let (source_state, source_artifacts) =
        cold_recover(fixture.root.path(), &fixture.session_id).expect("截断源 Session 应可冷恢复");
    assert_snapshot_bytes(
        &source_state,
        &source_artifacts,
        &fixture.request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
}

/// 冷恢复应删除无状态引用的孤儿 Artifact，但不能删除文件快照仍引用的实体。
#[test]
fn cold_recovery_reclaims_unrelated_orphan_but_keeps_snapshot_entities() {
    let fixture = create_fixture(InitialToolState::AppliedAndCompleted, false);
    let (journal, artifacts) =
        open_session(fixture.root.path(), &fixture.session_id).expect("Session 应重新打开");
    let orphan = artifacts
        .put(b"ORPH", None)
        .expect("无关孤儿 Artifact 应写入")
        .as_event_use();
    drop(journal);
    drop(artifacts);

    let (state, recovered_artifacts) =
        cold_recover(fixture.root.path(), &fixture.session_id).expect("冷恢复应完成孤儿回收");
    let change = assert_snapshot_bytes(
        &state,
        &recovered_artifacts,
        &fixture.request_id,
        &fixture.before_bytes,
        &fixture.after_bytes,
    );
    assert!(
        matches!(
            recovered_artifacts.read_use(&orphan),
            Err(ResourceError::ArtifactNotFound)
        ),
        "无关孤儿 Artifact 必须被回收"
    );
    assert!(
        artifact_content_path(fixture.root.path(), &fixture.session_id, &change.after,).is_file(),
        "仍被快照引用的 Artifact 实体不能被删除"
    );
}

/// 冷恢复遇到缺失或同尺寸篡改的快照块必须失败关闭，不能静默跳过损坏证据。
#[test]
fn cold_recovery_rejects_missing_or_damaged_snapshots() {
    let missing_fixture = create_fixture(InitialToolState::AppliedAndCompleted, false);
    let missing_path = missing_fixture
        .root
        .path()
        .join("sessions")
        .join(missing_fixture.session_id.as_str())
        .join("artifacts")
        .join(format!(
            "{}.artifact",
            missing_fixture.after_snapshot.chunks[0]
                .artifact_id
                .as_str()
        ));
    fs::remove_file(&missing_path).expect("测试应删除一个快照块");
    assert!(
        matches!(
            cold_recover(missing_fixture.root.path(), &missing_fixture.session_id),
            Err(ResourceError::ArtifactNotFound)
        ),
        "缺失快照块必须返回 ArtifactNotFound"
    );

    let damaged_fixture = create_fixture(InitialToolState::AppliedAndCompleted, false);
    let damaged_path = damaged_fixture
        .root
        .path()
        .join("sessions")
        .join(damaged_fixture.session_id.as_str())
        .join("artifacts")
        .join(format!(
            "{}.artifact",
            damaged_fixture.after_snapshot.chunks[0]
                .artifact_id
                .as_str()
        ));
    fs::write(&damaged_path, b"xxxx").expect("测试应篡改一个同尺寸快照块");
    assert!(
        matches!(
            cold_recover(damaged_fixture.root.path(), &damaged_fixture.session_id),
            Err(ResourceError::ArtifactHashMismatch)
        ),
        "篡改快照块必须返回 ArtifactHashMismatch"
    );
}
