//! 工作流 Tool 叶节点的桌面生产桥接。
//!
//! Workflow 没有模型回合，不能借用旧的 `model_round`/`RequestId` 工具生命周期。
//! 本模块只把现有本地工具注册表、Hook、PlanGuard、取消和输出边界接到一个独立的
//! Workflow Journal 生命周期，并把文件变更的真实前后字节保存到父 Session 的
//! ArtifactStore。实际挂载由 `agent_runtime.rs` 完成，避免改变 Agent Actor 的装配边界。

use super::{
    AgentCapabilities, AgentProfile, AgentRuntime, AgentRuntimeError, RootAgentSeed, WorkflowActor,
};
use keencode_agent::{
    AgentId, AgentRunner, ControlledToolEvent, ControlledToolIdentity, ControlledToolLifecycleSink,
    ControlledToolPreflight, ControlledToolRequest, ControlledToolReservation,
    ControlledToolResult, PlanGuard, RunLimits, SessionId, ToolCompletionStatus, ToolEffect,
    ToolError, ToolRegistry, TurnCancellation, TurnId,
};
use keencode_model::ToolCall;
use keencode_resources::{ArtifactUse, WorkflowJournalEvent};
use keencode_runtime::RuntimeSession;
use keencode_tools::{
    BashTool, EditTool, FileMutationRecorder, PreparedFileMutation, ReadTool, ToolEnvironment,
    WriteTool,
};
use keencode_workflow::{
    DriverError, DriverRequest, EffectClass, NodeAddress, NodeContext, TypedOutput,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WORKFLOW_TOOL_ARTIFACT_MEDIA_TYPE: &str = "application/octet-stream";
const WORKFLOW_TOOL_RESULT_MEDIA_TYPE: &str = "application/json";

/// 将工作流 Tool 叶节点接入父 RuntimeSession。
impl AgentRuntime {
    /// 在父 Session 的冻结 Workflow 作用域内执行一个真实本地工具。
    ///
    /// 该入口不创建模型回合，也不直接操作文件或 Shell；工具只能通过
    /// `AgentRunner::execute_controlled_tool` 进入既有的 Schema、Hook、PlanGuard、
    /// 取消、超时和输出归一边界。父 Journal 的 Tool 生命周期与文件证据均由本模块
    /// 单独持久化，因而不会伪造模型 `model_round` 或旧工具 `RequestId`。
    pub(crate) async fn execute_workflow_tool(
        self: &Arc<Self>,
        parent: RuntimeSession,
        request: DriverRequest,
        context: NodeContext,
    ) -> Result<TypedOutput, DriverError> {
        validate_request(&parent, &request, &context)?;
        let frozen = frozen_run_binding(&parent, &context.run_id)?;
        let attempt =
            workflow_tool_attempt(&parent, &context.run_id, &context.address, &request.name)?;
        if attempt > 0 && request.effect != EffectClass::ReadOnly {
            return Err(DriverError::new(
                "workflow_recovery_indeterminate",
                "变更态工作流 Tool 已有未安全重放的生命周期事实",
            ));
        }
        let actor = WorkflowActor::new(Arc::clone(self), parent.clone());
        let provider = actor
            .frozen_provider_for_run(&context.run_id)
            .map_err(runtime_error)?;
        let current_provider = self
            .workflow_provider_snapshot(parent.session_id().as_str())
            .map_err(runtime_error)?;
        if provider != current_provider {
            return Err(DriverError::new(
                "workflow_provider_changed",
                "工作流冻结的 Provider 已不再匹配当前注册表",
            ));
        }
        // 节点声明只读是更严格的边界：即使父运行未开启 Plan，也不能让模型
        // 通过 Read 之外的受控路径获得写权限；声明写入的节点仍由受控 API 校验。
        let plan_enabled = actor
            .frozen_plan_enabled(&context.run_id)
            .map_err(runtime_error)?
            || request.effect == EffectClass::ReadOnly;
        let resolved = self
            .resolve_session_provider(Some(&provider))
            .map_err(runtime_error)?;
        let operation_id = workflow_tool_operation_id_for_attempt(
            &context.run_id,
            &context.address,
            &request.name,
            attempt,
        );
        let turn_id = workflow_tool_turn_id_for_attempt(&context.run_id, &context.address, attempt);
        let source_agent_id =
            workflow_tool_agent_id_for_attempt(&context.run_id, &context.address, attempt);
        let identity = ControlledToolIdentity {
            session_id: SessionId::new(parent.session_id().as_str().to_owned())
                .map_err(|error| DriverError::new("workflow_identity", error.to_string()))?,
            turn_id: TurnId::new(turn_id.clone())
                .map_err(|error| DriverError::new("workflow_identity", error.to_string()))?,
            source_agent_id: AgentId::new(source_agent_id.clone())
                .map_err(|error| DriverError::new("workflow_identity", error.to_string()))?,
            operation_id: operation_id.clone(),
        };

        // 通过既有完整装配冻结扩展 Hook；工具实现本身在下方使用同一 ToolEnvironment
        // 重新注册，以便安装 Workflow 专用 FileMutationRecorder。
        let collaboration = self
            .ensure_collaboration_runtime(
                &parent,
                RootAgentSeed {
                    model: resolved.model().to_owned(),
                    reasoning_effort: provider
                        .reasoning_effort
                        .map(super::reasoning_effort_snapshot_name),
                    plan_guard: if plan_enabled {
                        PlanGuard::read_only()
                    } else {
                        PlanGuard::inactive()
                    },
                },
            )
            .map_err(runtime_error)?;
        let plan_guard = if plan_enabled {
            PlanGuard::read_only()
        } else {
            PlanGuard::inactive()
        };
        let profile = AgentProfile {
            model: resolved.model().to_owned(),
            reasoning_effort: provider
                .reasoning_effort
                .map(super::reasoning_effort_snapshot_name),
            plan_guard,
            cwd: frozen.cwd.clone(),
            worktree_lease: None,
            tool_snapshot: vec![request.name.clone()],
        };
        let (_, hooks, _) = self
            .assemble_agent_tools(
                &collaboration.execution,
                Arc::clone(&collaboration.coordinator),
                &profile,
                "workflow-tool",
                plan_guard,
                AgentCapabilities {
                    can_spawn_agent: false,
                },
            )
            .map_err(runtime_error)?;

        let lifecycle = Arc::new(WorkflowToolLifecycle::new(WorkflowToolState::new(
            parent.clone(),
            context.run_id.clone(),
            frozen.tool_call_id,
            frozen.launch_input_id,
            context.node_id.clone(),
            context.address.clone(),
            request.name.clone(),
            request.effect,
            identity.clone(),
        )));
        let environment = workflow_tool_environment(
            self,
            &parent,
            frozen.cwd.as_path(),
            Arc::clone(&lifecycle.state),
        )?;
        let tools = workflow_tool_registry(environment, &request.name)?;
        let runner = AgentRunner::new(Arc::new(resolved), tools, RunLimits::default())
            .with_hook_runtime(hooks)
            .with_tool_approval_gate(self.permissions.clone());
        let runner = parent.bind_agent_runner(runner);

        let cancellation = TurnCancellation::new();
        let cancellation_bridge = cancellation.clone();
        let workflow_token = context.cancellation_token.clone();
        let cancellation_watcher = tokio::spawn(async move {
            workflow_token.cancelled().await;
            cancellation_bridge.cancel();
        });
        let call = ToolCall::new(operation_id, request.name.clone(), request.input.clone());
        let controlled = runner
            .execute_controlled_tool(ControlledToolRequest {
                identity,
                call,
                plan_guard,
                cancellation,
                lifecycle,
            })
            .await;
        cancellation_watcher.abort();
        let controlled = controlled
            .map_err(|error| DriverError::new("workflow_tool_execution", error.to_string()))?;
        controlled_output(controlled, request)
    }
}

/// 创建与桌面 AgentRuntime 相同路径策略的本地工具环境。
fn workflow_tool_environment(
    runtime: &AgentRuntime,
    parent: &RuntimeSession,
    cwd: &Path,
    state: Arc<WorkflowToolState>,
) -> Result<Arc<ToolEnvironment>, DriverError> {
    crate::shell_env::wait_for_capture_applied(Duration::from_secs(4));
    let output_directory = runtime
        .session_storage_directory(parent.session_id().as_str())
        .map_err(runtime_error)?
        .join("tool-output");
    // Workflow Tool 的 cwd 是 run-started 冻结值；工作流写入必须限制在该目录，不能
    // 继承普通桌面会话为兼容旧行为而关闭的 workspace guard。
    let environment = ToolEnvironment::new(cwd)
        .map(|environment| environment.with_workspace_guard())
        .and_then(|environment| environment.with_artifact_directory(output_directory))
        .map(|environment| {
            environment
                .with_file_mutation_recorder(Arc::new(WorkflowFileMutationRecorder::new(state)))
        })
        .map_err(|error| DriverError::new("workflow_tool_environment", error.to_string()))?;
    Ok(Arc::new(environment))
}

/// 只注册现有四个核心本地工具，并由 Runner 按名称精确筛选。
fn workflow_tool_registry(
    environment: Arc<ToolEnvironment>,
    requested_name: &str,
) -> Result<ToolRegistry, DriverError> {
    if !matches!(requested_name, "Read" | "Edit" | "Write" | "Bash") {
        return Err(DriverError::new(
            "workflow_tool_unavailable",
            format!("工作流工具 {requested_name} 不在受控本地工具白名单中"),
        ));
    }
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(ReadTool::new(Arc::clone(&environment))))
        .and_then(|_| registry.register(Arc::new(EditTool::new(Arc::clone(&environment)))))
        .and_then(|_| registry.register(Arc::new(WriteTool::new(Arc::clone(&environment)))))
        .and_then(|_| registry.register(Arc::new(BashTool::new(environment))))
        .map_err(|error| DriverError::new("workflow_tool_registry", error.to_string()))?;
    registry
        .select_exact(&[requested_name.to_owned()])
        .map_err(|error| DriverError::new("workflow_tool_registry", error.to_string()))
}

fn validate_request(
    parent: &RuntimeSession,
    request: &DriverRequest,
    context: &NodeContext,
) -> Result<(), DriverError> {
    if context.run_id.trim().is_empty()
        || context.node_id != request.node_id
        || context.address.node_id != request.node_id
    {
        return Err(DriverError::new(
            "workflow_context_mismatch",
            "工作流工具请求与节点地址不一致",
        ));
    }
    if context.cancellation_token.is_cancelled() {
        return Err(DriverError::new("cancelled", "工作流已取消"));
    }
    if request.effect == EffectClass::ReadOnly && request.name != "Read" {
        return Err(DriverError::new(
            "workflow_effect_mismatch",
            "只有 Read 工具可以声明只读副作用",
        ));
    }
    if !matches!(request.name.as_str(), "Read" | "Edit" | "Write" | "Bash") {
        return Err(DriverError::new(
            "workflow_tool_unavailable",
            "工作流工具不在受控本地工具白名单中",
        ));
    }
    if parent.session_id().as_str().trim().is_empty() {
        return Err(DriverError::new(
            "workflow_session",
            "工作流父 Session 标识为空",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct FrozenWorkflowRun {
    tool_call_id: String,
    launch_input_id: Option<String>,
    cwd: PathBuf,
}

fn frozen_run_binding(
    parent: &RuntimeSession,
    run_id: &str,
) -> Result<FrozenWorkflowRun, DriverError> {
    let events = parent
        .read_state(|state| {
            state
                .workflow_events
                .get(run_id)
                .cloned()
                .unwrap_or_default()
        })
        .map_err(|error| DriverError::new("workflow_journal", error.to_string()))?;
    let Some(started) = events
        .first()
        .filter(|event| event.event_type == "run-started")
    else {
        return Err(DriverError::new(
            "workflow_journal",
            "工作流工具缺少 run-started 冻结事实",
        ));
    };
    if started.run_id != run_id
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
    let cwd = started
        .payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| DriverError::new("workflow_journal", "工作流 cwd 不是绝对路径"))?;
    Ok(FrozenWorkflowRun {
        tool_call_id: started.tool_call_id.clone(),
        launch_input_id: started.launch_input_id.clone(),
        cwd,
    })
}

/// 一次 Workflow Tool 调用共享的持久身份和文件证据集合。
struct WorkflowToolState {
    parent: RuntimeSession,
    run_id: String,
    run_tool_call_id: String,
    launch_input_id: Option<String>,
    node_id: String,
    address: NodeAddress,
    tool_name: String,
    declared_effect: EffectClass,
    identity: ControlledToolIdentity,
    artifacts: Mutex<BTreeMap<String, ArtifactUse>>,
}

impl WorkflowToolState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        parent: RuntimeSession,
        run_id: String,
        run_tool_call_id: String,
        launch_input_id: Option<String>,
        node_id: String,
        address: NodeAddress,
        tool_name: String,
        declared_effect: EffectClass,
        identity: ControlledToolIdentity,
    ) -> Self {
        Self {
            parent,
            run_id,
            run_tool_call_id,
            launch_input_id,
            node_id,
            address,
            tool_name,
            declared_effect,
            identity,
            artifacts: Mutex::new(BTreeMap::new()),
        }
    }

    fn operation_id(&self, suffix: &str) -> String {
        stable_operation_id(
            "workflow-tool-event",
            &[&self.identity.operation_id, suffix],
        )
    }

    fn operation_id_for_path(&self, suffix: &str, path: &str) -> String {
        stable_operation_id(
            "workflow-file-event",
            &[&self.identity.operation_id, suffix, path],
        )
    }

    fn record_artifacts(&self, artifacts: impl IntoIterator<Item = ArtifactUse>) {
        let mut stored = self
            .artifacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for artifact in artifacts {
            stored.insert(artifact.artifact_id.to_string(), artifact);
        }
    }

    fn artifact_snapshot(&self) -> Vec<ArtifactUse> {
        self.artifacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn put_artifact(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
    ) -> Result<ArtifactUse, String> {
        let artifact = self
            .parent
            .put_artifact(bytes, media_type)
            .map_err(|error| error.to_string())?;
        let use_ = artifact.as_event_use();
        self.record_artifacts([use_.clone()]);
        Ok(use_)
    }

    fn commit_event(
        &self,
        suffix: &str,
        event_type: &str,
        payload: Value,
        mut artifacts: Vec<ArtifactUse>,
    ) -> Result<(), String> {
        artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        artifacts.dedup_by(|left, right| left.artifact_id == right.artifact_id);
        let operation_id = if suffix.starts_with("file-") {
            self.operation_id_for_path(suffix, &self.node_id)
        } else {
            self.operation_id(suffix)
        };
        let record = WorkflowJournalEvent {
            run_id: self.run_id.clone(),
            tool_call_id: self.run_tool_call_id.clone(),
            sequence: 0,
            event_type: event_type.to_owned(),
            payload,
            artifacts,
            actor_session_id: None,
            launch_input_id: self.launch_input_id.clone(),
        };
        self.parent
            .append_workflow_event(&operation_id, record)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

impl fmt::Debug for WorkflowToolState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowToolState")
            .field("run_id", &self.run_id)
            .field("node_id", &self.node_id)
            .field("tool_name", &self.tool_name)
            .finish_non_exhaustive()
    }
}

/// 独立于模型回合的 Workflow Tool 生命周期 Sink。
struct WorkflowToolLifecycle {
    state: Arc<WorkflowToolState>,
}

impl WorkflowToolLifecycle {
    fn new(state: WorkflowToolState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }
}

impl ControlledToolLifecycleSink for WorkflowToolLifecycle {
    fn preflight(
        &self,
        request: &ControlledToolPreflight,
    ) -> Result<Box<dyn ControlledToolReservation>, keencode_agent::AgentCommitSinkError> {
        if request.identity != self.state.identity
            || request.call.name != self.state.tool_name
            || request.call.id != self.state.identity.operation_id
        {
            return Err(keencode_agent::AgentCommitSinkError::rejected(
                "工作流 Tool 生命周期身份不一致",
            ));
        }
        Ok(Box::new(WorkflowToolReservation {
            state: Arc::clone(&self.state),
        }))
    }
}

struct WorkflowToolReservation {
    state: Arc<WorkflowToolState>,
}

impl ControlledToolReservation for WorkflowToolReservation {
    fn commit(
        &mut self,
        event: ControlledToolEvent,
    ) -> Result<(), keencode_agent::AgentCommitSinkError> {
        self.state
            .commit_controlled_event(event)
            .map_err(keencode_agent::AgentCommitSinkError::indeterminate)
    }

    fn consume(self: Box<Self>) {}

    fn release(self: Box<Self>) {}

    fn retain_indeterminate(self: Box<Self>, event: ControlledToolEvent) {
        let payload = json!({
            "nodeId": self.state.node_id,
            "nodeAddress": self.state.address,
            "toolName": self.state.tool_name,
            "operationId": self.state.identity.operation_id,
            "status": "indeterminate",
            "effect": "unknown",
            "startedEvent": controlled_event_summary(&event),
        });
        let _ = self.state.commit_event(
            "indeterminate",
            "tool-indeterminate",
            payload,
            self.state.artifact_snapshot(),
        );
    }
}

impl WorkflowToolState {
    fn commit_controlled_event(&self, event: ControlledToolEvent) -> Result<(), String> {
        match event {
            ControlledToolEvent::Requested { call, effect, .. } => self.commit_event(
                "requested",
                "tool-requested",
                json!({
                    "nodeId": self.node_id,
                    "nodeAddress": self.address,
                    "toolName": call.name,
                    "toolCallId": call.id,
                    "inputSha256": hash_json(&call.arguments),
                    "effect": controlled_effect_name(effect),
                }),
                self.artifact_snapshot(),
            ),
            ControlledToolEvent::ExecutionStarted { tool_call_id, .. } => self.commit_event(
                "started",
                "tool-execution-started",
                json!({
                    "nodeId": self.node_id,
                    "nodeAddress": self.address,
                    "toolName": self.tool_name,
                    "toolCallId": tool_call_id.to_string(),
                    "operationId": self.identity.operation_id,
                    "status": "started",
                }),
                self.artifact_snapshot(),
            ),
            ControlledToolEvent::Completed {
                tool_call_id,
                status,
                result,
                ..
            } => {
                let result_bytes =
                    serde_json::to_vec(&result).map_err(|error| error.to_string())?;
                let result_artifact = self.put_artifact(
                    &result_bytes,
                    Some(WORKFLOW_TOOL_RESULT_MEDIA_TYPE.to_owned()),
                )?;
                let status_name = completion_status_name(status);
                let effect = workflow_effect_name(self.declared_effect);
                self.commit_event(
                    "settled",
                    "tool-settled",
                    json!({
                        "nodeId": self.node_id,
                        "nodeAddress": self.address,
                        "toolName": self.tool_name,
                        "toolCallId": tool_call_id.to_string(),
                        "operationId": self.identity.operation_id,
                        "status": status_name,
                        "effect": effect,
                        "resultArtifactId": result_artifact.artifact_id,
                    }),
                    self.artifact_snapshot(),
                )
            }
        }
    }
}

/// Workflow 专用文件证据适配器：保存前后真实字节，完全不查找模型 ToolRequested。
#[derive(Clone)]
struct WorkflowFileMutationRecorder {
    state: Arc<WorkflowToolState>,
}

impl WorkflowFileMutationRecorder {
    fn new(state: Arc<WorkflowToolState>) -> Self {
        Self { state }
    }
}

impl fmt::Debug for WorkflowFileMutationRecorder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowFileMutationRecorder")
            .field("run_id", &self.state.run_id)
            .field("node_id", &self.state.node_id)
            .finish_non_exhaustive()
    }
}

impl FileMutationRecorder for WorkflowFileMutationRecorder {
    fn prepare(
        &self,
        context: &keencode_agent::ToolContext,
        path: &Path,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<Box<dyn PreparedFileMutation>, ToolError> {
        if context.cancellation.is_cancelled()
            || context.session_id.as_str() != self.state.identity.session_id.as_str()
            || context.turn_id.as_str() != self.state.identity.turn_id.as_str()
            || context.source_agent_id.as_str() != self.state.identity.source_agent_id.as_str()
            || context.tool_call_id.as_str() != self.state.identity.operation_id
            || !path.is_absolute()
        {
            return Err(workflow_recording_error());
        }
        let path = path
            .to_str()
            .ok_or_else(workflow_recording_error)?
            .to_owned();
        let before_artifact = before
            .map(|bytes| {
                self.state
                    .put_artifact(bytes, Some(WORKFLOW_TOOL_ARTIFACT_MEDIA_TYPE.to_owned()))
            })
            .transpose()
            .map_err(|_| workflow_recording_error())?;
        let after_artifact = self
            .state
            .put_artifact(after, Some(WORKFLOW_TOOL_ARTIFACT_MEDIA_TYPE.to_owned()))
            .map_err(|_| workflow_recording_error())?;
        let mut artifacts = Vec::new();
        if let Some(before) = before_artifact.clone() {
            artifacts.push(before);
        }
        artifacts.push(after_artifact.clone());
        self.state
            .commit_event(
                &format!("file-prepared-{}", hash_text(&path)),
                "file-change-prepared",
                json!({
                    "nodeId": self.state.node_id,
                    "nodeAddress": self.state.address,
                    "toolName": self.state.tool_name,
                    "path": path,
                    "before": before_artifact,
                    "after": after_artifact,
                    "status": "prepared",
                }),
                artifacts.clone(),
            )
            .map_err(|_| workflow_recording_error())?;
        Ok(Box::new(WorkflowPreparedFileMutation {
            state: Arc::clone(&self.state),
            path,
            artifacts,
        }))
    }
}

struct WorkflowPreparedFileMutation {
    state: Arc<WorkflowToolState>,
    path: String,
    artifacts: Vec<ArtifactUse>,
}

impl PreparedFileMutation for WorkflowPreparedFileMutation {
    fn mark_applied(&self) -> Result<(), ToolError> {
        self.state
            .commit_event(
                &format!("file-applied-{}", hash_text(&self.path)),
                "file-change-applied",
                json!({
                    "nodeId": self.state.node_id,
                    "nodeAddress": self.state.address,
                    "toolName": self.state.tool_name,
                    "path": self.path,
                    "status": "applied",
                }),
                self.artifacts.clone(),
            )
            .map_err(|_| workflow_recording_error())
    }
}

fn controlled_event_summary(event: &ControlledToolEvent) -> Value {
    match event {
        ControlledToolEvent::Requested { call, effect, .. } => json!({
            "type": "requested",
            "toolName": call.name,
            "toolCallId": call.id,
            "effect": controlled_effect_name(*effect),
        }),
        ControlledToolEvent::ExecutionStarted { tool_call_id, .. } => json!({
            "type": "started",
            "toolCallId": tool_call_id.to_string(),
        }),
        ControlledToolEvent::Completed {
            tool_call_id,
            status,
            ..
        } => json!({
            "type": "settled",
            "toolCallId": tool_call_id.to_string(),
            "status": completion_status_name(*status),
        }),
    }
}

fn controlled_output(
    result: ControlledToolResult,
    request: DriverRequest,
) -> Result<TypedOutput, DriverError> {
    if result.status != ToolCompletionStatus::Succeeded {
        let code = match result.status {
            ToolCompletionStatus::Cancelled => "cancelled",
            ToolCompletionStatus::Failed => "workflow_tool_failed",
            ToolCompletionStatus::Succeeded => unreachable!(),
        };
        return Err(DriverError::new(code, "工作流 Tool 未成功完成"));
    }
    if request.effect == EffectClass::ReadOnly
        && result.effect != Some(keencode_agent::ToolEffect::ReadOnly)
    {
        return Err(DriverError::new(
            "workflow_effect_mismatch",
            "工作流节点声明只读，但受控工具实际 effect 不是只读",
        ));
    }
    let value = serde_json::to_value(&result.result)
        .map_err(|error| DriverError::new("workflow_tool_output", error.to_string()))?;
    Ok(TypedOutput::new(
        value,
        request.output_type.unwrap_or_else(|| "json".to_owned()),
        request.effect,
    ))
}

fn runtime_error(error: AgentRuntimeError) -> DriverError {
    DriverError::new("agent_runtime", error.to_string())
}

fn workflow_recording_error() -> ToolError {
    ToolError::permanent(
        "workflow_file_recording_failed",
        "工作流文件变更证据无法可靠提交，已阻止文件副作用",
    )
}

fn workflow_tool_operation_id(run_id: &str, address: &NodeAddress, tool_name: &str) -> String {
    stable_operation_id(
        "workflow-tool-call",
        &[run_id, &node_address_key(address), tool_name],
    )
}

/// 为同一节点的只读恢复生成下一个稳定生命周期身份。
///
/// 首次执行使用不含尝试号的稳定身份；只有此前已有受控 Tool 生命周期时才追加尝试号，
/// 这样取消后的 Read 重执行不会与已结算的 cancelled 事件发生正文冲突。这个计数只
/// 影响身份，不放宽 Runtime 的 RecoveryRequired 栅栏，也不为写入/未知副作用创建重放。
fn workflow_tool_attempt(
    parent: &RuntimeSession,
    run_id: &str,
    address: &NodeAddress,
    tool_name: &str,
) -> Result<u32, DriverError> {
    let address_value = serde_json::to_value(address)
        .map_err(|error| DriverError::new("workflow_identity", error.to_string()))?;
    // 在 Journal 只读锁内统计生命周期，不复制整个运行事件向量；权威事实仍来自同一 state。
    let count = parent
        .read_state(|state| {
            workflow_tool_attempt_count(
                state.workflow_events.get(run_id).into_iter().flatten(),
                &address_value,
                tool_name,
            )
        })
        .map_err(|error| DriverError::new("workflow_journal", error.to_string()))?;
    u32::try_from(count)
        .map_err(|_| DriverError::new("workflow_budget", "工作流 Tool 尝试次数超出地址预算"))
}

#[cfg(test)]
fn workflow_tool_attempt_from_events(
    events: &[WorkflowJournalEvent],
    address: &NodeAddress,
    tool_name: &str,
) -> Result<u32, DriverError> {
    let address_value = serde_json::to_value(address)
        .map_err(|error| DriverError::new("workflow_identity", error.to_string()))?;
    let count = workflow_tool_attempt_count(events.iter(), &address_value, tool_name);
    u32::try_from(count)
        .map_err(|_| DriverError::new("workflow_budget", "工作流 Tool 尝试次数超出地址预算"))
}

fn workflow_tool_attempt_count<'a>(
    events: impl Iterator<Item = &'a WorkflowJournalEvent>,
    address_value: &Value,
    tool_name: &str,
) -> usize {
    events
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "tool-requested" | "tool-execution-started" | "tool-settled" | "tool-indeterminate"
            ) && event.payload.get("nodeAddress") == Some(address_value)
                && event.payload.get("toolName").and_then(Value::as_str) == Some(tool_name)
        })
        .count()
}

fn workflow_tool_operation_id_for_attempt(
    run_id: &str,
    address: &NodeAddress,
    tool_name: &str,
    attempt: u32,
) -> String {
    if attempt == 0 {
        return workflow_tool_operation_id(run_id, address, tool_name);
    }
    let attempt = attempt.to_string();
    stable_operation_id(
        "workflow-tool-call",
        &[
            run_id,
            &node_address_key(address),
            tool_name,
            "attempt",
            &attempt,
        ],
    )
}

fn workflow_tool_turn_id(run_id: &str, address: &NodeAddress) -> String {
    stable_operation_id("workflow-tool-turn", &[run_id, &node_address_key(address)])
}

fn workflow_tool_turn_id_for_attempt(run_id: &str, address: &NodeAddress, attempt: u32) -> String {
    if attempt == 0 {
        return workflow_tool_turn_id(run_id, address);
    }
    let attempt = attempt.to_string();
    stable_operation_id(
        "workflow-tool-turn",
        &[run_id, &node_address_key(address), "attempt", &attempt],
    )
}

fn workflow_tool_agent_id(run_id: &str, address: &NodeAddress) -> String {
    stable_operation_id("workflow-tool-agent", &[run_id, &node_address_key(address)])
}

fn workflow_tool_agent_id_for_attempt(run_id: &str, address: &NodeAddress, attempt: u32) -> String {
    if attempt == 0 {
        return workflow_tool_agent_id(run_id, address);
    }
    let attempt = attempt.to_string();
    stable_operation_id(
        "workflow-tool-agent",
        &[run_id, &node_address_key(address), "attempt", &attempt],
    )
}

fn stable_operation_id(prefix: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode.workflow.tool.v1\0");
    digest.update(prefix.as_bytes());
    for part in parts {
        digest.update([0]);
        digest.update(part.as_bytes());
    }
    format!("{prefix}-{:x}", digest.finalize())
}

fn node_address_key(address: &NodeAddress) -> String {
    let invocation = address
        .invocation
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    if invocation.is_empty() {
        address.node_id.clone()
    } else {
        format!("{}@{}", address.node_id, invocation)
    }
}

fn hash_text(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
}

fn hash_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn controlled_effect_name(effect: ToolEffect) -> &'static str {
    match effect {
        ToolEffect::ReadOnly => "read_only",
        ToolEffect::ChangesState => "write",
    }
}

fn workflow_effect_name(effect: EffectClass) -> &'static str {
    match effect {
        EffectClass::ReadOnly => "read_only",
        EffectClass::Write => "write",
        EffectClass::Unknown => "unknown",
    }
}

fn completion_status_name(status: ToolCompletionStatus) -> &'static str {
    match status {
        ToolCompletionStatus::Succeeded => "succeeded",
        ToolCompletionStatus::Failed => "failed",
        ToolCompletionStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_invocations_receive_distinct_tool_identity() {
        let outer = NodeAddress {
            node_id: "write".to_owned(),
            invocation: vec![2, 1],
        };
        let sibling = NodeAddress {
            node_id: "write".to_owned(),
            invocation: vec![3, 1],
        };
        assert_ne!(
            workflow_tool_operation_id("run-a", &outer, "Write"),
            workflow_tool_operation_id("run-a", &sibling, "Write")
        );
        assert_ne!(
            workflow_tool_turn_id("run-a", &outer),
            workflow_tool_turn_id("run-a", &sibling)
        );
    }

    #[test]
    fn cancelled_read_retry_gets_new_identity_without_reusing_settled_event() {
        let address = NodeAddress {
            node_id: "read_once".to_owned(),
            invocation: vec![17],
        };
        let event = |event_type: &str| WorkflowJournalEvent {
            run_id: "run-a".to_owned(),
            tool_call_id: "launch".to_owned(),
            sequence: 1,
            event_type: event_type.to_owned(),
            payload: json!({
                "nodeId": "read_once",
                "nodeAddress": address.clone(),
                "toolName": "Read",
            }),
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: None,
        };
        let events = vec![
            event("tool-requested"),
            event("tool-execution-started"),
            event("tool-settled"),
        ];
        let attempt = workflow_tool_attempt_from_events(&events, &address, "Read").unwrap();
        assert_eq!(attempt, 3);
        assert_ne!(
            workflow_tool_operation_id("run-a", &address, "Read"),
            workflow_tool_operation_id_for_attempt("run-a", &address, "Read", attempt)
        );
    }

    #[test]
    fn unrelated_tool_lifecycle_does_not_change_read_retry_identity() {
        let address = NodeAddress {
            node_id: "read_once".to_owned(),
            invocation: vec![17],
        };
        let unrelated = WorkflowJournalEvent {
            run_id: "run-a".to_owned(),
            tool_call_id: "launch".to_owned(),
            sequence: 1,
            event_type: "tool-settled".to_owned(),
            payload: json!({
                "nodeAddress": {"node_id": "read_once", "invocation": [18]},
                "toolName": "Read",
            }),
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: None,
        };
        assert_eq!(
            workflow_tool_attempt_from_events(&[unrelated], &address, "Read").unwrap(),
            0
        );
    }

    #[test]
    fn workflow_effect_names_keep_recovery_classification() {
        assert_eq!(workflow_effect_name(EffectClass::ReadOnly), "read_only");
        assert_eq!(workflow_effect_name(EffectClass::Write), "write");
        assert_eq!(workflow_effect_name(EffectClass::Unknown), "unknown");
    }
}
