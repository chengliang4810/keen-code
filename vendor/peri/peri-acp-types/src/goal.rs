//! Goal DTO contract for the ACP surface.
//!
//! 本模块定义 KeenCode 桌面端与 peri ACP 之间的 Goal wire 结构。
//! 纯 serde，不依赖 peri-agent；`ThreadGoal` 转换实现位于 peri-acp。

use serde::{Deserialize, Serialize};

/// Goal 变更类型（`goal_changed` 事件的 change 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalChangeKind {
    /// 创建（upsert 且此前无 goal）
    Created,
    /// 完整更新（upsert 且此前已有 goal）
    Updated,
    /// 状态迁移（transition）
    Transitioned,
}

/// Goal 记录的当前 ACP wire 结构（snake_case）。
///
/// peri 模型为每个 Session 单例 Goal，使用三态状态机和 project 作用域。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GoalRecordDto {
    /// Goal 唯一标识（ThreadGoal.goal_id）。
    pub id: String,
    /// 目标标题 —— 由 objective 镜像。
    pub title: String,
    /// 作用域 —— peri 恒为 "project"。
    pub scope: String,
    /// 生命周期状态："active" | "completed" | "blocked"
    /// （peri 内部 GoalStatus 序列化为 "complete"，此处映射为前端契约的 "completed"）。
    pub status: String,
    /// 目标描述 —— 由 objective 镜像。
    pub description: Option<String>,
    /// 进度百分比（0-100），token_budget 未设置时为 None。
    pub progress_percent: Option<f64>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 最后更新时间（RFC3339）。
    pub updated_at: String,
    /// 目标描述（peri 原生字段）。
    pub objective: String,
    /// Token 预算上限。
    pub token_budget: Option<u64>,
    /// 已用 token。
    pub tokens_used: u64,
    /// 已用时间（秒）。
    pub time_used_seconds: u64,
    /// 阻塞原因（仅 Blocked 状态有值）。
    pub blocked_reason: Option<String>,
}

impl GoalRecordDto {
    /// 从 peri 原生模型构建 wire DTO。
    ///
    /// 转换逻辑放在 peri-acp（本 crate 不依赖 peri-agent）：
    /// 调用方把字段逐一传入。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        objective: String,
        status: String,
        token_budget: Option<u64>,
        tokens_used: u64,
        time_used_seconds: u64,
        blocked_reason: Option<String>,
        created_at: String,
        updated_at: String,
    ) -> Self {
        let progress_percent = token_budget
            .filter(|&b| b > 0)
            .map(|b| tokens_used as f64 / b as f64 * 100.0);
        Self {
            id,
            title: objective.clone(),
            scope: "project".to_string(),
            status,
            description: Some(objective.clone()),
            progress_percent,
            created_at,
            updated_at,
            objective,
            token_budget,
            tokens_used,
            time_used_seconds,
            blocked_reason,
        }
    }
}

#[cfg(test)]
#[path = "goal_test.rs"]
mod tests;
