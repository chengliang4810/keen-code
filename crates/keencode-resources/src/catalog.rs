//! Session 目录的安全枚举与永久删除边界。

use std::fs;
use std::path::Path;

use crate::atomic::{prepare_root, secure_child_dir, sync_directory};
use crate::{ResourceError, SessionId};

/// 按稳定标识排序列出当前存储根中的全部 Session 目录。
///
/// 目录项必须是可移植 Session 标识对应的真实目录；遇到符号链接、非 UTF-8 名称、
/// 普通文件或越界规范路径时整次查询失败，调用方不得把不完整结果当作完整目录。
pub fn list_session_ids(storage_root: impl AsRef<Path>) -> Result<Vec<SessionId>, ResourceError> {
    let root = prepare_root(storage_root.as_ref())?;
    let sessions = secure_child_dir(&root, "sessions")?;
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
    let sessions = secure_child_dir(&root, "sessions")?;
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
