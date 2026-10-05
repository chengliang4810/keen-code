//! 工作树交接与现有 Session workspace 事务装配；不建立另一份会话事实源。
use super::AcpHost;
use crate::session_commands::{
    authorized_metadata, close_session_for_mutation, restore_session_after_mutation,
    restore_session_after_workspace_mutation, retry_session_mutation,
};
use crate::ui_worktree_handoff::{
    HandoffInput, Transfer, begin, finish, prepare, read_receipt, receipt_path,
};
use keencode_resources::{SessionId, SessionWorkspaceRequest};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tauri::Manager;

fn association(app: &tauri::AppHandle, id: &str, result: &Value) -> Result<(), String> {
    crate::ui_presentation::ui_presentation_patch(
        app.clone(),
        "threads".into(),
        Some(id.into()),
        json!({
            "pendingWorktree":false,
            "branch":result["branch"],
            "associatedWorktreePath":result["associatedWorktreePath"],
            "associatedWorktreeBranch":result["associatedWorktreeBranch"],
            "associatedWorktreeRef":result["associatedWorktreeRef"],
        }),
        Some(false),
    )?;
    Ok(())
}

async fn rollback_git(record: &Transfer) -> String {
    let record = record.clone();
    tauri::async_runtime::spawn_blocking(move || crate::ui_worktree_handoff::rollback(&record))
        .await
        .unwrap_or_else(|error| format!("Git 回滚任务失败，恢复点仍保留：{error}"))
}

/// 只记录交接阶段和结果，便于区分 Git、MCP、workspace Journal 与恢复失败；
/// 不写入路径、请求参数、Session 正文或凭据。
fn handoff_phase(phase: &'static str, status: &'static str) {
    tracing::info!(
        target: "keencode_diagnostics",
        component = "workspace_handoff",
        phase,
        status,
        "workspace handoff phase"
    );
}

/// 将 workspace Journal 变更失败归一为不含路径、请求正文或底层 IO 的稳定种类。
/// 该诊断用于区分 Runtime 句柄占用和资源事务拒绝，不能把错误正文写入日志。
fn workspace_mutation_error_kind(error: &keencode_runtime::RuntimeError) -> &'static str {
    match error {
        keencode_runtime::RuntimeError::SessionBusy => "runtime_session_busy",
        keencode_runtime::RuntimeError::SessionNotCreated => "runtime_session_not_created",
        keencode_runtime::RuntimeError::SessionNotRegistered => "runtime_session_not_registered",
        keencode_runtime::RuntimeError::SessionClosed => "runtime_session_closed",
        keencode_runtime::RuntimeError::RecoveryRequired => "runtime_recovery_required",
        keencode_runtime::RuntimeError::StateUnavailable => "runtime_state_unavailable",
        keencode_runtime::RuntimeError::Resource(
            keencode_resources::ResourceError::SessionMutationBusy,
        ) => "resource_session_mutation_busy",
        keencode_runtime::RuntimeError::Resource(
            keencode_resources::ResourceError::SessionMutationConflict,
        ) => "resource_session_mutation_conflict",
        keencode_runtime::RuntimeError::Resource(
            keencode_resources::ResourceError::SessionMutationNotApplicable(_),
        ) => "resource_session_mutation_not_applicable",
        keencode_runtime::RuntimeError::Resource(
            keencode_resources::ResourceError::SessionMutationRecoveryRequired(_),
        ) => "resource_session_mutation_recovery_required",
        keencode_runtime::RuntimeError::Resource(_) => "resource_other",
        _ => "runtime_other",
    }
}

fn handoff_phase_failure(phase: &'static str, error: &keencode_runtime::RuntimeError) {
    tracing::warn!(
        target: "keencode_diagnostics",
        component = "workspace_handoff",
        phase,
        status = "failed",
        error_kind = workspace_mutation_error_kind(error),
        "workspace handoff phase failed"
    );
}

/// 冷历史会话没有 Runtime 槽位；通过活动注册表与持久状态共同检查 checkout 占用。
fn ensure_checkout_idle(
    runtime: &crate::agent_runtime::AgentRuntime,
    moving_id: &str,
    checkouts: &[std::path::PathBuf],
) -> Result<(), String> {
    let active = runtime
        .active_session_ids()
        .map_err(|error| error.to_string())?;
    for session in runtime
        .stored_sessions()
        .map_err(|error| error.to_string())?
    {
        if session.session_id.as_str() == moving_id {
            continue;
        }
        let Ok(candidate) = fs::canonicalize(&session.project_root) else {
            continue;
        };
        if checkouts.contains(&candidate)
            && (session.corrupt
                || active.iter().any(|id| id == session.session_id.as_str())
                || matches!(
                    session.status,
                    keencode_resources::SessionStatus::Running
                        | keencode_resources::SessionStatus::Waiting
                ))
        {
            return Err("同一 checkout 中还有运行中的会话，请先停止后再交接".into());
        }
    }
    Ok(())
}

impl AcpHost {
    pub(crate) async fn stop_workspace_session(&self, id: &str) -> Result<(), String> {
        let _control = self
            .lock_session_control(id)
            .await
            .map_err(|_| "会话控制锁不可用")?;
        authorized_metadata(&self.runtime, &self.app, id)?;
        // 复用 Runtime 的根 Agent、后台任务、MCP、投递与 Session 所有权清理，不能只取消页面当前 Turn。
        self.runtime
            .close_session(id)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn handoff_workspace(&self, input: HandoffInput) -> Result<Value, String> {
        // 与其他 Session 控制及 stash 切换共用锁，不能在 apply 期间改变 stash reflog 身份。
        let _control = self
            .lock_session_control(&input.thread_id)
            .await
            .map_err(|_| "会话控制锁不可用")?;
        let _git = crate::ui_git_stash::STASH_GATE.lock().await;
        let (_, current) = authorized_metadata(&self.runtime, &self.app, &input.thread_id)?;
        let root = self
            .authorized_cwd(Path::new(&input.cwd))
            .map_err(|_| "交接项目目录未获授权")?;
        let data = crate::storage::root_dir(&self.app).map_err(|error| error.to_string())?;
        let receipt = receipt_path(&data, &input);
        if let Some(mut previous) = read_receipt(&receipt, &input)? {
            if current != previous.target || previous.result.is_none() {
                return Err(format!(
                    "此交接尚有待检查的 Git 恢复点，请先检查 {}；不会再次转移或清理文件",
                    receipt.display()
                ));
            }
            if previous.completed {
                return previous.result.ok_or("交接回执缺少结果".into());
            }
            // Git 与权威 cwd 已提交，但展示落盘/客户端连接曾失败：只补齐持久化与清理，不重新转移修改。
            handoff_phase("target_extensions", "started");
            if self.ensure_extensions(&current).await.is_err() {
                handoff_phase("target_extensions", "failed");
                return Err("无法恢复工作区扩展".to_owned());
            }
            handoff_phase("target_extensions", "completed");
            let _session = crate::session_commands::open_authorized_session(
                &self.runtime,
                &self.app,
                &input.thread_id,
            )?;
            self.runtime
                .ensure_session_delivery(&input.thread_id)
                .map_err(|error| error.to_string())?;
            handoff_phase("association", "started");
            let association_result = previous
                .result
                .as_ref()
                .ok_or_else(|| "交接结果缺失".to_owned())
                .and_then(|result| association(&self.app, &input.thread_id, result));
            if let Err(error) = association_result {
                handoff_phase("association", "failed");
                return Err(error);
            }
            handoff_phase("association", "completed");
            let remove_source = self.can_remove_handoff_source(&previous)?;
            handoff_phase("finish", "started");
            let finished = match tauri::async_runtime::spawn_blocking(move || {
                finish(&mut previous, &receipt, remove_source)
            })
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    handoff_phase("finish", "failed");
                    return Err(error.to_string());
                }
            };
            handoff_phase(
                "finish",
                if finished.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
            );
            return finished;
        }
        if input.target_mode == "worktree" {
            let metadata = crate::ui_presentation::thread_metadata(&self.app, &input.thread_id)?;
            for (key, supplied) in [
                ("associatedWorktreePath", &input.associated_worktree_path),
                (
                    "associatedWorktreeBranch",
                    &input.associated_worktree_branch,
                ),
                ("associatedWorktreeRef", &input.associated_worktree_ref),
            ] {
                if metadata[key].as_str() != supplied.as_deref() {
                    return Err("工作树关联信息已改变，请刷新后重新交接".into());
                }
            }
        } else if input
            .worktree_path
            .as_deref()
            .and_then(|path| fs::canonicalize(path).ok())
            .as_ref()
            != Some(&current)
        {
            return Err("会话工作树已改变，请刷新后重新交接".into());
        }
        let mut checkouts = vec![current.clone(), root.clone()];
        if let Some(associated) = input
            .associated_worktree_path
            .as_deref()
            .and_then(|path| fs::canonicalize(path).ok())
        {
            checkouts.push(associated);
        }
        ensure_checkout_idle(&self.runtime, &input.thread_id, &checkouts)?;
        handoff_phase("close_session_for_mutation", "started");
        let context =
            match close_session_for_mutation(&self.runtime, &self.app, &input.thread_id).await {
                Ok(context) => {
                    handoff_phase("close_session_for_mutation", "completed");
                    context
                }
                Err(error) => {
                    handoff_phase("close_session_for_mutation", "failed");
                    return Err(error);
                }
            };
        let git_root = root.clone();
        let git_source = current.clone();
        let git_input = input.clone();
        let git_receipt = receipt.clone();
        handoff_phase("git_prepare", "started");
        let prepared_git = tauri::async_runtime::spawn_blocking(move || {
            let record = begin(&git_root, &git_source, git_input, &git_receipt)?;
            prepare(&git_root, record, &git_receipt)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
        handoff_phase(
            "git_prepare",
            if prepared_git.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        let mut record = match prepared_git {
            Ok(record) => record,
            Err(error) => {
                let restored =
                    restore_session_after_mutation(&self.runtime, &input.thread_id, &context);
                return Err(format!(
                    "{error}{}",
                    restored
                        .err()
                        .map(|error| format!("；会话恢复失败：{error}"))
                        .unwrap_or_default()
                ));
            }
        };
        if record.target_created {
            // 仅宿主实际新建的 checkout 可参与原页面自动归档清理；历史外部目录不猜测来源。
            if let Err(error) =
                crate::ui_worktree_archive::mark_created(&data, &root, &record.target)
            {
                let rollback = rollback_git(&record).await;
                let restored =
                    restore_session_after_mutation(&self.runtime, &input.thread_id, &context);
                return Err(format!("{error}；{rollback}；会话恢复结果：{restored:?}"));
            }
        }
        handoff_phase("target_extensions", "started");
        if let Err(error) = self.ensure_extensions(&record.target).await {
            handoff_phase("target_extensions", "failed");
            let rollback = rollback_git(&record).await;
            let restored =
                restore_session_after_mutation(&self.runtime, &input.thread_id, &context);
            let error = error.rpc_error().message;
            return Err(format!("{error}；{rollback}；会话恢复结果：{restored:?}"));
        }
        handoff_phase("target_extensions", "completed");
        handoff_phase("prepare_workspace_mcp", "started");
        let prepared_mcp = match self
            .runtime
            .prepare_workspace_mcp(&input.thread_id, &record.target, &context.session_mcp)
            .await
        {
            Ok(prepared) => {
                handoff_phase("prepare_workspace_mcp", "completed");
                prepared
            }
            Err(error) => {
                handoff_phase("prepare_workspace_mcp", "failed");
                let rollback = rollback_git(&record).await;
                let restored =
                    restore_session_after_mutation(&self.runtime, &input.thread_id, &context);
                return Err(format!("{error};{rollback};会话恢复结果：{restored:?}"));
            }
        };
        let request = SessionWorkspaceRequest {
            session_id: SessionId::new(input.thread_id.clone())
                .map_err(|error| error.to_string())?,
            operation_id: format!("{}:workspace", input.command_id),
            expected_project_root: current.to_string_lossy().into_owned(),
            project_root: record.target.to_string_lossy().into_owned(),
        };
        handoff_phase("change_closed_session_workspace", "started");
        let mutation = retry_session_mutation(|| {
            self.runtime
                .runtime_manager()
                .change_closed_session_workspace(request.clone())
        })
        .await;
        match &mutation {
            Ok(()) => handoff_phase("change_closed_session_workspace", "completed"),
            Err(error) => handoff_phase_failure("change_closed_session_workspace", error),
        }
        // 资源层的返回错误不能证明没有提交；先读权威日志再决定是否允许 Git 回滚。
        let root_after =
            authorized_metadata(&self.runtime, &self.app, &input.thread_id).map(|(_, root)| root);
        if root_after.as_ref().is_ok_and(|root| *root == current) {
            let rollback = rollback_git(&record).await;
            let restored =
                restore_session_after_mutation(&self.runtime, &input.thread_id, &context);
            return Err(format!(
                "会话目录未提交：{mutation:?}；{rollback}；会话恢复结果：{restored:?}"
            ));
        }
        handoff_phase("restore_session_after_workspace_mutation", "started");
        if let Err(error) = restore_session_after_workspace_mutation(
            &self.runtime,
            &self.app,
            &input.thread_id,
            &context,
            &prepared_mcp,
        )
        .await
        {
            handoff_phase("restore_session_after_workspace_mutation", "failed");
            return Err(error);
        }
        handoff_phase("restore_session_after_workspace_mutation", "completed");
        mutation.map_err(|error| error.to_string())?;
        handoff_phase("association", "started");
        if let Err(error) = association(
            &self.app,
            &input.thread_id,
            record.result.as_ref().ok_or("Git 交接缺少结果")?,
        ) {
            handoff_phase("association", "failed");
            return Err(error);
        }
        handoff_phase("association", "completed");
        let remove_source = self.can_remove_handoff_source(&record)?;
        handoff_phase("finish", "started");
        let finished = match tauri::async_runtime::spawn_blocking(move || {
            finish(&mut record, &receipt, remove_source)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                handoff_phase("finish", "failed");
                return Err(error.to_string());
            }
        };
        handoff_phase(
            "finish",
            if finished.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        finished
    }

    /// 其他会话（包括归档会话）仍绑定源目录时保留 checkout，避免破坏其下次恢复。
    fn can_remove_handoff_source(&self, record: &Transfer) -> Result<bool, String> {
        let terminals = self
            .app
            .state::<std::sync::Arc<crate::terminal::TerminalManager>>();
        if terminals.has_live_checkout(&record.source)? {
            return Ok(false);
        }
        for session in self
            .runtime
            .stored_sessions()
            .map_err(|error| error.to_string())?
        {
            if session.session_id.as_str() != record.input.thread_id
                && fs::canonicalize(&session.project_root).is_ok_and(|path| path == record.source)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程重启后同项目存在未加载历史 Session，交接不能尝试读取不存在的活动槽位。
    #[test]
    fn cold_idle_history_does_not_block_handoff() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("data");
        let project = fixture.path().join("project");
        fs::create_dir(&project).unwrap();
        let project = fs::canonicalize(project).unwrap();
        let runtime = crate::agent_runtime::AgentRuntime::new_for_control_test(&data).unwrap();
        let session = runtime
            .open_or_create_session(&project, None, "cold-history")
            .unwrap();
        let id = session.session_id().as_str().to_owned();
        drop(session);
        drop(runtime);
        let cold = crate::agent_runtime::AgentRuntime::new_for_control_test(&data).unwrap();
        assert_eq!(cold.stored_sessions().unwrap().len(), 1);
        assert!(
            cold.session_has_active_work(&id).is_err(),
            "夹具必须没有 Runtime 活动槽位"
        );
        ensure_checkout_idle(&cold, "moving-session", &[project]).unwrap();
    }
}
