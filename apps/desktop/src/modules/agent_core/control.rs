use rcode_runtime::AgentRuntime;

#[tauri::command]
pub fn agent_core_cancel(
    state: tauri::State<'_, AgentRuntime>,
    run_id: String,
) -> Result<(), String> {
    state.cancel(&run_id)
}

#[tauri::command]
pub fn agent_core_approve(
    state: tauri::State<'_, AgentRuntime>,
    approval_id: String,
    approved: bool,
) -> Result<(), String> {
    state.approve(&approval_id, approved)
}

#[tauri::command]
pub fn agent_core_tool_result(
    state: tauri::State<'_, AgentRuntime>,
    request_id: String,
    result: serde_json::Value,
) -> Result<(), String> {
    state.respond_to_tool(&request_id, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcode_agent::{
        AgentId, AgentRunner, PlanGuard, RunLimits, SessionId, ToolEffect, ToolRegistry, TurnId,
        TurnRequest,
    };
    use rcode_model::{
        ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities,
        ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
    };
    use rcode_runtime::{EventBridge, PermissionMode};
    use rcode_tools::ToolEnvironment;
    use serde_json::json;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    async fn run_approval_roundtrip(
        decision: Option<bool>,
        permission_mode: PermissionMode,
        plan_mode: bool,
        file_path: &str,
    ) {
        let automatic = permission_mode.auto_approves("Write", ToolEffect::ChangesState);
        let expected_approvals = usize::from(!automatic && !plan_mode);
        let approved =
            (automatic || decision == Some(true)) && !plan_mode && file_path == "proof.txt";
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(file_path);
        let state = Arc::new(AgentRuntime::default());
        let lease = state.register("proof-run", "proof-session").unwrap();
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received_for_channel = received.clone();
        let approval_state = state.clone();
        let approval_target = target.clone();
        let events = EventBridge::new(move |event: Value| {
            if event["type"] == "approval" {
                // 审批触发时文件必须尚未落盘，回复直接恢复同一 Rust 循环。
                assert!(!approval_target.exists());
                let id = event["id"].as_str().unwrap();
                assert_eq!(id, "rcode:proof-run:call-1");
                if let Some(approved) = decision {
                    approval_state.approve(id, approved).unwrap();
                    assert!(approval_state.approve(id, approved).is_err());
                } else {
                    approval_state.cancel("proof-run").unwrap();
                }
            }
            received_for_channel.lock().unwrap().push(event);
            Ok(())
        });
        let provider = Arc::new(ScriptedProvider::new(
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                ..Default::default()
            },
            [
                ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::ToolCallStart {
                        index: 0,
                        id: "call-1".into(),
                        name: "Write".into(),
                    },
                    ModelStreamEvent::ToolCallArgumentsDelta {
                        index: 0,
                        id: "call-1".into(),
                        delta: json!({"file_path":file_path,"content":"verified"}).to_string(),
                    },
                    ModelStreamEvent::ToolCallEnd {
                        index: 0,
                        id: "call-1".into(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: StopReason::ToolUse,
                    },
                ]),
                ScriptedReply::events([
                    ModelStreamEvent::MessageStart {
                        metadata: ResponseMetadata::default(),
                    },
                    ModelStreamEvent::TextDelta {
                        index: 0,
                        delta: "complete".into(),
                    },
                    ModelStreamEvent::MessageEnd {
                        stop_reason: StopReason::Completed,
                    },
                ]),
            ],
        ));
        let mut tools = ToolRegistry::new();
        let root = directory.path().canonicalize().unwrap();
        rcode_runtime::register_workspace_tools(
            &mut tools,
            Arc::new(
                ToolEnvironment::new(&root)
                    .unwrap()
                    .with_workspace_guard()
                    .with_file_access_policy(Arc::new(
                        rcode_runtime::security::WorkspacePathPolicy(root.clone()),
                    )),
            ),
            &root,
            false,
        )
        .unwrap();
        let runner = AgentRunner::new(provider.clone(), tools, RunLimits::new(4, 4).unwrap())
            .with_event_sink(events.clone())
            .with_commit_sink(events.clone())
            .with_tool_approval_gate(state.approval_gate(
                "proof-run",
                events,
                permission_mode,
                plan_mode,
            ));
        let mut request = TurnRequest::new(
            SessionId::new("proof-session").unwrap(),
            TurnId::new("proof-run").unwrap(),
            AgentId::new("main").unwrap(),
            "proof-model",
            vec![Message::text(MessageRole::User, "write proof")],
            if plan_mode {
                PlanGuard::read_only()
            } else {
                PlanGuard::inactive()
            },
        );
        request.set_cancellation(lease.cancellation.clone());
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), runner.run_turn(request))
                .await
                .unwrap();
        assert_eq!(target.exists(), approved);
        if decision.is_none() && expected_approvals > 0 {
            assert!(lease.cancellation.is_cancelled());
            assert_eq!(provider.requests().unwrap().len(), 1);
            drop(lease);

            assert!(state.approve("rcode:proof-run:call-1", true).is_err());
            assert!(state.register("next-run", "proof-session").is_ok());
            return;
        }
        assert!(result.error.is_none(), "{:?}", result.error);
        if approved {
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "verified");
        }
        let requests = provider.requests().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().flat_map(|message| &message.content).any(|block|
            matches!(block, ContentBlock::ToolResult { tool_result } if tool_result.tool_call_id == "call-1" && tool_result.is_error != approved)));
        let received = received.lock().unwrap();
        if expected_approvals > 0 {
            let input_position = received
                .iter()
                .position(|event| event["type"] == "tool_input")
                .unwrap();
            let approval_position = received
                .iter()
                .position(|event| event["type"] == "approval")
                .unwrap();
            assert!(input_position < approval_position);
        }
        assert_eq!(
            received
                .iter()
                .filter(|event| event["type"] == "tool_input")
                .count(),
            1
        );
        assert_eq!(
            received
                .iter()
                .filter(|event| event["type"] == "approval")
                .count(),
            expected_approvals
        );
        assert_eq!(
            received
                .iter()
                .filter(|event| event["type"] == "tool_result")
                .count(),
            1
        );
        let mut parts: Vec<_> = received
            .iter()
            .filter(|event| event["type"] == "transcript")
            .map(
                |event| json!({"type":"data-rcode-messages","data":{"messages":event["messages"]}}),
            )
            .collect();
        parts.push(json!({"type":"text","text":"duplicate display projection"}));
        let replay = super::super::history::model_history(&[
            json!({"role":"user","parts":[{"type":"text","text":"write proof"}]}),
            json!({"role":"assistant","parts":parts}),
        ])
        .unwrap();
        assert_eq!(replay, *result.messages);
    }

    #[tokio::test]
    async fn approved_native_write_resumes_and_replays_one_authoritative_tool_pair() {
        run_approval_roundtrip(Some(true), PermissionMode::Ask, false, "proof.txt").await;
    }

    #[tokio::test]
    async fn denied_native_write_keeps_disk_unchanged_and_returns_denial_to_model() {
        run_approval_roundtrip(Some(false), PermissionMode::Ask, false, "proof.txt").await;
    }

    #[tokio::test]
    async fn cancelled_approval_never_writes_and_releases_session_lease() {
        run_approval_roundtrip(None, PermissionMode::Ask, false, "proof.txt").await;
    }

    #[tokio::test]
    async fn automatic_permissions_resume_without_a_manual_approval() {
        for mode in [PermissionMode::Edit, PermissionMode::FullAccess] {
            run_approval_roundtrip(None, mode, false, "proof.txt").await;
        }
    }

    #[tokio::test]
    async fn full_access_cannot_bypass_plan_guard_or_secret_path_policy() {
        run_approval_roundtrip(None, PermissionMode::FullAccess, true, "proof.txt").await;
        run_approval_roundtrip(None, PermissionMode::FullAccess, false, ".env.local").await;
    }
}
