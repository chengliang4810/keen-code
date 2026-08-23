//! SessionConfig — 会话级可变配置
//!
//! 与 SessionStore 的不可变 frozen 数据相反，SessionConfig 在会话生命周期内可变。
//! 通过 `Arc<SessionConfig>` 在 Session、Agent、TurnContext 之间共享。
//! 外部写入（cancel、超时和思考配置），内部读取（循环检查）。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Extended thinking 配置（Anthropic）/ reasoning_effort（OpenAI）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 思考预算（token 数）
    pub budget_tokens: u32,
    /// 努力程度（low/medium/high）
    pub effort: String,
}

// ─── SessionConfig ───────────────────────────────────────────────────────────

/// 会话级可变配置
///
/// 内部用 RwLock 包装可变字段，外部通过 Arc 共享。Cancel token 独立持有，
/// 因为它需要原子的 child_token 创建能力（不能在 RwLock 后面）。
#[derive(Debug)]
pub struct SessionConfig {
    /// Cancel token（会话级，派生 turn 级子 token）
    pub cancel_token: Arc<CancellationToken>,
    /// 单 turn 超时（None 表示无超时）
    turn_timeout: RwLock<Option<Duration>>,
    /// Thinking 配置（None 表示禁用 extended thinking）
    thinking: RwLock<Option<ThinkingConfig>>,
    /// 最大 ReAct 迭代数（防死循环）
    max_iterations: RwLock<usize>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cancel_token: Arc::new(CancellationToken::new()),
            turn_timeout: RwLock::new(None),
            thinking: RwLock::new(None),
            max_iterations: RwLock::new(500),
        }
    }
}

impl SessionConfig {
    /// 创建新 SessionConfig
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前 turn 超时
    pub fn turn_timeout(&self) -> Option<Duration> {
        *self.turn_timeout.read()
    }

    /// 设置 turn 超时
    pub fn set_turn_timeout(&self, timeout: Option<Duration>) {
        *self.turn_timeout.write() = timeout;
    }

    /// 当前 Thinking 配置
    pub fn thinking(&self) -> Option<ThinkingConfig> {
        self.thinking.read().clone()
    }

    /// 设置 Thinking 配置
    pub fn set_thinking(&self, thinking: Option<ThinkingConfig>) {
        *self.thinking.write() = thinking;
    }

    /// 最大 ReAct 迭代数
    pub fn max_iterations(&self) -> usize {
        *self.max_iterations.read()
    }

    /// 设置最大迭代数
    pub fn set_max_iterations(&self, n: usize) {
        *self.max_iterations.write() = n;
    }

    /// 是否已取消
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// 触发 cancel
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
