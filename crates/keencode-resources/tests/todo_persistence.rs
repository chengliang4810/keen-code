mod support;

use keencode_resources::{
    JournalConfig, ResourceError, SessionEvent, SessionId, SessionJournal, SessionOpen,
    SnapshotPolicy, TodoItem, TodoStatus,
};
use serde_json::json;
use tempfile::TempDir;

use support::TestJournalAppend;

/// 打开一个使用默认容量且关闭快照的测试 Journal。
fn open_journal(root: &TempDir, session_id: &str) -> SessionJournal {
    match SessionJournal::open(
        root.path(),
        SessionId::new(session_id).expect("Session ID 应有效"),
        JournalConfig {
            snapshot_policy: SnapshotPolicy::Disabled,
            ..JournalConfig::default()
        },
    )
    .expect("Journal 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => panic!("测试 Journal 不应损坏"),
    }
}

/// 创建当前测试 Journal 的权威 Session 起点。
fn create_session(journal: &SessionJournal) {
    journal
        .append(SessionEvent::SessionCreated {
            title: "Todo 测试".to_owned(),
            project_root: "C:/todo-project".to_owned(),
        })
        .expect("Session 应创建");
}

/// 创建一个有效 Todo 条目。
fn todo(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_owned(),
        status,
        active_form: format!("正在{content}"),
    }
}

/// 构造满足事件契约的固定测试载荷摘要。
fn payload_sha256(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// Todo 新 schema、revision 与 no-op 事件必须跨 Journal 重开保持一致。
#[test]
fn todo_schema_revision_and_restart_are_authoritative() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "todo-restart");
    create_session(&journal);
    let items = vec![
        todo("实现", TodoStatus::InProgress),
        todo("验证", TodoStatus::Pending),
    ];
    let event = SessionEvent::TodoReplaced {
        items: items.clone(),
        operation_payload_sha256: payload_sha256('0'),
        revision: 1,
    };
    let encoded = serde_json::to_value(&event).expect("Todo 事件应序列化");
    assert_eq!(encoded["payload"]["items"][0]["content"], json!("实现"));
    assert_eq!(
        encoded["payload"]["items"][0]["status"],
        json!("in_progress")
    );
    assert_eq!(
        encoded["payload"]["items"][0]["activeForm"],
        json!("正在实现")
    );
    assert!(encoded["payload"]["items"][0].get("todoId").is_none());
    assert!(encoded["payload"]["items"][0].get("text").is_none());
    assert_eq!(
        encoded["payload"]["operation_payload_sha256"],
        json!(payload_sha256('0'))
    );

    journal.append(event.clone()).expect("Todo 应首次替换");
    assert_eq!(journal.state().expect("Todo 状态应读取").todos.revision, 1);
    journal
        .append(event)
        .expect("相同正文新事件应作为 no-op 记录");
    let state = journal.state().expect("Todo no-op 后状态应读取");
    assert_eq!(state.todos.revision, 1);
    assert_eq!(state.todos.items, items);
    drop(journal);

    let reopened = open_journal(&root, "todo-restart");
    let recovered = reopened.state().expect("Todo 应从 Journal 恢复");
    assert_eq!(recovered.todos.revision, 1);
    assert_eq!(recovered.todos.items, items);
}

/// Todo 归约必须拒绝歧义列表，并只接受空列表表达全部完成后的收起状态。
#[test]
fn todo_reducer_rejects_ambiguous_or_noncanonical_lists() {
    let root = TempDir::new().expect("临时目录应创建");
    let journal = open_journal(&root, "todo-validation");
    create_session(&journal);
    let ambiguous = journal.append(SessionEvent::TodoReplaced {
        items: vec![
            todo("实现", TodoStatus::InProgress),
            todo("验证", TodoStatus::InProgress),
        ],
        operation_payload_sha256: payload_sha256('1'),
        revision: 1,
    });
    assert!(matches!(ambiguous, Err(ResourceError::Reduction(_))));
    let completed = journal.append(SessionEvent::TodoReplaced {
        items: vec![todo("实现", TodoStatus::Completed)],
        operation_payload_sha256: payload_sha256('2'),
        revision: 1,
    });
    assert!(matches!(completed, Err(ResourceError::Reduction(_))));
    assert_eq!(journal.state().expect("失败后状态应读取").last_sequence, 1);

    journal
        .append(SessionEvent::TodoReplaced {
            items: vec![todo("实现", TodoStatus::InProgress)],
            operation_payload_sha256: payload_sha256('3'),
            revision: 1,
        })
        .expect("活动 Todo 应写入");
    journal
        .append(SessionEvent::TodoReplaced {
            items: Vec::new(),
            operation_payload_sha256: payload_sha256('4'),
            revision: 2,
        })
        .expect("空列表应收起完成 Todo");
    let state = journal.state().expect("清除后状态应读取");
    assert_eq!(state.todos.revision, 2);
    assert!(state.todos.items.is_empty());
}
