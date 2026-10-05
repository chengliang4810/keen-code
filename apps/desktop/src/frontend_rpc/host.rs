//! 唯一桌面服务聚合器：原始 RPC 只在这里选择真实 Rust 领域入口。

use super::dispatch::{EventCallback, GatewayContext, Handler, RpcError, RpcFuture, Subscription};
use super::{services::ServiceHandler, session, workflows};
use crate::workflows::{
    EngineWorkflowDriver, RuntimeWorkflowJournal, WorkflowActorBinding, WorkflowHost,
    WorkflowHostError, WorkflowJournalError, WorkflowJournalPort, WorkflowRuntimeError,
    WorkflowScope, WorkflowStore, workflow_actor_transcript_reader, workflow_question_resolver,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tauri::Manager;

#[derive(Default)]
pub(crate) struct DesktopHandler {
    /// 活跃执行任务自行持有 Host；弱缓存只供取消/查询复用，不阻碍结束后的 Session 关闭。
    /// 运行事实与重启恢复始终读取父会话 Journal。
    workflow_hosts: Mutex<HashMap<String, Weak<WorkflowHost>>>,
}

impl Handler for DesktopHandler {
    fn call(&self, ctx: GatewayContext, channel: String, method: String, args: Value) -> RpcFuture {
        let args = service_arguments(args);
        Box::pin(async move {
            if channel == "window-controller" {
                return super::controller::call(ctx, &method, args).await;
            }
            if matches!(channel.as_str(), "provider-settings" | "model-selection") {
                return super::providers::call(ctx, channel, method, args).await;
            }
            if channel == "git" && method == "generateCommitMessage" {
                return super::native_actions::generate_commit_message(ctx, args).await;
            }
            if channel == "zcode-agent"
                && super::services::automations::is_automation_method(&method)
            {
                return super::services::automations::call_automation(
                    &ctx.app,
                    &method,
                    args,
                    Some(ctx.connection_id.as_str()),
                )
                .await
                .map_err(|error| RpcError::new("automation.error", error));
            }
            if channel == "terminal" {
                return super::terminal::call(ctx, &method, args).await;
            }
            if channel == "zcode-agent" && super::agent_views::is_method(&method) {
                return super::agent_views::call(ctx, &method, args).await;
            }
            if channel == "zcode-agent" && workflows::is_workflow_method(&method) {
                return workflow_call(ctx, &method, args).await;
            }
            if is_session_channel(&channel) {
                session::call_session(ctx, &channel, &method, args).await
            } else {
                ServiceHandler.call(ctx, channel, method, args).await
            }
        })
    }

    fn listen(
        &self,
        ctx: GatewayContext,
        channel: String,
        event: String,
        args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError> {
        if channel == "window-controller" {
            super::controller::listen(&ctx, &event, args, callback)
        } else if channel == "terminal" {
            super::terminal::listen(&ctx, &event, args, callback)
        } else if matches!(channel.as_str(), "provider-settings" | "model-selection") {
            super::providers::listen(&ctx, &channel, &event, args, callback)
        } else if is_session_channel(&channel) {
            session::listen_session(&ctx, &channel, &event, args, callback)
        } else {
            ServiceHandler.listen(ctx, channel, event, args, callback)
        }
    }

    fn connection_closed(&self, ctx: GatewayContext) {
        // 问答等待者也由连接拥有；不能只清理投影而遗留真实 AskUser Future。
        super::services::automations::connection_closed(&ctx.connection_id);
        session::SessionHandler.connection_closed(ctx.clone());
        super::controller::connection_closed(&ctx);
        super::terminal::close_connection(&ctx);
    }

    fn response_sent(
        &self,
        ctx: GatewayContext,
        channel: String,
        method: String,
        args: Value,
        response: Value,
    ) {
        // 只有底层已发送成功 ACK 后才能激活订阅，不能用调度延迟猜测顺序。
        if channel == "window-controller" {
            super::controller::response_sent(
                &ctx,
                &channel,
                &method,
                service_arguments(args),
                response,
            );
            return;
        }
        session::SessionHandler.response_sent(
            ctx,
            channel,
            method,
            service_arguments(args),
            response,
        );
    }
}

fn is_session_channel(channel: &str) -> bool {
    matches!(channel, "zcode-agent" | "zcode-session" | "zcode-task")
}

/// ProxyChannel 的方法载荷是位置参数数组，动态事件载荷则是单个参数。
/// 单对象参数在聚合边界展开；文件、终端等位置参数仍由各服务按原接口解码。
fn service_arguments(args: Value) -> Value {
    match args {
        Value::Array(mut values) if values.len() == 1 && values[0].is_object() => values.remove(0),
        Value::Array(values) if values.is_empty() => json!({}),
        other => other,
    }
}

fn workflow_error(error: impl std::fmt::Display) -> RpcError {
    RpcError::new("workflow.error", error.to_string())
}

fn workflow_call_error(error: WorkflowHostError) -> RpcError {
    match error {
        WorkflowHostError::SettingsRejected { reason, detail } => RpcError::new(
            format!("fault.command.workflowRunSettingsRejected.{reason}"),
            detail,
        ),
        other => workflow_error(other),
    }
}

fn workspace_path(args: &Value) -> Option<&str> {
    args.get("workspacePath")
        .or_else(|| args.pointer("/workspace/path"))
        .or_else(|| args.pointer("/workspace/workspacePath"))
        .and_then(Value::as_str)
}

/// 读取 Source 传入的父会话身份；调用方随后会从下游 DTO 删除这些宿主字段。
fn workflow_session_id(args: &Value) -> Option<String> {
    args.get("parentSessionId")
        .or_else(|| args.get("sessionId"))
        .or_else(|| args.pointer("/key/sessionId"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// 清理 workflow 下游 DTO 不认识的本地宿主字段。
///
/// `workspaceIdentity` 参与上游 workspace 授权/隔离，但不是 Rust workflow
/// query DTO 的字段；必须在授权身份捕获后移除。当前桌面 host 没有远程 workflow
/// 路由，收到 `remoteSessionId` 时显式拒绝，不能静默降级成本地 Session。
fn strip_workflow_host_context(
    object: &mut serde_json::Map<String, Value>,
) -> Result<(), RpcError> {
    let remote_requested = object.contains_key("remoteSessionId")
        || object
            .get("workspace")
            .and_then(Value::as_object)
            .is_some_and(|workspace| workspace.contains_key("remoteSessionId"));
    if remote_requested {
        return Err(RpcError::new(
            "workflow.remoteUnsupported",
            "本地 workflow transport 不支持 remoteSessionId",
        ));
    }

    for field in [
        "workspacePath",
        "workspaceIdentity",
        "sessionId",
        "workspace",
        "projectStorage",
    ] {
        object.remove(field);
    }
    Ok(())
}

fn is_workflow_start_method(method: &str) -> bool {
    matches!(
        method,
        "startSavedWorkflow" | "start" | "run" | "startWorkflow" | "workflows.start"
    )
}

fn is_workflow_run_list_method(method: &str) -> bool {
    matches!(
        method,
        "conversationWorkflowRuns"
            | "conversationWorkflowRunsV4"
            | "listRuns"
            | "listWorkflowRuns"
            | "workflows.listRuns"
    )
}

/// 定义保存只接收 Source DTO；AgentTool 的会话/调用身份只属于宿主运行面。
fn strip_definition_host_context(object: &mut serde_json::Map<String, Value>) {
    object.remove("parentSessionId");
    object.remove("toolCallId");
}

/// 将模型工作流工具绑定到普通父 Session 的生产 Host；页面不能覆盖 ToolContext 身份。
pub(crate) fn install_workflow_tools(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::workflows::agent_tools::{WorkflowToolError, workflow_tool_port};
    let runtime = crate::require_owned_runtime(app)?;
    let tool_app = app.clone();
    runtime
        .set_workflow_tool_port(workflow_tool_port(move |request| {
            let app = tool_app.clone();
            Box::pin(async move {
                let failure = |message: String| {
                    WorkflowToolError::permanent(
                        "workflow.identity",
                        keencode_model::redact_error_secrets(&message),
                    )
                };
                if request.source_agent_id != keencode_resources::ROOT_AGENT_ID {
                    return Err(failure("只有普通根 Agent 可以管理工作流".into()));
                }
                let runtime = crate::require_owned_runtime(&app).map_err(failure)?;
                let parent = crate::session_commands::open_authorized_session(
                    &runtime,
                    &app,
                    &request.session_id,
                )
                .map_err(failure)?;
                if parent
                    .is_workflow_actor()
                    .map_err(|error| failure(error.to_string()))?
                {
                    return Err(failure("工作流 actor 不能嵌套工作流".into()));
                }
                let state = parent
                    .snapshot()
                    .map_err(|error| failure(error.to_string()))?
                    .state;
                let turn_id = keencode_resources::TurnId::new(request.turn_id.clone())
                    .map_err(|error| failure(error.to_string()))?;
                if state.turns.get(&turn_id).is_none_or(|turn| {
                    turn.status != keencode_resources::TurnStatus::Running
                        || turn.source_agent_id.as_str() != request.source_agent_id
                }) {
                    return Err(failure("工作流工具不属于正在执行的根回合".into()));
                }
                if state.plan.enabled
                    && matches!(request.method.as_str(), "createWorkflow" | "saveWorkflow")
                {
                    return Err(failure("Plan 模式禁止工作流写入".into()));
                }
                let mut args = request.args;
                let object = args
                    .as_object_mut()
                    .ok_or_else(|| failure("工作流工具参数必须为对象".into()))?;
                object.insert("parentSessionId".into(), json!(request.session_id));
                object.insert("workspacePath".into(), json!(state.project_root));
                object.insert("toolCallId".into(), json!(request.tool_call_id));
                workflow_call_app(app, &request.method, args)
                    .await
                    .map_err(|error| {
                        WorkflowToolError::permanent("workflow.host", error.to_string())
                    })
            })
        }))
        .map_err(|error| error.to_string())
}

/// 工作流控制面也供 conversation command 调用；身份、路径、模型在这里由宿主冻结。
pub(crate) async fn workflow_call(
    ctx: GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let connection_id = keencode_acp::ConnectionId::new(ctx.connection_id.clone())
        .map_err(|error| workflow_error(error.to_string()))?;
    workflow_call_app_with_connection(ctx.app, method, args, Some(&connection_id)).await
}

/// AgentTool 和页面走同一个权威入口；工具不伪造页面连接，也不依赖窗口生命周期。
pub(crate) async fn workflow_call_app(
    app: tauri::AppHandle,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    workflow_call_app_with_connection(app, method, args, None).await
}

async fn workflow_call_app_with_connection(
    app: tauri::AppHandle,
    method: &str,
    mut args: Value,
    permission_connection: Option<&keencode_acp::ConnectionId>,
) -> Result<Value, RpcError> {
    let runtime = crate::require_owned_runtime(&app).map_err(workflow_error)?;
    // Source 查询 DTO 只带 `sessionId`，而下游 Host 使用 `parentSessionId`。
    // 宿主字段必须在清理前捕获，否则查询会丢失唯一的父会话身份。
    let requested_session_id = workflow_session_id(&args);
    let requested_workspace_path = workspace_path(&args).map(ToOwned::to_owned);
    // `strip_workflow_host_context` 会移除 workspacePath，因此先复制已授权路径，
    // 再执行清理；远程身份也在此处被拒绝，避免错误地落入本地 Host。
    let object = args
        .as_object_mut()
        .ok_or_else(|| workflow_error("工作流参数必须为对象"))?;
    strip_workflow_host_context(object)?;
    let root = requested_workspace_path
        .as_deref()
        .map(|path| crate::workspace::registered_project_root(&app, path))
        .transpose()
        .map_err(workflow_error)?;
    let project_storage = root
        .as_ref()
        .map(|root| {
            keencode_resources::ensure_project_storage(
                runtime.storage_root(),
                &root.to_string_lossy(),
            )
        })
        .transpose()
        .map_err(workflow_error)?;
    let keep_scope = workflows::is_definition_method(method)
        || method == "listSavedWorkflowRuns"
        || is_workflow_start_method(method)
        || is_workflow_run_list_method(method);
    if keep_scope && !object.contains_key("scope") {
        object.insert("scope".into(), json!("project"));
    } else if !keep_scope {
        object.remove("scope");
    }
    if workflows::is_definition_method(method) {
        // AgentTool 端口会把父会话和工具调用身份临时附加到同一个 Value，供运行面
        // 校验；定义面只接收 Source 的 `{scope, definition}`，项目存储已由上面的
        // registered_project_root 绑定。若把这些宿主字段继续传给严格 DTO，真实
        // CreateWorkflow/SaveWorkflow 会在落盘前因 parentSessionId 被拒绝。
        strip_definition_host_context(object);
        let store = match project_storage {
            Some(path) => WorkflowStore::with_project_storage(runtime.storage_root(), path),
            None => WorkflowStore::new(runtime.storage_root()),
        };
        return workflows::call_definitions(&store, method, args).map_err(workflow_error);
    }
    if method == "listSavedWorkflowRuns" {
        // 中枢的历史属于工作区而非当前选中会话；逐个读取父 Journal，禁止另建索引事实源。
        let scope: WorkflowScope =
            serde_json::from_value(args["scope"].clone()).map_err(workflow_error)?;
        let name = args.get("name").and_then(Value::as_str);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 50) as usize;
        let mut runs = Vec::new();
        let mut truncated = false;
        for metadata in runtime.stored_sessions().map_err(workflow_error)? {
            let Ok(stored_root) =
                crate::session_commands::authorize_stored_root(&app, &metadata.project_root)
            else {
                continue;
            };
            if scope == WorkflowScope::Project && root.as_ref() != Some(&stored_root) {
                continue;
            }
            let parent = crate::session_commands::open_authorized_session(
                &runtime,
                &app,
                metadata.session_id.as_str(),
            )
            .map_err(workflow_error)?;
            if parent.is_workflow_actor().map_err(workflow_error)? {
                continue;
            }
            let page = RuntimeWorkflowJournal::new(parent)
                .list_runs(None, name, Some(scope), limit + 1)
                .map_err(workflow_error)?;
            truncated |= page.truncated;
            runs.extend(page.runs);
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
        truncated |= runs.len() > limit;
        runs.truncate(limit);
        return workflows::project_saved_runs(runs, truncated).map_err(workflow_error);
    }
    let session_id =
        requested_session_id.ok_or_else(|| workflow_error("工作流运行必须指定父会话"))?;
    let parent = crate::session_commands::open_authorized_session(&runtime, &app, &session_id)
        .map_err(workflow_error)?;
    if let Some(connection_id) = permission_connection {
        // Workflow 没有普通 root Turn 的 start_root_turn 绑定点；页面启动/恢复
        // 必须在创建真实 Driver 前绑定同一窗口连接，Artifact/Tool 审批与 actor
        // 的 AskUserQuestion 才能回投同一页面。
        runtime
            .elicitation_coordinator()
            .bind_session_connection(&session_id, connection_id)
            .map_err(workflow_error)?;
        runtime
            .bind_permission_session_connection(&session_id, connection_id)
            .map_err(workflow_error)?;
    }
    let snapshot = parent.snapshot().map_err(workflow_error)?;
    if parent.is_workflow_actor().map_err(workflow_error)? {
        return Err(workflow_error("工作流 actor 禁止嵌套工作流"));
    }
    let parent_root =
        crate::session_commands::authorize_stored_root(&app, &snapshot.state.project_root)
            .map_err(workflow_error)?;
    if root.as_ref().is_some_and(|root| root != &parent_root) {
        return Err(workflow_error("工作流工作区与父会话不一致"));
    }
    let parent_storage = keencode_resources::ensure_project_storage(
        runtime.storage_root(),
        &parent_root.to_string_lossy(),
    )
    .map_err(workflow_error)?;
    let object = args.as_object_mut().expect("object checked above");
    let keep_parent_session = is_workflow_start_method(method)
        || method == "amendSettings"
        || is_workflow_run_list_method(method);
    let keep_execution_context = is_workflow_start_method(method) || method == "amendSettings";
    if keep_parent_session {
        object.insert("parentSessionId".into(), json!(session_id));
    } else {
        object.remove("parentSessionId");
    }
    if keep_execution_context {
        object.insert("cwd".into(), json!(parent_root));
    } else {
        object.remove("cwd");
    }
    if is_workflow_start_method(method) {
        object.insert("projectStorage".into(), json!(parent_storage));
    } else {
        object.remove("projectStorage");
    }
    if keep_execution_context {
        let frozen_provider = runtime
            .workflow_provider_snapshot(&session_id)
            .map_err(workflow_error)?;
        // `amendSettings` 只能由已校验 runId/Session 的 command 路由调用。该路由携带
        // Journal 中的旧 `models`，因此修订 maxConcurrency 时仍使用原冻结模型；其他
        // 入口一律重新取当前已授权 Session 快照，页面不能覆盖 Provider 事实。
        let model_selection = object
            .remove("frozenModelSelection")
            .filter(|_| method == "amendSettings")
            .unwrap_or_else(|| {
                json!({
                    "provider": frozen_provider,
                    "planEnabled": snapshot.state.plan.enabled,
                })
            });
        object.insert("modelSelection".into(), model_selection);
    } else {
        object.remove("modelSelection");
        object.remove("frozenModelSelection");
        object.remove("toolCallId");
        object.remove("launchInputId");
    }
    let frontend = app.state::<Arc<DesktopHandler>>();
    let host = {
        let mut hosts = frontend
            .workflow_hosts
            .lock()
            .map_err(|_| workflow_error("工作流调度锁不可用"))?;
        hosts.retain(|_, host| host.strong_count() > 0);
        if let Some(host) = hosts.get(&session_id).and_then(Weak::upgrade) {
            host
        } else {
            let host = {
                // 工具事实留在 actor 自己的 Journal；只在已授权的父会话范围内读回展示。
                // 不捕获 AppHandle，避免 Host 缓存与应用状态形成持有环。
                let transcript_runtime = Arc::clone(&runtime);
                let transcript_parent = parent.session_id().as_str().to_owned();
                let transcript_root = parent_root.clone();
                let reader = workflow_actor_transcript_reader(move |binding| {
                    let fail = |error: String| WorkflowJournalError(error);
                    let actor_session_id = binding.actor_session_id.as_str();
                    if binding.parent_session_id != transcript_parent
                        || crate::workspace::canonical_session_root(&binding.project_root)
                            .map_err(fail)?
                            != transcript_root
                    {
                        return Err(fail("工作流 actor 读取请求超出父会话范围".into()));
                    }
                    let metadata = transcript_runtime
                        .runtime_manager()
                        .stored_session_metadata(actor_session_id)
                        .map_err(|error| fail(error.to_string()))?;
                    if metadata.corrupt
                        || crate::workspace::canonical_session_root(&metadata.project_root)
                            .map_err(fail)?
                            != transcript_root
                    {
                        return Err(fail("工作流 actor 的项目或 Journal 不可读取".into()));
                    }
                    let actor = transcript_runtime
                        .open_or_create_session(
                            &transcript_root,
                            Some(actor_session_id),
                            "workflow-workspace-read",
                        )
                        .map_err(|error| fail(error.to_string()))?;
                    let authorized = actor
                        .read_state(|state| {
                            state
                                .workflow_events
                                .values()
                                .flatten()
                                .any(|event| workflow_actor_binding_matches(event, binding))
                        })
                        .map_err(|error| fail(error.to_string()))?;
                    if !authorized {
                        return Err(fail("工作流 actor 与父会话的权威绑定不一致".into()));
                    }
                    transcript_runtime
                        .session_transcript(actor_session_id)
                        .map(Some)
                        .map_err(|error| fail(error.to_string()))
                });
                let journal = Arc::new(RuntimeWorkflowJournal::new_with_actor_transcript_reader(
                    parent.clone(),
                    reader,
                ));
                // 新 Host 接管父会话时先把上次进程遗留的未结算 run 写成
                // interrupted；只有 Journal 的恢复门禁允许的只读节点才能随后 Resume。
                journal.reconcile_cold_runs().map_err(workflow_error)?;
                let question_runtime = Arc::clone(&runtime);
                let question_parent = parent.session_id().as_str().to_owned();
                let question_resolver = workflow_question_resolver(move |qid, answer| {
                    let runtime = Arc::clone(&question_runtime);
                    let parent_id = question_parent.clone();
                    Box::pin(async move {
                        runtime
                            .resolve_workflow_question(&parent_id, &qid, &answer)
                            .map_err(|error| WorkflowRuntimeError(error.to_string()))
                    })
                });
                let leaf = Arc::new(crate::workflows::NativeWorkflowDriver::new(
                    Arc::clone(&runtime),
                    parent,
                ));
                let driver = Arc::new(EngineWorkflowDriver::new(
                    leaf,
                    journal.clone(),
                    question_resolver,
                ));
                WorkflowHost::new_with_project_storage(
                    runtime.storage_root(),
                    parent_storage,
                    journal,
                    driver,
                )
            };
            hosts.insert(session_id, Arc::downgrade(&host));
            host
        }
    };
    workflows::call(host, method, args)
        .await
        .map_err(workflow_call_error)
}

/// actor 自身的 Journal 必须确认父会话和运行身份；父记录不能单方面授予读取权。
fn workflow_actor_binding_matches(
    event: &keencode_resources::WorkflowJournalEvent,
    binding: &WorkflowActorBinding,
) -> bool {
    event.event_type == "actor-bound"
        && event.actor_session_id.as_deref() == Some(binding.actor_session_id.as_str())
        && event.run_id == binding.run_id
        && event.payload.get("parentSessionId").and_then(Value::as_str)
            == Some(binding.parent_session_id.as_str())
        && event.payload.get("runId").and_then(Value::as_str) == Some(binding.run_id.as_str())
        && event.payload.get("nodeId").and_then(Value::as_str) == Some(binding.node_id.as_str())
        && event.payload.get("nodeAddress") == Some(&binding.node_address)
        && event.payload.get("cwd").and_then(Value::as_str) == Some(binding.project_root.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_method_array_keeps_positional_arguments() {
        assert_eq!(
            service_arguments(json!([{"sessionId":"s"}])),
            json!({"sessionId":"s"})
        );
        assert_eq!(
            service_arguments(json!(["file", "body"])),
            json!(["file", "body"])
        );
        assert_eq!(service_arguments(json!([])), json!({}));
    }

    #[test]
    fn definition_route_strips_agent_context_but_keeps_source_fields() {
        let mut args = json!({
            "scope": "project",
            "definition": {"version": 1},
            "parentSessionId": "session-1",
            "toolCallId": "call-1",
        });
        strip_definition_host_context(args.as_object_mut().expect("definition args object"));
        assert_eq!(
            args,
            json!({"scope": "project", "definition": {"version": 1}})
        );
    }

    #[test]
    fn workflow_query_captures_source_session_before_host_field_cleanup() {
        let mut args = json!({
            "sessionId": "session-source",
            "workspacePath": "C:/repo",
            "runId": "run-1"
        });
        let captured = workflow_session_id(&args);
        args.as_object_mut()
            .expect("workflow args must be an object")
            .remove("sessionId");

        assert_eq!(captured.as_deref(), Some("session-source"));
        assert!(args.get("sessionId").is_none());
    }

    #[test]
    fn workflow_host_context_cleanup_strips_identity_and_rejects_remote() {
        let mut args = json!({
            "workspacePath": "C:/repo",
            "workspaceIdentity": "workspace-key",
            "sessionId": "session-source",
            "workspace": {"workspacePath": "C:/repo"},
            "projectStorage": "C:/data",
            "runId": "run-1"
        });
        strip_workflow_host_context(args.as_object_mut().expect("workflow args object"))
            .expect("local host fields should be cleaned");
        assert_eq!(args, json!({"runId": "run-1"}));

        let mut remote = json!({
            "workspacePath": "C:/repo",
            "workspaceIdentity": "workspace-key",
            "remoteSessionId": "remote-1",
            "sessionId": "session-source",
            "runId": "run-1"
        });
        let error =
            strip_workflow_host_context(remote.as_object_mut().expect("workflow args object"))
                .expect_err("local workflow host must reject remote identity");
        assert_eq!(error.code(), "workflow.remoteUnsupported");
        assert_eq!(remote["remoteSessionId"], "remote-1");

        let mut nested_remote = json!({
            "workspace": {"workspacePath": "C:/repo", "remoteSessionId": "remote-2"},
            "runId": "run-1"
        });
        assert_eq!(
            strip_workflow_host_context(
                nested_remote.as_object_mut().expect("workflow args object")
            )
            .expect_err("nested remote identity must also be rejected")
            .code(),
            "workflow.remoteUnsupported"
        );
    }

    #[test]
    fn workflow_session_id_prefers_parent_and_accepts_nested_host_key() {
        assert_eq!(
            workflow_session_id(&json!({
                "parentSessionId": "parent",
                "sessionId": "source"
            }))
            .as_deref(),
            Some("parent")
        );
        assert_eq!(
            workflow_session_id(&json!({"key": {"sessionId": "nested"}})).as_deref(),
            Some("nested")
        );
        assert_eq!(workflow_session_id(&json!({"sessionId": ""})), None);
    }

    #[test]
    fn workflow_actor_transcript_requires_its_own_parent_and_run_binding() {
        let mut binding = WorkflowActorBinding {
            actor_session_id: "actor-1".into(),
            parent_session_id: "parent-1".into(),
            run_id: "run-1".into(),
            node_id: "review".into(),
            node_address: json!({"node_id":"review", "iteration":[]}),
            project_root: "D:/isolated-project".into(),
        };
        let mut event = keencode_resources::WorkflowJournalEvent {
            run_id: "run-1".into(),
            tool_call_id: "workflow-tool".into(),
            sequence: 1,
            event_type: "actor-bound".into(),
            payload: json!({
                "parentSessionId":"parent-1", "runId":"run-1", "nodeId":"review",
                "nodeAddress":binding.node_address, "cwd":binding.project_root,
            }),
            artifacts: Vec::new(),
            actor_session_id: Some("actor-1".into()),
            launch_input_id: Some("launch-1".into()),
        };
        assert!(workflow_actor_binding_matches(&event, &binding));
        binding.actor_session_id = "actor-2".into();
        assert!(!workflow_actor_binding_matches(&event, &binding));
        binding.actor_session_id = "actor-1".into();
        binding.parent_session_id = "parent-2".into();
        assert!(!workflow_actor_binding_matches(&event, &binding));
        binding.parent_session_id = "parent-1".into();
        event.payload["runId"] = json!("run-2");
        assert!(!workflow_actor_binding_matches(&event, &binding));
        event.payload["runId"] = json!("run-1");
        event.payload["parentSessionId"] = Value::Null;
        event.payload["parent_session_id"] = json!("parent-1");
        assert!(!workflow_actor_binding_matches(&event, &binding));
        event.payload["parentSessionId"] = json!("parent-1");
        binding.node_address = json!({"node_id":"other", "iteration":[]});
        assert!(!workflow_actor_binding_matches(&event, &binding));
        binding.node_address = event.payload["nodeAddress"].clone();
        binding.project_root = "D:/another-project".into();
        assert!(!workflow_actor_binding_matches(&event, &binding));
        binding.project_root = "D:/isolated-project".into();
        event.event_type = "actor-started".into();
        assert!(!workflow_actor_binding_matches(&event, &binding));
    }
}
