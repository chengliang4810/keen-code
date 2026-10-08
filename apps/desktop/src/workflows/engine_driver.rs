//! `keencode-workflow` 执行器到 WorkflowHost 的适配。
//!
//! 纯 Rust 引擎只依赖叶节点 Driver 和自己的 Journal；这里把引擎事件转换成父 Session
//! 的 WorkflowJournalEvent，并在 Journal 确认后才向 Host sink 发布进度。

use super::ports::{
    WorkflowExecutionDriver, WorkflowExecutionRequest, WorkflowFuture, WorkflowJournalPort,
    WorkflowPreflightMode, WorkflowProgressSink, WorkflowQuestionResolver,
};
use super::types::*;
use keencode_resources::ArtifactCapacity;
use keencode_workflow::{
    ExecutionOptions, ExecutionRequest, JournalError, JournalEvent, RecoverySnapshot,
    WorkflowDriver as EngineLeafDriver, WorkflowExecutor, WorkflowFuture as EngineFuture,
    WorkflowJournal as EngineJournal, WorkflowLimits, compile,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// 使用真实 AgentRuntime/工具注册表的叶节点 Driver 创建一个可提交到 Host 的执行 Driver。
pub struct EngineWorkflowDriver {
    leaf_driver: Arc<dyn EngineLeafDriver>,
    journal: Arc<dyn WorkflowJournalPort>,
    question_resolver: Arc<dyn WorkflowQuestionResolver>,
    cancellation: Arc<WorkflowCancellationState>,
}

/// 工作流取消令牌的同步注册表。
///
/// Host 在异步执行任务排队前调用 `prepare`，所以取消请求不会因为任务尚未第一次
/// poll 而观察到一个不存在的运行。令牌只在执行 future 结束后移除；未知 run 不会
/// 通过取消入口隐式创建令牌。
struct WorkflowCancellationState {
    tokens: Mutex<BTreeMap<String, CancellationToken>>,
}

impl WorkflowCancellationState {
    fn new() -> Self {
        Self {
            tokens: Mutex::new(BTreeMap::new()),
        }
    }

    fn prepare(&self, run_id: &str) -> Result<(), WorkflowRuntimeError> {
        let mut tokens = self.tokens.lock().map_err(|_| {
            WorkflowRuntimeError("workflow cancellation state unavailable".to_owned())
        })?;
        if tokens.contains_key(run_id) {
            return Err(WorkflowRuntimeError(format!(
                "workflow run is already active: {run_id}"
            )));
        }
        tokens.insert(run_id.to_owned(), CancellationToken::new());
        Ok(())
    }

    fn token(&self, run_id: &str) -> Result<CancellationToken, WorkflowRuntimeError> {
        let mut tokens = self.tokens.lock().map_err(|_| {
            WorkflowRuntimeError("workflow cancellation state unavailable".to_owned())
        })?;
        Ok(tokens
            .entry(run_id.to_owned())
            .or_insert_with(CancellationToken::new)
            .clone())
    }

    fn existing(&self, run_id: &str) -> Result<CancellationToken, WorkflowRuntimeError> {
        let tokens = self.tokens.lock().map_err(|_| {
            WorkflowRuntimeError("workflow cancellation state unavailable".to_owned())
        })?;
        tokens
            .get(run_id)
            .cloned()
            .ok_or_else(|| WorkflowRuntimeError(format!("workflow run is not active: {run_id}")))
    }

    fn remove(&self, run_id: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(run_id);
        }
    }
}

impl EngineWorkflowDriver {
    pub fn new(
        leaf_driver: Arc<dyn EngineLeafDriver>,
        journal: Arc<dyn WorkflowJournalPort>,
        question_resolver: Arc<dyn WorkflowQuestionResolver>,
    ) -> Self {
        Self {
            leaf_driver,
            journal,
            question_resolver,
            cancellation: Arc::new(WorkflowCancellationState::new()),
        }
    }

    fn token(&self, run_id: &str) -> Result<CancellationToken, WorkflowRuntimeError> {
        self.cancellation.token(run_id)
    }

    fn existing_token(&self, run_id: &str) -> Result<CancellationToken, WorkflowRuntimeError> {
        self.cancellation.existing(run_id)
    }

    fn prepare_run(&self, run_id: &str) -> Result<(), WorkflowRuntimeError> {
        self.cancellation.prepare(run_id)
    }
}

impl WorkflowExecutionDriver for EngineWorkflowDriver {
    fn preflight(
        &self,
        request: &WorkflowExecutionRequest,
        mode: WorkflowPreflightMode,
    ) -> Result<(), WorkflowRuntimeError> {
        let compiled = compile(request.definition.clone())
            .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
        if compiled.definition_hash() != request.canonical_hash {
            return Err(WorkflowRuntimeError(
                "workflow definition hash does not match the frozen run".to_owned(),
            ));
        }
        let completed = match mode {
            WorkflowPreflightMode::Start => Vec::new(),
            WorkflowPreflightMode::Resume => recovery_from_events(
                &request.run_id,
                self.journal
                    .all_events(&request.run_id)
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))?,
            )
            .map_err(|error| WorkflowRuntimeError(error.to_string()))?
            .map(|snapshot| {
                snapshot
                    .completed
                    .into_iter()
                    .map(|node| node.address)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        };
        let capacity = self
            .journal
            .artifact_capacity()
            .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
        ensure_artifact_capacity(&compiled, &completed, capacity)
    }

    fn prepare(&self, run_id: &str) -> Result<(), WorkflowRuntimeError> {
        self.prepare_run(run_id)
    }

    fn start(
        &self,
        request: WorkflowExecutionRequest,
        sink: Arc<dyn WorkflowProgressSink>,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
        let leaf_driver = Arc::clone(&self.leaf_driver);
        let journal = Arc::clone(&self.journal);
        let token = match self.token(&request.run_id) {
            Ok(token) => token,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let run_id = request.run_id.clone();
        let cancellation = Arc::clone(&self.cancellation);
        Box::pin(async move {
            let result = async {
                validate_frozen_request(&request, journal.as_ref())?;
                let compiled = compile(request.definition.clone())
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
                if compiled.definition_hash() != request.canonical_hash {
                    return Err(WorkflowRuntimeError(
                        "workflow definition hash does not match the frozen run".to_owned(),
                    ));
                }
                let engine_journal = Arc::new(EngineJournalAdapter::new(
                    journal,
                    Arc::clone(&sink),
                    run_id.clone(),
                    request
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| format!("workflow:{run_id}")),
                    request.launch_input_id.clone(),
                ));
                let executor = WorkflowExecutor::new(compiled, leaf_driver, engine_journal);
                let options = ExecutionOptions {
                    limits: decode_limits(request.budgets.as_ref())?,
                    cancellation_token: token,
                };
                let input_hash = keencode_workflow::hash_inputs(&request.inputs)
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
                executor
                    .execute(ExecutionRequest {
                        run_id: run_id.clone(),
                        inputs: request.inputs,
                        options,
                        recovery: None,
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| WorkflowRuntimeError(format!("{}: {error}", input_hash)))
            }
            .await;
            cancellation.remove(&run_id);
            result
        })
    }

    fn cancel(&self, run_id: String) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
        let result = self.existing_token(&run_id);
        Box::pin(async move {
            result?.cancel();
            Ok(())
        })
    }

    fn resume(
        &self,
        request: WorkflowExecutionRequest,
        sink: Arc<dyn WorkflowProgressSink>,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
        let leaf_driver = Arc::clone(&self.leaf_driver);
        let journal = Arc::clone(&self.journal);
        let token = match self.token(&request.run_id) {
            Ok(token) => token,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let run_id = request.run_id.clone();
        let cancellation = Arc::clone(&self.cancellation);
        Box::pin(async move {
            let result = async {
                validate_frozen_request(&request, journal.as_ref())?;
                let compiled = compile(request.definition.clone())
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
                if compiled.definition_hash() != request.canonical_hash {
                    return Err(WorkflowRuntimeError(
                        "workflow definition hash does not match the frozen run".to_owned(),
                    ));
                }
                let engine_journal = Arc::new(EngineJournalAdapter::new(
                    journal,
                    sink,
                    run_id.clone(),
                    request
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| format!("workflow:{run_id}")),
                    request.launch_input_id.clone(),
                ));
                let recovery = engine_journal
                    .load_recovery(&run_id)
                    .await
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
                let executor = WorkflowExecutor::new(compiled, leaf_driver, engine_journal);
                executor
                    .execute(ExecutionRequest {
                        run_id: run_id.clone(),
                        inputs: request.inputs,
                        options: ExecutionOptions {
                            limits: decode_limits(request.budgets.as_ref())?,
                            cancellation_token: token,
                        },
                        recovery,
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| WorkflowRuntimeError(error.to_string()))
            }
            .await;
            cancellation.remove(&run_id);
            result
        })
    }

    fn resolve_question(
        &self,
        qid: String,
        answer: String,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
        self.question_resolver.resolve(qid, answer)
    }
}

/// 校验每次执行都来自同一个 `run-started` 冻结事实。
///
/// 这一步在 start/resume 两条路径都执行：resume 只能重放已确认的定义、输入、父会话、
/// 工作目录、模型和预算，调用方无法通过构造新的 `WorkflowExecutionRequest` 改写运行语义。
fn validate_frozen_request(
    request: &WorkflowExecutionRequest,
    journal: &dyn WorkflowJournalPort,
) -> Result<(), WorkflowRuntimeError> {
    let events = journal
        .all_events(&request.run_id)
        .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
    let started = events
        .iter()
        .find(|event| event.event_type == "run-started")
        .ok_or_else(|| WorkflowRuntimeError("workflow run has no run-started event".to_owned()))?;
    let payload = &started.payload;
    let required_string = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| WorkflowRuntimeError(format!("run-started is missing {field}")))
    };
    if required_string("parentSessionId")? != request.parent_session_id {
        return Err(WorkflowRuntimeError(
            "workflow parent session does not match the frozen run".to_owned(),
        ));
    }
    let cwd = required_string("cwd")?;
    if cwd != request.cwd.to_string_lossy().as_ref() {
        return Err(WorkflowRuntimeError(
            "workflow cwd does not match the frozen run".to_owned(),
        ));
    }
    if required_string("canonicalHash")? != request.canonical_hash {
        return Err(WorkflowRuntimeError(
            "workflow canonical hash does not match the frozen run".to_owned(),
        ));
    }
    let input_hash = keencode_workflow::hash_inputs(&request.inputs)
        .map_err(|error| WorkflowRuntimeError(error.to_string()))?;
    if required_string("inputHash")? != input_hash {
        return Err(WorkflowRuntimeError(
            "workflow input hash does not match the frozen run".to_owned(),
        ));
    }
    let json_or_null = |field: &str| payload.get(field).cloned().unwrap_or(Value::Null);
    let models = payload.get("models").ok_or_else(|| {
        WorkflowRuntimeError("workflow run-started is missing frozen models".to_owned())
    })?;
    WorkflowModelSelection::parse(models).map_err(|error| {
        WorkflowRuntimeError(format!("workflow run-started models are invalid: {error}"))
    })?;
    if json_or_null("models") != request.model_selection.clone().unwrap_or(Value::Null) {
        return Err(WorkflowRuntimeError(
            "workflow model selection does not match the frozen run".to_owned(),
        ));
    }
    if json_or_null("budgets") != request.budgets.clone().unwrap_or(Value::Null) {
        return Err(WorkflowRuntimeError(
            "workflow budgets do not match the frozen run".to_owned(),
        ));
    }
    let expected_tool_call_id = request
        .tool_call_id
        .clone()
        .unwrap_or_else(|| format!("workflow:{}", request.run_id));
    if started.tool_call_id != expected_tool_call_id {
        return Err(WorkflowRuntimeError(
            "workflow tool call does not match the frozen run".to_owned(),
        ));
    }
    if started.launch_input_id != request.launch_input_id {
        return Err(WorkflowRuntimeError(
            "workflow launch input does not match the frozen run".to_owned(),
        ));
    }
    let predecessor = payload
        .get("predecessorRunId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if predecessor != request.predecessor_run_id {
        return Err(WorkflowRuntimeError(
            "workflow predecessor does not match the frozen run".to_owned(),
        ));
    }
    Ok(())
}

struct EngineJournalAdapter {
    journal: Arc<dyn WorkflowJournalPort>,
    sink: Arc<dyn WorkflowProgressSink>,
    run_id: String,
    tool_call_id: String,
    launch_input_id: Option<String>,
    pending: Mutex<BTreeMap<String, WorkflowJournalEvent>>,
}

impl EngineJournalAdapter {
    fn new(
        journal: Arc<dyn WorkflowJournalPort>,
        sink: Arc<dyn WorkflowProgressSink>,
        run_id: String,
        tool_call_id: String,
        launch_input_id: Option<String>,
    ) -> Self {
        Self {
            journal,
            sink,
            run_id,
            tool_call_id,
            launch_input_id,
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    fn key(event: &JournalEvent) -> String {
        serde_json::to_string(event).unwrap_or_else(|_| "<invalid-engine-event>".to_owned())
    }

    fn translate(&self, event: &JournalEvent) -> Result<WorkflowJournalEvent, JournalError> {
        let (event_type, payload) = match event {
            JournalEvent::NodeStarted(value) => ("node-started", serde_json::to_value(value)),
            JournalEvent::NodeSettled(value) => ("node-settled", serde_json::to_value(value)),
            JournalEvent::NodeReused(value) => ("node-reused", serde_json::to_value(value)),
            JournalEvent::RunFinished(value) => ("run-settled", serde_json::to_value(value)),
        };
        Ok(WorkflowJournalEvent {
            run_id: self.run_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
            sequence: 0,
            event_type: event_type.to_owned(),
            payload: payload
                .map_err(|error| JournalError::new("serialization", error.to_string()))?,
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: self.launch_input_id.clone(),
        })
    }
}

impl EngineJournal for EngineJournalAdapter {
    fn append<'a>(&'a self, event: JournalEvent) -> EngineFuture<'a, Result<(), JournalError>> {
        Box::pin(async move {
            // sequence=0 交给父 RuntimeSession 的原子追加出口分配；本地先读尾序号
            // 会与 actor/native 事件并发碰撞，operationId 则必须独立于 sequence 稳定去重。
            let record = self.translate(&event)?;
            let operation_id = super::workflow_event_operation_id(&record);
            let committed = self
                .journal
                .commit_event(&operation_id, record)
                .map_err(|error| JournalError::new("commit", error.to_string()))?;
            self.pending
                .lock()
                .map_err(|_| JournalError::new("state", "workflow event state unavailable"))?
                .insert(Self::key(&event), committed.event);
            Ok(())
        })
    }

    fn notify<'a>(&'a self, event: JournalEvent) -> EngineFuture<'a, Result<(), JournalError>> {
        Box::pin(async move {
            let record = self
                .pending
                .lock()
                .map_err(|_| JournalError::new("state", "workflow event state unavailable"))?
                .remove(&Self::key(&event))
                .ok_or_else(|| JournalError::new("notify", "workflow event was not committed"))?;
            self.sink
                .publish(
                    progress_from_workflow_event(&record),
                    record.artifacts.clone(),
                )
                .map_err(|error| JournalError::new("notify", error.to_string()))
        })
    }

    fn load_recovery<'a>(
        &'a self,
        run_id: &'a str,
    ) -> EngineFuture<'a, Result<Option<RecoverySnapshot>, JournalError>> {
        Box::pin(async move {
            let events = self
                .journal
                .all_events(run_id)
                .map_err(|error| JournalError::new("read", error.to_string()))?;
            recovery_from_events(run_id, events)
        })
    }
}

fn recovery_from_events(
    run_id: &str,
    events: Vec<WorkflowJournalEvent>,
) -> Result<Option<RecoverySnapshot>, JournalError> {
    if events.is_empty() {
        return Ok(None);
    }
    let mut completed = Vec::new();
    let mut started = BTreeMap::<String, keencode_workflow::StartedNode>::new();
    let mut definition_hash = String::new();
    let mut input_hash = String::new();
    for event in events {
        if event.event_type == "run-started" {
            definition_hash = event
                .payload
                .get("canonicalHash")
                .or_else(|| event.payload.get("definition_hash"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            input_hash = event
                .payload
                .get("inputHash")
                .or_else(|| event.payload.get("input_hash"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        if event.event_type == "node-started"
            && let Ok(value) =
                serde_json::from_value::<keencode_workflow::NodeStarted>(event.payload.clone())
        {
            let key = serde_json::to_string(&value.address)
                .map_err(|error| JournalError::new("serialization", error.to_string()))?;
            started.insert(
                key,
                keencode_workflow::StartedNode {
                    address: value.address,
                    effect: value.effect,
                },
            );
        }
        if event.event_type == "node-settled"
            && let Ok(value) =
                serde_json::from_value::<keencode_workflow::NodeSettled>(event.payload.clone())
        {
            let key = serde_json::to_string(&value.address)
                .map_err(|error| JournalError::new("serialization", error.to_string()))?;
            // 只读节点的 cancelled/failed 终态可以在 Resume 时安全重新执行；
            // 已越过执行边界的 Write/Unknown 仍保留为 started，让 core 返回
            // Indeterminate，避免把未知副作用静默重放。
            if let Some(started_node) = started.remove(&key)
                && retain_started_after_settlement(value.status, started_node.effect)
            {
                started.insert(key.clone(), started_node);
            }
            if let Some(output) = value.output
                && value.status == keencode_workflow::NodeStatus::Succeeded
            {
                completed.push(keencode_workflow::CompletedNode {
                    address: value.address,
                    output,
                });
            }
        }
    }
    Ok(Some(RecoverySnapshot {
        run_id: run_id.to_owned(),
        definition_hash,
        input_hash,
        completed,
        started: started.into_values().collect(),
    }))
}

fn ensure_artifact_capacity(
    compiled: &keencode_workflow::CompiledWorkflow,
    completed: &[keencode_workflow::NodeAddress],
    capacity: ArtifactCapacity,
) -> Result<(), WorkflowRuntimeError> {
    let required = compiled.artifact_slots_after_completed(completed) as usize;
    if capacity.remaining() < required {
        return Err(WorkflowRuntimeError(format!(
            "artifact_capacity: available={} required={} committed={} maximum={}",
            capacity.remaining(),
            required,
            capacity.committed_unique_artifacts,
            capacity.maximum_unique_artifacts,
        )));
    }
    Ok(())
}

fn retain_started_after_settlement(
    status: keencode_workflow::NodeStatus,
    effect: EffectClass,
) -> bool {
    status != keencode_workflow::NodeStatus::Succeeded
        && matches!(effect, EffectClass::Write | EffectClass::Unknown)
}

fn decode_limits(value: Option<&Value>) -> Result<WorkflowLimits, WorkflowRuntimeError> {
    let Some(value) = value else {
        return Ok(WorkflowLimits::default());
    };
    serde_json::from_value(value.clone()).map_err(|error| WorkflowRuntimeError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn cancellation_prepare_is_atomic_and_unknown_runs_are_not_created() {
        let state = WorkflowCancellationState::new();
        state
            .prepare("run-1")
            .expect("first prepare should reserve run");
        assert!(state.prepare("run-1").is_err());

        let token = state
            .existing("run-1")
            .expect("prepared run should be cancellable");
        token.cancel();
        assert!(
            state
                .existing("run-1")
                .expect("cancellation keeps the token until execution exits")
                .is_cancelled()
        );

        state.remove("run-1");
        assert!(state.existing("run-1").is_err());
        assert!(state.existing("never-started").is_err());
    }

    #[test]
    fn recovery_keeps_mutating_non_success_nodes_indeterminate() {
        assert!(retain_started_after_settlement(
            keencode_workflow::NodeStatus::Cancelled,
            EffectClass::Write,
        ));
        assert!(retain_started_after_settlement(
            keencode_workflow::NodeStatus::Failed,
            EffectClass::Unknown,
        ));
        assert!(!retain_started_after_settlement(
            keencode_workflow::NodeStatus::Cancelled,
            EffectClass::ReadOnly,
        ));
        assert!(!retain_started_after_settlement(
            keencode_workflow::NodeStatus::Succeeded,
            EffectClass::Write,
        ));
    }

    #[test]
    fn artifact_capacity_preflight_accounts_for_reuse_and_cancelled_slots() {
        let definition = keencode_workflow::WorkflowDefinitionV1 {
            version: 1,
            meta: keencode_workflow::WorkflowMeta {
                name: "capacity".to_owned(),
                ..Default::default()
            },
            inputs: BTreeMap::new(),
            body: vec![keencode_workflow::Node::Repeat {
                node_id: "loop".to_owned(),
                body: vec![keencode_workflow::Node::Tool(keencode_workflow::ToolNode {
                    node_id: "read".to_owned(),
                    name: "Read".to_owned(),
                    input: keencode_workflow::ValueExpr::literal(json!({"path": "x"})),
                    config: serde_json::Value::Null,
                    output_type: Some("json".to_owned()),
                    effect: EffectClass::ReadOnly,
                })],
                max_iterations: 1024,
            }],
        };
        let compiled = keencode_workflow::compile(definition).expect("capacity workflow compiles");
        let cancelled_slot = ArtifactCapacity {
            committed_unique_artifacts: 1,
            maximum_unique_artifacts: 1024,
        };
        assert!(ensure_artifact_capacity(&compiled, &[], cancelled_slot).is_err());

        let larger_store = ArtifactCapacity {
            committed_unique_artifacts: 1,
            maximum_unique_artifacts: 2048,
        };
        assert!(ensure_artifact_capacity(&compiled, &[], larger_store).is_ok());
        assert!(
            ensure_artifact_capacity(
                &compiled,
                &[keencode_workflow::NodeAddress {
                    node_id: "read".to_owned(),
                    invocation: vec![0],
                }],
                cancelled_slot,
            )
            .is_ok()
        );
    }
}
