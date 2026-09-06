use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::atomic::{
    ensure_regular_file_or_absent, prepare_root, secure_child_dir, sync_directory,
};
use crate::{ResourceError, SessionId};

/// 一个 Session Runtime 的跨进程独占所有权凭证。
///
/// 该值不可克隆；只要值仍存活，`runtime.lock` 的操作系统独占锁就保持有效。调用方
/// 同时使用 Runtime lease 与 Journal 时必须先取得本凭证，再调用会获取 `append.lock`
/// 的 Journal API，从而固定锁顺序为 `runtime.lock -> append.lock`。
#[derive(Debug)]
#[must_use = "SessionLease 必须保持存活，提前丢弃会释放 Runtime 所有权"]
pub struct SessionLease {
    /// lease 所属 Session 标识。
    session_id: SessionId,
    /// 已验证的 Session 隔离目录。
    session_dir: PathBuf,
    /// 保持操作系统独占锁存活的文件句柄。
    file: File,
}

/// 一次非阻塞 Session Runtime lease 获取结果。
#[derive(Debug)]
#[must_use = "必须处理 Session lease 已取得或正忙的结果"]
pub enum SessionLeaseAcquire {
    /// 当前调用方已经取得独占 lease，凭证必须在 Runtime 生命周期内保持存活。
    Acquired(SessionLease),
    /// 另一个句柄或进程当前持有同一 Session 的独占 lease。
    Busy {
        /// 当前已被其他 Runtime 占用的 Session 标识。
        session_id: SessionId,
    },
}

impl SessionLease {
    /// 非阻塞获取指定 Session 的 Runtime 独占 lease。
    ///
    /// 锁文件固定为 `<root>/sessions/<session_id>/runtime.lock`，文件正文永久为空且
    /// Drop 时只释放操作系统锁、不删除文件。只有 `fs2` 报告的精确锁竞争错误会映射
    /// 为 [`SessionLeaseAcquire::Busy`]；其余打开、校验或锁错误均失败关闭。
    pub fn try_acquire(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
    ) -> Result<SessionLeaseAcquire, ResourceError> {
        let root = prepare_root(storage_root.as_ref())?;
        let sessions = secure_child_dir(&root, "sessions")?;
        let session_dir = secure_child_dir(&sessions, session_id.as_str())?;
        let lock_path = session_dir.join("runtime.lock");
        let file = open_runtime_lock_file(&lock_path, &session_dir)?;
        ensure_empty_runtime_lock(&file)?;

        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                // 锁内再次检查，避免把检查与 try-lock 之间出现的非空文件当成 lease。
                ensure_empty_runtime_lock(&file)?;
                Ok(SessionLeaseAcquire::Acquired(Self {
                    session_id,
                    session_dir,
                    file,
                }))
            }
            Err(error) if is_exact_lock_contention(&error) => {
                Ok(SessionLeaseAcquire::Busy { session_id })
            }
            Err(error) => Err(ResourceError::io("try_lock_runtime_lease", error)),
        }
    }

    /// 返回 lease 所属的 Session 标识。
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回已验证且包含 `runtime.lock` 的 Session 隔离目录。
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }
}

impl Drop for SessionLease {
    /// 尽力提前释放 lease；即使显式解锁失败，关闭句柄或进程退出仍会释放锁。
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// 创建或打开空的 Runtime 锁文件，并在竞争创建后重新验证路径类型。
fn open_runtime_lock_file(path: &Path, session_dir: &Path) -> Result<File, ResourceError> {
    open_runtime_lock_file_with_sync(path, session_dir, |directory| {
        sync_directory(directory, true)
    })
}

/// 创建或打开锁文件，并允许测试确定性注入首次目录同步结果。
fn open_runtime_lock_file_with_sync(
    path: &Path,
    session_dir: &Path,
    sync_new_file: impl FnOnce(&Path) -> Result<(), ResourceError>,
) -> Result<File, ResourceError> {
    ensure_regular_file_or_absent(path)?;
    let mut create = OpenOptions::new();
    create.read(true).write(true).create_new(true);
    let file = match create.open(path) {
        Ok(file) => {
            // 同步失败会通过 `?` 立即丢弃尚未返回的句柄；空锁文件可以保留供后续
            // 获取重试，但调用方绝不会得到未锁定的 SessionLease。
            sync_new_file(session_dir)?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // 竞争方可能创建了目录、链接或其他非法目标，必须在跟随路径前再次复验。
            ensure_regular_file_or_absent(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .map_err(|error| ResourceError::io("open_runtime_lock_file", error))?
        }
        Err(error) => return Err(ResourceError::io("create_runtime_lock_file", error)),
    };
    // 对最终路径再做一次尽力复验，保持与其他持久化边界相同的链接拒绝策略。
    ensure_regular_file_or_absent(path)?;
    Ok(file)
}

/// 确认已打开的 Runtime 锁句柄仍指向空普通文件。
fn ensure_empty_runtime_lock(file: &File) -> Result<(), ResourceError> {
    let metadata = file
        .metadata()
        .map_err(|error| ResourceError::io("inspect_runtime_lock_file", error))?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(ResourceError::UnsafePath(
            "runtime.lock 必须是永久为空的普通文件".to_owned(),
        ));
    }
    Ok(())
}

/// 只识别 `fs2` 当前平台声明的精确锁竞争原始错误码。
fn is_exact_lock_contention(error: &std::io::Error) -> bool {
    let expected = fs2::lock_contended_error();
    matches!(
        (error.raw_os_error(), expected.raw_os_error()),
        (Some(actual), Some(expected)) if actual == expected
    )
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use fs2::FileExt;

    use super::{is_exact_lock_contention, open_runtime_lock_file_with_sync};
    use crate::ResourceError;

    /// 验证只有平台精确竞争码会映射为 Busy，普通 WouldBlock 仍失败关闭。
    #[test]
    fn 仅精确平台锁竞争码映射为busy() {
        let exact = fs2::lock_contended_error();
        assert!(is_exact_lock_contention(&exact));
        let unrelated = std::io::Error::new(std::io::ErrorKind::WouldBlock, "非平台原始锁错误");
        assert!(!is_exact_lock_contention(&unrelated));
    }

    /// 验证首次锁文件目录同步失败时句柄和锁都不会泄漏给后续获取。
    #[test]
    fn 首次锁文件同步失败会释放句柄且只保留空文件() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_dir = root.path().join("session");
        std::fs::create_dir(&session_dir).expect("Session 目录应创建");
        let lock_path = session_dir.join("runtime.lock");
        let result = open_runtime_lock_file_with_sync(&lock_path, &session_dir, |_| {
            Err(ResourceError::UnsafePath("注入目录同步失败".to_owned()))
        });
        assert!(matches!(result, Err(ResourceError::UnsafePath(_))));
        assert_eq!(
            std::fs::metadata(&lock_path)
                .expect("失败后空锁文件应保留")
                .len(),
            0
        );

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .expect("后续句柄应打开");
        FileExt::try_lock_exclusive(&file).expect("同步失败不应遗留操作系统锁");
        FileExt::unlock(&file).expect("测试锁应释放");
    }
}
