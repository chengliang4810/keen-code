//! KeenCode 本地持久化目录。
//!
//! 所有 Rust 后端配置、会话、扩展和日志统一写入当前用户主目录下的
//! 正式构建使用 `.keencode`，开发构建使用 `.keencode-dev`，不再使用各平台的
//! 应用配置或应用数据目录。

use crate::native_paths::NativePaths;
use anyhow::{Context, Result};
use std::fs::File;
use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// 返回宿主已解析的 KeenCode 私有持久化根目录。
///
/// 路径发现只发生在 `NativePaths::discover`，存储层不再从窗口句柄或进程
/// 全局状态推导目录，避免后台任务写入与宿主不一致的数据根。
pub(crate) fn root_dir(paths: &NativePaths) -> Result<PathBuf> {
    if paths.data_root.as_os_str().is_empty() {
        anyhow::bail!("KeenCode 数据根不能为空");
    }
    Ok(paths.data_root.clone())
}

/// 以只读方式打开普通文件，并在支持的平台上禁止跟随最终符号链接。
///
/// 调用方仍须先用 `symlink_metadata` 校验文件类型并在读取后复核路径；
/// 这里的 no-follow 标志负责关闭“检查后、打开前”替换成符号链接的竞态。
pub(crate) fn open_readonly_regular_file(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // Windows FILE_FLAG_OPEN_REPARSE_POINT，拒绝把最终重解析点当作目标文件跟随。
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Linux O_NOFOLLOW 与 O_CLOEXEC，避免最终符号链接竞态和句柄泄露到子进程。
        const O_CLOEXEC: i32 = 0o2_000_000;
        const O_NOFOLLOW: i32 = 0o400_000;
        OpenOptions::new()
            .read(true)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW)
            .open(path)
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // macOS O_NOFOLLOW 与 O_CLOEXEC，语义与 Linux 分支一致。
        const O_CLOEXEC: i32 = 0x0100_0000;
        const O_NOFOLLOW: i32 = 0x0100;
        OpenOptions::new()
            .read(true)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW)
            .open(path)
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        File::open(path)
    }

    #[cfg(not(any(unix, windows)))]
    File::open(path)
}

/// 将私有数据写入同目录唯一临时文件后原子替换目标。
///
/// `std::fs::rename` 在 Windows 不能可靠覆盖已有目标；`NamedTempFile::persist`
/// 使用平台替换原语。临时文件由 RAII 管理，提交前失败时不会残留，也不会先
/// 删除原文件。替换一旦完成就视为已提交；随后父目录同步失败只记录警告，不能
/// 再向调用方报告“写入失败”，否则跨文件/系统密钥库的补偿逻辑会错误回滚已经
/// 与新文件配套的数据，制造内容不一致。
///
/// 与 `workspace::atomic_write_bytes` 的权限差异是刻意的：本函数面向
/// `~/.keencode` 下的 app 私有数据，始终强制 0600；而 workspace 版写用户项目
/// 文件，覆盖时保留原文件权限、新建时走默认 umask。不要把两者合并成一条
/// 无差别路径，也不要在这里把项目文件改成 0600。
/// 按打开句柄有界读取普通文件，并复核路径在读取前后仍指向同一长度的普通文件。
///
/// 这是桌面私有持久化记录的统一读端：拒绝符号链接、目录和读取期间发生的
/// 替换或增长；文件不存在时返回 `None`，由调用方决定默认值还是报错。
pub(crate) fn read_private_bytes_bounded(
    path: &Path,
    max_bytes: u64,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("检查{label}失败：{}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("{label}路径不是普通文件：{}", path.display());
    }
    if metadata.len() > max_bytes {
        anyhow::bail!("{label}超过 {max_bytes} 字节：{}", path.display());
    }
    let file = open_readonly_regular_file(path)
        .with_context(|| format!("打开{label}失败：{}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("读取已打开{label}元数据失败：{}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() != metadata.len() {
        anyhow::bail!("{label}在打开期间发生变化：{}", path.display());
    }
    let mut bytes = Vec::new();
    {
        use std::io::Read;
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取{label}失败：{}", path.display()))?;
    }
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len > max_bytes || actual_len != opened_metadata.len() {
        anyhow::bail!(
            "{label}在读取期间发生变化或超过 {max_bytes} 字节：{}",
            path.display()
        );
    }
    let final_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("复核{label}失败：{}", path.display()))?;
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
    {
        anyhow::bail!("{label}在读取期间发生变化：{}", path.display());
    }
    Ok(Some(bytes))
}

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
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::warn!(
            path = %path.display(),
            parent = %parent.display(),
            %error,
            "私有文件已原子替换，但父目录同步失败"
        );
    }
    Ok(())
}

/// 兼容 NativeHost 对二进制私有记录的明确命名；实现复用同一原子替换边界。
pub(crate) fn atomic_write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_private(path, bytes)
}

/// 为待替换的私有文件创建不覆盖既有文件的持久备份。
///
/// 目标使用 `create_new` 消除“先检查再复制”的覆盖竞态；文件内容先同步，
/// Unix 再同步父目录。任何失败都会清理未完成备份，由调用方拒绝覆盖原文件。
#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::{atomic_write_private, backup_private_file};
    use std::fs;

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
        let marker = target.join("original.txt");
        fs::write(&marker, b"original").expect("写入旧目标标记");

        assert!(atomic_write_private(&target, b"new").is_err());

        assert!(target.is_dir());
        assert_eq!(fs::read(&marker).unwrap(), b"original");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
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
