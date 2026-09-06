use std::fs;

use keencode_resources::{
    ResourceError, SessionId, SessionJournal, SessionOpen, delete_session_storage, list_session_ids,
};
use tempfile::tempdir;

/// 创建一个由资源层完整初始化的空 Session 目录。
fn create_session_directory(root: &std::path::Path, id: &str) {
    let opened = SessionJournal::open(
        root,
        SessionId::new(id).expect("测试 Session 标识应有效"),
        Default::default(),
    )
    .expect("测试 Session 目录应创建");
    assert!(matches!(opened, SessionOpen::Ready(_)));
}

#[test]
fn catalog_lists_only_verified_session_directories_in_stable_order() {
    let directory = tempdir().expect("应创建测试目录");
    create_session_directory(directory.path(), "session-b");
    create_session_directory(directory.path(), "session-a");

    let listed = list_session_ids(directory.path()).expect("合法目录应可列举");
    assert_eq!(
        listed.iter().map(SessionId::as_str).collect::<Vec<_>>(),
        ["session-a", "session-b"]
    );
}

#[test]
fn catalog_fails_closed_on_non_session_entries() {
    let directory = tempdir().expect("应创建测试目录");
    create_session_directory(directory.path(), "session-a");
    fs::write(
        directory.path().join("sessions").join("unexpected.txt"),
        b"x",
    )
    .expect("应创建非法目录项");

    assert!(matches!(
        list_session_ids(directory.path()),
        Err(ResourceError::UnsafePath(_))
    ));
}

#[test]
fn deletion_is_bounded_and_idempotent() {
    let directory = tempdir().expect("应创建测试目录");
    let session_id = SessionId::new("session-a").expect("测试 Session 标识应有效");
    create_session_directory(directory.path(), session_id.as_str());

    assert!(delete_session_storage(directory.path(), &session_id).expect("首次删除应成功"));
    assert!(!delete_session_storage(directory.path(), &session_id).expect("重复删除应幂等"));
    assert!(
        list_session_ids(directory.path())
            .expect("删除后应可列举")
            .is_empty()
    );
}
