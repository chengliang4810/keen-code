//! NativeHost 运行时服务。
//!
//! 这里装配设置页所需的 Automation scheduler 与 WorkflowHost。它们都复用启动器
//! 创建的同一个 `AgentRuntime` 和 Tokio runtime，不创建第二套模型、Session 或 Journal
//! 状态。设置页收到的投影只从这些真实状态读取。

use crate::agent_runtime::{AgentRuntime, RootTurnOptions};
use crate::native_paths::NativePaths;
use crate::native_ui::settings::contracts::*;
use crate::native_ui::settings::native_settings::{
    NativeSettingsAdapter, NativeSettingsRuntimePort,
};
use crate::storage;
use crate::workflows::agent_tools::{WorkflowToolError, WorkflowToolRequest};
use crate::workflows::{
    DynamicWorkflowRunProgressPayload, EngineWorkflowDriver, FrozenWorkflowRun,
    NativeWorkflowDriver, RuntimeWorkflowJournal, WorkflowActorBinding, WorkflowArtifact,
    WorkflowArtifactBytes, WorkflowArtifactPage, WorkflowDefinition, WorkflowEventPage,
    WorkflowGraphResult, WorkflowHost, WorkflowJournalPort, WorkflowModelSelection,
    WorkflowNodeResult, WorkflowRunDefinitionChangeRequest, WorkflowRunStatus, WorkflowRunSummary,
    WorkflowScope, WorkflowStore, WorkflowWorkspaceResult,
};
use chrono::{Local, TimeZone};
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};
use keencode_acp::ConnectionId;
use keencode_agent::GoalController;
use keencode_resources::{ROOT_AGENT_ID, SessionEvent, TurnId, TurnStatus, TurnStopReason};
use keencode_runtime::{
    PersistentAgentState, RuntimeEventPayload, RuntimeEventReceiveError, RuntimeSession,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

// schema 2 只持久化 workspacePath；旧 schema 直接拒绝，不做隐式迁移。
pub(crate) const AUTOMATION_SCHEMA: u64 = 2;
const MAX_SETTINGS_JSON_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WORKFLOW_RUNS: usize = 100;
const MAX_WORKFLOW_HOSTS: usize = 64;
const MAX_WORKFLOW_NODE_RESULT_BYTES: usize = 512 * 1024;
const MAX_WORKFLOW_ARTIFACT_CHUNK_BYTES: usize = 512 * 1024;
// 设置页问答沿用 typed elicitation，但不能让单次答案绕过控制面内存上限。
const MAX_WORKFLOW_ANSWER_BYTES: usize = 256 * 1024;
const MAX_SUBAGENT_FILE_BYTES: u64 = 512 * 1024;
const MAX_SUBAGENT_FILES: usize = 256;
const MAX_SUBAGENT_LIST_ITEMS: usize = 128;
const MAX_SUBAGENT_LIST_ITEM_BYTES: usize = 256;
const MAX_SUBAGENT_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_SUBAGENT_NAME_BYTES: usize = 128;
const MAX_SUBAGENT_TURNS: u32 = 10_000;
const SUBAGENT_STATE_FILE: &str = "agents-state.json";
// 禁用的 Agent 保留原文但改用非 md 后缀，候选发现会自然跳过它；状态文件仍保存
// 稳定 ID，重新启用时可恢复原配置而不会让当前候选误注册禁用模板。
const SUBAGENT_DISABLED_SUFFIX: &str = ".disabled";
const AUTOMATION_ERROR_RETRY: Duration = Duration::from_secs(5);
const NATIVE_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
// Automation 共享 AgentRuntime，必须限制同时派发的后台回合，避免恢复或大量到期
// 任务一次性创建无界 Tokio task 峰值；完成任务会通过 scheduler_wake 继续排队。
const AUTOMATION_MAX_CONCURRENT: usize = 2;
static NEXT_SCHEDULED_RUN: AtomicU64 = AtomicU64::new(0);

/// Workflow AgentTool 的定义参数；Session、cwd 与工具调用身份均由 NativeServices
/// 从可信 Runtime 上下文注入，不能由模型参数覆盖。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkflowToolDefinitionArgs {
    #[serde(default)]
    scope: WorkflowScope,
    definition: WorkflowDefinition,
}

/// Workflow AgentTool 的查询参数；run 归属由当前父 Session 的 Journal 再次确认。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkflowToolRunArgs {
    run_id: String,
}

fn workflow_tool_error(
    code: impl Into<String>,
    error: impl std::fmt::Display,
) -> WorkflowToolError {
    WorkflowToolError::permanent(
        code,
        keencode_model::redact_error_secrets_bounded(&error.to_string(), 2_048),
    )
}

/// NativeHost 的 Automation/Workflow 生产服务。
///
/// `WorkflowHost` 绑定父 Session，因此按 Session 缓存；Automation 只保存调度事实和
/// 运行历史，具体模型回合仍由共享 `AgentRuntime` 启动。`shutdown` 只收口本服务创建的
/// 后台任务，不释放 Runtime 的所有权。
pub struct NativeServices {
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    executor: Arc<Runtime>,
    workflow_hosts: Mutex<HashMap<String, Arc<WorkflowHost>>>,
    automation_lock: Arc<Mutex<()>>,
    // 运行索引只保存当前进程内的真实 Session 绑定；台账仍是 Automation 运行历史的事实源。
    automation_runs: Arc<Mutex<HashMap<String, AutomationRunBinding>>>,
    // 调度器与手动触发共用此索引，关闭时可等待所有 Automation 任务收口。
    automation_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    subagent_lock: Mutex<()>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
    scheduler_stop: CancellationToken,
    scheduler_wake: Arc<Notify>,
    // Scheduler 只持有设置适配器的弱引用；台账完成写入后通知已打开的 Automations 页，
    // 避免设置服务与运行时服务互相强持有形成生命周期环。
    settings_invalidator: Mutex<Option<std::sync::Weak<NativeSettingsAdapter>>>,
    started: AtomicBool,
    revisions: AtomicU64,
    next_run: AtomicU64,
}

struct NativeServicesShutdownState {
    scheduler: Option<JoinHandle<()>>,
    automation_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    workflow_hosts: Vec<Arc<WorkflowHost>>,
}

impl NativeServices {
    /// 使用 NativeHost 已装配的路径、Runtime 和 Tokio executor 创建服务。
    pub fn new(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        executor: Arc<Runtime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            runtime,
            executor,
            workflow_hosts: Mutex::new(HashMap::new()),
            automation_lock: Arc::new(Mutex::new(())),
            automation_runs: Arc::new(Mutex::new(HashMap::new())),
            automation_tasks: Arc::new(Mutex::new(HashMap::new())),
            subagent_lock: Mutex::new(()),
            scheduler: Mutex::new(None),
            scheduler_stop: CancellationToken::new(),
            scheduler_wake: Arc::new(Notify::new()),
            settings_invalidator: Mutex::new(None),
            started: AtomicBool::new(false),
            revisions: AtomicU64::new(0),
            next_run: AtomicU64::new(0),
        })
    }

    fn take_shutdown_state(&self) -> Option<NativeServicesShutdownState> {
        if !self.started.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.scheduler_stop.cancel();
        self.scheduler_wake.notify_waiters();

        let scheduler = self.scheduler.lock().ok().and_then(|mut slot| slot.take());
        if scheduler.is_none() {
            tracing::debug!("Automation scheduler 句柄已不可用");
        }
        let workflow_hosts = self
            .workflow_hosts
            .lock()
            .map(|mut hosts| hosts.drain().map(|(_, host)| host).collect())
            .unwrap_or_else(|_| {
                tracing::warn!("WorkflowHost 缓存锁不可用，跳过关闭等待");
                Vec::new()
            });
        Some(NativeServicesShutdownState {
            scheduler,
            automation_tasks: self.automation_tasks.clone(),
            workflow_hosts,
        })
    }

    async fn finish_shutdown(state: NativeServicesShutdownState) {
        let deadline = time::Instant::now() + NATIVE_TASK_SHUTDOWN_TIMEOUT;
        let workflow_shutdown = futures::future::join_all(
            state
                .workflow_hosts
                .into_iter()
                .map(|host| async move { host.shutdown(deadline).await }),
        );
        let automation_tasks = state.automation_tasks;
        let automation_shutdown = async move {
            if let Some(mut scheduler) = state.scheduler {
                await_native_task(&mut scheduler, "Automation scheduler", deadline).await;
            }
            let tasks = automation_tasks
                .lock()
                .map(|mut tasks| std::mem::take(&mut *tasks).into_iter().collect())
                .unwrap_or_else(|_| {
                    tracing::warn!("Automation 执行句柄锁不可用，跳过等待");
                    Vec::new()
                });
            await_native_tasks(tasks, deadline).await;
        };
        let (_, ()) = tokio::join!(workflow_shutdown, automation_shutdown);
    }

    /// NativeHost 使用异步边界等待所有本服务派生的任务，避免在 Runtime worker 内嵌套
    /// `block_on`；同步设置端口仍通过 `shutdown_blocking` 复用同一收口实现。
    pub(crate) async fn shutdown_async(&self) {
        let Some(state) = self.take_shutdown_state() else {
            return;
        };
        Self::finish_shutdown(state).await;
    }

    fn shutdown_blocking(&self) {
        let Some(state) = self.take_shutdown_state() else {
            return;
        };
        let handle = self.executor.handle().clone();
        let join = std::thread::Builder::new()
            .name("keencode-native-services-shutdown".to_owned())
            .spawn(move || handle.block_on(Self::finish_shutdown(state)));
        match join {
            Ok(join) => {
                if join.join().is_err() {
                    tracing::error!("NativeServices 关闭线程异常退出");
                }
            }
            Err(error) => tracing::error!(%error, "无法创建 NativeServices 关闭线程"),
        }
    }

    /// 判断指定 Session 是否可以释放 WorkflowHost 缓存。
    ///
    /// 无缓存或全部 run 已终态时返回 `true`；存在活动 run 或无法可靠读取
    /// Journal 时返回 `false`，以阻止 Runtime 同时释放仍被 WorkflowHost 使用的 Session。
    pub(crate) fn release_session_cache(&self, session_id: &str) -> bool {
        let mut hosts = match self.workflow_hosts.lock() {
            Ok(hosts) => hosts,
            Err(_) => {
                tracing::warn!(session_id, "WorkflowHost 缓存锁不可用，跳过空闲会话释放");
                return false;
            }
        };
        let Some(host) = hosts.get(session_id).cloned() else {
            return true;
        };
        let runs = match host.list_runs(Some(session_id), None, None, MAX_WORKFLOW_RUNS) {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(session_id, %error, "读取 WorkflowHost 运行状态失败，保留缓存");
                return false;
            }
        };
        if runs.truncated
            || runs.runs.iter().any(|run| {
                !matches!(
                    &run.status,
                    WorkflowRunStatus::Completed
                        | WorkflowRunStatus::Errored
                        | WorkflowRunStatus::Stopped
                )
            })
        {
            return false;
        }
        hosts.remove(session_id);
        true
    }

    fn next_revision(&self) -> u64 {
        self.revisions
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    /// 装配设置页的失效通知端口。只保存弱引用，NativeHost 关闭设置服务后，
    /// scheduler 仍可安全完成台账收尾而不会访问已释放的窗口投影。
    pub fn set_settings_invalidator(&self, settings: &Arc<NativeSettingsAdapter>) {
        match self.settings_invalidator.lock() {
            Ok(mut slot) => *slot = Some(Arc::downgrade(settings)),
            Err(_) => tracing::warn!("NativeSettings 失效通知锁不可用"),
        }
    }

    fn settings_invalidator(&self) -> Option<std::sync::Weak<NativeSettingsAdapter>> {
        self.settings_invalidator
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    fn snapshot(&self, page: SettingsPage) -> SettingsSnapshot {
        SettingsSnapshot {
            page,
            revision: self.next_revision(),
            ..SettingsSnapshot::default()
        }
    }

    fn current_session(&self) -> SettingsResult<Option<(String, RuntimeSession)>> {
        let session_id = self
            .runtime
            .focused_session_id()
            .map_err(|error| settings_error("settings_focus", error))?;
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let session = self
            .runtime
            .runtime_manager()
            .get(session_id.clone())
            .map_err(|error| settings_error("settings_session", error))?;
        Ok(Some((session_id, session)))
    }

    fn current_project(&self) -> SettingsResult<Option<PathBuf>> {
        let Some((_, session)) = self.current_session()? else {
            return Ok(None);
        };
        let project = session
            .read_state(|state| state.project_root.clone())
            .map_err(|error| settings_error("settings_session_state", error))?;
        let project = project.trim();
        Ok((!project.is_empty()).then(|| PathBuf::from(project)))
    }

    fn workflow_host(
        &self,
        session_id: &str,
        session: RuntimeSession,
    ) -> SettingsResult<Arc<WorkflowHost>> {
        if let Some(host) = self
            .workflow_hosts
            .lock()
            .map_err(|_| settings_error("settings_workflow_cache", "工作流缓存锁不可用"))?
            .get(session_id)
            .cloned()
        {
            return Ok(host);
        }
        let project_path = session
            .read_state(|state| state.project_root.clone())
            .map_err(|error| settings_error("settings_session_state", error))?;
        let project_path = PathBuf::from(project_path);
        let actor_runtime = self.runtime.clone();
        let actor_reader = crate::workflows::workflow_actor_transcript_reader(
            move |binding: &WorkflowActorBinding| {
                let actor = match actor_runtime.open_or_create_session(
                    Path::new(&binding.project_root),
                    Some(&binding.actor_session_id),
                    "workflow-workspace-read",
                ) {
                    Ok(actor) => actor,
                    // 父 Journal 的 actor-started 事实可能早于 actor Session 落盘；
                    // 此时读面返回缺失，不能以任意同名 Session 代替 actor。
                    Err(_) => return Ok(None),
                };
                let bound = actor
                    .read_state(|state| {
                        state.project_root == binding.project_root
                            && state.workflow_events.values().flatten().any(|event| {
                                event.event_type == "actor-bound"
                                    && event.run_id == binding.run_id
                                    && event.actor_session_id.as_deref()
                                        == Some(binding.actor_session_id.as_str())
                                    && event.payload.get("parentSessionId").and_then(Value::as_str)
                                        == Some(binding.parent_session_id.as_str())
                                    && event.payload.get("runId").and_then(Value::as_str)
                                        == Some(binding.run_id.as_str())
                                    && event.payload.get("nodeId").and_then(Value::as_str)
                                        == Some(binding.node_id.as_str())
                                    && event.payload.get("nodeAddress")
                                        == Some(&binding.node_address)
                                    && event.payload.get("cwd").and_then(Value::as_str)
                                        == Some(binding.project_root.as_str())
                            })
                    })
                    .map_err(|error| crate::workflows::WorkflowJournalError(error.to_string()))?;
                if !bound {
                    return Ok(None);
                }
                actor_runtime
                    .session_transcript(&binding.actor_session_id)
                    .map(Some)
                    .map_err(|error| crate::workflows::WorkflowJournalError(error.to_string()))
            },
        );
        let journal = Arc::new(RuntimeWorkflowJournal::new_with_actor_transcript_reader(
            session.clone(),
            actor_reader,
        ));
        journal
            .reconcile_cold_runs()
            .map_err(|error| settings_error("settings_workflow_reconcile", error))?;
        let leaf = Arc::new(NativeWorkflowDriver::new(self.runtime.clone(), session));
        let runtime = self.runtime.clone();
        let parent_session_id = session_id.to_owned();
        let resolver = crate::workflows::workflow_question_resolver(move |qid, answer| {
            let runtime = runtime.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                runtime
                    .resolve_workflow_question(&parent_session_id, &qid, &answer)
                    .map_err(|error| crate::workflows::WorkflowRuntimeError(error.to_string()))
            })
        });
        let driver = Arc::new(EngineWorkflowDriver::new(leaf, journal.clone(), resolver));
        let host = WorkflowHost::new_with_project_storage(
            self.paths.data_root.clone(),
            project_path,
            journal,
            driver,
        );
        let mut hosts = self
            .workflow_hosts
            .lock()
            .map_err(|_| settings_error("settings_workflow_cache", "工作流缓存锁不可用"))?;
        if hosts.len() >= MAX_WORKFLOW_HOSTS {
            let evict = hosts
                .iter()
                .find(|(cached_session_id, cached_host)| {
                    cached_host
                        .list_runs(
                            Some(cached_session_id.as_str()),
                            None,
                            None,
                            MAX_WORKFLOW_RUNS,
                        )
                        .map(|runs| {
                            !runs.truncated
                                && runs.runs.iter().all(|run| {
                                    matches!(
                                        &run.status,
                                        WorkflowRunStatus::Completed
                                            | WorkflowRunStatus::Errored
                                            | WorkflowRunStatus::Stopped
                                    )
                                })
                        })
                        .unwrap_or(false)
                })
                .map(|(cached_session_id, _)| cached_session_id.clone());
            if let Some(cached_session_id) = evict {
                hosts.remove(&cached_session_id);
            }
        }
        hosts.insert(session_id.to_owned(), host.clone());
        Ok(host)
    }

    fn workflow_host_for_current(&self) -> SettingsResult<(String, Arc<WorkflowHost>)> {
        let Some((session_id, session)) = self.current_session()? else {
            return Err(unsupported(
                "settings_workflow_session",
                "当前没有可用的 Session",
            ));
        };
        Ok((
            session_id.clone(),
            self.workflow_host(&session_id, session)?,
        ))
    }

    /// 将普通 root Agent 的 Workflow AgentTool 路由到当前父 Session 的真实 Host。
    ///
    /// AgentTool 的输入只包含定义或 runId；身份、项目根、当前 Turn 和 Plan 状态
    /// 全部从 Runtime Journal 读取，并在进入缓存 Host 前完成校验，避免模型参数
    /// 伪造另一个 Session 或让 workflow actor 递归创建工作流。
    pub(crate) fn workflow_tool_call(
        &self,
        request: WorkflowToolRequest,
    ) -> Result<Value, WorkflowToolError> {
        if request.source_agent_id != ROOT_AGENT_ID {
            return Err(workflow_tool_error(
                "workflow.identity",
                "只有普通根 Agent 可以管理工作流",
            ));
        }
        let parent = crate::session_commands::open_authorized_session(
            &self.runtime,
            &self.paths,
            &request.session_id,
        )
        .map_err(|error| workflow_tool_error("workflow.identity", error))?;
        if parent
            .is_workflow_actor()
            .map_err(|error| workflow_tool_error("workflow.identity", error))?
        {
            return Err(workflow_tool_error(
                "workflow.identity",
                "工作流 actor 不能嵌套工作流",
            ));
        }
        let snapshot = parent
            .snapshot()
            .map_err(|error| workflow_tool_error("workflow.identity", error))?;
        let turn_id = TurnId::new(request.turn_id.clone())
            .map_err(|error| workflow_tool_error("workflow.identity", error))?;
        let turn = snapshot.state.turns.get(&turn_id).ok_or_else(|| {
            workflow_tool_error("workflow.identity", "工作流工具不属于当前父会话")
        })?;
        if turn.status != TurnStatus::Running
            || turn.source_agent_id.as_str() != request.source_agent_id
        {
            return Err(workflow_tool_error(
                "workflow.identity",
                "工作流工具不属于正在执行的根回合",
            ));
        }
        if snapshot.state.plan.enabled
            && matches!(request.method.as_str(), "createWorkflow" | "saveWorkflow")
        {
            return Err(workflow_tool_error(
                "workflow.plan_read_only",
                "Plan 模式禁止工作流写入",
            ));
        }

        let project_root = PathBuf::from(snapshot.state.project_root);
        if project_root.as_os_str().is_empty() {
            return Err(workflow_tool_error(
                "workflow.identity",
                "父会话没有有效的项目根目录",
            ));
        }

        match request.method.as_str() {
            "createWorkflow" | "saveWorkflow" => {
                let input: WorkflowToolDefinitionArgs = serde_json::from_value(request.args)
                    .map_err(|error| workflow_tool_error("workflow.input", error))?;
                let store = WorkflowStore::new(self.paths.data_root.clone());
                let metadata = store
                    .save(input.scope, Some(project_root.as_path()), &input.definition)
                    .map_err(|error| workflow_tool_error("workflow.store", error))?;
                serde_json::to_value(metadata)
                    .map_err(|error| workflow_tool_error("workflow.result", error))
            }
            "getWorkflowRun" | "runSummary" | "graph" => {
                let input: WorkflowToolRunArgs = serde_json::from_value(request.args)
                    .map_err(|error| workflow_tool_error("workflow.input", error))?;
                let host = self
                    .workflow_host(&request.session_id, parent)
                    .map_err(|error| workflow_tool_error("workflow.host", error))?;
                let result = if request.method == "graph" {
                    serde_json::to_value(
                        host.graph(&input.run_id)
                            .map_err(|error| workflow_tool_error("workflow.host", error))?,
                    )
                } else {
                    serde_json::to_value(
                        host.run_summary(&input.run_id)
                            .map_err(|error| workflow_tool_error("workflow.host", error))?,
                    )
                };
                result.map_err(|error| workflow_tool_error("workflow.result", error))
            }
            method => Err(workflow_tool_error(
                "workflow.method",
                format!("未知的 Workflow AgentTool 方法：{method}"),
            )),
        }
    }

    fn load_automations(&self) -> SettingsResult<SettingsSnapshot> {
        let document = read_automation_document(&self.paths.data_root)?;
        let runs = document
            .get("runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut items = document
            .get("automations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| automation_record(record, &runs))
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.id.cmp(&right.id));
        let mut snapshot = self.snapshot(SettingsPage::Automations);
        snapshot.automations = Some(AutomationSettings { items });
        Ok(snapshot)
    }

    fn load_workflows(&self) -> SettingsResult<SettingsSnapshot> {
        let project = self.current_project()?;
        let mut items = Vec::new();
        let global = WorkflowStore::new(self.paths.data_root.clone());
        append_workflow_records(&global, WorkflowScope::Global, None, self, &mut items)?;
        if let Some(project) = project.as_deref() {
            let store = WorkflowStore::with_project_storage(
                self.paths.data_root.clone(),
                project.to_path_buf(),
            );
            append_workflow_records(
                &store,
                WorkflowScope::Project,
                Some(project),
                self,
                &mut items,
            )?;
        }
        items.sort_by(|left, right| {
            left.scope
                .cmp(&right.scope)
                .then_with(|| left.name.cmp(&right.name))
        });
        let mut snapshot = self.snapshot(SettingsPage::Workflows);
        snapshot.workflows = Some(WorkflowSettings {
            items,
            inspection: None,
        });
        Ok(snapshot)
    }

    fn load_agents(&self) -> SettingsResult<SettingsSnapshot> {
        let mut subagents = self
            .load_persisted_subagents()?
            .into_iter()
            .map(|entry| (entry.summary.id.clone(), entry.summary))
            .collect::<HashMap<_, _>>();
        if let Some((_, session)) = self.current_session()? {
            let runtime_subagents = session
                .read_state(|state| {
                    state
                        .sub_agents
                        .values()
                        .map(|agent| SubagentSummary {
                            id: agent.agent_id.to_string(),
                            name: agent.agent_path.clone(),
                            source: "runtime".to_owned(),
                            enabled: !matches!(
                                agent.status,
                                keencode_resources::SubAgentStatus::Stopped
                            ),
                            model: None,
                            tools: Vec::new(),
                            max_turns: None,
                            description: Some(agent.task.clone()),
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(|error| settings_error("settings_agent_snapshot", error))?;
            for agent in runtime_subagents {
                subagents.entry(agent.id.clone()).or_insert(agent);
            }
        }
        let mut subagents = subagents.into_values().collect::<Vec<_>>();
        subagents.sort_by(|left, right| left.id.cmp(&right.id));
        let mut snapshot = self.snapshot(SettingsPage::Agents);
        snapshot.agents = Some(AgentSettings {
            goal: None,
            goal_revision: 0,
            subagents,
        });
        Ok(snapshot)
    }

    fn load_persisted_subagents(&self) -> SettingsResult<Vec<StoredSubagent>> {
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let disabled = read_disabled_subagents(&self.paths.data_root)?;
        self.scan_subagents(&disabled)
    }

    fn scan_subagents(&self, disabled: &BTreeSet<String>) -> SettingsResult<Vec<StoredSubagent>> {
        let mut entries = Vec::new();
        for (source, root) in self.subagent_roots()? {
            scan_subagent_root(&root, &source, disabled, &mut entries)?;
        }
        entries.sort_by(|left, right| left.summary.id.cmp(&right.summary.id));
        Ok(entries)
    }

    fn subagent_roots(&self) -> SettingsResult<Vec<(String, PathBuf)>> {
        let mut roots = vec![("global".to_owned(), self.paths.data_root.join("agents"))];
        if let Some(project) = self.current_project()? {
            let project = fs::canonicalize(&project).map_err(|error| {
                settings_error(
                    "settings_subagent_project",
                    format!("无法规范化项目根：{error}"),
                )
            })?;
            if !project.is_dir() {
                return Err(unsupported(
                    "settings_subagent_project",
                    "当前项目根必须是目录",
                ));
            }
            // Native candidate 只从项目 `.keencode/agents` 发现模板；设置端必须写入同一根，
            // 不能沿旧 UI 的 `.agents/agents` 另存一份看似成功但运行时不可见的配置。
            roots.push((
                "project".to_owned(),
                project.join(".keencode").join("agents"),
            ));
        }
        Ok(roots)
    }

    fn mutable_subagent_root(&self, source: &str) -> SettingsResult<PathBuf> {
        let source = normalize_subagent_source(source)?;
        self.subagent_roots()?
            .into_iter()
            .find_map(|(candidate, root)| (candidate == source).then_some(root))
            .ok_or_else(|| {
                unsupported(
                    "settings_subagent_scope",
                    "项目 Subagent 需要当前已打开项目",
                )
            })
    }

    fn find_subagent_locked(&self, agent_id: &str) -> SettingsResult<StoredSubagent> {
        validate_subagent_identifier(agent_id, "settings_subagent_id")?;
        let disabled = read_disabled_subagents(&self.paths.data_root)?;
        let entries = self.scan_subagents(&disabled)?;
        entries
            .into_iter()
            .find(|entry| subagent_identifier_matches(agent_id, entry))
            .ok_or_else(|| unsupported("settings_subagent_not_found", "找不到指定 Subagent"))
    }

    /// 配置文件写入成功后只标记当前项目候选失效；活动 Turn 继续使用其冻结快照，
    /// 下一轮由 NativeHost 的候选入口重建并重新读取 Agent 文件。
    fn invalidate_subagent_candidate(&self) -> SettingsResult<()> {
        let Some(project) = self.current_project()? else {
            return Ok(());
        };
        self.runtime
            .invalidate_extension_candidate(&project)
            .map_err(|error| settings_error("settings_subagent_refresh", error))
    }

    fn create_subagent(&self, summary: &SubagentSummary) -> SettingsResult<()> {
        validate_subagent_summary(summary)?;
        let source = normalize_subagent_source(&summary.source)?;
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let root = self.mutable_subagent_root(source)?;
        ensure_subagent_root(&root)?;
        let active_path = root.join(format!("{}.md", summary.name));
        let disabled_path = subagent_disabled_path(&active_path);
        if active_path.exists() || disabled_path.exists() {
            return Err(unsupported("settings_subagent_exists", "Subagent 已存在"));
        }
        write_subagent_document(&active_path, summary, None)?;
        if !summary.enabled {
            fs::rename(&active_path, &disabled_path).map_err(|error| {
                settings_error(
                    "settings_subagent_write",
                    format!("保存禁用状态失败：{error}"),
                )
            })?;
        }
        update_disabled_subagent_state(&self.paths.data_root, summary, summary.enabled)?;
        Ok(())
    }

    fn update_subagent(&self, summary: &SubagentSummary) -> SettingsResult<()> {
        validate_subagent_summary(summary)?;
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let existing = if summary.id.trim().is_empty() {
            let disabled = read_disabled_subagents(&self.paths.data_root)?;
            self.scan_subagents(&disabled)?
                .into_iter()
                .find(|entry| entry.summary.name.eq_ignore_ascii_case(&summary.name))
                .ok_or_else(|| unsupported("settings_subagent_not_found", "找不到指定 Subagent"))?
        } else {
            self.find_subagent_locked(&summary.id)?
        };
        let source = if summary.source.trim().is_empty() {
            existing.source.clone()
        } else {
            normalize_subagent_source(&summary.source)?.to_owned()
        };
        let mut persisted_summary = summary.clone();
        persisted_summary.source = source.clone();
        let root = self.mutable_subagent_root(&source)?;
        ensure_subagent_root(&root)?;
        let target_active = root.join(format!("{}.md", persisted_summary.name));
        let target = if persisted_summary.enabled {
            target_active.clone()
        } else {
            subagent_disabled_path(&target_active)
        };
        let disabled_target = subagent_disabled_path(&target_active);
        let target_conflicts = [target_active.as_path(), disabled_target.as_path()]
            .into_iter()
            .any(|path| path != existing.path.as_path() && path.exists());
        if target_conflicts {
            return Err(unsupported(
                "settings_subagent_exists",
                "目标 Subagent 已存在",
            ));
        }
        let document = read_subagent_document(&existing.path)?;
        write_subagent_document(&target, &persisted_summary, Some(&document))?;
        if target != existing.path {
            fs::remove_file(&existing.path).map_err(|error| {
                settings_error(
                    "settings_subagent_write",
                    format!("删除旧 Subagent 失败：{error}"),
                )
            })?;
        }
        remove_disabled_subagent_state(&self.paths.data_root, &existing.summary)?;
        update_disabled_subagent_state(
            &self.paths.data_root,
            &persisted_summary,
            persisted_summary.enabled,
        )?;
        Ok(())
    }

    fn delete_subagent(&self, agent_id: &str) -> SettingsResult<()> {
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let existing = self.find_subagent_locked(agent_id)?;
        // 先校验状态文件可读，避免删除已经成功后才发现状态无法持久化。
        let _ = read_disabled_subagents(&self.paths.data_root)?;
        fs::remove_file(&existing.path).map_err(|error| {
            settings_error(
                "settings_subagent_delete",
                format!("删除 Subagent 失败：{error}"),
            )
        })?;
        remove_disabled_subagent_state(&self.paths.data_root, &existing.summary)?;
        Ok(())
    }

    fn set_subagent_enabled(&self, agent_id: &str, enabled: bool) -> SettingsResult<()> {
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let existing = self.find_subagent_locked(agent_id)?;
        let target_active = existing
            .path
            .parent()
            .map(|parent| parent.join(format!("{}.md", existing.summary.name)))
            .ok_or_else(|| unsupported("settings_subagent_path", "Subagent 路径无效"))?;
        let target = if enabled {
            target_active
        } else {
            subagent_disabled_path(&target_active)
        };
        if target != existing.path {
            if target.exists() {
                return Err(unsupported(
                    "settings_subagent_exists",
                    "目标 Subagent 文件已存在",
                ));
            }
            fs::rename(&existing.path, &target).map_err(|error| {
                settings_error(
                    "settings_subagent_enabled",
                    format!("更新 Subagent 启用状态失败：{error}"),
                )
            })?;
        }
        update_disabled_subagent_state(&self.paths.data_root, &existing.summary, enabled)
    }

    fn set_subagent_model(
        &self,
        agent_id: &str,
        model: Option<ModelSelection>,
    ) -> SettingsResult<()> {
        validate_optional_model_selection(model.as_ref())?;
        let _guard = self
            .subagent_lock
            .lock()
            .map_err(|_| settings_error("settings_subagent_lock", "Subagent 锁不可用"))?;
        let existing = self.find_subagent_locked(agent_id)?;
        let document = read_subagent_document(&existing.path)?;
        let mut summary = existing.summary.clone();
        summary.model = model;
        write_subagent_document(&existing.path, &summary, Some(&document))
    }

    fn create_automation(&self, record: AutomationRecord) -> SettingsResult<()> {
        validate_automation(&record)?;
        let _guard = self
            .automation_lock
            .lock()
            .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
        let mut document = read_automation_document(&self.paths.data_root)?;
        let items = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| unsupported("settings_automation_shape", "Automation 列表无效"))?;
        let id = if record.id.trim().is_empty() {
            format!("automation-{}-{}", now_ms(), self.next_revision())
        } else {
            validate_component(&record.id, "settings_automation_id")?;
            record.id.clone()
        };
        if items
            .iter()
            .any(|value| value.get("automationId").and_then(Value::as_str) == Some(id.as_str()))
        {
            return Err(unsupported(
                "settings_automation_exists",
                "Automation 已存在",
            ));
        }
        let now = now_ms();
        let workspace = self
            .current_project()?
            .unwrap_or_else(|| self.paths.data_root.clone());
        let next_run_at = if record.enabled {
            Some(next_cron_hint(&record.cron_expr, now).ok_or_else(|| {
                unsupported(
                    "settings_automation_cron",
                    "Cron 表达式没有可执行的下一次时间",
                )
            })?)
        } else {
            record.next_run_at_ms
        };
        items.push(json!({
            "automationId": id,
            "title": record.title,
            "cronExpr": record.cron_expr,
            "prompt": record.prompt,
            "workspacePath": workspace.to_string_lossy(),
            "locationKind": "local",
            "recurring": record.recurring,
            "maxRuns": record.max_runs,
            "runCount": 0,
            "scheduledRunCount": 0,
            "enabled": record.enabled,
            "lifecycleStatus": if record.enabled { "active" } else { "paused" },
            "nextRunAt": next_run_at,
            "dispatchStatus": "idle",
            "dispatchAttempts": 0,
            "lastError": Value::Null,
            "createdAt": now,
            "updatedAt": now,
        }));
        write_automation_document(&self.paths.data_root, &document)?;
        self.scheduler_wake.notify_one();
        Ok(())
    }

    fn update_automation(&self, record: AutomationRecord) -> SettingsResult<()> {
        validate_automation(&record)?;
        validate_component(&record.id, "settings_automation_id")?;
        let _guard = self
            .automation_lock
            .lock()
            .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
        let mut document = read_automation_document(&self.paths.data_root)?;
        let now = now_ms();
        let next_run_at = if record.enabled {
            Some(next_cron_hint(&record.cron_expr, now).ok_or_else(|| {
                unsupported(
                    "settings_automation_cron",
                    "Cron 表达式没有可执行的下一次时间",
                )
            })?)
        } else {
            None
        };
        let value = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .and_then(|items| {
                items.iter_mut().find(|value| {
                    value.get("automationId").and_then(Value::as_str) == Some(record.id.as_str())
                })
            })
            .ok_or_else(|| unsupported("settings_automation_not_found", "找不到 Automation"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| unsupported("settings_automation_shape", "Automation 条目无效"))?;
        object.insert("title".to_owned(), Value::String(record.title));
        object.insert("prompt".to_owned(), Value::String(record.prompt));
        object.insert("cronExpr".to_owned(), Value::String(record.cron_expr));
        object.insert("enabled".to_owned(), Value::Bool(record.enabled));
        object.insert("recurring".to_owned(), Value::Bool(record.recurring));
        object.insert(
            "maxRuns".to_owned(),
            record.max_runs.map_or(Value::Null, |value| json!(value)),
        );
        object.insert(
            "nextRunAt".to_owned(),
            next_run_at.map_or(Value::Null, |value| json!(value)),
        );
        object.insert(
            "lastError".to_owned(),
            record.last_error.map_or(Value::Null, Value::String),
        );
        object.insert(
            "lifecycleStatus".to_owned(),
            Value::String(if record.enabled { "active" } else { "paused" }.to_owned()),
        );
        object.insert("updatedAt".to_owned(), json!(now));
        write_automation_document(&self.paths.data_root, &document)?;
        self.scheduler_wake.notify_one();
        Ok(())
    }

    fn delete_automation(&self, id: &str) -> SettingsResult<()> {
        validate_component(id, "settings_automation_id")?;
        let _guard = self
            .automation_lock
            .lock()
            .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
        let mut document = read_automation_document(&self.paths.data_root)?;
        let items = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| unsupported("settings_automation_shape", "Automation 列表无效"))?;
        let old_len = items.len();
        items.retain(|value| value.get("automationId").and_then(Value::as_str) != Some(id));
        if old_len == items.len() {
            return Err(unsupported(
                "settings_automation_not_found",
                "找不到 Automation",
            ));
        }
        if let Some(runs) = document.get_mut("runs").and_then(Value::as_array_mut) {
            runs.retain(|value| value.get("automationId").and_then(Value::as_str) != Some(id));
        }
        write_automation_document(&self.paths.data_root, &document)
    }

    fn set_automation_enabled(&self, id: &str, enabled: bool) -> SettingsResult<()> {
        validate_component(id, "settings_automation_id")?;
        let _guard = self
            .automation_lock
            .lock()
            .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
        let mut document = read_automation_document(&self.paths.data_root)?;
        let value = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .and_then(|items| {
                items
                    .iter_mut()
                    .find(|value| value.get("automationId").and_then(Value::as_str) == Some(id))
            })
            .ok_or_else(|| unsupported("settings_automation_not_found", "找不到 Automation"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| unsupported("settings_automation_shape", "Automation 条目无效"))?;
        let now = now_ms();
        if enabled {
            let cron = required_automation_string(object, "cronExpr")?;
            let next = next_cron_hint(cron, now).ok_or_else(|| {
                unsupported(
                    "settings_automation_cron",
                    "Cron 表达式没有可执行的下一次时间",
                )
            })?;
            object.insert("nextRunAt".to_owned(), json!(next));
        } else {
            object.insert("nextRunAt".to_owned(), Value::Null);
        }
        object.insert("enabled".to_owned(), Value::Bool(enabled));
        object.insert(
            "lifecycleStatus".to_owned(),
            Value::String(if enabled { "active" } else { "paused" }.to_owned()),
        );
        object.insert("updatedAt".to_owned(), json!(now));
        write_automation_document(&self.paths.data_root, &document)?;
        self.scheduler_wake.notify_one();
        Ok(())
    }

    fn run_automation(&self, id: &str) -> SettingsResult<()> {
        let job = self.claim_automation(id, false)?;
        self.spawn_automation_job(job);
        Ok(())
    }

    fn cancel_automation(&self, run_id: &str) -> SettingsResult<()> {
        validate_component(run_id, "settings_automation_run_id")?;
        let session_id = {
            let _guard = self
                .automation_lock
                .lock()
                .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
            let document = read_automation_document(&self.paths.data_root)?;
            let run = document
                .get("runs")
                .and_then(Value::as_array)
                .and_then(|runs| {
                    runs.iter()
                        .find(|value| value.get("runId").and_then(Value::as_str) == Some(run_id))
                })
                .ok_or_else(|| {
                    unsupported(
                        "settings_automation_run_not_found",
                        "找不到 Automation 运行",
                    )
                })?;
            if run.get("outcome").and_then(Value::as_str) != Some("running") {
                return Err(unsupported(
                    "settings_automation_not_running",
                    "Automation 运行已经结束",
                ));
            }
            let mut bindings = self.automation_runs.lock().map_err(|_| {
                settings_error("settings_automation_runs", "Automation 运行索引不可用")
            })?;
            let binding = bindings.entry(run_id.to_owned()).or_default();
            binding.cancel_requested = true;
            binding.session_id.clone()
        };

        if let Some(session_id) = session_id
            && self
                .runtime
                .cancel_turn(&session_id, &format!("automation-turn-{run_id}"))
                .is_err()
        {
            // Worker 可能正处于 TurnStarted 的临界区；待取消标志已登记后由 worker 再提交一次。
            tracing::warn!(run_id, "Automation 取消请求未立即提交");
        }
        let wait_result = self.executor.block_on(time::timeout(
            Duration::from_secs(10),
            wait_for_automation_terminal(
                self.paths.clone(),
                self.automation_lock.clone(),
                run_id.to_owned(),
            ),
        ));
        match wait_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(settings_error("settings_automation_cancel", error));
            }
            Err(_) => {
                return Err(NativeSettingsError::new(
                    "settings_automation_cancel_pending",
                    "Automation 取消请求已提交，但仍在等待 Runtime 终态",
                )
                .retryable(true));
            }
        }
        self.scheduler_wake.notify_one();
        Ok(())
    }

    fn claim_automation(&self, id: &str, scheduled: bool) -> SettingsResult<AutomationJob> {
        validate_component(id, "settings_automation_id")?;
        let _guard = self
            .automation_lock
            .lock()
            .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
        let mut document = read_automation_document(&self.paths.data_root)?;
        let items = document
            .get("automations")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                unsupported(
                    "settings_automation_shape",
                    "Automation 台账缺少有效的 automations 列表",
                )
            })?;
        if items.iter().any(|value| {
            value.get("automationId").and_then(Value::as_str) == Some(id)
                && value.get("dispatchStatus").and_then(Value::as_str) == Some("running")
        }) {
            return Err(unsupported(
                "settings_automation_running",
                "Automation 正在运行",
            ));
        }
        if items
            .iter()
            .filter(|value| value.get("dispatchStatus").and_then(Value::as_str) == Some("running"))
            .count()
            >= AUTOMATION_MAX_CONCURRENT
        {
            return Err(NativeSettingsError::new(
                "settings_automation_capacity",
                "后台 Automation 并发已满",
            )
            .retryable(true));
        }
        let value = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .and_then(|items| {
                items
                    .iter_mut()
                    .find(|value| value.get("automationId").and_then(Value::as_str) == Some(id))
            })
            .ok_or_else(|| unsupported("settings_automation_not_found", "找不到 Automation"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| unsupported("settings_automation_shape", "Automation 条目无效"))?;
        if scheduled && object.get("enabled").and_then(Value::as_bool) != Some(true) {
            return Err(unsupported(
                "settings_automation_disabled",
                "Automation 已暂停",
            ));
        }
        let max_runs = object.get("maxRuns").and_then(Value::as_u64);
        let run_count = object.get("runCount").and_then(Value::as_u64).unwrap_or(0);
        if max_runs.is_some_and(|max| run_count >= max) {
            object.insert("enabled".to_owned(), Value::Bool(false));
            object.insert(
                "lifecycleStatus".to_owned(),
                Value::String("completed".to_owned()),
            );
            write_automation_document(&self.paths.data_root, &document)?;
            return Err(unsupported(
                "settings_automation_limit",
                "Automation 已达到运行次数上限",
            ));
        }
        let prompt = object
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| unsupported("settings_automation_prompt", "Automation 提示词为空"))?
            .to_owned();
        let workspace = object
            .get("workspacePath")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| self.paths.data_root.clone());
        let run_id = format!(
            "automation-run-{}-{}",
            now_ms(),
            self.next_run.fetch_add(1, Ordering::AcqRel)
        );
        let now = now_ms();
        object.insert("runCount".to_owned(), json!(run_count.saturating_add(1)));
        let attempts = object
            .get("dispatchAttempts")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        object.insert(
            "dispatchAttempts".to_owned(),
            json!(attempts.saturating_add(1)),
        );
        object.insert(
            "dispatchStatus".to_owned(),
            Value::String("running".to_owned()),
        );
        object.insert("updatedAt".to_owned(), json!(now));
        if let Some(runs) = document.get_mut("runs").and_then(Value::as_array_mut) {
            runs.push(json!({
                "runId": run_id.clone(),
                "automationId": id,
                "createdAt": now,
                "updatedAt": Value::Null,
                "outcome": "running",
            }));
        }
        write_automation_document(&self.paths.data_root, &document)?;
        Ok(AutomationJob {
            automation_id: id.to_owned(),
            run_id,
            prompt,
            workspace,
        })
    }

    fn spawn_automation_job(&self, job: AutomationJob) {
        spawn_automation_task(
            &self.executor,
            self.automation_tasks.clone(),
            self.paths.clone(),
            self.runtime.clone(),
            self.automation_lock.clone(),
            self.automation_runs.clone(),
            self.scheduler_stop.clone(),
            self.scheduler_wake.clone(),
            self.settings_invalidator(),
            job,
        );
    }

    fn save_workflow(&self, record: WorkflowRecord) -> SettingsResult<()> {
        let scope = parse_workflow_scope(&record.scope)?;
        let project = self.current_project()?;
        let mut definition: crate::workflows::WorkflowDefinition =
            serde_json::from_str(&record.definition_json)
                .map_err(|error| settings_error("settings_workflow_json", error))?;
        if definition.meta.name != record.name {
            return Err(unsupported(
                "settings_workflow_name",
                "工作流名称必须与定义中的 meta.name 一致",
            ));
        }
        // 设置页的元数据字段是 typed contract 的一部分；写入前同步回定义本体，
        // 这样 description、whenToUse 和 tags 不会只停留在一次快照里。
        definition.meta.description = record.description;
        definition.meta.when_to_use = record.when_to_use;
        definition.meta.tags = record.tags;
        let store = WorkflowStore::new(self.paths.data_root.clone());
        let path = store
            .path(scope, project.as_deref(), &record.name)
            .map_err(|error| settings_error("settings_workflow_save", error))?;
        let current_revision = file_revision(&path);
        if path.is_file() {
            if record.revision != current_revision {
                return Err(unsupported(
                    "settings_workflow_conflict",
                    "工作流定义已被其他操作修改，请重新加载后再保存",
                ));
            }
        } else if record.revision != 0 {
            return Err(unsupported(
                "settings_workflow_not_found",
                "工作流定义已不存在，请重新加载后再保存",
            ));
        }
        store
            .save(scope, project.as_deref(), &definition)
            .map_err(|error| settings_error("settings_workflow_save", error))?;
        Ok(())
    }

    fn move_workflow(
        &self,
        name: &str,
        from_scope: &str,
        to_scope: &str,
        expected_revision: u64,
    ) -> SettingsResult<()> {
        validate_component(name, "settings_workflow_name")?;
        let from_scope = parse_workflow_scope(from_scope)?;
        let to_scope = parse_workflow_scope(to_scope)?;
        if from_scope == to_scope {
            return Err(unsupported(
                "settings_workflow_scope",
                "工作流源和目标作用域必须不同",
            ));
        }
        let project = self
            .current_project()?
            .ok_or_else(|| unsupported("settings_workflow_project", "当前没有可用的项目工作区"))?;
        let store = WorkflowStore::new(self.paths.data_root.clone());
        let source_path = store
            .path(
                from_scope,
                (from_scope == WorkflowScope::Project).then_some(project.as_path()),
                name,
            )
            .map_err(|error| settings_error("settings_workflow_move", error))?;
        if !source_path.is_file() {
            return Err(unsupported(
                "settings_workflow_not_found",
                "找不到待移动的工作流定义",
            ));
        }
        if file_revision(&source_path) != expected_revision {
            return Err(unsupported(
                "settings_workflow_conflict",
                "工作流定义已被其他操作修改，请重新加载后再移动",
            ));
        }
        store
            .move_between_scopes(
                from_scope,
                (from_scope == WorkflowScope::Project).then_some(project.as_path()),
                to_scope,
                (to_scope == WorkflowScope::Project).then_some(project.as_path()),
                name,
            )
            .map_err(|error| match error {
                crate::workflows::WorkflowStoreError::TargetExists(_) => unsupported(
                    "settings_workflow_conflict",
                    "目标作用域已存在同名工作流，未覆盖现有定义",
                ),
                crate::workflows::WorkflowStoreError::NotFound(_) => {
                    unsupported("settings_workflow_not_found", "找不到待移动的工作流定义")
                }
                other => settings_error("settings_workflow_move", other),
            })?;
        Ok(())
    }

    fn delete_workflow(&self, name: &str, scope: &str) -> SettingsResult<()> {
        let scope = parse_workflow_scope(scope)?;
        let project = self.current_project()?;
        WorkflowStore::new(self.paths.data_root.clone())
            .delete(scope, project.as_deref(), name)
            .map_err(|error| settings_error("settings_workflow_delete", error))?;
        Ok(())
    }

    fn run_workflow(&self, name: String, scope: String, inputs_json: String) -> SettingsResult<()> {
        let scope = parse_workflow_scope(&scope)?;
        let inputs = serde_json::from_str::<Value>(&inputs_json)
            .map_err(|error| settings_error("settings_workflow_inputs", error))?;
        let (session_id, host) = self.workflow_host_for_current()?;
        let session = self
            .runtime
            .runtime_manager()
            .get(session_id.clone())
            .map_err(|error| settings_error("settings_workflow_session", error))?;
        let snapshot = session
            .snapshot()
            .map_err(|error| settings_error("settings_session_state", error))?;
        let cwd = snapshot.state.project_root.clone();
        let plan_enabled = snapshot.state.plan.enabled;
        let provider = self
            .runtime
            .workflow_provider_snapshot_for_bound(&session_id, snapshot.state.provider.as_ref())
            .map_err(|error| settings_error("settings_workflow_model", error))?;
        // 工作流启动事实必须同时冻结 Provider 与 Plan；两者都来自当前父 Session，
        // actor/Tool 只读取该 Journal 值，不能在首次 Driver 启动后重新推断。
        let model_selection =
            Some(WorkflowModelSelection::new(provider, plan_enabled).into_value());
        // 设置页没有 Chat 输入事件可代为绑定 Native 连接；先把父 Session 绑定到
        // GPUI 的唯一 typed pending 连接，Workflow actor 才能复用同一问答账本。
        let connection = ConnectionId::new("gpui-owner")
            .map_err(|error| settings_error("settings_workflow_connection", error))?;
        self.runtime
            .elicitation_coordinator()
            .bind_native_session(&session_id, &connection)
            .map_err(|error| settings_error("settings_workflow_connection", error))?;
        self.runtime
            .permissions()
            .bind_native_session(&session_id, &connection)
            .map_err(|error| settings_error("settings_workflow_connection", error))?;
        let request = crate::workflows::WorkflowRunStartRequest {
            name,
            scope,
            project_storage: None,
            parent_session_id: session_id,
            tool_call_id: None,
            launch_input_id: None,
            inputs,
            cwd: PathBuf::from(cwd),
            model_selection,
            budgets: None,
        };
        let _entered = self.executor.enter();
        host.start(request)
            .map_err(|error| settings_error("settings_workflow_run", error))?;
        Ok(())
    }

    fn cancel_workflow(&self, run_id: &str) -> SettingsResult<()> {
        let host = self.workflow_host_for_run(run_id)?;
        self.executor
            .block_on(host.cancel(run_id))
            .map_err(|error| settings_error("settings_workflow_cancel", error))?;
        Ok(())
    }

    /// 通过 predecessor 的冻结事实创建 successor，避免设置页伪造父会话、cwd 或模型预算。
    fn amend_workflow(
        &self,
        predecessor_run_id: String,
        definition_json: String,
        inputs_json: String,
    ) -> SettingsResult<()> {
        validate_component(&predecessor_run_id, "settings_workflow_run")?;
        if definition_json.len() > crate::workflows::MAX_WORKFLOW_DEFINITION_BYTES {
            return Err(unsupported(
                "settings_workflow_json",
                "工作流定义超过大小限制",
            ));
        }
        if inputs_json.len() as u64 > MAX_SETTINGS_JSON_BYTES {
            return Err(unsupported(
                "settings_workflow_inputs",
                "工作流输入超过大小限制",
            ));
        }
        let definition = serde_json::from_str::<WorkflowDefinition>(&definition_json)
            .map_err(|error| settings_error("settings_workflow_json", error))?;
        let inputs = serde_json::from_str::<Value>(&inputs_json)
            .map_err(|error| settings_error("settings_workflow_inputs", error))?;
        let host = self.workflow_host_for_run(&predecessor_run_id)?;
        let frozen = host
            .frozen_run(&predecessor_run_id)
            .map_err(|error| settings_error("settings_workflow_frozen_run", error))?
            .ok_or_else(|| unsupported("settings_workflow_not_found", "找不到工作流运行"))?;
        let request = WorkflowRunDefinitionChangeRequest {
            predecessor_run_id,
            definition,
            parent_session_id: frozen.parent_session_id,
            // 定义修订是一次新的显式命令；不复用 predecessor 的 launchInputId，
            // 否则幂等校验会把合法 successor 误判为旧启动冲突。
            tool_call_id: None,
            launch_input_id: None,
            inputs,
            cwd: frozen.cwd,
            model_selection: frozen.model_selection,
            budgets: frozen.budgets,
            subagent_model: frozen.subagent_model,
        };
        self.executor
            .block_on(host.start_definition_change(request))
            .map_err(|error| settings_error("settings_workflow_amend", error))?;
        Ok(())
    }

    fn resume_workflow_run(&self, run_id: &str) -> SettingsResult<()> {
        let host = self.workflow_host_for_run(run_id)?;
        self.executor
            .block_on(host.resume(run_id))
            .map_err(|error| settings_error("settings_workflow_resume", error))?;
        Ok(())
    }

    fn resolve_workflow_question_run(&self, question_id: &str, answer: &str) -> SettingsResult<()> {
        validate_component(question_id, "settings_workflow_question")?;
        if answer.len() > MAX_WORKFLOW_ANSWER_BYTES {
            return Err(unsupported(
                "settings_workflow_answer",
                "工作流答案超过大小限制",
            ));
        }
        let (_, host) = self.workflow_host_for_current()?;
        self.executor
            .block_on(host.resolve_question(question_id.to_owned(), answer.to_owned()))
            .map_err(|error| settings_error("settings_workflow_question", error))?;
        Ok(())
    }

    fn workflow_host_for_run(&self, run_id: &str) -> SettingsResult<Arc<WorkflowHost>> {
        validate_component(run_id, "settings_workflow_run")?;
        let (_, host) = self.workflow_host_for_current()?;
        if host
            .run_summary(run_id)
            .map_err(|error| settings_error("settings_workflow_run", error))?
            .is_none()
        {
            return Err(unsupported(
                "settings_workflow_not_found",
                "找不到当前 Session 所属的工作流运行",
            ));
        }
        Ok(host)
    }

    fn workflow_inspection_snapshot(
        &self,
        mut inspection: WorkflowInspection,
    ) -> SettingsResult<SettingsSnapshot> {
        let base = self.workflow_inspection_base(&inspection.run_id)?;
        inspection.run = base.run;
        inspection.frozen_run = base.frozen_run;
        inspection.active_barrier = base.active_barrier;
        if inspection.error_code.is_none() {
            inspection.error_code = base.error_code;
        }
        if inspection.error_code.is_none() {
            inspection.error_code = inspection
                .progress_events
                .as_deref()
                .and_then(workflow_error_code_from_progress);
        }
        let mut snapshot = self.load_workflows()?;
        snapshot
            .workflows
            .as_mut()
            .expect("load_workflows always fills workflow settings")
            .inspection = Some(inspection);
        Ok(snapshot)
    }

    fn workflow_inspection_base(&self, run_id: &str) -> SettingsResult<WorkflowInspection> {
        let host = self.workflow_host_for_run(run_id)?;
        let summary = host
            .run_summary(run_id)
            .map_err(|error| settings_error("settings_workflow_run", error))?
            .ok_or_else(|| unsupported("settings_workflow_not_found", "找不到工作流运行"))?;
        let frozen_run = self.read_workflow_frozen_run(run_id)?;
        let status = workflow_status_name(&summary.status);
        Ok(WorkflowInspection {
            run_id: run_id.to_owned(),
            run: Some(workflow_run_inspection(&summary)),
            frozen_run,
            active_barrier: Some(WorkflowActiveBarrierInspection {
                active: matches!(
                    &summary.status,
                    WorkflowRunStatus::Pending | WorkflowRunStatus::Running
                ),
                status,
            }),
            ..WorkflowInspection::default()
        })
    }

    fn read_workflow_events(&self, run_id: &str) -> SettingsResult<WorkflowEventPage> {
        let host = self.workflow_host_for_run(run_id)?;
        host.list_events(run_id, None, crate::workflows::MAX_WORKFLOW_EVENT_LIMIT)
            .map_err(|error| settings_error("settings_workflow_events", error))
    }

    fn read_workflow_progress(
        &self,
        run_id: &str,
    ) -> SettingsResult<Vec<DynamicWorkflowRunProgressPayload>> {
        let host = self.workflow_host_for_run(run_id)?;
        host.progress_events(run_id, None, crate::workflows::MAX_WORKFLOW_EVENT_LIMIT)
            .map_err(|error| settings_error("settings_workflow_progress", error))
    }

    fn read_workflow_frozen_run(
        &self,
        run_id: &str,
    ) -> SettingsResult<Option<WorkflowFrozenRunInspection>> {
        let host = self.workflow_host_for_run(run_id)?;
        host.frozen_run(run_id)
            .map(|frozen| frozen.map(workflow_frozen_run_inspection))
            .map_err(|error| settings_error("settings_workflow_frozen_run", error))
    }

    fn read_workflow_graph(&self, run_id: &str) -> SettingsResult<WorkflowGraphResult> {
        let host = self.workflow_host_for_run(run_id)?;
        host.graph(run_id)
            .map_err(|error| settings_error("settings_workflow_graph", error))
    }

    fn read_workflow_workspace(&self, run_id: &str) -> SettingsResult<WorkflowWorkspaceResult> {
        let host = self.workflow_host_for_run(run_id)?;
        Ok(host
            .workspace(run_id)
            .map_err(|error| settings_error("settings_workflow_workspace", error))?
            .unwrap_or(WorkflowWorkspaceResult {
                nodes: Vec::new(),
                truncated: false,
            }))
    }

    fn read_workflow_node_result(
        &self,
        run_id: &str,
        site_id: &str,
        ordinal: u32,
        max_bytes: usize,
    ) -> SettingsResult<WorkflowNodeResult> {
        validate_component(site_id, "settings_workflow_site")?;
        let host = self.workflow_host_for_run(run_id)?;
        host.node_result(
            run_id,
            site_id,
            ordinal,
            max_bytes.clamp(1, MAX_WORKFLOW_NODE_RESULT_BYTES),
        )
        .map_err(|error| settings_error("settings_workflow_node_result", error))?
        .ok_or_else(|| unsupported("settings_workflow_node_not_found", "找不到工作流节点结果"))
    }

    fn list_workflow_artifacts(&self, run_id: &str) -> SettingsResult<Vec<WorkflowArtifact>> {
        let host = self.workflow_host_for_run(run_id)?;
        Ok(host
            .artifacts(run_id)
            .map_err(|error| settings_error("settings_workflow_artifacts", error))?
            .unwrap_or_default())
    }

    fn list_workflow_artifact_items(
        &self,
        run_id: &str,
        artifact_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> SettingsResult<WorkflowArtifactPage> {
        validate_component(artifact_id, "settings_workflow_artifact")?;
        let host = self.workflow_host_for_run(run_id)?;
        host.artifact_items(
            run_id,
            artifact_id,
            after_sequence,
            limit.clamp(1, crate::workflows::MAX_WORKFLOW_EVENT_LIMIT),
        )
        .map_err(|error| settings_error("settings_workflow_artifact_items", error))
    }

    fn read_workflow_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
        version: u32,
        offset: usize,
        limit: usize,
    ) -> SettingsResult<WorkflowArtifactBytes> {
        validate_component(artifact_id, "settings_workflow_artifact")?;
        if !(1..=16).contains(&version) {
            return Err(unsupported(
                "settings_workflow_artifact_version",
                "产物版本必须在 1 到 16 之间",
            ));
        }
        let host = self.workflow_host_for_run(run_id)?;
        host.read_artifact(
            run_id,
            artifact_id,
            version,
            offset,
            limit.clamp(1, MAX_WORKFLOW_ARTIFACT_CHUNK_BYTES),
        )
        .map_err(|error| settings_error("settings_workflow_artifact_read", error))?
        .ok_or_else(|| unsupported("settings_workflow_artifact_not_found", "找不到受控产物内容"))
    }
}

impl NativeSettingsRuntimePort for NativeServices {
    fn start(&self) -> SettingsResult<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Err(error) = reconcile_automation_runs(&self.paths, &self.automation_lock) {
            self.started.store(false, Ordering::Release);
            return Err(error);
        }
        let paths = self.paths.clone();
        let runtime = self.runtime.clone();
        let executor = self.executor.clone();
        let lock = self.automation_lock.clone();
        let bindings = self.automation_runs.clone();
        let automation_tasks = self.automation_tasks.clone();
        let stop = self.scheduler_stop.clone();
        let wake = self.scheduler_wake.clone();
        let settings_invalidator = self.settings_invalidator();
        let handle = self.executor.spawn(automation_scheduler_loop(
            paths,
            runtime,
            executor,
            lock,
            bindings,
            automation_tasks,
            stop,
            wake,
            settings_invalidator,
        ));
        *self
            .scheduler
            .lock()
            .map_err(|_| settings_error("settings_scheduler", "Automation scheduler 锁不可用"))? =
            Some(handle);
        Ok(())
    }

    fn shutdown(&self) {
        self.shutdown_blocking();
    }

    fn load(&self, page: SettingsPage) -> SettingsResult<SettingsSnapshot> {
        match page {
            SettingsPage::Automations => self.load_automations(),
            SettingsPage::Workflows => self.load_workflows(),
            SettingsPage::Agents => self.load_agents(),
            _ => Err(unsupported(
                "settings_page_owner",
                "该页面不属于 NativeServices",
            )),
        }
    }

    fn resume_workflow(&self, run_id: &str) -> SettingsResult<SettingsSnapshot> {
        self.resume_workflow_run(run_id)?;
        self.load_workflows()
    }

    fn resolve_workflow_question(
        &self,
        question_id: &str,
        answer: &str,
    ) -> SettingsResult<SettingsSnapshot> {
        self.resolve_workflow_question_run(question_id, answer)?;
        self.load_workflows()
    }

    fn execute(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot> {
        match command {
            SettingsCommand::CreateAutomation(record) => {
                self.create_automation(record)?;
                self.load_automations()
            }
            SettingsCommand::UpdateAutomation(record) => {
                self.update_automation(record)?;
                self.load_automations()
            }
            SettingsCommand::DeleteAutomation { automation_id } => {
                self.delete_automation(&automation_id)?;
                self.load_automations()
            }
            SettingsCommand::SetAutomationEnabled {
                automation_id,
                enabled,
            } => {
                self.set_automation_enabled(&automation_id, enabled)?;
                self.load_automations()
            }
            SettingsCommand::RunAutomation { automation_id } => {
                self.run_automation(&automation_id)?;
                self.load_automations()
            }
            SettingsCommand::CancelAutomation { run_id } => {
                self.cancel_automation(&run_id)?;
                self.load_automations()
            }
            SettingsCommand::ListAutomationHistory { automation_id } => {
                validate_component(&automation_id, "settings_automation_id")?;
                self.load_automations()
            }
            SettingsCommand::SaveWorkflow(record) => {
                self.save_workflow(record)?;
                self.load_workflows()
            }
            SettingsCommand::MoveWorkflow {
                name,
                from_scope,
                to_scope,
                expected_revision,
            } => {
                self.move_workflow(&name, &from_scope, &to_scope, expected_revision)?;
                self.load_workflows()
            }
            SettingsCommand::DeleteWorkflow { name, scope } => {
                self.delete_workflow(&name, &scope)?;
                self.load_workflows()
            }
            SettingsCommand::RunWorkflow {
                name,
                scope,
                inputs_json,
            } => {
                self.run_workflow(name, scope, inputs_json)?;
                self.load_workflows()
            }
            SettingsCommand::CancelWorkflow { run_id } => {
                self.cancel_workflow(&run_id)?;
                self.load_workflows()
            }
            SettingsCommand::AmendWorkflow {
                predecessor_run_id,
                definition_json,
                inputs_json,
            } => {
                self.amend_workflow(predecessor_run_id, definition_json, inputs_json)?;
                self.load_workflows()
            }
            SettingsCommand::ResumeWorkflow { run_id } => self.resume_workflow(&run_id),
            SettingsCommand::ResolveWorkflowQuestion {
                question_id,
                answer,
            } => self.resolve_workflow_question(&question_id, &answer),
            SettingsCommand::ListWorkflowRuns { .. } => self.load_workflows(),
            SettingsCommand::ReadWorkflowEvents { run_id } => {
                let events = self.read_workflow_events(&run_id)?;
                let progress_events = self.read_workflow_progress(&run_id)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    events: Some(events),
                    progress_events: Some(progress_events),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ReadWorkflowGraph { run_id } => {
                let graph = self.read_workflow_graph(&run_id)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    graph: Some(graph),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ReadWorkflowWorkspace { run_id } => {
                let workspace = self.read_workflow_workspace(&run_id)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    workspace: Some(workspace),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ReadWorkflowNodeResult {
                run_id,
                site_id,
                ordinal,
                max_bytes,
            } => {
                let node_result =
                    self.read_workflow_node_result(&run_id, &site_id, ordinal, max_bytes)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    node_result: Some(node_result),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ListWorkflowArtifacts { run_id } => {
                let artifacts = self.list_workflow_artifacts(&run_id)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    artifacts: Some(artifacts),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ListWorkflowArtifactItems {
                run_id,
                artifact_id,
                after_sequence,
                limit,
            } => {
                let artifact_items = self.list_workflow_artifact_items(
                    &run_id,
                    &artifact_id,
                    after_sequence,
                    limit,
                )?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    artifact_items: Some(artifact_items),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::ReadWorkflowArtifact {
                run_id,
                artifact_id,
                version,
                offset,
                limit,
            } => {
                let artifact_bytes =
                    self.read_workflow_artifact(&run_id, &artifact_id, version, offset, limit)?;
                self.workflow_inspection_snapshot(WorkflowInspection {
                    run_id,
                    artifact_bytes: Some(artifact_bytes),
                    ..WorkflowInspection::default()
                })
            }
            SettingsCommand::CreateSubagent(summary) => {
                self.create_subagent(&summary)?;
                self.invalidate_subagent_candidate()?;
                self.load_agents()
            }
            SettingsCommand::UpdateSubagent(summary) => {
                self.update_subagent(&summary)?;
                self.invalidate_subagent_candidate()?;
                self.load_agents()
            }
            SettingsCommand::DeleteSubagent { agent_id } => {
                self.delete_subagent(&agent_id)?;
                self.invalidate_subagent_candidate()?;
                self.load_agents()
            }
            SettingsCommand::SetSubagentEnabled { agent_id, enabled } => {
                self.set_subagent_enabled(&agent_id, enabled)?;
                self.invalidate_subagent_candidate()?;
                self.load_agents()
            }
            SettingsCommand::SetSubagentModel { agent_id, model } => {
                self.set_subagent_model(&agent_id, model)?;
                self.invalidate_subagent_candidate()?;
                self.load_agents()
            }
            SettingsCommand::ResumeGoal {
                goal_id,
                expected_revision,
            } => {
                let Some((session_id, session)) = self.current_session()? else {
                    return Err(unsupported(
                        "settings_goal_session",
                        "当前没有可恢复的 Session",
                    ));
                };
                let state =
                    PersistentAgentState::open_with_goal_root(session, self.runtime.storage_root())
                        .map_err(|error| settings_error("settings_goal_open", error))?;
                let snapshot = state
                    .goal_snapshot()
                    .map_err(|error| settings_error("settings_goal_read", error))?;
                if snapshot.revision != expected_revision {
                    return Err(unsupported(
                        "settings_goal_conflict",
                        "Goal 修订已变化，请重新加载",
                    ));
                }
                if snapshot.goal.as_ref().is_none_or(|goal| goal.id != goal_id) {
                    return Err(unsupported("settings_goal_id", "Goal 标识已变化"));
                }
                let operation_id = format!("settings-goal-resume-{session_id}-{expected_revision}");
                self.executor
                    .block_on(Arc::clone(&self.runtime).resume_session_goal(
                        &session_id,
                        &operation_id,
                        expected_revision,
                    ))
                    .map_err(|error| settings_error("settings_goal_resume", error))?;
                self.load_agents()
            }
            _ => Err(unsupported(
                "settings_command_owner",
                "该命令不属于 NativeServices",
            )),
        }
    }
}

#[derive(Clone, Debug)]
struct SubagentDocument {
    fields: Map<String, Value>,
    body: String,
}

#[derive(Clone, Debug)]
struct StoredSubagent {
    summary: SubagentSummary,
    source: String,
    path: PathBuf,
}

fn normalize_subagent_source(source: &str) -> SettingsResult<&'static str> {
    match source.trim() {
        "" | "global" => Ok("global"),
        "project" => Ok("project"),
        _ => Err(unsupported(
            "settings_subagent_scope",
            "Subagent 只允许 global 或 project 作用域",
        )),
    }
}

fn validate_subagent_identifier(value: &str, code: &str) -> SettingsResult<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(unsupported(code, "Subagent 标识无效"));
    }
    Ok(())
}

fn validate_subagent_name(value: &str) -> SettingsResult<()> {
    if value.is_empty()
        || value.len() > MAX_SUBAGENT_NAME_BYTES
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(unsupported(
            "settings_subagent_name",
            "Subagent 名称只能包含 ASCII 字母、数字、连字符和下划线",
        ));
    }
    Ok(())
}

fn validate_subagent_text(value: &str, maximum: usize, code: &str) -> SettingsResult<()> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
    {
        return Err(unsupported(code, "Subagent 文本无效"));
    }
    Ok(())
}

fn validate_subagent_tools(tools: &[String]) -> SettingsResult<()> {
    if tools.len() > MAX_SUBAGENT_LIST_ITEMS {
        return Err(unsupported(
            "settings_subagent_tools",
            "Subagent 工具数量过多",
        ));
    }
    let mut seen = BTreeSet::new();
    for tool in tools {
        if tool.trim().is_empty()
            || tool.len() > MAX_SUBAGENT_LIST_ITEM_BYTES
            || tool.chars().any(char::is_control)
            || !seen.insert(tool.to_ascii_lowercase())
        {
            return Err(unsupported(
                "settings_subagent_tools",
                "Subagent 工具列表无效",
            ));
        }
    }
    Ok(())
}

fn validate_optional_model_selection(model: Option<&ModelSelection>) -> SettingsResult<()> {
    let Some(model) = model else {
        return Ok(());
    };
    for (value, code) in [
        (&model.provider_id, "settings_subagent_provider"),
        (&model.model_id, "settings_subagent_model"),
    ] {
        if value.trim().is_empty()
            || value.len() > MAX_SUBAGENT_LIST_ITEM_BYTES
            || value.contains("::")
            || value.chars().any(char::is_control)
        {
            return Err(unsupported(code, "Subagent 模型标识无效"));
        }
    }
    Ok(())
}

fn validate_subagent_summary(summary: &SubagentSummary) -> SettingsResult<()> {
    validate_subagent_name(&summary.name)?;
    if !summary.source.trim().is_empty() {
        normalize_subagent_source(&summary.source)?;
    }
    if !summary.id.trim().is_empty() {
        validate_subagent_identifier(&summary.id, "settings_subagent_id")?;
    }
    validate_subagent_tools(&summary.tools)?;
    if let Some(max_turns) = summary.max_turns
        && !(1..=MAX_SUBAGENT_TURNS).contains(&max_turns)
    {
        return Err(unsupported(
            "settings_subagent_turns",
            "Subagent maxTurns 必须在 1 到 10000 之间",
        ));
    }
    if let Some(description) = summary.description.as_deref() {
        validate_subagent_text(
            description,
            MAX_SUBAGENT_DESCRIPTION_BYTES,
            "settings_subagent_description",
        )?;
    }
    validate_optional_model_selection(summary.model.as_ref())
}

fn subagent_identifier_matches(agent_id: &str, entry: &StoredSubagent) -> bool {
    entry.summary.id.eq_ignore_ascii_case(agent_id)
}

fn subagent_disabled_path(active_path: &Path) -> PathBuf {
    PathBuf::from(format!(
        "{}{}",
        active_path.display(),
        SUBAGENT_DISABLED_SUFFIX
    ))
}

fn is_disabled_subagent_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.ends_with(".md.disabled"))
}

fn subagent_name_from_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let name = file_name
        .strip_suffix(".md.disabled")
        .or_else(|| file_name.strip_suffix(".md"))?;
    (!name.is_empty()).then(|| name.to_owned())
}

fn ensure_subagent_root(root: &Path) -> SettingsResult<()> {
    fs::create_dir_all(root).map_err(|error| {
        settings_error(
            "settings_subagent_write",
            format!("创建 Subagent 目录失败：{error}"),
        )
    })?;
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        settings_error(
            "settings_subagent_write",
            format!("读取 Subagent 目录失败：{error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsupported(
            "settings_subagent_path",
            "Subagent 根目录必须是普通目录",
        ));
    }
    Ok(())
}

fn read_disabled_subagents(root: &Path) -> SettingsResult<BTreeSet<String>> {
    let path = root.join(SUBAGENT_STATE_FILE);
    let bytes = storage::read_private_bytes_bounded(&path, MAX_SETTINGS_JSON_BYTES, "Agent 设置")
        .map_err(|error| settings_error("settings_subagent_state", error))?;
    let Some(bytes) = bytes else {
        return Ok(BTreeSet::new());
    };
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|error| settings_error("settings_subagent_state", error))?;
    let object = value
        .as_object()
        .ok_or_else(|| unsupported("settings_subagent_state", "Agent 设置必须是对象"))?;
    let Some(values) = object.get("disabledAgentIds") else {
        return Ok(BTreeSet::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| unsupported("settings_subagent_state", "disabledAgentIds 必须是数组"))?;
    Ok(values
        .iter()
        .filter_map(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .collect())
}

fn subagent_state_value(root: &Path) -> SettingsResult<Value> {
    let path = root.join(SUBAGENT_STATE_FILE);
    let bytes = storage::read_private_bytes_bounded(&path, MAX_SETTINGS_JSON_BYTES, "Agent 设置")
        .map_err(|error| settings_error("settings_subagent_state", error))?;
    let Some(bytes) = bytes else {
        return Ok(json!({}));
    };
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|error| settings_error("settings_subagent_state", error))
}

fn write_subagent_state(root: &Path, value: &Value) -> SettingsResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| settings_error("settings_subagent_state", error))?;
    if bytes.len() as u64 > MAX_SETTINGS_JSON_BYTES {
        return Err(unsupported(
            "settings_subagent_state",
            "Agent 设置超过大小限制",
        ));
    }
    storage::atomic_write_private(&root.join(SUBAGENT_STATE_FILE), &bytes)
        .map_err(|error| settings_error("settings_subagent_state", error))
}

/// 禁用状态只保存当前作用域生成的 canonical ID，不接受调用方携带的历史别名。
fn canonical_subagent_id(summary: &SubagentSummary) -> SettingsResult<String> {
    let source = normalize_subagent_source(&summary.source)?;
    Ok(format!("{source}:{}", summary.name))
}

fn update_disabled_subagent_state(
    root: &Path,
    summary: &SubagentSummary,
    enabled: bool,
) -> SettingsResult<()> {
    let mut value = subagent_state_value(root)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| unsupported("settings_subagent_state", "Agent 设置必须是对象"))?;
    let values = object
        .entry("disabledAgentIds".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| unsupported("settings_subagent_state", "disabledAgentIds 必须是数组"))?;
    let canonical_id = canonical_subagent_id(summary)?;
    values.retain(|value| {
        value
            .as_str()
            .is_none_or(|stored| !stored.eq_ignore_ascii_case(&canonical_id))
    });
    if !enabled {
        values.push(Value::String(canonical_id));
    }
    let mut unique = BTreeSet::new();
    values.retain(|value| {
        value
            .as_str()
            .is_some_and(|stored| unique.insert(stored.to_ascii_lowercase()))
    });
    write_subagent_state(root, &value)
}

fn remove_disabled_subagent_state(root: &Path, summary: &SubagentSummary) -> SettingsResult<()> {
    update_disabled_subagent_state(root, summary, true)
}

fn scan_subagent_root(
    root: &Path,
    source: &str,
    disabled: &BTreeSet<String>,
    output: &mut Vec<StoredSubagent>,
) -> SettingsResult<()> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(settings_error(
                "settings_subagent_list",
                format!("读取 Subagent 目录失败：{error}"),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsupported(
            "settings_subagent_path",
            "Subagent 根目录必须是普通目录",
        ));
    }
    let mut paths = fs::read_dir(root)
        .map_err(|error| settings_error("settings_subagent_list", error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|value| value.to_str()) == Some("md")
                || is_disabled_subagent_path(path)
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > MAX_SUBAGENT_FILES {
        return Err(unsupported(
            "settings_subagent_list",
            "Subagent 文件数量超过限制",
        ));
    }
    for path in paths {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| settings_error("settings_subagent_list", error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_SUBAGENT_FILE_BYTES {
            return Err(unsupported(
                "settings_subagent_size",
                "Subagent 文件超过大小限制",
            ));
        }
        let summary = parse_subagent_summary(&path, source, disabled)?;
        output.push(StoredSubagent {
            summary,
            source: source.to_owned(),
            path,
        });
    }
    Ok(())
}

fn parse_subagent_summary(
    path: &Path,
    source: &str,
    disabled: &BTreeSet<String>,
) -> SettingsResult<SubagentSummary> {
    let document = read_subagent_document(path)?;
    let name = document
        .fields
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| subagent_name_from_path(path))
        .ok_or_else(|| unsupported("settings_subagent_name", "Subagent 文件名无效"))?;
    validate_subagent_name(&name)?;
    let description = document
        .fields
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            document
                .body
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        });
    if let Some(description) = description.as_deref() {
        validate_subagent_text(
            description,
            MAX_SUBAGENT_DESCRIPTION_BYTES,
            "settings_subagent_description",
        )?;
    }
    let model = parse_subagent_model(document.fields.get("model"))?;
    let tools = parse_subagent_list(document.fields.get("tools"), "tools")?;
    let max_turns = parse_subagent_max_turns(document.fields.get("maxTurns"))?;
    let source = normalize_subagent_source(source)?.to_owned();
    let id = format!("{source}:{name}");
    let enabled = !is_disabled_subagent_path(path) && !disabled.contains(&id.to_ascii_lowercase());
    Ok(SubagentSummary {
        id,
        name,
        source,
        enabled,
        model,
        tools,
        max_turns,
        description,
    })
}

fn parse_subagent_model(value: Option<&Value>) -> SettingsResult<Option<ModelSelection>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(unsupported(
            "settings_subagent_model",
            "Subagent model 必须是字符串",
        ));
    };
    if value.trim() == "inherit" {
        return Ok(None);
    }
    let Some((provider_id, model_id)) = value.split_once("::") else {
        return Ok(None);
    };
    let model = ModelSelection {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
    };
    validate_optional_model_selection(Some(&model))?;
    Ok(Some(model))
}

fn parse_subagent_list(value: Option<&Value>, field: &str) -> SettingsResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = match value {
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    unsupported(
                        "settings_subagent_tools",
                        format!("Subagent {field} 必须是字符串数组"),
                    )
                })
            })
            .collect::<SettingsResult<Vec<_>>>()?,
        Value::String(value) => value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => {
            return Err(unsupported(
                "settings_subagent_tools",
                format!("Subagent {field} 必须是字符串数组"),
            ));
        }
    };
    validate_subagent_tools(&values)?;
    Ok(values)
}

fn parse_subagent_max_turns(value: Option<&Value>) -> SettingsResult<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_u64() else {
        return Err(unsupported(
            "settings_subagent_turns",
            "Subagent maxTurns 必须是正整数",
        ));
    };
    let value = u32::try_from(value)
        .map_err(|_| unsupported("settings_subagent_turns", "Subagent maxTurns 超出范围"))?;
    if !(1..=MAX_SUBAGENT_TURNS).contains(&value) {
        return Err(unsupported(
            "settings_subagent_turns",
            "Subagent maxTurns 必须在 1 到 10000 之间",
        ));
    }
    Ok(Some(value))
}

fn parse_subagent_scalar(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.starts_with('[') || raw.starts_with('{') {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    } else if raw.eq_ignore_ascii_case("true") {
        Value::Bool(true)
    } else if raw.eq_ignore_ascii_case("false") {
        Value::Bool(false)
    } else if raw.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(value) = raw.parse::<u64>() {
        Value::Number(value.into())
    } else if raw.starts_with('"') {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    } else if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        Value::String(raw[1..raw.len() - 1].replace("''", "'"))
    } else {
        Value::String(raw.to_owned())
    }
}

fn read_subagent_document(path: &Path) -> SettingsResult<SubagentDocument> {
    let bytes = storage::read_private_bytes_bounded(path, MAX_SUBAGENT_FILE_BYTES, "Subagent 文件")
        .map_err(|error| settings_error("settings_subagent_read", error))?
        .ok_or_else(|| unsupported("settings_subagent_not_found", "找不到 Subagent 文件"))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| settings_error("settings_subagent_encoding", error))?;
    let mut fields = Map::new();
    let mut body = text.clone();
    let mut lines = text.split_inclusive('\n');
    let first = lines
        .next()
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n']);
    if first == "---" {
        let mut closed = false;
        let mut parsed_body = String::new();
        while let Some(line) = lines.next() {
            let value = line.trim_end_matches(['\r', '\n']);
            if value == "---" {
                closed = true;
                parsed_body = lines.collect();
                break;
            }
            if let Some((key, raw)) = value.split_once(':') {
                fields.insert(key.trim().to_owned(), parse_subagent_scalar(raw));
            }
        }
        if !closed {
            return Err(unsupported(
                "settings_subagent_read",
                "Subagent frontmatter 未闭合",
            ));
        }
        body = parsed_body;
    }
    Ok(SubagentDocument { fields, body })
}

fn write_subagent_document(
    path: &Path,
    summary: &SubagentSummary,
    existing: Option<&SubagentDocument>,
) -> SettingsResult<()> {
    validate_subagent_summary(summary)?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(unsupported(
            "settings_subagent_path",
            "Subagent 目标必须是普通文件",
        ));
    }
    let mut fields = existing
        .map(|document| document.fields.clone())
        .unwrap_or_default();
    fields.retain(|key, _| {
        matches!(
            key.as_str(),
            "name"
                | "description"
                | "model"
                | "effort"
                | "injectAgentsMd"
                | "tools"
                | "disallowedTools"
                | "maxTurns"
                | "allowedWriteDirs"
        )
    });
    let description = summary
        .description
        .as_deref()
        .unwrap_or(summary.name.as_str());
    fields.insert("name".to_owned(), Value::String(summary.name.clone()));
    fields.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    fields
        .entry("injectAgentsMd".to_owned())
        .or_insert(Value::Bool(true));
    fields.insert("tools".to_owned(), json!(summary.tools));
    fields.remove("maxTurns");
    if let Some(max_turns) = summary.max_turns {
        fields.insert("maxTurns".to_owned(), json!(max_turns));
    }
    fields.remove("model");
    fields.remove("effort");
    if let Some(model) = summary.model.as_ref() {
        fields.insert(
            "model".to_owned(),
            Value::String(format!("{}::{}", model.provider_id, model.model_id)),
        );
    }
    let body = existing
        .map(|document| document.body.clone())
        .filter(|body| !body.trim().is_empty())
        .unwrap_or_else(|| format!("{}\n", description.trim()));
    validate_subagent_text(
        &body,
        MAX_SUBAGENT_FILE_BYTES as usize,
        "settings_subagent_body",
    )?;

    let mut content = String::from("---\n");
    for key in [
        "name",
        "description",
        "injectAgentsMd",
        "model",
        "effort",
        "tools",
        "disallowedTools",
        "maxTurns",
        "allowedWriteDirs",
    ] {
        let Some(value) = fields.get(key) else {
            continue;
        };
        let value = match value {
            Value::String(value) => serde_json::to_string(value).unwrap_or_default(),
            Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::Null => continue,
        };
        content.push_str(key);
        content.push_str(": ");
        content.push_str(&value);
        content.push('\n');
    }
    content.push_str("---\n\n");
    content.push_str(body.trim_start());
    if !content.ends_with('\n') {
        content.push('\n');
    }
    if content.len() as u64 > MAX_SUBAGENT_FILE_BYTES {
        return Err(unsupported(
            "settings_subagent_size",
            "Subagent 文件超过大小限制",
        ));
    }
    storage::atomic_write_private(path, content.as_bytes())
        .map_err(|error| settings_error("settings_subagent_write", error))
}

#[derive(Clone)]
struct AutomationJob {
    automation_id: String,
    run_id: String,
    prompt: String,
    workspace: PathBuf,
}

async fn await_native_task(task: &mut JoinHandle<()>, label: &str, deadline: time::Instant) {
    if time::timeout(remaining_until(deadline), &mut *task)
        .await
        .is_err()
    {
        tracing::warn!(%label, "原生后台任务关闭等待超时，终止任务句柄");
        task.abort();
        let _ = task.await;
    }
}

async fn await_native_tasks(mut tasks: Vec<(String, JoinHandle<()>)>, deadline: time::Instant) {
    // timeout 只取消对 join_all future 的等待；先保存 AbortHandle，超时后继续
    // await 同一个 future，避免 join_all 已消费的完成句柄被二次 await。
    let abort_handles = tasks
        .iter()
        .map(|(_, task)| task.abort_handle())
        .collect::<Vec<_>>();
    let mut wait = futures::future::join_all(tasks.iter_mut().map(|(_, task)| task));
    if time::timeout(remaining_until(deadline), &mut wait)
        .await
        .is_err()
    {
        tracing::warn!("Automation 关闭等待达到共同 deadline，终止执行句柄");
        for handle in abort_handles {
            handle.abort();
        }
        let _ = wait.await;
    }
}

fn remaining_until(deadline: time::Instant) -> Duration {
    let now = time::Instant::now();
    if deadline > now {
        deadline - now
    } else {
        Duration::ZERO
    }
}

// 手动触发与 scheduler 触发必须共用同一登记入口，否则关闭时只能等待其中一类任务。
#[allow(clippy::too_many_arguments)]
fn spawn_automation_task(
    executor: &Runtime,
    tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    lock: Arc<Mutex<()>>,
    bindings: Arc<Mutex<HashMap<String, AutomationRunBinding>>>,
    stop: CancellationToken,
    wake: Arc<Notify>,
    settings_invalidator: Option<std::sync::Weak<NativeSettingsAdapter>>,
    job: AutomationJob,
) {
    let run_id = job.run_id.clone();
    let task = executor.spawn(async move {
        let result = execute_automation_job(&runtime, &bindings, &stop, &job).await;
        finish_automation_job(
            &paths,
            &lock,
            &bindings,
            &wake,
            settings_invalidator.as_ref(),
            &job,
            result,
        );
    });
    match tasks.lock() {
        Ok(mut tasks) => {
            // 完成句柄在下一次派发时清理，避免每次定时运行都增加常驻内存。
            tasks.retain(|_, task| !task.is_finished());
            tasks.insert(run_id, task);
        }
        Err(_) => {
            task.abort();
            tracing::error!("Automation 执行句柄锁不可用，已终止未受管任务");
        }
    }
}

#[derive(Default)]
struct AutomationRunBinding {
    session_id: Option<String>,
    cancel_requested: bool,
}

enum AutomationJobResult {
    Completed,
    Cancelled(String),
    Failed(String),
}

fn reconcile_automation_runs(paths: &NativePaths, lock: &Mutex<()>) -> SettingsResult<()> {
    let _guard = lock
        .lock()
        .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
    let mut document = read_automation_document(&paths.data_root)?;
    let now = now_ms();
    let interrupted_message = "Automation scheduler 在进程重启时中断";
    let mut changed = false;

    {
        let items = document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                unsupported(
                    "settings_automation_shape",
                    "Automation 台账缺少有效的 automations 列表",
                )
            })?;
        for value in items {
            let object = value
                .as_object_mut()
                .ok_or_else(|| unsupported("settings_automation_shape", "Automation 条目无效"))?;
            if object.get("dispatchStatus").and_then(Value::as_str) != Some("running") {
                continue;
            }
            required_automation_string(object, "automationId")?;
            object.insert(
                "dispatchStatus".to_owned(),
                Value::String("idle".to_owned()),
            );
            object.insert(
                "lastError".to_owned(),
                Value::String(interrupted_message.to_owned()),
            );
            object.insert("updatedAt".to_owned(), json!(now));

            let recurring = object
                .get("recurring")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !recurring {
                // 一次性任务的外部副作用无法从进程重启中推断是否完成，不能自动重放。
                object.insert("enabled".to_owned(), Value::Bool(false));
                object.insert("nextRunAt".to_owned(), Value::Null);
                object.insert(
                    "lifecycleStatus".to_owned(),
                    Value::String("interrupted".to_owned()),
                );
            } else {
                let has_future_next = object
                    .get("nextRunAt")
                    .and_then(Value::as_i64)
                    .is_some_and(|next| next > now);
                if !has_future_next {
                    let next = object
                        .get("cronExpr")
                        .and_then(Value::as_str)
                        .and_then(|cron| next_cron_hint(cron, now));
                    if let Some(next) = next {
                        object.insert("nextRunAt".to_owned(), json!(next));
                    } else {
                        object.insert("enabled".to_owned(), Value::Bool(false));
                        object.insert("nextRunAt".to_owned(), Value::Null);
                        object.insert(
                            "lifecycleStatus".to_owned(),
                            Value::String("interrupted".to_owned()),
                        );
                    }
                }
            }
            changed = true;
        }
    }

    {
        let runs = document
            .get_mut("runs")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                unsupported(
                    "settings_automation_shape",
                    "Automation 台账缺少有效的 runs 列表",
                )
            })?;
        for value in runs {
            let object = value.as_object_mut().ok_or_else(|| {
                unsupported("settings_automation_shape", "Automation 运行记录无效")
            })?;
            if object.get("outcome").and_then(Value::as_str) != Some("running") {
                continue;
            }
            object.insert(
                "outcome".to_owned(),
                Value::String("interrupted".to_owned()),
            );
            object.insert(
                "error".to_owned(),
                Value::String(interrupted_message.to_owned()),
            );
            object.insert("updatedAt".to_owned(), json!(now));
            changed = true;
        }
    }

    if changed {
        write_automation_document(&paths.data_root, &document)?;
    }
    Ok(())
}

// scheduler 的参数分别对应文件台账、共享 Runtime、执行器、并发锁、运行索引、
// 关闭令牌、唤醒通知和设置页弱引用；它们具有独立所有权与关闭语义，合并成上下文
// 结构会掩盖实际生命周期边界，因此在此处保留显式参数。
#[allow(clippy::too_many_arguments)]
async fn automation_scheduler_loop(
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    executor: Arc<Runtime>,
    lock: Arc<Mutex<()>>,
    bindings: Arc<Mutex<HashMap<String, AutomationRunBinding>>>,
    automation_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    stop: CancellationToken,
    wake: Arc<Notify>,
    settings_invalidator: Option<std::sync::Weak<NativeSettingsAdapter>>,
) {
    loop {
        let wait = match next_automation_wait(&paths, &lock) {
            Ok(wait) => wait,
            Err(error) => {
                // 台账错误不能被当成“没有计划”，否则 scheduler 会永久等待而不再
                // 响应外部唤醒；只记录稳定错误码，避免把路径或用户正文写入日志。
                tracing::error!(
                    code = %error.code,
                    retryable = error.retryable,
                    "Automation scheduler 读取台账失败，将在退避后重试"
                );
                Some(AUTOMATION_ERROR_RETRY)
            }
        };
        match wait {
            Some(delay) => {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = wake.notified() => {}
                    _ = time::sleep(delay) => {}
                }
            }
            None => {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = wake.notified() => {}
                }
            }
        }
        let due = match claim_due_automations(&paths, &lock) {
            Ok(values) => values,
            Err(error) => {
                tracing::error!(
                    code = %error.code,
                    retryable = error.retryable,
                    "Automation scheduler 认领任务失败，将在退避后重试"
                );
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = wake.notified() => {}
                    _ = time::sleep(AUTOMATION_ERROR_RETRY) => {}
                }
                continue;
            }
        };
        if !due.is_empty() {
            invalidate_automations(settings_invalidator.as_ref());
        }
        for job in due {
            spawn_automation_task(
                &executor,
                automation_tasks.clone(),
                paths.clone(),
                runtime.clone(),
                lock.clone(),
                bindings.clone(),
                stop.clone(),
                wake.clone(),
                settings_invalidator.clone(),
                job,
            );
        }
    }
}

fn claim_due_automations(
    paths: &NativePaths,
    lock: &Mutex<()>,
) -> SettingsResult<Vec<AutomationJob>> {
    let _guard = lock
        .lock()
        .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
    let mut document = read_automation_document(&paths.data_root)?;
    let now = now_ms();
    let mut jobs = Vec::new();
    let mut run_records = Vec::new();
    let running = document
        .get("automations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            unsupported(
                "settings_automation_shape",
                "Automation 台账缺少有效的 automations 列表",
            )
        })?
        .iter()
        .filter(|value| value.get("dispatchStatus").and_then(Value::as_str) == Some("running"))
        .count();
    let available = AUTOMATION_MAX_CONCURRENT.saturating_sub(running);
    if available == 0 {
        // 并发已满时保持台账原样；完成任务会通过 wake 通知下一轮认领。
        return Ok(jobs);
    }
    let mut changed = false;
    let Some(items) = document
        .get_mut("automations")
        .and_then(Value::as_array_mut)
    else {
        return Err(unsupported(
            "settings_automation_shape",
            "Automation 台账缺少有效的 automations 列表",
        ));
    };
    for value in items {
        if jobs.len() >= available {
            break;
        }
        let Some(object) = value.as_object_mut() else {
            return Err(unsupported(
                "settings_automation_shape",
                "Automation 条目无效",
            ));
        };
        if object.get("enabled").and_then(Value::as_bool) != Some(true)
            || object.get("dispatchStatus").and_then(Value::as_str) == Some("running")
            || object
                .get("nextRunAt")
                .and_then(Value::as_i64)
                .is_none_or(|next| next > now)
        {
            continue;
        }
        let id = required_automation_string(object, "automationId")?.to_owned();
        let max = object.get("maxRuns").and_then(Value::as_u64);
        let count = object.get("runCount").and_then(Value::as_u64).unwrap_or(0);
        if max.is_some_and(|max| count >= max) {
            object.insert("enabled".to_owned(), Value::Bool(false));
            object.insert(
                "lifecycleStatus".to_owned(),
                Value::String("completed".to_owned()),
            );
            changed = true;
            continue;
        }
        let prompt = required_automation_string(object, "prompt")?.to_owned();
        let workspace = object
            .get("workspacePath")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| paths.data_root.clone());
        let run_id = format!(
            "automation-run-{}-{}",
            now,
            NEXT_SCHEDULED_RUN.fetch_add(1, Ordering::AcqRel)
        );
        let recurring = object
            .get("recurring")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let next = object
            .get("cronExpr")
            .and_then(Value::as_str)
            .and_then(|cron| next_cron_hint(cron, now));
        object.insert("runCount".to_owned(), json!(count.saturating_add(1)));
        let scheduled_count = object
            .get("scheduledRunCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        object.insert(
            "scheduledRunCount".to_owned(),
            json!(scheduled_count.saturating_add(1)),
        );
        let attempts = object
            .get("dispatchAttempts")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        object.insert(
            "dispatchAttempts".to_owned(),
            json!(attempts.saturating_add(1)),
        );
        object.insert(
            "dispatchStatus".to_owned(),
            Value::String("running".to_owned()),
        );
        object.insert("updatedAt".to_owned(), json!(now));
        if recurring {
            if let Some(next) = next {
                object.insert("nextRunAt".to_owned(), json!(next));
            } else {
                // Croner 已接受语法但没有可搜索到的下一次命中；当前任务仍正常派发，
                // 完成后由台账进入 completed，避免空 nextRunAt 反复触发。
                object.insert("nextRunAt".to_owned(), Value::Null);
                object.insert("enabled".to_owned(), Value::Bool(false));
                object.insert(
                    "lifecycleStatus".to_owned(),
                    Value::String("completed".to_owned()),
                );
            }
        } else {
            object.insert("nextRunAt".to_owned(), Value::Null);
            object.insert("enabled".to_owned(), Value::Bool(false));
            object.insert(
                "lifecycleStatus".to_owned(),
                Value::String("completed".to_owned()),
            );
        }
        run_records.push(json!({
            "runId": run_id.clone(),
            "automationId": id,
            "createdAt": now,
            "updatedAt": Value::Null,
            "outcome": "running",
            "scheduled": true,
        }));
        jobs.push(AutomationJob {
            run_id,
            automation_id: id,
            prompt,
            workspace,
        });
        changed = true;
    }
    if !changed {
        return Ok(jobs);
    }
    let runs = document
        .get_mut("runs")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            unsupported(
                "settings_automation_shape",
                "Automation 台账缺少有效的 runs 列表",
            )
        })?;
    runs.extend(run_records);
    write_automation_document(&paths.data_root, &document)?;
    Ok(jobs)
}

fn next_automation_wait(paths: &NativePaths, lock: &Mutex<()>) -> SettingsResult<Option<Duration>> {
    let _guard = lock
        .lock()
        .map_err(|_| settings_error("settings_automation_lock", "Automation 锁不可用"))?;
    let document = read_automation_document(&paths.data_root)?;
    let now = now_ms();
    let automations = document
        .get("automations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            unsupported(
                "settings_automation_shape",
                "Automation 台账缺少有效的 automations 列表",
            )
        })?;
    let running = automations
        .iter()
        .filter(|value| value.get("dispatchStatus").and_then(Value::as_str) == Some("running"))
        .count();
    if running >= AUTOMATION_MAX_CONCURRENT {
        return Ok(None);
    }
    let next = automations
        .iter()
        .filter(|value| {
            value.get("enabled").and_then(Value::as_bool) == Some(true)
                && value.get("dispatchStatus").and_then(Value::as_str) != Some("running")
        })
        .filter_map(|value| value.get("nextRunAt").and_then(Value::as_i64))
        .min();
    Ok(next
        .map(|at| Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or_default())))
}

async fn execute_automation_job(
    runtime: &Arc<AgentRuntime>,
    bindings: &Mutex<HashMap<String, AutomationRunBinding>>,
    stop: &CancellationToken,
    job: &AutomationJob,
) -> AutomationJobResult {
    if stop.is_cancelled() {
        return AutomationJobResult::Failed("Automation scheduler 已停止".to_owned());
    }
    let session = match runtime.open_or_create_session(
        &job.workspace,
        None,
        &format!("automation-open-{}", job.run_id),
    ) {
        Ok(session) => session,
        Err(error) => return AutomationJobResult::Failed(redact(&error.to_string())),
    };
    let session_id = session.session_id().as_str().to_owned();
    let turn_id = format!("automation-turn-{}", job.run_id);
    let result = async {
        let cancel_requested = match bind_automation_run(bindings, &job.run_id, &session_id) {
            Ok(cancel_requested) => cancel_requested,
            Err(error) => return AutomationJobResult::Failed(error),
        };
        // Automation 没有 Chat 输入事件替它绑定 Native 连接；复用 GPUI 的唯一
        // typed pending 连接，才能让 AskUser 与权限请求进入同一原生投影账本。
        let connection = match bind_automation_interactions(runtime, &session_id) {
            Ok(connection) => connection,
            Err(error) => return AutomationJobResult::Failed(error),
        };
        let mut subscription = match session.subscribe() {
            Ok(subscription) => subscription,
            Err(error) => return AutomationJobResult::Failed(redact(&error.to_string())),
        };
        let options = RootTurnOptions {
            elicitation_connection_id: Some(connection),
            ..RootTurnOptions::default()
        };
        if let Err(error) = runtime
            .start_root_turn(&session_id, &turn_id, &job.prompt, options)
            .await
        {
            return AutomationJobResult::Failed(redact(&error.to_string()));
        }
        if cancel_requested {
            // 取消可能在 worker 建立 Session 前到达；映射建立后补发同一个真实 Turn 的取消请求。
            if runtime.cancel_turn(&session_id, &turn_id).is_err() {
                tracing::warn!(run_id = %job.run_id, "Automation 延迟取消请求未立即提交");
            }
        }
        wait_for_turn(&session, &turn_id, &mut subscription, stop).await
    }
    .await;
    // 每次 Automation 都创建临时 Session；关闭时同时清理问答/权限绑定，避免
    // 历史运行数量增长后继续占用 Runtime 的 Session 和投影映射。
    if let Err(error) = runtime.close_session(&session_id).await {
        return AutomationJobResult::Failed(redact(&error.to_string()));
    }
    result
}

fn bind_automation_interactions(
    runtime: &AgentRuntime,
    session_id: &str,
) -> Result<ConnectionId, String> {
    let connection = ConnectionId::new("gpui-owner")
        .map_err(|error| format!("Automation 原生连接标识无效：{error}"))?;
    runtime
        .elicitation_coordinator()
        .bind_native_session(session_id, &connection)
        .map_err(|error| redact(&error.to_string()))?;
    runtime
        .permissions()
        .bind_native_session(session_id, &connection)
        .map_err(|error| redact(&error.to_string()))?;
    Ok(connection)
}

fn bind_automation_run(
    bindings: &Mutex<HashMap<String, AutomationRunBinding>>,
    run_id: &str,
    session_id: &str,
) -> Result<bool, String> {
    let mut bindings = bindings
        .lock()
        .map_err(|_| "Automation 运行索引不可用".to_owned())?;
    let binding = bindings.entry(run_id.to_owned()).or_default();
    binding.session_id = Some(session_id.to_owned());
    Ok(binding.cancel_requested)
}

async fn wait_for_turn(
    session: &RuntimeSession,
    turn_id: &str,
    subscription: &mut keencode_runtime::RuntimeEventSubscription,
    stop: &CancellationToken,
) -> AutomationJobResult {
    loop {
        if stop.is_cancelled() {
            return AutomationJobResult::Failed("Automation scheduler 已停止".to_owned());
        }
        let state = match session.read_state(|state| {
            state
                .turns
                .values()
                .find(|turn| turn.turn_id.as_str() == turn_id)
                .map(|turn| (turn.status.clone(), turn.outcome_message.clone()))
        }) {
            Ok(state) => state,
            Err(error) => return AutomationJobResult::Failed(redact(&error.to_string())),
        };
        if let Some(status) = state {
            match status.0 {
                TurnStatus::Completed => return AutomationJobResult::Completed,
                TurnStatus::Failed => {
                    return AutomationJobResult::Failed(
                        status
                            .1
                            .unwrap_or_else(|| "Automation Turn 执行失败".to_owned()),
                    );
                }
                TurnStatus::Cancelled => {
                    return AutomationJobResult::Cancelled(
                        status
                            .1
                            .unwrap_or_else(|| "Automation Turn 已取消".to_owned()),
                    );
                }
                TurnStatus::Running => {}
            }
        }
        tokio::select! {
            _ = stop.cancelled() => return AutomationJobResult::Failed("Automation scheduler 已停止".to_owned()),
            result = subscription.recv() => match result {
                Ok(delivery) => {
                    if let RuntimeEventPayload::Authoritative(record) = delivery.payload {
                        match record.event {
                            SessionEvent::TurnCompleted { turn_id: ref ended } if ended.as_str() == turn_id => return AutomationJobResult::Completed,
                            SessionEvent::TurnStopped {
                                turn_id: ref ended,
                                reason: TurnStopReason::Cancelled,
                                message,
                                ..
                            } if ended.as_str() == turn_id => return AutomationJobResult::Cancelled(message),
                            SessionEvent::TurnStopped {
                                turn_id: ref ended,
                                message,
                                ..
                            } if ended.as_str() == turn_id => return AutomationJobResult::Failed(message),
                            _ => {}
                        }
                    }
                }
                Err(RuntimeEventReceiveError::Lagged(_)) => {}
                Err(RuntimeEventReceiveError::Closed) => return AutomationJobResult::Failed("Automation Runtime 事件流已关闭".to_owned()),
            }
        }
    }
}

async fn wait_for_automation_terminal(
    paths: Arc<NativePaths>,
    lock: Arc<Mutex<()>>,
    run_id: String,
) -> Result<(), String> {
    loop {
        let terminal = {
            let _guard = lock.lock().map_err(|_| "Automation 锁不可用".to_owned())?;
            let document = read_automation_document(&paths.data_root)
                .map_err(|error| redact(&error.to_string()))?;
            let run = document
                .get("runs")
                .and_then(Value::as_array)
                .and_then(|runs| {
                    runs.iter().find(|value| {
                        value.get("runId").and_then(Value::as_str) == Some(run_id.as_str())
                    })
                })
                .ok_or_else(|| "找不到 Automation 运行记录".to_owned())?;
            run.get("outcome")
                .and_then(Value::as_str)
                .filter(|outcome| *outcome != "running")
                .is_some()
        };
        if terminal {
            return Ok(());
        }
        time::sleep(Duration::from_millis(25)).await;
    }
}

struct AutomationRunBindingCleanup {
    bindings: Arc<Mutex<HashMap<String, AutomationRunBinding>>>,
    run_id: String,
}

impl Drop for AutomationRunBindingCleanup {
    fn drop(&mut self) {
        if let Ok(mut bindings) = self.bindings.lock() {
            bindings.remove(&self.run_id);
        }
    }
}

fn invalidate_automations(settings_invalidator: Option<&std::sync::Weak<NativeSettingsAdapter>>) {
    let Some(adapter) = settings_invalidator.and_then(|weak| weak.upgrade()) else {
        return;
    };
    adapter.invalidate(SettingsPage::Automations);
}

fn finish_automation_job(
    paths: &NativePaths,
    lock: &Mutex<()>,
    bindings: &Arc<Mutex<HashMap<String, AutomationRunBinding>>>,
    wake: &Notify,
    settings_invalidator: Option<&std::sync::Weak<NativeSettingsAdapter>>,
    job: &AutomationJob,
    result: AutomationJobResult,
) {
    let _binding_cleanup = AutomationRunBindingCleanup {
        bindings: bindings.clone(),
        run_id: job.run_id.clone(),
    };
    let Ok(_guard) = lock.lock() else {
        wake.notify_one();
        return;
    };
    let mut document = match read_automation_document(&paths.data_root) {
        Ok(document) => document,
        Err(error) => {
            tracing::error!(
                code = %error.code,
                retryable = error.retryable,
                "Automation 任务完成时读取台账失败，保留 running 状态等待恢复"
            );
            wake.notify_one();
            return;
        }
    };
    // 先定位两个目标记录，再进入可变写入，避免台账结构损坏时只落盘一半终态。
    let Some(run_index) = document
        .get("runs")
        .and_then(Value::as_array)
        .and_then(|runs| {
            runs.iter().position(|value| {
                value.is_object()
                    && value.get("runId").and_then(Value::as_str) == Some(job.run_id.as_str())
            })
        })
    else {
        tracing::error!(
            code = "settings_automation_shape",
            retryable = false,
            "Automation 任务完成时找不到有效的 runs 记录，放弃状态写入"
        );
        wake.notify_one();
        return;
    };
    let Some(item_index) = document
        .get("automations")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().position(|value| {
                value.is_object()
                    && value.get("automationId").and_then(Value::as_str)
                        == Some(job.automation_id.as_str())
            })
        })
    else {
        tracing::error!(
            code = "settings_automation_shape",
            retryable = false,
            "Automation 任务完成时找不到有效的 automations 记录，放弃状态写入"
        );
        wake.notify_one();
        return;
    };

    {
        let runs = match document.get_mut("runs").and_then(Value::as_array_mut) {
            Some(runs) => runs,
            None => {
                tracing::error!(
                    code = "settings_automation_shape",
                    retryable = false,
                    "Automation 任务完成时 runs 列表无效，放弃状态写入"
                );
                wake.notify_one();
                return;
            }
        };
        let run = match runs.get_mut(run_index).and_then(Value::as_object_mut) {
            Some(run) => run,
            None => {
                tracing::error!(
                    code = "settings_automation_shape",
                    retryable = false,
                    "Automation 任务完成时 runs 记录无效，放弃状态写入"
                );
                wake.notify_one();
                return;
            }
        };
        run.insert("updatedAt".to_owned(), json!(now_ms()));
        match &result {
            AutomationJobResult::Completed => {
                run.insert("outcome".to_owned(), Value::String("completed".to_owned()));
                run.insert("error".to_owned(), Value::Null);
            }
            AutomationJobResult::Cancelled(message) => {
                run.insert("outcome".to_owned(), Value::String("cancelled".to_owned()));
                run.insert("error".to_owned(), Value::String(redact(message)));
            }
            AutomationJobResult::Failed(error) => {
                run.insert("outcome".to_owned(), Value::String("failed".to_owned()));
                run.insert("error".to_owned(), Value::String(redact(error)));
            }
        }
    }

    {
        let items = match document
            .get_mut("automations")
            .and_then(Value::as_array_mut)
        {
            Some(items) => items,
            None => {
                tracing::error!(
                    code = "settings_automation_shape",
                    retryable = false,
                    "Automation 任务完成时 automations 列表无效，放弃状态写入"
                );
                wake.notify_one();
                return;
            }
        };
        let item = match items.get_mut(item_index).and_then(Value::as_object_mut) {
            Some(item) => item,
            None => {
                tracing::error!(
                    code = "settings_automation_shape",
                    retryable = false,
                    "Automation 任务完成时 automations 记录无效，放弃状态写入"
                );
                wake.notify_one();
                return;
            }
        };
        item.insert(
            "dispatchStatus".to_owned(),
            Value::String("idle".to_owned()),
        );
        item.insert("updatedAt".to_owned(), json!(now_ms()));
        match &result {
            AutomationJobResult::Completed => {
                item.insert("lastError".to_owned(), Value::Null);
            }
            AutomationJobResult::Cancelled(_) => {
                item.insert("lastError".to_owned(), Value::Null);
            }
            AutomationJobResult::Failed(error) => {
                item.insert("lastError".to_owned(), Value::String(redact(error)));
            }
        }
    }
    let persisted = match write_automation_document(&paths.data_root, &document) {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(
                code = %error.code,
                retryable = error.retryable,
                "Automation 任务完成状态写入台账失败，保留可恢复通知"
            );
            false
        }
    };
    if persisted {
        invalidate_automations(settings_invalidator);
    }
    wake.notify_one();
}

fn append_workflow_records(
    store: &WorkflowStore,
    scope: WorkflowScope,
    project: Option<&Path>,
    services: &NativeServices,
    output: &mut Vec<WorkflowRecord>,
) -> SettingsResult<()> {
    let listed = store
        .list(scope, project)
        .map_err(|error| settings_error("settings_workflow_list", error))?;
    for meta in listed.workflows {
        let definition = store
            .get(scope, project, &meta.name)
            .ok()
            .and_then(|value| value.definition);
        let definition_json = definition
            .as_ref()
            .and_then(|definition| serde_json::to_string_pretty(definition).ok())
            .unwrap_or_else(|| "{}".to_owned());
        output.push(WorkflowRecord {
            name: meta.name.clone(),
            scope: workflow_scope_name(scope).to_owned(),
            path: meta.path.to_string_lossy().into_owned(),
            revision: file_revision(&meta.path),
            description: meta.description.clone(),
            when_to_use: definition
                .as_ref()
                .and_then(|value| value.meta.when_to_use.clone()),
            tags: definition
                .as_ref()
                .map(|value| value.meta.tags.clone())
                .unwrap_or_default(),
            definition_json,
            valid: true,
            validation_error: None,
            runs: workflow_runs(services, &meta.name, scope),
        });
    }
    for invalid in listed.invalid {
        let name = invalid
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_owned();
        let definition_json = read_bounded(
            &invalid.path,
            crate::workflows::MAX_WORKFLOW_DEFINITION_BYTES as u64,
        )
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| "{}".to_owned());
        output.push(WorkflowRecord {
            name,
            scope: workflow_scope_name(scope).to_owned(),
            path: invalid.path.to_string_lossy().into_owned(),
            revision: file_revision(&invalid.path),
            description: None,
            when_to_use: None,
            tags: Vec::new(),
            definition_json,
            valid: false,
            validation_error: Some(invalid.reason),
            runs: Vec::new(),
        });
    }
    Ok(())
}

fn workflow_runs(services: &NativeServices, name: &str, scope: WorkflowScope) -> Vec<WorkflowRun> {
    let Ok(Some((session_id, session))) = services.current_session() else {
        return Vec::new();
    };
    let Ok(host) = services.workflow_host(&session_id, session) else {
        return Vec::new();
    };
    let Ok(result) = host.list_runs(
        Some(&session_id),
        Some(name),
        Some(scope),
        MAX_WORKFLOW_RUNS,
    ) else {
        return Vec::new();
    };
    result
        .runs
        .into_iter()
        .map(|run| {
            let status = serde_json::to_value(&run.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{:?}", run.status).to_ascii_lowercase());
            let finished = matches!(
                &run.status,
                WorkflowRunStatus::Completed
                    | WorkflowRunStatus::Errored
                    | WorkflowRunStatus::Stopped
            )
            .then_some(to_i64(run.updated_at));
            let run_id = run.run_id.clone();
            let events = host
                .list_events(
                    &run.run_id,
                    None,
                    crate::workflows::MAX_WORKFLOW_EVENT_LIMIT,
                )
                .map(|page| page.events)
                .unwrap_or_default();
            WorkflowRun {
                run_id,
                started_at_ms: to_i64(run.created_at),
                finished_at_ms: finished,
                status,
                event_count: u32::try_from(events.len()).unwrap_or(u32::MAX),
                artifact_count: u32::try_from(run.artifacts.len()).unwrap_or(u32::MAX),
                error: run
                    .stop_reason
                    .map(|reason| format!("{reason:?}").to_ascii_lowercase()),
                error_code: events
                    .iter()
                    .rev()
                    .find_map(|event| workflow_error_code_from_payload(&event.payload)),
                resumable: run.resumable,
            }
        })
        .collect()
}

fn workflow_status_name(status: &WorkflowRunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}").to_ascii_lowercase())
}

fn workflow_stop_reason_name(
    reason: &Option<crate::workflows::WorkflowRunStopReason>,
) -> Option<String> {
    reason.as_ref().map(|reason| {
        serde_json::to_value(reason)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{reason:?}").to_ascii_lowercase())
    })
}

fn workflow_run_inspection(summary: &WorkflowRunSummary) -> WorkflowRunInspection {
    WorkflowRunInspection {
        run_id: summary.run_id.clone(),
        status: workflow_status_name(&summary.status),
        stop_reason: workflow_stop_reason_name(&summary.stop_reason),
        created_at_ms: to_i64(summary.created_at),
        updated_at_ms: to_i64(summary.updated_at),
        canonical_hash: summary.canonical_hash.clone(),
        resumable: summary.resumable,
    }
}

fn workflow_frozen_run_inspection(frozen: FrozenWorkflowRun) -> WorkflowFrozenRunInspection {
    WorkflowFrozenRunInspection {
        canonical_hash: frozen.canonical_hash,
        input_hash: frozen.input_hash,
        parent_session_id: frozen.parent_session_id,
        tool_call_id: frozen.tool_call_id,
        launch_input_id: frozen.launch_input_id,
        cwd: frozen.cwd.to_string_lossy().into_owned(),
        predecessor_run_id: frozen.predecessor_run_id,
    }
}

fn workflow_error_code_from_progress(
    progress: &[DynamicWorkflowRunProgressPayload],
) -> Option<String> {
    progress
        .iter()
        .rev()
        .find_map(|event| workflow_error_code_from_payload(&event.payload))
}

fn workflow_error_code_from_payload(payload: &Value) -> Option<String> {
    let candidates = [
        payload.get("errorCode"),
        payload.get("error_code"),
        payload
            .get("error")
            .and_then(|error| error.get("code").or_else(|| error.get("errorCode"))),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .find(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        })
        .map(str::to_owned)
}

fn read_automation_document(root: &Path) -> SettingsResult<Value> {
    let path = root.join("automations.json");
    let Some(bytes) =
        storage::read_private_bytes_bounded(&path, MAX_SETTINGS_JSON_BYTES, "Automation 台账")
            .map_err(|error| settings_error("settings_automation_read", error))?
    else {
        return Ok(json!({"schema": AUTOMATION_SCHEMA, "automations": [], "runs": []}));
    };
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|error| settings_error("settings_automation_parse", error))?;
    let object = value
        .as_object()
        .ok_or_else(|| unsupported("settings_automation_shape", "Automation 台账顶层必须是对象"))?;
    reject_unknown_automation_fields(object, &AUTOMATION_DOCUMENT_FIELDS, "Automation 台账")?;
    if value.get("schema").and_then(Value::as_u64) != Some(AUTOMATION_SCHEMA) {
        return Err(unsupported(
            "settings_automation_schema",
            "Automation 台账版本不受支持",
        ));
    }
    let Some(automations) = value.get("automations").and_then(Value::as_array) else {
        return Err(unsupported(
            "settings_automation_shape",
            "Automation 台账缺少有效的 automations 列表",
        ));
    };
    let Some(runs) = value.get("runs").and_then(Value::as_array) else {
        return Err(unsupported(
            "settings_automation_shape",
            "Automation 台账缺少有效的 runs 列表",
        ));
    };
    for (index, item) in automations.iter().enumerate() {
        validate_automation_entry(item, index)?;
    }
    for (index, item) in runs.iter().enumerate() {
        validate_automation_run_entry(item, index)?;
    }
    Ok(value)
}

const AUTOMATION_DOCUMENT_FIELDS: [&str; 3] = ["schema", "automations", "runs"];
const AUTOMATION_ENTRY_FIELDS: [&str; 18] = [
    "automationId",
    "title",
    "prompt",
    "cronExpr",
    "workspacePath",
    "locationKind",
    "recurring",
    "maxRuns",
    "runCount",
    "scheduledRunCount",
    "enabled",
    "lifecycleStatus",
    "nextRunAt",
    "dispatchStatus",
    "dispatchAttempts",
    "lastError",
    "createdAt",
    "updatedAt",
];
const AUTOMATION_RUN_FIELDS: [&str; 7] = [
    "runId",
    "automationId",
    "createdAt",
    "updatedAt",
    "outcome",
    "error",
    "scheduled",
];

fn reject_unknown_automation_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> SettingsResult<()> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(unsupported(
            "settings_automation_shape",
            format!("{context}包含未知字段 {field}"),
        ));
    }
    Ok(())
}

fn required_automation_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> SettingsResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            unsupported(
                "settings_automation_shape",
                format!("Automation 字段 {field} 无效"),
            )
        })
}

fn optional_automation_string(object: &Map<String, Value>, field: &str) -> SettingsResult<()> {
    if object
        .get(field)
        .is_some_and(|value| !value.is_null() && value.as_str().is_none())
    {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 字段 {field} 类型无效"),
        ));
    }
    Ok(())
}

fn optional_automation_integer(object: &Map<String, Value>, field: &str) -> SettingsResult<()> {
    if object
        .get(field)
        .is_some_and(|value| !value.is_null() && value.as_i64().is_none())
    {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 字段 {field} 类型无效"),
        ));
    }
    Ok(())
}

fn optional_automation_unsigned(object: &Map<String, Value>, field: &str) -> SettingsResult<()> {
    if object
        .get(field)
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
    {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 字段 {field} 类型无效"),
        ));
    }
    Ok(())
}

fn optional_automation_bool(object: &Map<String, Value>, field: &str) -> SettingsResult<()> {
    if object
        .get(field)
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 字段 {field} 类型无效"),
        ));
    }
    Ok(())
}

fn required_automation_bool(object: &Map<String, Value>, field: &str) -> SettingsResult<()> {
    if object.get(field).and_then(Value::as_bool).is_none() {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 字段 {field} 无效"),
        ));
    }
    Ok(())
}

fn validate_automation_entry(value: &Value, index: usize) -> SettingsResult<()> {
    let object = value.as_object().ok_or_else(|| {
        unsupported(
            "settings_automation_shape",
            format!("Automation 条目 {index} 必须是对象"),
        )
    })?;
    reject_unknown_automation_fields(
        object,
        &AUTOMATION_ENTRY_FIELDS,
        &format!("Automation 条目 {index}"),
    )?;
    let id = required_automation_string(object, "automationId")?;
    validate_component(id, "settings_automation_id")?;
    let title = required_automation_string(object, "title")?;
    let prompt = required_automation_string(object, "prompt")?;
    let cron = required_automation_string(object, "cronExpr")?;
    if title.trim().is_empty() || prompt.trim().is_empty() || !valid_cron_expr(cron) {
        return Err(unsupported(
            "settings_automation_shape",
            format!("Automation 条目 {index} 的内容无效"),
        ));
    }
    required_automation_bool(object, "enabled")?;
    required_automation_bool(object, "recurring")?;
    optional_automation_unsigned(object, "maxRuns")?;
    optional_automation_integer(object, "nextRunAt")?;
    optional_automation_string(object, "lastError")?;
    required_automation_string(object, "workspacePath")?;
    optional_automation_string(object, "locationKind")?;
    optional_automation_string(object, "lifecycleStatus")?;
    optional_automation_string(object, "dispatchStatus")?;
    for field in [
        "runCount",
        "scheduledRunCount",
        "dispatchAttempts",
        "createdAt",
        "updatedAt",
    ] {
        optional_automation_unsigned(object, field)?;
    }
    Ok(())
}

fn validate_automation_run_entry(value: &Value, index: usize) -> SettingsResult<()> {
    let object = value.as_object().ok_or_else(|| {
        unsupported(
            "settings_automation_shape",
            format!("Automation 运行记录 {index} 必须是对象"),
        )
    })?;
    reject_unknown_automation_fields(
        object,
        &AUTOMATION_RUN_FIELDS,
        &format!("Automation 运行记录 {index}"),
    )?;
    required_automation_string(object, "runId")?;
    required_automation_string(object, "automationId")?;
    required_automation_string(object, "outcome")?;
    optional_automation_integer(object, "createdAt")?;
    optional_automation_integer(object, "updatedAt")?;
    optional_automation_string(object, "error")?;
    optional_automation_bool(object, "scheduled")?;
    Ok(())
}

fn write_automation_document(root: &Path, value: &Value) -> SettingsResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| settings_error("settings_automation_encode", error))?;
    if bytes.len() as u64 > MAX_SETTINGS_JSON_BYTES {
        return Err(unsupported(
            "settings_automation_size",
            "Automation 台账超过大小限制",
        ));
    }
    storage::atomic_write_private(&root.join("automations.json"), &bytes)
        .map_err(|error| settings_error("settings_automation_write", error))
}

fn automation_record(value: &Value, runs: &[Value]) -> Option<AutomationRecord> {
    let id = value.get("automationId")?.as_str()?.to_owned();
    let history = runs
        .iter()
        .filter(|run| run.get("automationId").and_then(Value::as_str) == Some(id.as_str()))
        .filter_map(automation_run)
        .collect();
    Some(AutomationRecord {
        id,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        prompt: value
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        cron_expr: value
            .get("cronExpr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        enabled: value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        recurring: value
            .get("recurring")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        max_runs: value
            .get("maxRuns")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        next_run_at_ms: value.get("nextRunAt").and_then(Value::as_i64),
        last_error: value
            .get("lastError")
            .and_then(Value::as_str)
            .map(str::to_owned),
        history,
    })
}

fn automation_run(value: &Value) -> Option<AutomationRun> {
    Some(AutomationRun {
        id: value.get("runId")?.as_str()?.to_owned(),
        started_at_ms: value
            .get("createdAt")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        finished_at_ms: value.get("updatedAt").and_then(Value::as_i64),
        status: value
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        detail: value
            .get("error")
            .or_else(|| value.get("outcome"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn validate_automation(record: &AutomationRecord) -> SettingsResult<()> {
    if record.title.trim().is_empty() || record.prompt.trim().is_empty() {
        return Err(unsupported(
            "settings_automation_input",
            "Automation 标题和提示词不能为空",
        ));
    }
    if !valid_cron_expr(&record.cron_expr) {
        return Err(unsupported(
            "settings_automation_cron",
            "Cron 表达式格式无效",
        ));
    }
    if record.max_runs == Some(0) {
        return Err(unsupported(
            "settings_automation_runs",
            "maxRuns 必须大于零",
        ));
    }
    Ok(())
}

fn parse_cron_expr(value: &str) -> Result<Cron, croner::errors::CronError> {
    // Automation 合同固定为五字段分钟级 cron；禁止隐式秒和年份字段，避免 UI 输入被
    // Croner 当成另一种精度解释。Croner 的月份/星期别名、范围、列表和步长由同一解析器
    // 统一校验，next occurrence 也复用其 DOM/DOW 与 DST 语义。
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(value)
}

fn valid_cron_expr(value: &str) -> bool {
    parse_cron_expr(value).is_ok()
}

fn parse_workflow_scope(value: &str) -> SettingsResult<WorkflowScope> {
    match value {
        "global" => Ok(WorkflowScope::Global),
        "project" => Ok(WorkflowScope::Project),
        _ => Err(unsupported("settings_workflow_scope", "工作流作用域无效")),
    }
}

fn workflow_scope_name(scope: WorkflowScope) -> &'static str {
    match scope {
        WorkflowScope::Global => "global",
        WorkflowScope::Project => "project",
    }
}

fn validate_component(value: &str, code: &str) -> SettingsResult<()> {
    if value.trim().is_empty()
        || value != value.trim()
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(unsupported(code, "标识无效"));
    }
    Ok(())
}

fn next_cron_hint(cron: &str, now: i64) -> Option<i64> {
    let schedule = parse_cron_expr(cron).ok()?;
    let local_now = Local.timestamp_millis_opt(now).single()?;
    schedule
        .find_next_occurrence(&local_now, false)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    storage::read_private_bytes_bounded(path, max_bytes, "Workflow 定义")
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "文件不存在".to_owned())
}

fn file_revision(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn settings_error(code: &str, error: impl std::fmt::Display) -> NativeSettingsError {
    NativeSettingsError::new(code, redact(&error.to_string()))
}

fn unsupported(code: &str, message: impl Into<String>) -> NativeSettingsError {
    NativeSettingsError::new(code, message)
}

fn redact(value: &str) -> String {
    keencode_model::redact_error_secrets_bounded(value, 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::{BTreeSet, HashMap};
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::{TempDir, tempdir};
    use tokio::time::timeout;

    fn fixture_paths() -> (TempDir, NativePaths) {
        let directory = tempdir().expect("应创建 Automation 临时目录");
        let paths = NativePaths::from_data_root(directory.path().to_path_buf());
        (directory, paths)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_native_tasks_reaps_completed_and_pending_handles_after_deadline() {
        let completed = tokio::spawn(async {});
        let pending = tokio::spawn(std::future::pending::<()>());
        let pending_abort = pending.abort_handle();
        tokio::task::yield_now().await;
        assert!(completed.is_finished());

        await_native_tasks(
            vec![
                ("completed".to_owned(), completed),
                ("pending".to_owned(), pending),
            ],
            time::Instant::now() + Duration::from_millis(20),
        )
        .await;

        assert!(pending_abort.is_finished());
    }

    struct AutomationFixture<'a> {
        id: &'a str,
        cron: &'a str,
        enabled: bool,
        recurring: bool,
        max_runs: Option<u64>,
        run_count: u64,
        next_run_at: Option<i64>,
        dispatch_status: &'a str,
    }

    fn automation_value(fixture: AutomationFixture<'_>) -> Value {
        let AutomationFixture {
            id,
            cron,
            enabled,
            recurring,
            max_runs,
            run_count,
            next_run_at,
            dispatch_status,
        } = fixture;
        json!({
            "automationId": id,
            "title": format!("任务 {id}"),
            "prompt": "执行离线回归",
            "cronExpr": cron,
            "workspacePath": "offline",
            "locationKind": "local",
            "recurring": recurring,
            "maxRuns": max_runs,
            "runCount": run_count,
            "scheduledRunCount": 0,
            "enabled": enabled,
            "lifecycleStatus": if enabled { "active" } else { "paused" },
            "nextRunAt": next_run_at,
            "dispatchStatus": dispatch_status,
            "dispatchAttempts": 0,
            "lastError": Value::Null,
            "createdAt": 1,
            "updatedAt": 1,
        })
    }

    fn run_value(run_id: &str, automation_id: &str, outcome: &str) -> Value {
        json!({
            "runId": run_id,
            "automationId": automation_id,
            "createdAt": 1,
            "updatedAt": Value::Null,
            "outcome": outcome,
            "scheduled": true,
        })
    }

    fn document(automations: Vec<Value>, runs: Vec<Value>) -> Value {
        json!({
            "schema": AUTOMATION_SCHEMA,
            "automations": automations,
            "runs": runs,
        })
    }

    fn write_fixture(paths: &NativePaths, value: &Value) {
        write_automation_document(&paths.data_root, value).expect("应写入 Automation 夹具");
    }

    #[test]
    fn five_field_cron_uses_the_real_next_occurrence() {
        let now = Local
            .with_ymd_and_hms(2026, 1, 15, 12, 34, 0)
            .single()
            .expect("测试时间应无歧义")
            .timestamp_millis();
        let expected = Local
            .with_ymd_and_hms(2026, 1, 16, 0, 0, 0)
            .single()
            .expect("预期时间应无歧义")
            .timestamp_millis();

        assert_eq!(next_cron_hint("0 0 * * *", now), Some(expected));
        assert_ne!(next_cron_hint("0 0 * * *", now), Some(now + 60_000));
        assert!(valid_cron_expr("*/15 9-17 * * 1-5"));
        assert!(!valid_cron_expr("0 0 0 * * *"));
    }

    #[test]
    fn claim_due_respects_disabled_recurring_limit_and_future_schedule() {
        let (_directory, paths) = fixture_paths();
        let now = now_ms();
        let document = document(
            vec![
                automation_value(AutomationFixture {
                    id: "disabled",
                    cron: "0 0 * * *",
                    enabled: false,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "idle",
                }),
                automation_value(AutomationFixture {
                    id: "recurring",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "idle",
                }),
                automation_value(AutomationFixture {
                    id: "capped",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: Some(1),
                    run_count: 1,
                    next_run_at: Some(now - 1),
                    dispatch_status: "idle",
                }),
                automation_value(AutomationFixture {
                    id: "future",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now + 60 * 60 * 1_000),
                    dispatch_status: "idle",
                }),
                automation_value(AutomationFixture {
                    id: "one-shot",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: false,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "idle",
                }),
            ],
            Vec::new(),
        );
        write_fixture(&paths, &document);

        let jobs = claim_due_automations(&paths, &Mutex::new(())).expect("到期任务应可认领");
        assert_eq!(
            jobs.iter()
                .map(|job| job.automation_id.as_str())
                .collect::<Vec<_>>(),
            ["recurring", "one-shot"]
        );

        let saved = read_automation_document(&paths.data_root).expect("应读取更新后的台账");
        let items = saved["automations"].as_array().expect("任务列表应存在");
        let find = |id: &str| {
            items
                .iter()
                .find(|item| item["automationId"] == id)
                .expect("应找到测试任务")
        };
        let recurring = find("recurring");
        assert_eq!(recurring["dispatchStatus"], "running");
        assert!(
            recurring["nextRunAt"]
                .as_i64()
                .is_some_and(|next| next > now)
        );
        let one_shot = find("one-shot");
        assert_eq!(one_shot["enabled"], false);
        assert!(one_shot["nextRunAt"].is_null());
        assert_eq!(one_shot["lifecycleStatus"], "completed");
        assert_eq!(find("disabled")["dispatchStatus"], "idle");
        assert_eq!(find("future")["nextRunAt"], json!(now + 60 * 60 * 1_000));
        assert_eq!(find("capped")["lifecycleStatus"], "completed");
        assert_eq!(find("capped")["enabled"], false);
        assert_eq!(saved["runs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn full_concurrency_waits_without_claiming_or_rewriting_the_store() {
        let (_directory, paths) = fixture_paths();
        let now = now_ms();
        let document = document(
            vec![
                automation_value(AutomationFixture {
                    id: "running-a",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now + 60_000),
                    dispatch_status: "running",
                }),
                automation_value(AutomationFixture {
                    id: "running-b",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now + 60_000),
                    dispatch_status: "running",
                }),
                automation_value(AutomationFixture {
                    id: "pending",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "idle",
                }),
            ],
            vec![
                run_value("run-a", "running-a", "running"),
                run_value("run-b", "running-b", "running"),
            ],
        );
        write_fixture(&paths, &document);
        let before = fs::read(paths.data_root.join("automations.json")).expect("应读取原始台账");

        let jobs = claim_due_automations(&paths, &Mutex::new(())).expect("满并发应正常等待");
        assert!(jobs.is_empty());
        assert_eq!(
            next_automation_wait(&paths, &Mutex::new(())).expect("应读取等待状态"),
            None
        );
        let after = fs::read(paths.data_root.join("automations.json")).expect("应读取台账");
        assert_eq!(before, after, "满并发时不应写入 pending 或运行记录");
    }

    #[test]
    fn cold_recovery_interrupts_running_jobs_and_preserves_future_recurring_work() {
        let (_directory, paths) = fixture_paths();
        let now = now_ms();
        let document = document(
            vec![
                automation_value(AutomationFixture {
                    id: "recurring-cold",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: true,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "running",
                }),
                automation_value(AutomationFixture {
                    id: "one-shot-cold",
                    cron: "*/5 * * * *",
                    enabled: true,
                    recurring: false,
                    max_runs: None,
                    run_count: 0,
                    next_run_at: Some(now - 1),
                    dispatch_status: "running",
                }),
            ],
            vec![
                run_value("cold-recurring-run", "recurring-cold", "running"),
                run_value("cold-one-shot-run", "one-shot-cold", "running"),
            ],
        );
        write_fixture(&paths, &document);

        reconcile_automation_runs(&paths, &Mutex::new(())).expect("冷恢复应完成运行对账");
        let saved = read_automation_document(&paths.data_root).expect("应读取恢复后的台账");
        let items = saved["automations"].as_array().expect("任务列表应存在");
        let recurring = items
            .iter()
            .find(|item| item["automationId"] == "recurring-cold")
            .expect("应找到 recurring 任务");
        assert_eq!(recurring["dispatchStatus"], "idle");
        assert!(
            recurring["nextRunAt"]
                .as_i64()
                .is_some_and(|next| next > now)
        );
        let one_shot = items
            .iter()
            .find(|item| item["automationId"] == "one-shot-cold")
            .expect("应找到一次性任务");
        assert_eq!(one_shot["enabled"], false);
        assert_eq!(one_shot["nextRunAt"], Value::Null);
        assert_eq!(one_shot["lifecycleStatus"], "interrupted");
        assert!(
            saved["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|run| { run["outcome"] == "interrupted" && run["error"].as_str().is_some() })
        );
    }

    #[tokio::test]
    async fn finishing_a_job_writes_terminal_state_and_wakes_scheduler() {
        let (_directory, paths) = fixture_paths();
        let document = document(
            vec![automation_value(AutomationFixture {
                id: "finish",
                cron: "*/5 * * * *",
                enabled: true,
                recurring: true,
                max_runs: None,
                run_count: 1,
                next_run_at: Some(now_ms() + 60_000),
                dispatch_status: "running",
            })],
            vec![run_value("finish-run", "finish", "running")],
        );
        write_fixture(&paths, &document);
        let wake = Notify::new();
        let notified = wake.notified();
        let job = AutomationJob {
            automation_id: "finish".to_owned(),
            run_id: "finish-run".to_owned(),
            prompt: "执行离线回归".to_owned(),
            workspace: paths.data_root.clone(),
        };

        let bindings = Arc::new(Mutex::new(HashMap::new()));
        finish_automation_job(
            &paths,
            &Mutex::new(()),
            &bindings,
            &wake,
            None,
            &job,
            AutomationJobResult::Completed,
        );
        timeout(Duration::from_millis(100), notified)
            .await
            .expect("完成任务应唤醒 scheduler");

        let saved = read_automation_document(&paths.data_root).expect("应读取完成台账");
        assert_eq!(saved["automations"][0]["dispatchStatus"], "idle");
        assert!(saved["automations"][0]["lastError"].is_null());
        assert_eq!(saved["runs"][0]["outcome"], "completed");
        assert!(saved["runs"][0]["error"].is_null());
    }

    #[test]
    fn automation_document_rejects_legacy_or_unknown_fields() {
        let (_directory, paths) = fixture_paths();
        let mut entry_with_unknown = automation_value(AutomationFixture {
            id: "unknown-entry",
            cron: "0 0 * * *",
            enabled: false,
            recurring: true,
            max_runs: None,
            run_count: 0,
            next_run_at: None,
            dispatch_status: "idle",
        });
        entry_with_unknown
            .as_object_mut()
            .unwrap()
            .insert("legacyRpcField".to_owned(), json!(true));
        let mut run_with_unknown = run_value("unknown-run", "unknown-entry", "completed");
        run_with_unknown
            .as_object_mut()
            .unwrap()
            .insert("trigger".to_owned(), json!("schedule"));
        let cases = [
            // schema 1 的旧台账不迁移，必须由当前严格读取路径拒绝。
            (
                json!({
                    "schema": 1,
                    "automations": [],
                    "runs": [],
                }),
                "settings_automation_schema",
            ),
            (
                json!({
                    "schema": AUTOMATION_SCHEMA,
                    "automations": [],
                    "runs": [],
                    "legacyRpcStore": true,
                }),
                "settings_automation_shape",
            ),
            (
                document(vec![entry_with_unknown], Vec::new()),
                "settings_automation_shape",
            ),
            (
                document(
                    vec![automation_value(AutomationFixture {
                        id: "unknown-entry",
                        cron: "0 0 * * *",
                        enabled: false,
                        recurring: true,
                        max_runs: None,
                        run_count: 0,
                        next_run_at: None,
                        dispatch_status: "idle",
                    })],
                    vec![run_with_unknown],
                ),
                "settings_automation_shape",
            ),
        ];

        for (value, expected_code) in cases {
            write_fixture(&paths, &value);
            let error =
                read_automation_document(&paths.data_root).expect_err("旧版本或未知字段必须拒绝");
            assert_eq!(error.code, expected_code);
        }
    }

    fn subagent_fixture(enabled: bool, model: Option<ModelSelection>) -> SubagentSummary {
        SubagentSummary {
            id: "global:reviewer".to_owned(),
            name: "reviewer".to_owned(),
            source: "global".to_owned(),
            enabled,
            model,
            tools: vec!["Read".to_owned(), "Grep".to_owned()],
            max_turns: Some(8),
            description: Some("Review the change".to_owned()),
        }
    }

    #[test]
    fn subagent_document_round_trip_preserves_model_tools_and_limits() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("reviewer.md");
        let model = Some(ModelSelection {
            provider_id: "provider".to_owned(),
            model_id: "model".to_owned(),
        });
        let summary = subagent_fixture(true, model.clone());

        write_subagent_document(&path, &summary, None).expect("应写入 Subagent 文件");
        let disabled = BTreeSet::new();
        let loaded = parse_subagent_summary(&path, "global", &disabled)
            .expect("应读取已写入的 Subagent 文件");
        assert_eq!(loaded.name, summary.name);
        assert_eq!(loaded.tools, summary.tools);
        assert_eq!(loaded.max_turns, summary.max_turns);
        assert_eq!(
            loaded
                .model
                .as_ref()
                .map(|model| (&model.provider_id, &model.model_id)),
            model
                .as_ref()
                .map(|model| (&model.provider_id, &model.model_id))
        );
        assert!(loaded.enabled);
    }

    #[test]
    fn subagent_model_clear_removes_file_override_and_keeps_prompt() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("reviewer.md");
        let summary = subagent_fixture(
            true,
            Some(ModelSelection {
                provider_id: "provider".to_owned(),
                model_id: "model".to_owned(),
            }),
        );
        write_subagent_document(&path, &summary, None).unwrap();
        let document = read_subagent_document(&path).unwrap();

        let mut cleared = summary.clone();
        cleared.model = None;
        write_subagent_document(&path, &cleared, Some(&document)).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("model:"));
        assert!(content.contains("Review the change"));
        assert!(
            parse_subagent_summary(&path, "global", &BTreeSet::new())
                .unwrap()
                .model
                .is_none()
        );
    }

    #[test]
    fn disabled_subagent_state_and_file_keep_disabled_semantics_after_reload() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("agents");
        fs::create_dir_all(&root).unwrap();
        let active = root.join("reviewer.md");
        let summary = subagent_fixture(true, None);
        write_subagent_document(&active, &summary, None).unwrap();

        update_disabled_subagent_state(temporary.path(), &summary, false).unwrap();
        let disabled = read_disabled_subagents(temporary.path()).unwrap();
        assert!(disabled.contains("global:reviewer"));
        let loaded = parse_subagent_summary(&active, "global", &disabled).unwrap();
        assert!(!loaded.enabled);

        let inactive = subagent_disabled_path(&active);
        fs::rename(&active, &inactive).unwrap();
        let loaded_from_disabled_file =
            parse_subagent_summary(&inactive, "global", &disabled).unwrap();
        assert!(!loaded_from_disabled_file.enabled);
        remove_disabled_subagent_state(temporary.path(), &summary).unwrap();
        assert!(
            read_disabled_subagents(temporary.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn subagent_identifier_resolution_requires_canonical_ids() {
        let global_entry = StoredSubagent {
            summary: subagent_fixture(true, None),
            source: "global".to_owned(),
            path: PathBuf::from("reviewer.md"),
        };
        assert!(subagent_identifier_matches(
            "global:reviewer",
            &global_entry
        ));
        assert!(!subagent_identifier_matches(
            "user:user:reviewer",
            &global_entry
        ));
        assert!(!subagent_identifier_matches(
            "user:workspace:reviewer",
            &global_entry
        ));
        assert!(!subagent_identifier_matches("reviewer", &global_entry));
        assert!(!subagent_identifier_matches("global:other", &global_entry));
    }

    #[test]
    fn subagent_scopes_and_ids_are_canonical() {
        assert_eq!(normalize_subagent_source("global").unwrap(), "global");
        assert_eq!(normalize_subagent_source("project").unwrap(), "project");
        for legacy_source in ["user", "user:user", "workspace"] {
            assert!(normalize_subagent_source(legacy_source).is_err());
        }

        let global_entry = StoredSubagent {
            summary: subagent_fixture(true, None),
            source: "global".to_owned(),
            path: PathBuf::from("global-reviewer.md"),
        };
        let mut project_summary = subagent_fixture(true, None);
        project_summary.id = "project:reviewer".to_owned();
        project_summary.source = "project".to_owned();
        let project_entry = StoredSubagent {
            summary: project_summary,
            source: "project".to_owned(),
            path: PathBuf::from("project-reviewer.md"),
        };
        assert!(subagent_identifier_matches(
            "global:reviewer",
            &global_entry
        ));
        assert!(!subagent_identifier_matches(
            "project:reviewer",
            &global_entry
        ));
        assert!(subagent_identifier_matches(
            "project:reviewer",
            &project_entry
        ));
        assert!(!subagent_identifier_matches(
            "global:reviewer",
            &project_entry
        ));
    }
}
