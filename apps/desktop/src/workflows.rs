//! 桌面 WorkflowHost：定义文件、父会话 Journal、纯 Rust 引擎和 RPC 查询的装配边界。
//!
//! 运行事实全部来自 `WorkflowJournalPort`。本模块只保留取消 token 之外的瞬时调度句柄，
//! 不写 run JSON 日志，也不把内存投影当成恢复来源。

pub mod agent_tools;
mod engine_driver;
mod native_driver;
mod ports;
mod runtime_journal;
mod store;
pub mod types;
pub(crate) mod workspace_gate;

#[allow(unused_imports)]
pub use engine_driver::EngineWorkflowDriver;
#[allow(unused_imports)]
pub use native_driver::NativeWorkflowDriver;
#[allow(unused_imports)]
pub use ports::{
    CommittedWorkflowEvent, WorkflowActorBinding, WorkflowActorTranscriptReader,
    WorkflowExecutionDriver, WorkflowExecutionRequest, WorkflowFuture, WorkflowJournalPort,
    WorkflowLaunchRecord, WorkflowPreflightMode, WorkflowProgressSink, WorkflowQuestionResolver,
    workflow_actor_transcript_reader, workflow_question_resolver,
};
#[allow(unused_imports)]
pub use runtime_journal::RuntimeWorkflowJournal;
pub use store::{WorkflowStore, WorkflowStoreError};
pub use types::*;

use keencode_workflow::{EffectClass, InputSpec, InputType, Node, WorkflowLimits, hash_inputs};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// Host 会随父 Session 的弱缓存生命周期重建；计数器必须跨 Host 共享，避免同一毫秒
// 的新 Host 又从 ordinal=1 开始，生成与旧 Journal 冲突的 run ID。
static NEXT_WORKFLOW_RUN: AtomicU64 = AtomicU64::new(1);

// Host 可能因父 Session 的弱缓存重建；这个进程级门把 Journal 查询和首个
// run-started 提交放在同一临界区，避免两个 Host 同时为同一 launchInputId 派生 actor。
// 它只承担瞬时并发协调，不保存任何运行事实；事实仍由 Workflow Journal 持有。
static WORKFLOW_LAUNCH_GATE: Mutex<()> = Mutex::new(());

// resume 会先追加 run-resumed 再异步预留 Driver；这个瞬时集合阻止同一 run 在
// 该窗口内追加两条 resume 事实。它不保存运行状态，终态和冷恢复仍只读取 Journal。
static WORKFLOW_RESUME_CLAIMS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// WorkflowHost 的统一错误；RPC 层将它序列化为稳定的 `workflow_*` 错误。
#[derive(Debug)]
pub enum WorkflowHostError {
    Store(WorkflowStoreError),
    Journal(WorkflowJournalError),
    Runtime(WorkflowRuntimeError),
    InvalidInputs(String),
    NotFound(String),
    NotResumable(String),
    FrozenRun(String),
    LaunchConflict(String),
    /// 命令自己的确定性拒绝；RPC 层映射到 Source 的稳定 reason code。
    SettingsRejected {
        reason: String,
        detail: String,
    },
    PayloadTooLarge(usize),
}

impl std::fmt::Display for WorkflowHostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Journal(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::InvalidInputs(error) => write!(formatter, "invalid workflow inputs: {error}"),
            Self::NotFound(value) => write!(formatter, "workflow resource not found: {value}"),
            Self::NotResumable(value) => write!(formatter, "workflow run cannot resume: {value}"),
            Self::FrozenRun(value) => write!(formatter, "workflow frozen run is invalid: {value}"),
            Self::LaunchConflict(value) => write!(formatter, "workflow launch conflict: {value}"),
            Self::SettingsRejected { detail, .. } => {
                write!(formatter, "workflow settings rejected: {detail}")
            }
            Self::PayloadTooLarge(bytes) => write!(
                formatter,
                "workflow journal payload is too large: {bytes} bytes"
            ),
        }
    }
}

impl std::error::Error for WorkflowHostError {}

impl From<WorkflowStoreError> for WorkflowHostError {
    fn from(error: WorkflowStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<WorkflowJournalError> for WorkflowHostError {
    fn from(error: WorkflowJournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<WorkflowRuntimeError> for WorkflowHostError {
    fn from(error: WorkflowRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

fn map_preflight_error(error: WorkflowRuntimeError) -> WorkflowHostError {
    if let Some(detail) = error.0.strip_prefix("artifact_capacity:") {
        return WorkflowHostError::SettingsRejected {
            reason: "artifact_capacity".to_owned(),
            detail: detail.trim().to_owned(),
        };
    }
    WorkflowHostError::Runtime(error)
}

/// 事件去重身份只由事件正文决定，不能把运行时分配的 sequence 混入 operationId。
/// sequence=0 的记录会交给父 RuntimeSession 在同一控制提交事务内分配。
pub(crate) fn workflow_event_operation_id(event: &WorkflowJournalEvent) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode.workflow.event.v1\0");
    digest.update(event.run_id.as_bytes());
    digest.update([0]);
    digest.update(event.tool_call_id.as_bytes());
    digest.update([0]);
    digest.update(event.event_type.as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(&event.payload).unwrap_or_default());
    digest.update([0]);
    digest.update(serde_json::to_vec(&event.artifacts).unwrap_or_default());
    digest.update([0]);
    digest.update(
        event
            .actor_session_id
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    digest.update([0]);
    digest.update(
        event
            .launch_input_id
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    format!("workflow-event-{:x}", digest.finalize())
}

/// 生产 WorkflowHost。`journal` 与 `driver` 均由桌面装配注入，避免此层复制 AgentRuntime。
pub struct WorkflowHost {
    store: WorkflowStore,
    journal: Arc<dyn WorkflowJournalPort>,
    driver: Arc<dyn WorkflowExecutionDriver>,
}

/// 创建 run 时一次性携带的冻结输入；把这些字段集中在一个值里，避免调用方漏传
/// 模型、预算或修订前驱，且让启动函数保持可审计的窄签名。
struct WorkflowStartDefinition {
    definition: WorkflowDefinition,
    scope: WorkflowScope,
    inputs: Value,
    parent_session_id: String,
    tool_call_id: Option<String>,
    launch_input_id: Option<String>,
    cwd: PathBuf,
    model_selection: Option<Value>,
    budgets: Option<Value>,
    subagent_model: Option<String>,
    predecessor_run_id: Option<String>,
    /// amend 已在取消 predecessor 前完成容量快照；成功后不重复做会误伤旧 run 的预检。
    capacity_preflighted: bool,
}

fn predecessor_is_amendable(summary: &WorkflowRunSummary) -> bool {
    matches!(
        summary.status,
        WorkflowRunStatus::Running | WorkflowRunStatus::Pending
    ) || (matches!(summary.status, WorkflowRunStatus::Stopped)
        && matches!(
            summary.stop_reason.as_ref(),
            Some(WorkflowRunStopReason::Superseded)
        ))
}

impl WorkflowHost {
    /// 与当前工作区绑定的生产 Host 构造器。项目存储路径在桌面装配层解析并授权，
    /// 运行期间的 RPC 不再接受前端传入的任意 projectStorage。
    pub fn new_with_project_storage(
        data_root: impl Into<PathBuf>,
        project_storage: impl Into<PathBuf>,
        journal: Arc<dyn WorkflowJournalPort>,
        driver: Arc<dyn WorkflowExecutionDriver>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: WorkflowStore::with_project_storage(data_root, project_storage),
            journal,
            driver,
        })
    }

    /// 启动新 run；`run-started` 先写 Journal，成功后才调用真实引擎。
    pub fn start(
        self: &Arc<Self>,
        request: WorkflowRunStartRequest,
    ) -> Result<WorkflowRunStartResult, WorkflowHostError> {
        // 项目定义只能从 Host 装配时注入的授权根读取；请求中的 projectStorage
        // 是不可信的协议字段，不能在没有授权根时成为路径回退。
        let result = self.load_definition(
            request.scope,
            self.store.authorized_project_storage(),
            &request.name,
        )?;
        let definition = result.definition.ok_or_else(|| {
            WorkflowHostError::NotFound(result.detail.unwrap_or_else(|| request.name.clone()))
        })?;
        self.start_definition(WorkflowStartDefinition {
            definition,
            scope: request.scope,
            inputs: request.inputs,
            parent_session_id: request.parent_session_id,
            tool_call_id: request.tool_call_id,
            launch_input_id: request.launch_input_id,
            cwd: request.cwd,
            model_selection: request.model_selection,
            budgets: request.budgets,
            subagent_model: None,
            predecessor_run_id: None,
            capacity_preflighted: false,
        })
    }

    /// 修改定义必然铸造新的 run；旧 run 的 Journal 仍可查询，定义和输入不会被覆盖。
    pub(crate) async fn start_definition_change(
        self: &Arc<Self>,
        request: WorkflowRunDefinitionChangeRequest,
    ) -> Result<WorkflowRunStartResult, WorkflowHostError> {
        let predecessor = self
            .journal
            .run_summary(&request.predecessor_run_id)?
            .ok_or_else(|| WorkflowHostError::NotFound(request.predecessor_run_id.clone()))?;
        // amendSettings 已在 Session command 中读取并校验一次状态；这里仍在同一
        // Journal 事实源上复核，防止校验与铸造 successor 之间 predecessor 已完成后被重启。
        if !predecessor_is_amendable(&predecessor) {
            return Err(WorkflowHostError::FrozenRun(
                "predecessor run is no longer amendable".to_owned(),
            ));
        }
        // 所有可在本地证明的定义/输入校验必须发生在取消 predecessor 之前；否则
        // 编译失败会先改变旧运行状态，再返回看似普通的 validation error。
        validate_definition(&request.definition)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        resolve_inputs(&request.definition.inputs, request.inputs.clone())?;

        // 先读取 predecessor 的冻结 scope，并对整个 successor 做无副作用预校验。
        // launchInputId 冲突、definition/input/model/budget hash 冲突必须在取消旧 run
        // 之前返回；否则一次确定性的拒绝会把仍可用的 predecessor 留在 cancelled 态。
        let scope = self
            .journal
            .all_events(&request.predecessor_run_id)?
            .iter()
            .find(|event| event.event_type == "run-started")
            .and_then(|event| event.payload.get("scope"))
            .and_then(|scope| serde_json::from_value::<WorkflowScope>(scope.clone()).ok())
            .ok_or_else(|| {
                WorkflowHostError::FrozenRun(
                    "predecessor run is missing a valid frozen scope".to_owned(),
                )
            })?;
        let predecessor_run_id = request.predecessor_run_id.clone();
        let successor = WorkflowStartDefinition {
            definition: request.definition.clone(),
            scope,
            inputs: request.inputs.clone(),
            parent_session_id: request.parent_session_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            launch_input_id: request.launch_input_id.clone(),
            cwd: request.cwd.clone(),
            model_selection: request.model_selection.clone(),
            budgets: request.budgets.clone(),
            subagent_model: request.subagent_model.clone(),
            predecessor_run_id: Some(predecessor_run_id.clone()),
            capacity_preflighted: true,
        };
        self.preflight_start_definition(&successor)?;

        // 预校验可能读取 Journal，期间 predecessor 也可能自然结束；以取消前的最新
        // 事实再检查一次，避免把已完成 run 错误地标成 superseded。
        let predecessor = self
            .journal
            .run_summary(&predecessor_run_id)?
            .ok_or_else(|| WorkflowHostError::NotFound(predecessor_run_id.clone()))?;
        if !predecessor_is_amendable(&predecessor) {
            return Err(WorkflowHostError::FrozenRun(
                "predecessor run is no longer amendable".to_owned(),
            ));
        }
        if matches!(
            predecessor.status,
            WorkflowRunStatus::Running | WorkflowRunStatus::Pending
        ) {
            self.driver
                .cancel(predecessor_run_id.clone())
                .await
                .map_err(WorkflowHostError::Runtime)?;
        }
        // Definition changes create a new frozen run, but keep the saved-run scope
        // from the predecessor so global/project history aggregation remains exact.
        self.start_definition(successor)
    }

    /// 取消入口只操作真实 Driver；终态事件仍由引擎的 Journal 结算。
    pub async fn cancel(&self, run_id: &str) -> Result<bool, WorkflowHostError> {
        let Some(summary) = self.journal.run_summary(run_id)? else {
            return Err(WorkflowHostError::NotFound(run_id.to_owned()));
        };
        if !matches!(
            summary.status,
            WorkflowRunStatus::Running | WorkflowRunStatus::Pending
        ) {
            return Ok(false);
        }
        self.driver.cancel(run_id.to_owned()).await?;
        Ok(true)
    }

    /// 恢复读取 Journal 中冻结的 definition/inputs/models/budgets，不能接受新参数覆盖。
    pub async fn resume(
        self: &Arc<Self>,
        run_id: &str,
    ) -> Result<WorkflowRunStartResult, WorkflowHostError> {
        let _resume_claim = claim_resume(run_id)?;
        let summary = self
            .journal
            .run_summary(run_id)?
            .ok_or_else(|| WorkflowHostError::NotFound(run_id.to_owned()))?;
        if !summary.resumable {
            return Err(WorkflowHostError::NotResumable(run_id.to_owned()));
        }
        let events = self.journal.all_events(run_id)?;
        let frozen = frozen_from_events(run_id, &events)?;
        let actual_hash = canonical_hash(&frozen.definition)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        if frozen.canonical_hash != actual_hash {
            return Err(WorkflowHostError::FrozenRun(
                "definition hash changed".to_owned(),
            ));
        }
        if frozen.input_hash
            != hash_inputs(&frozen.resolved_inputs)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?
        {
            return Err(WorkflowHostError::FrozenRun(
                "input hash changed".to_owned(),
            ));
        }
        let canonical_hash = frozen.canonical_hash.clone();
        let execution = WorkflowExecutionRequest {
            run_id: run_id.to_owned(),
            definition: frozen.definition.clone(),
            canonical_hash: canonical_hash.clone(),
            parent_session_id: frozen.parent_session_id.clone(),
            tool_call_id: Some(frozen.tool_call_id.clone()),
            launch_input_id: frozen.launch_input_id.clone(),
            inputs: frozen.resolved_inputs.clone(),
            cwd: frozen.cwd.clone(),
            model_selection: frozen.model_selection.clone(),
            budgets: frozen.budgets.clone(),
            predecessor_run_id: frozen.predecessor_run_id.clone(),
        };
        self.driver
            .preflight(&execution, WorkflowPreflightMode::Resume)
            .map_err(map_preflight_error)?;
        let resume_payload = json!({
            "status": "running",
            "resumedAt": unix_time_ms(),
            "canonicalHash": frozen.canonical_hash,
            "inputHash": frozen.input_hash,
        });
        self.append_event(
            run_id,
            frozen.tool_call_id,
            frozen.launch_input_id,
            "run-resumed",
            resume_payload,
            Vec::new(),
        )?;
        self.spawn_execution(execution, true);
        Ok(WorkflowRunStartResult {
            run_id: run_id.to_owned(),
            canonical_hash,
        })
    }

    pub async fn resolve_question(
        &self,
        qid: String,
        answer: String,
    ) -> Result<(), WorkflowHostError> {
        self.driver.resolve_question(qid, answer).await?;
        Ok(())
    }

    pub fn list_runs(
        &self,
        parent_session_id: Option<&str>,
        name: Option<&str>,
        scope: Option<WorkflowScope>,
        limit: usize,
    ) -> Result<WorkflowRunsResult, WorkflowHostError> {
        Ok(self
            .journal
            .list_runs(parent_session_id, name, scope, limit)?)
    }

    pub fn run_summary(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkflowRunSummary>, WorkflowHostError> {
        Ok(self.journal.run_summary(run_id)?)
    }

    /// 读取设置修订所需的唯一冻结快照；定义、解析后的输入、模型和预算都来自
    /// predecessor 的 `run-started`，调用方不能以页面参数替换这些事实。
    pub fn frozen_run(&self, run_id: &str) -> Result<Option<FrozenWorkflowRun>, WorkflowHostError> {
        let events = self.journal.all_events(run_id)?;
        if events.is_empty() {
            return Ok(None);
        }
        frozen_from_events(run_id, &events).map(Some)
    }

    pub fn list_events(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowEventPage, WorkflowHostError> {
        Ok(self.journal.list_events(run_id, after_sequence, limit)?)
    }

    pub fn progress_events(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<DynamicWorkflowRunProgressPayload>, WorkflowHostError> {
        Ok(self
            .journal
            .list_events(run_id, after_sequence, limit)?
            .events
            .iter()
            .map(progress_from_workflow_event)
            .collect())
    }

    pub fn graph(&self, run_id: &str) -> Result<WorkflowGraphResult, WorkflowHostError> {
        let events = self.journal.all_events(run_id)?;
        let definition = frozen_from_events(run_id, &events)?.definition;
        validate_definition(&definition)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        let (nodes, edges) = project_graph(&definition.body);
        Ok(WorkflowGraphResult {
            run_id: run_id.to_owned(),
            definition,
            nodes,
            edges,
        })
    }

    pub fn workspace(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkflowWorkspaceResult>, WorkflowHostError> {
        Ok(self.journal.list_workspace_nodes(run_id)?)
    }

    pub fn node_result(
        &self,
        run_id: &str,
        site_id: &str,
        ordinal: u32,
        max_bytes: usize,
    ) -> Result<Option<WorkflowNodeResult>, WorkflowHostError> {
        Ok(self
            .journal
            .read_node_result(run_id, site_id, ordinal, max_bytes)?)
    }

    pub fn artifacts(
        &self,
        run_id: &str,
    ) -> Result<Option<Vec<WorkflowArtifact>>, WorkflowHostError> {
        Ok(self.journal.list_artifacts(run_id)?)
    }

    pub fn artifact_items(
        &self,
        run_id: &str,
        artifact_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowArtifactPage, WorkflowHostError> {
        Ok(self
            .journal
            .list_artifact_items(run_id, artifact_id, after_sequence, limit)?)
    }

    pub fn read_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
        version: u32,
        offset: usize,
        limit: usize,
    ) -> Result<Option<WorkflowArtifactBytes>, WorkflowHostError> {
        Ok(self
            .journal
            .read_artifact(run_id, artifact_id, version, offset, limit)?)
    }

    fn start_definition(
        self: &Arc<Self>,
        request: WorkflowStartDefinition,
    ) -> Result<WorkflowRunStartResult, WorkflowHostError> {
        let WorkflowStartDefinition {
            definition,
            scope,
            inputs,
            parent_session_id,
            tool_call_id,
            launch_input_id,
            cwd,
            model_selection,
            budgets,
            subagent_model,
            predecessor_run_id,
            capacity_preflighted,
        } = request;
        let _launch_guard = WORKFLOW_LAUNCH_GATE.lock().map_err(|_| {
            WorkflowHostError::Runtime(WorkflowRuntimeError(
                "workflow launch coordination is unavailable".to_owned(),
            ))
        })?;
        let compiled = validate_definition(&definition)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        let resolved_inputs = resolve_inputs(&definition.inputs, inputs)?;
        let input_hash = hash_inputs(&resolved_inputs)
            .map_err(|error| WorkflowHostError::InvalidInputs(error.to_string()))?;
        let canonical = compiled.definition_hash().to_owned();
        let launch_input_id =
            launch_input_id.and_then(|value| (!value.trim().is_empty()).then_some(value));
        let model_hash = workflow_value_digest(model_selection.as_ref());
        let budgets_hash = workflow_value_digest(budgets.as_ref());

        // `list_runs` intentionally serves a bounded UI history. Durable idempotency must
        // query the complete Journal launch index so a completed run older than that window
        // is still reused after cold restart.
        if let Some(launch_input_id) = launch_input_id.as_deref() {
            let launches = self
                .journal
                .find_launches(&parent_session_id, launch_input_id)?;
            let mut reusable = None;
            for launch in launches {
                let events = self.journal.all_events(&launch.run_id)?;
                let frozen = frozen_from_events(&launch.run_id, &events)?;
                let comparison = LaunchComparison {
                    parent_session_id: &parent_session_id,
                    scope: &scope,
                    cwd: &cwd,
                    canonical_hash: &canonical,
                    input_hash: &input_hash,
                    model_hash: &model_hash,
                    budgets_hash: &budgets_hash,
                    subagent_model: subagent_model.as_deref(),
                    predecessor_run_id: predecessor_run_id.as_deref(),
                };
                if let Some(reason) = launch_conflict_reason(&frozen, &launch.event, comparison) {
                    return Err(WorkflowHostError::LaunchConflict(format!(
                        "launchInputId {launch_input_id} is already frozen by run {}: {reason}",
                        launch.run_id
                    )));
                }
                reusable.get_or_insert((launch.run_id, frozen.canonical_hash));
            }
            if let Some((run_id, canonical_hash)) = reusable {
                // Returning the frozen run is the idempotent result. In particular, do not
                // call prepare/start again: those are the points that create actor/artifact
                // side effects for an in-flight or already completed run.
                return Ok(WorkflowRunStartResult {
                    run_id,
                    canonical_hash,
                });
            }
        }
        let run_id = self.new_run_id();
        let tool_call_id = tool_call_id.unwrap_or_else(|| format!("workflow:{run_id}"));
        let mut payload = json!({
            "status": "running",
            "name": definition.meta.name.clone(),
            "scope": scope,
            "definition": definition.clone(),
            "canonicalHash": canonical.clone(),
            "inputHash": input_hash.clone(),
            "inputs": resolved_inputs.clone(),
            "parentSessionId": parent_session_id.clone(),
            "cwd": cwd.to_string_lossy(),
            "models": model_selection.clone(),
            "modelHash": model_hash,
            "budgets": budgets.clone(),
            "budgetsHash": budgets_hash,
            "predecessorRunId": predecessor_run_id.clone(),
            "createdAt": unix_time_ms(),
            "updatedAt": unix_time_ms(),
        });
        if let Some(subagent_model) = subagent_model.as_deref() {
            payload["subagentModel"] = Value::String(subagent_model.to_owned());
        }
        if let Some(max_concurrency) = budgets.as_ref().and_then(budget_max_concurrency) {
            // Source 的并发投影只读这两个 camelCase 字段；实际执行仍从同一 budgets
            // 解析 max_concurrency，避免出现 UI 显示与 Executor 不一致的第二事实。
            payload["caps"] = json!({"maxConcurrency": max_concurrency});
            payload["concurrencyCeiling"] = json!(WorkflowLimits::default().max_concurrency);
        }
        let execution = WorkflowExecutionRequest {
            run_id: run_id.clone(),
            definition,
            canonical_hash: canonical.clone(),
            parent_session_id,
            tool_call_id: Some(tool_call_id.clone()),
            launch_input_id: launch_input_id.clone(),
            inputs: resolved_inputs,
            cwd,
            model_selection,
            budgets,
            predecessor_run_id,
        };
        if !capacity_preflighted {
            self.driver
                .preflight(&execution, WorkflowPreflightMode::Start)
                .map_err(map_preflight_error)?;
        }
        self.append_event(
            &run_id,
            tool_call_id.clone(),
            launch_input_id.clone(),
            "run-started",
            payload,
            Vec::new(),
        )?;
        self.spawn_execution(execution, false);
        Ok(WorkflowRunStartResult {
            run_id,
            canonical_hash: canonical,
        })
    }

    /// 只做 successor 的确定性校验和持久幂等冲突检查，不写 Journal、不取消旧 run。
    ///
    /// 这段预校验与 `start_definition` 的最终提交仍会在取消后再次执行；后者负责
    /// 应对正常并发提交，前者负责保证一个已知的冲突不会先破坏 predecessor。
    fn preflight_start_definition(
        &self,
        request: &WorkflowStartDefinition,
    ) -> Result<(), WorkflowHostError> {
        let compiled = validate_definition(&request.definition)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        let resolved_inputs = resolve_inputs(&request.definition.inputs, request.inputs.clone())?;
        let input_hash = hash_inputs(&resolved_inputs)
            .map_err(|error| WorkflowHostError::InvalidInputs(error.to_string()))?;
        let canonical = compiled.definition_hash().to_owned();
        let launch_input_id = request
            .launch_input_id
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        if let Some(launch_input_id) = launch_input_id {
            let model_hash = workflow_value_digest(request.model_selection.as_ref());
            let budgets_hash = workflow_value_digest(request.budgets.as_ref());
            let comparison = LaunchComparison {
                parent_session_id: &request.parent_session_id,
                scope: &request.scope,
                cwd: &request.cwd,
                canonical_hash: &canonical,
                input_hash: &input_hash,
                model_hash: &model_hash,
                budgets_hash: &budgets_hash,
                subagent_model: request.subagent_model.as_deref(),
                predecessor_run_id: request.predecessor_run_id.as_deref(),
            };
            for launch in self
                .journal
                .find_launches(&request.parent_session_id, launch_input_id)?
            {
                let events = self.journal.all_events(&launch.run_id)?;
                let frozen = frozen_from_events(&launch.run_id, &events)?;
                if let Some(reason) = launch_conflict_reason(&frozen, &launch.event, comparison) {
                    return Err(WorkflowHostError::LaunchConflict(format!(
                        "launchInputId {launch_input_id} is already frozen by run {}: {reason}",
                        launch.run_id
                    )));
                }
            }
        }
        // 这里必须在 amend 取消 predecessor 之前读取父 Store 容量；生产 Driver 的
        // 预检只读 Journal/ArtifactStore，不写启动事实，也不执行任何叶节点。
        let preflight_request = WorkflowExecutionRequest {
            run_id: "workflow:capacity-preflight".to_owned(),
            definition: request.definition.clone(),
            canonical_hash: canonical,
            parent_session_id: request.parent_session_id.clone(),
            tool_call_id: Some(
                request
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "workflow:capacity-preflight".to_owned()),
            ),
            launch_input_id: launch_input_id.map(str::to_owned),
            inputs: resolved_inputs,
            cwd: request.cwd.clone(),
            model_selection: request.model_selection.clone(),
            budgets: request.budgets.clone(),
            predecessor_run_id: request.predecessor_run_id.clone(),
        };
        self.driver
            .preflight(&preflight_request, WorkflowPreflightMode::Start)
            .map_err(map_preflight_error)?;
        Ok(())
    }

    fn load_definition(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
        name: &str,
    ) -> Result<WorkflowGetResult, WorkflowHostError> {
        Ok(self.store.get(scope, project_storage, name)?)
    }

    fn append_event(
        &self,
        run_id: &str,
        tool_call_id: String,
        launch_input_id: Option<String>,
        event_type: &str,
        payload: Value,
        artifacts: Vec<ArtifactUse>,
    ) -> Result<WorkflowJournalEvent, WorkflowHostError> {
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
        if bytes.len() > MAX_WORKFLOW_EVENT_PAYLOAD_BYTES {
            return Err(WorkflowHostError::PayloadTooLarge(bytes.len()));
        }
        let event = WorkflowJournalEvent {
            run_id: run_id.to_owned(),
            tool_call_id,
            sequence: 0,
            event_type: event_type.to_owned(),
            payload,
            artifacts,
            actor_session_id: None,
            launch_input_id,
        };
        let committed = self
            .journal
            .commit_event(&workflow_event_operation_id(&event), event.clone())?
            .event;
        Ok(committed)
    }

    fn spawn_execution(self: &Arc<Self>, request: WorkflowExecutionRequest, resume: bool) {
        // 取消可以在 tokio 任务第一次 poll 前到达；先同步预留 Driver 的运行令牌，
        // 再派生任务，保证 Journal 中的 running run 从写入时就可被取消。
        if let Err(error) = self.driver.prepare(&request.run_id) {
            let _ = self.settle_driver_error(&request.run_id, error);
            return;
        }
        let host = Arc::clone(self);
        // RuntimeSession 的 WorkflowEventCommitted 已经是动态事件唯一来源；
        // 引擎通知只需消费掉适配器的提交回执，不再维护第二套 Host 监听器。
        let sink: Arc<dyn WorkflowProgressSink> = Arc::new(JournalProgressSink);
        tokio::spawn(async move {
            let run_id = request.run_id.clone();
            let result = if resume {
                host.driver.resume(request, sink).await
            } else {
                host.driver.start(request, sink).await
            };
            if let Err(error) = result {
                let _ = host.settle_driver_error(&run_id, error);
            }
        });
    }

    fn settle_driver_error(
        &self,
        run_id: &str,
        error: WorkflowRuntimeError,
    ) -> Result<(), WorkflowHostError> {
        let page = self
            .journal
            .list_events(run_id, None, MAX_WORKFLOW_EVENT_LIMIT)?;
        if page
            .events
            .iter()
            .rev()
            .any(|event| event.event_type == "run-settled")
        {
            return Ok(());
        }
        self.append_event(
            run_id,
            page.events
                .first()
                .map(|event| event.tool_call_id.clone())
                .unwrap_or_else(|| format!("workflow:{run_id}")),
            page.events
                .first()
                .and_then(|event| event.launch_input_id.clone()),
            "run-settled",
            json!({
                "status": "errored",
                "error": { "code": "driver_error", "message": error.to_string() },
                "updatedAt": unix_time_ms(),
            }),
            Vec::new(),
        )?;
        Ok(())
    }

    fn new_run_id(&self) -> String {
        let now = unix_time_ms();
        let ordinal = NEXT_WORKFLOW_RUN.fetch_add(1, Ordering::Relaxed);
        format!("workflow-{now:x}-{ordinal:x}")
    }
}

struct ResumeClaim {
    run_id: String,
}

impl Drop for ResumeClaim {
    fn drop(&mut self) {
        if let Ok(mut claims) = WORKFLOW_RESUME_CLAIMS.lock() {
            claims.remove(&self.run_id);
        }
    }
}

fn claim_resume(run_id: &str) -> Result<ResumeClaim, WorkflowHostError> {
    let mut claims = WORKFLOW_RESUME_CLAIMS.lock().map_err(|_| {
        WorkflowHostError::Runtime(WorkflowRuntimeError(
            "workflow resume coordination is unavailable".to_owned(),
        ))
    })?;
    if !claims.insert(run_id.to_owned()) {
        return Err(WorkflowHostError::Runtime(WorkflowRuntimeError(
            "workflow resume is already being admitted".to_owned(),
        )));
    }
    Ok(ResumeClaim {
        run_id: run_id.to_owned(),
    })
}

struct JournalProgressSink;

impl WorkflowProgressSink for JournalProgressSink {
    fn publish(
        &self,
        _payload: DynamicWorkflowRunProgressPayload,
        _artifacts: Vec<ArtifactUse>,
    ) -> Result<(), WorkflowJournalError> {
        Ok(())
    }
}

/// 把引擎的控制树压平成 v4 的可见站点图。Agent 是 ask，读工具是 world-read；写工具和
/// Artifact 只在 workspace/artifact 查询面出现，控制节点只贡献阶段间的内部边。
fn project_graph(body: &[Node]) -> (Vec<WorkflowGraphNode>, Vec<WorkflowGraphEdge>) {
    let mut nodes = BTreeMap::new();
    let mut edges = BTreeSet::new();
    let _ = project_graph_body(body, 0, &mut nodes, &mut edges);
    (
        nodes.into_values().collect(),
        edges
            .into_iter()
            .map(|(from, to, back)| WorkflowGraphEdge {
                from,
                to,
                back: back.then_some(true),
            })
            .collect(),
    )
}

/// 将已校验的定义图提供给会话行投影。
///
/// `CreateWorkflow` 的 Source 详情页从工具行的 `display.causalityGraph` 取图，
/// 而不是调用独立的 graph RPC。这里复用同一份 Rust 图投影，避免会话行再维护
/// 一套节点/边归约规则；调用方仍负责将结果限制到 Source display 的字段边界。
pub(crate) fn project_graph_for_display(
    definition: &WorkflowDefinition,
) -> (Vec<WorkflowGraphNode>, Vec<WorkflowGraphEdge>) {
    project_graph(&definition.body)
}

fn project_graph_body(
    body: &[Node],
    depth: u16,
    nodes: &mut BTreeMap<String, WorkflowGraphNode>,
    edges: &mut std::collections::BTreeSet<(String, String, bool)>,
) -> Vec<String> {
    let mut visible: Vec<String> = Vec::new();
    for node in body {
        let current = match node {
            Node::Sequence {
                nodes: children, ..
            } => project_graph_body(children, depth.saturating_add(1), nodes, edges),
            Node::Parallel {
                nodes: children, ..
            } => {
                let mut branches = Vec::new();
                for child in children {
                    branches.extend(project_graph_body(
                        std::slice::from_ref(child),
                        depth.saturating_add(1),
                        nodes,
                        edges,
                    ));
                }
                branches
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => {
                let mut branches =
                    project_graph_body(then_body, depth.saturating_add(1), nodes, edges);
                branches.extend(project_graph_body(
                    else_body,
                    depth.saturating_add(1),
                    nodes,
                    edges,
                ));
                branches
            }
            Node::Foreach { body: children, .. } | Node::Repeat { body: children, .. } => {
                let branch = project_graph_body(children, depth.saturating_add(1), nodes, edges);
                if let (Some(first), Some(last)) = (branch.first(), branch.last())
                    && first != last
                {
                    edges.insert((last.clone(), first.clone(), true));
                }
                branch
            }
            Node::Agent(agent) => {
                let id = agent.node_id.clone();
                nodes.insert(
                    id.clone(),
                    WorkflowGraphNode {
                        id: id.clone(),
                        kind: WorkflowGraphNodeKind::Ask,
                        label: agent.name.chars().take(128).collect(),
                        lane: format!("agent:{}", agent.name.chars().take(96).collect::<String>()),
                        depth,
                    },
                );
                vec![id]
            }
            Node::Tool(tool) if tool.effect == EffectClass::ReadOnly => {
                let id = tool.node_id.clone();
                nodes.insert(
                    id.clone(),
                    WorkflowGraphNode {
                        id: id.clone(),
                        kind: WorkflowGraphNodeKind::WorldRead,
                        label: tool.name.chars().take(128).collect(),
                        lane: "workspace".to_owned(),
                        depth,
                    },
                );
                vec![id]
            }
            Node::Tool(_) | Node::Artifact(_) => Vec::new(),
        };
        if let (Some(previous), Some(first)) = (visible.last(), current.first())
            && previous != first
        {
            edges.insert((previous.clone(), first.clone(), false));
        }
        visible.extend(current);
    }
    visible
}

fn frozen_from_events(
    run_id: &str,
    events: &[WorkflowJournalEvent],
) -> Result<FrozenWorkflowRun, WorkflowHostError> {
    let event = events
        .iter()
        .find(|event| event.event_type == "run-started")
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun(format!("run {run_id} has no run-started event"))
        })?;
    let payload = &event.payload;
    let definition = serde_json::from_value::<WorkflowDefinition>(required(payload, "definition")?)
        .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?;
    let resolved_inputs = required(payload, "inputs")?;
    let canonical_hash = required_str(payload, "canonicalHash")?.to_owned();
    let input_hash = required_str(payload, "inputHash")?.to_owned();
    let parent_session_id = required_str(payload, "parentSessionId")?.to_owned();
    let cwd = PathBuf::from(required_str(payload, "cwd")?);
    Ok(FrozenWorkflowRun {
        definition,
        canonical_hash,
        input_hash,
        resolved_inputs,
        parent_session_id,
        tool_call_id: event.tool_call_id.clone(),
        launch_input_id: event.launch_input_id.clone(),
        cwd,
        model_selection: payload
            .get("models")
            .cloned()
            .filter(|value| !value.is_null()),
        budgets: payload
            .get("budgets")
            .cloned()
            .filter(|value| !value.is_null()),
        subagent_model: payload
            .get("subagentModel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        predecessor_run_id: payload
            .get("predecessorRunId")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// 同一 `launchInputId` 的重放比较必须包含所有被冻结的运行输入；集中为一个值，
/// 避免新增冻结字段时在调用点遗漏比较项，也让 Host 保持清晰的参数边界。
/// 比较值仅持有只读借用，允许在多个既有 launch 的预检中重复使用。
#[derive(Clone, Copy)]
struct LaunchComparison<'a> {
    parent_session_id: &'a str,
    scope: &'a WorkflowScope,
    cwd: &'a Path,
    canonical_hash: &'a str,
    input_hash: &'a str,
    model_hash: &'a str,
    budgets_hash: &'a str,
    subagent_model: Option<&'a str>,
    predecessor_run_id: Option<&'a str>,
}

fn launch_conflict_reason(
    frozen: &FrozenWorkflowRun,
    started: &WorkflowJournalEvent,
    comparison: LaunchComparison<'_>,
) -> Option<String> {
    if frozen.parent_session_id != comparison.parent_session_id {
        return Some("parent session differs".to_owned());
    }
    if frozen.canonical_hash != comparison.canonical_hash {
        return Some("definition digest differs".to_owned());
    }
    if frozen.input_hash != comparison.input_hash {
        return Some("resolved input digest differs".to_owned());
    }
    let existing_model_hash = started
        .payload
        .get("modelHash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            workflow_value_digest(
                started
                    .payload
                    .get("models")
                    .filter(|value| !value.is_null()),
            )
        });
    if existing_model_hash != comparison.model_hash {
        return Some("model selection digest differs".to_owned());
    }
    let existing_budgets_hash = started
        .payload
        .get("budgetsHash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            workflow_value_digest(
                started
                    .payload
                    .get("budgets")
                    .filter(|value| !value.is_null()),
            )
        });
    if existing_budgets_hash != comparison.budgets_hash {
        return Some("budget digest differs".to_owned());
    }
    let existing_subagent_model = started.payload.get("subagentModel").and_then(Value::as_str);
    if existing_subagent_model != comparison.subagent_model {
        return Some("subagent model differs".to_owned());
    }
    let Some(existing_scope) = started
        .payload
        .get("scope")
        .and_then(|value| serde_json::from_value::<WorkflowScope>(value.clone()).ok())
    else {
        return Some("frozen scope is missing or invalid".to_owned());
    };
    if &existing_scope != comparison.scope {
        return Some("workflow scope differs".to_owned());
    }
    let cwd_string = comparison.cwd.to_string_lossy();
    if started.payload.get("cwd").and_then(Value::as_str) != Some(cwd_string.as_ref()) {
        return Some("working directory differs".to_owned());
    }
    let existing_predecessor = started
        .payload
        .get("predecessorRunId")
        .and_then(Value::as_str);
    if existing_predecessor != comparison.predecessor_run_id {
        return Some("predecessor run differs".to_owned());
    }
    None
}

fn budget_max_concurrency(value: &Value) -> Option<u16> {
    value
        .get("max_concurrency")
        .or_else(|| value.get("maxConcurrency"))
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

/// 用带类型边界的递归编码计算模型/预算摘要，避免 JSON object 的输入键顺序影响幂等判断。
fn workflow_value_digest(value: Option<&Value>) -> String {
    fn update(digest: &mut Sha256, value: &Value) {
        match value {
            Value::Null => digest.update([0]),
            Value::Bool(value) => digest.update([1, u8::from(*value)]),
            Value::Number(value) => {
                digest.update([2]);
                digest.update(value.to_string().as_bytes());
                digest.update([0]);
            }
            Value::String(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            Value::Array(values) => {
                digest.update([4]);
                digest.update((values.len() as u64).to_be_bytes());
                for value in values {
                    update(digest, value);
                }
            }
            Value::Object(values) => {
                digest.update([5]);
                let mut entries = values.iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                digest.update((entries.len() as u64).to_be_bytes());
                for (key, value) in entries {
                    digest.update((key.len() as u64).to_be_bytes());
                    digest.update(key.as_bytes());
                    update(digest, value);
                }
            }
        }
    }

    let mut digest = Sha256::new();
    digest.update(b"keencode.workflow.frozen-value.v1\0");
    update(&mut digest, value.unwrap_or(&Value::Null));
    format!("{:x}", digest.finalize())
}

fn required(payload: &Value, name: &str) -> Result<Value, WorkflowHostError> {
    payload
        .get(name)
        .cloned()
        .ok_or_else(|| WorkflowHostError::FrozenRun(format!("missing {name}")))
}

fn required_str<'a>(payload: &'a Value, name: &str) -> Result<&'a str, WorkflowHostError> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| WorkflowHostError::FrozenRun(format!("missing string {name}")))
}

fn resolve_inputs(
    specs: &BTreeMap<String, InputSpec>,
    inputs: Value,
) -> Result<Value, WorkflowHostError> {
    let Value::Object(mut provided) = inputs else {
        return Err(WorkflowHostError::InvalidInputs(
            "top-level inputs must be an object".to_owned(),
        ));
    };
    for key in provided.keys() {
        if !specs.contains_key(key) {
            return Err(WorkflowHostError::InvalidInputs(format!(
                "unknown input {key}"
            )));
        }
    }
    let mut resolved = Map::new();
    for (name, spec) in specs {
        let value = provided.remove(name).or_else(|| spec.default.clone());
        let Some(value) = value else {
            if spec.required {
                return Err(WorkflowHostError::InvalidInputs(format!(
                    "missing input {name}"
                )));
            }
            continue;
        };
        if !input_type_accepts(&spec.value_type, &value) {
            return Err(WorkflowHostError::InvalidInputs(format!(
                "input {name} does not match {:?}",
                spec.value_type
            )));
        }
        resolved.insert(name.clone(), value);
    }
    Ok(Value::Object(resolved))
}

fn input_type_accepts(input_type: &InputType, value: &Value) -> bool {
    match input_type {
        InputType::Any | InputType::Json => true,
        InputType::String => value.is_string(),
        InputType::Number => value.is_number(),
        InputType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        InputType::Boolean => value.is_boolean(),
        InputType::Object => value.is_object(),
        InputType::Array => value.is_array(),
        InputType::Null => value.is_null(),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn amendable_predecessor_status_is_rechecked_at_successor_boundary() {
        let mut summary = WorkflowRunSummary {
            run_id: "run-1".to_owned(),
            name: None,
            status: WorkflowRunStatus::Running,
            stop_reason: None,
            created_at: 1,
            updated_at: 1,
            spent_tokens: 0,
            parent_session_id: None,
            tool_call_id: None,
            args: Value::Null,
            cwd: None,
            canonical_hash: None,
            artifacts: Vec::new(),
            resumable: false,
        };
        assert!(predecessor_is_amendable(&summary));
        summary.status = WorkflowRunStatus::Completed;
        assert!(!predecessor_is_amendable(&summary));
        summary.status = WorkflowRunStatus::Stopped;
        summary.stop_reason = Some(WorkflowRunStopReason::Superseded);
        assert!(predecessor_is_amendable(&summary));
        summary.stop_reason = Some(WorkflowRunStopReason::User);
        assert!(!predecessor_is_amendable(&summary));
    }

    struct MemoryWorkflowJournal {
        events: Mutex<BTreeMap<String, Vec<WorkflowJournalEvent>>>,
        artifact_capacity: Mutex<keencode_resources::ArtifactCapacity>,
    }

    impl Default for MemoryWorkflowJournal {
        fn default() -> Self {
            Self {
                events: Mutex::new(BTreeMap::new()),
                artifact_capacity: Mutex::new(keencode_resources::ArtifactCapacity {
                    committed_unique_artifacts: 0,
                    maximum_unique_artifacts: 1024,
                }),
            }
        }
    }

    impl MemoryWorkflowJournal {
        fn set_artifact_capacity(&self, committed: usize, maximum: usize) {
            *self.artifact_capacity.lock().expect("test capacity lock") =
                keencode_resources::ArtifactCapacity {
                    committed_unique_artifacts: committed,
                    maximum_unique_artifacts: maximum,
                };
        }
    }

    impl WorkflowJournalPort for MemoryWorkflowJournal {
        fn commit_event(
            &self,
            _operation_id: &str,
            mut event: WorkflowJournalEvent,
        ) -> Result<CommittedWorkflowEvent, WorkflowJournalError> {
            let mut runs = self
                .events
                .lock()
                .map_err(|_| WorkflowJournalError("test Journal lock poisoned".to_owned()))?;
            let run = runs.entry(event.run_id.clone()).or_default();
            event.sequence = run.last().map_or(1, |last| last.sequence + 1);
            run.push(event.clone());
            Ok(CommittedWorkflowEvent { event })
        }

        fn list_events(
            &self,
            run_id: &str,
            after_sequence: Option<u64>,
            limit: usize,
        ) -> Result<WorkflowEventPage, WorkflowJournalError> {
            let runs = self
                .events
                .lock()
                .map_err(|_| WorkflowJournalError("test Journal lock poisoned".to_owned()))?;
            let mut events = runs.get(run_id).cloned().unwrap_or_default();
            events.retain(|event| after_sequence.is_none_or(|after| event.sequence > after));
            let bounded = limit.max(1);
            let has_more = events.len() > bounded;
            events.truncate(bounded);
            Ok(WorkflowEventPage { events, has_more })
        }

        fn all_events(
            &self,
            run_id: &str,
        ) -> Result<Vec<WorkflowJournalEvent>, WorkflowJournalError> {
            Ok(self
                .events
                .lock()
                .map_err(|_| WorkflowJournalError("test Journal lock poisoned".to_owned()))?
                .get(run_id)
                .cloned()
                .unwrap_or_default())
        }

        fn list_runs(
            &self,
            _parent_session_id: Option<&str>,
            _name: Option<&str>,
            _scope: Option<WorkflowScope>,
            _limit: usize,
        ) -> Result<WorkflowRunsResult, WorkflowJournalError> {
            Ok(WorkflowRunsResult {
                runs: Vec::new(),
                truncated: false,
            })
        }

        fn find_launches(
            &self,
            parent_session_id: &str,
            launch_input_id: &str,
        ) -> Result<Vec<WorkflowLaunchRecord>, WorkflowJournalError> {
            let runs = self
                .events
                .lock()
                .map_err(|_| WorkflowJournalError("test Journal lock poisoned".to_owned()))?;
            Ok(runs
                .iter()
                .filter_map(|(run_id, events)| {
                    events
                        .iter()
                        .find(|event| {
                            event.event_type == "run-started"
                                && event.launch_input_id.as_deref() == Some(launch_input_id)
                                && event.payload.get("parentSessionId").and_then(Value::as_str)
                                    == Some(parent_session_id)
                        })
                        .cloned()
                        .map(|event| WorkflowLaunchRecord {
                            run_id: run_id.clone(),
                            event,
                        })
                })
                .collect())
        }

        fn run_summary(
            &self,
            run_id: &str,
        ) -> Result<Option<WorkflowRunSummary>, WorkflowJournalError> {
            let runs = self
                .events
                .lock()
                .map_err(|_| WorkflowJournalError("test Journal lock poisoned".to_owned()))?;
            let Some(events) = runs.get(run_id) else {
                return Ok(None);
            };
            let Some(started) = events
                .iter()
                .find(|event| event.event_type == "run-started")
            else {
                return Ok(None);
            };
            let terminal = events
                .iter()
                .rev()
                .find(|event| event.event_type == "run-settled");
            let status = match terminal
                .and_then(|event| event.payload.get("status"))
                .and_then(Value::as_str)
            {
                Some("completed" | "succeeded") => WorkflowRunStatus::Completed,
                Some("stopped" | "cancelled") => WorkflowRunStatus::Stopped,
                Some("errored" | "failed" | "indeterminate") => WorkflowRunStatus::Errored,
                _ => WorkflowRunStatus::Running,
            };
            Ok(Some(WorkflowRunSummary {
                run_id: run_id.to_owned(),
                name: started
                    .payload
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                status,
                stop_reason: None,
                created_at: 0,
                updated_at: 0,
                spent_tokens: 0,
                parent_session_id: started
                    .payload
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_call_id: Some(started.tool_call_id.clone()),
                args: started
                    .payload
                    .get("inputs")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                cwd: started
                    .payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(PathBuf::from),
                canonical_hash: started
                    .payload
                    .get("canonicalHash")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                artifacts: Vec::new(),
                resumable: false,
            }))
        }

        fn list_workspace_nodes(
            &self,
            _run_id: &str,
        ) -> Result<Option<WorkflowWorkspaceResult>, WorkflowJournalError> {
            Ok(None)
        }

        fn read_node_result(
            &self,
            _run_id: &str,
            _site_id: &str,
            _ordinal: u32,
            _max_bytes: usize,
        ) -> Result<Option<WorkflowNodeResult>, WorkflowJournalError> {
            Ok(None)
        }

        fn list_artifacts(
            &self,
            _run_id: &str,
        ) -> Result<Option<Vec<WorkflowArtifact>>, WorkflowJournalError> {
            Ok(None)
        }

        fn list_artifact_items(
            &self,
            _run_id: &str,
            _artifact_id: &str,
            _after_sequence: Option<u64>,
            _limit: usize,
        ) -> Result<WorkflowArtifactPage, WorkflowJournalError> {
            Ok(WorkflowArtifactPage {
                items: Vec::new(),
                has_more: false,
            })
        }

        fn read_artifact(
            &self,
            _run_id: &str,
            _artifact_id: &str,
            _version: u32,
            _offset: usize,
            _limit: usize,
        ) -> Result<Option<WorkflowArtifactBytes>, WorkflowJournalError> {
            Ok(None)
        }

        fn artifact_capacity(
            &self,
        ) -> Result<keencode_resources::ArtifactCapacity, WorkflowJournalError> {
            self.artifact_capacity
                .lock()
                .map(|capacity| *capacity)
                .map_err(|_| WorkflowJournalError("test capacity lock poisoned".to_owned()))
        }
    }

    struct CountingDriver {
        prepares: AtomicUsize,
        starts: AtomicUsize,
        cancels: AtomicUsize,
        reject_prepare: bool,
        reject_preflight: bool,
    }

    impl CountingDriver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                prepares: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                reject_prepare: false,
                reject_preflight: false,
            })
        }

        fn rejecting_prepare() -> Arc<Self> {
            Arc::new(Self {
                prepares: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                reject_prepare: true,
                reject_preflight: false,
            })
        }

        fn rejecting_preflight() -> Arc<Self> {
            Arc::new(Self {
                prepares: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                reject_prepare: false,
                reject_preflight: true,
            })
        }
    }

    impl WorkflowExecutionDriver for CountingDriver {
        fn preflight(
            &self,
            _request: &WorkflowExecutionRequest,
            _mode: WorkflowPreflightMode,
        ) -> Result<(), WorkflowRuntimeError> {
            if self.reject_preflight {
                return Err(WorkflowRuntimeError(
                    "artifact_capacity: available=0 required=1 committed=1 maximum=1".to_owned(),
                ));
            }
            Ok(())
        }

        fn prepare(&self, _run_id: &str) -> Result<(), WorkflowRuntimeError> {
            self.prepares.fetch_add(1, Ordering::Relaxed);
            if self.reject_prepare {
                return Err(WorkflowRuntimeError("test prepare rejection".to_owned()));
            }
            Ok(())
        }

        fn start(
            &self,
            _request: WorkflowExecutionRequest,
            _sink: Arc<dyn WorkflowProgressSink>,
        ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(()) })
        }

        fn cancel(&self, _run_id: String) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
            self.cancels.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(()) })
        }

        fn resume(
            &self,
            _request: WorkflowExecutionRequest,
            _sink: Arc<dyn WorkflowProgressSink>,
        ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
            Box::pin(async { Ok(()) })
        }

        fn resolve_question(
            &self,
            _qid: String,
            _answer: String,
        ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn test_definition(name: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            version: WORKFLOW_VERSION,
            meta: WorkflowMeta {
                id: None,
                name: name.to_owned(),
                description: None,
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: BTreeMap::new(),
            body: vec![Node::Agent(AgentNode {
                node_id: "inspect".to_owned(),
                name: "inspect".to_owned(),
                input: ValueExpr::literal(json!("inspect")),
                config: Value::Null,
                output_type: None,
                effect: EffectClass::ReadOnly,
            })],
        }
    }

    fn tool_definition(name: &str) -> WorkflowDefinition {
        let mut definition = test_definition(name);
        definition.body = vec![Node::Tool(keencode_workflow::ToolNode {
            node_id: "read".to_owned(),
            name: "Read".to_owned(),
            input: keencode_workflow::ValueExpr::literal(json!({
                "path": "README.md"
            })),
            config: Value::Null,
            output_type: Some("json".to_owned()),
            effect: EffectClass::ReadOnly,
        })];
        definition
    }

    fn start_request(model_selection: Value) -> WorkflowRunStartRequest {
        WorkflowRunStartRequest {
            name: "inspect".to_owned(),
            scope: WorkflowScope::Project,
            project_storage: None,
            parent_session_id: "parent-1".to_owned(),
            tool_call_id: Some("tool-1".to_owned()),
            launch_input_id: Some("command-1".to_owned()),
            inputs: json!({}),
            cwd: PathBuf::from("/workspace"),
            model_selection: Some(model_selection),
            budgets: Some(json!({"maxNodes": 4})),
        }
    }

    fn test_host(
        journal: Arc<MemoryWorkflowJournal>,
        driver: Arc<CountingDriver>,
    ) -> (Arc<WorkflowHost>, tempfile::TempDir, tempfile::TempDir) {
        test_host_with_definition(journal, driver, test_definition("inspect"))
    }

    fn test_host_with_definition(
        journal: Arc<MemoryWorkflowJournal>,
        driver: Arc<CountingDriver>,
        definition: WorkflowDefinition,
    ) -> (Arc<WorkflowHost>, tempfile::TempDir, tempfile::TempDir) {
        let data_root = tempfile::tempdir().expect("test data root");
        let project = tempfile::tempdir().expect("test project root");
        let store = WorkflowStore::with_project_storage(data_root.path(), project.path());
        store
            .save(WorkflowScope::Project, Some(project.path()), &definition)
            .expect("test definition should save");
        (
            WorkflowHost::new_with_project_storage(
                data_root.path(),
                project.path(),
                journal,
                driver,
            ),
            data_root,
            project,
        )
    }

    #[test]
    fn artifact_capacity_rejection_happens_before_run_started_or_driver() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        journal.set_artifact_capacity(1, 1);
        let driver = CountingDriver::rejecting_preflight();
        let (host, _data_root, _project) =
            test_host_with_definition(journal.clone(), driver.clone(), tool_definition("read"));

        let mut request = start_request(json!({"provider": "test"}));
        request.name = "read".to_owned();
        let error = host
            .start(request)
            .expect_err("insufficient capacity must reject the run");
        assert!(matches!(
            error,
            WorkflowHostError::SettingsRejected { ref reason, .. }
                if reason == "artifact_capacity"
        ));
        assert!(journal.events.lock().unwrap().is_empty());
        assert_eq!(driver.prepares.load(Ordering::SeqCst), 0);
        assert_eq!(driver.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successor_launch_conflict_is_rejected_before_predecessor_cancel() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        let driver = CountingDriver::new();
        let (host, _data_root, _project) = test_host(journal.clone(), driver.clone());
        let definition = test_definition("inspect");
        let inputs = json!({});
        let model = json!({"provider": "old"});
        let budgets = json!({"maxNodes": 4});
        let started = WorkflowJournalEvent {
            run_id: "predecessor".to_owned(),
            tool_call_id: "tool-1".to_owned(),
            sequence: 0,
            event_type: "run-started".to_owned(),
            payload: json!({
                "status": "running",
                "name": "inspect",
                "scope": "project",
                "definition": definition.clone(),
                "canonicalHash": canonical_hash(&definition).unwrap(),
                "inputHash": hash_inputs(&inputs).unwrap(),
                "inputs": inputs.clone(),
                "parentSessionId": "parent-1",
                "cwd": "/workspace",
                "models": model.clone(),
                "modelHash": workflow_value_digest(Some(&model)),
                "budgets": budgets.clone(),
                "budgetsHash": workflow_value_digest(Some(&budgets)),
                "createdAt": 1,
                "updatedAt": 1,
            }),
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: Some("settings-command".to_owned()),
        };
        journal
            .commit_event("started", started)
            .expect("predecessor fact should be stored");

        let error = host
            .start_definition_change(WorkflowRunDefinitionChangeRequest {
                predecessor_run_id: "predecessor".to_owned(),
                definition: test_definition("inspect"),
                parent_session_id: "parent-1".to_owned(),
                tool_call_id: Some("tool-2".to_owned()),
                launch_input_id: Some("settings-command".to_owned()),
                inputs: json!({}),
                cwd: PathBuf::from("/workspace"),
                model_selection: Some(json!({"provider": "new"})),
                budgets: Some(json!({"maxNodes": 4})),
                subagent_model: None,
            })
            .await
            .expect_err("frozen launch conflict should reject successor");
        assert!(matches!(error, WorkflowHostError::LaunchConflict(_)));
        assert_eq!(driver.cancels.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn artifact_capacity_successor_is_rejected_before_predecessor_cancel() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        let driver = CountingDriver::rejecting_preflight();
        let (host, _data_root, _project) = test_host(journal.clone(), driver.clone());
        let definition = test_definition("inspect");
        let inputs = json!({});
        let model = json!({"provider": "old"});
        let budgets = json!({"maxNodes": 4});
        journal
            .commit_event(
                "started",
                WorkflowJournalEvent {
                    run_id: "predecessor-capacity".to_owned(),
                    tool_call_id: "tool-1".to_owned(),
                    sequence: 0,
                    event_type: "run-started".to_owned(),
                    payload: json!({
                        "status": "running",
                        "name": "inspect",
                        "scope": "project",
                        "definition": definition.clone(),
                        "canonicalHash": canonical_hash(&definition).unwrap(),
                        "inputHash": hash_inputs(&inputs).unwrap(),
                        "inputs": inputs,
                        "parentSessionId": "parent-1",
                        "cwd": "/workspace",
                        "models": model.clone(),
                        "modelHash": workflow_value_digest(Some(&model)),
                        "budgets": budgets.clone(),
                        "budgetsHash": workflow_value_digest(Some(&budgets)),
                        "createdAt": 1,
                        "updatedAt": 1,
                    }),
                    artifacts: Vec::new(),
                    actor_session_id: None,
                    launch_input_id: Some("old-command".to_owned()),
                },
            )
            .expect("predecessor fact should be stored");

        let error = host
            .start_definition_change(WorkflowRunDefinitionChangeRequest {
                predecessor_run_id: "predecessor-capacity".to_owned(),
                definition,
                parent_session_id: "parent-1".to_owned(),
                tool_call_id: Some("tool-2".to_owned()),
                launch_input_id: Some("new-command".to_owned()),
                inputs: json!({}),
                cwd: PathBuf::from("/workspace"),
                model_selection: Some(json!({"provider": "new"})),
                budgets: Some(json!({"maxNodes": 4})),
                subagent_model: None,
            })
            .await
            .expect_err("capacity rejection must preserve predecessor");
        assert!(matches!(
            error,
            WorkflowHostError::SettingsRejected { ref reason, .. }
                if reason == "artifact_capacity"
        ));
        assert_eq!(driver.cancels.load(Ordering::SeqCst), 0);
        assert_eq!(journal.all_events("predecessor-capacity").unwrap().len(), 1);
    }

    #[test]
    fn input_resolution_applies_defaults_and_rejects_unknown_keys() {
        let mut specs = BTreeMap::new();
        specs.insert(
            "count".to_owned(),
            InputSpec {
                value_type: InputType::Integer,
                description: None,
                required: true,
                default: Some(json!(1)),
            },
        );
        assert_eq!(
            resolve_inputs(&specs, json!({})).unwrap(),
            json!({"count": 1})
        );
        assert!(resolve_inputs(&specs, json!({"other": 1})).is_err());
    }

    #[test]
    fn workflow_run_ordinals_are_shared_across_host_instances() {
        let first = NEXT_WORKFLOW_RUN.fetch_add(1, Ordering::Relaxed);
        let second = NEXT_WORKFLOW_RUN.fetch_add(1, Ordering::Relaxed);
        assert_ne!(first, second);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn launch_input_id_reuses_journal_run_without_restarting_driver() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        let driver = CountingDriver::new();
        let (host, _data_root, _project) = test_host(journal.clone(), driver.clone());
        let first = host
            .start(start_request(json!({"provider": "test"})))
            .expect("first launch should succeed");
        let second = host
            .start(start_request(json!({"provider": "test"})))
            .expect("same launch should reuse");
        assert_eq!(first, second);
        assert_eq!(driver.prepares.load(Ordering::Relaxed), 1);
        tokio::task::yield_now().await;
        assert_eq!(driver.starts.load(Ordering::Relaxed), 1);
        assert_eq!(journal.all_events(&first.run_id).unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn launch_input_id_reuses_after_host_reconstruction_and_rejects_frozen_conflict() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        let driver = CountingDriver::new();
        let (host, data_root, project) = test_host(journal.clone(), driver.clone());
        let first = host
            .start(start_request(json!({"provider": "test"})))
            .expect("first launch should succeed");
        drop(host);
        let rebuilt = WorkflowHost::new_with_project_storage(
            data_root.path(),
            project.path(),
            journal.clone(),
            driver.clone(),
        );
        let reused = rebuilt
            .start(start_request(json!({"provider": "test"})))
            .expect("cold host should reuse Journal run");
        assert_eq!(reused, first);
        let conflict = rebuilt
            .start(start_request(json!({"provider": "other"})))
            .expect_err("changed frozen model must conflict");
        assert!(
            matches!(conflict, WorkflowHostError::LaunchConflict(message) if message.contains("model selection digest differs"))
        );
        assert_eq!(driver.prepares.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_same_launch_id_commits_one_run_before_driver_failure() {
        let journal = Arc::new(MemoryWorkflowJournal::default());
        let driver = CountingDriver::rejecting_prepare();
        let (host, _data_root, _project) = test_host(journal.clone(), driver.clone());
        let left_host = Arc::clone(&host);
        let right_host = Arc::clone(&host);
        let left_request = start_request(json!({"provider": "test"}));
        let right_request = left_request.clone();
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(move || left_host.start(left_request));
            let right = scope.spawn(move || right_host.start(right_request));
            (
                left.join().expect("left launch thread should finish"),
                right.join().expect("right launch thread should finish"),
            )
        });
        let left = left.expect("left launch should succeed");
        let right = right.expect("right launch should reuse");
        assert_eq!(left, right);
        assert_eq!(driver.prepares.load(Ordering::Relaxed), 1);
        assert_eq!(journal.all_events(&left.run_id).unwrap().len(), 2);
        assert_eq!(
            journal
                .all_events(&left.run_id)
                .unwrap()
                .iter()
                .filter(|event| event.event_type == "run-started")
                .count(),
            1
        );
    }
}
