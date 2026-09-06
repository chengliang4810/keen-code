//! Session Todo、项目 Goal 与计划沙箱文档的 Provider 中立状态契约。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AgentId, SessionId};

/// 单个内存状态作用域保留的最近幂等操作收据上限。
const MAX_STATE_OPERATION_RECEIPTS: usize = 256;
/// 单次 Plan 正文允许的最大 Unicode 字符数。
pub const MAX_PLAN_CONTENT_CHARS: usize = 200_000;
/// Goal complete 操作要求的完成证据最大 Unicode 字符数。
pub const MAX_GOAL_EVIDENCE_CHARS: usize = 20_000;

/// 根 Session 唯一 Todo 列表中的条目状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// 尚未开始处理。
    Pending,
    /// 当前正在处理；同一列表最多只能有一项。
    InProgress,
    /// 已经完成。
    Completed,
}

/// 根 Session 唯一 Todo 列表中的一个可展示步骤。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct TodoItem {
    /// 完成后展示的祈使式任务内容。
    pub content: String,
    /// 当前任务状态。
    pub status: TodoStatus,
    /// 任务进行中用于界面展示的现在进行时文本。
    pub active_form: String,
}

impl TodoItem {
    /// 校验并去除任务内容与进行时文本首尾空白。
    pub fn normalized(mut self) -> Result<Self, RuntimeStateError> {
        self.content = normalize_required_text("Todo content", self.content, 500)?;
        self.active_form = normalize_required_text("Todo active_form", self.active_form, 500)?;
        Ok(self)
    }
}

/// 当前根 Session Todo 列表的不可变快照。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TodoSnapshot {
    /// 每次实际变化递增的版本号。
    pub revision: u64,
    /// 当前仍需展示和恢复的 Todo 条目。
    pub items: Vec<TodoItem>,
}

/// 一次 Todo 全量替换的前后状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TodoChange {
    /// 替换前的稳定快照。
    pub previous: TodoSnapshot,
    /// 模型提交的完整列表；全部完成时仍保留在工具结果中。
    pub submitted: Vec<TodoItem>,
    /// 实际保存的当前快照；全部完成后自动清空。
    pub current: TodoSnapshot,
    /// 本次调用是否真正改变了当前状态。
    pub changed: bool,
}

/// 根 Session 唯一权威 Todo 状态的同步事务边界。
pub trait TodoController: Send + Sync {
    /// 返回当前 Session Todo 快照。
    fn todo_snapshot(&self) -> Result<TodoSnapshot, RuntimeStateError>;

    /// 按可信操作标识原子校验并全量替换当前 Session Todo 列表。
    fn replace_todos(
        &self,
        operation_id: &str,
        items: Vec<TodoItem>,
    ) -> Result<TodoChange, RuntimeStateError>;
}

/// 项目级持久 Goal 的生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Agent 仍应继续推进目标。
    Active,
    /// 目标已经完成，不允许再次迁移。
    Completed,
    /// 目标因无法自行解决的原因阻塞，不允许再次迁移。
    Blocked,
}

impl GoalStatus {
    /// 返回当前状态是否已经不可逆地结束。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Blocked)
    }
}

/// 一次 Goal 终态迁移的完整规范输入。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GoalTransition {
    /// 只能是完成或阻塞终态。
    pub status: GoalStatus,
    /// 仅阻塞终态携带的非空原因。
    pub blocked_reason: Option<String>,
    /// 仅完成终态携带的非空验收证据。
    pub completion_evidence: Option<String>,
}

impl GoalTransition {
    /// 校验终态及条件字段，并规范化原因或完成证据。
    pub fn normalized(mut self) -> Result<Self, RuntimeStateError> {
        match self.status {
            GoalStatus::Active => Err(RuntimeStateError::invalid(
                "Goal 只能从 active 迁移到 completed 或 blocked",
            )),
            GoalStatus::Completed => {
                if self.blocked_reason.is_some() {
                    return Err(RuntimeStateError::invalid(
                        "Goal completed 状态不能携带 blocked_reason",
                    ));
                }
                self.completion_evidence = Some(normalize_required_text(
                    "Goal completion_evidence",
                    self.completion_evidence.unwrap_or_default(),
                    MAX_GOAL_EVIDENCE_CHARS,
                )?);
                Ok(self)
            }
            GoalStatus::Blocked => {
                if self.completion_evidence.is_some() {
                    return Err(RuntimeStateError::invalid(
                        "Goal blocked 状态不能携带 completion_evidence",
                    ));
                }
                self.blocked_reason = Some(normalize_required_text(
                    "Goal blocked_reason",
                    self.blocked_reason.unwrap_or_default(),
                    4_000,
                )?);
                Ok(self)
            }
        }
    }
}

/// 创建项目 Goal 时必须一次性提供的字段。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GoalDraft {
    /// 输入框上方展示的简短标题。
    pub title: String,
    /// 可验证且完整的目标描述。
    pub objective: String,
    /// 可选的补充说明。
    pub description: Option<String>,
    /// 可选 Token 预算；`None` 表示不限制。
    pub token_budget: Option<u64>,
    /// 可选人工进度百分比。
    pub progress_percent: Option<u8>,
}

impl GoalDraft {
    /// 校验创建字段并规范化全部文本。
    pub fn normalized(mut self) -> Result<Self, RuntimeStateError> {
        self.title = normalize_required_text("Goal title", self.title, 200)?;
        self.objective = normalize_required_text("Goal objective", self.objective, 20_000)?;
        self.description = normalize_optional_text("Goal description", self.description, 20_000)?;
        validate_goal_budget(self.token_budget)?;
        validate_goal_progress(self.progress_percent)?;
        Ok(self)
    }
}

/// 对当前活跃 Goal 执行的部分字段更新。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GoalPatch {
    /// 新标题；`None` 表示保持不变。
    pub title: Option<String>,
    /// 新目标描述；`None` 表示保持不变。
    pub objective: Option<String>,
    /// 外层 `None` 表示保持不变，内层 `None` 表示清除说明。
    pub description: Option<Option<String>>,
    /// 外层 `None` 表示保持不变，内层 `None` 表示取消预算。
    pub token_budget: Option<Option<u64>>,
    /// 外层 `None` 表示保持不变，内层 `None` 表示清除人工进度。
    pub progress_percent: Option<Option<u8>>,
}

impl GoalPatch {
    /// 校验补丁至少包含一个字段并规范化其中的文本。
    pub fn normalized(mut self) -> Result<Self, RuntimeStateError> {
        if self.title.is_none()
            && self.objective.is_none()
            && self.description.is_none()
            && self.token_budget.is_none()
            && self.progress_percent.is_none()
        {
            return Err(RuntimeStateError::invalid("Goal 更新至少需要一个字段"));
        }
        if let Some(title) = self.title.take() {
            self.title = Some(normalize_required_text("Goal title", title, 200)?);
        }
        if let Some(objective) = self.objective.take() {
            self.objective = Some(normalize_required_text(
                "Goal objective",
                objective,
                20_000,
            )?);
        }
        if let Some(description) = self.description.take() {
            self.description = Some(normalize_optional_text(
                "Goal description",
                description,
                20_000,
            )?);
        }
        if let Some(token_budget) = self.token_budget {
            validate_goal_budget(token_budget)?;
        }
        if let Some(progress_percent) = self.progress_percent {
            validate_goal_progress(progress_percent)?;
        }
        Ok(self)
    }
}

/// 一次模型调用结束后追加到项目 Goal 的用量增量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GoalUsageDelta {
    /// 新增的明确 Token 数；缺失的 Provider 用量不得估算后写入。
    pub tokens: u64,
    /// 新增的实际运行秒数。
    pub elapsed_seconds: u64,
}

/// 项目当前唯一 Goal 的完整持久字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GoalRecord {
    /// 跨进程唯一且按创建时间排序的 Goal 标识。
    pub id: String,
    /// 输入框上方展示的简短标题。
    pub title: String,
    /// 固定项目级作用域。
    pub scope: String,
    /// 当前生命周期状态。
    pub status: GoalStatus,
    /// 可选补充说明。
    pub description: Option<String>,
    /// 可选人工进度百分比。
    pub progress_percent: Option<u8>,
    /// 可验证且完整的目标描述。
    pub objective: String,
    /// 可选 Token 预算。
    pub token_budget: Option<u64>,
    /// Provider 明确报告并累计的 Token 数。
    pub tokens_used: u64,
    /// 累计实际运行秒数。
    pub time_used_seconds: u64,
    /// 仅在阻塞状态存在的原因。
    pub blocked_reason: Option<String>,
    /// 仅在完成状态存在的非空验收证据。
    pub completion_evidence: Option<String>,
    /// 创建时间的 Unix 毫秒值。
    pub created_at_unix_ms: u64,
    /// 最后变化时间的 Unix 毫秒值。
    pub updated_at_unix_ms: u64,
}

impl GoalRecord {
    /// 返回明确预算存在且大于零时的使用比例。
    pub fn usage_ratio(&self) -> Option<f64> {
        self.token_budget
            .filter(|budget| *budget > 0)
            .map(|budget| self.tokens_used as f64 / budget as f64)
    }
}

/// 项目 Goal 当前版本和可选记录。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GoalSnapshot {
    /// 每次实际变化递增的版本号。
    pub revision: u64,
    /// 当前唯一 Goal；清除后为 `None`。
    pub goal: Option<GoalRecord>,
}

/// 项目 Goal 变化的稳定类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalChangeKind {
    /// 创建此前不存在的 Goal。
    Created,
    /// 更新活跃 Goal 的目标字段或用量。
    Updated,
    /// 从活跃状态进入完成或阻塞终态。
    Transitioned,
    /// 清除当前 Goal 并释放项目单例槽位。
    Cleared,
}

/// 一次 Goal 原子变化后的快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GoalChange {
    /// 变化类别。
    pub kind: GoalChangeKind,
    /// 变化完成后的完整快照。
    pub current: GoalSnapshot,
    /// 本次调用是否真正改变了 Goal 状态。
    pub changed: bool,
}

/// 项目唯一 Goal 的同步事务边界。
pub trait GoalController: Send + Sync {
    /// 返回当前项目 Goal 快照。
    fn goal_snapshot(&self) -> Result<GoalSnapshot, RuntimeStateError>;

    /// 在项目没有 Goal 时创建一个新 Goal。
    fn create_goal(
        &self,
        operation_id: &str,
        draft: GoalDraft,
    ) -> Result<GoalChange, RuntimeStateError>;

    /// 更新当前活跃 Goal 的部分字段。
    fn update_goal(
        &self,
        operation_id: &str,
        patch: GoalPatch,
    ) -> Result<GoalChange, RuntimeStateError>;

    /// 将当前活跃 Goal 迁移到完成或阻塞终态。
    fn transition_goal(
        &self,
        operation_id: &str,
        transition: GoalTransition,
    ) -> Result<GoalChange, RuntimeStateError>;

    /// 仅清除已经完成或阻塞的 Goal；活跃 Goal 必须先显式收敛到终态。
    fn clear_goal(&self, operation_id: &str) -> Result<GoalChange, RuntimeStateError>;

    /// 原子累计一次明确的 Token 与运行时间增量。
    fn record_goal_usage(
        &self,
        operation_id: &str,
        delta: GoalUsageDelta,
    ) -> Result<GoalChange, RuntimeStateError>;
}

/// Session 或子 Agent 在应用数据沙箱中的计划文档快照。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanSnapshot {
    /// 每次实际变化递增的版本号。
    pub revision: u64,
    /// 当前计划或报告正文；清除后为 `None`。
    pub content: Option<String>,
    /// 最后变化时间的 Unix 毫秒值。
    pub updated_at_unix_ms: Option<u64>,
}

/// 一次计划文档替换或清除的前后状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanChange {
    /// 变化前快照。
    pub previous: PlanSnapshot,
    /// 变化后快照。
    pub current: PlanSnapshot,
    /// 本次调用是否真正改变了正文。
    pub changed: bool,
}

/// 计划模式专用应用数据沙箱文档的同步事务边界。
pub trait PlanController: Send + Sync {
    /// 返回指定 Session 与来源 Agent 的计划文档快照。
    fn plan_snapshot(
        &self,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<PlanSnapshot, RuntimeStateError>;

    /// 原子完整替换指定 Session 与来源 Agent 的计划文档。
    fn replace_plan(
        &self,
        operation_id: &str,
        session_id: &SessionId,
        agent_id: &AgentId,
        content: String,
    ) -> Result<PlanChange, RuntimeStateError>;

    /// 清除指定 Session 与来源 Agent 的计划文档。
    fn clear_plan(
        &self,
        operation_id: &str,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<PlanChange, RuntimeStateError>;
}

/// 内存控制器中一条最近成功操作的幂等收据。
#[derive(Clone, Debug, Eq, PartialEq)]
struct StateOperationReceipt {
    /// 可信调用上下文派生的操作标识。
    operation_id: String,
    /// 操作种类与规范化参数的稳定 SHA-256。
    payload_sha256: String,
}

/// 一个状态作用域内按提交顺序保存的有界幂等收据队列。
#[derive(Default)]
struct StateOperationLedger {
    /// 最近成功接受的操作收据。
    receipts: VecDeque<StateOperationReceipt>,
}

impl StateOperationLedger {
    /// 返回相同操作是否已经提交，并拒绝同一标识复用于不同载荷。
    fn is_replay(
        &self,
        operation_id: &str,
        payload_sha256: &str,
    ) -> Result<bool, RuntimeStateError> {
        match self
            .receipts
            .iter()
            .find(|receipt| receipt.operation_id == operation_id)
        {
            Some(receipt) if receipt.payload_sha256 == payload_sha256 => Ok(true),
            Some(_) => Err(RuntimeStateError::Conflict {
                message: "状态 operation_id 已绑定到不同请求".to_owned(),
            }),
            None => Ok(false),
        }
    }

    /// 记录一次成功操作，并在达到固定上限时淘汰最旧收据。
    fn record(&mut self, operation_id: &str, payload_sha256: String) {
        if self.receipts.len() == MAX_STATE_OPERATION_RECEIPTS {
            self.receipts.pop_front();
        }
        self.receipts.push_back(StateOperationReceipt {
            operation_id: operation_id.to_owned(),
            payload_sha256,
        });
    }
}

/// 内存中的确定性状态控制器，供测试、原型和持久化控制器缓存使用。
#[derive(Default)]
pub struct InMemoryRuntimeState {
    /// 根 Session 唯一 Todo 快照。
    todos: RwLock<TodoSnapshot>,
    /// 当前项目唯一 Goal 快照。
    goal: RwLock<GoalSnapshot>,
    /// 按 Session 与 Agent 隔离的计划文档快照。
    plans: RwLock<BTreeMap<(SessionId, AgentId), PlanSnapshot>>,
    /// Session Todo 最近成功操作的有界收据。
    todo_operations: Mutex<StateOperationLedger>,
    /// 项目 Goal 最近成功操作的有界收据。
    goal_operations: Mutex<StateOperationLedger>,
    /// 每个 Session/Agent Plan 最近成功操作的有界收据。
    plan_operations: Mutex<BTreeMap<(SessionId, AgentId), StateOperationLedger>>,
}

impl InMemoryRuntimeState {
    /// 创建全部状态为空且版本号为零的控制器。
    pub fn new() -> Self {
        Self::default()
    }
}

impl TodoController for InMemoryRuntimeState {
    /// 返回不存在时版本为零的空 Todo 快照。
    fn todo_snapshot(&self) -> Result<TodoSnapshot, RuntimeStateError> {
        self.todos
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RuntimeStateError::LockPoisoned)
    }

    /// 校验单活动项、重复内容和条目数量后原子替换列表。
    fn replace_todos(
        &self,
        operation_id: &str,
        items: Vec<TodoItem>,
    ) -> Result<TodoChange, RuntimeStateError> {
        let submitted = normalize_todos(items)?;
        let stored_items = if !submitted.is_empty()
            && submitted
                .iter()
                .all(|item| item.status == TodoStatus::Completed)
        {
            Vec::new()
        } else {
            submitted.clone()
        };
        let payload_sha256 =
            state_operation_sha256(operation_id, &("todo_replace_v1", &submitted))?;
        let mut operations = self
            .todo_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .todos
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let previous = guard.clone();
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(TodoChange {
                previous: previous.clone(),
                submitted,
                current: previous,
                changed: false,
            });
        }
        if previous.items == stored_items {
            operations.record(operation_id, payload_sha256);
            return Ok(TodoChange {
                previous: previous.clone(),
                submitted,
                current: previous,
                changed: false,
            });
        }
        let current = TodoSnapshot {
            revision: next_revision(previous.revision)?,
            items: stored_items,
        };
        *guard = current.clone();
        operations.record(operation_id, payload_sha256);
        Ok(TodoChange {
            previous,
            submitted,
            current,
            changed: true,
        })
    }
}

impl GoalController for InMemoryRuntimeState {
    /// 返回项目当前 Goal 快照。
    fn goal_snapshot(&self) -> Result<GoalSnapshot, RuntimeStateError> {
        self.goal
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RuntimeStateError::LockPoisoned)
    }

    /// 创建项目单例 Goal，并拒绝覆盖任何现有状态。
    fn create_goal(
        &self,
        operation_id: &str,
        draft: GoalDraft,
    ) -> Result<GoalChange, RuntimeStateError> {
        let draft = draft.normalized()?;
        let payload_sha256 = state_operation_sha256(operation_id, &("goal_create_v1", &draft))?;
        let mut operations = self
            .goal_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .goal
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(GoalChange {
                kind: GoalChangeKind::Created,
                current: guard.clone(),
                changed: false,
            });
        }
        if guard.goal.is_some() {
            return Err(RuntimeStateError::Conflict {
                message: "项目已有 Goal；请先更新或清除当前 Goal".to_owned(),
            });
        }
        let now = unix_time_ms()?;
        let record = GoalRecord {
            id: Uuid::now_v7().to_string(),
            title: draft.title,
            scope: "project".to_owned(),
            status: GoalStatus::Active,
            description: draft.description,
            progress_percent: draft.progress_percent,
            objective: draft.objective,
            token_budget: draft.token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            blocked_reason: None,
            completion_evidence: None,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        guard.revision = next_revision(guard.revision)?;
        guard.goal = Some(record);
        operations.record(operation_id, payload_sha256);
        Ok(GoalChange {
            kind: GoalChangeKind::Created,
            current: guard.clone(),
            changed: true,
        })
    }

    /// 在同一写锁内验证并更新活跃 Goal 字段。
    fn update_goal(
        &self,
        operation_id: &str,
        patch: GoalPatch,
    ) -> Result<GoalChange, RuntimeStateError> {
        let patch = patch.normalized()?;
        let payload_sha256 = state_operation_sha256(operation_id, &("goal_update_v1", &patch))?;
        let mut operations = self
            .goal_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .goal
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(GoalChange {
                kind: GoalChangeKind::Updated,
                current: guard.clone(),
                changed: false,
            });
        }
        let mut candidate = guard
            .goal
            .clone()
            .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
        if candidate.status.is_terminal() {
            return Err(RuntimeStateError::Terminal { entity: "Goal" });
        }
        let previous = candidate.clone();
        if let Some(title) = patch.title {
            candidate.title = title;
        }
        if let Some(objective) = patch.objective {
            candidate.objective = objective;
        }
        if let Some(description) = patch.description {
            candidate.description = description;
        }
        if let Some(token_budget) = patch.token_budget {
            candidate.token_budget = token_budget;
        }
        if let Some(progress_percent) = patch.progress_percent {
            candidate.progress_percent = progress_percent;
        }
        let changed = candidate != previous;
        if changed {
            candidate.updated_at_unix_ms = unix_time_ms()?.max(candidate.updated_at_unix_ms);
        }
        if !changed {
            operations.record(operation_id, payload_sha256);
            return Ok(GoalChange {
                kind: GoalChangeKind::Updated,
                current: guard.clone(),
                changed: false,
            });
        }
        guard.revision = next_revision(guard.revision)?;
        guard.goal = Some(candidate);
        operations.record(operation_id, payload_sha256);
        Ok(GoalChange {
            kind: GoalChangeKind::Updated,
            current: guard.clone(),
            changed: true,
        })
    }

    /// 只允许活跃 Goal 进入完成或带原因的阻塞终态。
    fn transition_goal(
        &self,
        operation_id: &str,
        transition: GoalTransition,
    ) -> Result<GoalChange, RuntimeStateError> {
        let transition = transition.normalized()?;
        let payload_sha256 =
            state_operation_sha256(operation_id, &("goal_transition_v1", &transition))?;
        let mut operations = self
            .goal_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .goal
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(GoalChange {
                kind: GoalChangeKind::Transitioned,
                current: guard.clone(),
                changed: false,
            });
        }
        let mut candidate = guard
            .goal
            .clone()
            .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
        if candidate.status.is_terminal() {
            return Err(RuntimeStateError::Terminal { entity: "Goal" });
        }
        candidate.status = transition.status;
        candidate.blocked_reason = transition.blocked_reason;
        candidate.completion_evidence = transition.completion_evidence;
        candidate.updated_at_unix_ms = unix_time_ms()?.max(candidate.updated_at_unix_ms);
        guard.revision = next_revision(guard.revision)?;
        guard.goal = Some(candidate);
        operations.record(operation_id, payload_sha256);
        Ok(GoalChange {
            kind: GoalChangeKind::Transitioned,
            current: guard.clone(),
            changed: true,
        })
    }

    /// 只清除终态 Goal 并保留单调版本号，防止活跃目标被无证据丢弃。
    fn clear_goal(&self, operation_id: &str) -> Result<GoalChange, RuntimeStateError> {
        let payload_sha256 = state_operation_sha256(operation_id, &"goal_clear_v1")?;
        let mut operations = self
            .goal_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .goal
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(GoalChange {
                kind: GoalChangeKind::Cleared,
                current: guard.clone(),
                changed: false,
            });
        }
        let record = guard
            .goal
            .as_ref()
            .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
        if !record.status.is_terminal() {
            return Err(RuntimeStateError::invalid(
                "活跃 Goal 必须先完成或阻塞，不能直接清除",
            ));
        }
        guard.revision = next_revision(guard.revision)?;
        guard.goal = None;
        operations.record(operation_id, payload_sha256);
        Ok(GoalChange {
            kind: GoalChangeKind::Cleared,
            current: guard.clone(),
            changed: true,
        })
    }

    /// 使用检查加法累计明确用量并拒绝整数溢出。
    fn record_goal_usage(
        &self,
        operation_id: &str,
        delta: GoalUsageDelta,
    ) -> Result<GoalChange, RuntimeStateError> {
        let payload_sha256 = state_operation_sha256(operation_id, &("goal_usage_v1", delta))?;
        let mut operations = self
            .goal_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let mut guard = self
            .goal
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(GoalChange {
                kind: GoalChangeKind::Updated,
                current: guard.clone(),
                changed: false,
            });
        }
        if guard
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status.is_terminal())
        {
            return Err(RuntimeStateError::Terminal { entity: "Goal" });
        }
        if delta.tokens == 0 && delta.elapsed_seconds == 0 {
            if guard.goal.is_none() {
                return Err(RuntimeStateError::NotFound { entity: "Goal" });
            }
            operations.record(operation_id, payload_sha256);
            return Ok(GoalChange {
                kind: GoalChangeKind::Updated,
                current: guard.clone(),
                changed: false,
            });
        }
        let mut candidate = guard
            .goal
            .clone()
            .ok_or(RuntimeStateError::NotFound { entity: "Goal" })?;
        candidate.tokens_used = candidate.tokens_used.checked_add(delta.tokens).ok_or(
            RuntimeStateError::CounterOverflow {
                counter: "Goal Token",
            },
        )?;
        candidate.time_used_seconds = candidate
            .time_used_seconds
            .checked_add(delta.elapsed_seconds)
            .ok_or(RuntimeStateError::CounterOverflow {
                counter: "Goal 时间",
            })?;
        candidate.updated_at_unix_ms = unix_time_ms()?.max(candidate.updated_at_unix_ms);
        guard.revision = next_revision(guard.revision)?;
        guard.goal = Some(candidate);
        operations.record(operation_id, payload_sha256);
        Ok(GoalChange {
            kind: GoalChangeKind::Updated,
            current: guard.clone(),
            changed: true,
        })
    }
}

impl PlanController for InMemoryRuntimeState {
    /// 返回不存在时版本为零的空计划快照。
    fn plan_snapshot(
        &self,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<PlanSnapshot, RuntimeStateError> {
        let guard = self
            .plans
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        Ok(guard
            .get(&(session_id.clone(), agent_id.clone()))
            .cloned()
            .unwrap_or_default())
    }

    /// 完整替换计划正文并保持 Session 与 Agent 隔离。
    fn replace_plan(
        &self,
        operation_id: &str,
        session_id: &SessionId,
        agent_id: &AgentId,
        content: String,
    ) -> Result<PlanChange, RuntimeStateError> {
        let content = normalize_required_text("Plan content", content, MAX_PLAN_CONTENT_CHARS)?;
        let payload_sha256 = state_operation_sha256(operation_id, &("plan_replace_v1", &content))?;
        let key = (session_id.clone(), agent_id.clone());
        let mut operation_ledgers = self
            .plan_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let operations = operation_ledgers.entry(key.clone()).or_default();
        let mut guard = self
            .plans
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let previous = guard.get(&key).cloned().unwrap_or_default();
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(PlanChange {
                previous: previous.clone(),
                current: previous,
                changed: false,
            });
        }
        if previous.content.as_deref() == Some(content.as_str()) {
            operations.record(operation_id, payload_sha256);
            return Ok(PlanChange {
                previous: previous.clone(),
                current: previous,
                changed: false,
            });
        }
        let current = PlanSnapshot {
            revision: next_revision(previous.revision)?,
            content: Some(content),
            updated_at_unix_ms: Some(
                unix_time_ms()?.max(previous.updated_at_unix_ms.unwrap_or_default()),
            ),
        };
        guard.insert(key, current.clone());
        operations.record(operation_id, payload_sha256);
        Ok(PlanChange {
            previous,
            current,
            changed: true,
        })
    }

    /// 清除计划正文；重复清除保持版本号不变。
    fn clear_plan(
        &self,
        operation_id: &str,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<PlanChange, RuntimeStateError> {
        let payload_sha256 = state_operation_sha256(operation_id, &"plan_clear_v1")?;
        let key = (session_id.clone(), agent_id.clone());
        let mut operation_ledgers = self
            .plan_operations
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let operations = operation_ledgers.entry(key.clone()).or_default();
        let mut guard = self
            .plans
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let previous = guard.get(&key).cloned().unwrap_or_default();
        if operations.is_replay(operation_id, &payload_sha256)? {
            return Ok(PlanChange {
                previous: previous.clone(),
                current: previous,
                changed: false,
            });
        }
        if previous.content.is_none() {
            operations.record(operation_id, payload_sha256);
            return Ok(PlanChange {
                previous: previous.clone(),
                current: previous,
                changed: false,
            });
        }
        let current = PlanSnapshot {
            revision: next_revision(previous.revision)?,
            content: None,
            updated_at_unix_ms: Some(
                unix_time_ms()?.max(previous.updated_at_unix_ms.unwrap_or_default()),
            ),
        };
        guard.insert(key, current.clone());
        operations.record(operation_id, payload_sha256);
        Ok(PlanChange {
            previous,
            current,
            changed: true,
        })
    }
}

/// 运行状态校验、冲突、终态、计数或存储错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeStateError {
    /// 输入不满足领域约束。
    Invalid {
        /// 不包含凭据的具体校验说明。
        message: String,
    },
    /// 当前状态与请求操作冲突。
    Conflict {
        /// 不包含敏感内容的冲突说明。
        message: String,
    },
    /// 请求的状态实体不存在。
    NotFound {
        /// 稳定实体名称。
        entity: &'static str,
    },
    /// 请求修改的状态实体已经进入不可逆终态。
    Terminal {
        /// 稳定实体名称。
        entity: &'static str,
    },
    /// 版本或用量计数器溢出。
    CounterOverflow {
        /// 稳定计数器名称。
        counter: &'static str,
    },
    /// 共享状态锁因先前线程 panic 而中毒。
    LockPoisoned,
    /// 持久化控制器无法原子读取或保存状态。
    Storage {
        /// 已去除敏感路径或凭据的存储说明。
        message: String,
    },
}

impl RuntimeStateError {
    /// 创建输入校验错误。
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }

    /// 创建持久化存储错误。
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
        }
    }
}

impl fmt::Display for RuntimeStateError {
    /// 输出适合工具层归一化的状态错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { message } => formatter.write_str(message),
            Self::Conflict { message } => formatter.write_str(message),
            Self::NotFound { entity } => write!(formatter, "{entity} 不存在"),
            Self::Terminal { entity } => write!(formatter, "{entity} 已进入终态，不能再次修改"),
            Self::CounterOverflow { counter } => write!(formatter, "{counter} 计数器溢出"),
            Self::LockPoisoned => formatter.write_str("运行状态锁已中毒"),
            Self::Storage { message } => write!(formatter, "运行状态持久化失败：{message}"),
        }
    }
}

impl Error for RuntimeStateError {}

/// 校验并规范化完整 Todo 列表。
fn normalize_todos(items: Vec<TodoItem>) -> Result<Vec<TodoItem>, RuntimeStateError> {
    if items.len() > 100 {
        return Err(RuntimeStateError::invalid("Todo 条目不能超过 100 项"));
    }
    let mut normalized = Vec::with_capacity(items.len());
    let mut in_progress = 0_usize;
    let mut contents = BTreeSet::new();
    for item in items {
        let item = item.normalized()?;
        if item.status == TodoStatus::InProgress {
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

/// 校验并去除必填文本首尾空白。
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

/// 校验并规范化可选文本；纯空白视为清除字段。
fn normalize_optional_text(
    field: &str,
    value: Option<String>,
    max_chars: usize,
) -> Result<Option<String>, RuntimeStateError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max_chars {
        return Err(RuntimeStateError::invalid(format!(
            "{field} 超过 {max_chars} 个字符上限"
        )));
    }
    Ok(Some(value))
}

/// 校验 Goal Token 预算必须为正数。
fn validate_goal_budget(value: Option<u64>) -> Result<(), RuntimeStateError> {
    if value == Some(0) {
        return Err(RuntimeStateError::invalid(
            "Goal token_budget 必须大于零或设为 null",
        ));
    }
    Ok(())
}

/// 校验 Goal 人工进度必须位于百分比范围。
fn validate_goal_progress(value: Option<u8>) -> Result<(), RuntimeStateError> {
    if value.is_some_and(|progress| progress > 100) {
        return Err(RuntimeStateError::invalid(
            "Goal progress_percent 必须位于 0..=100 范围",
        ));
    }
    Ok(())
}

/// 校验操作标识并计算操作种类与规范化载荷的稳定 SHA-256。
fn state_operation_sha256<T: Serialize + ?Sized>(
    operation_id: &str,
    operation: &T,
) -> Result<String, RuntimeStateError> {
    validate_state_operation_id(operation_id)?;
    let bytes = serde_json::to_vec(operation)
        .map_err(|_| RuntimeStateError::storage("状态操作载荷无法序列化"))?;
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    Ok(output)
}

/// 校验可信状态操作标识具有固定上限且没有隐式空白或控制字符。
fn validate_state_operation_id(operation_id: &str) -> Result<(), RuntimeStateError> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || operation_id.trim() != operation_id
        || operation_id.chars().any(char::is_control)
    {
        return Err(RuntimeStateError::invalid(
            "状态 operation_id 长度必须为 1..=128 字节且不能包含首尾空白或控制字符",
        ));
    }
    Ok(())
}

/// 使用检查加法生成下一个单调版本号。
fn next_revision(current: u64) -> Result<u64, RuntimeStateError> {
    current
        .checked_add(1)
        .ok_or(RuntimeStateError::CounterOverflow {
            counter: "状态版本",
        })
}

/// 返回当前 Unix 毫秒并拒绝系统时钟早于 Epoch 的异常环境。
fn unix_time_ms() -> Result<u64, RuntimeStateError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeStateError::storage("系统时钟早于 Unix Epoch"))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| RuntimeStateError::CounterOverflow {
        counter: "时间戳",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建稳定的测试 Agent 标识。
    fn agent(value: &str) -> AgentId {
        AgentId::new(value).expect("测试 Agent ID 应有效")
    }

    /// 创建稳定的测试 Session 标识。
    fn session(value: &str) -> SessionId {
        SessionId::new(value).expect("测试 Session ID 应有效")
    }

    /// 创建一个有效 Todo 条目。
    fn todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_owned(),
            status,
            active_form: format!("正在{content}"),
        }
    }

    /// Session Todo 必须拒绝多个活动项，并在全部完成时清空当前状态。
    #[test]
    fn todo_state_is_session_scoped_and_clears_when_complete() {
        let state = InMemoryRuntimeState::new();
        let first = state
            .replace_todos(
                "todo-operation-1",
                vec![
                    todo("分析", TodoStatus::InProgress),
                    todo("验证", TodoStatus::Pending),
                ],
            )
            .expect("首次 Todo 应成功");
        assert_eq!(first.current.revision, 1);
        assert!(
            state
                .replace_todos(
                    "todo-operation-2",
                    vec![
                        todo("分析", TodoStatus::InProgress),
                        todo("验证", TodoStatus::InProgress),
                    ],
                )
                .is_err()
        );
        assert_eq!(
            state.todo_snapshot().expect("失败后快照应读取"),
            first.current
        );

        let completed = state
            .replace_todos(
                "todo-operation-3",
                vec![
                    todo("分析", TodoStatus::Completed),
                    todo("验证", TodoStatus::Completed),
                ],
            )
            .expect("完成 Todo 应成功");
        assert_eq!(completed.submitted.len(), 2);
        assert!(completed.current.items.is_empty());
        assert_eq!(completed.current.revision, 2);
    }

    /// Goal 单例必须遵守创建、更新、终态、清除和重新创建顺序。
    #[test]
    fn goal_state_enforces_singleton_and_terminal_lifecycle() {
        let state = InMemoryRuntimeState::new();
        let created = state
            .create_goal(
                "goal-create-1",
                GoalDraft {
                    title: "Runtime 重写".to_owned(),
                    objective: "实现并验证全部 Runtime 能力".to_owned(),
                    description: None,
                    token_budget: Some(1_000),
                    progress_percent: Some(10),
                },
            )
            .expect("Goal 应创建");
        assert_eq!(created.current.revision, 1);
        assert_eq!(
            created.current.goal.as_ref().map(|goal| goal.status),
            Some(GoalStatus::Active)
        );
        assert!(
            state
                .create_goal(
                    "goal-create-2",
                    GoalDraft {
                        title: "重复".to_owned(),
                        objective: "重复".to_owned(),
                        description: None,
                        token_budget: None,
                        progress_percent: None,
                    },
                )
                .is_err()
        );
        let updated = state
            .update_goal(
                "goal-update-1",
                GoalPatch {
                    progress_percent: Some(Some(80)),
                    description: Some(Some("接近完成".to_owned())),
                    ..GoalPatch::default()
                },
            )
            .expect("活跃 Goal 应更新");
        assert_eq!(updated.current.revision, 2);
        let repeated = state
            .update_goal(
                "goal-update-2",
                GoalPatch {
                    progress_percent: Some(Some(80)),
                    description: Some(Some("接近完成".to_owned())),
                    ..GoalPatch::default()
                },
            )
            .expect("相同 Goal 更新应幂等");
        assert!(!repeated.changed);
        assert_eq!(repeated.current.revision, 2);
        assert!(
            state
                .transition_goal(
                    "goal-transition-missing-evidence",
                    GoalTransition {
                        status: GoalStatus::Completed,
                        blocked_reason: None,
                        completion_evidence: None,
                    },
                )
                .is_err()
        );
        let completed = state
            .transition_goal(
                "goal-transition-1",
                GoalTransition {
                    status: GoalStatus::Completed,
                    blocked_reason: None,
                    completion_evidence: Some("目标字段与用量测试均通过".to_owned()),
                },
            )
            .expect("Goal 应完成");
        assert_eq!(completed.current.revision, 3);
        assert_eq!(
            completed
                .current
                .goal
                .as_ref()
                .and_then(|goal| goal.completion_evidence.as_deref()),
            Some("目标字段与用量测试均通过")
        );
        assert!(
            state
                .update_goal(
                    "goal-update-3",
                    GoalPatch {
                        title: Some("禁止修改".to_owned()),
                        ..GoalPatch::default()
                    },
                )
                .is_err()
        );
        assert!(matches!(
            state.record_goal_usage(
                "goal-usage-1",
                GoalUsageDelta {
                    tokens: 1,
                    elapsed_seconds: 1,
                },
            ),
            Err(RuntimeStateError::Terminal { entity: "Goal" })
        ));
        let cleared = state.clear_goal("goal-clear-1").expect("终态 Goal 应清除");
        assert_eq!(cleared.current.revision, 4);
        assert!(cleared.current.goal.is_none());
        let recreated = state
            .create_goal(
                "goal-create-3",
                GoalDraft {
                    title: "下一目标".to_owned(),
                    objective: "继续".to_owned(),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
            .expect("清除后应重新创建");
        assert_eq!(recreated.current.revision, 5);
    }

    /// Goal 阻塞必须带原因，用量只累计明确增量。
    #[test]
    fn goal_block_requires_reason_and_usage_is_explicit() {
        let state = InMemoryRuntimeState::new();
        state
            .create_goal(
                "goal-create-usage",
                GoalDraft {
                    title: "验证".to_owned(),
                    objective: "验证用量".to_owned(),
                    description: None,
                    token_budget: Some(100),
                    progress_percent: None,
                },
            )
            .expect("Goal 应创建");
        assert!(
            state
                .transition_goal(
                    "goal-transition-invalid",
                    GoalTransition {
                        status: GoalStatus::Blocked,
                        blocked_reason: Some("  ".to_owned()),
                        completion_evidence: None,
                    },
                )
                .is_err()
        );
        let usage = state
            .record_goal_usage(
                "goal-usage-explicit",
                GoalUsageDelta {
                    tokens: 25,
                    elapsed_seconds: 3,
                },
            )
            .expect("Goal 用量应累计");
        let goal = usage.current.goal.expect("Goal 应存在");
        assert_eq!(goal.tokens_used, 25);
        assert_eq!(goal.time_used_seconds, 3);
        assert_eq!(goal.usage_ratio(), Some(0.25));
        let unchanged = state
            .record_goal_usage("goal-usage-zero", GoalUsageDelta::default())
            .expect("零用量增量应幂等");
        assert!(!unchanged.changed);
        assert_eq!(unchanged.current.revision, 2);
    }

    /// Goal 用量任一计数溢出时必须保持整个内存快照不变。
    #[test]
    fn goal_usage_overflow_is_atomic() {
        let state = InMemoryRuntimeState::new();
        state
            .create_goal(
                "goal-create-overflow",
                GoalDraft {
                    title: "溢出原子性".to_owned(),
                    objective: "验证任一计数失败都不留下部分写入".to_owned(),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
            .expect("Goal 应创建");
        {
            let mut snapshot = state.goal.write().expect("Goal 测试锁应可写");
            snapshot
                .goal
                .as_mut()
                .expect("Goal 应存在")
                .time_used_seconds = u64::MAX;
        }
        let before = state.goal_snapshot().expect("溢出前快照应读取");
        assert!(matches!(
            state.record_goal_usage(
                "goal-usage-overflow",
                GoalUsageDelta {
                    tokens: 1,
                    elapsed_seconds: 1,
                },
            ),
            Err(RuntimeStateError::CounterOverflow {
                counter: "Goal 时间"
            })
        ));
        assert_eq!(state.goal_snapshot().expect("溢出后快照应读取"), before);
    }

    /// 计划文档必须按 Session 和 Agent 隔离并保持幂等版本。
    #[test]
    fn plan_state_is_isolated_and_idempotent() {
        let state = InMemoryRuntimeState::new();
        let session_a = session("session-a");
        let session_b = session("session-b");
        let root = agent("root");
        let first = state
            .replace_plan(
                "plan-replace-1",
                &session_a,
                &root,
                "# 计划\n步骤".to_owned(),
            )
            .expect("计划应写入");
        assert_eq!(first.current.revision, 1);
        let repeated = state
            .replace_plan(
                "plan-replace-2",
                &session_a,
                &root,
                "# 计划\n步骤".to_owned(),
            )
            .expect("重复计划应成功");
        assert!(!repeated.changed);
        assert_eq!(repeated.current.revision, 1);
        assert!(
            state
                .plan_snapshot(&session_b, &root)
                .expect("另一 Session 快照应读取")
                .content
                .is_none()
        );
        let cleared = state
            .clear_plan("plan-clear-1", &session_a, &root)
            .expect("计划应清除");
        assert_eq!(cleared.current.revision, 2);
        assert!(cleared.current.content.is_none());
    }

    /// 状态操作标识只能绑定一份规范载荷，并在后续状态变化后仍能识别重试。
    #[test]
    fn operation_receipts_deduplicate_retries_and_reject_payload_conflicts() {
        let state = InMemoryRuntimeState::new();
        let initial_todos = vec![todo("分析", TodoStatus::InProgress)];
        state
            .replace_todos("todo-retry", initial_todos.clone())
            .expect("首次 Todo 应成功");
        let todo_retry = state
            .replace_todos("todo-retry", initial_todos)
            .expect("相同 Todo 操作应去重");
        assert!(!todo_retry.changed);
        assert!(matches!(
            state.replace_todos("todo-retry", vec![todo("不同任务", TodoStatus::InProgress)]),
            Err(RuntimeStateError::Conflict { .. })
        ));

        let draft = GoalDraft {
            title: "幂等目标".to_owned(),
            objective: "验证后续状态变化后的创建重试".to_owned(),
            description: None,
            token_budget: None,
            progress_percent: None,
        };
        state
            .create_goal("goal-create-retry", draft.clone())
            .expect("首次 Goal 创建应成功");
        state
            .update_goal(
                "goal-update-after-create",
                GoalPatch {
                    progress_percent: Some(Some(50)),
                    ..GoalPatch::default()
                },
            )
            .expect("后续 Goal 更新应成功");
        let create_retry = state
            .create_goal("goal-create-retry", draft)
            .expect("原始 Goal 创建操作应由收据去重");
        assert!(!create_retry.changed);
        assert_eq!(create_retry.current.revision, 2);
        assert_eq!(
            create_retry
                .current
                .goal
                .as_ref()
                .and_then(|goal| goal.progress_percent),
            Some(50)
        );

        let delta = GoalUsageDelta {
            tokens: 7,
            elapsed_seconds: 2,
        };
        state
            .record_goal_usage("goal-usage-retry", delta)
            .expect("首次 Goal 用量应成功");
        let usage_retry = state
            .record_goal_usage("goal-usage-retry", delta)
            .expect("相同 Goal 用量应去重");
        assert!(!usage_retry.changed);
        assert_eq!(
            usage_retry
                .current
                .goal
                .as_ref()
                .map(|goal| goal.tokens_used),
            Some(7)
        );
        assert!(matches!(
            state.record_goal_usage(
                "goal-usage-retry",
                GoalUsageDelta {
                    tokens: 8,
                    elapsed_seconds: 2,
                }
            ),
            Err(RuntimeStateError::Conflict { .. })
        ));

        let completion = GoalTransition {
            status: GoalStatus::Completed,
            blocked_reason: None,
            completion_evidence: Some("状态与持久化回归均通过".to_owned()),
        };
        state
            .transition_goal("goal-complete-retry", completion.clone())
            .expect("首次 Goal 完成应成功");
        let completion_retry = state
            .transition_goal("goal-complete-retry", completion)
            .expect("相同完成证据应去重");
        assert!(!completion_retry.changed);
        assert!(matches!(
            state.transition_goal(
                "goal-complete-retry",
                GoalTransition {
                    status: GoalStatus::Completed,
                    blocked_reason: None,
                    completion_evidence: Some("不同完成证据".to_owned()),
                },
            ),
            Err(RuntimeStateError::Conflict { .. })
        ));

        let session = session("receipt-session");
        let root = agent("root");
        state
            .replace_plan("plan-retry", &session, &root, "第一版计划".to_owned())
            .expect("首次 Plan 应成功");
        state
            .replace_plan(
                "plan-later-change",
                &session,
                &root,
                "第二版计划".to_owned(),
            )
            .expect("后续 Plan 应成功");
        let plan_retry = state
            .replace_plan("plan-retry", &session, &root, "第一版计划".to_owned())
            .expect("原始 Plan 操作应由收据去重");
        assert!(!plan_retry.changed);
        assert_eq!(plan_retry.current.content.as_deref(), Some("第二版计划"));
        assert!(matches!(
            state.replace_plan("plan-retry", &session, &root, "冲突计划".to_owned(),),
            Err(RuntimeStateError::Conflict { .. })
        ));
    }
}
