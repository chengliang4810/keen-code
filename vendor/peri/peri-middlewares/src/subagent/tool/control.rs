use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::agent::async_tasks::{AgentFollowupDelivery, TaskManager};
use peri_agent::messages::BaseMessage;
use peri_agent::thread::{AgentStatus, ThreadMeta, ThreadStore};
use peri_agent::tools::BaseTool;
use serde_json::json;

use super::SubAgentTool;

const FOLLOWUP_AGENT_DESCRIPTION: &str = "Continue or adjust an existing non-root Agent. Running Agents receive the message at the next boundary; inactive, interrupted, or failed Agents resume automatically on the same thread.";
const INTERRUPT_AGENT_DESCRIPTION: &str = "Interrupt an agent's current turn, if any, and return its previous status. The agent remains available for messages and follow-up tasks.";
const LIST_AGENTS_DESCRIPTION: &str = "List every direct child Agent of the current task with its child_thread_id, type, and current status.";

fn parent_thread_id(agent: &SubAgentTool) -> Result<String, String> {
    let host = agent
        .host()
        .ok_or_else(|| "Agent runtime is not available".to_string())?;
    agent
        .parent_session
        .read()
        .as_ref()
        .and_then(|session| session.store().thread_id.clone())
        .or_else(|| host.parent_thread_id.clone())
        .ok_or_else(|| "current parent thread id is not available".to_string())
}

struct AgentTarget {
    thread_id: String,
    meta: ThreadMeta,
    thread_store: Arc<dyn ThreadStore>,
    task_manager: Arc<TaskManager>,
}

async fn resolve_target(agent: &SubAgentTool, target: &str) -> Result<AgentTarget, String> {
    let thread_id = target.trim();
    if uuid::Uuid::parse_str(thread_id).is_err() {
        return Err(format!("invalid agent target: {target}"));
    }

    let host = agent
        .host()
        .ok_or_else(|| "Agent runtime is not available".to_string())?;
    let thread_store = host
        .thread_store
        .clone()
        .ok_or_else(|| "Agent thread store is not available".to_string())?;
    let task_manager = host
        .task_manager
        .clone()
        .ok_or_else(|| "Agent task manager is not available".to_string())?;
    let meta = thread_store
        .load_meta(&thread_id.to_string())
        .await
        .map_err(|_| format!("agent target not found: {thread_id}"))?;
    let parent_thread_id = parent_thread_id(agent)?;

    match meta.parent_thread_id.as_deref() {
        None => return Err("root is not a spawned agent".to_string()),
        Some(parent) if parent != parent_thread_id => {
            return Err(format!(
                "agent target belongs to another parent: {thread_id}"
            ));
        }
        Some(_) => {}
    }

    Ok(AgentTarget {
        thread_id: thread_id.to_string(),
        meta,
        thread_store,
        task_manager,
    })
}

#[derive(Clone)]
pub struct ListAgentsTool {
    agent: SubAgentTool,
}

impl ListAgentsTool {
    pub fn new(agent: SubAgentTool) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl BaseTool for ListAgentsTool {
    fn name(&self) -> &str {
        "ListAgents"
    }

    fn description(&self) -> &str {
        LIST_AGENTS_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let host = self.agent.host().ok_or("Agent runtime is not available")?;
        let store = host
            .thread_store
            .as_ref()
            .ok_or("Agent thread store is not available")?;
        let parent = parent_thread_id(&self.agent)?;
        let agents = store
            .list_child_threads(&parent)
            .await?
            .into_iter()
            .map(|meta| {
                json!({
                    "child_thread_id": meta.id,
                    "agent_type": meta.title,
                    "status": meta.agent_status,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "agents": agents }).to_string())
    }
}

#[derive(Clone)]
pub struct FollowupAgentTool {
    agent: SubAgentTool,
}

impl FollowupAgentTool {
    pub fn new(agent: SubAgentTool) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl BaseTool for FollowupAgentTool {
    fn name(&self) -> &str {
        "FollowupAgent"
    }

    fn description(&self) -> &str {
        FOLLOWUP_AGENT_DESCRIPTION
    }

    fn is_direct(&self) -> bool {
        true
    }

    fn namespace(&self) -> Option<&str> {
        Some("interaction")
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["target", "message"],
            "properties": {
                "target": {
                    "type": "string",
                    "description": "The child_thread_id returned by Agent."
                },
                "message": {
                    "type": "string",
                    "description": "Message text to send to the target agent."
                }
            }
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let target = input
            .get("target")
            .and_then(|value| value.as_str())
            .ok_or("missing required parameter target")?;
        let message = input
            .get("message")
            .and_then(|value| value.as_str())
            .ok_or("missing required parameter message")?;
        if message.trim().is_empty() {
            return Err("Empty message can't be sent to an agent".into());
        }

        let target = resolve_target(&self.agent, target).await?;
        let mut changes = target.task_manager.subscribe_agent_changes();
        match target
            .task_manager
            .deliver_agent_followup(&target.thread_id, BaseMessage::human(message.to_string()))
        {
            AgentFollowupDelivery::Delivered { .. } => Ok(String::new()),
            AgentFollowupDelivery::Finishing { .. } => {
                while target
                    .task_manager
                    .running_agent_task(&target.thread_id)
                    .is_some()
                {
                    changes
                        .changed()
                        .await
                        .map_err(|_| "Agent task notifications closed")?;
                }
                self.agent
                    .invoke_resume(target.thread_id, Some(message.to_string()), target.meta.cwd)
                    .await?;
                Ok(String::new())
            }
            AgentFollowupDelivery::NotRunning => {
                self.agent
                    .invoke_resume(target.thread_id, Some(message.to_string()), target.meta.cwd)
                    .await?;
                Ok(String::new())
            }
            AgentFollowupDelivery::Unavailable { task_id } => {
                Err(format!("Agent task {task_id} cannot receive follow-up tasks").into())
            }
        }
    }
}

#[derive(Clone)]
pub struct InterruptAgentTool {
    agent: SubAgentTool,
}

impl InterruptAgentTool {
    pub fn new(agent: SubAgentTool) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl BaseTool for InterruptAgentTool {
    fn name(&self) -> &str {
        "InterruptAgent"
    }

    fn description(&self) -> &str {
        INTERRUPT_AGENT_DESCRIPTION
    }

    fn is_direct(&self) -> bool {
        true
    }

    fn namespace(&self) -> Option<&str> {
        Some("interaction")
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": {
                    "type": "string",
                    "description": "The child_thread_id returned by Agent."
                }
            }
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let target = input
            .get("target")
            .and_then(|value| value.as_str())
            .ok_or("missing required parameter target")?;
        let target = resolve_target(&self.agent, target).await?;
        let previous_status = target.meta.agent_status;

        if previous_status == AgentStatus::Active {
            target
                .task_manager
                .interrupt_agent(&target.thread_id)
                .await?;
            let meta = target.thread_store.load_meta(&target.thread_id).await?;
            if meta.agent_status == AgentStatus::Active {
                target
                    .thread_store
                    .update_thread_status(&target.thread_id, AgentStatus::Cancelled.as_str())
                    .await?;
            }
        }

        Ok(json!({ "previous_status": previous_status }).to_string())
    }
}
