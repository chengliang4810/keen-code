use crate::{register_workspace_tools, AgentRuntime, EventBridge, PermissionMode};
use rcode_agent::{
    AgentId, AgentTool, CollaborationIdGenerator, PlanGuard, SessionId, ToolConcurrency,
    ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput, ToolRegistry, TurnId, TurnRequest,
    UuidCollaborationIdGenerator,
};
use rcode_model::{ContentBlock, Message, MessageRole, ModelProvider, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentTemplate {
    pub id: String,
    pub label: String,
    pub description: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
}

impl SubagentTemplate {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.label.len() > 128
            || self.description.len() > 4096
            || self.system_prompt.trim().is_empty()
            || self.system_prompt.len() > 256 * 1024
            || self.tools.len() > 4
        {
            return Err("子 Agent 配置无效或超过大小上限".into());
        }
        let names = self.native_tools()?;
        if names.iter().collect::<std::collections::HashSet<_>>().len() != names.len() {
            return Err("子 Agent 工具不能重复".into());
        }
        Ok(())
    }

    fn native_tools(&self) -> Result<Vec<String>, String> {
        self.tools
            .iter()
            .map(|tool| match tool.as_str() {
                "read_file" => Ok("Read".into()),
                "grep" => Ok("Grep".into()),
                "glob" => Ok("Glob".into()),
                "list_directory" => Ok(tool.clone()),
                _ => Err("子 Agent 只能使用只读文件工具".into()),
            })
            .collect()
    }
}

pub fn builtin_subagents() -> Vec<SubagentTemplate> {
    serde_json::from_str(include_str!("../resources/subagents.json"))
        .expect("validated built-in subagents")
}

#[derive(Clone)]
pub struct ResolvedSubagent {
    pub template: SubagentTemplate,
    pub model: String,
    pub provider: Arc<dyn ModelProvider>,
}

pub(crate) struct SubagentTool {
    pub runtime: AgentRuntime,
    pub root: PathBuf,
    pub agents: Vec<ResolvedSubagent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    r#type: String,
    prompt: String,
    description: Option<String>,
}

impl AgentTool for SubagentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("run_subagent", format!("Run an isolated read-only subagent and return its summary. It cannot mutate files, run Shell commands, delegate recursively, or use extensions.\n{}", self.agents.iter().map(|a| format!("{}: {}",a.template.id,a.template.description)).collect::<Vec<_>>().join("\n")), json!({"type":"object","properties": {
            "type":{"type":"string","enum":self.agents.iter().map(|a| &a.template.id).collect::<Vec<_>>()},
            "prompt":{"type":"string","minLength":1,"maxLength":262144},
            "description":{"type":"string","maxLength":128}},"required":["type","prompt"],"additionalProperties":false}))
    }
    fn effect(&self, _: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }
    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let input: Input = serde_json::from_value(input)
                .map_err(|_| ToolError::permanent("invalid_input", "子 Agent 输入无效"))?;
            if input.prompt.trim().is_empty()
                || input.prompt.len() > 256 * 1024
                || input.description.as_ref().is_some_and(|s| s.len() > 128)
                || context.cancellation.is_cancelled()
            {
                return Err(ToolError::permanent(
                    "invalid_input",
                    "子 Agent 请求为空、超出上限或已取消",
                ));
            }
            let agent = self
                .agents
                .iter()
                .find(|agent| agent.template.id == input.r#type)
                .ok_or_else(|| {
                    ToolError::permanent("unknown_subagent", "子 Agent 不存在或未启用")
                })?;
            let session = UuidCollaborationIdGenerator.next_session_id();
            let run_id = format!("child-{}", session.as_str());
            let lease = self
                .runtime
                .register(&run_id, session.as_str())
                .map_err(|e| ToolError::permanent("subagent_busy", e))?;
            let _cleanup = ChildSession {
                runtime: self.runtime.clone(),
                id: session.as_str().into(),
            };
            let environment = self
                .runtime
                .environment(session.as_str(), &self.root, None)
                .map_err(|e| ToolError::permanent("workspace_denied", e))?;
            let mut tools = ToolRegistry::new();
            register_workspace_tools(&mut tools, environment, &self.root, true)
                .map_err(|e| ToolError::permanent("tool_setup", e))?;
            let names = agent
                .template
                .native_tools()
                .map_err(|e| ToolError::permanent("tool_setup", e))?;
            let tools = tools
                .select_exact(&names)
                .map_err(|e| ToolError::permanent("tool_setup", e.to_string()))?;
            let rounds = Arc::new(AtomicU64::new(0));
            let count = rounds.clone();
            let events = EventBridge::new(move |event| {
                if event["type"] == "model" && event["event"]["type"] == "message_start" {
                    count.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            });
            let request = TurnRequest::new(
                SessionId::new(session.as_str()).expect("generated session"),
                TurnId::new(&run_id).expect("generated turn"),
                AgentId::new("child").expect("static agent"),
                agent.model.clone(),
                vec![
                    Message::text(MessageRole::System, &agent.template.system_prompt),
                    Message::text(MessageRole::User, input.prompt),
                ],
                PlanGuard::read_only(),
            );
            let start = Instant::now();
            let running = self.runtime.run_turn(
                &lease,
                agent.provider.clone(),
                tools,
                request,
                events,
                PermissionMode::Ask,
            );
            tokio::pin!(running);
            let result = tokio::select! {
                result = &mut running => result,
                _ = context.cancellation.cancelled() => { lease.cancellation.cancel(); running.await }
            }.map_err(|e|ToolError::permanent("subagent_failed",e))?;
            if result.state.terminal_reason() == Some(rcode_agent::TerminalReason::LimitReached) {
                return Err(ToolError::permanent(
                    "subagent_limit_reached",
                    "子 Agent 达到执行上限",
                ));
            }
            if let Some(error) = result.error {
                return Err(ToolError::permanent(
                    "subagent_failed",
                    rcode_model::redact_error_secrets_bounded(&error.to_string(), 2048),
                ));
            }
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent("cancelled", "子 Agent 已取消"));
            }
            let summary = result
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.role == MessageRole::Assistant
                        && message
                            .content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::Text { .. }))
                })
                .map(|message| {
                    message
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Ok(ToolOutput::text(json!({"type":input.r#type,"description":input.description,"summary":summary,"stepCount":rounds.load(Ordering::Relaxed),"durationMs":start.elapsed().as_millis()}).to_string()))
        })
    }
}

struct ChildSession {
    runtime: AgentRuntime,
    id: String,
}
impl Drop for ChildSession {
    fn drop(&mut self) {
        self.runtime.release_session(&self.id);
    }
}
