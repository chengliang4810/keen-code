use super::security::check_file_path;
use rcode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolRegistry,
};
use rcode_model::ToolDefinition;
use rcode_tools::{
    BashTool, EditTool, GlobTool, GrepTool, MultiEditTool, PowerShellTool, ReadTool,
    ToolEnvironment, WriteTool,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct WorkspaceTool {
    inner: Arc<dyn AgentTool>,
    root: PathBuf,
}

impl AgentTool for WorkspaceTool {
    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        // Shell 不具备操作系统沙箱，必须按变更操作交给审批策略。
        if matches!(self.inner.definition().name.as_str(), "Bash" | "PowerShell") {
            return Ok(ToolEffect::ChangesState);
        }
        self.inner.effect(input)
    }
    fn concurrency(&self) -> ToolConcurrency {
        self.inner.concurrency()
    }
    fn timeout(&self) -> Option<std::time::Duration> {
        self.inner.timeout()
    }
    fn output_artifact_sink(&self) -> Option<Arc<dyn rcode_agent::ToolOutputArtifactSink>> {
        self.inner.output_artifact_sink()
    }
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let raw = input
                .get("file_path")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str)
                .unwrap_or(".");
            check_file_path(&self.root, raw).map_err(|e| ToolError::permanent("path_denied", e))?;
            self.inner.execute(context, input).await
        })
    }
}

// 所有应用入口和真实模型验收共用同一组工具与权限包装，测试不能绕过宿主边界。
pub fn register_workspace_tools(
    registry: &mut ToolRegistry,
    env: Arc<ToolEnvironment>,
    root: &Path,
    plan_mode: bool,
) -> Result<(), String> {
    let mut local: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(ReadTool::new(env.clone())),
        Arc::new(GlobTool::new(env.clone())),
        Arc::new(GrepTool::new(env.clone())),
    ];
    if !plan_mode {
        local.extend([
            Arc::new(EditTool::new(env.clone())) as Arc<dyn AgentTool>,
            Arc::new(MultiEditTool::new(env.clone())),
            Arc::new(WriteTool::new(env.clone())),
        ]);
        if cfg!(windows) {
            local.push(Arc::new(PowerShellTool::new(env)));
        } else {
            local.push(Arc::new(BashTool::new(env)));
        }
    }
    for inner in local {
        registry
            .register(Arc::new(WorkspaceTool {
                inner,
                root: root.into(),
            }))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcode_agent::{AgentId, SessionId, ToolCallId, TurnCancellation, TurnId};
    use serde_json::json;

    #[test]
    fn plan_registry_excludes_mutations_and_shell_cannot_claim_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let env = Arc::new(ToolEnvironment::new(&root).unwrap());
        let mut plan = ToolRegistry::new();
        register_workspace_tools(&mut plan, env.clone(), &root, true).unwrap();
        assert_eq!(
            plan.definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            ["Glob", "Grep", "Read"]
        );
        for inner in [
            Arc::new(BashTool::new(env.clone())) as Arc<dyn AgentTool>,
            Arc::new(PowerShellTool::new(env.clone())),
        ] {
            let wrapped = WorkspaceTool {
                inner,
                root: root.clone(),
            };
            assert_eq!(
                wrapped
                    .effect(&json!({"command":"echo test", "read_only":true}))
                    .unwrap(),
                ToolEffect::ChangesState
            );
        }
    }

    #[tokio::test]
    async fn registered_workspace_guard_rejects_secret_and_outside_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join(".env.fixture"), "synthetic secret").unwrap();
        std::fs::write(directory.path().join("outside.txt"), "outside").unwrap();
        std::fs::write(root.join("public.txt"), "allowed").unwrap();
        let tool = WorkspaceTool {
            inner: Arc::new(ReadTool::new(Arc::new(
                ToolEnvironment::new(&root).unwrap(),
            ))),
            root,
        };
        let context = ToolContext {
            session_id: SessionId::new("guard-session").unwrap(),
            turn_id: TurnId::new("guard-turn").unwrap(),
            source_agent_id: AgentId::new("main").unwrap(),
            tool_call_id: ToolCallId::new("guard-call").unwrap(),
            cancellation: TurnCancellation::new(),
        };
        for path in [".env.fixture", "../outside.txt"] {
            let error = tool
                .execute(context.clone(), json!({"file_path":path}))
                .await
                .unwrap_err();
            assert_eq!(error.code, "path_denied");
            assert!(!error.retryable);
            assert!(!error.message.contains("synthetic secret"));
        }
        let output = tool
            .execute(context, json!({"file_path":"public.txt"}))
            .await
            .unwrap();
        assert!(serde_json::to_string(&output.content)
            .unwrap()
            .contains("allowed"));
    }
}
