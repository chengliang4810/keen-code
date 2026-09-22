use std::sync::atomic::{AtomicU64, Ordering};

use keencode_resources::{
    AppendReceipt, IdempotentAppendOutcome, ResourceError, SessionEvent, SessionEventId,
    SessionJournal, SnapshotStatus,
};

/// 为仍聚焦其他不变量的旧测试提供唯一事件标识和 CAS 重试。
pub(crate) trait TestJournalAppend {
    /// 使用生产幂等 API 追加测试事件并返回原有测试需要的回执。
    fn append(&self, event: SessionEvent) -> Result<AppendReceipt, ResourceError>;
}

impl TestJournalAppend for SessionJournal {
    /// 在并发旧测试中仅对明确 sequence 冲突更新水位后重试。
    fn append(&self, event: SessionEvent) -> Result<AppendReceipt, ResourceError> {
        static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

        let event_id = SessionEventId::new(format!(
            "test-event-{}",
            NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed)
        ))?;
        let mut expected_sequence = self.state()?.last_sequence;
        loop {
            match self.append_idempotent(event_id.clone(), expected_sequence, event.clone())? {
                IdempotentAppendOutcome::Appended(receipt) => return Ok(receipt),
                IdempotentAppendOutcome::AlreadyCommitted { record } => {
                    return Ok(AppendReceipt {
                        record,
                        snapshot: SnapshotStatus::NotDue,
                    });
                }
                IdempotentAppendOutcome::SequenceConflict {
                    actual_sequence, ..
                } => expected_sequence = actual_sequence,
                IdempotentAppendOutcome::EventIdConflict { .. } => {
                    return Err(ResourceError::Reduction(
                        "测试事件标识意外绑定到不同正文".to_owned(),
                    ));
                }
                IdempotentAppendOutcome::Indeterminate { error } => return Err(error),
            }
        }
    }
}
