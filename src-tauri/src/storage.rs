//! KeenCode 本地持久化目录。
//!
//! 所有 Rust 后端配置、会话、扩展和日志统一写入当前用户主目录下的
//! `.keencode`，不再使用各平台的应用配置或应用数据目录。

use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};

/// KeenCode 在用户主目录下使用的唯一持久化目录名。
const KEENCODE_HOME_NAME: &str = ".keencode";

/// 返回当前用户唯一的 KeenCode 持久化根目录。
pub(crate) fn root_dir(app: &AppHandle) -> Result<PathBuf> {
    let home = app.path().home_dir().context("无法确定当前用户目录")?;
    Ok(root_dir_from_home(home))
}

/// 将已经解析的用户主目录转换为 KeenCode 持久化根目录。
fn root_dir_from_home(home: PathBuf) -> PathBuf {
    home.join(KEENCODE_HOME_NAME)
}

/// 将私有数据写入同目录唯一临时文件后原子替换目标。
///
/// `std::fs::rename` 在 Windows 不能可靠覆盖已有目标；`NamedTempFile::persist`
/// 使用平台替换原语。临时文件由 RAII 管理，失败时不会残留，也不会先删除原文件。
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("私有文件路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建私有文件目录失败：{}", parent.display()))?;

    let mut builder = tempfile::Builder::new();
    builder.prefix(".keencode-write-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o600));
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
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("同步临时文件失败：{}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("原子替换私有文件失败：{}", path.display()))?;

    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("同步私有文件目录失败：{}", parent.display()))?;
    Ok(())
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
    let parent = source.parent().context("待备份私有文件路径缺少父目录")?;
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
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .with_context(|| format!("同步私有备份目录失败：{}", parent.display()))?;
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
        KEENCODE_HOME_NAME, atomic_write_private, backup_private_file, root_dir_from_home,
    };
    use std::{fs, path::PathBuf};

    /// 持久化根目录名必须保持为当前唯一的 `.keencode`。
    #[test]
    fn uses_single_keencode_home_name() {
        assert_eq!(KEENCODE_HOME_NAME, ".keencode");
        assert_eq!(
            root_dir_from_home(PathBuf::from("/Users/demo")),
            PathBuf::from("/Users/demo/.keencode")
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

    /// 替换失败必须保留原目标，并由临时文件 RAII 清理现场。
    #[test]
    fn private_atomic_write_failure_preserves_target_and_cleans_temp() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("occupied");
        fs::create_dir(&target).expect("创建不可被普通文件替换的目标目录");

        assert!(atomic_write_private(&target, b"new").is_err());

        assert!(target.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
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
