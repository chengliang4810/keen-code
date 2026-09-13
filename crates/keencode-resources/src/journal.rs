#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic::{
    BoundedJson, BoundedRead, atomic_write, ensure_regular_file_or_absent, exclusive_lock,
    exclusive_lock_with_timeout, prepare_root, read_file_bounded, secure_child_dir,
    serialize_json_bounded, sync_directory,
};
use crate::canonical::canonical_json_sha256;
use crate::reducer::{
    reduce_record_from_valid_state, validate_atomic_batch_shape, validate_owned_atomic_batch_shape,
};
use crate::{
    ArtifactId, ArtifactMaterialization, ArtifactUse, ArtifactValidator, CorruptionIssue,
    CorruptionKind, MessageImageSource, ResourceError, SessionEvent, SessionEventId,
    SessionEventRecord, SessionId, SessionState, StoredSessionMetadata, ToolResultPart,
};

/// Snapshot 文件使用的固定 schema 名称。
const SNAPSHOT_SCHEMA: &str = "keencode/session-snapshot";
/// Snapshot 文件格式版本。
const SNAPSHOT_VERSION: u32 = 4;
/// 单页重放允许返回的最大权威事件数量，与 ACP 边界保持一致。
pub const MAX_REPLAY_PAGE_RECORDS: usize = 1_000;

#[cfg(test)]
thread_local! {
    /// 当前测试线程执行重放定位的次数。
    static REPLAY_SEEK_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// 可重建的历史定位索引；只保存轮次边界和稀疏 Provider 快照，不复制正文。
#[derive(Clone, Debug, Default)]
pub struct SessionHistoryIndex {
    /// 根轮次起始物理序号，按提交顺序排列。
    pub root_starts: Vec<u64>,
    /// 稀疏 Provider 变更，用于从中间窗口准确恢复模型统计。
    pub providers: BTreeMap<u64, crate::ProviderSnapshot>,
    /// 子 Agent 身份、状态和 Todo 的稀疏定位，不保留事件正文。
    pub context: BTreeMap<String, Vec<u64>>,
}
impl SessionHistoryIndex {
    fn observe(&mut self, sequence: u64, event: &SessionEvent) {
        match event {
            SessionEvent::AtomicBatch { events } => {
                for event in events {
                    self.observe(sequence, event);
                }
            }
            SessionEvent::TurnStarted {
                parent_turn_id: None,
                ..
            } => {
                if self.root_starts.last() != Some(&sequence) {
                    self.root_starts.push(sequence);
                }
            }
            SessionEvent::TurnStarted {
                turn_id,
                parent_turn_id: Some(_),
                ..
            } => {
                self.context
                    .entry(format!("child-start:{turn_id}"))
                    .or_default()
                    .push(sequence);
            }
            SessionEvent::TurnCompleted { turn_id } | SessionEvent::TurnStopped { turn_id, .. } => {
                if self.context.contains_key(&format!("child-start:{turn_id}")) {
                    self.context
                        .entry(format!("child-end:{turn_id}"))
                        .or_default()
                        .push(sequence);
                }
            }
            SessionEvent::SubAgentSpawned { agent } => {
                self.context
                    .entry(format!("spawn:{}", agent.agent_id))
                    .or_default()
                    .push(sequence);
            }
            SessionEvent::SubAgentStatusChanged { agent_id, .. } => {
                self.context
                    .entry(format!("status:{agent_id}"))
                    .or_default()
                    .push(sequence);
            }
            SessionEvent::TodoReplaced { .. } => {
                self.context
                    .entry("todo".to_owned())
                    .or_default()
                    .push(sequence);
            }
            SessionEvent::ProviderSnapshotUpdated { provider } => {
                self.providers.insert(sequence, provider.clone());
            }
            _ => {}
        }
    }
}

/// 单个 JSONL 事件落盘后的持久化强度。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    /// 只写入操作系统文件缓存，适合可重建测试数据。
    Buffered,
    /// 每次追加后执行 `flush`。
    Flush,
    /// 攒批后执行 `flush` 与 `sync_data`：默认 64 条或 100ms 触发一次
    /// 持久化（以先到为准）。返回的事件对当前实例立即可见，但进程崩溃会
    /// 丢失窗口内尚未 sync 的尾部（≤64 条/100ms）；崩溃窗口外的事件与旧
    /// 语义一致，落盘即 durable。后台 fsync worker 受进程级并发上限约束，
    /// 超出部分排队等待槽位。需要"返回即落盘"的调用点必须显式调用
    /// [`SessionJournal::flush`]（例如 mutation `Prepared` 记录、Barrier ack
    /// 前的积压），`AlreadyCommitted` 快捷路径会按配置补齐持久化。
    #[default]
    FlushAndSync,
}

/// 批量刷盘的触发阈值：攒满该条数即执行一次 flush+sync_data。
pub const JOURNAL_BATCH_MAX_RECORDS: usize = 64;
/// 批量刷盘的超时阈值：自本批首条记录完成写入起计时，即使未满批。
pub const JOURNAL_BATCH_MAX_DELAY: Duration = Duration::from_millis(100);
/// 后台刷盘失败后的最大尝试次数；耗尽后保留 sticky failure，由下一次 barrier 对账。
const JOURNAL_BATCH_MAX_ATTEMPTS: u32 = 4;
/// 进程级调度器没有待刷 Journal 后保留线程的时间，吸收相邻短批次后自动退出。
const JOURNAL_BATCH_SCHEDULER_IDLE_TIMEOUT: Duration = Duration::from_millis(250);
/// 单个后台 Journal 获取跨进程锁的等待上限，避免一个异常 Session 阻塞全部批次。
const JOURNAL_BATCH_LOCK_WAIT: Duration = Duration::from_millis(20);
/// 进程级后台 fsync 的最大并发数；超出部分保留在 scheduler 中等待空闲槽位。
const JOURNAL_BATCH_MAX_CONCURRENT_FSYNC: usize = 8;

/// 自动 Snapshot 的频率策略。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SnapshotPolicy {
    /// 不自动生成 Snapshot，仍可显式调用写入。
    Disabled,
    /// 每经过固定数量事件写入一次 Snapshot。
    Every {
        /// 必须大于零的事件间隔。
        events: u64,
    },
}

impl Default for SnapshotPolicy {
    /// 默认每 100 个事件写入一次 Snapshot。
    fn default() -> Self {
        Self::Every { events: 100 }
    }
}

/// Session 日志与 Snapshot 的运行配置。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalConfig {
    /// 每个事件追加后的持久化强度。
    pub durability: Durability,
    /// 自动 Snapshot 策略。
    pub snapshot_policy: SnapshotPolicy,
    /// 单个编码后 JSONL 事件允许的最大字节数。
    pub max_event_bytes: u64,
    /// 单个 Session 事件日志允许的最大字节数。
    pub max_log_bytes: u64,
    /// 单个 Session 允许的最大事件记录数量。
    pub max_records: u64,
    /// 任一归约状态集合允许的最大元素数量。
    pub max_state_collection_items: usize,
}

impl Default for JournalConfig {
    /// 返回限制单事件为 1 MiB、单日志为 256 MiB 的默认配置。
    fn default() -> Self {
        Self {
            durability: Durability::default(),
            snapshot_policy: SnapshotPolicy::default(),
            max_event_bytes: 1024 * 1024,
            max_log_bytes: 256 * 1024 * 1024,
            max_records: 100_000,
            max_state_collection_items: 50_000,
        }
    }
}

impl JournalConfig {
    /// 校验 Snapshot 周期不为零。
    fn validate(self) -> Result<Self, ResourceError> {
        if matches!(self.snapshot_policy, SnapshotPolicy::Every { events: 0 }) {
            return Err(ResourceError::UnsafePath(
                "Snapshot 周期必须大于零".to_owned(),
            ));
        }
        if self.max_event_bytes == 0
            || self.max_log_bytes == 0
            || self.max_event_bytes > self.max_log_bytes
            || self.max_records == 0
            || self.max_state_collection_items == 0
        {
            return Err(ResourceError::UnsafePath(
                "事件、日志、记录和状态集合限制必须大于零，且单事件限制不得超过日志限制".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// 自动 Snapshot 在一次已提交追加后的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotStatus {
    /// 当前 sequence 不需要写 Snapshot。
    NotDue,
    /// Snapshot 已原子写入。
    Written,
    /// 事件已提交，但 Snapshot 写入失败；日志仍是权威来源。
    Failed {
        /// 不包含事件正文的失败说明。
        message: String,
    },
}

/// 一次 append-only 提交的明确结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendReceipt {
    /// 已写入 JSONL 的完整事件记录。
    pub record: SessionEventRecord,
    /// 本次自动 Snapshot 的独立结果。
    pub snapshot: SnapshotStatus,
}

/// 从权威 JSONL 日志读取的一页类型化 Session 事件。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayPage {
    /// 严格位于请求游标之后、按 sequence 升序排列的事件。
    pub records: Vec<SessionEventRecord>,
    /// 当前页最后一个事件序号；空页没有下一游标。
    pub next_after: Option<u64>,
    /// 本次读取锁内观察到的权威日志末尾序号。
    pub through_sequence: u64,
    /// 当前页之后是否仍有不晚于 `through_sequence` 的事件。
    pub has_more: bool,
}

/// 一次带稳定事件标识和 sequence CAS 的追加结果。
#[derive(Debug)]
pub enum IdempotentAppendOutcome {
    /// 当前调用新写入了完整事件记录。
    Appended(AppendReceipt),
    /// 相同事件标识与正文已经由当前或先前进程提交。
    AlreadyCommitted {
        /// 日志中已经存在的完整事件记录。
        record: SessionEventRecord,
    },
    /// 相同事件标识已经绑定到不同事件正文。
    EventIdConflict {
        /// 日志中首次使用该事件标识的 sequence。
        existing_sequence: u64,
    },
    /// 调用方的 sequence CAS 水位已落后或超前。
    SequenceConflict {
        /// 调用方声明的最后已知 sequence。
        expected_sequence: u64,
        /// 当前权威日志的实际最后 sequence。
        actual_sequence: u64,
    },
    /// 写入已经开始，但重读仍无法证明事件完整提交或明确未提交。
    Indeterminate {
        /// 首次写入或持久化失败的底层错误。
        error: ResourceError,
    },
}

/// 打开 Session 日志后的安全结果。
// 两个结果均由调用方直接消费；为消除 208 字节差异引入公开 Box 会制造无价值 API 间接层。
#[allow(clippy::large_enum_variant)]
pub enum SessionOpen {
    /// 权威日志健康；有效 Snapshot 已使用，坏 Snapshot 已忽略并尽力重建。
    Ready(SessionJournal),
    /// 检测到损坏，只返回只读事实报告。
    Corrupt(ReadOnlySessionReport),
}

/// 损坏 Session 的只读恢复报告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOnlySessionReport {
    /// 目标 Session 标识。
    pub session_id: SessionId,
    /// 首个损坏点之前可确定归约的状态。
    pub last_valid_state: SessionState,
    /// 成功读取并归约的完整事件数。
    pub valid_records: usize,
    /// 所有已检测到的结构化损坏事实。
    pub issues: Vec<CorruptionIssue>,
    /// 只用于本地诊断的事件日志路径。
    pub log_path: PathBuf,
}

/// 一次显式截断尾部恢复的结果。
pub struct TruncatedTailRecovery {
    /// 已恢复且可以继续追加的 Session 日志。
    pub journal: SessionJournal,
    /// 原样保存损坏尾部字节的证据文件。
    pub evidence_path: PathBuf,
    /// 证据文件保存的原始尾部字节数。
    pub preserved_bytes: u64,
}

/// 可并发追加且在检测到外部损坏后停止写入的 Session 日志。
pub struct SessionJournal {
    /// Session 标识。
    session_id: SessionId,
    /// 已验证的 Session 目录。
    session_dir: PathBuf,
    /// append-only JSONL 文件。
    log_path: PathBuf,
    /// 原子 Snapshot 文件。
    snapshot_path: PathBuf,
    /// 多实例追加协调锁。
    lock_path: PathBuf,
    /// 落盘策略。
    config: JournalConfig,
    /// 事件提交前使用的可选 Artifact 实体校验器。
    artifact_validator: Option<Arc<dyn ArtifactValidator>>,
    /// 同一实例内的状态与 sequence 互斥边界。
    inner: Arc<Mutex<JournalInner>>,
    /// 进程级批量刷盘调度器使用的常量大小文件上下文。
    batch_target: Arc<BatchFlushTarget>,
    /// 当前 Journal 在进程级批量刷盘调度器中的弱引用任务。
    batch_job: Arc<BatchFlushJob>,
}

/// SessionJournal 的可变状态。
struct JournalInner {
    history_index: SessionHistoryIndex,
    /// 当前完整归约状态（含尚未 sync 的批量窗口事件）。
    state: SessionState,
    /// 从健康权威日志重放得到的幂等事件索引（含尚未 sync 的批量窗口事件）。
    event_index: BTreeMap<SessionEventId, EventIndexEntry>,
    /// 每条物理 JSONL 记录包含换行符后的排他结束字节偏移（含尚未 sync 的批量窗口事件）。
    record_end_offsets: Vec<u64>,
    /// 上次加载或追加后的日志字节数（含尚未 sync 的批量窗口事件）。
    log_len: u64,
    /// 上次加载或追加后观察到的文件系统变化戳。
    log_stamp: LogStamp,
    /// 发现损坏后永久阻止当前实例继续写入。
    read_only: bool,
    /// 当前实例是否仍需为 events.jsonl 的目录项确认一次父目录同步。
    directory_sync_required: bool,
    /// 尚未 sync 的批量窗口事件数。
    pending_records: usize,
    /// 当前实例最近一次成功 sync 时已经归约到的 sequence 下界。
    synced_through_sequence: u64,
    /// 当前批次第一条事件完成写入的单调时间。
    pending_since: Option<Instant>,
    /// 当前非空批次是否已经登记到进程级调度器。
    batch_worker_active: bool,
    /// 后台刷盘连续失败次数；成功后归零。
    batch_retry_attempts: u32,
    /// 后台刷盘失败后的下一次有界重试时间。
    batch_retry_at: Option<Instant>,
    /// 后台重试耗尽后的可观察失败；显式 barrier 成功前不得继续普通追加。
    batch_flush_failure: Option<String>,
}

impl JournalInner {
    /// 当前批量窗口是否已经超过第一条事件起算的超时阈值。
    fn batch_delay_expired(&self) -> bool {
        self.pending_since
            .is_some_and(|started| started.elapsed() >= JOURNAL_BATCH_MAX_DELAY)
    }

    /// 把自本实例最近一次 sync 后观察到的权威物理记录纳入批次计数。
    /// 外部实例事件也计入，从而交错追加仍会在全局 64 条附近触发刷盘；
    /// 无法证明对方是否已经 sync 时宁可保守地提前重复 sync。
    fn observe_unsynced_through(&mut self, sequence: u64) {
        let records = usize::try_from(sequence.saturating_sub(self.synced_through_sequence))
            .unwrap_or(usize::MAX);
        if records == 0 {
            return;
        }
        self.pending_records = self.pending_records.max(records);
        self.pending_since.get_or_insert_with(Instant::now);
    }
}

/// 后台刷盘只保留路径与测试计数，不复制 Journal 正文或归约状态。
struct BatchFlushTarget {
    log_path: PathBuf,
    session_dir: PathBuf,
    lock_path: PathBuf,
    #[cfg(any(test, feature = "test-support"))]
    sync_count: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    background_failures_remaining: std::sync::atomic::AtomicUsize,
    #[cfg(any(test, feature = "test-support"))]
    background_attempt_count: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    background_sync_delay_ms: std::sync::atomic::AtomicU64,
}

/// 一个 Journal 在进程级调度器中的常量大小弱引用任务，不延长 Journal 生命周期。
struct BatchFlushJob {
    id: u64,
    inner: Weak<Mutex<JournalInner>>,
    target: Weak<BatchFlushTarget>,
}

impl BatchFlushJob {
    fn new(inner: &Arc<Mutex<JournalInner>>, target: &Arc<BatchFlushTarget>) -> Self {
        static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            inner: Arc::downgrade(inner),
            target: Arc::downgrade(target),
        }
    }
}

/// 进程级调度状态只保存活跃任务的弱引用；空闲后不保留 Journal 或线程栈。
#[derive(Default)]
struct BatchSchedulerState {
    jobs: BTreeMap<u64, Weak<BatchFlushJob>>,
    /// 已交给短生命周期线程执行 fsync 的任务，避免同一 Journal 重复并发刷盘，
    /// 且其数量受 `JOURNAL_BATCH_MAX_CONCURRENT_FSYNC` 限制。
    in_flight: BTreeSet<u64>,
    worker_active: bool,
    /// 每次任务集合或 deadline 变化时递增，避免通知发生在 worker 处理任务期间而丢失。
    revision: u64,
}

/// 所有 Journal 共享的惰性 deadline 调度器；Condvar 直接等待最早 deadline，
/// 到期 Journal 的 fsync 由数量有界的短生命周期线程并发执行。
struct BatchScheduler {
    state: Mutex<BatchSchedulerState>,
    wake: Condvar,
    #[cfg(any(test, feature = "test-support"))]
    active_worker_count: std::sync::atomic::AtomicUsize,
    #[cfg(any(test, feature = "test-support"))]
    active_flush_worker_count: std::sync::atomic::AtomicUsize,
    #[cfg(any(test, feature = "test-support"))]
    worker_start_count: AtomicU64,
}

impl Default for BatchScheduler {
    fn default() -> Self {
        Self {
            state: Mutex::new(BatchSchedulerState::default()),
            wake: Condvar::new(),
            #[cfg(any(test, feature = "test-support"))]
            active_worker_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            active_flush_worker_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            worker_start_count: AtomicU64::new(0),
        }
    }
}

/// 返回进程内唯一的惰性 Journal 刷盘调度器。
fn batch_scheduler() -> &'static Arc<BatchScheduler> {
    static SCHEDULER: OnceLock<Arc<BatchScheduler>> = OnceLock::new();
    SCHEDULER.get_or_init(|| Arc::new(BatchScheduler::default()))
}

impl BatchScheduler {
    /// 登记或唤醒一个 Journal 任务；进程内任意时刻至多存在一个 deadline 调度线程。
    fn schedule(self: &Arc<Self>, job: &Arc<BatchFlushJob>) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        state.jobs.insert(job.id, Arc::downgrade(job));
        state.revision = state.revision.wrapping_add(1);
        if state.worker_active {
            self.wake.notify_one();
            return true;
        }
        state.worker_active = true;
        let scheduler = Arc::clone(self);
        match std::thread::Builder::new()
            .name("keencode-journal-timer".to_owned())
            .spawn(move || run_batch_scheduler(scheduler))
        {
            Ok(_) => true,
            Err(_) => {
                state.jobs.remove(&job.id);
                state.worker_active = false;
                false
            }
        }
    }

    /// 移除已经同步、关闭或失效的任务，并唤醒调度线程重算最早截止时间。
    fn cancel(&self, job_id: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.jobs.remove(&job_id).is_some() {
                state.revision = state.revision.wrapping_add(1);
                self.wake.notify_one();
            }
        }
    }

    /// Journal 的 deadline 或 pending 状态改变后唤醒调度线程。
    fn notify(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.revision = state.revision.wrapping_add(1);
            self.wake.notify_one();
        }
    }

    /// 在任务仍登记且未执行且仍有全局槽位时原子标记一次 fsync 派发。
    fn begin_flush(&self, job_id: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.jobs.contains_key(&job_id)
            || state.in_flight.len() >= JOURNAL_BATCH_MAX_CONCURRENT_FSYNC
            || !state.in_flight.insert(job_id)
        {
            return false;
        }
        #[cfg(any(test, feature = "test-support"))]
        self.active_flush_worker_count
            .fetch_add(1, Ordering::AcqRel);
        state.revision = state.revision.wrapping_add(1);
        true
    }

    /// fsync worker 退出时解除执行标记并唤醒调度器处理排队任务、重试或新批次。
    fn finish_flush(&self, job_id: u64) {
        if let Ok(mut state) = self.state.lock()
            && state.in_flight.remove(&job_id)
        {
            #[cfg(any(test, feature = "test-support"))]
            self.active_flush_worker_count
                .fetch_sub(1, Ordering::AcqRel);
            state.revision = state.revision.wrapping_add(1);
            self.wake.notify_one();
        }
    }
}

/// 测试用进程级调度线程生命周期计数守卫。
#[cfg(any(test, feature = "test-support"))]
struct ActiveBatchSchedulerGuard<'a> {
    scheduler: &'a BatchScheduler,
}

#[cfg(any(test, feature = "test-support"))]
impl<'a> ActiveBatchSchedulerGuard<'a> {
    fn new(scheduler: &'a BatchScheduler) -> Self {
        scheduler.active_worker_count.fetch_add(1, Ordering::AcqRel);
        scheduler.worker_start_count.fetch_add(1, Ordering::Relaxed);
        Self { scheduler }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ActiveBatchSchedulerGuard<'_> {
    fn drop(&mut self) {
        self.scheduler
            .active_worker_count
            .fetch_sub(1, Ordering::AcqRel);
    }
}

/// 确保独立 fsync 线程即使 panic 也解除调度器中的 in-flight 标记。
struct BatchFlushCompletionGuard {
    scheduler: Arc<BatchScheduler>,
    job_id: u64,
}

impl BatchFlushCompletionGuard {
    fn new(scheduler: Arc<BatchScheduler>, job_id: u64) -> Self {
        Self { scheduler, job_id }
    }
}

impl Drop for BatchFlushCompletionGuard {
    fn drop(&mut self) {
        self.scheduler.finish_flush(self.job_id);
    }
}

/// 测试可观测的 sync_data 计数器：每次真实 sync 累加一次。
#[cfg(any(test, feature = "test-support"))]
static SYNC_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 记录一次真实 sync_data，供测试统计 fsync 合并效果。
fn count_sync_for_tests(_target: &BatchFlushTarget) {
    #[cfg(any(test, feature = "test-support"))]
    {
        _target
            .sync_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        SYNC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 写入已经开始后进行权威日志对账所需的不可变上下文。
#[derive(Clone, Copy)]
struct StartedWriteContext<'a> {
    /// 本次追加的幂等事件标识。
    event_id: &'a SessionEventId,
    /// 本次追加的类型化事件。
    event: &'a SessionEvent,
    /// 本次追加原计划占用的 sequence。
    sequence: u64,
    /// 本次追加开始前的日志长度。
    original_log_len: u64,
    /// 本次追加是否首次创建日志文件。
    created: bool,
}

/// 幂等索引只保存比较和重建回执所需的小型元数据，避免复制事件正文。
#[derive(Clone, Debug)]
struct EventIndexEntry {
    /// 已提交事件的 sequence。
    sequence: u64,
    /// 已提交事件的原始时间戳。
    time_unix_ms: u64,
    /// 事件 payload 规范 JSON 的 SHA-256。
    event_sha256: String,
}

/// 用于发现跨实例长度变化和同长度重写的文件系统变化戳。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct LogStamp {
    /// 文件当前字节数。
    len: u64,
    /// 文件系统报告的最后修改时间；不支持时为 `None`。
    modified: Option<SystemTime>,
}

/// 可丢弃的列表索引，不用于授权或打开 Session 时的完整性判定。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MetadataIndex {
    stamp: LogStamp,
    metadata: StoredSessionMetadata,
}

/// 索引只包含标题和标量状态，禁止读取正文大小的文件。
const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// 索引失败不改变已提交日志的成功语义；下次列表按日志重建。
fn write_metadata_index(directory: &Path, state: &SessionState, stamp: &LogStamp, corrupt: bool) {
    let Some(metadata) = StoredSessionMetadata::from_state(state, corrupt) else {
        return;
    };
    let index = MetadataIndex {
        stamp: stamp.clone(),
        metadata,
    };
    if let Ok(BoundedJson::Bytes(bytes)) = serialize_json_bounded(&index, MAX_METADATA_BYTES, false)
    {
        let _ = atomic_write(&directory.join("metadata.json"), &bytes, false);
    }
}

impl SessionJournal {
    /// 读取可重建列表索引；只在日志变化或索引缺失/损坏时恢复该 Session。
    /// 此投影不证明日志健康，执行操作必须走 `open` 的权威校验。
    pub fn read_metadata(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        config: JournalConfig,
    ) -> Result<Option<StoredSessionMetadata>, ResourceError> {
        config.validate()?;
        let root = prepare_root(storage_root.as_ref())?;
        let sessions = root.clone();
        let directory = secure_child_dir(&sessions, session_id.as_str())?;
        let log = directory.join("events.jsonl");
        let index = directory.join("metadata.json");
        ensure_regular_file_or_absent(&log)?;
        ensure_regular_file_or_absent(&index)?;
        let stamp = log_stamp(&log)?;
        if stamp.modified.is_some()
            && let Ok(BoundedRead::Bytes(bytes)) = read_file_bounded(&index, MAX_METADATA_BYTES)
            && let Ok(index) = serde_json::from_slice::<MetadataIndex>(&bytes)
            && index.stamp == stamp
            && index.metadata.session_id == session_id
        {
            return Ok(Some(index.metadata));
        }
        // 索引是当前日志的派生缓存；丢失后重建，不读取任何历史数据格式。
        match Self::open(root, session_id, config)? {
            SessionOpen::Ready(journal) => {
                journal.read_state(|state| StoredSessionMetadata::from_state(state, false))
            }
            SessionOpen::Corrupt(report) => Ok(StoredSessionMetadata::from_state(
                &report.last_valid_state,
                true,
            )),
        }
    }

    /// 打开或创建一个全新格式 Session；损坏时只返回报告而不修复事件日志。
    ///
    /// 路径隔离仅为尽力检查，不承诺抵御具有本机目录写权限的并发攻击者。
    pub fn open(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        config: JournalConfig,
    ) -> Result<SessionOpen, ResourceError> {
        Self::open_internal(storage_root.as_ref(), session_id, config, None)
    }

    /// 打开 Session，并注入所有 Artifact 引用在 append 前必须通过的实体校验器。
    ///
    /// 路径隔离能力与 [`crate::filesystem_capabilities`] 报告一致。
    pub fn open_with_artifact_validator(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        config: JournalConfig,
        artifact_validator: Arc<dyn ArtifactValidator>,
    ) -> Result<SessionOpen, ResourceError> {
        Self::open_internal(
            storage_root.as_ref(),
            session_id,
            config,
            Some(artifact_validator),
        )
    }

    /// 使用已经规范化的参数打开 Session。
    fn open_internal(
        storage_root: &Path,
        session_id: SessionId,
        config: JournalConfig,
        artifact_validator: Option<Arc<dyn ArtifactValidator>>,
    ) -> Result<SessionOpen, ResourceError> {
        let config = config.validate()?;
        let root = prepare_root(storage_root)?;
        let sessions = root.clone();
        let session_dir = secure_child_dir(&sessions, session_id.as_str())?;
        let log_path = session_dir.join("events.jsonl");
        let snapshot_path = session_dir.join("snapshot.json");
        let lock_path = session_dir.join("append.lock");
        ensure_regular_file_or_absent(&log_path)?;
        ensure_regular_file_or_absent(&snapshot_path)?;
        ensure_regular_file_or_absent(&lock_path)?;

        // 打开与追加共用同一把跨进程锁，避免把正在落盘的一行误判为损坏尾记录。
        let _file_lock = exclusive_lock(&lock_path)?;
        let loaded = load_session(&session_id, &log_path, &snapshot_path, config)?;
        write_metadata_index(
            &session_dir,
            &loaded.state,
            &loaded.log_stamp,
            !loaded.issues.is_empty(),
        );
        if !loaded.issues.is_empty() {
            return Ok(SessionOpen::Corrupt(ReadOnlySessionReport {
                session_id,
                last_valid_state: loaded.state,

                valid_records: loaded.valid_records,
                issues: loaded.issues,
                log_path,
            }));
        }
        // 冷打开不能仅因日志能够重放就把既有 sequence 当作 durable。另一个
        // 进程可能刚把完整 JSONL 行写入页缓存后退出；必须先在 append.lock 内
        // 同步文件与目录，再建立 synced_through_sequence 基线。
        let existing_log_synced =
            config.durability == Durability::FlushAndSync && log_path.exists();
        if existing_log_synced {
            let file = OpenOptions::new()
                .write(true)
                .open(&log_path)
                .map_err(|error| ResourceError::io("open_event_log_on_open", error))?;
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::Sync) {
                return Err(injected_io_error("sync_event_log_on_open"));
            }
            file.sync_data()
                .map_err(|error| ResourceError::io("sync_event_log_on_open", error))?;
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::DirectorySync) {
                return Err(injected_io_error("sync_event_log_directory_on_open"));
            }
            sync_directory(&session_dir, true)?;
        }
        if loaded.snapshot_needs_rebuild {
            if let Ok(anchor) = complete_log_anchor(&log_path, config.max_log_bytes) {
                let _ = write_snapshot_file(
                    &snapshot_path,
                    &loaded.state,
                    anchor,
                    config.durability == Durability::FlushAndSync,
                    config.max_log_bytes,
                );
            }
        }
        let loaded_sequence = loaded.state.last_sequence;
        let inner = Arc::new(Mutex::new(JournalInner {
            state: loaded.state,
            history_index: loaded.history_index,
            event_index: loaded.event_index,
            record_end_offsets: loaded.record_end_offsets,
            log_len: loaded.log_len,
            log_stamp: loaded.log_stamp,
            read_only: false,
            directory_sync_required: config.durability == Durability::FlushAndSync
                && !existing_log_synced,
            pending_records: 0,
            synced_through_sequence: loaded_sequence,
            pending_since: None,
            batch_worker_active: false,
            batch_retry_attempts: 0,
            batch_retry_at: None,
            batch_flush_failure: None,
        }));
        let batch_target = Arc::new(BatchFlushTarget {
            log_path: log_path.clone(),
            session_dir: session_dir.clone(),
            lock_path: lock_path.clone(),
            #[cfg(any(test, feature = "test-support"))]
            sync_count: std::sync::atomic::AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_attempt_count: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_sync_delay_ms: AtomicU64::new(0),
        });
        let batch_job = Arc::new(BatchFlushJob::new(&inner, &batch_target));
        Ok(SessionOpen::Ready(Self {
            session_id,
            session_dir,
            log_path,
            snapshot_path,
            lock_path,
            config,
            artifact_validator,
            inner,
            batch_target,
            batch_job,
        }))
    }

    /// 显式恢复仅含一条截断尾记录的 Session，并原样保留尾部证据后继续追加。
    ///
    /// 该操作不会恢复中间坏行、sequence 损坏或 reducer 失败；这些情况继续保持只读。
    pub fn recover_truncated_tail(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        config: JournalConfig,
    ) -> Result<TruncatedTailRecovery, ResourceError> {
        Self::recover_truncated_tail_internal(storage_root.as_ref(), session_id, config, None)
    }

    /// 显式恢复截断尾记录，并为恢复后的追加注入 Artifact 实体校验器。
    pub fn recover_truncated_tail_with_artifact_validator(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        config: JournalConfig,
        artifact_validator: Arc<dyn ArtifactValidator>,
    ) -> Result<TruncatedTailRecovery, ResourceError> {
        Self::recover_truncated_tail_internal(
            storage_root.as_ref(),
            session_id,
            config,
            Some(artifact_validator),
        )
    }

    /// 在跨实例追加锁内保留证据、截断日志并构造可继续写入的 Journal。
    fn recover_truncated_tail_internal(
        storage_root: &Path,
        session_id: SessionId,
        config: JournalConfig,
        artifact_validator: Option<Arc<dyn ArtifactValidator>>,
    ) -> Result<TruncatedTailRecovery, ResourceError> {
        let config = config.validate()?;
        let root = prepare_root(storage_root)?;
        let sessions = root.clone();
        let session_dir = secure_child_dir(&sessions, session_id.as_str())?;
        let log_path = session_dir.join("events.jsonl");
        let snapshot_path = session_dir.join("snapshot.json");
        let lock_path = session_dir.join("append.lock");
        ensure_regular_file_or_absent(&log_path)?;
        ensure_regular_file_or_absent(&snapshot_path)?;
        ensure_regular_file_or_absent(&lock_path)?;

        let _file_lock = exclusive_lock(&lock_path)?;
        let loaded = load_session(&session_id, &log_path, &snapshot_path, config)?;
        let [issue] = loaded.issues.as_slice() else {
            return Err(ResourceError::TruncatedTailRecoveryNotApplicable);
        };
        let CorruptionKind::TruncatedTail { byte_offset } = &issue.kind else {
            return Err(ResourceError::TruncatedTailRecoveryNotApplicable);
        };
        let bytes = match read_file_bounded(&log_path, config.max_log_bytes) {
            Ok(BoundedRead::Bytes(bytes)) => bytes,
            Ok(BoundedRead::TooLarge { actual }) => {
                return Err(ResourceError::JournalTooLarge {
                    actual,
                    limit: config.max_log_bytes,
                });
            }
            Err(error) => {
                return Err(ResourceError::io("read_truncated_event_log", error));
            }
        };
        let offset = usize::try_from(*byte_offset)
            .map_err(|_| ResourceError::TruncatedTailRecoveryNotApplicable)?;
        let tail = bytes
            .get(offset..)
            .filter(|tail| !tail.is_empty())
            .ok_or(ResourceError::TruncatedTailRecoveryNotApplicable)?;
        let evidence_path = write_truncated_tail_evidence(
            &session_dir,
            tail,
            config.durability == Durability::FlushAndSync,
        )?;

        let file = OpenOptions::new()
            .write(true)
            .open(&log_path)
            .map_err(|error| ResourceError::io("open_event_log_for_tail_recovery", error))?;
        file.set_len(*byte_offset)
            .map_err(|error| ResourceError::io("truncate_event_log_tail", error))?;
        if config.durability == Durability::FlushAndSync {
            file.sync_all()
                .map_err(|error| ResourceError::io("sync_recovered_event_log", error))?;
        }

        let recovered = load_session(&session_id, &log_path, &snapshot_path, config)?;
        if !recovered.issues.is_empty() {
            return Err(ResourceError::CorruptReadOnly);
        }
        if recovered.snapshot_needs_rebuild {
            if let Ok(anchor) = complete_log_anchor(&log_path, config.max_log_bytes) {
                let _ = write_snapshot_file(
                    &snapshot_path,
                    &recovered.state,
                    anchor,
                    config.durability == Durability::FlushAndSync,
                    config.max_log_bytes,
                );
            }
        }
        let preserved_bytes = tail.len() as u64;
        let recovered_sequence = recovered.state.last_sequence;
        let inner = Arc::new(Mutex::new(JournalInner {
            state: recovered.state,
            history_index: recovered.history_index,
            event_index: recovered.event_index,
            record_end_offsets: recovered.record_end_offsets,
            log_len: recovered.log_len,
            log_stamp: recovered.log_stamp,
            read_only: false,
            directory_sync_required: config.durability == Durability::FlushAndSync,
            pending_records: 0,
            synced_through_sequence: recovered_sequence,
            pending_since: None,
            batch_worker_active: false,
            batch_retry_attempts: 0,
            batch_retry_at: None,
            batch_flush_failure: None,
        }));
        let batch_target = Arc::new(BatchFlushTarget {
            log_path: log_path.clone(),
            session_dir: session_dir.clone(),
            lock_path: lock_path.clone(),
            #[cfg(any(test, feature = "test-support"))]
            sync_count: std::sync::atomic::AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_attempt_count: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            background_sync_delay_ms: AtomicU64::new(0),
        });
        let batch_job = Arc::new(BatchFlushJob::new(&inner, &batch_target));
        Ok(TruncatedTailRecovery {
            journal: Self {
                session_id,
                session_dir,
                log_path,
                snapshot_path,
                lock_path,
                config,
                artifact_validator,
                inner,
                batch_target,
                batch_job,
            },
            evidence_path,
            preserved_bytes,
        })
    }

    /// 返回当前内存中与完整日志重放一致的状态快照。
    ///
    /// 注意：该快照包含批量窗口内已写但尚未 sync 的事件（同实例读写一致）。
    pub fn state(&self) -> Result<SessionState, ResourceError> {
        self.read_state(Clone::clone)
    }

    /// 在权威状态上执行只读投影，不克隆完整 SessionState。
    ///
    /// 高频轮询（后台任务列表、活动状态检查、Session 列表）必须走此入口：
    /// 这些调用持有跨进程追加锁执行，若每次克隆 MB 级状态，并发轮询会退化成
    /// 持锁排队列车并饿死其余等待者。
    /// 读取只刷新已经完整写入的 JSONL，不承担 durability barrier；因此
    /// Runtime 在每次正式追加前读取 sequence 时不会把批量 fsync 退化为逐条。
    pub fn read_state<T>(
        &self,
        project: impl FnOnce(&SessionState) -> T,
    ) -> Result<T, ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        Ok(project(&inner.state))
    }

    /// 只复制实际工具图片的引用，预览时不克隆整个会话历史。
    pub fn tool_image_artifact(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<Option<ArtifactUse>, ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        Ok(inner
            .state
            .tools
            .values()
            .filter_map(|tool| tool.outcome.as_ref())
            .filter(|outcome| !outcome.result.is_error)
            .flat_map(|outcome| &outcome.result.content)
            .find_map(|part| {
                let artifact = match part {
                    ToolResultPart::Image {
                        source: MessageImageSource::Artifact { artifact },
                    }
                    | ToolResultPart::Artifact {
                        artifact,
                        materialization: ArtifactMaterialization::Image,
                    } => artifact,
                    _ => return None,
                };
                (&artifact.artifact_id == artifact_id).then(|| artifact.clone())
            }))
    }

    /// 返回与已验证日志同步的稀疏历史定位索引。
    pub fn history_index(&self) -> Result<SessionHistoryIndex, ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        Ok(inner.history_index.clone())
    }

    /// 按已验证的字节偏移读取物理事件页。
    pub fn read_page(
        &self,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<ReplayPage, ResourceError> {
        if limit == 0 || limit > MAX_REPLAY_PAGE_RECORDS {
            return Err(ResourceError::InvalidReplayPageLimit {
                actual: limit,
                limit: MAX_REPLAY_PAGE_RECORDS,
            });
        }
        if after_sequence == Some(0) {
            return Err(ResourceError::InvalidReplayCursor);
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        let after = after_sequence.unwrap_or(0);
        // 某些文件系统的修改时间精度不足以观察同长度改写；边界校验失败时
        // 重新加载一次权威日志，避免继续使用已经失效的内存偏移。
        let mut retried_after_reload = false;
        'read_page: loop {
            let through_sequence = inner.state.last_sequence;
            if after > through_sequence {
                return Err(ResourceError::InvalidReplayCursor);
            }
            if after == through_sequence {
                return Ok(ReplayPage {
                    records: Vec::new(),
                    next_after: None,
                    through_sequence,
                    has_more: false,
                });
            }

            let start_offset = if after == 0 {
                0
            } else {
                let index = match usize::try_from(after - 1) {
                    Ok(index) => index,
                    Err(_) if !retried_after_reload => {
                        self.reload_from_disk(&mut inner)?;
                        retried_after_reload = true;
                        continue 'read_page;
                    }
                    Err(_) => {
                        inner.read_only = true;
                        return Err(ResourceError::ReplayLogChanged);
                    }
                };
                let Some(offset) = inner.record_end_offsets.get(index).copied() else {
                    if !retried_after_reload {
                        self.reload_from_disk(&mut inner)?;
                        retried_after_reload = true;
                        continue 'read_page;
                    }
                    inner.read_only = true;
                    return Err(ResourceError::ReplayLogChanged);
                };
                if offset > inner.log_len {
                    if !retried_after_reload {
                        self.reload_from_disk(&mut inner)?;
                        retried_after_reload = true;
                        continue 'read_page;
                    }
                    inner.read_only = true;
                    return Err(ResourceError::ReplayLogChanged);
                }
                offset
            };

            ensure_regular_file_or_absent(&self.log_path)?;
            let mut file = File::open(&self.log_path)
                .map_err(|error| ResourceError::io("open_event_log_for_replay", error))?;
            #[cfg(test)]
            REPLAY_SEEK_COUNT.with(|count| count.set(count.get().saturating_add(1)));
            file.seek(SeekFrom::Start(start_offset))
                .map_err(|error| ResourceError::io("seek_event_log_for_replay", error))?;
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            let available = through_sequence.saturating_sub(after);
            let take = usize::try_from(available).unwrap_or(usize::MAX).min(limit);
            let mut records = Vec::with_capacity(take);
            for offset in 0..take {
                let expected_sequence = after
                    .checked_add(
                        u64::try_from(offset).map_err(|_| ResourceError::ReplayLogChanged)?,
                    )
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or(ResourceError::ReplayLogChanged)?;
                let record = match read_replay_record(
                    &mut reader,
                    &mut line,
                    self.config.max_event_bytes,
                    &self.session_id,
                    expected_sequence,
                ) {
                    Ok(record) => record,
                    Err(error) if matches!(&error, &ResourceError::ReplayLogChanged) => {
                        if !retried_after_reload {
                            self.reload_from_disk(&mut inner)?;
                            retried_after_reload = true;
                            continue 'read_page;
                        }
                        inner.read_only = true;
                        return Err(error);
                    }
                    Err(error) => {
                        if matches!(error, ResourceError::EventTooLarge { .. }) {
                            inner.read_only = true;
                        }
                        return Err(error);
                    }
                };
                records.push(record);
            }
            if log_stamp(&self.log_path)? != inner.log_stamp {
                inner.read_only = true;
                return Err(ResourceError::ReplayLogChanged);
            }
            let next_after = records.last().map(|record| record.sequence);
            let has_more = next_after.is_some_and(|sequence| sequence < through_sequence);
            return Ok(ReplayPage {
                records,
                next_after,
                through_sequence,
                has_more,
            });
        }
    }

    /// 返回用于本地诊断和只读备份的 JSONL 路径。
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// 返回当前 Snapshot 路径。
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// 返回已验证的 Session 隔离目录。
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// 判断稳定事件标识是否已经提交到当前独占 Session 的权威日志。
    pub fn contains_event_id(&self, event_id: &SessionEventId) -> Result<bool, ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        Ok(inner.event_index.contains_key(event_id))
    }

    /// 使用稳定事件标识与最后已知 sequence 原子追加一行。
    ///
    /// 相同标识和正文可跨进程、跨重启安全重试；正文不同或 sequence 已变化时不会写入。
    pub fn append_idempotent(
        &self,
        event_id: SessionEventId,
        expected_sequence: u64,
        event: SessionEvent,
    ) -> Result<IdempotentAppendOutcome, ResourceError> {
        let event = validate_owned_atomic_batch_shape(event)
            .map_err(|error| ResourceError::Reduction(error.message))?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        if self.config.durability == Durability::FlushAndSync
            && inner.batch_flush_failure.is_some()
            && let Err(error) = self.flush_pending_locked(&mut inner, true)
        {
            // 先前后台刷盘已经耗尽有界重试。当前事件尚未写入，但整个
            // Session 的 durable 水位仍不确定；用结构化不确定结果冻结上层，
            // 不能继续扩大窗口或把本次事件误报为普通明确拒绝。
            return Ok(IdempotentAppendOutcome::Indeterminate { error });
        }
        let event_sha256 = canonical_json_sha256(&event)?;
        if let Some(existing) = inner.event_index.get(&event_id).cloned() {
            if existing.event_sha256 != event_sha256 {
                return Ok(IdempotentAppendOutcome::EventIdConflict {
                    existing_sequence: existing.sequence,
                });
            }
            if let Err(error) = self.confirm_committed_durability(&mut inner) {
                return Ok(IdempotentAppendOutcome::Indeterminate { error });
            }
            return Ok(IdempotentAppendOutcome::AlreadyCommitted {
                record: SessionEventRecord::new(
                    event_id,
                    self.session_id.clone(),
                    existing.sequence,
                    existing.time_unix_ms,
                    event,
                ),
            });
        }
        if inner.state.last_sequence != expected_sequence {
            return Ok(IdempotentAppendOutcome::SequenceConflict {
                expected_sequence,
                actual_sequence: inner.state.last_sequence,
            });
        }
        validate_event_artifacts(&self.session_id, &event, self.artifact_validator.as_deref())?;

        let sequence =
            inner
                .state
                .last_sequence
                .checked_add(1)
                .ok_or(ResourceError::JournalRecordLimit {
                    actual: u64::MAX,
                    limit: self.config.max_records,
                })?;
        if sequence > self.config.max_records {
            return Err(ResourceError::JournalRecordLimit {
                actual: sequence,
                limit: self.config.max_records,
            });
        }
        // 系统墙钟可能被 NTP 或用户向后校准；Journal 仍必须保持可重放的非递减时间。
        let time_unix_ms = unix_time_millis()?.max(inner.state.updated_at_unix_ms);
        let record = SessionEventRecord::new(
            event_id.clone(),
            self.session_id.clone(),
            sequence,
            time_unix_ms,
            event.clone(),
        );
        let mut line = match serialize_json_bounded(&record, self.config.max_event_bytes, false)? {
            BoundedJson::Bytes(bytes) => bytes,
            BoundedJson::TooLarge { actual } => {
                return Err(ResourceError::EventTooLarge {
                    actual,
                    limit: self.config.max_event_bytes,
                });
            }
        };
        line.push(b'\n');
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if line_len > self.config.max_event_bytes {
            return Err(ResourceError::EventTooLarge {
                actual: line_len,
                limit: self.config.max_event_bytes,
            });
        }
        let mut candidate = inner.state.clone();
        reduce_record_from_valid_state(&mut candidate, &record)
            .map_err(|error| ResourceError::Reduction(error.message))?;
        validate_state_collections(&candidate, self.config.max_state_collection_items)?;
        let next_log_len =
            inner
                .log_len
                .checked_add(line_len)
                .ok_or(ResourceError::JournalTooLarge {
                    actual: u64::MAX,
                    limit: self.config.max_log_bytes,
                })?;
        if next_log_len > self.config.max_log_bytes {
            return Err(ResourceError::JournalTooLarge {
                actual: next_log_len,
                limit: self.config.max_log_bytes,
            });
        }
        ensure_regular_file_or_absent(&self.log_path)?;
        let created = !self.log_path.exists();
        if created && self.config.durability == Durability::FlushAndSync {
            inner.directory_sync_required = true;
        }
        let original_log_len = inner.log_len;
        // FlushAndSync 走批量路径：整条 JSONL 行先写文件（crash 后恰好是
        // 旧 `recover_truncated_tail` 能处理的“完整行/坏尾”二态），内存投影
        // 立刻推进保证同实例读写一致，sync 按 64 条/100ms 合并。
        if self.config.durability == Durability::FlushAndSync {
            return self.append_batched(
                &mut inner,
                event_id,
                sequence,
                record,
                candidate,
                event_sha256,
                line,
                next_log_len,
            );
        }
        let started_write = StartedWriteContext {
            event_id: &event_id,
            event: &event,
            sequence,
            original_log_len,
            created,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|error| ResourceError::io("open_event_log", error))?;
        #[cfg(any(test, feature = "test-support"))]
        if take_append_fault(AppendFault::ZeroWrite) {
            drop(file);
            return self.reconcile_after_started_write(
                &mut inner,
                started_write,
                injected_io_error("append_event_zero_write"),
            );
        }
        #[cfg(any(test, feature = "test-support"))]
        if take_append_fault(AppendFault::PartialWrite) {
            let partial_len = (line.len() / 2).max(1);
            if let Err(error) = file.write_all(&line[..partial_len]) {
                drop(file);
                return self.reconcile_after_started_write(
                    &mut inner,
                    started_write,
                    ResourceError::io("append_event", error),
                );
            }
            drop(file);
            return self.reconcile_after_started_write(
                &mut inner,
                started_write,
                injected_io_error("append_event_partial"),
            );
        }
        if let Err(error) = file.write_all(&line) {
            drop(file);
            return self.reconcile_after_started_write(
                &mut inner,
                started_write,
                ResourceError::io("append_event", error),
            );
        }
        if let Err(error) = apply_durability(&mut file, self.config.durability) {
            drop(file);
            return self.reconcile_after_started_write(&mut inner, started_write, error);
        }
        drop(file);
        if self.config.durability == Durability::FlushAndSync && inner.directory_sync_required {
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::DirectorySync) {
                return self.reconcile_after_started_write(
                    &mut inner,
                    started_write,
                    injected_io_error("sync_event_log_directory"),
                );
            }
            if let Err(error) = sync_directory(&self.session_dir, true) {
                return self.reconcile_after_started_write(&mut inner, started_write, error);
            }
            inner.directory_sync_required = false;
        }

        #[cfg(any(test, feature = "test-support"))]
        if take_append_fault(AppendFault::PostWriteMetadata) {
            return self.reconcile_after_started_write(
                &mut inner,
                started_write,
                injected_io_error("stat_event_log"),
            );
        }
        let next_log_stamp = match log_stamp(&self.log_path) {
            Ok(stamp) => stamp,
            Err(error) => {
                return self.reconcile_after_started_write(&mut inner, started_write, error);
            }
        };
        inner.state = candidate;
        inner.event_index.insert(
            event_id,
            EventIndexEntry {
                sequence,
                time_unix_ms: record.time_unix_ms,
                event_sha256,
            },
        );
        inner.history_index.observe(sequence, &record.event);
        inner.record_end_offsets.push(next_log_len);
        inner.log_len = next_log_len;
        inner.log_stamp = next_log_stamp;
        write_metadata_index(&self.session_dir, &inner.state, &inner.log_stamp, false);
        let snapshot = if snapshot_due(self.config.snapshot_policy, sequence) {
            match complete_log_anchor(&self.log_path, self.config.max_log_bytes).and_then(
                |anchor| {
                    write_snapshot_file(
                        &self.snapshot_path,
                        &inner.state,
                        anchor,
                        self.config.durability == Durability::FlushAndSync,
                        self.config.max_log_bytes,
                    )
                },
            ) {
                Ok(()) => SnapshotStatus::Written,
                Err(error) => SnapshotStatus::Failed {
                    message: error.to_string(),
                },
            }
        } else {
            SnapshotStatus::NotDue
        };
        Ok(IdempotentAppendOutcome::Appended(AppendReceipt {
            record,
            snapshot,
        }))
    }

    /// 使用一个稳定批次标识把多项不可分割事件写入同一条物理 JSONL 记录。
    ///
    /// 批次内事件按给定顺序确定性归约，但只消耗一个 Journal sequence；调用方可用
    /// 此接口保证 Turn 起点与完整用户消息、恢复结果与合成 Transcript 等事实不会只
    /// 提交一半。相同批次标识和正文可安全重试，正文变化会返回事件标识冲突。
    pub fn append_batch_idempotent(
        &self,
        batch_id: SessionEventId,
        expected_sequence: u64,
        events: Vec<SessionEvent>,
    ) -> Result<IdempotentAppendOutcome, ResourceError> {
        self.append_idempotent(
            batch_id,
            expected_sequence,
            SessionEvent::AtomicBatch { events },
        )
    }

    /// 立即为当前完整状态写入一个原子 Snapshot。
    ///
    /// `FlushAndSync` 下先把 Snapshot 将要引用的日志前缀同步，再写原子
    /// Snapshot，禁止 durable Snapshot 锚定尚未 durable 的日志。
    pub fn write_snapshot(&self) -> Result<(), ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        if self.config.durability == Durability::FlushAndSync {
            self.flush_pending_locked(&mut inner, true)?;
        }
        let anchor = complete_log_anchor(&self.log_path, self.config.max_log_bytes)?;
        write_snapshot_file(
            &self.snapshot_path,
            &inner.state,
            anchor,
            self.config.durability == Durability::FlushAndSync,
            self.config.max_log_bytes,
        )
    }

    /// 把批量窗口内已写但尚未 sync 的事件一次性持久化（Barrier 语义）。
    ///
    /// `ack` 之后返回即表示当前锁内观察到的完整日志已经达到配置要求。
    /// mutation `Prepared` 记录等崩溃恢复锚点必须显式调用本方法。
    pub fn flush(&self) -> Result<(), ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        match self.config.durability {
            Durability::Buffered => Ok(()),
            Durability::Flush => {
                ensure_regular_file_or_absent(&self.log_path)?;
                if !self.log_path.exists() {
                    return Ok(());
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .open(&self.log_path)
                    .map_err(|error| ResourceError::io("open_event_log_for_flush", error))?;
                apply_durability(&mut file, Durability::Flush)
            }
            Durability::FlushAndSync => self.flush_pending_locked(&mut inner, true),
        }
    }

    /// 返回批量窗口内尚未 sync 的事件数，便于测试断言攒批行为。
    pub fn pending_flush_records(&self) -> Result<usize, ResourceError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        Ok(inner.pending_records)
    }

    /// 返回当前 Journal 实例实际执行的 `sync_data` 次数。
    #[cfg(any(test, feature = "test-support"))]
    pub fn sync_count_for_tests(&self) -> u64 {
        self.batch_target
            .sync_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 返回进程级调度器当前实际存活的刷盘线程数。
    #[cfg(any(test, feature = "test-support"))]
    pub fn active_batch_scheduler_workers_for_tests(&self) -> usize {
        batch_scheduler()
            .active_worker_count
            .load(Ordering::Acquire)
    }

    /// 返回进程级调度器当前实际存活或已预留的 fsync worker 数量。
    #[cfg(any(test, feature = "test-support"))]
    pub fn active_batch_flush_workers_for_tests(&self) -> usize {
        batch_scheduler()
            .active_flush_worker_count
            .load(Ordering::Acquire)
    }

    /// 返回进程级调度器累计启动的刷盘线程数。
    #[cfg(any(test, feature = "test-support"))]
    pub fn batch_scheduler_start_count_for_tests(&self) -> u64 {
        batch_scheduler().worker_start_count.load(Ordering::Relaxed)
    }

    /// 把当前或下一批的超时截止点推迟，供跨 crate 测试隔离 64 条计数阈值。
    #[cfg(any(test, feature = "test-support"))]
    pub fn defer_batch_timeout_for_tests(&self) -> Result<(), ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        inner.pending_since = Some(Instant::now() + Duration::from_secs(60));
        if inner.batch_worker_active {
            batch_scheduler().notify();
        }
        Ok(())
    }

    /// 在持有实例锁与跨进程追加锁期间同步当前批次。
    fn flush_pending_locked(
        &self,
        inner: &mut JournalInner,
        force: bool,
    ) -> Result<(), ResourceError> {
        let result = sync_pending_batch(&self.batch_target, inner, force, true);
        let scheduler = batch_scheduler();
        if result.is_ok() && inner.pending_records == 0 {
            inner.batch_worker_active = false;
            scheduler.cancel(self.batch_job.id);
        } else {
            scheduler.notify();
        }
        result
    }

    /// 首条待刷事件至多登记一次；所有 Journal 共用进程级 deadline 调度器。
    fn arm_batch_worker(&self, inner: &mut JournalInner) -> bool {
        if inner.pending_records == 0 {
            return true;
        }
        if inner.batch_worker_active {
            return true;
        }
        inner.batch_worker_active = true;
        if batch_scheduler().schedule(&self.batch_job) {
            true
        } else {
            inner.batch_worker_active = false;
            false
        }
    }

    /// 批量路径：整行先写文件、内存投影立刻推进，sync 按 64 条/100ms 合并。
    #[allow(clippy::too_many_arguments)]
    fn append_batched(
        &self,
        inner: &mut JournalInner,
        event_id: SessionEventId,
        sequence: u64,
        record: SessionEventRecord,
        candidate: SessionState,
        event_sha256: String,
        line: Vec<u8>,
        next_log_len: u64,
    ) -> Result<IdempotentAppendOutcome, ResourceError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|error| ResourceError::io("open_event_log", error))?;
        #[cfg(any(test, feature = "test-support"))]
        if take_append_fault(AppendFault::ZeroWrite) {
            drop(file);
            return self
                .reconcile_batched_write(inner, injected_io_error("append_event_zero_write"));
        }
        #[cfg(any(test, feature = "test-support"))]
        if take_append_fault(AppendFault::PartialWrite) {
            let partial_len = (line.len() / 2).max(1);
            if let Err(error) = file.write_all(&line[..partial_len]) {
                drop(file);
                return self
                    .reconcile_batched_write(inner, ResourceError::io("append_event", error));
            }
            drop(file);
            return self.reconcile_batched_write(inner, injected_io_error("append_event_partial"));
        }
        if let Err(error) = file.write_all(&line) {
            drop(file);
            return self.reconcile_batched_write(inner, ResourceError::io("append_event", error));
        }
        if let Err(error) = file.flush() {
            drop(file);
            return self
                .reconcile_batched_write(inner, ResourceError::io("flush_event_log", error));
        }
        // 整行字节已进入文件（OS 缓存）；先推进内存投影保证同实例读写
        // 一致，sync 延迟到刷盘阈值。注意：`flush` 让其他实例的
        // `read` 立即可见本行，避免"长度已变但内容不可读"窗口；这不
        // 改变 crash 语义（sync 仍按 64 条/100ms 合并）。
        drop(file);
        let next_log_stamp = match log_stamp(&self.log_path) {
            Ok(stamp) => stamp,
            Err(error) => return self.reconcile_batched_write(inner, error),
        };
        inner.state = candidate;
        inner.event_index.insert(
            event_id,
            EventIndexEntry {
                sequence,
                time_unix_ms: record.time_unix_ms,
                event_sha256,
            },
        );
        inner.history_index.observe(sequence, &record.event);
        inner.record_end_offsets.push(next_log_len);
        inner.log_len = next_log_len;
        inner.log_stamp = next_log_stamp;
        inner.observe_unsynced_through(sequence);
        write_metadata_index(&self.session_dir, &inner.state, &inner.log_stamp, false);
        let snapshot_due = snapshot_due(self.config.snapshot_policy, sequence);
        let due = inner.pending_records >= JOURNAL_BATCH_MAX_RECORDS
            || inner.batch_delay_expired()
            || snapshot_due
            // 让线程局部故障在发起 append 的线程内确定性生效，避免后台
            // worker（看不到该 thread_local）先完成同步而掩盖测试故障。
            || append_fault_pending();
        if due {
            // 满批/超时刷盘失败走旧语义：内存投影已含本条但落库未证明，
            // 只能返回不确定，调用方按幂等重试对账。
            if let Err(error) = self.flush_pending_locked(inner, false) {
                return Ok(IdempotentAppendOutcome::Indeterminate { error });
            }
        } else if !self.arm_batch_worker(inner)
            && let Err(error) = self.flush_pending_locked(inner, false)
        {
            // 无法创建一次性等待线程时退化为当前调用内刷盘，不能留下没有
            // 超时唤醒来源的无限窗口。
            return Ok(IdempotentAppendOutcome::Indeterminate { error });
        }
        let snapshot = if snapshot_due {
            match self.snapshot_for_current_prefix(inner) {
                Ok(()) => SnapshotStatus::Written,
                Err(error) => SnapshotStatus::Failed {
                    message: error.to_string(),
                },
            }
        } else {
            SnapshotStatus::NotDue
        };
        Ok(IdempotentAppendOutcome::Appended(AppendReceipt {
            record,
            snapshot,
        }))
    }

    /// 批量写行失败后从磁盘重建：先尝试回滚本次追加产生的截断尾，
    /// 再按旧逐条路径对账，保证内存投影不超前、单条部分行不残留。
    fn reconcile_batched_write(
        &self,
        inner: &mut JournalInner,
        error: ResourceError,
    ) -> Result<IdempotentAppendOutcome, ResourceError> {
        // 部分行失败与旧 PartialWrite 语义一致：本次追加前的日志长度是
        // 截断尾的精确起点，先回滚再重建，避免 load 直接判 Corrupt。
        if rollback_partial_append(
            &self.log_path,
            &self.session_dir,
            inner.log_len,
            false,
            Durability::Buffered,
        )
        .is_ok()
            && self.reload_from_disk(inner).is_ok()
        {
            return Ok(IdempotentAppendOutcome::Indeterminate { error });
        }
        inner.read_only = true;
        Ok(IdempotentAppendOutcome::Indeterminate { error })
    }

    /// 为已经完成 durability barrier 的当前前缀写 Snapshot。
    fn snapshot_for_current_prefix(&self, inner: &JournalInner) -> Result<(), ResourceError> {
        let anchor = complete_log_anchor(&self.log_path, self.config.max_log_bytes)?;
        write_snapshot_file(
            &self.snapshot_path,
            &inner.state,
            anchor,
            true,
            self.config.max_log_bytes,
        )
    }

    /// 多实例写入改变文件时，在持有 OS 文件锁期间重新加载状态。
    ///
    /// 刷盘本身不得更新 `log_stamp`：sync 不改变日志内容，若把外部实例
    /// 的新长度写进本实例 stamp 却不刷新 state，会掩盖变化并产生重复
    /// sequence。这里持有 append.lock，读取期间不存在合法并发追加。
    fn refresh_if_changed(&self, inner: &mut JournalInner) -> Result<(), ResourceError> {
        let current_stamp = log_stamp(&self.log_path)?;
        if current_stamp == inner.log_stamp {
            return Ok(());
        }
        if current_stamp.len < inner.log_len {
            inner.read_only = true;
            return Err(ResourceError::ReplayLogChanged);
        }
        self.reload_from_disk(inner)
    }

    /// 从权威日志重新建立状态、幂等索引和物理记录边界。
    fn reload_from_disk(&self, inner: &mut JournalInner) -> Result<(), ResourceError> {
        let loaded = load_session(
            &self.session_id,
            &self.log_path,
            &self.snapshot_path,
            self.config,
        )?;
        if !loaded.issues.is_empty() {
            inner.read_only = true;
            return Err(ResourceError::CorruptReadOnly);
        }
        let sequence = loaded.state.last_sequence;
        install_loaded_session(inner, loaded);
        if self.config.durability == Durability::FlushAndSync {
            inner.observe_unsynced_through(sequence);
            if !self.arm_batch_worker(inner) {
                // 调度线程创建失败时在当前 append.lock 临界区直接完成同步，
                // 不能让一次只读 reload 留下没有超时唤醒来源的窗口。
                self.flush_pending_locked(inner, false)?;
            }
        }
        Ok(())
    }

    /// 在幂等重试确认已存在事件前补齐调用方要求的文件和目录持久化等级。
    fn confirm_committed_durability(&self, inner: &mut JournalInner) -> Result<(), ResourceError> {
        if self.config.durability == Durability::Buffered {
            return Ok(());
        }
        if self.config.durability == Durability::Flush {
            ensure_regular_file_or_absent(&self.log_path)?;
            let mut file = OpenOptions::new()
                .write(true)
                .open(&self.log_path)
                .map_err(|error| ResourceError::io("open_event_log_for_durability", error))?;
            apply_durability(&mut file, self.config.durability)?;
            return Ok(());
        }
        // 幂等确认是显式 durability barrier；即使本实例没有待刷计数，
        // 也同步当前锁内观察到的完整文件，覆盖跨实例/跨进程已可见事件。
        self.flush_pending_locked(inner, true)
    }

    /// 写入开始后的失败通过权威日志重读对账，避免把已提交事件误报为普通错误。
    fn reconcile_after_started_write(
        &self,
        inner: &mut JournalInner,
        context: StartedWriteContext<'_>,
        error: ResourceError,
    ) -> Result<IdempotentAppendOutcome, ResourceError> {
        let mut loaded = match load_session(
            &self.session_id,
            &self.log_path,
            &self.snapshot_path,
            self.config,
        ) {
            Ok(loaded) => loaded,
            Err(_) => {
                inner.read_only = true;
                return Ok(IdempotentAppendOutcome::Indeterminate { error });
            }
        };
        if !loaded.issues.is_empty() {
            let is_own_partial_tail = loaded.issues.len() == 1
                && matches!(
                    loaded.issues[0].kind,
                    CorruptionKind::TruncatedTail { byte_offset }
                        if byte_offset == context.original_log_len
                )
                && loaded.state.last_sequence.saturating_add(1) == context.sequence;
            if !is_own_partial_tail {
                inner.read_only = true;
                return Ok(IdempotentAppendOutcome::Indeterminate { error });
            }
            if rollback_partial_append(
                &self.log_path,
                &self.session_dir,
                context.original_log_len,
                context.created,
                self.config.durability,
            )
            .is_err()
            {
                inner.read_only = true;
                return Ok(IdempotentAppendOutcome::Indeterminate { error });
            }
            loaded = match load_session(
                &self.session_id,
                &self.log_path,
                &self.snapshot_path,
                self.config,
            ) {
                Ok(loaded) if loaded.issues.is_empty() => loaded,
                Ok(_) | Err(_) => {
                    inner.read_only = true;
                    return Ok(IdempotentAppendOutcome::Indeterminate { error });
                }
            };
        }
        install_loaded_session(inner, loaded);
        let event_sha256 = canonical_json_sha256(context.event).ok();
        let _commit_visible = inner
            .event_index
            .get(context.event_id)
            .is_some_and(|entry| {
                entry.sequence == context.sequence
                    && Some(&entry.event_sha256) == event_sha256.as_ref()
            });
        // 即使当前文件内容可见，失败的 flush/fsync/目录同步或提交后元数据读取
        // 仍无法证明调用方要求的持久化等级已经满足，因此只能返回不确定结果。
        Ok(IdempotentAppendOutcome::Indeterminate { error })
    }
}

/// 在 append.lock 内把当前完整 JSONL 文件同步到稳定存储。
///
/// `pending_records` 只记录需要 barrier 的数量，不保存正文；正文已经由
/// `File::write_all` 写入文件。失败时保留计数与起始时间，后续 append、显式
/// barrier 或 Drop 会再次尝试，不能把未证明 durable 的批次误标为已同步。
fn sync_pending_batch(
    target: &BatchFlushTarget,
    inner: &mut JournalInner,
    force: bool,
    inject_faults: bool,
) -> Result<(), ResourceError> {
    if inner.pending_records == 0 && !force {
        return Ok(());
    }
    ensure_regular_file_or_absent(&target.log_path)?;
    if !target.log_path.exists() {
        return if inner.pending_records == 0 {
            Ok(())
        } else {
            Err(ResourceError::io(
                "open_event_log_for_flush",
                std::io::Error::new(std::io::ErrorKind::NotFound, "待刷事件日志不存在"),
            ))
        };
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(&target.log_path)
        .map_err(|error| ResourceError::io("open_event_log_for_flush", error))?;
    #[cfg(any(test, feature = "test-support"))]
    if inject_faults && take_append_fault(AppendFault::Flush) {
        return Err(injected_io_error("flush_event_log"));
    }
    file.flush()
        .map_err(|error| ResourceError::io("flush_event_log", error))?;
    #[cfg(any(test, feature = "test-support"))]
    if inject_faults
        && (take_append_fault(AppendFault::Sync) || take_append_fault(AppendFault::BarrierSync))
    {
        return Err(injected_io_error("sync_event_log"));
    }
    #[cfg(any(test, feature = "test-support"))]
    if !inject_faults {
        target
            .background_attempt_count
            .fetch_add(1, Ordering::Relaxed);
        if target
            .background_failures_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                (remaining > 0).then(|| remaining - 1)
            })
            .is_ok()
        {
            return Err(injected_io_error("background_sync_event_log"));
        }
        let delay_ms = target.background_sync_delay_ms.load(Ordering::Acquire);
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = inject_faults;
    file.sync_data()
        .map_err(|error| ResourceError::io("sync_event_log", error))?;
    count_sync_for_tests(target);
    drop(file);
    if inner.directory_sync_required {
        #[cfg(any(test, feature = "test-support"))]
        if inject_faults && take_append_fault(AppendFault::DirectorySync) {
            return Err(injected_io_error("sync_event_log_directory"));
        }
        sync_directory(&target.session_dir, true)?;
        inner.directory_sync_required = false;
    }
    #[cfg(any(test, feature = "test-support"))]
    if inject_faults && take_append_fault(AppendFault::PostWriteMetadata) {
        return Err(injected_io_error("stat_event_log"));
    }
    inner.synced_through_sequence = inner.state.last_sequence;
    inner.pending_records = 0;
    inner.pending_since = None;
    inner.batch_retry_attempts = 0;
    inner.batch_retry_at = None;
    inner.batch_flush_failure = None;
    Ok(())
}

/// 返回后台连续失败后的指数退避；总重试窗口保持有界且无需轮询。
fn batch_retry_delay(attempts: u32) -> Duration {
    let shift = attempts.saturating_sub(1).min(3);
    Duration::from_millis(5_u64 << shift)
}

/// 只读取一个未执行 Journal 的下一截止时间；调度线程不得在这里执行 fsync。
fn batch_job_deadline(scheduler: &BatchScheduler, job: &BatchFlushJob) -> Option<Instant> {
    let Some(shared_inner) = job.inner.upgrade() else {
        scheduler.cancel(job.id);
        return None;
    };
    let Ok(mut inner) = shared_inner.lock() else {
        scheduler.cancel(job.id);
        return None;
    };
    if inner.pending_records == 0 || inner.batch_flush_failure.is_some() {
        inner.batch_worker_active = false;
        scheduler.cancel(job.id);
        return None;
    }
    inner
        .batch_retry_at
        .or_else(|| {
            inner
                .pending_since
                .map(|started| started + JOURNAL_BATCH_MAX_DELAY)
        })
        .or_else(|| {
            inner.batch_flush_failure = Some("待刷批次缺少超时起点".to_owned());
            inner.batch_worker_active = false;
            scheduler.cancel(job.id);
            None
        })
}

/// 执行一个到期 Journal；成功/耗尽时在同一 Journal 锁内取消登记，避免丢失重新 arm。
fn process_batch_job(scheduler: &BatchScheduler, job: &BatchFlushJob) -> Option<Instant> {
    let Some(shared_inner) = job.inner.upgrade() else {
        scheduler.cancel(job.id);
        return None;
    };
    let Some(target) = job.target.upgrade() else {
        scheduler.cancel(job.id);
        return None;
    };
    let Ok(mut inner) = shared_inner.lock() else {
        scheduler.cancel(job.id);
        return None;
    };
    if inner.pending_records == 0 || inner.batch_flush_failure.is_some() {
        inner.batch_worker_active = false;
        scheduler.cancel(job.id);
        return None;
    }
    let Some(deadline) = inner.batch_retry_at.or_else(|| {
        inner
            .pending_since
            .map(|started| started + JOURNAL_BATCH_MAX_DELAY)
    }) else {
        inner.batch_flush_failure = Some("待刷批次缺少超时起点".to_owned());
        inner.batch_worker_active = false;
        scheduler.cancel(job.id);
        return None;
    };
    let now = Instant::now();
    if now < deadline {
        return Some(deadline);
    }
    let result = exclusive_lock_with_timeout(&target.lock_path, JOURNAL_BATCH_LOCK_WAIT)
        .and_then(|_file_lock| sync_pending_batch(&target, &mut inner, false, false));
    match result {
        Ok(()) => {
            inner.batch_worker_active = false;
            scheduler.cancel(job.id);
            None
        }
        Err(error) => {
            inner.batch_retry_attempts = inner.batch_retry_attempts.saturating_add(1);
            if inner.batch_retry_attempts < JOURNAL_BATCH_MAX_ATTEMPTS {
                let retry_at = Instant::now() + batch_retry_delay(inner.batch_retry_attempts);
                inner.batch_retry_at = Some(retry_at);
                Some(retry_at)
            } else {
                inner.batch_retry_at = None;
                inner.batch_flush_failure = Some(error.to_string());
                inner.batch_worker_active = false;
                scheduler.cancel(job.id);
                None
            }
        }
    }
}

/// 线程创建失败时形成可观察 sticky 栅栏，下一次显式 barrier 可继续对账。
fn mark_batch_dispatch_failure(scheduler: &BatchScheduler, job: &BatchFlushJob) {
    let Some(shared_inner) = job.inner.upgrade() else {
        scheduler.cancel(job.id);
        return;
    };
    let Ok(mut inner) = shared_inner.lock() else {
        scheduler.cancel(job.id);
        return;
    };
    if inner.pending_records > 0 {
        inner.batch_flush_failure = Some("无法创建 Journal fsync 线程".to_owned());
    }
    inner.batch_worker_active = false;
    scheduler.cancel(job.id);
}

/// 为一个到期 Journal 派发一个有界短生命周期 fsync worker；同一任务由 in-flight 集合去重。
fn dispatch_batch_job(scheduler: &Arc<BatchScheduler>, job: Arc<BatchFlushJob>) -> bool {
    if !scheduler.begin_flush(job.id) {
        return false;
    }
    let worker_scheduler = Arc::clone(scheduler);
    let worker_job = Arc::clone(&job);
    let spawned = std::thread::Builder::new()
        .name(format!("keencode-journal-fsync-{}", job.id))
        .spawn(move || {
            let _completion =
                BatchFlushCompletionGuard::new(Arc::clone(&worker_scheduler), worker_job.id);
            let _ = process_batch_job(&worker_scheduler, &worker_job);
        });
    if spawned.is_err() {
        mark_batch_dispatch_failure(scheduler, &job);
        scheduler.finish_flush(job.id);
    }
    true
}

/// 等待所有 Journal 的最早截止时间并有界并发派发到期 fsync；队列空闲后释放调度线程。
fn run_batch_scheduler(scheduler: Arc<BatchScheduler>) {
    #[cfg(any(test, feature = "test-support"))]
    let _active_worker_guard = ActiveBatchSchedulerGuard::new(&scheduler);
    loop {
        let (jobs, observed_revision) = {
            let Ok(mut state) = scheduler.state.lock() else {
                return;
            };
            state.jobs.retain(|_, job| job.strong_count() > 0);
            if state.jobs.is_empty() {
                let Ok((mut next, timeout)) = scheduler
                    .wake
                    .wait_timeout(state, JOURNAL_BATCH_SCHEDULER_IDLE_TIMEOUT)
                else {
                    return;
                };
                next.jobs.retain(|_, job| job.strong_count() > 0);
                if timeout.timed_out() && next.jobs.is_empty() {
                    next.worker_active = false;
                    return;
                }
                continue;
            }
            let jobs = state
                .jobs
                .iter()
                .filter(|(id, _)| !state.in_flight.contains(id))
                .map(|(_, job)| job.clone())
                .collect::<Vec<_>>();
            if jobs.is_empty() {
                if scheduler.wake.wait(state).is_err() {
                    return;
                }
                continue;
            }
            (jobs, state.revision)
        };
        let mut next_deadline = None;
        let mut due = Vec::new();
        let now = Instant::now();
        for job in jobs.into_iter().filter_map(|job| job.upgrade()) {
            if let Some(deadline) = batch_job_deadline(&scheduler, &job) {
                if deadline <= now {
                    due.push(job);
                } else {
                    next_deadline = Some(
                        next_deadline.map_or(deadline, |current: Instant| current.min(deadline)),
                    );
                }
            }
        }
        if !due.is_empty() {
            let mut dispatched = false;
            for job in due {
                dispatched |= dispatch_batch_job(&scheduler, job);
            }
            if !dispatched {
                let Ok(state) = scheduler.state.lock() else {
                    return;
                };
                if state.in_flight.len() >= JOURNAL_BATCH_MAX_CONCURRENT_FSYNC
                    && scheduler.wake.wait(state).is_err()
                {
                    return;
                }
            }
            continue;
        }
        let Some(deadline) = next_deadline else {
            continue;
        };
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            continue;
        }
        let Ok(state) = scheduler.state.lock() else {
            return;
        };
        // Condvar 通知发生在任务处理阶段时不会被后续 wait 捕获；revision
        // 让 worker 在真正休眠前重新计算所有 deadline，避免把已到期任务
        // 按旧的未来 deadline 再等待一轮。
        if state.revision != observed_revision {
            continue;
        }
        if scheduler.wake.wait_timeout(state, wait).is_err() {
            return;
        }
    }
}

impl Drop for SessionJournal {
    /// 关闭前尽力完成当前批次；Drop 无法报告 IO 错误，因此失败时仍保留
    /// 文件中的完整 JSONL 行，并由截尾/重放恢复语义处理。
    fn drop(&mut self) {
        if self.config.durability != Durability::FlushAndSync {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.batch_worker_active = false;
        batch_scheduler().cancel(self.batch_job.id);
        if inner.pending_records == 0 {
            return;
        }
        let _file_lock = match exclusive_lock(&self.lock_path) {
            Ok(lock) => lock,
            Err(_) => return,
        };
        let _ = sync_pending_batch(&self.batch_target, &mut inner, false, true);
    }
}

/// 在持有跨进程锁时回滚本次追加产生且可精确定位的截断尾记录。
fn rollback_partial_append(
    log_path: &Path,
    session_dir: &Path,
    original_log_len: u64,
    created: bool,
    durability: Durability,
) -> Result<(), ResourceError> {
    let file = OpenOptions::new()
        .write(true)
        .open(log_path)
        .map_err(|error| ResourceError::io("open_partial_event_log", error))?;
    file.set_len(original_log_len)
        .map_err(|error| ResourceError::io("rollback_partial_event", error))?;
    if durability == Durability::FlushAndSync {
        file.sync_all()
            .map_err(|error| ResourceError::io("sync_partial_rollback", error))?;
    }
    drop(file);
    if created && durability == Durability::FlushAndSync {
        sync_directory(session_dir, true)?;
    }
    Ok(())
}

/// Snapshot 文件的完整自描述结构。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionSnapshot {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// Snapshot 所属 Session。
    session: SessionId,
    /// 状态覆盖到的最后一个 sequence。
    through_sequence: u64,
    /// 对应 JSONL 行的 SHA-256；空状态为 `None`。
    through_event_sha256: Option<String>,
    /// 覆盖日志前缀（含换行）的 SHA-256；空状态为 `None`。
    through_log_sha256: Option<String>,
    /// 规范序列化完整状态的 SHA-256。
    state_sha256: String,
    /// 完整类型化归约状态。
    state: SessionState,
}

/// 写入时借用现有状态，避免为 Snapshot 再克隆一次完整 Session。
#[derive(Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionSnapshotRef<'a> {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// Snapshot 所属 Session。
    session: SessionId,
    /// 状态覆盖到的最后一个 sequence。
    through_sequence: u64,
    /// 对应 JSONL 行的 SHA-256；空状态为 `None`。
    through_event_sha256: Option<String>,
    /// 覆盖日志前缀的 SHA-256；空状态为 `None`。
    through_log_sha256: Option<String>,
    /// 规范序列化完整状态的 SHA-256。
    state_sha256: String,
    /// 借用的完整类型化归约状态。
    state: &'a SessionState,
}

/// 日志与 Snapshot 的只读加载结果。
struct LoadedSession {
    history_index: SessionHistoryIndex,
    /// 可确定的最终或前缀状态。
    state: SessionState,
    /// 健康日志中全部已验证记录的幂等事件索引。
    event_index: BTreeMap<SessionEventId, EventIndexEntry>,
    /// 每条物理 JSONL 记录包含换行符后的排他结束字节偏移。
    record_end_offsets: Vec<u64>,
    /// 可确定的完整记录数。
    valid_records: usize,
    /// 实际日志文件长度。
    log_len: u64,
    /// 读取完成时观察到的日志变化戳。
    log_stamp: LogStamp,
    /// 损坏事实。
    issues: Vec<CorruptionIssue>,
    /// Snapshot 缺失以外的缓存损坏是否需要按健康日志重建。
    snapshot_needs_rebuild: bool,
}

/// 将一次完整日志加载结果原子替换到当前实例的内存投影。
fn install_loaded_session(inner: &mut JournalInner, loaded: LoadedSession) {
    inner.state = loaded.state;
    inner.history_index = loaded.history_index;
    inner.event_index = loaded.event_index;
    inner.record_end_offsets = loaded.record_end_offsets;
    inner.log_len = loaded.log_len;
    inner.log_stamp = loaded.log_stamp;
}

/// 一次 JSONL 结构读取的结果。
struct ReadRecords {
    /// 首个结构损坏点之前的类型化事件。
    records: Vec<SessionEventRecord>,
    /// 每个完整事件行正文的 SHA-256。
    event_hashes: Vec<String>,
    /// 截止每行且包含换行的日志前缀 SHA-256。
    prefix_hashes: Vec<String>,
    /// 每条完整物理 JSONL 记录包含换行符后的排他结束字节偏移。
    record_end_offsets: Vec<u64>,
    /// 实际日志文件长度。
    log_len: u64,
    /// 读取完成时观察到的日志变化戳。
    log_stamp: LogStamp,
    /// JSONL 结构损坏事实。
    issues: Vec<CorruptionIssue>,
}

/// 读取日志，优先从可校验 Snapshot 只归约尾部；坏 Snapshot 作为可重建缓存忽略。
fn load_session(
    session_id: &SessionId,
    log_path: &Path,
    snapshot_path: &Path,
    config: JournalConfig,
) -> Result<LoadedSession, ResourceError> {
    let read = read_records(session_id, log_path, config)?;
    let mut snapshot_needs_rebuild = false;
    let mut snapshot = None;
    if snapshot_path.exists() {
        ensure_regular_file_or_absent(snapshot_path)?;
        match read_snapshot(snapshot_path, config.max_log_bytes) {
            Ok(candidate) if snapshot_is_valid(session_id, &candidate, &read, config) => {
                snapshot = Some(candidate);
            }
            Ok(_) | Err(_) => snapshot_needs_rebuild = true,
        }
    }

    let (mut state, start) = snapshot.map_or_else(
        || (SessionState::empty(session_id.clone()), 0),
        |snapshot| {
            let start = snapshot.through_sequence as usize;
            (snapshot.state, start)
        },
    );
    validate_state_collections(&state, config.max_state_collection_items)?;
    let mut valid_records = start;
    let mut issues = read.issues;
    for record in read.records.iter().skip(start) {
        if let Err(error) = reduce_record_from_valid_state(&mut state, record) {
            issues.push(CorruptionIssue::new(
                CorruptionKind::ReductionFailure {
                    sequence: record.sequence,
                },
                format!("事件无法归约：{}", error.message),
            ));
            break;
        }
        validate_state_collections(&state, config.max_state_collection_items)?;
        valid_records += 1;
    }

    let mut history_index = SessionHistoryIndex::default();
    for record in read.records.iter().take(valid_records) {
        history_index.observe(record.sequence, &record.event);
    }
    Ok(LoadedSession {
        history_index,
        state,
        event_index: read
            .records
            .iter()
            .take(valid_records)
            .map(|record| {
                Ok((
                    record.event_id.clone(),
                    EventIndexEntry {
                        sequence: record.sequence,
                        time_unix_ms: record.time_unix_ms,
                        event_sha256: canonical_json_sha256(&record.event)?,
                    },
                ))
            })
            .collect::<Result<_, ResourceError>>()?,
        record_end_offsets: read.record_end_offsets,
        valid_records,
        log_len: read.log_len,
        log_stamp: read.log_stamp,
        issues,
        snapshot_needs_rebuild,
    })
}

/// 读取完整换行记录并检查 envelope 与 sequence。
fn read_records(
    session_id: &SessionId,
    log_path: &Path,
    config: JournalConfig,
) -> Result<ReadRecords, ResourceError> {
    if !log_path.exists() {
        return Ok(ReadRecords {
            records: Vec::new(),
            event_hashes: Vec::new(),
            prefix_hashes: Vec::new(),
            record_end_offsets: Vec::new(),
            log_len: 0,
            log_stamp: LogStamp {
                len: 0,
                modified: None,
            },
            issues: Vec::new(),
        });
    }
    ensure_regular_file_or_absent(log_path)?;
    let bytes = match read_file_bounded(log_path, config.max_log_bytes) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { actual }) => {
            return Err(ResourceError::JournalTooLarge {
                actual,
                limit: config.max_log_bytes,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReadRecords {
                records: Vec::new(),
                event_hashes: Vec::new(),
                prefix_hashes: Vec::new(),
                record_end_offsets: Vec::new(),
                log_len: 0,
                log_stamp: LogStamp {
                    len: 0,
                    modified: None,
                },
                issues: Vec::new(),
            });
        }
        Err(error) => return Err(ResourceError::io("read_event_log", error)),
    };
    let log_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if log_len > config.max_log_bytes {
        return Err(ResourceError::JournalTooLarge {
            actual: log_len,
            limit: config.max_log_bytes,
        });
    }
    let mut issues = Vec::new();
    let complete_len = if bytes.is_empty() || bytes.ends_with(b"\n") {
        bytes.len()
    } else {
        let offset = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        issues.push(CorruptionIssue::new(
            CorruptionKind::TruncatedTail {
                byte_offset: offset as u64,
            },
            "事件日志包含未完整落盘的尾记录",
        ));
        let tail_len = u64::try_from(bytes.len() - offset).unwrap_or(u64::MAX);
        if tail_len > config.max_event_bytes {
            let line = bytes[..offset]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count()
                + 1;
            issues.push(CorruptionIssue::new(
                CorruptionKind::EventTooLarge {
                    line,
                    actual: tail_len,
                    limit: config.max_event_bytes,
                },
                format!("第 {line} 行的截断事件超过大小限制"),
            ));
        }
        offset
    };
    let mut records = Vec::new();
    let mut event_hashes = Vec::new();
    let mut prefix_hashes = Vec::new();
    let mut record_end_offsets = Vec::new();
    let mut prefix_hasher = Sha256::new();
    let mut previous = 0_u64;
    let mut event_sequences = BTreeMap::new();
    let mut consumed_len = 0_usize;
    if complete_len == 1 && bytes.first() == Some(&b'\n') {
        issues.push(CorruptionIssue::new(
            CorruptionKind::InvalidJson { line: 1 },
            "事件日志包含空的 JSONL 记录",
        ));
    }
    if complete_len > 0 {
        // 去掉唯一允许的末尾换行；中间空行必须保留并报告为损坏。
        let complete_records = &bytes[..complete_len - 1];
        for (index, line) in complete_records.split(|byte| *byte == b'\n').enumerate() {
            let line_number = index + 1;
            let record_count = u64::try_from(line_number).unwrap_or(u64::MAX);
            if record_count > config.max_records {
                return Err(ResourceError::JournalRecordLimit {
                    actual: record_count,
                    limit: config.max_records,
                });
            }
            let event_bytes = u64::try_from(line.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            if event_bytes > config.max_event_bytes {
                issues.push(CorruptionIssue::new(
                    CorruptionKind::EventTooLarge {
                        line: line_number,
                        actual: event_bytes,
                        limit: config.max_event_bytes,
                    },
                    format!("第 {line_number} 行的事件超过大小限制"),
                ));
                break;
            }
            let record: SessionEventRecord = match serde_json::from_slice(line) {
                Ok(record) => record,
                Err(_) => {
                    issues.push(CorruptionIssue::new(
                        CorruptionKind::InvalidJson { line: line_number },
                        format!("第 {line_number} 行不是有效事件 JSON"),
                    ));
                    break;
                }
            };
            if record.schema != crate::types::SESSION_EVENT_SCHEMA
                || record.version != crate::types::SESSION_EVENT_VERSION
                || record.session != *session_id
            {
                issues.push(CorruptionIssue::new(
                    CorruptionKind::EnvelopeMismatch { line: line_number },
                    format!("第 {line_number} 行的事件 envelope 不匹配"),
                ));
                break;
            }
            let expected = previous.saturating_add(1);
            if record.sequence != expected {
                let kind = if previous != 0 && record.sequence == previous {
                    CorruptionKind::DuplicateSequence {
                        sequence: record.sequence,
                    }
                } else if record.sequence < expected {
                    CorruptionKind::OutOfOrderSequence {
                        previous,
                        actual: record.sequence,
                    }
                } else {
                    CorruptionKind::SequenceGap {
                        expected,
                        actual: record.sequence,
                    }
                };
                issues.push(CorruptionIssue::new(
                    kind,
                    format!("第 {line_number} 行的 sequence 不连续"),
                ));
                break;
            }
            if let Some(first_sequence) = event_sequences.get(&record.event_id) {
                issues.push(CorruptionIssue::new(
                    CorruptionKind::DuplicateEventId {
                        event_id: record.event_id.to_string(),
                        first_sequence: *first_sequence,
                        duplicate_sequence: record.sequence,
                    },
                    format!("第 {line_number} 行的事件标识重复"),
                ));
                break;
            }
            previous = record.sequence;
            event_sequences.insert(record.event_id.clone(), record.sequence);
            event_hashes.push(sha256_hex(line));
            prefix_hasher.update(line);
            prefix_hasher.update(b"\n");
            prefix_hashes.push(digest_hex(prefix_hasher.clone().finalize()));
            records.push(record);
            consumed_len += line.len() + 1;
            record_end_offsets.push(u64::try_from(consumed_len).unwrap_or(u64::MAX));
        }
    }
    Ok(ReadRecords {
        records,
        event_hashes,
        prefix_hashes,
        record_end_offsets,
        log_len,
        log_stamp: log_stamp(log_path)?,
        issues,
    })
}

/// 从缓冲读取器取得一条包含终止换行的有界 JSONL 记录，并去掉行尾换行。
fn read_jsonl_line_bounded(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max_event_bytes: u64,
) -> Result<bool, ResourceError> {
    line.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| ResourceError::io("read_event_log_for_replay", error))?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(false);
            }
            return Err(ResourceError::ReplayLogChanged);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let next_len = u64::try_from(line.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
        if next_len > max_event_bytes {
            return Err(ResourceError::EventTooLarge {
                actual: next_len,
                limit: max_event_bytes,
            });
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            line.pop();
            return Ok(true);
        }
    }
}

/// 流式读取并验证一条重放记录的 envelope 和精确 sequence。
fn read_replay_record(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max_event_bytes: u64,
    session_id: &SessionId,
    expected_sequence: u64,
) -> Result<SessionEventRecord, ResourceError> {
    if !read_jsonl_line_bounded(reader, line, max_event_bytes)? {
        return Err(ResourceError::ReplayLogChanged);
    }
    let record: SessionEventRecord =
        serde_json::from_slice(line).map_err(|_| ResourceError::ReplayLogChanged)?;
    if record.schema != crate::types::SESSION_EVENT_SCHEMA
        || record.version != crate::types::SESSION_EVENT_VERSION
        || record.session != *session_id
        || record.sequence != expected_sequence
    {
        return Err(ResourceError::ReplayLogChanged);
    }
    Ok(record)
}

/// 验证 Snapshot 被日志前缀和自身状态摘要同时锚定。
fn snapshot_is_valid(
    session_id: &SessionId,
    snapshot: &SessionSnapshot,
    read: &ReadRecords,
    config: JournalConfig,
) -> bool {
    if snapshot.schema != SNAPSHOT_SCHEMA
        || snapshot.version != SNAPSHOT_VERSION
        || snapshot.session != *session_id
        || snapshot.state.session_id != *session_id
    {
        return false;
    }
    let Ok(sequence) = usize::try_from(snapshot.through_sequence) else {
        return false;
    };
    let expected_event_hash = sequence
        .checked_sub(1)
        .and_then(|index| read.event_hashes.get(index))
        .cloned();
    let expected_log_hash = sequence
        .checked_sub(1)
        .and_then(|index| read.prefix_hashes.get(index))
        .cloned();
    let anchors_match = sequence <= read.records.len()
        && snapshot.through_event_sha256 == expected_event_hash
        && snapshot.through_log_sha256 == expected_log_hash
        && snapshot.state.last_sequence == snapshot.through_sequence
        && state_hash(&snapshot.state).is_ok_and(|hash| hash == snapshot.state_sha256);
    if !anchors_match {
        return false;
    }
    let mut replayed = SessionState::empty(session_id.clone());
    for record in read.records.iter().take(sequence) {
        if reduce_record_from_valid_state(&mut replayed, record).is_err()
            || validate_state_collections(&replayed, config.max_state_collection_items).is_err()
        {
            return false;
        }
    }
    replayed == snapshot.state
}

/// 读取并反序列化 Snapshot。
fn read_snapshot(path: &Path, max_bytes: u64) -> Result<SessionSnapshot, ResourceError> {
    let bytes = match read_file_bounded(path, max_bytes) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { actual }) => {
            return Err(ResourceError::SnapshotTooLarge {
                actual,
                limit: max_bytes,
            });
        }
        Err(error) => return Err(ResourceError::io("read_snapshot", error)),
    };
    serde_json::from_slice(&bytes).map_err(|error| ResourceError::Json(error.to_string()))
}

/// Snapshot 与日志前缀之间的内容寻址锚点。
struct SnapshotAnchor {
    /// 最后一个完整事件正文的 SHA-256。
    event_sha256: Option<String>,
    /// 全部完整日志字节的 SHA-256。
    log_sha256: Option<String>,
}

/// 原子写入完整 Snapshot。
fn write_snapshot_file(
    path: &Path,
    state: &SessionState,
    anchor: SnapshotAnchor,
    sync: bool,
    max_bytes: u64,
) -> Result<(), ResourceError> {
    let state_sha256 = state_hash(state)?;
    let snapshot = SessionSnapshotRef {
        schema: SNAPSHOT_SCHEMA.to_owned(),
        version: SNAPSHOT_VERSION,
        session: state.session_id.clone(),
        through_sequence: state.last_sequence,
        through_event_sha256: anchor.event_sha256,
        through_log_sha256: anchor.log_sha256,
        state_sha256,
        state,
    };
    let mut bytes = match serialize_json_bounded(&snapshot, max_bytes, true)? {
        BoundedJson::Bytes(bytes) => bytes,
        BoundedJson::TooLarge { actual } => {
            return Err(ResourceError::SnapshotTooLarge {
                actual,
                limit: max_bytes,
            });
        }
    };
    bytes.push(b'\n');
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > max_bytes {
        return Err(ResourceError::SnapshotTooLarge {
            actual,
            limit: max_bytes,
        });
    }
    atomic_write(path, &bytes, sync)
}

/// 获取完整日志前缀及其最后一个事件的 Hash。
fn complete_log_anchor(
    log_path: &Path,
    max_log_bytes: u64,
) -> Result<SnapshotAnchor, ResourceError> {
    if !log_path.exists() {
        return Ok(SnapshotAnchor {
            event_sha256: None,
            log_sha256: None,
        });
    }
    let bytes = match read_file_bounded(log_path, max_log_bytes) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { actual }) => {
            return Err(ResourceError::JournalTooLarge {
                actual,
                limit: max_log_bytes,
            });
        }
        Err(error) => return Err(ResourceError::io("read_event_log", error)),
    };
    if bytes.is_empty() {
        return Ok(SnapshotAnchor {
            event_sha256: None,
            log_sha256: None,
        });
    }
    if !bytes.ends_with(b"\n") {
        return Err(ResourceError::CorruptReadOnly);
    }
    let event_sha256 = bytes[..bytes.len() - 1]
        .rsplit(|byte| *byte == b'\n')
        .next()
        .filter(|line| !line.is_empty())
        .map(sha256_hex);
    Ok(SnapshotAnchor {
        event_sha256,
        log_sha256: Some(sha256_hex(&bytes)),
    })
}

/// 写入不会覆盖既有文件的截断尾部证据，并在支持的平台同步目录项。
fn write_truncated_tail_evidence(
    session_dir: &Path,
    bytes: &[u8],
    sync: bool,
) -> Result<PathBuf, ResourceError> {
    let time = unix_time_millis()?;
    for attempt in 0_u16..=u16::MAX {
        let path = session_dir.join(format!("events.truncated-tail-{time}-{attempt}.bin"));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ResourceError::io("create_truncated_tail_evidence", error)),
        };
        file.write_all(bytes)
            .map_err(|error| ResourceError::io("write_truncated_tail_evidence", error))?;
        apply_durability(
            &mut file,
            if sync {
                Durability::FlushAndSync
            } else {
                Durability::Flush
            },
        )?;
        sync_directory(session_dir, sync)?;
        return Ok(path);
    }
    Err(ResourceError::Io {
        operation: "allocate_truncated_tail_evidence",
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "截断尾部证据文件名空间已耗尽",
        ),
    })
}

/// 校验事件中出现的每个 Artifact 引用均有实际实体支撑。
fn validate_event_artifacts(
    session_id: &SessionId,
    event: &SessionEvent,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    validate_atomic_batch_shape(event).map_err(|error| ResourceError::Reduction(error.message))?;
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                validate_non_batch_event_artifacts(session_id, event, validator)?;
            }
        }
        event => validate_non_batch_event_artifacts(session_id, event, validator)?,
    }
    Ok(())
}

/// 校验一个已经确认不是 AtomicBatch 的事件内全部 Artifact 引用。
fn validate_non_batch_event_artifacts(
    session_id: &SessionId,
    event: &SessionEvent,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    match event {
        SessionEvent::AtomicBatch { .. } => {
            return Err(ResourceError::Reduction("原子批次禁止嵌套".to_owned()));
        }
        SessionEvent::MessageAdded { message } => {
            for part in &message.content {
                validate_message_part_artifacts(part, session_id, validator)?;
            }
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            for message in &segment.messages {
                for part in &message.content {
                    validate_message_part_artifacts(part, session_id, validator)?;
                }
            }
        }
        SessionEvent::ToolFileChangePrepared { change, .. } => {
            if let Some(snapshot) = &change.before {
                validate_file_snapshot(session_id, snapshot, validator)?;
            }
            validate_file_snapshot(session_id, &change.after, validator)?;
        }
        SessionEvent::ToolFileChangeApplied { .. } => {}
        SessionEvent::DynamicInputReceiptCommitted { .. } => {}
        SessionEvent::ToolCompleted { outcome, .. } => {
            for part in &outcome.result.content {
                validate_tool_result_part_artifacts(part, session_id, validator)?;
            }
        }
        SessionEvent::ToolSideEffectUnknown { result, .. } => {
            for part in &result.content {
                validate_tool_result_part_artifacts(part, session_id, validator)?;
            }
        }
        SessionEvent::TerminalStarted { terminal } => {
            for artifact in &terminal.output_artifacts {
                validate_artifact_use(session_id, artifact, validator)?;
            }
        }
        SessionEvent::TerminalOutputRecorded { artifact, .. } => {
            validate_artifact_use(session_id, artifact, validator)?;
        }
        SessionEvent::PlanChanged { plan } => {
            if let Some(artifact) = &plan.plan_artifact {
                validate_artifact_use(session_id, artifact, validator)?;
            }
        }
        SessionEvent::MailboxMessageQueued { message } => {
            if let Some(artifact) = &message.artifact {
                validate_artifact_use(session_id, artifact, validator)?;
            }
        }
        SessionEvent::OnErrorHookQueued { .. }
        | SessionEvent::OnErrorHookReceiptCommitted { .. } => {}
        SessionEvent::SessionCreated { .. }
        | SessionEvent::SessionRenamed { .. }
        | SessionEvent::SessionStatusChanged { .. }
        | SessionEvent::TurnStarted { .. }
        | SessionEvent::TurnCompleted { .. }
        | SessionEvent::TurnStopped { .. }
        | SessionEvent::ModelRoundCompleted { .. }
        | SessionEvent::CompactionApplied { .. }
        | SessionEvent::ToolRequested { .. }
        | SessionEvent::ToolExecutionStarted { .. }
        | SessionEvent::TerminalExited { .. }
        | SessionEvent::TodoReplaced { .. }
        | SessionEvent::ProviderSnapshotUpdated { .. }
        | SessionEvent::TitleGenerated { .. }
        | SessionEvent::SubAgentSpawned { .. }
        | SessionEvent::SubAgentStatusChanged { .. }
        | SessionEvent::MailboxMessageDelivered { .. }
        | SessionEvent::WorktreeAssigned { .. }
        | SessionEvent::WorktreeReleased { .. }
        | SessionEvent::SessionClosed {} => {}
    }
    Ok(())
}

/// 递归核验消息、图片和工具结果中的全部 Artifact 引用。
fn validate_message_part_artifacts(
    part: &crate::MessagePart,
    session_id: &SessionId,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    match part {
        crate::MessagePart::Image {
            source: crate::MessageImageSource::Artifact { artifact },
        } => validate_artifact_materialization(
            session_id,
            artifact,
            crate::ArtifactMaterialization::Image,
            validator,
        ),
        crate::MessagePart::Artifact {
            artifact,
            materialization,
        } => validate_artifact_materialization(session_id, artifact, *materialization, validator),
        crate::MessagePart::ToolResult { content, .. } => {
            for item in content {
                validate_tool_result_part_artifacts(item, session_id, validator)?;
            }
            Ok(())
        }
        crate::MessagePart::Text { .. }
        | crate::MessagePart::Reasoning { .. }
        | crate::MessagePart::ToolCall { .. }
        | crate::MessagePart::Image {
            source: crate::MessageImageSource::Url { .. },
        } => Ok(()),
    }
}

/// 核验单个工具结果内容块中的 Artifact 引用。
fn validate_tool_result_part_artifacts(
    part: &crate::ToolResultPart,
    session_id: &SessionId,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    match part {
        crate::ToolResultPart::Image {
            source: crate::MessageImageSource::Artifact { artifact },
        } => validate_artifact_materialization(
            session_id,
            artifact,
            crate::ArtifactMaterialization::Image,
            validator,
        ),
        crate::ToolResultPart::Artifact {
            artifact,
            materialization,
        } => validate_artifact_materialization(session_id, artifact, *materialization, validator),
        crate::ToolResultPart::Text { .. }
        | crate::ToolResultPart::Image {
            source: crate::MessageImageSource::Url { .. },
        } => Ok(()),
    }
}

/// 要求当前 Journal 配置实体校验器并核验一个普通 Artifact 引用。
fn validate_artifact_use(
    session_id: &SessionId,
    artifact: &ArtifactUse,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    validator
        .ok_or(ResourceError::ArtifactValidatorRequired)?
        .validate(session_id, artifact)
}

/// 要求当前 Journal 配置实体校验器并核验明确的 Artifact 物化方式。
fn validate_artifact_materialization(
    session_id: &SessionId,
    artifact: &ArtifactUse,
    materialization: crate::ArtifactMaterialization,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    validator
        .ok_or(ResourceError::ArtifactValidatorRequired)?
        .validate_materialization(session_id, artifact, materialization)
}

/// 要求当前 Journal 配置实体校验器并核验文件快照的全部 Artifact 块。
fn validate_file_snapshot(
    session_id: &SessionId,
    snapshot: &crate::FileSnapshot,
    validator: Option<&dyn ArtifactValidator>,
) -> Result<(), ResourceError> {
    validator
        .ok_or(ResourceError::ArtifactValidatorRequired)?
        .validate_file_snapshot(session_id, snapshot)
}

/// 计算确定性序列化 SessionState 的 SHA-256。
fn state_hash(state: &SessionState) -> Result<String, ResourceError> {
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, state)
        .map_err(|error| ResourceError::Json(error.to_string()))?;
    Ok(digest_hex(writer.0.finalize()))
}

/// 直接把序列化字节送入 SHA-256，避免为状态 Hash 分配完整 JSON。
struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    /// 把当前字节块加入摘要。
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    /// 摘要 Writer 没有额外缓冲区。
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 返回日志不存在或当前 metadata 的稳定变化戳。
fn log_stamp(path: &Path) -> Result<LogStamp, ResourceError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(LogStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LogStamp {
            len: 0,
            modified: None,
        }),
        Err(error) => Err(ResourceError::io("stat_event_log", error)),
    }
}

/// 拒绝反序列化放大后超过配置的主要 Session 状态集合。
fn validate_state_collections(state: &SessionState, limit: usize) -> Result<(), ResourceError> {
    let transcript_messages = state.raw_transcript_messages();
    let raw_message_count = state
        .transcript
        .iter()
        .try_fold(0_usize, |total, record| {
            let count = match record {
                crate::TranscriptRecord::MessageAdded(_) => 1,
                crate::TranscriptRecord::SegmentCommitted(segment) => segment.messages.len(),
                crate::TranscriptRecord::CompactionApplied(_) => 0,
            };
            total.checked_add(count)
        })
        .unwrap_or(usize::MAX);
    let transcript_segment_count = state.transcript_segments().count();
    let compaction_count = state.applied_compactions().count();
    let file_change_count = state
        .tools
        .values()
        .filter(|tool| tool.file_change.is_some())
        .count();
    let file_snapshot_chunk_count = state
        .tools
        .values()
        .filter_map(|tool| tool.file_change.as_ref())
        .try_fold(0_usize, |total, change| {
            let before = change
                .before
                .as_ref()
                .map_or(0, |snapshot| snapshot.chunks.len());
            total
                .checked_add(before)
                .and_then(|total| total.checked_add(change.after.chunks.len()))
        })
        .unwrap_or(usize::MAX);
    let collections = [
        ("turns", state.turns.len()),
        ("transcript", state.transcript.len()),
        ("messages", raw_message_count),
        ("transcript_segments", transcript_segment_count),
        ("model_rounds", state.model_rounds.len()),
        ("tools", state.tools.len()),
        ("file_changes", file_change_count),
        ("terminals", state.terminals.len()),
        ("compactions", compaction_count),
        ("todos", state.todos.items.len()),
        ("sub_agents", state.sub_agents.len()),
        ("mailbox", state.mailbox.len()),
        ("worktrees", state.worktrees.len()),
        ("generated_titles", state.generated_titles.len()),
        ("dynamic_input_receipts", state.dynamic_input_receipts.len()),
        ("on_error_hook_outbox", state.on_error_hook_outbox.len()),
    ];
    for (collection, actual) in collections {
        if actual > limit {
            return Err(ResourceError::StateCollectionLimit {
                collection,
                actual,
                limit,
            });
        }
    }
    for (collection, actual) in [
        (
            "message_parts",
            transcript_messages
                .iter()
                .try_fold(0_usize, |total, message| {
                    total.checked_add(message.content.len())
                })
                .unwrap_or(usize::MAX),
        ),
        (
            "message_tool_result_content",
            transcript_messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|part| match part {
                    crate::MessagePart::ToolResult { content, .. } => Some(content.len()),
                    crate::MessagePart::Text { .. }
                    | crate::MessagePart::Reasoning { .. }
                    | crate::MessagePart::Image { .. }
                    | crate::MessagePart::ToolCall { .. }
                    | crate::MessagePart::Artifact { .. } => None,
                })
                .try_fold(0_usize, |total, count| total.checked_add(count))
                .unwrap_or(usize::MAX),
        ),
        (
            "tool_outcome_result_content",
            state
                .tools
                .values()
                .filter_map(|tool| {
                    tool.outcome
                        .as_ref()
                        .map(|outcome| outcome.result.content.len())
                })
                .try_fold(0_usize, |total, count| total.checked_add(count))
                .unwrap_or(usize::MAX),
        ),
        (
            "terminal_output_artifacts",
            state
                .terminals
                .values()
                .try_fold(0_usize, |total, terminal| {
                    total.checked_add(terminal.output_artifacts.len())
                })
                .unwrap_or(usize::MAX),
        ),
        ("file_snapshot_chunks", file_snapshot_chunk_count),
        (
            "json_collection_items",
            transcript_messages
                .iter()
                .flat_map(|message| message.content.iter())
                .map(message_part_json_collection_items)
                .chain(
                    state
                        .tools
                        .values()
                        .map(|tool| json_collection_items(&tool.request.arguments)),
                )
                .try_fold(0_usize, |total, count| total.checked_add(count))
                .unwrap_or(usize::MAX),
        ),
    ] {
        if actual > limit {
            return Err(ResourceError::StateCollectionLimit {
                collection,
                actual,
                limit,
            });
        }
    }
    Ok(())
}

/// 统计一个消息块内部所有 JSON Array 元素和 Object 成员，并递归覆盖嵌套值。
fn message_part_json_collection_items(part: &crate::MessagePart) -> usize {
    match part {
        crate::MessagePart::Reasoning {
            continuation: Some(continuation),
            ..
        } => json_collection_items(&continuation.data),
        crate::MessagePart::ToolCall { arguments, .. } => json_collection_items(arguments),
        crate::MessagePart::Text { .. }
        | crate::MessagePart::Reasoning {
            continuation: None, ..
        }
        | crate::MessagePart::Image { .. }
        | crate::MessagePart::ToolResult { .. }
        | crate::MessagePart::Artifact { .. } => 0,
    }
}

/// 递归统计 JSON Array 元素和 Object 成员，任一加法溢出即按无限大处理。
fn json_collection_items(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(json_collection_items)
            .try_fold(items.len(), |total, nested| total.checked_add(nested))
            .unwrap_or(usize::MAX),
        serde_json::Value::Object(entries) => entries
            .values()
            .map(json_collection_items)
            .try_fold(entries.len(), |total, nested| total.checked_add(nested))
            .unwrap_or(usize::MAX),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

/// 测试专用追加故障点，每次只消费一个当前线程内故障。
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendFault {
    /// 首次创建日志后、尚未写入任何事件字节时模拟失败。
    ZeroWrite,
    /// 只写入一半 JSONL 后模拟短写。
    PartialWrite,
    /// 完整写入后模拟 flush 失败。
    Flush,
    /// flush 后模拟文件 fsync 失败。
    Sync,
    /// 仅让下一次显式 barrier 的文件 fsync 失败，不主动触发追加线程内刷盘。
    BarrierSync,
    /// 文件 fsync 后模拟父目录同步失败。
    DirectorySync,
    /// 全部持久化完成后模拟 metadata 读取失败。
    PostWriteMetadata,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// 当前测试线程下一次要触发的追加故障。
    static APPEND_FAULT: std::cell::RefCell<Option<AppendFault>> = const { std::cell::RefCell::new(None) };
}

/// 为当前测试线程设置一次性追加故障。
#[cfg(any(test, feature = "test-support"))]
fn set_append_fault(fault: AppendFault) {
    APPEND_FAULT.with(|current| {
        let previous = current.replace(Some(fault));
        assert!(previous.is_none(), "追加故障必须在设置下一个故障前被消费");
    });
}

/// 仅在当前故障点匹配时消费测试故障。
#[cfg(any(test, feature = "test-support"))]
fn take_append_fault(fault: AppendFault) -> bool {
    APPEND_FAULT.with(|current| {
        if *current.borrow() == Some(fault) {
            current.replace(None);
            true
        } else {
            false
        }
    })
}

/// 当前线程是否仍有一个待消费的追加/持久化故障。
fn append_fault_pending() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    {
        APPEND_FAULT.with(|current| {
            current
                .borrow()
                .is_some_and(|fault| fault != AppendFault::BarrierSync)
        })
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        false
    }
}

/// 构造不依赖平台的测试 IO 故障。
#[cfg(any(test, feature = "test-support"))]
fn injected_io_error(operation: &'static str) -> ResourceError {
    ResourceError::io(
        operation,
        std::io::Error::other("keencode-resources 测试注入故障"),
    )
}

/// 跨 crate 集成测试使用的一次性 Journal 追加故障注入入口。
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    /// 重新导出可注入的追加阶段故障类型。
    pub use super::AppendFault;

    /// 为当前测试线程设置下一次追加操作要触发的故障。
    pub fn set_append_fault(fault: AppendFault) {
        super::set_append_fault(fault);
    }

    /// 清除当前测试线程尚未消费的一次性追加故障，避免污染后续测试。
    pub fn clear_append_fault() {
        super::APPEND_FAULT.with(|current| {
            current.replace(None);
        });
    }

    /// 重置进程的 sync_data 计数器并返回重置前的值。
    pub fn take_sync_count() -> u64 {
        super::SYNC_COUNT.swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// 返回进程累计的 sync_data 次数，不重置。
    pub fn sync_count() -> u64 {
        super::SYNC_COUNT.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// 根据配置对已写事件执行 flush 与 sync。
fn apply_durability(file: &mut fs::File, durability: Durability) -> Result<(), ResourceError> {
    match durability {
        Durability::Buffered => Ok(()),
        Durability::Flush => {
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::Flush) {
                return Err(injected_io_error("flush_event_log"));
            }
            file.flush()
                .map_err(|error| ResourceError::io("flush_event_log", error))
        }
        Durability::FlushAndSync => {
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::Flush) {
                return Err(injected_io_error("flush_event_log"));
            }
            file.flush()
                .map_err(|error| ResourceError::io("flush_event_log", error))?;
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::Sync) {
                return Err(injected_io_error("sync_event_log"));
            }
            file.sync_data()
                .map_err(|error| ResourceError::io("sync_event_log", error))
        }
    }
}

/// 判断当前 sequence 是否达到自动 Snapshot 周期。
fn snapshot_due(policy: SnapshotPolicy, sequence: u64) -> bool {
    match policy {
        SnapshotPolicy::Disabled => false,
        SnapshotPolicy::Every { events } => sequence % events == 0,
    }
}

/// 返回当前 Unix Epoch 毫秒时间。
fn unix_time_millis() -> Result<u64, ResourceError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ResourceError::Json(format!("系统时间早于 Unix Epoch：{error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|_| ResourceError::Json("系统时间毫秒溢出".to_owned()))
}

/// 计算小写十六进制 SHA-256。
fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(Sha256::digest(bytes))
}

/// 把 SHA-256 原始摘要编码为小写十六进制。
fn digest_hex(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_idle_batch_job(journal: &SessionJournal) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (pending_records, worker_active) = {
                let inner = journal.inner.lock().expect("Journal 锁应可用");
                (inner.pending_records, inner.batch_worker_active)
            };
            if pending_records == 0 && !worker_active {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "批次任务未按时完成：pending={pending_records}, scheduled={worker_active}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_active_batch_scheduler() {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let actual = batch_scheduler()
                .active_worker_count
                .load(Ordering::Acquire);
            if actual == 1 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "进程级批次调度线程未按时启动：actual={actual}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_sticky_batch_failure(journal: &SessionJournal) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (pending_records, scheduled, failed) = {
                let inner = journal.inner.lock().expect("Journal 锁应可用");
                (
                    inner.pending_records,
                    inner.batch_worker_active,
                    inner.batch_flush_failure.is_some(),
                )
            };
            if pending_records > 0 && !scheduled && failed {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "后台刷盘失败未按时形成 sticky 状态：pending={pending_records}, scheduled={scheduled}, failed={failed}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn metadata_index_is_rebuilt_and_does_not_wait_for_append_lock() {
        let root = tempfile::tempdir().unwrap();
        let id = SessionId::new("metadata-test").unwrap();
        let config = JournalConfig::default();
        let SessionOpen::Ready(journal) =
            SessionJournal::open(root.path(), id.clone(), config).unwrap()
        else {
            panic!("healthy journal")
        };
        journal
            .append_idempotent(
                SessionEventId::new("create").unwrap(),
                0,
                SessionEvent::SessionCreated {
                    title: "indexed".into(),
                    project_root: "/workspace".into(),
                },
            )
            .unwrap();
        let directory = root.path().join("metadata-test");
        // 命中索引时不进入正文恢复所需的跨进程锁。
        let guard = exclusive_lock(&directory.join("append.lock")).unwrap();
        assert_eq!(
            SessionJournal::read_metadata(root.path(), id.clone(), config)
                .unwrap()
                .unwrap()
                .title,
            "indexed"
        );
        drop(guard);
        for damaged in [false, true] {
            if damaged {
                fs::write(directory.join("metadata.json"), b"broken").unwrap();
            } else {
                fs::remove_file(directory.join("metadata.json")).unwrap();
            }
            assert_eq!(
                SessionJournal::read_metadata(root.path(), id.clone(), config)
                    .unwrap()
                    .unwrap()
                    .last_sequence,
                1
            );
        }
        // 日志变化后不能继续返回旧索引中的健康状态。
        OpenOptions::new()
            .append(true)
            .open(directory.join("events.jsonl"))
            .unwrap()
            .write_all(b"broken\n")
            .unwrap();
        assert!(
            SessionJournal::read_metadata(root.path(), id, config)
                .unwrap()
                .unwrap()
                .corrupt
        );
    }

    /// read_state 投影必须与 state() 克隆结果一致，且能看到追加后的最新状态。
    #[test]
    fn read_state投影与完整克隆一致() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("read-state").expect("Session ID 应有效");
        let config = JournalConfig::default();
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        let event_id = SessionEventId::new("event-create").expect("事件 ID 应有效");
        let event = SessionEvent::SessionCreated {
            title: "投影测试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        };
        journal
            .append_idempotent(event_id, 0, event)
            .expect("事件应可追加");

        let projected_title = journal
            .read_state(|state| state.title.clone())
            .expect("只读投影应成功");
        assert_eq!(projected_title, "投影测试");
        assert_eq!(
            journal.state().expect("完整快照应成功").title,
            projected_title
        );
        let last_sequence = journal
            .read_state(|state| state.last_sequence)
            .expect("只读投影应成功");
        assert_eq!(last_sequence, 1);
    }

    /// 验证可见事件在补齐持久化失败时保持不确定，成功确认后才报告已提交。
    ///
    /// 批量语义下满足"返回即落盘"的仍是显式 `flush`：`Flush`/`Sync`/
    /// `DirectorySync`/`PostWriteMetadata` 四类故障注入在批量路径的满批自动
    /// 刷盘与显式刷盘都会被消费——前者经 `append` 转为 `Indeterminate`，
    /// 后者（幂等重试的 `AlreadyCommitted` 确认）透传错误。`PartialWrite`
    /// 仍走写行失败路径（回滚截断尾后重建）。
    #[test]
    fn append故障重试保持单一记录() {
        for (index, fault) in [
            AppendFault::PartialWrite,
            AppendFault::Flush,
            AppendFault::Sync,
            AppendFault::DirectorySync,
            AppendFault::PostWriteMetadata,
        ]
        .into_iter()
        .enumerate()
        {
            let root = tempfile::tempdir().expect("临时目录应创建");
            let session_id = SessionId::new(format!("fault-{index}")).expect("Session ID 应有效");
            let config = JournalConfig {
                durability: Durability::FlushAndSync,
                snapshot_policy: SnapshotPolicy::Disabled,
                ..JournalConfig::default()
            };
            let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
                .expect("Session 应打开")
            {
                SessionOpen::Ready(journal) => journal,
                SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
            };
            let event_id = SessionEventId::new("event-create").expect("事件 ID 应有效");
            let event = SessionEvent::SessionCreated {
                title: "故障注入".to_owned(),
                project_root: "D:/workspace".to_owned(),
            };
            set_append_fault(fault);
            let first = journal
                .append_idempotent(event_id.clone(), 0, event.clone())
                .expect("故障应返回结构化结果");
            // 测试故障会让本次 append 在调用线程内立即进入刷盘路径；若
            // 后续实现选择保留窗口，显式 flush 分支仍验证相同 barrier。
            let first = if matches!(first, IdempotentAppendOutcome::Appended(_)) {
                let flush_error = journal
                    .flush()
                    .expect_err("注入的持久化故障应在显式刷盘时暴露");
                let retry = journal
                    .append_idempotent(event_id.clone(), 0, event.clone())
                    .expect("刷盘失败后重试应返回结构化结果");
                assert!(
                    matches!(retry, IdempotentAppendOutcome::AlreadyCommitted { .. }),
                    "刷盘失败 {flush_error:?} 后重试应已确认落盘"
                );
                retry
            } else {
                first
            };
            assert!(matches!(
                first,
                IdempotentAppendOutcome::AlreadyCommitted { .. }
                    | IdempotentAppendOutcome::Indeterminate { .. }
            ));

            drop(journal);
            let journal = match SessionJournal::open(root.path(), session_id, config)
                .expect("故障后的 Session 应可重开")
            {
                SessionOpen::Ready(journal) => journal,
                SessionOpen::Corrupt(_) => panic!("已对账故障不应损坏 Session"),
            };

            if fault != AppendFault::PartialWrite {
                let retry_fault = match fault {
                    AppendFault::Flush => AppendFault::Flush,
                    AppendFault::Sync | AppendFault::PostWriteMetadata => AppendFault::Sync,
                    AppendFault::BarrierSync => AppendFault::BarrierSync,
                    // 冷打开已经同步既有目录项；幂等 barrier 仍必须补齐文件 sync。
                    AppendFault::DirectorySync => AppendFault::Sync,
                    AppendFault::ZeroWrite | AppendFault::PartialWrite => {
                        unreachable!("零字节与截断写入已经单独排除")
                    }
                };
                set_append_fault(retry_fault);
                let pending = journal
                    .append_idempotent(event_id.clone(), 0, event.clone())
                    .expect("补持久化失败应返回结构化结果");
                assert!(matches!(
                    pending,
                    IdempotentAppendOutcome::Indeterminate { .. }
                ));
                assert_eq!(
                    fs::read_to_string(journal.log_path())
                        .expect("日志应读取")
                        .lines()
                        .count(),
                    1
                );
            }

            let retry = journal
                .append_idempotent(event_id, 0, event)
                .expect("相同事件应可安全重试");
            if fault == AppendFault::PartialWrite {
                assert!(matches!(retry, IdempotentAppendOutcome::Appended(_)));
            } else {
                assert!(matches!(
                    retry,
                    IdempotentAppendOutcome::AlreadyCommitted { .. }
                ));
            }
            assert_eq!(journal.state().expect("状态应读取").last_sequence, 1);
            assert_eq!(
                fs::read_to_string(journal.log_path())
                    .expect("日志应读取")
                    .lines()
                    .count(),
                1
            );
        }
    }

    /// sync 失败不能清除常量大小 pending 状态；下一次 barrier 可安全补齐。
    #[test]
    fn sync故障保留批次并允许barrier重试() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-sync-retry").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        set_append_fault(AppendFault::Sync);
        assert!(matches!(
            journal
                .append_idempotent(
                    SessionEventId::new("event-create").expect("事件 ID 应有效"),
                    0,
                    SessionEvent::SessionCreated {
                        title: "sync retry".to_owned(),
                        project_root: "D:/workspace".to_owned(),
                    },
                )
                .expect("故障应转为结构化结果"),
            IdempotentAppendOutcome::Indeterminate { .. }
        ));
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);
        journal.flush().expect("第二次 barrier 应补齐持久化");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        drop(journal);
        let reopened = match SessionJournal::open(root.path(), session_id, config)
            .expect("重试后 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("补齐后的事件不应损坏"),
        };
        assert_eq!(reopened.state().expect("状态应恢复").last_sequence, 1);
    }

    /// 验证零字节写入故障留下的空日志跨重启后仍必须补齐首次目录同步。
    #[test]
    fn 零字节首次写入跨重启仍补齐目录持久化() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("zero-write-retry").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let event_id = SessionEventId::new("event-create").expect("事件 ID 应有效");
        let event = SessionEvent::SessionCreated {
            title: "零字节重试".to_owned(),
            project_root: "D:/workspace".to_owned(),
        };

        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        set_append_fault(AppendFault::ZeroWrite);
        assert!(matches!(
            journal
                .append_idempotent(event_id.clone(), 0, event.clone())
                .expect("零字节故障应返回结构化结果"),
            IdempotentAppendOutcome::Indeterminate { .. }
        ));
        assert_eq!(
            fs::metadata(journal.log_path())
                .expect("空日志应存在")
                .len(),
            0
        );
        drop(journal);

        set_append_fault(AppendFault::DirectorySync);
        assert!(matches!(
            SessionJournal::open(root.path(), session_id.clone(), config),
            Err(ResourceError::Io {
                operation: "sync_event_log_directory_on_open",
                ..
            })
        ));
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("目录同步成功后空日志 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("零字节故障不应损坏 Session"),
        };
        let created = journal
            .append_idempotent(event_id.clone(), 0, event.clone())
            .expect("目录项已确认后首次事件应追加");
        assert!(matches!(created, IdempotentAppendOutcome::Appended(_)));
        journal.flush().expect("事件文件内容应完成最终同步");
        assert_eq!(
            fs::read_to_string(journal.log_path())
                .expect("事件日志应读取")
                .lines()
                .count(),
            1
        );
        drop(journal);

        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("目录同步故障后的 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("完整事件不应损坏 Session"),
        };
        set_append_fault(AppendFault::Sync);
        assert!(matches!(
            journal
                .append_idempotent(event_id.clone(), 0, event.clone())
                .expect("幂等文件同步故障应返回结构化结果"),
            IdempotentAppendOutcome::Indeterminate { .. }
        ));
        assert!(matches!(
            journal
                .append_idempotent(event_id, 0, event)
                .expect("最终补同步应成功"),
            IdempotentAppendOutcome::AlreadyCommitted { .. }
        ));
        assert_eq!(journal.state().expect("最终状态应读取").last_sequence, 1);
        assert_eq!(
            fs::read_to_string(journal.log_path())
                .expect("最终日志应读取")
                .lines()
                .count(),
            1
        );
    }

    /// 验证非首个游标只执行一次文件定位，不重新扫描游标之前的物理记录。
    #[test]
    fn replay_page_uses_record_end_offset_index() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("replay-seek-index").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };

        let append = |journal: &SessionJournal, id: &str, event: SessionEvent| {
            let expected_sequence = journal.state().expect("状态应读取").last_sequence;
            assert!(matches!(
                journal
                    .append_idempotent(
                        SessionEventId::new(id).expect("事件 ID 应有效"),
                        expected_sequence,
                        event,
                    )
                    .expect("事件应追加"),
                IdempotentAppendOutcome::Appended(_)
            ));
        };
        append(
            &journal,
            "event-create",
            SessionEvent::SessionCreated {
                title: "重放定位测试".to_owned(),
                project_root: "D:/workspace".to_owned(),
            },
        );
        for sequence in 1..=128 {
            append(
                &journal,
                &format!("event-rename-{sequence}"),
                SessionEvent::SessionRenamed {
                    title: format!("标题-{sequence}"),
                },
            );
        }

        let before = REPLAY_SEEK_COUNT.with(Cell::get);
        let page = journal
            .read_page(Some(120), 1)
            .expect("索引定位后的页面应读取");
        let after = REPLAY_SEEK_COUNT.with(Cell::get);
        assert_eq!(after - before, 1);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].sequence, 121);
        append(
            &journal,
            "event-root-start",
            SessionEvent::TurnStarted {
                turn_id: crate::TurnId::new("root-turn").unwrap(),
                source_agent_id: crate::AgentId::new("root").unwrap(),
                root_turn_id: crate::TurnId::new("root-turn").unwrap(),
                parent_turn_id: None,
                prompt_summary: "question".to_owned(),
            },
        );
        assert_eq!(journal.history_index().unwrap().root_starts, [130]);
        drop(journal);
        let reopened = match SessionJournal::open(
            root.path(),
            SessionId::new("replay-seek-index").unwrap(),
            config,
        )
        .unwrap()
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("重开不应损坏"),
        };
        assert_eq!(reopened.history_index().unwrap().root_starts, [130]);
    }

    /// 第 64 条待刷记录必须在 append 返回前完成一次 sync。
    #[test]
    fn 第64条记录触发满批刷盘() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-full").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "满批".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        let baseline = journal.sync_count_for_tests();
        // 只隔离 64 条阈值；真实 100ms 由独立空闲超时测试覆盖。
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now() + Duration::from_secs(60));

        for index in 1..=JOURNAL_BATCH_MAX_RECORDS {
            journal
                .append_idempotent(
                    SessionEventId::new(format!("event-rename-{index}")).expect("事件 ID 应有效"),
                    index as u64,
                    SessionEvent::SessionRenamed {
                        title: format!("标题-{index}"),
                    },
                )
                .expect("重命名事件应追加");
        }
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        assert_eq!(journal.sync_count_for_tests(), baseline + 1);
    }

    /// 第 64 条与 100ms 同时到达时只能按锁内先后形成一批或两批，不能丢记录。
    #[test]
    fn 满批与超时竞速保持完整sequence() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-deadline-race").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = Arc::new(
            match SessionJournal::open(root.path(), session_id, config).expect("Session 应打开")
            {
                SessionOpen::Ready(journal) => journal,
                SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
            },
        );
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "阈值竞速".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        let baseline = journal.sync_count_for_tests();
        // 先隔离计数阈值，构造 63 条后再把 deadline 推到当前时刻。
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now() + Duration::from_secs(60));
        for index in 1..JOURNAL_BATCH_MAX_RECORDS {
            journal
                .append_idempotent(
                    SessionEventId::new(format!("race-event-{index}")).expect("事件 ID 应有效"),
                    index as u64,
                    SessionEvent::SessionRenamed {
                        title: format!("竞速-{index}"),
                    },
                )
                .expect("竞速前缀应追加");
        }
        {
            let mut inner = journal.inner.lock().expect("Journal 锁应可用");
            inner.pending_since = Some(Instant::now() - JOURNAL_BATCH_MAX_DELAY);
        }
        batch_scheduler().notify();
        let appending = Arc::clone(&journal);
        std::thread::spawn(move || {
            appending
                .append_idempotent(
                    SessionEventId::new("race-event-64").expect("事件 ID 应有效"),
                    JOURNAL_BATCH_MAX_RECORDS as u64,
                    SessionEvent::SessionRenamed {
                        title: "竞速-64".to_owned(),
                    },
                )
                .expect("第 64 条应追加")
        })
        .join()
        .expect("追加线程应完成");
        std::thread::sleep(JOURNAL_BATCH_MAX_DELAY + Duration::from_millis(80));
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        assert_eq!(
            journal.state().expect("状态应读取").last_sequence,
            1 + JOURNAL_BATCH_MAX_RECORDS as u64
        );
        let syncs = journal.sync_count_for_tests().saturating_sub(baseline);
        assert!(
            (1..=2).contains(&syncs),
            "锁内先后只允许一次满批 sync，或超时后新开一批共两次，实际 {syncs}"
        );
    }

    /// 批量语义：连续 640 条正式物理记录的 sync 次数远小于逐条语义。
    #[test]
    fn 批量攒批合并fsync次数远小于逐条() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-coalesce").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "批量合并".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        let baseline = journal.sync_count_for_tests();
        // 640 条放大正式写入路径；每轮推迟超时，只隔离验证 64 条满批
        // 阈值。真实 100ms 路径由独立超时与竞速测试覆盖。
        const BATCH_ROUNDS: u64 = 10;
        for round in 0..BATCH_ROUNDS {
            journal
                .defer_batch_timeout_for_tests()
                .expect("测试 deadline 应推迟");
            for index in 1..=JOURNAL_BATCH_MAX_RECORDS as u64 {
                let sequence = 1 + round * JOURNAL_BATCH_MAX_RECORDS as u64 + index - 1;
                journal
                    .append_idempotent(
                        SessionEventId::new(format!("event-rename-{round}-{index}"))
                            .expect("事件 ID 应有效"),
                        sequence,
                        SessionEvent::SessionRenamed {
                            title: format!("标题-{round}-{index}"),
                        },
                    )
                    .expect("重命名事件应追加");
            }
        }
        let syncs = journal.sync_count_for_tests().saturating_sub(baseline);
        let appended = BATCH_ROUNDS * JOURNAL_BATCH_MAX_RECORDS as u64;
        assert_eq!(syncs, BATCH_ROUNDS, "每 64 条应合并为一次 sync");
        journal.flush().expect("显式刷盘应成功");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        assert_eq!(
            journal.state().expect("状态应读取").last_sequence,
            1 + appended
        );
    }

    /// 批量语义：最后一次 append 后即使没有任何读写，也会在 100ms 到期刷盘。
    #[test]
    fn 批量超时刷盘无需攒满() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-timeout").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "超时刷盘".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("首条应先落盘");
        let baseline = journal.sync_count_for_tests();
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now() + Duration::from_secs(60));
        journal
            .append_idempotent(
                SessionEventId::new("event-rename-1").expect("事件 ID 应有效"),
                1,
                SessionEvent::SessionRenamed {
                    title: "待刷".to_owned(),
                },
            )
            .expect("待刷事件应追加");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now());
        batch_scheduler().notify();
        // 期间不调用 append/read/flush；唯一唤醒来源必须是 Condvar 超时。
        wait_for_idle_batch_job(&journal);
        assert_eq!(journal.sync_count_for_tests(), baseline + 1);
    }

    /// FlushAndSync 冷打开必须先同步既有日志，不能把仅可重放的页缓存内容标成 durable。
    #[test]
    fn 冷打开先同步既有日志再建立durable水位() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("cold-open-sync").expect("Session ID 应有效");
        let buffered = JournalConfig {
            durability: Durability::Buffered,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), buffered)
            .expect("Buffered Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "冷打开同步".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("Buffered 事件应追加");
        drop(journal);

        let durable = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        set_append_fault(AppendFault::Sync);
        let failed = SessionJournal::open(root.path(), session_id.clone(), durable);
        assert!(matches!(
            failed,
            Err(ResourceError::Io {
                operation: "sync_event_log_on_open",
                ..
            })
        ));

        let reopened = match SessionJournal::open(root.path(), session_id, durable)
            .expect("清除同步故障后应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("完整既有日志不应损坏"),
        };
        let inner = reopened.inner.lock().expect("Journal 锁应可用");
        assert_eq!(inner.synced_through_sequence, 1);
        assert_eq!(inner.pending_records, 0);
        assert!(!inner.directory_sync_required);
    }

    /// 只读 refresh 发现外部完整新增后也必须自动登记 100ms flush，不能依赖后续 append。
    #[test]
    fn reload外部新增会自动arm超时刷盘() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("reload-arms-flush").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let writer = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("写实例应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        writer
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "外部 reload".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        writer.flush().expect("创建事件应先落盘");
        let observer = match SessionJournal::open(root.path(), session_id, config)
            .expect("观察实例应冷打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("健康 Session 不应损坏"),
        };
        let baseline = observer.sync_count_for_tests();

        writer
            .defer_batch_timeout_for_tests()
            .expect("写实例 deadline 应推迟");
        writer
            .append_idempotent(
                SessionEventId::new("event-external").expect("事件 ID 应有效"),
                1,
                SessionEvent::SessionRenamed {
                    title: "外部新增".to_owned(),
                },
            )
            .expect("外部事件应追加");
        assert_eq!(
            observer.state().expect("观察实例应 reload").last_sequence,
            2
        );
        {
            let inner = observer.inner.lock().expect("Journal 锁应可用");
            assert_eq!(inner.pending_records, 1);
            assert!(inner.batch_worker_active);
        }

        wait_for_idle_batch_job(&observer);
        assert_eq!(observer.sync_count_for_tests(), baseline + 1);
        assert_eq!(observer.pending_flush_records().expect("窗口应读取"), 0);
    }

    /// 一个 Journal 的任务完成后可重新登记；相邻批次复用同一进程级调度器。
    #[test]
    fn 超时刷盘后journal任务可重新登记() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-worker-rearm").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id, config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "worker rearm".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        wait_for_idle_batch_job(&journal);
        let mut expected_sequence = 1_u64;

        for cycle in 1_u64..=2 {
            // 先固定未来 deadline，确保测试能观察到实际存活的 worker，
            // 再把 deadline 推到当前时刻触发它自己的超时刷盘路径。
            journal
                .inner
                .lock()
                .expect("Journal 锁应可用")
                .pending_since = Some(Instant::now() + Duration::from_secs(60));
            journal
                .append_idempotent(
                    SessionEventId::new(format!("event-rearm-{cycle}-first"))
                        .expect("事件 ID 应有效"),
                    expected_sequence,
                    SessionEvent::SessionRenamed {
                        title: format!("rearm-{cycle}-first"),
                    },
                )
                .expect("重新 arm 后事件应追加");
            expected_sequence += 1;
            wait_for_active_batch_scheduler();
            assert!(
                journal
                    .inner
                    .lock()
                    .expect("Journal 锁应可用")
                    .batch_worker_active
            );
            journal
                .append_idempotent(
                    SessionEventId::new(format!("event-rearm-{cycle}-second"))
                        .expect("事件 ID 应有效"),
                    expected_sequence,
                    SessionEvent::SessionRenamed {
                        title: format!("rearm-{cycle}-second"),
                    },
                )
                .expect("同一批次后续事件应追加");
            expected_sequence += 1;
            assert_eq!(journal.active_batch_scheduler_workers_for_tests(), 1);

            journal
                .inner
                .lock()
                .expect("Journal 锁应可用")
                .pending_since = Some(Instant::now() - JOURNAL_BATCH_MAX_DELAY);
            batch_scheduler().notify();
            wait_for_idle_batch_job(&journal);
        }

        assert_eq!(
            journal.state().expect("最终状态应读取").last_sequence,
            expected_sequence
        );
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
    }

    /// 256 个 Journal 同时到期时必须排队并发 fsync，慢盘不能形成全局串行队头阻塞，
    /// 也不能突破进程级 worker 上限。连续两轮覆盖排队完成、重新 arm 与 deadline 通知竞速。
    #[test]
    fn 大量journal到期fsync并发且重复竞速不丢任务() {
        const JOURNAL_COUNT: usize = 256;
        const ROUNDS: u64 = 2;
        const INJECTED_SYNC_DELAY: Duration = Duration::from_millis(50);

        let root = tempfile::tempdir().expect("临时目录应创建");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let mut journals = Vec::with_capacity(JOURNAL_COUNT);
        for index in 0..JOURNAL_COUNT {
            let session_id =
                SessionId::new(format!("idle-worker-{index}")).expect("Session ID 应有效");
            let journal = match SessionJournal::open(root.path(), session_id, config)
                .expect("Session 应打开")
            {
                SessionOpen::Ready(journal) => journal,
                SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
            };
            journal
                .batch_target
                .background_sync_delay_ms
                .store(INJECTED_SYNC_DELAY.as_millis() as u64, Ordering::Release);
            journals.push(journal);
        }
        // 让一个排队任务先失败一次，确认 worker 槽位释放后仍会按原有退避路径重试。
        journals[0]
            .batch_target
            .background_failures_remaining
            .store(1, Ordering::Release);

        for round in 0..ROUNDS {
            for (index, journal) in journals.iter().enumerate() {
                journal
                    .defer_batch_timeout_for_tests()
                    .expect("测试 deadline 应推迟");
                let event = if round == 0 {
                    SessionEvent::SessionCreated {
                        title: format!("并发 Journal {index}"),
                        project_root: "D:/workspace".to_owned(),
                    }
                } else {
                    SessionEvent::SessionRenamed {
                        title: format!("并发 Journal {index} round {round}"),
                    }
                };
                journal
                    .append_idempotent(
                        SessionEventId::new(format!("event-{index}-{round}"))
                            .expect("事件 ID 应有效"),
                        round,
                        event,
                    )
                    .expect("并发批次事件应追加");
            }

            wait_for_active_batch_scheduler();
            for journal in &journals {
                journal
                    .inner
                    .lock()
                    .expect("Journal 锁应可用")
                    .pending_since = Some(Instant::now() - JOURNAL_BATCH_MAX_DELAY);
            }
            let started = Instant::now();
            batch_scheduler().notify();
            let attempts_deadline = started + Duration::from_secs(20);
            let mut max_active_flush_workers = 0;
            loop {
                let active_flush_workers = journals[0].active_batch_flush_workers_for_tests();
                max_active_flush_workers = max_active_flush_workers.max(active_flush_workers);
                assert!(
                    active_flush_workers <= JOURNAL_BATCH_MAX_CONCURRENT_FSYNC,
                    "第 {round} 轮 fsync worker 超过进程级上限：actual={active_flush_workers}, limit={JOURNAL_BATCH_MAX_CONCURRENT_FSYNC}"
                );
                let all_started = journals.iter().all(|journal| {
                    journal
                        .batch_target
                        .background_attempt_count
                        .load(Ordering::Acquire)
                        > round
                });
                if all_started {
                    break;
                }
                assert!(
                    Instant::now() < attempts_deadline,
                    "第 {round} 轮 256 个 fsync 未在排队窗口内开始"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                max_active_flush_workers <= JOURNAL_BATCH_MAX_CONCURRENT_FSYNC,
                "第 {round} 轮 worker 峰值超过上限：actual={max_active_flush_workers}, limit={JOURNAL_BATCH_MAX_CONCURRENT_FSYNC}"
            );
            for journal in &journals {
                wait_for_idle_batch_job(journal);
                assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
                assert_eq!(
                    journal.state().expect("状态应读取").last_sequence,
                    round + 1
                );
            }
            if round == 0 {
                assert_eq!(
                    journals[0]
                        .batch_target
                        .background_attempt_count
                        .load(Ordering::Acquire),
                    2,
                    "排队 Journal 首次失败后必须在 worker 槽位释放时重试并成功"
                );
            }
        }
    }

    /// 暂时 IO 失败必须由进程级调度器按有界退避重试，不能静默留下无限窗口。
    #[test]
    fn 后台临时失败会有界退避并最终刷盘() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(
            root.path(),
            SessionId::new("batch-background-retry").expect("Session ID 应有效"),
            config,
        )
        .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "后台重试".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        journal
            .batch_target
            .background_failures_remaining
            .store(2, Ordering::Release);
        journal
            .defer_batch_timeout_for_tests()
            .expect("测试 deadline 应推迟");
        journal
            .append_idempotent(
                SessionEventId::new("event-retry").expect("事件 ID 应有效"),
                1,
                SessionEvent::SessionRenamed {
                    title: "最终落盘".to_owned(),
                },
            )
            .expect("待刷事件应追加");
        {
            let mut inner = journal.inner.lock().expect("Journal 锁应可用");
            inner.pending_since = Some(Instant::now() - JOURNAL_BATCH_MAX_DELAY);
        }
        batch_scheduler().notify();
        wait_for_idle_batch_job(&journal);

        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        assert_eq!(
            journal
                .batch_target
                .background_attempt_count
                .load(Ordering::Acquire),
            3,
            "两次临时失败后必须进行第三次成功尝试"
        );
        let inner = journal.inner.lock().expect("Journal 锁应可用");
        assert_eq!(inner.batch_retry_attempts, 0);
        assert!(inner.batch_retry_at.is_none());
        assert!(inner.batch_flush_failure.is_none());
    }

    /// 后台重试耗尽后，普通追加和显式 barrier 都必须先对账，不能继续静默成功。
    #[test]
    fn 后台持续失败形成sticky栅栏且显式barrier可恢复() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(
            root.path(),
            SessionId::new("batch-background-sticky").expect("Session ID 应有效"),
            config,
        )
        .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "sticky failure".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应先落盘");
        journal
            .batch_target
            .background_failures_remaining
            .store(JOURNAL_BATCH_MAX_ATTEMPTS as usize, Ordering::Release);
        journal
            .defer_batch_timeout_for_tests()
            .expect("测试 deadline 应推迟");
        journal
            .append_idempotent(
                SessionEventId::new("event-pending").expect("事件 ID 应有效"),
                1,
                SessionEvent::SessionRenamed {
                    title: "待确认".to_owned(),
                },
            )
            .expect("待刷事件应追加");
        {
            let mut inner = journal.inner.lock().expect("Journal 锁应可用");
            inner.pending_since = Some(Instant::now() - JOURNAL_BATCH_MAX_DELAY);
        }
        batch_scheduler().notify();
        wait_for_sticky_batch_failure(&journal);

        set_append_fault(AppendFault::Sync);
        let blocked = journal
            .append_idempotent(
                SessionEventId::new("event-blocked").expect("事件 ID 应有效"),
                2,
                SessionEvent::SessionRenamed {
                    title: "不得追加".to_owned(),
                },
            )
            .expect("sticky 栅栏应返回结构化结果");
        assert!(matches!(
            blocked,
            IdempotentAppendOutcome::Indeterminate { .. }
        ));
        assert_eq!(journal.state().expect("状态应读取").last_sequence, 2);
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);

        journal.flush().expect("后续显式 barrier 应完成对账");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        let inner = journal.inner.lock().expect("Journal 锁应可用");
        assert!(inner.batch_flush_failure.is_none());
    }

    /// 没有任务的独立调度器必须在空闲窗口后退出，不能形成常驻轮询线程。
    #[test]
    fn 进程级调度器空闲后退出() {
        let scheduler = Arc::new(BatchScheduler::default());
        scheduler
            .state
            .lock()
            .expect("调度器锁应可用")
            .worker_active = true;
        let worker_scheduler = Arc::clone(&scheduler);
        let worker = std::thread::spawn(move || run_batch_scheduler(worker_scheduler));
        worker.join().expect("空闲调度线程应正常退出");
        assert!(
            !scheduler
                .state
                .lock()
                .expect("调度器锁应可用")
                .worker_active
        );
        assert_eq!(scheduler.active_worker_count.load(Ordering::Acquire), 0);
        assert_eq!(scheduler.worker_start_count.load(Ordering::Acquire), 1);
    }

    /// Barrier 语义：显式 flush 后返回即落盘，同实例窗口清空。
    ///
    /// 攒批中的事件对同实例立即可见（`state` 含窗口事件），显式 `flush`
    /// 后窗口清空；跨重启重开仍能读到全部事件，证明"ack 后必落盘"。
    #[test]
    fn 显式flush保证返回即落盘() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-barrier").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now() + Duration::from_secs(60));
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "屏障语义".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);
        // 普通状态读取不构成 durability barrier，但必须看见同实例事件。
        assert_eq!(journal.state().expect("状态应读取").last_sequence, 1);
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);
        journal.flush().expect("显式刷盘应成功");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        drop(journal);
        let reopened = match SessionJournal::open(root.path(), session_id, config)
            .expect("刷盘后 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("已刷盘事件不应损坏 Session"),
        };
        assert_eq!(
            reopened.state().expect("状态应读取").last_sequence,
            1,
            "flush 后跨重启必须读到事件"
        );
    }

    /// Drop/关闭会唤醒 worker，并在返回前尽力同步未满批窗口。
    #[test]
    fn drop同步未满批窗口() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-drop").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        let target = Arc::clone(&journal.batch_target);
        journal
            .inner
            .lock()
            .expect("Journal 锁应可用")
            .pending_since = Some(Instant::now() + Duration::from_secs(60));
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "Drop 刷盘".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 1);
        drop(journal);
        assert_eq!(
            target.sync_count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "Drop 应恰好同步一次未满批窗口"
        );
        let reopened = match SessionJournal::open(root.path(), session_id, config)
            .expect("Drop 后 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("Drop 刷盘后不应损坏"),
        };
        assert_eq!(reopened.state().expect("状态应恢复").last_sequence, 1);
    }

    /// 自动 Snapshot 必须在写入前同步其锚定的日志前缀。
    #[test]
    fn snapshot锚点先于snapshot持久化() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-snapshot-anchor").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Every { events: 2 },
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "Snapshot 锚点".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        let receipt = journal
            .append_idempotent(
                SessionEventId::new("event-rename").expect("事件 ID 应有效"),
                1,
                SessionEvent::SessionRenamed {
                    title: "已锚定".to_owned(),
                },
            )
            .expect("Snapshot 边界事件应追加");
        assert!(matches!(
            receipt,
            IdempotentAppendOutcome::Appended(AppendReceipt {
                snapshot: SnapshotStatus::Written,
                ..
            })
        ));
        assert_eq!(journal.pending_flush_records().expect("窗口应读取"), 0);
        let snapshot: SessionSnapshot =
            serde_json::from_slice(&fs::read(journal.snapshot_path()).expect("Snapshot 应读取"))
                .expect("Snapshot 应解码");
        assert_eq!(snapshot.through_sequence, 2);
        drop(journal);
        let reopened = match SessionJournal::open(root.path(), session_id, config)
            .expect("Snapshot Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("Snapshot 锚点不应损坏"),
        };
        assert_eq!(reopened.state().expect("状态应恢复").last_sequence, 2);
    }

    /// 截尾恢复：批量窗口内丢失的尾部仍走 `recover_truncated_tail`。
    ///
    /// 未 sync 窗口的字节已是文件中的完整行/坏尾二态；模拟崩溃时损坏尾部
    /// 写入后显式恢复应保留证据、截断坏尾并允许 sequence 连续追加。
    #[test]
    fn 批量窗口截尾仍可显式恢复() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("batch-tail-recovery").expect("Session ID 应有效");
        let config = JournalConfig {
            durability: Durability::FlushAndSync,
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        };
        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("Session 应打开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("全新 Session 不应损坏"),
        };
        journal
            .append_idempotent(
                SessionEventId::new("event-create").expect("事件 ID 应有效"),
                0,
                SessionEvent::SessionCreated {
                    title: "截尾恢复".to_owned(),
                    project_root: "D:/workspace".to_owned(),
                },
            )
            .expect("创建事件应追加");
        journal.flush().expect("创建事件应落盘");
        let log_path = journal.log_path().to_owned();
        drop(journal);
        let damaged_tail = b"{\"schema\":\"partial";
        OpenOptions::new()
            .append(true)
            .open(&log_path)
            .expect("日志应打开")
            .write_all(damaged_tail)
            .expect("坏尾部应写入");
        let recovery = SessionJournal::recover_truncated_tail(root.path(), session_id, config)
            .expect("单一截断尾部应显式恢复");
        assert_eq!(recovery.preserved_bytes, damaged_tail.len() as u64);
        assert_eq!(
            fs::read(&recovery.evidence_path).expect("证据应读取"),
            damaged_tail
        );
        assert_eq!(
            recovery.journal.state().expect("状态应读取").last_sequence,
            1,
            "恢复后有效前缀应保留"
        );
    }
}
