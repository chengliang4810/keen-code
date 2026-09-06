use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Session 日志或快照损坏的稳定分类。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorruptionKind {
    /// 最后一个 JSONL 记录没有完整换行结尾。
    TruncatedTail {
        /// 不完整记录开始的字节偏移。
        byte_offset: u64,
    },
    /// 某一完整行不是有效事件 JSON。
    InvalidJson {
        /// 从一开始计数的日志行号。
        line: usize,
    },
    /// 事件 sequence 跳过了一个或多个值。
    SequenceGap {
        /// 当前应出现的 sequence。
        expected: u64,
        /// 日志实际出现的 sequence。
        actual: u64,
    },
    /// 相邻事件使用了同一个 sequence。
    DuplicateSequence {
        /// 重复的 sequence。
        sequence: u64,
    },
    /// 不同日志记录复用了同一个幂等事件标识。
    DuplicateEventId {
        /// 重复的事件标识。
        event_id: String,
        /// 首次出现该标识的 sequence。
        first_sequence: u64,
        /// 再次出现该标识的 sequence。
        duplicate_sequence: u64,
    },
    /// 事件 sequence 小于先前已接受的值。
    OutOfOrderSequence {
        /// 先前已接受的 sequence。
        previous: u64,
        /// 当前倒退的 sequence。
        actual: u64,
    },
    /// 事件的 schema、version 或 Session 标识不匹配。
    EnvelopeMismatch {
        /// 出现不匹配的日志行号。
        line: usize,
    },
    /// 类型化事件不能应用到此前状态。
    ReductionFailure {
        /// 失败事件的 sequence。
        sequence: u64,
    },
    /// 某一完整或截断事件超过配置的单事件字节上限。
    EventTooLarge {
        /// 从一开始计数的日志行号。
        line: usize,
        /// 事件实际 JSONL 字节数。
        actual: u64,
        /// 配置允许的最大 JSONL 字节数。
        limit: u64,
    },
    /// Snapshot 文件不是有效且受支持的结构。
    InvalidSnapshot,
    /// Snapshot 指向日志中不存在的 sequence 或事件 Hash。
    SnapshotLogMismatch,
    /// Snapshot 状态与相同日志前缀的实时归约结果不同。
    SnapshotStateMismatch,
}

/// 一项可展示但不会触发静默修复的损坏事实。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorruptionIssue {
    /// 机器可读的稳定分类。
    pub kind: CorruptionKind,
    /// 不包含原始事件正文的中文说明。
    pub message: String,
}

impl CorruptionIssue {
    /// 创建不回显可能敏感 payload 的损坏记录。
    pub(crate) fn new(kind: CorruptionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Session 资源层的可恢复错误。
#[derive(Debug, Error)]
pub enum ResourceError {
    /// 外部标识不能安全映射为单一路径段。
    #[error("资源标识无效：{0}")]
    InvalidId(String),
    /// 路径边界不存在、类型不符或越过已验证根目录。
    #[error("资源路径边界无效：{0}")]
    UnsafePath(String),
    /// 路径链包含不允许跟随的符号链接或重解析点。
    #[error("资源路径包含符号链接：{0}")]
    SymlinkRejected(String),
    /// 底层文件系统操作失败。
    #[error("资源 IO 操作失败（{operation}）：{source}")]
    Io {
        /// 不包含用户正文的操作名称。
        operation: &'static str,
        /// 原始 IO 错误。
        #[source]
        source: io::Error,
    },
    /// 调用方提供或磁盘读取的 JSON 无效。
    #[error("资源 JSON 无效：{0}")]
    Json(String),
    /// Session 已因损坏切换为只读报告状态。
    #[error("Session 日志已损坏，只允许读取报告")]
    CorruptReadOnly,
    /// 显式尾部恢复只接受单一截断尾记录，当前日志不满足条件。
    #[error("Session 日志不满足截断尾部恢复条件")]
    TruncatedTailRecoveryNotApplicable,
    /// 文档 revision 与调用方声明的期望值不一致。
    #[error("文档 revision 冲突：期望 {expected}，实际 {actual}")]
    RevisionConflict {
        /// 调用方读取后期望仍保持的 revision。
        expected: u64,
        /// 文件锁内重新读取到的实际 revision。
        actual: u64,
    },
    /// Goal 比较交换试图绕过单例或不可逆生命周期。
    #[error("Goal 生命周期迁移无效：{0}")]
    InvalidGoalTransition(String),
    /// Plan 比较交换试图保存空初态、重复正文或倒退时间。
    #[error("Plan 生命周期迁移无效：{0}")]
    InvalidPlanTransition(String),
    /// 相同幂等操作标识已经绑定到另一份规范化载荷。
    #[error("资源操作标识已绑定到不同请求")]
    OperationConflict,
    /// 单个事件编码后的 JSONL 字节数超过配置限制。
    #[error("Session 事件大小 {actual} 超过限制 {limit}")]
    EventTooLarge {
        /// 编码后的实际 JSONL 字节数。
        actual: u64,
        /// 允许的最大 JSONL 字节数。
        limit: u64,
    },
    /// Session 事件日志超过配置限制。
    #[error("Session 日志大小 {actual} 超过限制 {limit}")]
    JournalTooLarge {
        /// 当前或追加后的日志字节数。
        actual: u64,
        /// 允许的最大日志字节数。
        limit: u64,
    },
    /// Session 事件记录数量超过配置限制。
    #[error("Session 事件数量 {actual} 超过限制 {limit}")]
    JournalRecordLimit {
        /// 当前或追加后的事件数量。
        actual: u64,
        /// 允许的最大事件数量。
        limit: u64,
    },
    /// Session 重放单页数量为零或超过固定安全上限。
    #[error("Session 重放单页数量 {actual} 超过有效范围 1..={limit}")]
    InvalidReplayPageLimit {
        /// 调用方请求的事件数量。
        actual: usize,
        /// 单页允许返回的最大事件数量。
        limit: usize,
    },
    /// Session 重放使用了零或尚未提交的 sequence 游标。
    #[error("Session 重放游标必须指向当前已提交的正 sequence")]
    InvalidReplayCursor,
    /// 重放读取期间观察到与已验证状态不一致的日志内容。
    #[error("Session 事件日志在重放读取期间发生变化")]
    ReplayLogChanged,
    /// Session 某一归约集合超过配置限制。
    #[error("Session 状态集合 {collection} 的数量 {actual} 超过限制 {limit}")]
    StateCollectionLimit {
        /// 稳定集合名称。
        collection: &'static str,
        /// 当前集合元素数量。
        actual: usize,
        /// 允许的最大元素数量。
        limit: usize,
    },
    /// Memory、Goal 或 Plan 文档超过配置限制。
    #[error("资源文档大小 {actual} 超过限制 {limit}")]
    DocumentTooLarge {
        /// 文档实际编码或磁盘字节数。
        actual: u64,
        /// 允许的最大文档字节数。
        limit: u64,
    },
    /// Session Snapshot 超过日志配置允许的缓存上限。
    #[error("Session Snapshot 大小 {actual} 超过限制 {limit}")]
    SnapshotTooLarge {
        /// Snapshot 实际编码或磁盘字节数。
        actual: u64,
        /// 允许的最大 Snapshot 字节数。
        limit: u64,
    },
    /// Session 复制或截断操作正在被另一个 Runtime 占用。
    #[error("Session 变更目标正被另一个 Runtime 占用")]
    SessionMutationBusy,
    /// 相同 operationId 已经绑定到不同 Session 变更请求。
    #[error("Session 变更 operationId 与既有请求冲突")]
    SessionMutationConflict,
    /// Session 复制或截断请求不满足当前权威历史。
    #[error("Session 变更请求不适用：{0}")]
    SessionMutationNotApplicable(String),
    /// 崩溃恢复无法证明目标仍与已持久化事务一致。
    #[error("Session 变更需要人工恢复：{0}")]
    SessionMutationRecoveryRequired(String),
    /// 新事件不能应用到当前权威状态。
    #[error("Session 事件归约失败：{0}")]
    Reduction(String),
    /// Artifact 大小超过当前 Session 限制。
    #[error("Artifact 大小 {actual} 超过限制 {limit}")]
    ArtifactTooLarge {
        /// 实际字节数。
        actual: u64,
        /// 允许的最大字节数。
        limit: u64,
    },
    /// 当前 Session 的 Artifact 数量达到上限。
    #[error("Artifact 数量达到限制 {limit}")]
    ArtifactCountLimit {
        /// 允许的最大文件数。
        limit: usize,
    },
    /// Artifact 文件内容与引用中的摘要不一致。
    #[error("Artifact 内容 Hash 不匹配")]
    ArtifactHashMismatch,
    /// Artifact 引用没有配置可核验实际文件的校验器。
    #[error("Session 未配置 Artifact 引用校验器")]
    ArtifactValidatorRequired,
    /// Artifact 引用属于另一个 Session。
    #[error("Artifact 引用不属于当前 Session")]
    ArtifactScopeMismatch,
    /// Artifact 引用指向的内容文件不存在。
    #[error("Artifact 引用的内容文件不存在")]
    ArtifactNotFound,
    /// Artifact 引用声明的字节数与实际文件不一致。
    #[error("Artifact 引用大小不匹配：声明 {expected}，实际 {actual}")]
    ArtifactSizeMismatch {
        /// 引用中声明的字节数。
        expected: u64,
        /// 文件系统中核验到的实际字节数。
        actual: u64,
    },
    /// Artifact 引用中的媒体类型与首次写入的规范元数据不一致。
    #[error("Artifact 媒体类型与持久化元数据不匹配")]
    ArtifactMediaTypeMismatch,
    /// Artifact 实际字节不能按声明方式恢复为模型内容。
    #[error("Artifact 内容不能按 {materialization} 方式恢复")]
    ArtifactMaterializationMismatch {
        /// 失败的稳定物化类型名称。
        materialization: &'static str,
    },
    /// Artifact 的规范元数据缺失、损坏或与内容身份不一致。
    #[error("Artifact 持久化元数据缺失或不一致")]
    ArtifactMetadataMismatch,
}

impl ResourceError {
    /// 包装一个底层 IO 错误并保留稳定操作名称。
    pub(crate) fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}
