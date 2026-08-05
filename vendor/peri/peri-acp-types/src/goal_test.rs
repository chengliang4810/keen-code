//! Goal DTO 序列化契约测试。

use super::{GoalChangeKind, GoalRecordDto};

/// DTO 必须序列化为当前 ACP 契约的 snake_case 字段名。
#[test]
fn serializes_with_frontend_field_names() {
    let dto = GoalRecordDto::new(
        "goal_1".into(),
        "Ship v2".into(),
        "active".into(),
        Some(100_000),
        42_500,
        300,
        None,
        "2026-08-01T10:00:00Z".into(),
        "2026-08-01T10:05:00Z".into(),
    );
    let value = serde_json::to_value(&dto).expect("序列化 GoalRecordDto 失败");

    assert_eq!(value["id"], "goal_1");
    assert_eq!(value["title"], "Ship v2");
    assert_eq!(value["scope"], "project");
    assert_eq!(value["status"], "active");
    assert_eq!(value["description"], "Ship v2");
    assert_eq!(value["objective"], "Ship v2");
    assert_eq!(value["progress_percent"], 42.5);
    assert_eq!(value["token_budget"], 100_000);
    assert_eq!(value["tokens_used"], 42_500);
    assert_eq!(value["time_used_seconds"], 300);
    assert!(value.get("blocked_reason").is_some()); // null 字段保留
                                                    // 前端不消费的字段不应出现
    assert!(value.get("why").is_none());
    assert!(value.get("milestones").is_none());
}

/// progress_percent 在无 token 预算时应为 null。
#[test]
fn progress_percent_null_without_budget() {
    let dto = GoalRecordDto::new(
        "goal_1".into(),
        "objective".into(),
        "blocked".into(),
        None,
        0,
        0,
        Some("需要用户提供密钥".into()),
        "2026-08-01T10:00:00Z".into(),
        "2026-08-01T10:05:00Z".into(),
    );
    let value = serde_json::to_value(&dto).expect("序列化 GoalRecordDto 失败");
    assert!(value["progress_percent"].is_null());
    assert_eq!(value["blocked_reason"], "需要用户提供密钥");
}

/// GoalChangeKind 序列化必须使用当前 ACP 契约的 snake_case。
#[test]
fn change_kind_serializes_snake_case() {
    for (kind, expected) in [
        (GoalChangeKind::Created, "created"),
        (GoalChangeKind::Updated, "updated"),
        (GoalChangeKind::Transitioned, "transitioned"),
    ] {
        assert_eq!(serde_json::to_value(kind).expect("序列化失败"), expected);
    }
}
