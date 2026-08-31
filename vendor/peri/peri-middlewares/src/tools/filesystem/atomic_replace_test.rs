use super::{atomic_replace_preserving_permissions, AtomicReplaceError};

/// 连续写入必须覆盖已有普通文件，并且目录中不得残留临时文件。
#[test]
fn atomic_replace_overwrites_existing_target_without_temp_residue() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let target = directory.path().join("script.sh");
    std::fs::write(&target, b"old").expect("写入旧目标");

    atomic_replace_preserving_permissions(&target, b"first").expect("首次覆盖应成功");
    atomic_replace_preserving_permissions(&target, b"second").expect("连续覆盖应成功");

    assert_eq!(std::fs::read(&target).unwrap(), b"second");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

/// 替换失败必须完整保留旧目标，并清除已经写入的同目录临时文件。
#[test]
fn atomic_replace_failure_preserves_target_and_cleans_temp() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let target = directory.path().join("occupied");
    std::fs::create_dir(&target).expect("创建不可由普通文件替换的旧目标");
    let marker = target.join("original.txt");
    std::fs::write(&marker, b"original").expect("写入旧目标标记");

    let error = atomic_replace_preserving_permissions(&target, b"new").unwrap_err();

    assert!(matches!(error, AtomicReplaceError::Replace(_)));
    assert!(target.is_dir());
    assert_eq!(std::fs::read(&marker).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

/// Unix 原子覆盖必须保留原文件的完整权限位，包括可执行位。
#[cfg(unix)]
#[test]
fn atomic_replace_preserves_unix_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("创建临时目录");
    let target = directory.path().join("script.sh");
    std::fs::write(&target, b"old").expect("写入旧目标");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o751))
        .expect("设置旧目标权限");

    atomic_replace_preserving_permissions(&target, b"new").expect("覆盖目标应成功");

    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o751
    );
}
