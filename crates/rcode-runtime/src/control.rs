use crate::EventBridge;
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

#[derive(Clone, Default)]
pub struct AgentRuntime(pub(crate) Arc<Mutex<RunRegistry>>);

#[derive(Default)]
pub(crate) struct RunRegistry {
    pub runs: HashMap<String, (String, TurnCancellation)>,
    approvals: HashMap<String, oneshot::Sender<bool>>,
    pub(crate) tool_responses: HashMap<String, oneshot::Sender<serde_json::Value>>,
    pub(crate) environments:
        HashMap<(String, std::path::PathBuf), Arc<rcode_tools::ToolEnvironment>>,
}

impl AgentRuntime {
    // 命令和验收入口复用同一消费式审批路由，重复响应不能恢复第二次执行。
    pub fn approve(&self, approval_id: &str, approved: bool) -> Result<(), String> {
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

    pub fn cancel(&self, run_id: &str) -> Result<(), String> {
        let registry = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if let Some((_, cancellation)) = registry.runs.get(run_id) {
            cancellation.cancel();
        }
        Ok(())
    }

    // 响应仍受原来的大小上限约束；关闭或取消后，旧请求不能再次提交结果。
    pub fn respond_to_tool(
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

    pub fn register(&self, id: &str, session: &str) -> Result<RunLease, String> {
        rcode_agent::TurnId::new(id).map_err(|e| e.to_string())?;
        rcode_agent::SessionId::new(session).map_err(|e| e.to_string())?;
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
            session_id: session.into(),
            state: self.0.clone(),
            cancellation,
        })
    }
}

pub struct RunLease {
    pub(crate) id: String,
    pub(crate) state: Arc<Mutex<RunRegistry>>,
    pub cancellation: TurnCancellation,
    pub(crate) session_id: String,
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

pub(crate) struct ApprovalGate {
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
