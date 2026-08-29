//! 会话级 Agent 路径注册表(双轨寻址,对齐 Codex AgentPath)。
//!
//! 单层委派语义下路径只有两级:`/root`(父)与 `/root/{name}`(子代理)。
//! 内部身份始终是 child_thread_id(UUIDv7);路径仅用于消息头、提示词与
//! 工具入参的友好寻址。不持久化——会话重启后路径失效,由 ListAgents
//! 输出 UUID 兜底。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Default)]
pub struct AgentPathRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    path_to_thread: HashMap<String, String>,
    thread_to_path: HashMap<String, String>,
}

impl AgentPathRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 path ↔ thread_id,返回最终生效路径。
    /// 同 (path, thread_id) 幂等(resume 复注册);路径被其他线程占用时
    /// 追加 `-2`/`-3` 后缀,与昵称分配器的风格一致。
    pub fn register(&self, base_path: &str, thread_id: &str) -> String {
        let mut inner = self.inner.lock();
        if inner.path_to_thread.get(base_path) == Some(&thread_id.to_string()) {
            return base_path.to_string();
        }
        let mut candidate = base_path.to_string();
        let mut suffix = 2;
        while let Some(occupied) = inner.path_to_thread.get(&candidate) {
            if occupied == thread_id {
                return candidate;
            }
            candidate = format!("{base_path}-{suffix}");
            suffix += 1;
        }
        inner
            .path_to_thread
            .insert(candidate.clone(), thread_id.to_string());
        inner
            .thread_to_path
            .insert(thread_id.to_string(), candidate.clone());
        candidate
    }

    /// 路径 → thread_id;未注册的路径返回 None(调用方给出模型可读错误)。
    pub fn thread_id_for_path(&self, path: &str) -> Option<String> {
        self.inner.lock().path_to_thread.get(path).cloned()
    }

    /// thread_id → 当前路径(消息头 / ListAgents 展示用)。
    pub fn path_of(&self, thread_id: &str) -> Option<String> {
        self.inner.lock().thread_to_path.get(thread_id).cloned()
    }
}

/// 会话内共享的路径注册表句柄。
pub type SharedAgentPathRegistry = Arc<AgentPathRegistry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_returns_base_path_when_free() {
        let registry = AgentPathRegistry::new();
        assert_eq!(
            registry.register("/root/explorer", "thread-1"),
            "/root/explorer"
        );
    }

    #[test]
    fn register_is_idempotent_for_same_pair() {
        let registry = AgentPathRegistry::new();
        registry.register("/root/explorer", "thread-1");
        assert_eq!(
            registry.register("/root/explorer", "thread-1"),
            "/root/explorer"
        );
    }

    #[test]
    fn colliding_path_gets_disambiguation_suffix() {
        let registry = AgentPathRegistry::new();
        assert_eq!(
            registry.register("/root/explorer", "thread-1"),
            "/root/explorer"
        );
        assert_eq!(
            registry.register("/root/explorer", "thread-2"),
            "/root/explorer-2"
        );
        assert_eq!(
            registry.thread_id_for_path("/root/explorer"),
            Some("thread-1".to_string())
        );
        assert_eq!(
            registry.thread_id_for_path("/root/explorer-2"),
            Some("thread-2".to_string())
        );
    }

    #[test]
    fn resolve_unknown_path_returns_none() {
        let registry = AgentPathRegistry::new();
        assert_eq!(registry.thread_id_for_path("/root/ghost"), None);
    }

    #[test]
    fn path_of_looks_up_by_thread_id() {
        let registry = AgentPathRegistry::new();
        registry.register("/root/explorer", "thread-1");
        assert_eq!(registry.path_of("thread-1"), Some("/root/explorer".to_string()));
        assert_eq!(registry.path_of("thread-9"), None);
    }
}
