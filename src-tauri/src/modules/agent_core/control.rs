use super::events::EventBridge;
use rcode_agent::{
    ToolApprovalDecision, ToolApprovalError, ToolApprovalFuture, ToolApprovalGate,
    ToolApprovalRequest, ToolEffect, TurnCancellation,
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

#[derive(Default)]
pub struct AgentCoreState(pub(super) Arc<Mutex<RunRegistry>>);

#[derive(Default)]
pub(super) struct RunRegistry {
    pub runs: HashMap<String, (String, TurnCancellation)>,
    approvals: HashMap<String, oneshot::Sender<bool>>,
    pub(super) tool_responses: HashMap<String, oneshot::Sender<serde_json::Value>>,
    pub(super) environments:
        HashMap<(String, std::path::PathBuf), Arc<rcode_tools::ToolEnvironment>>,
}

impl AgentCoreState {
    // 命令和验收入口复用同一消费式审批路由，重复响应不能恢复第二次执行。
    pub(super) fn approve(&self, approval_id: &str, approved: bool) -> Result<(), String> {
        let sender = self
            .0
            .lock()
            .map_err(|_| "Agent 状态锁不可用")?
            .approvals
            .remove(approval_id)
            .ok_or("工具审批已失效或已经处理")?;
        sender
            .send(approved)
            .map_err(|_| "工具审批等待已结束".into())
    }

    pub(super) fn cancel(&self, run_id: &str) -> Result<(), String> {
        let registry = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if let Some((_, cancellation)) = registry.runs.get(run_id) {
            cancellation.cancel();
        }
        Ok(())
    }

    // 响应仍受原来的大小上限约束；关闭或取消后，旧请求不能再次提交结果。
    pub(super) fn respond_to_tool(
        &self,
        request_id: &str,
        result: serde_json::Value,
    ) -> Result<(), String> {
        if serde_json::to_vec(&result)
            .map_err(|e| e.to_string())?
            .len()
            > 1024 * 1024
        {
            return Err("界面工具返回值超过 1 MiB 上限".into());
        }
        let sender = self
            .0
            .lock()
            .map_err(|_| "Agent 状态锁不可用")?
            .tool_responses
            .remove(request_id)
            .ok_or("界面工具请求已结束")?;
        sender.send(result).map_err(|_| "界面工具等待已结束".into())
    }

    pub fn shutdown(&self) {
        if let Ok(mut registry) = self.0.lock() {
            for (_, token) in registry.runs.values() {
                token.cancel();
            }
            registry.approvals.clear();
            registry.tool_responses.clear();
        }
    }

    pub(super) fn register(&self, id: &str, session: &str) -> Result<RunLease, String> {
        let mut registry = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if registry.runs.len() >= 8
            || registry.runs.contains_key(id)
            || registry.runs.values().any(|(active, _)| active == session)
        {
            return Err("当前对话已有 Agent 正在运行，或并发任务达到上限".into());
        }
        let cancellation = TurnCancellation::new();
        registry
            .runs
            .insert(id.into(), (session.into(), cancellation.clone()));
        Ok(RunLease {
            id: id.into(),
            state: self.0.clone(),
            cancellation,
        })
    }
}

pub(super) struct RunLease {
    id: String,
    state: Arc<Mutex<RunRegistry>>,
    pub cancellation: TurnCancellation,
}

impl Drop for RunLease {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut registry) = self.state.lock() {
            registry.runs.remove(&self.id);
            let prefix = format!("rcode:{}:", self.id);
            registry.approvals.retain(|id, _| !id.starts_with(&prefix));
            registry
                .tool_responses
                .retain(|id, _| !id.starts_with(&prefix));
        }
    }
}

pub(super) struct ApprovalGate {
    pub run_id: String,
    pub state: Arc<Mutex<RunRegistry>>,
    pub events: Arc<EventBridge>,
    pub permission_mode: super::permissions::PermissionMode,
    pub plan_mode: bool,
}

impl ToolApprovalGate for ApprovalGate {
    fn request(
        &self,
        request: ToolApprovalRequest,
        cancellation: TurnCancellation,
    ) -> ToolApprovalFuture<'_> {
        Box::pin(async move {
            if request.effect == ToolEffect::ReadOnly {
                return Ok(ToolApprovalDecision::Approved);
            }
            if self.plan_mode {
                return Ok(ToolApprovalDecision::Denied {
                    reason: "计划模式只允许读取操作".into(),
                });
            }
            if self
                .permission_mode
                .auto_approves(&request.tool_name, request.effect)
            {
                return Ok(ToolApprovalDecision::Approved);
            }
            let id = format!("rcode:{}:{}", self.run_id, request.tool_call_id.as_str());
            let (sender, receiver) = oneshot::channel();
            self.state
                .lock()
                .map_err(|_| ToolApprovalError::ConnectionClosed)?
                .approvals
                .insert(id.clone(), sender);
            // 核心先审批再冻结工具请求；UI 必须先收到 input 才能接收 approval。
            let emit = self.events.emit(json!({"type":"tool_input", "id":request.tool_call_id.as_str(),
                "name":request.tool_name,"input":request.input})).and_then(|_| {
                self.events.emit(json!({"type":"approval", "id":id,
                    "toolCallId":request.tool_call_id.as_str(),"name":request.tool_name,"input":request.input}))
            });
            let decision = if emit.is_err() {
                Err(ToolApprovalError::ConnectionClosed)
            } else {
                tokio::select! {
                    _ = cancellation.cancelled() => Ok(ToolApprovalDecision::Cancelled),
                    result = receiver => match result {
                        Ok(true) => Ok(ToolApprovalDecision::Approved),
                        Ok(false) => Ok(ToolApprovalDecision::Denied { reason:"用户拒绝工具操作".into() }),
                        Err(_) => Err(ToolApprovalError::ConnectionClosed),
                    }
                }
            };
            if let Ok(mut registry) = self.state.lock() {
                registry.approvals.remove(&id);
            }
            decision
        })
    }
}

#[tauri::command]
pub fn agent_core_cancel(
    state: tauri::State<'_, AgentCoreState>,
    run_id: String,
) -> Result<(), String> {
    state.cancel(&run_id)
}

#[tauri::command]
pub fn agent_core_approve(
    state: tauri::State<'_, AgentCoreState>,
    approval_id: String,
    approved: bool,
) -> Result<(), String> {
    state.approve(&approval_id, approved)
}

#[tauri::command]
pub fn agent_core_tool_result(
    state: tauri::State<'_, AgentCoreState>,
    request_id: String,
    result: serde_json::Value,
) -> Result<(), String> {
    state.respond_to_tool(&request_id, result)
}

#[cfg(test)]
mod tests {
    use super::super::permissions::PermissionMode;
    use super::*;
    use rcode_agent::{
        AgentId, AgentRunner, PlanGuard, RunLimits, SessionId, ToolRegistry, TurnId, TurnRequest,
    };
    use rcode_model::{
        ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities,
        ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
    };
    use rcode_tools::ToolEnvironment;
    use serde_json::Value;
    use tauri::ipc::Channel;

    #[test]
    fn one_run_per_session_and_lease_cleanup_are_enforced() {
        let state = AgentCoreState::default();
        let lease = state.register("run-1", "session-1").unwrap();
        assert!(state.register("run-2", "session-1").is_err());
        let token = lease.cancellation.clone();
        let (sender, receiver) = oneshot::channel();
        state
            .0
            .lock()
            .unwrap()
            .tool_responses
            .insert("rcode:run-1:tool-1".into(), sender);
        drop(lease);
        assert!(receiver.blocking_recv().is_err());
        assert!(token.is_cancelled());
        assert!(state.register("run-2", "session-1").is_ok());
    }

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
        let state = Arc::new(AgentCoreState::default());
        let lease = state.register("proof-run", "proof-session").unwrap();
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received_for_channel = received.clone();
        let approval_state = state.clone();
        let approval_target = target.clone();
        let events = EventBridge::new(Channel::new(move |body| {
            let event: Value = body.deserialize().unwrap();
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
        }));
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
        super::super::workspace_tools::register_workspace_tools(
            &mut tools,
            Arc::new(
                ToolEnvironment::new(&root)
                    .unwrap()
                    .with_workspace_guard()
                    .with_file_access_policy(Arc::new(
                        super::super::security::WorkspacePathPolicy(root.clone()),
                    )),
            ),
            &root,
            false,
        )
        .unwrap();
        let runner = AgentRunner::new(provider.clone(), tools, RunLimits::new(4, 4).unwrap())
            .with_event_sink(events.clone())
            .with_commit_sink(events.clone())
            .with_tool_approval_gate(Arc::new(ApprovalGate {
                run_id: "proof-run".into(),
                state: state.0.clone(),
                events,
                permission_mode,
                plan_mode,
            }));
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
            assert!(state.0.lock().unwrap().approvals.is_empty());
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

    #[test]
    fn client_result_limit_preserves_pending_request_and_response_is_consumed_once() {
        let state = AgentCoreState::default();
        let lease = state.register("run-1", "session-1").unwrap();
        let (sender, receiver) = oneshot::channel();
        state
            .0
            .lock()
            .unwrap()
            .tool_responses
            .insert("rcode:run-1:call-1".into(), sender);
        assert!(state
            .respond_to_tool("rcode:run-1:call-1", json!("x".repeat(1024 * 1024)))
            .is_err());
        state
            .respond_to_tool("rcode:run-1:call-1", json!({"ok":true}))
            .unwrap();
        assert!(state
            .respond_to_tool("rcode:run-1:call-1", json!({"ok":false}))
            .is_err());
        assert_eq!(receiver.blocking_recv().unwrap(), json!({"ok":true}));
        drop(lease);
    }
}
