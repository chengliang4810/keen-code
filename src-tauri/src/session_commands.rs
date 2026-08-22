//! 会话命令层：把前端 session_* invoke 映射为 ACP JSON-RPC。
//!
//! 命令直接暴露当前 peri ACP 会话能力，不维护第二套会话协议。

use crate::peri_runtime::{PeriRuntime, RuntimeSession, SessionState, SessionStopAction};
use anyhow::{Context, Result};
use peri_agent::thread::ThreadMeta;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Tauri 注入的共享 peri 运行时状态。
type RuntimeState<'a> = State<'a, Arc<PeriRuntime>>;

/// 侧栏使用的 peri Session 元数据。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListItem {
    /// peri ThreadStore 分配的唯一标识。
    pub id: String,
    /// peri 保存的标题；尚未生成标题时为空。
    pub title: Option<String>,
    /// 创建 Session 时使用的工作目录。
    pub cwd: String,
    /// peri 记录的最近更新时间，使用 RFC 3339 文本。
    pub updated_at: String,
}

/// Host 已接受本轮消息的即时响应；快照字段保持 session_send 现有平铺形状。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSendAccepted {
    #[serde(flatten)]
    pub snapshot: crate::peri_runtime::SessionSnapshot,
    /// Host 完成同步校验并切换 Streaming 的 Unix 毫秒时间。
    pub accepted_at_ms: u64,
}

/// 将后端错误转换为 Tauri 命令的文本错误。
fn runtime_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// 严格校验可选文本参数；缺失与空字符串具有不同语义。
fn optional_non_empty(value: Option<String>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(value) if value.trim().is_empty() => Err(format!("{field} 不能为空")),
        value => Ok(value),
    }
}

/// 已通过 ThreadStore 与当前项目登记表校验的 Session 元数据。
struct AuthorizedSession {
    /// Session 唯一标识。
    session_id: String,
    /// 当前授权且规范化的工作目录。
    cwd: String,
    /// ThreadStore 当前保存的标题。
    title: Option<String>,
}

/// 严格校验必填 Session 标识。
fn required_session_id<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    if value.trim().is_empty() {
        Err(format!("{field} 不能为空"))
    } else if value.trim() != value {
        Err(format!("{field} 不能包含首尾空白"))
    } else {
        Ok(value)
    }
}

/// 严格校验 prompt 回合请求标识，避免空值或隐式裁剪破坏终态配对。
fn required_request_id(value: &str) -> Result<&str, String> {
    if value.trim().is_empty() {
        Err("requestId 不能为空".to_owned())
    } else if value.trim() != value {
        Err("requestId 不能包含首尾空白".to_owned())
    } else {
        Ok(value)
    }
}

/// 严格校验 MCP Server 名称，避免空值或隐式裁剪后命中另一配置。
fn required_mcp_server_name(value: &str) -> Result<&str, String> {
    if value.trim().is_empty() {
        Err("serverName 不能为空".to_owned())
    } else if value.trim() != value {
        Err("serverName 不能包含首尾空白".to_owned())
    } else {
        Ok(value)
    }
}

/// 校验持久 Session 的工作目录与本次已授权目录规范化后完全一致。
fn require_matching_session_root(stored_cwd: &str, authorized_root: &Path) -> Result<(), String> {
    let stored_root = crate::workspace::canonical_session_root(stored_cwd)?;
    if stored_root != authorized_root {
        return Err("Session 工作目录与当前授权目录不一致".to_owned());
    }
    Ok(())
}

/// 校验请求标识只能命中可见的根 Session，禁止操作隐藏子 Agent Thread。
fn require_root_session_metadata(metadata: &ThreadMeta, requested_id: &str) -> Result<(), String> {
    if metadata.id != requested_id {
        return Err("Session 元数据标识与请求不一致".to_owned());
    }
    if metadata.hidden || metadata.parent_thread_id.is_some() {
        return Err("目标标识不是可操作的根 Session".to_owned());
    }
    Ok(())
}

/// 加载现有 Session 前校验其元数据与当前授权目录。
async fn authorize_stored_session(
    runtime: &PeriRuntime,
    app: &AppHandle,
    session_id: &str,
    expected_root: Option<&Path>,
) -> Result<AuthorizedSession, String> {
    required_session_id(session_id, "sessionId")?;
    let metadata = runtime
        .thread_store
        .load_meta(&session_id.to_owned())
        .await
        .map_err(|error| format!("无法读取 Session {session_id} 元数据：{error:#}"))?;
    require_root_session_metadata(&metadata, session_id)?;
    let authorized_root = match expected_root {
        Some(root) => {
            require_matching_session_root(&metadata.cwd, root)?;
            root.to_path_buf()
        }
        None => authorize_stored_root(app, &metadata.cwd)?,
    };
    if let Some(registered) = runtime.session(session_id) {
        require_matching_session_root(&registered.cwd, &authorized_root)?;
    }
    let cwd = authorized_root
        .to_str()
        .ok_or_else(|| "Session 工作目录必须是 UTF-8 路径".to_owned())?
        .to_owned();
    Ok(AuthorizedSession {
        session_id: metadata.id,
        cwd,
        title: metadata.title,
    })
}

/// 根据当前项目登记表授权持久 Session 的工作目录。
fn authorize_stored_root(app: &AppHandle, stored_cwd: &str) -> Result<PathBuf, String> {
    let stored_root = crate::workspace::canonical_session_root(stored_cwd)?;
    let app_data_root = crate::workspace::app_data_session_root(app)?;
    if stored_root == app_data_root {
        return Ok(stored_root);
    }
    let registered_root = crate::workspace::registered_project_root(app, stored_cwd)?;
    require_matching_session_root(stored_cwd, &registered_root)?;
    Ok(registered_root)
}

/// 同步一个已授权 Session 的持久元数据，不改变其前后台运行状态。
fn sync_authorized_session(
    runtime: &PeriRuntime,
    session: &AuthorizedSession,
) -> Result<(), String> {
    runtime
        .sync_session_metadata(
            session.session_id.clone(),
            session.cwd.clone(),
            session.title.clone(),
        )
        .map_err(runtime_error)
}

/// 确保指定 Session 已经加载进 ACP server，焦点状态不参与判断。
async fn ensure_loaded_session(
    runtime: &PeriRuntime,
    app: &AppHandle,
    session_id: &str,
) -> Result<AuthorizedSession, String> {
    let session = authorize_stored_session(runtime, app, session_id, None).await?;
    sync_authorized_session(runtime, &session)?;
    if runtime
        .session(session_id)
        .is_some_and(|registered| registered.is_loaded())
    {
        return Ok(session);
    }
    runtime.register_session(RuntimeSession::new(
        session.session_id.clone(),
        session.cwd.clone(),
        session.title.clone(),
        SessionState::Connecting,
        false,
    ));
    let result = runtime
        .send_request(
            "session/load",
            json!({ "sessionId": &session.session_id, "cwd": &session.cwd }),
        )
        .await;
    match result {
        Ok(_) => {
            runtime
                .set_session_loaded(session_id, true)
                .map_err(runtime_error)?;
            runtime
                .set_session_state(session_id, SessionState::Ready)
                .map_err(runtime_error)?;
            runtime
                .set_session_error(session_id, None)
                .map_err(runtime_error)?;
            Ok(session)
        }
        Err(error) => {
            let message = error.to_string();
            let _ = runtime.set_session_state(session_id, SessionState::Disconnected);
            let _ = runtime.set_session_error(session_id, Some(message.clone()));
            Err(message)
        }
    }
}

/// 校验一个必须已加载的运行中 Session。
async fn authorize_loaded_session(
    runtime: &PeriRuntime,
    app: &AppHandle,
    session_id: &str,
) -> Result<AuthorizedSession, String> {
    let session = authorize_stored_session(runtime, app, session_id, None).await?;
    let registered = runtime
        .session(session_id)
        .ok_or_else(|| format!("Session 尚未连接：{session_id}"))?;
    if !registered.is_loaded() {
        return Err(format!("Session 尚未加载：{session_id}"));
    }
    Ok(session)
}

/// 将取消通知结果转换为 Tauri 命令结果，禁止吞掉传输失败。
fn require_cancel_notification(result: anyhow::Result<()>) -> Result<(), String> {
    result
        .context("发送 session/cancel 通知失败")
        .map_err(|error| format!("{error:#}"))
}

/// 按 Peri Host 契约构造精确绑定 Session 与 Task 的后台取消参数。
fn background_task_cancel_params(session_id: &str, task_id: &str) -> Value {
    json!({ "sessionId": session_id, "taskId": task_id })
}

/// 按 peri `session/prompt` 契约构造带回合标识的单条文本消息参数。
fn prompt_params(
    session_id: &str,
    text: String,
    request_id: &str,
    developer_context: Option<String>,
) -> Value {
    json!({
        "sessionId": session_id,
        "requestId": request_id,
        "message": {
            "content": text,
        },
        "developerContext": developer_context,
    })
}

/// 按标准 ACP `session/delete` 契约构造删除参数。
fn session_delete_params(session_id: &str) -> Value {
    json!({ "sessionId": session_id })
}

/// 按当前唯一问答契约构造响应，拒绝未知决策值。
fn elicitation_outcome(decision: &str, answers: Option<Value>) -> Result<Value, String> {
    match decision {
        "accepted" => {
            Ok(json!({ "action": { "accept": { "content": answers.unwrap_or(json!({})) } } }))
        }
        "cancelled" => Ok(json!({ "action": "cancel" })),
        _ => Err(format!("未知 elicitation decision：{decision}")),
    }
}

/// 当前焦点会话与全部活跃 turn 的一致快照（session_get_state）。
#[tauri::command]
pub fn session_get_state(runtime: RuntimeState<'_>) -> crate::peri_runtime::RuntimeStateSnapshot {
    runtime.log("info", "ipc.session_get_state", "命令进入");
    runtime.runtime_state_snapshot()
}

/// 返回 Peri MCP 连接池当前快照；查询本身不会启动连接或子进程。
#[tauri::command]
pub async fn mcp_list(runtime: RuntimeState<'_>) -> Result<Value, String> {
    runtime
        .send_request("mcp/list", json!({}))
        .await
        .map_err(runtime_error)
}

/// 显式启动指定 MCP Server 的 OAuth 授权流程。
#[tauri::command]
pub async fn mcp_oauth_start(
    server_name: String,
    runtime: RuntimeState<'_>,
) -> Result<Value, String> {
    let server_name = required_mcp_server_name(&server_name)?;
    runtime
        .send_request("mcp/oauth_start", json!({ "server_name": server_name }))
        .await
        .map_err(runtime_error)
}

/// 将宿主收到的 OAuth 授权码回传给指定 MCP Server。
#[tauri::command]
pub async fn mcp_oauth_callback(
    server_name: String,
    code: String,
    state: String,
    runtime: RuntimeState<'_>,
) -> Result<Value, String> {
    let server_name = required_mcp_server_name(&server_name)?;
    runtime
        .send_request(
            "mcp/oauth_callback",
            json!({
                "server_name": server_name,
                "code": code,
                "state": state,
            }),
        )
        .await
        .map_err(runtime_error)
}

/// 取消指定 MCP Server 尚未完成的 OAuth 授权回传。
#[tauri::command]
pub async fn mcp_oauth_cancel(
    server_name: String,
    runtime: RuntimeState<'_>,
) -> Result<Value, String> {
    let server_name = required_mcp_server_name(&server_name)?;
    runtime
        .send_request("mcp/oauth_cancel", json!({ "server_name": server_name }))
        .await
        .map_err(runtime_error)
}

/// 列出所有 Session 当前仍在运行的后台任务。
#[tauri::command]
pub fn background_tasks_list(
    runtime: RuntimeState<'_>,
) -> Vec<crate::peri_runtime::BackgroundTaskInfo> {
    runtime.background_tasks()
}

/// 通过 Peri Host 公共 RPC 精确取消一个后台任务。
#[tauri::command]
pub async fn background_task_cancel(
    session_id: String,
    task_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    authorize_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/cancel-bg-task",
            background_task_cancel_params(&session_id, &task_id),
        )
        .await
        .map(|_| ())
        .map_err(runtime_error)
}

/// 通过 Peri Host 公共 RPC 取消当前登记的全部后台任务。
#[tauri::command]
pub async fn background_tasks_cancel_all(
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    let tasks = runtime.background_tasks();
    for task in tasks {
        authorize_loaded_session(runtime.inner().as_ref(), &app, &task.session_id).await?;
        runtime
            .send_request(
                "session/cancel-bg-task",
                background_task_cancel_params(&task.session_id, &task.task_id),
            )
            .await
            .map(|_| ())
            .map_err(runtime_error)?;
    }
    Ok(())
}

/// 连接会话：无 sessionId → 新建；有 → 加载。
#[tauri::command]
pub async fn session_connect(
    project_path: Option<String>,
    session_id: Option<String>,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<crate::peri_runtime::SessionSnapshot, String> {
    let project_path = optional_non_empty(project_path, "projectPath")?;
    let session_id = optional_non_empty(session_id, "sessionId")?;
    runtime.log(
        "info",
        "ipc.session_connect",
        format!("命令进入 session_id_present={}", session_id.is_some()),
    );
    let authorized_root = crate::workspace::authorized_session_root(&app, project_path.as_deref())?;
    let cwd = authorized_root
        .to_str()
        .ok_or_else(|| "Session 工作目录必须是 UTF-8 路径".to_owned())?
        .to_owned();
    if let Some(session_id) = session_id {
        let stored = authorize_stored_session(
            runtime.inner().as_ref(),
            &app,
            &session_id,
            Some(&authorized_root),
        )
        .await?;
        if runtime
            .session(&session_id)
            .is_some_and(|session| session.is_loaded())
        {
            sync_authorized_session(runtime.inner().as_ref(), &stored)?;
            runtime.focus_session(&session_id).map_err(runtime_error)?;
            return runtime.snapshot_for(&session_id).map_err(runtime_error);
        }
        runtime.register_session(RuntimeSession::new(
            stored.session_id.clone(),
            stored.cwd.clone(),
            stored.title.clone(),
            SessionState::Connecting,
            false,
        ));
        runtime.focus_session(&session_id).map_err(runtime_error)?;
        let result = runtime
            .send_request(
                "session/load",
                json!({ "sessionId": &session_id, "cwd": &cwd }),
            )
            .await;
        return match result {
            Ok(_) => {
                runtime
                    .set_session_loaded(&session_id, true)
                    .map_err(runtime_error)?;
                runtime
                    .set_session_state(&session_id, SessionState::Ready)
                    .map_err(runtime_error)?;
                runtime
                    .set_session_error(&session_id, None)
                    .map_err(runtime_error)?;
                runtime.snapshot_for(&session_id).map_err(runtime_error)
            }
            Err(error) => {
                runtime.log(
                    "error",
                    "ipc.session_connect",
                    format!("命令失败 session_id={session_id}: {error:#}"),
                );
                let _ = runtime.set_session_state(&session_id, SessionState::Disconnected);
                let _ = runtime.set_session_error(&session_id, Some(error.to_string()));
                Err(runtime_error(error))
            }
        };
    }

    let result = runtime
        // 空占位标题阻止 peri ThreadStore 直接截取首条用户消息；
        // 首轮成功后由 KeenCode 的独立标题模型生成语义化短标题。
        .send_request("session/new", json!({ "cwd": &cwd, "title": "" }))
        .await
        .and_then(|response| {
            response
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("session/new 响应缺少 sessionId")
        });
    let session_id = match result {
        Ok(session_id) => session_id,
        Err(error) => {
            runtime.log(
                "error",
                "ipc.session_connect",
                format!("命令失败: {error:#}"),
            );
            return Err(runtime_error(error));
        }
    };
    let stored = authorize_stored_session(
        runtime.inner().as_ref(),
        &app,
        &session_id,
        Some(&authorized_root),
    )
    .await?;
    runtime.register_session(RuntimeSession::new(
        stored.session_id.clone(),
        stored.cwd,
        stored.title,
        SessionState::Ready,
        true,
    ));
    runtime.focus_session(&session_id).map_err(runtime_error)?;
    runtime.snapshot_for(&session_id).map_err(runtime_error)
}

/// Plan Mode contract injected into `developerContext`, requiring the main
/// agent to perform read-only research and delegate implementation planning
/// to the built-in `plan` subagent. The subagent's `disallowedTools` enforce
/// the hard read-only boundary.
const PLAN_MODE_CONTRACT_EN: &str = "\
## Plan Mode Contract

This session is in Plan Mode: the user wants research and an implementation plan, not direct code changes. You must:

1. Never invoke Write, Edit, folder_operations, Bash or any other side-effecting tool yourself; only read-only exploration (Read, Grep, Glob) is allowed.
2. For deep codebase research, delegate to the built-in read-only `plan` subagent (subagent_type=\"plan\"), which explores the codebase and may save the plan into its host-managed sandbox via SandboxWrite (outside the project).
3. Present a structured implementation plan in your final reply: goal, steps, critical files, risks, and verification; if the subagent saved a plan file, mention its path.
4. End by reminding the user to turn Plan Mode off before asking you to implement the plan.";

fn plan_mode_contract() -> &'static str {
    PLAN_MODE_CONTRACT_EN
}

/// Host 接受一条用户消息并立即返回；模型回合在后台运行并经现有 ACP 事件收口。
#[tauri::command]
pub async fn session_send(
    text: String,
    session_id: String,
    request_id: String,
    plan_mode: Option<bool>,
    runtime: RuntimeState<'_>,
    memories: State<'_, Arc<crate::memories::MemoryService>>,
    app: AppHandle,
) -> Result<SessionSendAccepted, String> {
    required_request_id(&request_id)?;
    runtime.log(
        "info",
        "ipc.session_send",
        format!("命令进入 session_id={} text_len={}", session_id, text.len()),
    );
    ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    if let Err(error) = runtime.ensure_provider_configured() {
        let _ = runtime.set_session_error(&session_id, Some(error.to_string()));
        return Err(runtime_error(error));
    }
    let turn_id = runtime
        .begin_session_turn(&session_id, request_id)
        .map_err(runtime_error)?;
    let accepted_at_ms = crate::analytics::now_ms();
    let snapshot = runtime.snapshot_for(&session_id).map_err(runtime_error)?;

    let runtime = runtime.inner().clone();
    let memories = memories.inner().clone();
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let result: Result<Option<(bool, crate::app_settings::InterfaceLanguage)>, String> =
            async {
                let settings = crate::app_settings::get(&app_for_task).map_err(runtime_error)?;
                let memory_context = memories
                    .prompt_context(settings.local_memories, settings.interface_language)
                    .map_err(runtime_error)?;
                let custom_instructions =
                    crate::personalization::get(&app_for_task).map_err(runtime_error)?;
                let developer_context = [
                    (!custom_instructions.trim().is_empty()).then(|| {
                        format!(
                            "## Global Custom Instructions\n\n{}",
                            custom_instructions.trim()
                        )
                    }),
                    memory_context,
                    plan_mode
                        .unwrap_or(false)
                        .then(|| plan_mode_contract().to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n\n");
                if !runtime
                    .prepare_session_prompt_dispatch(&session_id, &turn_id)
                    .await
                    .map_err(runtime_error)?
                {
                    return Ok(None);
                }
                runtime
                    .send_request(
                        "session/prompt",
                        prompt_params(
                            &session_id,
                            text,
                            &turn_id,
                            (!developer_context.is_empty()).then_some(developer_context),
                        ),
                    )
                    .await
                    .map_err(runtime_error)?;
                Ok(Some((settings.local_memories, settings.interface_language)))
            }
            .await;
        match result {
            Ok(Some((local_memories, interface_language))) => {
                if local_memories {
                    memories.trigger(runtime.clone(), Some(session_id), interface_language, false);
                }
            }
            Ok(None) => {}
            Err(error) => {
                let message = error;
                runtime.log(
                    "error",
                    "ipc.session_send",
                    format!("后台请求失败 session_id={session_id}: {message}"),
                );
                if let Ok(Some(finished_turn_id)) =
                    runtime.finish_session_turn(&session_id, Some(&turn_id), Some(message.clone()))
                {
                    let completed_at_ms = crate::analytics::now_ms();
                    runtime.emit_prompt_failure(
                        &app_for_task,
                        &session_id,
                        &finished_turn_id,
                        &message,
                        completed_at_ms,
                    );
                }
            }
        }
    });

    Ok(SessionSendAccepted {
        snapshot,
        accepted_at_ms,
    })
}

/// 将一条用户消息注入当前正在运行的回合，不中断已有工具或模型调用。
#[tauri::command]
pub async fn session_steer(
    text: String,
    session_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    authorize_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    if text.trim().is_empty() {
        return Err("引导消息不能为空".to_string());
    }
    runtime
        .send_request(
            "session/steer",
            json!({ "sessionId": session_id, "text": text }),
        )
        .await
        .map(|_| ())
        .map_err(runtime_error)
}

/// 停止当前回合，并拒绝该回合尚未回答的问题。
#[tauri::command]
pub async fn session_stop(
    session_id: String,
    request_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<crate::peri_runtime::SessionSnapshot, String> {
    required_request_id(&request_id)?;
    authorize_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    match runtime
        .request_session_stop(&session_id, &request_id)
        .map_err(runtime_error)?
    {
        SessionStopAction::CompleteLocally(finished_turn_id) => {
            let completed_at_ms = crate::analytics::now_ms();
            runtime.emit_local_turn_done(
                &app,
                &session_id,
                &finished_turn_id,
                "cancelled",
                completed_at_ms,
            );
        }
        SessionStopAction::NotifyRuntime(active_turn_id) => {
            runtime.cancel_pending_for(&session_id).await;
            let cancel_result = runtime
                .send_notification("session/cancel", json!({ "sessionId": &session_id }))
                .await;
            if let Err(error) = require_cancel_notification(cancel_result) {
                let _ = runtime.rollback_session_stop_request(&session_id, &active_turn_id);
                runtime.log("error", "ipc.session_stop", &error);
                let _ = runtime.set_session_error(&session_id, Some(error.clone()));
                return Err(error);
            }
        }
        SessionStopAction::AlreadyRequested(_) => {}
    }
    // 已派发 turn 的 cancel 只表示请求已送达；继续保留 Streaming，直到带同一
    // requestId 的 Peri done 到达。尚未派发的 turn 已在上方由 Host 单次本地收口。
    runtime.snapshot_for(&session_id).map_err(runtime_error)
}

/// 分叉会话。
#[tauri::command]
pub async fn session_fork(
    source_id: String,
    title: Option<String>,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    required_session_id(&source_id, "sourceId")?;
    let source = ensure_loaded_session(runtime.inner().as_ref(), &app, &source_id).await?;
    let title = optional_non_empty(title, "title")?;
    let result = runtime
        .send_request(
            "session/fork",
            json!({ "sessionId": &source_id, "cwd": &source.cwd }),
        )
        .await
        .map_err(runtime_error)?;
    let new_id = result
        .get("sessionId")
        .and_then(|v| v.as_str())
        .context("session/fork 响应缺少 sessionId")
        .map_err(runtime_error)?
        .to_string();
    let forked = authorize_stored_session(
        runtime.inner().as_ref(),
        &app,
        &new_id,
        Some(Path::new(&source.cwd)),
    )
    .await?;
    runtime.register_session(RuntimeSession::new(
        forked.session_id,
        forked.cwd,
        forked.title,
        SessionState::Ready,
        true,
    ));
    if let Some(title) = title {
        runtime
            .send_request(
                "session/rename",
                json!({ "sessionId": &new_id, "title": &title }),
            )
            .await
            .map_err(runtime_error)?;
        runtime
            .set_session_title(&new_id, title)
            .map_err(runtime_error)?;
    }
    Ok(json!({ "id": new_id }))
}

/// 重命名会话。
#[tauri::command]
pub async fn session_rename(
    id: String,
    title: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<crate::peri_runtime::SessionSnapshot, String> {
    required_session_id(&id, "id")?;
    let stored = authorize_stored_session(runtime.inner().as_ref(), &app, &id, None).await?;
    sync_authorized_session(runtime.inner().as_ref(), &stored)?;
    runtime
        .send_request(
            "session/rename",
            json!({ "sessionId": &id, "title": &title }),
        )
        .await
        .map_err(runtime_error)?;
    runtime
        .set_session_title(&id, title)
        .map_err(runtime_error)?;
    runtime.snapshot_for(&id).map_err(runtime_error)
}

/// 切换会话级模型（Q1 决策：每会话独立 provider）。
///
/// 值编码为 `"{provider_id}::{model}"`，仅影响当前 session 的 provider，
/// 不改动"新会话默认值"（`cfg.provider`）。provider_id 或
/// model_id 无效（含已删除）时返回错误，与"删除后回退会话 provider"的
/// Q2 语义一致——前端选择已删除模型会立即看到错误而非静默忽略。
#[tauri::command]
pub async fn session_set_model(
    session_id: String,
    provider_id: String,
    model_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    required_session_id(&session_id, "sessionId")?;
    required_session_id(&provider_id, "providerId")?;
    required_session_id(&model_id, "modelId")?;
    authorize_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    let listed = crate::providers::list(&app).map_err(runtime_error)?;
    let provider = listed
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| format!("找不到供应商 {provider_id}"))?;
    if !provider.models.iter().any(|model| model == &model_id) {
        return Err(format!("供应商 {provider_id} 中找不到模型 {model_id}"));
    }
    if provider.api_key.as_deref().is_none_or(str::is_empty) {
        return Err(format!("供应商 {provider_id} 未配置 API Key"));
    }
    runtime.log(
        "info",
        "ipc.session_set_model",
        format!("命令进入 session_id={session_id} provider_id={provider_id} model_id={model_id}"),
    );
    runtime
        .send_request(
            "session/set_config_option",
            json!({
                "sessionId": &session_id,
                "configId": "model",
                "value": format!("{provider_id}::{model_id}"),
            }),
        )
        .await
        .map_err(runtime_error)?;
    runtime.log(
        "info",
        "ipc.session_set_model",
        format!("命令完成 session_id={session_id}"),
    );
    Ok(())
}

/// 切换会话级推理强度。
#[tauri::command]
pub async fn session_set_effort(
    session_id: String,
    effort: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    required_session_id(&session_id, "sessionId")?;
    if !matches!(
        effort.as_str(),
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    ) {
        return Err(format!("不支持的推理强度 {effort}"));
    }
    authorize_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/set_config_option",
            json!({
                "sessionId": &session_id,
                "configId": "thinking_effort",
                "value": &effort,
            }),
        )
        .await
        .map_err(runtime_error)?;
    Ok(())
}

/// 使用当前供应商对首条用户消息生成语义化短标题，不写入主对话历史。
#[tauri::command]
pub async fn session_generate_title(
    id: String,
    user_message: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<String, String> {
    required_session_id(&id, "id")?;
    ensure_loaded_session(runtime.inner().as_ref(), &app, &id).await?;
    runtime
        .ensure_provider_configured()
        .map_err(runtime_error)?;
    runtime.log(
        "info",
        "ipc.session_generate_title",
        format!("命令进入 session_id={} user_len={}", id, user_message.len()),
    );
    let result = runtime
        .send_request(
            "peri/session-title",
            json!({
                "sessionId": id,
                "userMessage": user_message,
            }),
        )
        .await
        .map_err(runtime_error)?;
    let title = result
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| "标题生成响应缺少 title".to_string())?
        .to_string();
    runtime.log(
        "info",
        "ipc.session_generate_title",
        format!("命令完成 session_id={} title_len={}", id, title.len()),
    );
    Ok(title)
}

/// 读取会话消息（本地 ThreadStore）。
#[tauri::command]
pub async fn session_messages(
    id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Vec<Value>, String> {
    required_session_id(&id, "id")?;
    let stored = authorize_stored_session(runtime.inner().as_ref(), &app, &id, None).await?;
    sync_authorized_session(runtime.inner().as_ref(), &stored)?;
    let messages = runtime
        .thread_store
        .load_messages(&id)
        .await
        .map_err(runtime_error)?;
    messages
        .iter()
        .map(|m| serde_json::to_value(m).map_err(|e| e.to_string()))
        .collect()
}

/// 通过标准 ACP 清理链永久删除已授权且未运行的根 Session。
#[tauri::command]
pub async fn session_delete(
    id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<(), String> {
    required_session_id(&id, "id")?;
    authorize_stored_session(runtime.inner().as_ref(), &app, &id, None).await?;
    if runtime.session(&id).is_some_and(|session| {
        matches!(
            session.state,
            SessionState::Connecting | SessionState::Streaming
        )
    }) {
        return Err("运行中的对话不能删除，请先停止任务".to_owned());
    }
    runtime
        .send_request("session/delete", session_delete_params(&id))
        .await
        .map_err(|error| format!("永久删除 Session {id} 失败：{error:#}"))?;
    // ACP Host 已负责取消令牌、SessionManager、LSP pool 与 ThreadStore；
    // 这里只移除 KeenCode 自身的只读界面投影和待回答请求。
    runtime.forget_session(&id);
    runtime.log(
        "info",
        "ipc.session_delete",
        format!("已永久删除 session_id={id}"),
    );
    Ok(())
}

/// 断开会话（清理本地会话绑定，保持进程内运行时 idle）。
#[tauri::command]
pub async fn session_disconnect(
    runtime: RuntimeState<'_>,
) -> Result<crate::peri_runtime::SessionSnapshot, String> {
    runtime.log("info", "ipc.session_disconnect", "命令进入");
    // 只清理界面焦点；所有前后台 Session 与 ACP transport 继续运行。
    runtime.clear_focus();
    runtime.log(
        "info",
        "ipc.session_disconnect",
        "命令完成，运行时保持 idle",
    );
    Ok(runtime.snapshot())
}

/// elicitation/create 表单应答。
#[tauri::command]
pub async fn session_resolve_ask_user(
    rpc_id: i64,
    decision: String,
    answers: Option<Value>,
    runtime: RuntimeState<'_>,
) -> Result<(), String> {
    let outcome = elicitation_outcome(&decision, answers)?;
    runtime
        .respond_rpc(rpc_id, outcome)
        .await
        .map_err(runtime_error)?;
    Ok(())
}

// ── (KeenCode) Goal / replay 命令 ────────────────────────────────────────────────

/// 查询显式目标 Session 的项目 Goal（session/goal-get）。
#[tauri::command]
pub async fn goals_list(
    session_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/goal-get",
            json!({ "sessionId": session.session_id }),
        )
        .await
        .map_err(runtime_error)
}

/// 创建或更新显式目标 Session 的 Goal（session/goal-upsert）。
#[tauri::command]
pub async fn goal_upsert(
    session_id: String,
    goal: Value,
    expected_revision: Option<u64>,
    request_nonce: Option<String>,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/goal-upsert",
            json!({
                "sessionId": session.session_id,
                "goal": goal,
                "expectedRevision": expected_revision,
                "requestNonce": request_nonce,
            }),
        )
        .await
        .map_err(runtime_error)
}

/// 切换显式目标 Session 的 Goal 状态（session/goal-transition）。
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri IPC 当前字段需保持平铺并与前端命令契约一致。
pub async fn goal_transition(
    session_id: String,
    goal_id: String,
    status: String,
    reason: Option<String>,
    expected_revision: Option<u64>,
    request_nonce: Option<String>,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/goal-transition",
            json!({
                "sessionId": session.session_id,
                "goalId": goal_id,
                "status": status,
                "reason": reason,
                "expectedRevision": expected_revision,
                "requestNonce": request_nonce,
            }),
        )
        .await
        .map_err(runtime_error)
}

/// 清除显式目标 Session 的 Goal（session/goal-clear）。
#[tauri::command]
pub async fn goal_clear(
    session_id: String,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/goal-clear",
            json!({ "sessionId": session.session_id }),
        )
        .await
        .map_err(runtime_error)
}

/// 分页增量重放显式目标 Session（session/replay）。
#[tauri::command]
pub async fn session_replay(
    session_id: String,
    after: Option<Value>,
    limit: Option<u32>,
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = ensure_loaded_session(runtime.inner().as_ref(), &app, &session_id).await?;
    runtime
        .send_request(
            "session/replay",
            json!({
                "sessionId": session.session_id,
                "after": after,
                "limit": limit.unwrap_or(100),
            }),
        )
        .await
        .map_err(runtime_error)
}

#[cfg(test)]
mod tests {
    use super::{
        SessionListItem, SessionSendAccepted, background_task_cancel_params, elicitation_outcome,
        optional_non_empty, plan_mode_contract, prompt_params, require_cancel_notification,
        require_matching_session_root, require_root_session_metadata, required_request_id,
        required_session_id, session_delete_params,
    };
    use crate::peri_runtime::{SessionSnapshot, SessionState};
    use peri_agent::thread::ThreadMeta;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 保证桌面命令使用 peri 真实的 `message.content` 请求结构。
    #[test]
    fn prompt_params_match_peri_contract() {
        let params = prompt_params("session-1", "hello".to_string(), "request-1", None);

        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["requestId"], "request-1");
        assert_eq!(params["message"]["content"], "hello");
        assert!(params.get("prompt").is_none());
    }

    /// Plan Mode always uses the English model-visible contract, regardless of
    /// the interface language used for the final response.
    #[test]
    fn plan_mode_contract_is_english_and_keeps_constraints() {
        let contract = plan_mode_contract();

        assert!(contract.contains("Plan Mode"));
        assert!(contract.contains("plan"));
        assert!(contract.contains("SandboxWrite"));
        assert!(contract.contains("Bash"));
        assert!(contract.contains("turn Plan Mode off"));
        // Plan files must remain in the host-managed sandbox, not the project.
        assert!(!contract.contains(".peri/"));
        assert!(!contract.contains("计划"));
    }

    /// 后台取消 RPC 必须同时精确携带根 Session 与 Task 标识。
    #[test]
    fn background_task_cancel_params_match_host_contract() {
        assert_eq!(
            background_task_cancel_params("session-1", "agent-task-1"),
            json!({ "sessionId": "session-1", "taskId": "agent-task-1" })
        );
    }

    /// 请求标识拒绝空值与隐式裁剪，保证回带后可以做精确相等比较。
    #[test]
    fn request_id_must_be_strictly_non_empty() {
        assert_eq!(required_request_id("request-1").unwrap(), "request-1");
        assert_eq!(required_request_id("  ").unwrap_err(), "requestId 不能为空");
        assert_eq!(
            required_request_id(" request-1").unwrap_err(),
            "requestId 不能包含首尾空白"
        );
    }

    /// 删除命令必须进入标准 ACP 请求面，不能绕过 Host 清理链直删 ThreadStore。
    #[test]
    fn delete_params_match_standard_acp_contract() {
        assert_eq!(
            session_delete_params("session-1"),
            json!({ "sessionId": "session-1" })
        );
    }

    /// session_send 的即时响应保持旧快照平铺字段，并显式携带 Host 接受时间。
    #[test]
    fn send_accepted_serializes_streaming_snapshot_and_timestamp() {
        let accepted = SessionSendAccepted {
            snapshot: SessionSnapshot {
                session_id: Some("session-1".to_owned()),
                state: SessionState::Streaming,
                active_turn_id: Some("turn-1".to_owned()),
                backend: "peri_acp",
                project_path: Some("/tmp/demo".to_owned()),
                title: Some("Demo".to_owned()),
                last_error: None,
                diagnostics_path: "/tmp/keencode.log".to_owned(),
            },
            accepted_at_ms: 1234,
        };

        let value = serde_json::to_value(accepted).unwrap();
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["state"], "streaming");
        assert_eq!(value["activeTurnId"], "turn-1");
        assert_eq!(value["acceptedAtMs"], 1234);
        assert!(value.get("turnId").is_none());
        assert!(value.get("requestId").is_none());
        assert!(value.get("snapshot").is_none());
    }

    /// 问答命令只接受当前前端声明的 accepted 与 cancelled。
    #[test]
    fn elicitation_decision_rejects_unknown_values() {
        assert_eq!(
            elicitation_outcome("cancelled", None).unwrap(),
            json!({"action": "cancel"})
        );
        assert_eq!(
            elicitation_outcome("accepted", Some(json!({"answer": "yes"}))).unwrap(),
            json!({"action": {"accept": {"content": {"answer": "yes"}}}})
        );
        assert!(elicitation_outcome("declined", None).is_err());
    }

    /// 空字符串不得被当作参数缺失并触发另一条执行路径。
    #[test]
    fn optional_text_rejects_empty_values() {
        assert_eq!(optional_non_empty(None, "sessionId").unwrap(), None);
        assert_eq!(
            optional_non_empty(Some("session-1".to_string()), "sessionId").unwrap(),
            Some("session-1".to_string())
        );
        assert_eq!(
            optional_non_empty(Some("  ".to_string()), "sessionId").unwrap_err(),
            "sessionId 不能为空"
        );
    }

    /// Session 标识必须为无首尾空白的显式值，不依赖当前界面焦点。
    #[test]
    fn command_session_id_is_explicit_and_strict() {
        assert_eq!(
            required_session_id("session-1", "sessionId").unwrap(),
            "session-1"
        );
        assert_eq!(
            required_session_id("  ", "sessionId").unwrap_err(),
            "sessionId 不能为空"
        );
        assert_eq!(
            required_session_id(" session-1", "sessionId").unwrap_err(),
            "sessionId 不能包含首尾空白"
        );
    }

    /// 任意 ID 不能借由隐藏子 Agent Thread 绕过根 Session 边界。
    #[test]
    fn command_rejects_non_root_thread_metadata() {
        let mut metadata = ThreadMeta::new("/tmp/project");
        metadata.id = "session-1".to_owned();
        assert!(require_root_session_metadata(&metadata, "session-1").is_ok());
        assert!(require_root_session_metadata(&metadata, "session-2").is_err());

        metadata.hidden = true;
        assert!(require_root_session_metadata(&metadata, "session-1").is_err());
        metadata.hidden = false;
        metadata.parent_thread_id = Some("parent".to_owned());
        assert!(require_root_session_metadata(&metadata, "session-1").is_err());
    }

    /// 加载 Session 时只接受规范化后与授权根目录完全相同的持久工作目录。
    #[test]
    fn stored_session_root_must_match_authorized_root() {
        let base = std::env::temp_dir().join(format!(
            "keencode-session-cwd-auth-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let root = base.join("project");
        let other = base.join("other");
        fs::create_dir_all(&root).expect("create authorized root");
        fs::create_dir_all(&other).expect("create other root");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize authorized root");

        assert!(require_matching_session_root(root.to_str().unwrap(), &canonical_root).is_ok());
        assert_eq!(
            require_matching_session_root(other.to_str().unwrap(), &canonical_root).unwrap_err(),
            "Session 工作目录与当前授权目录不一致"
        );

        fs::remove_dir_all(&base).expect("remove session cwd fixture");
    }

    /// ACP 取消通知的传输失败必须原样转成命令错误。
    #[test]
    fn cancel_notification_failure_is_not_ignored() {
        assert!(require_cancel_notification(Ok(())).is_ok());
        let error =
            require_cancel_notification(Err(anyhow::anyhow!("transport closed"))).unwrap_err();
        assert!(error.contains("发送 session/cancel 通知失败"));
        assert!(error.contains("transport closed"));
    }

    /// Session 列表只能暴露 peri 当前的四个真实字段。
    #[test]
    fn session_list_item_serializes_only_current_peri_fields() {
        let item = SessionListItem {
            id: "session-1".to_string(),
            title: Some("Demo".to_string()),
            cwd: "/tmp/demo".to_string(),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            serde_json::to_value(item).unwrap(),
            json!({
                "id": "session-1",
                "title": "Demo",
                "cwd": "/tmp/demo",
                "updatedAt": "2026-08-01T00:00:00Z",
            })
        );
    }
}

/// 从 peri ThreadStore 返回当前唯一的 Session 元数据结构。
#[tauri::command]
pub async fn sessions_list(
    runtime: RuntimeState<'_>,
    app: AppHandle,
) -> Result<Vec<SessionListItem>, String> {
    runtime.log("info", "ipc.sessions_list", "命令进入");
    let threads = runtime.thread_store.list_threads().await.map_err(|error| {
        runtime.log(
            "error",
            "ipc.sessions_list",
            format!("读取会话列表失败: {error:#}"),
        );
        runtime_error(error)
    })?;
    let mut rows = Vec::new();
    let mut roots_by_cwd = HashMap::<String, Result<PathBuf, String>>::new();
    for meta in threads {
        let root_result = if let Some(result) = roots_by_cwd.get(&meta.cwd) {
            result.clone()
        } else {
            let result = authorize_stored_root(&app, &meta.cwd);
            roots_by_cwd.insert(meta.cwd.clone(), result.clone());
            result
        };
        let authorized_root = match root_result {
            Ok(root) => root,
            Err(error) => {
                runtime.log(
                    "error",
                    "ipc.sessions_list",
                    format!("忽略未授权 Session {}：{error}", meta.id),
                );
                continue;
            }
        };
        let cwd = authorized_root
            .to_str()
            .ok_or_else(|| "Session 工作目录必须是 UTF-8 路径".to_owned())?
            .to_owned();
        runtime
            .sync_session_metadata(meta.id.clone(), cwd.clone(), meta.title.clone())
            .map_err(runtime_error)?;
        rows.push(SessionListItem {
            id: meta.id,
            title: meta.title,
            cwd,
            updated_at: meta.updated_at.to_rfc3339(),
        });
    }
    runtime.log(
        "info",
        "ipc.sessions_list",
        format!("命令完成 count={}", rows.len()),
    );
    Ok(rows)
}
