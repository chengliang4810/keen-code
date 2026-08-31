use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// 原子替换文件时可由调用层分别映射的失败阶段。
#[derive(Debug)]
pub(crate) enum AtomicReplaceError {
    /// 临时文件内容未完整写入，目标文件尚未进入替换阶段。
    Write(std::io::Error),
    /// 临时文件已完整写入，但原子替换目标失败。
    Replace(std::io::Error),
}

/// 通过同目录唯一临时文件原子替换目标，并在 Unix 保留既有权限位。
///
/// 临时文件与目标位于同一文件系统；任何写入或替换失败都会清理临时文件，
/// 且不会预先删除旧目标。调用层可根据错误阶段保留各工具既有的错误文案和
/// 草稿恢复行为。
pub(crate) fn atomic_replace_preserving_permissions(
    target: &Path,
    bytes: &[u8],
) -> Result<(), AtomicReplaceError> {
    let temporary = target.with_extension(format!("tmp.{}", uuid::Uuid::now_v7()));
    let mut temporary_file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
    {
        Ok(file) => file,
        Err(error) => return Err(AtomicReplaceError::Write(error)),
    };
    if let Err(error) = temporary_file.write_all(bytes) {
        drop(temporary_file);
        let _ = std::fs::remove_file(&temporary);
        return Err(AtomicReplaceError::Write(error));
    }
    drop(temporary_file);

    // 原文件的 Unix 权限位（含可执行位）必须随替换内容保留。
    if let Ok(metadata) = std::fs::metadata(target) {
        #[cfg(unix)]
        {
            let _ = std::fs::set_permissions(&temporary, metadata.permissions());
        }
        #[cfg(not(unix))]
        let _ = &metadata;
    }

    if let Err(error) = std::fs::rename(&temporary, target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AtomicReplaceError::Replace(error));
    }
    Ok(())
}

#[cfg(test)]
#[path = "atomic_replace_test.rs"]
mod tests;
