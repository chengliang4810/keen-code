//! Session v2 — 会话统一入口
//!
//! Session 是 peri-agent 的顶层门面，聚合五个核心实体：
//! - [`SessionStore`]：会话生命周期数据（不可变），含 FrozenContext
//! - [`MessageQueue`]：收件箱，异步消息注入
//! - [`SessionConfig`]：可变配置（Cancel Token、超时、思考配置、最大迭代数）
//! - [`MessageTranscript`]：对话笔录，只追加不篡改
//! - [`TurnContext`]：单次 turn 上下文，turn 结束即销毁
//!
//! 外部通过 `Session::new()` 创建，按需访问五个实体，通过 `start_turn()` 启动新 turn。
//!
//! ## Cron Owner（v2 新增）
//!
//! 有 SessionManager 的交互路径由 `AcpSession.cron_bridge` 持有 session 级
//! cron bridge。无 SessionManager 的 print fallback 则由本 Session 持有
//! [`CronOwner`](crate::agent::session::CronOwner)，确保桥接任务存活到 turn 结束。

pub mod agent_path;
pub mod async_router;
pub mod config;
pub mod exec;
pub mod factory;
pub mod queue;
pub mod retry_events;
pub mod runtime;
pub mod store;
pub mod subagent;
pub mod transcript;
pub mod turn;

pub use config::{SessionConfig, ThinkingConfig};
/// MessageFlags 已下沉 peri-acp-types（store 契约），此处 re-export 保持兼容。
pub use peri_acp_types::store::MessageFlags;
pub use queue::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
pub use store::{FrozenContext, FrozenContextBuilder, SessionId, SessionStore};
pub use transcript::{MessageTranscript, StagedData, TranscriptEntry};
pub use turn::{TurnContext, TurnId};

use std::sync::Arc;

use parking_lot::RwLock;

use crate::agent::session::cron_owner::CronOwner;
use crate::thread::ThreadId;

/// Session — 会话统一入口
///
/// 聚合五个核心实体，提供统一的创建和访问 API。
/// 通过 `Arc<Self>` 共享，外部通过 `Session::new()` 创建。
///
/// Print fallback 可选持有 turn 级 CronOwner，维持桥接任务生命周期。
pub struct Session {
    /// 会话生命周期数据（不可变）
    store: Arc<SessionStore>,
    /// 对话笔录（只追加，RwLock 保护内部可变性）
    transcript: Arc<RwLock<MessageTranscript>>,
    /// 收件箱（独立于 Transcript，会话内持续可变）
    queue: MessageQueue,
    /// 可变配置（Arc 共享，外部写入，循环读取）
    config: Arc<SessionConfig>,
    /// Print fallback 的 turn 级 CronOwner（set-once，RwLock 保护）。
    /// `None` 表示交互路径或尚未启动 cron；外层 `Some` 仅在 v2 路径启用。
    cron_owner: Option<parking_lot::RwLock<Option<CronOwner>>>,
    /// 子 agent 运行时宿主（L3）：executor/builder 在主 session 创建后注入，
    /// subagent 创建所需的运行时通道（thread_store / task_manager / bg 事件 /
    /// register / deregister / frozen local_md / frozen system_prompt）
    /// 统一经此读取，SubAgentMiddleware 不再逐字段透传。
    subagent_host: parking_lot::RwLock<Option<Arc<subagent::SubagentHost>>>,
}

impl Session {
    /// 创建新 Session
    ///
    /// - `cwd`：工作目录
    /// - `frozen`：会话级不可变上下文（System Prompt / CLAUDE.md / Skills）
    /// - `thread_id`：关联的 Thread ID（可选，用于持久化）
    pub fn new(cwd: Arc<str>, frozen: FrozenContext, thread_id: Option<ThreadId>) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let queue = MessageQueue::new();
        let config = Arc::new(SessionConfig::new());
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            cron_owner: None,
            subagent_host: parking_lot::RwLock::new(None),
        })
    }

    /// 创建新 Session，复用外部 cancel token（v2 路径用）
    ///
    /// 与 [`Session::new`] 的差异仅在于 cancel_token：传入的 token 是
    /// "linked clone"（`CancellationToken::clone()` 创建的关联 token），
    /// 父 token 取消时本 Session 也能感知。
    pub fn new_with_cancel(
        cwd: Arc<str>,
        frozen: FrozenContext,
        thread_id: Option<ThreadId>,
        cancel_token: Arc<tokio_util::sync::CancellationToken>,
    ) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let queue = MessageQueue::new();
        let mut config = SessionConfig::new();
        config.cancel_token = cancel_token;
        let config = Arc::new(config);
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            cron_owner: None,
            subagent_host: parking_lot::RwLock::new(None),
        })
    }

    /// 创建新 Session，复用外部 cancel token + 外部共享 MessageQueue（v2 路径用）
    ///
    /// 与 [`Session::new_with_cancel`] 的差异仅在于 queue：传入的 `queue`
    /// 是会话级共享实例（通常由 ACP `AcpSession.v2_message_queue` 持有），
    /// 让每个 turn 构造的 v2 Session 都指向**同一个**底层收件箱。
    ///
    /// **背景**：`MessageQueue` 内部用 `Arc<Mutex<VecDeque>> + Arc<Notify>` 实现，
    /// `clone()` 共享底层数据。因此传入 `queue` 后，Session 内的 queue 与外部
    /// 共享同一份消息流——SubAgent / Hook / GoalSteering 注入的 deferred / info
    /// 消息可被 main agent 的 ReAct 循环看到。
    ///
    /// 不传时（即 [`Session::new_with_cancel`]）每 turn 新建 MessageQueue，
    /// 跨 turn / 跨组件的消息互不可见。
    pub fn new_with_cancel_and_queue(
        cwd: Arc<str>,
        frozen: FrozenContext,
        thread_id: Option<ThreadId>,
        cancel_token: Arc<tokio_util::sync::CancellationToken>,
        queue: MessageQueue,
    ) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let mut config = SessionConfig::new();
        config.cancel_token = cancel_token;
        let config = Arc::new(config);
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            // v2 路径启用 owner 容器，供 print fallback 注入 turn 级 CronOwner。
            cron_owner: Some(parking_lot::RwLock::new(None)),
            subagent_host: parking_lot::RwLock::new(None),
        })
    }

    /// 会话生命周期数据（不可变）
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// 对话笔录（RwLock 保护）
    pub fn transcript(&self) -> Arc<RwLock<MessageTranscript>> {
        self.transcript.clone()
    }

    /// 收件箱
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }

    /// 子 agent 运行时宿主（L3）。executor/builder 在主 session 创建后注入；
    /// 未注入（测试/遗留路径）返回 None。
    pub fn subagent_host(&self) -> Option<Arc<subagent::SubagentHost>> {
        self.subagent_host.read().clone()
    }

    /// 注入子 agent 运行时宿主（L3）。
    ///
    /// set-once：重复注入仅记录 warn 并忽略（宿主随主 session 创建，每 turn 重建）。
    pub fn set_subagent_host(&self, host: subagent::SubagentHost) {
        let mut guard = self.subagent_host.write();
        if guard.is_some() {
            tracing::warn!("set_subagent_host: already set, ignoring duplicate call");
            return;
        }
        *guard = Some(Arc::new(host));
    }

    /// 可变配置
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// 启动新 turn — 创建 TurnContext
    ///
    /// 共享 cwd 和 cancel token，turn 内 step 从 0 开始。
    pub fn start_turn(&self) -> TurnContext {
        TurnContext::new(self.store.cwd.clone(), self.config.cancel_token.clone())
    }

    /// 注入已经启动的 print fallback CronOwner。
    ///
    /// 所有权必须留在 Session 内，否则 `CronOwner::drop` 会立即停止桥接任务。
    /// 返回 `true` 表示注入成功；重复注入或非 v2 Session 返回 `false`。
    pub fn set_cron_owner(&self, owner: CronOwner) -> bool {
        if let Some(rwlock) = &self.cron_owner {
            let mut guard = rwlock.write();
            if guard.is_some() {
                tracing::warn!("set_cron_owner: already set, ignoring duplicate call");
                return false;
            }
            *guard = Some(owner);
            true
        } else {
            false
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
