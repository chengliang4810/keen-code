//! 同一 Session 的执行目录切换事务；物理存储和会话身份保持稳定。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::atomic::{
    ATOMIC_TEMP_PREFIX, BoundedRead, atomic_write, ensure_regular_file_or_absent, exclusive_lock,
    prepare_root, read_file_bounded, secure_child_dir, sync_directory,
};
use crate::session_mutation::{
    SourceBundle, ensure_mutable_source, open_source, operation_key, records_sha256,
    secure_existing_session_dir, validate_operation_id,
};
use crate::{
    ArtifactLimits, Durability, IdempotentAppendOutcome, JournalConfig, ResourceError,
    SessionEvent, SessionEventId, SessionId, SessionJournal, SessionOpen,
};

/// 同一会话的目录变更输入，宿主必须先授权并规范化两个目录。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionWorkspaceRequest {
    /// 历史与资源继续所属的稳定 Session。
    pub session_id: SessionId,
    /// 跨重启复用的操作身份。
    pub operation_id: String,
    /// 必须仍与权威状态一致的原执行目录。
    pub expected_project_root: String,
    /// 新的真实执行目录；不能用它推导物理存储位置。
    pub project_root: String,
}

/// 唯一格式的可恢复事务记录，不包含消息正文或模型配置。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceRecord {
    schema: String,
    version: u32,
    request: SessionWorkspaceRequest,
    source_sequence: u64,
    source_sha256: String,
    completed: bool,
}

/// 事务正文预算，两个 Windows 长路径也必须有界。
const RECORD_LIMIT: u64 = 64 * 1024;
/// 避免恢复时无界枚举磁盘内容。
const RECORD_COUNT_LIMIT: usize = 10_000;

/// 在独占 lease 下归档旧协作 cwd，并幂等追加新的权威目录事件。
pub fn change_session_workspace(
    storage_root: impl AsRef<Path>,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    request: SessionWorkspaceRequest,
) -> Result<(), ResourceError> {
    validate_request(&request)?;
    let root = prepare_root(storage_root.as_ref())?;
    let operations = secure_child_dir(&root, "session-workspaces")?;
    let lock = operations.join("operations.lock");
    ensure_regular_file_or_absent(&lock)?;
    let _guard = exclusive_lock(&lock)?;
    let records = secure_child_dir(&operations, "records")?;
    let key = operation_key(&request.session_id, &request.operation_id);
    let path = records.join(format!("{key}.json"));
    let (record, source) = if let Some(record) = read_record(&path)? {
        if record.request != request {
            return Err(ResourceError::SessionMutationNotApplicable(
                "工作目录 operationId 已绑定其他输入".to_owned(),
            ));
        }
        (record, None)
    } else {
        let source = open_source(&root, &request.session_id, journal_config, artifact_limits)?;
        ensure_mutable_source(&source.state)?;
        if source.state.project_root != request.expected_project_root {
            return Err(ResourceError::SessionMutationNotApplicable(
                "原工作目录已改变".to_owned(),
            ));
        }
        let record = WorkspaceRecord {
            schema: "keencode/session-workspace".to_owned(),
            version: 1,
            request,
            source_sequence: source.state.last_sequence,
            source_sha256: records_sha256(&source.records)?,
            completed: false,
        };
        write_record(&path, &record)?;
        (record, Some(source))
    };
    resume(
        &root,
        &operations,
        &path,
        journal_config,
        artifact_limits,
        record,
        source,
    )
}

/// open、元数据读取和列表在观察新执行目录前收敛未完成的资源事务。
pub fn recover_session_workspaces(
    storage_root: impl AsRef<Path>,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
) -> Result<(), ResourceError> {
    let root = prepare_root(storage_root.as_ref())?;
    if !root
        .join("session-workspaces")
        .try_exists()
        .map_err(|error| ResourceError::io("inspect_workspace_transactions", error))?
    {
        return Ok(());
    }
    let operations = secure_child_dir(&root, "session-workspaces")?;
    let lock = operations.join("operations.lock");
    ensure_regular_file_or_absent(&lock)?;
    let _guard = exclusive_lock(&lock)?;
    let records = secure_child_dir(&operations, "records")?;
    let entries = fs::read_dir(&records)
        .map_err(|error| ResourceError::io("list_workspace_transactions", error))?;
    for (index, entry) in entries.enumerate() {
        if index >= RECORD_COUNT_LIMIT {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "工作目录事务数量超限".to_owned(),
            ));
        }
        let entry = entry.map_err(|error| ResourceError::io("read_workspace_entry", error))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| ResourceError::UnsafePath("工作目录事务名称无效".to_owned()))?;
        if name.starts_with(ATOMIC_TEMP_PREFIX) {
            continue;
        }
        let record = read_record(&entry.path())?.ok_or_else(|| {
            ResourceError::SessionMutationRecoveryRequired("工作目录事务消失".to_owned())
        })?;
        let key = operation_key(&record.request.session_id, &record.request.operation_id);
        if name != format!("{key}.json") {
            return Err(ResourceError::UnsafePath(
                "工作目录事务身份不一致".to_owned(),
            ));
        }
        resume(
            &root,
            &operations,
            &entry.path(),
            journal_config,
            artifact_limits,
            record,
            None,
        )?;
    }
    Ok(())
}

/// 完成墓碑先返回，防止旧操作重试归档新 cwd 下产生的 checkpoint。
fn resume(
    root: &Path,
    operations: &Path,
    path: &Path,
    mut journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    mut record: WorkspaceRecord,
    source: Option<SourceBundle>,
) -> Result<(), ResourceError> {
    if record.completed {
        return Ok(());
    }
    let source = match source {
        Some(source) => source,
        None => open_source(
            root,
            &record.request.session_id,
            journal_config,
            artifact_limits,
        )?,
    };
    journal_config.durability = Durability::FlushAndSync;
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        record.request.session_id.clone(),
        journal_config,
        source.artifacts.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => return Err(ResourceError::CorruptReadOnly),
    };
    let key = operation_key(&record.request.session_id, &record.request.operation_id);
    let event_id = SessionEventId::new(format!("workspace-{key}"))?;
    // Journal 提交成功后但墓碑落盘前可能崩溃；先看事件，不能再移动新恢复文件。
    if !journal.contains_event_id(&event_id)? {
        ensure_mutable_source(&source.state)?;
        if source.state.last_sequence != record.source_sequence
            || source.state.project_root != record.request.expected_project_root
            || records_sha256(&source.records)? != record.source_sha256
        {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "工作目录事务的冻结日志已改变".to_owned(),
            ));
        }
        archive_collaboration(root, operations, &key, &record.request.session_id)?;
    }
    let outcome = journal.append_idempotent(
        event_id,
        record.source_sequence,
        SessionEvent::SessionWorkspaceChanged {
            expected_project_root: record.request.expected_project_root.clone(),
            project_root: record.request.project_root.clone(),
        },
    )?;
    match outcome {
        IdempotentAppendOutcome::Appended(_) | IdempotentAppendOutcome::AlreadyCommitted { .. } => {
        }
        IdempotentAppendOutcome::Indeterminate { error } => return Err(error),
        _ => {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "工作目录事件的身份或水位冲突".to_owned(),
            ));
        }
    }
    record.completed = true;
    write_record(path, &record)
}

/// 部分归档可恢复；不覆盖证据，不生成用户可见的新会话。
fn archive_collaboration(
    root: &Path,
    operations: &Path,
    key: &str,
    id: &SessionId,
) -> Result<(), ResourceError> {
    let source = secure_existing_session_dir(root, id)?;
    let evidence = secure_child_dir(operations, "evidence")?;
    let archive = secure_child_dir(&evidence, key)?;
    for (name, directory) in [
        ("collaboration-v2.json", false),
        ("collaboration-v2-agents", true),
    ] {
        let from = source.join(name);
        let to = archive.join(name);
        let metadata = match fs::symlink_metadata(&from) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ResourceError::io("inspect_workspace_collaboration", error)),
        };
        if metadata.file_type().is_symlink()
            || metadata.is_dir() != directory
            || (!directory && !metadata.is_file())
        {
            return Err(ResourceError::UnsafePath(
                "协作恢复证据不是安全文件或目录".to_owned(),
            ));
        }
        // canonicalize 也拒绝 Windows junction，归档位置必须是 Session 的直接子项。
        let canonical = fs::canonicalize(&from)
            .map_err(|error| ResourceError::io("canonicalize_workspace_evidence", error))?;
        if canonical.parent() != Some(source.as_path()) {
            return Err(ResourceError::UnsafePath("协作恢复证据越界".to_owned()));
        }
        match fs::symlink_metadata(&to) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(ResourceError::SessionMutationRecoveryRequired(
                    "协作源和归档同时存在，拒绝覆盖".to_owned(),
                ));
            }
            Err(error) => return Err(ResourceError::io("inspect_workspace_archive", error)),
        }
        fs::rename(&from, &to)
            .map_err(|error| ResourceError::io("archive_workspace_collaboration", error))?;
        sync_directory(&archive, true)?;
        sync_directory(&source, true)?;
    }
    Ok(())
}

fn validate_request(request: &SessionWorkspaceRequest) -> Result<(), ResourceError> {
    validate_operation_id(&request.operation_id)?;
    for root in [&request.expected_project_root, &request.project_root] {
        if root.is_empty()
            || root.trim() != root
            || root.len() > 16 * 1024
            || root.chars().any(char::is_control)
            || !Path::new(root).is_absolute()
        {
            return Err(ResourceError::SessionMutationNotApplicable(
                "工作目录必须是有界绝对路径".to_owned(),
            ));
        }
    }
    Ok(())
}

fn read_record(path: &Path) -> Result<Option<WorkspaceRecord>, ResourceError> {
    let bytes = match read_file_bounded(path, RECORD_LIMIT) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { actual }) => {
            return Err(ResourceError::DocumentTooLarge {
                actual,
                limit: RECORD_LIMIT,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ResourceError::io("read_workspace_transaction", error)),
    };
    let record: WorkspaceRecord = serde_json::from_slice(&bytes).map_err(|_| {
        ResourceError::SessionMutationRecoveryRequired("工作目录事务正文无效".to_owned())
    })?;
    if record.schema != "keencode/session-workspace" || record.version != 1 {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "工作目录事务格式无效".to_owned(),
        ));
    }
    validate_request(&record.request)?;
    Ok(Some(record))
}

fn write_record(path: &Path, record: &WorkspaceRecord) -> Result<(), ResourceError> {
    let bytes = serde_json::to_vec(record).map_err(|_| {
        ResourceError::SessionMutationRecoveryRequired("工作目录事务编码失败".to_owned())
    })?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(ResourceError::DocumentTooLarge {
            actual: bytes.len() as u64,
            limit: RECORD_LIMIT,
        });
    }
    atomic_write(path, &bytes, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentId, PlanState, SessionLease, SessionLeaseAcquire, TurnId};

    /// 夹具保持真实 Journal、独占 lease 和物理资源布局。
    fn fixture() -> (tempfile::TempDir, SessionWorkspaceRequest) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let id = SessionId::new("workspace-test").unwrap();
        let journal = open_journal(&root, &id);
        append(
            &journal,
            "created",
            SessionEvent::SessionCreated {
                title: "保留历史".into(),
                project_root: root.to_string_lossy().into_owned(),
            },
        );
        append(
            &journal,
            "pinned",
            SessionEvent::SessionPreferenceSet {
                pinned: Some(true),
                archived: None,
                permission_mode: None,
                vision_enabled: None,
            },
        );
        append(
            &journal,
            "plan",
            SessionEvent::PlanChanged {
                plan: PlanState {
                    enabled: true,
                    ..PlanState::default()
                },
            },
        );
        fs::write(
            root.join(id.as_str()).join("goal-evidence.txt"),
            "不搬动资源",
        )
        .unwrap();
        let request = SessionWorkspaceRequest {
            session_id: id,
            operation_id: "switch-a".into(),
            expected_project_root: root.to_string_lossy().into_owned(),
            project_root: root.join("checkout").to_string_lossy().into_owned(),
        };
        (temp, request)
    }

    fn open_journal(root: &Path, id: &SessionId) -> SessionJournal {
        match SessionJournal::open(root, id.clone(), JournalConfig::default()).unwrap() {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("夹具不应损坏"),
        }
    }

    fn append(journal: &SessionJournal, id: &str, event: SessionEvent) {
        assert!(matches!(
            journal
                .append_idempotent(
                    SessionEventId::new(id).unwrap(),
                    journal.state().unwrap().last_sequence,
                    event
                )
                .unwrap(),
            IdempotentAppendOutcome::Appended(_)
        ));
    }

    /// 直接冻结 Prepared，模拟任意步骤后的进程退出；恢复不依赖进程内故障开关。
    fn prepared(
        root: &Path,
        request: &SessionWorkspaceRequest,
    ) -> (std::path::PathBuf, std::path::PathBuf, WorkspaceRecord) {
        let source = open_source(
            root,
            &request.session_id,
            JournalConfig::default(),
            ArtifactLimits::default(),
        )
        .unwrap();
        let operations = secure_child_dir(root, "session-workspaces").unwrap();
        let records = secure_child_dir(&operations, "records").unwrap();
        let key = operation_key(&request.session_id, &request.operation_id);
        let path = records.join(format!("{key}.json"));
        let record = WorkspaceRecord {
            schema: "keencode/session-workspace".into(),
            version: 1,
            request: request.clone(),
            source_sequence: source.state.last_sequence,
            source_sha256: records_sha256(&source.records).unwrap(),
            completed: false,
        };
        write_record(&path, &record).unwrap();
        (operations, path, record)
    }

    #[test]
    fn workspace_switch_preserves_identity_preferences_plan_and_resources() {
        let (temp, request) = fixture();
        let root = fs::canonicalize(temp.path()).unwrap();
        change_session_workspace(
            &root,
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .unwrap();
        let state = open_journal(&root, &request.session_id).state().unwrap();
        assert_eq!(state.session_id, request.session_id);
        assert_eq!(state.project_root, request.project_root);
        assert_eq!(state.title, "保留历史");
        assert!(state.pinned && state.plan.enabled);
        assert_eq!(state.last_sequence, 4);
        assert_eq!(
            crate::list_session_ids(&root).unwrap(),
            vec![request.session_id.clone()]
        );
        assert_eq!(
            fs::read_to_string(
                root.join(request.session_id.as_str())
                    .join("goal-evidence.txt")
            )
            .unwrap(),
            "不搬动资源"
        );
    }

    #[test]
    fn workspace_recovery_after_partial_archive_completes_without_overwriting_evidence() {
        let (temp, request) = fixture();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join(request.session_id.as_str());
        fs::write(source.join("collaboration-v2.json"), "old-cwd").unwrap();
        fs::create_dir(source.join("collaboration-v2-agents")).unwrap();
        fs::write(
            source.join("collaboration-v2-agents/agent.json"),
            "old-agent",
        )
        .unwrap();
        let (operations, _, _) = prepared(&root, &request);
        let evidence = secure_child_dir(&operations, "evidence").unwrap();
        let key = operation_key(&request.session_id, &request.operation_id);
        let archive = secure_child_dir(&evidence, &key).unwrap();
        fs::rename(
            source.join("collaboration-v2.json"),
            archive.join("collaboration-v2.json"),
        )
        .unwrap();
        recover_session_workspaces(&root, JournalConfig::default(), ArtifactLimits::default())
            .unwrap();
        assert_eq!(
            fs::read_to_string(archive.join("collaboration-v2.json")).unwrap(),
            "old-cwd"
        );
        assert_eq!(
            fs::read_to_string(archive.join("collaboration-v2-agents/agent.json")).unwrap(),
            "old-agent"
        );
        assert_eq!(
            open_journal(&root, &request.session_id)
                .state()
                .unwrap()
                .project_root,
            request.project_root
        );
    }

    #[test]
    fn workspace_recovery_after_event_and_completed_retry_keep_new_checkpoint() {
        let (temp, request) = fixture();
        let root = fs::canonicalize(temp.path()).unwrap();
        let (_, _, record) = prepared(&root, &request);
        let journal = open_journal(&root, &request.session_id);
        let key = operation_key(&request.session_id, &request.operation_id);
        assert!(matches!(
            journal
                .append_idempotent(
                    SessionEventId::new(format!("workspace-{key}")).unwrap(),
                    record.source_sequence,
                    SessionEvent::SessionWorkspaceChanged {
                        expected_project_root: request.expected_project_root.clone(),
                        project_root: request.project_root.clone()
                    }
                )
                .unwrap(),
            IdempotentAppendOutcome::Appended(_)
        ));
        drop(journal);
        let checkpoint = root
            .join(request.session_id.as_str())
            .join("collaboration-v2.json");
        fs::write(&checkpoint, "new-cwd").unwrap();
        recover_session_workspaces(&root, JournalConfig::default(), ArtifactLimits::default())
            .unwrap();
        change_session_workspace(
            &root,
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(checkpoint).unwrap(), "new-cwd");
        assert_eq!(
            open_journal(&root, &request.session_id)
                .state()
                .unwrap()
                .last_sequence,
            4
        );
        let mut conflicting = request;
        conflicting.project_root = root.join("other").to_string_lossy().into_owned();
        assert!(
            change_session_workspace(
                &root,
                JournalConfig::default(),
                ArtifactLimits::default(),
                conflicting
            )
            .is_err()
        );
    }

    #[test]
    fn workspace_switch_rejects_active_turn_and_busy_lease() {
        let (temp, request) = fixture();
        let root = fs::canonicalize(temp.path()).unwrap();
        let lease = match SessionLease::try_acquire(&root, request.session_id.clone()).unwrap() {
            SessionLeaseAcquire::Acquired(lease) => lease,
            _ => panic!("应取得 lease"),
        };
        assert!(matches!(
            change_session_workspace(
                &root,
                JournalConfig::default(),
                ArtifactLimits::default(),
                request.clone()
            ),
            Err(ResourceError::SessionMutationBusy)
        ));
        drop(lease);
        let journal = open_journal(&root, &request.session_id);
        append(
            &journal,
            "running",
            SessionEvent::TurnStarted {
                turn_id: TurnId::new("turn-a").unwrap(),
                source_agent_id: AgentId::new(crate::ROOT_AGENT_ID).unwrap(),
                root_turn_id: TurnId::new("turn-a").unwrap(),
                parent_turn_id: None,
                prompt_summary: "read".into(),
            },
        );
        assert!(matches!(
            change_session_workspace(
                &root,
                JournalConfig::default(),
                ArtifactLimits::default(),
                request
            ),
            Err(ResourceError::SessionMutationNotApplicable(_))
        ));
        assert_eq!(journal.state().unwrap().last_sequence, 4);
    }
}
