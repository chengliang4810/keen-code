//! Session 目录的安全枚举与永久删除边界。

use std::fs;
use std::path::Path;

use crate::atomic::{prepare_root, sync_directory};
use crate::{ROOT_AGENT_ID, ResourceError, SessionId, SessionState, SessionStatus};
use serde::{Deserialize, Serialize};

/// 按稳定标识排序列出当前存储根中的全部 Session 目录。
///
/// 目录项必须是可移植 Session 标识对应的真实目录；遇到符号链接、非 UTF-8 名称、
/// 普通文件或越界规范路径时整次查询失败，调用方不得把不完整结果当作完整目录。
pub fn list_session_ids(storage_root: impl AsRef<Path>) -> Result<Vec<SessionId>, ResourceError> {
    let root = prepare_root(storage_root.as_ref())?;
    let sessions = root.clone();
    let mut session_ids = Vec::new();
    let entries = fs::read_dir(&sessions)
        .map_err(|error| ResourceError::io("list_session_directories", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| ResourceError::io("read_session_directory", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ResourceError::io("inspect_session_directory", error))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ResourceError::UnsafePath("Session 目录名称必须是 UTF-8".to_owned()))?;
        if name == "project.json" || name == "session-mutations" {
            continue;
        }
        if file_type.is_symlink() {
            return Err(ResourceError::SymlinkRejected(name));
        }
        if !file_type.is_dir() {
            return Err(ResourceError::UnsafePath(format!(
                "Session 存储包含非目录项：{name}"
            )));
        }
        let session_id = SessionId::new(name)?;
        let canonical = fs::canonicalize(entry.path())
            .map_err(|error| ResourceError::io("canonicalize_session_directory", error))?;
        if canonical.parent() != Some(sessions.as_path()) {
            return Err(ResourceError::UnsafePath(
                "Session 目录越过持久化 sessions 根目录".to_owned(),
            ));
        }
        session_ids.push(session_id);
    }
    session_ids.sort();
    Ok(session_ids)
}

/// 永久删除一个经过标识和目录边界复核的 Session 目录。
///
/// 调用方必须先保证当前进程和其他进程均不再持有该 Session lease。本函数对不存在的
/// Session 幂等返回 `false`；成功删除已有目录后返回 `true`。
pub fn delete_session_storage(
    storage_root: impl AsRef<Path>,
    session_id: &SessionId,
) -> Result<bool, ResourceError> {
    let root = prepare_root(storage_root.as_ref())?;
    let sessions = root.clone();
    let candidate = sessions.join(session_id.as_str());
    let metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ResourceError::io("inspect_deleted_session", error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ResourceError::SymlinkRejected(
            session_id.as_str().to_owned(),
        ));
    }
    if !metadata.is_dir() {
        return Err(ResourceError::UnsafePath(
            "Session 删除目标不是目录".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| ResourceError::io("canonicalize_deleted_session", error))?;
    if canonical.parent() != Some(sessions.as_path()) {
        return Err(ResourceError::UnsafePath(
            "Session 删除目标越过持久化 sessions 根目录".to_owned(),
        ));
    }
    fs::remove_dir_all(&canonical)
        .map_err(|error| ResourceError::io("delete_session_directory", error))?;
    sync_directory(&sessions, true)?;
    Ok(true)
}

/// 持久 Session 列表使用且不包含 Transcript 正文的元数据。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredSessionMetadata {
    /// Session 稳定标识。
    pub session_id: SessionId,
    /// 当前用户可见标题。
    pub title: String,
    /// Session 创建时绑定的项目根目录。
    pub project_root: String,
    /// 权威日志归约得到的当前状态。
    pub status: SessionStatus,
    /// SessionCreated 事件的 Unix Epoch 毫秒时间。
    pub created_at_unix_ms: u64,
    /// 最近一条有效权威事件的 Unix Epoch 毫秒时间。
    pub updated_at_unix_ms: u64,
    /// 最近一条根用户 Turn 起点的 Unix Epoch 毫秒时间；从未发送消息时为 0。
    pub last_user_message_at_unix_ms: u64,
    /// 最近一条有效权威事件的 Journal sequence。
    pub last_sequence: u64,
    /// 事件日志是否在首个无效记录处进入只读损坏状态。
    pub corrupt: bool,
}

impl StoredSessionMetadata {
    /// 从健康或损坏日志的最后有效状态创建不含正文的元数据。
    pub fn from_state(state: &SessionState, corrupt: bool) -> Option<Self> {
        if !state.created {
            return None;
        }
        Some(Self {
            session_id: state.session_id.clone(),
            title: state.title.clone(),
            project_root: state.project_root.clone(),
            status: state.status.clone(),
            created_at_unix_ms: state.created_at_unix_ms,
            updated_at_unix_ms: state.updated_at_unix_ms,
            last_user_message_at_unix_ms: last_user_message_at(state),
            last_sequence: state.last_sequence,
            corrupt,
        })
    }
}

/// 返回最近一条根用户 Turn 的起点时间；子 Agent 续跑不代表新的用户消息。
fn last_user_message_at(state: &SessionState) -> u64 {
    state
        .turns
        .values()
        .filter(|turn| {
            turn.source_agent_id.as_str() == ROOT_AGENT_ID
                && turn.root_turn_id == turn.turn_id
                && turn.parent_turn_id.is_none()
        })
        .map(|turn| turn.started_at_unix_ms)
        .max()
        .unwrap_or(0)
}
