//! Todo、Goal 与 Plan 的生产持久化控制器。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use keencode_agent::{
    GoalChange, GoalChangeKind, GoalController, GoalDraft, GoalPatch,
    GoalRecord as AgentGoalRecord, GoalSnapshot as AgentGoalSnapshot,
    GoalStatus as AgentGoalStatus, GoalTransition, GoalUsageDelta, MAX_PLAN_CONTENT_CHARS,
    PlanChange, PlanController, PlanSnapshot, RuntimeStateError, TodoChange, TodoController,
    TodoItem as AgentTodoItem, TodoSnapshot as AgentTodoSnapshot, TodoStatus as AgentTodoStatus,
};
use keencode_resources::{
    AgentId as ResourceAgentId, DocumentOperationOutcome, GoalDocument, GoalFileStore,
    GoalRecord as ResourceGoalRecord, GoalSnapshot as ResourceGoalSnapshot,
    GoalStatus as ResourceGoalStatus, PlanDocument, PlanFileStore, PlanState, ResourceError,
    ScopeId, SessionEvent, SessionId as ResourceSessionId, TodoItem as ResourceTodoItem,
    TodoSnapshot as ResourceTodoSnapshot, TodoStatus as ResourceTodoStatus, project_scope_id,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    RuntimeError, RuntimeSession, RuntimeSessionLifecycle, canonical_sha256,
    commit_runtime_lifecycle_event, runtime_control_event_id, validate_control_operation_id,
};

/// 文件比较交换在持续竞争下允许自动重新读取的最大次数。
const MAX_DOCUMENT_CAS_ATTEMPTS: usize = 16;

/// 绑定一个权威 Session、项目 Goal 与计划沙箱的生产状态控制器。
pub struct PersistentAgentState {
    /// Todo 权威事件所属的共享 Session Runtime。
    session: RuntimeSession,
    /// 应用数据根下唯一的项目 Goal 文件存储。
    goal_store: GoalFileStore,
    /// 应用数据根下按项目、Session 与 Agent 隔离的计划文件存储。
    plan_store: PlanFileStore,
    /// 从 Session 绑定项目根派生且不暴露原路径的稳定作用域。
    project_scope: ScopeId,
}

impl PersistentAgentState {
    /// 从 Session 自身的应用数据根与权威项目根创建生产状态控制器。
    pub fn open(session: RuntimeSession) -> Result<Self, RuntimeError> {
        let snapshot = session.snapshot()?;
        let project_scope = project_scope_id(Path::new(&snapshot.state.project_root))?;
        let storage_root = session.inner.config.storage_root.clone();
        Ok(Self {
            session,
            goal_store: GoalFileStore::open(&storage_root)?,
            plan_store: PlanFileStore::open(&storage_root)?,
            project_scope,
        })
    }

    /// 返回供桌面 ACP Goal 投影复用的稳定项目作用域。
    pub fn project_scope(&self) -> &ScopeId {
        &self.project_scope
    }

    /// 读取项目 Goal 完整文档；不存在时返回 `None`。
    fn read_goal(&self) -> Result<Option<GoalDocument>, RuntimeStateError> {
        self.goal_store
            .read(&self.project_scope)
            .map_err(storage_error)
    }

    /// 把 Goal 候选及其幂等操作载荷通过文件锁内 CAS 保存。
    fn save_goal<P: Serialize + ?Sized>(
        &self,
        operation_id: &str,
        operation: &P,
        expected_revision: u64,
        goal: Option<ResourceGoalRecord>,
        retired_goal_ids: Vec<String>,
    ) -> Result<DocumentOperationOutcome<GoalDocument>, ResourceError> {
        self.goal_store.compare_and_swap(
            operation_id,
            operation,
            expected_revision,
            GoalDocument::from_snapshot(
                self.project_scope.clone(),
                ResourceGoalSnapshot {
                    revision: expected_revision,
                    goal,
                    retired_goal_ids,
                },
            ),
        )
    }

    /// 校验并转换 Plan 请求中的 Session 与 Agent 身份。
    fn plan_identity(
        &self,
        session_id: &keencode_agent::SessionId,
        agent_id: &keencode_agent::AgentId,
    ) -> Result<(ResourceSessionId, ResourceAgentId), RuntimeStateError> {
        if session_id.as_str() != self.session.session_id().as_str() {
            return Err(RuntimeStateError::invalid(
                "Plan Session 与当前持久控制器不一致",
            ));
        }
        let session_id = ResourceSessionId::new(session_id.as_str()).map_err(identifier_error)?;
        let agent_id = ResourceAgentId::new(agent_id.as_str()).map_err(identifier_error)?;
        Ok((session_id, agent_id))
    }

    /// 读取一个隔离计划文档并转换为运行时快照。
    fn read_plan(
        &self,
        session_id: &ResourceSessionId,
        agent_id: &ResourceAgentId,
    ) -> Result<(PlanSnapshot, Option<PlanDocument>), RuntimeStateError> {
        let document = self
            .plan_store
            .read(&self.project_scope, session_id, agent_id)
            .map_err(storage_error)?;
        let snapshot = document
            .as_ref()
            .map_or_else(PlanSnapshot::default, |document| PlanSnapshot {
                revision: document.revision,
                content: document.content.clone(),
                updated_at_unix_ms: document.updated_at_unix_ms,
            });
        Ok((snapshot, document))
    }
}

impl TodoController for PersistentAgentState {
    /// 从 Session Journal 归约状态读取当前唯一 Todo 快照。
    fn todo_snapshot(&self) -> Result<AgentTodoSnapshot, RuntimeStateError> {
        self.session
            .snapshot()
            .map(|snapshot| agent_todo_snapshot(&snapshot.state.todos))
            .map_err(runtime_state_error)
    }

    /// 使用工具调用派生的操作标识原子提交 TodoReplaced 权威事件。
    fn replace_todos(
        &self,
        operation_id: &str,
        items: Vec<AgentTodoItem>,
    ) -> Result<TodoChange, RuntimeStateError> {
        let submitted = normalize_todos(items)?;
        let stored = if !submitted.is_empty()
            && submitted
                .iter()
                .all(|item| item.status == AgentTodoStatus::Completed)
        {
            Vec::new()
        } else {
            submitted.clone()
        };
        let operation_payload_sha256 =
            canonical_sha256(&("todo_replace_v1", &submitted)).map_err(runtime_state_error)?;
        let stored = stored.iter().map(resource_todo_item).collect();
        let (previous, current) = self
            .session
            .replace_todos_authoritative(operation_id, operation_payload_sha256, stored)
            .map_err(runtime_state_error)?;
        Ok(TodoChange {
            previous: agent_todo_snapshot(&previous),
            submitted,
            current: agent_todo_snapshot(&current),
            changed: previous != current,
        })
    }
}

impl GoalController for PersistentAgentState {
    /// 从项目级 GoalFileStore 读取当前完整快照。
    fn goal_snapshot(&self) -> Result<AgentGoalSnapshot, RuntimeStateError> {
        let document = self.read_goal()?;
        Ok(
            document.map_or_else(AgentGoalSnapshot::default, |document| {
                agent_goal_snapshot(document.revision, document.goal)
            }),
        )
    }

    /// 在项目没有当前 Goal 时创建 UUID v7 标识的新 Goal。
    fn create_goal(
        &self,
        operation_id: &str,
        draft: GoalDraft,
    ) -> Result<GoalChange, RuntimeStateError> {
        let draft = draft.normalized()?;
        let operation = ("goal_create_v1", &draft);
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let document = self.read_goal()?;
            if let Some(change) = deduplicated_goal_change(
                document.as_ref(),
                operation_id,
                &operation,
                GoalChangeKind::Created,
            )? {
                return Ok(change);
            }
            let revision = document.as_ref().map_or(0, |document| document.revision);
            let current = document
                .as_ref()
                .and_then(|document| document.goal.as_ref());
            if current.is_some() {
                return Err(RuntimeStateError::Conflict {
                    message: "项目已有 Goal；请先更新或清除当前 Goal".to_owned(),
                });
            }
            let retired_goal_ids = document
                .as_ref()
                .map_or_else(Vec::new, |document| document.retired_goal_ids.clone());
            let now = unix_time_ms()?;
            let goal = ResourceGoalRecord {
                id: Uuid::now_v7().to_string(),
                title: draft.title.clone(),
                scope: "project".to_owned(),
                status: ResourceGoalStatus::Active,
                description: draft.description.clone(),
                progress_percent: draft.progress_percent,
                objective: draft.objective.clone(),
                token_budget: draft.token_budget,
                tokens_used: 0,
                time_used_seconds: 0,
                blocked_reason: None,
                completion_evidence: None,
                created_at_unix_ms: now,
                updated_at_unix_ms: now,
            };
            match self.save_goal(
                operation_id,
                &operation,
                revision,
                Some(goal),
                retired_goal_ids,
            ) {
                Ok(outcome) => {
                    return Ok(goal_change_from_outcome(
                        GoalChangeKind::Created,
                        outcome,
                        true,
                    ));
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Goal 创建"))
    }

    /// 对文件锁内最新的活跃 Goal 应用一次规范化字段补丁。
    fn update_goal(
        &self,
        operation_id: &str,
        patch: GoalPatch,
    ) -> Result<GoalChange, RuntimeStateError> {
        let patch = patch.normalized()?;
        let operation = ("goal_update_v1", &patch);
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let document = self.read_goal()?;
            if let Some(change) = deduplicated_goal_change(
                document.as_ref(),
                operation_id,
                &operation,
                GoalChangeKind::Updated,
            )? {
                return Ok(change);
            }
            let revision = document.as_ref().map_or(0, |document| document.revision);
            let retired_goal_ids = document
                .as_ref()
                .map_or_else(Vec::new, |document| document.retired_goal_ids.clone());
            let mut goal = document
                .as_ref()
                .and_then(|document| document.goal.clone())
                .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
            if goal.status.is_terminal() {
                return Err(RuntimeStateError::Terminal { entity: "Goal" });
            }
            let previous = goal.clone();
            if let Some(title) = &patch.title {
                goal.title = title.clone();
            }
            if let Some(objective) = &patch.objective {
                goal.objective = objective.clone();
            }
            if let Some(description) = &patch.description {
                goal.description = description.clone();
            }
            if let Some(token_budget) = patch.token_budget {
                goal.token_budget = token_budget;
            }
            if let Some(progress_percent) = patch.progress_percent {
                goal.progress_percent = progress_percent;
            }
            let changed = goal != previous;
            if changed {
                goal.updated_at_unix_ms = unix_time_ms()?.max(goal.updated_at_unix_ms);
            }
            match self.save_goal(
                operation_id,
                &operation,
                revision,
                Some(goal),
                retired_goal_ids,
            ) {
                Ok(outcome) => {
                    return Ok(goal_change_from_outcome(
                        GoalChangeKind::Updated,
                        outcome,
                        changed,
                    ));
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Goal 更新"))
    }

    /// 只允许项目活跃 Goal 进入完成或带原因的阻塞终态。
    fn transition_goal(
        &self,
        operation_id: &str,
        transition: GoalTransition,
    ) -> Result<GoalChange, RuntimeStateError> {
        let transition = transition.normalized()?;
        let operation = ("goal_transition_v1", &transition);
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let document = self.read_goal()?;
            if let Some(change) = deduplicated_goal_change(
                document.as_ref(),
                operation_id,
                &operation,
                GoalChangeKind::Transitioned,
            )? {
                return Ok(change);
            }
            let revision = document.as_ref().map_or(0, |document| document.revision);
            let retired_goal_ids = document
                .as_ref()
                .map_or_else(Vec::new, |document| document.retired_goal_ids.clone());
            let mut goal = document
                .as_ref()
                .and_then(|document| document.goal.clone())
                .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
            if goal.status.is_terminal() {
                return Err(RuntimeStateError::Terminal { entity: "Goal" });
            }
            goal.status = resource_goal_status(transition.status);
            goal.blocked_reason = transition.blocked_reason.clone();
            goal.completion_evidence = transition.completion_evidence.clone();
            goal.updated_at_unix_ms = unix_time_ms()?.max(goal.updated_at_unix_ms);
            match self.save_goal(
                operation_id,
                &operation,
                revision,
                Some(goal),
                retired_goal_ids,
            ) {
                Ok(outcome) => {
                    return Ok(goal_change_from_outcome(
                        GoalChangeKind::Transitioned,
                        outcome,
                        true,
                    ));
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Goal 终态迁移"))
    }

    /// 仅清除已经完成或阻塞的项目 Goal，并由 GoalFileStore 保存墓碑。
    fn clear_goal(&self, operation_id: &str) -> Result<GoalChange, RuntimeStateError> {
        let operation = "goal_clear_v1";
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let document = self.read_goal()?;
            if let Some(change) = deduplicated_goal_change(
                document.as_ref(),
                operation_id,
                &operation,
                GoalChangeKind::Cleared,
            )? {
                return Ok(change);
            }
            let revision = document.as_ref().map_or(0, |document| document.revision);
            let retired_goal_ids = document
                .as_ref()
                .map_or_else(Vec::new, |document| document.retired_goal_ids.clone());
            let goal = document
                .as_ref()
                .and_then(|document| document.goal.as_ref())
                .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
            if !goal.status.is_terminal() {
                return Err(RuntimeStateError::invalid(
                    "活跃 Goal 必须先完成或阻塞，不能直接清除",
                ));
            }
            match self.save_goal(operation_id, &operation, revision, None, retired_goal_ids) {
                Ok(outcome) => {
                    return Ok(goal_change_from_outcome(
                        GoalChangeKind::Cleared,
                        outcome,
                        true,
                    ));
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Goal 清除"))
    }

    /// 使用文件锁内最新值检查加法累计明确 Provider 用量与运行时间。
    fn record_goal_usage(
        &self,
        operation_id: &str,
        delta: GoalUsageDelta,
    ) -> Result<GoalChange, RuntimeStateError> {
        let operation = ("goal_usage_v1", delta);
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let document = self.read_goal()?;
            if let Some(change) = deduplicated_goal_change(
                document.as_ref(),
                operation_id,
                &operation,
                GoalChangeKind::Updated,
            )? {
                return Ok(change);
            }
            let revision = document.as_ref().map_or(0, |document| document.revision);
            let retired_goal_ids = document
                .as_ref()
                .map_or_else(Vec::new, |document| document.retired_goal_ids.clone());
            let mut goal = document
                .as_ref()
                .and_then(|document| document.goal.clone())
                .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
            if goal.status.is_terminal() {
                return Err(RuntimeStateError::Terminal { entity: "Goal" });
            }
            let changed = delta.tokens != 0 || delta.elapsed_seconds != 0;
            if changed {
                goal.tokens_used = goal.tokens_used.checked_add(delta.tokens).ok_or(
                    RuntimeStateError::CounterOverflow {
                        counter: "Goal Token",
                    },
                )?;
                goal.time_used_seconds = goal
                    .time_used_seconds
                    .checked_add(delta.elapsed_seconds)
                    .ok_or(RuntimeStateError::CounterOverflow {
                        counter: "Goal 时间",
                    })?;
                goal.updated_at_unix_ms = unix_time_ms()?.max(goal.updated_at_unix_ms);
            }
            match self.save_goal(
                operation_id,
                &operation,
                revision,
                Some(goal),
                retired_goal_ids,
            ) {
                Ok(outcome) => {
                    return Ok(goal_change_from_outcome(
                        GoalChangeKind::Updated,
                        outcome,
                        changed,
                    ));
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Goal 用量累计"))
    }
}

impl PlanController for PersistentAgentState {
    /// 从应用数据沙箱读取指定 Session 与 Agent 的计划文档。
    fn plan_snapshot(
        &self,
        session_id: &keencode_agent::SessionId,
        agent_id: &keencode_agent::AgentId,
    ) -> Result<PlanSnapshot, RuntimeStateError> {
        let (session_id, agent_id) = self.plan_identity(session_id, agent_id)?;
        self.read_plan(&session_id, &agent_id)
            .map(|(snapshot, _)| snapshot)
    }

    /// 原子完整替换应用数据沙箱中的计划正文。
    fn replace_plan(
        &self,
        operation_id: &str,
        session_id: &keencode_agent::SessionId,
        agent_id: &keencode_agent::AgentId,
        content: String,
    ) -> Result<PlanChange, RuntimeStateError> {
        let content = normalize_required_text("Plan content", content, MAX_PLAN_CONTENT_CHARS)?;
        let (session_id, agent_id) = self.plan_identity(session_id, agent_id)?;
        // Plan 文档与根 Session 的权威 PlanChanged 事件跨越两个持久化边界；
        // 整个 mutation 必须串行，不能让并发调用以旧 Artifact 回写权威状态。
        let _plan_commit_guard = self
            .session
            .lock_plan_commit()
            .map_err(runtime_state_error)?;
        // 只有根 Agent 的最终计划进入单一权威 PlanState；子 Agent 报告保留在三层计划沙箱。
        let artifact = if agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
            // 根计划正文先形成可冷恢复的内容寻址 Artifact，再由 PlanFileStore 记录同一引用。
            Some(
                self.session
                    .put_artifact(content.as_bytes(), Some("text/markdown".to_owned()))
                    .map_err(runtime_state_error)?,
            )
        } else {
            None
        };
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let (previous, document) = self.read_plan(&session_id, &agent_id)?;
            let mut document = document.unwrap_or_else(|| {
                PlanDocument::new(
                    self.project_scope.clone(),
                    session_id.clone(),
                    agent_id.clone(),
                )
            });
            let requested_change = document.content.as_deref() != Some(content.as_str());
            if requested_change {
                document.content = Some(content.clone());
                document.updated_at_unix_ms =
                    Some(unix_time_ms()?.max(document.updated_at_unix_ms.unwrap_or_default()));
            }
            let outcome = match artifact.as_ref() {
                Some(artifact) => self.plan_store.compare_and_swap_with_artifact_ref(
                    artifact,
                    operation_id,
                    previous.revision,
                    document,
                ),
                None => self.plan_store.compare_and_swap(
                    operation_id,
                    &("plan_replace_v1", &content),
                    previous.revision,
                    document,
                ),
            };
            match outcome {
                Ok(outcome) => {
                    let deduplicated = outcome.deduplicated();
                    let document = outcome.into_document();
                    let current = plan_snapshot(&document);
                    if agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
                        self.publish_plan_artifact(operation_id, document.plan_artifact.clone())?;
                    }
                    let previous = if deduplicated {
                        current.clone()
                    } else {
                        previous
                    };
                    return Ok(PlanChange {
                        previous,
                        current,
                        changed: requested_change && !deduplicated,
                    });
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Plan 替换"))
    }

    /// 原子清除应用数据沙箱中的计划正文并保留单调 revision。
    fn clear_plan(
        &self,
        operation_id: &str,
        session_id: &keencode_agent::SessionId,
        agent_id: &keencode_agent::AgentId,
    ) -> Result<PlanChange, RuntimeStateError> {
        let operation = "plan_clear_v1";
        let (session_id, agent_id) = self.plan_identity(session_id, agent_id)?;
        // 与 replace_plan 使用同一提交锁，确保清除不会和根计划发布交错。
        let _plan_commit_guard = self
            .session
            .lock_plan_commit()
            .map_err(runtime_state_error)?;
        for _ in 0..MAX_DOCUMENT_CAS_ATTEMPTS {
            let (previous, document) = self.read_plan(&session_id, &agent_id)?;
            let mut document = document.unwrap_or_else(|| {
                PlanDocument::new(
                    self.project_scope.clone(),
                    session_id.clone(),
                    agent_id.clone(),
                )
            });
            let requested_change = document.content.is_some();
            if requested_change {
                document.content = None;
                document.plan_artifact = None;
                document.updated_at_unix_ms =
                    Some(unix_time_ms()?.max(document.updated_at_unix_ms.unwrap_or_default()));
            }
            match self.plan_store.compare_and_swap(
                operation_id,
                &operation,
                previous.revision,
                document,
            ) {
                Ok(outcome) => {
                    let deduplicated = outcome.deduplicated();
                    let document = outcome.into_document();
                    let current = plan_snapshot(&document);
                    if agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
                        self.publish_plan_artifact(operation_id, document.plan_artifact.clone())?;
                    }
                    let previous = if deduplicated {
                        current.clone()
                    } else {
                        previous
                    };
                    return Ok(PlanChange {
                        previous,
                        current,
                        changed: requested_change && !deduplicated,
                    });
                }
                Err(ResourceError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(state_resource_error(error)),
            }
        }
        Err(document_contention("Plan 清除"))
    }
}

impl PersistentAgentState {
    /// 将计划 Artifact 引用写入权威 PlanChanged 事件，保证冷恢复可验证其实体。
    fn publish_plan_artifact(
        &self,
        operation_id: &str,
        artifact: Option<keencode_resources::ArtifactUse>,
    ) -> Result<(), RuntimeStateError> {
        let snapshot = self.session.snapshot().map_err(runtime_state_error)?;
        if snapshot.state.plan.plan_artifact == artifact {
            return Ok(());
        }
        self.session
            .set_plan(
                operation_id,
                PlanState {
                    enabled: snapshot.state.plan.enabled,
                    plan_artifact: artifact,
                },
            )
            .map(|_| ())
            .map_err(runtime_state_error)
    }
}

impl RuntimeSession {
    /// 在 Runtime 控制锁内幂等提交 Todo 权威事件并返回前后快照。
    fn replace_todos_authoritative(
        &self,
        operation_id: &str,
        operation_payload_sha256: String,
        items: Vec<ResourceTodoItem>,
    ) -> Result<(ResourceTodoSnapshot, ResourceTodoSnapshot), RuntimeError> {
        validate_control_operation_id(operation_id)?;
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        let previous = self.inner.journal.state()?.todos;
        let revision = if previous.items == items {
            previous.revision
        } else {
            previous
                .revision
                .checked_add(1)
                .ok_or(RuntimeError::RecoveryRequired)?
        };
        let event_id = runtime_control_event_id(self.session_id(), operation_id)?;
        commit_runtime_lifecycle_event(
            &self.inner,
            &mut control,
            event_id,
            SessionEvent::TodoReplaced {
                operation_payload_sha256,
                items,
                revision,
            },
            true,
        )?;
        let current = self.inner.journal.state()?.todos;
        Ok((previous, current))
    }
}

/// 校验完整 Todo 列表并返回去除首尾空白后的模型提交值。
fn normalize_todos(items: Vec<AgentTodoItem>) -> Result<Vec<AgentTodoItem>, RuntimeStateError> {
    if items.len() > 100 {
        return Err(RuntimeStateError::invalid("Todo 条目不能超过 100 项"));
    }
    let mut normalized = Vec::with_capacity(items.len());
    let mut in_progress = 0_usize;
    let mut contents = BTreeSet::new();
    for item in items {
        let item = item.normalized()?;
        if item.status == AgentTodoStatus::InProgress {
            in_progress = in_progress.saturating_add(1);
        }
        if !contents.insert(item.content.clone()) {
            return Err(RuntimeStateError::invalid("Todo 内容不能重复"));
        }
        normalized.push(item);
    }
    if in_progress > 1 {
        return Err(RuntimeStateError::invalid(
            "Todo 列表最多只能有一个 in_progress 条目",
        ));
    }
    Ok(normalized)
}

/// 把 Agent Todo 条目无损转换为 Session 事件条目。
fn resource_todo_item(item: &AgentTodoItem) -> ResourceTodoItem {
    ResourceTodoItem {
        content: item.content.clone(),
        status: match item.status {
            AgentTodoStatus::Pending => ResourceTodoStatus::Pending,
            AgentTodoStatus::InProgress => ResourceTodoStatus::InProgress,
            AgentTodoStatus::Completed => ResourceTodoStatus::Completed,
        },
        active_form: item.active_form.clone(),
    }
}

/// 把 Session Journal Todo 快照无损转换为 Agent 工具快照。
fn agent_todo_snapshot(snapshot: &ResourceTodoSnapshot) -> AgentTodoSnapshot {
    AgentTodoSnapshot {
        revision: snapshot.revision,
        items: snapshot
            .items
            .iter()
            .map(|item| AgentTodoItem {
                content: item.content.clone(),
                status: match item.status {
                    ResourceTodoStatus::Pending => AgentTodoStatus::Pending,
                    ResourceTodoStatus::InProgress => AgentTodoStatus::InProgress,
                    ResourceTodoStatus::Completed => AgentTodoStatus::Completed,
                },
                active_form: item.active_form.clone(),
            })
            .collect(),
    }
}

/// 把 Agent Goal 状态转换为持久文档状态。
fn resource_goal_status(status: AgentGoalStatus) -> ResourceGoalStatus {
    match status {
        AgentGoalStatus::Active => ResourceGoalStatus::Active,
        AgentGoalStatus::Completed => ResourceGoalStatus::Completed,
        AgentGoalStatus::Blocked => ResourceGoalStatus::Blocked,
    }
}

/// 把持久 Goal 状态转换为 Agent 工具状态。
fn agent_goal_status(status: ResourceGoalStatus) -> AgentGoalStatus {
    match status {
        ResourceGoalStatus::Active => AgentGoalStatus::Active,
        ResourceGoalStatus::Completed => AgentGoalStatus::Completed,
        ResourceGoalStatus::Blocked => AgentGoalStatus::Blocked,
    }
}

/// 把持久 Goal 记录无损转换为 Agent 工具记录。
fn agent_goal_record(goal: ResourceGoalRecord) -> AgentGoalRecord {
    AgentGoalRecord {
        id: goal.id,
        title: goal.title,
        scope: goal.scope,
        status: agent_goal_status(goal.status),
        description: goal.description,
        progress_percent: goal.progress_percent,
        objective: goal.objective,
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        blocked_reason: goal.blocked_reason,
        completion_evidence: goal.completion_evidence,
        created_at_unix_ms: goal.created_at_unix_ms,
        updated_at_unix_ms: goal.updated_at_unix_ms,
    }
}

/// 从持久 Goal 文档字段构造 Agent 工具快照。
fn agent_goal_snapshot(revision: u64, goal: Option<ResourceGoalRecord>) -> AgentGoalSnapshot {
    AgentGoalSnapshot {
        revision,
        goal: goal.map(agent_goal_record),
    }
}

/// 从持久 Plan 文档构造 Agent 工具快照。
fn plan_snapshot(document: &PlanDocument) -> PlanSnapshot {
    PlanSnapshot {
        revision: document.revision,
        content: document.content.clone(),
        updated_at_unix_ms: document.updated_at_unix_ms,
    }
}

/// 在领域状态检查前识别已经持久化的 Goal 操作，并拒绝标识载荷冲突。
fn deduplicated_goal_change<P: Serialize + ?Sized>(
    document: Option<&GoalDocument>,
    operation_id: &str,
    operation: &P,
    kind: GoalChangeKind,
) -> Result<Option<GoalChange>, RuntimeStateError> {
    let Some(document) = document else {
        return Ok(None);
    };
    document
        .applied_operation_revision(operation_id, operation)
        .map_err(state_resource_error)
        .map(|revision| {
            revision.map(|_| GoalChange {
                kind,
                current: agent_goal_snapshot(document.revision, document.goal.clone()),
                changed: false,
            })
        })
}

/// 把 Goal 文件存储的首次应用或去重结果转换为控制器变化结果。
fn goal_change_from_outcome(
    kind: GoalChangeKind,
    outcome: DocumentOperationOutcome<GoalDocument>,
    requested_change: bool,
) -> GoalChange {
    let deduplicated = outcome.deduplicated();
    let document = outcome.into_document();
    GoalChange {
        kind,
        current: agent_goal_snapshot(document.revision, document.goal),
        changed: requested_change && !deduplicated,
    }
}

/// 校验必填文本并去除首尾空白。
fn normalize_required_text(
    field: &str,
    value: String,
    max_chars: usize,
) -> Result<String, RuntimeStateError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(RuntimeStateError::invalid(format!("{field} 不能为空")));
    }
    if value.chars().count() > max_chars {
        return Err(RuntimeStateError::invalid(format!(
            "{field} 超过 {max_chars} 个字符上限"
        )));
    }
    Ok(value)
}

/// 返回当前 Unix Epoch 毫秒时间并拒绝时钟或整数异常。
fn unix_time_ms() -> Result<u64, RuntimeStateError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeStateError::storage("系统时钟早于 Unix Epoch"))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| RuntimeStateError::CounterOverflow {
        counter: "时间戳",
    })
}

/// 把资源标识校验错误映射为 Agent 输入错误。
fn identifier_error(error: ResourceError) -> RuntimeStateError {
    RuntimeStateError::invalid(error.to_string())
}

/// 把 Runtime 持久化错误映射为工具控制器稳定错误。
fn runtime_state_error(error: RuntimeError) -> RuntimeStateError {
    match error {
        RuntimeError::ControlOperationConflict => RuntimeStateError::Conflict {
            message: "Todo operationId 已绑定到不同状态变更".to_owned(),
        },
        RuntimeError::InvalidControlOperation => {
            RuntimeStateError::invalid("Todo operationId 无效")
        }
        error => RuntimeStateError::storage(error.to_string()),
    }
}

/// 把资源存储错误映射为不回显文档正文的状态错误。
fn storage_error(error: ResourceError) -> RuntimeStateError {
    RuntimeStateError::storage(error.to_string())
}

/// 把资源操作标识冲突和格式错误映射为稳定领域错误，其余视为存储失败。
fn state_resource_error(error: ResourceError) -> RuntimeStateError {
    match error {
        ResourceError::OperationConflict => RuntimeStateError::Conflict {
            message: "状态 operation_id 已绑定到不同请求".to_owned(),
        },
        ResourceError::InvalidId(message) => RuntimeStateError::invalid(message),
        error => storage_error(error),
    }
}

/// 返回文件 CAS 在有界重试后仍持续竞争的稳定冲突错误。
fn document_contention(operation: &str) -> RuntimeStateError {
    RuntimeStateError::Conflict {
        message: format!("{operation} 因并发 revision 持续变化而失败，请重试"),
    }
}
