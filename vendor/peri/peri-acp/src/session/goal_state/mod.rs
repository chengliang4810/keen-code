//! GoalState — goal 子系统的并发状态机。
//!
//! 基于 `Arc<RwLock<GoalStateInner>>` + `parking_lot::RwLock`（短锁无 await）。
//! store 写入失败时退化为纯内存模式（snapshot 读仍可用），不阻塞 agent。
//!
//! 并发模型：read-and-reset + epoch（本 Task 先实现基础读写，account_progress 的
//! read-and-reset 在 Task 6 实现）。

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::goal::{GoalStatus, GoalStore, GoalStoreError, ThreadGoal};

/// 每个 Session 最多保留的 Goal requestNonce 回执数。
///
/// 回执只服务于短时间内的重试去重；固定上限避免客户端持续生成 nonce
/// 令 Session 长期持有无界的请求参数、Goal 快照和文本。
const GOAL_REQUEST_RECEIPT_CAPACITY: usize = 64;

/// Goal 快照（只读视图，供 middleware / TUI 读取）
#[derive(Debug, Clone, Default)]
pub struct GoalSnapshot {
    /// 当前 Session Goal 集合的单调修订号。
    pub revision: u64,
    pub goal_id: Option<String>,
    pub objective: Option<String>,
    pub status: Option<GoalStatus>,
    pub token_budget: Option<u64>,
    /// 进入 blocked 状态时记录的原因。
    pub blocked_reason: Option<String>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    /// set_goal / edit 后置 true，middleware 注入后清零
    pub objective_just_updated: bool,
}

/// 一次 Goal 写操作提交后的不可变结果。
///
/// Host 直接使用这里的快照构造响应，避免写操作完成后又读取到下一次并发
/// 写入的 Goal。`deduplicated` 只在 requestNonce 重放响应中为 true。
#[derive(Debug, Clone)]
pub struct GoalMutationResult {
    /// 写操作完成后的集合修订号。
    pub revision: u64,
    /// 与该修订号对应的 Goal 快照。
    pub snapshot: GoalSnapshot,
    /// 是否是已完成请求的幂等重放。
    pub deduplicated: bool,
}

/// requestNonce 绑定的完整操作身份，防止同一 nonce 被另一种请求误用。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GoalMutationRequest {
    /// 创建或替换 Goal 的请求身份。
    Upsert {
        objective: String,
        token_budget: Option<u64>,
        expected_revision: Option<u64>,
    },
    /// Goal 状态迁移的请求身份。
    Transition {
        goal_id: Option<String>,
        expected_revision: Option<u64>,
        target: GoalStatus,
        reason: String,
    },
    /// 清除 Goal 的请求身份。
    Clear {
        goal_id: Option<String>,
        expected_revision: Option<u64>,
    },
}

/// 已在内存层提交的 requestNonce 及其原始响应。
#[derive(Debug, Clone)]
struct GoalMutationReceipt {
    request: GoalMutationRequest,
    result: GoalMutationResult,
}

/// 按最近访问顺序保存有限数量的 requestNonce 回执。
///
/// `HashMap` 负责 O(1) 查找，`VecDeque` 记录从旧到新的顺序。更新已有 nonce
/// 先移除旧位置再追加到队尾，保证同一 nonce 不会重复占用顺序槽位；超过容量
/// 时只淘汰队首最旧回执。
struct GoalMutationReceipts {
    entries: HashMap<String, GoalMutationReceipt>,
    order: VecDeque<String>,
}

impl GoalMutationReceipts {
    /// 创建空的有界回执缓存。
    fn new() -> Self {
        debug_assert!(GOAL_REQUEST_RECEIPT_CAPACITY > 0);
        Self {
            entries: HashMap::with_capacity(GOAL_REQUEST_RECEIPT_CAPACITY),
            order: VecDeque::with_capacity(GOAL_REQUEST_RECEIPT_CAPACITY),
        }
    }

    /// 查找 nonce 对应的回执，不改变顺序。
    fn get(&self, nonce: &str) -> Option<&GoalMutationReceipt> {
        self.entries.get(nonce)
    }

    /// 将已命中的 nonce 移到队尾，表示它仍是最近使用的请求。
    fn touch(&mut self, nonce: &str) {
        if !self.entries.contains_key(nonce) {
            return;
        }
        self.remove_from_order(nonce);
        self.order.push_back(nonce.to_owned());
    }

    /// 插入或更新回执，并在超过容量时淘汰最旧项。
    fn insert(&mut self, nonce: String, receipt: GoalMutationReceipt) {
        self.remove_from_order(&nonce);
        self.entries.insert(nonce.clone(), receipt);
        self.order.push_back(nonce);

        while self.entries.len() > GOAL_REQUEST_RECEIPT_CAPACITY {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    /// 删除 nonce 在顺序队列中的全部旧位置，防止历史重复项残留。
    fn remove_from_order(&mut self, nonce: &str) {
        while let Some(index) = self.order.iter().position(|item| item == nonce) {
            self.order.remove(index);
        }
    }

    #[cfg(test)]
    /// 返回当前回执数，仅供容量边界测试使用。
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    /// 判断测试用 nonce 是否仍在回执缓存中。
    fn contains(&self, nonce: &str) -> bool {
        self.entries.contains_key(nonce)
    }
}

/// Goal 控制面错误；Host 将并发冲突与参数/存储错误映射为 ACP 错误码。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GoalMutationError {
    /// 当前 Session 没有 Goal。
    #[error("当前没有 Goal")]
    NoGoal,
    /// 请求使用了另一个 Goal 的身份。
    #[error("goalId 不匹配：请求 {expected}，当前 {actual}")]
    GoalIdMismatch { expected: String, actual: String },
    /// 请求基于过期的集合修订号。
    #[error("revision 冲突：期望 {expected}，当前 {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    /// 状态机拒绝该迁移。
    #[error("非法状态转换：{from} → {to}（终态不可恢复）")]
    InvalidTransition { from: GoalStatus, to: GoalStatus },
    /// Blocked 必须有非空原因。
    #[error("Blocked 状态必须附带 reason")]
    BlockedReasonRequired,
    /// 内部 create_goal 不允许覆盖现有 Goal。
    #[error("goal 已存在，请先 clear 后重建")]
    AlreadyExists,
    /// 同一 requestNonce 被用于不同的操作。
    #[error("requestNonce 已用于不同的 Goal 请求：{nonce}")]
    NonceConflict { nonce: String },
    /// 修订号溢出；理论上不会在单个 Session 生命周期内发生。
    #[error("Goal revision 已达到最大值")]
    RevisionOverflow,
    /// GoalStore 写入失败。
    #[error("Goal 存储失败：{0}")]
    Store(String),
}

impl GoalSnapshot {
    /// 是否有活跃的 goal
    pub fn has_active_goal(&self) -> bool {
        self.status == Some(GoalStatus::Active)
    }

    /// usage 百分比（0.0-1.0），budget=None 或 0 时返回 None
    pub fn usage_pct(&self) -> Option<f32> {
        self.token_budget
            .filter(|&b| b > 0)
            .map(|b| self.tokens_used as f32 / b as f32)
    }
}

/// 内部可变状态（受 RwLock 保护）
struct GoalStateInner {
    goal: Option<ThreadGoal>,
    /// Session 级 Goal 集合修订号；clear 也只递增、不回退。
    revision: u64,
    /// set_goal / clear_goal 后置 true，GoalMiddleware 注入后清零
    objective_just_updated: bool,
    store: Arc<dyn GoalStore>,
    thread_id: String,
    /// 机制 3：continuation 期间用户消息缓冲（多条覆盖，只保留最后一条）
    pending_user_message: Option<String>,
    /// 待 flush 的 token 增量
    pending_token_delta: u64,
    /// 待 flush 的 time 增量（秒）
    pending_time_delta_seconds: u64,
    /// 已提交的 nonce 请求缓存；随 Session 句柄生命周期存在，且有固定容量。
    request_receipts: GoalMutationReceipts,
}

/// 并发安全的状态句柄
#[derive(Clone)]
pub struct GoalState {
    inner: Arc<RwLock<GoalStateInner>>,
    /// 串行化“内存状态变更 + store 写入”，避免异步 store 完成顺序倒置。
    mutation_lock: Arc<tokio::sync::Mutex<()>>,
}

impl GoalState {
    pub fn new(store: Arc<dyn GoalStore>, thread_id: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(GoalStateInner {
                goal: None,
                revision: 0,
                objective_just_updated: false,
                store,
                thread_id,
                pending_user_message: None,
                pending_token_delta: 0,
                pending_time_delta_seconds: 0,
                request_receipts: GoalMutationReceipts::new(),
            })),
            mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// 规范化 requestNonce；空白 nonce 不提供幂等语义。
    fn normalize_nonce(request_nonce: Option<&str>) -> Option<String> {
        request_nonce
            .map(str::trim)
            .filter(|nonce| !nonce.is_empty())
            .map(ToOwned::to_owned)
    }

    /// 检查 nonce 是否可以直接重放原始结果。
    fn replay_nonce(
        guard: &mut GoalStateInner,
        nonce: Option<&str>,
        request: &GoalMutationRequest,
    ) -> Result<Option<GoalMutationResult>, GoalMutationError> {
        let Some(nonce) = nonce else {
            return Ok(None);
        };
        let Some(receipt) = guard.request_receipts.get(nonce).cloned() else {
            return Ok(None);
        };
        if receipt.request != *request {
            return Err(GoalMutationError::NonceConflict {
                nonce: nonce.to_string(),
            });
        }
        guard.request_receipts.touch(nonce);
        let mut result = receipt.result;
        result.deduplicated = true;
        Ok(Some(result))
    }

    /// 记录内存层已提交请求，后续相同 nonce 只返回原始提交结果。
    ///
    /// 调用方在异步 `GoalStore` 写入前记录回执；因此 store 失败时首次调用仍
    /// 返回存储错误，但同 nonce 重试会重放已提交的内存结果，不再次执行 mutation。
    fn remember_nonce(
        guard: &mut GoalStateInner,
        nonce: Option<String>,
        request: GoalMutationRequest,
        result: &GoalMutationResult,
    ) {
        if let Some(nonce) = nonce {
            guard.request_receipts.insert(
                nonce,
                GoalMutationReceipt {
                    request,
                    result: result.clone(),
                },
            );
        }
    }

    /// 将 Session 修订号递增一次，拒绝极端溢出而不是回绕到旧版本。
    fn next_revision(guard: &mut GoalStateInner) -> Result<u64, GoalMutationError> {
        guard.revision = guard
            .revision
            .checked_add(1)
            .ok_or(GoalMutationError::RevisionOverflow)?;
        Ok(guard.revision)
    }

    /// 在已持有内部锁时构造与当前修订号一致的快照。
    fn snapshot_locked(guard: &GoalStateInner) -> GoalSnapshot {
        match &guard.goal {
            Some(g) => GoalSnapshot {
                revision: guard.revision,
                goal_id: Some(g.goal_id.clone()),
                objective: Some(g.objective.clone()),
                status: Some(g.status),
                token_budget: g.token_budget,
                blocked_reason: g.blocked_reason.clone(),
                tokens_used: g.accounting.tokens_used,
                time_used_seconds: g.accounting.time_used_seconds,
                objective_just_updated: guard.objective_just_updated,
            },
            None => GoalSnapshot {
                revision: guard.revision,
                objective_just_updated: guard.objective_just_updated,
                ..Default::default()
            },
        }
    }

    /// 将带有状态错误的 mutation 错误转换为旧 set_goal/clear API 的存储错误。
    fn as_store_error(error: GoalMutationError) -> GoalStoreError {
        GoalStoreError::Io(error.to_string())
    }

    /// 创建或替换 Goal，并按 expectedRevision 与 requestNonce 保证并发契约。
    pub async fn upsert_goal(
        &self,
        objective: String,
        token_budget: Option<u64>,
        expected_revision: Option<u64>,
        request_nonce: Option<&str>,
    ) -> Result<GoalMutationResult, GoalMutationError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let request = GoalMutationRequest::Upsert {
            objective: objective.clone(),
            token_budget,
            expected_revision,
        };
        let nonce = Self::normalize_nonce(request_nonce);
        let (thread_id, store, new_goal, result) = {
            let mut guard = self.inner.write();
            if let Some(result) = Self::replay_nonce(&mut guard, nonce.as_deref(), &request)? {
                return Ok(result);
            }
            if let Some(expected) = expected_revision {
                if expected != guard.revision {
                    return Err(GoalMutationError::RevisionConflict {
                        expected,
                        actual: guard.revision,
                    });
                }
            }

            let new_goal = ThreadGoal::new(objective, token_budget);
            guard.goal = Some(new_goal.clone());
            guard.objective_just_updated = true;
            Self::next_revision(&mut guard)?;
            let result = GoalMutationResult {
                revision: guard.revision,
                snapshot: Self::snapshot_locked(&guard),
                deduplicated: false,
            };
            Self::remember_nonce(&mut guard, nonce, request, &result);
            (
                guard.thread_id.clone(),
                guard.store.clone(),
                new_goal,
                result,
            )
        };

        if let Err(error) = store.save(&thread_id, new_goal).await {
            tracing::warn!(error = %error, "GoalState: upsert store 保存失败，保留内存镜像");
            return Err(GoalMutationError::Store(error.to_string()));
        }
        Ok(result)
    }

    /// set_goal：UPSERT（新 goal_id），触发 objective_updated。
    /// store 写入失败不回滚内存镜像（内存优于 store 原则）。
    pub async fn set_goal(
        &self,
        objective: String,
        token_budget: Option<u64>,
    ) -> Result<(), peri_acp_types::goal::GoalStoreError> {
        self.upsert_goal(objective, token_budget, None, None)
            .await
            .map(|_| ())
            .map_err(Self::as_store_error)
    }

    /// clear：清空 Goal，并递增集合修订号（空集合 clear 为幂等 no-op）。
    pub async fn clear(&self) -> Result<(), peri_acp_types::goal::GoalStoreError> {
        self.clear_with_preconditions(None, None, None)
            .await
            .map(|_| ())
            .map_err(Self::as_store_error)
    }

    /// 按 Goal 身份、集合修订号和 requestNonce 清除 Goal。
    pub async fn clear_with_preconditions(
        &self,
        expected_goal_id: Option<&str>,
        expected_revision: Option<u64>,
        request_nonce: Option<&str>,
    ) -> Result<GoalMutationResult, GoalMutationError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let request = GoalMutationRequest::Clear {
            goal_id: expected_goal_id.map(ToOwned::to_owned),
            expected_revision,
        };
        let nonce = Self::normalize_nonce(request_nonce);
        let (thread_id, store, result) = {
            let mut guard = self.inner.write();

            if let Some(result) = Self::replay_nonce(&mut guard, nonce.as_deref(), &request)? {
                return Ok(result);
            }
            if let Some(expected) = expected_goal_id {
                match guard.goal.as_ref() {
                    None => return Err(GoalMutationError::NoGoal),
                    Some(goal) if goal.goal_id != expected => {
                        return Err(GoalMutationError::GoalIdMismatch {
                            expected: expected.to_string(),
                            actual: goal.goal_id.clone(),
                        })
                    }
                    Some(_) => {}
                }
            }
            if let Some(expected) = expected_revision {
                if expected != guard.revision {
                    return Err(GoalMutationError::RevisionConflict {
                        expected,
                        actual: guard.revision,
                    });
                }
            }

            let had_goal = guard.goal.is_some();
            guard.goal = None;
            guard.objective_just_updated = false;
            guard.pending_user_message = None;
            if had_goal {
                Self::next_revision(&mut guard)?;
            }
            let result = GoalMutationResult {
                revision: guard.revision,
                snapshot: Self::snapshot_locked(&guard),
                deduplicated: false,
            };
            Self::remember_nonce(&mut guard, nonce, request, &result);
            (guard.thread_id.clone(), guard.store.clone(), result)
        };

        if let Err(error) = store.delete(&thread_id).await {
            tracing::warn!(error = %error, "GoalState: clear store 删除失败");
            return Err(GoalMutationError::Store(error.to_string()));
        }
        Ok(result)
    }

    /// set_status（简化封装，reason 为空字符串）
    pub async fn set_status(&self, target: GoalStatus) -> Result<(), String> {
        self.set_status_with_reason(target, String::new()).await
    }

    /// set_status 附带 reason（Blocked 必填）
    pub async fn set_status_with_reason(
        &self,
        target: GoalStatus,
        reason: String,
    ) -> Result<(), String> {
        match self.transition_goal(None, None, target, reason, None).await {
            Ok(_) => Ok(()),
            // Legacy middleware API 保留 store 失败时“内存优先”的行为。
            Err(GoalMutationError::Store(_)) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// 按 Goal 身份、集合修订号和 requestNonce 执行状态迁移。
    pub async fn transition_goal(
        &self,
        expected_goal_id: Option<&str>,
        expected_revision: Option<u64>,
        target: GoalStatus,
        reason: String,
        request_nonce: Option<&str>,
    ) -> Result<GoalMutationResult, GoalMutationError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let request = GoalMutationRequest::Transition {
            goal_id: expected_goal_id.map(ToOwned::to_owned),
            expected_revision,
            target,
            reason: reason.clone(),
        };
        let nonce = Self::normalize_nonce(request_nonce);
        let (thread_id, store, goal_clone, result) = {
            let mut guard = self.inner.write();
            if let Some(result) = Self::replay_nonce(&mut guard, nonce.as_deref(), &request)? {
                return Ok(result);
            }
            let current_revision = guard.revision;
            let goal_clone = {
                let goal = guard.goal.as_mut().ok_or(GoalMutationError::NoGoal)?;
                if let Some(expected) = expected_goal_id {
                    if goal.goal_id != expected {
                        return Err(GoalMutationError::GoalIdMismatch {
                            expected: expected.to_string(),
                            actual: goal.goal_id.clone(),
                        });
                    }
                }
                if let Some(expected) = expected_revision {
                    if expected != current_revision {
                        return Err(GoalMutationError::RevisionConflict {
                            expected,
                            actual: current_revision,
                        });
                    }
                }

                if !goal.status.can_transition_to(&target) {
                    return Err(GoalMutationError::InvalidTransition {
                        from: goal.status,
                        to: target,
                    });
                }

                // Blocked 必须附带 reason
                if matches!(target, GoalStatus::Blocked) && reason.trim().is_empty() {
                    return Err(GoalMutationError::BlockedReasonRequired);
                }

                goal.status = target;
                goal.updated_at = chrono::Utc::now();
                if matches!(target, GoalStatus::Blocked) {
                    goal.blocked_reason = Some(reason.clone());
                }
                goal.clone()
            };
            // 终态清零 pending_user_message（终态不需要用户消息）
            if target.is_terminal() {
                guard.pending_user_message = None;
            }
            Self::next_revision(&mut guard)?;
            let result = GoalMutationResult {
                revision: guard.revision,
                snapshot: Self::snapshot_locked(&guard),
                deduplicated: false,
            };
            Self::remember_nonce(&mut guard, nonce, request, &result);
            (
                guard.thread_id.clone(),
                guard.store.clone(),
                goal_clone,
                result,
            )
        };

        if let Err(error) = store.save(&thread_id, goal_clone).await {
            tracing::warn!(error = %error, "GoalState: transition store 保存失败，保留内存镜像");
            return Err(GoalMutationError::Store(error.to_string()));
        }
        Ok(result)
    }

    /// 只读快照（短锁，立即释放）
    pub fn snapshot(&self) -> GoalSnapshot {
        let guard = self.inner.read();
        Self::snapshot_locked(&guard)
    }

    /// 消费 objective_just_updated 标志（middleware 注入后调用）
    pub fn consume_objective_updated(&self) -> bool {
        let mut guard = self.inner.write();
        let was_set = guard.objective_just_updated;
        guard.objective_just_updated = false;
        was_set
    }

    /// 机制 3：写入用户消息（覆盖旧值）
    pub fn put_pending_user_message(&self, message: String) {
        self.inner.write().pending_user_message = Some(message);
    }

    /// 机制 3：取出并清空用户消息
    pub fn take_pending_user_message(&self) -> Option<String> {
        self.inner.write().pending_user_message.take()
    }

    /// 记录 token 增量到 pending 缓冲
    pub fn record_token_usage(&self, delta: u64) {
        self.inner.write().pending_token_delta += delta;
    }

    /// 记录时间增量到 pending 缓冲
    pub fn record_time_usage(&self, delta_seconds: u64) {
        self.inner.write().pending_time_delta_seconds += delta_seconds;
    }

    /// flush：将 pending 增量累加到 goal.accounting 并写 store
    pub async fn flush_progress(&self) -> Result<(), String> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let (thread_id, store, goal_clone) = {
            let mut guard = self.inner.write();
            let token_delta = std::mem::take(&mut guard.pending_token_delta);
            let time_delta = std::mem::take(&mut guard.pending_time_delta_seconds);

            if token_delta == 0 && time_delta == 0 {
                return Ok(());
            }

            {
                let goal = match guard.goal.as_mut() {
                    Some(g) => g,
                    None => return Ok(()), // 无 goal，no-op
                };

                goal.accounting.tokens_used += token_delta;
                goal.accounting.time_used_seconds += time_delta;
                goal.updated_at = chrono::Utc::now();
            }
            Self::next_revision(&mut guard).map_err(|error| error.to_string())?;

            let goal_clone = guard
                .goal
                .as_ref()
                .expect("Goal 在 flush 期间不应被清除")
                .clone();
            (guard.thread_id.clone(), guard.store.clone(), goal_clone)
        };

        // best-effort store 写入（短锁已释放）
        let _ = store.save(&thread_id, goal_clone).await;
        Ok(())
    }
}

impl peri_acp_types::goal::GoalStateView for GoalState {
    fn snapshot(&self) -> peri_acp_types::goal::GoalViewSnapshot {
        let snap = self.snapshot();
        peri_acp_types::goal::GoalViewSnapshot {
            objective: snap.objective,
            status: snap.status,
            token_budget: snap.token_budget,
            tokens_used: snap.tokens_used,
            objective_just_updated: snap.objective_just_updated,
        }
    }

    fn consume_objective_updated(&self) -> bool {
        self.consume_objective_updated()
    }
}

#[async_trait]
impl peri_acp_types::goal::GoalController for GoalState {
    async fn create_goal(&self, objective: String) -> Result<(), String> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let (thread_id, store, new_goal) = {
            let mut guard = self.inner.write();
            if guard.goal.is_some() {
                return Err(GoalMutationError::AlreadyExists.to_string());
            }
            // 原子化：检查 + 插入在同一写锁内，消除 TOCTOU 竞态窗口
            let new_goal = ThreadGoal::new(objective, None);
            guard.goal = Some(new_goal.clone());
            guard.objective_just_updated = true;
            Self::next_revision(&mut guard).map_err(|error| error.to_string())?;
            (guard.thread_id.clone(), guard.store.clone(), new_goal)
        };
        // best-effort store 写入（短锁已释放）
        if let Err(e) = store.save(&thread_id, new_goal).await {
            tracing::warn!(error = %e, "GoalState: store save 失败，退化为纯内存模式");
            return Err(e.to_string());
        }
        Ok(())
    }

    async fn complete_goal(&self) -> Result<(), String> {
        self.set_status(GoalStatus::Complete).await
    }

    async fn block_goal(&self, reason: String) -> Result<(), String> {
        self.set_status_with_reason(GoalStatus::Blocked, reason)
            .await
    }

    async fn clear_goal(&self) -> Result<(), String> {
        self.clear().await.map_err(|e| e.to_string())
    }

    fn snapshot(&self) -> peri_acp_types::goal::GoalViewSnapshot {
        peri_acp_types::goal::GoalStateView::snapshot(self)
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
