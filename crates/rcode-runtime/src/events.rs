use rcode_agent::{
    AgentCommitEvent, AgentCommitEventKind, AgentCommitSink, AgentCommitSinkError,
    AgentEventFuture, AgentEventSink, AgentEventSinkError, AgentStreamEvent, AgentStreamEventKind,
    AgentToolRoundPreflight, AgentToolRoundPreflightError, AgentToolRoundReservation,
    ModelRoundUsage, NoopAgentCommitSink,
};
use rcode_model::ContentBlock;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub struct EventBridge {
    send: Box<dyn Fn(Value) -> Result<(), String> + Send + Sync>,
    // 同一提交 ID 只向宿主投递一次，重试不会重复工具结果或历史消息。
    committed: Mutex<std::collections::HashSet<String>>,
    tool_projection: Mutex<ToolProjection>,
}

#[derive(Default)]
struct ToolProjection {
    inputs: std::collections::HashSet<String>,
    results: std::collections::HashSet<String>,
}

impl EventBridge {
    pub fn new(send: impl Fn(Value) -> Result<(), String> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            send: Box::new(send),
            committed: Mutex::new(Default::default()),
            tool_projection: Mutex::new(ToolProjection::default()),
        })
    }

    pub fn emit(&self, event: Value) -> Result<(), String> {
        let tool_id = match event["type"].as_str() {
            Some("tool_input") => event["id"].as_str(),
            Some("tool_result") => event["result"]["toolCallId"].as_str(),
            _ => None,
        }
        .map(str::to_owned);
        if let Some(id) = tool_id {
            let mut state = self
                .tool_projection
                .lock()
                .map_err(|_| "工具投影锁不可用")?;
            let seen = if event["type"] == "tool_input" {
                &mut state.inputs
            } else {
                &mut state.results
            };
            if seen.contains(&id) {
                return Ok(());
            }
            (self.send)(event)?;
            seen.insert(id);
            return Ok(());
        }
        (self.send)(event)
    }
}

impl AgentEventSink for EventBridge {
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async move {
            let data = match event.kind() {
                AgentStreamEventKind::ModelEvent { event: model_event } => json!({
                    "type":"model", "round":event.model_round(), "event":model_event,
                }),
                AgentStreamEventKind::ContextCompactionStarted { .. } => json!({"type":"compact"}),
                _ => return Ok(()),
            };
            self.emit(data).map_err(AgentEventSinkError::new)
        })
    }
}

impl AgentCommitSink for EventBridge {
    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        NoopAgentCommitSink.preflight_tool_round(round)
    }

    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        self.emit(json!({"type":"usage", "usage":usage.completion().usage}))
            .map_err(AgentCommitSinkError::rejected)
    }

    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        let id = event.event_id().as_str().to_owned();
        let mut committed = self
            .committed
            .lock()
            .map_err(|_| AgentCommitSinkError::rejected("提交锁不可用"))?;
        if committed.contains(&id) {
            return Ok(());
        }
        let value = match event.kind() {
            AgentCommitEventKind::ToolRequested { call, .. } => json!({
                "type":"tool_input", "id":call.id, "name":call.name, "input":call.arguments,
            }),
            AgentCommitEventKind::ToolCompleted { result, .. } => {
                json!({"type":"tool_result", "result":result})
            }
            AgentCommitEventKind::ModelRoundCommitted { messages, .. }
            | AgentCommitEventKind::RoundCommitted { messages, .. } => {
                // 拒绝、预检失败和取消不产生 ToolCompleted，权威 Transcript 仍保留结果。
                for message in messages {
                    for block in &message.content {
                        let projection = match block {
                            ContentBlock::ToolCall { tool_call } => Some(
                                json!({"type":"tool_input", "id":tool_call.id, "name":tool_call.name, "input":tool_call.arguments}),
                            ),
                            ContentBlock::ToolResult { tool_result } => {
                                Some(json!({"type":"tool_result", "result":tool_result}))
                            }
                            _ => None,
                        };
                        if let Some(projection) = projection {
                            self.emit(projection)
                                .map_err(AgentCommitSinkError::rejected)?;
                        }
                    }
                }
                json!({"type":"transcript", "id":id, "messages":messages})
            }
            _ => {
                committed.insert(id);
                return Ok(());
            }
        };
        self.emit(value).map_err(AgentCommitSinkError::rejected)?;
        committed.insert(id);
        Ok(())
    }
}
