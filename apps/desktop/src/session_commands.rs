//! 桌面 Session 控制面：直接操作自研 Runtime，并向前端投影标准 ACP 语义。

use crate::agent_runtime::{AgentRuntime, SessionMutationGuard, SessionWorkspaceMutationAdmission};
use crate::native_paths::NativePaths;
use keencode_resources::ResourceError;
use keencode_runtime::{RuntimeError, RuntimeSession, StoredSessionMetadata};
use std::path::PathBuf;
use std::sync::Arc;

/// 将任意内部错误转换为不包含请求正文的宿主文本错误。
fn runtime_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// 严格校验一个必填标识，不允许隐式裁剪或控制字符。
fn required_identifier<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{field} 不能为空或包含首尾空白"));
    }
    if value.len() > 128 || value.chars().any(char::is_control) {
        return Err(format!("{field} 超出长度限制或包含控制字符"));
    }
    Ok(value)
}

/// 根据项目登记表或唯一内部对话根授权持久 Session 的规范目录。
pub(crate) fn authorize_stored_root(
    paths: &NativePaths,
    stored_root: &str,
) -> Result<PathBuf, String> {
    let canonical = crate::workspace::canonical_session_root(stored_root)?;
    let app_data_root = crate::workspace::app_data_session_root(paths)?;
    let conversation_root = crate::workspace::conversation_workspace_root(paths)?;
    if canonical == app_data_root || canonical == conversation_root {
        return Ok(canonical);
    }
    let registered = crate::workspace::registered_project_root(paths, stored_root)?;
    if canonical != registered {
        return Err("Session 项目目录与当前授权目录不一致".to_owned());
    }
    Ok(registered)
}

/// 查找并授权一个健康的新格式持久 Session。
pub(crate) fn authorized_metadata(
    runtime: &AgentRuntime,
    paths: &NativePaths,
    session_id: &str,
) -> Result<(StoredSessionMetadata, PathBuf), String> {
    required_identifier(session_id, "sessionId")?;
    let metadata = runtime
        .runtime_manager()
        .stored_session_metadata(session_id)
        .map_err(runtime_error)?;
    if metadata.corrupt {
        return Err(format!("Session {session_id} 的权威日志已损坏"));
    }
    let root = authorize_stored_root(paths, &metadata.project_root)?;
    Ok((metadata, root))
}

/// 依据权威元数据授权执行目录；locator 只定位物理存储，不能作为执行授权。
pub(crate) fn authorize_stored_session_root(
    runtime: &AgentRuntime,
    paths: &NativePaths,
    session_id: &str,
) -> Result<PathBuf, String> {
    required_identifier(session_id, "sessionId")?;
    if let Ok(session) = runtime.runtime_manager().get(session_id) {
        let metadata = session
            .read_state(|state| StoredSessionMetadata::from_state(state, false))
            .map_err(runtime_error)?
            .ok_or_else(|| format!("Session {session_id} 尚未创建"))?;
        return authorize_stored_root(paths, &metadata.project_root);
    }
    let (_, root) = authorized_metadata(runtime, paths, session_id)?;
    Ok(root)
}

/// 打开一个已经通过项目登记表或唯一内部对话根授权的 Session。
pub(crate) fn open_authorized_session(
    runtime: &AgentRuntime,
    paths: &NativePaths,
    session_id: &str,
) -> Result<RuntimeSession, String> {
    let (_, root) = authorized_metadata(runtime, paths, session_id)?;
    runtime
        .open_or_create_session(&root, Some(session_id), "session-open")
        .map_err(runtime_error)
}

/// 临时关闭 Session Runtime 后用于恢复桌面连接的最小上下文。
pub(crate) struct ClosedSessionMutationContext {
    /// Session 绑定且已经通过项目登记表授权的规范根目录。
    pub(crate) project_root: PathBuf,
    /// 变更前 Session 是否是桌面当前焦点。
    pub(crate) was_focused: bool,
    /// 覆盖 Git、workspace Journal 与恢复的 Turn gate；drop 前 root start 只能等待。
    _mutation_guard: SessionMutationGuard,
}

/// 投递泵取消后等待最后一个共享句柄释放时允许的最大调度重试次数。
const SESSION_MUTATION_LEASE_RETRIES: usize = 32;

/// 仅对投递泵尚未释放 lease 的瞬时 Busy 执行有限调度重试。
pub(crate) async fn retry_session_mutation<T>(
    mut operation: impl FnMut() -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    for attempt in 0..SESSION_MUTATION_LEASE_RETRIES {
        match operation() {
            Err(
                RuntimeError::SessionBusy
                | RuntimeError::Resource(ResourceError::SessionMutationBusy),
            ) if attempt + 1 < SESSION_MUTATION_LEASE_RETRIES => {
                tokio::task::yield_now().await;
            }
            result => return result,
        }
    }
    unreachable!("有限重试循环最后一次必须返回结果")
}

/// 在 Session Turn gate 内确认没有活动工作，停止投递并释放资源层独占 lease。
/// 新的 root start 即使先取得 admission，也会在关闭前复查并使本次 mutation 退出。
pub(crate) async fn close_session_for_mutation(
    runtime: &Arc<AgentRuntime>,
    paths: &NativePaths,
    session_id: &str,
) -> Result<ClosedSessionMutationContext, String> {
    let (_, project_root) = authorized_metadata(runtime, paths, session_id)?;
    let was_focused = runtime
        .focused_session_id()
        .map_err(runtime_error)?
        .as_deref()
        == Some(session_id);
    // 在同一个 Turn gate 下判定“仍登记”或“已冷关闭”。冷路径不能先 reopen，
    // 也不能在返回上下文后才取得 gate，否则 root start 可在 Git 事务中重新登记。
    let Some(admission) = runtime
        .begin_workspace_mutation(session_id)
        .await
        .map_err(runtime_error)?
    else {
        return Err("运行中的对话不能复制或编辑，请先停止任务".to_owned());
    };
    match admission {
        SessionWorkspaceMutationAdmission::Closed(mutation_guard) => {
            Ok(ClosedSessionMutationContext {
                project_root,
                was_focused,
                _mutation_guard: mutation_guard,
            })
        }
        SessionWorkspaceMutationAdmission::Registered(mutation) => {
            // 已登记路径仍通过 open helper 完成项目根校验；Turn gate 已由 admission
            // 持有，因此不会重新引入 start/open 与 close 的竞态。
            let session = runtime
                .open_or_create_session(&project_root, Some(session_id), "session-mutation-open")
                .map_err(runtime_error)?;
            drop(session);
            let mutation_guard = match mutation.close_and_hold().await {
                Ok(Some(mutation_guard)) => mutation_guard,
                Ok(None) => {
                    if was_focused {
                        let _ = runtime.focus_session(session_id);
                    }
                    return Err("运行中的对话不能复制或编辑，请先停止任务".to_owned());
                }
                Err(error) => {
                    if was_focused {
                        let _ = runtime.focus_session(session_id);
                    }
                    return Err(runtime_error(error));
                }
            };
            Ok(ClosedSessionMutationContext {
                project_root,
                was_focused,
                _mutation_guard: mutation_guard,
            })
        }
    }
}

/// 无论资源事务成功或失败，都重新打开源 Session 并恢复原焦点。
pub(crate) fn restore_session_after_mutation(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    context: &ClosedSessionMutationContext,
) -> Result<(), String> {
    runtime
        .open_or_create_session_for_workspace_mutation(
            &context.project_root,
            Some(session_id),
            "session-mutation-restore",
        )
        .map_err(runtime_error)?;
    if context.was_focused {
        runtime.focus_session(session_id).map_err(runtime_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SessionWorkspaceMutationAdmission, required_identifier};

    /// Session 标识必须稳定拒绝空值、隐式裁剪和控制字符。
    #[test]
    fn session_identifier_is_strict() {
        assert_eq!(
            required_identifier("session-1", "sessionId").unwrap(),
            "session-1"
        );
        assert!(required_identifier("", "sessionId").is_err());
        assert!(required_identifier(" session-1", "sessionId").is_err());
        assert!(required_identifier("session\n1", "sessionId").is_err());
    }

    /// 前置 stop 后交接应在线性化 gate 下取得冷 admission，不得为二次 close 重新登记。
    #[test]
    fn pre_stopped_session_uses_closed_workspace_admission() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(fixture.path().join("data"))
                .unwrap();
        let session = runtime
            .open_or_create_session(&project, None, "pre-stopped-handoff")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runtime.close_session(&session_id))
            .unwrap();
        let admission = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runtime.begin_workspace_mutation(&session_id))
            .unwrap()
            .expect("停止后的 Session 应取得冷 workspace admission");
        assert!(matches!(
            admission,
            SessionWorkspaceMutationAdmission::Closed(_)
        ));
    }
}
