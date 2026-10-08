//! NativeHost 的 workspace-only 文件撤销事务。
//!
//! 文件撤销是文件副作用，不能由 GPUI 临时投影或旧 RPC 状态推断。Prepared、
//! Completed、Indeterminate 事务记录是进程重启后唯一允许使用的副作用事实。

use crate::{
    agent_runtime::AgentRuntime,
    native_paths::NativePaths,
    session_commands::{close_session_for_mutation, restore_session_after_mutation},
    storage,
};
use keencode_resources::{FileSnapshot, MessagePart, SessionState, TurnId};
use keencode_runtime::RuntimeSession;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) const REWIND_RECORD_SCHEMA: &str = "keencode/native-file-rewind";
pub(crate) const REWIND_RECORD_VERSION: u32 = 2;
pub(crate) const REWIND_RECORD_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewindReceipt {
    pub session_id: String,
    pub operation_id: String,
    pub turn_id: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeRewindError {
    Invalid(String),
    TargetNotFound,
    /// 目标 Turn 已有其他 operationId 完成撤销，禁止重复写入。
    AlreadyCompleted,
    WorkspaceStale(String),
    Indeterminate(String),
    Backend(String),
}

impl NativeRewindError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "native-rewind-invalid",
            Self::TargetNotFound => "native-rewind-target-not-found",
            Self::AlreadyCompleted => "native-rewind-already-completed",
            Self::WorkspaceStale(_) => "native-rewind-workspace-stale",
            Self::Indeterminate(_) => "native-rewind-indeterminate",
            Self::Backend(_) => "native-rewind-failed",
        }
    }
}

impl fmt::Display for NativeRewindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::WorkspaceStale(message) | Self::Backend(message) => {
                f.write_str(message)
            }
            Self::TargetNotFound => f.write_str("文件撤销目标行不属于当前会话"),
            Self::AlreadyCompleted => f.write_str("目标 Turn 已完成文件撤销"),
            Self::Indeterminate(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NativeRewindError {}

#[derive(Clone, Debug)]
struct RewindPlan {
    path: PathBuf,
    expected_hash: String,
    before: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RewindPhase {
    Prepared,
    Completed,
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RewindRecord {
    pub(crate) schema: String,
    pub(crate) version: u32,
    pub(crate) session_id: String,
    pub(crate) operation_id: String,
    pub(crate) project_root: String,
    pub(crate) turn_id: String,
    pub(crate) phase: RewindPhase,
    pub(crate) paths: Vec<String>,
}

/// 按显式 target Turn 和工作区快照校验执行 workspace-only 文件撤销。
pub async fn apply(
    runtime: Arc<AgentRuntime>,
    paths: &NativePaths,
    session_id: &str,
    target_turn_id: &str,
    operation_id: &str,
) -> Result<RewindReceipt, NativeRewindError> {
    validate_identifier(session_id, "sessionId")?;
    validate_identifier(target_turn_id, "targetTurnId")?;
    validate_identifier(operation_id, "operationId")?;
    let project_root =
        crate::session_commands::authorize_stored_session_root(&runtime, paths, session_id)
            .map_err(NativeRewindError::Backend)?;
    let project_root_text = project_root.to_string_lossy().into_owned();
    let record_path = rewind_record_path(runtime.storage_root(), session_id, operation_id);

    if let Some(record) = read_rewind_record(&record_path)? {
        validate_record(
            &record,
            session_id,
            operation_id,
            &project_root_text,
            target_turn_id,
        )?;
        match record.phase {
            RewindPhase::Prepared | RewindPhase::Indeterminate => return Err(indeterminate_error()),
            RewindPhase::Completed => {
                return Ok(RewindReceipt {
                    session_id: session_id.to_owned(),
                    operation_id: operation_id.to_owned(),
                    turn_id: record.turn_id,
                    paths: record.paths,
                });
            }
        }
    }

    // 先检查已完成事务，避免撤销后的工作区 hash 不再匹配时把重复操作误报为 stale。
    if completed_rewind_turn_ids(runtime.storage_root(), session_id)
        .map_err(NativeRewindError::Backend)?
        .contains(target_turn_id)
    {
        return Err(NativeRewindError::AlreadyCompleted);
    }

    let session = crate::session_commands::open_authorized_session(&runtime, paths, session_id)
        .map_err(NativeRewindError::Backend)?;
    let (turn_id, plans) = session
        .read_state(|state| {
            let turn_id = validate_target(state, target_turn_id)?;
            let plans = build_rewind_plans(&session, state, turn_id.as_str(), &project_root)?;
            Ok((turn_id, plans))
        })
        .map_err(|error| NativeRewindError::Backend(error.to_string()))??;
    if plans.is_empty() {
        return Err(NativeRewindError::Backend(
            "目标 Turn 没有可用的已应用文件快照".to_owned(),
        ));
    }
    // close_session_for_mutation 会先关闭 Manager 中的句柄，再重新取得 lease；
    // 读取阶段的临时 Session 必须先释放，否则恢复路径会稳定撞上 SessionBusy。
    drop(session);

    let context = close_session_for_mutation(&runtime, paths, session_id)
        .await
        .map_err(NativeRewindError::Backend)?;
    // 取得 mutation gate 后再复查，覆盖不同 operationId 并发进入读取阶段的竞态。
    let completed_turns = match completed_rewind_turn_ids(runtime.storage_root(), session_id) {
        Ok(turns) => turns,
        Err(error) => {
            return Err(with_restore_error(
                &runtime,
                session_id,
                &context,
                NativeRewindError::Backend(error),
            ));
        }
    };
    if completed_turns.contains(target_turn_id) {
        return Err(with_restore_error(
            &runtime,
            session_id,
            &context,
            NativeRewindError::AlreadyCompleted,
        ));
    }
    let mut record = RewindRecord {
        schema: REWIND_RECORD_SCHEMA.to_owned(),
        version: REWIND_RECORD_VERSION,
        session_id: session_id.to_owned(),
        operation_id: operation_id.to_owned(),
        project_root: project_root_text,
        turn_id: turn_id.as_str().to_owned(),
        phase: RewindPhase::Prepared,
        paths: plans
            .iter()
            .map(|plan| plan.path.to_string_lossy().into_owned())
            .collect(),
    };
    if let Err(error) = write_rewind_record(&record_path, &record) {
        return Err(with_restore_error(&runtime, session_id, &context, error));
    }
    if let Err(error) = apply_rewind_plans(&project_root, &plans) {
        record.phase = RewindPhase::Indeterminate;
        let _ = write_rewind_record(&record_path, &record);
        let error = NativeRewindError::Indeterminate(format!(
            "文件撤销结果不确定，禁止自动重放，请人工核对工作区：{error}"
        ));
        return Err(with_restore_error(&runtime, session_id, &context, error));
    }
    record.phase = RewindPhase::Completed;
    if let Err(error) = write_rewind_record(&record_path, &record) {
        record.phase = RewindPhase::Indeterminate;
        let _ = write_rewind_record(&record_path, &record);
        let error = NativeRewindError::Indeterminate(format!(
            "文件已写入，但完成事务记录失败，禁止自动重放：{error}"
        ));
        return Err(with_restore_error(&runtime, session_id, &context, error));
    }
    if let Err(error) = restore_session_after_mutation(&runtime, session_id, &context) {
        return Err(NativeRewindError::Backend(format!(
            "文件撤销已完成，但 Session 恢复失败：{error}"
        )));
    }
    Ok(RewindReceipt {
        session_id: session_id.to_owned(),
        operation_id: operation_id.to_owned(),
        turn_id: turn_id.as_str().to_owned(),
        paths: record.paths,
    })
}

fn with_restore_error(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    context: &crate::session_commands::ClosedSessionMutationContext,
    error: NativeRewindError,
) -> NativeRewindError {
    match restore_session_after_mutation(runtime, session_id, context) {
        Ok(()) => error,
        Err(restore_error) => NativeRewindError::Indeterminate(format!(
            "{error}；且 Session 恢复失败：{restore_error}"
        )),
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), NativeRewindError> {
    if value.is_empty() || value.trim() != value {
        return Err(NativeRewindError::Invalid(format!(
            "{field} 不能为空或包含首尾空白"
        )));
    }
    if value.len() > 128 || value.chars().any(char::is_control) {
        return Err(NativeRewindError::Invalid(format!(
            "{field} 超出长度限制或包含控制字符"
        )));
    }
    Ok(())
}

fn validate_target(
    state: &SessionState,
    target_turn_id: &str,
) -> Result<TurnId, NativeRewindError> {
    let target = TurnId::new(target_turn_id.to_owned())
        .map_err(|error| NativeRewindError::Invalid(format!("targetTurnId 无效：{error}")))?;
    if !state.turns.contains_key(&target) {
        return Err(NativeRewindError::TargetNotFound);
    }
    Ok(target)
}

#[derive(Clone)]
struct ChangeEntry {
    path: String,
    before: Option<FileSnapshot>,
    after: FileSnapshot,
    transcript_order: Option<usize>,
    requested_at: u64,
    request_id: String,
}

fn build_rewind_plans(
    session: &RuntimeSession,
    state: &SessionState,
    turn_id: &str,
    root: &Path,
) -> Result<Vec<RewindPlan>, NativeRewindError> {
    let mut transcript_order = HashMap::new();
    let mut order = 0usize;
    for message in state.raw_transcript_messages() {
        for part in &message.content {
            let call_id = match part {
                MessagePart::ToolCall { tool_call_id, .. }
                | MessagePart::ToolResult { tool_call_id, .. } => tool_call_id,
                _ => continue,
            };
            transcript_order.entry(call_id.as_str()).or_insert(order);
            order = order.saturating_add(1);
        }
    }

    let mut grouped = BTreeMap::<String, Vec<ChangeEntry>>::new();
    for lifecycle in state.tools.values() {
        if lifecycle.request.turn_id.as_str() != turn_id {
            continue;
        }
        let Some(change) = lifecycle.file_change.as_ref() else {
            continue;
        };
        if !change.applied {
            continue;
        }
        grouped
            .entry(change.path.clone())
            .or_default()
            .push(ChangeEntry {
                path: change.path.clone(),
                before: change.before.clone(),
                after: change.after.clone(),
                transcript_order: transcript_order
                    .get(lifecycle.request.model_tool_call_id.as_str())
                    .copied(),
                requested_at: lifecycle.requested_at_unix_ms,
                request_id: lifecycle.request.request_id.as_str().to_owned(),
            });
    }

    let mut plans = Vec::new();
    for entries in grouped.values_mut() {
        entries.sort_by(|left, right| {
            left.transcript_order
                .unwrap_or(usize::MAX)
                .cmp(&right.transcript_order.unwrap_or(usize::MAX))
                .then_with(|| left.requested_at.cmp(&right.requested_at))
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        let first = entries
            .first()
            .ok_or_else(|| NativeRewindError::Backend("文件变更记录为空".to_owned()))?;
        let last = entries
            .last()
            .ok_or_else(|| NativeRewindError::Backend("文件变更记录为空".to_owned()))?;
        let path = resolve_workspace_path(root, Path::new(&first.path))?;
        let current = read_current_file(&path)?.ok_or_else(|| {
            NativeRewindError::WorkspaceStale("工作区文件在撤销前消失，拒绝继续".to_owned())
        })?;
        if sha256_hex(&current) != last.after.sha256 {
            return Err(NativeRewindError::WorkspaceStale(
                "工作区文件在撤销前被外部修改".to_owned(),
            ));
        }
        let before = first
            .before
            .as_ref()
            .map(|snapshot| {
                session
                    .read_file_snapshot(snapshot)
                    .map_err(|error| NativeRewindError::Backend(error.to_string()))
            })
            .transpose()?;
        plans.push(RewindPlan {
            path,
            expected_hash: last.after.sha256.clone(),
            before,
        });
    }
    Ok(plans)
}

fn resolve_workspace_path(root: &Path, path: &Path) -> Result<PathBuf, NativeRewindError> {
    if !path.is_absolute() {
        return Err(NativeRewindError::Invalid(
            "文件变更路径必须为绝对路径".to_owned(),
        ));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(NativeRewindError::Backend(format!(
                "检查文件变更路径失败：{error}"
            )));
        }
    };
    let candidate = if let Some(metadata) = metadata {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(NativeRewindError::Backend(
                "撤销目标不是普通文件".to_owned(),
            ));
        }
        fs::canonicalize(path)
            .map_err(|error| NativeRewindError::Backend(format!("无法访问文件变更路径：{error}")))?
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| NativeRewindError::Backend("文件变更路径缺少父目录".to_owned()))?;
        let parent = fs::canonicalize(parent).map_err(|error| {
            NativeRewindError::Backend(format!("无法访问文件变更父目录：{error}"))
        })?;
        parent.join(
            path.file_name()
                .ok_or_else(|| NativeRewindError::Backend("文件变更文件名为空".to_owned()))?,
        )
    };
    if !candidate.starts_with(root) {
        return Err(NativeRewindError::Invalid(
            "文件变更路径越过 Session 工作区".to_owned(),
        ));
    }
    Ok(candidate)
}

fn read_current_file(path: &Path) -> Result<Option<Vec<u8>>, NativeRewindError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(NativeRewindError::Backend(format!(
                "检查撤销目标失败：{error}"
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(NativeRewindError::Backend(
            "撤销目标不是普通文件".to_owned(),
        ));
    }
    let file = storage::open_readonly_regular_file(path)
        .map_err(|error| NativeRewindError::Backend(format!("读取撤销目标失败：{error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| NativeRewindError::Backend(format!("读取撤销目标元数据失败：{error}")))?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err(NativeRewindError::Backend(
            "撤销目标在打开期间发生变化".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(metadata.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| NativeRewindError::Backend(format!("读取撤销目标失败：{error}")))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(NativeRewindError::Backend(
            "撤销目标在读取期间发生变化".to_owned(),
        ));
    }
    let final_metadata = fs::symlink_metadata(path)
        .map_err(|error| NativeRewindError::Backend(format!("复核撤销目标失败：{error}")))?;
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
    {
        return Err(NativeRewindError::Backend(
            "撤销目标在读取期间发生变化".to_owned(),
        ));
    }
    Ok(Some(bytes))
}

fn apply_rewind_plans(root: &Path, plans: &[RewindPlan]) -> Result<(), NativeRewindError> {
    let root = fs::canonicalize(root)
        .map_err(|error| NativeRewindError::Backend(format!("无法访问 Session 工作区：{error}")))?;
    for plan in plans {
        let current = read_current_file(&plan.path)?.ok_or_else(|| {
            NativeRewindError::WorkspaceStale("工作区文件在写入前消失".to_owned())
        })?;
        if sha256_hex(&current) != plan.expected_hash {
            return Err(NativeRewindError::WorkspaceStale(
                "工作区文件在写入前被外部修改".to_owned(),
            ));
        }
        let canonical = resolve_workspace_path(&root, &plan.path)?;
        if let Some(before) = plan.before.as_deref() {
            atomic_write_workspace_file(&canonical, before)?;
        } else {
            let metadata = fs::symlink_metadata(&canonical).map_err(|error| {
                NativeRewindError::Backend(format!("检查删除目标失败：{error}"))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(NativeRewindError::Backend(
                    "撤销目标不是普通文件".to_owned(),
                ));
            }
            fs::remove_file(&canonical).map_err(|error| {
                NativeRewindError::Backend(format!("删除撤销目标失败：{error}"))
            })?;
        }
    }
    Ok(())
}

fn atomic_write_workspace_file(path: &Path, bytes: &[u8]) -> Result<(), NativeRewindError> {
    let parent = path
        .parent()
        .ok_or_else(|| NativeRewindError::Backend("撤销目标缺少父目录".to_owned()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| NativeRewindError::Backend(format!("检查撤销目标权限失败：{error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(NativeRewindError::Backend(
            "撤销目标不是普通文件".to_owned(),
        ));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| NativeRewindError::Backend(format!("创建撤销临时文件失败：{error}")))?;
    temporary
        .write_all(bytes)
        .map_err(|error| NativeRewindError::Backend(format!("写入撤销临时文件失败：{error}")))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| NativeRewindError::Backend(format!("同步撤销临时文件失败：{error}")))?;
    fs::set_permissions(temporary.path(), metadata.permissions())
        .map_err(|error| NativeRewindError::Backend(format!("保留撤销目标权限失败：{error}")))?;
    temporary.persist(path).map_err(|error| {
        NativeRewindError::Backend(format!("原子替换撤销目标失败：{}", error.error))
    })?;
    #[cfg(unix)]
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn rewind_record_path(storage_root: &Path, session_id: &str, operation_id: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{session_id}\0{operation_id}").as_bytes());
    storage_root
        .join("file-rewinds")
        .join(format!("{digest:x}.json"))
}

fn read_rewind_record(path: &Path) -> Result<Option<RewindRecord>, NativeRewindError> {
    let Some(bytes) =
        storage::read_private_bytes_bounded(path, REWIND_RECORD_LIMIT as u64, "文件撤销事务记录")
            .map_err(|error| NativeRewindError::Backend(error.to_string()))?
    else {
        return Ok(None);
    };
    let record = serde_json::from_slice::<RewindRecord>(&bytes).map_err(|error| {
        NativeRewindError::Indeterminate(format!("文件撤销事务记录损坏：{error}"))
    })?;
    validate_record_shape(&record)?;
    Ok(Some(record))
}

fn write_rewind_record(path: &Path, record: &RewindRecord) -> Result<(), NativeRewindError> {
    validate_record_shape(record)?;
    let bytes = serde_json::to_vec(record)
        .map_err(|error| NativeRewindError::Backend(error.to_string()))?;
    if bytes.len() > REWIND_RECORD_LIMIT {
        return Err(NativeRewindError::Backend(
            "文件撤销事务记录超过大小限制".to_owned(),
        ));
    }
    storage::atomic_write_private(path, &bytes)
        .map_err(|error| NativeRewindError::Backend(error.to_string()))
}

fn validate_record_shape(record: &RewindRecord) -> Result<(), NativeRewindError> {
    if record.schema != REWIND_RECORD_SCHEMA || record.version != REWIND_RECORD_VERSION {
        return Err(NativeRewindError::Indeterminate(
            "文件撤销事务记录版本不受支持".to_owned(),
        ));
    }
    for (value, field) in [
        (&record.session_id, "sessionId"),
        (&record.operation_id, "operationId"),
        (&record.project_root, "projectRoot"),
        (&record.turn_id, "turnId"),
    ] {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(NativeRewindError::Indeterminate(format!(
                "文件撤销事务记录的 {field} 无效"
            )));
        }
    }
    if record
        .paths
        .iter()
        .any(|path| path.is_empty() || path.chars().any(char::is_control))
    {
        return Err(NativeRewindError::Indeterminate(
            "文件撤销事务记录包含无效路径".to_owned(),
        ));
    }
    Ok(())
}

fn validate_record(
    record: &RewindRecord,
    session_id: &str,
    operation_id: &str,
    project_root: &str,
    target_turn_id: &str,
) -> Result<(), NativeRewindError> {
    validate_record_shape(record)?;
    if record.session_id != session_id
        || record.operation_id != operation_id
        || record.project_root != project_root
        || record.turn_id != target_turn_id
    {
        return Err(indeterminate_error());
    }
    Ok(())
}

pub(crate) fn completed_rewind_turn_ids(
    storage_root: &Path,
    session_id: &str,
) -> Result<BTreeSet<String>, String> {
    let directory = storage_root.join("file-rewinds");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("读取文件撤销事务目录失败：{error}")),
    };
    let mut turn_ids = BTreeSet::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("读取文件撤销事务目录项失败：{error}"))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = read_rewind_record(&path).map_err(|error| error.to_string())? else {
            continue;
        };
        if record.session_id == session_id && record.phase == RewindPhase::Completed {
            turn_ids.insert(record.turn_id);
        }
    }
    Ok(turn_ids)
}

fn indeterminate_error() -> NativeRewindError {
    NativeRewindError::Indeterminate(
        "文件撤销结果不确定，禁止自动重放，请人工核对工作区".to_owned(),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        REWIND_RECORD_SCHEMA, REWIND_RECORD_VERSION, RewindPhase, RewindRecord,
        completed_rewind_turn_ids, rewind_record_path, sha256_hex, validate_record_shape,
    };
    use sha2::Digest;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn record(phase: RewindPhase, operation_id: &str, turn_id: &str) -> RewindRecord {
        RewindRecord {
            schema: REWIND_RECORD_SCHEMA.to_owned(),
            version: REWIND_RECORD_VERSION,
            session_id: "session-1".to_owned(),
            operation_id: operation_id.to_owned(),
            project_root: "C:/project".to_owned(),
            turn_id: turn_id.to_owned(),
            phase,
            paths: vec!["C:/project/a.txt".to_owned()],
        }
    }

    #[test]
    fn rewind_record_round_trips_and_rejects_unknown_fields() {
        let prepared = record(RewindPhase::Prepared, "command-1", "turn-1");
        let value = serde_json::to_value(&prepared).expect("encode");
        let decoded: RewindRecord = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded.phase, RewindPhase::Prepared);

        let mut value = serde_json::to_value(prepared).expect("encode");
        value["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<RewindRecord>(value).is_err());
    }

    #[test]
    fn rewind_record_rejects_legacy_version_and_sidecar_fields() {
        let prepared = record(RewindPhase::Prepared, "command-1", "turn-1");
        let mut legacy_version = serde_json::to_value(&prepared).expect("encode");
        legacy_version["version"] = serde_json::json!(1);
        let legacy_version =
            serde_json::from_value::<RewindRecord>(legacy_version).expect("decode legacy shape");
        assert!(validate_record_shape(&legacy_version).is_err());

        let mut legacy_field = serde_json::to_value(prepared).expect("encode");
        legacy_field["baseRevision"] = serde_json::json!(3);
        assert!(serde_json::from_value::<RewindRecord>(legacy_field).is_err());
    }

    #[test]
    fn duplicate_rewind_error_has_stable_code_and_message() {
        let error = super::NativeRewindError::AlreadyCompleted;
        assert_eq!(error.code(), "native-rewind-already-completed");
        assert_eq!(error.to_string(), "目标 Turn 已完成文件撤销");
    }

    #[test]
    fn completed_rewind_turn_ids_only_returns_completed_records() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-file-rewind-native-test-{}-{suffix}",
            std::process::id()
        ));
        let directory = root.join("file-rewinds");
        fs::create_dir_all(&directory).expect("create fixture directory");
        for (name, value) in [
            (
                "completed.json",
                record(RewindPhase::Completed, "completed", "turn-completed"),
            ),
            (
                "prepared.json",
                record(RewindPhase::Prepared, "prepared", "turn-prepared"),
            ),
        ] {
            fs::write(
                directory.join(name),
                serde_json::to_vec(&value).expect("encode fixture"),
            )
            .expect("write fixture");
        }
        let result = completed_rewind_turn_ids(&root, "session-1").expect("read fixture");
        assert_eq!(
            result.into_iter().collect::<Vec<_>>(),
            vec!["turn-completed"]
        );
        fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn rewind_record_path_binds_session_and_operation_with_sha256() {
        let actual = rewind_record_path(PathBuf::from("D:/data").as_path(), "session", "op");
        let expected = PathBuf::from("D:/data/file-rewinds")
            .join(format!("{:x}.json", sha2::Sha256::digest(b"session\0op")));
        assert_eq!(actual, expected);
    }

    #[test]
    fn sha256_hex_is_lowercase_content_hash() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
