#![cfg(unix)]
use rcode_tools::{BackgroundOutputCursor, BackgroundTaskManager, BoundedCommandRequest};
use std::{path::Path, time::Duration};
fn request(root: &Path, command: &str) -> BoundedCommandRequest {
    BoundedCommandRequest::new("/bin/sh", root, Duration::from_secs(60), 64 * 1024)
        .with_args(vec!["-c".into(), command.into()])
}
async fn finished(manager: &BackgroundTaskManager, handle: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .list()
                .unwrap()
                .iter()
                .any(|t| t.task_id == handle && t.status.is_terminal())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn command_port_captures_exit_and_advances_independent_log_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let manager = BackgroundTaskManager::new(directory.path().join("output"), 64 * 1024).unwrap();
    let task = manager
        .start_command(
            "session",
            "test".into(),
            request(
                directory.path(),
                "printf stdout; printf stderr >&2; exit 42",
            ),
            None,
        )
        .await
        .unwrap();
    finished(&manager, &task.task_id).await;
    let first = manager
        .read_output(
            "session",
            &task.task_id,
            BackgroundOutputCursor::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(first.stdout, "stdout");
    assert_eq!(first.stderr, "stderr");
    assert_eq!(first.task.exit_code, Some(42));
    let next = manager
        .read_output("session", &task.task_id, first.next_cursor, None)
        .await
        .unwrap();
    assert!(next.stdout.is_empty() && next.stderr.is_empty());
    assert_eq!(next.next_cursor, first.next_cursor);
}
#[tokio::test]
async fn command_port_caps_disk_output_and_reports_discarded_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let manager = BackgroundTaskManager::new(directory.path().join("output"), 64 * 1024).unwrap();
    let task = manager
        .start_command(
            "session",
            "large".into(),
            request(directory.path(), "head -c 5242880 /dev/zero"),
            None,
        )
        .await
        .unwrap();
    finished(&manager, &task.task_id).await;
    let output = manager
        .read_output(
            "session",
            &task.task_id,
            BackgroundOutputCursor::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(output.task.stdout_bytes, 4 * 1024 * 1024);
    assert_eq!(output.task.discarded_bytes, 1024 * 1024);
    assert!(output.stdout.len() <= 64 * 1024);
    assert!(output.stdout_has_more);
}
#[tokio::test]
async fn command_port_rejects_invalid_cwd_and_stdin_and_honors_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let manager = BackgroundTaskManager::new(directory.path().join("output"), 64 * 1024).unwrap();
    assert!(
        manager
            .start_command(
                "session",
                "invalid".into(),
                request(&directory.path().join("missing"), "true"),
                None
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .start_command(
                "session",
                "invalid".into(),
                request(directory.path(), "cat").with_stdin(vec![1]),
                None
            )
            .await
            .is_err()
    );
    let task = manager
        .start_command(
            "session",
            "sleep".into(),
            request(directory.path(), "sleep 60"),
            None,
        )
        .await
        .unwrap();
    assert!(manager.cancel("other", &task.task_id).is_err());
    manager.cancel("session", &task.task_id).unwrap();
    manager.shutdown().await.unwrap();
    assert!(manager.list_running().unwrap().is_empty());
    assert!(
        manager
            .start_command(
                "session",
                "closed".into(),
                request(directory.path(), "true"),
                None
            )
            .await
            .is_err()
    );
}
