use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::Serialize;

use crate::ResourceError;

/// 所有原子替换临时文件使用的固定前缀，便于所属资源目录在崩溃后安全识别。
pub(crate) const ATOMIC_TEMP_PREFIX: &str = ".keencode-atomic-";

/// 当前编译目标实际提供的文件系统安全能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilesystemCapabilities {
    /// 是否使用不可被路径替换竞态绕过的目录句柄完成全部资源操作。
    pub strong_path_isolation: bool,
    /// FlushAndSync 是否还会同步首次创建或原子替换文件的父目录元数据。
    pub parent_directory_sync: bool,
}

/// 返回当前实现可明确承诺的文件系统能力。
///
/// 当前路径检查会拒绝检查时可见的符号链接，但尚未把所有操作改为相对已打开目录
/// 句柄，因此不对具有本机目录写权限的并发攻击者承诺强 TOCTOU 隔离。
pub const fn filesystem_capabilities() -> FilesystemCapabilities {
    FilesystemCapabilities {
        strong_path_isolation: false,
        parent_directory_sync: cfg!(unix),
    }
}

/// 创建并验证一个在检查时不是符号链接的持久化根目录。
pub(crate) fn prepare_root(root: &Path) -> Result<PathBuf, ResourceError> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| ResourceError::io("read_current_directory", error))?
            .join(root)
    };
    create_directory_chain(&absolute)?;
    ensure_real_directory(&absolute)?;
    fs::canonicalize(&absolute).map_err(|error| ResourceError::io("canonicalize_root", error))
}

/// 在已验证根目录下创建或打开一个检查时未越界的固定/已校验子目录。
pub(crate) fn secure_child_dir(
    canonical_root: &Path,
    name: &str,
) -> Result<PathBuf, ResourceError> {
    let candidate = canonical_root.join(name);
    match fs::symlink_metadata(&candidate) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ResourceError::SymlinkRejected(name.to_owned()));
            }
            if !metadata.is_dir() {
                return Err(ResourceError::UnsafePath(format!("{name} 不是目录")));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&candidate) {
                Ok(()) => sync_directory(canonical_root, true)?,
                // 另一个进程可能在检查后先创建同名路径；统一进入下方类型、链接和
                // 越界复验，不能把任何 AlreadyExists 都直接当成安全目录。
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(ResourceError::io("create_child_directory", error));
                }
            }
        }
        Err(error) => return Err(ResourceError::io("inspect_child_directory", error)),
    }
    ensure_real_directory(&candidate)?;
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| ResourceError::io("canonicalize_child_directory", error))?;
    if !canonical.starts_with(canonical_root) {
        return Err(ResourceError::UnsafePath(
            "子目录越过持久化根目录".to_owned(),
        ));
    }
    Ok(canonical)
}

/// 一个受调用方字节上限保护的完整文件读取结果。
pub(crate) enum BoundedRead {
    /// 文件内容没有超过限制。
    Bytes(Vec<u8>),
    /// 文件至少具有给定字节数，且没有继续分配或读取剩余内容。
    TooLarge {
        /// 已知的实际大小或超过限制后的最小大小。
        actual: u64,
    },
}

/// 从同一已打开句柄检查大小并最多读取 `limit + 1` 字节。
pub(crate) fn read_file_bounded(path: &Path, limit: u64) -> std::io::Result<BoundedRead> {
    let file = File::open(path)?;
    let metadata_len = file.metadata()?.len();
    if metadata_len > limit {
        return Ok(BoundedRead::TooLarge {
            actual: metadata_len,
        });
    }
    let read_limit = limit.saturating_add(1);
    let mut bytes = Vec::new();
    file.take(read_limit).read_to_end(&mut bytes)?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > limit {
        Ok(BoundedRead::TooLarge { actual })
    } else {
        Ok(BoundedRead::Bytes(bytes))
    }
}

/// 一个不会在超过限制后继续增长的 JSON 序列化结果。
pub(crate) enum BoundedJson {
    /// 完整 JSON 字节。
    Bytes(Vec<u8>),
    /// JSON 至少具有给定字节数。
    TooLarge {
        /// 序列化器尝试写入的累计字节数。
        actual: u64,
    },
}

/// 把任意可序列化值编码到有界内存，并可选择可读缩进格式。
pub(crate) fn serialize_json_bounded<T: Serialize>(
    value: &T,
    limit: u64,
    pretty: bool,
) -> Result<BoundedJson, ResourceError> {
    let mut writer = BoundedVecWriter::new(limit);
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    result.map_err(|error| ResourceError::Json(error.to_string()))?;
    Ok(writer.finish())
}

/// 只保留限制内字节但继续接受序列化器写入的内存 Writer。
struct BoundedVecWriter {
    /// 限制内已经保存的字节。
    bytes: Vec<u8>,
    /// 最多保存的字节数。
    limit: u64,
    /// 序列化器累计尝试写入的字节数。
    attempted: u64,
    /// 是否已经观察到超限。
    overflowed: bool,
}

impl BoundedVecWriter {
    /// 创建一个空的有界 Writer。
    fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            attempted: 0,
            overflowed: false,
        }
    }

    /// 返回完整字节或稳定的超限事实。
    fn finish(self) -> BoundedJson {
        if self.overflowed {
            BoundedJson::TooLarge {
                actual: self.attempted,
            }
        } else {
            BoundedJson::Bytes(self.bytes)
        }
    }
}

impl Write for BoundedVecWriter {
    /// 记录总尝试量，并只复制仍处于上限内的前缀。
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.attempted = self
            .attempted
            .saturating_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        let saved = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        let remaining = self.limit.saturating_sub(saved);
        let copy_len = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        self.bytes.extend_from_slice(&buffer[..copy_len]);
        if copy_len < buffer.len() {
            self.overflowed = true;
        }
        Ok(buffer.len())
    }

    /// 内存 Writer 不需要执行额外刷新。
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 拒绝一个已有的符号链接或非普通文件目标。
pub(crate) fn ensure_regular_file_or_absent(path: &Path) -> Result<(), ResourceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ResourceError::SymlinkRejected(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("文件")
                .to_owned(),
        )),
        Ok(metadata) if !metadata.is_file() => Err(ResourceError::UnsafePath(
            "目标存在但不是普通文件".to_owned(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResourceError::io("inspect_file", error)),
    }
}

/// 在目标目录内写入临时文件并原子替换目标。
pub(crate) fn atomic_write(
    destination: &Path,
    bytes: &[u8],
    sync: bool,
) -> Result<(), ResourceError> {
    ensure_regular_file_or_absent(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| ResourceError::UnsafePath("原子写入目标缺少父目录".to_owned()))?;
    ensure_real_directory(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(ATOMIC_TEMP_PREFIX)
        .tempfile_in(parent)
        .map_err(|error| ResourceError::io("create_atomic_temporary", error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| ResourceError::io("write_atomic_temporary", error))?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|error| ResourceError::io("flush_atomic_temporary", error))?;
    if sync {
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| ResourceError::io("sync_atomic_temporary", error))?;
    }
    temporary
        .persist(destination)
        .map_err(|error| ResourceError::io("persist_atomic_file", error.error))?;
    sync_directory(parent, sync)
}

/// 在打开前拒绝检查时可见的符号链接，并独占锁定协调文件。
pub(crate) fn exclusive_lock(path: &Path) -> Result<ExclusiveFileLock, ResourceError> {
    ensure_regular_file_or_absent(path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| ResourceError::io("open_lock_file", error))?;
    file.lock_exclusive()
        .map_err(|error| ResourceError::io("lock_file", error))?;
    Ok(ExclusiveFileLock { file })
}

/// 一个在 Drop 时由操作系统释放的独占文件锁。
pub(crate) struct ExclusiveFileLock {
    /// 保持锁存活的文件句柄。
    file: File,
}

impl Drop for ExclusiveFileLock {
    /// 尽力提前解锁；进程退出或句柄关闭仍会由操作系统释放。
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// 逐层创建缺失目录，并在 Unix 上同步每个新目录的父目录项。
fn create_directory_chain(path: &Path) -> Result<(), ResourceError> {
    if path.exists() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor
            .parent()
            .ok_or_else(|| ResourceError::UnsafePath("持久化根目录缺少已有父目录".to_owned()))?;
    }
    ensure_real_directory(cursor)?;
    for directory in missing.into_iter().rev() {
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ResourceError::io("create_root_directory", error)),
        }
        ensure_real_directory(&directory)?;
        let parent = directory
            .parent()
            .ok_or_else(|| ResourceError::UnsafePath("新目录缺少可同步父目录".to_owned()))?;
        sync_directory(parent, true)?;
    }
    Ok(())
}

/// 验证目录本身不是符号链接且确实是目录。
fn ensure_real_directory(path: &Path) -> Result<(), ResourceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ResourceError::io("inspect_directory", error))?;
    if metadata.file_type().is_symlink() {
        return Err(ResourceError::SymlinkRejected(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("目录")
                .to_owned(),
        ));
    }
    if !metadata.is_dir() {
        return Err(ResourceError::UnsafePath("目标不是目录".to_owned()));
    }
    Ok(())
}

/// 在支持目录 fsync 的平台同步原子重命名元数据。
pub(crate) fn sync_directory(parent: &Path, sync: bool) -> Result<(), ResourceError> {
    if !sync {
        return Ok(());
    }
    #[cfg(unix)]
    {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ResourceError::io("sync_parent_directory", error))?;
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}
