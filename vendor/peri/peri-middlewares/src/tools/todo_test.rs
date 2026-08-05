use tokio::sync::mpsc;

use super::*;

#[test]
fn test_description_extended() {
    let (tx, _rx) = mpsc::channel(8);
    let tool = TodoWriteTool::new(tx);
    let desc = tool.description();
    assert!(
        desc.contains("full replacement") || desc.contains("fully replaces"),
        "description 应提及全量替换语义"
    );
    assert!(
        desc.contains("pending") && desc.contains("in_progress") && desc.contains("completed"),
        "description 应提及三种状态值"
    );
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_TodoWrite() {
    let (tx, _rx) = mpsc::channel(8);
    let tool = TodoWriteTool::new(tx);
    assert_eq!(tool.name(), "TodoWrite");
}

#[test]
fn test_todo_item_no_id() {
    let item: TodoItem = serde_json::from_value(serde_json::json!({
        "content": "test",
        "status": "pending"
    }))
    .unwrap();
    assert_eq!(item.content, "test");
}

#[test]
fn test_todo_item_active_form() {
    let item: TodoItem = serde_json::from_value(serde_json::json!({
        "content": "test",
        "activeForm": "Running tests",
        "status": "in_progress"
    }))
    .unwrap();
    assert_eq!(item.active_form, Some("Running tests".to_string()));
}

#[test]
fn test_summarize_changes_by_index() {
    let old = vec![
        TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoItem {
            content: "B".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
    ];
    let new = vec![
        TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::InProgress,
        },
        TodoItem {
            content: "B".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoItem {
            content: "C".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
    ];
    let summary = summarize_changes(&old, &new);
    assert!(
        summary.contains("[0]→in_progress"),
        "should detect status change at [0]: {summary}"
    );
    assert!(
        summary.contains("+[2]"),
        "should detect addition at [2]: {summary}"
    );
}

#[test]
fn test_summarize_changes_empty() {
    let old = vec![TodoItem {
        content: "A".into(),
        active_form: None,
        status: TodoStatus::Pending,
    }];
    let new = vec![TodoItem {
        content: "A".into(),
        active_form: None,
        status: TodoStatus::Pending,
    }];
    let summary = summarize_changes(&old, &new);
    assert_eq!(summary, "saved");
}
