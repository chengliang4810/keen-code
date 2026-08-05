//! Goal 持久化存储抽象。
//!
//! `GoalStore` trait 定义 save/load/delete 接口，供 ACP 层注入。
//! `InMemoryGoalStore` 是测试和 fallback 用的纯内存实现。
//! SQLite 实现见 Plan 2。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::model::ThreadGoal;

/// Goal 持久化存储 trait
#[async_trait]
pub trait GoalStore: Send + Sync {
    /// 保存（upsert）goal 到指定 thread
    async fn save(&self, thread_id: &str, goal: ThreadGoal) -> Result<(), GoalStoreError>;

    /// 加载指定 thread 的 goal，无 goal 返回 None
    async fn load(&self, thread_id: &str) -> Result<Option<ThreadGoal>, GoalStoreError>;

    /// 删除指定 thread 的 goal
    async fn delete(&self, thread_id: &str) -> Result<(), GoalStoreError>;
}

/// Store 错误类型
#[derive(Debug, thiserror::Error)]
pub enum GoalStoreError {
    #[error("存储 IO 错误: {0}")]
    Io(String),
    #[error("序列化错误: {0}")]
    Serde(String),
}

/// 纯内存实现（测试 + fallback）
pub struct InMemoryGoalStore {
    inner: Arc<RwLock<HashMap<String, ThreadGoal>>>,
}

impl InMemoryGoalStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryGoalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GoalStore for InMemoryGoalStore {
    async fn save(&self, thread_id: &str, goal: ThreadGoal) -> Result<(), GoalStoreError> {
        self.inner.write().insert(thread_id.to_string(), goal);
        Ok(())
    }

    async fn load(&self, thread_id: &str) -> Result<Option<ThreadGoal>, GoalStoreError> {
        Ok(self.inner.read().get(thread_id).cloned())
    }

    async fn delete(&self, thread_id: &str) -> Result<(), GoalStoreError> {
        self.inner.write().remove(thread_id);
        Ok(())
    }
}

#[cfg(test)]
#[path = "store_test.rs"]
mod tests;
