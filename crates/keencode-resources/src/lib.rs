//! KeenCode 的 Session 权威事件、崩溃恢复和本地资源持久化核心。
//!
//! 本 crate 不读取旧存储、不执行迁移，也不包含模型调用或界面投影。
//! 路径层会拒绝检查时可见的符号链接，但当前未对全部操作提供基于目录句柄的强
//! TOCTOU 隔离；调用方可通过 [`filesystem_capabilities`] 查询精确能力边界。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod artifact;
mod atomic;
mod canonical;
mod catalog;
mod document;
mod error;
mod file_snapshot;
mod id;
mod journal;
mod reducer;
mod session_lease;
mod session_mutation;
mod transcript;
mod types;

pub use artifact::{
    ArtifactCapacity, ArtifactLimits, ArtifactMaterialized, ArtifactPreview, ArtifactRef,
    ArtifactStore, ArtifactValidator,
};
pub use atomic::{FilesystemCapabilities, filesystem_capabilities};
pub use catalog::{delete_session_storage, list_session_ids};
pub use document::{
    DocumentLimits, DocumentOperationOutcome, DocumentOperationReceipt, GoalDocument,
    GoalFileStore, GoalRecord, GoalSnapshot, GoalStatus, MAX_DOCUMENT_OPERATION_RECEIPTS,
    MemoryDocument, MemoryEntry, MemoryFileStore, PlanDocument, PlanFileStore,
};
pub use error::{CorruptionIssue, CorruptionKind, ResourceError};
pub use file_snapshot::{
    FILE_SNAPSHOT_CHUNK_BYTES, FileSnapshot, MAX_FILE_SNAPSHOT_BYTES, MAX_FILE_SNAPSHOT_CHUNKS,
};
pub use id::{
    AgentId, ArtifactId, MailboxMessageId, RequestId, ScopeId, SessionEventId, SessionId,
    TerminalId, TurnId, project_scope_id,
};
#[cfg(any(test, feature = "test-support"))]
pub use journal::test_support;
pub use journal::{
    AppendReceipt, Durability, IdempotentAppendOutcome, JournalConfig, MAX_REPLAY_PAGE_RECORDS,
    ReadOnlySessionReport, ReplayPage, SessionJournal, SessionOpen, SnapshotPolicy, SnapshotStatus,
    TruncatedTailRecovery,
};
pub use reducer::{ReductionError, reduce_record};
pub use session_lease::{SessionLease, SessionLeaseAcquire};
pub use session_mutation::{
    SessionEditUserRequest, SessionEditUserResult, SessionForkRequest, SessionForkResult,
    fork_session, prepare_edit_user, recover_session_mutations,
};
pub use transcript::{COMPACTION_SUMMARY_PREFIX, compaction_source_digest_sha256};
pub use types::{
    AppliedCompaction, ArtifactMaterialization, ArtifactUse, CompactionRecord,
    ContextCompressionTrigger, DynamicInputKind, DynamicInputReceipt, GeneratedTitleRecord,
    MailboxMessage, MailboxState, MessageImageSource, MessagePart, MessageRole, ModelRoundState,
    PersistedToolResult, PlanState, ProviderProtocolSnapshot, ProviderSnapshot,
    ReasoningContinuation, ReasoningEffortSnapshot, SessionEvent, SessionEventRecord,
    SessionMessage, SessionState, SessionStatus, SubAgentState, SubAgentStatus, TerminalRecord,
    TodoItem, TodoSnapshot, TodoStatus, ToolCompletionStatus, ToolEffect, ToolFileChange,
    ToolLifecycle, ToolOutcome, ToolRequest, ToolResultPart, TranscriptRecord, TranscriptSegment,
    TranscriptSegmentReference, TurnState, TurnStatus, TurnStopReason, WorktreeRecord,
};
pub use types::{ROOT_AGENT_ID, SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION};
pub use types::{SIDE_EFFECT_UNKNOWN_RESULT_TEXT, side_effect_unknown_result};
