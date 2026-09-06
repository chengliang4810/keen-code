use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use keencode_resources::{
    Durability, IdempotentAppendOutcome, JournalConfig, MessagePart, MessageRole, SessionEvent,
    SessionEventId, SessionEventRecord, SessionId, SessionJournal, SessionMessage, SessionOpen,
    SessionStatus, SnapshotPolicy,
};
use tempfile::TempDir;

/// 构造不自动写 Snapshot 的 Journal 配置，避免测试把缓存写入混入重放路径。
fn config() -> JournalConfig {
    JournalConfig {
        durability: Durability::Buffered,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    }
}

/// 打开一个指定 Session，并断言日志处于可写的健康状态。
fn ready(root: &Path, session: &str) -> SessionJournal {
    match SessionJournal::open(
        root,
        SessionId::new(session).expect("测试 Session 标识应有效"),
        config(),
    )
    .expect("Session 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Session 不应损坏：{:?}", report.issues),
    }
}

/// 追加一条事件并返回权威日志中的完整记录。
fn append(journal: &SessionJournal, event_id: &str, event: SessionEvent) -> SessionEventRecord {
    let expected_sequence = journal.state().expect("状态应读取").last_sequence;
    match journal
        .append_idempotent(
            SessionEventId::new(event_id).expect("事件 ID 应有效"),
            expected_sequence,
            event,
        )
        .expect("事件应追加")
    {
        IdempotentAppendOutcome::Appended(receipt) => receipt.record,
        IdempotentAppendOutcome::AlreadyCommitted { record } => record,
        other => panic!("测试事件不应返回冲突或不确定结果：{other:?}"),
    }
}

/// 创建一个满足 Journal 第一条记录约束的 Session。
fn create_session(journal: &SessionJournal, title: &str) -> SessionEventRecord {
    append(
        journal,
        "event-create",
        SessionEvent::SessionCreated {
            title: title.to_owned(),
            project_root: "D:/workspace".to_owned(),
        },
    )
}

/// 创建一条独立用户消息，用于让各物理记录拥有不同长度。
fn message_event(index: usize, text: &str) -> SessionEvent {
    SessionEvent::MessageAdded {
        message: SessionMessage {
            message_id: format!("message-{index}"),
            turn_id: None,
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: text.to_owned(),
            }],
        },
    }
}

/// 按页读取完整日志，结果用于和不同 page size 的读取结果比较。
fn read_all(journal: &SessionJournal, limit: usize) -> Vec<SessionEventRecord> {
    let mut after = None;
    let mut records = Vec::new();
    loop {
        let page = journal.read_page(after, limit).expect("重放页应读取");
        records.extend(page.records);
        if !page.has_more {
            break;
        }
        after = page.next_after;
        assert!(after.is_some(), "有后续页面时必须提供游标");
    }
    records
}

/// 在字节串中替换一次固定长度或可变长度的片段，并返回是否命中。
fn replace_once(bytes: &mut Vec<u8>, old: &[u8], new: &[u8]) -> bool {
    let Some(index) = bytes.windows(old.len()).position(|window| window == old) else {
        return false;
    };
    bytes.splice(index..index + old.len(), new.iter().copied());
    true
}

/// 验证不同游标和 page size 的结果与完整读取完全一致。
#[test]
fn replay_pages_match_full_read_for_multiple_cursors_and_limits() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-index-pages");
    create_session(&journal, "分页测试");
    append(
        &journal,
        "event-noop-status",
        SessionEvent::SessionStatusChanged {
            status: SessionStatus::Idle,
        },
    );
    append(
        &journal,
        "event-atomic-batch",
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::SessionRenamed {
                    title: "批次标题".to_owned(),
                },
                SessionEvent::SessionStatusChanged {
                    status: SessionStatus::Idle,
                },
            ],
        },
    );
    for index in 1..=17 {
        append(
            &journal,
            &format!("event-message-{index}"),
            message_event(index, &format!("content-{index}")),
        );
    }

    let expected = read_all(&journal, 64);
    assert_eq!(expected.len(), 20);
    for limit in [1, 2, 3, 5, 64] {
        assert_eq!(read_all(&journal, limit), expected, "page size {limit}");
    }
}

/// 验证零可见变化记录和 AtomicBatch 仍各占一个连续的物理 sequence。
#[test]
fn replay_keeps_noop_and_atomic_batch_as_physical_records() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-index-physical");
    create_session(&journal, "物理记录测试");
    append(
        &journal,
        "event-noop-status",
        SessionEvent::SessionStatusChanged {
            status: SessionStatus::Idle,
        },
    );
    append(
        &journal,
        "event-atomic-batch",
        SessionEvent::AtomicBatch {
            events: vec![SessionEvent::SessionRenamed {
                title: "批次标题".to_owned(),
            }],
        },
    );
    append(&journal, "event-message", message_event(1, "最后一条"));

    let page = journal.read_page(None, 16).expect("物理记录页应读取");
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        page.records[1].event,
        SessionEvent::SessionStatusChanged {
            status: SessionStatus::Idle
        }
    ));
    assert!(matches!(
        page.records[2].event,
        SessionEvent::AtomicBatch { .. }
    ));
}

/// 验证冷打开会重建 offset 索引，且重建后追加仍能从准确边界读取。
#[test]
fn replay_index_rebuilds_on_cold_open_and_appends() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-index-reopen");
    create_session(&journal, "冷打开测试");
    append(&journal, "event-message-1", message_event(1, "第一条内容"));
    append(
        &journal,
        "event-message-2",
        message_event(2, "第二条内容更长"),
    );
    let expected = read_all(&journal, 64);
    drop(journal);

    let reopened = ready(root.path(), "replay-index-reopen");
    assert_eq!(read_all(&reopened, 1), expected);
    append(
        &reopened,
        "event-message-3",
        message_event(3, "冷打开后追加"),
    );
    let page = reopened
        .read_page(Some(3), 1)
        .expect("追加后的索引边界应读取");
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![4]
    );
}

/// 验证同长度外部改写会先刷新日志和 offset 索引，不会沿用旧边界。
#[test]
fn replay_refreshes_same_length_external_rewrite_before_using_index() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-index-rewrite");
    create_session(&journal, "base");
    append(&journal, "event-message-1", message_event(1, "content-1"));
    let log_path = journal.log_path().to_owned();
    let before = fs::read(&log_path).expect("原始日志应读取");
    let mut rewritten = before.clone();
    assert!(replace_once(
        &mut rewritten,
        br#""title":"base""#,
        br#""title":"base-long""#,
    ));
    assert!(replace_once(&mut rewritten, b"content-1", b"cont"));
    assert_eq!(rewritten.len(), before.len(), "改写必须保持日志字节长度");
    let mut file = OpenOptions::new()
        .write(true)
        .open(&log_path)
        .expect("外部日志应打开");
    file.set_len(0).expect("外部改写前应截断");
    file.write_all(&rewritten).expect("外部改写应写入");
    file.sync_all().expect("外部改写应同步");
    drop(file);

    let page = journal
        .read_page(Some(1), 1)
        .expect("刷新后的索引边界应读取");
    assert_eq!(page.records[0].sequence, 2);
    let SessionEvent::MessageAdded { message } = &page.records[0].event else {
        panic!("第二条记录应是消息");
    };
    assert!(matches!(
        &message.content[0],
        MessagePart::Text { text } if text == "cont"
    ));
}

/// 验证截断尾部恢复会重建边界，恢复后的新记录仍可由上一个游标读取。
#[test]
fn replay_index_rebuilds_after_truncated_tail_recovery() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = ready(root.path(), "replay-index-tail");
    create_session(&journal, "尾部恢复测试");
    append(&journal, "event-message-1", message_event(1, "完整记录"));
    let log_path = journal.log_path().to_owned();
    drop(journal);

    let damaged_tail = br#"{"schema":"partial"#;
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .expect("日志应打开");
    file.write_all(damaged_tail).expect("截断尾部应写入");
    file.sync_all().expect("截断尾部应同步");
    drop(file);

    let recovery = SessionJournal::recover_truncated_tail(
        root.path(),
        SessionId::new("replay-index-tail").expect("测试 Session 标识应有效"),
        config(),
    )
    .expect("截断尾部应可恢复");
    assert_eq!(recovery.preserved_bytes, damaged_tail.len() as u64);
    append(
        &recovery.journal,
        "event-message-2",
        message_event(2, "恢复后追加"),
    );
    let page = recovery
        .journal
        .read_page(Some(2), 1)
        .expect("恢复后的索引边界应读取");
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![3]
    );
}
