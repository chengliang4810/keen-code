use rcode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    ToolRegistry,
};
use rcode_model::ToolDefinition;
use rcode_runtime::{AgentRuntime, EventBridge};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc};

const CLIENT_TOOLS: &[&str] = &[
    "memory_read",
    "memory_write",
    "memory_delete",
    "list_directory",
    "create_directory",
    "bash_background",
    "bash_logs",
    "bash_list",
    "bash_kill",
    "suggest_command",
    "get_terminal_output",
    "open_preview",
    "todo_write",
    "run_subagent",
    "spawn_coding_agent",
    "send_to_agent",
    "read_agent_output",
];
const CLIENT_READ_ONLY: &[&str] = &[
    "memory_read",
    "list_directory",
    "bash_logs",
    "bash_list",
    "get_terminal_output",
    "read_agent_output",
    "suggest_command",
    "todo_write",
    "open_preview",
    "run_subagent",
];

pub(super) fn validate_client_tools(tools: &[ToolDefinition]) -> Result<(), String> {
    let mut names = HashSet::new();
    if tools.len() > CLIENT_TOOLS.len() {
        return Err("界面工具数量超过上限".into());
    }
    for tool in tools {
        tool.validate().map_err(|e| e.to_string())?;
        if !CLIENT_TOOLS.contains(&tool.name.as_str())
            || !names.insert(&tool.name)
            || tool.description.len() > 16 * 1024
            || tool.input_schema.to_string().len() > 64 * 1024
        {
            return Err("界面工具名称、Schema 或资源限制无效".into());
        }
    }
    Ok(())
}

struct ClientTool {
    definition: ToolDefinition,
    run_id: String,
    state: AgentRuntime,
    events: Arc<EventBridge>,
}

impl AgentTool for ClientTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn effect(&self, _: &Value) -> Result<ToolEffect, ToolError> {
        Ok(
            if CLIENT_READ_ONLY.contains(&self.definition.name.as_str()) {
                ToolEffect::ReadOnly
            } else {
                ToolEffect::ChangesState
            },
        )
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let (id, receiver) = self
                .state
                .request_client_tool(&self.run_id, context.tool_call_id.as_str())
                .map_err(|error| ToolError::permanent("connection_closed", error))?;
            let result = async {
                self.events.emit(json!({"type":"client_tool", "id":id, "name":self.definition.name, "input":input}))
                    .map_err(|e| ToolError::permanent("connection_closed", e))?;
                tokio::select! {
                    _ = context.cancellation.cancelled() => Err(ToolError::permanent("cancelled", "界面工具已取消")),
                    result = tokio::time::timeout(std::time::Duration::from_secs(600), receiver) => {
                        let value = result.map_err(|_| ToolError::permanent("tool_timeout", "界面工具等待超时"))?
                            .map_err(|_| ToolError::permanent("connection_closed", "界面工具连接已关闭"))?;
                        if let Some(error) = value.get("error").and_then(Value::as_str) {
                            Err(ToolError::permanent("client_tool_failed", error))
                        } else { Ok(ToolOutput::text(value.to_string())) }
                    }
                }
            }.await;
            self.state.finish_client_tool(&id);
            result
        })
    }
}

// 生产入口与验收入口共用界面工具端口；审批与取消仍由当前 Rust 运行控制。
pub(super) fn register_client_tools(
    registry: &mut ToolRegistry,
    definitions: &[ToolDefinition],
    run_id: &str,
    state: AgentRuntime,
    events: Arc<EventBridge>,
) -> Result<(), String> {
    validate_client_tools(definitions)?;
    for definition in definitions {
        registry
            .register(Arc::new(ClientTool {
                definition: definition.clone(),
                run_id: run_id.into(),
                state: state.clone(),
                events: events.clone(),
            }))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_tool_allowlist_cannot_replace_core_filesystem_tools() {
        let schema = json!({"type":"object"});
        assert!(validate_client_tools(&[ToolDefinition::new(
            "bash_background",
            "shell",
            schema.clone()
        )])
        .is_ok());
        assert!(validate_client_tools(&[ToolDefinition::new(
            "bash_run",
            "legacy shell",
            schema.clone()
        )])
        .is_err());
        assert!(validate_client_tools(&[ToolDefinition::new("Read", "read", schema)]).is_err());
    }
}
