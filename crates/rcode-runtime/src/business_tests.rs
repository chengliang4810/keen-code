use super::*;
use rcode_agent::{
    AgentId, AgentRunner, ControlledToolIdentity, ControlledToolRequest,
    NoopControlledToolLifecycleSink, PlanGuard, RunLimits, SessionId, TodoController, TurnId,
};
use rcode_model::{
    ModelProvider, ModelStreamEvent, ProviderCapabilities, ResponseMetadata, ScriptedProvider,
    ScriptedReply, StopReason, ToolCall,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn provider(replies: Vec<ScriptedReply>) -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..Default::default()
        },
        replies,
    ))
}
fn text_reply(text: &str) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.into(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ])
}
fn call_reply(name: &str, input: Value) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: "child-call".into(),
            name: name.into(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: "child-call".into(),
            delta: input.to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: "child-call".into(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ])
}
fn registry(
    runtime: &AgentRuntime,
    root: &std::path::Path,
    session: &str,
    plan: bool,
    agents: Vec<ResolvedSubagent>,
    events: Arc<EventBridge>,
) -> rcode_agent::ToolRegistry {
    let mut tools = rcode_agent::ToolRegistry::new();
    runtime
        .register_business_tools(
            &mut tools,
            BusinessToolOptions {
                session_id: session.into(),
                root: root.into(),
                output_directory: root.join("output"),
                shell_target: ShellTarget::Local,
                initial_todos: vec![],
                subagents: agents,
                events,
                plan_mode: plan,
            },
        )
        .unwrap();
    tools
}
fn runner(
    runtime: &AgentRuntime,
    tools: rcode_agent::ToolRegistry,
    lease: &RunLease,
    events: Arc<EventBridge>,
    permission: PermissionMode,
) -> AgentRunner {
    AgentRunner::new(provider(vec![]), tools, RunLimits::new(24, 256).unwrap())
        .with_tool_approval_gate(runtime.approval_gate(&lease.id, events, permission, false))
}
async fn invoke(
    runner: &AgentRunner,
    lease: &RunLease,
    id: &str,
    name: &str,
    input: Value,
    plan: bool,
) -> rcode_model::ToolResult {
    invoke_result(runner, lease, id, name, input, plan)
        .await
        .unwrap()
}
async fn invoke_result(
    runner: &AgentRunner,
    lease: &RunLease,
    id: &str,
    name: &str,
    input: Value,
    plan: bool,
) -> Result<rcode_model::ToolResult, rcode_agent::AgentRunError> {
    runner
        .execute_controlled_tool(ControlledToolRequest {
            identity: ControlledToolIdentity {
                session_id: SessionId::new(&lease.session_id).unwrap(),
                turn_id: TurnId::new(&lease.id).unwrap(),
                source_agent_id: AgentId::new("main").unwrap(),
                operation_id: id.into(),
            },
            call: ToolCall::new(id, name, input),
            plan_guard: if plan {
                PlanGuard::read_only()
            } else {
                PlanGuard::inactive()
            },
            cancellation: lease.cancellation.clone(),
            lifecycle: Arc::new(NoopControlledToolLifecycleSink),
        })
        .await
        .map(|result| result.result)
}
fn value(result: rcode_model::ToolResult) -> Value {
    assert!(!result.is_error, "{result:?}");
    let rcode_model::ToolResultContent::Text { text } = &result.content[0] else {
        panic!("text result")
    };
    serde_json::from_str(text).unwrap()
}
fn events() -> (Arc<EventBridge>, Arc<Mutex<Vec<Value>>>) {
    let values = Arc::new(Mutex::new(vec![]));
    let sink = values.clone();
    (
        EventBridge::new(move |event| {
            sink.lock().unwrap().push(event);
            Ok(())
        }),
        values,
    )
}

#[tokio::test]
async fn shared_todo_validates_full_replacements_emits_projection_and_isolates_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let runtime = AgentRuntime::default();
    let lease = runtime.register("run", "session").unwrap();
    let (events, received) = events();
    let runner = runner(
        &runtime,
        registry(&runtime, &root, "session", false, vec![], events.clone()),
        &lease,
        events.clone(),
        PermissionMode::Ask,
    );
    let item = |status| json!({"content":"inspect","active_form":"inspecting","status":status});
    assert!(invoke(&runner,&lease,"invalid","todo_write",json!({"todos":[item("in_progress"),{"content":"fix","active_form":"fixing","status":"in_progress"}]}),false).await.is_error);
    assert!(received.lock().unwrap().is_empty());
    value(
        invoke(
            &runner,
            &lease,
            "valid",
            "todo_write",
            json!({"todos":[item("in_progress")]}),
            true,
        )
        .await,
    );
    assert_eq!(received.lock().unwrap()[0]["sessionId"], "session");
    assert_eq!(
        received.lock().unwrap()[0]["todos"][0]["content"],
        "inspect"
    );
    let done = value(
        invoke(
            &runner,
            &lease,
            "done",
            "todo_write",
            json!({"todos":[item("completed")]}),
            false,
        )
        .await,
    );
    assert_eq!(done["current_todos"], json!([]));
    assert_eq!(
        received.lock().unwrap().last().unwrap()["todos"][0]["status"],
        "completed"
    );
    let _other = registry(&runtime, &root, "other", false, vec![], events);
    assert!(runtime.0.lock().unwrap().todos["other"]
        .todo_snapshot()
        .unwrap()
        .items
        .is_empty());
    assert!(runtime.0.lock().unwrap().todos["session"]
        .todo_snapshot()
        .unwrap()
        .items
        .is_empty());
}

#[tokio::test]
async fn shared_background_enforces_owner_paths_plan_approval_and_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let runtime = AgentRuntime::default();
    let lease = runtime.register("run", "session").unwrap();
    let (events, _) = events();
    let tools = registry(&runtime, &root, "session", false, vec![], events.clone());
    let running = runner(
        &runtime,
        tools.clone(),
        &lease,
        events.clone(),
        PermissionMode::FullAccess,
    );
    let plan = registry(&runtime, &root, "session", true, vec![], events.clone());
    for name in ["bash_background", "bash_kill"] {
        assert!(!plan.definitions().iter().any(|d| d.name == name));
    }
    assert!(
        invoke(
            &running,
            &lease,
            "plan",
            "bash_background",
            json!({"command":"echo forbidden"}),
            true
        )
        .await
        .is_error
    );
    assert!(
        invoke(
            &running,
            &lease,
            "outside",
            "bash_background",
            json!({"command":"echo forbidden","cwd":"../"}),
            false
        )
        .await
        .is_error
    );
    let approval_runtime = runtime.clone();
    let deny = EventBridge::new(move |event| {
        if event["type"] == "approval" {
            approval_runtime.approve(event["id"].as_str().unwrap(), false)?;
        }
        Ok(())
    });
    let asking = runner(&runtime, tools, &lease, deny, PermissionMode::Ask);
    assert!(
        invoke(
            &asking,
            &lease,
            "denied",
            "bash_background",
            json!({"command":"echo forbidden"}),
            false
        )
        .await
        .is_error
    );
    #[cfg(unix)]
    let command = "printf stdout; printf stderr >&2; sleep 60";
    #[cfg(windows)]
    let command = "Write-Output stdout; [Console]::Error.WriteLine('stderr'); Start-Sleep 60";
    let started = value(
        invoke(
            &running,
            &lease,
            "start",
            "bash_background",
            json!({"command":command}),
            false,
        )
        .await,
    );
    let handle = started["handle"].as_str().unwrap();
    let foreign = runtime.register("foreign-run", "foreign-session").unwrap();
    let foreign_runner = runner(
        &runtime,
        registry(
            &runtime,
            &root,
            "foreign-session",
            false,
            vec![],
            events.clone(),
        ),
        &foreign,
        events,
        PermissionMode::FullAccess,
    );
    assert!(
        invoke(
            &foreign_runner,
            &foreign,
            "foreign-logs",
            "bash_logs",
            json!({"handle":handle}),
            false
        )
        .await
        .is_error
    );
    assert!(
        invoke(
            &foreign_runner,
            &foreign,
            "foreign-kill",
            "bash_kill",
            json!({"handle":handle}),
            false
        )
        .await
        .is_error
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let output = value(
            invoke(
                &running,
                &lease,
                "logs",
                "bash_logs",
                json!({"handle":handle}),
                false,
            )
            .await,
        );
        if output["stdout"].as_str().unwrap().contains("stdout")
            && output["stderr"].as_str().unwrap().contains("stderr")
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 普通 Turn 取消不会结束显式后台任务，应用退出必须等待进程树结束。
    lease.cancellation.cancel();
    assert_eq!(
        runtime
            .background(&root.join("output"))
            .unwrap()
            .list_running()
            .unwrap()
            .len(),
        1
    );
    runtime.shutdown();
    runtime.shutdown_background().await.unwrap();
    assert!(runtime
        .background(&root.join("output"))
        .unwrap()
        .list_running()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn shared_subagent_uses_selected_model_and_restricted_tools_and_releases_lease() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let runtime = AgentRuntime::default();
    let lease = runtime.register("parent", "session").unwrap();
    let (events, _) = events();
    let child = provider(vec![
        call_reply(
            "Write",
            json!({"file_path":"forbidden.txt","content":"must not write"}),
        ),
        text_reply("shared child summary"),
    ]);
    let agents = vec![ResolvedSubagent {
        template: builtin_subagents()[0].clone(),
        model: "selected-child-model".into(),
        provider: child.clone(),
    }];
    let running = runner(
        &runtime,
        registry(&runtime, &root, "session", false, agents, events.clone()),
        &lease,
        events,
        PermissionMode::FullAccess,
    );
    let result = value(
        invoke(
            &running,
            &lease,
            "child",
            "run_subagent",
            json!({"type":"explore","prompt":"investigate"}),
            false,
        )
        .await,
    );
    assert_eq!(result["summary"], "shared child summary");
    assert_eq!(result["stepCount"], 2);
    assert!(!root.join("forbidden.txt").exists());
    let requests = child.requests().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].model, "selected-child-model");
    let names: Vec<_> = requests[0].tools.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Glob", "Grep", "Read", "list_directory"]);
    let state = runtime.0.lock().unwrap();
    assert_eq!(state.runs.len(), 1);
    assert!(state.environments.keys().all(|(id, _)| id == "session"));
}

struct WaitingProvider;
impl ModelProvider for WaitingProvider {
    fn capabilities(&self, _: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            ..Default::default()
        }
    }
    fn stream(
        &self,
        _: rcode_model::ModelRequest,
    ) -> rcode_model::ModelFuture<'_, Result<rcode_model::ModelStream, rcode_model::ModelError>>
    {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn parent_cancel_stops_child_and_releases_runtime_state() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let runtime = AgentRuntime::default();
    let lease = runtime.register("parent", "session").unwrap();
    let (events, _) = events();
    let agents = vec![ResolvedSubagent {
        template: builtin_subagents()[0].clone(),
        model: "waiting".into(),
        provider: Arc::new(WaitingProvider),
    }];
    let running = runner(
        &runtime,
        registry(&runtime, &root, "session", false, agents, events.clone()),
        &lease,
        events,
        PermissionMode::FullAccess,
    );
    let call = invoke_result(
        &running,
        &lease,
        "child",
        "run_subagent",
        json!({"type":"explore","prompt":"wait"}),
        false,
    );
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        lease.cancellation.cancel();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(call, cancel) })
            .await
            .unwrap();
    assert!(matches!(result, Err(rcode_agent::AgentRunError::Cancelled)));
    assert_eq!(runtime.0.lock().unwrap().runs.len(), 1);
    assert!(runtime.0.lock().unwrap().environments.is_empty());
}
