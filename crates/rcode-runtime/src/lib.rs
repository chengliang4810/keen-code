mod control;
mod events;
mod model;
pub mod network;
mod permissions;
pub mod security;
pub mod storage;
mod workspace_tools;

pub use control::{AgentRuntime, RunLease};
pub use events::EventBridge;
pub use model::ModelConfig;
pub use permissions::PermissionMode;
pub use workspace_tools::register_workspace_tools;

use rcode_agent::{
    AgentRunner, PlanGuardState, RunLimits, ToolApprovalGate, ToolRegistry, TurnRequest, TurnResult,
};
use rcode_model::ModelProvider;
use rcode_tools::ToolEnvironment;
use serde_json::Value;
use std::{collections::HashSet, path::Path, sync::Arc};
use tokio::sync::oneshot;

impl AgentRuntime {
    pub fn approval_gate(
        &self,
        run_id: &str,
        events: Arc<EventBridge>,
        permission_mode: PermissionMode,
        plan_mode: bool,
    ) -> Arc<dyn ToolApprovalGate> {
        Arc::new(control::ApprovalGate {
            run_id: run_id.into(),
            state: self.0.clone(),
            events,
            permission_mode,
            plan_mode,
        })
    }

    pub async fn run_turn(
        &self,
        lease: &RunLease,
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        mut request: TurnRequest,
        events: Arc<EventBridge>,
        permission_mode: PermissionMode,
    ) -> Result<TurnResult, String> {
        if !Arc::ptr_eq(&lease.state, &self.0)
            || request.turn_id().as_str() != lease.id
            || request.session_id().as_str() != lease.session_id
        {
            return Err("Agent 请求与运行租约不匹配".into());
        }
        let plan_mode = request.plan_guard().state() == PlanGuardState::ReadOnly;
        request.set_cancellation(lease.cancellation.clone());
        let runner = AgentRunner::new(
            provider,
            tools,
            RunLimits::new(24, 256).expect("static run limits"),
        )
        .with_event_sink(events.clone())
        .with_commit_sink(events.clone())
        .with_tool_approval_gate(self.approval_gate(
            &lease.id,
            events,
            permission_mode,
            plan_mode,
        ));
        Ok(runner.run_turn(request).await)
    }

    pub fn environment(
        &self,
        session_id: &str,
        root: &Path,
        artifacts: Option<&Path>,
    ) -> Result<Arc<ToolEnvironment>, String> {
        let key = (session_id.to_owned(), root.to_path_buf());
        let mut registry = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if let Some(environment) = registry.environments.get(&key) {
            return Ok(environment.clone());
        }
        let mut environment = ToolEnvironment::new(root)
            .map_err(|error| error.to_string())?
            .with_workspace_guard()
            .with_file_access_policy(Arc::new(security::WorkspacePathPolicy(root.into())));
        if let Some(directory) = artifacts {
            environment = environment
                .with_artifact_directory(directory)
                .map_err(|error| error.to_string())?;
        }
        if registry.environments.len() >= 16 {
            let active: HashSet<_> = registry
                .runs
                .values()
                .map(|(session, _)| session.clone())
                .collect();
            registry
                .environments
                .retain(|(session, _), _| active.contains(session));
        }
        let environment = Arc::new(environment);
        registry.environments.insert(key, environment.clone());
        Ok(environment)
    }

    pub fn request_client_tool(
        &self,
        run_id: &str,
        call_id: &str,
    ) -> Result<(String, oneshot::Receiver<Value>), String> {
        let id = format!("rcode:{run_id}:{call_id}");
        let mut registry = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if !registry.runs.contains_key(run_id) || registry.tool_responses.contains_key(&id) {
            return Err("客户端工具请求没有有效运行或调用 ID 重复".into());
        }
        let (sender, receiver) = oneshot::channel();
        registry.tool_responses.insert(id.clone(), sender);
        Ok((id, receiver))
    }

    pub fn finish_client_tool(&self, request_id: &str) {
        if let Ok(mut registry) = self.0.lock() {
            registry.tool_responses.remove(request_id);
        }
    }
}

#[cfg(test)]
mod tests;
