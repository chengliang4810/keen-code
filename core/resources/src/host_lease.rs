//! 用户数据根目录的 Host 进程独占租约。
//!
//! Session 级 `runtime.lock` 只能防止同一 Session 被两个 Runtime 同时修改，不能
//! 防止两个进程分别加载 Provider、Journal、扩展和 Web 服务。Host 启动必须先取得
//! 这里的根级锁，并在整个 Host 生命周期内保持返回的文件句柄存活。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::ResourceError;
use crate::atomic::{
    ensure_regular_file_or_absent, is_exact_lock_contention, prepare_root, sync_directory,
};

/// Host 锁文件在用户数据根目录中的固定名称。
pub const HOST_LOCK_FILE_NAME: &str = "host.lock";

/// 当前进程持有根级 Host 锁时的逻辑 owner 类型。
///
/// 该值只用于生命周期与诊断，不会写入锁文件。锁文件必须保持空内容，避免把
/// PID、端点或其他可能过期的发现信息误当成操作系统锁的权威状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOwner {
    /// Desktop 进程内嵌并拥有 Host Core。
    Desktop,
    /// 独立 headless Host 进程拥有 Host Core。
    Headless,
}

/// 根级 Host 租约的非阻塞获取结果。
#[derive(Debug)]
#[must_use = "必须处理 Host lease 已取得或已有 owner 的结果"]
pub enum HostLeaseAcquire {
    /// 当前进程取得了整个数据根的 Host 所有权。
    Acquired(HostLease),
    /// 另一个进程已经持有同一数据根的 Host 所有权。
    Busy {
        /// 发生竞争的数据根；调用方可使用独立 discovery 文件查询端点。
        storage_root: PathBuf,
        /// 权威 OS 锁文件路径；不应删除或覆盖该文件以抢占 owner。
        lock_path: PathBuf,
    },
}

/// 覆盖 Runtime、Provider、Journal、扩展和本地服务生命周期的根级所有权凭证。
#[derive(Debug)]
#[must_use = "HostLease 必须保持存活，提前丢弃会释放 Host 所有权"]
pub struct HostLease {
    storage_root: PathBuf,
    lock_path: PathBuf,
    owner: HostOwner,
    file: File,
}

impl HostLease {
    /// 非阻塞取得指定数据根的 Host owner lease。
    ///
    /// 只把平台报告的精确锁竞争错误转换为 [`HostLeaseAcquire::Busy`]；普通 IO、
    /// 路径替换、非普通文件和空锁文件校验失败均 fail-closed。
    pub fn try_acquire(
        storage_root: impl AsRef<Path>,
        owner: HostOwner,
    ) -> Result<HostLeaseAcquire, ResourceError> {
        let storage_root = prepare_root(storage_root.as_ref())?;
        let lock_path = storage_root.join(HOST_LOCK_FILE_NAME);
        let file = open_host_lock_file(&lock_path, &storage_root)?;
        ensure_empty_host_lock(&file)?;

        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                // 锁内复验，避免检查与 try-lock 之间被替换成非空文件。
                ensure_empty_host_lock(&file)?;
                Ok(HostLeaseAcquire::Acquired(Self {
                    storage_root,
                    lock_path,
                    owner,
                    file,
                }))
            }
            Err(error) if is_exact_lock_contention(&error) => Ok(HostLeaseAcquire::Busy {
                storage_root,
                lock_path,
            }),
            Err(error) => Err(ResourceError::io("try_lock_host_lease", error)),
        }
    }

    /// 返回 Host 所属的数据根目录。
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// 返回固定的 Host 锁文件路径。
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// 返回当前 owner 类型。
    pub const fn owner(&self) -> HostOwner {
        self.owner
    }

    /// 消费租约并显式释放操作系统锁；消费后不能继续把旧值当作 owner 凭证。
    pub fn release(self) -> Result<(), ResourceError> {
        FileExt::unlock(&self.file).map_err(|error| ResourceError::io("release_host_lease", error))
    }
}

impl Drop for HostLease {
    /// 尽力释放根级 OS 锁；即使显式 unlock 失败，进程退出仍会释放句柄对应的锁。
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// 创建或打开空 Host 锁文件，并在创建竞争后重新验证目标类型。
fn open_host_lock_file(path: &Path, storage_root: &Path) -> Result<File, ResourceError> {
    ensure_regular_file_or_absent(path)?;
    let mut create = OpenOptions::new();
    create.read(true).write(true).create_new(true);
    let file = match create.open(path) {
        Ok(file) => {
            // 首次创建后同步父目录；失败时不向调用方返回未锁定句柄。
            sync_directory(storage_root, true)?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // 竞争方可能创建了链接、目录或非空文件，必须重新验证后才打开。
            ensure_regular_file_or_absent(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .map_err(|error| ResourceError::io("open_host_lock_file", error))?
        }
        Err(error) => return Err(ResourceError::io("create_host_lock_file", error)),
    };
    ensure_regular_file_or_absent(path)?;
    Ok(file)
}

/// Host 锁文件必须始终是空的普通文件；不把文件正文当作 owner 状态。
fn ensure_empty_host_lock(file: &File) -> Result<(), ResourceError> {
    let metadata = file
        .metadata()
        .map_err(|error| ResourceError::io("inspect_host_lock_file", error))?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(ResourceError::UnsafePath(
            "host.lock 必须是永久为空的普通文件".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 首次取得根锁并暴露owner和路径() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let lease = match HostLease::try_acquire(root.path(), HostOwner::Headless)
            .expect("首次 Host lease 应成功")
        {
            HostLeaseAcquire::Acquired(lease) => lease,
            HostLeaseAcquire::Busy { .. } => panic!("首次 Host lease 不应竞争"),
        };
        assert_eq!(lease.owner(), HostOwner::Headless);
        assert_eq!(lease.storage_root(), root.path().canonicalize().unwrap());
        assert_eq!(lease.lock_path().file_name().unwrap(), HOST_LOCK_FILE_NAME);
        assert_eq!(std::fs::metadata(lease.lock_path()).unwrap().len(), 0);
    }

    #[test]
    fn 同一根目录的第二个句柄只能得到busy() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let first = match HostLease::try_acquire(root.path(), HostOwner::Desktop)
            .expect("首次 Host lease 应成功")
        {
            HostLeaseAcquire::Acquired(lease) => lease,
            HostLeaseAcquire::Busy { .. } => panic!("首次 Host lease 不应竞争"),
        };
        let second = HostLease::try_acquire(root.path(), HostOwner::Headless)
            .expect("第二次尝试应有确定结果");
        assert!(matches!(second, HostLeaseAcquire::Busy { .. }));
        drop(first);
        assert!(matches!(
            HostLease::try_acquire(root.path(), HostOwner::Headless).unwrap(),
            HostLeaseAcquire::Acquired(_)
        ));
    }

    #[test]
    fn 非空锁文件fail_closed且不会被清空() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let path = root.path().join(HOST_LOCK_FILE_NAME);
        std::fs::write(&path, b"stale-owner").expect("测试锁文件应写入");
        let result = HostLease::try_acquire(root.path(), HostOwner::Desktop);
        assert!(matches!(result, Err(ResourceError::UnsafePath(_))));
        assert_eq!(std::fs::read(path).unwrap(), b"stale-owner");
    }

    #[test]
    fn 释放租约后可以重新取得根锁() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let lease = match HostLease::try_acquire(root.path(), HostOwner::Desktop).unwrap() {
            HostLeaseAcquire::Acquired(lease) => lease,
            HostLeaseAcquire::Busy { .. } => panic!("首次 Host lease 不应竞争"),
        };
        lease.release().expect("显式释放应成功");
        assert!(matches!(
            HostLease::try_acquire(root.path(), HostOwner::Headless).unwrap(),
            HostLeaseAcquire::Acquired(_)
        ));
    }

    #[test]
    fn 锁文件被替换为目录时拒绝打开() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let path = root.path().join(HOST_LOCK_FILE_NAME);
        std::fs::create_dir(&path).expect("测试目录应创建");
        let result = HostLease::try_acquire(root.path(), HostOwner::Desktop);
        assert!(matches!(result, Err(ResourceError::UnsafePath(_))));
    }
}
