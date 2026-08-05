use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

use super::types::{PendingTool, ThreadId, ThreadMeta};
use crate::messages::{BaseMessage, MessageId};
use crate::session::MessageFlags;

#[derive(Clone, Debug)]
pub struct CompactionLifecycle {
    pub flag_updates: Vec<(MessageId, MessageFlags)>,
    pub appended_messages: Vec<BaseMessage>,
}

#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// 创建新 thread，返回分配的 ThreadId
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId>;

    /// 追加消息到指定 thread（追加写，不覆盖）
    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()>;

    /// 追加单条消息到指定 thread（默认实现复用 append_messages）
    async fn append_message(&self, id: &ThreadId, message: BaseMessage) -> Result<()> {
        self.append_messages(id, &[message]).await
    }

    /// 加载指定 thread 的全部消息
    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>>;

    /// 加载指定 thread 的元数据
    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta>;

    /// 更新指定 thread 的元数据
    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()>;

    /// 列举所有 thread 元数据，按 updated_at 降序（不含 hidden 的子 agent）
    async fn list_threads(&self) -> Result<Vec<ThreadMeta>>;

    /// 删除指定 thread（包含消息和元数据）
    async fn delete_thread(&self, id: &ThreadId) -> Result<()>;

    /// 更新指定 thread 的标题
    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let mut meta = self.load_meta(id).await?;
        meta.title = Some(title.to_string());
        self.update_meta(id, meta).await
    }

    /// 加载 thread 的完整上下文（含祖先链 + 缓存）
    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>>;

    /// 列举指定父 thread 的直接子 thread
    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 递归列举以 root_id 为根的所有 thread（含自身）
    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 更新 thread 的 agent_status 字段
    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()>;

    /// 清除 thread 的 cached_context
    async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()>;

    /// 按 message_id 列表精确删除消息，并刷新 cached_context。
    async fn delete_messages(&self, thread_id: &ThreadId, message_ids: &[MessageId]) -> Result<()>;

    /// 更新消息的 compact 标记（truncated / excluded / projection directive）
    async fn update_message_flags(
        &self,
        message_id: &MessageId,
        flags: &MessageFlags,
    ) -> Result<()> {
        let _ = (message_id, flags);
        Ok(()) // 默认 no-op
    }

    /// 返回后端是否支持原子 compact lifecycle 提交。
    fn supports_compaction_lifecycle(&self) -> bool {
        false
    }

    /// 原子持久化压缩生命周期的消息标记和追加消息。
    async fn commit_compaction_lifecycle(
        &self,
        thread_id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> Result<()> {
        let _ = (thread_id, lifecycle);
        anyhow::bail!("unsupported compact lifecycle persistence")
    }

    /// 加载 thread 中所有非默认 compact 标记
    async fn load_message_flags(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<HashMap<MessageId, MessageFlags>> {
        Ok(HashMap::new())
    }

    /// 删除指定消息之后的所有记录（用于 rewind）
    ///
    /// 查找 message_id 对应的序列位置，删除该位置之后的所有消息。
    /// 若 message_id 不存在则不执行任何操作。
    async fn delete_messages_since(
        &self,
        thread_id: &ThreadId,
        message_id: &MessageId,
    ) -> Result<()> {
        let _ = (thread_id, message_id);
        Ok(()) // 默认 no-op
    }

    /// H6: 获取 context cache epoch 值。
    ///
    /// 每次 compact 提交后递增，用于检测 context_cache 是否因 compact 变更而失效。
    async fn get_context_cache_epoch(&self, _thread_id: &ThreadId) -> Result<u64> {
        Ok(0) // 默认无 epoch 支持
    }
    // ── (KeenCode) replay 游标与 pending_tools 持久化 ──────────────────────────
    //
    // 默认实现均为 no-op / 空结果，FilesystemThreadStore 等非 SQLite 后端
    // 不受影响；SQLite 后端提供真实实现。

    /// 加载 event_seq > after_seq 的消息页（按 event_seq 升序，最多 limit 条）。
    ///
    /// 返回 (event_seq, BaseMessage) 列表；after_seq=None 表示从起点开始。
    /// 默认实现不支持（返回空），调用方应回退全量 load_messages。
    async fn load_messages_since(
        &self,
        _thread_id: &ThreadId,
        _after_seq: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<(i64, BaseMessage)>> {
        Ok(Vec::new())
    }

    /// 当前 thread 最新的 event_seq（无事件时为 0）。
    async fn latest_event_seq(&self, _thread_id: &ThreadId) -> Result<i64> {
        Ok(0)
    }

    /// 读取 thread 的 replay epoch（None = 从未设置，客户端应全量重放）。
    async fn get_replay_epoch(&self, _thread_id: &ThreadId) -> Result<Option<String>> {
        Ok(None)
    }

    /// 设置 thread 的 replay epoch（线程创建 / 会话恢复时写入）。
    async fn set_replay_epoch(&self, _thread_id: &ThreadId, _epoch: &str) -> Result<()> {
        Ok(())
    }

    /// 记录未完成工具调用（ToolStart）。
    async fn record_pending_tool(
        &self,
        _thread_id: &ThreadId,
        _tool_call_id: &str,
        _name: &str,
        _input_json: Option<String>,
    ) -> Result<()> {
        Ok(())
    }

    /// 删除未完成工具调用（ToolEnd）。
    async fn remove_pending_tool(&self, _tool_call_id: &str) -> Result<()> {
        Ok(())
    }

    /// 列出 thread 的残留未完成工具调用。
    async fn list_pending_tools(&self, _thread_id: &ThreadId) -> Result<Vec<PendingTool>> {
        Ok(Vec::new())
    }
}
