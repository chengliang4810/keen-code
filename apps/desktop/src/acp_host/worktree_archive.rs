//! 将原页面归档水位对接到 ACP 的精确控制回执，并保护 Undo 与重新归档之间的竞态。
use super::AcpHost;
use crate::ui_worktree_archive::{self as archive, CleanupInput};
use keencode_resources::SessionEvent;
use std::{collections::HashSet, path::Path, sync::Arc};
use tauri::Manager;

fn changes_archive(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::SessionPreferenceSet { archived, .. } => archived.is_some(),
        SessionEvent::AtomicBatch { events } => events.iter().any(changes_archive),
        _ => false,
    }
}

/// 同一 commandId 重试必须返回原事件；之后哪怕 Undo 后重新归档为 true，旧回执也已失效。
fn validate_archive(
    session: &keencode_runtime::RuntimeSession,
    operation: &str,
    sequence: u64,
) -> Result<(), String> {
    let committed = session
        .committed_control_event(operation)
        .map_err(|error| error.to_string())?
        .ok_or("归档操作没有权威回执，工作树已保留")?;
    if committed.sequence != sequence
        || !matches!(
            committed.event,
            SessionEvent::SessionPreferenceSet {
                archived: Some(true),
                ..
            }
        )
    {
        return Err("归档回执与请求不匹配，工作树已保留".into());
    }
    let snapshot = session.snapshot().map_err(|error| error.to_string())?;
    if !snapshot.state.archived
        || snapshot.recovery_required
        || snapshot.active_reservations != 0
        || matches!(
            snapshot.state.status,
            keencode_resources::SessionStatus::Running | keencode_resources::SessionStatus::Waiting
        )
    {
        return Err("会话已取消归档或仍有活动工作，工作树已保留".into());
    }
    let mut after = Some(sequence);
    loop {
        let page = session
            .replay(after, 256)
            .map_err(|error| error.to_string())?;
        if page
            .records
            .iter()
            .any(|record| changes_archive(&record.event))
        {
            return Err("归档已被后续操作替代，工作树已保留".into());
        }
        if !page.has_more {
            break;
        }
        after = Some(page.next_after.ok_or("归档回执分页游标无效")?);
    }
    Ok(())
}

/// 已完成删除的 Session 仍保留日志元数据，但不再拥有当前工作树的活动引用。
fn is_archive_reference_candidate(
    session: &keencode_resources::StoredSessionMetadata,
    owner: &str,
    deleted: &HashSet<keencode_resources::SessionId>,
) -> bool {
    session.session_id.as_str() != owner && !deleted.contains(&session.session_id)
}

impl AcpHost {
    fn archive_checkout_unreferenced(&self, owner: &str, target: &Path) -> Result<(), String> {
        let sessions = self
            .runtime
            .stored_sessions()
            .map_err(|error| error.to_string())?;
        let target_text = target.to_string_lossy();
        let deleted =
            keencode_resources::list_deleted_session_ids(self.runtime.storage_root(), &target_text)
                .map_err(|error| error.to_string())?
                .into_iter()
                .collect::<HashSet<_>>();
        for other in &sessions {
            if is_archive_reference_candidate(other, owner, &deleted)
                && crate::ui_worktree_remove::path_references_checkout(&other.project_root, target)
            {
                return Err("其他会话仍绑定此工作树，已保留".into());
            }
        }
        let ids: Vec<_> = sessions
            .iter()
            .filter(|other| is_archive_reference_candidate(other, owner, &deleted))
            .map(|other| other.session_id.as_str())
            .collect();
        for path in crate::ui_presentation::associated_worktree_paths(&self.app, &ids)? {
            if crate::ui_worktree_remove::path_references_checkout(&path, target) {
                return Err("其他会话仍关联此工作树，已保留".into());
            }
        }
        if self
            .app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
            .ok_or("终端管理器不可用，已保留工作树")?
            .has_live_checkout(target)?
        {
            return Err("仍有终端使用此工作树，已保留".into());
        }
        Ok(())
    }

    pub(crate) async fn cleanup_archived_worktree(
        &self,
        input: CleanupInput,
    ) -> Result<(), String> {
        let _control = self
            .lock_session_control(&input.thread_id)
            .await
            .map_err(|_| "会话控制锁不可用")?;
        let _git = crate::ui_git_stash::STASH_GATE.lock().await;
        let data = crate::storage::root_dir(&self.app).map_err(|error| error.to_string())?;
        if let Some(record) = archive::read_receipt(&data, &input.thread_id)? {
            // 删除回执丢失后的相同请求只补返回；不重新恢复再删除，也不触碰后来出现的目录。
            let owner_is_registered = crate::workspace::registered_project_root(
                &self.app,
                &crate::path_utils::path_to_frontend(&record.checkout.root),
            )
            .is_ok_and(|root| root == record.checkout.root);
            let cwd_is_same_owner_or_checkout = input.cwd
                == crate::path_utils::path_to_frontend(&record.checkout.path)
                || crate::workspace::registered_project_root(&self.app, &input.cwd)
                    .is_ok_and(|root| root == record.checkout.root);
            if owner_is_registered
                && cwd_is_same_owner_or_checkout
                && record.operation_id == input.operation_id
                && record.archive_sequence == input.journal_sequence
                && crate::path_utils::path_to_frontend(&record.checkout.path) == input.path
                && !record.checkout.path.exists()
                && matches!(record.phase.as_str(), "prepared" | "removed")
            {
                return Ok(());
            }
        }
        // `cwd` 解析为当前 linked checkout；持久登记的记录才拥有 Git 使用的源根目录，
        // handoff 后这两个路径可能不同。
        let active_checkout = crate::workspace::registered_project_root(&self.app, &input.cwd)?;
        let owner_hint = archive::managed_checkout_owner(&data, &active_checkout)?;
        let root = crate::workspace::registered_project_root(
            &self.app,
            &crate::path_utils::path_to_frontend(&owner_hint),
        )?;
        if root != owner_hint {
            return Err("工作树登记的项目身份已改变，已保留".into());
        }
        let target = crate::ui_worktree_remove::validate_target(&root, &input.path)?;
        if target != active_checkout {
            return Err("归档目标不是当前工作树，已保留".into());
        }
        let checkout = archive::managed_checkout(&data, &root, &target)?;
        let (_, current) = crate::session_commands::authorized_metadata(
            &self.runtime,
            &self.app,
            &input.thread_id,
        )?;
        let association = crate::ui_presentation::thread_metadata(&self.app, &input.thread_id)?;
        if current != target
            && !association["associatedWorktreePath"]
                .as_str()
                .is_some_and(|path| {
                    crate::ui_worktree_remove::path_references_checkout(path, &target)
                })
        {
            return Err("会话不拥有该工作树，已保留".into());
        }
        let session = crate::session_commands::open_authorized_session(
            &self.runtime,
            &self.app,
            &input.thread_id,
        )?;
        validate_archive(&session, &input.operation_id, input.journal_sequence)?;
        self.archive_checkout_unreferenced(&input.thread_id, &target)?;
        drop(session);
        let context = crate::session_commands::close_session_for_mutation(
            &self.runtime,
            &self.app,
            &input.thread_id,
        )
        .await?;
        let session_id = input.thread_id.clone();
        // 关闭投递与 MCP 后再核对引用；不自动关闭其他会话或未知终端。
        let result = self.archive_checkout_unreferenced(&input.thread_id, &target);
        let result = if result.is_ok() {
            tauri::async_runtime::spawn_blocking(move || {
                let mut record = archive::prepare_receipt(&data, &input, checkout)?;
                archive::remove_checkout(&data, &mut record)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
        } else {
            result
        };
        if result.is_err() && context.project_root.exists() {
            crate::session_commands::restore_session_after_mutation(
                &self.runtime,
                &session_id,
                &context,
            )?;
        }
        result
    }

    pub(crate) async fn recover_archived_worktree(&self, id: &str) -> Result<bool, String> {
        let _control = self
            .lock_session_control(id)
            .await
            .map_err(|_| "会话控制锁不可用")?;
        let _git = crate::ui_git_stash::STASH_GATE.lock().await;
        let data = crate::storage::root_dir(&self.app).map_err(|error| error.to_string())?;
        let Some(mut record) = archive::read_receipt(&data, id)? else {
            return Ok(false);
        };
        let metadata = self
            .runtime
            .runtime_manager()
            .stored_session_metadata(id)
            .map_err(|error| error.to_string())?;
        if metadata.corrupt {
            return Err("会话日志损坏，未恢复工作树".into());
        }
        if crate::path_utils::path_to_frontend(Path::new(&metadata.project_root))
            != crate::path_utils::path_to_frontend(&record.checkout.path)
        {
            // local 会话的历史关联可以被清理，但不能用该回执改变 local cwd。
            return Ok(false);
        }
        let root = crate::workspace::registered_project_root(
            &self.app,
            &crate::path_utils::path_to_frontend(&record.checkout.root),
        )?;
        if root != record.checkout.root {
            return Err("恢复工作树的项目身份已改变".into());
        }
        tauri::async_runtime::spawn_blocking(move || archive::recover_checkout(&data, &mut record))
            .await
            .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests;
