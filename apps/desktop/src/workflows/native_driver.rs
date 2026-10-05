//! 生产 WorkflowDriver：把 Agent 叶节点接到真实桌面 AgentRuntime。

use crate::agent_runtime::{AgentRuntime, WorkflowActor, WorkflowActorRequest};
use crate::workflows::workspace_gate::acquire_workspace_write_gate;
use keencode_agent::{
    AgentId, SessionId, ToolApprovalDecision, ToolApprovalGate, ToolApprovalRequest, ToolCallId,
    ToolEffect, TurnCancellation, TurnId,
};
use keencode_resources::WorkflowJournalEvent as ResourceWorkflowJournalEvent;
use keencode_runtime::RuntimeSession;
use keencode_workflow::{
    DriverError, DriverLeafKind, DriverRequest, EffectClass, NodeAddress, NodeContext, TypedOutput,
    WorkflowDriver,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// 绑定父 RuntimeSession 的生产工作流叶节点 Driver。
///
/// Agent 叶节点使用独立 actor Session；Artifact 叶节点直接写父 Session 的
/// ArtifactStore 并追加明确引用；工具叶节点通过 AgentRuntime 的受控工具桥接执行。
pub struct NativeWorkflowDriver {
    runtime: Arc<AgentRuntime>,
    parent: RuntimeSession,
    actor: Arc<WorkflowActor>,
}

impl NativeWorkflowDriver {
    /// 创建绑定父会话的生产 Driver。
    pub fn new(runtime: Arc<AgentRuntime>, parent: RuntimeSession) -> Self {
        let actor = Arc::new(WorkflowActor::new(Arc::clone(&runtime), parent.clone()));
        Self {
            runtime,
            parent,
            actor,
        }
    }
}

impl WorkflowDriver for NativeWorkflowDriver {
    fn execute_agent<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> keencode_workflow::WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        let actor = Arc::clone(&self.actor);
        Box::pin(async move {
            let run_id = context.run_id.clone();
            let address = context.address.clone();
            let node_id = request.node_id.clone();
            let output_type = request
                .output_type
                .clone()
                .unwrap_or_else(|| "json".to_owned());
            let effect = request.effect;
            let binding = run_binding(&self.parent, &run_id)?;
            let _workspace_write_gate =
                if matches!(effect, EffectClass::Write | EffectClass::Unknown) {
                    Some(
                        acquire_workspace_write_gate(&binding.cwd, &context.cancellation_token)
                            .await
                            .map_err(|error| DriverError::new("workspace_write_gate", error))?,
                    )
                } else {
                    None
                };
            let result = actor
                .execute(WorkflowActorRequest {
                    run_id: run_id.clone(),
                    node_id: node_id.clone(),
                    address: address.clone(),
                    name: request.name,
                    input: request.input,
                    config: request.config,
                    output_type: request.output_type,
                    effect,
                    cancellation_token: context.cancellation_token,
                })
                .await
                .map_err(|error| DriverError::new("agent_runtime", error.to_string()))?;
            let actor_session_id = result.actor_session_id.clone();
            let actor_turn_id = result.turn_id.clone();
            let result_event = ResourceWorkflowJournalEvent {
                run_id: run_id.clone(),
                tool_call_id: binding.tool_call_id,
                sequence: 0,
                event_type: "agent-result".to_owned(),
                payload: json!({
                    "nodeId": node_id.clone(),
                    "nodeAddress": address.clone(),
                    "status": "completed",
                    "actorSessionId": actor_session_id.clone(),
                    "turnId": actor_turn_id.clone(),
                    "provider": result.provider.clone(),
                    "output": bounded_json(result.value.clone()),
                }),
                artifacts: Vec::new(),
                actor_session_id: Some(actor_session_id),
                launch_input_id: binding.launch_input_id,
            };
            commit_parent_event(&self.parent, result_event)?;
            Ok(TypedOutput::new(result.value, output_type, effect))
        })
    }

    fn execute_tool<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> keencode_workflow::WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        let runtime = Arc::clone(&self.runtime);
        let parent = self.parent.clone();
        Box::pin(async move {
            let binding = run_binding(&parent, &context.run_id)?;
            let _workspace_write_gate =
                if matches!(request.effect, EffectClass::Write | EffectClass::Unknown) {
                    Some(
                        acquire_workspace_write_gate(&binding.cwd, &context.cancellation_token)
                            .await
                            .map_err(|error| DriverError::new("workspace_write_gate", error))?,
                    )
                } else {
                    None
                };
            runtime
                .execute_workflow_tool(parent, request, context)
                .await
        })
    }

    fn execute_artifact<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> keencode_workflow::WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        let parent = self.parent.clone();
        Box::pin(async move {
            if context.cancellation_token.is_cancelled() {
                return Err(DriverError::new("cancelled", "工作流已取消"));
            }
            if request.effect == keencode_workflow::EffectClass::ReadOnly {
                return Err(DriverError::new(
                    "workflow_effect_mismatch",
                    "Artifact 持久化不能声明为只读",
                ));
            }
            if context.address.node_id != request.node_id {
                return Err(DriverError::new(
                    "workflow_identity",
                    "节点地址与节点 ID 不一致",
                ));
            }
            let address = context.address.clone();
            let binding = run_binding(&parent, &context.run_id)?;
            let _workspace_write_gate =
                acquire_workspace_write_gate(&binding.cwd, &context.cancellation_token)
                    .await
                    .map_err(|error| DriverError::new("workspace_write_gate", error))?;
            if binding.plan_enabled {
                return Err(DriverError::new("plan_guard", "计划模式禁止提交 Artifact"));
            }
            let operation_id = artifact_operation_id(&context.run_id, &address, &request.name);
            let identity_suffix = operation_id
                .strip_prefix("workflow-artifact-")
                .unwrap_or(operation_id.as_str());
            let cancellation = TurnCancellation::new();
            let cancellation_bridge = cancellation.clone();
            let workflow_token = context.cancellation_token.clone();
            let cancellation_watcher = tokio::spawn(async move {
                workflow_token.cancelled().await;
                cancellation_bridge.cancel();
            });
            let approval = self
                .runtime
                .permission_coordinator()
                .request(
                    ToolApprovalRequest {
                        session_id: SessionId::new(parent.session_id().as_str().to_owned())
                            .map_err(|error| {
                                DriverError::new("permission_identity", error.to_string())
                            })?,
                        turn_id: TurnId::new(format!("workflow-artifact-turn-{identity_suffix}"))
                            .map_err(|error| {
                            DriverError::new("permission_identity", error.to_string())
                        })?,
                        source_agent_id: AgentId::new(format!(
                            "workflow-artifact-agent-{identity_suffix}"
                        ))
                        .map_err(|error| {
                            DriverError::new("permission_identity", error.to_string())
                        })?,
                        tool_call_id: ToolCallId::new(format!(
                            "workflow-artifact-call-{identity_suffix}"
                        ))
                        .map_err(|error| {
                            DriverError::new("permission_identity", error.to_string())
                        })?,
                        tool_name: request.name.clone(),
                        input: request.input.clone(),
                        // Artifact 始终写入持久 ArtifactStore；未知声明也只能走保守写门。
                        effect: ToolEffect::ChangesState,
                        operation_id: Some(operation_id),
                    },
                    cancellation.clone(),
                )
                .await;
            cancellation_watcher.abort();
            match approval {
                Ok(ToolApprovalDecision::Approved) => {}
                Ok(ToolApprovalDecision::Denied { reason }) => {
                    return Err(DriverError::new(
                        "permission_denied",
                        format!("Artifact 权限被拒绝：{reason}"),
                    ));
                }
                Ok(ToolApprovalDecision::Cancelled) => {
                    return Err(DriverError::new("cancelled", "Artifact 权限审批已取消"));
                }
                Err(error) => {
                    return Err(DriverError::new(
                        "permission_unavailable",
                        format!("Artifact 权限审批未完成：{error}"),
                    ));
                }
            }
            if cancellation.is_cancelled() {
                return Err(DriverError::new(
                    "cancelled",
                    "Artifact 在真实持久化前因取消而中止",
                ));
            }
            let output_type = request
                .output_type
                .clone()
                .unwrap_or_else(|| "artifact".to_owned());
            let (bytes, input_media_type) = artifact_bytes(&request.input)?;
            // 先验证封闭的 source kind，再写入 ArtifactStore；未知 outputType 不能
            // 通过默认 file 悄悄进入 Journal，避免产生 UI 看似有效的错误产物。
            let kind = artifact_kind(&output_type, input_media_type.as_deref())?;
            let media_type = artifact_media_type(&output_type, input_media_type);
            let artifact = parent
                .put_artifact(&bytes, media_type)
                .map_err(|error| DriverError::new("artifact_store", error.to_string()))?;
            let event = ResourceWorkflowJournalEvent {
                run_id: context.run_id.clone(),
                tool_call_id: binding.tool_call_id,
                sequence: 0,
                event_type: "artifact-committed".to_owned(),
                payload: json!({
                    "nodeId": request.node_id,
                    "nodeAddress": address.clone(),
                    "artifactId": artifact.artifact_id,
                    "sizeBytes": bytes.len(),
                    "outputType": output_type,
                    "kind": kind,
                    "contentType": artifact.media_type.clone(),
                    "version": 1,
                    "publishedAt": unix_time_ms(),
                    "primary": true,
                }),
                artifacts: vec![artifact.as_event_use()],
                actor_session_id: None,
                launch_input_id: binding.launch_input_id,
            };
            commit_parent_event(&parent, event)?;
            let output = json!({
                "artifactId": artifact.artifact_id,
                "sha256": artifact.sha256,
                "sizeBytes": artifact.size_bytes,
                "mediaType": artifact.media_type,
            });
            Ok(TypedOutput::new(output, output_type, request.effect))
        })
    }

    fn recover_agent<'a>(
        &'a self,
        request: DriverRequest,
        _context: NodeContext,
    ) -> keencode_workflow::WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        Box::pin(async move {
            if request.effect != EffectClass::ReadOnly {
                return Err(DriverError::new(
                    "workflow_recovery_effect_mismatch",
                    "Agent 恢复请求不是只读副作用",
                ));
            }
            // Agent 的模型回合即使只读取工作区，也可能产生新的模型/问答状态；
            // 当前 Runtime 没有可证明幂等且硬限制为只读的 actor recovery 入口。
            Err(DriverError::new(
                "workflow_agent_recovery_unsupported",
                "只读 Agent 没有 Runtime 硬只读恢复能力",
            ))
        })
    }

    fn recover_tool<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> keencode_workflow::WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        if let Err(error) = validate_read_only_tool_recovery(&request) {
            return Box::pin(async move { Err(error) });
        }
        // Read 仍通过同一个受控 Tool registry、Schema、权限、PlanGuard、取消和
        // Journal 生命周期执行；恢复入口只放宽了“可重执行”的类别，不绕过边界。
        self.execute_tool(request, context)
    }
}

fn validate_read_only_tool_recovery(request: &DriverRequest) -> Result<(), DriverError> {
    if request.leaf_kind != DriverLeafKind::Tool {
        return Err(DriverError::new(
            "workflow_recovery_kind_mismatch",
            "只读 Tool 恢复请求的叶节点类别不一致",
        ));
    }
    if request.effect != EffectClass::ReadOnly || request.name != "Read" {
        return Err(DriverError::new(
            "workflow_read_recovery_unsupported",
            "只有受控 Read 工具可以安全恢复",
        ));
    }
    Ok(())
}

fn artifact_bytes(input: &Value) -> Result<(Vec<u8>, Option<String>), DriverError> {
    let media_type = input
        .get("mediaType")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let text_content = input
        .get("data")
        .and_then(Value::as_str)
        .or_else(|| input.get("content").and_then(Value::as_str));
    // 允许结构化 JSON 作为产物正文，但 null 和空对象没有可交付内容，不能
    // 仅因为 JSON 编码后包含几个字节就绕过空内容校验。
    let bytes = text_content
        .map(|text| text.as_bytes().to_vec())
        .unwrap_or_else(|| match input {
            Value::String(text) => text.as_bytes().to_vec(),
            Value::Null => Vec::new(),
            Value::Object(object) if object.is_empty() => Vec::new(),
            _ => serde_json::to_vec(input).unwrap_or_default(),
        });
    if bytes.is_empty() {
        return Err(DriverError::new(
            "invalid_artifact",
            "Artifact 内容不能为空",
        ));
    }
    Ok((bytes, media_type))
}

/// Markdown 节点的输出类型就是用户面渲染契约；输入未附 MIME 时补足唯一可渲染类型。
fn artifact_media_type(output_type: &str, media_type: Option<String>) -> Option<String> {
    if media_type.is_none() && output_type == "markdown" {
        Some("text/markdown".to_owned())
    } else {
        media_type
    }
}

fn artifact_kind(output_type: &str, media_type: Option<&str>) -> Result<&'static str, DriverError> {
    match output_type {
        "markdown" => Ok("markdown"),
        "chart" => Ok("chart"),
        "table" => Ok("table"),
        "metrics" => Ok("metrics"),
        "board" => Ok("board"),
        "file" => Ok("file"),
        "artifact" => {
            if media_type.is_some_and(|value| value.eq_ignore_ascii_case("text/markdown")) {
                Ok("markdown")
            } else {
                Ok("file")
            }
        }
        _ => Err(DriverError::new(
            "invalid_artifact_kind",
            format!("未知 Artifact kind/outputType: {output_type}"),
        )),
    }
}

fn bounded_json(value: Value) -> Value {
    const MAX_BYTES: usize = 128 * 1024;
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_BYTES) {
        json!({"truncated": true})
    } else {
        value
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Debug)]
struct WorkflowRunBinding {
    tool_call_id: String,
    launch_input_id: Option<String>,
    plan_enabled: bool,
    cwd: std::path::PathBuf,
}

fn run_binding(parent: &RuntimeSession, run_id: &str) -> Result<WorkflowRunBinding, DriverError> {
    let events = parent
        .read_state(|state| {
            state
                .workflow_events
                .get(run_id)
                .cloned()
                .unwrap_or_default()
        })
        .map_err(|error| DriverError::new("workflow_journal", error.to_string()))?;
    let Some(started) = events.first() else {
        return Err(DriverError::new(
            "workflow_journal",
            "Artifact 节点缺少 run-started 冻结事实",
        ));
    };
    if started.event_type != "run-started"
        || started.run_id != run_id
        || started
            .payload
            .get("parentSessionId")
            .and_then(Value::as_str)
            != Some(parent.session_id().as_str())
        || started.tool_call_id.trim().is_empty()
    {
        return Err(DriverError::new(
            "workflow_journal",
            "工作流启动事实的父会话或 toolCallId 无效",
        ));
    }
    let plan_enabled = started
        .payload
        .get("models")
        .and_then(Value::as_object)
        .and_then(|models| models.get("planEnabled"))
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            DriverError::new("workflow_journal", "工作流启动事实缺少冻结 planEnabled")
        })?;
    let cwd = started
        .payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| DriverError::new("workflow_journal", "工作流 cwd 不是绝对路径"))?;
    Ok(WorkflowRunBinding {
        tool_call_id: started.tool_call_id.clone(),
        launch_input_id: started.launch_input_id.clone(),
        plan_enabled,
        cwd,
    })
}

fn artifact_operation_id(run_id: &str, address: &NodeAddress, tool_name: &str) -> String {
    let invocation = address
        .invocation
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let mut digest = Sha256::new();
    digest.update(b"keencode-workflow-artifact\0");
    for value in [
        run_id,
        address.node_id.as_str(),
        invocation.as_str(),
        tool_name,
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("workflow-artifact-{:x}", digest.finalize())
}

fn commit_parent_event(
    parent: &RuntimeSession,
    mut event: ResourceWorkflowJournalEvent,
) -> Result<(), DriverError> {
    event.sequence = 0;
    let operation_id = super::workflow_event_operation_id(&event);
    parent
        .append_workflow_event(&operation_id, event)
        .map_err(|error| DriverError::new("workflow_journal", error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_input_requires_non_empty_content() {
        assert!(artifact_bytes(&json!({})).is_err());
        assert!(artifact_bytes(&json!({"data": ""})).is_err());
        let (bytes, media_type) = artifact_bytes(&json!({
            "content": "hello",
            "mediaType": "text/plain"
        }))
        .expect("text artifact should be accepted");
        assert_eq!(bytes, b"hello");
        assert_eq!(media_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn markdown_string_artifact_uses_plain_utf8_and_default_media_type() {
        let (bytes, media_type) =
            artifact_bytes(&Value::String("WORKFLOW_LEFT_OK_41".to_owned())).unwrap();

        assert_eq!(bytes, b"WORKFLOW_LEFT_OK_41");
        assert!(media_type.is_none());
        assert_eq!(
            artifact_media_type("markdown", media_type),
            Some("text/markdown".to_owned())
        );
    }

    #[test]
    fn explicit_artifact_media_type_is_preserved() {
        assert_eq!(
            artifact_media_type("markdown", Some("text/plain".to_owned())),
            Some("text/plain".to_owned())
        );
        assert_eq!(artifact_media_type("file", None), None);
    }

    #[test]
    fn artifact_kind_rejects_unknown_output_type() {
        assert_eq!(
            artifact_kind("markdown", Some("text/plain")).unwrap(),
            "markdown"
        );
        assert_eq!(
            artifact_kind("artifact", Some("text/markdown")).unwrap(),
            "markdown"
        );
        assert_eq!(
            artifact_kind("artifact", Some("application/octet-stream")).unwrap(),
            "file"
        );
        assert_eq!(
            artifact_kind("custom", Some("text/markdown"))
                .unwrap_err()
                .code,
            "invalid_artifact_kind"
        );
        assert_eq!(
            artifact_kind("md", Some("text/markdown")).unwrap_err().code,
            "invalid_artifact_kind"
        );
    }

    #[test]
    fn read_recovery_requires_typed_read_tool() {
        let request = DriverRequest {
            leaf_kind: DriverLeafKind::Tool,
            node_id: "read".to_owned(),
            name: "Read".to_owned(),
            input: json!({"path": "README.md"}),
            config: Value::Null,
            output_type: Some("json".to_owned()),
            effect: EffectClass::ReadOnly,
        };
        assert!(validate_read_only_tool_recovery(&request).is_ok());

        let mut non_tool = request.clone();
        non_tool.leaf_kind = DriverLeafKind::Agent;
        assert_eq!(
            validate_read_only_tool_recovery(&non_tool)
                .unwrap_err()
                .code,
            "workflow_recovery_kind_mismatch"
        );

        let mut write = request.clone();
        write.name = "Write".to_owned();
        write.effect = EffectClass::Write;
        assert_eq!(
            validate_read_only_tool_recovery(&write).unwrap_err().code,
            "workflow_read_recovery_unsupported"
        );
    }

    #[test]
    fn workflow_event_identity_includes_event_body_but_not_sequence() {
        let mut first = ResourceWorkflowJournalEvent {
            run_id: "run".to_owned(),
            tool_call_id: "tool".to_owned(),
            sequence: 1,
            event_type: "artifact-committed".to_owned(),
            payload: json!({"nodeAddress": {"node_id": "artifact", "invocation": [1]}}),
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: None,
        };
        let second = ResourceWorkflowJournalEvent {
            payload: json!({"nodeAddress": {"node_id": "artifact", "invocation": [2]}}),
            ..first.clone()
        };
        first.sequence = 999;
        assert_ne!(
            crate::workflows::workflow_event_operation_id(&first),
            crate::workflows::workflow_event_operation_id(&second)
        );
        first.sequence = 1;
        assert_eq!(
            crate::workflows::workflow_event_operation_id(&first),
            crate::workflows::workflow_event_operation_id(&ResourceWorkflowJournalEvent {
                sequence: 1,
                ..first
            })
        );
    }
}
