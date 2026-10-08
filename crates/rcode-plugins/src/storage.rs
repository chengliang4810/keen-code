use anyhow::{Context, Result};
use std::{
    fs,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

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

pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("私有文件路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建私有文件目录失败：{}", parent.display()))?;

    let mut builder = tempfile::Builder::new();
    builder.prefix(".rcode-write-");
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
