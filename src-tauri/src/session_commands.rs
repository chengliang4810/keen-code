//! 桌面 Session 控制面：直接操作自研 Runtime，并向前端投影标准 ACP 语义。

use crate::agent_runtime::AgentRuntime;
use chrono::{SecondsFormat, TimeZone, Utc};
use keencode_acp::schema::{SessionMode, SessionModeState};
use keencode_resources::ResourceError;
use keencode_runtime::{RuntimeError, RuntimeSession, StoredSessionMetadata};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Tauri 注入的唯一自研 Agent Runtime。
type RuntimeState<'a> = State<'a, Arc<AgentRuntime>>;

/// Tauri 注入的后端诊断记录器。
type DiagnosticsState<'a> = State<'a, Arc<crate::diagnostics::Diagnostics>>;

/// 自研 Runtime 向现有工作台暴露的无旧协议 Session 快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    /// 当前投影对应的 Session；空闲快照为空。
    pub session_id: Option<String>,
    /// 当前桌面连接状态。
    pub state: &'static str,
    /// 当前唯一运行中的根 Turn；子 Agent Turn 不占用此字段。
    pub active_turn_id: Option<String>,
    /// 当前唯一进程内协议后端。
    pub backend: &'static str,
    /// Session 绑定的项目目录。
    pub project_path: Option<String>,
    /// Session 当前标题。
    pub title: Option<String>,
    /// 最近一次桌面控制错误；权威 Runtime 不把临时错误混入 Session 状态。
    pub last_error: Option<String>,
    /// 后端诊断日志绝对路径。
    pub diagnostics_path: Option<String>,
}

/// Plan 模式的模型可见合同；真正的只读边界仍由 Runtime PlanGuard 强制执行。
pub(crate) const PLAN_MODE_CONTRACT_EN: &str = "\
## Plan Mode Contract

This session is in Plan Mode. Research the codebase and produce an implementation plan only.

1. Do not use any tool that can modify files, execute side effects, change configuration, or mutate external state.
2. Read-only sub-agents may be used for independent research when available; they must remain read-only as well.
3. Return one concrete plan containing the goal, ordered steps, critical files, risks, and verification.
4. Remind the user to turn Plan Mode off before implementation.";

/// Ultra 模式的模型可见合同；语义与单层异步 Agent V2 生命周期保持一致。
pub(crate) const ULTRA_MODE_CONTRACT_EN: &str = "\
## Ultra Mode Contract

Ultra Mode is enabled for this turn. Proactively delegate independent work when doing so materially improves speed or quality.

1. Keep every delegated task aligned with the active Goal and include the relevant constraints in its prompt.
2. Use only the single-level Agent tree. Compare available agent descriptions before choosing a specialist.
3. Agent turns are asynchronous. A parent turn may finish while a child continues; child completion is queued in the parent mailbox and does not automatically start a new parent turn.
4. Use spawn_agent to create a child, list_agents to inspect known children, send_message for queue-only delivery, followup_task to start a later turn on an idle child, interrupt_agent to stop only the child's current turn, and wait_agent only when the current turn actually depends on mailbox activity.
5. Resolve conflicting results before presenting a conclusion. In Plan Mode, every parent and child remains read-only.";

/// 将任意内部错误转换为不包含请求正文的 Tauri 文本错误。
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

/// 把持久毫秒时间转换为稳定 UTC RFC 3339 文本。
pub(crate) fn rfc3339_from_ms(value: u64) -> Result<String, String> {
    let value = i64::try_from(value).map_err(|_| "Session 更新时间超出支持范围".to_owned())?;
    Utc.timestamp_millis_opt(value)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| "Session 更新时间无效".to_owned())
}

/// 根据当前项目登记表授权持久 Session 的规范项目目录。
pub(crate) fn authorize_stored_root(app: &AppHandle, stored_root: &str) -> Result<PathBuf, String> {
    let canonical = crate::workspace::canonical_session_root(stored_root)?;
    let app_data_root = crate::workspace::app_data_session_root(app)?;
    if canonical == app_data_root {
        return Ok(canonical);
    }
    let registered = crate::workspace::registered_project_root(app, stored_root)?;
    if canonical != registered {
        return Err("Session 项目目录与当前授权目录不一致".to_owned());
    }
    Ok(registered)
}

/// 查找并授权一个健康的新格式持久 Session。
pub(crate) fn authorized_metadata(
    runtime: &AgentRuntime,
    app: &AppHandle,
    session_id: &str,
) -> Result<(StoredSessionMetadata, PathBuf), String> {
    required_identifier(session_id, "sessionId")?;
    let metadata = runtime
        .stored_sessions()
        .map_err(runtime_error)?
        .into_iter()
        .find(|item| item.session_id.as_str() == session_id)
        .ok_or_else(|| format!("找不到 Session {session_id}"))?;
    if metadata.corrupt {
        return Err(format!("Session {session_id} 的权威日志已损坏"));
    }
    let root = authorize_stored_root(app, &metadata.project_root)?;
    Ok((metadata, root))
}

/// 打开一个已经通过当前项目登记表授权的 Session。
pub(crate) fn open_authorized_session(
    runtime: &AgentRuntime,
    app: &AppHandle,
    session_id: &str,
) -> Result<RuntimeSession, String> {
    let (_, root) = authorized_metadata(runtime, app, session_id)?;
    runtime
        .open_or_create_session(&root, Some(session_id), "session-open")
        .map_err(runtime_error)
}

/// 临时关闭 Session Runtime 后用于恢复桌面连接的最小上下文。
pub(crate) struct ClosedSessionMutationContext {
    /// Session 绑定且已经通过项目登记表授权的规范根目录。
    pub(crate) project_root: PathBuf,
    /// 变更前 Session 是否是桌面当前焦点。
    was_focused: bool,
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

/// 确认 Session 没有活动工作，停止投递并释放资源层独占 lease。
pub(crate) async fn close_session_for_mutation(
    runtime: &Arc<AgentRuntime>,
    app: &AppHandle,
    session_id: &str,
) -> Result<ClosedSessionMutationContext, String> {
    let (_, project_root) = authorized_metadata(runtime, app, session_id)?;
    let session = runtime
        .open_or_create_session(&project_root, Some(session_id), "session-mutation-open")
        .map_err(runtime_error)?;
    if runtime
        .session_has_active_work(session_id)
        .map_err(runtime_error)?
    {
        return Err("运行中的对话不能复制或编辑，请先停止任务".to_owned());
    }
    let was_focused = runtime
        .focused_session_id()
        .map_err(runtime_error)?
        .as_deref()
        == Some(session_id);
    drop(session);
    if let Err(error) = runtime.close_session(session_id).await {
        let restore = runtime
            .ensure_session_delivery(session_id)
            .map(|_| ())
            .map_err(runtime_error);
        if was_focused {
            let _ = runtime.focus_session(session_id);
        }
        return match restore {
            Ok(()) => Err(runtime_error(error)),
            Err(restore_error) => Err(format!(
                "关闭 Session 失败：{}；恢复投递也失败：{restore_error}",
                runtime_error(error)
            )),
        };
    }
    // 投递泵收到取消后在下一次调度点释放最后一个 RuntimeSession 共享句柄。
    tokio::task::yield_now().await;
    Ok(ClosedSessionMutationContext {
        project_root,
        was_focused,
    })
}

/// 无论资源事务成功或失败，都重新打开源 Session 并恢复投递与原焦点。
pub(crate) fn restore_session_after_mutation(
    runtime: &Arc<AgentRuntime>,
    session_id: &str,
    context: &ClosedSessionMutationContext,
) -> Result<(), String> {
    runtime
        .open_or_create_session(
            &context.project_root,
            Some(session_id),
            "session-mutation-restore",
        )
        .map_err(runtime_error)?;
    runtime
        .ensure_session_delivery(session_id)
        .map_err(runtime_error)?;
    if context.was_focused {
        runtime.focus_session(session_id).map_err(runtime_error)?;
    }
    Ok(())
}

/// 创建当前没有桌面焦点时使用的空闲快照。
fn idle_session_snapshot(diagnostics_path: &Path) -> SessionSnapshot {
    SessionSnapshot {
        session_id: None,
        state: "idle",
        active_turn_id: None,
        backend: "acp",
        project_path: None,
        title: None,
        last_error: None,
        diagnostics_path: Some(diagnostics_path.to_string_lossy().into_owned()),
    }
}

/// 返回 Plan 模式当前值对应的标准 ACP 状态。
pub(crate) fn session_mode_state(plan_enabled: bool) -> SessionModeState {
    SessionModeState::new(
        if plan_enabled { "plan" } else { "default" },
        vec![
            SessionMode::new("default", "Default"),
            SessionMode::new("plan", "Plan"),
        ],
    )
}

/// 清除桌面焦点；该赋值天然幂等且不会关闭或取消任何 Session。
#[tauri::command]
pub fn session_disconnect(
    runtime: RuntimeState<'_>,
    diagnostics: DiagnosticsState<'_>,
) -> SessionSnapshot {
    runtime.clear_focus();
    idle_session_snapshot(diagnostics.path())
}

#[cfg(test)]
mod tests {
    use super::required_identifier;

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
}
