//! 原生桌面本地记忆的生命周期协调器。
//!
//! 记忆服务只负责提取、整合和文件事务；本模块负责把 Session 的权威终态转换为
//! 可取消的 idle 调度。启用前不创建 watcher、timer 或后台扫描任务。

use crate::agent_runtime::{AgentRuntime, AgentRuntimeError};
use crate::app_settings::InterfaceLanguage;
use crate::memories::MemoryService;
use keencode_resources::{SessionStatus, StoredSessionMetadata};
use keencode_runtime::{RuntimeEventPayload, RuntimeEventReceiveError};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

const MEMORY_IDLE_DURATION: Duration = Duration::from_secs(6 * 60 * 60);
#[derive(Default)]
struct SessionEntry {
    watcher: Option<(u64, CancellationToken)>,
    due_at: Option<Instant>,
}

struct CoordinatorState {
    next_generation: u64,
    sessions: HashMap<String, SessionEntry>,
    /// NativeHost 当前已经打开的 Session；禁用记忆时取消 watcher，重新启用后恢复观察。
    open_sessions: HashSet<String>,
    /// 所有 Session 共用一个到期任务；每次重新计算最早到期项时替换它。
    scheduler: Option<(u64, Instant, CancellationToken)>,
}

/// 由 NativeHost 持有的本地记忆调度边界。
pub(crate) struct NativeMemoryCoordinator {
    service: Arc<MemoryService>,
    runtime: Arc<AgentRuntime>,
    executor: Handle,
    language: Mutex<InterfaceLanguage>,
    idle_duration: Duration,
    enabled: AtomicBool,
    state: Mutex<CoordinatorState>,
}

impl NativeMemoryCoordinator {
    pub(crate) fn new(
        service: Arc<MemoryService>,
        runtime: Arc<AgentRuntime>,
        executor: &tokio::runtime::Runtime,
        language: InterfaceLanguage,
    ) -> Arc<Self> {
        Self::new_with_idle_duration(service, runtime, executor, language, MEMORY_IDLE_DURATION)
    }

    /// 测试可以缩短 idle 窗口；生产入口仍固定使用六小时，避免真实配置改变语义。
    pub(crate) fn new_with_idle_duration(
        service: Arc<MemoryService>,
        runtime: Arc<AgentRuntime>,
        executor: &tokio::runtime::Runtime,
        language: InterfaceLanguage,
        idle_duration: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            runtime,
            executor: executor.handle().clone(),
            language: Mutex::new(language),
            idle_duration,
            enabled: AtomicBool::new(false),
            state: Mutex::new(CoordinatorState {
                next_generation: 0,
                sessions: HashMap::new(),
                open_sessions: HashSet::new(),
                scheduler: None,
            }),
        })
    }

    /// 同步 Runtime 与记忆协调器的开关；关闭会取消 watcher、timer 和流水线。
    ///
    /// 设置页和冷启动共用此入口，确保新建 Turn 读取的 Runtime 原子值与后台
    /// 提取调度器不会出现一开一关的中间状态。
    pub(crate) fn start(self: &Arc<Self>, enabled: bool) -> Result<(), AgentRuntimeError> {
        self.runtime.set_local_memories_enabled(enabled)?;
        self.enabled.store(enabled, Ordering::Release);
        self.service.set_enabled(enabled);
        if !enabled {
            self.cancel_all();
            return Ok(());
        }

        let open_session_ids = {
            let state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            state.open_sessions.iter().cloned().collect::<Vec<_>>()
        };
        for session_id in open_session_ids {
            self.watch_session(&session_id);
        }

        let sessions = match self.runtime.stored_sessions() {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "初始化本地记忆 idle 调度失败");
                return Ok(());
            }
        };
        for session in sessions {
            self.update_metadata(session);
        }
        self.arm_scheduler();
        Ok(())
    }

    /// NativeHost 每次取得 Session 句柄时调用；同一 Session 至多建立一个观察任务。
    pub(crate) fn watch_session(self: &Arc<Self>, session_id: &str) {
        let session_id = session_id.to_owned();
        let (generation, cancellation) = {
            let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            state.open_sessions.insert(session_id.clone());
            if !self.enabled.load(Ordering::Acquire) {
                return;
            }
            state.next_generation = state.next_generation.saturating_add(1);
            let generation = state.next_generation;
            let entry = state.sessions.entry(session_id.clone()).or_default();
            if entry.watcher.is_some() {
                return;
            }
            let cancellation = CancellationToken::new();
            entry.watcher = Some((generation, cancellation.clone()));
            (generation, cancellation)
        };

        let weak = Arc::downgrade(self);
        let runtime = Arc::clone(&self.runtime);
        let watcher_session_id = session_id.clone();
        self.executor.spawn(async move {
            Self::run_session_watch(weak, runtime, watcher_session_id, generation, cancellation)
                .await;
        });
        self.refresh_session(&session_id);
    }

    /// 取消全部进程内观察和定时任务；不会删除记忆文件。
    pub(crate) fn shutdown(&self) {
        self.enabled.store(false, Ordering::Release);
        self.service.set_enabled(false);
        self.cancel_all();
    }

    fn cancel_all(&self) {
        let (entries, scheduler) = {
            let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            let entries = state
                .sessions
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            (entries, state.scheduler.take())
        };
        if let Some((_, _, cancellation)) = scheduler {
            cancellation.cancel();
        }
        for entry in entries {
            if let Some((_, cancellation)) = entry.watcher {
                cancellation.cancel();
            }
        }
    }

    async fn run_session_watch(
        weak: Weak<Self>,
        runtime: Arc<AgentRuntime>,
        session_id: String,
        generation: u64,
        cancellation: CancellationToken,
    ) {
        let mut source = match runtime.subscribe_session_events(&session_id) {
            Ok(source) => source,
            Err(error) => {
                tracing::debug!(session_id, %error, "本地记忆 Session 观察未启动");
                if let Some(coordinator) = weak.upgrade() {
                    coordinator.remove_watcher(&session_id, generation);
                }
                return;
            }
        };
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                received = source.recv() => match received {
                    Ok(delivery) => match delivery.payload {
                        RuntimeEventPayload::Authoritative(_) | RuntimeEventPayload::Control(_) => {
                            if let Some(coordinator) = weak.upgrade() {
                                coordinator.refresh_session(&session_id);
                            }
                        }
                        RuntimeEventPayload::Transient(_)
                        | RuntimeEventPayload::ModelRetryScheduled(_) => {}
                    },
                    Err(RuntimeEventReceiveError::Lagged(_)) => {
                        if let Some(coordinator) = weak.upgrade() {
                            coordinator.refresh_session(&session_id);
                        }
                    }
                    Err(RuntimeEventReceiveError::Closed) => break,
                },
            }
        }
        if let Some(coordinator) = weak.upgrade() {
            coordinator.refresh_session(&session_id);
            coordinator.remove_watcher(&session_id, generation);
        }
    }

    fn remove_watcher(&self, session_id: &str, generation: u64) {
        let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
        let remove_entry = if let Some(entry) = state.sessions.get_mut(session_id)
            && entry
                .watcher
                .as_ref()
                .is_some_and(|(current, _)| *current == generation)
        {
            entry.watcher = None;
            entry.due_at.is_none()
        } else {
            false
        };
        if remove_entry {
            state.sessions.remove(session_id);
        }
    }

    fn refresh_session(self: &Arc<Self>, session_id: &str) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        match self
            .runtime
            .runtime_manager()
            .stored_session_metadata(session_id)
        {
            Ok(metadata) => self.schedule_metadata(metadata),
            Err(error) => {
                tracing::debug!(session_id, %error, "本地记忆无法读取 Session 元数据");
                self.cancel_timer(session_id);
            }
        }
    }

    fn schedule_metadata(self: &Arc<Self>, metadata: StoredSessionMetadata) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        self.update_metadata(metadata);
        self.arm_scheduler();
    }

    fn update_metadata(&self, metadata: StoredSessionMetadata) {
        let session_id = metadata.session_id.as_str().to_owned();
        let due_at = if metadata.corrupt
            || !matches!(metadata.status, SessionStatus::Idle | SessionStatus::Closed)
        {
            None
        } else {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let updated_at_ms = u128::from(metadata.updated_at_unix_ms);
            let idle_ms = self.idle_duration.as_millis();
            let delay_ms = updated_at_ms.saturating_add(idle_ms).saturating_sub(now_ms);
            let delay = Duration::from_millis(u64::try_from(delay_ms).unwrap_or(u64::MAX));
            Some(Instant::now() + delay)
        };
        let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
        let entry = state.sessions.entry(session_id.clone()).or_default();
        entry.due_at = due_at;
        if entry.watcher.is_none() && entry.due_at.is_none() {
            state.sessions.remove(&session_id);
        }
    }

    fn arm_scheduler(self: &Arc<Self>) {
        let (generation, deadline, cancellation) = {
            let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            let next_deadline = state
                .sessions
                .values()
                .filter_map(|entry| entry.due_at)
                .min();
            match (state.scheduler.as_ref(), next_deadline) {
                (Some((_, current_deadline, _)), Some(deadline))
                    if *current_deadline == deadline =>
                {
                    return;
                }
                (None, None) => return,
                _ => {}
            }
            if let Some((_, _, current)) = state.scheduler.take() {
                current.cancel();
            }
            let Some(deadline) = next_deadline else {
                return;
            };
            state.next_generation = state.next_generation.saturating_add(1);
            let generation = state.next_generation;
            let cancellation = CancellationToken::new();
            state.scheduler = Some((generation, deadline, cancellation.clone()));
            (generation, deadline, cancellation)
        };
        let weak = Arc::downgrade(self);
        let delay = deadline.saturating_duration_since(Instant::now());
        self.executor.spawn(async move {
            tokio::select! {
                _ = cancellation.cancelled() => {}
                _ = tokio::time::sleep(delay) => {
                    if let Some(coordinator) = weak.upgrade() {
                        coordinator.fire_scheduler(generation);
                    }
                }
            }
        });
    }

    fn cancel_timer(self: &Arc<Self>, session_id: &str) {
        {
            let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            let remove_entry = if let Some(entry) = state.sessions.get_mut(session_id) {
                entry.due_at = None;
                entry.watcher.is_none()
            } else {
                false
            };
            if remove_entry {
                state.sessions.remove(session_id);
            }
        }
        self.arm_scheduler();
    }

    fn fire_scheduler(self: &Arc<Self>, generation: u64) {
        let due_count = {
            let mut state = self.state.lock().expect("本地记忆协调器状态锁已损坏");
            let Some((current, _, cancellation)) = state.scheduler.take() else {
                return;
            };
            if current != generation || cancellation.is_cancelled() {
                return;
            }
            let now = Instant::now();
            let mut due_count = 0;
            for entry in state.sessions.values_mut() {
                if entry.due_at.is_some_and(|deadline| deadline <= now) {
                    entry.due_at = None;
                    due_count += 1;
                }
            }
            state
                .sessions
                .retain(|_, entry| entry.watcher.is_some() || entry.due_at.is_some());
            due_count
        };
        if due_count > 0 && self.enabled.load(Ordering::Acquire) {
            let language = *self.language.lock().expect("本地记忆语言锁已损坏");
            self.service
                .trigger(Arc::clone(&self.runtime), None, language, false);
        }
        self.arm_scheduler();
    }
}
