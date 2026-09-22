use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use keencode_resources::{
    CorruptionKind, Durability, IdempotentAppendOutcome, JournalConfig, SessionEvent,
    SessionEventId, SessionId, SessionJournal, SessionOpen, SessionStatus, SnapshotPolicy,
};
use tempfile::TempDir;

/// 返回关闭自动 Snapshot 的幂等追加测试配置。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::FlushAndSync,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开指定测试 Session 的健康日志。
fn ready(root: &std::path::Path, session: &str) -> SessionJournal {
    match SessionJournal::open(
        root,
        SessionId::new(session).expect("Session ID 应有效"),
        config(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    }
}

/// 构造唯一 Session 创建事件。
fn created_event(title: &str) -> SessionEvent {
    SessionEvent::SessionCreated {
        title: title.to_owned(),
        project_root: "D:/workspace".to_owned(),
    }
}

/// 验证相同幂等标识可跨重启确认已提交，且不同正文不会覆盖原记录。
#[test]
fn retry_across_restart_is_idempotent_and_payload_conflict_is_explicit() {
    let root = TempDir::new().expect("临时目录应创建");
    let event_id = SessionEventId::new("event-create").expect("事件 ID 应有效");
    let event = created_event("原始标题");
    let journal = ready(root.path(), "session-retry");
    let first = journal
        .append_idempotent(event_id.clone(), 0, event.clone())
        .expect("首次追加应返回结果");
    assert!(matches!(first, IdempotentAppendOutcome::Appended(_)));
    drop(journal);

    let reopened = ready(root.path(), "session-retry");
    let retry = reopened
        .append_idempotent(event_id.clone(), 0, event)
        .expect("重启重试应返回结果");
    let IdempotentAppendOutcome::AlreadyCommitted { record } = retry else {
        panic!("相同事件应识别为已提交");
    };
    assert_eq!(record.sequence, 1);

    let conflict = reopened
        .append_idempotent(event_id, 1, created_event("不同标题"))
        .expect("不同正文冲突应返回结果");
    assert!(matches!(
        conflict,
        IdempotentAppendOutcome::EventIdConflict {
            existing_sequence: 1
        }
    ));
    assert_eq!(reopened.state().expect("状态应读取").title, "原始标题");
}

/// 验证过期 sequence CAS 不写入任何事件。
#[test]
fn stale_expected_sequence_is_rejected_without_append() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "session-stale");
    journal
        .append_idempotent(
            SessionEventId::new("event-create").expect("事件 ID 应有效"),
            0,
            created_event("会话"),
        )
        .expect("创建事件应返回结果");
    let before = fs::read(journal.log_path()).expect("日志应读取");

    let outcome = journal
        .append_idempotent(
            SessionEventId::new("event-stale").expect("事件 ID 应有效"),
            0,
            SessionEvent::SessionStatusChanged {
                status: SessionStatus::Waiting,
            },
        )
        .expect("CAS 冲突应返回结果");
    assert!(matches!(
        outcome,
        IdempotentAppendOutcome::SequenceConflict {
            expected_sequence: 0,
            actual_sequence: 1
        }
    ));
    assert_eq!(fs::read(journal.log_path()).expect("日志应读取"), before);
}

/// 验证两个实例竞争同一事件标识时只产生一条物理记录。
#[test]
fn concurrent_same_event_has_one_append_and_one_committed_retry() {
    let root = TempDir::new().expect("临时目录应创建");
    let first = ready(root.path(), "session-race");
    let second = ready(root.path(), "session-race");
    let barrier = Arc::new(Barrier::new(2));
    let event_id = SessionEventId::new("event-race").expect("事件 ID 应有效");
    let event = created_event("竞态会话");

    let first_barrier = barrier.clone();
    let first_id = event_id.clone();
    let first_event = event.clone();
    let first_handle = thread::spawn(move || {
        first_barrier.wait();
        first
            .append_idempotent(first_id, 0, first_event)
            .expect("第一实例应返回结果")
    });
    let second_handle = thread::spawn(move || {
        barrier.wait();
        second
            .append_idempotent(event_id, 0, event)
            .expect("第二实例应返回结果")
    });
    let outcomes = [
        first_handle.join().expect("第一线程应结束"),
        second_handle.join().expect("第二线程应结束"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, IdempotentAppendOutcome::Appended(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(outcome, IdempotentAppendOutcome::AlreadyCommitted { .. })
            })
            .count(),
        1
    );
    let reopened = ready(root.path(), "session-race");
    assert_eq!(reopened.state().expect("状态应读取").last_sequence, 1);
    assert_eq!(
        fs::read_to_string(reopened.log_path())
            .expect("日志应读取")
            .lines()
            .count(),
        1
    );
}

/// 验证磁盘上重复幂等事件标识会被报告为只读损坏，而不是静默覆盖索引。
#[test]
fn duplicate_event_id_on_disk_is_reported_as_corruption() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "session-duplicate-id");
    journal
        .append_idempotent(
            SessionEventId::new("event-first").expect("事件 ID 应有效"),
            0,
            created_event("会话"),
        )
        .expect("创建事件应返回结果");
    journal
        .append_idempotent(
            SessionEventId::new("event-second").expect("事件 ID 应有效"),
            1,
            SessionEvent::SessionStatusChanged {
                status: SessionStatus::Waiting,
            },
        )
        .expect("第二事件应返回结果");
    let log_path = journal.log_path().to_path_buf();
    let mut lines = fs::read_to_string(&log_path)
        .expect("日志应读取")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut second: serde_json::Value = serde_json::from_str(&lines[1]).expect("第二行应是 JSON");
    second["eventId"] = serde_json::Value::String("event-first".to_owned());
    lines[1] = serde_json::to_string(&second).expect("篡改记录应编码");
    fs::write(&log_path, format!("{}\n", lines.join("\n"))).expect("篡改日志应写入");
    drop(journal);

    let opened = SessionJournal::open(
        root.path(),
        SessionId::new("session-duplicate-id").expect("Session ID 应有效"),
        config(),
    )
    .expect("损坏日志应返回报告");
    let SessionOpen::Corrupt(report) = opened else {
        panic!("重复事件标识必须进入只读损坏状态");
    };
    assert!(report.issues.iter().any(|issue| matches!(
        issue.kind,
        CorruptionKind::DuplicateEventId {
            first_sequence: 1,
            duplicate_sequence: 2,
            ..
        }
    )));
}
