//! 原页面手动工作树移除。Git 决定 checkout 生命周期，Session/PTY 引用由宿主检查。
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveInput {
    pub(crate) cwd: String,
    pub(crate) path: String,
    #[serde(default)]
    force: bool,
    /// 保留当前 Source 协议字段；旧的临时分支自动回收请求会在任何 Git 操作前拒绝。
    #[serde(default)]
    reclaim_temporary_branch: bool,
    archive_cleanup: Option<ArchiveCleanup>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveCleanup {
    thread_id: String,
    archive_sequence: u64,
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    crate::ui_git_stash::git(root, args)
}

fn reject_unsupported_temporary_branch_reclaim(reclaim: bool) -> Result<(), String> {
    if reclaim {
        return Err("已不支持自动回收临时分支，工作树已保留".into());
    }
    Ok(())
}

/// 删除墓碑只表示 Session 删除事实已经提交；崩溃残留的元数据不能继续阻挡
/// 工作树，但活动注册、运行状态或损坏状态仍然代表写入可能在进行，必须保留保护。
fn is_session_reference_candidate(
    session: &keencode_resources::StoredSessionMetadata,
    deleted: &[keencode_resources::SessionId],
    active: &[String],
    closing: &[String],
) -> bool {
    if !deleted.contains(&session.session_id) {
        return true;
    }
    session.corrupt
        || active.iter().any(|id| id == session.session_id.as_str())
        || closing.iter().any(|id| id == session.session_id.as_str())
        || matches!(
            &session.status,
            keencode_resources::SessionStatus::Running | keencode_resources::SessionStatus::Waiting
        )
}

fn filter_session_reference_candidates(
    sessions: Vec<keencode_resources::StoredSessionMetadata>,
    deleted: &[keencode_resources::SessionId],
    active: &[String],
    closing: &[String],
) -> Vec<keencode_resources::StoredSessionMetadata> {
    sessions
        .into_iter()
        .filter(|session| is_session_reference_candidate(session, deleted, active, closing))
        .collect()
}

fn any_associated_path_references_checkout<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    candidate: &Path,
) -> bool {
    paths
        .into_iter()
        .any(|path| path_references_checkout(path, candidate))
}

/// 即使 cwd 本身是 linked worktree，也不能将主 checkout 或用户指定 cwd 作为删除目标。
pub(crate) fn validate_target(root: &Path, supplied: &str) -> Result<PathBuf, String> {
    let path = Path::new(supplied);
    if !path.is_absolute() || supplied.chars().any(char::is_control) {
        return Err("移除目标必须是绝对工作树目录".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("不能移除链接或 junction 形式的工作树目标".into());
        }
    }
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("移除目标不是普通工作树目录".into());
    }
    let candidate = fs::canonicalize(path).map_err(|error| error.to_string())?;
    if candidate == root
        || !candidate.join(".git").is_file()
        || !crate::ui_worktrees::is_linked_worktree(root, &candidate)
    {
        return Err("移除目标不是该项目的 linked worktree；主目录不会被删除".into());
    }
    Ok(candidate)
}

/// 保留 checkout 中或其子目录中的所有 Session/PTY；force 只控制 Git 脏文件，不能绕过引用。
pub(crate) fn path_references_checkout(path: &str, candidate: &Path) -> bool {
    let mut ancestor = Path::new(path);
    if !ancestor.is_absolute() {
        return false;
    }
    // 已绑定的子目录可能被外部删除；最近存在的祖先仍证明 Session 对 checkout 的引用。
    loop {
        if let Ok(actual) = fs::canonicalize(ancestor) {
            return actual.starts_with(candidate);
        }
        let Some(parent) = ancestor.parent() else {
            return false;
        };
        ancestor = parent;
    }
}

fn remove(root: &Path, candidate: &Path, force: bool) -> Result<(), String> {
    // 无 --force 时显式保护 ignored 文件，Git 默认行为并不能保护这些文件。
    if !force {
        crate::ui_worktree_handoff::remove_clean_worktree(root, candidate)?;
        return Ok(());
    }
    let text = crate::path_utils::path_to_frontend(candidate);
    git(root, &["worktree", "remove", "--force", "--", &text]).map(|_| ())
}

fn remove_with_ref(root: &Path, candidate: &Path, force: bool) -> Result<(), String> {
    remove(root, candidate, force)
}

#[tauri::command]
pub async fn ui_git_worktree_remove(app: AppHandle, input: RemoveInput) -> Result<(), String> {
    reject_unsupported_temporary_branch_reclaim(input.reclaim_temporary_branch)?;
    if let Some(archive) = input.archive_cleanup {
        // 原 archiveSequence 属于上游投影水位，不能当作 ACP Journal sequence 来假装校验通过。
        let _ = (archive.thread_id, archive.archive_sequence);
        return Err("自动清理必须通过准确的 ACP 归档回执适配，工作树已保留".into());
    }
    let _git = crate::ui_git_stash::STASH_GATE.lock().await;
    let root = crate::workspace::registered_project_root(&app, &input.cwd)?;
    let candidate = validate_target(&root, &input.path)?;
    let runtime = app
        .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or("工作树移除需要本进程持有本地 Runtime")?;
    let active = runtime
        .active_session_ids()
        .map_err(|error| error.to_string())?;
    let target_text = candidate.to_string_lossy();
    let deleted =
        keencode_resources::list_deleted_session_ids(runtime.storage_root(), &target_text)
            .map_err(|error| error.to_string())?;
    let stored_sessions = runtime
        .stored_sessions()
        .map_err(|error| error.to_string())?;
    let mut closing = Vec::new();
    // SessionMutationGuard 会先注销已登记 Session；此时 active_session_ids 看不到它，
    // 但 deferred lifecycle 仍是 workspace 写入屏障，不能被删除墓碑绕过。
    for session in &stored_sessions {
        if runtime
            .workspace_mutation_in_progress(session.session_id.as_str())
            .map_err(|error| error.to_string())?
        {
            closing.push(session.session_id.as_str().to_owned());
        }
    }
    let sessions =
        filter_session_reference_candidates(stored_sessions, &deleted, &active, &closing);
    for session in &sessions {
        if path_references_checkout(&session.project_root, &candidate) {
            return Err("仍有会话绑定此工作树；请先删除会话或交接到其他目录".into());
        }
    }
    // 会话 cwd 已在 local 时仍可能保留 associatedWorktreePath，必须同时检查历史关联。
    let ids: Vec<_> = sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    let associated = crate::ui_presentation::associated_worktree_paths(&app, &ids)?;
    if any_associated_path_references_checkout(associated.iter().map(String::as_str), &candidate) {
        return Err("仍有会话关联此工作树，未移除".into());
    }
    if app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .ok_or("终端管理器不可用，未移除工作树")?
        .has_live_checkout(&candidate)?
    {
        return Err("仍有终端使用此工作树，未移除".into());
    }
    tauri::async_runtime::spawn_blocking(move || remove_with_ref(&root, &candidate, input.force))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests;
