//! KeenCode 本地持久化目录。
//!
//! 所有 Rust 后端配置、会话、扩展和日志统一写入当前用户主目录下的
//! 正式构建使用 `.keencode`，开发构建使用 `.keencode-dev`，不再使用各平台的
//! 应用配置或应用数据目录。

use anyhow::{Context, Result};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};

/// 开发运行时与正式安装版必须隔离，避免两个进程共享会话数据库和运行时状态。
const KEENCODE_HOME_NAME: &str = if cfg!(debug_assertions) {
    ".keencode-dev"
} else {
    ".keencode"
};

/// 返回当前用户唯一的 KeenCode 持久化根目录。
pub(crate) fn root_dir(app: &AppHandle) -> Result<PathBuf> {
    if std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1"))
        && let Some(path) = std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let home = app.path().home_dir().context("无法确定当前用户目录")?;
    Ok(root_dir_from_home(home))
}

/// 将已经解析的用户主目录转换为 KeenCode 持久化根目录。
fn root_dir_from_home(home: PathBuf) -> PathBuf {
    home.join(KEENCODE_HOME_NAME)
}

/// 原子写入时对新文件和既有文件权限的处理方式。
#[derive(Clone, Copy)]
enum AtomicWriteMode {
    /// 私有数据始终使用仅当前用户可读写的权限。
    Private,
    /// 项目文件新建时遵循普通文件权限，覆盖时保留既有权限。
    PreserveExistingPermissions,
}

/// Windows 原子写入流程的进程内互斥锁，覆盖元数据、临时文件和目标替换。
#[cfg(windows)]
static PERSIST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 使用同目录唯一临时文件写入并原子替换目标。
///
/// `std::fs::rename` 在 Windows 不能可靠覆盖已有目标；`NamedTempFile::persist`
/// 使用平台替换原语。临时文件由 RAII 管理，提交前失败时不会残留，也不会先
/// 删除原文件。替换一旦完成就视为已提交；随后父目录同步失败只记录警告，不能
/// 再向调用方报告“写入失败”，否则跨文件/系统密钥库的补偿逻辑会错误回滚已经
/// 与新文件配套的数据，制造内容不一致。
#[allow(clippy::permissions_set_readonly_false)]
fn atomic_write(path: &Path, bytes: &[u8], mode: AtomicWriteMode) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    // Windows 的目标替换、只读属性补偿和元数据读取必须作为一个临界区执行；
    // 锁要在创建目录和读取权限之前获取，确保同一路径并发写入不会交错整个流程。
    #[cfg(windows)]
    let _persist_guard = PERSIST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    fs::create_dir_all(parent).with_context(|| {
        format!(
            "创建原子写入目录失败（{}）：{}",
            parent.display(),
            path.display()
        )
    })?;

    // 通用文件覆盖时沿用旧权限；私有文件只在 Windows 读取只读属性，
    // 其余平台仍由临时文件模式强制设为 0600。
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("读取原文件权限失败：{}", path.display()));
        }
    };

    let mut builder = tempfile::Builder::new();
    builder.prefix(".keencode-write-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // tempfile 默认是 0600；项目新文件需要像普通 OpenOptions 文件一样受 umask 影响。
        let permissions = match mode {
            AtomicWriteMode::Private => fs::Permissions::from_mode(0o600),
            AtomicWriteMode::PreserveExistingPermissions => fs::Permissions::from_mode(0o666),
        };
        builder.permissions(permissions);
    }
    let mut temporary = builder
        .tempfile_in(parent)
        .with_context(|| format!("创建同目录临时文件失败：{}", parent.display()))?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("写入临时文件失败：{}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("刷新临时文件失败：{}", path.display()))?;
    if matches!(mode, AtomicWriteMode::PreserveExistingPermissions)
        && let Some(permissions) = existing_permissions.as_ref()
    {
        fs::set_permissions(temporary.path(), permissions.clone())
            .with_context(|| format!("保留原文件权限失败：{}", path.display()))?;
    }
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("同步临时文件失败：{}", path.display()))?;

    // Windows 会因目标文件的只读属性拒绝替换；先清除属性，提交失败时再恢复。
    #[cfg(windows)]
    let readonly_target_permissions = existing_permissions
        .as_ref()
        .filter(|permissions| permissions.readonly())
        .cloned();

    #[cfg(windows)]
    if let Some(permissions) = readonly_target_permissions.as_ref() {
        let mut writable_permissions = permissions.clone();
        writable_permissions.set_readonly(false);
        fs::set_permissions(path, writable_permissions)
            .with_context(|| format!("临时清除原文件只读属性失败：{}", path.display()))?;
    }

    let replacement = temporary.persist(path);

    #[cfg(windows)]
    if let Some(permissions) = readonly_target_permissions.as_ref()
        && replacement.is_err()
        && let Err(error) = fs::set_permissions(path, permissions.clone())
    {
        return Err(error).with_context(|| format!("恢复原文件只读属性失败：{}", path.display()));
    }

    if let Err(error) = replacement {
        return Err(error.error).with_context(|| format!("原子替换文件失败：{}", path.display()));
    }

    // persist 会清除临时文件的 Windows 属性；提交成功后补回原目标只读属性，
    // 该补偿失败只能记录警告，不能把已经提交的内容伪装成失败返回。
    #[cfg(windows)]
    if let Some(permissions) = readonly_target_permissions
        && let Err(error) = fs::set_permissions(path, permissions)
    {
        tracing::warn!(
            path = %path.display(),
            %error,
            "文件已原子替换，但恢复新目标只读属性失败"
        );
    }

    #[cfg(unix)]
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::warn!(
            path = %path.display(),
            parent = %parent.display(),
            %error,
            "文件已原子替换，但父目录同步失败"
        );
    }
    Ok(())
}

/// 将私有数据写入同目录唯一临时文件后原子替换目标。
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write(path, bytes, AtomicWriteMode::Private)
}

/// 将一般项目文件写入同目录唯一临时文件后原子替换目标。
///
/// 新文件遵循普通文件的 `0666 & !umask` 权限；覆盖既有文件时保留原权限。
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write(path, bytes, AtomicWriteMode::PreserveExistingPermissions)
}

/// 为待替换的私有文件创建不覆盖既有文件的持久备份。
///
/// 目标使用 `create_new` 消除“先检查再复制”的覆盖竞态；文件内容先同步，
/// Unix 再同步父目录。任何失败都会清理未完成备份，由调用方拒绝覆盖原文件。
pub(crate) fn backup_private_file(source: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("检查待备份私有文件失败：{}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("待备份路径不是普通文件：{}", source.display());
    }
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .context("待备份私有文件名不是有效 Unicode")?;
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut source_file =
        File::open(source).with_context(|| format!("打开待备份文件失败：{}", source.display()))?;

    for suffix in 0..=999_u16 {
        let backup_name = if suffix == 0 {
            format!("{file_name}.{timestamp}.bak")
        } else {
            format!("{file_name}.{timestamp}-{suffix}.bak")
        };
        let backup_path = source.with_file_name(backup_name);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut backup_file = match options.open(&backup_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("创建私有备份文件失败：{}", backup_path.display()));
            }
        };

        let backup_result = (|| -> Result<()> {
            std::io::copy(&mut source_file, &mut backup_file)
                .with_context(|| format!("复制私有备份失败：{}", backup_path.display()))?;
            backup_file
                .flush()
                .with_context(|| format!("刷新私有备份失败：{}", backup_path.display()))?;
            backup_file
                .sync_all()
                .with_context(|| format!("同步私有备份失败：{}", backup_path.display()))?;
            #[cfg(unix)]
            {
                let parent = source.parent().context("待备份私有文件路径缺少父目录")?;
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .with_context(|| format!("同步私有备份目录失败：{}", parent.display()))?;
            }
            Ok(())
        })();
        if let Err(error) = backup_result {
            drop(backup_file);
            let _ = fs::remove_file(&backup_path);
            return Err(error);
        }
        return Ok(backup_path);
    }
    anyhow::bail!("同一秒内的私有文件备份数量已达到上限")
}

/// 在 Tauri 启动前从当前进程环境解析 Windows 用户目录。
#[cfg(target_os = "windows")]
pub(crate) fn root_dir_before_start() -> Result<PathBuf> {
    let user_profile = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("无法从 USERPROFILE 确定当前用户目录")?;
    Ok(root_dir_from_home(user_profile))
}

#[cfg(test)]
mod tests {
    use super::{
        KEENCODE_HOME_NAME, atomic_write_bytes, atomic_write_private, backup_private_file,
        root_dir_from_home,
    };
    use std::{fs, path::PathBuf};

    /// 开发构建必须与正式构建使用不同的持久化根目录。
    #[test]
    fn development_build_uses_isolated_home_name() {
        assert_eq!(KEENCODE_HOME_NAME, ".keencode-dev");
        assert_eq!(
            root_dir_from_home(PathBuf::from("/Users/demo")),
            PathBuf::from("/Users/demo/.keencode-dev")
        );
    }

    /// 连续保存必须直接替换已有目标；Windows 不得因目标存在而失败。
    #[test]
    fn private_atomic_write_replaces_existing_target_repeatedly() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("settings.json");

        atomic_write_private(&path, b"first").expect("首次写入应成功");
        atomic_write_private(&path, b"second").expect("第二次应原子覆盖");
        atomic_write_private(&path, b"third").expect("连续覆盖仍应成功");

        assert_eq!(fs::read(&path).unwrap(), b"third");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// Windows 同一路径并发写入必须串行完成，最终内容只能是某个完整版本且无临时残留。
    #[cfg(windows)]
    #[test]
    fn private_atomic_write_serializes_concurrent_same_path_replacements() {
        use std::{
            sync::{Arc, Barrier},
            thread,
        };

        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = Arc::new(directory.path().join("settings.json"));
        let writer_count = 8;
        let contents: Vec<Vec<u8>> = (0..writer_count)
            .map(|index| format!("完整版本-{index}-keencode").into_bytes())
            .collect();
        let barrier = Arc::new(Barrier::new(writer_count));
        let handles = contents
            .iter()
            .cloned()
            .map(|bytes| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    atomic_write_private(path.as_path(), &bytes).map_err(|error| error.to_string())
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle
                .join()
                .expect("并发写入线程不应 panic")
                .expect("并发写入应全部成功");
        }

        let actual = fs::read(path.as_path()).expect("读取最终配置");
        assert!(
            contents.iter().any(|expected| expected == &actual),
            "最终文件必须是某个完整写入版本"
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// 私有原子写入覆盖历史宽权限文件时必须收紧为当前用户读写权限。
    #[cfg(unix)]
    #[test]
    fn private_atomic_write_tightens_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("settings.json");
        fs::write(&path, b"old").expect("写入旧目标");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).expect("设置旧目标宽权限");

        atomic_write_private(&path, b"new").expect("覆盖私有目标");

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// 一般原子写入新建文件时必须与普通 OpenOptions 的 0666 & umask 语义一致。
    #[cfg(unix)]
    #[test]
    fn general_atomic_write_uses_normal_new_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let expected_path = directory.path().join("expected.txt");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&expected_path)
            .expect("创建普通权限参照文件");
        let expected_mode = fs::metadata(&expected_path)
            .expect("读取普通权限参照文件")
            .permissions()
            .mode()
            & 0o777;

        let target = directory.path().join("project.json");
        atomic_write_bytes(&target, b"project").expect("创建普通目标");

        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            expected_mode
        );
    }

    /// Windows 私有原子写入必须能够覆盖只读目标，并保留目标的只读属性。
    #[cfg(windows)]
    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn private_atomic_write_replaces_readonly_target_and_preserves_attribute() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("settings.json");
        fs::write(&path, b"old").expect("写入旧目标");

        let mut permissions = fs::metadata(&path).expect("读取旧目标权限").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("设置旧目标只读属性");

        atomic_write_private(&path, b"new").expect("覆盖只读私有目标应成功");

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(
            fs::metadata(&path)
                .expect("读取新目标权限")
                .permissions()
                .readonly()
        );
        let mut permissions = fs::metadata(&path).expect("读取清理前权限").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).expect("清理测试文件只读属性");
    }

    /// Windows 通用原子写入必须能够覆盖只读目标，并保留目标的只读属性。
    #[cfg(windows)]
    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn general_atomic_write_replaces_readonly_target_and_preserves_attribute() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("project.txt");
        fs::write(&path, b"old").expect("写入旧目标");

        let mut permissions = fs::metadata(&path).expect("读取旧目标权限").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("设置旧目标只读属性");

        atomic_write_bytes(&path, b"new").expect("覆盖只读通用目标应成功");

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(
            fs::metadata(&path)
                .expect("读取新目标权限")
                .permissions()
                .readonly()
        );
        let mut permissions = fs::metadata(&path).expect("读取清理前权限").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).expect("清理测试文件只读属性");
    }

    /// 替换失败必须保留原目标，并由临时文件 RAII 清理现场。
    #[test]
    fn private_atomic_write_failure_preserves_target_and_cleans_temp() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("occupied");
        fs::create_dir(&target).expect("创建不可被普通文件替换的目标目录");
        let marker = target.join("original.txt");
        fs::write(&marker, b"original").expect("写入旧目标标记");

        assert!(atomic_write_private(&target, b"new").is_err());

        assert!(target.is_dir());
        assert_eq!(fs::read(&marker).unwrap(), b"original");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// Windows 替换只读目标失败时必须恢复旧目标的只读属性。
    #[cfg(windows)]
    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn atomic_write_failure_restores_readonly_target_attribute() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("occupied");
        fs::create_dir(&target).expect("创建不可被普通文件替换的目标目录");
        fs::write(target.join("original.txt"), b"original").expect("写入旧目标标记");

        let mut permissions = fs::metadata(&target)
            .expect("读取旧目标目录权限")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&target, permissions).expect("设置旧目标目录只读属性");

        assert!(atomic_write_private(&target, b"new").is_err());
        assert!(
            fs::metadata(&target)
                .expect("读取恢复后的目标权限")
                .permissions()
                .readonly()
        );
        let mut permissions = fs::metadata(&target).expect("读取清理前权限").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&target, permissions).expect("清理测试目录只读属性");
    }

    /// 私有备份必须不覆盖、完整落盘，并在 Unix 保持仅当前用户可读写。
    #[test]
    fn private_backup_is_unique_synced_and_restricted() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let source = directory.path().join("settings.json");
        fs::write(&source, b"user data").expect("写入源文件");

        let first = backup_private_file(&source).expect("创建首个备份");
        let second = backup_private_file(&source).expect("创建不冲突备份");

        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"user data");
        assert_eq!(fs::read(&second).unwrap(), b"user data");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(first).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(second).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
