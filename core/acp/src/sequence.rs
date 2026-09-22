//! Session 内扩展事件的单调序号分配。

use crate::AcpBoundaryError;

/// 为一个 Session 分配从一开始、严格单调且不回绕的事件序号。
#[derive(Debug, Eq, PartialEq)]
pub struct SessionSequence {
    next: u64,
}

impl SessionSequence {
    /// 创建尚未发出事件的新 Session 序号分配器。
    pub const fn new() -> Self {
        Self { next: 1 }
    }

    /// 从持久层最后确认落盘的序号恢复分配器。
    ///
    /// 调用方只能传入已经持久成功的水位，不得传入仅在内存中分配的序号。
    pub const fn restore(last_persisted_sequence: u64) -> Result<Self, AcpBoundaryError> {
        let Some(next) = last_persisted_sequence.checked_add(1) else {
            return Err(AcpBoundaryError::SequenceExhausted);
        };
        if next == 0 {
            return Err(AcpBoundaryError::InvalidSequence);
        }
        Ok(Self { next })
    }

    /// 返回下一条事件将使用的序号，但不推进状态。
    pub const fn peek(&self) -> u64 {
        self.next
    }

    /// 分配一个序号，并在达到 `u64::MAX` 后永久拒绝继续发出事件。
    pub fn allocate(&mut self) -> Result<u64, AcpBoundaryError> {
        if self.next == 0 {
            return Err(AcpBoundaryError::SequenceExhausted);
        }
        let allocated = self.next;
        self.next = self.next.checked_add(1).unwrap_or(0);
        Ok(allocated)
    }

    /// 返回已经分配的最后序号；尚未分配时返回零。
    pub const fn last_allocated(&self) -> u64 {
        if self.next == 0 {
            u64::MAX
        } else {
            self.next - 1
        }
    }
}

impl Default for SessionSequence {
    /// 创建从一开始的新序号分配器。
    fn default() -> Self {
        Self::new()
    }
}
