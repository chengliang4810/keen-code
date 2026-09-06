//! 文件工具变更事件的状态机、Artifact 校验与容量限制回归。

use std::sync::Arc;

use keencode_resources::{
    AgentId, ArtifactLimits, ArtifactStore, Durability, FileSnapshot, IdempotentAppendOutcome,
    JournalConfig, PersistedToolResult, RequestId, ResourceError, SessionEvent, SessionEventId,
    SessionId, SessionJournal, SessionOpen, SnapshotPolicy, ToolCompletionStatus, ToolEffect,
    ToolFileChange, ToolOutcome, ToolRequest, ToolResultPart, TurnId,
};
use tempfile::TempDir;

/// 追加一条测试事件并使用 Journal 的现有 CAS 入口。
fn append(journal: &SessionJournal, event: SessionEvent) -> Result<(), ResourceError> {
    let event_id = SessionEventId::new(format!(
        "file-change-{}",
        journal.state()?.last_sequence + 1
    ))?;
    let expected = journal.state()?.last_sequence;
    match journal.append_idempotent(event_id, expected, event)? {
        IdempotentAppendOutcome::Appended(_) => Ok(()),
        IdempotentAppendOutcome::AlreadyCommitted { .. } => Ok(()),
        IdempotentAppendOutcome::SequenceConflict { .. }
        | IdempotentAppendOutcome::EventIdConflict { .. } => Err(ResourceError::Reduction(
            "测试事件追加发生意外幂等冲突".to_owned(),
        )),
        IdempotentAppendOutcome::Indeterminate { error } => Err(error),
    }
}

/// 追加一条使用指定事件标识的事件并返回生产入口的结果。
fn append_named(
    journal: &SessionJournal,
    event_id: &str,
    event: SessionEvent,
) -> Result<IdempotentAppendOutcome, ResourceError> {
    let expected = journal.state()?.last_sequence;
    journal.append_idempotent(SessionEventId::new(event_id)?, expected, event)
}

/// 为文件变更回归创建已运行副作用工具的 Journal。
fn running_fixture(
    limits: ArtifactLimits,
    config: JournalConfig,
    with_validator: bool,
) -> (
    TempDir,
    SessionJournal,
    Arc<ArtifactStore>,
    TurnId,
    RequestId,
) {
    let directory = tempfile::tempdir().expect("临时目录应创建");
    let session_id = SessionId::new("file-change-events").expect("Session ID 应有效");
    let artifacts = Arc::new(
        ArtifactStore::open(directory.path(), session_id.clone(), limits)
            .expect("ArtifactStore 应打开"),
    );
    let opened = if with_validator {
        SessionJournal::open_with_artifact_validator(
            directory.path(),
            session_id.clone(),
            config,
            artifacts.clone(),
        )
        .expect("Journal 应打开")
    } else {
        SessionJournal::open(directory.path(), session_id.clone(), config).expect("Journal 应打开")
    };
    let SessionOpen::Ready(journal) = opened else {
        panic!("新 Journal 不应损坏");
    };
    append(
        &journal,
        SessionEvent::SessionCreated {
            title: "文件变更事件".to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    )
    .expect("Session 应创建");
    let turn_id = TurnId::new("turn-file-change").expect("Turn ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    append(
        &journal,
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: agent_id.clone(),
            root_turn_id: turn_id.clone(),
            parent_turn_id: None,
            prompt_summary: "记录文件变更".to_owned(),
        },
    )
    .expect("Turn 应开始");
    let request_id =
        RequestId::derive_model_tool_call(&session_id, &turn_id, &agent_id, 1, "call-write")
            .expect("Request ID 应派生");
    append(
        &journal,
        SessionEvent::ToolRequested {
            request: ToolRequest {
                request_id: request_id.clone(),
                turn_id: turn_id.clone(),
                agent_id,
                model_round: 1,
                request_index: 0,
                model_tool_call_id: "call-write".to_owned(),
                tool_name: "write_file".to_owned(),
                arguments: serde_json::json!({"path": "result.txt"}),
                effect: ToolEffect::ChangesState,
            },
        },
    )
    .expect("工具请求应记录");
    append(
        &journal,
        SessionEvent::ToolExecutionStarted {
            request_id: request_id.clone(),
        },
    )
    .expect("工具执行起点应记录");
    (directory, journal, artifacts, turn_id, request_id)
}

/// 生成并持久化完整快照；事件只会携带其 Artifact 引用。
fn persisted_snapshot(store: &ArtifactStore, bytes: &[u8]) -> FileSnapshot {
    let snapshot = store.plan_file_snapshot(bytes).expect("文件快照应可规划");
    store
        .persist_file_snapshot(&snapshot, bytes)
        .expect("文件快照块应持久化");
    snapshot
}

/// 构造与工具调用严格配对的成功结果。
fn success_outcome() -> ToolOutcome {
    ToolOutcome {
        status: ToolCompletionStatus::Succeeded,
        result: PersistedToolResult {
            tool_call_id: "call-write".to_owned(),
            content: vec![ToolResultPart::Text {
                text: "写入完成".to_owned(),
            }],
            is_error: false,
        },
    }
}

/// 构造待提交的文件变更事件。
fn prepared_event(request_id: &RequestId, path: &str, after: FileSnapshot) -> SessionEvent {
    SessionEvent::ToolFileChangePrepared {
        request_id: request_id.clone(),
        change: ToolFileChange {
            path: path.to_owned(),
            before: None,
            after,
            applied: false,
        },
    }
}

/// 验证准备与应用是单向迁移，且工具终态不会丢失前后快照证据。
#[test]
fn file_change_prepared_applied_and_terminal_evidence_are_preserved() {
    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits::default(),
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
        true,
    );
    let after = persisted_snapshot(&artifacts, b"new file\r\n");
    let prepared = prepared_event(&request_id, r"C:\workspace\result.txt", after);
    let encoded = serde_json::to_string(&prepared).expect("事件应可序列化");
    assert!(encoded.contains("chunks"));
    assert!(!encoded.contains("new file"));
    append(&journal, prepared).expect("准备事件应提交");
    let state = journal.state().expect("状态应读取");
    assert!(
        !state.tools[&request_id]
            .file_change
            .as_ref()
            .expect("应保存文件变更")
            .applied
    );

    append(
        &journal,
        SessionEvent::ToolFileChangeApplied {
            request_id: request_id.clone(),
        },
    )
    .expect("应用事件应提交");
    assert!(state_after(&journal, &request_id).applied);
    append(
        &journal,
        SessionEvent::ToolCompleted {
            request_id: request_id.clone(),
            outcome: success_outcome(),
        },
    )
    .expect("工具终态应提交");
    assert!(state_after(&journal, &request_id).applied);
    let duplicate = append_named(
        &journal,
        "file-change-duplicate-applied",
        SessionEvent::ToolFileChangeApplied { request_id },
    )
    .expect_err("已终态工具不能再次应用文件变更");
    assert!(matches!(duplicate, ResourceError::Reduction(_)));
}

/// 读取当前变更的应用位，避免测试重复依赖 Journal 内部实现。
fn state_after(journal: &SessionJournal, request_id: &RequestId) -> ToolFileChange {
    journal
        .state()
        .expect("状态应读取")
        .tools
        .get(request_id)
        .and_then(|tool| tool.file_change.clone())
        .expect("文件变更应存在")
}

/// 验证相对路径不能创建文件变更证据，而跨平台绝对路径可以通过归约。
#[test]
fn file_change_preconditions_are_rejected_without_state_mutation() {
    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits::default(),
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
        true,
    );
    let after = persisted_snapshot(&artifacts, b"new");
    let error = append_named(
        &journal,
        "file-change-relative",
        prepared_event(&request_id, "relative.txt", after),
    )
    .expect_err("相对路径应拒绝");
    assert!(matches!(error, ResourceError::Reduction(_)));
    assert!(
        journal.state().expect("状态应读取").tools[&request_id]
            .file_change
            .is_none()
    );

    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits::default(),
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
        true,
    );
    let snapshot = persisted_snapshot(&artifacts, b"new");
    append_named(
        &journal,
        "file-change-valid-prepared",
        prepared_event(&request_id, "/workspace/result.txt", snapshot),
    )
    .expect("Unix 绝对路径应被跨平台识别");
}

/// 验证没有 Artifact 校验器或缺少快照块时，Prepared 事件在归约前即失败。
#[test]
fn file_change_snapshot_validation_is_fail_closed() {
    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits::default(),
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
        false,
    );
    let snapshot = artifacts
        .plan_file_snapshot(b"not persisted")
        .expect("快照结构应可规划");
    let error = append_named(
        &journal,
        "file-change-without-validator",
        prepared_event(&request_id, "/workspace/result.txt", snapshot),
    )
    .expect_err("未注入校验器时应拒绝");
    assert!(matches!(error, ResourceError::ArtifactValidatorRequired));
    assert!(
        journal.state().expect("状态应读取").tools[&request_id]
            .file_change
            .is_none()
    );

    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits::default(),
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
        true,
    );
    let missing = artifacts
        .plan_file_snapshot(b"not persisted")
        .expect("快照结构应可规划");
    let error = append_named(
        &journal,
        "file-change-missing-artifact",
        prepared_event(&request_id, "/workspace/result.txt", missing),
    )
    .expect_err("缺少快照块时应拒绝");
    assert!(matches!(
        error,
        ResourceError::ArtifactNotFound | ResourceError::Io { .. }
    ));
}

/// 验证快照块引用数量计入状态集合上限，不能借快照结构绕过限制。
#[test]
fn file_change_snapshot_chunks_count_toward_state_limit() {
    let (_directory, journal, artifacts, _turn_id, request_id) = running_fixture(
        ArtifactLimits {
            max_artifact_bytes: 1,
            ..ArtifactLimits::default()
        },
        JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            max_state_collection_items: 1,
            ..JournalConfig::default()
        },
        true,
    );
    let snapshot = persisted_snapshot(&artifacts, b"two");
    assert_eq!(snapshot.chunks.len(), 3);
    let error = append_named(
        &journal,
        "file-change-state-limit",
        prepared_event(&request_id, "/workspace/result.txt", snapshot),
    )
    .expect_err("快照块应计入状态集合上限");
    assert!(matches!(
        error,
        ResourceError::StateCollectionLimit {
            collection: "file_snapshot_chunks",
            ..
        }
    ));
}
