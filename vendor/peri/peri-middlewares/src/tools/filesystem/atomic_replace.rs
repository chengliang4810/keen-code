use std::path::Path;

use crate::atomic_file::{atomic_replace, AtomicFileError};

/// 原子替换文件时可由调用层分别映射的失败阶段。
#[derive(Debug)]
pub(crate) enum AtomicReplaceError {
    /// 临时文件内容未完整写入，目标文件尚未进入替换阶段。
    Write(std::io::Error),
    /// 临时文件已完整写入，但原子替换目标失败。
    Replace(std::io::Error),
}

/// 通过共享原子文件原语替换目标，并在 Unix 保留既有权限位。
///
/// `atomic_file` 负责临时文件、刷新、同步、权限和平台原子替换；这里仅将
/// 共享原语的错误阶段映射回文件工具既有的 Write/Replace 语义，供草稿恢复
/// 和错误文案继续按原阶段工作。
pub(crate) fn atomic_replace_preserving_permissions(
    target: &Path,
    bytes: &[u8],
) -> Result<(), AtomicReplaceError> {
    atomic_replace(target, bytes).map_err(|error| match error {
        AtomicFileError::Replace(error) => AtomicReplaceError::Replace(error),
        AtomicFileError::Create(error)
        | AtomicFileError::Write(error)
        | AtomicFileError::Flush(error)
        | AtomicFileError::Sync(error)
        | AtomicFileError::Permissions(error)
        | AtomicFileError::Metadata(error) => AtomicReplaceError::Write(error),
    })
}

#[cfg(test)]
#[path = "atomic_replace_test.rs"]
mod tests;
