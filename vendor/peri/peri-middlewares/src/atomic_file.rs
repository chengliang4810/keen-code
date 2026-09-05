//! 需要把完整内容一次性替换到目标路径时使用的原子文件写入原语。

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

#[cfg(unix)]
use std::fs::File;
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

/// Windows 原子写入流程的进程内互斥锁，避免元数据、只读属性和 MoveFileEx 彼此竞态。
#[cfg(windows)]
static PERSIST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 原子写入时使用的文件权限策略。
#[derive(Clone, Copy)]
enum AtomicFileMode {
    /// 普通项目文件使用 0666，并由操作系统 umask 进一步收紧。
    Project,
    /// 含有凭据或其他敏感数据的文件仅允许当前用户读写。
    Private,
}

/// 原子文件写入失败的阶段，便于调用层保留各自的错误语义。
#[derive(Debug)]
pub enum AtomicFileError {
    /// 创建同目录临时文件失败。
    Create(io::Error),
    /// 向临时文件写入内容失败。
    Write(io::Error),
    /// 刷新临时文件用户态缓冲区失败。
    Flush(io::Error),
    /// 将临时文件内容同步到存储设备失败。
    Sync(io::Error),
    /// 将已有目标文件权限复制到临时文件失败。
    Permissions(io::Error),
    /// 用临时文件替换目标文件失败。
    Replace(io::Error),
    /// 读取已有目标文件元数据失败。
    Metadata(io::Error),
}

impl AtomicFileError {
    /// 取出底层 IO 错误，供调用层转换为既有错误类型。
    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::Create(error)
            | Self::Write(error)
            | Self::Flush(error)
            | Self::Sync(error)
            | Self::Permissions(error)
            | Self::Replace(error)
            | Self::Metadata(error) => error,
        }
    }
}

/// 使用普通项目文件权限写入并原子替换目标文件。
///
/// 新文件使用 `0666 & !umask`，覆盖已有文件时保留 Unix 权限位；含有凭据的
/// 配置应调用 [`atomic_replace_private`]。临时文件由 `tempfile` 管理，写入、
/// 刷新和同步完成后通过官方 `persist` 入口替换目标；任何提交前失败都会由
/// RAII 自动清理临时文件，替换失败也不会预先删除或破坏已有目标文件。
pub fn atomic_replace(path: &Path, contents: &[u8]) -> Result<(), AtomicFileError> {
    atomic_replace_with_mode(path, contents, AtomicFileMode::Project)
}

/// 使用私有文件权限写入并原子替换目标文件。
///
/// 新文件和覆盖后的文件均使用 `0600 & !umask`，不会继承历史目标的宽权限；
/// Windows 上仍保留目标原有的只读属性。
pub fn atomic_replace_private(path: &Path, contents: &[u8]) -> Result<(), AtomicFileError> {
    atomic_replace_with_mode(path, contents, AtomicFileMode::Private)
}

/// 按指定权限策略执行一次完整的原子文件替换。
fn atomic_replace_with_mode(
    path: &Path,
    contents: &[u8],
    mode: AtomicFileMode,
) -> Result<(), AtomicFileError> {
    // 非 Unix 平台没有可移植的 POSIX 权限位；Windows 只需沿用目标只读属性。
    #[cfg(not(unix))]
    let _ = mode;

    // 空的 parent 只表示当前目录，不能直接传给 tempfile::Builder。
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    // Windows 的目标替换、只读属性补偿和元数据读取必须作为一个临界区执行；
    // 否则并发调用可能在某个调用临时清除只读属性或刚完成 MoveFileEx 时观察到
    // 中间状态，并收到 ERROR_ACCESS_DENIED。Unix 的 rename 原子性不需要此锁。
    #[cfg(windows)]
    let _persist_guard = PERSIST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    fs::create_dir_all(parent).map_err(AtomicFileError::Create)?;

    // 提前读取权限；普通文件覆盖时需要复制 Unix 权限，私有文件覆盖时刻意不复制。
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(AtomicFileError::Metadata(error)),
    };

    let mut builder = tempfile::Builder::new();
    builder.prefix(".peri-atomic-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // tempfile 在创建 syscall 中应用此 mode，因此 umask 会按普通文件语义生效。
        let permissions = match mode {
            AtomicFileMode::Project => fs::Permissions::from_mode(0o666),
            AtomicFileMode::Private => fs::Permissions::from_mode(0o600),
        };
        builder.permissions(permissions);
    }

    // tempfile 使用 create_new 创建随机唯一名称，且显式指定目标父目录，
    // 保证替换不会跨文件系统，也避免固定 .tmp 名称的并发碰撞。
    let mut temporary = builder
        .tempfile_in(parent)
        .map_err(AtomicFileError::Create)?;

    temporary
        .as_file_mut()
        .write_all(contents)
        .map_err(AtomicFileError::Write)?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(AtomicFileError::Flush)?;

    // 普通项目文件覆盖时保留已有 Unix 权限（包括可执行位）；私有文件不复制历史
    // 权限，确保旧的 0644/0666 等宽权限在本次写入后收紧为 0600。
    #[cfg(unix)]
    if matches!(mode, AtomicFileMode::Project) {
        if let Some(permissions) = existing_permissions.as_ref() {
            fs::set_permissions(temporary.path(), permissions.clone())
                .map_err(AtomicFileError::Permissions)?;
        }
    }

    temporary
        .as_file()
        .sync_all()
        .map_err(AtomicFileError::Sync)?;

    // Windows 的只读属性可能使 MoveFileExW 拒绝替换。临时清除旧目标的只读位；
    // 替换成功后恢复到新目标，替换失败则在返回前恢复旧目标属性，绝不删除旧目标。
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    let readonly_target_permissions = if existing_permissions
        .as_ref()
        .is_some_and(std::fs::Permissions::readonly)
    {
        let permissions = existing_permissions.as_ref().expect("只读目标应有权限");
        let mut writable_permissions = permissions.clone();
        writable_permissions.set_readonly(false);
        fs::set_permissions(path, writable_permissions).map_err(AtomicFileError::Permissions)?;
        Some(permissions.clone())
    } else {
        None
    };

    // 使用 tempfile 官方 persist：它会在 Windows 上清除 FILE_ATTRIBUTE_TEMPORARY，
    // 并在成功后让 TempPath 放弃析构清理，避免“替换成功后析构竞态删除新文件”。
    match temporary.persist(path) {
        Ok(_persisted_file) =>
        {
            #[cfg(windows)]
            if let Some(permissions) = readonly_target_permissions {
                if let Err(error) = fs::set_permissions(path, permissions) {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "文件已原子替换，但恢复新目标只读属性失败"
                    );
                }
            }
        }
        Err(error) => {
            let tempfile::PersistError {
                error,
                file: _temporary_file,
            } = error;

            #[cfg(windows)]
            if let Some(permissions) = readonly_target_permissions {
                if let Err(restore_error) = fs::set_permissions(path, permissions) {
                    return Err(AtomicFileError::Permissions(restore_error));
                }
            }

            return Err(AtomicFileError::Replace(error));
        }
    }

    #[cfg(unix)]
    sync_parent_directory(parent);

    Ok(())
}

/// 在 Unix 上尽力把父目录元数据同步到存储设备，失败不影响已完成的替换。
#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::warn!(
            path = %parent.display(),
            error = %error,
            "原子替换后同步父目录失败"
        );
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::atomic_replace_private;
    use super::{atomic_replace, AtomicFileError};

    /// 连续覆盖必须保留最后一次内容，并且不留下同目录临时文件。
    #[test]
    fn atomic_replace_overwrites_existing_target_without_temp_residue() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("settings.json");
        std::fs::write(&target, b"old").expect("写入旧目标");

        atomic_replace(&target, b"first").expect("首次覆盖应成功");
        atomic_replace(&target, b"second").expect("连续覆盖应成功");

        assert_eq!(std::fs::read(&target).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// 普通项目文件新建时必须与 OpenOptions 的 0666 & umask 语义一致。
    #[cfg(unix)]
    #[test]
    fn atomic_replace_new_file_uses_normal_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let expected_path = directory.path().join("expected.txt");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&expected_path)
            .expect("创建普通权限参照文件");
        let expected_mode = std::fs::metadata(&expected_path)
            .expect("读取普通权限参照文件")
            .permissions()
            .mode()
            & 0o777;

        let target = directory.path().join("project.txt");
        atomic_replace(&target, b"project").expect("创建普通项目文件");

        assert_eq!(
            std::fs::metadata(target)
                .expect("读取普通项目文件")
                .permissions()
                .mode()
                & 0o777,
            expected_mode
        );
    }

    /// 私有文件新建时必须限制为当前用户读写权限。
    #[cfg(unix)]
    #[test]
    fn atomic_replace_private_new_file_is_restricted() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("credentials.json");
        atomic_replace_private(&target, b"secret").expect("创建私有文件");

        assert_eq!(
            std::fs::metadata(target)
                .expect("读取私有文件")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// 私有文件覆盖历史宽权限目标时必须收紧为当前用户读写权限。
    #[cfg(unix)]
    #[test]
    fn atomic_replace_private_tightens_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("credentials.json");
        std::fs::write(&target, b"old").expect("写入旧目标");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o664))
            .expect("设置旧目标宽权限");

        atomic_replace_private(&target, b"new").expect("覆盖私有文件");

        assert_eq!(
            std::fs::metadata(target)
                .expect("读取收紧后的目标")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// 替换失败必须保留旧目标，并由 TempPath 清理临时文件。
    #[test]
    fn atomic_replace_failure_preserves_target_and_cleans_temp() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("occupied");
        std::fs::create_dir(&target).expect("创建不可被普通文件替换的旧目标");
        let marker = target.join("original.txt");
        std::fs::write(&marker, b"original").expect("写入旧目标标记");

        let error = atomic_replace(&target, b"new").expect_err("替换目录目标应失败");

        assert!(matches!(error, AtomicFileError::Replace(_)));
        assert!(target.is_dir());
        assert_eq!(std::fs::read(&marker).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// Unix 原子覆盖必须保留已有文件的完整权限位，包括可执行位。
    #[cfg(unix)]
    #[test]
    fn atomic_replace_preserves_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("script.sh");
        std::fs::write(&target, b"old").expect("写入旧目标");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o751))
            .expect("设置旧目标权限");

        atomic_replace(&target, b"new").expect("覆盖目标应成功");

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o751
        );
    }

    /// 普通项目文件新建和覆盖都不得在目录中留下临时文件。
    #[cfg(unix)]
    #[test]
    fn atomic_replace_project_failure_cleans_temp_file() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("occupied");
        std::fs::create_dir(&target).expect("创建目录目标");

        assert!(atomic_replace(&target, b"new").is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// Windows 原子覆盖必须保留已有文件的只读属性。
    #[cfg(windows)]
    #[test]
    fn atomic_replace_preserves_windows_readonly_permission() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("settings.json");
        std::fs::write(&target, b"old").expect("写入旧目标");

        let mut permissions = std::fs::metadata(&target)
            .expect("读取旧目标权限")
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions).expect("设置旧目标只读属性");

        atomic_replace(&target, b"new").expect("覆盖只读目标应成功");

        assert!(std::fs::metadata(&target)
            .expect("读取新目标权限")
            .permissions()
            .readonly());
    }

    /// Windows 同一路径并发写入必须全部完成，最终内容只能是某个完整版本且无临时残留。
    #[cfg(windows)]
    #[test]
    fn atomic_replace_serializes_concurrent_same_path_replacements() {
        use std::{
            sync::{Arc, Barrier},
            thread,
        };

        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = Arc::new(directory.path().join("settings.json"));
        let writer_count = 8;
        let contents: Vec<Vec<u8>> = (0..writer_count)
            .map(|index| format!("完整版本-{index}-peri").into_bytes())
            .collect();
        let barrier = Arc::new(Barrier::new(writer_count));
        let handles = contents
            .iter()
            .cloned()
            .map(|bytes| {
                let target = Arc::clone(&target);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    atomic_replace(target.as_path(), &bytes)
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle
                .join()
                .expect("并发写入线程不应 panic")
                .expect("并发写入应全部成功");
        }

        let actual = std::fs::read(target.as_path()).expect("读取最终文件");
        assert!(
            contents.iter().any(|expected| expected == &actual),
            "最终文件必须是某个完整写入版本"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
