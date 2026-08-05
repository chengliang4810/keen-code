//! 一次性 hook 状态跟踪器。
//!
//! 将每个已触发的 once hook 用稳定 key 记录到 HashSet，
//! 后续命中同一 key 的 hook 会被跳过。

use std::collections::HashSet;

use parking_lot::Mutex;

use crate::hooks::types::{HookType, RegisteredHook};

/// Once-fired 状态管理：用 `HashSet<String>` 跟踪一次性 hook。
///
/// 由 `HookMiddleware` 通过 `Arc` 共享，可被 dispatcher 与 standalone
/// 路径同时访问。所有方法均为 `&self`，内部用 `parking_lot::Mutex`。
pub struct OnceTracker {
    fired: Mutex<HashSet<String>>,
}

impl OnceTracker {
    pub fn new() -> Self {
        Self {
            fired: Mutex::new(HashSet::new()),
        }
    }

    /// 判断给定 hook 是否是一次性 hook（依据 `HookType::is_once`）。
    pub fn is_once_hook(hook: &HookType) -> bool {
        hook.is_once()
    }

    /// 构造 once key：由 `plugin_id + hook 序列化 + event` 三元组组合，
    /// 保证同一 hook 配置在多次调用间稳定。
    pub fn once_key(registered: &RegisteredHook) -> String {
        format!(
            "{}:{}:{:?}",
            registered.plugin_id,
            serde_json::to_string(&registered.hook).unwrap_or_default(),
            registered.event
        )
    }

    /// 该 once hook 是否已经触发过。
    pub fn was_fired(&self, registered: &RegisteredHook) -> bool {
        let key = Self::once_key(registered);
        self.fired.lock().contains(&key)
    }

    /// 标记该 once hook 已触发。
    pub fn mark_fired(&self, registered: &RegisteredHook) {
        let key = Self::once_key(registered);
        self.fired.lock().insert(key);
    }
}

impl Default for OnceTracker {
    fn default() -> Self {
        Self::new()
    }
}
