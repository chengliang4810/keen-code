#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic::{
    BoundedJson, BoundedRead, atomic_write, ensure_regular_file_or_absent, exclusive_lock,
    prepare_root, read_file_bounded, secure_child_dir, serialize_json_bounded, sync_directory,
};
use crate::canonical::canonical_json_sha256;
use crate::reducer::{
    reduce_record_from_valid_state, validate_atomic_batch_shape, validate_owned_atomic_batch_shape,
};
use crate::{
    ArtifactUse, ArtifactValidator, CorruptionIssue, CorruptionKind, ResourceError, SessionEvent,
    SessionEventId, SessionEventRecord, SessionId, SessionState,
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

/// 单个 JSONL 事件落盘后的持久化强度。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    /// 只写入操作系统文件缓存，适合可重建测试数据。
    Buffered,
    /// 每次追加后执行 `flush`。
    Flush,
    /// 每次追加执行 `flush` 与 `sync_data`。
    #[default]
    FlushAndSync,
}

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
    inner: Mutex<JournalInner>,
}

/// SessionJournal 的可变状态。
struct JournalInner {
    /// 当前完整归约状态。
    state: SessionState,
    /// 从健康权威日志重放得到的幂等事件索引。
    event_index: BTreeMap<SessionEventId, EventIndexEntry>,
    /// 每条物理 JSONL 记录包含换行符后的排他结束字节偏移。
    record_end_offsets: Vec<u64>,
    /// 上次加载或追加后的日志字节数。
    log_len: u64,
    /// 上次加载或追加后观察到的文件系统变化戳。
    log_stamp: LogStamp,
    /// 发现损坏后永久阻止当前实例继续写入。
    read_only: bool,
    /// 当前实例是否仍需为 events.jsonl 的目录项确认一次父目录同步。
    directory_sync_required: bool,
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
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogStamp {
    /// 文件当前字节数。
    len: u64,
    /// 文件系统报告的最后修改时间；不支持时为 `None`。
    modified: Option<SystemTime>,
}

impl SessionJournal {
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
        let sessions = secure_child_dir(&root, "sessions")?;
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
        if !loaded.issues.is_empty() {
            return Ok(SessionOpen::Corrupt(ReadOnlySessionReport {
                session_id,
                last_valid_state: loaded.state,
                valid_records: loaded.valid_records,
                issues: loaded.issues,
                log_path,
            }));
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
        Ok(SessionOpen::Ready(Self {
            session_id,
            session_dir,
            log_path,
            snapshot_path,
            lock_path,
            config,
            artifact_validator,
            inner: Mutex::new(JournalInner {
                state: loaded.state,
                event_index: loaded.event_index,
                record_end_offsets: loaded.record_end_offsets,
                log_len: loaded.log_len,
                log_stamp: loaded.log_stamp,
                read_only: false,
                directory_sync_required: config.durability == Durability::FlushAndSync,
            }),
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
        let sessions = secure_child_dir(&root, "sessions")?;
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
        Ok(TruncatedTailRecovery {
            journal: Self {
                session_id,
                session_dir,
                log_path,
                snapshot_path,
                lock_path,
                config,
                artifact_validator,
                inner: Mutex::new(JournalInner {
                    state: recovered.state,
                    event_index: recovered.event_index,
                    record_end_offsets: recovered.record_end_offsets,
                    log_len: recovered.log_len,
                    log_stamp: recovered.log_stamp,
                    read_only: false,
                    directory_sync_required: config.durability == Durability::FlushAndSync,
                }),
            },
            evidence_path,
            preserved_bytes,
        })
    }

    /// 返回当前内存中与完整日志重放一致的状态快照。
    pub fn state(&self) -> Result<SessionState, ResourceError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
        if inner.read_only {
            return Err(ResourceError::CorruptReadOnly);
        }
        let _file_lock = exclusive_lock(&self.lock_path)?;
        self.refresh_if_changed(&mut inner)?;
        Ok(inner.state.clone())
    }

    /// 从可选独占 sequence 游标之后读取一页权威事件，不把完整日志载入内存。
    ///
    /// `after_sequence` 为 `None` 时从第一条事件开始；游标必须指向当前已经提交的正
    /// sequence，零或超过当前日志末尾的值都会被拒绝，避免未来追加被错误跳过。
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
        let inner = self
            .inner
            .lock()
            .map_err(|_| ResourceError::CorruptReadOnly)?;
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
        inner.record_end_offsets.push(next_log_len);
        inner.log_len = next_log_len;
        inner.log_stamp = next_log_stamp;
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
        let anchor = complete_log_anchor(&self.log_path, self.config.max_log_bytes)?;
        write_snapshot_file(
            &self.snapshot_path,
            &inner.state,
            anchor,
            self.config.durability == Durability::FlushAndSync,
            self.config.max_log_bytes,
        )
    }

    /// 多实例写入改变文件长度时，在持有 OS 文件锁期间重新加载状态。
    fn refresh_if_changed(&self, inner: &mut JournalInner) -> Result<(), ResourceError> {
        let current_stamp = log_stamp(&self.log_path)?;
        if current_stamp == inner.log_stamp {
            return Ok(());
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
        install_loaded_session(inner, loaded);
        Ok(())
    }

    /// 在幂等重试确认已存在事件前补齐调用方要求的文件和目录持久化等级。
    fn confirm_committed_durability(&self, inner: &mut JournalInner) -> Result<(), ResourceError> {
        if self.config.durability == Durability::Buffered {
            return Ok(());
        }
        ensure_regular_file_or_absent(&self.log_path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .open(&self.log_path)
            .map_err(|error| ResourceError::io("open_event_log_for_durability", error))?;
        apply_durability(&mut file, self.config.durability)?;
        drop(file);
        if self.config.durability == Durability::FlushAndSync && inner.directory_sync_required {
            #[cfg(any(test, feature = "test-support"))]
            if take_append_fault(AppendFault::DirectorySync) {
                return Err(injected_io_error("sync_event_log_directory"));
            }
            sync_directory(&self.session_dir, true)?;
            inner.directory_sync_required = false;
        }
        Ok(())
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

    Ok(LoadedSession {
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

    /// 验证可见事件在补齐持久化失败时保持不确定，成功确认后才报告已提交。
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
            assert!(matches!(
                first,
                IdempotentAppendOutcome::Indeterminate { .. }
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
                    AppendFault::DirectorySync => AppendFault::DirectorySync,
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

        let journal = match SessionJournal::open(root.path(), session_id.clone(), config)
            .expect("空日志 Session 应重开")
        {
            SessionOpen::Ready(journal) => journal,
            SessionOpen::Corrupt(_) => panic!("零字节故障不应损坏 Session"),
        };
        set_append_fault(AppendFault::DirectorySync);
        assert!(matches!(
            journal
                .append_idempotent(event_id.clone(), 0, event.clone())
                .expect("首次目录同步故障应返回结构化结果"),
            IdempotentAppendOutcome::Indeterminate { .. }
        ));
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
        set_append_fault(AppendFault::DirectorySync);
        assert!(matches!(
            journal
                .append_idempotent(event_id.clone(), 0, event.clone())
                .expect("幂等补同步故障应返回结构化结果"),
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
    }
}
