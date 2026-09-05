//! MiddlewareState trait — middleware 钩子的状态上下文
//!
//! object-safe trait，让 `trait Middleware` 接收 `&mut dyn MiddlewareState`。
//!
//! ## 与 `AgentState` 的关系
//!
//! - `AgentState`（`crate::agent::state::AgentState`）是唯一实现者
//! - middleware_runner 通过此 trait 桥接 v2 stages ↔ middleware 钩子

use crate::{agent::state::AgentState, agent::token::TokenTracker, messages::BaseMessage};

/// Middleware 在每次钩子调用中看到的状态上下文。
///
/// object-safe：无 `Clone`/`'static` 约束、无泛型方法（`impl Into<String>` 改为 `String`）。
/// 这让 `trait Middleware` 可以改为非泛型，钩子签名用 `&mut dyn MiddlewareState`。
pub trait MiddlewareState: Send + Sync {
    fn cwd(&self) -> &str;

    fn messages(&self) -> &[BaseMessage];
    fn add_message(&mut self, message: BaseMessage);
    fn prepend_message(&mut self, message: BaseMessage);
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage>;

    fn current_step(&self) -> usize;

    fn get_context(&self, key: &str) -> Option<&str>;
    fn set_context(&mut self, key: String, value: String);

    fn token_tracker(&self) -> &TokenTracker;
    fn token_tracker_mut(&mut self) -> &mut TokenTracker;

    fn push_recall(&mut self, item: String);
    fn drain_recall(&mut self) -> Vec<String>;

    fn ancestor_len(&self) -> usize;

    /// 返回共享的 v2 MessageQueue 引用（用于 goal steering / stop-hook feedback 等异步注入）
    ///
    /// 实现者必须返回**同一个** session 级实例（不能每次新建）。
    /// middleware push 的消息（Info / Defer）由 Receive / End 阶段统一消费。
    fn v2_queue(&self) -> &crate::session::MessageQueue;
}

/// `AgentState` 唯一实现 `MiddlewareState`。
///
/// 通过显式 `AgentState::method(self, ...)` 调用避免与 `MiddlewareState` 自身方法递归。
impl MiddlewareState for AgentState {
    fn cwd(&self) -> &str {
        AgentState::cwd(self)
    }

    fn messages(&self) -> &[BaseMessage] {
        AgentState::messages(self)
    }

    fn add_message(&mut self, message: BaseMessage) {
        AgentState::add_message(self, message);
    }

    fn prepend_message(&mut self, message: BaseMessage) {
        AgentState::prepend_message(self, message);
    }

    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        AgentState::messages_mut(self)
    }

    fn current_step(&self) -> usize {
        AgentState::current_step(self)
    }

    fn get_context(&self, key: &str) -> Option<&str> {
        AgentState::get_context(self, key)
    }

    fn set_context(&mut self, key: String, value: String) {
        AgentState::set_context(self, key, value);
    }

    fn token_tracker(&self) -> &TokenTracker {
        AgentState::token_tracker(self)
    }

    fn token_tracker_mut(&mut self) -> &mut TokenTracker {
        AgentState::token_tracker_mut(self)
    }

    fn push_recall(&mut self, item: String) {
        AgentState::push_recall(self, item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        AgentState::drain_recall(self)
    }

    fn ancestor_len(&self) -> usize {
        AgentState::ancestor_len(self)
    }

    fn v2_queue(&self) -> &crate::session::MessageQueue {
        AgentState::v2_queue(self)
    }
}
