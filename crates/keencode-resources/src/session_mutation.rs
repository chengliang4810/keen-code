//! Session 全历史复制、编辑前归档与原子日志截断事务。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic::{
    ATOMIC_TEMP_PREFIX, BoundedJson, BoundedRead, atomic_write, ensure_regular_file_or_absent,
    exclusive_lock, prepare_root, read_file_bounded, secure_child_dir, serialize_json_bounded,
    sync_directory,
};
use crate::{
    ArtifactLimits, ArtifactMaterialization, ArtifactStore, ArtifactUse, JournalConfig,
    MAX_REPLAY_PAGE_RECORDS, MessageImageSource, MessagePart, MessageRole, RequestId,
    ResourceError, SessionEvent, SessionEventId, SessionEventRecord, SessionId, SessionJournal,
    SessionLease, SessionLeaseAcquire, SessionOpen, SessionState, SessionStatus, SubAgentStatus,
    ToolResultPart, TranscriptRecord, TurnStatus, reduce_record,
};

/// Session 变更事务记录使用的固定 schema。
const MUTATION_SCHEMA: &str = "keencode/session-mutation";
/// Session 变更事务记录的唯一格式版本。
const MUTATION_VERSION: u32 = 2;
/// 单个事务记录允许占用的最大字节数。
const MAX_MUTATION_RECORD_BYTES: u64 = 64 * 1024;
/// 启动恢复一次允许扫描的最大事务记录数。
const MAX_MUTATION_RECORDS: usize = 10_000;
/// Session 变更 operationId 允许的最大 UTF-8 字节数。
const MAX_OPERATION_ID_BYTES: usize = 128;

/// 创建完整 Session 分支所需的不可变输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionForkRequest {
    /// 被复制的现有 Session。
    pub source_session_id: SessionId,
    /// 跨响应丢失重试时必须复用的操作标识。
    pub operation_id: String,
    /// 可选的新标题；为空时保留源 Session 当前标题。
    pub title: Option<String>,
}

/// 已完成 Session 分支事务的稳定结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionForkResult {
    /// 新分支的确定性 Session 标识。
    pub session_id: SessionId,
}

/// 编辑指定用户消息前所需的不可变输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEditUserRequest {
    /// 将被截断并继续使用的现有 Session。
    pub source_session_id: SessionId,
    /// 作为截断边界的根 Agent 用户消息稳定标识。
    pub target_message_id: String,
    /// 前端当前展示的目标用户消息完整模型文本。
    pub expected_text: String,
    /// 归档与截断共同使用的跨重启操作标识。
    pub operation_id: String,
}

/// 编辑前归档与源 Session 截断完成后的稳定结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEditUserResult {
    /// 保存截断前完整历史的确定性归档 Session。
    pub archived_session_id: SessionId,
}

/// 磁盘事务当前是否已经完整提交。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MutationState {
    /// 已冻结源日志锚点与全部目标摘要，尚可安全恢复。
    Prepared,
    /// 全部可见文件系统效果均已完成。
    Completed,
}

/// 两类互斥 Session 变更的持久参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MutationKind {
    /// 完整复制源 Session，并可覆盖最终标题。
    Fork {
        /// 调用方请求的新标题；`None` 表示保留源标题。
        title: Option<String>,
    },
    /// 保存完整归档后，把源日志截断到指定根用户 Turn 之前。
    EditUser {
        /// 作为截断边界的根 Agent 用户消息稳定标识。
        target_message_id: String,
        /// 不落盘用户正文，只保存期望文本摘要用于重试绑定。
        expected_text_sha256: String,
        /// 目标用户消息所在物理 Journal sequence。
        cutoff_sequence: u64,
        /// 截断后源日志的语义摘要。
        truncated_log_sha256: String,
    },
}

/// 唯一受支持的 Session 变更事务记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MutationRecord {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 原始可信 operationId。
    operation_id: String,
    /// 源 Session 标识。
    source_session_id: SessionId,
    /// 方法和全部调用参数的摘要，用于拒绝 operationId 复用。
    request_sha256: String,
    /// 具体变更类型及其恢复数据。
    kind: MutationKind,
    /// 分支或编辑前归档的目标 Session 标识。
    target_session_id: SessionId,
    /// 目标 Session 完成后的标题。
    target_title: String,
    /// 事务准备时完整源日志的语义摘要。
    source_log_sha256: String,
    /// 事务准备时源日志末尾 sequence。
    source_last_sequence: u64,
    /// 确定性目标日志的语义摘要。
    target_log_sha256: String,
    /// 确定性目标日志末尾 sequence。
    target_last_sequence: u64,
    /// 目标最后一个物理事件使用的 Unix Epoch 毫秒。
    target_time_unix_ms: u64,
    /// 当前持久事务状态。
    state: MutationState,
}

/// 已取得源 Session 独占 lease 的健康历史与 Artifact 句柄。
struct SourceBundle {
    /// 保持源 Session 跨进程独占的 lease。
    _lease: SessionLease,
    /// 用于读取并复核目标引用内容的源 ArtifactStore。
    artifacts: std::sync::Arc<ArtifactStore>,
    /// 源 Session 的全部物理 Journal 记录。
    records: Vec<SessionEventRecord>,
    /// 与全部记录一致的权威状态。
    state: SessionState,
}

/// 在资源层完成一个可崩溃恢复的完整 Session 分支。
pub fn fork_session(
    storage_root: impl AsRef<Path>,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    request: SessionForkRequest,
) -> Result<SessionForkResult, ResourceError> {
    validate_operation_id(&request.operation_id)?;
    validate_optional_title(request.title.as_deref())?;
    let root = prepare_root(storage_root.as_ref())?;
    let layout = MutationLayout::open(&root)?;
    let _operation_lock = exclusive_lock(&layout.lock_path)?;
    let operation_key = operation_key(&request.source_session_id, &request.operation_id);
    let record_path = layout.record_path(&operation_key);
    let request_sha256 = fork_request_sha256(&request);
    if let Some(record) = read_record_if_present(&record_path)? {
        validate_existing_request(&record, &request_sha256)?;
        resume_record(&root, &layout, journal_config, artifact_limits, &record)?;
        return Ok(SessionForkResult {
            session_id: record.target_session_id,
        });
    }

    let source = open_source(
        &root,
        &request.source_session_id,
        journal_config,
        artifact_limits,
    )?;
    ensure_mutable_source(&source.state)?;
    let target_session_id =
        derived_session_id("fork", &request.source_session_id, &request.operation_id)?;
    let target_title = request
        .title
        .clone()
        .unwrap_or_else(|| source.state.title.clone());
    let target_time_unix_ms = mutation_time_ms(source.state.updated_at_unix_ms)?;
    let target_records = target_records(
        &source.records,
        &target_session_id,
        &target_title,
        &operation_key,
        target_time_unix_ms,
    )?;
    encode_records(&target_records, journal_config)?;
    let record = MutationRecord {
        schema: MUTATION_SCHEMA.to_owned(),
        version: MUTATION_VERSION,
        operation_id: request.operation_id,
        source_session_id: request.source_session_id,
        request_sha256,
        kind: MutationKind::Fork {
            title: request.title,
        },
        target_session_id,
        target_title,
        source_log_sha256: records_sha256(&source.records)?,
        source_last_sequence: source.state.last_sequence,
        target_log_sha256: records_sha256(&target_records)?,
        target_last_sequence: target_records.last().map_or(0, |record| record.sequence),
        target_time_unix_ms,
        state: MutationState::Prepared,
    };
    write_record(&record_path, &record)?;
    resume_prepared(
        &root,
        &layout,
        journal_config,
        artifact_limits,
        &record,
        Some(&source),
    )?;
    #[cfg(test)]
    fail_mutation_if(&operation_key, MutationFault::BeforeCompletion)?;
    mark_completed(&record_path, record)?;
    Ok(SessionForkResult {
        session_id: target_records
            .first()
            .map(|record| record.session.clone())
            .ok_or_else(|| {
                ResourceError::SessionMutationRecoveryRequired("目标日志为空".to_owned())
            })?,
    })
}

/// 原子保存编辑前完整分支，并把源 Session 截断到指定真实用户消息之前。
pub fn prepare_edit_user(
    storage_root: impl AsRef<Path>,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    request: SessionEditUserRequest,
) -> Result<SessionEditUserResult, ResourceError> {
    validate_operation_id(&request.operation_id)?;
    validate_message_id(&request.target_message_id)?;
    if request.expected_text.is_empty() {
        return Err(ResourceError::SessionMutationNotApplicable(
            "期望用户消息不能为空".to_owned(),
        ));
    }
    let root = prepare_root(storage_root.as_ref())?;
    let layout = MutationLayout::open(&root)?;
    let _operation_lock = exclusive_lock(&layout.lock_path)?;
    let operation_key = operation_key(&request.source_session_id, &request.operation_id);
    let record_path = layout.record_path(&operation_key);
    let request_sha256 = edit_request_sha256(&request);
    if let Some(record) = read_record_if_present(&record_path)? {
        validate_existing_request(&record, &request_sha256)?;
        resume_record(&root, &layout, journal_config, artifact_limits, &record)?;
        return Ok(SessionEditUserResult {
            archived_session_id: record.target_session_id,
        });
    }

    let source = open_source(
        &root,
        &request.source_session_id,
        journal_config,
        artifact_limits,
    )?;
    ensure_mutable_source(&source.state)?;
    let cutoff_sequence = target_root_user_sequence(
        &source.records,
        &source.artifacts,
        &request.target_message_id,
        &request.expected_text,
    )?;
    let truncated_records = source
        .records
        .iter()
        .take_while(|record| record.sequence < cutoff_sequence)
        .cloned()
        .collect::<Vec<_>>();
    let truncated_state = reduce_records(&request.source_session_id, &truncated_records)?;
    ensure_mutable_source(&truncated_state)?;
    encode_records(&truncated_records, journal_config)?;
    let target_session_id = derived_session_id(
        "edit-archive",
        &request.source_session_id,
        &request.operation_id,
    )?;
    let target_title = format!("{} · 编辑前版本", source.state.title);
    let target_time_unix_ms = mutation_time_ms(source.state.updated_at_unix_ms)?;
    let target_records = target_records(
        &source.records,
        &target_session_id,
        &target_title,
        &operation_key,
        target_time_unix_ms,
    )?;
    encode_records(&target_records, journal_config)?;
    let record = MutationRecord {
        schema: MUTATION_SCHEMA.to_owned(),
        version: MUTATION_VERSION,
        operation_id: request.operation_id,
        source_session_id: request.source_session_id,
        request_sha256,
        kind: MutationKind::EditUser {
            target_message_id: request.target_message_id,
            expected_text_sha256: sha256_hex(request.expected_text.as_bytes()),
            cutoff_sequence,
            truncated_log_sha256: records_sha256(&truncated_records)?,
        },
        target_session_id,
        target_title,
        source_log_sha256: records_sha256(&source.records)?,
        source_last_sequence: source.state.last_sequence,
        target_log_sha256: records_sha256(&target_records)?,
        target_last_sequence: target_records.last().map_or(0, |record| record.sequence),
        target_time_unix_ms,
        state: MutationState::Prepared,
    };
    write_record(&record_path, &record)?;
    resume_prepared(
        &root,
        &layout,
        journal_config,
        artifact_limits,
        &record,
        Some(&source),
    )?;
    mark_completed(&record_path, record.clone())?;
    Ok(SessionEditUserResult {
        archived_session_id: record.target_session_id,
    })
}

/// 在 Runtime 打开任何 Session 前恢复全部已准备但未完成的复制或截断事务。
pub fn recover_session_mutations(
    storage_root: impl AsRef<Path>,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
) -> Result<usize, ResourceError> {
    let root = prepare_root(storage_root.as_ref())?;
    let layout = MutationLayout::open(&root)?;
    let _operation_lock = exclusive_lock(&layout.lock_path)?;
    let records = list_records(&layout)?;
    let mut recovered = 0_usize;
    for (path, mut record) in records {
        if record.state == MutationState::Completed {
            cleanup_staging(
                &layout,
                &operation_key(&record.source_session_id, &record.operation_id),
            )?;
            continue;
        }
        resume_prepared(
            &root,
            &layout,
            journal_config,
            artifact_limits,
            &record,
            None,
        )?;
        record.state = MutationState::Completed;
        write_record(&path, &record)?;
        recovered = recovered.saturating_add(1);
    }
    Ok(recovered)
}

/// Session 变更固定目录布局。
struct MutationLayout {
    /// 持久事务记录目录。
    records_root: PathBuf,
    /// 不会被 Session 列表观察到的构建目录。
    staging_root: PathBuf,
    /// 所有复制和截断共同使用的跨进程锁。
    lock_path: PathBuf,
}

impl MutationLayout {
    /// 创建并验证唯一事务目录布局。
    fn open(storage_root: &Path) -> Result<Self, ResourceError> {
        let root = secure_child_dir(storage_root, "session-mutations")?;
        let records_root = secure_child_dir(&root, "records")?;
        let staging_root = secure_child_dir(&root, "staging")?;
        let lock_path = root.join("operations.lock");
        ensure_regular_file_or_absent(&lock_path)?;
        Ok(Self {
            records_root,
            staging_root,
            lock_path,
        })
    }

    /// 返回一个 operationId 在源 Session 内唯一的记录路径。
    fn record_path(&self, operation_key: &str) -> PathBuf {
        self.records_root.join(format!("{operation_key}.json"))
    }

    /// 返回一个 operationId 独占的隐藏构建根。
    fn operation_staging_root(&self, operation_key: &str) -> PathBuf {
        self.staging_root.join(operation_key)
    }
}

/// 校验 operationId 具有稳定的有限文本身份。
fn validate_operation_id(operation_id: &str) -> Result<(), ResourceError> {
    if operation_id.is_empty()
        || operation_id.len() > MAX_OPERATION_ID_BYTES
        || operation_id.trim() != operation_id
        || operation_id.chars().any(char::is_control)
    {
        return Err(ResourceError::SessionMutationNotApplicable(
            "operationId 无效".to_owned(),
        ));
    }
    Ok(())
}

/// 校验用户消息标识可作为稳定事务输入，但不把它当作文件路径使用。
fn validate_message_id(message_id: &str) -> Result<(), ResourceError> {
    if message_id.is_empty()
        || message_id.len() > MAX_OPERATION_ID_BYTES
        || message_id.trim() != message_id
        || message_id.chars().any(char::is_control)
    {
        return Err(ResourceError::SessionMutationNotApplicable(
            "目标用户消息标识无效".to_owned(),
        ));
    }
    Ok(())
}

/// 校验可选标题不依赖隐式裁剪。
fn validate_optional_title(title: Option<&str>) -> Result<(), ResourceError> {
    if title.is_some_and(|title| title.is_empty() || title.trim() != title) {
        return Err(ResourceError::SessionMutationNotApplicable(
            "Session 标题不能为空或包含首尾空白".to_owned(),
        ));
    }
    Ok(())
}

/// 打开并完整验证源 Session，同时保持跨进程 lease 存活。
fn open_source(
    root: &Path,
    session_id: &SessionId,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
) -> Result<SourceBundle, ResourceError> {
    if !crate::list_session_ids(root)?.contains(session_id) {
        return Err(ResourceError::SessionMutationNotApplicable(
            "源 Session 不存在".to_owned(),
        ));
    }
    let lease = acquire_session_lease(root, session_id.clone())?;
    let artifacts = std::sync::Arc::new(ArtifactStore::open(
        root,
        session_id.clone(),
        artifact_limits,
    )?);
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        session_id.clone(),
        journal_config,
        artifacts.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => return Err(ResourceError::CorruptReadOnly),
    };
    let state = journal.state()?;
    artifacts.recover_for_state(&lease, &state)?;
    let records = read_all_records(&journal)?;
    Ok(SourceBundle {
        _lease: lease,
        artifacts,
        records,
        state,
    })
}

/// 非阻塞取得一个现有 Session 的独占资源 lease。
fn acquire_session_lease(
    root: &Path,
    session_id: SessionId,
) -> Result<SessionLease, ResourceError> {
    match SessionLease::try_acquire(root, session_id)? {
        SessionLeaseAcquire::Acquired(lease) => Ok(lease),
        SessionLeaseAcquire::Busy { .. } => Err(ResourceError::SessionMutationBusy),
    }
}

/// 拒绝仍有活动工作或已经关闭的源状态。
fn ensure_mutable_source(state: &SessionState) -> Result<(), ResourceError> {
    let active = state
        .turns
        .values()
        .any(|turn| turn.status == TurnStatus::Running)
        || state.tools.values().any(|tool| tool.outcome.is_none())
        || state.terminals.values().any(|terminal| !terminal.exited)
        || state.sub_agents.values().any(|agent| {
            matches!(
                agent.status,
                SubAgentStatus::Pending | SubAgentStatus::Running | SubAgentStatus::Waiting
            )
        })
        || state.worktrees.values().any(|worktree| !worktree.released);
    if active || state.status == SessionStatus::Closed {
        return Err(ResourceError::SessionMutationNotApplicable(
            "Session 仍有活动工作或已经关闭".to_owned(),
        ));
    }
    Ok(())
}

/// 分页读取完整权威日志，并确认读取水位没有漂移。
fn read_all_records(journal: &SessionJournal) -> Result<Vec<SessionEventRecord>, ResourceError> {
    let mut records = Vec::new();
    let mut after = None;
    let mut through = None;
    loop {
        let page = journal.read_page(after, MAX_REPLAY_PAGE_RECORDS)?;
        if through.is_some_and(|through| through != page.through_sequence) {
            return Err(ResourceError::ReplayLogChanged);
        }
        through = Some(page.through_sequence);
        records.extend(page.records);
        if !page.has_more {
            break;
        }
        after = page.next_after;
    }
    Ok(records)
}

/// 将记录完整归约为指定 Session 的状态并复核创建事实。
fn reduce_records(
    session_id: &SessionId,
    records: &[SessionEventRecord],
) -> Result<SessionState, ResourceError> {
    let mut state = SessionState::empty(session_id.clone());
    for record in records {
        reduce_record(&mut state, record.clone())
            .map_err(|error| ResourceError::Reduction(error.message))?;
    }
    if !state.created {
        return Err(ResourceError::SessionMutationNotApplicable(
            "Session 尚未创建".to_owned(),
        ));
    }
    Ok(state)
}

/// 定位指定根用户消息的物理记录，并精确校验完整文本。
fn target_root_user_sequence(
    records: &[SessionEventRecord],
    artifacts: &ArtifactStore,
    target_message_id: &str,
    expected_text: &str,
) -> Result<u64, ResourceError> {
    let mut candidate = None;
    for record in records {
        let matches = root_user_messages(&record.event)
            .into_iter()
            .filter(|message| message.message_id == target_message_id)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(ResourceError::SessionMutationNotApplicable(
                "目标用户消息标识不唯一".to_owned(),
            ));
        }
        let Some(message) = matches.into_iter().next() else {
            continue;
        };
        if candidate.replace((record, message)).is_some() {
            return Err(ResourceError::SessionMutationNotApplicable(
                "目标用户消息标识不唯一".to_owned(),
            ));
        }
    }
    let (record, message) = candidate.ok_or_else(|| {
        ResourceError::SessionMutationNotApplicable("目标用户消息不存在或不可编辑".to_owned())
    })?;
    let root_messages = root_user_messages(&record.event);
    if root_messages.len() > 1
        && root_messages
            .first()
            .is_some_and(|message| message.message_id != target_message_id)
    {
        return Err(ResourceError::SessionMutationNotApplicable(
            "目标用户消息不是原子批次中的第一条，不能安全截断".to_owned(),
        ));
    }
    let actual = materialize_user_text(message, artifacts)?;
    if actual != expected_text {
        return Err(ResourceError::SessionMutationNotApplicable(
            "目标用户消息已变化".to_owned(),
        ));
    }
    Ok(record.sequence)
}

/// 从一个物理事件中提取与根 Turn 起点原子提交的真实用户消息。
fn root_user_messages(event: &SessionEvent) -> Vec<&crate::SessionMessage> {
    let SessionEvent::AtomicBatch { events } = event else {
        return Vec::new();
    };
    let Some(root_turn_id) = events.iter().find_map(|event| match event {
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            ..
        } if source_agent_id.as_str() == crate::ROOT_AGENT_ID
            && root_turn_id == turn_id
            && parent_turn_id.is_none() =>
        {
            Some(turn_id)
        }
        _ => None,
    }) else {
        return Vec::new();
    };
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::MessageAdded { message }
                if message.role == MessageRole::User
                    && message.agent_id.is_none()
                    && message.turn_id.as_ref() == Some(root_turn_id) =>
            {
                Some(message)
            }
            _ => None,
        })
        .collect()
}

/// 将可编辑用户消息恢复为前端提交给 Runtime 的完整文本。
fn materialize_user_text(
    message: &crate::SessionMessage,
    artifacts: &ArtifactStore,
) -> Result<String, ResourceError> {
    let mut text = String::new();
    for part in &message.content {
        match part {
            MessagePart::Text { text: part } => text.push_str(part),
            MessagePart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Utf8Text,
            } => match artifacts.materialize_use(artifact, ArtifactMaterialization::Utf8Text)? {
                crate::ArtifactMaterialized::Utf8Text(part) => text.push_str(&part),
                crate::ArtifactMaterialized::Image { .. }
                | crate::ArtifactMaterialized::Binary { .. } => {
                    return Err(ResourceError::SessionMutationNotApplicable(
                        "目标用户消息不是纯文本".to_owned(),
                    ));
                }
            },
            MessagePart::Reasoning { .. }
            | MessagePart::Image { .. }
            | MessagePart::ToolCall { .. }
            | MessagePart::ToolResult { .. }
            | MessagePart::Artifact { .. } => {
                return Err(ResourceError::SessionMutationNotApplicable(
                    "目标用户消息不是纯文本".to_owned(),
                ));
            }
        }
    }
    Ok(text)
}

/// 将源记录重绑定到目标 Session，并追加确定性标题覆盖事件。
fn target_records(
    source: &[SessionEventRecord],
    target_session_id: &SessionId,
    target_title: &str,
    operation_key: &str,
    target_time_unix_ms: u64,
) -> Result<Vec<SessionEventRecord>, ResourceError> {
    let request_id_bindings = request_id_bindings(source, target_session_id)?;
    let mut records = source
        .iter()
        .cloned()
        .map(|mut record| {
            record.session = target_session_id.clone();
            record.event = rebind_event_request_ids(&record.event, &request_id_bindings)?;
            Ok(record)
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;
    let state = reduce_records(target_session_id, &records)?;
    let title_event_id = SessionEventId::new(format!("session-mutation-title-{operation_key}"))?;
    if records
        .iter()
        .any(|record| record.event_id == title_event_id)
    {
        return Err(ResourceError::SessionMutationConflict);
    }
    let sequence = state
        .last_sequence
        .checked_add(1)
        .ok_or(ResourceError::JournalRecordLimit {
            actual: u64::MAX,
            limit: u64::MAX,
        })?;
    records.push(SessionEventRecord {
        schema: crate::SESSION_EVENT_SCHEMA.to_owned(),
        version: crate::SESSION_EVENT_VERSION,
        event_id: title_event_id,
        session: target_session_id.clone(),
        sequence,
        time_unix_ms: target_time_unix_ms.max(state.updated_at_unix_ms),
        event: SessionEvent::SessionRenamed {
            title: target_title.to_owned(),
        },
    });
    let final_state = reduce_records(target_session_id, &records)?;
    if final_state.title != target_title {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "目标标题无法确定性应用".to_owned(),
        ));
    }
    Ok(records)
}

/// 为复制到目标 Session 的全部工具请求派生新的 Session 作用域标识。
///
/// RequestId 的摘要输入包含 SessionId；目标 Journal 若继续保存源标识，会在重放时被
/// reducer 拒绝。因此这里先扫描完整源日志，建立一一映射，再统一改写所有引用。
fn request_id_bindings(
    source: &[SessionEventRecord],
    target_session_id: &SessionId,
) -> Result<BTreeMap<RequestId, RequestId>, ResourceError> {
    let mut bindings = BTreeMap::new();
    let mut reverse = BTreeMap::new();
    for record in source {
        collect_request_id_binding(
            &record.event,
            target_session_id,
            &mut bindings,
            &mut reverse,
        )?;
    }
    Ok(bindings)
}

/// 递归扫描顶层或原子批次中的 ToolRequested，并拒绝不一致的派生映射。
fn collect_request_id_binding(
    event: &SessionEvent,
    target_session_id: &SessionId,
    bindings: &mut BTreeMap<RequestId, RequestId>,
    reverse: &mut BTreeMap<RequestId, RequestId>,
) -> Result<(), ResourceError> {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                collect_request_id_binding(event, target_session_id, bindings, reverse)?;
            }
        }
        SessionEvent::ToolRequested { request } => {
            let target_request_id = RequestId::derive_model_tool_call(
                target_session_id,
                &request.turn_id,
                &request.agent_id,
                request.model_round,
                &request.model_tool_call_id,
            )?;
            if bindings
                .insert(request.request_id.clone(), target_request_id.clone())
                .is_some_and(|existing| existing != target_request_id)
                || reverse
                    .insert(target_request_id, request.request_id.clone())
                    .is_some_and(|existing| existing != request.request_id)
            {
                return Err(ResourceError::SessionMutationRecoveryRequired(
                    "复制 Session 的工具请求标识映射不一致".to_owned(),
                ));
            }
        }
        SessionEvent::SessionCreated { .. }
        | SessionEvent::SessionRenamed { .. }
        | SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TurnStarted { .. }
        | SessionEvent::TurnCompleted { .. }
        | SessionEvent::TurnStopped { .. }
        | SessionEvent::MessageAdded { .. }
        | SessionEvent::TranscriptSegmentCommitted { .. }
        | SessionEvent::DynamicInputReceiptCommitted { .. }
        | SessionEvent::ModelRoundCompleted { .. }
        | SessionEvent::ToolExecutionStarted { .. }
        | SessionEvent::ToolFileChangePrepared { .. }
        | SessionEvent::ToolFileChangeApplied { .. }
        | SessionEvent::ToolCompleted { .. }
        | SessionEvent::ToolSideEffectUnknown { .. }
        | SessionEvent::TerminalStarted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::CompactionApplied { .. }
        | SessionEvent::TodoReplaced { .. }
        | SessionEvent::PlanChanged { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::TitleGenerated { .. }
        | SessionEvent::SubAgentSpawned { .. }
        | SessionEvent::SubAgentStatusChanged { .. }
        | SessionEvent::MailboxMessageQueued { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => {}
    }
    Ok(())
}

/// 将目标 Session 的工具请求标识写入一个已知引用，未知引用一律失败关闭。
fn rebound_request_id(
    request_id: &RequestId,
    bindings: &BTreeMap<RequestId, RequestId>,
) -> Result<RequestId, ResourceError> {
    bindings.get(request_id).cloned().ok_or_else(|| {
        ResourceError::SessionMutationRecoveryRequired(
            "复制 Session 的工具请求引用无法重绑定".to_owned(),
        )
    })
}

/// 递归改写所有 SessionEvent 中的 RequestId，保留模型调用与 Transcript 正文不变。
fn rebind_event_request_ids(
    event: &SessionEvent,
    bindings: &BTreeMap<RequestId, RequestId>,
) -> Result<SessionEvent, ResourceError> {
    match event {
        SessionEvent::AtomicBatch { events } => Ok(SessionEvent::AtomicBatch {
            events: events
                .iter()
                .map(|event| rebind_event_request_ids(event, bindings))
                .collect::<Result<Vec<_>, _>>()?,
        }),
        SessionEvent::ToolRequested { request } => {
            let mut request = request.clone();
            request.request_id = rebound_request_id(&request.request_id, bindings)?;
            Ok(SessionEvent::ToolRequested { request })
        }
        SessionEvent::ToolExecutionStarted { request_id } => {
            Ok(SessionEvent::ToolExecutionStarted {
                request_id: rebound_request_id(request_id, bindings)?,
            })
        }
        SessionEvent::ToolFileChangePrepared { request_id, change } => {
            Ok(SessionEvent::ToolFileChangePrepared {
                request_id: rebound_request_id(request_id, bindings)?,
                change: change.clone(),
            })
        }
        SessionEvent::ToolFileChangeApplied { request_id } => {
            Ok(SessionEvent::ToolFileChangeApplied {
                request_id: rebound_request_id(request_id, bindings)?,
            })
        }
        SessionEvent::ToolCompleted {
            request_id,
            outcome,
        } => Ok(SessionEvent::ToolCompleted {
            request_id: rebound_request_id(request_id, bindings)?,
            outcome: outcome.clone(),
        }),
        SessionEvent::ToolSideEffectUnknown { request_id, result } => {
            Ok(SessionEvent::ToolSideEffectUnknown {
                request_id: rebound_request_id(request_id, bindings)?,
                result: result.clone(),
            })
        }
        SessionEvent::TerminalStarted { terminal } => {
            let mut terminal = terminal.clone();
            terminal.request_id = rebound_request_id(&terminal.request_id, bindings)?;
            Ok(SessionEvent::TerminalStarted { terminal })
        }
        SessionEvent::SessionCreated { .. }
        | SessionEvent::SessionRenamed { .. }
        | SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TurnStarted { .. }
        | SessionEvent::TurnCompleted { .. }
        | SessionEvent::TurnStopped { .. }
        | SessionEvent::MessageAdded { .. }
        | SessionEvent::TranscriptSegmentCommitted { .. }
        | SessionEvent::DynamicInputReceiptCommitted { .. }
        | SessionEvent::ModelRoundCompleted { .. }
        | SessionEvent::TerminalOutputRecorded { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::CompactionApplied { .. }
        | SessionEvent::TodoReplaced { .. }
        | SessionEvent::PlanChanged { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::TitleGenerated { .. }
        | SessionEvent::SubAgentSpawned { .. }
        | SessionEvent::SubAgentStatusChanged { .. }
        | SessionEvent::MailboxMessageQueued { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => Ok(event.clone()),
    }
}

/// 恢复一条已经持久准备的事务，并在成功后提交完成墓碑。
fn resume_record(
    root: &Path,
    layout: &MutationLayout,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    record: &MutationRecord,
) -> Result<(), ResourceError> {
    validate_record(record)?;
    if record.state == MutationState::Completed {
        cleanup_staging(
            layout,
            &operation_key(&record.source_session_id, &record.operation_id),
        )?;
        return Ok(());
    }
    resume_prepared(root, layout, journal_config, artifact_limits, record, None)?;
    let record_path = layout.record_path(&operation_key(
        &record.source_session_id,
        &record.operation_id,
    ));
    mark_completed(&record_path, record.clone())
}

/// 根据当前文件系统事实把 Prepared 事务收敛到完整可见结果。
fn resume_prepared(
    root: &Path,
    layout: &MutationLayout,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    record: &MutationRecord,
    source: Option<&SourceBundle>,
) -> Result<(), ResourceError> {
    validate_record(record)?;
    if target_is_complete(root, journal_config, artifact_limits, record)?
        && matches!(record.kind, MutationKind::Fork { .. })
    {
        cleanup_staging(
            layout,
            &operation_key(&record.source_session_id, &record.operation_id),
        )?;
        return Ok(());
    }

    let owned_source;
    let source = match source {
        Some(source) => source,
        None => {
            owned_source = open_source(
                root,
                &record.source_session_id,
                journal_config,
                artifact_limits,
            )?;
            &owned_source
        }
    };
    let source_digest = records_sha256(&source.records)?;
    let source_is_original = source_digest == record.source_log_sha256
        && source.state.last_sequence == record.source_last_sequence;
    let source_is_truncated = match &record.kind {
        MutationKind::Fork { .. } => false,
        MutationKind::EditUser {
            cutoff_sequence,
            truncated_log_sha256,
            ..
        } => {
            source_digest == *truncated_log_sha256
                && source.state.last_sequence == cutoff_sequence.saturating_sub(1)
        }
    };
    if !source_is_original && !source_is_truncated {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "源 Session 已偏离事务准备锚点".to_owned(),
        ));
    }

    if !target_is_complete(root, journal_config, artifact_limits, record)? {
        if source_is_truncated {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "源日志已截断但归档分支缺失".to_owned(),
            ));
        }
        let target_records = target_records(
            &source.records,
            &record.target_session_id,
            &record.target_title,
            &operation_key(&record.source_session_id, &record.operation_id),
            record.target_time_unix_ms,
        )?;
        if records_sha256(&target_records)? != record.target_log_sha256
            || target_records.last().map_or(0, |item| item.sequence) != record.target_last_sequence
        {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "目标日志无法从冻结源锚点重建".to_owned(),
            ));
        }
        publish_target(
            root,
            layout,
            journal_config,
            artifact_limits,
            record,
            source,
            &target_records,
        )?;
    }

    if let MutationKind::EditUser {
        cutoff_sequence,
        truncated_log_sha256,
        ..
    } = &record.kind
    {
        #[cfg(test)]
        fail_mutation_if(
            &operation_key(&record.source_session_id, &record.operation_id),
            MutationFault::AfterArchivePublished,
        )?;
        if source_is_original {
            let truncated = source
                .records
                .iter()
                .take_while(|item| item.sequence < *cutoff_sequence)
                .cloned()
                .collect::<Vec<_>>();
            if records_sha256(&truncated)? != *truncated_log_sha256 {
                return Err(ResourceError::SessionMutationRecoveryRequired(
                    "截断日志摘要与准备记录不一致".to_owned(),
                ));
            }
            rewrite_source_log(root, &record.source_session_id, journal_config, &truncated)?;
            #[cfg(test)]
            fail_mutation_if(
                &operation_key(&record.source_session_id, &record.operation_id),
                MutationFault::AfterSourceRewritten,
            )?;
        }
    }
    cleanup_staging(
        layout,
        &operation_key(&record.source_session_id, &record.operation_id),
    )
}

/// 构建并原子发布一个不会被 Session 列表观察到半成品的目标目录。
#[allow(clippy::too_many_arguments)]
fn publish_target(
    root: &Path,
    layout: &MutationLayout,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    record: &MutationRecord,
    source: &SourceBundle,
    target_records: &[SessionEventRecord],
) -> Result<(), ResourceError> {
    let operation_key = operation_key(&record.source_session_id, &record.operation_id);
    cleanup_staging(layout, &operation_key)?;
    let staging = secure_child_dir(&layout.staging_root, &operation_key)?;
    let staging_lease = acquire_session_lease(&staging, record.target_session_id.clone())?;
    let target_artifacts =
        ArtifactStore::open(&staging, record.target_session_id.clone(), artifact_limits)?;
    copy_state_artifacts(&source.artifacts, &target_artifacts, &source.state)?;
    let target_log = encode_records(target_records, journal_config)?;
    let staging_session_dir = staging_lease.session_dir().to_path_buf();
    atomic_write(&staging_session_dir.join("events.jsonl"), &target_log, true)?;
    let validator = std::sync::Arc::new(ArtifactStore::open(
        &staging,
        record.target_session_id.clone(),
        artifact_limits,
    )?);
    let target_journal = match SessionJournal::open_with_artifact_validator(
        &staging,
        record.target_session_id.clone(),
        journal_config,
        validator.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "目标构建日志未通过健康校验".to_owned(),
            ));
        }
    };
    let target_state = target_journal.state()?;
    validator.recover_for_state(&staging_lease, &target_state)?;
    target_journal.write_snapshot()?;
    drop(target_journal);
    drop(validator);
    drop(target_artifacts);
    drop(staging_lease);

    let sessions_root = secure_child_dir(root, "sessions")?;
    let destination = sessions_root.join(record.target_session_id.as_str());
    ensure_absent_target(&destination)?;
    fs::rename(&staging_session_dir, &destination)
        .map_err(|error| ResourceError::io("publish_session_mutation_target", error))?;
    sync_directory(&sessions_root, true)?;
    if !target_is_complete(root, journal_config, artifact_limits, record)? {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "目标发布后校验失败".to_owned(),
        ));
    }
    Ok(())
}

/// 判断目标不存在，或存在且与事务摘要完全一致。
fn target_is_complete(
    root: &Path,
    journal_config: JournalConfig,
    artifact_limits: ArtifactLimits,
    record: &MutationRecord,
) -> Result<bool, ResourceError> {
    let sessions_root = secure_child_dir(root, "sessions")?;
    let target = sessions_root.join(record.target_session_id.as_str());
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ResourceError::io("inspect_session_mutation_target", error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "目标 Session 路径类型无效".to_owned(),
        ));
    }
    let lease = acquire_session_lease(root, record.target_session_id.clone())?;
    let artifacts = std::sync::Arc::new(ArtifactStore::open(
        root,
        record.target_session_id.clone(),
        artifact_limits,
    )?);
    let journal = match SessionJournal::open_with_artifact_validator(
        root,
        record.target_session_id.clone(),
        journal_config,
        artifacts.clone(),
    )? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "目标 Session 日志已损坏".to_owned(),
            ));
        }
    };
    let state = journal.state()?;
    artifacts.recover_for_state(&lease, &state)?;
    let records = read_all_records(&journal)?;
    let matches = state.title == record.target_title
        && state.last_sequence == record.target_last_sequence
        && records_sha256(&records)? == record.target_log_sha256;
    if !matches {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "目标 Session 与事务摘要不一致".to_owned(),
        ));
    }
    Ok(true)
}

/// 拒绝覆盖已经存在的可见 Session 目录。
fn ensure_absent_target(target: &Path) -> Result<(), ResourceError> {
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ResourceError::SessionMutationRecoveryRequired(
            "目标 Session 已存在且尚未验证".to_owned(),
        )),
        Err(error) => Err(ResourceError::io(
            "inspect_session_mutation_destination",
            error,
        )),
    }
}

/// 在持有源 Runtime lease 时原子替换日志，并移除可安全重建的旧 Snapshot。
fn rewrite_source_log(
    root: &Path,
    source_session_id: &SessionId,
    journal_config: JournalConfig,
    records: &[SessionEventRecord],
) -> Result<(), ResourceError> {
    let sessions_root = secure_child_dir(root, "sessions")?;
    let session_dir = secure_existing_session_dir(&sessions_root, source_session_id)?;
    let _append_lock = exclusive_lock(&session_dir.join("append.lock"))?;
    let bytes = encode_records(records, journal_config)?;
    atomic_write(&session_dir.join("events.jsonl"), &bytes, true)?;
    let snapshot_path = session_dir.join("snapshot.json");
    ensure_regular_file_or_absent(&snapshot_path)?;
    match fs::remove_file(&snapshot_path) {
        Ok(()) => sync_directory(&session_dir, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResourceError::io("remove_stale_session_snapshot", error)),
    }
}

/// 返回已存在且未越过 sessions 根的源目录。
fn secure_existing_session_dir(
    sessions_root: &Path,
    session_id: &SessionId,
) -> Result<PathBuf, ResourceError> {
    let candidate = sessions_root.join(session_id.as_str());
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| ResourceError::io("inspect_session_mutation_source", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ResourceError::UnsafePath(
            "Session 变更源不是安全目录".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| ResourceError::io("canonicalize_session_mutation_source", error))?;
    if canonical.parent() != Some(sessions_root) {
        return Err(ResourceError::UnsafePath(
            "Session 变更源越过 sessions 根目录".to_owned(),
        ));
    }
    Ok(canonical)
}

/// 将选定权威记录编码为完整且有界的 JSONL 文件。
fn encode_records(
    records: &[SessionEventRecord],
    config: JournalConfig,
) -> Result<Vec<u8>, ResourceError> {
    if u64::try_from(records.len()).unwrap_or(u64::MAX) > config.max_records {
        return Err(ResourceError::JournalRecordLimit {
            actual: u64::try_from(records.len()).unwrap_or(u64::MAX),
            limit: config.max_records,
        });
    }
    let mut bytes = Vec::new();
    for record in records {
        let mut line = match serialize_json_bounded(record, config.max_event_bytes, false)? {
            BoundedJson::Bytes(line) => line,
            BoundedJson::TooLarge { actual } => {
                return Err(ResourceError::EventTooLarge {
                    actual,
                    limit: config.max_event_bytes,
                });
            }
        };
        line.push(b'\n');
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if line_len > config.max_event_bytes {
            return Err(ResourceError::EventTooLarge {
                actual: line_len,
                limit: config.max_event_bytes,
            });
        }
        bytes.extend_from_slice(&line);
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if total > config.max_log_bytes {
            return Err(ResourceError::JournalTooLarge {
                actual: total,
                limit: config.max_log_bytes,
            });
        }
    }
    Ok(bytes)
}

/// 把源状态仍引用的全部 Artifact 复制并重新验证到目标 Store。
fn copy_state_artifacts(
    source: &ArtifactStore,
    target: &ArtifactStore,
    state: &SessionState,
) -> Result<(), ResourceError> {
    let references = state_artifact_references(state)?;
    for reference in references.values() {
        let bytes = source.read_use(reference)?;
        let copied = target.put(&bytes, reference.media_type.clone())?;
        if copied.as_event_use() != *reference {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "Artifact 复制后身份不一致".to_owned(),
            ));
        }
    }
    Ok(())
}

/// 从完整状态收集并去重所有隐式 Session 作用域的 Artifact 引用。
fn state_artifact_references(
    state: &SessionState,
) -> Result<BTreeMap<String, ArtifactUse>, ResourceError> {
    let mut references = BTreeMap::new();
    for record in &state.transcript {
        match record {
            TranscriptRecord::MessageAdded(message) => {
                collect_message_artifacts(&message.content, &mut references)?;
            }
            TranscriptRecord::SegmentCommitted(segment) => {
                for message in &segment.messages {
                    collect_message_artifacts(&message.content, &mut references)?;
                }
            }
            TranscriptRecord::CompactionApplied(_) => {}
        }
    }
    for lifecycle in state.tools.values() {
        if let Some(outcome) = &lifecycle.outcome {
            collect_tool_result_artifacts(&outcome.result.content, &mut references)?;
        }
        if let Some(change) = &lifecycle.file_change {
            // Session 分支及编辑前归档必须同时复制写前、写后证据，不能只复制模型结果。
            for snapshot in change.before.iter().chain(std::iter::once(&change.after)) {
                snapshot.validate_shape()?;
                for artifact in &snapshot.chunks {
                    insert_artifact(artifact, &mut references)?;
                }
            }
        }
    }
    for terminal in state.terminals.values() {
        for artifact in &terminal.output_artifacts {
            insert_artifact(artifact, &mut references)?;
        }
    }
    if let Some(artifact) = &state.plan.plan_artifact {
        insert_artifact(artifact, &mut references)?;
    }
    for message in state.mailbox.values() {
        if let Some(artifact) = &message.artifact {
            insert_artifact(artifact, &mut references)?;
        }
    }
    Ok(references)
}

/// 递归收集一条消息内的 Artifact 引用。
fn collect_message_artifacts(
    parts: &[MessagePart],
    references: &mut BTreeMap<String, ArtifactUse>,
) -> Result<(), ResourceError> {
    for part in parts {
        match part {
            MessagePart::Image {
                source: MessageImageSource::Artifact { artifact },
            }
            | MessagePart::Artifact { artifact, .. } => insert_artifact(artifact, references)?,
            MessagePart::ToolResult { content, .. } => {
                collect_tool_result_artifacts(content, references)?;
            }
            MessagePart::Text { .. }
            | MessagePart::Reasoning { .. }
            | MessagePart::ToolCall { .. }
            | MessagePart::Image {
                source: MessageImageSource::Url { .. },
            } => {}
        }
    }
    Ok(())
}

/// 收集工具结果中的 Artifact 引用。
fn collect_tool_result_artifacts(
    parts: &[ToolResultPart],
    references: &mut BTreeMap<String, ArtifactUse>,
) -> Result<(), ResourceError> {
    for part in parts {
        match part {
            ToolResultPart::Image {
                source: MessageImageSource::Artifact { artifact },
            }
            | ToolResultPart::Artifact { artifact, .. } => insert_artifact(artifact, references)?,
            ToolResultPart::Text { .. }
            | ToolResultPart::Image {
                source: MessageImageSource::Url { .. },
            } => {}
        }
    }
    Ok(())
}

/// 按内容身份去重，并拒绝同一身份出现不同声明。
fn insert_artifact(
    artifact: &ArtifactUse,
    references: &mut BTreeMap<String, ArtifactUse>,
) -> Result<(), ResourceError> {
    match references.get(artifact.artifact_id.as_str()) {
        Some(existing) if existing != artifact => Err(ResourceError::ArtifactMetadataMismatch),
        Some(_) => Ok(()),
        None => {
            references.insert(artifact.artifact_id.as_str().to_owned(), artifact.clone());
            Ok(())
        }
    }
}

/// 读取并严格校验一个已经存在的事务记录。
fn read_record_if_present(path: &Path) -> Result<Option<MutationRecord>, ResourceError> {
    ensure_regular_file_or_absent(path)?;
    let bytes = match read_file_bounded(path, MAX_MUTATION_RECORD_BYTES) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { actual }) => {
            return Err(ResourceError::DocumentTooLarge {
                actual,
                limit: MAX_MUTATION_RECORD_BYTES,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ResourceError::io("read_session_mutation_record", error)),
    };
    let record: MutationRecord =
        serde_json::from_slice(&bytes).map_err(|error| ResourceError::Json(error.to_string()))?;
    validate_record(&record)?;
    Ok(Some(record))
}

/// 原子写入一条有界事务记录。
fn write_record(path: &Path, record: &MutationRecord) -> Result<(), ResourceError> {
    validate_record(record)?;
    let bytes = match serialize_json_bounded(record, MAX_MUTATION_RECORD_BYTES, true)? {
        BoundedJson::Bytes(bytes) => bytes,
        BoundedJson::TooLarge { actual } => {
            return Err(ResourceError::DocumentTooLarge {
                actual,
                limit: MAX_MUTATION_RECORD_BYTES,
            });
        }
    };
    atomic_write(path, &bytes, true)
}

/// 将 Prepared 事务原子替换为不可复用的完成墓碑。
fn mark_completed(path: &Path, mut record: MutationRecord) -> Result<(), ResourceError> {
    record.state = MutationState::Completed;
    write_record(path, &record)
}

/// 校验事务记录结构、文件身份和恢复字段一致性。
fn validate_record(record: &MutationRecord) -> Result<(), ResourceError> {
    if record.schema != MUTATION_SCHEMA || record.version != MUTATION_VERSION {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "事务记录 schema 或版本无效".to_owned(),
        ));
    }
    validate_operation_id(&record.operation_id)?;
    if record.source_last_sequence == 0
        || record.target_last_sequence == 0
        || !is_sha256(&record.request_sha256)
        || !is_sha256(&record.source_log_sha256)
        || !is_sha256(&record.target_log_sha256)
        || record.target_title.trim().is_empty()
        || record.target_time_unix_ms == 0
    {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "事务记录字段无效".to_owned(),
        ));
    }
    if let MutationKind::EditUser {
        target_message_id,
        expected_text_sha256,
        cutoff_sequence,
        truncated_log_sha256,
    } = &record.kind
        && (validate_message_id(target_message_id).is_err()
            || !is_sha256(expected_text_sha256)
            || !is_sha256(truncated_log_sha256)
            || *cutoff_sequence <= 1
            || *cutoff_sequence > record.source_last_sequence)
    {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "编辑事务恢复字段无效".to_owned(),
        ));
    }
    let kind = match record.kind {
        MutationKind::Fork { .. } => "fork",
        MutationKind::EditUser { .. } => "edit-archive",
    };
    if derived_session_id(kind, &record.source_session_id, &record.operation_id)?
        != record.target_session_id
    {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "事务目标 Session 身份无效".to_owned(),
        ));
    }
    let expected_request_sha256 = match &record.kind {
        MutationKind::Fork { title } => fork_request_sha256_parts(
            &record.source_session_id,
            &record.operation_id,
            title.as_deref(),
        ),
        MutationKind::EditUser {
            target_message_id,
            expected_text_sha256,
            ..
        } => edit_request_sha256_parts(
            &record.source_session_id,
            &record.operation_id,
            target_message_id,
            expected_text_sha256,
        ),
    };
    if expected_request_sha256 != record.request_sha256 {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "事务请求摘要与正文不一致".to_owned(),
        ));
    }
    Ok(())
}

/// 拒绝同一 operationId 绑定到不同方法或参数。
fn validate_existing_request(
    record: &MutationRecord,
    request_sha256: &str,
) -> Result<(), ResourceError> {
    if record.request_sha256 != request_sha256 {
        return Err(ResourceError::SessionMutationConflict);
    }
    Ok(())
}

/// 稳定列出并校验全部事务记录，不接受未知目录项。
fn list_records(layout: &MutationLayout) -> Result<Vec<(PathBuf, MutationRecord)>, ResourceError> {
    let mut records = Vec::new();
    let mut temporary_files = Vec::new();
    for entry in fs::read_dir(&layout.records_root)
        .map_err(|error| ResourceError::io("list_session_mutation_records", error))?
    {
        let entry =
            entry.map_err(|error| ResourceError::io("read_session_mutation_entry", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ResourceError::io("inspect_session_mutation_entry", error))?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(ResourceError::UnsafePath(
                "Session 变更记录目录包含非普通文件".to_owned(),
            ));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            ResourceError::UnsafePath("Session 变更记录名称必须是 UTF-8".to_owned())
        })?;
        if name.starts_with(ATOMIC_TEMP_PREFIX) {
            temporary_files.push(entry.path());
            continue;
        }
        let key = name
            .strip_suffix(".json")
            .filter(|key| is_sha256(key))
            .ok_or_else(|| ResourceError::UnsafePath("Session 变更记录文件名无效".to_owned()))?;
        let path = entry.path();
        let record = read_record_if_present(&path)?.ok_or_else(|| {
            ResourceError::SessionMutationRecoveryRequired("事务记录在扫描期间消失".to_owned())
        })?;
        if operation_key(&record.source_session_id, &record.operation_id) != key {
            return Err(ResourceError::SessionMutationRecoveryRequired(
                "事务记录文件名与正文身份不一致".to_owned(),
            ));
        }
        records.push((path, record));
        if records.len() > MAX_MUTATION_RECORDS {
            return Err(ResourceError::StateCollectionLimit {
                collection: "session_mutation_records",
                actual: records.len(),
                limit: MAX_MUTATION_RECORDS,
            });
        }
    }
    if !temporary_files.is_empty() {
        for path in temporary_files {
            fs::remove_file(path).map_err(|error| {
                ResourceError::io("remove_session_mutation_atomic_temporary", error)
            })?;
        }
        sync_directory(&layout.records_root, true)?;
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(records)
}

/// 删除一个 operationId 专属且严格位于 staging 根下的构建目录。
fn cleanup_staging(layout: &MutationLayout, operation_key: &str) -> Result<(), ResourceError> {
    let candidate = layout.operation_staging_root(operation_key);
    let metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ResourceError::io("inspect_session_mutation_staging", error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ResourceError::UnsafePath(
            "Session 变更 staging 不是安全目录".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| ResourceError::io("canonicalize_session_mutation_staging", error))?;
    if canonical.parent() != Some(layout.staging_root.as_path()) {
        return Err(ResourceError::UnsafePath(
            "Session 变更 staging 越过受管根目录".to_owned(),
        ));
    }
    fs::remove_dir_all(&canonical)
        .map_err(|error| ResourceError::io("remove_session_mutation_staging", error))?;
    sync_directory(&layout.staging_root, true)
}

/// 生成源 Session 内 operationId 唯一且跨方法共享的事务键。
fn operation_key(source_session_id: &SessionId, operation_id: &str) -> String {
    sha256_parts(&[
        b"keencode/session-mutation-operation/v1",
        source_session_id.as_str().as_bytes(),
        operation_id.as_bytes(),
    ])
}

/// 为目标 Session 派生不可碰撞且可安全映射目录的稳定标识。
fn derived_session_id(
    kind: &str,
    source_session_id: &SessionId,
    operation_id: &str,
) -> Result<SessionId, ResourceError> {
    SessionId::new(format!(
        "session-{}",
        sha256_parts(&[
            b"keencode/session-mutation-target/v1",
            kind.as_bytes(),
            source_session_id.as_str().as_bytes(),
            operation_id.as_bytes(),
        ])
    ))
}

/// 计算 fork 方法和全部输入的稳定请求摘要。
fn fork_request_sha256(request: &SessionForkRequest) -> String {
    fork_request_sha256_parts(
        &request.source_session_id,
        &request.operation_id,
        request.title.as_deref(),
    )
}

/// 由已经拆分的 fork 参数计算可复核请求摘要。
fn fork_request_sha256_parts(
    source_session_id: &SessionId,
    operation_id: &str,
    title: Option<&str>,
) -> String {
    let (presence, title) = title.map_or((b"none".as_slice(), b"".as_slice()), |title| {
        (b"some".as_slice(), title.as_bytes())
    });
    sha256_parts(&[
        b"keencode/session-fork-request/v1",
        source_session_id.as_str().as_bytes(),
        operation_id.as_bytes(),
        presence,
        title,
    ])
}

/// 计算编辑方法和完整期望文本的稳定请求摘要。
fn edit_request_sha256(request: &SessionEditUserRequest) -> String {
    let expected_text_sha256 = sha256_hex(request.expected_text.as_bytes());
    edit_request_sha256_parts(
        &request.source_session_id,
        &request.operation_id,
        &request.target_message_id,
        &expected_text_sha256,
    )
}

/// 由已经脱敏的期望文本摘要计算可复核编辑请求摘要。
fn edit_request_sha256_parts(
    source_session_id: &SessionId,
    operation_id: &str,
    target_message_id: &str,
    expected_text_sha256: &str,
) -> String {
    sha256_parts(&[
        b"keencode/session-edit-user-request/v2",
        source_session_id.as_str().as_bytes(),
        operation_id.as_bytes(),
        target_message_id.as_bytes(),
        expected_text_sha256.as_bytes(),
    ])
}

/// 对记录的规范 JSON 编码计算语义摘要。
fn records_sha256(records: &[SessionEventRecord]) -> Result<String, ResourceError> {
    let bytes =
        serde_json::to_vec(records).map_err(|error| ResourceError::Json(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}

/// 返回不早于源 Session 最近事件的当前 Unix Epoch 毫秒。
fn mutation_time_ms(source_updated_at_unix_ms: u64) -> Result<u64, ResourceError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            ResourceError::SessionMutationNotApplicable("系统时间早于 Unix Epoch".to_owned())
        })?
        .as_millis();
    Ok(u64::try_from(now)
        .unwrap_or(u64::MAX)
        .max(source_updated_at_unix_ms))
}

/// 使用带长度前缀的多个字节段计算无歧义 SHA-256。
fn sha256_parts(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

/// 计算单段字节的小写 SHA-256。
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 判断文本是否是规范的小写 SHA-256。
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 测试中可注入的事务崩溃位置。
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum MutationFault {
    /// 归档已发布但源日志尚未截断。
    AfterArchivePublished,
    /// 源日志已截断但完成墓碑尚未提交。
    AfterSourceRewritten,
    /// 目标效果均完成但完成墓碑尚未提交。
    BeforeCompletion,
}

/// 返回按 operation key 隔离的一次性测试故障集合。
#[cfg(test)]
fn mutation_faults()
-> &'static std::sync::Mutex<std::collections::BTreeSet<(String, MutationFault)>> {
    static FAULTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeSet<(String, MutationFault)>>,
    > = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

/// 为一个精确事务登记一次性崩溃故障。
#[cfg(test)]
fn inject_mutation_fault(operation_key: &str, fault: MutationFault) {
    mutation_faults()
        .lock()
        .expect("Session 变更测试故障锁应可用")
        .insert((operation_key.to_owned(), fault));
}

/// 消费精确事务的测试故障并模拟进程在持久效果后退出。
#[cfg(test)]
fn fail_mutation_if(operation_key: &str, fault: MutationFault) -> Result<(), ResourceError> {
    if mutation_faults()
        .lock()
        .expect("Session 变更测试故障锁应可用")
        .remove(&(operation_key.to_owned(), fault))
    {
        return Err(ResourceError::SessionMutationRecoveryRequired(
            "测试注入 Session 变更崩溃".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::{
        MutationFault, edit_request_sha256, fork_session, inject_mutation_fault, operation_key,
        prepare_edit_user, read_all_records, recover_session_mutations, root_user_messages,
        target_root_user_sequence,
    };
    use crate::{
        AgentId, ArtifactLimits, ArtifactMaterialization, ArtifactStore, IdempotentAppendOutcome,
        JournalConfig, MessagePart, MessageRole, ResourceError, SessionEditUserRequest,
        SessionEvent, SessionEventId, SessionEventRecord, SessionForkRequest, SessionId,
        SessionJournal, SessionLease, SessionLeaseAcquire, SessionMessage, SessionOpen,
        SubAgentState, SubAgentStatus, TranscriptSegment, TurnId,
    };

    /// 在临时存储根创建默认的两个完整用户 Turn 和一个 Artifact。
    fn create_source(root: &Path, session_id: &str) {
        create_source_with_texts(root, session_id, "第一条用户消息", "第二条用户消息");
    }

    /// 在临时存储根创建可自定义用户正文的健康 Session。
    fn create_source_with_texts(
        root: &Path,
        session_id: &str,
        first_user_text: &str,
        second_user_text: &str,
    ) {
        let session_id = SessionId::new(session_id).expect("测试 SessionId 应有效");
        let lease = match SessionLease::try_acquire(root, session_id.clone())
            .expect("测试 lease 应获取")
        {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => panic!("测试 Session 不应已被占用"),
        };
        let artifacts = Arc::new(
            ArtifactStore::open(root, session_id.clone(), ArtifactLimits::default())
                .expect("测试 ArtifactStore 应打开"),
        );
        let artifact = artifacts
            .put(b"artifact-body", Some("text/plain".to_owned()))
            .expect("测试 Artifact 应写入")
            .as_event_use();
        let journal = match SessionJournal::open_with_artifact_validator(
            root,
            session_id.clone(),
            JournalConfig::default(),
            artifacts.clone(),
        )
        .expect("测试 Journal 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("新测试 Journal 不应损坏"),
        };
        append(
            &journal,
            "created",
            SessionEvent::SessionCreated {
                title: "原会话".to_owned(),
                project_root: root.to_string_lossy().into_owned(),
            },
        );
        append_root_turn_start(&journal, "turn-1", first_user_text);
        append(
            &journal,
            "assistant-1",
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: "assistant-message-1".to_owned(),
                    turn_id: Some(TurnId::new("turn-1").expect("TurnId 应有效")),
                    agent_id: Some(AgentId::new("root").expect("AgentId 应有效")),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Artifact {
                        artifact,
                        materialization: ArtifactMaterialization::Utf8Text,
                    }],
                },
            },
        );
        append(
            &journal,
            "completed-1",
            SessionEvent::TurnCompleted {
                turn_id: TurnId::new("turn-1").expect("TurnId 应有效"),
            },
        );
        append_root_turn_start(&journal, "turn-2", second_user_text);
        append(
            &journal,
            "assistant-2",
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    message_id: "assistant-message-2".to_owned(),
                    turn_id: Some(TurnId::new("turn-2").expect("TurnId 应有效")),
                    agent_id: Some(AgentId::new("root").expect("AgentId 应有效")),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Text {
                        text: "第二条回复".to_owned(),
                    }],
                },
            },
        );
        append(
            &journal,
            "completed-2",
            SessionEvent::TurnCompleted {
                turn_id: TurnId::new("turn-2").expect("TurnId 应有效"),
            },
        );
        let state = journal.state().expect("源状态应读取");
        artifacts
            .recover_for_state(&lease, &state)
            .expect("源 Artifact 应完整");
    }

    /// 原子追加一个测试事件并要求真实新提交。
    fn append(journal: &SessionJournal, event_id: &str, event: SessionEvent) {
        let expected = journal.state().expect("测试状态应读取").last_sequence;
        assert!(matches!(
            journal
                .append_idempotent(
                    SessionEventId::new(event_id).expect("EventId 应有效"),
                    expected,
                    event,
                )
                .expect("测试事件应提交"),
            IdempotentAppendOutcome::Appended(_)
        ));
    }

    /// 原子追加根 Turn 起点和对应用户消息。
    fn append_root_turn_start(journal: &SessionJournal, turn_id: &str, text: &str) {
        append_root_turn_start_with_message_id(
            journal,
            turn_id,
            &format!("user-message-{turn_id}"),
            text,
        );
    }

    /// 原子追加带指定稳定消息标识的根 Turn 起点和用户消息。
    fn append_root_turn_start_with_message_id(
        journal: &SessionJournal,
        turn_id: &str,
        message_id: &str,
        text: &str,
    ) {
        let turn_id = TurnId::new(turn_id).expect("TurnId 应有效");
        append(
            journal,
            &format!("start-{turn_id}"),
            SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                        source_agent_id: AgentId::new("root").expect("AgentId 应有效"),
                        root_turn_id: turn_id.clone(),
                        parent_turn_id: None,
                        prompt_summary: text.to_owned(),
                    },
                    SessionEvent::MessageAdded {
                        message: SessionMessage {
                            message_id: message_id.to_owned(),
                            turn_id: Some(turn_id),
                            agent_id: None,
                            role: MessageRole::User,
                            content: vec![MessagePart::Text {
                                text: text.to_owned(),
                            }],
                        },
                    },
                ],
            },
        );
    }

    /// 原子追加一个仍处于 Pending 状态的单层子 Agent。
    fn append_pending_sub_agent(journal: &SessionJournal, agent_id: &str) {
        append(
            journal,
            &format!("spawn-{agent_id}"),
            SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: AgentId::new(agent_id).expect("子 Agent 标识应有效"),
                    parent_agent_id: AgentId::new("root").expect("根 Agent 标识应有效"),
                    agent_path: format!("/root/{agent_id}"),
                    task: "等待执行的测试任务".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            },
        );
    }

    /// 重新打开现有 Session 并追加仍处于 Pending 状态的测试子 Agent。
    fn append_pending_sub_agent_to_source(root: &Path, session_id: &str, agent_id: &str) {
        let session_id = SessionId::new(session_id).expect("测试 SessionId 应有效");
        let lease = match SessionLease::try_acquire(root, session_id.clone())
            .expect("测试 lease 应获取")
        {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => panic!("测试 Session 不应已被占用"),
        };
        let artifacts = Arc::new(
            ArtifactStore::open(root, session_id.clone(), ArtifactLimits::default())
                .expect("测试 ArtifactStore 应打开"),
        );
        let journal = match SessionJournal::open_with_artifact_validator(
            root,
            session_id,
            JournalConfig::default(),
            artifacts,
        )
        .expect("测试 Journal 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("测试 Journal 不应损坏"),
        };
        append_pending_sub_agent(&journal, agent_id);
        drop(lease);
    }

    /// 打开目标 Session 并返回状态、全部记录和 ArtifactStore。
    fn load_session(
        root: &Path,
        session_id: &SessionId,
    ) -> (
        crate::SessionState,
        Vec<crate::SessionEventRecord>,
        Arc<ArtifactStore>,
    ) {
        let lease = match SessionLease::try_acquire(root, session_id.clone())
            .expect("目标 lease 应获取")
        {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => panic!("目标 Session 不应繁忙"),
        };
        let artifacts = Arc::new(
            ArtifactStore::open(root, session_id.clone(), ArtifactLimits::default())
                .expect("目标 ArtifactStore 应打开"),
        );
        let journal = match SessionJournal::open_with_artifact_validator(
            root,
            session_id.clone(),
            JournalConfig::default(),
            artifacts.clone(),
        )
        .expect("目标 Journal 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("目标 Journal 不应损坏"),
        };
        let state = journal.state().expect("目标状态应读取");
        artifacts
            .recover_for_state(&lease, &state)
            .expect("目标 Artifact 应完整");
        let records = read_all_records(&journal).expect("目标记录应读取");
        (state, records, artifacts)
    }

    /// 构造只用于纯定位测试的根 Turn 起点事件。
    fn root_turn_started_event(turn_id: &str, prompt_summary: &str) -> SessionEvent {
        let turn_id = TurnId::new(turn_id).expect("测试 TurnId 应有效");
        SessionEvent::TurnStarted {
            turn_id: turn_id.clone(),
            source_agent_id: AgentId::new("root").expect("根 AgentId 应有效"),
            root_turn_id: turn_id,
            parent_turn_id: None,
            prompt_summary: prompt_summary.to_owned(),
        }
    }

    /// 构造只用于纯定位测试的文本用户消息。
    fn user_message(message_id: &str, turn_id: &str, text: &str) -> SessionMessage {
        SessionMessage {
            message_id: message_id.to_owned(),
            turn_id: Some(TurnId::new(turn_id).expect("测试 TurnId 应有效")),
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: text.to_owned(),
            }],
        }
    }

    /// 验证指定回退边界在故障恢复后保留完整归档、精确源前缀并保持重试幂等。
    fn assert_recovered_edit_case(
        fault: MutationFault,
        source_session_name: &str,
        operation_id: &str,
        target_message_id: &str,
        expected_source_record_sequences: &[u64],
    ) {
        let root = tempdir().expect("临时目录应创建");
        create_source_with_texts(
            root.path(),
            source_session_name,
            "重复用户消息",
            "重复用户消息",
        );
        let source_id = SessionId::new(source_session_name).expect("SessionId 应有效");
        let operation_key = operation_key(&source_id, operation_id);
        inject_mutation_fault(&operation_key, fault);
        let request = SessionEditUserRequest {
            source_session_id: source_id.clone(),
            target_message_id: target_message_id.to_owned(),
            expected_text: "重复用户消息".to_owned(),
            operation_id: operation_id.to_owned(),
        };
        assert!(
            prepare_edit_user(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
                request.clone(),
            )
            .is_err(),
            "注入故障后首次编辑应中断"
        );

        assert_eq!(
            recover_session_mutations(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
            )
            .expect("启动恢复应完成编辑事务"),
            1
        );

        let (source, source_records, _) = load_session(root.path(), &source_id);
        assert_eq!(
            source_records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            expected_source_record_sequences,
            "源日志必须只保留目标前的物理记录前缀"
        );
        let expected_source_transcript_messages = match expected_source_record_sequences.len() {
            1 => 0,
            4 => 2,
            actual => panic!("测试只支持 turn-1/turn-2，实际源记录数为 {actual}"),
        };
        assert_eq!(
            source.raw_transcript_messages().len(),
            expected_source_transcript_messages,
            "源 Transcript 必须与物理前缀一致"
        );
        let source_message_ids = source
            .raw_transcript_messages()
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>();
        let expected_source_message_ids = match expected_source_record_sequences.len() {
            1 => Vec::new(),
            4 => vec!["user-message-turn-1", "assistant-message-1"],
            actual => panic!("测试只支持 turn-1/turn-2，实际源记录数为 {actual}"),
        };
        assert_eq!(
            source_message_ids, expected_source_message_ids,
            "相同正文的消息必须按 target_message_id 截断"
        );

        let archive_id = super::derived_session_id("edit-archive", &source_id, operation_id)
            .expect("归档 SessionId 应有效");
        let (archive, archive_records, _) = load_session(root.path(), &archive_id);
        assert_eq!(archive.title, "原会话 · 编辑前版本");
        assert_eq!(
            archive_records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>(),
            "归档必须保留原始 7 条记录并追加标题记录"
        );
        assert_eq!(
            archive.raw_transcript_messages().len(),
            4,
            "归档必须保留完整的 4 条 Transcript 消息"
        );
        assert_eq!(
            archive
                .raw_transcript_messages()
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "user-message-turn-1",
                "assistant-message-1",
                "user-message-turn-2",
                "assistant-message-2",
            ],
            "归档必须保留两个相同正文但不同标识的用户消息"
        );

        let first_retry = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .expect("恢复后相同 operationId 应返回归档结果");
        let source_records_before_repeat = load_session(root.path(), &source_id).1;
        let archive_records_before_repeat = load_session(root.path(), &archive_id).1;
        let sessions_before_repeat =
            crate::list_session_ids(root.path()).expect("重复前 Session 列表应可读取");
        let repeated = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect("相同 operationId 重试应幂等");
        assert_eq!(first_retry, repeated, "重试必须返回同一归档结果");
        assert_eq!(
            load_session(root.path(), &source_id).1,
            source_records_before_repeat,
            "重复 operationId 不得再次改写源日志"
        );
        assert_eq!(
            load_session(root.path(), &archive_id).1,
            archive_records_before_repeat,
            "重复 operationId 不得再次创建或改写归档"
        );
        assert_eq!(
            crate::list_session_ids(root.path()).expect("重复后 Session 列表应可读取"),
            sessions_before_repeat,
            "重复 operationId 不得产生额外 Session"
        );
        assert_eq!(
            recover_session_mutations(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
            )
            .expect("重复恢复应幂等"),
            0
        );
    }

    /// fork 必须复制完整历史和 Artifact，并按 operationId 幂等返回同一目标。
    #[test]
    fn fork_copies_history_artifacts_and_is_idempotent() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-fork");
        let request = SessionForkRequest {
            source_session_id: SessionId::new("session-source-fork").expect("SessionId 应有效"),
            operation_id: "fork-operation".to_owned(),
            title: Some("分支会话".to_owned()),
        };
        let first = fork_session(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .expect("首次 fork 应成功");
        let repeated = fork_session(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect("重复 fork 应复用结果");
        assert_eq!(first, repeated);
        let (state, records, artifacts) = load_session(root.path(), &first.session_id);
        assert_eq!(state.title, "分支会话");
        assert_eq!(state.raw_transcript_messages().len(), 4);
        assert_eq!(records.len(), 8);
        let artifact = match &state.raw_transcript_messages()[1].content[0] {
            MessagePart::Artifact { artifact, .. } => artifact,
            _ => panic!("复制历史应保留 Artifact 引用"),
        };
        assert_eq!(
            artifacts.read_use(artifact).expect("复制 Artifact 应可读"),
            b"artifact-body"
        );
    }

    /// 同一 operationId 不能用不同标题重新绑定 fork 请求。
    #[test]
    fn fork_rejects_operation_id_reuse_with_different_request() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-conflict");
        let source_session_id =
            SessionId::new("session-source-conflict").expect("SessionId 应有效");
        fork_session(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionForkRequest {
                source_session_id: source_session_id.clone(),
                operation_id: "same-operation".to_owned(),
                title: Some("标题一".to_owned()),
            },
        )
        .expect("首次 fork 应成功");
        let error = fork_session(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionForkRequest {
                source_session_id,
                operation_id: "same-operation".to_owned(),
                title: Some("标题二".to_owned()),
            },
        )
        .expect_err("相同 operationId 的不同正文应失败");
        assert!(matches!(error, ResourceError::SessionMutationConflict));
    }

    /// 编辑前事务必须完整归档，并把源日志截到指定用户 Turn 之前。
    #[test]
    fn edit_archives_full_history_and_truncates_source() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-edit");
        let request = SessionEditUserRequest {
            source_session_id: SessionId::new("session-source-edit").expect("SessionId 应有效"),
            target_message_id: "user-message-turn-2".to_owned(),
            expected_text: "第二条用户消息".to_owned(),
            operation_id: "edit-operation".to_owned(),
        };
        let first = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .expect("编辑前事务应成功");
        let repeated = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect("编辑前事务重试应幂等");
        assert_eq!(first, repeated);
        let (source, source_records, _) = load_session(
            root.path(),
            &SessionId::new("session-source-edit").expect("SessionId 应有效"),
        );
        assert_eq!(source.raw_transcript_messages().len(), 2);
        assert_eq!(source_records.len(), 4);
        let (archive, archive_records, _) = load_session(root.path(), &first.archived_session_id);
        assert_eq!(archive.raw_transcript_messages().len(), 4);
        assert_eq!(archive_records.len(), 8);
        assert_eq!(archive.title, "原会话 · 编辑前版本");
    }

    /// 指定更早用户消息时只保留其前面的物理记录，归档仍保留完整历史。
    #[test]
    fn edit_can_target_an_earlier_user_message_without_touching_project_files() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-early-edit");
        let project_file = root.path().join("project-file.txt");
        fs::write(&project_file, b"outside-session-state").expect("项目文件应写入");
        let source_id = SessionId::new("session-source-early-edit").expect("SessionId 应有效");
        let result = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionEditUserRequest {
                source_session_id: source_id.clone(),
                target_message_id: "user-message-turn-1".to_owned(),
                expected_text: "第一条用户消息".to_owned(),
                operation_id: "early-edit-operation".to_owned(),
            },
        )
        .expect("更早用户消息应可回退");

        let (source, source_records, _) = load_session(root.path(), &source_id);
        assert_eq!(source.raw_transcript_messages().len(), 0);
        assert_eq!(source_records.len(), 1, "只能保留 SessionCreated 物理记录");
        let (archive, archive_records, _) = load_session(root.path(), &result.archived_session_id);
        assert_eq!(archive.raw_transcript_messages().len(), 4);
        assert_eq!(archive_records.len(), 8, "归档必须保留完整原历史");
        assert_eq!(
            fs::read(&project_file).expect("项目文件应仍可读"),
            b"outside-session-state"
        );
    }

    /// 相同正文的不同用户消息必须按 target_message_id 选择，而不是按正文或末条选择。
    #[test]
    fn edit_binds_duplicate_text_to_the_requested_message_id() {
        let root = tempdir().expect("临时目录应创建");
        create_source_with_texts(
            root.path(),
            "session-source-duplicate-text",
            "重复用户消息",
            "重复用户消息",
        );
        let source_id = SessionId::new("session-source-duplicate-text").expect("SessionId 应有效");
        let result = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionEditUserRequest {
                source_session_id: source_id.clone(),
                target_message_id: "user-message-turn-1".to_owned(),
                expected_text: "重复用户消息".to_owned(),
                operation_id: "duplicate-text-operation".to_owned(),
            },
        )
        .expect("按早期消息标识回退应成功");

        let (_, source_records, _) = load_session(root.path(), &source_id);
        assert_eq!(
            source_records.len(),
            1,
            "必须命中第一条而不是正文相同的末条"
        );
        let (_, archive_records, _) = load_session(root.path(), &result.archived_session_id);
        assert_eq!(archive_records.len(), 8);
    }

    /// 相同 operationId 的重试幂等，但改绑另一目标消息必须冲突且不改变源日志。
    #[test]
    fn edit_rejects_operation_id_rebinding_to_another_target() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-edit-conflict");
        let source_id = SessionId::new("session-source-edit-conflict").expect("SessionId 应有效");
        let request = SessionEditUserRequest {
            source_session_id: source_id.clone(),
            target_message_id: "user-message-turn-2".to_owned(),
            expected_text: "第二条用户消息".to_owned(),
            operation_id: "edit-rebinding-operation".to_owned(),
        };
        let first = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request.clone(),
        )
        .expect("首次编辑事务应成功");
        let repeated = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect("相同请求应幂等");
        assert_eq!(first, repeated);
        let (_, before, _) = load_session(root.path(), &source_id);
        let error = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionEditUserRequest {
                source_session_id: source_id.clone(),
                target_message_id: "user-message-turn-1".to_owned(),
                expected_text: "第一条用户消息".to_owned(),
                operation_id: "edit-rebinding-operation".to_owned(),
            },
        )
        .expect_err("同 operationId 改绑目标必须冲突");
        assert!(matches!(error, ResourceError::SessionMutationConflict));
        let (_, after, _) = load_session(root.path(), &source_id);
        assert_eq!(before, after, "冲突不得修改源 Journal");
    }

    /// 任意未完成子 Agent 都必须在归档前被拒绝，且不得留下事务副作用。
    #[test]
    fn edit_rejects_pending_sub_agent_without_side_effects() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-pending-agent");
        append_pending_sub_agent_to_source(
            root.path(),
            "session-source-pending-agent",
            "pending_child",
        );
        let source_id = SessionId::new("session-source-pending-agent").expect("SessionId 应有效");
        let (_, before, _) = load_session(root.path(), &source_id);
        let error = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            SessionEditUserRequest {
                source_session_id: source_id.clone(),
                target_message_id: "user-message-turn-2".to_owned(),
                expected_text: "第二条用户消息".to_owned(),
                operation_id: "pending-agent-operation".to_owned(),
            },
        )
        .expect_err("Pending 子 Agent 应阻止回退");
        assert!(matches!(
            error,
            ResourceError::SessionMutationNotApplicable(_)
        ));
        let (_, after, _) = load_session(root.path(), &source_id);
        assert_eq!(before, after, "活动子 Agent 拒绝不得改写源 Journal");
        let records_root = root.path().join("session-mutations").join("records");
        assert!(
            fs::read_dir(records_root)
                .expect("事务目录应存在")
                .next()
                .is_none(),
            "活动子 Agent 拒绝不得留下事务记录"
        );
    }

    /// 期望文本过期时不得创建归档或改写源日志。
    #[test]
    fn edit_rejects_stale_expected_text_without_side_effects() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-stale");
        let source_id = SessionId::new("session-source-stale").expect("SessionId 应有效");
        let (_, before, _) = load_session(root.path(), &source_id);
        let request = SessionEditUserRequest {
            source_session_id: source_id.clone(),
            target_message_id: "user-message-turn-2".to_owned(),
            expected_text: "旧文本".to_owned(),
            operation_id: "stale-operation".to_owned(),
        };
        let request_digest = edit_request_sha256(&request);
        let error = prepare_edit_user(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect_err("过期文本应失败");
        assert!(matches!(
            error,
            ResourceError::SessionMutationNotApplicable(_)
        ));
        let (_, after, _) = load_session(root.path(), &source_id);
        assert_eq!(before, after);
        let records_root = root.path().join("session-mutations").join("records");
        assert!(
            fs::read_dir(records_root)
                .expect("事务目录应存在")
                .next()
                .is_none(),
            "失败前不得留下事务记录 {request_digest}"
        );
    }

    /// 归档发布后的崩溃必须由下一次启动完成源日志截断。
    #[test]
    fn recovery_finishes_edit_after_archive_publish() {
        assert_recovered_edit_case(
            MutationFault::AfterArchivePublished,
            "session-source-recovery-archive-turn-1",
            "recover-after-archive-turn-1",
            "user-message-turn-1",
            &[1],
        );
        assert_recovered_edit_case(
            MutationFault::AfterArchivePublished,
            "session-source-recovery-archive-turn-2",
            "recover-after-archive-turn-2",
            "user-message-turn-2",
            &[1, 2, 3, 4],
        );
    }

    /// 源日志提交后的崩溃必须只补写完成墓碑，不重复截断或创建分支。
    #[test]
    fn recovery_reconciles_edit_after_source_rewrite() {
        assert_recovered_edit_case(
            MutationFault::AfterSourceRewritten,
            "session-source-recovery-source-turn-1",
            "recover-after-source-turn-1",
            "user-message-turn-1",
            &[1],
        );
        assert_recovered_edit_case(
            MutationFault::AfterSourceRewritten,
            "session-source-recovery-source-turn-2",
            "recover-after-source-turn-2",
            "user-message-turn-2",
            &[1, 2, 3, 4],
        );
    }

    /// Transcript 动态输入中的用户消息不能伪装成根 Turn 起点消息。
    #[test]
    fn root_user_messages_ignore_dynamic_transcript_segment_user_messages() {
        let turn_id = TurnId::new("turn-dynamic-input").expect("测试 TurnId 应有效");
        let event = SessionEvent::AtomicBatch {
            events: vec![
                root_turn_started_event(turn_id.as_str(), "根用户消息"),
                SessionEvent::TranscriptSegmentCommitted {
                    segment: TranscriptSegment {
                        turn_id,
                        source_agent_id: AgentId::new("root").expect("根 AgentId 应有效"),
                        model_round: 0,
                        segment_index: 0,
                        expected_transcript_revision: 0,
                        messages: vec![user_message(
                            "dynamic-user-message",
                            "turn-dynamic-input",
                            "动态追加用户消息",
                        )],
                    },
                },
            ],
        };

        assert!(
            root_user_messages(&event).is_empty(),
            "TranscriptSegmentCommitted 中的动态用户消息不是可编辑根消息"
        );
    }

    /// 非 root Turn 起点及其用户消息不能被识别为根用户消息。
    #[test]
    fn root_user_messages_ignore_non_root_turn_started_event() {
        let child_turn_id = TurnId::new("turn-child").expect("测试 TurnId 应有效");
        let event = SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: child_turn_id.clone(),
                    source_agent_id: AgentId::new("child").expect("子 AgentId 应有效"),
                    root_turn_id: child_turn_id,
                    parent_turn_id: None,
                    prompt_summary: "子 Agent 输入".to_owned(),
                },
                SessionEvent::MessageAdded {
                    message: user_message("child-user-message", "turn-child", "子 Agent 输入"),
                },
            ],
        };

        assert!(
            root_user_messages(&event).is_empty(),
            "非 root TurnStarted 不得产生可编辑根用户消息"
        );
    }

    /// 同一 AtomicBatch 中第二条真实用户消息不能作为安全截断边界。
    #[test]
    fn target_root_user_sequence_rejects_second_user_message_in_atomic_batch() {
        let root = tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("session-second-user-in-batch").expect("SessionId 应有效");
        let event = SessionEvent::AtomicBatch {
            events: vec![
                root_turn_started_event("turn-two-users", "首条用户消息"),
                SessionEvent::MessageAdded {
                    message: user_message("batch-user-first", "turn-two-users", "相同正文"),
                },
                SessionEvent::MessageAdded {
                    message: user_message("batch-user-second", "turn-two-users", "相同正文"),
                },
            ],
        };
        assert_eq!(
            root_user_messages(&event)
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["batch-user-first", "batch-user-second"]
        );
        let artifacts =
            ArtifactStore::open(root.path(), session_id.clone(), ArtifactLimits::default())
                .expect("测试 ArtifactStore 应打开");
        let records = vec![SessionEventRecord::new(
            SessionEventId::new("atomic-batch").expect("EventId 应有效"),
            session_id,
            2,
            1,
            event,
        )];
        let error =
            target_root_user_sequence(&records, &artifacts, "batch-user-second", "相同正文")
                .expect_err("同一 AtomicBatch 的第二条用户消息必须被拒绝");
        assert!(matches!(
            error,
            ResourceError::SessionMutationNotApplicable(message)
                if message == "目标用户消息不是原子批次中的第一条，不能安全截断"
        ));
    }

    /// 分支已经发布但完成墓碑未写入时，启动恢复必须只对账而不重复创建。
    #[test]
    fn recovery_reconciles_fork_before_completion_marker() {
        let root = tempdir().expect("临时目录应创建");
        create_source(root.path(), "session-source-recovery-fork");
        let source_id = SessionId::new("session-source-recovery-fork").expect("SessionId 应有效");
        let operation_id = "recover-fork-completion";
        inject_mutation_fault(
            &operation_key(&source_id, operation_id),
            MutationFault::BeforeCompletion,
        );
        let request = SessionForkRequest {
            source_session_id: source_id,
            operation_id: operation_id.to_owned(),
            title: Some("恢复分支".to_owned()),
        };
        assert!(
            fork_session(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
                request.clone(),
            )
            .is_err()
        );
        assert_eq!(
            recover_session_mutations(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
            )
            .expect("启动恢复应完成 fork 对账"),
            1
        );
        let result = fork_session(
            root.path(),
            JournalConfig::default(),
            ArtifactLimits::default(),
            request,
        )
        .expect("恢复后原请求应返回同一分支");
        let (state, records, _) = load_session(root.path(), &result.session_id);
        assert_eq!(state.title, "恢复分支");
        assert_eq!(state.raw_transcript_messages().len(), 4);
        assert_eq!(records.len(), 8);
    }

    /// 启动恢复必须清理事务记录原子替换在进程崩溃后遗留的可信临时文件。
    #[test]
    fn recovery_removes_abandoned_atomic_record_temporary() {
        let root = tempdir().expect("临时目录应创建");
        let records_root = root.path().join("session-mutations").join("records");
        fs::create_dir_all(&records_root).expect("事务记录目录应创建");
        let temporary = records_root.join(format!(
            "{}crash-leftover",
            crate::atomic::ATOMIC_TEMP_PREFIX
        ));
        fs::write(&temporary, b"partial-record").expect("崩溃临时文件应写入");

        assert_eq!(
            recover_session_mutations(
                root.path(),
                JournalConfig::default(),
                ArtifactLimits::default(),
            )
            .expect("启动恢复应清理原子临时文件"),
            0
        );
        assert!(!temporary.exists());
    }
}
