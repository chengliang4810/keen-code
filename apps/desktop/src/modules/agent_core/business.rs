use super::{validate_request, validate_secret_account, NativeAgentRequest};
use rcode_agent::{
    AgentId, AgentRunner, ControlledToolIdentity, ControlledToolRequest,
    NoopControlledToolLifecycleSink, PlanGuard, RunLimits, SessionId, ToolRegistry,
    TurnCancellation, TurnId,
};
use rcode_model::{ModelProvider, ToolCall, ToolDefinition};
use rcode_provider::ApiKey;
use rcode_runtime::{
    AgentRuntime, BusinessToolOptions, EventBridge, ModelConfig, ResolvedSubagent, RunLease,
    ShellTarget, SubagentTemplate,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tauri::{ipc::Channel, AppHandle, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeSubagent {
    pub template: SubagentTemplate,
    pub model: ModelConfig,
    pub secret_account: Option<String>,
}

pub(super) fn validate_subagents(agents: &[NativeSubagent]) -> Result<(), String> {
    if agents.len() > 132 {
        return Err("子 Agent 数量超过上限".into());
    }
    let mut ids = HashSet::new();
    for agent in agents {
        agent.template.validate()?;
        agent.model.validate()?;
        validate_secret_account(agent.secret_account.as_deref())?;
        if !ids.insert(&agent.template.id) {
            return Err("子 Agent 标识重复".into());
        }
    }
    Ok(())
}

async fn resolve_provider(
    app: &AppHandle,
    model: &ModelConfig,
    account: Option<&str>,
) -> Result<Arc<dyn ModelProvider>, String> {
    validate_secret_account(account)?;
    let key = match account {
        Some(account) => {
            crate::modules::secrets::read_secret(app, &app.state(), "rcode-ai", account)?
        }
        None => None,
    };
    let key = key
        .filter(|key| !key.is_empty())
        .map(ApiKey::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(model.provider(key).await?))
}

pub(super) async fn resolve_subagents(
    app: &AppHandle,
    agents: &[NativeSubagent],
) -> Result<Vec<ResolvedSubagent>, String> {
    validate_subagents(agents)?;
    let mut resolved = Vec::new();
    for agent in agents {
        resolved.push(ResolvedSubagent {
            template: agent.template.clone(),
            model: agent.model.model.clone(),
            provider: resolve_provider(app, &agent.model, agent.secret_account.as_deref()).await?,
        });
    }
    Ok(resolved)
}

struct BusinessRun {
    cancellation: TurnCancellation,
    runner: AgentRunner,
    calls: AsyncMutex<BusinessCalls>,
    session_id: String,
    plan_mode: bool,
}

struct BusinessCalls {
    lease: Option<RunLease>,
    ids: HashSet<String>,
}

#[derive(Clone, Default)]
pub struct BusinessBridgeState(Arc<Mutex<HashMap<String, Arc<BusinessRun>>>>);

#[tauri::command]
pub async fn agent_business_start(
    app: AppHandle,
    state: State<'_, AgentRuntime>,
    bridges: State<'_, BusinessBridgeState>,
    request: NativeAgentRequest,
    workspace: crate::modules::workspace::WorkspaceEnv,
    on_event: Channel<Value>,
) -> Result<Vec<ToolDefinition>, String> {
    validate_request(&request)?;
    let root = crate::modules::workspace::authorize_spawn_cwd(
        &app.state(),
        request.cwd.as_deref(),
        &workspace,
    )?
    .ok_or("业务工具需要任务工作区")?;
    let lease = state.register(&request.run_id, &request.session_id)?;
    let runtime = state.inner().clone();
    let event_run = request.run_id.clone();
    let events = EventBridge::new(move |event| {
        on_event.send(event).map_err(|_| {
            let _ = runtime.cancel(&event_run);
            "业务工具界面连接已关闭".into()
        })
    });
    let shell_target = match workspace {
        crate::modules::workspace::WorkspaceEnv::Local => ShellTarget::Local,
        crate::modules::workspace::WorkspaceEnv::Wsl { distro } => ShellTarget::Wsl {
            distro,
            cwd: request.cwd.clone().ok_or("WSL 需要任务目录")?,
        },
    };
    let provider = resolve_provider(
        &app,
        &request.model_config(),
        request.secret_account.as_deref(),
    )
    .await?;
    let mut registry = ToolRegistry::new();
    state.register_business_tools(
        &mut registry,
        BusinessToolOptions {
            session_id: request.session_id.clone(),
            root,
            output_directory: crate::modules::storage::directory("outputs/background")?,
            shell_target,
            initial_todos: request.todos,
            subagents: resolve_subagents(&app, &request.subagents).await?,
            events: events.clone(),
            plan_mode: request.plan_mode,
        },
    )?;
    let definitions = registry.definitions();
    let runner = AgentRunner::new(
        provider,
        registry,
        RunLimits::new(24, 256).expect("static limits"),
    )
    .with_tool_approval_gate(state.approval_gate(
        &request.run_id,
        events,
        request.permission_mode,
        request.plan_mode,
    ));
    if lease.cancellation.is_cancelled() {
        return Err("业务工具启动已取消".into());
    }
    let cancellation = lease.cancellation.clone();
    let cleanup_bridges = Arc::downgrade(&bridges.0);
    let cleanup_id = request.run_id.clone();
    bridges.0.lock().map_err(|_| "业务工具状态不可用")?.insert(
        request.run_id,
        Arc::new(BusinessRun {
            cancellation: cancellation.clone(),
            runner,
            calls: AsyncMutex::new(BusinessCalls {
                lease: Some(lease),
                ids: HashSet::new(),
            }),
            session_id: request.session_id,
            plan_mode: request.plan_mode,
        }),
    );
    tauri::async_runtime::spawn(async move {
        cancellation.cancelled().await;
        if let Some(bridges) = cleanup_bridges.upgrade() {
            if let Err(error) = finish_run(&BusinessBridgeState(bridges), &cleanup_id).await {
                log::error!("business run cleanup failed: {error}");
            }
        }
    });
    Ok(definitions)
}

#[tauri::command]
pub async fn agent_business_execute(
    bridges: State<'_, BusinessBridgeState>,
    run_id: String,
    call_id: String,
    name: String,
    input: Value,
) -> Result<Value, String> {
    execute_tool(&bridges, &run_id, call_id, name, input).await
}

async fn execute_tool(
    bridges: &BusinessBridgeState,
    run_id: &str,
    call_id: String,
    name: String,
    input: Value,
) -> Result<Value, String> {
    if serde_json::to_vec(&input).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("业务工具输入超过上限".into());
    }
    let run = bridges
        .0
        .lock()
        .map_err(|_| "业务工具状态不可用")?
        .get(run_id)
        .cloned()
        .ok_or("业务工具运行已结束")?;
    // 保持一个运行内的工具顺序和消费式调用身份，取消仍由 Runtime 租约传播。
    let mut calls = run.calls.lock().await;
    if calls.lease.is_none() || run.cancellation.is_cancelled() {
        return Err("业务工具运行已结束".into());
    }
    if calls.ids.len() >= 256 || !calls.ids.insert(call_id.clone()) {
        return Err("业务工具调用重复或超过上限".into());
    }
    let call = ToolCall::new(&call_id, name, input);
    let result = run
        .runner
        .execute_controlled_tool(ControlledToolRequest {
            identity: ControlledToolIdentity {
                session_id: SessionId::new(&run.session_id).map_err(|e| e.to_string())?,
                turn_id: TurnId::new(run_id).map_err(|e| e.to_string())?,
                source_agent_id: AgentId::new("main").expect("static agent"),
                operation_id: call_id,
            },
            call,
            plan_guard: if run.plan_mode {
                PlanGuard::read_only()
            } else {
                PlanGuard::inactive()
            },
            cancellation: run.cancellation.clone(),
            lifecycle: Arc::new(NoopControlledToolLifecycleSink),
        })
        .await
        .map_err(|e| e.to_string())?;
    let text = result
        .result
        .content
        .iter()
        .filter_map(|block| {
            if let rcode_model::ToolResultContent::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if result.result.is_error {
        return Err(text);
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[tauri::command]
pub async fn agent_business_finish(
    bridges: State<'_, BusinessBridgeState>,
    run_id: String,
) -> Result<(), String> {
    finish_run(&bridges, &run_id).await
}

async fn finish_run(bridges: &BusinessBridgeState, run_id: &str) -> Result<(), String> {
    let run = bridges
        .0
        .lock()
        .map_err(|_| "业务工具状态不可用")?
        .get(run_id)
        .cloned();
    if let Some(run) = run {
        run.cancellation.cancel();
        // 等待当前工具退出，再释放租约；并发结束请求也等待同一个清理结果。
        let mut calls = run.calls.lock().await;
        calls.lease.take();
        bridges
            .0
            .lock()
            .map_err(|_| "业务工具状态不可用")?
            .remove(run_id);
    }
    Ok(())
}

#[tauri::command]
pub fn agent_core_release_session(
    state: State<'_, AgentRuntime>,
    session_id: String,
) -> Result<(), String> {
    SessionId::new(&session_id).map_err(|e| e.to_string())?;
    state.release_session(&session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcode_model::{ProviderCapabilities, ScriptedProvider};
    use rcode_runtime::PermissionMode;
    use serde_json::json;

    #[test]
    fn native_subagent_config_rejects_mutating_tools_google_keys_and_duplicate_ids() {
        let template = rcode_runtime::builtin_subagents().remove(0);
        let config = json!({"template":template,"model":{"providerId":"local","protocol":"chat_completions","model":"child","baseUrl":"http://127.0.0.1:1234/v1","contextLimit":65536,"allowPrivateNetwork":true}});
        let agent: NativeSubagent = serde_json::from_value(config.clone()).unwrap();
        assert!(validate_subagents(std::slice::from_ref(&agent)).is_ok());
        assert!(validate_subagents(&[agent.clone(), agent]).is_err());
        for tools in [
            json!(["write_file"]),
            json!(["bash_run"]),
            json!(["run_subagent"]),
            json!(["read_file", "read_file"]),
        ] {
            let mut value = config.clone();
            value["template"]["tools"] = tools;
            assert!(validate_subagents(&[serde_json::from_value(value).unwrap()]).is_err());
        }
        let mut invalid = config.clone();
        invalid["secretAccount"] = json!("google-api-key");
        assert!(validate_subagents(&[serde_json::from_value(invalid).unwrap()]).is_err());
        let mut invalid = config;
        invalid["model"]["ignoredApproval"] = json!(true);
        assert!(serde_json::from_value::<NativeSubagent>(invalid).is_err());
    }

    #[tokio::test]
    async fn sdk_bridge_consumes_call_ids_preserves_rust_approval_and_expires_on_finish() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let runtime = AgentRuntime::default();
        let lease = runtime.register("bridge", "session").unwrap();
        let approval_runtime = runtime.clone();
        let (pending_approval, mut approvals) = tokio::sync::mpsc::channel(1);
        let events = EventBridge::new(move |event| {
            if event["type"] == "approval" {
                if event["toolCallId"] == "pending" {
                    pending_approval.try_send(()).map_err(|e| e.to_string())?;
                } else {
                    approval_runtime.approve(event["id"].as_str().unwrap(), false)?;
                }
            }
            Ok(())
        });
        let mut tools = ToolRegistry::new();
        runtime
            .register_business_tools(
                &mut tools,
                BusinessToolOptions {
                    session_id: "session".into(),
                    root: root.clone(),
                    output_directory: root.join("output"),
                    shell_target: ShellTarget::Local,
                    initial_todos: vec![],
                    subagents: vec![],
                    events: events.clone(),
                    plan_mode: false,
                },
            )
            .unwrap();
        let runner = AgentRunner::new(
            Arc::new(ScriptedProvider::new(ProviderCapabilities::default(), [])),
            tools,
            RunLimits::new(24, 256).unwrap(),
        )
        .with_tool_approval_gate(runtime.approval_gate(
            "bridge",
            events,
            PermissionMode::Ask,
            false,
        ));
        let bridges = BusinessBridgeState::default();
        bridges.0.lock().unwrap().insert(
            "bridge".into(),
            Arc::new(BusinessRun {
                cancellation: lease.cancellation.clone(),
                runner,
                calls: AsyncMutex::new(BusinessCalls {
                    lease: Some(lease),
                    ids: HashSet::new(),
                }),
                session_id: "session".into(),
                plan_mode: false,
            }),
        );
        let args = json!({"command":"echo denied"});
        assert!(execute_tool(
            &bridges,
            "bridge",
            "call".into(),
            "bash_background".into(),
            args.clone()
        )
        .await
        .is_err());
        assert!(runtime
            .background(&root.join("output"))
            .unwrap()
            .list()
            .unwrap()
            .is_empty());
        assert!(execute_tool(
            &bridges,
            "bridge",
            "call".into(),
            "bash_background".into(),
            args.clone()
        )
        .await
        .unwrap_err()
        .contains("重复"));
        let retained = bridges.0.lock().unwrap().get("bridge").cloned().unwrap();
        let executing_bridges = bridges.clone();
        let pending_args = args.clone();
        let executing = tokio::spawn(async move {
            execute_tool(
                &executing_bridges,
                "bridge",
                "pending".into(),
                "bash_background".into(),
                pending_args,
            )
            .await
        });
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), approvals.recv())
                .await
                .unwrap(),
            Some(())
        );
        finish_run(&bridges, "bridge").await.unwrap();
        assert!(runtime.register("next", "session").is_ok());
        drop(retained);
        assert!(executing.await.unwrap().is_err());
        assert!(execute_tool(
            &bridges,
            "bridge",
            "late".into(),
            "bash_background".into(),
            args
        )
        .await
        .unwrap_err()
        .contains("已结束"));
    }
}
