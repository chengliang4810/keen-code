//! GoalStateView trait — 供 GoalMiddleware 依赖注入的抽象接口。
//!
//! GoalState（peri-acp）实现此 trait，GoalMiddleware（peri-middlewares）
//! 依赖 trait 而非具体类型，避免 peri-middlewares → peri-acp 循环依赖。

use super::model::GoalStatus;

/// 只读快照（与 GoalSnapshot 平行，但定义在 peri-agent 层避免依赖）
#[derive(Debug, Clone, Default)]
pub struct GoalViewSnapshot {
    pub objective: Option<String>,
    pub status: Option<GoalStatus>,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub objective_just_updated: bool,
}

/// GoalState 的抽象视图（供 middleware 依赖注入）
pub trait GoalStateView: Send + Sync {
    /// 只读快照
    fn snapshot(&self) -> GoalViewSnapshot;

    /// 消费 objective_just_updated 标志
    fn consume_objective_updated(&self) -> bool;
}
